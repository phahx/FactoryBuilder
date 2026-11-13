//! Power on the master graph (View › Power connections): which factories
//! make power and which use it, wired through power poles.
//!
//! A factory that makes more power than it uses has a Power output for the
//! difference; one that uses more than it makes has a Power input. Wires
//! join them, through poles: small nodes, drawn with the game's Power Pole
//! icon, that only route wires (like reroute nodes), so a circuit's wires
//! run short, pole to pole, instead of all to one place. Everything wired
//! together is one circuit; hovering a pole shows its figures.
//!
//! The project keeps its poles and wires with the master graph's plan
//! (`Planner::power_net`), so hiding them loses nothing. "Make project from
//! save" adds them from the save's circuits (map_import.rs).

use crate::fmt::fmt_num;
use crate::graph::*;
use crate::theme::*;
use egui::{vec2, Align, Color32, Layout, RichText, Ui, Vec2};
use egui_node_graph2::{InputId, InputParamKind, NodeId, OutputId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// Port type of every power port: anything can be wired to anything.
pub const POWER: &str = "@power";
/// A factory block's power port (in or out; a factory has one at most).
pub const PORT: &str = "Power";
/// A pole's two ports (not shown).
const POLE_IN: &str = "In";
const POLE_OUT: &str = "Out";
/// Port stats on a pole: its circuit's MW made and used.
pub const MADE: &str = "made";
pub const USED: &str = "used";
/// Most Power Line connections a Power Pole Mk.1, Mk.2 and Mk.3 take, from
/// the game's Docs ("Allows for up to 4 / 7 / 10 Power Line connections").
pub const POLE_WIRES: [usize; 3] = [4, 7, 10];
/// Their icons.
const POLE_ICONS: [&str; 3] = ["Desc_PowerPoleMk1_C", "Desc_PowerPoleMk2_C", "Desc_PowerPoleMk3_C"];
/// Port stat key on a pole: how many wires it has.
pub const WIRES: &str = "wires";

/// Most wires a pole of `tier` (1 to 3) takes.
pub fn max_wires(tier: u8) -> usize {
    POLE_WIRES[tier.clamp(1, 3) as usize - 1]
}

/// The lowest tier that takes `wires`.
pub fn tier_for(wires: usize) -> u8 {
    (1..=3).find(|t| max_wires(*t) >= wires).unwrap_or(3)
}

fn mk1() -> u8 {
    1
}
/// A pole node's size (unzoomed).
pub const POLE_SIZE: Vec2 = vec2(36.0, 40.0);
/// Less than this (MW) and a factory neither sends nor needs power.
const MIN_MW: f64 = 0.01;
/// Power wires and ports: the grey the map draws power lines in.
pub const WIRE: Color32 = Color32::from_gray(0xB0);

pub fn is_power(port_type: &str) -> bool {
    port_type == POWER
}

/// One end of a power wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum End {
    Factory(u32),
    Pole(u32),
}

/// A power wire between two ends. Power has no set way along it: a wire to
/// a factory leaves its Power output when it makes more than it uses and
/// enters its Power input when it uses more (see [`wire_for`]); between
/// poles it runs `from`'s output to `to`'s input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PowerLink {
    pub from: End,
    pub to: End,
}

impl PowerLink {
    /// The same for either order of the ends.
    fn key(&self) -> (End, End) {
        (self.from.min(self.to), self.from.max(self.to))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pole {
    pub id: u32,
    /// On the master graph (unzoomed graph units).
    pub x: f32,
    pub y: f32,
    /// Power Pole Mk.1, 2 or 3: how many wires it takes.
    #[serde(default = "mk1")]
    pub tier: u8,
}

/// The project's power poles and wires.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PowerNet {
    #[serde(default)]
    pub poles: Vec<Pole>,
    #[serde(default)]
    pub links: Vec<PowerLink>,
}

impl PowerNet {
    pub fn is_empty(&self) -> bool {
        self.poles.is_empty() && self.links.is_empty()
    }

    /// Adds a pole at (`x`, `y`); returns its id.
    pub fn add_pole(&mut self, x: f32, y: f32) -> u32 {
        let id = self.poles.iter().map(|p| p.id).max().unwrap_or(0) + 1;
        self.poles.push(Pole { id, x, y, tier: 1 });
        id
    }
}

/// A factory's (surplus, need) in MW from what it uses and makes.
pub fn balance(uses_mw: f64, makes_mw: f64) -> (f64, f64) {
    let net = makes_mw - uses_mw;
    (net.max(0.0), (-net).max(0.0))
}

fn factory_balance(g: &FactoryGraph, node: NodeId) -> (f64, f64) {
    match g[node].user_data.kind {
        NodeKind::Factory { uses_mw, makes_mw, .. } => balance(uses_mw, makes_mw),
        _ => (0.0, 0.0),
    }
}

pub fn pole_node(g: &FactoryGraph, id: u32) -> Option<NodeId> {
    g.iter_nodes().find(|n| matches!(g[*n].user_data.kind, NodeKind::Pole { id: x } if x == id))
}

pub fn pole_of(g: &FactoryGraph, node: NodeId) -> Option<u32> {
    match g.nodes.get(node)?.user_data.kind {
        NodeKind::Pole { id } => Some(id),
        _ => None,
    }
}

fn end_of(g: &FactoryGraph, node: NodeId) -> Option<End> {
    match g.nodes.get(node)?.user_data.kind {
        NodeKind::Factory { id, .. } => Some(End::Factory(id)),
        NodeKind::Pole { id } => Some(End::Pole(id)),
        _ => None,
    }
}

/// A node's power output / input, if it has one.
fn power_out(g: &FactoryGraph, node: NodeId) -> Option<OutputId> {
    g[node].outputs.iter().map(|(_, o)| *o).find(|o| g[*o].typ.0 == POWER)
}

fn power_in(g: &FactoryGraph, node: NodeId) -> Option<InputId> {
    g[node].inputs.iter().map(|(_, i)| *i).find(|i| g[*i].typ.0 == POWER)
}

/// A pole's ports (from `build_node`).
pub fn add_pole_ports(g: &mut FactoryGraph, node: NodeId) {
    g.add_wide_input_param(node, POLE_IN.into(), ItemType(POWER.into()), PortValue::Power, InputParamKind::ConnectionOnly, None, false);
    g.add_output_param(node, POLE_OUT.into(), ItemType(POWER.into()));
}

/// The ports a link's wire joins, when both ends can take it now: `from`'s
/// output to `to`'s input, or the other way round.
fn wire_for(g: &FactoryGraph, at: &BTreeMap<End, NodeId>, l: &PowerLink) -> Option<(OutputId, InputId)> {
    let (a, b) = (*at.get(&l.from)?, *at.get(&l.to)?);
    let forward = power_out(g, a).zip(power_in(g, b));
    forward.or_else(|| power_out(g, b).zip(power_in(g, a)))
}

/// Every node an end can be: factory blocks and poles, by end.
fn ends(g: &FactoryGraph) -> BTreeMap<End, NodeId> {
    g.iter_nodes().filter_map(|n| Some((end_of(g, n)?, n))).collect()
}

/// The power wires drawn now, as links.
fn drawn_links(g: &FactoryGraph) -> BTreeSet<PowerLink> {
    g.iter_connections()
        .filter(|(i, _)| g[*i].typ.0 == POWER)
        .filter_map(|(i, o)| Some(PowerLink { from: end_of(g, g[o].node)?, to: end_of(g, g[i].node)? }))
        .collect()
}

/// Brings the master graph's power ports, poles and power wires in line
/// with `net` (or takes them away when `show` is off). Wires to factories
/// or poles that no longer exist go.
pub fn sync(state: &mut EditorState, user: &mut UserState, net: &mut PowerNet, show: bool) {
    // Anything wired since the last look is kept, not undone.
    if show {
        read_back(state, net);
    }
    let factories: BTreeSet<u32> = ends(&state.graph).keys().filter_map(|e| if let End::Factory(f) = e { Some(*f) } else { None }).collect();
    let poles: BTreeSet<u32> = net.poles.iter().map(|p| p.id).collect();
    let exists = |e: &End| match e {
        End::Factory(f) => factories.contains(f),
        End::Pole(p) => poles.contains(p),
    };
    if net.links.iter().any(|l| !exists(&l.from) || !exists(&l.to)) {
        net.links.retain(|l| exists(&l.from) && exists(&l.to));
    }

    // Power ports on factory blocks, after the item ports (the evaluation
    // counts on that).
    let g = &mut state.graph;
    for node in g.iter_nodes().collect::<Vec<_>>() {
        if !matches!(g[node].user_data.kind, NodeKind::Factory { .. }) {
            continue;
        }
        let (surplus, need) = factory_balance(g, node);
        match (power_out(g, node), show && surplus > MIN_MW) {
            (Some(o), false) => g.remove_output_param(o),
            (None, true) => {
                g.add_output_param(node, PORT.into(), ItemType(POWER.into()));
            }
            _ => {}
        }
        match (power_in(g, node), show && need > MIN_MW) {
            (Some(i), false) => g.remove_input_param(i),
            (None, true) => {
                g.add_wide_input_param(node, PORT.into(), ItemType(POWER.into()), PortValue::Power, InputParamKind::ConnectionOnly, None, true);
            }
            _ => {}
        }
    }

    // Poles: one node each while shown.
    user.pole_tiers = net.poles.iter().map(|p| (p.id, p.tier.clamp(1, 3))).collect();
    let mut seen = BTreeSet::new();
    for node in state.graph.iter_nodes().collect::<Vec<_>>() {
        let NodeKind::Pole { id } = state.graph[node].user_data.kind else { continue };
        if !(show && poles.contains(&id) && seen.insert(id)) {
            remove_node(state, node);
        }
    }
    if !show {
        return;
    }
    let z = state.pan_zoom.zoom.max(0.01);
    for p in &net.poles {
        if pole_node(&state.graph, p.id).is_none() {
            crate::project::add_node_to(state, user, Template::Pole(p.id), egui::pos2(p.x * z, p.y * z));
        }
    }

    // Wires.
    let g = &mut state.graph;
    let at = ends(g);
    let wanted: BTreeSet<(OutputId, InputId)> = net.links.iter().filter_map(|l| wire_for(g, &at, l)).collect();
    let drawn: Vec<(InputId, OutputId)> = g.iter_connections().filter(|(i, _)| g[*i].typ.0 == POWER).collect();
    for (i, o) in &drawn {
        if !wanted.contains(&(*o, *i)) {
            g.remove_connection(*i, *o);
        }
    }
    for (o, i) in wanted {
        if !g.connections(i).contains(&o) {
            let pos = g.connections(i).len();
            g.add_connection(o, i, pos);
        }
    }
}

fn remove_node(state: &mut EditorState, node: NodeId) {
    state.graph.remove_node(node);
    state.node_positions.remove(node);
    state.node_order.retain(|n| *n != node);
    state.selected_nodes.retain(|n| *n != node);
}

/// Takes what was done on the master graph back into `net`: where poles
/// are, and the power wires. A wire to a factory that has no power port
/// right now (or to a pole not drawn) is kept: it shows again when the
/// port does. Poles whose node isn't there are left alone: they're deleted
/// only on purpose ([`remove_poles`]).
pub fn read_back(state: &EditorState, net: &mut PowerNet) {
    let g = &state.graph;
    let z = state.pan_zoom.zoom.max(0.01);
    for p in &mut net.poles {
        let Some(at) = pole_node(g, p.id).and_then(|n| state.node_positions.get(n)) else { continue };
        let (x, y) = (at.x / z, at.y / z);
        if (x - p.x).abs() > 0.01 || (y - p.y).abs() > 0.01 {
            (p.x, p.y) = (x, y);
        }
    }
    let at = ends(g);
    let mut links: BTreeMap<(End, End), PowerLink> = net.links.iter().filter(|l| wire_for(g, &at, l).is_none()).map(|l| (l.key(), *l)).collect();
    for l in drawn_links(g) {
        links.insert(l.key(), l);
    }
    let old: BTreeSet<(End, End)> = net.links.iter().map(PowerLink::key).collect();
    if links.keys().copied().collect::<BTreeSet<_>>() != old || links.len() != net.links.len() {
        net.links = links.into_values().collect();
    }
}

/// Deletes poles `ids`, their nodes and their wires.
pub fn remove_poles(state: &mut EditorState, net: &mut PowerNet, ids: &[u32]) {
    net.poles.retain(|p| !ids.contains(&p.id));
    net.links.retain(|l| ![l.from, l.to].iter().any(|e| matches!(e, End::Pole(p) if ids.contains(p))));
    for id in ids {
        if let Some(node) = pole_node(&state.graph, *id) {
            remove_node(state, node);
        }
    }
}

/// Circuits: the nodes joined by power wires, as a component index per node.
fn circuits(g: &FactoryGraph) -> HashMap<NodeId, usize> {
    fn find(p: &mut HashMap<NodeId, NodeId>, x: NodeId) -> NodeId {
        let mut r = x;
        while let Some(&up) = p.get(&r).filter(|up| **up != r) {
            r = up;
        }
        p.insert(x, r);
        r
    }
    let mut parent: HashMap<NodeId, NodeId> = HashMap::new();
    for (i, o) in g.iter_connections().filter(|(i, _)| g[*i].typ.0 == POWER) {
        let (a, b) = (g[o].node, g[i].node);
        parent.entry(a).or_insert(a);
        parent.entry(b).or_insert(b);
        let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
        if ra != rb {
            parent.insert(ra, rb);
        }
    }
    let mut index: HashMap<NodeId, usize> = HashMap::new();
    let nodes: Vec<NodeId> = parent.keys().copied().collect();
    let mut out = HashMap::new();
    for n in nodes {
        let r = find(&mut parent, n);
        let next = index.len();
        out.insert(n, *index.entry(r).or_insert(next));
    }
    out
}

/// Power figures for the master graph: what each factory sends or needs,
/// each pole's circuit, and a note for circuits that are short.
pub fn evaluate(g: &FactoryGraph, ports: &mut HashMap<(NodeId, String), PortStat>, issues: &mut Vec<(NodeId, String)>) {
    let circuit = circuits(g);
    let n = circuit.values().max().map_or(0, |m| m + 1);
    let (mut made, mut used) = (vec![0.0; n], vec![0.0; n]);
    let mut takers: Vec<Vec<NodeId>> = vec![Vec::new(); n];
    for (&node, &c) in &circuit {
        if matches!(g[node].user_data.kind, NodeKind::Factory { .. }) {
            let (s, need) = factory_balance(g, node);
            // Only what's wired into the circuit counts.
            if power_out(g, node).is_some_and(|o| !g.output_connections(o).is_empty()) {
                made[c] += s;
            }
            if power_in(g, node).is_some_and(|i| !g.connections(i).is_empty()) {
                used[c] += need;
                takers[c].push(node);
            }
        }
    }
    for node in g.iter_nodes() {
        match g[node].user_data.kind {
            NodeKind::Factory { .. } => {
                let (surplus, need) = factory_balance(g, node);
                if let Some(o) = power_out(g, node) {
                    ports.insert((node, PORT.into()), PortStat { have: surplus, want: surplus, connected: !g.output_connections(o).is_empty(), rated: false });
                }
                if let Some(i) = power_in(g, node) {
                    let share = circuit.get(&node).map_or(0.0, |&c| if used[c] > 1e-9 { (made[c] / used[c]).min(1.0) } else { 1.0 });
                    let connected = !g.connections(i).is_empty();
                    ports.insert((node, PORT.into()), PortStat { have: if connected { need * share } else { 0.0 }, want: need, connected, rated: false });
                }
            }
            NodeKind::Pole { .. } => {
                let (m, u) = circuit.get(&node).map_or((0.0, 0.0), |&c| (made[c], used[c]));
                ports.insert((node, MADE.into()), PortStat { have: m, want: m, connected: true, rated: false });
                ports.insert((node, USED.into()), PortStat { have: u, want: u, connected: true, rated: false });
                let n = wire_count(g, node) as f64;
                ports.insert((node, WIRES.into()), PortStat { have: n, want: n, connected: true, rated: false });
            }
            _ => {}
        }
    }
    for c in 0..n {
        if used[c] > made[c] + MIN_MW {
            let mut names: Vec<&str> = takers[c].iter().map(|n| g[*n].label.as_str()).collect();
            names.sort();
            let who = match names.len() {
                1 => names[0].to_string(),
                k => format!("{} and {} more", names[0], k - 1),
            };
            issues.push((takers[c][0], format!("Power for {who} is {} MW short", fmt_num(used[c] - made[c]))));
        }
    }
}

/// Wires on a node, all ports.
pub fn wire_count(g: &FactoryGraph, node: NodeId) -> usize {
    g[node].inputs.iter().map(|(_, i)| g.connections(*i).len()).sum::<usize>() + g[node].outputs.iter().map(|(_, o)| g.output_connections(*o).len()).sum::<usize>()
}

/// A power wire: faint, so a big circuit's wires stay in the background,
/// or (`lit`) in the accent colour the map lights a circuit in.
pub fn wire_color(lit: bool) -> Color32 {
    if lit {
        ACCENT
    } else {
        WIRE.gamma_multiply(0.35)
    }
}

/// The power wires to light for `focus` (selected or hovered nodes): every
/// wire of a circuit any of them is on.
pub fn lit_wires(g: &FactoryGraph, focus: &[NodeId]) -> HashSet<(NodeId, NodeId)> {
    let circuit = circuits(g);
    let on: HashSet<usize> = focus.iter().filter_map(|n| circuit.get(n).copied()).collect();
    if on.is_empty() {
        return HashSet::new();
    }
    g.iter_connections()
        .filter(|(i, _)| g[*i].typ.0 == POWER)
        .map(|(i, o)| (g[o].node, g[i].node))
        .filter(|(a, _)| circuit.get(a).is_some_and(|c| on.contains(c)))
        .collect()
}

/// A factory's power port row: the power icon and name, and under them the MW.
pub fn port_row(ui: &mut Ui, s: &mut UserState, node_id: NodeId, name: &str, input: bool) {
    let stat = s.ports.get(&(node_id, name.to_string())).copied().unwrap_or_default();
    let (text, color) = if !stat.connected {
        (format!("{} MW · not connected", fmt_num(stat.want)), TEXT_DIM)
    } else if input {
        (format!("{} / {} MW", fmt_num(stat.have), fmt_num(stat.want)), supply_color(stat.have, stat.want))
    } else {
        (format!("{} MW", fmt_num(stat.want)), TEXT_DIM)
    };
    let layout = if input { Layout::left_to_right(Align::Center) } else { Layout::right_to_left(Align::Center) };
    ui.allocate_ui_with_layout(vec2(ui.available_width(), 0.0), layout, |ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        s.ui_icon(ui, crate::assets::UiIcon::Power, 24.0, TEXT_DIM);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.with_layout(Layout::top_down(if input { Align::Min } else { Align::Max }), |ui| {
                ui.label(RichText::new(name).color(TEXT));
                ui.label(RichText::new(text).small().color(color));
            });
        });
    });
}

/// A pole: the game's icon for its tier (red ring when its circuit is
/// short), and its wires and circuit's figures on hover.
pub fn pole_ui(ui: &mut Ui, rect: egui::Rect, node_id: NodeId, pole: u32, s: &mut UserState) {
    let get = |name: &str| s.ports.get(&(node_id, name.to_string())).map(|p| p.have).unwrap_or(0.0);
    let (made, used, wires) = (get(MADE), get(USED), get(WIRES) as usize);
    let tier = s.pole_tiers.get(&pole).copied().unwrap_or(1).clamp(1, 3);
    let short = used > made + MIN_MW;
    if short {
        ui.painter().circle_stroke(rect.center(), rect.width().max(rect.height()) * 0.55, egui::Stroke::new(2.0_f32, DANGER));
    }
    match s.assets.as_mut().and_then(|a| a.item_uri(POLE_ICONS[tier as usize - 1])) {
        Some(uri) => {
            egui::Image::new(uri).paint_at(ui, egui::Rect::from_center_size(rect.center(), egui::Vec2::splat(rect.height())));
        }
        None => {
            ui.painter().circle_filled(rect.center(), rect.width() * 0.25, WIRE);
        }
    }
    if ui.rect_contains_pointer(rect) && !ui.input(|i| i.pointer.any_down()) {
        egui::show_tooltip_at_pointer(ui.ctx(), ui.layer_id(), egui::Id::new(("pole_tip", node_id)), |ui| {
            crate::widgets::tooltip_title(ui, &format!("Power Pole Mk.{tier}"));
            ui.label(RichText::new(format!("{wires} of {} Power Line connections", max_wires(tier))).color(TEXT_DIM));
            ui.label(RichText::new(format!("Its circuit makes {} MW and uses {} MW.", fmt_num(made), fmt_num(used))).color(TEXT));
            let (note, color) = if short {
                (format!("{} MW short", fmt_num(used - made)), DANGER)
            } else {
                (format!("{} MW spare", fmt_num(made - used)), TEXT_DIM)
            };
            ui.label(RichText::new(note).color(color));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poles_get_new_ids() {
        let mut net = PowerNet::default();
        assert_eq!((net.add_pole(0.0, 0.0), net.add_pole(0.0, 0.0)), (1, 2));
        net.poles.remove(1);
        assert_eq!(net.add_pole(0.0, 0.0), 2);
    }

    #[test]
    fn pole_tiers_take_the_games_wire_counts() {
        assert_eq!((max_wires(1), max_wires(2), max_wires(3)), (4, 7, 10));
        assert_eq!((tier_for(0), tier_for(4), tier_for(5), tier_for(7), tier_for(8), tier_for(10)), (1, 1, 2, 2, 3, 3));
        let p: Pole = serde_json::from_str(r#"{"id":1,"x":0,"y":0}"#).unwrap();
        assert_eq!(p.tier, 1, "files from before tiers are Mk.1");
    }

    #[test]
    fn balance_is_what_crosses_the_factory_edge() {
        assert_eq!(balance(100.0, 0.0), (0.0, 100.0));
        assert_eq!(balance(20.0, 150.0), (130.0, 0.0));
        assert_eq!(balance(75.0, 75.0), (0.0, 0.0));
    }

    #[test]
    fn links_serialize_plainly() {
        let l = PowerLink { from: End::Factory(3), to: End::Pole(1) };
        assert_eq!(serde_json::to_string(&l).unwrap(), r#"{"from":{"factory":3},"to":{"pole":1}}"#);
    }
}
