//! Floors: a factory built over several levels. Every node is on a floor
//! (Floor 1 is the ground), and the graph shows one floor at a time, picked
//! from the tabs over it. A belt or pipe to another floor ends in a stub
//! saying where it goes and what carries it there: a Conveyor Lift (the
//! game's lifts run at the same speeds as its belts, Mk.1 to Mk.6), or a
//! pipe with the head lift it needs on the way up.
//!
//! Each floor's height (metres above Floor 1) is kept with the plan; it's
//! what the head lift of a pipe going up is worked out from.

use crate::app::FactoryApp;
use crate::graph::{FactoryGraph, NodeKind};
use crate::theme::*;
use egui::{pos2, vec2, Align2, FontId, Pos2, Rect, Sense, Stroke};
use egui_node_graph2::{InputId, NodeId, OutputId};
use std::collections::HashSet;

/// A new floor's height over the one below it: two 4 m walls.
pub const STEP_M: f32 = 8.0;
/// Most floors a factory can have.
pub const MAX: usize = 32;

/// "Floor 1" for floor 0.
pub fn name(floor: u8) -> String {
    format!("Floor {}", floor as u32 + 1)
}

/// How many floors there are: the ones with a height, and any a node is on.
pub fn count(g: &FactoryGraph, heights: &[f32]) -> usize {
    let used = g.iter_nodes().map(|n| g[n].user_data.floor as usize + 1).max().unwrap_or(1);
    (heights.len() + 1).max(used).clamp(1, MAX)
}

/// Floor `f`'s height above Floor 1, in metres.
pub fn height(heights: &[f32], f: u8) -> f32 {
    match f {
        0 => 0.0,
        f => heights.get(f as usize - 1).copied().unwrap_or_else(|| {
            let last = heights.len();
            heights.last().copied().unwrap_or(0.0) + STEP_M * (f as usize - last) as f32
        }),
    }
}

/// The nodes not on `floor` (the graph draws the rest).
pub fn hidden(g: &FactoryGraph, floor: u8) -> HashSet<NodeId> {
    g.iter_nodes().filter(|n| g[*n].user_data.floor != floor).collect()
}

/// What carries a wire between floors: "Conveyor Lift Mk.2", or for a pipe
/// going up "Pipe · 16 m head lift".
pub fn carrier(app: &FactoryApp, input: InputId, output: OutputId) -> String {
    let g = &app.state.graph;
    let game = &app.user.game;
    let item = g[output].typ.0.clone();
    let fluid = game.item_form(&item).is_fluid();
    let flow = app.wire_flow.get(&(input, output)).copied().unwrap_or(0.0);
    let (from, to) = (g[g[output].node].user_data.floor, g[g[input].node].user_data.floor);
    if fluid {
        let up = height(&app.planner.floors, to) - height(&app.planner.floors, from);
        return if up > 0.0 { format!("Pipe · {} m head lift", crate::fmt::fmt_num(up as f64)) } else { "Pipe".into() };
    }
    // The chosen belt's tier, or the slowest that carries the flow.
    let tier = crate::wires::chosen(&app.planner.belts, input, output).map(|t| t as usize).or_else(|| {
        let c = game.extraction.carrier(false, flow)?;
        Some(game.extraction.belts.iter().position(|b| std::ptr::eq(b, c.kind))? + 1)
    });
    match tier {
        Some(t) => format!("Conveyor Lift Mk.{t}"),
        None => "Conveyor Lift".into(),
    }
}

/// The floor tabs over the graph: one per floor, then + for a new one; the
/// floor shown can have its height set. Returns the floor picked, if any.
pub fn bar(app: &mut FactoryApp, ui: &mut egui::Ui, rect: Rect) {
    let floors = count(&app.state.graph, &app.planner.floors);
    let current = app.user.floor.min(floors as u8 - 1);
    let h = 26.0;
    let mut x = rect.left() + 8.0;
    let y = rect.top() + 8.0;
    let painter = ui.painter().clone();
    let font = FontId::proportional(14.0);
    let mut pick = None;
    for f in 0..floors as u8 {
        let label = name(f);
        let g = painter.layout_no_wrap(label.clone(), font.clone(), TEXT);
        let r = Rect::from_min_size(pos2(x, y), vec2(g.size().x + 20.0, h));
        let resp = ui.interact(r, ui.id().with(("floor_tab", f)), Sense::click());
        #[cfg(test)]
        crate::widgets::TEST_BUTTONS.with(|b| b.borrow_mut().push((label.clone(), r, true)));
        let selected = f == current;
        let (fill, fg) = if selected {
            (TAB_SELECTED, TEXT_DARK)
        } else if resp.hovered() {
            (ACCENT, TEXT)
        } else {
            (TAB, TEXT)
        };
        painter.rect_filled(r, 0.0, fill);
        painter.text(r.center(), Align2::CENTER_CENTER, label, font.clone(), fg);
        if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() && !selected {
            pick = Some(f);
        }
        x = r.right() + 2.0;
    }
    // A new floor above the top one.
    if floors < MAX {
        let r = Rect::from_min_size(pos2(x, y), vec2(h, h));
        let resp = ui.interact(r, ui.id().with("floor_add"), Sense::click());
        #[cfg(test)]
        crate::widgets::TEST_BUTTONS.with(|b| b.borrow_mut().push(("Add floor".into(), r, true)));
        painter.rect_filled(r, 0.0, if resp.hovered() { ACCENT } else { TAB });
        painter.text(r.center(), Align2::CENTER_CENTER, "+", FontId::proportional(18.0), TEXT);
        if resp.on_hover_text("Add a floor above the top one").on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            let top = height(&app.planner.floors, floors as u8 - 1);
            app.planner.floors.resize(floors - 1, 0.0);
            for f in app.planner.floors.len()..floors - 1 {
                let below = height(&app.planner.floors, f as u8);
                app.planner.floors.push(below + STEP_M);
            }
            app.planner.floors.push(top + STEP_M);
            pick = Some(floors as u8);
            app.changed();
        }
        x = r.right() + 10.0;
    }
    // The floor shown: how high it is.
    if current > 0 {
        let at = Rect::from_min_size(pos2(x, y), vec2(220.0, h));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(at).layout(egui::Layout::left_to_right(egui::Align::Center)));
        let mut v = height(&app.planner.floors, current) as f64;
        child.label(egui::RichText::new("Height").color(TEXT_DIM));
        if crate::widgets::num_field(&mut child, egui::Id::new(("floor_height", current)), &mut v, 56.0) {
            let n = current as usize;
            let floors_now = app.planner.floors.clone();
            app.planner.floors = (1..=n.max(floors_now.len())).map(|f| height(&floors_now, f as u8)).collect();
            app.planner.floors[n - 1] = (v.clamp(0.0, 10_000.0)) as f32;
            app.changed();
        }
        child.label(egui::RichText::new("m above Floor 1").color(TEXT_DIM));
    }
    if let Some(f) = pick {
        app.user.floor = f;
        app.state.selected_nodes.clear();
        app.frame_all = true;
    }
}

/// Where each wire to another floor ends on this one: a short tail and a
/// chip saying which floor and what carries it (click to go there).
pub fn paint_stubs(app: &mut FactoryApp, ui: &mut egui::Ui, cut: &[(InputId, OutputId, Pos2)]) {
    let z = app.state.pan_zoom.zoom;
    if z < 0.35 || cut.is_empty() {
        return;
    }
    let painter = ui.painter().with_clip_rect(app.editor_rect);
    let font = FontId::proportional((11.0 * z).clamp(9.0, 14.0));
    let mut go: Option<(u8, NodeId)> = None;
    let here = app.user.floor;
    // What each stub says, worked out before drawing.
    struct Stub {
        at: Pos2,
        out_here: bool,
        other: NodeId,
        floor: u8,
        item: String,
        text: String,
    }
    let stubs: Vec<Stub> = cut
        .iter()
        .filter_map(|(input, output, at)| {
            let g = &app.state.graph;
            let (o, i) = (g.try_get_output(*output)?, g.try_get_input(*input)?);
            let out_here = g[o.node].user_data.floor == here;
            let other = if out_here { i.node } else { o.node };
            let floor = g[other].user_data.floor;
            Some(Stub { at: *at, out_here, other, floor, item: o.typ.0.clone(), text: format!("{} · {}", name(floor), carrier(app, *input, *output)) })
        })
        .collect();
    for (k, st) in stubs.into_iter().enumerate() {
        // The drawn end: an output reaches out right, an input in from the left.
        let dir = if st.out_here { 1.0 } else { -1.0 };
        let color = app.user.item_color(&st.item);
        let tail = st.at + vec2(24.0 * z * dir, 0.0);
        painter.line_segment([st.at, tail], Stroke::new(3.0 * z.max(0.6), color));
        let up = st.floor > here;
        let galley = painter.layout_no_wrap(st.text, font.clone(), TEXT);
        let icon = galley.size().y;
        let size = vec2(galley.size().x + icon + 14.0, galley.size().y + 6.0);
        let chip = if st.out_here { Rect::from_min_size(tail - vec2(0.0, size.y / 2.0), size) } else { Rect::from_min_size(tail - vec2(size.x, size.y / 2.0), size) };
        let resp = ui.interact(chip, ui.id().with(("floor_stub", k)), Sense::click());
        #[cfg(test)]
        crate::widgets::TEST_BUTTONS.with(|b| b.borrow_mut().push((format!("stub {}", name(st.floor)), chip, true)));
        painter.rect_filled(chip, 2.0, if resp.hovered() { ACCENT } else { CONTENT });
        painter.rect_stroke(chip, 2.0, Stroke::new(1.0_f32, color));
        let a = Rect::from_min_size(chip.min + vec2(5.0, 3.0), vec2(icon, icon));
        // The menus' arrow, turned to point up or down.
        if let Some(uri) = app.user.assets.as_mut().and_then(|a| a.ui_uri(crate::assets::UiIcon::Arrow)) {
            let angle = if up { -std::f32::consts::FRAC_PI_2 } else { std::f32::consts::FRAC_PI_2 };
            egui::Image::new(uri).tint(TEXT).rotate(angle, vec2(0.5, 0.5)).paint_at(ui, a);
        }
        painter.galley(pos2(a.right() + 4.0, chip.top() + 3.0), galley, TEXT);
        let tip = format!("Goes {} to {}. Click to go there.", if up { "up" } else { "down" }, name(st.floor));
        if resp.on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            go = Some((st.floor, st.other));
        }
    }
    if let Some((floor, node)) = go {
        app.user.floor = floor;
        app.focus_node = Some(node);
    }
}

/// Floors of a factory's nodes from their buildings' heights (centimetres,
/// as the save has them): heights within `gap_cm` of each other are one
/// floor, lowest first. Returns each height's floor and the floors' heights
/// above the lowest, in metres (Floor 2 on).
pub fn from_heights(z_cm: &[f32], gap_cm: f32) -> (Vec<u8>, Vec<f32>) {
    let bottoms = levels(z_cm, gap_cm);
    let floors = z_cm.iter().map(|z| level_of(&bottoms, *z)).collect();
    (floors, heights_of(&bottoms))
}

/// Where each level starts (world Z, cm), lowest first: a new level where a
/// height is more than `gap_cm` above the last level's start.
pub fn levels(z_cm: &[f32], gap_cm: f32) -> Vec<f32> {
    let mut sorted: Vec<f32> = z_cm.iter().copied().filter(|z| z.is_finite()).collect();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let mut bottoms: Vec<f32> = Vec::new();
    for z in sorted {
        if bottoms.last().is_none_or(|b| z - *b > gap_cm) && bottoms.len() < MAX {
            bottoms.push(z);
        }
    }
    bottoms
}

/// The level `z` is on: the highest that starts at or below it (below the
/// lowest: the lowest).
pub fn level_of(bottoms: &[f32], z: f32) -> u8 {
    if !z.is_finite() {
        return 0;
    }
    bottoms.iter().rposition(|b| z + 1e-3 >= *b).unwrap_or(0) as u8
}

/// Levels 2 and up: each one's height above the lowest, in metres.
pub fn heights_of(bottoms: &[f32]) -> Vec<f32> {
    let base = bottoms.first().copied().unwrap_or(0.0);
    bottoms.iter().skip(1).map(|b| ((b - base) / 100.0 * 10.0).round() / 10.0).collect()
}

/// Buildings closer in height than this are on one floor (a wall is 4 m).
pub const FLOOR_GAP_CM: f32 = 400.0;

/// Whether `kind` is something that stands on a floor (everything does; the
/// master graph's blocks and poles have none, and its graph has no tabs).
pub fn on_floors(kind: &NodeKind) -> bool {
    !matches!(kind, NodeKind::Factory { .. } | NodeKind::Pole { .. })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heights_make_floors() {
        // Ground at 0 and 50 cm, a floor 8 m up, another 16.5 m up.
        let (f, h) = from_heights(&[0.0, 50.0, 800.0, 820.0, 1650.0], 300.0);
        assert_eq!(f, vec![0, 0, 1, 1, 2]);
        assert_eq!(h, vec![8.0, 16.5]);
    }

    #[test]
    fn heights_of_floors_without_one_set() {
        assert_eq!(height(&[], 0), 0.0);
        assert_eq!(height(&[], 2), 16.0);
        assert_eq!(height(&[10.0], 1), 10.0);
        assert_eq!(height(&[10.0], 3), 26.0);
    }
}
