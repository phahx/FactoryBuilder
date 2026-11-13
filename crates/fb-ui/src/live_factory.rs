//! The Live view: a project of its own, beside your planning project, built
//! from the map. Each live factory (a region drawn on the Map tab, or
//! buildings picked there) is a factory of it; its master graph links them
//! by the belts, pipes, storage and power running between them, with the
//! save's HUB. It's rebuilt as the map changes (a newer save, or the Ficsit
//! Remote Monitoring mod's live figures), and shows live figures under each
//! node.
//!
//! What's built can't be changed here: its nodes show their settings as
//! text. In planning mode, pieces can be added to a live factory: they look
//! like plans (dashed outline, "Planned"), stay editable, and are kept with
//! the live factory (`LiveFactoryDef::plan`) across rebuilds.
//!
//! While the Live view is shown, the planning project waits in a slot
//! (`ProjectSlot`); saving, autosave and rooms only ever see that one.

use crate::app::FactoryApp;
use crate::graph::{EditorState, NodeKind, PortValue, UserState};
use crate::map_import::Members;
use crate::portable::{self, FactoryFile, ProjectFile};
use crate::project::{Doc, DocBody, MASTER};
use crate::theme::*;
use crate::widgets;
use egui::{RichText, Ui};
use egui_node_graph2::{InputId, NodeId, OutputId};
use std::collections::HashSet;
use fb_model::world::World;
use fb_model::{BuildingRef, LiveFactoryDef};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::hash::{Hash, Hasher};

/// How near (cm) a building must stand to where a group remembers it.
const SAME_CM: f32 = 100.0;
/// The live project's master tab.
pub const LIVE_NAME: &str = "Live";

/// What's live about one node of a live factory's graph, drawn the way the
/// node draws everything else: counts on item icons (like the game's
/// inventory slots), a short status line in the footer, and the footer's
/// MW figure.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LiveNode {
    /// How many buildings on the map it stands for.
    pub buildings: usize,
    /// "11 of 12 producing · 87%", and whether it wants attention.
    pub status: Option<(String, bool)>,
    /// What machines hold now in their slots, by item (shown on the item
    /// icons of their ports).
    pub held: Vec<(String, f32)>,
    /// Storage: what's in it (shown under it, as an inventory).
    pub contents: Vec<(String, f32)>,
    /// MW now: drawn, or made (generators).
    pub mw: Option<f32>,
    pub makes_mw: Option<f32>,
    /// Storage: "12 of 48 slots", or a fluid buffer's fill, and whether full.
    pub fill: Option<(String, bool)>,
}

impl LiveNode {
    /// How much of `item` is held.
    pub fn held_of(&self, item: &str) -> Option<f32> {
        self.held.iter().find(|(i, _)| i == item).map(|(_, n)| *n)
    }
}

/// A count as an inventory slot shows it (big ones shortened).
pub fn count_text(n: f32, fluid: bool) -> String {
    if fluid {
        return crate::fmt::fmt_num((n as f64 * 10.0).round() / 10.0);
    }
    let n = n.round() as f64;
    if n >= 100_000.0 {
        format!("{}k", (n / 1000.0).round())
    } else if n >= 10_000.0 {
        format!("{}k", (n / 100.0).round() / 10.0)
    } else {
        crate::fmt::fmt_num(n)
    }
}

/// A node's live status line, under its footer's name (if it has one).
pub fn status_line(ui: &mut Ui, s: &UserState, node_id: NodeId) {
    if let Some((text, warn)) = s.live.get(&node_id).and_then(|l| l.status.clone()) {
        widgets::name_label(ui, RichText::new(text).small().color(if warn { ACCENT } else { TEXT_DIM }));
    }
}

/// Whether `p` is inside the polygon `poly` (even-odd rule).
pub fn inside(poly: &[[f32; 2]], p: [f32; 2]) -> bool {
    let n = poly.len();
    if n < 3 {
        return false;
    }
    let mut c = false;
    let mut j = n - 1;
    for i in 0..n {
        let (a, b) = (poly[i], poly[j]);
        if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0] {
            c = !c;
        }
        j = i;
    }
    c
}

/// The machines (ids) and storage (structure indexes) a live factory is
/// made of on `world`.
pub fn members(def: &LiveFactoryDef, world: &World) -> (BTreeSet<u32>, BTreeSet<u32>) {
    let mut machines = BTreeSet::new();
    let mut stores = BTreeSet::new();
    if def.region.len() >= 3 {
        for m in &world.machines {
            if inside(&def.region, [m.x, m.y]) && def.takes_height(m.z) {
                machines.insert(m.id);
            }
        }
        for (k, s) in world.structures.iter().enumerate() {
            if fb_model::is_storage(&s.class) && inside(&def.region, [s.x, s.y]) && def.takes_height(s.z) {
                stores.insert(k as u32);
            }
        }
    }
    let near = |a: [f32; 2], b: [f32; 2]| (a[0] - b[0]).hypot(a[1] - b[1]) <= SAME_CM;
    for b in &def.buildings {
        if fb_model::is_storage(&b.class) {
            if let Some(k) = world.structures.iter().position(|s| s.class == b.class && near([s.x, s.y], [b.x, b.y])) {
                stores.insert(k as u32);
            }
        } else if let Some(m) = world.machines.iter().find(|m| m.class == b.class && near([m.x, m.y], [b.x, b.y])) {
            machines.insert(m.id);
        }
    }
    (machines, stores)
}

/// The picked buildings, for a live factory's definition.
pub fn group_of(world: &World, machines: &BTreeSet<u32>, stores: &BTreeSet<u32>) -> Vec<BuildingRef> {
    let mut out: Vec<BuildingRef> = machines.iter().filter_map(|id| world.machines.get(*id as usize)).map(|m| BuildingRef { class: m.class.clone(), x: m.x, y: m.y }).collect();
    out.extend(stores.iter().filter_map(|k| world.structures.get(*k as usize)).map(|s| BuildingRef { class: s.class.clone(), x: s.x, y: s.y }));
    out
}

// ------------------------------------------------------------ planned pieces

/// A live factory's planned pieces: nodes, and wires among them or to built
/// nodes (named by what they are, as built nodes are rebuilt).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Plans {
    nodes: Vec<portable::Node>,
    wires: Vec<PlanWire>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct PlanWire {
    from: End,
    output: usize,
    to: End,
    input: usize,
    #[serde(default)]
    slot: usize,
    #[serde(default)]
    belt: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
enum End {
    /// A planned node (index in `Plans::nodes`).
    Planned(usize),
    /// The first built node of this kind, class and item.
    Built { kind: portable::Kind, class: String, item: Option<String> },
}

/// The planned pieces of a factory file (planned nodes, wires touching
/// them, and the wires `extra` says were planned between built nodes), as
/// `LiveFactoryDef::plan` keeps them.
fn plans_of(f: &FactoryFile, extra: &HashSet<usize>) -> Plans {
    let mut index: HashMap<usize, usize> = HashMap::new();
    let mut p = Plans::default();
    for (i, n) in f.nodes.iter().enumerate() {
        if n.planned {
            index.insert(i, p.nodes.len());
            p.nodes.push(n.clone());
        }
    }
    let end = |i: usize| match index.get(&i) {
        Some(k) => End::Planned(*k),
        None => End::Built { kind: f.nodes[i].kind, class: f.nodes[i].class.clone(), item: f.nodes[i].item.clone() },
    };
    for (k, w) in f.wires.iter().enumerate() {
        if index.contains_key(&w.from) || index.contains_key(&w.to) || extra.contains(&k) {
            p.wires.push(PlanWire { from: end(w.from), output: w.output, to: end(w.to), input: w.input, slot: w.slot, belt: w.belt });
        }
    }
    p
}

/// Adds planned pieces to a live factory's file (and None to its members).
fn add_plans(f: &mut FactoryFile, members: &mut Vec<Members>, plan: &str) {
    let Ok(p) = serde_json::from_str::<Plans>(plan) else { return };
    let base = f.nodes.len();
    let count = p.nodes.len().min(500);
    for n in p.nodes.iter().take(count) {
        f.nodes.push(portable::Node { planned: true, ..n.clone() });
        members.push(Members::None);
    }
    for w in p.wires.iter().take(2000) {
        let at = |e: &End, f: &FactoryFile| match e {
            End::Planned(k) => (*k < count).then_some(base + k),
            End::Built { kind, class, item } => f.nodes[..base].iter().position(|n| n.kind == *kind && n.class == *class && n.item == *item),
        };
        if let (Some(from), Some(to)) = (at(&w.from, f), at(&w.to, f)) {
            f.wires.push(portable::Wire { from, output: w.output, to, input: w.input, slot: w.slot, belt: w.belt });
        }
    }
}

// ------------------------------------------------------------ the Live view

/// A whole project's documents and editor, while another one is shown.
pub(crate) struct ProjectSlot {
    body: DocBody,
    docs: Vec<Doc>,
    active: u32,
    tabs: Vec<u32>,
    name: Option<String>,
    path: Option<String>,
    dirty: bool,
}

#[derive(Default)]
pub struct LiveView {
    /// The live project is the one shown (the planning one is in `other`).
    pub on: bool,
    other: Option<ProjectSlot>,
    /// What was last looked at (`refresh_key`), and what the live project
    /// was built from (`signature`).
    seen: u64,
    built_from: Option<u64>,
    /// Per live factory (document id): what each node stands for.
    pub members: HashMap<u32, Vec<(NodeId, Members)>>,
    /// Planning mode: planned pieces shown, and new ones can be added.
    pub planning: bool,
    /// What the built part of the open graph is (`built_sig`): edits that
    /// change it are taken back.
    guard: u64,
    /// Per live factory (document id): its wires as built (planned wires
    /// may be added, these can't go).
    generated: HashMap<u32, HashSet<(InputId, OutputId)>>,
    /// A live factory to open once the project is built (by name).
    pub open_name: Option<String>,
    /// Live factories arranged (by name) since the app started: the first
    /// time one is shown without a saved layout, its nodes are arranged by
    /// their drawn heights (the importer's are a guess).
    arranged: std::collections::HashSet<String>,
}

/// The built part of a graph: its nodes, their settings and its wires as
/// built (`built`; none known: every wire between built nodes). Not planned
/// pieces or planned wires, nor where nodes stand.
pub fn built_sig(state: &EditorState, built: Option<&HashSet<(InputId, OutputId)>>) -> u64 {
    let g = &state.graph;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let planned = |n: NodeId| g.nodes.get(n).is_some_and(|n| n.user_data.planned);
    for (id, n) in g.nodes.iter() {
        if n.user_data.planned {
            continue;
        }
        id.hash(&mut h);
        // What it is, not its figures (a factory block's rates move).
        serde_json::to_string(&n.user_data.kind.template()).unwrap_or_default().hash(&mut h);
        for (_, i) in &n.inputs {
            if let PortValue::Number { value, .. } = &g[*i].value {
                value.to_bits().hash(&mut h);
            }
        }
    }
    match built {
        Some(set) => {
            let mut wires: Vec<&(InputId, OutputId)> = set.iter().collect();
            wires.sort();
            for (input, output) in wires {
                let there = g.try_get_input(*input).is_some() && g.connections(*input).contains(output);
                (input, output, there).hash(&mut h);
            }
        }
        None => {
            for (input, output) in g.iter_connections() {
                if !planned(g[input].node) && !planned(g[output].node) {
                    (input, output).hash(&mut h);
                }
            }
        }
    }
    h.finish()
}

/// A file's wire as the graph has it: (input, output), given the node made
/// for each of the file's nodes.
fn wire_ids(state: &EditorState, ids: &[Option<NodeId>], w: &portable::Wire) -> Option<(InputId, OutputId)> {
    let g = &state.graph;
    let (from, to) = ((*ids.get(w.from)?)?, (*ids.get(w.to)?)?);
    let out = g.nodes.get(from)?.outputs.get(w.output)?.1;
    let inputs: Vec<InputId> = g.nodes.get(to)?.inputs.iter().filter(|(_, i)| matches!(g[*i].value, PortValue::Item(_))).map(|(_, i)| *i).collect();
    Some((*inputs.get(w.input)?, out))
}

/// The open graph's wires in the order `portable_of` writes them.
fn file_wire_ids(state: &EditorState) -> Vec<(InputId, OutputId)> {
    let g = &state.graph;
    let ids = crate::app_files::portable_order(state);
    let index: HashMap<NodeId, usize> = ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    let mut out = Vec::new();
    for &id in &ids {
        let item_inputs: Vec<InputId> = g[id].inputs.iter().filter(|(_, i)| matches!(g[*i].value, PortValue::Item(_))).map(|(_, i)| *i).collect();
        for &iid in &item_inputs {
            for o in g.connections(iid) {
                let from_node = g[o].node;
                if index.contains_key(&from_node) && g[from_node].outputs.iter().any(|(_, x)| *x == o) {
                    out.push((iid, o));
                }
            }
        }
    }
    out
}

/// Where a graph's nodes stand (unzoomed), by what they are: their
/// template (a factory block: its name, as ids follow the order of the live
/// factories), and which of that kind (in order).
type Placed = HashMap<(String, usize), egui::Pos2>;

/// Most node places kept per graph.
const PLACES_KEPT: usize = 4000;

fn node_key(kind: &NodeKind, names: &HashMap<u32, String>) -> String {
    match kind {
        NodeKind::Factory { id, .. } => format!("factory:{}", names.get(id).cloned().unwrap_or_default()),
        k => serde_json::to_string(&k.template()).unwrap_or_default(),
    }
}

fn positions_of(state: &EditorState, names: &HashMap<u32, String>) -> Placed {
    let z = state.pan_zoom.zoom.max(0.01);
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut out = HashMap::new();
    for (id, n) in state.graph.nodes.iter() {
        let key = node_key(&n.user_data.kind, names);
        let k = seen.entry(key.clone()).or_default();
        if let Some(p) = state.node_positions.get(id) {
            out.insert((key, *k), (p.to_vec2() / z).to_pos2());
        }
        *k += 1;
    }
    out
}

fn put_positions(state: &mut EditorState, placed: &Placed, names: &HashMap<u32, String>) {
    let z = state.pan_zoom.zoom.max(0.01);
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut moves = Vec::new();
    for (id, n) in state.graph.nodes.iter() {
        let key = node_key(&n.user_data.kind, names);
        let k = seen.entry(key.clone()).or_default();
        if let Some(p) = placed.get(&(key, *k)) {
            moves.push((id, (p.to_vec2() * z).to_pos2()));
        }
        *k += 1;
    }
    for (id, p) in moves {
        state.node_positions.insert(id, p);
    }
}

fn to_places(p: &Placed) -> Vec<fb_model::NodePlace> {
    let mut out: Vec<fb_model::NodePlace> = p.iter().map(|((key, n), at)| fb_model::NodePlace { key: key.clone(), n: *n as u32, x: (at.x * 10.0).round() / 10.0, y: (at.y * 10.0).round() / 10.0 }).collect();
    out.sort_by(|a, b| (&a.key, a.n).cmp(&(&b.key, b.n)));
    out.truncate(PLACES_KEPT);
    out
}

fn from_places(v: &[fb_model::NodePlace]) -> Placed {
    v.iter().filter(|p| p.x.is_finite() && p.y.is_finite() && p.x.abs() < 1e7 && p.y.abs() < 1e7).map(|p| ((p.key.clone(), p.n as usize), egui::pos2(p.x, p.y))).collect()
}

fn num(v: f64) -> String {
    crate::fmt::fmt_num((v * 10.0).round() / 10.0)
}

/// Item amounts summed by item.
fn add_up<'a>(lists: impl Iterator<Item = &'a Vec<(String, f32)>>) -> Vec<(String, f32)> {
    let mut sum: Vec<(String, f32)> = Vec::new();
    for list in lists {
        for (item, n) in list {
            match sum.iter_mut().find(|(i, _)| i == item) {
                Some(e) => e.1 += n,
                None => sum.push((item.clone(), *n)),
            }
        }
    }
    sum
}

impl FactoryApp {
    /// The documents' names, by id.
    fn doc_names(&self) -> HashMap<u32, String> {
        self.docs.iter().map(|d| (d.id, d.name.clone())).collect()
    }

    /// A live factory was renamed (`to`) or deleted (None): its layout goes
    /// with it, and its block's place on the master graph.
    pub(crate) fn move_live_layout(&mut self, from: &str, to: Option<&str>) {
        let mut l = self.live_settings();
        let mut changed = false;
        if let Some(v) = l.layouts.remove(&self.layout_key(from)) {
            changed = true;
            if let Some(to) = to {
                l.layouts.insert(self.layout_key(to), v);
            }
        }
        if let Some(master) = l.layouts.get_mut(&self.layout_key(LIVE_NAME)) {
            let old = format!("factory:{from}");
            for p in master.iter_mut().filter(|p| p.key == old) {
                changed = true;
                if let Some(to) = to {
                    p.key = format!("factory:{to}");
                }
            }
            if to.is_none() {
                master.retain(|p| p.key != old);
            }
        }
        if changed {
            self.live_save(l, false);
        }
    }

    /// Every live factory, of every save session (Live map settings).
    pub(crate) fn live_defs(&self) -> Vec<LiveFactoryDef> {
        self.live_settings().factories
    }

    /// The save session the map shows, when one is read.
    pub(crate) fn live_world(&self) -> Option<String> {
        self.map.world.as_ref().map(|w| w.save.session.clone()).filter(|s| !s.trim().is_empty())
    }

    /// The live factories of the save session the map shows.
    pub(crate) fn world_defs(&self) -> Vec<LiveFactoryDef> {
        let Some(world) = self.live_world() else { return Vec::new() };
        self.live_defs().into_iter().filter(|d| d.world == world).collect()
    }

    /// Live factories made before they knew their session belong to the
    /// first one they're seen with.
    pub(crate) fn adopt_live_defs(&mut self) {
        let Some(world) = self.live_world() else { return };
        let mut l = self.live_settings();
        let loose: Vec<String> = l.layouts.keys().filter(|k| !k.contains('|')).cloned().collect();
        if !l.factories.iter().any(|d| d.world.is_empty()) && loose.is_empty() {
            return;
        }
        for d in l.factories.iter_mut().filter(|d| d.world.is_empty()) {
            d.world = world.clone();
        }
        // Layouts saved before they were kept by session, too.
        for k in loose {
            if let Some(v) = l.layouts.remove(&k) {
                l.layouts.entry(format!("{world}|{k}")).or_insert(v);
            }
        }
        self.live_save(l, false);
    }

    /// A Live view graph's saved layout, by session: "session|graph".
    fn layout_key(&self, doc: &str) -> String {
        format!("{}|{doc}", self.live_world().unwrap_or_default())
    }

    /// Saves the live factories (Live map settings) and rebuilds.
    pub(crate) fn set_live_defs(&mut self, defs: Vec<LiveFactoryDef>) {
        let mut l = self.live_settings();
        l.factories = defs;
        self.live_save(l, false);
        self.live_view.built_from = None;
        self.live_view.seen = u64::MAX;
    }

    fn take_project(&mut self) -> ProjectSlot {
        let body = self.take_live();
        ProjectSlot {
            body,
            docs: std::mem::take(&mut self.docs),
            active: self.active,
            tabs: std::mem::take(&mut self.tabs),
            name: self.project_name.take(),
            path: self.current_path.take(),
            dirty: self.dirty,
        }
    }

    fn put_project(&mut self, s: ProjectSlot) {
        self.docs = s.docs;
        self.active = s.active;
        self.tabs = s.tabs;
        self.project_name = s.name;
        self.current_path = s.path;
        self.dirty = s.dirty;
        self.put_live(s.body);
        self.user.master = self.active == MASTER;
        self.find = None;
        self.reset_transient();
        self.rev += 1;
    }

    /// Swaps the projects without touching what's on screen (for a moment:
    /// `with_planning`).
    fn swap_quietly(&mut self) {
        let shown = self.take_project_quietly();
        if let Some(o) = self.live_view.other.take() {
            self.put_project_quietly(o);
        }
        self.live_view.other = Some(shown);
        self.live_view.on = !self.live_view.on;
        self.user.live_view = self.live_view.on;
    }

    fn take_project_quietly(&mut self) -> ProjectSlot {
        self.take_project()
    }

    fn put_project_quietly(&mut self, s: ProjectSlot) {
        self.docs = s.docs;
        self.active = s.active;
        self.tabs = s.tabs;
        self.project_name = s.name;
        self.current_path = s.path;
        self.dirty = s.dirty;
        self.put_live(s.body);
        self.user.master = self.active == MASTER;
    }

    /// Shows the other project (planning <-> live).
    fn swap_projects(&mut self) {
        self.flush_commit();
        if !self.live_view.on {
            // Leaving the planning project: save it now, not after a pause.
            self.autosave_now();
        }
        let shown = self.take_project();
        match self.live_view.other.take() {
            Some(o) => self.put_project(o),
            None => {
                // The live project, the first time: empty until built.
                self.load_docs(Vec::new(), MASTER, Vec::new());
                if let Some(d) = self.docs.iter_mut().find(|d| d.id == MASTER) {
                    d.name = LIVE_NAME.into();
                }
                self.live_view.built_from = None;
            }
        }
        self.live_view.other = Some(shown);
        self.live_view.on = !self.live_view.on;
        self.user.live_view = self.live_view.on;
        self.user.live.clear();
        self.live_view.seen = u64::MAX;
    }

    /// Shows the Live view's project.
    pub(crate) fn enter_live(&mut self) {
        if !self.live_view.on {
            self.swap_projects();
        }
        self.view = crate::map::View::Graph;
    }

    /// Back to the planning project.
    pub(crate) fn leave_live(&mut self) {
        if self.live_view.on {
            self.swap_projects();
        }
        self.view = crate::map::View::Graph;
    }

    /// Runs `f` on the planning project, whichever is shown (the Live
    /// view's stays as it is on screen).
    pub(crate) fn with_planning(&mut self, f: impl FnOnce(&mut FactoryApp)) {
        if self.live_view.on && self.live_view.other.is_some() {
            self.swap_quietly();
            f(self);
            self.swap_quietly();
        } else {
            f(self);
        }
    }

    /// Changes when the map's world, the open document, the documents or
    /// planning mode do: then the live project is looked at again.
    fn refresh_key(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (self.map.version, self.active, self.docs.iter().map(|d| d.id).collect::<Vec<u32>>(), self.live_view.planning).hash(&mut h);
        h.finish()
    }

    /// What each storage holds now (the mod's figures): its fluid, or the
    /// item it has most of.
    fn holds(&self, world: &World) -> HashMap<u32, String> {
        let mut out = HashMap::new();
        for k in 0..world.structures.len() as u32 {
            let Some(Some(s)) = self.live.structure_status.get(k as usize) else { continue };
            let item = match &s.fluid {
                Some((Some(f), _, _)) => Some(f.clone()),
                _ => s.items.iter().max_by(|a, b| a.1.total_cmp(&b.1)).map(|(i, _)| i.clone()),
            };
            if let Some(i) = item {
                out.insert(k, i);
            }
        }
        out
    }

    /// What the live project is built from: the live factories and their
    /// buildings (recipes, clocks, what storage holds), the belts and pipes,
    /// the HUB, and whether planned pieces are shown.
    fn signature(&self, world: &World, defs: &[LiveFactoryDef], holds: &HashMap<u32, String>) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.live_view.planning.hash(&mut h);
        for def in defs {
            def.name.hash(&mut h);
            for p in &def.region {
                (p[0] as i64, p[1] as i64).hash(&mut h);
            }
            for b in &def.buildings {
                (&b.class, b.x as i64, b.y as i64).hash(&mut h);
            }
            let (ms, ss) = members(def, world);
            for id in &ms {
                let m = &world.machines[*id as usize];
                (&m.class, &m.recipe, (m.clock * 1000.0) as i64, (m.boost * 1000.0) as i64, m.x as i64, m.y as i64, &m.node).hash(&mut h);
            }
            for k in &ss {
                let s = &world.structures[*k as usize];
                (&s.class, s.x as i64, s.y as i64, holds.get(k)).hash(&mut h);
            }
        }
        for l in &world.links {
            (l.from, l.to, &l.item).hash(&mut h);
        }
        for l in &world.store_links {
            (l.machine, l.store, &l.item, l.into).hash(&mut h);
        }
        world.structures.iter().filter(|s| s.class.starts_with("Build_TradingPost")).map(|s| (s.x as i64, s.y as i64, s.circuit)).collect::<Vec<_>>().hash(&mut h);
        world.hub_burners.iter().map(|b| (b.circuit, &b.fuel)).collect::<Vec<_>>().hash(&mut h);
        h.finish()
    }

    /// Keeps the Live view current: rebuilds the live project when what
    /// it's built from changed, and fills in the open graph's live lines.
    pub(crate) fn live_refresh(&mut self) {
        if !self.live_view.on || !self.game_loaded {
            return;
        }
        let key = self.refresh_key();
        if key == self.live_view.seen {
            return;
        }
        self.live_view.seen = key;
        self.adopt_live_defs();
        if let Some(world) = self.map.world.clone() {
            let defs = self.world_defs();
            let holds = self.holds(&world);
            let sig = self.signature(&world, &defs, &holds);
            if self.live_view.built_from != Some(sig) {
                self.live_build(&world, &defs, &holds);
                self.live_view.built_from = Some(sig);
            }
            if let Some(name) = self.live_view.open_name.take() {
                if let Some(id) = self.docs.iter().find(|d| d.name == name && d.id != MASTER).map(|d| d.id) {
                    self.activate(id);
                }
            }
            if self.active != MASTER {
                let name = self.doc_name(self.active);
                let saved = self.live_settings().layouts.contains_key(&self.layout_key(&name));
                if !saved && self.live_view.arranged.insert(name) {
                    self.relayout = true;
                }
            }
            self.live_lines(&world);
            self.live_view.seen = self.refresh_key();
        }
        self.live_view.guard = built_sig(&self.state, self.live_view.generated.get(&self.active));
    }

    /// Builds the live project and shows it, keeping the open tab (and its
    /// view) where it can.
    fn live_build(&mut self, world: &World, defs: &[LiveFactoryDef], holds: &HashMap<u32, String>) {
        let game = self.user.game.clone();
        let mut lp = crate::map_import::region_project(world, defs, holds, &game, self.map.info.as_ref());
        // The wires as built, before planned ones join them.
        let built_wires: HashMap<u32, Vec<portable::Wire>> = lp.file.factories.iter().map(|f| (f.id, f.factory.wires.clone())).collect();
        if self.live_view.planning {
            for (k, def) in defs.iter().filter(|d| d.valid()).enumerate() {
                let id = k as u32 + 1;
                if def.plan.is_empty() {
                    continue;
                }
                if let (Some(f), Some(m)) = (lp.file.factories.iter_mut().find(|f| f.id == id), lp.members.get_mut(&id)) {
                    add_plans(&mut f.factory, m, &def.plan);
                }
            }
        }
        let first = self.live_view.members.is_empty();
        // Where nodes were moved to, per document, to put them back.
        // (What's on screen now, then what was saved, which wins: it
        // survives reconnecting and restarting.)
        let names = self.doc_names();
        let mut placed: HashMap<String, Placed> = HashMap::new();
        placed.insert(self.doc_name(self.active), positions_of(&self.state, &names));
        for d in &self.docs {
            if let Some(b) = &d.body {
                placed.insert(d.name.clone(), positions_of(&b.state, &names));
            }
        }
        let prefix = self.layout_key("");
        for (key, places) in &self.live_settings().layouts {
            if let Some(doc) = key.strip_prefix(&prefix) {
                placed.entry(doc.to_string()).or_default().extend(from_places(places));
            }
        }
        let was = (self.active != MASTER).then(|| self.doc_name(self.active));
        let tabs: Vec<String> = self.tabs.iter().map(|t| self.doc_name(*t)).collect();
        let view = self.state.pan_zoom.clone();
        let file: ProjectFile = lp.file;
        self.loaded_ids.clear();
        self.load_project_file(&file);
        if let Some(d) = self.docs.iter_mut().find(|d| d.id == MASTER) {
            d.name = LIVE_NAME.into();
        }
        self.project_name = Some(LIVE_NAME.into());
        self.current_path = None;
        self.dirty = false;
        // What each node stands for, and the wires as built, by document.
        self.live_view.members.clear();
        self.live_view.generated.clear();
        for (file_id, (doc, ids)) in std::mem::take(&mut self.loaded_ids) {
            let state = if doc == self.active { Some(&self.state) } else { self.doc(doc).and_then(|d| d.body.as_ref()).map(|b| &b.state) };
            if let (Some(state), Some(ws)) = (state, built_wires.get(&file_id)) {
                let set: HashSet<(InputId, OutputId)> = ws.iter().filter_map(|w| wire_ids(state, &ids, w)).collect();
                self.live_view.generated.insert(doc, set);
            }
            let Some(ms) = lp.members.get(&file_id) else { continue };
            let pairs = ids.into_iter().zip(ms.iter().cloned()).filter_map(|(n, m)| Some((n?, m))).collect();
            self.live_view.members.insert(doc, pairs);
        }
        // The same tabs, and the same open one, by name.
        self.tabs = tabs.iter().filter_map(|n| self.docs.iter().find(|d| d.name == *n && d.id != MASTER).map(|d| d.id)).collect();
        if let Some(id) = was.and_then(|n| self.docs.iter().find(|d| d.name == n && d.id != MASTER).map(|d| d.id)) {
            self.activate(id);
        }
        let names = self.doc_names();
        if let Some(p) = placed.get(&self.doc_name(self.active)) {
            put_positions(&mut self.state, p, &names);
        }
        for d in &mut self.docs {
            if let (Some(b), Some(p)) = (d.body.as_mut(), placed.get(&d.name)) {
                put_positions(&mut b.state, p, &names);
            }
        }
        if !first {
            // Keep the view where it was (positions are at zoom 1).
            let z = view.zoom;
            let moved: Vec<(NodeId, egui::Pos2)> = self.state.node_positions.iter().map(|(n, p)| (n, (p.to_vec2() * z).to_pos2())).collect();
            for (n, p) in moved {
                self.state.node_positions.insert(n, p);
            }
            self.state.pan_zoom = view;
            self.frame_all = false;
        }
        self.reset_history();
        self.committed = self.project_json();
    }

    /// The open live factory's live details: per node, from the mod's
    /// figures for the buildings it stands for.
    fn live_lines(&mut self, world: &World) {
        self.user.live.clear();
        let Some(pairs) = self.live_view.members.get(&self.active).cloned() else { return };
        let game = self.user.game.clone();
        for (node, m) in pairs {
            let mut l = LiveNode::default();
            match &m {
                Members::Machines(ids) => {
                    l.buildings = ids.len();
                    let st: Vec<&fb_model::LiveMachine> = ids.iter().filter_map(|i| self.live.status.get(*i as usize).and_then(|s| s.as_ref())).collect();
                    if !st.is_empty() {
                        let producing = st.iter().filter(|s| s.producing == Some(true)).count();
                        let paused = st.iter().filter(|s| s.paused == Some(true)).count();
                        let prods: Vec<f32> = st.iter().filter_map(|s| s.productivity).collect();
                        let n = ids.len();
                        let mut text = match (n, producing, paused) {
                            (1, _, 1) => "Paused".to_string(),
                            (1, 1, _) => "Producing".to_string(),
                            (1, _, _) => "Idle".to_string(),
                            _ => format!("{producing} of {n} producing"),
                        };
                        if paused > 0 && n > 1 {
                            text += &format!(" · {paused} paused");
                        }
                        if !prods.is_empty() {
                            text += &format!(" · {}%", num(prods.iter().sum::<f32>() as f64 / prods.len() as f64));
                        }
                        l.status = Some((text, producing < n));
                        l.held = add_up(st.iter().map(|s| &s.inputs).chain(st.iter().map(|s| &s.outputs)));
                    }
                    let uses: Vec<&fb_model::world::PowerUse> = ids.iter().filter_map(|i| world.machine_power.get(i)).collect();
                    if !uses.is_empty() {
                        l.mw = Some(uses.iter().map(|u| u.now).sum());
                        let made: f32 = uses.iter().map(|u| u.makes).sum();
                        l.makes_mw = (made > 0.0).then_some(made);
                    }
                }
                Members::Stores(ks) => {
                    l.buildings = ks.len();
                    let st: Vec<&fb_model::LiveStructure> = ks.iter().filter_map(|k| self.live.structure_status.get(*k as usize).and_then(|s| s.as_ref())).collect();
                    if !st.is_empty() {
                        l.contents = add_up(st.iter().map(|s| &s.items));
                        let tanks: Vec<&(Option<String>, f32, f32)> = st.iter().filter_map(|s| s.fluid.as_ref()).collect();
                        if !tanks.is_empty() {
                            let (have, cap) = tanks.iter().fold((0.0f32, 0.0f32), |a, (_, c, m)| (a.0 + c, a.1 + m));
                            if let Some(f) = tanks.iter().find_map(|(f, c, _)| f.clone().filter(|_| *c > 0.0)) {
                                l.contents = vec![(f, have)];
                            }
                            l.fill = Some((format!("{} of {} m³", num(have as f64), num(cap as f64)), cap > 0.0 && have >= cap * 0.999));
                        } else {
                            let (used, all) = st.iter().filter_map(|s| s.slots).fold((0u32, 0u32), |a, b| (a.0 + b.0, a.1 + b.1));
                            if all > 0 {
                                l.fill = Some((format!("{used} of {all} slots"), used >= all));
                            }
                        }
                    }
                    let _ = &game;
                }
                Members::Burner(k) => {
                    let Some(b) = world.hub_burners.get(*k) else { continue };
                    l.buildings = 1;
                    l.makes_mw = Some(b.makes);
                    if let (Some(f), Some(n)) = (&b.fuel, b.fuel_amount) {
                        l.held = vec![(f.clone(), n)];
                    }
                    l.status = Some(match (&b.fuel, b.makes > 0.0) {
                        (Some(_), true) => ("Burning".to_string(), false),
                        (Some(_), false) => ("Not burning (nothing draws power)".to_string(), true),
                        (None, _) => ("No fuel".to_string(), true),
                    });
                }
                Members::None => continue,
            }
            self.user.live.insert(node, l);
        }
    }

    /// An edit in the Live view: what's built must be as it was; planned
    /// pieces are kept with their live factory. Returns whether to keep it.
    pub(crate) fn live_edit_ok(&mut self) -> bool {
        if built_sig(&self.state, self.live_view.generated.get(&self.active)) != self.live_view.guard {
            return false;
        }
        // Where nodes stand now, kept for next time.
        let name = self.doc_name(self.active);
        let places = to_places(&positions_of(&self.state, &self.doc_names()));
        let key = self.layout_key(&name);
        let mut l = self.live_settings();
        if l.layouts.get(&key) != Some(&places) {
            l.layouts.insert(key, places);
            self.live_save(l, false);
        }
        if self.live_view.planning && self.active != MASTER {
            let f = self.portable_of(&self.state, &self.planner, None);
            // Wires that aren't as built were planned (between built nodes too).
            let built = self.live_view.generated.get(&self.active);
            let extra: HashSet<usize> = file_wire_ids(&self.state).iter().enumerate().filter(|(_, w)| built.is_some_and(|b| !b.contains(w))).map(|(k, _)| k).collect();
            let p = plans_of(&f, &extra);
            let plan = if p == Plans::default() { String::new() } else { serde_json::to_string(&p).unwrap_or_default() };
            let mut l = self.live_settings();
            if let Some(d) = l.factories.iter_mut().find(|d| d.name == name) {
                if d.plan != plan {
                    d.plan = plan;
                    // Saved without a rebuild: the graph already shows it.
                    self.live_save(l, false);
                }
            }
        }
        true
    }

    /// Turns planning mode on or off (the live project is rebuilt with or
    /// without the planned pieces).
    pub(crate) fn set_planning(&mut self, on: bool) {
        self.live_view.planning = on;
        self.live_view.built_from = None;
        self.live_view.seen = u64::MAX;
    }

    /// The live factory with this name (index in the settings).
    pub(crate) fn live_index(&self, name: &str) -> Option<usize> {
        self.live_defs().iter().position(|d| d.name == name)
    }

    /// Opens live factory `name`'s graph in the Live view.
    pub(crate) fn open_live(&mut self, name: &str) {
        self.enter_live();
        self.live_view.open_name = Some(name.to_string());
        self.live_view.seen = u64::MAX;
    }

    /// A name no live factory has yet.
    pub(crate) fn unique_live_name(&self, base: &str) -> String {
        let defs = self.live_defs();
        let taken = |n: &str| defs.iter().any(|d| d.name.eq_ignore_ascii_case(n));
        if !taken(base) {
            return base.to_string();
        }
        (2..).map(|k| format!("{base} {k}")).find(|n| !taken(n)).unwrap_or_else(|| base.to_string())
    }
}

/// Shown in a live factory's left panel.
pub fn panel(app: &mut FactoryApp, ui: &mut Ui) {
    let name = app.doc_name(app.active);
    let def = app.live_defs().into_iter().find(|d| d.name == name);
    let (machines, stores) = match (&def, &app.map.world) {
        (Some(d), Some(w)) => members(d, w),
        _ => Default::default(),
    };
    let how = match &def {
        Some(d) if !d.region.is_empty() => "everything inside its region on the Map tab (and whatever is built there later)",
        _ => "the buildings picked for it on the Map tab",
    };
    ui.label(RichText::new(format!("Live: built from {how}, kept up to date as the map changes. What's built shows its settings; it isn't changed here.")).color(TEXT));
    ui.add_space(8.0);
    if app.map.world.is_some() {
        ui.label(RichText::new(format!("{} machines · {} storage", machines.len(), stores.len())).color(TEXT_DIM));
    } else {
        ui.label(RichText::new("Reading the map's save (or server)…").color(TEXT_DIM));
    }
    ui.add_space(10.0);
    let mut planning = app.live_view.planning;
    if ui
        .checkbox(&mut planning, "Planning mode")
        .on_hover_text("Add planned pieces to this live factory (right-click the canvas). They're marked Planned, stay editable and are kept with it; turn this off to see only what's built.")
        .changed()
    {
        app.set_planning(planning);
    }
    ui.add_space(10.0);
    if widgets::button(ui, "Show on map").clicked() {
        if let Some(k) = app.live_index(&name) {
            app.show_live_on_map(k);
        }
    }
}
