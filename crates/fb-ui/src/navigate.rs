//! Getting around big graphs: Find (Ctrl+F) and the minimap.
//!
//! Find looks through the open graph's nodes (their names, machines and
//! items) and comment frames, highlights every match and steps through them
//! one at a time. The minimap in the canvas's corner shows the whole graph
//! and the part on screen; click or drag on it to move the view.

use crate::app::FactoryApp;
use crate::graph::NodeKind;
use crate::theme::*;
use egui::{pos2, vec2, Color32, CursorIcon, Key, Pos2, Rect, RichText, Sense, Stroke};
use egui_node_graph2::NodeId;

const MAP_W: f32 = 220.0;
const MAP_H: f32 = 150.0;
const MAP_MARGIN: f32 = 12.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Node(NodeId),
    Frame(u32),
}

#[derive(Default)]
pub struct FindState {
    pub query: String,
    /// Which match is shown.
    pub current: usize,
    /// The query the current match belongs to.
    shown_for: String,
    focused: bool,
}

impl FindState {
    pub fn new(query: &str) -> Self {
        FindState { query: query.to_string(), ..Default::default() }
    }
}

impl FactoryApp {
    /// Everything a node can be found by: its name, its machine, its items.
    fn search_text(&self, id: NodeId) -> String {
        let g = &self.state.graph;
        let game = &self.user.game;
        let n = &g[id];
        let mut parts = vec![n.label.clone()];
        match &n.user_data.kind {
            NodeKind::Recipe { recipe } => {
                if let Some(r) = game.recipes.get(recipe) {
                    parts.push(r.name.clone());
                    if let Some(b) = game.buildings.get(&r.building) {
                        parts.push(b.name.clone());
                    }
                }
            }
            NodeKind::Resource { .. } => parts.push("Resource".into()),
            NodeKind::Input { .. } => parts.push("Input".into()),
            NodeKind::Output { .. } => parts.push("Output".into()),
            NodeKind::Sink { .. } => parts.push("Sink".into()),
            NodeKind::Factory { .. } => parts.push("Factory".into()),
            NodeKind::Pole { .. } => parts.push("Power pole".into()),
            NodeKind::Extractor { item, class, .. } => {
                parts.push(crate::extractors::find(game, class).map(|e| e.name.clone()).unwrap_or_default());
                parts.push(game.item_name(item).to_string());
            }
            k @ NodeKind::Pump { .. } => parts.push(crate::routers::name(game, k).into()),
            k @ (NodeKind::Merger { .. } | NodeKind::Splitter { .. }) => parts.push(crate::routers::name(game, k).into()),
        }
        for (_, i) in &n.inputs {
            if let crate::graph::PortValue::Item(item) = &g[*i].value {
                parts.push(game.item_name(item).to_string());
            }
        }
        for (_, o) in &n.outputs {
            parts.push(game.item_name(&g[*o].typ.0).to_string());
        }
        parts.join(" ").to_lowercase()
    }

    /// Matches for the query, top to bottom then left to right.
    pub(crate) fn find_matches(&self, query: &str) -> Vec<Hit> {
        let words: Vec<String> = query.split_whitespace().map(|w| w.to_lowercase()).collect();
        if words.is_empty() {
            return Vec::new();
        }
        let z = self.state.pan_zoom.zoom.max(0.01);
        let mut hits: Vec<(Pos2, Hit)> = Vec::new();
        for id in self.state.graph.iter_nodes() {
            let text = self.search_text(id);
            if words.iter().all(|w| text.contains(w.as_str())) {
                let p = self.state.node_positions.get(id).map(|p| (p.to_vec2() / z).to_pos2()).unwrap_or(Pos2::ZERO);
                hits.push((p, Hit::Node(id)));
            }
        }
        for f in &self.planner.frames {
            let t = f.title.to_lowercase();
            if words.iter().all(|w| t.contains(w.as_str())) {
                hits.push((pos2(f.x, f.y), Hit::Frame(f.id)));
            }
        }
        hits.sort_by(|a, b| a.0.y.total_cmp(&b.0.y).then(a.0.x.total_cmp(&b.0.x)));
        hits.into_iter().map(|(_, h)| h).collect()
    }

    pub(crate) fn open_find(&mut self) {
        let keep = self.find.as_ref().map(|f| f.query.clone()).unwrap_or_default();
        self.find = Some(FindState::new(&keep));
    }

    /// Centres the view on a match (and selects it, if it's a node).
    fn show_hit(&mut self, hit: Hit) {
        // Moving the view myself stops following someone's.
        self.room.following = None;
        match hit {
            Hit::Node(id) => {
                self.focus_node = Some(id);
            }
            Hit::Frame(fid) => {
                if let Some(f) = self.planner.frames.iter().find(|f| f.id == fid) {
                    let z = self.state.pan_zoom.zoom;
                    let c = f.rect().center();
                    self.state.pan_zoom.pan = self.editor_rect.size() / 2.0 - c.to_vec2() * z;
                    self.frames.selected = Some(fid);
                }
            }
        }
    }

    /// The Find bar over the canvas's top-right corner, and the outline on
    /// every match.
    pub(crate) fn find_ui(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let Some(mut find) = self.find.take() else { return };
        let matches = self.find_matches(&find.query);
        let mut assets = self.user.assets.take();
        let mut step: Option<i32> = None;
        let mut close = false;
        egui::Area::new(egui::Id::new("find_bar"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos2(rect.right() - 424.0 - MAP_MARGIN, rect.top() + MAP_MARGIN))
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(400.0);
                    ui.horizontal(|ui| {
                        let edit = ui.add(egui::TextEdit::singleline(&mut find.query).hint_text("Find nodes and frames").desired_width(170.0));
                        if !find.focused {
                            edit.request_focus();
                            find.focused = true;
                        }
                        if edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                            step = Some(if ui.input(|i| i.modifiers.shift) { -1 } else { 1 });
                            edit.request_focus();
                        }
                        let count = if find.query.trim().is_empty() {
                            String::new()
                        } else if matches.is_empty() {
                            "No matches".into()
                        } else {
                            format!("{} of {}", find.current.min(matches.len() - 1) + 1, matches.len())
                        };
                        let count_color = if matches.is_empty() && !find.query.trim().is_empty() { ACCENT } else { TEXT_DIM };
                        // Right to left, so the count never runs into the buttons.
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if let Some(a) = assets.as_mut() {
                                if crate::widgets::icon_button(ui, a, crate::assets::UiIcon::Close, "Close (Esc)").clicked() {
                                    close = true;
                                }
                            }
                            let any = !matches.is_empty();
                            if ui.add_enabled(any, egui::Button::new("⏷")).on_hover_text("Next (Enter)").clicked() {
                                step = Some(1);
                            }
                            if ui.add_enabled(any, egui::Button::new("⏶")).on_hover_text("Previous (Shift+Enter)").clicked() {
                                step = Some(-1);
                            }
                            ui.add_space(4.0);
                            ui.add(egui::Label::new(RichText::new(count).small().color(count_color)).truncate());
                        });
                    });
                });
            });
        self.user.assets = assets;
        if ui.input(|i| i.key_pressed(Key::Escape)) {
            close = true;
        }
        // A new query jumps to its first match.
        if find.query != find.shown_for {
            find.shown_for = find.query.clone();
            find.current = 0;
            if let Some(h) = matches.first() {
                self.show_hit(*h);
            }
        }
        if let (Some(s), false) = (step, matches.is_empty()) {
            let n = matches.len() as i32;
            find.current = ((find.current.min(matches.len() - 1) as i32 + s).rem_euclid(n)) as usize;
            self.show_hit(matches[find.current]);
        }
        // Every match outlined; the current one stronger.
        let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Middle, egui::Id::new("find_marks"))).with_clip_rect(rect);
        for (k, h) in matches.iter().enumerate() {
            let r = match h {
                Hit::Node(id) => self.node_rects.get(id).copied(),
                Hit::Frame(fid) => self.planner.frames.iter().find(|f| f.id == *fid).map(|f| {
                    let z = self.state.pan_zoom.zoom;
                    Rect::from_min_size(rect.min + self.state.pan_zoom.pan + f.rect().min.to_vec2() * z, f.rect().size() * z)
                }),
            };
            if let Some(r) = r {
                let current = k == find.current.min(matches.len() - 1);
                painter.rect_stroke(r.expand(3.0), 2.0, Stroke::new(if current { 3.0_f32 } else { 1.5_f32 }, if current { ACCENT } else { ACCENT.gamma_multiply(0.6) }));
            }
        }
        if !close {
            self.find = Some(find);
        }
    }

    /// The whole graph in miniature, bottom-right of the canvas.
    pub(crate) fn minimap_ui(&mut self, ui: &mut egui::Ui, rect: Rect) {
        if !self.minimap || self.state.graph.nodes.is_empty() || rect.width() < MAP_W * 2.0 || rect.height() < MAP_H * 2.0 {
            return;
        }
        let z = self.state.pan_zoom.zoom.max(0.01);
        let pan = self.state.pan_zoom.pan;
        let w = theme_node_width();
        // Everything in unzoomed graph units.
        let mut items: Vec<(Rect, Color32)> = Vec::new();
        for (id, p) in self.state.node_positions.iter() {
            if !self.state.graph.nodes.contains_key(id) {
                continue;
            }
            let h = self.node_heights.get(&id).copied().unwrap_or(200.0);
            let color = self.state.graph[id].user_data.color.map(|[r, g, b]| Color32::from_rgb(r, g, b)).unwrap_or(TEXT_DIM);
            items.push((Rect::from_min_size((p.to_vec2() / z).to_pos2(), vec2(w, h)), color));
        }
        let view = Rect::from_min_size(((-pan) / z).to_pos2(), rect.size() / z);
        let mut bounds = view;
        for (r, _) in &items {
            bounds = bounds.union(*r);
        }
        for f in &self.planner.frames {
            bounds = bounds.union(f.rect());
        }
        let map = Rect::from_min_size(rect.max - vec2(MAP_W + MAP_MARGIN, MAP_H + MAP_MARGIN), vec2(MAP_W, MAP_H));
        let inner = map.shrink(6.0);
        let k = (inner.width() / bounds.width()).min(inner.height() / bounds.height());
        let off = inner.center() - bounds.center().to_vec2() * k;
        let to_map = |g: Pos2| off + g.to_vec2() * k;
        let to_graph = |m: Pos2| ((m - off) / k).to_pos2();

        // While a wire or node is being dragged, it can be dropped through
        // the minimap onto what's under it.
        let pressed_elsewhere = ui.input(|i| i.pointer.any_down() && i.pointer.press_origin().is_some_and(|p| !map.contains(p)));
        let busy = self.state.connection_in_progress.is_some() || pressed_elsewhere;
        let sense = if busy { Sense::hover() } else { Sense::click_and_drag() };
        let resp = ui.interact(map, ui.id().with("minimap"), sense);
        let resp = if busy { resp } else { resp.on_hover_cursor(CursorIcon::Crosshair).on_hover_text("Click or drag to move the view") };
        if resp.is_pointer_button_down_on() {
            if let Some(p) = resp.interact_pointer_pos() {
                // Centre the view on the point under the pointer.
                let g = to_graph(p);
                self.state.pan_zoom.pan = rect.size() / 2.0 - g.to_vec2() * z;
                ui.ctx().request_repaint();
            }
        }
        let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Middle, egui::Id::new("minimap"))).with_clip_rect(rect);
        painter.rect_filled(map, 2.0, CONTENT.gamma_multiply(0.92));
        painter.rect_stroke(map, 2.0, Stroke::new(1.0_f32, DIVIDER));
        for f in &self.planner.frames {
            let r = Rect::from_min_max(to_map(f.rect().min), to_map(f.rect().max));
            painter.rect_filled(r, 0.0, f.color().gamma_multiply(0.25));
        }
        let marks: Vec<NodeId> = match &self.find {
            Some(fs) => self.find_matches(&fs.query).into_iter().filter_map(|h| if let Hit::Node(n) = h { Some(n) } else { None }).collect(),
            None => Vec::new(),
        };
        for ((r, c), id) in items.iter().zip(self.state.node_positions.iter().filter(|(id, _)| self.state.graph.nodes.contains_key(*id)).map(|(id, _)| id)) {
            let m = Rect::from_min_max(to_map(r.min), to_map(r.max));
            let fill = if marks.contains(&id) { ACCENT } else if self.state.selected_nodes.contains(&id) { TEXT } else { c.gamma_multiply(0.7) };
            painter.rect_filled(m, 0.0, fill);
        }
        let v = Rect::from_min_max(to_map(view.min), to_map(view.max)).intersect(map);
        painter.rect_stroke(v, 0.0, Stroke::new(1.5_f32, TEXT));
        #[cfg(test)]
        crate::widgets::TEST_BUTTONS.with(|b| b.borrow_mut().push(("Minimap".into(), map, true)));
    }
}

fn theme_node_width() -> f32 {
    crate::theme::graph_style().node_width
}
