//! Room extras: pings (Alt+click), short messages (/), following someone's
//! view, and who has which nodes selected. The state lives in room.rs; this
//! is the logic and drawing, hooked into the app at a few points:
//! `room_keys` (keyboard shortcuts), `follow_doc` (start of the frame),
//! `follow_view` and `room_after_draw` (around the graph editor), `chat_box`
//! (after the panels), `paint_room_marks` (with the cursors) and
//! `room_section` (both overview panels).
//!
//! Everything that arrives is checked again with `Wire::from_peer` before
//! use: the host checks guests, but guests can't trust the host either.

use crate::app::FactoryApp;
use crate::backend::Msg;
use crate::dialogs::Dialog;
use crate::room::{paint_bubble, paint_name_tag, rgb, text_on, with_alpha, ChatDraft, ChatLine, PeerView, PingMark, RoomUi};
use crate::theme::{self, *};
use crate::widgets;
use egui::text::{LayoutJob, TextFormat};
use egui::{pos2, vec2, Align, FontId, Id, Key, Layout, Pos2, Rect, RichText, Sense, Stroke, Ui};
use egui_node_graph2::NodeId;
use fb_model::room::{clean_chat, NodeRef, Wire, MAX_CHAT, MAX_SELECTION, MAX_VIEW_ZOOM, MIN_VIEW_ZOOM};
use serde_json::json;
use std::collections::BTreeMap;

/// How long a ping shows.
pub const PING_SECS: f64 = 2.0;
/// How long a message shows beside its sender's cursor: long enough to
/// read, so longer the longer it is (3 s and 1 s per 12 characters, at
/// least 4 s; 13 s for the longest).
pub fn bubble_secs(text: &str) -> f64 {
    (3.0 + text.chars().count() as f64 / 12.0).max(4.0)
}
/// Messages kept in the overview's list.
pub const MESSAGES_KEPT: usize = 20;
/// Messages the overview shows (the newest), wrapped; with messages at
/// most MAX_CHAT long, the rest of the overview stays in view.
pub const MESSAGES_SHOWN: usize = 5;
/// Where I am goes out again this often, for people who just joined.
const PLACE_EVERY: f64 = 3.0;
/// Views and selections go out at most this often (about 10 a second).
const SEND_EVERY: f64 = 0.1;
/// Pings kept at once; the oldest go first (bounds what a flood can cost).
const PINGS_KEPT: usize = 32;
/// How far a ping's ring spreads, in screen points.
const PING_RADIUS: f32 = 40.0;
/// Width of the outline round nodes someone else has selected.
const OUTLINE: f32 = 2.0;
/// The follow bar's button padding and the gap round the button.
const BAR_PAD_Y: f32 = 3.0;
const BAR_MARGIN: f32 = 3.0;
/// Self-test screenshots freeze pings and bubbles at these ages.
const FROZEN_PING_AGE: f64 = 0.6;

/// A node id as it travels (see `fb_model::room::NodeRef`).
pub fn node_ref(id: NodeId) -> Option<NodeRef> {
    serde_json::to_value(id).ok().and_then(|v| serde_json::from_value(v).ok())
}

/// A node id from the network; it may not be a node of any graph here.
pub fn node_id(r: NodeRef) -> Option<NodeId> {
    serde_json::to_value(r).ok().and_then(|v| serde_json::from_value(v).ok())
}

fn sender(w: &Wire) -> Option<u32> {
    match w {
        Wire::Cursor { id, .. } | Wire::Ping { id, .. } | Wire::Chat { id, .. } | Wire::View { id, .. } | Wire::Selection { id, .. } | Wire::Place { id, .. } => Some(*id),
        _ => None,
    }
}

/// Should `current` go out now? Only when it differs from what was last
/// sent and the last send was long enough ago. When it differs but it's too
/// soon, a repaint is booked so the latest value still goes out.
fn due<T: PartialEq>(ctx: &egui::Context, last: &Option<(T, f64)>, current: &T, now: f64) -> bool {
    match last {
        Some((sent, _)) if sent == current => false,
        Some((_, t)) if now - t < SEND_EVERY => {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64((SEND_EVERY - (now - t)).max(0.0)));
            false
        }
        _ => true,
    }
}

/// Height of the "Following" bar: one row of button text plus the padding.
pub fn follow_bar_height(ctx: &egui::Context) -> f32 {
    let font = egui::TextStyle::Button.resolve(&ctx.style());
    ctx.fonts(|f| f.row_height(&font)) + 2.0 * (BAR_PAD_Y + BAR_MARGIN)
}

impl FactoryApp {
    fn room_send(&self, wire: Wire) {
        self.backend.call::<()>("room_send", json!({ "wire": wire }), |_| Msg::RoomEvents(Ok(Vec::new())));
    }

    fn graph_point(&self, rect: Rect, screen: Pos2) -> Pos2 {
        let z = self.state.pan_zoom.zoom.max(0.01);
        ((screen - rect.min - self.state.pan_zoom.pan) / z).to_pos2()
    }

    /// The bar across the top of the canvas while following someone.
    fn follow_bar_rect(&self, ctx: &egui::Context, rect: Rect) -> Option<Rect> {
        let id = self.room.following?;
        self.room.peers.contains_key(&id).then(|| Rect::from_min_size(rect.min, vec2(rect.width(), follow_bar_height(ctx))))
    }

    /// A cursor, ping, message, view or selection from someone else.
    pub(crate) fn peer_message(&mut self, ctx: &egui::Context, wire: Wire) {
        let Some(id) = sender(&wire) else { return };
        // Only from people in the room (so nothing here grows without bound).
        if id == self.room.me || !self.room.peers.contains_key(&id) {
            return;
        }
        let Some(wire) = wire.from_peer(id) else { return };
        let now = ctx.input(|i| i.time);
        match wire {
            Wire::Cursor { x, y, doc, .. } => {
                self.room.cursors.insert(id, pos2(x, y));
                // A cursor that moved to another graph jumps rather than glides.
                if self.room.cursor_doc.insert(id, doc) != Some(doc) {
                    self.room.cursor_shown.insert(id, pos2(x, y));
                }
            }
            Wire::Ping { x, y, doc, .. } => self.add_ping(id, pos2(x, y), doc, now),
            Wire::Chat { text, .. } => self.add_message(id, text, now),
            Wire::View { doc, x, y, zoom, .. } => {
                self.room.views.insert(id, PeerView { doc, center: pos2(x, y), zoom });
            }
            Wire::Selection { doc, nodes, .. } => {
                let ids = nodes.into_iter().filter_map(node_id).collect();
                self.room.selections.insert(id, (doc, ids));
            }
            Wire::Place { place, .. } => {
                self.room.places.insert(id, place);
            }
            _ => {}
        }
        ctx.request_repaint();
    }

    /// Tells the room where I am (when it changes, and now and then), and
    /// keeps the shared project's document names for saying where others are.
    pub(crate) fn send_place(&mut self, ctx: &egui::Context) {
        use fb_model::room::Place;
        if !self.room.active() {
            return;
        }
        let place = if self.view == crate::map::View::Map {
            Place::Map
        } else if self.live_view.on {
            Place::Live
        } else {
            Place::Graph { doc: self.active }
        };
        if !self.live_view.on {
            self.room.doc_names = self.docs.iter().map(|d| (d.id, if d.id == crate::project::MASTER { "the master graph".to_string() } else { d.name.clone() })).collect();
        }
        let me = self.room.me;
        self.room.places.insert(me, place);
        let now = ctx.input(|i| i.time);
        let stale = self.room.sent_place.as_ref().is_none_or(|(p, at)| *p != place || now - at >= PLACE_EVERY);
        if stale {
            self.room.sent_place = Some((place, now));
            self.room_send(Wire::Place { id: me, place });
        }
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(PLACE_EVERY));
    }

    pub(crate) fn add_ping(&mut self, id: u32, pos: Pos2, doc: u32, now: f64) {
        self.room.pings.push(PingMark { id, pos, doc, at: now });
        if self.room.pings.len() > PINGS_KEPT {
            self.room.pings.remove(0);
        }
    }

    pub(crate) fn add_message(&mut self, id: u32, text: String, now: f64) {
        let (name, color) = self.room.peers.get(&id).map(|p| (p.name.clone(), p.color)).unwrap_or_else(|| ("Someone".into(), [TEXT_DIM.r(), TEXT_DIM.g(), TEXT_DIM.b()]));
        self.room.bubbles.insert(id, (text.clone(), now));
        self.room.messages.push_back(ChatLine { id, name, color, text });
        while self.room.messages.len() > MESSAGES_KEPT {
            self.room.messages.pop_front();
        }
    }

    /// Start following `id`'s view, or stop if I already am.
    pub(crate) fn toggle_follow(&mut self, id: u32) {
        if self.room.following == Some(id) {
            self.room.following = None;
        } else if id != self.room.me && self.room.peers.contains_key(&id) {
            self.room.following = Some(id);
            self.view = crate::map::View::Graph;
        }
    }

    /// Room keys on the graph (not while typing): / opens a message box at
    /// the pointer; Esc stops following.
    pub(crate) fn room_keys(&mut self, ctx: &egui::Context) {
        if !self.room.active() {
            return;
        }
        let menus_open = self.finder.is_some() || self.context_menu.is_some();
        let slash = ctx.input(|i| {
            i.events.iter().any(|e| match e {
                egui::Event::Text(t) => t == "/",
                egui::Event::Key { key: Key::Slash, pressed: true, modifiers, .. } => !modifiers.command && !modifiers.alt,
                _ => false,
            })
        });
        if slash && !menus_open && self.room.chat.is_none() {
            // The box opens empty: the / that opened it isn't typed into it.
            ctx.input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Text(t) if t == "/") && !matches!(e, egui::Event::Key { key: Key::Slash, .. })));
            let canvas = self.editor_rect;
            let fallback = if canvas.is_positive() { canvas.center() } else { ctx.screen_rect().center() };
            let at = ctx.pointer_hover_pos().filter(|p| canvas.contains(*p)).unwrap_or(fallback);
            self.room.chat = Some(ChatDraft { at, text: String::new(), focus: true });
        }
        if self.room.following.is_some() && !menus_open && ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.room.following = None;
        }
    }

    /// Start of the frame: while following, open the document they're on.
    pub(crate) fn follow_doc(&mut self) {
        let Some(id) = self.room.following else { return };
        if !self.room.active() || !self.room.peers.contains_key(&id) {
            self.room.following = None;
            return;
        }
        if let Some(v) = self.room.views.get(&id) {
            if v.doc != self.active && self.switch_to.is_none() && self.doc(v.doc).is_some() {
                self.switch_to = Some(v.doc);
            }
        }
    }

    /// Before the graph is drawn: stop following if I moved the view or
    /// touched the canvas myself; otherwise take the leader's pan and zoom.
    pub(crate) fn follow_view(&mut self, ui: &Ui, rect: Rect) {
        let Some(id) = self.room.following else { return };
        let bar = self.follow_bar_rect(ui.ctx(), rect).unwrap_or(Rect::NOTHING);
        // Frame all, frame selection, "show this node" (issue rows).
        let reframed = self.frame_all || self.frame_selected || self.focus_node.is_some();
        let touched = ui.rect_contains_pointer(rect)
            && ui.input(|i| {
                let on_bar = i.pointer.interact_pos().is_some_and(|p| bar.contains(p));
                // Alt+click is a ping: it doesn't take the view back.
                let pressed = i.pointer.any_pressed() && !i.modifiers.alt;
                let wheel = i.raw_scroll_delta != egui::Vec2::ZERO || i.smooth_scroll_delta != egui::Vec2::ZERO || i.zoom_delta() != 1.0;
                !on_bar && (pressed || wheel)
            });
        if reframed || touched {
            self.room.following = None;
            return;
        }
        let Some(v) = self.room.views.get(&id).copied() else { return };
        if v.doc != self.active {
            return;
        }
        let z = self.state.pan_zoom.zoom;
        let target = v.zoom.clamp(MIN_VIEW_ZOOM, MAX_VIEW_ZOOM);
        if (target - z).abs() > 1e-5 && z > 0.0 {
            // Positions are stored zoomed (graph position x zoom).
            let k = target / z;
            for (_, p) in self.state.node_positions.iter_mut() {
                *p = (p.to_vec2() * k).to_pos2();
            }
            self.state.pan_zoom.zoom(ui.clip_rect(), ui.style(), k);
        }
        let z = self.state.pan_zoom.zoom;
        // Their view's centre in the middle of mine.
        self.state.pan_zoom.pan = rect.size() / 2.0 - v.center.to_vec2() * z;
    }

    /// After the graph is drawn: Alt+click pings, my view and selection go
    /// out, and the follow bar is drawn over the canvas.
    pub(crate) fn room_after_draw(&mut self, ui: &mut Ui, rect: Rect, selected_before: Vec<NodeId>) {
        if !self.room.active() {
            return;
        }
        let ctx = ui.ctx().clone();
        let now = ctx.input(|i| i.time);
        let bar = self.follow_bar_rect(&ctx, rect);
        // Alt+click on empty canvas: a ping where I clicked.
        if ui.input(|i| i.pointer.primary_clicked() && i.modifiers.alt) && ui.rect_contains_pointer(rect) {
            if let Some(p) = ui.input(|i| i.pointer.interact_pos()) {
                let on_node = self.node_rects.values().any(|r| r.contains(p));
                if !on_node && !bar.is_some_and(|b| b.contains(p)) {
                    // The editor took the click as "clear the selection"; a ping isn't that.
                    self.state.selected_nodes = selected_before;
                    let g = self.graph_point(rect, p);
                    self.add_ping(self.room.me, g, self.active, now);
                    self.room_send(Wire::Ping { id: self.room.me, x: g.x, y: g.y, doc: self.active });
                    ctx.request_repaint();
                }
            }
        }
        // My view: the centre of the canvas in graph coordinates, and the zoom.
        let z = self.state.pan_zoom.zoom;
        let view = PeerView { doc: self.active, center: self.graph_point(rect, rect.center()), zoom: z };
        let sendable = rect.is_positive() && (MIN_VIEW_ZOOM..=MAX_VIEW_ZOOM).contains(&z) && view.center.x.is_finite() && view.center.y.is_finite();
        if sendable && due(&ctx, &self.room.sent_view, &view, now) {
            self.room.sent_view = Some((view, now));
            self.room_send(Wire::View { id: self.room.me, doc: view.doc, x: view.center.x, y: view.center.y, zoom: view.zoom });
        }
        // My selection on this document.
        let mut nodes = self.state.selected_nodes.clone();
        nodes.truncate(MAX_SELECTION);
        let selection = (self.active, nodes);
        if due(&ctx, &self.room.sent_selection, &selection, now) {
            let refs = selection.1.iter().filter_map(|n| node_ref(*n)).collect();
            self.room_send(Wire::Selection { id: self.room.me, doc: selection.0, nodes: refs });
            self.room.sent_selection = Some((selection, now));
        }
        if let Some(bar) = bar {
            self.paint_follow_bar(ui, rect, bar);
        }
    }

    /// "Following Ada" across the top of the canvas in her colour, with a
    /// frame round the canvas and a Stop following button.
    fn paint_follow_bar(&mut self, ui: &mut Ui, rect: Rect, bar: Rect) {
        let Some(peer) = self.room.following.and_then(|id| self.room.peers.get(&id)).cloned() else { return };
        let color = rgb(peer.color);
        let fg = text_on(color);
        ui.painter().rect_stroke(rect.shrink(1.0), 0.0, Stroke::new(2.0_f32, color));
        // The bar takes its own clicks, so they don't reach the canvas under it.
        let _ = ui.interact(bar, Id::new("room_follow_bar"), Sense::click());
        ui.painter().rect_filled(bar, 0.0, color);
        let mut stop = false;
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(bar.shrink(BAR_MARGIN)).layout(Layout::right_to_left(Align::Center)));
        child.spacing_mut().button_padding.y = BAR_PAD_Y;
        child.spacing_mut().interact_size.y = 0.0;
        if widgets::button(&mut child, "Stop following").on_hover_text("Esc").clicked() {
            stop = true;
        }
        let room_for_text = child.min_rect().left() - bar.left() - 24.0;
        let mut job = LayoutJob::simple_singleline(format!("Following {}", peer.name), theme::semibold(14.0), fg);
        job.wrap = egui::text::TextWrapping::truncate_at_width(room_for_text.max(0.0));
        let g = ui.painter().layout_job(job);
        ui.painter().galley(pos2(bar.left() + 12.0, bar.center().y - g.size().y / 2.0), g, fg);
        if stop {
            self.room.following = None;
        }
    }

    /// The message box opened with /, beside the pointer. Enter sends, Esc
    /// (or clicking elsewhere) cancels.
    pub(crate) fn chat_box(&mut self, ctx: &egui::Context) {
        if self.room.chat.is_none() {
            return;
        }
        if !self.room.active() || !matches!(self.dialog, Dialog::None) || self.view != crate::map::View::Graph {
            self.room.chat = None;
            return;
        }
        let color = self.room.my_color();
        let mut done: Option<bool> = None;
        let Some(draft) = self.room.chat.as_mut() else { return };
        egui::Area::new(Id::new("room_chat"))
            .order(egui::Order::Foreground)
            .fixed_pos(draft.at + vec2(16.0, 16.0))
            .constrain(true)
            .show(ctx, |ui| {
                egui::Frame::none().fill(CONTENT).stroke(Stroke::new(2.0_f32, color)).inner_margin(egui::Margin::same(4.0)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let edit = egui::TextEdit::singleline(&mut draft.text)
                            .id(Id::new("room_chat_text"))
                            .hint_text("Message everyone")
                            .char_limit(MAX_CHAT)
                            .desired_width(260.0)
                            .margin(vec2(8.0, 6.0));
                        let r = ui.add(edit);
                        if draft.focus {
                            r.request_focus();
                            draft.focus = false;
                        } else if r.lost_focus() {
                            done = Some(ui.input(|i| i.key_pressed(Key::Enter)));
                        }
                        // How much is left of the limit.
                        let n = draft.text.chars().count();
                        let c = if n >= MAX_CHAT { ACCENT } else { TEXT_DIM };
                        ui.label(RichText::new(format!("{n} / {MAX_CHAT}")).small().color(c));
                    });
                });
            });
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            done = Some(false);
        }
        match done {
            Some(true) => self.send_chat(ctx),
            Some(false) => self.room.chat = None,
            None => {}
        }
    }

    pub(crate) fn send_chat(&mut self, ctx: &egui::Context) {
        let Some(draft) = self.room.chat.take() else { return };
        let text = clean_chat(&draft.text);
        if text.is_empty() {
            return;
        }
        let now = ctx.input(|i| i.time);
        self.add_message(self.room.me, text.clone(), now);
        self.room_send(Wire::Chat { id: self.room.me, text });
    }

    /// On the canvas, under the cursors: pings, outlines and name chips on
    /// nodes others have selected, and my own latest message by my pointer.
    pub(crate) fn paint_room_marks(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        let frozen = self.room.frozen;
        if !frozen {
            self.room.pings.retain(|p| now - p.at < PING_SECS);
            self.room.bubbles.retain(|_, (text, t)| now - *t < bubble_secs(text));
        }
        let rect = self.editor_rect;
        if !rect.is_positive() {
            return;
        }
        let clip = match self.follow_bar_rect(ctx, rect) {
            Some(bar) => Rect::from_min_max(pos2(rect.min.x, bar.max.y), rect.max),
            None => rect,
        };
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Middle, Id::new("room_marks"))).with_clip_rect(clip);
        let (z, pan) = (self.state.pan_zoom.zoom, self.state.pan_zoom.pan);
        let to_screen = |g: Pos2| rect.min + pan + g.to_vec2() * z;

        // Nodes others have selected here: an outline each (nested when
        // several people share a node) and their name chips side by side.
        let mut who: BTreeMap<NodeId, Vec<u32>> = BTreeMap::new();
        for (pid, (doc, nodes)) in &self.room.selections {
            if *doc != self.active || !self.room.peers.contains_key(pid) {
                continue;
            }
            for n in nodes {
                if self.node_rects.contains_key(n) {
                    who.entry(*n).or_default().push(*pid);
                }
            }
        }
        for (node, people) in &who {
            let r = self.node_rects[node];
            for (k, pid) in people.iter().enumerate() {
                // Strokes are centred on the rect's edge: this puts each one
                // just outside the node (and the one before it).
                let off = OUTLINE * k as f32 + OUTLINE / 2.0;
                painter.rect_stroke(r.expand(off), 0.0, Stroke::new(OUTLINE, rgb(self.room.peers[pid].color)));
            }
            let outer = OUTLINE * people.len() as f32;
            let mut x = r.min.x - outer;
            for pid in people {
                let peer = &self.room.peers[pid];
                let c = rgb(peer.color);
                let g = painter.layout_no_wrap(peer.name.clone(), FontId::proportional(12.0), text_on(c));
                let size = g.size() + vec2(10.0, 4.0);
                let chip = Rect::from_min_size(pos2(x, r.min.y - outer - size.y), size);
                painter.rect_filled(chip, 0.0, c);
                painter.galley(chip.min + vec2(5.0, 2.0), g, text_on(c));
                x = chip.max.x + 2.0;
            }
        }

        // Pings: two rings spreading out and fading, a dot and the name.
        let mut animating = false;
        for p in &self.room.pings {
            if p.doc != self.active {
                continue;
            }
            let Some(peer) = self.room.peers.get(&p.id) else { continue };
            let age = if frozen { FROZEN_PING_AGE } else { now - p.at };
            let t = (age / PING_SECS).clamp(0.0, 1.0) as f32;
            animating |= t < 1.0;
            let c = rgb(peer.color);
            let at = to_screen(p.pos);
            for lag in [0.0_f32, 0.25] {
                if t < lag {
                    continue;
                }
                let s = (t - lag) / (1.0 - lag);
                let radius = 6.0 + (PING_RADIUS - 6.0) * (1.0 - (1.0 - s).powi(3));
                painter.circle_stroke(at, radius, Stroke::new(2.5_f32, with_alpha(c, ((1.0 - s) * 255.0) as u8)));
            }
            // Solid until the last quarter, then fading out.
            let fade = (((1.0 - t) * 4.0).min(1.0) * 255.0) as u8;
            painter.circle_filled(at, 5.0, with_alpha(c, fade));
            paint_name_tag(&painter, at + vec2(10.0, 8.0), &peer.name, c, fade);
        }
        if animating && !frozen {
            ctx.request_repaint();
        }

        // My own latest message, by my pointer (others see it by my cursor).
        if let Some((text, _)) = self.room.bubbles.get(&self.room.me) {
            if let Some(p) = ctx.pointer_hover_pos().filter(|p| clip.contains(*p)) {
                paint_bubble(&painter, p + vec2(14.0, 24.0), text, self.room.my_color());
            }
        }
        if let Some(next) = self.room.bubbles.values().map(|(text, t)| bubble_secs(text) - (now - t)).reduce(f64::min) {
            if !frozen {
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(next.max(0.0)));
            }
        }
    }
}

/// "In this room": everyone here (click someone else to follow their view),
/// then the room's recent messages. Returns the person clicked.
pub fn room_section(ui: &mut Ui, room: &RoomUi) -> Option<u32> {
    let mut clicked = None;
    widgets::section(ui, "In this room", None);
    // Rows touch, like other clickable lists.
    let gap = std::mem::replace(&mut ui.spacing_mut().item_spacing.y, 0.0);
    for p in room.peers.values() {
        let me = p.id == room.me;
        let following = room.following == Some(p.id);
        let mut tags = Vec::new();
        if p.id == 0 {
            tags.push("host");
        }
        if me {
            tags.push("you");
        }
        if following {
            tags.push("following");
        }
        let place = room.places.get(&p.id).map(|pl| place_text(room, *pl));
        let content = |ui: &mut Ui| {
            let (r, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
            ui.painter().rect_filled(r, 0.0, rgb(p.color));
            widgets::name_label(ui, RichText::new(&p.name).color(TEXT));
            if !tags.is_empty() {
                ui.label(RichText::new(tags.join(", ")).small().color(TEXT_DIM));
            }
            if let Some(place) = &place {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    widgets::name_label(ui, RichText::new(place).small().color(TEXT_DIM));
                });
            }
        };
        if me {
            // Not clickable, so no hover look.
            widgets::plain_row(ui, 26.0, content);
            continue;
        }
        let row = widgets::list_row(ui, 26.0, content);
        #[cfg(test)]
        widgets::TEST_BUTTONS.with(|b| b.borrow_mut().push((format!("Follow {}", p.name), row.rect, true)));
        let tip = if following { format!("Stop following {}", p.name) } else { format!("Follow {}'s view", p.name) };
        if row.on_hover_text(tip).clicked() {
            clicked = Some(p.id);
        }
    }
    ui.spacing_mut().item_spacing.y = gap;
    widgets::divider(ui);
    widgets::section(ui, "Messages", None);
    if room.messages.is_empty() {
        widgets::caption(ui, "Press / over the graph to send everyone a short message.");
    }
    // The newest few, each wrapped to the panel.
    let skip = room.messages.len().saturating_sub(MESSAGES_SHOWN);
    for m in room.messages.iter().skip(skip) {
        let mut job = LayoutJob::default();
        job.append(&m.name, 0.0, TextFormat { font_id: theme::semibold(14.0), color: rgb(m.color), ..Default::default() });
        job.append(&m.text, 6.0, TextFormat { font_id: FontId::proportional(14.0), color: TEXT, ..Default::default() });
        ui.add(egui::Label::new(job).wrap());
    }
    widgets::divider(ui);
    clicked
}

/// Where someone is, in words ("on Smelting", "on the Map").
pub fn place_text(room: &RoomUi, place: fb_model::room::Place) -> String {
    use fb_model::room::Place;
    match place {
        Place::Map => "on the Map".into(),
        Place::Live => "in the Live view".into(),
        Place::Graph { doc } => match room.doc_names.get(&doc) {
            Some(n) => format!("on {n}"),
            None => "on a factory".into(),
        },
    }
}

/// People's colours on a tab: who is there (`at` matches their place).
pub fn tab_marks(ui: &Ui, room: &RoomUi, tab: Rect, at: impl Fn(fb_model::room::Place) -> bool) {
    if !room.active() {
        return;
    }
    let mut x = tab.right() - 4.0;
    for (id, place) in &room.places {
        if *id == room.me || !at(*place) {
            continue;
        }
        let Some(p) = room.peers.get(id) else { continue };
        let r = Rect::from_min_size(pos2(x - 6.0, tab.top() + 3.0), vec2(6.0, 6.0));
        ui.painter().rect_filled(r, 0.0, rgb(p.color));
        x -= 8.0;
        if x < tab.left() + 6.0 {
            break;
        }
    }
}
