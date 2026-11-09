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
        NodeKind::Merger { item } | NodeKind::Splitter { item } if fluid(game, item) => crate::power_net::wire_count(g, node) < JUNCTION_PIPES,
        NodeKind::Merger { .. } => on(port) < if matches!(port, P::Input(_)) { MERGER_INPUTS } else { 1 },
        NodeKind::Splitter { .. } => on(port) < if matches!(port, P::Input(_)) { 1 } else { SPLITTER_OUTPUTS },
        _ => true,
    }
}

pub fn is_router(kind: &NodeKind) -> bool {
    matches!(kind, NodeKind::Merger { .. } | NodeKind::Splitter { .. })
}

fn fluid(game: &fb_model::GameData, item: &str) -> bool {
    game.item_form(item).is_fluid()
}

/// The game's name for it: pipes have one junction for both jobs.
pub fn name(game: &fb_model::GameData, kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::Merger { item } | NodeKind::Splitter { item } if fluid(game, item) => "Pipeline Junction",
        NodeKind::Merger { .. } => "Merger",
        _ => "Splitter",
    }
}

/// The game's icon for it (icon class).
pub fn icon(game: &fb_model::GameData, kind: &NodeKind) -> &'static str {
    match kind {
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

/// A router: its icon, and what goes through it on hover.
pub fn router_ui(ui: &mut Ui, rect: egui::Rect, node_id: NodeId, kind: &NodeKind, s: &mut UserState) {
    let game = s.game.clone();
    let (NodeKind::Merger { item } | NodeKind::Splitter { item }) = kind else { return };
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
            let what = match name(&game, kind) {
                "Merger" => "Joins belts into one.",
                "Splitter" => "Shares a belt out over several.",
                _ => "Joins and shares out pipes.",
            };
            ui.label(RichText::new(what).small().color(TEXT_DIM));
        });
    }
}

/// Port stat key on a router: what goes through it per minute.
pub const ROUTED: &str = "routed";
