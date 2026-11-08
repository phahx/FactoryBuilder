//! Comment frames: titled, coloured boxes around groups of nodes, like the
//! comment boxes in Unreal's Blueprint editor.
//!
//! A frame lives with its graph (in the document's `Planner`, so undo, the
//! autosave, rooms and files all carry it) in unzoomed graph units. It's
//! drawn under the nodes. Dragging its title bar moves it with every node
//! and frame inside it; the corner handle resizes it; double-clicking the
//! title renames it; right-clicking the title bar opens its menu.

use crate::app::{FactoryApp, GAME_COLORS};
use crate::theme::{self, *};
use egui::{pos2, vec2, Color32, CursorIcon, Pos2, Rect, Sense, Shape, Stroke};
use egui_node_graph2::NodeId;
use serde::{Deserialize, Serialize};

/// Title bar height and padding around nodes, in graph units.
pub const TITLE_H: f32 = 40.0;
const PAD: f32 = 30.0;
const HANDLE: f32 = 16.0;
const MIN_W: f32 = 160.0;
const MIN_H: f32 = 90.0;
/// Frames without a colour.
const PLAIN: Color32 = Color32::from_rgb(0x9A, 0x9A, 0x9A);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommentFrame {
    pub id: u32,
    pub title: String,
    #[serde(default)]
    pub color: Option<[u8; 3]>,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl CommentFrame {
    pub fn rect(&self) -> Rect {
        Rect::from_min_size(pos2(self.x, self.y), vec2(self.w, self.h))
    }
    pub fn color(&self) -> Color32 {
        self.color.map(|[r, g, b]| Color32::from_rgb(r, g, b)).unwrap_or(PLAIN)
    }
}

enum Drag {
    /// Moving a frame and what was inside it when the drag started.
    Move { nodes: Vec<NodeId>, frames: Vec<u32> },
    Resize,
}

/// What the frames are doing on screen (not saved).
#[derive(Default)]
pub struct FrameUi {
    drag: Option<(u32, Drag)>,
    /// Frame being renamed, and the text so far.
    pub editing: Option<(u32, String)>,
    /// Frame whose menu is open, and where.
    pub menu: Option<(u32, Pos2)>,
    pub selected: Option<u32>,
}

impl FactoryApp {
    fn to_screen(&self, rect: Rect, g: Pos2) -> Pos2 {
        rect.min + self.state.pan_zoom.pan + g.to_vec2() * self.state.pan_zoom.zoom
    }

    fn frame_screen_rect(&self, rect: Rect, f: &CommentFrame) -> Rect {
        let z = self.state.pan_zoom.zoom;
        Rect::from_min_size(self.to_screen(rect, pos2(f.x, f.y)), vec2(f.w, f.h) * z)
    }

    /// A frame around the selected nodes, ready to be named.
    pub(crate) fn add_frame_around_selection(&mut self) {
        let z = self.state.pan_zoom.zoom.max(0.01);
        let w = theme::graph_style().node_width;
        let mut b = Rect::NOTHING;
        for id in &self.state.selected_nodes {
            if let Some(p) = self.state.node_positions.get(*id) {
                let h = self.node_heights.get(id).copied().unwrap_or(200.0);
                b = b.union(Rect::from_min_size((p.to_vec2() / z).to_pos2(), vec2(w, h)));
            }
        }
        if !b.is_positive() {
            return;
        }
        let id = self.planner.frames.iter().map(|f| f.id).max().unwrap_or(0) + 1;
        self.planner.frames.push(CommentFrame {
            id,
            title: "Comment".into(),
            color: None,
            x: b.min.x - PAD,
            y: b.min.y - PAD - TITLE_H,
            w: b.width() + PAD * 2.0,
            h: b.height() + PAD * 2.0 + TITLE_H,
        });
        self.frames.selected = Some(id);
        self.frames.editing = Some((id, "Comment".into()));
        self.changed();
    }

    pub(crate) fn delete_frame(&mut self, id: u32) {
        self.planner.frames.retain(|f| f.id != id);
        if self.frames.selected == Some(id) {
            self.frames.selected = None;
        }
        self.changed();
    }

    /// Nodes and frames entirely inside frame `id` (on screen, this frame).
    fn frame_contents(&self, rect: Rect, id: u32) -> (Vec<NodeId>, Vec<u32>) {
        let Some(f) = self.planner.frames.iter().find(|f| f.id == id) else { return (Vec::new(), Vec::new()) };
        let outer = self.frame_screen_rect(rect, f).expand(1.0);
        let nodes = self.node_rects.iter().filter(|(_, r)| outer.contains_rect(**r)).map(|(n, _)| *n).collect();
        let inner = f.rect();
        let frames = self.planner.frames.iter().filter(|o| o.id != id && inner.contains_rect(o.rect())).map(|o| o.id).collect();
        (nodes, frames)
    }

    /// Reserves the place frames are painted (under the nodes); call before
    /// the graph is drawn.
    pub(crate) fn frames_slot(ui: &egui::Ui) -> egui::layers::ShapeIdx {
        ui.painter().add(Shape::Noop)
    }

    /// Paints the frames into `slot` and handles dragging, resizing and
    /// clicks. Call after the graph is drawn, so the title bars and handles
    /// win over the canvas behind them. Returns true if the pointer
    /// right-clicked a title bar (so the canvas menu shouldn't open).
    pub(crate) fn frames_ui(&mut self, ui: &mut egui::Ui, rect: Rect, slot: egui::layers::ShapeIdx) -> bool {
        let z = self.state.pan_zoom.zoom;
        let mut shapes = Vec::new();
        let mut menu_opened = false;
        let mut clicked_header = false;
        let title_h = TITLE_H * z;
        let font = theme::semibold((18.0 * z).clamp(9.0, 40.0));
        for f in self.planner.frames.clone() {
            let r = self.frame_screen_rect(rect, &f);
            if !r.intersects(rect) {
                continue;
            }
            let c = f.color();
            let header = Rect::from_min_size(r.min, vec2(r.width(), title_h.min(r.height())));
            shapes.push(Shape::rect_filled(r, 0.0, c.gamma_multiply(0.12)));
            shapes.push(Shape::rect_filled(header, 0.0, c.gamma_multiply(0.55)));
            let selected = self.frames.selected == Some(f.id);
            let stroke = if selected { Stroke::new(2.0_f32, ACCENT) } else { Stroke::new(1.0_f32, c.gamma_multiply(0.7)) };
            shapes.push(Shape::rect_stroke(r, 0.0, stroke));
            if self.frames.editing.as_ref().map(|e| e.0) != Some(f.id) {
                let mut job = egui::text::LayoutJob::simple_singleline(f.title.clone(), font.clone(), TEXT);
                job.wrap = egui::text::TextWrapping::truncate_at_width((header.width() - 20.0 * z).max(0.0));
                let g = ui.painter().layout_job(job);
                shapes.push(Shape::galley(pos2(header.left() + 10.0 * z, header.center().y - g.size().y / 2.0), g, TEXT));
            }
            let handle = Rect::from_min_max(r.max - vec2(HANDLE, HANDLE), r.max);
            for k in 1..=3 {
                let d = k as f32 * 4.0;
                shapes.push(Shape::line_segment([pos2(r.max.x - d, r.max.y - 2.0), pos2(r.max.x - 2.0, r.max.y - d)], Stroke::new(1.0_f32, c.gamma_multiply(0.8))));
            }

            // Title bar: drag to move, double-click to rename, right-click for
            // the menu. A node lying over it gets the pointer instead.
            let id = ui.id().with(("comment_frame", f.id));
            let pointer = ui.input(|i| i.pointer.hover_pos());
            let under_node = pointer.is_some_and(|p| self.node_rects.values().any(|r| r.contains(p)));
            let busy = self.state.connection_in_progress.is_some() || (self.frames.drag.is_none() && ui.input(|i| i.pointer.any_down() && i.pointer.press_origin().is_some_and(|p| !header.contains(p))));
            let sense = if under_node || busy { Sense::hover() } else { Sense::click_and_drag() };
            let hr = ui.interact(header.intersect(rect), id, sense);
            let hr = if sense == Sense::hover() { hr } else { hr.on_hover_cursor(CursorIcon::Grab) };
            #[cfg(test)]
            crate::widgets::TEST_BUTTONS.with(|b| b.borrow_mut().push((format!("Frame {}", f.title), header, true)));
            if hr.drag_started() {
                let (nodes, frames) = self.frame_contents(rect, f.id);
                self.frames.drag = Some((f.id, Drag::Move { nodes, frames }));
                self.frames.selected = Some(f.id);
                clicked_header = true;
            }
            if hr.clicked() {
                self.frames.selected = Some(f.id);
                self.state.selected_nodes.clear();
                clicked_header = true;
            }
            if hr.double_clicked() {
                self.frames.editing = Some((f.id, f.title.clone()));
            }
            if hr.secondary_clicked() {
                self.frames.menu = hr.interact_pointer_pos().map(|p| (f.id, p));
                self.frames.selected = Some(f.id);
                self.custom_color = None;
                menu_opened = true;
            }
            let rr = ui.interact(handle, id.with("resize"), if under_node || busy { Sense::hover() } else { Sense::drag() });
            let rr = if under_node || busy { rr } else { rr.on_hover_cursor(CursorIcon::ResizeNwSe) };
            if rr.drag_started() {
                self.frames.drag = Some((f.id, Drag::Resize));
            }
        }
        ui.painter().set(slot, Shape::Vec(shapes));

        // An ongoing drag.
        if let Some((id, drag)) = &self.frames.drag {
            let id = *id;
            let delta = ui.input(|i| i.pointer.delta());
            let released = !ui.input(|i| i.pointer.primary_down());
            if delta != egui::Vec2::ZERO {
                let d = delta / z.max(0.01);
                match drag {
                    Drag::Move { nodes, frames } => {
                        for n in nodes {
                            if let Some(p) = self.state.node_positions.get_mut(*n) {
                                *p += delta;
                            }
                        }
                        for f in self.planner.frames.iter_mut().filter(|f| f.id == id || frames.contains(&f.id)) {
                            f.x += d.x;
                            f.y += d.y;
                        }
                    }
                    Drag::Resize => {
                        if let Some(f) = self.planner.frames.iter_mut().find(|f| f.id == id) {
                            f.w = (f.w + d.x).max(MIN_W);
                            f.h = (f.h + d.y).max(MIN_H);
                        }
                    }
                }
                ui.ctx().request_repaint();
            }
            if released {
                self.frames.drag = None;
                self.changed();
            }
        }
        // A click elsewhere on the canvas drops the frame selection.
        if !clicked_header && ui.input(|i| i.pointer.primary_clicked()) && ui.input(|i| i.pointer.interact_pos()).is_some_and(|p| rect.contains(p)) && self.frames.menu.is_none() {
            self.frames.selected = None;
        }
        self.frame_title_editor(ui.ctx(), rect);
        menu_opened
    }

    /// Renaming a frame: a text field over its title.
    fn frame_title_editor(&mut self, ctx: &egui::Context, rect: Rect) {
        let Some((id, mut text)) = self.frames.editing.clone() else { return };
        let Some(f) = self.planner.frames.iter().find(|f| f.id == id).cloned() else {
            self.frames.editing = None;
            return;
        };
        let r = self.frame_screen_rect(rect, &f);
        let z = self.state.pan_zoom.zoom;
        let mut done = None;
        egui::Area::new(egui::Id::new(("frame_title", id))).order(egui::Order::Foreground).fixed_pos(r.min + vec2(6.0, 4.0) * z.max(0.5)).show(ctx, |ui| {
            let edit = ui.add(egui::TextEdit::singleline(&mut text).desired_width((r.width() - 16.0).clamp(120.0, 600.0)).hint_text("Comment"));
            if !edit.has_focus() && !edit.lost_focus() {
                edit.request_focus();
            }
            if edit.lost_focus() {
                done = Some(!ui.input(|i| i.key_pressed(egui::Key::Escape)));
            }
        });
        match done {
            Some(keep) => {
                self.frames.editing = None;
                let title = text.trim().to_string();
                if keep && !title.is_empty() && title != f.title {
                    if let Some(f) = self.planner.frames.iter_mut().find(|f| f.id == id) {
                        f.title = title;
                    }
                    self.changed();
                }
            }
            None => self.frames.editing = Some((id, text)),
        }
    }

    /// The right-click menu of a frame's title bar.
    pub(crate) fn frame_menu(&mut self, ctx: &egui::Context) {
        let Some((id, pos)) = self.frames.menu else { return };
        let Some(f) = self.planner.frames.iter().find(|f| f.id == id).cloned() else {
            self.frames.menu = None;
            return;
        };
        let mut close = false;
        let (mut rename, mut delete, mut select) = (false, false, false);
        let mut pick: Option<Option<[u8; 3]>> = None;
        let area = egui::Area::new(egui::Id::new("frame_menu")).order(egui::Order::Foreground).fixed_pos(pos).show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_min_width(180.0);
                if ui.add(egui::Button::new("Rename").shortcut_text("Double-click")).clicked() {
                    rename = true;
                    close = true;
                }
                if ui.button("Select what's inside").clicked() {
                    select = true;
                    close = true;
                }
                if ui.button("Delete frame").on_hover_text("Removes the frame; what's inside stays.").clicked() {
                    delete = true;
                    close = true;
                }
                ui.separator();
                ui.label(egui::RichText::new("Colour").small().color(TEXT_DIM));
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    if crate::app::swatch(ui, self.user.assets.as_mut(), None, f.color.is_none(), "No colour").clicked() {
                        pick = Some(None);
                    }
                    for (name, rgb) in GAME_COLORS {
                        if crate::app::swatch(ui, self.user.assets.as_mut(), Some(rgb), f.color == Some(rgb), name).clicked() {
                            pick = Some(Some(rgb));
                        }
                    }
                });
                match self.custom_color {
                    None => {
                        if ui.button("Custom colour…").clicked() {
                            self.custom_color = Some(f.color.unwrap_or([0xE5, 0x93, 0x44]));
                        }
                    }
                    Some(rgb) => {
                        let mut c = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                        if egui::color_picker::color_picker_color32(ui, &mut c, egui::color_picker::Alpha::Opaque) {
                            self.custom_color = Some([c.r(), c.g(), c.b()]);
                            pick = Some(Some([c.r(), c.g(), c.b()]));
                        }
                    }
                }
            });
        });
        if let Some(color) = pick {
            if let Some(f) = self.planner.frames.iter_mut().find(|f| f.id == id) {
                f.color = color;
            }
            self.changed();
        }
        let clicked_elsewhere = ctx.input(|i| i.pointer.any_pressed()) && !area.response.contains_pointer();
        if close || clicked_elsewhere || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.frames.menu = None;
        }
        if rename {
            self.frames.editing = Some((id, f.title.clone()));
        }
        if select {
            let (nodes, _) = self.frame_contents(self.editor_rect, id);
            self.state.selected_nodes = nodes;
        }
        if delete {
            self.delete_frame(id);
        }
    }
}
