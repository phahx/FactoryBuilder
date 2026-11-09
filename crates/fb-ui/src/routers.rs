//! Mergers, splitters and pipeline junctions: small nodes, drawn with the
//! game's own icons, that gather or share out the wires of one item, the
//! way they do belts and pipes in game.
//!
//! They only shape the wiring. In the simulation every port wired together
//! is already one shared belt (see fb_engine::sim), so wiring through them
//! is the same as wiring their suppliers straight to their consumers:
//! [`route`] works out those direct links, and which of them each wire
//! through a router carries, so its flow (and belt label) is right.

use crate::fmt::fmt_rate_unit;
use crate::graph::*;
use crate::theme::*;
use egui::{vec2, RichText, Ui, Vec2};
use egui_node_graph2::{InputId, NodeId, OutputId};
use std::collections::{BTreeSet, HashMap, HashSet};

/// A router node's size (unzoomed).
pub const SIZE: Vec2 = vec2(36.0, 36.0);
/// Belts a Conveyor Merger joins, and a Conveyor Splitter shares out to;
/// pipes on a Pipeline Junction. From the game's Docs: "Merges up to three
/// Conveyor Belts into one", "Splits one Conveyor Belt into two or three",
/// "allowing it to be split up to 4 ways".
pub const MERGER_INPUTS: usize = 3;
pub const SPLITTER_OUTPUTS: usize = 3;
pub const JUNCTION_PIPES: usize = 4;

/// Whether router `kind` takes one more wire on `port` (`node`'s).
pub fn accepts(game: &fb_model::GameData, kind: &NodeKind, g: &FactoryGraph, node: NodeId, port: egui_node_graph2::AnyParameterId) -> bool {
    use egui_node_graph2::AnyParameterId as P;
    let on = |p: P| match p {
        P::Input(i) => g.connections(i).len(),
        P::Output(o) => g.output_connections(o).len(),
    };
    match kind {
        // A pump sits in one pipe.
        NodeKind::Pump { .. } => on(port) < 1,
        NodeKind::Merger { item } | NodeKind::Splitter { item } if fluid(game, item) => crate::power_net::wire_count(g, node) < JUNCTION_PIPES,
        NodeKind::Merger { .. } => on(port) < if matches!(port, P::Input(_)) { MERGER_INPUTS } else { 1 },
        NodeKind::Splitter { .. } => on(port) < if matches!(port, P::Input(_)) { 1 } else { SPLITTER_OUTPUTS },
        _ => true,
    }
}

pub fn is_router(kind: &NodeKind) -> bool {
    matches!(kind, NodeKind::Merger { .. } | NodeKind::Splitter { .. } | NodeKind::Pump { .. })
}

/// Pipeline Pumps, from the game's Docs: class, name, and the Head Lift its
/// description gives (power is the Docs' mPowerConsumption, in GameData).
pub const PUMPS: [(&str, &str, u32); 2] = [("Build_PipelinePump_C", "Pipeline Pump Mk.1", 20), ("Build_PipelinePumpMk2_C", "Pipeline Pump Mk.2", 50)];

fn fluid(game: &fb_model::GameData, item: &str) -> bool {
    game.item_form(item).is_fluid()
}

/// The game's name for it: pipes have one junction for both jobs.
pub fn name(game: &fb_model::GameData, kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::Pump { class, .. } => PUMPS.iter().find(|p| p.0 == class).map_or("Pipeline Pump", |p| p.1),
        NodeKind::Merger { item } | NodeKind::Splitter { item } if fluid(game, item) => "Pipeline Junction",
        NodeKind::Merger { .. } => "Merger",
        _ => "Splitter",
    }
}

/// The game's icon for it (icon class).
pub fn icon(game: &fb_model::GameData, kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::Pump { class, .. } if class == PUMPS[1].0 => "Desc_PipelinePumpMk2_C",
        NodeKind::Pump { .. } => "Desc_PipelinePump_C",
        NodeKind::Merger { item } | NodeKind::Splitter { item } if fluid(game, item) => "Desc_PipelineJunction_Cross_C",
        NodeKind::Merger { .. } => "Desc_ConveyorAttachmentMerger_C",
        _ => "Desc_ConveyorAttachmentSplitter_C",
    }
}

/// Wires through routers, as direct links: `pairs` are (supplier output,
/// consumer input) joined through routers; `carries` says which pairs (by
/// index) each wire that touches a router carries.
#[derive(Default)]
pub struct Routes {
    pub pairs: Vec<(OutputId, InputId)>,
    pub carries: HashMap<(InputId, OutputId), Vec<usize>>,
}

pub fn route(g: &FactoryGraph) -> Routes {
    let router = |n: NodeId| is_router(&g[n].user_data.kind);
    let routers: Vec<NodeId> = g.iter_nodes().filter(|n| router(*n)).collect();
    if routers.is_empty() {
        return Routes::default();
    }
    let ins = |r: NodeId| g[r].inputs.first().map(|(_, i)| *i);
    let outs = |r: NodeId| g[r].outputs.first().map(|(_, o)| *o);
    // Real suppliers reaching each router, and real consumers it reaches.
    let walk = |start: NodeId, up: bool| -> (BTreeSet<OutputId>, BTreeSet<InputId>) {
        let (mut sup, mut con) = (BTreeSet::new(), BTreeSet::new());
        let mut seen = HashSet::from([start]);
        let mut stack = vec![start];
        while let Some(r) = stack.pop() {
            if up {
                for o in ins(r).map(|i| g.connections(i)).unwrap_or_default() {
                    let n = g[o].node;
                    if !router(n) {
                        sup.insert(o);
                    } else if seen.insert(n) {
                        stack.push(n);
                    }
                }
            } else {
                for i in outs(r).map(|o| g.output_connections(o)).unwrap_or_default() {
                    let n = g[i].node;
                    if !router(n) {
                        con.insert(i);
                    } else if seen.insert(n) {
                        stack.push(n);
                    }
                }
            }
        }
        (sup, con)
    };
    let up: HashMap<NodeId, BTreeSet<OutputId>> = routers.iter().map(|r| (*r, walk(*r, true).0)).collect();
    let down: HashMap<NodeId, BTreeSet<InputId>> = routers.iter().map(|r| (*r, walk(*r, false).1)).collect();
    let mut index: HashMap<(OutputId, InputId), usize> = HashMap::new();
    let mut routes = Routes::default();
    let mut pair = |routes: &mut Routes, o: OutputId, i: InputId| -> usize {
        *index.entry((o, i)).or_insert_with(|| {
            routes.pairs.push((o, i));
            routes.pairs.len() - 1
        })
    };
    for (i, o) in g.iter_connections() {
        let (from, to) = (g[o].node, g[i].node);
        let (sources, sinks): (Vec<OutputId>, Vec<InputId>) = match (router(from), router(to)) {
            (false, false) => continue,
            (false, true) => (vec![o], down[&to].iter().copied().collect()),
            (true, false) => (up[&from].iter().copied().collect(), vec![i]),
            (true, true) => (up[&from].iter().copied().collect(), down[&to].iter().copied().collect()),
        };
        let carried: Vec<usize> = sources.iter().flat_map(|s| sinks.iter().map(move |c| (*s, *c))).map(|(s, c)| pair(&mut routes, s, c)).collect();
        routes.carries.insert((i, o), carried);
    }
    routes
}

/// Turns every belt (or pipe) that several machines share into mergers and
/// splitters, as it would be built in game: the suppliers' wires join
/// through mergers (three at a time) into one, which splitters (three ways
/// at a time) share out to the consumers. For fluids, Pipeline Junctions do
/// both. Wires between one supplier and one consumer stay as they are, and
/// flows don't change (see the module notes).
pub fn tidy(state: &mut EditorState, user: &mut UserState) {
    use std::collections::BTreeMap;
    let g = &state.graph;
    // Belts: the item wires joined through shared ports, by union-find.
    let wires: Vec<(InputId, OutputId)> = g
        .iter_connections()
        .filter(|(i, o)| matches!(g[*i].value, PortValue::Item(_)) && !is_router(&g[g[*o].node].user_data.kind) && !is_router(&g[g[*i].node].user_data.kind))
        // Not belts: extractors take straight from the resource.
        .filter(|(i, o)| !crate::extractors::from_the_ground(g, *o, *i))
        .collect();
    let mut parent: Vec<usize> = (0..wires.len()).collect();
    fn find(p: &mut [usize], mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    let mut by_in: HashMap<InputId, usize> = HashMap::new();
    let mut by_out: HashMap<OutputId, usize> = HashMap::new();
    for (k, (i, o)) in wires.iter().enumerate() {
        for first in [by_in.get(i).copied(), by_out.get(o).copied()].into_iter().flatten() {
            let (a, b) = (find(&mut parent, first), find(&mut parent, k));
            parent[a] = b;
        }
        by_in.entry(*i).or_insert(k);
        by_out.entry(*o).or_insert(k);
    }
    let mut belts: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for k in 0..wires.len() {
        let r = find(&mut parent, k);
        belts.entry(r).or_default().push(k);
    }
    let z = state.pan_zoom.zoom;
    let y_of = |state: &EditorState, n: NodeId| state.node_positions.get(n).map_or(0.0, |p| p.y);
    for members in belts.into_values() {
        let g = &state.graph;
        let mut sources: Vec<OutputId> = Vec::new();
        let mut sinks: Vec<InputId> = Vec::new();
        for &k in &members {
            let (i, o) = wires[k];
            if !sources.contains(&o) {
                sources.push(o);
            }
            if !sinks.contains(&i) {
                sinks.push(i);
            }
        }
        if sources.len() < 2 && sinks.len() < 2 {
            continue;
        }
        let item = g[sources[0]].typ.0.clone();
        // Top to bottom, so the trees don't cross themselves.
        sources.sort_by(|a, b| y_of(state, g[*a].node).total_cmp(&y_of(state, g[*b].node)));
        sinks.sort_by(|a, b| y_of(state, g[*a].node).total_cmp(&y_of(state, g[*b].node)));
        for &k in &members {
            let (i, o) = wires[k];
            state.graph.remove_connection(i, o);
        }
        let mid = |state: &EditorState, ns: &[NodeId]| {
            let ps: Vec<egui::Pos2> = ns.iter().filter_map(|n| state.node_positions.get(*n).copied()).collect();
            let n = ps.len().max(1) as f32;
            egui::pos2(ps.iter().map(|p| p.x).sum::<f32>() / n, ps.iter().map(|p| p.y).sum::<f32>() / n)
        };
        let wire = |state: &mut EditorState, o: OutputId, i: InputId| {
            let pos = state.graph.connections(i).len();
            state.graph.add_connection(o, i, pos);
        };
        let router_ports = |state: &EditorState, n: NodeId| (state.graph[n].inputs[0].1, state.graph[n].outputs[0].1);
        // Join the suppliers, three at a time.
        let mut level = sources;
        while level.len() > 1 {
            let groups = level.len().div_ceil(MERGER_INPUTS);
            let size = level.len().div_ceil(groups);
            let mut next = Vec::new();
            for chunk in level.chunks(size) {
                if chunk.len() == 1 {
                    next.push(chunk[0]);
                    continue;
                }
                let at = mid(state, &chunk.iter().map(|o| state.graph[*o].node).collect::<Vec<_>>()) + egui::vec2(200.0 * z, 0.0);
                let m = crate::project::add_node_to(state, user, Template::Merger(item.clone()), at);
                let (m_in, m_out) = router_ports(state, m);
                for o in chunk {
                    wire(state, *o, m_in);
                }
                next.push(m_out);
            }
            level = next;
        }
        // Share out to the consumers, three ways at a time.
        let fluid = user.game.item_form(&item).is_fluid();
        fn split(state: &mut EditorState, user: &mut UserState, item: &str, fluid: bool, from: OutputId, to: &[InputId], z: f32) {
            if to.len() == 1 {
                let pos = state.graph.connections(to[0]).len();
                state.graph.add_connection(from, to[0], pos);
                return;
            }
            let ns: Vec<egui::Pos2> = to.iter().filter_map(|i| state.node_positions.get(state.graph[*i].node).copied()).collect();
            let n = ns.len().max(1) as f32;
            let at = egui::pos2(ns.iter().map(|p| p.x).sum::<f32>() / n - 200.0 * z, ns.iter().map(|p| p.y).sum::<f32>() / n);
            let template = if fluid { Template::Merger(item.to_string()) } else { Template::Splitter(item.to_string()) };
            let s = crate::project::add_node_to(state, user, template, at);
            let (s_in, s_out) = (state.graph[s].inputs[0].1, state.graph[s].outputs[0].1);
            let pos = state.graph.connections(s_in).len();
            state.graph.add_connection(from, s_in, pos);
            let groups = to.len().min(SPLITTER_OUTPUTS);
            let size = to.len().div_ceil(groups);
            for chunk in to.chunks(size) {
                split(state, user, item, fluid, s_out, chunk, z);
            }
        }
        split(state, user, &item, fluid, level[0], &sinks, z);
    }
}

/// A router: its icon, and what goes through it on hover.
pub fn router_ui(ui: &mut Ui, rect: egui::Rect, node_id: NodeId, kind: &NodeKind, s: &mut UserState) {
    let game = s.game.clone();
    let (NodeKind::Merger { item } | NodeKind::Splitter { item } | NodeKind::Pump { item, .. }) = kind else { return };
    match s.assets.as_mut().and_then(|a| a.item_uri(icon(&game, kind))) {
        Some(uri) => {
            egui::Image::new(uri).paint_at(ui, rect);
        }
        None => {
            ui.painter().circle_filled(rect.center(), rect.width() * 0.3, s.item_color(item));
        }
    }
    if ui.rect_contains_pointer(rect) && !ui.input(|i| i.pointer.any_down()) {
        let through = s.ports.get(&(node_id, ROUTED.to_string())).map(|p| p.have).unwrap_or(0.0);
        egui::show_tooltip_at_pointer(ui.ctx(), ui.layer_id(), egui::Id::new(("router_tip", node_id)), |ui| {
            crate::widgets::tooltip_title(ui, name(&game, kind));
            crate::widgets::tip_item(ui, s.assets.as_mut().expect("assets are set in the app"), item, game.item_name(item), &fmt_rate_unit(through, game.item_form(item)));
            let what = match kind {
                NodeKind::Pump { class, .. } => {
                    let lift = PUMPS.iter().find(|p| p.0 == class).map_or(0, |p| p.2);
                    let mw = game.buildable_power.get(class).copied().unwrap_or(0.0);
                    format!("Head Lift up to {lift} m, {} MW.", crate::fmt::fmt_num(mw))
                }
                _ => match name(&game, kind) {
                    "Merger" => "Joins belts into one.".to_string(),
                    "Splitter" => "Shares a belt out over several.".to_string(),
                    _ => "Joins and shares out pipes.".to_string(),
                },
            };
            ui.label(RichText::new(what).small().color(TEXT_DIM));
        });
    }
}

/// Port stat key on a router: what goes through it per minute.
pub const ROUTED: &str = "routed";
