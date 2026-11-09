//! Projects: several factories, and the master graph that links them.
//!
//! Every document is a project. Each factory is its own graph and plan with
//! its own undo history. The master graph (doc 0) has one block per factory,
//! showing what it takes in and sends out, and wires between blocks for the
//! items one factory sends another. Only the document in the open tab lives
//! in the app's editor fields (`state`, `planner`, undo...); the others wait
//! in `Doc::body` until their tab is opened.
//!
//! On disk a one-factory project is a plain `.fbfactory`; anything more is a
//! `.fbproject` (portable.rs). The autosave keeps the editor's own form of
//! every document (`Workspace`), so a restored session has the same ids,
//! views and open tabs.

use crate::app::{evaluate_graph, FactoryApp, Planner, Summary};
use crate::graph::*;
use crate::portable::{self, FactoryFile, ProjectFile};
use crate::theme;
use egui::{vec2, Pos2, Vec2};
use egui_node_graph2::{InputId, NodeId, OutputId};
use fb_model::GameData;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// The master graph's document id.
pub(crate) const MASTER: u32 = 0;
/// Space between blocks placed automatically on the master graph.
const BLOCK_GAP: f32 = 120.0;
const WORKSPACE_VERSION: u32 = 3;

/// What a factory takes in, sends out and needs, for its block on the
/// master graph and the project totals.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FactoryInfo {
    pub name: String,
    /// Items from other factories (or outside), per minute.
    pub ins: Vec<(String, f64)>,
    /// Products and byproducts, per minute.
    pub outs: Vec<(String, f64)>,
    pub power_mw: f64,
    pub generation: Option<f64>,
    /// Whole buildings.
    pub machines: u32,
    pub buildings: BTreeMap<String, (f64, f64)>,
    pub resources: BTreeMap<String, f64>,
    pub sink_points: f64,
    pub power_shards: u32,
    pub somersloops: u32,
    /// Extractor nodes: building class -> (whole, at 100%), and per item.
    pub extractors: BTreeMap<String, (f64, f64)>,
    pub extracted: BTreeMap<String, Vec<String>>,
}

impl FactoryInfo {
    pub(crate) fn from_summary(name: &str, s: &Summary) -> Self {
        let add = |list: &mut Vec<(String, f64)>, item: &str, rate: f64| match list.iter_mut().find(|(i, _)| i == item) {
            Some(e) => e.1 += rate,
            None => list.push((item.to_string(), rate)),
        };
        let mut outs = Vec::new();
        for (item, got, _) in &s.outputs {
            add(&mut outs, item, *got);
        }
        for (item, got) in &s.sunk {
            add(&mut outs, item, *got);
        }
        let mut ins = Vec::new();
        for (item, rate) in &s.inputs {
            add(&mut ins, item, *rate);
        }
        FactoryInfo {
            name: name.to_string(),
            ins,
            outs,
            power_mw: s.power_mw,
            generation: s.generation,
            machines: s.buildings.values().map(|(whole, _)| *whole as u32).sum(),
            buildings: s.buildings.clone(),
            resources: s.resources.clone(),
            sink_points: s.sink_points,
            power_shards: s.power_shards,
            somersloops: s.somersloops,
            extractors: s.extractors.clone(),
            extracted: s.extracted.clone(),
        }
    }
}

/// One document: the master graph or a factory.
pub(crate) struct Doc {
    pub id: u32,
    pub name: String,
    /// None while it's the open one (its contents are the app's live fields).
    pub body: Option<Box<DocBody>>,
    /// Cached totals of a factory that isn't open.
    pub info: Option<FactoryInfo>,
}

/// A document's contents while another one is open.
pub(crate) struct DocBody {
    pub state: EditorState,
    pub planner: Planner,
    pub undo: Vec<String>,
    pub redo: Vec<String>,
    pub committed: String,
    pub solve_notes: Vec<String>,
    pub node_heights: BTreeMap<NodeId, f32>,
}

impl DocBody {
    pub fn new(planner: Planner) -> Self {
        DocBody { state: new_state(), planner, undo: Vec::new(), redo: Vec::new(), committed: String::new(), solve_notes: Vec::new(), node_heights: BTreeMap::new() }
    }
}

pub(crate) fn new_state() -> EditorState {
    let mut s = EditorState::new(1.0);
    s.style = theme::graph_style();
    s
}

/// A copy of a graph and its view (the editor state can't derive Clone).
fn copy_state(s: &EditorState) -> EditorState {
    let mut copy = serde_json::to_string(s).ok().and_then(|j| serde_json::from_str::<EditorState>(&j).ok()).unwrap_or_else(new_state);
    copy.style = theme::graph_style();
    copy.selected_nodes.clear();
    copy
}

/// The autosave: every document in the editor's own form.
#[derive(Serialize)]
struct WorkspaceRef<'a> {
    version: u32,
    name: &'a Option<String>,
    path: &'a Option<String>,
    active: u32,
    tabs: &'a [u32],
    docs: Vec<WorkspaceDocRef<'a>>,
}

#[derive(Serialize)]
struct WorkspaceDocRef<'a> {
    id: u32,
    name: &'a str,
    planner: &'a Planner,
    editor: &'a EditorState,
}

#[derive(Deserialize)]
pub(crate) struct Workspace {
    pub version: u32,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    pub active: u32,
    #[serde(default)]
    pub tabs: Vec<u32>,
    pub docs: Vec<WorkspaceDoc>,
}

#[derive(Deserialize)]
pub(crate) struct WorkspaceDoc {
    pub id: u32,
    pub name: String,
    pub planner: Planner,
    pub editor: EditorState,
}

impl Workspace {
    /// An autosave in this form (older ones are a single factory).
    pub fn parse(json: &str) -> Option<Workspace> {
        let w: Workspace = serde_json::from_str(json).ok()?;
        (w.version == WORKSPACE_VERSION && w.docs.iter().any(|d| d.id == MASTER)).then_some(w)
    }
}

/// The master graph's block for factory `id`.
pub(crate) fn block_of(g: &FactoryGraph, id: u32) -> Option<NodeId> {
    g.iter_nodes().find(|n| matches!(g[*n].user_data.kind, NodeKind::Factory { id: f, .. } if f == id))
}

/// The factory a master graph block stands for.
pub(crate) fn factory_of(g: &FactoryGraph, node: NodeId) -> Option<u32> {
    match g.nodes.get(node)?.user_data.kind {
        NodeKind::Factory { id, .. } => Some(id),
        _ => None,
    }
}

pub(crate) fn add_node_to(state: &mut EditorState, user: &mut UserState, template: Template, pos: Pos2) -> NodeId {
    use egui_node_graph2::NodeTemplateTrait;
    let label = template.node_graph_label(user);
    let data = template.user_data(user);
    let id = state.graph.add_node(label, data, |g, id| template.build_node(g, user, id));
    state.node_positions.insert(id, pos);
    state.node_order.push(id);
    id
}

/// Where the next block goes: right of the others (zoomed position).
fn free_block_pos(state: &EditorState) -> Pos2 {
    let z = state.pan_zoom.zoom.max(0.01);
    let w = theme::graph_style().node_width;
    let mut right = f32::NEG_INFINITY;
    let mut top = f32::INFINITY;
    for (_, p) in state.node_positions.iter() {
        let g = (p.to_vec2() / z).to_pos2();
        right = right.max(g.x + w);
        top = top.min(g.y);
    }
    let at = if right.is_finite() { egui::pos2(right + BLOCK_GAP, top) } else { Pos2::ZERO };
    (at.to_vec2() * z).to_pos2()
}

/// Brings the master graph up to date with the factories: one block each
/// (new factories get one), names, and ports for what each takes in and
/// sends out. Links stay on the items that are still there. Then the power
/// grids (shown or not, `show_power`; see power_net.rs).
pub(crate) fn sync_master_graph(game: &GameData, state: &mut EditorState, user: &mut UserState, infos: &[(u32, FactoryInfo)], net: &mut crate::power_net::PowerNet, show_power: bool) {
    user.factories = infos.iter().cloned().collect();
    let known: BTreeSet<u32> = infos.iter().map(|(id, _)| *id).collect();
    let mut seen = BTreeSet::new();
    for node in state.graph.iter_nodes().collect::<Vec<_>>() {
        let keep = match state.graph[node].user_data.kind {
            NodeKind::Factory { id, .. } => known.contains(&id) && seen.insert(id),
            // power_net::sync's to keep or not.
            NodeKind::Pole { .. } => true,
            _ => false,
        };
        if !keep {
            state.graph.remove_node(node);
            state.node_positions.remove(node);
            state.node_order.retain(|n| *n != node);
            state.selected_nodes.retain(|n| *n != node);
        }
    }
    for (id, info) in infos {
        match block_of(&state.graph, *id) {
            None => {
                let pos = free_block_pos(state);
                add_node_to(state, user, Template::Factory(*id), pos);
            }
            Some(node) => update_block(game, &mut state.graph, node, info),
        }
    }
    crate::power_net::sync(state, user, net, show_power);
}

fn update_block(game: &GameData, g: &mut FactoryGraph, node: NodeId, info: &FactoryInfo) {
    if g[node].label != info.name {
        g[node].label = info.name.clone();
    }
    if let NodeKind::Factory { ins, outs, uses_mw, makes_mw, .. } = &mut g[node].user_data.kind {
        if *ins != info.ins {
            *ins = info.ins.clone();
        }
        if *outs != info.outs {
            *outs = info.outs.clone();
        }
        let makes = info.generation.unwrap_or(0.0);
        if *uses_mw != info.power_mw || *makes_mw != makes {
            (*uses_mw, *makes_mw) = (info.power_mw, makes);
        }
    }
    // Item ports only: power ports are power_net.rs's (it puts back any
    // removed here).
    let item = |t: &str| !crate::power_net::is_power(t);
    let have_in: Vec<String> = g[node].inputs.iter().map(|(_, i)| g[*i].typ.0.clone()).filter(|t| item(t)).collect();
    let have_out: Vec<String> = g[node].outputs.iter().map(|(_, o)| g[*o].typ.0.clone()).filter(|t| item(t)).collect();
    let want_in: Vec<String> = info.ins.iter().map(|(i, _)| i.clone()).collect();
    let want_out: Vec<String> = info.outs.iter().map(|(i, _)| i.clone()).collect();
    if have_in == want_in && have_out == want_out {
        return;
    }
    // Rebuild the ports, keeping each item's links.
    let in_links: Vec<(String, Vec<OutputId>)> = g[node].inputs.iter().map(|(_, i)| (g[*i].typ.0.clone(), g.connections(*i))).collect();
    let out_links: Vec<(String, Vec<InputId>)> = g[node].outputs.iter().map(|(_, o)| (g[*o].typ.0.clone(), g.output_connections(*o))).collect();
    for i in g[node].inputs.iter().map(|(_, i)| *i).collect::<Vec<_>>() {
        g.remove_input_param(i);
    }
    for o in g[node].outputs.iter().map(|(_, o)| *o).collect::<Vec<_>>() {
        g.remove_output_param(o);
    }
    for item in &want_in {
        let new = add_item_input(game, g, node, item);
        for (_, outs) in in_links.iter().filter(|(i, _)| i == item) {
            for o in outs {
                if g.try_get_output(*o).is_some() {
                    let pos = g.connections(new).len();
                    g.add_connection(*o, new, pos);
                }
            }
        }
    }
    for item in &want_out {
        let new = add_item_output(game, g, node, item);
        for (_, ins) in out_links.iter().filter(|(i, _)| i == item) {
            for i in ins {
                if g.try_get_input(*i).is_some() {
                    let pos = g.connections(*i).len();
                    g.add_connection(new, *i, pos);
                }
            }
        }
    }
}

impl FactoryApp {
    // ------------------------------------------------------------ documents

    pub(crate) fn doc(&self, id: u32) -> Option<&Doc> {
        self.docs.iter().find(|d| d.id == id)
    }

    pub(crate) fn doc_mut(&mut self, id: u32) -> Option<&mut Doc> {
        self.docs.iter_mut().find(|d| d.id == id)
    }

    /// The factories, in project order.
    pub(crate) fn factory_ids(&self) -> Vec<u32> {
        self.docs.iter().filter(|d| d.id != MASTER).map(|d| d.id).collect()
    }

    pub(crate) fn factory_count(&self) -> usize {
        self.docs.len().saturating_sub(1)
    }

    pub(crate) fn on_master(&self) -> bool {
        self.active == MASTER
    }

    pub(crate) fn doc_name(&self, id: u32) -> String {
        self.doc(id).map(|d| d.name.clone()).unwrap_or_default()
    }

    /// A name no other factory has: "Factory", "Factory 2"...
    pub(crate) fn unique_name(&self, base: &str) -> String {
        let taken = |n: &str| self.docs.iter().any(|d| d.id != MASTER && d.name.eq_ignore_ascii_case(n));
        if !taken(base) {
            return base.to_string();
        }
        (2..).map(|k| format!("{base} {k}")).find(|n| !taken(n)).unwrap()
    }

    fn take_live(&mut self) -> DocBody {
        DocBody {
            state: std::mem::replace(&mut self.state, new_state()),
            planner: std::mem::take(&mut self.planner),
            undo: std::mem::take(&mut self.undo),
            redo: std::mem::take(&mut self.redo),
            committed: std::mem::take(&mut self.committed),
            solve_notes: std::mem::take(&mut self.solve_notes),
            node_heights: std::mem::take(&mut self.node_heights),
        }
    }

    fn put_live(&mut self, b: DocBody) {
        self.state = b.state;
        self.state.style = theme::graph_style();
        self.planner = b.planner;
        self.undo = b.undo;
        self.redo = b.redo;
        self.committed = b.committed;
        self.solve_notes = b.solve_notes;
        self.node_heights = b.node_heights;
        if self.committed.is_empty() {
            self.committed = self.project_json();
        }
    }

    /// View state that belongs to the graph on screen.
    fn reset_transient(&mut self) {
        self.context_menu = None;
        self.context_targets.clear();
        self.finder = None;
        self.focus_node = None;
        self.relayout = false;
        self.frame_selected = false;
        self.sim_cache = None;
        self.wire_flow.clear();
        self.node_rects.clear();
        self.user.ports.clear();
        self.user.nodes.clear();
        self.summary = Summary::default();
        self.state.connection_in_progress = None;
        self.state.ongoing_box_selection = None;
        self.state.pan_zoom.started = false;
        self.frames = Default::default();
        self.master_menu = None;
        self.block_spot = None;
        self.node_rename = None;
        self.snap_raw.clear();
    }

    /// Opens document `id` in the editor (its tab, or the master graph).
    pub(crate) fn activate(&mut self, id: u32) {
        if self.doc(id).is_none() {
            return;
        }
        if id != MASTER && !self.tabs.contains(&id) {
            self.tabs.push(id);
        }
        self.view = crate::map::View::Graph;
        if id == self.active {
            return;
        }
        self.flush_commit();
        let leaving = self.active;
        let body = self.take_live();
        if let Some(d) = self.doc_mut(leaving) {
            d.body = Some(Box::new(body));
            d.info = None;
        }
        let body = self.doc_mut(id).and_then(|d| d.body.take()).map(|b| *b).unwrap_or_else(|| DocBody::new(Planner::default()));
        self.put_live(body);
        self.active = id;
        self.user.master = id == MASTER;
        self.reset_transient();
        if id == MASTER {
            self.sync_master();
        }
        if self.game_loaded {
            self.evaluate_now();
        }
    }

    /// Totals of factory `id` (worked out once while it isn't open).
    pub(crate) fn factory_info(&mut self, id: u32) -> FactoryInfo {
        let name = self.doc_name(id);
        if id == self.active {
            return FactoryInfo::from_summary(&name, &self.summary);
        }
        let game = self.user.game.clone();
        let Some(d) = self.doc_mut(id) else { return FactoryInfo::default() };
        if d.info.is_none() {
            if let Some(b) = &d.body {
                let e = evaluate_graph(&game, &b.state.graph, &mut None, &b.planner.targets);
                d.info = Some(FactoryInfo::from_summary(&name, &e.summary));
            }
        }
        let mut info = d.info.clone().unwrap_or_default();
        info.name = name;
        info
    }

    pub(crate) fn factory_infos(&mut self) -> Vec<(u32, FactoryInfo)> {
        self.factory_ids().into_iter().map(|id| (id, self.factory_info(id))).collect()
    }

    /// Brings the master graph (open or not) up to date with the factories.
    pub(crate) fn sync_master(&mut self) {
        if !self.game_loaded {
            return;
        }
        let infos = self.factory_infos();
        let game = self.user.game.clone();
        let show = self.power_links;
        if self.active == MASTER {
            sync_master_graph(&game, &mut self.state, &mut self.user, &infos, &mut self.planner.power_net, show);
        } else {
            let mut scratch = UserState { game: game.clone(), zoom: 1.0, ..Default::default() };
            if let Some(b) = self.doc_mut(MASTER).and_then(|d| d.body.as_mut()) {
                sync_master_graph(&game, &mut b.state, &mut scratch, &infos, &mut b.planner.power_net, show);
            }
        }
    }

    /// Adds an empty factory (with the usual starting plan) and returns its id.
    pub(crate) fn add_factory(&mut self, name: &str, body: Option<DocBody>) -> u32 {
        let id = self.docs.iter().map(|d| d.id).max().unwrap_or(0) + 1;
        let body = body.unwrap_or_else(|| DocBody::new(self.default_planner()));
        let name = self.unique_name(name);
        self.docs.push(Doc { id, name, body: Some(Box::new(body)), info: None });
        self.project_changed();
        id
    }

    /// File-level change that isn't an edit inside a graph (factories added,
    /// removed or renamed): unsaved, autosaved, shared with the room.
    pub(crate) fn project_changed(&mut self) {
        self.dirty = true;
        self.rev += 1;
        self.sync_master();
        self.share_doc_now();
    }

    pub(crate) fn new_factory(&mut self) {
        let id = self.add_factory("Factory", None);
        self.activate(id);
        self.frame_all = true;
    }

    /// Puts factory `id`'s block at `at` (unzoomed) on the open master graph
    /// and selects it.
    pub(crate) fn place_block(&mut self, id: u32, at: Pos2) {
        if self.active != MASTER {
            return;
        }
        if let Some(n) = block_of(&self.state.graph, id) {
            let z = self.state.pan_zoom.zoom;
            self.state.node_positions.insert(n, (at.to_vec2() * z).to_pos2());
            self.state.selected_nodes = vec![n];
            self.changed();
        }
    }

    /// The master graph's right-click menu: a new power pole centred on
    /// `at` (unzoomed), selected.
    pub(crate) fn new_pole_at(&mut self, at: Pos2) {
        if self.active != MASTER {
            return;
        }
        let half = crate::power_net::POLE_SIZE / 2.0;
        let id = self.planner.power_net.add_pole(at.x - half.x, at.y - half.y);
        self.sync_master();
        if let Some(n) = crate::power_net::pole_node(&self.state.graph, id) {
            self.state.selected_nodes = vec![n];
        }
        self.changed();
    }

    /// The master graph's right-click menu: a new empty factory at `at`.
    pub(crate) fn new_factory_at(&mut self, at: Pos2) {
        let id = self.add_factory("Factory", None);
        self.place_block(id, at);
    }

    pub(crate) fn rename_factory(&mut self, id: u32, name: &str) {
        let name = name.trim();
        if name.is_empty() || id == MASTER {
            return;
        }
        let unique = if self.doc_name(id).eq_ignore_ascii_case(name) { name.to_string() } else { self.unique_name(name) };
        if let Some(d) = self.doc_mut(id) {
            d.name = unique;
        }
        self.project_changed();
    }

    /// Copies factory `id` (graph and plan, not its undo history).
    pub(crate) fn duplicate_factory(&mut self, id: u32) -> Option<u32> {
        self.flush_commit();
        let body = if id == self.active {
            DocBody { state: copy_state(&self.state), planner: self.planner.clone(), ..DocBody::new(Planner::default()) }
        } else {
            let b = self.doc(id)?.body.as_ref()?;
            DocBody { state: copy_state(&b.state), planner: b.planner.clone(), ..DocBody::new(Planner::default()) }
        };
        let name = format!("{} copy", self.doc_name(id));
        Some(self.add_factory(&name, Some(body)))
    }

    /// Removes factories (their blocks and links go with them).
    pub(crate) fn delete_factories(&mut self, ids: &[u32]) {
        if ids.contains(&self.active) {
            self.activate(MASTER);
        }
        self.docs.retain(|d| d.id == MASTER || !ids.contains(&d.id));
        self.tabs.retain(|t| !ids.contains(t));
        if self.factory_count() == 0 {
            // A project always has a factory to work in.
            self.add_factory("Factory", None);
        }
        self.project_changed();
    }

    pub(crate) fn close_tab(&mut self, id: u32) {
        let Some(at) = self.tabs.iter().position(|t| *t == id) else { return };
        self.tabs.remove(at);
        if self.active == id {
            let next = self.tabs.get(at).or_else(|| self.tabs.get(at.wrapping_sub(1))).copied().unwrap_or(MASTER);
            self.activate(next);
        }
    }

    // ----------------------------------------------------- whole projects

    /// Replaces everything with a project of one empty factory.
    pub(crate) fn new_workspace(&mut self) {
        let planner = self.default_planner();
        self.docs = vec![
            Doc { id: MASTER, name: "Project".into(), body: Some(Box::new(DocBody::new(Planner::default()))), info: None },
            Doc { id: 1, name: "Factory".into(), body: None, info: None },
        ];
        self.put_live(DocBody::new(planner));
        self.active = 1;
        self.tabs = vec![1];
        self.user.master = false;
        self.reset_transient();
        self.rev += 1;
    }

    /// Replaces everything with `docs`, opening `active`.
    fn load_docs(&mut self, mut docs: Vec<Doc>, active: u32, tabs: Vec<u32>) {
        if !docs.iter().any(|d| d.id == MASTER) {
            docs.insert(0, Doc { id: MASTER, name: "Project".into(), body: Some(Box::new(DocBody::new(Planner::default()))), info: None });
        }
        let active = if docs.iter().any(|d| d.id == active) { active } else { MASTER };
        // A different project: Find was searching the old one.
        self.find = None;
        let body = docs.iter_mut().find(|d| d.id == active).and_then(|d| d.body.take()).map(|b| *b).unwrap_or_else(|| DocBody::new(Planner::default()));
        self.docs = docs;
        self.tabs = tabs.into_iter().filter(|t| *t != MASTER && self.docs.iter().any(|d| d.id == *t)).collect();
        if active != MASTER && !self.tabs.contains(&active) {
            self.tabs.push(active);
        }
        self.put_live(body);
        self.active = active;
        self.user.master = active == MASTER;
        self.reset_transient();
        self.rev += 1;
        if self.game_loaded {
            let game = self.user.game.clone();
            crate::graph::add_missing_sloop_inputs(&game, &mut self.state.graph);
            let settings = self.planner.extraction.clone();
            crate::extractors::add_to_plan(&mut self.state, &mut self.user, &settings);
            let mut scratch = UserState { game: game.clone(), zoom: 1.0, ..Default::default() };
            for d in &mut self.docs {
                if let Some(b) = d.body.as_mut() {
                    crate::graph::add_missing_sloop_inputs(&game, &mut b.state.graph);
                    let settings = b.planner.extraction.clone();
                    crate::extractors::add_to_plan(&mut b.state, &mut scratch, &settings);
                }
            }
            self.sync_master();
        }
    }

    /// The autosave form of everything.
    pub(crate) fn workspace_json(&self) -> String {
        let docs = self
            .docs
            .iter()
            .map(|d| {
                let (planner, editor) = match &d.body {
                    Some(b) => (&b.planner, &b.state),
                    None => (&self.planner, &self.state),
                };
                WorkspaceDocRef { id: d.id, name: &d.name, planner, editor }
            })
            .collect();
        serde_json::to_string(&WorkspaceRef { version: WORKSPACE_VERSION, name: &self.project_name, path: &self.current_path, active: self.active, tabs: &self.tabs, docs }).unwrap_or_default()
    }

    /// Restores an autosave made by `workspace_json`.
    pub(crate) fn restore_workspace(&mut self, w: Workspace) {
        let docs = w
            .docs
            .into_iter()
            .map(|d| {
                let mut body = DocBody::new(d.planner);
                body.state = d.editor;
                body.state.style = theme::graph_style();
                Doc { id: d.id, name: d.name, body: Some(Box::new(body)), info: None }
            })
            .collect();
        self.load_docs(docs, w.active, w.tabs);
        self.project_name = w.name;
        self.current_path = w.path;
        for d in &mut self.docs {
            if let Some(b) = d.body.as_mut() {
                b.committed.clear();
            }
        }
        self.reset_history();
    }

    // ------------------------------------------------------- portable form

    /// Factory `id` in the portable file format.
    pub(crate) fn factory_file(&self, id: u32) -> Option<FactoryFile> {
        let name = Some(self.doc_name(id));
        if id == self.active {
            return Some(self.portable_of(&self.state, &self.planner, name));
        }
        let b = self.doc(id)?.body.as_ref()?;
        Some(self.portable_of(&b.state, &b.planner, name))
    }

    /// The whole project in the portable file format.
    pub(crate) fn project_file(&mut self) -> ProjectFile {
        self.flush_commit();
        self.sync_master();
        let mut p = ProjectFile::new(self.project_name.clone());
        let master = if self.active == MASTER { Some(&self.state) } else { self.doc(MASTER).and_then(|d| d.body.as_ref()).map(|b| &b.state) };
        let Some(master) = master else { return p };
        let g = &master.graph;
        let z = master.pan_zoom.zoom.max(0.01);
        for id in self.factory_ids() {
            let Some(factory) = self.factory_file(id) else { continue };
            let block = block_of(g, id);
            let pos = block.and_then(|n| master.node_positions.get(n)).map(|p| (p.to_vec2() / z).to_pos2()).unwrap_or(Pos2::ZERO);
            let color = block.and_then(|n| g[n].user_data.color).map(portable::color_to_hex);
            p.factories.push(portable::ProjectFactory { id, x: (pos.x * 10.0).round() / 10.0, y: (pos.y * 10.0).round() / 10.0, color, factory });
        }
        for (input, output) in g.iter_connections() {
            let (Some(from), Some(to)) = (factory_of(g, g[output].node), factory_of(g, g[input].node)) else { continue };
            let slot = g.output_connections(output).iter().position(|i| *i == input).unwrap_or(0);
            p.links.push(portable::Link { from, to, item: g[output].typ.0.clone(), slot });
        }
        p.links.sort_by(|a, b| (a.from, &a.item, a.slot, a.to).cmp(&(b.from, &b.item, b.slot, b.to)));
        let master_planner = if self.active == MASTER { &self.planner } else { self.doc(MASTER).and_then(|d| d.body.as_ref()).map(|b| &b.planner).unwrap_or(&self.planner) };
        p.frames = master_planner.frames.iter().map(crate::app_files::frame_to_file).collect();
        let r = |v: f32| (v * 10.0).round() / 10.0;
        let ids = self.factory_ids();
        let net = &master_planner.power_net;
        p.poles = net.poles.iter().map(|q| crate::power_net::Pole { id: q.id, x: r(q.x), y: r(q.y), tier: q.tier }).collect();
        let exists = |e: &crate::power_net::End| match e {
            crate::power_net::End::Factory(f) => ids.contains(f),
            crate::power_net::End::Pole(q) => net.poles.iter().any(|x| x.id == *q),
        };
        p.power_links = net.links.iter().copied().filter(|l| exists(&l.from) && exists(&l.to)).collect();
        p
    }

    /// Builds a factory from the portable form into a new document body.
    /// Returns it and how many parts this game version doesn't have.
    pub(crate) fn body_from_file(&mut self, f: &FactoryFile) -> (DocBody, usize) {
        let saved = self.take_live();
        let saved_master = self.user.master;
        self.user.master = false;
        self.put_live(DocBody::new(Planner::default()));
        self.planner = self.planner_from_portable(&f.plan);
        let (_, skipped) = self.insert_portable(f, Vec2::ZERO);
        self.committed = self.project_json();
        let body = self.take_live();
        self.put_live(saved);
        self.user.master = saved_master;
        (body, skipped)
    }

    /// Replaces everything with project `p`. Returns how many parts this
    /// game version doesn't have.
    pub(crate) fn load_project_file(&mut self, p: &ProjectFile) -> usize {
        let mut skipped = 0;
        let mut docs = vec![Doc { id: MASTER, name: "Project".into(), body: Some(Box::new(DocBody::new(Planner::default()))), info: None }];
        // File ids to document ids (0 is the master graph's).
        let mut ids: BTreeMap<u32, u32> = BTreeMap::new();
        for f in &p.factories {
            let mut id = f.id;
            if id == MASTER || ids.values().any(|v| *v == id) {
                id = p.factories.iter().map(|f| f.id).chain(ids.values().copied()).max().unwrap_or(0) + 1;
            }
            ids.insert(f.id, id);
        }
        for f in &p.factories {
            let (body, s) = self.body_from_file(&f.factory);
            skipped += s;
            let name = f.factory.name.clone().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "Factory".into());
            docs.push(Doc { id: ids[&f.id], name, body: Some(Box::new(body)), info: None });
        }
        if docs.len() == 1 {
            docs.push(Doc { id: 1, name: "Factory".into(), body: Some(Box::new(DocBody::new(self.default_planner()))), info: None });
        }
        // The master graph: blocks where the file puts them, then the links.
        self.load_docs(docs, MASTER, Vec::new());
        let z = self.state.pan_zoom.zoom.max(0.01);
        for f in &p.factories {
            if let Some(n) = block_of(&self.state.graph, ids[&f.id]) {
                self.state.node_positions.insert(n, (vec2(f.x, f.y) * z).to_pos2());
                self.state.graph[n].user_data.color = f.color.as_deref().and_then(portable::color_from_hex);
            }
        }
        let mut links: Vec<&portable::Link> = p.valid_links().collect();
        links.sort_by_key(|l| l.slot);
        for l in links {
            let g = &self.state.graph;
            let (Some(a), Some(b)) = (block_of(g, ids[&l.from]), block_of(g, ids[&l.to])) else { continue };
            let out = g[a].outputs.iter().map(|(_, o)| *o).find(|o| g[*o].typ.0 == l.item);
            let inp = g[b].inputs.iter().map(|(_, i)| *i).find(|i| g[*i].typ.0 == l.item);
            if let (Some(o), Some(i)) = (out, inp) {
                let pos = g.connections(i).len();
                self.state.graph.add_connection(o, i, pos);
            }
        }
        self.planner.frames = p.frames.iter().enumerate().map(|(k, f)| crate::app_files::frame_from_file(f, k as u32 + 1, Vec2::ZERO)).collect();
        {
            use crate::power_net::End;
            let poles: BTreeSet<u32> = p.poles.iter().map(|q| q.id).collect();
            let end = |e: End| match e {
                End::Factory(f) => ids.get(&f).map(|d| End::Factory(*d)),
                End::Pole(q) => poles.contains(&q).then_some(End::Pole(q)),
            };
            self.planner.power_net = crate::power_net::PowerNet {
                poles: p.poles.iter().map(|q| crate::power_net::Pole { tier: q.tier.clamp(1, 3), ..q.clone() }).collect(),
                links: p.power_links.iter().filter_map(|l| Some(crate::power_net::PowerLink { from: end(l.from)?, to: end(l.to)? })).collect(),
            };
        }
        self.sync_master();
        self.committed = self.project_json();
        self.frame_all = true;
        skipped
    }

    /// Moves the selected nodes of the open factory into a new factory.
    /// Wires that crossed the selection's edge become an Output and an
    /// Input node on each side (at the rate that flowed), linked on the
    /// master graph, so the totals don't change.
    pub(crate) fn move_selection_to_new_factory(&mut self, ctx: &egui::Context) {
        use portable::{Kind, Node, Wire};
        if self.on_master() {
            return;
        }
        let g = &self.state.graph;
        let sel: BTreeSet<NodeId> = self.state.selected_nodes.iter().copied().filter(|n| g.nodes.contains_key(*n)).collect();
        if sel.is_empty() {
            return;
        }
        if sel.len() == g.nodes.len() {
            self.say(ctx, "Select part of the factory to move (moving all of it would leave this one empty).", true);
            return;
        }
        let flow = |i: InputId, o: OutputId| self.wire_flow.get(&(i, o)).copied().unwrap_or(0.0);
        // Crossing wires by item: (outside port, inside port, flow).
        let mut into: BTreeMap<String, Vec<(OutputId, InputId, f64)>> = BTreeMap::new();
        let mut out_of: BTreeMap<String, Vec<(OutputId, InputId, f64)>> = BTreeMap::new();
        for (i, o) in g.iter_connections() {
            let (from, to) = (g[o].node, g[i].node);
            let item = g[o].typ.0.clone();
            match (sel.contains(&from), sel.contains(&to)) {
                (false, true) => into.entry(item).or_default().push((o, i, flow(i, o))),
                (true, false) => out_of.entry(item).or_default().push((o, i, flow(i, o))),
                _ => {}
            }
        }
        let rate = |list: &[(OutputId, InputId, f64)]| list.iter().map(|x| x.2).sum::<f64>();

        // The new factory: the selection as a file, plus its Inputs and Outputs.
        let order = crate::app_files::portable_order(&self.state);
        let full = self.portable_of(&self.state, &self.planner, None);
        let keep: Vec<usize> = order.iter().enumerate().filter(|(_, id)| sel.contains(id)).map(|(k, _)| k).collect();
        let new_index: BTreeMap<usize, usize> = keep.iter().enumerate().map(|(n, k)| (*k, n)).collect();
        let mut f = FactoryFile::new(None);
        f.plan = full.plan.clone();
        f.nodes = keep.iter().map(|k| full.nodes[*k].clone()).collect();
        f.wires = full.wires.iter().filter_map(|w| Some(Wire { from: *new_index.get(&w.from)?, to: *new_index.get(&w.to)?, ..*w })).collect();
        let (min_x, max_x, top) = f.nodes.iter().fold((f32::INFINITY, f32::NEG_INFINITY, f32::INFINITY), |(a, b, c), n| (a.min(n.x), b.max(n.x), c.min(n.y)));
        let file_pos = |id: NodeId| order.iter().position(|o| *o == id).and_then(|k| new_index.get(&k).copied());
        let item_input_index = |i: InputId| {
            let n = g[i].node;
            g[n].inputs.iter().filter(|(_, x)| matches!(g[*x].value, PortValue::Item(_))).position(|(_, x)| *x == i)
        };
        let output_index = |o: OutputId| g[g[o].node].outputs.iter().position(|(_, x)| *x == o);
        let io_node = |kind, item: &str, rate: f64, x: f32, y: f32| Node { kind, class: item.to_string(), machines: None, clock: None, sloops: None, rate: Some((rate * 1e4).round() / 1e4), name: None, color: None, item: None, purity: None, x, y };
        let mut row = 0.0;
        for (item, list) in &into {
            let at = f.nodes.len();
            f.nodes.push(io_node(Kind::Input, item, rate(list), min_x - 420.0, top + row));
            row += 160.0;
            for (_, i, _) in list {
                if let (Some(to), Some(input)) = (file_pos(g[*i].node), item_input_index(*i)) {
                    f.wires.push(Wire { from: at, output: 0, to, input, slot: 0 });
                }
            }
        }
        let mut row = 0.0;
        for (item, list) in &out_of {
            let at = f.nodes.len();
            f.nodes.push(io_node(Kind::Output, item, rate(list), max_x + 420.0, top + row));
            row += 160.0;
            for (o, _, _) in list {
                if let (Some(from), Some(output)) = (file_pos(g[*o].node), output_index(*o)) {
                    f.wires.push(Wire { from, output, to: at, input: 0, slot: 0 });
                }
            }
        }
        // Its plan makes what it outputs (moved Output nodes and the new
        // ones) from what it's given; it's named after its first product.
        let game = self.user.game.clone();
        let mut products: Vec<fb_model::Target> = Vec::new();
        for n in f.nodes.iter().filter(|n| n.kind == Kind::Output) {
            match products.iter_mut().find(|t| t.item == n.class) {
                Some(t) => t.rate += n.rate.unwrap_or(0.0),
                None => products.push(fb_model::Target { item: n.class.clone(), rate: n.rate.unwrap_or(0.0) }),
            }
        }
        let name = products.first().map(|t| format!("{} factory", game.item_name(&t.item))).unwrap_or_else(|| "Factory".into());
        f.plan.products = products;
        f.plan.available = into.iter().map(|(item, l)| fb_model::Target { item: item.clone(), rate: rate(l) }).collect();
        f.name = Some(name);

        // This factory: the selection goes; its edges become Outputs (what
        // it sent in) and Inputs (what it got back).
        let z = self.state.pan_zoom.zoom;
        let anchor = sel.iter().filter_map(|n| self.state.node_positions.get(*n)).fold(egui::pos2(f32::INFINITY, f32::INFINITY), |a, p| egui::pos2(a.x.min(p.x), a.y.min(p.y)));
        for n in &sel {
            self.state.graph.remove_node(*n);
            self.state.node_positions.remove(*n);
        }
        self.state.node_order.retain(|n| !sel.contains(n));
        self.state.selected_nodes.clear();
        let mut y = anchor.y;
        let mut added = Vec::new();
        for (item, list) in &into {
            let id = self.add_node(Template::Output(item.clone()), egui::pos2(anchor.x, y));
            y += 160.0 * z;
            self.set_rate(id, rate(list));
            let input = self.state.graph[id].inputs.iter().map(|(_, i)| *i).find(|i| matches!(self.state.graph[*i].value, PortValue::Item(_)));
            for (o, _, _) in list {
                if let Some(input) = input {
                    let pos = self.state.graph.connections(input).len();
                    self.state.graph.add_connection(*o, input, pos);
                }
            }
            added.push(id);
        }
        for (item, list) in &out_of {
            let id = self.add_node(Template::Input(item.clone()), egui::pos2(anchor.x, y));
            y += 160.0 * z;
            self.set_rate(id, rate(list));
            let output = self.state.graph[id].outputs.first().map(|(_, o)| *o);
            for (_, i, _) in list {
                if let Some(output) = output {
                    let pos = self.state.graph.connections(*i).len();
                    self.state.graph.add_connection(output, *i, pos);
                }
            }
            added.push(id);
        }
        self.state.selected_nodes = added;
        self.changed();

        // The new factory, linked to this one on the master graph.
        let here = self.active;
        let (body, _) = self.body_from_file(&f);
        let new = self.add_factory(f.name.as_deref().unwrap_or("Factory"), Some(body));
        self.activate(MASTER);
        for item in into.keys() {
            self.link_factories(here, new, item);
        }
        for item in out_of.keys() {
            self.link_factories(new, here, item);
        }
        self.activate(here);
        let name = self.doc_name(new);
        self.say(ctx, format!("Moved {} nodes to “{name}”, linked to this factory on the master graph.", sel.len()), false);
    }

    fn set_rate(&mut self, node: NodeId, v: f64) {
        if let Ok(i) = self.state.graph[node].get_input(crate::graph::RATE) {
            self.state.graph[i].value.set_number((v * 1e4).round() / 1e4);
        }
    }

    /// On the master graph (which must be open): sends factory `from`'s
    /// `item` to factory `to`. Returns false if either lacks the port.
    pub(crate) fn link_factories(&mut self, from: u32, to: u32, item: &str) -> bool {
        let g = &self.state.graph;
        let (Some(a), Some(b)) = (block_of(g, from), block_of(g, to)) else { return false };
        let out = g[a].outputs.iter().map(|(_, o)| *o).find(|o| g[*o].typ.0 == item);
        let inp = g[b].inputs.iter().map(|(_, i)| *i).find(|i| g[*i].typ.0 == item);
        let (Some(o), Some(i)) = (out, inp) else { return false };
        let pos = g.connections(i).len();
        self.state.graph.add_connection(o, i, pos);
        self.changed();
        true
    }

    /// Self-test only: two small factories, one feeding the other.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub(crate) fn selftest_project(&mut self, ctx: &egui::Context) {
        use portable::{Kind, Node, Wire};
        if self.docs.iter().any(|d| d.name == "Smelting") {
            return;
        }
        let node = |kind, class: &str, machines: Option<f64>, rate: Option<f64>, x: f32| Node { kind, class: class.into(), machines, clock: machines.map(|_| 100.0), sloops: None, rate, name: None, color: None, item: None, purity: None, x, y: 0.0 };
        let mut a = FactoryFile::new(Some("Smelting".into()));
        a.nodes = vec![
            node(Kind::Resource, "Desc_OreIron_C", None, Some(60.0), 0.0),
            node(Kind::Recipe, "Recipe_IngotIron_C", Some(2.0), None, 400.0),
            node(Kind::Output, "Desc_IronIngot_C", None, Some(60.0), 800.0),
        ];
        a.wires = vec![Wire { from: 0, output: 0, to: 1, input: 0, slot: 0 }, Wire { from: 1, output: 0, to: 2, input: 0, slot: 0 }];
        let mut b = FactoryFile::new(Some("Iron Rods".into()));
        b.nodes = vec![
            node(Kind::Input, "Desc_IronIngot_C", None, Some(90.0), 0.0),
            node(Kind::Recipe, "Recipe_IronRod_C", Some(6.0), None, 400.0),
            node(Kind::Output, "Desc_IronRod_C", None, Some(90.0), 800.0),
        ];
        b.wires = a.wires.clone();
        self.add_factory_file(ctx, &a);
        let smelting = self.active;
        self.add_factory_file(ctx, &b);
        let rods = self.active;
        self.activate(MASTER);
        self.link_factories(smelting, rods, "Desc_IronIngot_C");
        self.frame_all = true;
    }

    /// Adds factory file `f` to the project as a new factory and opens it.
    /// From the master graph's right-click menu, its block goes where the
    /// menu was opened and the master graph stays open.
    pub(crate) fn add_factory_file(&mut self, ctx: &egui::Context, f: &FactoryFile) {
        let (body, skipped) = self.body_from_file(f);
        let name = f.name.clone().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "Factory".into());
        let spot = self.block_spot.take().filter(|_| self.on_master());
        let id = self.add_factory(&name, Some(body));
        match spot {
            Some(at) => self.place_block(id, at),
            None => {
                self.activate(id);
                self.frame_all = true;
            }
        }
        let name = self.doc_name(id);
        if skipped > 0 {
            self.say(ctx, format!("Added “{name}” to the project. {skipped} parts weren't loaded: this game version doesn't have them."), true);
        } else {
            self.say(ctx, format!("Added “{name}” to the project."), false);
        }
    }
}
