//! Planning rooms, UI side: who is here, their cursors, and the shared
//! document. The backend does the networking; this polls it.
//!
//! The shared document is the whole project: each factory's plan, graph and
//! node positions, and the master graph. View state (pan, zoom, selection,
//! open tabs) stays per person. Positions travel unzoomed.

use crate::app::Planner;
use crate::graph::FactoryGraph;
use crate::theme::*;
use egui::{pos2, vec2, Align2, Color32, FontId, Pos2, Shape, Stroke};
use egui_node_graph2::NodeId;
use fb_model::room::{HostInfo, Peer};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Off,
    Hosting(HostInfo),
    Joining,
    Joined,
}

pub struct RoomUi {
    pub mode: Mode,
    pub me: u32,
    pub peers: BTreeMap<u32, Peer>,
    /// Other people's cursors in graph coordinates.
    pub cursors: BTreeMap<u32, Pos2>,
    /// The document each cursor is on (only those on mine are drawn).
    pub cursor_doc: BTreeMap<u32, u32>,
    pub poll_in_flight: bool,
    pub last_cursor: Option<(Pos2, f64)>,
    /// Where other cursors are drawn (eased toward `cursors`).
    pub cursor_shown: BTreeMap<u32, Pos2>,
    pub last_poll: f64,
    /// The backend pushed "something arrived" while a poll was running.
    pub wake_pending: bool,
}

impl Default for RoomUi {
    fn default() -> Self {
        Self {
            mode: Mode::Off,
            me: 0,
            peers: BTreeMap::new(),
            cursors: BTreeMap::new(),
            cursor_doc: BTreeMap::new(),
            poll_in_flight: false,
            last_cursor: None,
            cursor_shown: BTreeMap::new(),
            last_poll: 0.0,
            wake_pending: false,
        }
    }
}

impl RoomUi {
    pub fn active(&self) -> bool {
        matches!(self.mode, Mode::Hosting(_) | Mode::Joined)
    }
    pub fn reset(&mut self) {
        *self = RoomUi::default();
    }
    pub fn my_color(&self) -> Color32 {
        self.peers.get(&self.me).map(|p| rgb(p.color)).unwrap_or(ACCENT)
    }
}

pub fn rgb([r, g, b]: [u8; 3]) -> Color32 {
    Color32::from_rgb(r, g, b)
}

#[derive(Serialize)]
pub struct SharedProjectRef<'a> {
    pub docs: Vec<SharedDocRef<'a>>,
}

#[derive(Deserialize)]
pub struct SharedProject {
    pub docs: Vec<SharedDoc>,
}

#[derive(Serialize)]
pub struct SharedDocRef<'a> {
    pub id: u32,
    pub name: &'a str,
    pub planner: &'a Planner,
    pub graph: &'a FactoryGraph,
    pub positions: Vec<(NodeId, [f32; 2])>,
    pub order: &'a Vec<NodeId>,
}

#[derive(Deserialize)]
pub struct SharedDoc {
    pub id: u32,
    pub name: String,
    pub planner: Planner,
    pub graph: FactoryGraph,
    pub positions: Vec<(NodeId, [f32; 2])>,
    pub order: Vec<NodeId>,
}

/// The standard arrow pointer, tip at the origin, 20 px tall.
const ARROW: [[f32; 2]; 7] = [[0.0, 0.0], [0.0, 17.0], [4.5, 13.0], [7.5, 20.0], [10.0, 19.0], [7.0, 12.0], [12.5, 12.0]];

/// Draws a cursor at `tip`; others' cursors are ghosted and named.
pub fn paint_cursor(painter: &egui::Painter, tip: Pos2, color: Color32, name: Option<&str>) {
    let alpha = if name.is_some() { 150 } else { 255 };
    let fill = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha);
    let outline = Color32::from_rgba_unmultiplied(12, 12, 12, alpha);
    let pts: Vec<Pos2> = ARROW.iter().map(|p| tip + vec2(p[0], p[1])).collect();
    painter.add(Shape::convex_polygon(pts[..3].to_vec(), fill, Stroke::NONE));
    painter.add(Shape::convex_polygon(vec![pts[0], pts[2], pts[5], pts[6]], fill, Stroke::NONE));
    painter.add(Shape::convex_polygon(vec![pts[2], pts[3], pts[4], pts[5]], fill, Stroke::NONE));
    painter.add(Shape::closed_line(pts, Stroke::new(1.0_f32, outline)));
    if let Some(name) = name {
        let font = FontId::proportional(12.0);
        let galley = painter.layout_no_wrap(name.to_string(), font, TEXT);
        let at = tip + vec2(10.0, 22.0);
        let rect = egui::Rect::from_min_size(at, galley.size() + vec2(10.0, 4.0));
        painter.rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(12, 12, 12, 200));
        painter.rect_filled(egui::Rect::from_min_size(rect.min, vec2(3.0, rect.height())), 0.0, fill);
        painter.galley(pos2(rect.min.x + 6.0, rect.min.y + 2.0), galley, TEXT);
        let _ = Align2::LEFT_TOP;
    }
}

/// The local cursor as a native cursor image in `color` over the canvas, or
/// back to normal with `None`. A style rule with `!important` on the canvas
/// beats the cursor eframe sets on the page body.
pub fn set_own_cursor(color: Option<Color32>) {
    #[cfg(target_arch = "wasm32")]
    {
        use std::cell::Cell;
        thread_local! {
            static LAST: Cell<Option<Option<[u8; 3]>>> = const { Cell::new(None) };
        }
        let want = color.map(|c| [c.r(), c.g(), c.b()]);
        if LAST.with(|l| l.get()) == Some(want) {
            return;
        }
        LAST.with(|l| l.set(Some(want)));
        let Some(doc) = web_sys::window().and_then(|w| w.document()) else { return };
        let Some(canvas) = doc.get_element_by_id("app") else { return };
        match want {
            Some([r, g, b]) => {
                let svg = format!(
                    "<svg xmlns='http://www.w3.org/2000/svg' width='15' height='22' viewBox='-0.5 -0.5 14 22'><path d='M0 0 L0 17 L4.5 13 L7.5 20 L10 19 L7 12 L12.5 12 Z' fill='rgb({r},{g},{b})' stroke='rgb(12,12,12)' stroke-width='1' stroke-linejoin='round'/></svg>"
                );
                let url = String::from(js_sys::encode_uri_component(&svg));
                let css = format!("#app.fb-own-cursor {{ cursor: url(\"data:image/svg+xml,{url}\") 1 1, default !important; }}");
                let style = doc.get_element_by_id("fb-own-cursor-style").or_else(|| {
                    let el = doc.create_element("style").ok()?;
                    el.set_id("fb-own-cursor-style");
                    doc.head()?.append_child(&el).ok()?;
                    Some(el)
                });
                if let Some(style) = style {
                    style.set_text_content(Some(&css));
                }
                let _ = canvas.class_list().add_1("fb-own-cursor");
            }
            None => {
                let _ = canvas.class_list().remove_1("fb-own-cursor");
            }
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = color;
}
