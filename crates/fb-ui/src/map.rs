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
/// Behind the map image: the map art's own ocean colour (sampled from it).
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
    /// Top-down renders of buildings and belts (None until extracted).
    pub sprites: Option<fb_model::world::Sprites>,
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
    /// Self-test: zoom (px per cm) into the busiest part of the save and
    /// select it.
    pub selftest_zoom: Option<f64>,
    pub selftest_zoomed: bool,
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
        self.backend.call("map_sprites", json!({}), Msg::MapSprites);
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
            Msg::MapSprites(r) => self.map.sprites = r.ok(),
            Msg::SelftestSave(Ok(Some(path))) => {
                self.map.session = Some("Self-test".into());
                self.load_world(path);
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

    pub(crate) fn import_selection(&mut self, ctx: &egui::Context) {
        let Some(world) = self.map.world.clone() else { return };
        let game = self.user.game.clone();
        let (file, report) = crate::map_import::import(&world, &self.map.selected, &game, self.map.info.as_ref());
        if report.nodes == 0 {
            self.say(ctx, "Nothing in the selection can be planned (no machines with recipes or extractors).", true);
            return;
        }
        // A factory of its own in the project, opened in its tab.
        let mut file = file;
        file.name = Some("Imported factory".into());
        self.add_factory_file(ctx, &file);
        self.view = View::Graph;
        self.frame_all = true;
        let name = self.doc_name(self.active);
        let mut text = format!("Imported {} machines as {} nodes into “{name}”.", report.machines, report.nodes);
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
        // Right-hand controls first; the status takes what's left and
        // truncates (full text on hover) instead of running under them.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let n = app.map.selected.len();
            import = widgets::button_enabled(ui, "Import into planner", n > 0).on_hover_text("Adds the selected machines to the project as a new factory, one node per recipe.").clicked();
            if n > 0 {
                clear = widgets::button(ui, "Clear selection").clicked();
                ui.label(RichText::new(format!("{n} selected")).color(TEXT));
            } else if app.map.world.is_some() {
                ui.label(RichText::new("Drag on the map to select machines").small().color(TEXT_DIM));
            }
            ui.add_space(12.0);
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add(egui::Label::new(RichText::new(&status).small().color(if err { DANGER } else { TEXT_DIM })).truncate()).on_hover_text(&status);
            });
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
    // Self-test: centre on the median machine, zoomed in, with the machines
    // near it selected (so the screenshot shows marks and the selection).
    if let (Some(zoom), false) = (state.selftest_zoom, state.selftest_zoomed) {
        if let Some(w) = &state.world {
            if !w.machines.is_empty() {
                // The machine with the most others within 100 m.
                let busiest = w
                    .machines
                    .iter()
                    .max_by_key(|a| w.machines.iter().filter(|b| (a.x - b.x).abs() < 10_000.0 && (a.y - b.y).abs() < 10_000.0).count())
                    .unwrap();
                let (cx, cy) = (busiest.x as f64, busiest.y as f64);
                state.center = (cx, cy);
                state.scale = zoom;
                state.selected = w.machines.iter().filter(|m| ((m.x as f64 - cx).abs() < 4000.0) && ((m.y as f64 - cy).abs() < 3000.0)).map(|m| m.id).collect();
                state.selftest_zoomed = true;
            }
        }
    } else if state.selftest_zoom.is_none() && state.selftest_zoomed {
        state.fitted = false;
        state.selftest_zoomed = false;
        state.selected.clear();
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

    // Belts and pipes, under everything else.
    let ctx2 = ui.ctx().clone();
    let view = crate::map_draw::View { rect, center: app.map.center, scale: app.map.scale };
    let mut batch = crate::map_draw::Batch::default();
    if let (Some(w), Some(assets)) = (&app.map.world, app.user.assets.as_mut()) {
        let vw = {
            let a = app.map.to_world(rect, rect.min);
            let b = app.map.to_world(rect, rect.max);
            [a.0 as f32, a.1 as f32, b.0 as f32, b.1 as f32]
        };
        for (c, b) in w.carriers.iter().zip(&app.map.carrier_boxes) {
            if b[2] < vw[0] || b[0] > vw[2] || b[3] < vw[1] || b[1] > vw[3] {
                continue;
            }
            // Lifts have a top-down render of their own, placed like a building.
            let sprites = app.map.sprites.as_ref();
            if let Some(p) = c.points.first() {
                let mut unused = Vec::new();
                if crate::map_draw::building(&ctx2, assets, sprites, &mut batch, &mut unused, &view, &c.class, p[0], p[1], c.yaw, false) {
                    continue;
                }
            }
            crate::map_draw::carrier(&ctx2, assets, sprites, &mut batch, &painter, &view, c);
        }
    }
    std::mem::take(&mut batch).paint(&painter);
    let state = &mut app.map;

    // Resource nodes.
    let occupied: BTreeSet<&str> = state.world.as_ref().map(|w| w.machines.iter().filter_map(|m| m.node.as_deref()).collect()).unwrap_or_default();
    let node_px = ((state.scale * 1800.0) as f32).clamp(16.0, 28.0);
    // Resource node under the pointer: (title, icon, detail, extra).
    let mut hover: Option<(String, Option<String>, String, Option<&str>)> = None;
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
            // The save's overrides (1.2's randomized resources) come first.
            let over = state.world.as_ref().and_then(|w| w.node_overrides.iter().find(|o| o.id == n.id));
            let resource = over.and_then(|o| o.resource.clone()).or_else(|| n.resource.clone());
            let purity = over.and_then(|o| o.purity).unwrap_or(n.purity);
            let title = match n.kind {
                SiteKind::Geyser => "Geyser".to_string(),
                _ => resource.as_deref().map(|r| game.item_name(r).to_string()).unwrap_or_default(),
            };
            let purity = match purity {
                Purity::Impure => "Impure",
                Purity::Normal => "Normal",
                Purity::Pure => "Pure",
            };
            // Pressurizer sites have no purity in the game data.
            let detail = match n.kind {
                SiteKind::Node | SiteKind::Geyser => format!("{purity} node"),
                SiteKind::WellSatellite => format!("{purity} Resource Well node"),
                SiteKind::WellCore => "Resource Well Pressurizer site".to_string(),
            };
            let taken = occupied.contains(n.id.as_str()).then_some("Has an extractor");
            hover = Some((title, resource, detail, taken));
        }
    }

    // Buildings: their renders at true size and facing, or icons and dots.
    let state = &mut app.map;
    let drag_rect = state.drag_from.zip(pointer).map(|(a, b)| Rect::from_two_pos(a, b));
    let mut outline = Vec::new();
    if let (Some(w), Some(assets)) = (&state.world, app.user.assets.as_mut()) {
        let sprites = state.sprites.as_ref();
        let size = ((state.scale * MACHINE_CM) as f32).clamp(3.0, 28.0);
        for s in &w.structures {
            let drawn = crate::map_draw::building(&ctx2, assets, sprites, &mut batch, &mut outline, &view, &s.class, s.x, s.y, s.yaw, false);
            let p = state.to_screen(rect, s.x as f64, s.y as f64);
            if !drawn && rect.contains(p) && size > 6.0 {
                painter.rect_filled(Rect::from_center_size(p, vec2(size * 0.5, size * 0.5)), 1.0, DIVIDER);
            }
        }
        let mut icons = Vec::new();
        // What each drawn machine makes (or mines), shown on top of its render.
        let mut makes: Vec<(Pos2, f32, String)> = Vec::new();
        for m in &w.machines {
            let p = state.to_screen(rect, m.x as f64, m.y as f64);
            let selected = state.selected.contains(&m.id) || drag_rect.is_some_and(|d| d.contains(p));
            if crate::map_draw::building(&ctx2, assets, sprites, &mut batch, &mut outline, &view, &m.class, m.x, m.y, m.yaw, selected) {
                let across = sprites.and_then(|sp| sp.buildings.get(&m.class)).map(|s| ((s.max_x - s.min_x).min(s.max_y - s.min_y) as f64 * state.scale) as f32).unwrap_or(0.0);
                if across >= 14.0 && rect.contains(p) {
                    if let Some(item) = made_by(&app.user.game, &info, w, m) {
                        makes.push((p, (across * 0.45).clamp(14.0, 40.0), item));
                    }
                }
                continue;
            }
            if rect.contains(p) {
                icons.push((m.class.clone(), p, selected));
            }
        }
        std::mem::take(&mut batch).paint(&painter);
        crate::map_draw::outlines(&painter, std::mem::take(&mut outline));
        for (p, px, item) in makes {
            let r = Rect::from_center_size(p, vec2(px, px));
            painter.rect_filled(r.expand(px * 0.12), px * 0.18, CONTENT.gamma_multiply(0.85));
            if let Some(uri) = assets.item_uri(&item) {
                egui::Image::new(uri).paint_at(&canvas, r);
            }
        }
        // No render (yet): the building's icon, or a dot when zoomed out.
        for (class, p, selected) in icons {
            let r = Rect::from_center_size(p, vec2(size, size));
            if size >= 12.0 {
                painter.rect_filled(r.expand(2.0), 2.0, if selected { ACCENT } else { CONTENT });
                let icon = class.strip_prefix("Build_").map(|s| format!("Desc_{s}")).unwrap_or_default();
                if let Some(uri) = assets.item_uri(&icon) {
                    egui::Image::new(uri).paint_at(&canvas, r);
                }
            } else {
                painter.circle_filled(p, (size * 0.5).max(1.5), if selected { ACCENT } else { TEXT });
            }
        }
    }

    // Power lines, over the buildings (they hang from pole tops). The circuit
    // under the pointer (a line, or a pole or switch) lights up.
    // (title, circuit, the line under the pointer)
    let mut power_hover: Option<(String, Option<u32>, Option<usize>)> = None;
    if let Some(w) = &app.map.world {
        let st = &app.map;
        let sprites = st.sprites.as_ref();
        let size = ((st.scale * MACHINE_CM) as f32).clamp(3.0, 28.0);
        if let Some(q) = pointer {
            let on = |s: &fb_model::world::Structure| match sprites.and_then(|sp| sp.buildings.get(&s.class)) {
                Some(sp) => crate::map_draw::contains(&crate::map_draw::corners(&view, sp, s.x, s.y, s.yaw), q),
                None => view.screen(s.x as f64, s.y as f64).distance(q) <= (size * 0.5).max(4.0),
            };
            // Poles, switches and batteries (stations and the like have a
            // tip of their own, with their power in it).
            if let Some(s) = w.structures.iter().rev().filter(|s| s.circuit.is_some() && s.power.is_none()).find(|s| on(s)) {
                let name = app.user.game.building_names.get(&s.class).cloned().unwrap_or_else(|| s.class.clone());
                power_hover = Some((name, s.circuit, None));
            } else if let Some(k) = w.power_lines.iter().position(|l| segment_distance(q, st.to_screen(rect, l.a[0] as f64, l.a[1] as f64), st.to_screen(rect, l.b[0] as f64, l.b[1] as f64)) < 5.0) {
                power_hover = Some(("Power Line".into(), w.power_lines[k].circuit, Some(k)));
            }
        }
        let lit = power_hover.as_ref().and_then(|h| h.1);
        let lit_line = power_hover.as_ref().and_then(|h| h.2);
        let mut labels: Vec<(Pos2, Pos2, String)> = Vec::new();
        let width = ((st.scale * 60.0) as f32).clamp(1.0, 3.0);
        let screen = rect.expand(4.0);
        for (k, l) in w.power_lines.iter().enumerate() {
            let (a, b) = (st.to_screen(rect, l.a[0] as f64, l.a[1] as f64), st.to_screen(rect, l.b[0] as f64, l.b[1] as f64));
            if !screen.intersects(Rect::from_two_pos(a, b)) {
                continue;
            }
            // What has to cross this section, toward the end that's short.
            // (Labels from 1 MW: idle machines draw 0.1 MW each and would bury
            // the rest; the tip shows every section.)
            if let Some((need, to_b)) = l.sides.and_then(crossing) {
                if a.distance(b) >= 110.0 && need >= 1.0 {
                    let (from, to) = if to_b { (a, b) } else { (b, a) };
                    labels.push((from, to, format!("{} MW", fmt_mw(need))));
                }
            }
            if lit_line == Some(k) {
                painter.line_segment([a, b], Stroke::new(width + 3.0, ACCENT));
            } else if lit.is_some() && l.circuit == lit {
                painter.line_segment([a, b], Stroke::new(width + 1.0, ACCENT.gamma_multiply(0.6)));
            } else {
                painter.line_segment([a, b], Stroke::new(width + 1.0, Color32::from_black_alpha(170)));
                painter.line_segment([a, b], Stroke::new((width - 0.5).max(0.75), Color32::from_gray(0xB0).gamma_multiply(0.8)));
            }
        }
        // Labels: an arrow toward where the power goes, and how much.
        for (from, to, text) in labels {
            let mid = from + (to - from) * 0.5;
            let dir = (to - from).normalized();
            let g = painter.layout_no_wrap(text, egui::FontId::proportional(11.0), TEXT);
            let w_ = g.size().x + 22.0;
            let chip = Rect::from_center_size(mid, vec2(w_, g.size().y + 6.0));
            painter.rect_filled(chip, 3.0, CONTENT.gamma_multiply(0.9));
            painter.rect_stroke(chip, 3.0, Stroke::new(1.0_f32, ACCENT.gamma_multiply(0.7)));
            let tip = pos2(chip.left() + 13.0, mid.y) + dir * 4.0;
            let back = pos2(chip.left() + 13.0, mid.y) - dir * 4.0;
            let side = vec2(-dir.y, dir.x) * 3.5;
            painter.add(egui::Shape::convex_polygon(vec![tip, back + side, back - side], ACCENT, Stroke::NONE));
            painter.galley(pos2(chip.left() + 19.0, mid.y - g.size().y / 2.0), g, TEXT);
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
    if let Some((title, icon, detail, extra)) = hover {
        let assets = app.user.assets.as_mut();
        egui::show_tooltip_at_pointer(&ctx, ui.layer_id(), egui::Id::new("map_hover"), |ui| {
            widgets::tooltip_title(ui, &title);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                if let (Some(a), Some(icon)) = (assets, &icon) {
                    a.item(ui, icon, 20.0);
                }
                widgets::tip_line(ui, RichText::new(&detail).color(TEXT));
            });
            if let Some(extra) = extra {
                widgets::tip_line(ui, RichText::new(extra).small().color(TEXT_DIM));
            }
        });
    } else if let (Some(q), Some(w), Some(assets)) = (pointer, app.map.world.as_ref(), app.user.assets.as_mut()) {
        // Machines (topmost first), then splitters, storage and the like.
        let view = crate::map_draw::View { rect, center: app.map.center, scale: app.map.scale };
        let sprites = app.map.sprites.as_ref();
        let size = ((app.map.scale * MACHINE_CM) as f32).clamp(3.0, 28.0);
        let under = |class: &str, x: f32, y: f32, yaw: f32| -> bool {
            match sprites.and_then(|sp| sp.buildings.get(class)) {
                Some(s) => crate::map_draw::contains(&crate::map_draw::corners(&view, s, x, y, yaw), q),
                None => view.screen(x as f64, y as f64).distance(q) <= (size * 0.5).max(4.0),
            }
        };
        let game = app.user.game.clone();
        if let Some(m) = w.machines.iter().rev().find(|m| under(&m.class, m.x, m.y, m.yaw)) {
            egui::show_tooltip_at_pointer(&ctx, ui.layer_id(), egui::Id::new("map_machine"), |ui| {
                machine_tip(ui, assets, &game, &info, w, m);
            });
        } else if let Some((title, circuit, line)) = power_hover {
            egui::show_tooltip_at_pointer(&ctx, ui.layer_id(), egui::Id::new("map_power"), |ui| {
                widgets::tooltip_title(ui, &title);
                if let Some(l) = line.and_then(|k| w.power_lines.get(k)) {
                    section_tip(ui, l);
                    ui.add_space(4.0);
                    widgets::tip_line(ui, RichText::new("Whole circuit").small().color(ACCENT));
                }
                circuit_tip(ui, assets, &game, circuit.and_then(|c| w.circuits.get(c as usize)));
            });
        } else if let Some(s) = w.structures.iter().rev().find(|s| under(&s.class, s.x, s.y, s.yaw)) {
            egui::show_tooltip_at_pointer(&ctx, ui.layer_id(), egui::Id::new("map_structure"), |ui| {
                widgets::tooltip_title(ui, game.building_names.get(&s.class).map(String::as_str).unwrap_or(&s.class));
                if let Some(u) = s.power {
                    power_section(ui, u, None, s.circuit.and_then(|c| w.circuits.get(c as usize)));
                }
            });
        }
    }
}

/// The item a machine makes (its recipe's main product) or mines (its node's
/// resource, or the save's override).
fn made_by(game: &fb_model::GameData, info: &MapInfo, w: &World, m: &fb_model::world::Machine) -> Option<String> {
    if let Some(r) = m.recipe.as_ref().and_then(|r| game.recipes.get(r)) {
        return r.icon_item().map(str::to_string);
    }
    let id = m.node.as_deref()?;
    w.node_overrides
        .iter()
        .find(|o| o.id == id)
        .and_then(|o| o.resource.clone())
        .or_else(|| info.nodes.iter().find(|n| n.id == id).and_then(|n| n.resource.clone()))
}

/// Hover tip for a machine on the map: what it is, what it makes and how
/// it's set up.
fn machine_tip(ui: &mut egui::Ui, assets: &mut crate::assets::Assets, game: &fb_model::GameData, info: &MapInfo, w: &World, m: &fb_model::world::Machine) {
    let name = game.building_names.get(&m.class).map(String::as_str).unwrap_or(&m.class);
    widgets::tooltip_title(ui, name);
    if let Some(r) = m.recipe.as_ref().and_then(|r| game.recipes.get(r)) {
        widgets::tip_line(ui, RichText::new(&r.name).color(TEXT));
        let k = m.clock as f64 * m.boost as f64;
        for a in &r.products {
            let amount = crate::fmt::fmt_rate_unit(r.per_min(a) * k, game.item_form(&a.item));
            widgets::tip_item(ui, assets, &a.item, game.item_name(&a.item), &amount);
        }
    } else if let Some(item) = made_by(game, info, w, m) {
        let purity = m.node.as_deref().and_then(|id| {
            w.node_overrides.iter().find(|o| o.id == id).and_then(|o| o.purity).or_else(|| info.nodes.iter().find(|n| n.id == id).map(|n| n.purity))
        });
        let on = match purity {
            Some(Purity::Impure) => "Impure node",
            Some(Purity::Normal) => "Normal node",
            Some(Purity::Pure) => "Pure node",
            None => "",
        };
        widgets::tip_item(ui, assets, &item, game.item_name(&item), on);
    } else {
        widgets::tip_line(ui, RichText::new("No recipe set").color(TEXT_DIM));
    }
    let mut setup = format!("{}% clock speed", crate::fmt::fmt_num(m.clock as f64 * 100.0));
    if m.boost > 1.0001 {
        setup += &format!(" · {}× output from Somersloops", crate::fmt::fmt_num(m.boost as f64));
    }
    widgets::tip_line(ui, RichText::new(setup).small().color(TEXT_DIM));
    let circuit = w.machine_circuit.get(&m.id).and_then(|c| w.circuits.get(*c as usize));
    match w.machine_power.get(&m.id) {
        Some(u) => power_section(ui, *u, Some(m.clock), circuit),
        // Machines that need power but aren't wired to anything.
        None if game.buildings.get(&m.class).is_some_and(|b| b.power_mw > 0.0) || game.extraction.extractors.iter().any(|x| x.class == m.class) => {
            ui.add_space(4.0);
            widgets::tip_line(ui, RichText::new("Power").small().color(ACCENT));
            widgets::tip_line(ui, RichText::new("Not connected to power").color(TEXT_DIM));
        }
        None => {}
    }
}

/// The Power part of a building's tip: what it draws (now, and running at
/// its clock speed) or makes, and its circuit.
fn power_section(ui: &mut egui::Ui, u: fb_model::world::PowerUse, clock: Option<f32>, circuit: Option<&fb_model::world::Circuit>) {
    ui.add_space(4.0);
    widgets::tip_line(ui, RichText::new("Power").small().color(ACCENT));
    let row = |ui: &mut egui::Ui, label: &str, mw: f32| {
        ui.horizontal(|ui| {
            widgets::tip_line(ui, RichText::new(label).color(TEXT_DIM));
            widgets::tip_line(ui, RichText::new(format!("{} MW", fmt_mw(mw))).color(TEXT));
        });
    };
    if u.makes > 0.0 || (u.max <= 0.0 && u.now <= 0.0) {
        row(ui, "Makes", u.makes);
    } else {
        row(ui, "Uses now", u.now);
        let running = match clock {
            Some(c) => format!("Running at {}%", crate::fmt::fmt_num(c as f64 * 100.0)),
            None => "Running".to_string(),
        };
        row(ui, &running, u.max);
    }
    match circuit {
        Some(c) => {
            let text = format!("Its circuit makes {} MW and uses {} MW", fmt_mw(c.production), fmt_mw(c.consumption));
            widgets::tip_line(ui, RichText::new(text).small().color(TEXT_DIM));
        }
        None => {
            widgets::tip_line(ui, RichText::new("Not connected to power").small().color(TEXT_DIM));
        }
    }
}

/// What has to cross a line that splits its circuit: MW, and whether it
/// goes toward end `b` (else toward `a`). None when nothing has to.
pub(crate) fn crossing(sides: [fb_model::world::PowerSide; 2]) -> Option<(f32, bool)> {
    let [a, b] = sides;
    let (short_a, short_b) = (a.uses - a.makes, b.uses - b.makes);
    if short_b > 0.05 && short_b >= short_a {
        Some((short_b, true))
    } else if short_a > 0.05 {
        Some((short_a, false))
    } else {
        None
    }
}

/// "north-east" for a direction on the map (screen up is north).
pub(crate) fn compass(dx: f32, dy: f32) -> &'static str {
    let a = (-dy).atan2(dx).to_degrees();
    const NAMES: [&str; 8] = ["east", "north-east", "north", "north-west", "west", "south-west", "south", "south-east"];
    NAMES[(((a + 360.0 + 22.5) % 360.0) / 45.0) as usize % 8]
}

/// A line's own part of its tip: what each end's side makes and uses, and
/// what has to come across this section.
fn section_tip(ui: &mut egui::Ui, l: &fb_model::world::PowerLine) {
    let Some(sides) = l.sides else {
        let text = if l.circuit.is_some() {
            "This section is part of a loop: power can go round either way, so how much goes through here can't be told."
        } else {
            "Not connected to anything."
        };
        ui.add(egui::Label::new(RichText::new(text).color(TEXT_DIM)).wrap());
        return;
    };
    let end_a = compass(l.a[0] - l.b[0], l.a[1] - l.b[1]);
    let end_b = compass(l.b[0] - l.a[0], l.b[1] - l.a[1]);
    let n = |b: u32| format!("{b} {}", if b == 1 { "building" } else { "buildings" });
    egui::Grid::new("section_sides").num_columns(4).spacing(vec2(12.0, 2.0)).show(ui, |ui| {
        for t in ["", "Makes", "Uses", ""] {
            widgets::tip_line(ui, RichText::new(t).small().color(TEXT_DIM));
        }
        ui.end_row();
        for (end, s) in [(end_a, sides[0]), (end_b, sides[1])] {
            widgets::tip_line(ui, RichText::new(format!("The {end} side")).color(TEXT_DIM));
            widgets::tip_line(ui, RichText::new(format!("{} MW", fmt_mw(s.makes))).color(TEXT));
            widgets::tip_line(ui, RichText::new(format!("{} MW", fmt_mw(s.uses))).color(TEXT));
            widgets::tip_line(ui, RichText::new(n(s.buildings)).small().color(TEXT_DIM));
            ui.end_row();
        }
    });
    let [a, b] = sides;
    let text = match crossing(sides) {
        _ if a.uses > a.makes + 0.05 && b.uses > b.makes + 0.05 => "Both sides use more than they make: the circuit doesn't make enough".to_string(),
        Some((mw, true)) => format!("{} MW has to come through here to the {end_b} side", fmt_mw(mw)),
        Some((mw, false)) => format!("{} MW has to come through here to the {end_a} side", fmt_mw(mw)),
        None => "Each side makes enough for itself: nothing has to come through here".to_string(),
    };
    widgets::tip_line(ui, RichText::new(text).color(ACCENT));
}

fn fmt_mw(v: f32) -> String {
    crate::fmt::fmt_num((v as f64 * 10.0).round() / 10.0)
}

/// Screen distance from `p` to the segment `a`-`b`.
fn segment_distance(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let t = if ab.length_sq() < 1e-6 { 0.0 } else { ((p - a).dot(ab) / ab.length_sq()).clamp(0.0, 1.0) };
    p.distance(a + ab * t)
}

/// Hover tip for a power line, pole, switch or battery: its circuit's
/// production, capacity and consumption, and where the power comes from.
fn circuit_tip(ui: &mut egui::Ui, assets: &mut crate::assets::Assets, game: &fb_model::GameData, c: Option<&fb_model::world::Circuit>) {
    let Some(c) = c.filter(|c| c.consumers > 0 || !c.sources.is_empty() || c.batteries > 0) else {
        widgets::tip_line(ui, RichText::new("Not connected to anything that makes or uses power").color(TEXT_DIM));
        return;
    };
    // All but Biomass Burners run at full output (the save's figures).
    let production = c.production;
    egui::Grid::new("circuit_numbers").num_columns(2).spacing(vec2(16.0, 2.0)).show(ui, |ui| {
        for (label, mw) in [("Production", production), ("Production capacity", c.capacity), ("Consumption", c.consumption), ("Max. consumption", c.max_consumption)] {
            widgets::tip_line(ui, RichText::new(label).color(TEXT_DIM));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                widgets::tip_line(ui, RichText::new(format!("{} MW", fmt_mw(mw))).color(TEXT));
            });
            ui.end_row();
        }
    });
    if c.consumption > c.production + 0.01 {
        let text = if c.stored_mwh > 0.0 { "Drawing more than the generators make: the batteries cover the rest" } else { "Drawing more than the generators make" };
        widgets::tip_line(ui, RichText::new(text).small().color(ACCENT));
    } else if c.max_consumption > c.capacity + 0.01 {
        widgets::tip_line(ui, RichText::new("Not enough capacity if everything runs at once").small().color(ACCENT));
    }
    if c.batteries > 0 {
        let text = format!("{} Power Storage · {} of {} MWh", c.batteries, fmt_mw(c.stored_mwh), fmt_mw(c.battery_mwh));
        widgets::tip_line(ui, RichText::new(text).small().color(TEXT_DIM));
    }
    ui.add_space(4.0);
    if c.sources.is_empty() {
        widgets::tip_line(ui, RichText::new("No generators on this circuit").small().color(TEXT_DIM));
    } else {
        widgets::tip_line(ui, RichText::new("Power from").small().color(TEXT_DIM));
        for s in &c.sources {
            let name = game.building_names.get(&s.class).cloned().unwrap_or_else(|| s.class.clone());
            let icon = s.class.strip_prefix("Build_").map(|x| format!("Desc_{x}")).unwrap_or_default();
            let amount = if s.mw > 0.0 { format!("{} MW", fmt_mw(s.mw)) } else { "not running".into() };
            widgets::tip_item(ui, assets, &icon, &format!("{} × {name}", s.count), &amount);
        }
    }
    let n = c.consumers;
    widgets::tip_line(ui, RichText::new(format!("{n} {} use power", if n == 1 { "building" } else { "buildings" })).small().color(TEXT_DIM));
}

fn theme_font(size: f32) -> egui::FontId {
    crate::theme::semibold(size)
}
