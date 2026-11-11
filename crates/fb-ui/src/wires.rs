//! Labels on wires: the belt (or pipe, for fluids) and its speed. By
//! default the slowest that carries the wire's flow (and how many in
//! parallel when even the fastest isn't enough); click a label to choose
//! one, which then limits the wire like a belt in game. Belt and pipe
//! speeds come from the game's Docs (see fb-engine extraction.rs).

use crate::app::FactoryApp;
use crate::fmt::fmt_num;
use crate::theme::*;
use egui::{pos2, vec2, FontId, Pos2, Rect};
use egui_node_graph2::{InputId, NodeId, OutputId};
use serde::{Deserialize, Serialize};

/// A belt (or pipe) chosen for a wire: Mk.1 = 1.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BeltChoice {
    pub input: InputId,
    pub output: OutputId,
    pub tier: u8,
}

/// The tier chosen for a wire, if one was.
pub fn chosen(belts: &[BeltChoice], input: InputId, output: OutputId) -> Option<u8> {
    belts.iter().find(|b| b.input == input && b.output == output).map(|b| b.tier)
}

fn list(game: &fb_model::GameData, fluid: bool) -> &[fb_model::Belt] {
    if fluid {
        &game.extraction.pipes
    } else {
        &game.extraction.belts
    }
}

/// Per minute a belt (pipe) of `tier` carries.
pub fn speed(game: &fb_model::GameData, fluid: bool, tier: u8) -> Option<f64> {
    list(game, fluid).get((tier as usize).checked_sub(1)?).map(|b| b.per_min)
}

/// Below this zoom the labels would be unreadable, so they're hidden.
const MIN_ZOOM: f32 = 0.45;

/// "Mk.3" from "Conveyor Belt Mk.3"; "Pipe Mk.1" from "Pipeline Mk.1".
fn short(name: &str, fluid: bool) -> String {
    let tier = name.rsplit(' ').next().unwrap_or(name);
    if fluid {
        format!("Pipe {tier}")
    } else {
        tier.to_string()
    }
}

/// The point at `t` along a cubic bezier.
fn point(p: &[Pos2; 4], t: f32) -> Pos2 {
    let u = 1.0 - t;
    let w = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
    pos2(p.iter().zip(w).map(|(q, k)| q.x * k).sum(), p.iter().zip(w).map(|(q, k)| q.y * k).sum())
}

/// Labels sit at the middle of each wire, but never on a node or on top of
/// another label (where wires cross), so nothing gets covered.
pub fn paint_labels(app: &mut FactoryApp, ui: &egui::Ui, wires: &[(InputId, OutputId, [Pos2; 4])], nodes: &egui_node_graph2::NodeRects) {
    #[cfg(test)]
    app.wire_label_texts.clear();
    let zoom = app.state.pan_zoom.zoom;
    if zoom < MIN_ZOOM {
        return;
    }
    let game = app.user.game.clone();
    let clip = app.editor_rect;
    let painter = ui.painter().with_clip_rect(clip);
    let font = FontId::proportional((11.0 * zoom).clamp(9.0, 14.0));
    let mut placed: Vec<Rect> = Vec::new();
    let mut open = None;
    for (input, output, points) in wires {
        let flow = app.wire_flow.get(&(*input, *output)).copied().unwrap_or(0.0);
        // No belt between a resource node and the extractor standing on it.
        if crate::extractors::from_the_ground(&app.state.graph, *output, *input) {
            continue;
        }
        let Some(item) = app.state.graph.try_get_output(*output).map(|o| o.typ.0.clone()) else { continue };
        if crate::power_net::is_power(&item) {
            continue;
        }
        let fluid = game.item_form(&item).is_fluid();
        // The chosen belt, or the slowest that carries the flow.
        let pick = chosen(&app.planner.belts, *input, *output).and_then(|t| list(&game, fluid).get(t as usize - 1).map(|b| (b, 1, true)));
        let Some((belt, lines, fixed)) = pick.or_else(|| game.extraction.carrier(fluid, flow).map(|c| (c.kind, c.lines, false))) else { continue };
        let full = fixed && flow >= belt.per_min - 1e-6;
        let over = lines > 1 || full;
        let name = short(&belt.name, fluid);
        let lanes = if lines > 1 { format!("{lines}× ") } else { String::new() };
        // The tier over its speed (just the tier where that doesn't fit,
        // zoomed out), at the middle of the wire or the nearest clear spot.
        let full_text = format!("{lanes}{name}\n{}{}", fmt_num(belt.per_min), if fluid { " m³" } else { "/min" });
        let c_name = &belt.name;
        let free = |at: Pos2, size: egui::Vec2| {
            let r = Rect::from_center_size(at, size);
            clip.contains(at) && !nodes.values().any(|n| n.intersects(r)) && !placed.iter().any(|p| p.expand(2.0).intersects(r))
        };
        let spot = [full_text, format!("{lanes}{name}")].into_iter().find_map(|text| {
            let galley = painter.layout_no_wrap(text.clone(), font.clone(), if over { TEXT_DARK } else { TEXT });
            let size = galley.size() + vec2(8.0, 2.0);
            let at = [0.5, 0.4, 0.6, 0.3, 0.7].into_iter().map(|t| point(points, t)).find(|p| free(*p, size))?;
            Some((text, galley, Rect::from_center_size(at, size)))
        });
        #[cfg_attr(not(test), allow(unused_variables))]
        let Some((text, galley, rect)) = spot else { continue };
        placed.push(rect);
        let id = ui.id().with(("wire_label", *input, *output));
        let resp = ui.interact(rect, id, egui::Sense::click());
        painter.rect_filled(rect, 2.0, if over { ACCENT } else { CONTENT });
        if resp.hovered() {
            painter.rect_stroke(rect, 2.0, egui::Stroke::new(1.0_f32, TEXT));
        }
        painter.galley(rect.center() - galley.size() / 2.0, galley, TEXT);
        let per = if fluid { "m³/min" } else { "per min" };
        let how = if fixed { "chosen" } else { "the slowest that carries it" };
        let tip = if lines > 1 {
            format!("{} {per}: more than one {c_name} carries ({} {per}), so {lines} in parallel. Click to choose the belt.", fmt_num(flow), fmt_num(belt.per_min))
        } else if full {
            format!("{} {per}: a {c_name} carries at most {} {per}, so it can hold this line back. Click to choose another.", fmt_num(flow), fmt_num(belt.per_min))
        } else {
            format!("{} {per} on a {c_name} ({how}, up to {} {per}). Click to choose the belt.", fmt_num(flow), fmt_num(belt.per_min))
        };
        // In the Live view, only a planned piece's belts can be chosen.
        let g = &app.state.graph;
        let planned = |n: Option<NodeId>| n.and_then(|n| g.nodes.get(n)).is_some_and(|n| n.user_data.planned);
        let choosable = !app.user.live_view || planned(g.try_get_input(*input).map(|i| i.node)) || planned(g.try_get_output(*output).map(|o| o.node));
        let resp = if choosable {
            resp.on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand)
        } else {
            resp.on_hover_text(format!("{} {per}", fmt_num(flow)))
        };
        if resp.clicked() && choosable {
            open = Some((*input, *output, rect.left_bottom()));
        }
        #[cfg(test)]
        app.wire_label_texts.push(text);
    }
    if open.is_some() {
        app.belt_menu = open;
    }
}

/// A wire's belt menu: the slowest that carries it, or a chosen belt (pipe).
pub fn belt_menu(app: &mut FactoryApp, ctx: &egui::Context) {
    let Some((input, output, pos)) = app.belt_menu else { return };
    let g = &app.state.graph;
    if !g.connections(input).contains(&output) {
        app.belt_menu = None;
        return;
    }
    let game = app.user.game.clone();
    let fluid = game.item_form(&g[output].typ.0).is_fluid();
    let current = chosen(&app.planner.belts, input, output);
    let mut pick: Option<Option<u8>> = None;
    let title = if fluid { "Pipeline" } else { "Conveyor belt" };
    let menu = crate::menus::popup(ctx, "belt_menu", pos, title, |ui| {
        let auto = if fluid { "The slowest pipe that carries it" } else { "The slowest belt that carries it" };
        if crate::menus::choice(ui, "Automatic", "", current.is_none(), true).on_hover_text(auto).clicked() {
            pick = Some(None);
        }
        for (k, b) in list(&game, fluid).iter().enumerate() {
            let t = k as u8 + 1;
            let per = if fluid { "m³/min" } else { "/min" };
            if crate::menus::choice(ui, &b.name, &format!("{} {per}", fmt_num(b.per_min)), current == Some(t), true).clicked() {
                pick = Some(Some(t));
            }
        }
    });
    if pick.is_some() || crate::menus::dismissed(ctx, &menu) {
        app.belt_menu = None;
    }
    if let Some(choice) = pick {
        app.planner.belts.retain(|b| !(b.input == input && b.output == output));
        if let Some(tier) = choice {
            app.planner.belts.push(BeltChoice { input, output, tier });
        }
        app.changed();
    }
}

/// Points along a wire's bezier with the distance to each from its start.
fn samples(p: &[Pos2; 4]) -> Vec<(Pos2, f32)> {
    let at = |t: f32| {
        let u = 1.0 - t;
        let w = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
        pos2(p.iter().zip(w).map(|(q, k)| q.x * k).sum(), p.iter().zip(w).map(|(q, k)| q.y * k).sum())
    };
    let mut out: Vec<(Pos2, f32)> = vec![(p[0], 0.0)];
    for k in 1..=32 {
        let q = at(k as f32 / 32.0);
        let d = out.last().map(|(a, s)| s + a.distance(q)).unwrap_or(0.0);
        out.push((q, d));
    }
    out
}

/// Small arrows drifting along wires the way things flow: item wires that
/// carry something, and power wires into and out of factories while their
/// circuit is lit (between poles power can go either way). Arrows under a
/// node aren't drawn, as the wires there aren't. Returns whether any moved.
pub fn paint_flow(app: &FactoryApp, ui: &egui::Ui, wires: &[(InputId, OutputId, [Pos2; 4])], nodes: &egui_node_graph2::NodeRects) -> bool {
    let zoom = app.state.pan_zoom.zoom;
    if zoom < MIN_ZOOM {
        return false;
    }
    let g = &app.state.graph;
    let pole = |n| crate::power_net::pole_of(g, n).is_some();
    let clip = app.editor_rect;
    let painter = ui.painter().with_clip_rect(clip);
    let time = ui.input(|i| i.time) as f32;
    let (gap, speed, size) = (56.0 * zoom, 28.0 * zoom, 5.0 * zoom.max(0.8));
    let mut any = false;
    for (input, output, points) in wires {
        let (Some(o), Some(i)) = (g.try_get_output(*output), g.try_get_input(*input)) else { continue };
        let moving = if crate::power_net::is_power(&o.typ.0) {
            !(pole(o.node) && pole(i.node)) && app.user.power_lit.contains(&(o.node, i.node))
        } else {
            app.wire_flow.get(&(*input, *output)).is_some_and(|f| *f > 1e-6)
        };
        if !moving || !clip.intersects(Rect::from_points(points)) {
            continue;
        }
        any = true;
        let path = samples(points);
        let len = path.last().map(|p| p.1).unwrap_or(0.0);
        let mut s = (time * speed) % gap;
        let mut k = 1;
        while s < len {
            while k + 1 < path.len() && path[k].1 < s {
                k += 1;
            }
            let (a, b) = (path[k - 1], path[k]);
            let t = if b.1 > a.1 { (s - a.1) / (b.1 - a.1) } else { 0.0 };
            let at = a.0 + (b.0 - a.0) * t;
            let dir = (b.0 - a.0).normalized();
            if clip.contains(at) && !nodes.values().any(|r| r.contains(at)) {
                let side = vec2(-dir.y, dir.x) * size * 0.8;
                let tip = at + dir * size;
                let back = at - dir * size;
                painter.add(egui::Shape::convex_polygon(vec![tip, back + side, back - side], egui::Color32::from_white_alpha(190), egui::Stroke::NONE));
            }
            s += gap;
        }
    }
    any
}

#[cfg(test)]
mod tests {
    #[test]
    fn samples_measure_the_curve() {
        // A straight wire 100 long, as a bezier.
        let p = [egui::pos2(0.0, 0.0), egui::pos2(30.0, 0.0), egui::pos2(70.0, 0.0), egui::pos2(100.0, 0.0)];
        let s = super::samples(&p);
        assert!((s.last().unwrap().1 - 100.0).abs() < 1e-3);
        assert!(s.windows(2).all(|w| w[1].1 >= w[0].1), "distance only grows");
    }

    #[test]
    fn short_names() {
        assert_eq!(super::short("Conveyor Belt Mk.3", false), "Mk.3");
        assert_eq!(super::short("Pipeline Mk.2", true), "Pipe Mk.2");
    }
}
