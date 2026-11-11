//! Mergers, splitters, pipeline junctions, pumps and valves: nodes that
//! gather, share out or limit the wires of one item, the way their buildings
//! do belts and pipes in game. Plain ones are small knots drawn with the
//! game's icons; Smart and Programmable Splitters, Priority Mergers and
//! Valves are nodes with their settings, worded as the game's own UI does.
//!
//! In the simulation (fb_engine::sim) each is a router: what comes in goes
//! out, over the actual wires, shared as [`share`] says.

use crate::fmt::fmt_rate_unit;
use crate::graph::*;
use crate::theme::*;
use egui::{vec2, RichText, Ui, Vec2};
use egui_node_graph2::{InputId, NodeId, OutputId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A Smart or Programmable Splitter output's filter, as in game.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rule {
    None,
    Any,
    AnyUndefined,
    Overflow,
    Item(String),
}

/// A Priority Merger input's priority, as in game.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    High,
    Medium,
    #[default]
    Low,
}

impl Priority {
    pub const ALL: [Priority; 3] = [Priority::High, Priority::Medium, Priority::Low];
    pub fn label(self) -> &'static str {
        match self {
            Priority::High => "High",
            Priority::Medium => "Medium",
            Priority::Low => "Low",
        }
    }
    fn rank(self) -> u8 {
        self as u8
    }
}

/// The game's names for the three sides (Buildables_UI string table).
pub const OUTPUT_NAMES: [&str; 3] = ["Left Output", "Center Output", "Right Output"];
pub const INPUT_NAMES: [&str; 3] = ["Left Input", "Center Input", "Right Input"];
/// The Valve's setting, as the game names it.
pub const VALVE_LIMIT: &str = "Valve Limit";

/// The game's descriptions of the rules (Buildables_UI string table).
fn rule_tip(r: &Rule) -> &'static str {
    match r {
        Rule::None => "No parts will pass through this output.",
        Rule::Any => "Any part, regardless of other output settings.",
        Rule::AnyUndefined => "Any part that is NOT selected in another output.",
        Rule::Overflow => "Any part or amount the other outputs cannot handle.",
        Rule::Item(_) => "Only this part passes through this output.",
    }
}

pub fn rule_label(game: &fb_model::GameData, r: &Rule) -> String {
    match r {
        Rule::None => "None".into(),
        Rule::Any => "Any".into(),
        Rule::AnyUndefined => "Any Undefined".into(),
        Rule::Overflow => "Overflow".into(),
        Rule::Item(i) => game.item_name(i).into(),
    }
}

/// A file's word for a rule, and back ("any_undefined", or an item class).
pub fn rule_word(r: &Rule) -> String {
    match r {
        Rule::None => "none".into(),
        Rule::Any => "any".into(),
        Rule::AnyUndefined => "any_undefined".into(),
        Rule::Overflow => "overflow".into(),
        Rule::Item(i) => i.clone(),
    }
}

pub fn rule_from_word(game: &fb_model::GameData, w: &str) -> Option<Rule> {
    Some(match w {
        "none" => Rule::None,
        "any" => Rule::Any,
        "any_undefined" => Rule::AnyUndefined,
        "overflow" => Rule::Overflow,
        i if game.items.contains_key(i) => Rule::Item(i.into()),
        _ => return None,
    })
}

/// A file's settings onto a Smart or Programmable Splitter or a Priority
/// Merger; unknown words are dropped, and at most three sides are read.
pub fn apply_settings(game: &fb_model::GameData, kind: &mut NodeKind, r: &crate::portable::RouterSettings) {
    match kind {
        NodeKind::SmartSplitter { programmable, rules, .. } => {
            let most = if *programmable { 32 } else { 1 };
            let mut read: Vec<Vec<Rule>> = r.rules.iter().take(3).map(|ws| ws.iter().filter_map(|w| rule_from_word(game, w)).take(most).collect()).collect();
            read.resize(3, vec![Rule::None]);
            *rules = read;
        }
        NodeKind::PriorityMerger { priority, .. } => {
            let mut read: Vec<Priority> = r.priorities.iter().take(3).map(|w| Priority::ALL.into_iter().find(|p| p.label().eq_ignore_ascii_case(w)).unwrap_or_default()).collect();
            read.resize(3, Priority::default());
            *priority = read;
        }
        _ => {}
    }
}

/// A new Smart or Programmable Splitter passes everything, like a plain one.
pub fn default_rules() -> Vec<Vec<Rule>> {
    vec![vec![Rule::Any]; 3]
}

/// Which outputs `item` goes to, as in game: those with a rule it matches
/// first (Any; its own filter; Any Undefined when no other output filters
/// it), then Overflow outputs with what they can't take. With neither, the
/// output takes nothing (None) — and with no output at all, the splitter
/// jams.
pub fn out_ranks(rules: &[Vec<Rule>], item: &str) -> Vec<Option<u8>> {
    let filters = |k: usize| rules.iter().enumerate().any(|(j, rs)| j != k && rs.iter().any(|r| matches!(r, Rule::Item(i) if i == item)));
    (0..3)
        .map(|k| {
            let rs = rules.get(k).map(Vec::as_slice).unwrap_or(&[]);
            let matches = rs.iter().any(|r| match r {
                Rule::Any => true,
                Rule::Item(i) => i == item,
                Rule::AnyUndefined => !filters(k),
                Rule::None | Rule::Overflow => false,
            });
            if matches {
                Some(0)
            } else if rs.contains(&Rule::Overflow) {
                Some(1)
            } else {
                None
            }
        })
        .collect()
}

/// How a router node shares what goes through it (None: not a router).
pub fn share(g: &FactoryGraph, node: NodeId) -> Option<(usize, usize, fb_engine::sim::Share)> {
    use fb_engine::sim::Share;
    Some(match &g[node].user_data.kind {
        NodeKind::Merger { .. } | NodeKind::Splitter { .. } | NodeKind::Pump { .. } => (1, 1, Share::default()),
        NodeKind::SmartSplitter { item, rules, .. } => (1, 3, Share { out_rank: out_ranks(rules, item), ..Default::default() }),
        NodeKind::PriorityMerger { priority, .. } => (3, 1, Share { in_rank: priority.iter().map(|p| p.rank()).collect(), ..Default::default() }),
        NodeKind::Valve { .. } => (1, 1, Share { limit: Some(input_number(g, node, VALVE_LIMIT)), ..Default::default() }),
        _ => return None,
    })
}

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
        // A pump or valve sits in one pipe; each side of a Smart Splitter or
        // Priority Merger takes one belt.
        NodeKind::Pump { .. } | NodeKind::Valve { .. } | NodeKind::SmartSplitter { .. } | NodeKind::PriorityMerger { .. } => on(port) < 1,
        NodeKind::Merger { item } | NodeKind::Splitter { item } if fluid(game, item) => crate::power_net::wire_count(g, node) < JUNCTION_PIPES,
        NodeKind::Merger { .. } => on(port) < if matches!(port, P::Input(_)) { MERGER_INPUTS } else { 1 },
        NodeKind::Splitter { .. } => on(port) < if matches!(port, P::Input(_)) { 1 } else { SPLITTER_OUTPUTS },
        _ => true,
    }
}

pub fn is_router(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Merger { .. } | NodeKind::Splitter { .. } | NodeKind::Pump { .. } | NodeKind::SmartSplitter { .. } | NodeKind::PriorityMerger { .. } | NodeKind::Valve { .. }
    )
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
        NodeKind::SmartSplitter { programmable: true, .. } => "Programmable Splitter",
        NodeKind::SmartSplitter { .. } => "Smart Splitter",
        NodeKind::PriorityMerger { .. } => "Priority Merger",
        NodeKind::Valve { .. } => "Valve",
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
        NodeKind::SmartSplitter { programmable: true, .. } => "Desc_ConveyorAttachmentSplitterProgrammable_C",
        NodeKind::SmartSplitter { .. } => "Desc_ConveyorAttachmentSplitterSmart_C",
        NodeKind::PriorityMerger { .. } => "Desc_ConveyorAttachmentMergerPriority_C",
        NodeKind::Valve { .. } => "Desc_Valve_C",
        _ => "Desc_ConveyorAttachmentSplitter_C",
    }
}

/// Under a Smart or Programmable Splitter output's port: its rules.
pub fn output_row(ui: &mut Ui, s: &mut UserState, graph: &FactoryGraph, node_id: NodeId, name: &str) {
    let NodeKind::SmartSplitter { rules, programmable, .. } = &graph[node_id].user_data.kind else { return };
    let Some(k) = OUTPUT_NAMES.iter().position(|n| *n == name) else { return };
    let rs = rules.get(k).cloned().unwrap_or_default();
    let text = if rs.is_empty() { "None".to_string() } else { rs.iter().map(|r| rule_label(&s.game, r)).collect::<Vec<_>>().join(", ") };
    let tip = rs.first().map(rule_tip).unwrap_or(rule_tip(&Rule::None));
    let label = if *programmable { "Rules" } else { "Rule" };
    setting_row(ui, s, graph[node_id].user_data.planned, node_id, k, label, &text, tip, false);
}

/// Under a Priority Merger input's port: its priority.
pub fn input_row(ui: &mut Ui, s: &mut UserState, d: &NodeData, node_id: NodeId, name: &str) {
    let NodeKind::PriorityMerger { priority, .. } = &d.kind else { return };
    let Some(k) = INPUT_NAMES.iter().position(|n| *n == name) else { return };
    let p = priority.get(k).copied().unwrap_or_default();
    setting_row(ui, s, d.planned, node_id, k, "Priority", p.label(), "Inputs can be prioritized to always be merged before others.", true);
}

/// A setting as a row like a machine's fields: its name on the left, its
/// value on the right with the menus' arrow, the whole row a button that
/// opens its menu. Where it can't be changed (built, in the Live view):
/// the same row without the arrow or the hover.
#[allow(clippy::too_many_arguments)]
fn setting_row(ui: &mut Ui, s: &mut UserState, planned: bool, node_id: NodeId, port: usize, label: &str, value: &str, tip: &str, input: bool) {
    let editable = !s.live_view || planned;
    let h = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
    let sense = if editable { egui::Sense::click() } else { egui::Sense::hover() };
    let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width(), h), sense);
    if editable && r.hovered() {
        ui.painter().rect_filled(rect.expand2(vec2(4.0, 0.0)), 0.0, ROW_HOVER);
    }
    let font = egui::TextStyle::Body.resolve(ui.style());
    let painter = ui.painter();
    let g = painter.layout_no_wrap(label.to_string(), font.clone(), TEXT_DIM);
    painter.galley(egui::pos2(rect.left(), rect.center().y - g.size().y / 2.0), g, TEXT_DIM);
    let arrow = if editable { h * 0.7 } else { 0.0 };
    let g = painter.layout_no_wrap(value.to_string(), font, TEXT);
    let x = rect.right() - arrow - if editable { 4.0 } else { 0.0 } - g.size().x;
    painter.galley(egui::pos2(x, rect.center().y - g.size().y / 2.0), g, TEXT);
    if editable {
        let a = egui::Rect::from_center_size(egui::pos2(rect.right() - arrow / 2.0, rect.center().y), vec2(arrow, arrow));
        if let Some(assets) = s.assets.as_mut() {
            assets.paint_ui(ui, crate::assets::UiIcon::Arrow, a, TEXT_DIM);
        }
    }
    #[cfg(test)]
    crate::widgets::TEST_BUTTONS.with(|b| b.borrow_mut().push((format!("{} setting", if input { INPUT_NAMES[port] } else { OUTPUT_NAMES[port] }), rect, editable)));
    #[cfg(not(test))]
    let _ = input;
    let r = r.on_hover_text(tip);
    if editable && r.clicked() {
        s.router_menu = Some((node_id, port, rect.right_bottom() - vec2(arrow, 0.0)));
    }
    if editable {
        r.on_hover_cursor(egui::CursorIcon::PointingHand);
    }
}

/// A hairline between a router node's sides, as machines have.
pub fn divider(ui: &mut Ui) {
    ui.add_space(2.0);
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().rect_filled(r, 0.0, DIVIDER);
    ui.add_space(2.0);
}

/// Smart and Programmable Splitters, Priority Mergers and Valves: the
/// building and what goes through it, as a machine's footer.
pub fn footer(ui: &mut Ui, s: &mut UserState, node_id: NodeId, kind: &NodeKind) {
    let game = s.game.clone();
    let (NodeKind::SmartSplitter { item, .. } | NodeKind::PriorityMerger { item, .. } | NodeKind::Valve { item }) = kind else { return };
    let through = s.ports.get(&(node_id, ROUTED.to_string())).map(|p| p.have).unwrap_or(0.0);
    divider(ui);
    ui.horizontal(|ui| {
        s.item_icon(ui, icon(&game, kind), 32.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            crate::widgets::name_label(ui, RichText::new(name(&game, kind)).color(TEXT));
            let what = format!("{} through it", fmt_rate_unit(through, game.item_form(item)));
            crate::widgets::name_label(ui, RichText::new(what).small().color(TEXT_DIM));
        });
    });
}

/// The menu a setting row opens: an output's rules (one for a Smart
/// Splitter, several for a Programmable one) or an input's priority.
pub fn router_menu(app: &mut crate::app::FactoryApp, ctx: &egui::Context) {
    let Some((node, port, pos)) = app.user.router_menu else { return };
    let Some(kind) = app.state.graph.nodes.get(node).map(|n| n.user_data.kind.clone()) else {
        app.user.router_menu = None;
        return;
    };
    let game = app.user.game.clone();
    let mut next: Option<NodeKind> = None;
    let mut close = false;
    let menu = match &kind {
        NodeKind::SmartSplitter { item, programmable, rules } => {
            let title = OUTPUT_NAMES.get(port).copied().unwrap_or("Output");
            crate::menus::popup(ctx, "router_menu", pos, title, |ui| {
                let current = rules.get(port).cloned().unwrap_or_default();
                for r in [Rule::None, Rule::Any, Rule::AnyUndefined, Rule::Overflow, Rule::Item(item.clone())] {
                    let on = current.contains(&r);
                    if crate::menus::choice(ui, &rule_label(&game, &r), "", on, true).on_hover_text(rule_tip(&r)).clicked() {
                        let mut set = if *programmable { current.clone() } else { Vec::new() };
                        if *programmable && on {
                            set.retain(|x| *x != r);
                        } else if *programmable && r == Rule::None {
                            set = vec![Rule::None];
                        } else {
                            set.retain(|x| *x != Rule::None);
                            set.push(r);
                        }
                        let mut rules = rules.clone();
                        rules.resize(3, Vec::new());
                        rules[port] = set;
                        next = Some(NodeKind::SmartSplitter { item: item.clone(), programmable: *programmable, rules });
                        close = !*programmable;
                    }
                }
            })
        }
        NodeKind::PriorityMerger { item, priority } => {
            let title = INPUT_NAMES.get(port).copied().unwrap_or("Input");
            crate::menus::popup(ctx, "router_menu", pos, title, |ui| {
                crate::menus::caption(ui, "Priority");
                for p in Priority::ALL {
                    if crate::menus::choice(ui, p.label(), "", priority.get(port) == Some(&p), true).clicked() {
                        let mut priority = priority.clone();
                        priority.resize(3, Priority::default());
                        priority[port] = p;
                        next = Some(NodeKind::PriorityMerger { item: item.clone(), priority });
                        close = true;
                    }
                }
            })
        }
        _ => {
            app.user.router_menu = None;
            return;
        }
    };
    if close || crate::menus::dismissed(ctx, &menu) {
        app.user.router_menu = None;
    }
    if let Some(k) = next {
        app.state.graph[node].user_data.kind = k;
        app.changed();
    }
}

/// Turns every belt (or pipe) that several machines share into mergers and
/// splitters, as it would be built in game: the suppliers' wires join
/// through mergers (three at a time) into one, which splitters (three ways
/// at a time) share out to the consumers. For fluids, Pipeline Junctions do
/// both. A splitter shares equally, so where a belt feeds machines and also
/// an Output, Sink or Storage (which never back up), a Smart Splitter sends
/// the machines theirs and the rest to those on its Overflow output.
/// Wires between one supplier and one consumer stay as they are.
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
        // Outputs, sinks and storage never back up: a plain splitter would
        // give them a full share and starve the machines.
        let spills: Vec<InputId> = sinks.iter().copied().filter(|i| never_backs_up(&state.graph[state.graph[*i].node].user_data.kind)).collect();
        if fluid || spills.is_empty() || spills.len() == sinks.len() {
            split(state, user, &item, fluid, level[0], &sinks, z);
            continue;
        }
        let machines: Vec<InputId> = sinks.iter().copied().filter(|i| !spills.contains(i)).collect();
        let ns: Vec<NodeId> = sinks.iter().map(|i| state.graph[*i].node).collect();
        let at = mid(state, &ns) - egui::vec2(420.0 * z, 0.0);
        let smart = crate::project::add_node_to(state, user, Template::SmartSplitter { item: item.clone(), programmable: false }, at);
        state.graph[smart].user_data.kind = NodeKind::SmartSplitter { item: item.clone(), programmable: false, rules: vec![vec![Rule::Any], vec![Rule::None], vec![Rule::Overflow]] };
        let (s_in, left, right) = (state.graph[smart].inputs[0].1, state.graph[smart].outputs[0].1, state.graph[smart].outputs[2].1);
        state.graph.add_connection(level[0], s_in, 0);
        split(state, user, &item, fluid, left, &machines, z);
        split(state, user, &item, fluid, right, &spills, z);
    }
}

/// Takes whatever arrives, so a belt to it never backs up.
fn never_backs_up(kind: &NodeKind) -> bool {
    matches!(kind, NodeKind::Output { .. } | NodeKind::Sink { .. } | NodeKind::Storage { .. })
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
