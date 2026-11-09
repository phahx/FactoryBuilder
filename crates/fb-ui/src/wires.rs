//! Labels on wires: the slowest belt (or pipe, for fluids) that carries the
//! wire's flow, and how many in parallel when even the fastest isn't enough.
//! Belt and pipe speeds come from the game's Docs (see fb-engine extraction.rs).

use crate::app::FactoryApp;
use crate::fmt::fmt_num;
use crate::theme::*;
use egui::{pos2, vec2, FontId, Pos2, Rect};
use egui_node_graph2::{InputId, OutputId};

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

fn midpoint(p: &[Pos2; 4]) -> Pos2 {
    // Cubic bezier at t = 0.5.
    let x = (p[0].x + 3.0 * p[1].x + 3.0 * p[2].x + p[3].x) / 8.0;
    let y = (p[0].y + 3.0 * p[1].y + 3.0 * p[2].y + p[3].y) / 8.0;
    pos2(x, y)
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
    for (input, output, points) in wires {
        let Some(&flow) = app.wire_flow.get(&(*input, *output)) else { continue };
        // No belt between a resource node and the extractor standing on it.
        if crate::extractors::from_the_ground(&app.state.graph, *output, *input) {
            continue;
        }
        let Some(item) = app.state.graph.try_get_output(*output).map(|o| o.typ.0.clone()) else { continue };
        let fluid = game.item_form(&item).is_fluid();
        let Some(c) = game.extraction.carrier(fluid, flow) else { continue };
        let over = c.lines > 1;
        let text = if over { format!("{}× {}", c.lines, short(&c.kind.name, fluid)) } else { short(&c.kind.name, fluid) };
        let at = midpoint(points);
        if !clip.contains(at) {
            continue;
        }
        let galley = painter.layout_no_wrap(text.clone(), font.clone(), if over { TEXT_DARK } else { TEXT });
        let rect = Rect::from_center_size(at, galley.size() + vec2(8.0, 2.0));
        if nodes.values().any(|n| n.intersects(rect)) || placed.iter().any(|p| p.expand(2.0).intersects(rect)) {
            continue;
        }
        placed.push(rect);
        painter.rect_filled(rect, 2.0, if over { ACCENT } else { CONTENT });
        painter.galley(rect.center() - galley.size() / 2.0, galley, TEXT);
        let unit = if fluid { "m³/min" } else { "per min" };
        let tip = if over {
            format!("{} {unit}: more than one {} carries ({} {unit}), so {} in parallel.", fmt_num(flow), c.kind.name, fmt_num(c.kind.per_min), c.lines)
        } else {
            format!("{} {unit}: {} (up to {} {unit}).", fmt_num(flow), c.kind.name, fmt_num(c.kind.per_min))
        };
        let id = ui.id().with(("wire_label", *input, *output));
        // Hover only (a tooltip); the label isn't clickable, so no hover styling.
        ui.interact(rect, id, egui::Sense::hover()).on_hover_text(tip);
        #[cfg(test)]
        app.wire_label_texts.push(text);
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
