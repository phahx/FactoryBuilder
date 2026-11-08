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

#[cfg(test)]
mod tests {
    #[test]
    fn short_names() {
        assert_eq!(super::short("Conveyor Belt Mk.3", false), "Mk.3");
        assert_eq!(super::short("Pipeline Mk.2", true), "Pipe Mk.2");
    }
}
