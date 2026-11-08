//! The Map tab: the game's map art with resource nodes and, from a save,
//! what's been built. Pan and zoom like the graph (middle-drag or Ctrl+drag,
//! mouse wheel); drag on the map to select machines, then import them into
//! the planner. It follows the newest save of the chosen session.

use crate::app::FactoryApp;
use crate::backend::Msg;
use crate::theme::*;
use crate::widgets;
use egui::{pos2, vec2, Color32, Pos2, Rect, RichText, Sense, Stroke};
use fb_model::saves::SaveSummary;
use fb_model::world::{MapInfo, Session, SiteKind, World};
use fb_model::Purity;
use serde_json::json;
use std::collections::BTreeSet;

/// How often to ask whether the game wrote a newer save.
const FOLLOW_SECONDS: f64 = 15.0;
/// A building is about this wide (cm), for sizing its mark.
const MACHINE_CM: f64 = 1000.0;
/// Belts on the map: the game's dark window grey reads well on the map art.
const BELT: Color32 = HEADER;
/// Pipes: the map art's own ocean colour (sampled from the map image).
const PIPE: Color32 = Color32::from_rgb(0x4C, 0x6F, 0x78);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum View {
    #[default]
    Graph,
    Map,
}

#[derive(Default)]
pub struct MapState {
    pub info: Option<MapInfo>,
    pub info_error: Option<String>,
    pub info_requested: bool,
    pub sessions: Option<Vec<Session>>,
    pub session: Option<String>,
    pub world: Option<World>,
    pub loading: Option<String>,
    pub world_error: Option<String>,
    next_follow: f64,
    /// World X/Y at the centre of the view, and screen pixels per cm.
    center: (f64, f64),
    scale: f64,
    fitted: bool,
    /// Box selection being dragged (screen), and the machines selected.
    drag_from: Option<Pos2>,
    pub selected: BTreeSet<u32>,
    /// Bounding boxes of carriers (world), to skip the ones off screen.
    carrier_boxes: Vec<[f32; 4]>,
    /// The map area on screen last frame.
    pub rect: Option<Rect>,
}

impl MapState {
    fn to_screen(&self, rect: Rect, x: f64, y: f64) -> Pos2 {
        let c = rect.center();
        pos2(c.x + ((x - self.center.0) * self.scale) as f32, c.y + ((y - self.center.1) * self.scale) as f32)
    }
    fn to_world(&self, rect: Rect, p: Pos2) -> (f64, f64) {
        let c = rect.center();
        (self.center.0 + (p.x - c.x) as f64 / self.scale, self.center.1 + (p.y - c.y) as f64 / self.scale)
    }
    fn set_world(&mut self, w: World) {
        self.carrier_boxes = w
            .carriers
            .iter()
            .map(|c| {
                let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
                for p in &c.points {
                    b[0] = b[0].min(p[0]);
                    b[1] = b[1].min(p[1]);
                    b[2] = b[2].max(p[0]);
                    b[3] = b[3].max(p[1]);
                }
                b
            })
            .collect();
        self.selected.retain(|id| (*id as usize) < w.machines.len());
        self.world = Some(w);
    }
}

impl FactoryApp {
    /// First time the Map tab shows: fetch the map and the sessions.
    fn map_start(&mut self) {
        if self.map.info_requested {
            return;
        }
        self.map.info_requested = true;
        self.backend.call("map_info", json!({}), Msg::MapInfo);
        self.backend.call("world_sessions", json!({}), Msg::WorldSessions);
    }

    fn load_world(&mut self, path: String) {
        self.map.loading = Some(path.clone());
        self.map.world_error = None;
        let p = path.clone();
        self.backend.call("world_load", json!({ "path": path }), move |r| Msg::WorldLoaded { path: p, result: r });
    }

    fn choose_session(&mut self, session: String) {
        let newest = self.map.sessions.as_ref().and_then(|l| l.iter().find(|s| s.session == session)).and_then(|s| s.saves.first()).map(|s| s.path.clone());
        self.map.session = Some(session);
        self.map.selected.clear();
        if let Some(p) = newest {
            self.load_world(p);
        }
    }

    pub(crate) fn on_map_msg(&mut self, msg: Msg) {
        match msg {
            Msg::MapInfo(r) => match r {
                Ok(i) => self.map.info = Some(i),
                Err(e) => self.map.info_error = Some(e),
            },
            Msg::WorldSessions(r) => {
                let list = r.unwrap_or_default();
                let first = list.first().map(|s| s.session.clone());
                self.map.sessions = Some(list);
                if self.map.session.is_none() {
                    if let Some(s) = first {
                        self.choose_session(s);
                    }
                }
            }
            Msg::WorldLoaded { path, result } => {
                if self.map.loading.as_deref() == Some(path.as_str()) {
                    self.map.loading = None;
                }
                match result {
                    Ok(w) => self.map.set_world(w),
                    Err(e) => self.map.world_error = Some(e),
                }
            }
            Msg::WorldLatest(r) => {
                if let Ok(Some(latest)) = r {
                    let current = self.map.world.as_ref().map(|w| w.save.path.clone());
                    let newer = current.as_deref() != Some(latest.path.as_str()) && self.map.loading.is_none();
                    if newer {
                        self.load_world(latest.path.clone());
                    }
                }
            }
            _ => {}
        }
    }

    fn map_follow(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        let Some(session) = self.map.session.clone() else { return };
        if now >= self.map.next_follow {
            self.map.next_follow = now + FOLLOW_SECONDS;
            if self.map.world.is_some() {
                self.backend.call("world_latest", json!({ "session": session }), |r: Result<Option<SaveSummary>, String>| Msg::WorldLatest(r));
            }
        }
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(FOLLOW_SECONDS));
    }

    fn import_selection(&mut self, ctx: &egui::Context) {
        let Some(world) = self.map.world.clone() else { return };
        let game = self.user.game.clone();
        let (file, report) = crate::map_import::import(&world, &self.map.selected, &game, self.map.info.as_ref());
        if report.nodes == 0 {
            self.say(ctx, "Nothing in the selection can be planned (no machines with recipes or extractors).", true);
            return;
        }
        self.import_factory(ctx, file);
        self.view = View::Graph;
        let mut text = format!("Imported {} machines as {} nodes.", report.machines, report.nodes);
        if report.left_out > 0 {
            text += &format!(" {} left out (no recipe set, or generators).", report.left_out);
        }
        if report.mixed_sloops > 0 {
            text += " Some groups had different Somersloop counts; the most common is used.";
        }
        self.say(ctx, text, false);
    }
}

fn date(unix: i64) -> String {
    crate::progress_ui::date_text(unix)
}

/// The Map tab's content.
pub fn map_ui(app: &mut FactoryApp, ui: &mut egui::Ui) {
    app.map_start();
    let ctx = ui.ctx().clone();
    app.map_follow(&ctx);
    let assets_ready = app.status.as_ref().is_some_and(|s| s.icons_found > 0);

    // Toolbar: session, what's shown, and the import action.
    let mut pick: Option<String> = None;
    let mut import = false;
    let mut clear = false;
    let mut extract = false;
    let (bar, _) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::hover());
    ui.painter().rect_filled(bar, 0.0, HEADER);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(bar.shrink2(vec2(10.0, 0.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
    {
        let ui = &mut child;
        ui.label(RichText::new("Save").color(TEXT_DIM));
        let current = app.map.session.clone().unwrap_or_else(|| "None".into());
        egui::ComboBox::from_id_salt("map_session").selected_text(current).width(200.0).show_ui(ui, |ui| {
            for s in app.map.sessions.iter().flatten() {
                let when = s.saves.first().map(|x| date(x.saved_unix)).unwrap_or_default();
                if ui.selectable_label(app.map.session.as_deref() == Some(s.session.as_str()), format!("{}  ·  {when}", s.session)).clicked() {
                    pick = Some(s.session.clone());
                }
            }
        });
        let status = if app.map.loading.is_some() {
            "Reading the save…".to_string()
        } else if let Some(e) = &app.map.world_error {
            e.clone()
        } else if let Some(w) = &app.map.world {
            format!("{} · saved {} · follows the newest save", w.save.name, date(w.save.saved_unix))
        } else if app.map.sessions.as_ref().is_some_and(|l| l.is_empty()) {
            "No saves found in the game's save folder.".to_string()
        } else {
            String::new()
        };
        let err = app.map.world_error.is_some();
        ui.label(RichText::new(status).small().color(if err { DANGER } else { TEXT_DIM }));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let n = app.map.selected.len();
            import = widgets::button_enabled(ui, "Import into planner", n > 0).on_hover_text("Adds the selected machines to the graph, one node per recipe.").clicked();
            if n > 0 {
                clear = widgets::button(ui, "Clear selection").clicked();
                ui.label(RichText::new(format!("{n} selected")).color(TEXT));
            } else if app.map.world.is_some() {
                ui.label(RichText::new("Drag on the map to select machines").small().color(TEXT_DIM));
            }
        });
    }
    if let Some(s) = pick {
        app.choose_session(s);
    }
    if clear {
        app.map.selected.clear();
    }
    if import {
        app.import_selection(&ctx);
    }

    let rect = ui.available_rect_before_wrap();
    app.map.rect = Some(rect);
    let resp = ui.allocate_rect(rect, Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, PIPE);

    let Some(info) = app.map.info.clone() else {
        let text = match &app.map.info_error {
            Some(_) if assets_ready => "The map hasn't been extracted from the game yet.",
            Some(_) => "Extract the game's icons first (File › Settings).",
            None => "Loading the map…",
        };
        painter.text(rect.center(), egui::Align2::CENTER_CENTER, text, theme_font(15.0), TEXT);
        if app.map.info_error.is_some() && assets_ready {
            let b = Rect::from_center_size(rect.center() + vec2(0.0, 36.0), vec2(200.0, 30.0));
            let mut c = ui.new_child(egui::UiBuilder::new().max_rect(b));
            extract = widgets::button_full(&mut c, "Extract map data").clicked();
        }
        if extract {
            if let Some(s) = app.status.clone() {
                app.open_setup(crate::setup::Wizard::icons_only(&s));
                app.map.info_requested = false;
            }
        }
        return;
    };

    let state = &mut app.map;
    if !state.fitted && rect.width() > 10.0 {
        state.scale = (rect.width().min(rect.height()) as f64) / info.size;
        state.center = (info.x_min + info.size / 2.0, info.y_min + info.size / 2.0);
        state.fitted = true;
    }
    // Zoom around the pointer; pan with middle-drag or Ctrl+drag.
    let fit = (rect.width().min(rect.height()) as f64) / info.size;
    if let Some(p) = resp.hover_pos() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            let before = state.to_world(rect, p);
            state.scale = (state.scale * (scroll as f64 * 0.002).exp()).clamp(fit * 0.5, 0.5);
            let after = state.to_world(rect, p);
            state.center.0 += before.0 - after.0;
            state.center.1 += before.1 - after.1;
        }
    }
    let panning = ui.input(|i| i.pointer.middle_down() || (i.pointer.primary_down() && i.modifiers.command));
    if resp.dragged() && panning {
        let d = resp.drag_delta();
        state.center.0 -= d.x as f64 / state.scale;
        state.center.1 -= d.y as f64 / state.scale;
    } else if resp.drag_started_by(egui::PointerButton::Primary) && !panning {
        state.drag_from = resp.interact_pointer_pos();
    }

    // Tiles: the coarse level underneath, then the level that matches the zoom.
    let mut canvas = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    canvas.set_clip_rect(rect);
    let shown_px = info.size * state.scale;
    let mut want = 0u32;
    while want + 1 < info.levels && (info.tile_px as f64) * 2f64.powi(want as i32) < shown_px {
        want += 1;
    }
    let levels: Vec<u32> = if want == 0 { vec![0] } else { vec![0, want] };
    for z in levels {
        let n = 1u32 << z;
        let tile_cm = info.size / n as f64;
        for ty in 0..n {
            for tx in 0..n {
                let x0 = info.x_min + tx as f64 * tile_cm;
                let y0 = info.y_min + ty as f64 * tile_cm;
                let r = Rect::from_two_pos(app.map.to_screen(rect, x0, y0), app.map.to_screen(rect, x0 + tile_cm, y0 + tile_cm));
                if !r.intersects(rect) {
                    continue;
                }
                if let Some(uri) = app.user.assets.as_mut().and_then(|a| a.map_tile_uri(z, tx, ty)) {
                    egui::Image::new(uri).paint_at(&canvas, r);
                }
            }
        }
    }

    let state = &mut app.map;
    // Belts and pipes.
    if let Some(w) = &state.world {
        let view = {
            let a = state.to_world(rect, rect.min);
            let b = state.to_world(rect, rect.max);
            [a.0 as f32, a.1 as f32, b.0 as f32, b.1 as f32]
        };
        let width = ((state.scale * 60.0) as f32).clamp(0.8, 3.0);
        for (c, b) in w.carriers.iter().zip(&state.carrier_boxes) {
            if b[2] < view[0] || b[0] > view[2] || b[3] < view[1] || b[1] > view[3] {
                continue;
            }
            let pts: Vec<Pos2> = c.points.iter().map(|p| state.to_screen(rect, p[0] as f64, p[1] as f64)).collect();
            painter.add(egui::Shape::line(pts, Stroke::new(width, if c.fluid { PIPE } else { BELT })));
        }
    }

    // Resource nodes.
    let occupied: BTreeSet<&str> = state.world.as_ref().map(|w| w.machines.iter().filter_map(|m| m.node.as_deref()).collect()).unwrap_or_default();
    let node_px = ((state.scale * 1800.0) as f32).clamp(10.0, 26.0);
    let mut hover: Option<String> = None;
    let pointer = resp.hover_pos();
    for n in &info.nodes {
        let p = state.to_screen(rect, n.x, n.y);
        if !rect.contains(p) {
            continue;
        }
        let r = Rect::from_center_size(p, vec2(node_px, node_px));
        painter.circle_filled(p, node_px * 0.62, CONTENT);
        let ring = if occupied.contains(n.id.as_str()) { ACCENT } else { DIVIDER };
        painter.circle_stroke(p, node_px * 0.62, Stroke::new(1.5_f32, ring));
        if let Some(res) = &n.resource {
            if let Some(uri) = app.user.assets.as_mut().and_then(|a| a.item_uri(res)) {
                egui::Image::new(uri).paint_at(&canvas, r.shrink(node_px * 0.12));
            }
        }
        if pointer.is_some_and(|q| q.distance(p) < node_px * 0.7) {
            let game = &app.user.game;
            let what = match n.kind {
                SiteKind::Geyser => "Geyser".to_string(),
                _ => n.resource.as_deref().map(|r| game.item_name(r).to_string()).unwrap_or_default(),
            };
            let purity = match n.purity {
                Purity::Impure => "Impure",
                Purity::Normal => "Normal",
                Purity::Pure => "Pure",
            };
            let kind = match n.kind {
                SiteKind::Node | SiteKind::Geyser => "node",
                SiteKind::WellSatellite => "Resource Well node",
                SiteKind::WellCore => "Resource Well",
            };
            let taken = if occupied.contains(n.id.as_str()) { " · has an extractor" } else { "" };
            hover = Some(format!("{what} · {purity} {kind}{taken}"));
        }
    }

    // Machines.
    let state = &mut app.map;
    let drag_rect = state.drag_from.zip(pointer).map(|(a, b)| Rect::from_two_pos(a, b));
    if let Some(w) = &state.world {
        let size = ((state.scale * MACHINE_CM) as f32).clamp(3.0, 28.0);
        for s in &w.structures {
            let p = state.to_screen(rect, s.x as f64, s.y as f64);
            if rect.contains(p) && size > 6.0 {
                painter.rect_filled(Rect::from_center_size(p, vec2(size * 0.5, size * 0.5)), 1.0, DIVIDER);
            }
        }
        for m in &w.machines {
            let p = state.to_screen(rect, m.x as f64, m.y as f64);
            if !rect.contains(p) {
                continue;
            }
            let selected = state.selected.contains(&m.id) || drag_rect.is_some_and(|d| d.contains(p));
            let r = Rect::from_center_size(p, vec2(size, size));
            if size >= 12.0 {
                painter.rect_filled(r.expand(2.0), 2.0, if selected { ACCENT } else { CONTENT });
                let icon = m.class.strip_prefix("Build_").map(|s| format!("Desc_{s}")).unwrap_or_default();
                if let Some(uri) = app.user.assets.as_mut().and_then(|a| a.item_uri(&icon)) {
                    egui::Image::new(uri).paint_at(&canvas, r);
                }
            } else {
                painter.circle_filled(p, (size * 0.5).max(1.5), if selected { ACCENT } else { TEXT });
            }
        }
    }

    // Box selection.
    let state = &mut app.map;
    if let Some(d) = drag_rect {
        painter.rect_filled(d, 0.0, ACCENT.gamma_multiply(0.15));
        painter.rect_stroke(d, 0.0, Stroke::new(1.0_f32, ACCENT));
    }
    if resp.drag_stopped() {
        if let (Some(d), Some(w)) = (drag_rect, &state.world) {
            let additive = ui.input(|i| i.modifiers.shift);
            if !additive {
                state.selected.clear();
            }
            for m in &w.machines {
                if d.contains(state.to_screen(rect, m.x as f64, m.y as f64)) {
                    state.selected.insert(m.id);
                }
            }
        }
        state.drag_from = None;
    }
    if resp.clicked() && !ui.input(|i| i.modifiers.shift) {
        state.selected.clear();
    }
    if let Some(text) = hover {
        egui::show_tooltip_at_pointer(&ctx, ui.layer_id(), egui::Id::new("map_hover"), |ui| {
            ui.label(text);
        });
    }
}

fn theme_font(size: f32) -> egui::FontId {
    crate::theme::semibold(size)
}
