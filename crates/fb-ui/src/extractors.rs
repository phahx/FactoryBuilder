//! Extractor nodes: the building that takes a raw resource out of the
//! ground (or water) and puts it on a belt or pipe. A Resource node is the
//! deposit; an Extractor node after it is the Miner, Water Extractor, Oil
//! Extractor or Resource Well Extractor, with how many, at what clock speed,
//! on what purity of node. Their rates and power come from the game's Docs
//! (fb-engine/src/extraction.rs); purity multipliers from `fb_model::Purity`.

use crate::fmt::{fmt_num, fmt_rate_unit};
use crate::graph::*;
use crate::overview_extra::{plan_resource, ExtractionSettings};
use crate::theme::*;
use egui::{vec2, RichText, Ui};
use egui_node_graph2::NodeId;
use fb_model::{Extractor, ExtractorKind, GameData, Purity};

/// An extractor's node purity: its resource node's, when it stands on one.
pub fn purity_of(g: &FactoryGraph, node: NodeId) -> Purity {
    let own = match g[node].user_data.kind {
        NodeKind::Extractor { purity, .. } => purity,
        _ => return Purity::Normal,
    };
    g[node]
        .inputs
        .iter()
        .flat_map(|(_, i)| g.connections(*i))
        .find_map(|o| match g[g[o].node].user_data.kind {
            NodeKind::Resource { purity, .. } => Some(purity),
            _ => None,
        })
        .unwrap_or(own)
}

/// Whether an extractor stands on a resource node (so its purity is that node's).
pub fn on_resource(g: &FactoryGraph, node: NodeId) -> bool {
    g[node].inputs.iter().flat_map(|(_, i)| g.connections(*i)).any(|o| matches!(g[g[o].node].user_data.kind, NodeKind::Resource { .. }))
}

/// Under a resource node: its purity (not for water, which has none).
pub fn resource_footer(ui: &mut Ui, s: &mut UserState, graph: &FactoryGraph, node_id: NodeId, item: &str, purity: Purity) -> Vec<egui_node_graph2::NodeResponse<Resp, NodeData>> {
    let game = s.game.clone();
    let form = game.item_form(item);
    // Only when what extracts it cares (a Water Extractor doesn't).
    let on_it: Vec<String> = graph[node_id]
        .outputs
        .iter()
        .flat_map(|(_, o)| graph.output_connections(*o))
        .filter_map(|i| match &graph[graph[i].node].user_data.kind {
            NodeKind::Extractor { class, .. } => Some(class.clone()),
            _ => None,
        })
        .collect();
    // Leaves and the like (the HUB's fuel) come by hand: no purity.
    let raw = game.items.get(item).is_some_and(|i| i.raw);
    let uses = if on_it.is_empty() {
        raw && game.extraction.extractors.iter().any(|e| e.can_extract(item, form) && e.kind != ExtractorKind::Water)
    } else {
        on_it.iter().any(|c| has_purity(&game, c))
    };
    if !uses {
        return vec![];
    }
    let (r_div, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().rect_filled(r_div, 0.0, DIVIDER);
    let mut p = purity;
    let tip = |x: Purity| format!("A {} node gives {}x the output.", x.label(), fmt_num(x.multiplier()));
    let (ti, tn, tp) = (tip(Purity::Impure), tip(Purity::Normal), tip(Purity::Pure));
    if crate::widgets::tabs(ui, &mut p, &[(Purity::Impure, "Impure", &ti), (Purity::Normal, "Normal", &tn), (Purity::Pure, "Pure", &tp)]) {
        return vec![egui_node_graph2::NodeResponse::User(Resp::Purity(node_id, p))];
    }
    vec![]
}

pub fn find<'a>(game: &'a GameData, class: &str) -> Option<&'a Extractor> {
    game.extraction.extractors.iter().find(|e| e.class == class)
}

/// Per minute one machine makes on a node of `purity` at 100%.
pub fn per_machine(game: &GameData, class: &str, purity: Purity) -> f64 {
    find(game, class).map_or(0.0, |e| e.per_min(purity, 1.0))
}

/// Whether purity matters for it (everything but Water Extractors).
pub fn has_purity(game: &GameData, class: &str) -> bool {
    find(game, class).is_some_and(|e| e.kind != ExtractorKind::Water)
}

/// MW of `count` machines each at `clock` (1.0 = 100%).
pub fn power(e: &Extractor, count: u32, clock: f64) -> f64 {
    count as f64 * e.power_mw * clock.max(0.0).powf(e.power_exponent)
}

/// "3 Miner Mk.1 on Normal nodes at 93.75%"
pub fn line(game: &GameData, class: &str, purity: Purity, setup: &fb_model::MachineSetup) -> String {
    let name = find(game, class).map_or(class, |e| e.name.as_str());
    let mut t = format!("{} {name}", setup.count);
    if has_purity(game, class) {
        t.push_str(&format!(" on {} nodes", purity.label()));
    }
    t.push_str(&format!(" at {}%", fmt_num(setup.clock * 100.0)));
    t
}

/// Under an extractor's ports: its building, how many to build, the node
/// purity, and its power.
pub fn footer(ui: &mut Ui, s: &mut UserState, graph: &FactoryGraph, node_id: NodeId, item: &str, class: &str, purity: Purity) {
    let game = s.game.clone();
    let Some(e) = find(&game, class) else {
        ui.colored_label(DANGER, "This extractor is not in the game data.");
        return;
    };
    let setup = machine_setup(&game, graph, node_id);
    let mw = power(e, setup.count, setup.clock);
    let (r_div, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().rect_filled(r_div, 0.0, DIVIDER);
    let each = fmt_rate_unit(e.per_min(purity, 1.0), game.item_form(item));
    let node = if has_purity(&game, class) { format!("{} node · {each} each at 100%", purity.label()) } else { format!("{each} each at 100%") };
    let extra = extras_hint(&setup);
    crate::widgets::split_row_with(ui, if extra.is_some() { 60.0 } else { 48.0 }, s, |ui, s| {
        ui.label(RichText::new(format!("{} MW", fmt_num(mw))).small().color(TEXT_DIM));
        s.ui_icon(ui, crate::assets::UiIcon::Power, 14.0, TEXT_DIM);
    }, |ui, s| {
        s.item_icon(ui, &e.icon, 32.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            crate::widgets::name_label(ui, RichText::new(&e.name).color(TEXT));
            if let Some(line) = build_line(graph, node_id, &setup) {
                crate::widgets::name_label(ui, RichText::new(line).small().color(TEXT_DIM));
            }
            crate::widgets::name_label(ui, RichText::new(node).small().color(TEXT_DIM));
            if let Some(extra) = &extra {
                crate::widgets::name_label(ui, RichText::new(extra).small().color(ACCENT));
            }
        });
    });
}

/// A resource node's wire into an extractor: not a belt or pipe, as the
/// extractor stands on the resource node and takes from it directly. Only
/// what it puts out goes on a conveyor.
pub fn from_the_ground(g: &FactoryGraph, output: egui_node_graph2::OutputId, input: egui_node_graph2::InputId) -> bool {
    let (Some(o), Some(i)) = (g.try_get_output(output), g.try_get_input(input)) else { return false };
    matches!(g[o.node].user_data.kind, NodeKind::Resource { .. }) && matches!(g[i.node].user_data.kind, NodeKind::Extractor { .. })
}

/// MW per unit per minute of each raw resource, extracted as the settings
/// say: what the solver counts for extractors' power.
pub fn mw_per_unit(game: &GameData, settings: &ExtractionSettings) -> std::collections::BTreeMap<String, f64> {
    game.items
        .iter()
        .filter(|(_, i)| i.raw)
        .filter_map(|(c, _)| {
            let p = plan_resource(game, c, 60.0, settings)?;
            let e = find(game, &p.class)?;
            let per = e.per_min(p.purity.unwrap_or_default(), p.clock);
            (per > 0.0).then(|| (c.clone(), power(e, 1, p.clock) / per))
        })
        .collect()
}

/// Resource nodes wired straight into machines (Plan factory's, and older
/// projects') get an extractor between, chosen by the Overview's extractor
/// settings (miner tier, purity, clock speed): as many whole extractors as
/// the rate the resource gave needs, at that clock speed, as they'd be
/// built in game (any they extract beyond what's taken backs up). The
/// resource goes into it and what it extracts goes on to the machines.
/// Returns true if anything changed.
pub fn add_to_plan(state: &mut EditorState, user: &mut UserState, settings: &ExtractionSettings) -> bool {
    let game = user.game.clone();
    let z = state.pan_zoom.zoom;
    let g = &state.graph;
    let direct = |n: NodeId| g[n].outputs.iter().flat_map(|(_, o)| g.output_connections(*o)).any(|i| !matches!(g[g[i].node].user_data.kind, NodeKind::Extractor { .. }));
    let resources: Vec<(NodeId, String, f64)> = g
        .iter_nodes()
        .filter(|n| direct(*n))
        .filter_map(|n| match &g[n].user_data.kind {
            // Only what an extractor takes (not leaves fed by hand).
            NodeKind::Resource { item, .. } if game.items.get(item).is_some_and(|i| i.raw) && game.extraction.extractors.iter().any(|e| e.can_extract(item, game.item_form(item))) => Some((n, item.clone(), input_number(g, n, RATE))),
            _ => None,
        })
        .collect();
    let changed = !resources.is_empty();
    for (r, item, rate) in resources {
        let Some(p) = plan_resource(&game, &item, rate, settings) else { continue };
        let purity = p.purity.unwrap_or_default();
        if let NodeKind::Resource { purity: rp, .. } = &mut state.graph[r].user_data.kind {
            *rp = purity;
        }
        let per = per_machine(&game, &p.class, purity) * p.clock;
        if per <= 0.0 || p.count == 0 {
            continue;
        }
        let at = state.node_positions.get(r).copied().unwrap_or_default() + vec2(360.0 * z, 0.0);
        let x = crate::project::add_node_to(state, user, Template::Extractor { item: item.clone(), class: p.class.clone(), purity }, at);
        let g = &mut state.graph;
        let machines = p.count as f64;
        for (name, v) in [(MACHINES, machines), (CLOCK, p.clock * 100.0)] {
            if let Ok(i) = g[x].get_input(name) {
                g[i].value.set_number(v);
            }
        }
        // What the resource fed now comes from the extractor.
        let Some(out) = g[r].outputs.first().map(|(_, o)| *o) else { continue };
        let (Some(x_in), Some(x_out)) = (g[x].inputs.iter().map(|(_, i)| *i).find(|i| g[*i].typ.0 == item), g[x].outputs.first().map(|(_, o)| *o)) else { continue };
        for i in g.output_connections(out).into_iter().filter(|i| *i != x_in) {
            if matches!(g[g[i].node].user_data.kind, NodeKind::Extractor { .. }) {
                continue;
            }
            let pos = g.connections(i).iter().position(|o| *o == out).unwrap_or(0);
            g.remove_connection(i, out);
            g.add_connection(x_out, i, pos);
        }
        g.add_connection(out, x_in, 0);
    }
    changed
}
