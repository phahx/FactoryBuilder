//! The Map tab's live factory tools (live_factory.rs): drawing a region,
//! making a live factory from the selection, each live factory's outline
//! and name on the map, and a right-click menu to open, redraw, rename or
//! delete one. Live factories are kept in the Live map settings, by index.

use crate::app::FactoryApp;
use crate::live_factory::members;
use fb_model::LiveFactoryDef;
use crate::theme::*;
use egui::{pos2, vec2, Color32, Pos2, Rect, Response, Shape, Stroke};

/// Default name for a new live factory.
const NEW_NAME: &str = "Live factory";
/// Screen pixels: how near the first corner a click closes the region.
const CLOSE_PX: f32 = 10.0;

/// Where a live factory is on the map: its region, or the box around its
/// buildings (world cm).
#[derive(Debug, Clone, PartialEq)]
pub enum Outline {
    Region(Vec<[f32; 2]>),
    Box([f32; 4]),
}

impl Outline {
    fn bounds(&self) -> [f32; 4] {
        match self {
            Outline::Box(b) => *b,
            Outline::Region(pts) => pts.iter().fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |b, p| [b[0].min(p[0]), b[1].min(p[1]), b[2].max(p[0]), b[3].max(p[1])]),
        }
    }
    fn contains(&self, p: [f32; 2]) -> bool {
        match self {
            Outline::Region(pts) => crate::live_factory::inside(pts, p),
            Outline::Box(b) => p[0] >= b[0] && p[0] <= b[2] && p[1] >= b[1] && p[1] <= b[3],
        }
    }
}

/// A region being drawn: its corners so far, and the live factory it
/// redraws (its index; None: a new one).
#[derive(Debug, Clone, Default)]
pub struct Drawing {
    pub corners: Vec<[f32; 2]>,
    pub redraw: Option<usize>,
}

/// The Map tab's live factory state.
#[derive(Default)]
pub struct LiveMapState {
    pub drawing: Option<Drawing>,
    /// Storage selected (structure indexes), with the machines.
    pub selected_stores: std::collections::BTreeSet<u32>,
    /// Where the map was right-clicked (world cm).
    menu_at: Option<[f32; 2]>,
    /// Outlines of the live factories (index, name, where, colour).
    pub outlines: Vec<(usize, String, Outline, Option<[u8; 3]>)>,
    /// The custom colour being mixed in the right-click menu.
    custom: Option<[u8; 3]>,
}

/// A box around some buildings, with a margin (cm).
fn box_around(points: impl Iterator<Item = [f32; 2]>) -> Option<[f32; 4]> {
    let b = points.fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |b, p| [b[0].min(p[0]), b[1].min(p[1]), b[2].max(p[0]), b[3].max(p[1])]);
    (b[0] <= b[2]).then(|| [b[0] - 1500.0, b[1] - 1500.0, b[2] + 1500.0, b[3] + 1500.0])
}

impl FactoryApp {
    /// Works out where each live factory is on the map.
    pub(crate) fn live_outlines_refresh(&mut self) {
        self.adopt_live_defs();
        let world = self.live_world();
        let mut out = Vec::new();
        // Only the live factories of the session on the map (indexes stay
        // the settings' own).
        for (k, def) in self.live_defs().into_iter().enumerate().filter(|(_, d)| Some(&d.world) == world.as_ref()) {
            if def.region.len() >= 3 {
                out.push((k, def.name.clone(), Outline::Region(def.region.clone()), def.color));
            } else if let Some(w) = &self.map.world {
                let (ms, ss) = members(&def, w);
                let pts = ms.iter().filter_map(|i| w.machines.get(*i as usize)).map(|m| [m.x, m.y]).chain(ss.iter().filter_map(|k| w.structures.get(*k as usize)).map(|s| [s.x, s.y]));
                if let Some(b) = box_around(pts) {
                    out.push((k, def.name.clone(), Outline::Box(b), def.color));
                }
            }
        }
        self.live_map.outlines = out;
    }

    /// Shows live factory `k` on the Map tab.
    pub(crate) fn show_live_on_map(&mut self, k: usize) {
        self.view = crate::map::View::Map;
        self.live_outlines_refresh();
        let Some(o) = self.live_map.outlines.iter().find(|(i, _, _, _)| *i == k).map(|(_, _, o, _)| o.clone()) else { return };
        let b = o.bounds();
        let size = self.map.rect.map(|r| r.size()).unwrap_or(vec2(800.0, 600.0));
        let (w, h) = (((b[2] - b[0]) as f64).max(2000.0), ((b[3] - b[1]) as f64).max(2000.0));
        let scale = (size.x as f64 / w).min(size.y as f64 / h) * 0.8;
        self.map.look_at(((b[0] + b[2]) / 2.0) as f64, ((b[1] + b[3]) / 2.0) as f64, scale.clamp(1e-4, 0.5));
    }

    /// Starts drawing a region (for a new live factory, or to redraw one).
    pub(crate) fn start_region(&mut self, redraw: Option<usize>) {
        self.view = crate::map::View::Map;
        self.live_map.drawing = Some(Drawing { corners: Vec::new(), redraw });
    }

    /// Makes a live factory of the selected machines and storage (after
    /// asking its name).
    pub(crate) fn live_from_selection(&mut self) {
        let Some(w) = &self.map.world else { return };
        let buildings = crate::live_factory::group_of(w, &self.map.selected, &self.live_map.selected_stores);
        if buildings.is_empty() {
            return;
        }
        let def = LiveFactoryDef { buildings, ..Default::default() };
        self.dialog = crate::dialogs::Dialog::NewLiveFactory { name: self.unique_live_name(NEW_NAME), def };
    }

    fn finish_region(&mut self) {
        let Some(d) = self.live_map.drawing.take() else { return };
        if d.corners.len() < 3 {
            return;
        }
        match d.redraw {
            Some(k) => {
                let mut defs = self.live_defs();
                if let Some(def) = defs.get_mut(k) {
                    def.region = d.corners;
                    def.buildings.clear();
                }
                self.set_live_defs(defs);
                self.live_outlines_refresh();
            }
            None => self.dialog = crate::dialogs::Dialog::NewLiveFactory { name: self.unique_live_name(NEW_NAME), def: LiveFactoryDef { region: d.corners, ..Default::default() } },
        }
    }
}

/// The Map tab's live factory layer: outlines and names, the region being
/// drawn, selected storage, and the right-click menu. Returns whether a
/// region is being drawn (the map's own box selection then stands aside).
pub fn map_layer(app: &mut FactoryApp, ui: &mut egui::Ui, resp: &Response, rect: Rect, painter: &egui::Painter) -> bool {
    let to_screen = |app: &FactoryApp, p: [f32; 2]| app.map.screen_of(rect, p[0] as f64, p[1] as f64);
    let pointer = resp.hover_pos();
    let at_world = |app: &FactoryApp, p: Pos2| {
        let w = app.map.world_of(rect, p);
        [w.0 as f32, w.1 as f32]
    };
    if app.map.version != app.live_map_seen || app.live_map.outlines.len() != app.world_defs().len() {
        app.live_map_seen = app.map.version;
        app.live_outlines_refresh();
    }

    // Live factories: outline and name.
    let outlines = app.live_map.outlines.clone();
    for (_, name, o, color) in &outlines {
        let name = name.clone();
        let tint = color.map(|[r, g, b]| Color32::from_rgb(r, g, b)).unwrap_or(ACCENT);
        let (pts, top_left): (Vec<Pos2>, Pos2) = match o {
            Outline::Region(c) => {
                let pts: Vec<Pos2> = c.iter().map(|p| to_screen(app, *p)).collect();
                let tl = pts.iter().fold(pos2(f32::MAX, f32::MAX), |a, p| pos2(a.x.min(p.x), a.y.min(p.y)));
                (pts, tl)
            }
            Outline::Box(b) => {
                let (a, c) = (to_screen(app, [b[0], b[1]]), to_screen(app, [b[2], b[3]]));
                (vec![a, pos2(c.x, a.y), c, pos2(a.x, c.y)], a)
            }
        };
        if !Rect::from_points(&pts).intersects(rect) {
            continue;
        }
        if let Outline::Region(_) = o {
            // A light wash, then the edge.
            if pts.len() >= 3 {
                let mesh = fan(&pts, tint.gamma_multiply(0.1));
                painter.add(Shape::mesh(mesh));
            }
        }
        let mut closed = pts.clone();
        closed.push(pts[0]);
        painter.add(Shape::line(closed, Stroke::new(2.0_f32, tint)));
        let g = painter.layout_no_wrap(name, egui::FontId::proportional(13.0), TEXT);
        let chip = Rect::from_min_size(top_left + vec2(0.0, -g.size().y - 8.0), g.size() + vec2(12.0, 6.0));
        painter.rect_filled(chip, 3.0, CONTENT.gamma_multiply(0.92));
        painter.rect_stroke(chip, 3.0, Stroke::new(1.0_f32, tint));
        painter.galley(chip.min + vec2(6.0, 3.0), g, TEXT);
    }

    // Storage in the selection.
    if let Some(w) = &app.map.world {
        for k in &app.live_map.selected_stores {
            if let Some(s) = w.structures.get(*k as usize) {
                let p = to_screen(app, [s.x, s.y]);
                if rect.contains(p) {
                    painter.circle_stroke(p, 9.0, Stroke::new(2.5_f32, ACCENT));
                }
            }
        }
    }

    // Drawing a region: click for corners; double-click, Enter or the first
    // corner to finish; Esc or right-click to stop.
    let mut drawing = app.live_map.drawing.is_some();
    if drawing {
        let (enter, esc) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
        if esc || resp.secondary_clicked() {
            app.live_map.drawing = None;
            return true;
        }
        let corners: Vec<Pos2> = app.live_map.drawing.as_ref().map(|d| d.corners.iter().map(|p| to_screen(app, *p)).collect()).unwrap_or_default();
        if resp.double_clicked() || enter {
            app.finish_region();
            drawing = false;
        } else if resp.clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                if corners.len() >= 3 && corners[0].distance(p) <= CLOSE_PX {
                    app.finish_region();
                    drawing = false;
                } else {
                    let wp = at_world(app, p);
                    if let Some(d) = app.live_map.drawing.as_mut() {
                        d.corners.push(wp);
                    }
                }
            }
        }
        if let Some(d) = &app.live_map.drawing {
            let mut pts: Vec<Pos2> = d.corners.iter().map(|p| to_screen(app, *p)).collect();
            if pts.len() >= 3 {
                painter.add(Shape::mesh(fan(&pts, ACCENT.gamma_multiply(0.12))));
            }
            if let Some(q) = pointer {
                pts.push(q);
            }
            if pts.len() >= 2 {
                painter.add(Shape::line(pts.clone(), Stroke::new(2.0_f32, ACCENT)));
            }
            for (k, p) in pts.iter().enumerate().take(d.corners.len()) {
                let r = if k == 0 && d.corners.len() >= 3 { CLOSE_PX * 0.6 } else { 4.0 };
                painter.circle(*p, r, ACCENT, Stroke::new(1.5_f32, Color32::from_black_alpha(200)));
            }
        }
        return drawing;
    }

    // Right-click menu.
    if resp.secondary_clicked() {
        app.live_map.menu_at = resp.interact_pointer_pos().map(|p| at_world(app, p));
    }
    let menu_at = app.live_map.menu_at;
    let under: Option<(usize, String, bool, Option<[u8; 3]>)> = menu_at.and_then(|p| app.live_map.outlines.iter().rev().find(|(_, _, o, _)| o.contains(p)).map(|(k, n, o, c)| (*k, n.clone(), matches!(o, Outline::Region(_)), *c)));
    let mut assets = app.user.assets.take();
    let mut custom = app.live_map.custom;
    let mut recolor: Option<(usize, Option<[u8; 3]>)> = None;
    let has_world = app.map.world.is_some();
    let picked = app.map.selected.len() + app.live_map.selected_stores.len();
    let mut action: Option<Box<dyn FnOnce(&mut FactoryApp)>> = None;
    resp.context_menu(|ui| {
        if let Some((k, name, region, color)) = under {
            ui.label(egui::RichText::new(&name).color(TEXT_DIM));
            let open_name = name.clone();
            if ui.button("Open its graph (Live)").clicked() {
                action = Some(Box::new(move |app: &mut FactoryApp| app.open_live(&open_name)));
                ui.close_menu();
            }
            if region && ui.button("Redraw its region").clicked() {
                action = Some(Box::new(move |app: &mut FactoryApp| app.start_region(Some(k))));
                ui.close_menu();
            }
            let rename = name.clone();
            if ui.button("Rename…").clicked() {
                action = Some(Box::new(move |app: &mut FactoryApp| app.dialog = crate::dialogs::Dialog::RenameLiveFactory { index: k, name: rename }));
                ui.close_menu();
            }
            if ui.button("Delete…").clicked() {
                action = Some(Box::new(move |app: &mut FactoryApp| app.dialog = crate::dialogs::Dialog::DeleteLiveFactory { index: k }));
                ui.close_menu();
            }
            // Its colour, as a node's is chosen.
            let mut swatches = Vec::new();
            if let Some(c) = crate::menus::colours(ui, assets.as_mut(), color, &mut custom, &mut swatches) {
                recolor = Some((k, c));
            }
            ui.separator();
        }
        if ui.add_enabled(has_world, egui::Button::new("Draw factory region")).on_hover_text("Click the map for its corners; everything inside is a live factory, including what's built there later.").clicked() {
            action = Some(Box::new(|app: &mut FactoryApp| app.start_region(None)));
            ui.close_menu();
        }
        let label = if picked > 0 { format!("Make live factory from selection ({picked})…") } else { "Make live factory from selection…".to_string() };
        if ui.add_enabled(picked > 0, egui::Button::new(label)).on_disabled_hover_text("Drag on the map to select machines and storage first.").clicked() {
            action = Some(Box::new(|app: &mut FactoryApp| app.live_from_selection()));
            ui.close_menu();
        }
    });
    app.user.assets = assets;
    app.live_map.custom = custom;
    if let Some((k, c)) = recolor {
        let mut defs = app.live_defs();
        if let Some(d) = defs.get_mut(k) {
            d.color = c;
        }
        app.set_live_defs(defs);
        app.live_outlines_refresh();
    }
    if let Some(a) = action {
        a(app);
    }
    false
}

/// A polygon filled (ear clipping, so concave regions fill only inside).
fn fan(pts: &[Pos2], color: Color32) -> egui::Mesh {
    let mut mesh = egui::Mesh::default();
    let n = pts.len();
    if n < 3 {
        return mesh;
    }
    for p in pts {
        mesh.colored_vertex(*p, color);
    }
    let cross = |o: Pos2, a: Pos2, b: Pos2| (a.x - o.x) * (b.y - o.y) - (a.y - o.y) * (b.x - o.x);
    let area: f32 = (0..n).map(|i| cross(Pos2::ZERO, pts[i], pts[(i + 1) % n])).sum();
    let turn = if area >= 0.0 { 1.0 } else { -1.0 };
    let mut left: Vec<usize> = (0..n).collect();
    let mut guard = 0;
    while left.len() > 3 && guard < n * n {
        guard += 1;
        let m = left.len();
        let ear = (0..m).find(|&i| {
            let (a, b, c) = (pts[left[(i + m - 1) % m]], pts[left[i]], pts[left[(i + 1) % m]]);
            if cross(a, b, c) * turn <= 0.0 {
                return false;
            }
            !left.iter().any(|&j| {
                let p = pts[j];
                if p == a || p == b || p == c {
                    return false;
                }
                let (d1, d2, d3) = (cross(a, b, p) * turn, cross(b, c, p) * turn, cross(c, a, p) * turn);
                d1 >= 0.0 && d2 >= 0.0 && d3 >= 0.0
            })
        });
        let Some(i) = ear else { break };
        mesh.add_triangle(left[(i + m - 1) % m] as u32, left[i] as u32, left[(i + 1) % m] as u32);
        left.remove(i);
    }
    if left.len() == 3 {
        mesh.add_triangle(left[0] as u32, left[1] as u32, left[2] as u32);
    }
    mesh
}
