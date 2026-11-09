//! The Factory Builder window: menu bar, production planner (left), node
//! graph (centre), overview (right), status bar. Styled after the game's own
//! UI (theme.rs); interaction follows standard editor conventions: menus with
//! shortcuts, undo/redo, right-click context menus, Delete key, Save/Save as.

use crate::assets::Assets;
use crate::backend::{Backend, Msg};
use crate::dialogs::{Dialog, Pending};
use crate::graph::*;
use crate::theme::{self, *};
use egui::{vec2, Align, Align2, Key, KeyboardShortcut, Layout, Modifiers, RichText};
use egui_node_graph2::{NodeDataTrait, NodeId, NodeResponse};
use fb_engine::sim::{simulate, SimLink, SimNode, SimResult};
use fb_model::*;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

pub(crate) const PROJECT_VERSION: u32 = 2;
pub(crate) const AUTOSAVE: &str = "autosave";
/// Self-test only: an image export that goes to the probe, not a file.
pub(crate) const SELFTEST_EXPORT: &str = "selftest-export";
const HISTORY_LIMIT: usize = 100;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Planner {
    pub targets: Vec<Target>,
    pub inputs: Vec<Target>,
    pub objective: Objective,
    pub enabled: BTreeSet<String>,
    pub limits: BTreeMap<String, f64>,
    /// Overview extractor choices (miner tier, purity, clock speed).
    #[serde(default)]
    pub extraction: crate::overview_extra::ExtractionSettings,
    #[serde(skip)]
    pub recipe_filter: String,
    /// Grid or generate, and the generators allowed (old projects: grid).
    #[serde(default)]
    pub power: PowerSettings,
    /// Comment frames on this document's graph (kept with the plan so undo,
    /// files and rooms carry them).
    #[serde(default)]
    pub frames: Vec<crate::frames::CommentFrame>,
    /// The master graph's power grids (only the master graph has any).
    #[serde(default, skip_serializing_if = "crate::power_net::PowerNet::is_empty")]
    pub power_net: crate::power_net::PowerNet,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Project {
    pub version: u32,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    pub planner: Planner,
    pub editor: EditorState,
}

#[derive(Default)]
pub(crate) struct Summary {
    pub power_mw: f64,
    /// MW the generators in the graph produce; None when there are none.
    pub generation: Option<f64>,
    /// building class -> (whole machines, machines at 100%)
    pub buildings: BTreeMap<String, (f64, f64)>,
    pub resources: BTreeMap<String, f64>,
    pub inputs: BTreeMap<String, f64>,
    /// item, delivered, wanted
    pub outputs: Vec<(String, f64, f64)>,
    pub sunk: BTreeMap<String, f64>,
    pub sink_points: f64,
    pub issues: Vec<(NodeId, String)>,
    pub power_shards: u32,
    pub somersloops: u32,
    /// Extractor building class -> (whole machines, machines at 100%).
    pub extractors: BTreeMap<String, (f64, f64)>,
    /// Raw item -> its extractor nodes, as "3 Miner Mk.1 on Normal nodes at 100%".
    pub extracted: BTreeMap<String, Vec<String>>,
}

pub struct FactoryApp {
    pub(crate) backend: Backend,
    pub(crate) state: EditorState,
    pub(crate) user: UserState,
    pub(crate) templates: AllTemplates,
    pub(crate) planner: Planner,
    pub(crate) status: Option<Status>,
    pub(crate) game_loaded: bool,
    pub(crate) game_error: Option<String>,
    pub(crate) toast: Option<(String, bool, f64)>,
    pub(crate) projects: Vec<String>,
    /// None until the project has been saved under a name.
    pub(crate) project_name: Option<String>,
    pub(crate) dirty: bool,
    pub(crate) dialog: Dialog,
    pub(crate) after_save: Option<Pending>,
    pub(crate) undo: Vec<String>,
    pub(crate) redo: Vec<String>,
    pub(crate) committed: String,
    pub(crate) pending_commit: bool,
    pub(crate) last_change: f64,
    /// Bumped by every change to the project (autosave compares it).
    pub(crate) rev: u64,
    pub(crate) autosaved_rev: u64,
    pub(crate) autosave_in_flight: bool,
    pub(crate) tried_autoload: bool,
    pub(crate) solving: bool,
    pub(crate) solve_notes: Vec<String>,
    pub(crate) relayout: bool,
    pub(crate) frame_all: bool,
    pub(crate) focus_node: Option<NodeId>,
    pub(crate) context_menu: Option<(NodeId, egui::Pos2)>,
    /// Nodes the open context menu acts on (the selection when it opened).
    /// The graph editor clears its selection when you click on the menu,
    /// so the menu must not read the live selection.
    pub(crate) context_targets: Vec<NodeId>,
    #[cfg(test)]
    pub(crate) menu_swatches: Vec<egui::Rect>,
    pub(crate) summary: Summary,
    pub(crate) sim_cache: SimCache,
    /// Unzoomed node heights measured on the last frame (for Arrange).
    pub(crate) node_heights: BTreeMap<NodeId, f32>,
    /// Screen rects of the nodes on the last frame.
    pub(crate) node_rects: BTreeMap<NodeId, egui::Rect>,
    #[cfg(test)]
    pub(crate) port_hooks: egui_node_graph2::PortLocations,
    pub(crate) finder: Option<crate::finder::FinderState>,
    pub(crate) catalog: Option<crate::finder::Catalog>,
    pub(crate) editor_rect: egui::Rect,
    /// Custom colour being edited in the node menu.
    pub(crate) custom_color: Option<[u8; 3]>,
    pub(crate) selftest: bool,
    pub(crate) selftest_view: String,
    pub(crate) room: crate::room::RoomUi,
    /// The .fbfactory file this factory was opened from or saved to.
    pub(crate) current_path: Option<String>,
    pub(crate) export: Option<crate::app_files::ImageExport>,
    /// The backend was asked for a file passed at launch.
    pub(crate) startup_checked: bool,
    /// First-launch setup (or just its icons step), while open.
    pub(crate) wizard: Option<crate::setup::Wizard>,
    /// Flow per minute on each wire (input end, output end), from the simulation.
    pub(crate) wire_flow: std::collections::HashMap<(egui_node_graph2::InputId, egui_node_graph2::OutputId), f64>,
    /// View > Belt and pipe labels.
    pub(crate) wire_labels: bool,
    /// Graph or Map tab.
    pub(crate) view: crate::map::View,
    /// Fit the view to the selected nodes on the next frame.
    pub(crate) frame_selected: bool,
    pub(crate) map: crate::map::MapState,
    #[cfg(test)]
    pub(crate) wire_label_texts: Vec<String>,
    /// "Not now" on the icons notice, for this session.
    pub(crate) notice_dismissed: bool,
    /// The project's documents: the master graph (id 0) and the factories.
    pub(crate) docs: Vec<crate::project::Doc>,
    /// The document in the editor.
    pub(crate) active: u32,
    /// Factories open as tabs, in tab order.
    pub(crate) tabs: Vec<u32>,
    /// A tab was clicked: open that document before the next frame's work.
    pub(crate) switch_to: Option<u32>,
    /// Comment frames being dragged, renamed or opened.
    pub(crate) frames: crate::frames::FrameUi,
    /// The Find bar, while open.
    pub(crate) find: Option<crate::navigate::FindState>,
    /// View › Minimap.
    pub(crate) minimap: bool,
    /// View › Power connections: power grids on the master graph.
    pub(crate) power_links: bool,
    /// View › Flow direction: arrows moving along wires.
    pub(crate) flow_arrows: bool,
    /// The master graph's right-click menu on empty canvas: where it opened
    /// (screen) and where a factory added from it goes (unzoomed graph).
    pub(crate) master_menu: Option<(egui::Pos2, egui::Pos2)>,
    /// A factory being added from that menu (via a file or share code) goes
    /// here, and the master graph stays open.
    pub(crate) block_spot: Option<egui::Pos2>,
    /// A factory's right-click menu (from its tab or the factory list).
    pub(crate) factory_menu: Option<(u32, egui::Pos2)>,
    /// Node being renamed (F2), and the text so far.
    pub(crate) node_rename: Option<(NodeId, String)>,
    /// View › Snap to grid.
    pub(crate) snap: bool,
    /// Where dragged nodes would be without snapping (zoomed units).
    pub(crate) snap_raw: BTreeMap<NodeId, egui::Pos2>,
}

/// The background grid, in graph units; snapping uses it too.
pub(crate) const GRID: f32 = 32.0;

/// `v` (graph units) on the nearest grid line.
pub(crate) fn snap_to_grid(v: f32) -> f32 {
    (v / GRID).round() * GRID
}

fn is_selftest() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::window()
            .and_then(|w| js_sys::Reflect::get(&w, &"__fbSelftest".into()).ok())
            .is_some_and(|v| v.is_truthy())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        false
    }
}

impl FactoryApp {
    #[cfg(target_arch = "wasm32")]
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        Self::with_backend(&cc.egui_ctx, Backend::new(cc.egui_ctx.clone()))
    }

    pub fn with_backend(ctx: &egui::Context, backend: Backend) -> Self {
        egui_extras::install_image_loaders(ctx);
        theme::apply(ctx);
        let mut user = UserState {
            zoom: 1.0,
            ..Default::default()
        };
        user.assets = Some(Assets::new(backend.clone()));
        let mut state = EditorState::new(1.0);
        state.style = theme::graph_style();
        let mut app = Self {
            backend,
            state,
            user,
            templates: AllTemplates(Vec::new()),
            planner: Planner::default(),
            status: None,
            game_loaded: false,
            game_error: None,
            toast: None,
            projects: Vec::new(),
            project_name: None,
            dirty: false,
            dialog: Dialog::None,
            after_save: None,
            undo: Vec::new(),
            redo: Vec::new(),
            committed: String::new(),
            pending_commit: false,
            last_change: 0.0,
            rev: 0,
            autosaved_rev: 0,
            autosave_in_flight: false,
            tried_autoload: false,
            solving: false,
            solve_notes: Vec::new(),
            relayout: false,
            frame_all: false,
            focus_node: None,
            context_menu: None,
            context_targets: Vec::new(),
            #[cfg(test)]
            menu_swatches: Vec::new(),
            summary: Summary::default(),
            sim_cache: None,
            node_heights: BTreeMap::new(),
            node_rects: BTreeMap::new(),
            #[cfg(test)]
            port_hooks: Default::default(),
            finder: None,
            catalog: None,
            editor_rect: egui::Rect::NOTHING,
            custom_color: None,
            selftest: is_selftest(),
            selftest_view: String::new(),
            room: crate::room::RoomUi::default(),
            current_path: None,
            export: None,
            startup_checked: false,
            wizard: None,
            notice_dismissed: false,
            wire_flow: Default::default(),
            wire_labels: true,
            view: Default::default(),
            frame_selected: false,
            map: Default::default(),
            #[cfg(test)]
            wire_label_texts: Vec::new(),
            docs: Vec::new(),
            active: 1,
            tabs: Vec::new(),
            switch_to: None,
            frames: Default::default(),
            find: None,
            minimap: true,
            power_links: true,
            flow_arrows: true,
            master_menu: None,
            block_spot: None,
            factory_menu: None,
            node_rename: None,
            snap: false,
            snap_raw: BTreeMap::new(),
        };
        app.new_workspace();
        app.committed = app.project_json();
        app.autosaved_rev = app.rev;
        app.backend.call("status", json!({}), Msg::Status);
        app.backend.call("load_game", json!({}), Msg::Game);
        app.backend.call("list_projects", json!({}), Msg::Projects);
        app
    }

    pub(crate) fn assets(&mut self) -> &mut Assets {
        self.user
            .assets
            .as_mut()
            .expect("assets are created in with_backend")
    }

    pub(crate) fn say(&mut self, ctx: &egui::Context, text: impl Into<String>, error: bool) {
        self.toast = Some((text.into(), error, ctx.input(|i| i.time)));
    }

    pub(crate) fn title(&self) -> String {
        let untitled = if self.factory_count() > 1 { "Untitled project" } else { "Untitled factory" };
        let name = self.project_name.as_deref().unwrap_or(untitled);
        if self.dirty {
            format!("{name}*")
        } else {
            name.to_string()
        }
    }

    // ------------------------------------------------------------- history

    pub(crate) fn project_json(&self) -> String {
        #[derive(Serialize)]
        struct ProjectRef<'a> {
            version: u32,
            name: &'a Option<String>,
            path: &'a Option<String>,
            planner: &'a Planner,
            editor: &'a EditorState,
        }
        serde_json::to_string(&ProjectRef {
            version: PROJECT_VERSION,
            name: &self.project_name,
            path: &self.current_path,
            planner: &self.planner,
            editor: &self.state,
        })
        .unwrap_or_default()
    }

    /// Something changed; it becomes an undo step at the end of the frame
    /// (after drags and typing finish).
    pub(crate) fn changed(&mut self) {
        self.pending_commit = true;
    }

    fn commit(&mut self, ctx: &egui::Context) {
        if !self.pending_commit || ctx.input(|i| i.pointer.any_down()) {
            return;
        }
        if self.flush_commit() {
            self.last_change = ctx.input(|i| i.time);
        }
    }

    /// Makes a pending change an undo step now. Returns true if anything changed.
    pub(crate) fn flush_commit(&mut self) -> bool {
        if !self.pending_commit {
            return false;
        }
        self.pending_commit = false;
        let now = self.project_json();
        if now == self.committed {
            return false;
        }
        self.undo.push(std::mem::replace(&mut self.committed, now));
        if self.undo.len() > HISTORY_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.dirty = true;
        self.rev += 1;
        self.share_doc();
        true
    }

    fn restore(&mut self, json: &str) {
        if let Ok(p) = serde_json::from_str::<Project>(json) {
            self.state = p.editor;
            self.state.style = theme::graph_style();
            self.state.pan_zoom.started = false;
            let keep_filter = std::mem::take(&mut self.planner.recipe_filter);
            self.planner = p.planner;
            self.planner.recipe_filter = keep_filter;
        }
    }

    pub(crate) fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub(crate) fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub(crate) fn undo(&mut self) {
        if let Some(prev) = self.undo.pop() {
            self.redo
                .push(std::mem::replace(&mut self.committed, prev.clone()));
            self.restore(&prev);
            self.dirty = true;
            self.rev += 1;
            self.share_doc();
        }
    }

    pub(crate) fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            self.undo
                .push(std::mem::replace(&mut self.committed, next.clone()));
            self.restore(&next);
            self.dirty = true;
            self.rev += 1;
            self.share_doc();
        }
    }

    /// The plan a new factory starts with.
    pub(crate) fn default_planner(&self) -> Planner {
        let mut p = Planner { extraction: self.planner.extraction.clone(), ..Default::default() };
        if self.game_loaded {
            let g = &self.user.game;
            p.targets = vec![Target { item: "Desc_IronPlateReinforced_C".into(), rate: 5.0 }];
            p.enabled = g.recipes.values().filter(|r| !r.alternate && !r.is_generator()).map(|r| r.class.clone()).collect();
            p.limits = g.resource_limits.clone();
            p.objective = Objective::Resources;
            p.power = PowerSettings::default();
        }
        p
    }

    /// Start over with one factory, from `json` (or empty), with a fresh history.
    pub(crate) fn reset_to(&mut self, json: Option<&str>, name: Option<String>) {
        let keep_filter = std::mem::take(&mut self.planner.recipe_filter);
        let planner = self.default_planner();
        self.new_workspace();
        self.planner = planner;
        self.planner.recipe_filter = keep_filter;
        self.current_path = None;
        if let Some(d) = self.doc_mut(1) {
            d.name = name.clone().unwrap_or_else(|| "Factory".into());
        }
        if let Some(j) = json {
            self.restore(j);
        }
        if self.game_loaded {
            let game = self.user.game.clone();
            crate::graph::add_missing_sloop_inputs(&game, &mut self.state.graph);
        }
        self.project_name = name;
        self.reset_history();
        self.solve_notes.clear();
        self.frame_all = true;
    }

    // ------------------------------------------------------------ projects

    pub(crate) fn run_pending(&mut self, p: Pending) {
        match p {
            Pending::New => {
                self.reset_to(None, None);
                self.share_doc();
            }
            Pending::OpenDialog => self.pick_open(crate::app_files::OpenPurpose::Open),
            Pending::OpenPath(path) => self.read_file(crate::app_files::OpenPurpose::Open, path),
            Pending::OpenFactory(f) => {
                let ctx = self.backend_ctx();
                self.open_factory(&ctx, *f, None);
            }
            Pending::OpenProject(p) => {
                let ctx = self.backend_ctx();
                self.open_project(&ctx, *p, None);
                self.view = crate::map::View::Graph;
            }
        }
    }

    pub(crate) fn backend_ctx(&self) -> egui::Context {
        self.backend.ctx()
    }

    /// Every document starts a fresh history; nothing unsaved.
    pub(crate) fn reset_history(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.committed = self.project_json();
        self.pending_commit = false;
        self.dirty = false;
        for d in &mut self.docs {
            if let Some(b) = d.body.as_mut() {
                b.undo.clear();
                b.redo.clear();
                b.committed.clear();
            }
        }
        self.rev += 1;
    }

    pub(crate) fn mark_saved(&mut self) {
        self.flush_commit();
        self.dirty = false;
    }

    pub(crate) fn add_node_at(&mut self, template: Template, pos: egui::Pos2) -> NodeId {
        self.add_node(template, pos)
    }

    pub(crate) fn share_doc_now(&mut self) {
        self.share_doc();
    }

    /// New/Open: ask to save first when there are unsaved changes.
    pub(crate) fn guard(&mut self, p: Pending) {
        if self.dirty {
            self.dialog = Dialog::Unsaved { then: p };
        } else {
            self.run_pending(p);
        }
    }

    fn autosave(&mut self, ctx: &egui::Context) {
        if self.selftest || !self.game_loaded || self.autosave_in_flight {
            return;
        }
        let now = ctx.input(|i| i.time);
        if now - self.last_change < 2.0 {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
            return;
        }
        if self.pending_commit {
            return;
        }
        if self.rev != self.autosaved_rev {
            self.autosaved_rev = self.rev;
            self.autosave_in_flight = true;
            let json = self.workspace_json();
            self.backend.call::<()>(
                "save_project",
                json!({ "name": AUTOSAVE, "json": json }),
                |r| Msg::Saved {
                    name: AUTOSAVE.into(),
                    autosave: true,
                    result: r,
                },
            );
        }
    }

    /// Reopen the last session once the game data and project list are in.
    pub(crate) fn try_autoload(&mut self) {
        if self.tried_autoload || !self.game_loaded || self.selftest || !self.startup_checked {
            return;
        }
        self.tried_autoload = true;
        if self.projects.iter().any(|p| p == AUTOSAVE) {
            let name = AUTOSAVE.to_string();
            self.backend
                .call("load_project", json!({ "name": AUTOSAVE }), move |r| {
                    Msg::Loaded { name, result: r }
                });
        }
    }

    // ------------------------------------------------------------ messages

    fn handle_messages(&mut self, ctx: &egui::Context) {
        for msg in self.backend.drain() {
            match msg {
                Msg::Status(Ok(s)) => {
                    let first = self.status.is_none();
                    if first {
                        let v = &s.settings.view;
                        (self.snap, self.wire_labels, self.minimap, self.power_links, self.flow_arrows) = (v.snap_to_grid, v.wire_labels, v.minimap, v.power_links, v.flow_arrows);
                        (self.map.hide_power, self.map.hide_flow) = (!v.map_power, !v.map_belt_flow);
                    }
                    let setup_done = s.settings.setup_done;
                    self.status = Some(s.clone());
                    // The self-test drives its own views.
                    if first && !self.selftest {
                        if !setup_done {
                            self.open_setup(crate::setup::Wizard::first_launch(&s));
                        } else if self.game_error.is_some() && matches!(self.dialog, Dialog::None) {
                            self.dialog = Dialog::settings(&s.settings);
                        }
                    }
                }
                Msg::Status(Err(e)) => self.say(ctx, e, true),
                Msg::Game(Ok(game)) => self.set_game(game),
                Msg::Game(Err(e)) => {
                    self.game_error = Some(e);
                    let setup_done = self.status.as_ref().is_some_and(|s| s.settings.setup_done);
                    if setup_done && self.wizard.is_none() && matches!(self.dialog, Dialog::None) {
                        let settings = self
                            .status
                            .as_ref()
                            .map(|s| s.settings.clone())
                            .unwrap_or_default();
                        self.dialog = Dialog::settings(&settings);
                    }
                }
                Msg::Image(key, r) => self.assets().on_image(ctx, key, r),
                Msg::Font(i, r) => self.assets().on_font(ctx, i, r),
                m @ (Msg::GameDirs(_)
                | Msg::GameChecked { .. }
                | Msg::PickedFolder { .. }
                | Msg::InstallDirFor(_)
                | Msg::Installed(_)
                | Msg::Restarted(_)
                | Msg::ExtractStarted(_)
                | Msg::Extract(_)
                | Msg::DevDone(_)) => self.on_setup_msg(ctx, m),
                m @ (Msg::Saves(_) | Msg::PickedSaveGame(_) | Msg::SaveUnlocks(_)) => self.on_progress_msg(ctx, m),
                m @ (Msg::MapInfo(_) | Msg::WorldSessions(_) | Msg::WorldLoaded { .. } | Msg::WorldLatest(_) | Msg::SelftestSave(_) | Msg::MapSprites(_)) => self.on_map_msg(m),
                Msg::Solved(r) => {
                    self.solving = false;
                    match r {
                        Ok(SolveOutcome::Solved(plan)) => {
                            self.solve_notes.clear();
                            self.apply_plan(&plan);
                            // Each resource through its extractor.
                            crate::extractors::add_to_plan(&mut self.state, &mut self.user, &self.planner.extraction);
                            // Shared belts and pipes through mergers and splitters.
                            crate::routers::tidy(&mut self.state, &mut self.user);
                            self.changed();
                            self.relayout = true;
                            self.say(
                                ctx,
                                format!(
                                    "Planned {} machines and nodes using {} MW.",
                                    plan.nodes.len(),
                                    crate::fmt::fmt_num(plan.power_mw)
                                ),
                                false,
                            );
                        }
                        Ok(SolveOutcome::Infeasible(why)) => self.solve_notes = why,
                        Err(e) => self.solve_notes = vec![e],
                    }
                }
                Msg::Projects(Ok(p)) => {
                    self.projects = p;
                    self.try_autoload();
                }
                Msg::Projects(Err(e)) => self.say(ctx, e, true),
                Msg::Saved {
                    name,
                    autosave,
                    result,
                } => {
                    if autosave {
                        self.autosave_in_flight = false;
                    }
                    match result {
                        Ok(()) if !autosave => {
                            self.project_name = Some(name.clone());
                            self.dirty = false;
                            self.committed = self.project_json();
                            self.say(ctx, format!("Saved “{name}”."), false);
                            if let Some(p) = self.after_save.take() {
                                self.run_pending(p);
                            }
                        }
                        Ok(()) => {}
                        Err(e) => {
                            self.after_save = None;
                            self.say(ctx, format!("Couldn't save: {e}"), true);
                        }
                    }
                    self.backend.call("list_projects", json!({}), Msg::Projects);
                }
                Msg::Loaded { name, result: Ok(json) } if crate::project::Workspace::parse(&json).is_some() => {
                    // A session with several documents (always the autosave).
                    if let Some(w) = crate::project::Workspace::parse(&json) {
                        self.restore_workspace(w);
                        if name == AUTOSAVE {
                            self.autosaved_rev = self.rev;
                        }
                        self.frame_all = true;
                    }
                }
                Msg::Loaded { name, result } => {
                    let parsed = result.and_then(|s| {
                        serde_json::from_str::<Project>(&s)
                            .map(|p| (s, p))
                            .map_err(|e| format!("not a Factory Builder project ({e})"))
                    });
                    match parsed {
                        Ok((json, p)) => {
                            let restored_name = if name == AUTOSAVE {
                                p.name.clone()
                            } else {
                                Some(name.clone())
                            };
                            self.reset_to(Some(&json), restored_name);
                            self.current_path = p.path.clone();
                            if name == AUTOSAVE {
                                // The session may hold changes that were never saved.
                                self.autosaved_rev = self.rev;
                            } else {
                                self.say(ctx, format!("Opened “{name}”."), false);
                            }
                        }
                        Err(e) => self.say(ctx, format!("Couldn't open “{name}”: {e}"), true),
                    }
                }
                m @ (Msg::PickedOpen { .. }
                | Msg::FileRead { .. }
                | Msg::PickedSave { .. }
                | Msg::FileWritten { .. }
                | Msg::StartupFile(_)) => self.handle_file_msg(ctx, m),
                Msg::Deleted { name, result } => {
                    match result {
                        Ok(()) => self.say(ctx, format!("Deleted “{name}”."), false),
                        Err(e) => self.say(ctx, e, true),
                    }
                    self.backend.call("list_projects", json!({}), Msg::Projects);
                }
                Msg::RoomHosted(Ok(info)) => {
                    let name = self.user_name();
                    self.room.reset();
                    self.room.mode = crate::room::Mode::Hosting(info);
                    self.room.me = 0;
                    self.room.peers.insert(0, fb_model::room::Peer { id: 0, name, color: [0xFA, 0x95, 0x49] });
                    self.share_doc();
                    self.dialog = Dialog::RoomInfo;
                }
                Msg::RoomHosted(Err(e)) => {
                    self.room.reset();
                    self.dialog = Dialog::OpenRoom { name: self.user_name(), error: Some(e) };
                }
                Msg::RoomJoined(Ok(())) => {
                    self.room.mode = crate::room::Mode::Joined;
                    self.dialog = Dialog::None;
                }
                Msg::RoomJoined(Err(e)) => {
                    self.room.reset();
                    if let Dialog::JoinRoom { error, busy, .. } = &mut self.dialog {
                        *error = Some(e);
                        *busy = false;
                    } else {
                        self.say(ctx, e, true);
                    }
                }
                Msg::RoomEvents(r) => {
                    self.room.poll_in_flight = false;
                    for e in r.unwrap_or_default() {
                        self.room_event(ctx, e);
                    }
                }
            }
        }
    }

    pub(crate) fn reload_after_settings(&mut self, ctx: &egui::Context) {
        self.backend.call("load_game", json!({}), Msg::Game);
        self.assets().reload(ctx);
    }

    fn set_game(&mut self, game: GameData) {
        self.game_error = None;
        self.templates = AllTemplates::from_game(&game);
        self.catalog = Some(crate::finder::Catalog::new(&game));
        // A project restored before the game data arrived: old recipe nodes
        // get the Somersloops field, without counting as an edit.
        if crate::graph::add_missing_sloop_inputs(&game, &mut self.state.graph) {
            self.committed = self.project_json();
        }
        if self.planner.enabled.is_empty() {
            self.planner.enabled = game
                .recipes
                .values()
                .filter(|r| !r.alternate && !r.is_generator())
                .map(|r| r.class.clone())
                .collect();
        }
        if self.planner.limits.is_empty() {
            self.planner.limits = game.resource_limits.clone();
        }
        if self.planner.targets.is_empty() {
            self.planner.targets.push(Target {
                item: "Desc_IronPlateReinforced_C".into(),
                rate: 5.0,
            });
        }
        self.user.game = Rc::new(game);
        self.game_loaded = true;
        self.committed = self.project_json();
        self.sync_master();
        if self.selftest {
            // Exercise the whole wasm path: solve -> graph -> LP flow sim -> icons.
            self.planner.targets = vec![
                Target {
                    item: "Desc_ModularFrameHeavy_C".into(),
                    rate: 4.0,
                },
                Target {
                    item: "Desc_Plastic_C".into(),
                    rate: 30.0,
                },
            ];
            self.planner.enabled = self.user.game.recipes.keys().cloned().collect();
            self.request_plan();
        }
        self.backend.call("startup_file", json!({}), Msg::StartupFile);
    }

    pub(crate) fn request_plan(&mut self) {
        self.solving = true;
        let req = SolveRequest {
            targets: self.planner.targets.clone(),
            inputs: self.planner.inputs.clone(),
            enabled_recipes: self.planner.enabled.iter().cloned().collect(),
            resource_limits: self.planner.limits.clone(),
            objective: self.planner.objective,
            power: self.planner.power.clone(),
            extraction_mw: crate::extractors::mw_per_unit(&self.user.game, &self.planner.extraction),
        };
        self.backend
            .call("solve", json!({ "request": req }), Msg::Solved);
    }

    // --------------------------------------------------------------- graph

    pub(crate) fn add_node(&mut self, template: Template, pos: egui::Pos2) -> NodeId {
        crate::project::add_node_to(&mut self.state, &mut self.user, template, pos)
    }

    fn set_number(&mut self, node: NodeId, name: &str, v: f64) {
        if let Ok(input) = self.state.graph[node].get_input(name) {
            self.state.graph[input].value.set_number(v);
        }
    }

    fn apply_plan(&mut self, plan: &Plan) {
        self.state = EditorState::new(1.0);
        self.state.style = theme::graph_style();
        let mut ids = Vec::with_capacity(plan.nodes.len());
        for node in &plan.nodes {
            let (template, number) = match &node.kind {
                PlanNodeKind::Recipe { recipe, machines } => (
                    Template::Recipe(recipe.clone()),
                    Some((MACHINES, *machines)),
                ),
                PlanNodeKind::Resource { item, rate } => {
                    (Template::Resource(item.clone()), Some((RATE, *rate)))
                }
                PlanNodeKind::Input { item, rate } => {
                    (Template::Input(item.clone()), Some((RATE, *rate)))
                }
                PlanNodeKind::Output { item, rate } => {
                    (Template::Output(item.clone()), Some((RATE, *rate)))
                }
                PlanNodeKind::Surplus { item, .. } => (Template::Sink(item.clone()), None),
            };
            let id = self.add_node(template, egui::pos2(node.pos[0], node.pos[1]));
            if let Some((name, v)) = number {
                self.set_number(id, name, v);
            }
            ids.push(id);
        }
        for e in &plan.edges {
            let (Some(&from), Some(&to)) = (ids.get(e.from), ids.get(e.to)) else {
                continue;
            };
            let g = &self.state.graph;
            let out = g[from]
                .outputs
                .iter()
                .map(|(_, o)| *o)
                .find(|o| g[*o].typ.0 == e.item);
            let inp = g[to]
                .inputs
                .iter()
                .map(|(_, i)| *i)
                .find(|i| g[*i].typ.0 == e.item);
            if let (Some(out), Some(inp)) = (out, inp) {
                let pos = self.state.graph.connections(inp).len();
                self.state.graph.add_connection(out, inp, pos);
            }
        }
    }

    /// Lays the graph out, lining nodes up on the ports (`ports`, as just
    /// drawn) their wires join, so wires run level.
    fn arrange(&mut self, ports: &egui_node_graph2::PortLocations) {
        use egui_node_graph2::AnyParameterId as P;
        let ids: Vec<NodeId> = self.state.graph.iter_nodes().collect();
        let index: BTreeMap<NodeId, usize> =
            ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
        let g = &self.state.graph;
        let heights: Vec<f32> = ids
            .iter()
            .map(|id| self.node_heights.get(id).copied().unwrap_or(260.0))
            .collect();
        // How far down its node a port's k-th wire joins (graph units).
        let z = self.state.pan_zoom.zoom.max(0.01);
        let port_y = |port: P, k: usize, node: NodeId| {
            let top = self.node_rects.get(&node)?.min.y;
            let hooks = ports.get(&port)?;
            let at = hooks.get(k).or(hooks.last())?;
            Some((at.y - top) / z)
        };
        let wires: Vec<(usize, usize, f32, f32)> = g
            .iter_connections()
            .map(|(i, o)| {
                let (from, to) = (g[o].node, g[i].node);
                let (a, b) = (index[&from], index[&to]);
                let k = g.connections(i).iter().position(|c| *c == o).unwrap_or(0);
                let pa = port_y(P::Output(o), 0, from).unwrap_or(heights[a] / 2.0);
                let pb = port_y(P::Input(i), k, to).unwrap_or(heights[b] / 2.0);
                (a, b, pa, pb)
            })
            .collect();
        // Mergers, splitters and poles are narrow; their columns are too.
        let widths: Vec<f32> = ids.iter().map(|id| g[*id].user_data.knot_size().map_or(theme::graph_style().node_width, |s| s.x)).collect();
        let pos = fb_engine::layout::layered_ports_widths(ids.len(), &wires, &widths, &heights);
        let zoom = self.state.pan_zoom.zoom;
        for (id, p) in ids.iter().zip(pos) {
            self.state
                .node_positions
                .insert(*id, egui::pos2(p[0] * zoom, p[1] * zoom));
        }
        self.untangle_wires();
    }

    /// Orders every port's wires top to bottom by where their other end
    /// sits, so wires leave and enter without crossing. Only Plan and
    /// Arrange do this; connecting by hand never reorders existing wires.
    fn untangle_wires(&mut self) {
        let g = &self.state.graph;
        let y = |node: NodeId| {
            self.state
                .node_positions
                .get(node)
                .map(|p| p.y)
                .unwrap_or(0.0)
        };
        let in_key = |input: egui_node_graph2::InputId| {
            let node = g[input].node;
            (
                y(node),
                g[node]
                    .inputs
                    .iter()
                    .position(|(_, i)| *i == input)
                    .unwrap_or(0) as f32,
            )
        };
        let out_key = |output: egui_node_graph2::OutputId| {
            let node = g[output].node;
            (
                y(node),
                g[node]
                    .outputs
                    .iter()
                    .position(|(_, o)| *o == output)
                    .unwrap_or(0) as f32,
            )
        };
        let cmp = |a: (f32, f32), b: (f32, f32)| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1));
        let mut input_orders = Vec::new();
        for (input, outs) in g.iter_connection_groups() {
            let mut outs = outs;
            outs.sort_by(|a, b| cmp(out_key(*a), out_key(*b)));
            input_orders.push((input, outs));
        }
        let mut output_orders = Vec::new();
        for output in g.outputs.keys() {
            let mut ins = g.output_connections(output);
            ins.sort_by(|a, b| cmp(in_key(*a), in_key(*b)));
            output_orders.push((output, ins));
        }
        for (input, outs) in input_orders {
            self.state.graph.connections.insert(input, outs);
        }
        for (output, ins) in output_orders {
            self.state.graph.set_output_order(output, ins);
        }
    }

    pub(crate) fn request_arrange(&mut self) {
        self.relayout = true;
    }

    pub(crate) fn request_frame_all(&mut self) {
        self.frame_all = true;
    }

    pub(crate) fn has_selection(&self) -> bool {
        !self.state.selected_nodes.is_empty()
    }

    pub(crate) fn select_all(&mut self) {
        self.state.selected_nodes = self.state.graph.iter_nodes().collect();
    }

    pub(crate) fn delete_selected(&mut self) {
        if self.on_master() {
            let g = &self.state.graph;
            // Poles go now (Undo brings them back); factories ask first.
            let poles: Vec<u32> = self.state.selected_nodes.iter().filter_map(|n| crate::power_net::pole_of(g, *n)).collect();
            let ids: Vec<u32> = self.state.selected_nodes.iter().filter_map(|n| crate::project::factory_of(g, *n)).collect();
            if !poles.is_empty() {
                crate::power_net::remove_poles(&mut self.state, &mut self.planner.power_net, &poles);
                self.changed();
            }
            if !ids.is_empty() {
                self.dialog = Dialog::DeleteFactories { ids };
            }
            return;
        }
        for id in std::mem::take(&mut self.state.selected_nodes) {
            if self.state.graph.nodes.contains_key(id) {
                self.state.graph.remove_node(id);
                self.state.node_positions.remove(id);
                self.state.node_order.retain(|n| *n != id);
            }
        }
        self.changed();
    }

    /// Copies the selected nodes (with their numbers and the wires between
    /// them) 40 px down-right, and selects the copies.
    pub(crate) fn duplicate_selected(&mut self) {
        if self.on_master() {
            let g = &self.state.graph;
            let ids: Vec<u32> = self.state.selected_nodes.iter().filter_map(|n| crate::project::factory_of(g, *n)).collect();
            let copies: Vec<u32> = ids.into_iter().filter_map(|id| self.duplicate_factory(id)).collect();
            self.state.selected_nodes = copies.iter().filter_map(|id| crate::project::block_of(&self.state.graph, *id)).collect();
            return;
        }
        let selected: Vec<NodeId> = self
            .state
            .selected_nodes
            .iter()
            .copied()
            .filter(|id| self.state.graph.nodes.contains_key(*id))
            .collect();
        if selected.is_empty() {
            return;
        }
        let mut map = BTreeMap::new();
        for &old in &selected {
            let template = self.state.graph[old].user_data.kind.template();
            let pos = self.state.node_positions[old] + vec2(40.0, 40.0);
            let new = self.add_node(template, pos);
            self.state.graph[new].user_data.color = self.state.graph[old].user_data.color;
            let numbers: Vec<(String, f64)> = self.state.graph[old]
                .inputs
                .iter()
                .filter(|(_, i)| matches!(self.state.graph[*i].value, PortValue::Number { .. }))
                .map(|(n, i)| (n.clone(), self.state.graph[*i].value.number()))
                .collect();
            for (n, v) in numbers {
                self.set_number(new, &n, v);
            }
            map.insert(old, new);
        }
        let g = &self.state.graph;
        let links: Vec<(NodeId, usize, NodeId, usize)> = g
            .iter_connections()
            .filter_map(|(i, o)| {
                let (from, to) = (g[o].node, g[i].node);
                if !(map.contains_key(&from) && map.contains_key(&to)) {
                    return None;
                }
                let out_idx = g[from].outputs.iter().position(|(_, x)| *x == o)?;
                let in_idx = g[to].inputs.iter().position(|(_, x)| *x == i)?;
                Some((from, out_idx, to, in_idx))
            })
            .collect();
        for (from, oi, to, ii) in links {
            let out = self.state.graph[map[&from]].outputs[oi].1;
            let inp = self.state.graph[map[&to]].inputs[ii].1;
            let pos = self.state.graph.connections(inp).len();
            self.state.graph.add_connection(out, inp, pos);
        }
        self.state.selected_nodes = map.values().copied().collect();
        self.changed();
    }

    pub(crate) fn evaluate_now(&mut self) {
        self.evaluate();
    }

    /// Runs the flow simulation over the current graph and fills the stats
    /// the nodes and the overview show.
    fn evaluate(&mut self) {
        let game = self.user.game.clone();
        let e = evaluate_graph(&game, &self.state.graph, &mut self.sim_cache);
        self.wire_flow = e.wire_flow;
        self.user.ports = e.ports;
        self.user.nodes = e.nodes;
        self.summary = e.summary;
    }

    // ------------------------------------------------------------------ UI

    pub fn ui(&mut self, ctx: &egui::Context) {
        self.handle_messages(ctx);
        self.poll_room(ctx);
        self.follow_doc();
        if let Some(id) = self.switch_to.take() {
            self.activate(id);
        }
        if self.game_loaded {
            if self.on_master() {
                self.sync_master();
            }
            self.evaluate();
        }
        // Image export: only the graph, tile by tile, until it's done.
        if self.export_frame(ctx) {
            egui::CentralPanel::default()
                .frame(egui::Frame::none().fill(BACKDROP))
                .show(ctx, |ui| self.editor_ui(ui));
            return;
        }
        self.shortcuts(ctx);
        crate::menu::menu_bar(self, ctx);
        self.icons_notice(ctx);
        self.status_bar(ctx);
        egui::SidePanel::left("planner")
            .resizable(true)
            .default_width(340.0)
            .min_width(300.0)
            .frame(egui::Frame::none().fill(CONTENT))
            .show(ctx, |ui| {
                if self.on_master() {
                    crate::project_ui::factories_panel(self, ui)
                } else {
                    crate::panels::planner(self, ui)
                }
            });
        egui::SidePanel::right("overview")
            .resizable(true)
            .default_width(300.0)
            .min_width(260.0)
            .frame(egui::Frame::none().fill(CONTENT))
            .show(ctx, |ui| {
                if self.on_master() {
                    crate::project_ui::project_overview(self, ui)
                } else {
                    crate::panels::overview(self, ui)
                }
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(BACKDROP))
            .show(ctx, |ui| {
                // Project, factory and Map tabs, then the chosen view below them.
                let full = ui.max_rect();
                let (bar, _) = ui.allocate_exact_size(vec2(full.width(), 34.0), egui::Sense::hover());
                crate::project_ui::tab_bar(self, ui, bar);
                let content = egui::Rect::from_min_max(egui::pos2(full.min.x, bar.max.y), full.max);
                let mut view = ui.new_child(egui::UiBuilder::new().max_rect(content));
                view.set_clip_rect(content);
                match self.view {
                    crate::map::View::Graph => self.editor_ui(&mut view),
                    crate::map::View::Map => crate::map::map_ui(self, &mut view),
                }
            });
        self.node_menu(ctx);
        self.node_title_editor(ctx);
        self.frame_menu(ctx);
        crate::project_ui::master_canvas_menu(self, ctx);
        crate::project_ui::factory_menu(self, ctx);
        self.add_node_menu(ctx);
        self.chat_box(ctx);
        crate::dialogs::show(self, ctx);
        crate::setup::show(self, ctx);
        self.commit(ctx);
        self.autosave(ctx);
        self.paint_room_cursors(ctx);
        if self.selftest {
            self.publish_selftest();
            self.apply_selftest_view(ctx);
            // Keep frames coming so the probe always reads a freshly painted canvas.
            ctx.request_repaint();
        }
    }

    /// Self-test only: the probe sets `window.__fbSelftestView` to put the
    /// UI into a state worth a screenshot.
    fn apply_selftest_view(&mut self, _ctx: &egui::Context) {
        #[cfg(target_arch = "wasm32")]
        {
            let view = web_sys::window()
                .and_then(|w| js_sys::Reflect::get(&w, &"__fbSelftestView".into()).ok())
                .and_then(|v| v.as_string())
                .unwrap_or_default();
            if view == self.selftest_view {
                return;
            }
            self.selftest_view = view.clone();
            self.finder = None;
            self.context_menu = None;
            self.master_menu = None;
            self.factory_menu = None;
            self.wizard = None;
            let first_recipe = self
                .state
                .graph
                .iter_nodes()
                .find(|id| matches!(&self.state.graph[*id].user_data.kind, NodeKind::Recipe { recipe } if self.user.game.recipes.get(recipe).is_some_and(|r| r.ingredients.len() >= 2)));
            let center = self.editor_rect.center();
            match view.as_str() {
                "zoomed" => {
                    let recipes: Vec<NodeId> = self
                        .state
                        .graph
                        .iter_nodes()
                        .filter(|id| {
                            matches!(
                                self.state.graph[*id].user_data.kind,
                                NodeKind::Recipe { .. }
                            )
                        })
                        .collect();
                    for (n, id) in recipes.into_iter().enumerate() {
                        self.state.graph[id].user_data.color =
                            Some(GAME_COLORS[n % GAME_COLORS.len()].1);
                    }
                    if let Some(id) = first_recipe {
                        let k = 1.0 / self.state.pan_zoom.zoom;
                        for (_, p) in self.state.node_positions.iter_mut() {
                            *p = (p.to_vec2() * k).to_pos2();
                        }
                        self.state.pan_zoom.zoom(
                            egui::Rect::EVERYTHING,
                            &std::sync::Arc::new(_ctx.style().as_ref().clone()),
                            k,
                        );
                        self.focus_node = Some(id);
                    }
                }
                "finder" => {
                    self.finder = Some(crate::finder::FinderState::new(center - vec2(220.0, 260.0)))
                }
                "finder_search" => {
                    let mut f = crate::finder::FinderState::new(center - vec2(220.0, 260.0));
                    f.query = "iron plate".into();
                    self.finder = Some(f);
                }
                // A wire from the first recipe's first output let go on the canvas.
                "finder_wire" => {
                    let out = first_recipe.and_then(|n| self.state.graph.nodes.get(n)).and_then(|n| n.outputs.first().map(|o| o.1));
                    if let (Some(n), Some(o)) = (first_recipe, out) {
                        self.open_finder_for_wire(n, egui_node_graph2::AnyParameterId::Output(o), center - vec2(220.0, 260.0));
                    }
                }
                "menu" => {
                    if let Some(id) = first_recipe {
                        let r = self
                            .node_rects
                            .get(&id)
                            .copied()
                            .unwrap_or(egui::Rect::from_center_size(center, vec2(10.0, 10.0)));
                        self.state.selected_nodes = vec![id];
                        self.context_menu = Some((id, r.center()));
                        self.context_targets = vec![id];
                    }
                }
                // Setup screens with made-up progress (nothing is installed or extracted).
                "setup_install" | "setup_game" | "setup_icons" | "setup_name" => {
                    use crate::setup::{Step, Wizard};
                    let status = self.status.clone().unwrap_or_default();
                    let mut w = Wizard::first_launch(&status);
                    w.steps = vec![Step::Install, Step::Game, Step::Icons, Step::Name];
                    w.at = match view.as_str() {
                        "setup_install" => 0,
                        "setup_game" => 1,
                        "setup_icons" => 2,
                        _ => 3,
                    };
                    w.game_dir = status.settings.game_dir.clone();
                    w.found = vec![w.game_dir.clone(), r"D:\SteamLibrary\steamapps\common\Satisfactory".into()];
                    let g = &self.user.game;
                    w.check = Some((w.game_dir.clone(), Ok(fb_model::setup::GameCheck { dir: w.game_dir.clone(), recipes: g.recipes.len(), items: g.items.len(), buildings: g.buildings.len() })));
                    w.extract = Some(fb_model::setup::ExtractProgress { running: true, stage: "icons".into(), done: 400, total: 764, ..Default::default() });
                    self.dialog = Dialog::None;
                    self.wizard = Some(w);
                }
                // The Map tab with the self-test's sample save.
                "map" | "map_zoom" | "map_close" => {
                    self.dialog = Dialog::None;
                    self.view = crate::map::View::Map;
                    if self.map.world.is_none() {
                        self.backend.call("selftest_save", json!({}), Msg::SelftestSave);
                    }
                    self.map.selftest_zoom = match view.as_str() {
                        "map_zoom" => Some(0.012),
                        "map_close" => Some(0.08),
                        _ => None,
                    };
                    self.map.selftest_zoomed = false;
                }
                // Import the zoomed-in selection and show the result.
                "map_import" => {
                    self.import_selection(_ctx);
                }
                // Make project from save: the dialog, then the project.
                "world_dialog" => {
                    self.view = crate::map::View::Map;
                    self.dialog = Dialog::WorldProject(crate::dialogs::WorldProject { gap_m: 50.0, preview: None });
                }
                "world_project" => {
                    self.dialog = Dialog::None;
                    if let Some(w) = self.map.world.clone() {
                        let (p, _) = crate::map_import::world_project(&w, &self.user.game, self.map.info.as_ref(), 5000.0);
                        self.dirty = false;
                        self.run_pending(Pending::OpenProject(Box::new(p)));
                    }
                }
                // The first factory with two comment frames, Find open and the minimap.
                "frames" => {
                    self.dialog = Dialog::None;
                    self.room.reset();
                    if let Some(first) = self.factory_ids().first().copied() {
                        self.activate(first);
                    }
                    self.planner.frames.clear();
                    let game = self.user.game.clone();
                    let by_building = |app: &Self, b: &[&str]| -> Vec<NodeId> {
                        let g = &app.state.graph;
                        g.iter_nodes().filter(|n| matches!(&g[*n].user_data.kind, NodeKind::Recipe { recipe } if game.recipes.get(recipe).is_some_and(|r| b.contains(&r.building.as_str())))).collect()
                    };
                    for (title, buildings, color) in [("Plastic and rubber", &["Build_OilRefinery_C"][..], GAME_COLORS[1].1), ("Ingots", &["Build_SmelterMk1_C", "Build_FoundryMk1_C"][..], GAME_COLORS[0].1)] {
                        self.state.selected_nodes = by_building(self, buildings);
                        self.add_frame_around_selection();
                        if let Some(f) = self.planner.frames.last_mut() {
                            f.title = title.into();
                            f.color = Some(color);
                        }
                    }
                    self.frames.editing = None;
                    self.state.selected_nodes.clear();
                    self.find = Some(crate::navigate::FindState::new("rubber"));
                    self.frame_all = true;
                }
                // A factory tab's right-click menu.
                "tab_menu" => {
                    self.dialog = Dialog::None;
                    self.master_menu = None;
                    if let Some(id) = self.tabs.first().copied() {
                        self.factory_menu = Some((id, self.editor_rect.left_top() + vec2(110.0, -12.0)));
                    }
                }
                // The master graph's right-click menu on empty canvas.
                "project_menu" => {
                    self.dialog = Dialog::None;
                    self.activate(crate::project::MASTER);
                    let at = self.editor_rect.left_top() + vec2(120.0, 120.0);
                    let z = self.state.pan_zoom.zoom.max(0.01);
                    self.master_menu = Some((at, ((at - self.editor_rect.min - self.state.pan_zoom.pan) / z).to_pos2()));
                }
                // The master graph with two linked factories (one short of
                // ingots), and one of them open in its tab.
                "project" | "project_tab" => {
                    self.dialog = Dialog::None;
                    self.room.reset();
                    self.selftest_project(_ctx);
                    let target = if view == "project" { crate::project::MASTER } else { self.docs.iter().find(|d| d.name == "Iron Rods").map(|d| d.id).unwrap_or(crate::project::MASTER) };
                    self.activate(target);
                    self.frame_all = true;
                }
                "settings_dev" => {
                    let s = self.status.as_ref().map(|s| s.settings.clone()).unwrap_or_default();
                    self.dialog = Dialog::settings(&s);
                }
                "export" => {
                    self.view = crate::map::View::Graph;
                    self.room.reset();
                    self.dialog = Dialog::None;
                    self.start_image_export_for_selftest(_ctx);
                }
                // UI-only room state (no sockets, so no firewall prompt).
                "room_info" | "room_cursors" => {
                    use fb_model::room::{encode_code, HostInfo, Peer, RoomAddress};
                    let addr = RoomAddress { local: "192.168.1.23".parse().unwrap(), public: Some("203.0.113.7".parse().unwrap()), port: 47860 };
                    self.room.mode = crate::room::Mode::Hosting(HostInfo {
                        code: encode_code(addr),
                        local_ip: "192.168.1.23".into(),
                        port: 47860,
                        public_ip: Some("203.0.113.7".into()),
                        public_source: Some("router".into()),
                        forwarded: false,
                        forward_problem: Some("no router answered over UPnP (it may be turned off in the router's settings)".into()),
                        shared_address: false,
                    });
                    self.room.me = 0;
                    self.room.peers.clear();
                    for (id, name, color) in [(0, "Hoster", [0xFA, 0x95, 0x49]), (1, "Ada", [0x86, 0xC7, 0xF2]), (2, "Bo", [0xA6, 0xE0, 0x86])] {
                        self.room.peers.insert(id, Peer { id, name: name.into(), color });
                    }
                    let z = self.state.pan_zoom.zoom.max(0.01);
                    let g = |p: egui::Pos2| ((p - self.editor_rect.min - self.state.pan_zoom.pan) / z).to_pos2();
                    self.room.cursors.insert(1, g(center + vec2(-160.0, -60.0)));
                    self.room.cursors.insert(2, g(center + vec2(120.0, 90.0)));
                    self.room.cursor_doc.insert(1, self.active);
                    self.room.cursor_doc.insert(2, self.active);
                    // Room extras, held still for the screenshot: Bo's ping,
                    // Ada's message by her cursor (and Bo's earlier one in the
                    // list), a node both have selected, and following Ada.
                    let ping_at = g(center + vec2(220.0, -150.0));
                    let view_centre = g(center);
                    let now = _ctx.input(|i| i.time);
                    let room = &mut self.room;
                    room.pings.clear();
                    room.messages.clear();
                    room.bubbles.clear();
                    room.selections.clear();
                    room.views.clear();
                    room.following = None;
                    room.frozen = view == "room_cursors";
                    if view == "room_cursors" {
                        let doc = self.active;
                        self.room.pings.push(crate::room::PingMark { id: 2, pos: ping_at, doc, at: now });
                        self.add_message(2, "Smelters are done, moving on to the rods.".into(), now);
                        self.room.bubbles.remove(&2);
                        self.add_message(1, "Can we run the screws through here?".into(), now);
                        let picked = first_recipe.or_else(|| self.state.node_order.first().copied());
                        if let Some(n) = picked {
                            self.room.selections.insert(1, (doc, vec![n]));
                            self.room.selections.insert(2, (doc, vec![n]));
                        }
                        self.room.views.insert(1, crate::room::PeerView { doc, center: view_centre, zoom: z });
                        self.room.following = Some(1);
                    }
                    if view == "room_info" {
                        self.dialog = Dialog::RoomInfo;
                    } else {
                        self.dialog = Dialog::None;
                    }
                }
                // README: the project from the save, from its top left
                // corner (the biggest linked groups).
                "readme_world" => {
                    self.frame_all = false;
                    self.state.pan_zoom.pan = vec2(24.0, 24.0);
                }
                // The project from the save with its biggest power plant
                // selected: what its grid powers lights up.
                "power_lit" => {
                    self.frame_all = true;
                    let g = &self.state.graph;
                    let plant = g
                        .iter_nodes()
                        .filter_map(|n| match g[n].user_data.kind {
                            NodeKind::Factory { uses_mw, makes_mw, .. } => Some((n, makes_mw - uses_mw)),
                            _ => None,
                        })
                        .max_by(|a, b| a.1.total_cmp(&b.1));
                    self.state.selected_nodes = plant.map(|p| vec![p.0]).unwrap_or_default();
                }
                // Some factories and their poles up close.
                "power_close" => {
                    self.state.selected_nodes.clear();
                    let pole = self.planner.power_net.poles.get(self.planner.power_net.poles.len() / 3).and_then(|p| crate::power_net::pole_node(&self.state.graph, p.id));
                    if let Some(id) = pole {
                        let k = 1.0 / self.state.pan_zoom.zoom;
                        for (_, p) in self.state.node_positions.iter_mut() {
                            *p = (p.to_vec2() * k).to_pos2();
                        }
                        self.state.pan_zoom.zoom(egui::Rect::EVERYTHING, &std::sync::Arc::new(_ctx.style().as_ref().clone()), k);
                        self.focus_node = Some(id);
                    }
                }
                // A small factory through a splitter and a merger.
                "routers" => {
                    self.dialog = Dialog::None;
                    self.dirty = false;
                    self.run_pending(Pending::New);
                    self.view = crate::map::View::Graph;
                    let (ore, ingot) = ("Desc_OreIron_C", "Desc_IronIngot_C");
                    let z = self.state.pan_zoom.zoom;
                    let at = |x: f32, y: f32| egui::pos2(x * z, y * z);
                    self.state = crate::project::new_state();
                    let src = self.add_node(Template::Resource(ore.into()), at(0.0, 120.0));
                    let split = self.add_node(Template::Splitter(ore.into()), at(380.0, 200.0));
                    let s1 = self.add_node(Template::Recipe("Recipe_IngotIron_C".into()), at(520.0, 0.0));
                    let s2 = self.add_node(Template::Recipe("Recipe_IngotIron_C".into()), at(520.0, 300.0));
                    let merge = self.add_node(Template::Merger(ingot.into()), at(960.0, 200.0));
                    let out = self.add_node(Template::Output(ingot.into()), at(1100.0, 120.0));
                    self.set_number(src, RATE, 60.0);
                    self.set_number(out, RATE, 60.0);
                    for (a, b, item) in [(src, split, ore), (split, s1, ore), (split, s2, ore), (s1, merge, ingot), (s2, merge, ingot), (merge, out, ingot)] {
                        let g = &self.state.graph;
                        let o = g[a].outputs.iter().map(|(_, o)| *o).find(|o| g[*o].typ.0 == item);
                        let i = g[b].inputs.iter().map(|(_, i)| *i).find(|i| g[*i].typ.0 == item);
                        if let (Some(o), Some(i)) = (o, i) {
                            let pos = self.state.graph.connections(i).len();
                            self.state.graph.add_connection(o, i, pos);
                        }
                    }
                    self.frame_all = true;
                }
                // README screenshots: a typical factory (Motors, standard
                // recipes) in a new project, as Plan factory lays it out.
                // The plan arrives a few frames later, so it's shot in the
                // next view.
                "readme_prep" => {
                    self.dialog = Dialog::None;
                    self.find = None;
                    self.room.mode = crate::room::Mode::Off;
                    self.room.peers.clear();
                    self.room.cursors.clear();
                    self.dirty = false;
                    self.run_pending(Pending::New);
                    self.view = crate::map::View::Graph;
                    self.minimap = true;
                    self.planner.targets = vec![Target { item: "Desc_Motor_C".into(), rate: 5.0 }];
                    self.planner.enabled = self.user.game.recipes.values().filter(|r| !r.alternate).map(|r| r.class.clone()).collect();
                    self.request_plan();
                }
                "readme_plan" => {}
                // The same plan in a room: two others' cursors and
                // selections, a ping and their messages.
                "readme_room" => {
                    use fb_model::room::{encode_code, HostInfo, Peer, RoomAddress};
                    let addr = RoomAddress { local: "192.168.1.23".parse().unwrap(), public: Some("203.0.113.7".parse().unwrap()), port: 47860 };
                    self.room.mode = crate::room::Mode::Hosting(HostInfo {
                        code: encode_code(addr),
                        local_ip: "192.168.1.23".into(),
                        port: 47860,
                        public_ip: Some("203.0.113.7".into()),
                        public_source: Some("router".into()),
                        forwarded: true,
                        forward_problem: None,
                        shared_address: false,
                    });
                    self.room.me = 0;
                    self.room.peers.clear();
                    for (id, name, color) in [(0, "Hoster", [0xFA, 0x95, 0x49]), (1, "Ada", [0x86, 0xC7, 0xF2]), (2, "Bo", [0xA6, 0xE0, 0x86])] {
                        self.room.peers.insert(id, Peer { id, name: name.into(), color });
                    }
                    let z = self.state.pan_zoom.zoom.max(0.01);
                    let g = |p: egui::Pos2| ((p - self.editor_rect.min - self.state.pan_zoom.pan) / z).to_pos2();
                    // Ada on a node a quarter of the way across, Bo on one
                    // three quarters across, each with it selected.
                    let mut recipes: Vec<(NodeId, egui::Rect)> = self
                        .node_rects
                        .iter()
                        .filter(|(id, _)| matches!(self.state.graph[**id].user_data.kind, NodeKind::Recipe { .. }))
                        .map(|(id, r)| (*id, *r))
                        .collect();
                    recipes.sort_by(|a, b| a.1.min.x.total_cmp(&b.1.min.x).then(a.1.min.y.total_cmp(&b.1.min.y)));
                    // Bo's ping below the last machine, clear of Ada's message.
                    let picks = recipes.get(recipes.len() / 4).copied().zip(recipes.get(recipes.len() * 3 / 4).copied());
                    let last = recipes.last().map(|r| r.1);
                    let marks = picks.zip(last).map(|((a, b), last)| (a.0, g(a.1.center() + vec2(30.0, 8.0)), b.0, g(b.1.right_top() + vec2(-40.0, 26.0)), g(last.center_bottom() + vec2(0.0, 70.0))));
                    let doc = self.active;
                    let now = _ctx.input(|i| i.time);
                    let room = &mut self.room;
                    room.cursors.clear();
                    room.pings.clear();
                    room.messages.clear();
                    room.bubbles.clear();
                    room.selections.clear();
                    room.views.clear();
                    room.following = None;
                    room.frozen = true;
                    if let Some((a, at_a, b, at_b, ping)) = marks {
                        room.cursors.insert(1, at_a);
                        room.cursors.insert(2, at_b);
                        room.cursor_doc.insert(1, doc);
                        room.cursor_doc.insert(2, doc);
                        room.selections.insert(1, (doc, vec![a]));
                        room.selections.insert(2, (doc, vec![b]));
                        room.pings.push(crate::room::PingMark { id: 2, pos: ping, doc, at: now });
                    }
                    self.add_message(2, "Rotors are done, I'll do the stators next.".into(), now);
                    self.room.bubbles.remove(&2);
                    self.add_message(1, "Should the screws come from the next factory over?".into(), now);
                    self.dialog = Dialog::None;
                }
                // One recipe node up close (the Motor assembler), out of the room.
                "readme_nodes" => {
                    self.room.mode = crate::room::Mode::Off;
                    self.room.peers.clear();
                    self.room.cursors.clear();
                    self.room.selections.clear();
                    self.room.pings.clear();
                    self.room.bubbles.clear();
                    self.room.frozen = false;
                    let motor = self.state.graph.iter_nodes().find(|id| matches!(&self.state.graph[*id].user_data.kind, NodeKind::Recipe { recipe } if self.user.game.recipes.get(recipe).is_some_and(|r| r.products.iter().any(|p| p.item == "Desc_Motor_C"))));
                    if let Some(id) = motor {
                        let k = 1.0 / self.state.pan_zoom.zoom;
                        for (_, p) in self.state.node_positions.iter_mut() {
                            *p = (p.to_vec2() * k).to_pos2();
                        }
                        self.state.pan_zoom.zoom(egui::Rect::EVERYTHING, &std::sync::Arc::new(_ctx.style().as_ref().clone()), k);
                        self.focus_node = Some(id);
                    }
                }
                // Add node for a wire let go from the Iron Rod output.
                "readme_wire" => {
                    let rods = self.state.graph.iter_nodes().find_map(|id| {
                        let n = &self.state.graph[id];
                        let is_recipe = matches!(n.user_data.kind, NodeKind::Recipe { .. });
                        n.outputs.iter().find(|(_, o)| is_recipe && self.state.graph[*o].typ.0 == "Desc_IronRod_C").map(|(_, o)| (id, *o))
                    });
                    if let Some((n, o)) = rods {
                        self.open_finder_for_wire(n, egui_node_graph2::AnyParameterId::Output(o), center - vec2(220.0, 260.0));
                    }
                }
                _ => {}
            }
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        if !matches!(self.dialog, Dialog::None) {
            if ctx.input(|i| i.key_pressed(Key::Escape)) {
                self.dialog = Dialog::None;
                self.after_save = None;
                self.block_spot = None;
            }
            return;
        }
        let cmd = |key| KeyboardShortcut::new(Modifiers::COMMAND, key);
        let cmd_shift = |key| KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, key);
        let hit = |s: KeyboardShortcut| ctx.input_mut(|i| i.consume_shortcut(&s));
        if hit(cmd_shift(Key::S)) {
            self.pick_save(self.save_kind());
        }
        if hit(cmd(Key::S)) {
            self.save();
        }
        if hit(cmd(Key::N)) {
            self.guard(Pending::New);
        }
        if hit(cmd(Key::O)) {
            self.guard(Pending::OpenDialog);
        }
        if self.view == crate::map::View::Graph && hit(cmd(Key::F)) {
            self.open_find();
        }
        if ctx.wants_keyboard_input() || self.view == crate::map::View::Map {
            return; // typing in a field, or on the map: no graph editing keys
        }
        self.room_keys(ctx);
        if hit(cmd_shift(Key::Z)) || hit(cmd(Key::Y)) {
            self.redo();
        }
        if hit(cmd(Key::Z)) {
            self.undo();
        }
        if hit(cmd(Key::D)) {
            self.duplicate_selected();
        }
        if hit(cmd(Key::A)) {
            self.select_all();
        }
        if ctx.input(|i| i.key_pressed(Key::Delete)) {
            if self.has_selection() {
                self.delete_selected();
            } else if let Some(f) = self.frames.selected {
                self.delete_frame(f);
            }
        }
        if ctx.input(|i| i.key_pressed(Key::F2)) {
            self.start_rename();
        }
        if ctx.input(|i| i.key_pressed(Key::C) && i.modifiers.is_none()) && self.has_selection() {
            self.add_frame_around_selection();
        }
        if ctx.input(|i| i.key_pressed(Key::Home)) {
            self.frame_all = true;
        }
    }

    /// A bar under the menu when the icons are missing or the game has
    /// updated since they were extracted.
    fn icons_notice(&mut self, ctx: &egui::Context) {
        let Some(s) = self.status.clone() else { return };
        if self.notice_dismissed || self.wizard.is_some() || !s.settings.setup_done || !s.setup.helper_found {
            return;
        }
        let text = if s.icons_found == 0 || s.ui_assets_found == 0 {
            "The game's icons and font haven't been extracted yet."
        } else if s.game_changed {
            "Satisfactory has updated. Update the icons, font and map to match?"
        } else if !s.map_found {
            "The world map hasn't been extracted yet."
        } else {
            return;
        };
        let mut open = false;
        egui::TopBottomPanel::top("icons_notice")
            .exact_height(40.0)
            .frame(egui::Frame::none().fill(BACKDROP).inner_margin(egui::Margin::symmetric(12.0, 0.0)))
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.label(RichText::new(text).color(TEXT));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if crate::widgets::button(ui, "Not now").clicked() {
                            self.notice_dismissed = true;
                        }
                        if crate::widgets::button(ui, "Extract now").clicked() {
                            open = true;
                        }
                    });
                });
            });
        if open {
            self.open_setup(crate::setup::Wizard::icons_only(&s));
        }
    }

    fn status_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status")
            .exact_height(26.0)
            .frame(
                egui::Frame::none()
                    .fill(HEADER)
                    .inner_margin(egui::Margin::symmetric(10.0, 0.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    let now = ctx.input(|i| i.time);
                    if let Some((text, err, t)) = &self.toast {
                        if now - t < 6.0 {
                            ui.label(RichText::new(text).small().color(if *err {
                                DANGER
                            } else {
                                TEXT_DIM
                            }));
                            ctx.request_repaint_after(std::time::Duration::from_secs(1));
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if self.room.active() {
                            let n = self.room.peers.len();
                            let names: Vec<&str> = self.room.peers.values().map(|p| p.name.as_str()).collect();
                            ui.label(RichText::new(format!("Room · {n} {}", if n == 1 { "person" } else { "people" })).small().color(ACCENT))
                                .on_hover_text(names.join(", "));
                        }
                        if let Some(s) = &self.status {
                            ui.label(
                                RichText::new(format!("{} icons", s.icons_found))
                                    .small()
                                    .color(TEXT_DIM),
                            );
                        }
                        if self.game_loaded {
                            let g = &self.user.game;
                            ui.label(
                                RichText::new(format!(
                                    "{} recipes · {} items",
                                    g.recipes.len(),
                                    g.items.len()
                                ))
                                .small()
                                .color(TEXT_DIM),
                            );
                        }
                    });
                });
            });
    }

    fn editor_ui(&mut self, ui: &mut egui::Ui) {
        let rect = ui.max_rect();
        if !self.game_loaded {
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                "Reading game data…",
                theme::semibold(15.0),
                TEXT_DIM,
            );
            return;
        }
        self.follow_view(ui, rect);
        if self.frame_all {
            self.frame_all = false;
            self.fit_view(ui, false);
        }
        if self.frame_selected {
            self.frame_selected = false;
            self.fit_view(ui, true);
        }
        if let Some(node) = self.focus_node.take() {
            if let Some(pos) = self.state.node_positions.get(node).copied() {
                let z = self.state.pan_zoom.zoom;
                self.state.pan_zoom.pan =
                    rect.size() / 2.0 - pos.to_vec2() - vec2(150.0, 120.0) * z;
                self.state.selected_nodes = vec![node];
            }
        }
        self.draw_grid(ui, rect);
        self.user.zoom = self.state.pan_zoom.zoom;
        if self.state.graph.nodes.is_empty() && !self.on_master() {
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                "Plan a factory from the panel on the left,\nor right-click here to place machines yourself.",
                egui::FontId::proportional(15.0),
                TEXT_DIM,
            );
        }
        let frame_slot = Self::frames_slot(ui);
        let selected_before = self.state.selected_nodes.clone();
        // Power wires about what's selected or under the pointer stand out.
        self.user.power_lit.clear();
        if self.on_master() && self.power_links {
            let mut focus = self.state.selected_nodes.clone();
            if let Some(p) = ui.input(|i| i.pointer.hover_pos()).filter(|p| rect.contains(*p)) {
                focus.extend(self.node_rects.iter().filter(|(_, r)| r.contains(p)).map(|(id, _)| *id).last());
            }
            self.user.power_lit = crate::power_net::lit_wires(&self.state.graph, &focus);
        }
        let resp = self
            .state
            .draw_graph_editor(ui, &self.templates, &mut self.user, Vec::new());
        if self.snap {
            let z = self.state.pan_zoom.zoom.max(0.01);
            for r in &resp.node_responses {
                if let NodeResponse::MoveNode { node, drag_delta } = r {
                    let sel = &self.state.selected_nodes;
                    let moved: Vec<NodeId> = if sel.contains(node) && sel.len() > 1 { sel.clone() } else { vec![*node] };
                    for n in moved {
                        let Some(cur) = self.state.node_positions.get(n).copied() else { continue };
                        // The editor already moved it; track where it would be unsnapped.
                        let raw = self.snap_raw.entry(n).or_insert(cur - *drag_delta);
                        *raw += *drag_delta;
                        let g = (raw.to_vec2() / z).to_pos2();
                        self.state.node_positions.insert(n, (egui::vec2(snap_to_grid(g.x), snap_to_grid(g.y)) * z).to_pos2());
                    }
                }
            }
        }
        if !ui.input(|i| i.pointer.any_down()) {
            self.snap_raw.clear();
        }
        // Wiring, moving or renaming grids on the master graph.
        if self.on_master() && self.power_links {
            crate::power_net::read_back(&self.state, &mut self.planner.power_net);
        }
        let frame_menu = self.frames_ui(ui, rect, frame_slot);
        if frame_menu {
            self.state.node_finder = None;
        }
        for r in &resp.node_responses {
            match r {
                NodeResponse::User(Resp::Edited)
                | NodeResponse::ConnectEventEnded { .. }
                | NodeResponse::CreatedNode(_)
                | NodeResponse::DeleteNodeFull { .. }
                | NodeResponse::DisconnectEvent { .. }
                | NodeResponse::DisconnectOutputEvent { .. }
                | NodeResponse::MoveNode { .. } => self.changed(),
                _ => {}
            }
        }
        // A wire let go on empty canvas: Add node, showing only what fits it.
        if let Some((node, port, at)) = resp.wire_dropped.filter(|_| !self.on_master()) {
            self.open_finder_for_wire(node, port, at);
            if let Some(w) = self.finder.as_mut().and_then(|f| f.wire.as_mut()) {
                w.extras = resp.wire_dropped_extras.clone();
            }
        }
        if let Some(menu) = resp.node_context_menu {
            self.context_menu = Some(menu);
            self.context_targets = self.state.selected_nodes.clone();
            if !self.context_targets.contains(&menu.0) {
                self.context_targets = vec![menu.0];
            }
            self.custom_color = None;
        }
        // Right-click on empty canvas: show our Add node menu instead of the
        // library's finder (the master graph's blocks are its factories).
        if let Some(f) = self.state.node_finder.take() {
            let at = f.position.unwrap_or(rect.center());
            if self.on_master() {
                let z = self.state.pan_zoom.zoom.max(0.01);
                let graph = ((at - rect.min - self.state.pan_zoom.pan) / z).to_pos2();
                self.master_menu = Some((at, graph));
            } else {
                self.finder = Some(crate::finder::FinderState::new(at));
            }
            self.context_menu = None;
        }
        // Double-click a factory block to open it.
        if self.on_master() && ui.input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary)) {
            let at = ui.input(|i| i.pointer.interact_pos());
            let hit = at.and_then(|p| resp.node_rects.iter().filter(|(_, r)| r.contains(p)).map(|(id, _)| *id).last());
            if let Some(id) = hit.and_then(|n| crate::project::factory_of(&self.state.graph, n)) {
                self.switch_to = Some(id);
            }
        }
        self.editor_rect = rect;
        if self.export.is_none() {
            self.minimap_ui(ui, rect);
            self.find_ui(ui, rect);
        }
        self.send_cursor(ui.ctx(), rect);
        if self.flow_arrows && self.export.is_none() && crate::wires::paint_flow(self, ui, &resp.wires, &resp.node_rects) {
            ui.ctx().request_repaint();
        }
        if self.wire_labels {
            crate::wires::paint_labels(self, ui, &resp.wires, &resp.node_rects);
        }
        #[cfg(test)]
        {
            self.port_hooks = resp.port_locations.clone();
        }
        let z = self.state.pan_zoom.zoom.max(0.01);
        self.node_rects.clear();
        for (id, r) in &resp.node_rects {
            self.node_heights.insert(*id, r.height() / z);
            self.node_rects.insert(*id, *r);
        }
        // Arrange after a draw, so every node has a measured height.
        let measured = self
            .state
            .graph
            .iter_nodes()
            .all(|id| resp.node_rects.contains_key(&id));
        if self.relayout && measured {
            self.relayout = false;
            self.arrange(&resp.port_locations);
            self.frame_all = true;
            self.changed();
            ui.ctx().request_repaint();
        }
        self.room_after_draw(ui, rect, selected_before);
    }

    /// Fit every node into view (View > Frame all, Home).
    /// Zooms and pans so every node (or, with `selected`, the selected
    /// nodes) fits the view.
    fn fit_view(&mut self, ui: &egui::Ui, selected: bool) {
        let rect = ui.max_rect();
        let z = self.state.pan_zoom.zoom;
        let w = theme::graph_style().node_width;
        let size = |id: NodeId| vec2(w, self.node_heights.get(&id).copied().unwrap_or(260.0));
        let wanted = |id: NodeId| !selected || self.state.selected_nodes.contains(&id);
        let mut bounds = egui::Rect::NOTHING;
        for (id, p) in self.state.node_positions.iter().filter(|(id, _)| wanted(*id)) {
            bounds = bounds.union(egui::Rect::from_min_size(*p, size(id) * z));
        }
        if !bounds.is_positive() {
            self.state.pan_zoom.pan = egui::Vec2::ZERO;
            return;
        }
        let fit =
            ((rect.width() - 80.0) / bounds.width()).min((rect.height() - 80.0) / bounds.height());
        let target = (z * fit).clamp(0.35, 1.0);
        // Scale positions about the origin, then centre the result.
        let k = target / z;
        for (_, p) in self.state.node_positions.iter_mut() {
            *p = (p.to_vec2() * k).to_pos2();
        }
        self.state.pan_zoom.zoom(ui.clip_rect(), ui.style(), k);
        let mut b = egui::Rect::NOTHING;
        for (id, p) in self.state.node_positions.iter().filter(|(id, _)| wanted(*id)) {
            b = b.union(egui::Rect::from_min_size(*p, size(id) * target));
        }
        self.state.pan_zoom.pan = rect.size() / 2.0 - b.center().to_vec2();
    }

    fn draw_grid(&self, ui: &egui::Ui, rect: egui::Rect) {
        let step = GRID * self.state.pan_zoom.zoom;
        if step < 6.0 {
            return;
        }
        let off = self.state.pan_zoom.pan;
        let painter = ui.painter_at(rect);
        let color = egui::Color32::from_rgb(0x3B, 0x3B, 0x3B);
        let mut x = rect.left() + off.x.rem_euclid(step);
        while x < rect.right() {
            painter.vline(x, rect.y_range(), egui::Stroke::new(1.0_f32, color));
            x += step;
        }
        let mut y = rect.top() + off.y.rem_euclid(step);
        while y < rect.bottom() {
            painter.hline(rect.x_range(), y, egui::Stroke::new(1.0_f32, color));
            y += step;
        }
    }

    /// Right-click menu on a node.
    fn node_menu(&mut self, ctx: &egui::Context) {
        let Some((node, pos)) = self.context_menu else {
            return;
        };
        if !self.state.graph.nodes.contains_key(node) {
            self.context_menu = None;
            return;
        }
        // Keep acting on (and showing) the nodes the menu was opened for.
        self.context_targets
            .retain(|id| self.state.graph.nodes.contains_key(*id));
        self.state.selected_nodes = self.context_targets.clone();
        // Run after the menu is drawn: the menu reads the node it's for, and
        // deleting it mid-draw would index a node that no longer exists.
        let (mut do_duplicate, mut do_delete, mut do_move, mut do_frame) = (false, false, false, false);
        let (mut do_open, mut do_rename) = (false, false);
        let mut set_tier = None;
        let mut set_extractor: Option<(String, fb_model::Purity)> = None;
        let g = &self.state.graph;
        let title = if self.context_targets.len() > 1 { format!("{} nodes", self.context_targets.len()) } else { g[node].label.clone() };
        let factory = crate::project::factory_of(g, node);
        // Poles, mergers and splitters show no name or colour; a pole has
        // nothing to copy either.
        let only_poles = self.context_targets.iter().all(|n| crate::power_net::pole_of(g, *n).is_some());
        let only_knots = self.context_targets.iter().all(|n| g[*n].user_data.knot_size().is_some());
        let current = g[node].user_data.color;
        let on_master = self.on_master();
        let mut custom = self.custom_color;
        let mut swatches = Vec::new();
        let mut assets = self.user.assets.take();
        let mut pick = None;
        let menu = crate::menus::popup(ctx, "node_menu", pos, &title, |ui| {
            use crate::menus::{item, separator};
            if factory.is_some() {
                do_open = item(ui, "Open", "Double-click", true).clicked();
            }
            if !only_knots {
                do_rename = item(ui, "Rename", "F2", true).clicked();
                separator(ui);
            }
            if !only_poles {
                do_duplicate = item(ui, "Duplicate", "Ctrl+D", true).clicked();
            }
            // An extractor's building and node purity.
            if let [one] = &self.context_targets[..] {
                if let NodeKind::Extractor { item, class, purity } = &g[*one].user_data.kind {
                    let game = self.user.game.clone();
                    let form = game.item_form(item);
                    crate::menus::caption(ui, "Extractor");
                    for x in game.extraction.extractors.iter().filter(|x| x.can_extract(item, form)) {
                        let each = crate::fmt::fmt_rate_unit(x.per_min(*purity, 1.0), form);
                        if crate::menus::choice(ui, &x.name, &each, x.class == *class, true).clicked() {
                            set_extractor = Some((x.class.clone(), *purity));
                        }
                    }
                    if crate::extractors::has_purity(&game, class) {
                        crate::menus::caption(ui, "Node purity");
                        for p in fb_model::Purity::ALL {
                            if crate::menus::choice(ui, p.label(), "", p == *purity, true).clicked() {
                                set_extractor = Some((class.clone(), p));
                            }
                        }
                    }
                    separator(ui);
                }
            }
            // A pole's tier: how many wires it takes, as in game.
            if let (true, [one]) = (only_poles, &self.context_targets[..]) {
                let pole = crate::power_net::pole_of(g, *one);
                let tier = pole.and_then(|p| self.planner.power_net.poles.iter().find(|x| x.id == p)).map(|p| p.tier).unwrap_or(1);
                let wires = crate::power_net::wire_count(g, *one);
                crate::menus::caption(ui, "Power Pole");
                for t in 1..=3u8 {
                    let max = crate::power_net::max_wires(t);
                    let r = crate::menus::choice(ui, &format!("Mk.{t}"), &format!("up to {max} wires"), t == tier, wires <= max)
                        .on_disabled_hover_text(format!("This pole has {wires} wires."));
                    if r.clicked() {
                        set_tier = pole.map(|p| (p, t));
                    }
                }
                separator(ui);
            }
            do_delete = item(ui, "Delete", "Del", true).clicked();
            do_frame = item(ui, "Add comment frame", "C", true).clicked();
            if !on_master {
                separator(ui);
                do_move = item(ui, "Move to a new factory", "", true)
                    .on_hover_text("Moves these nodes into a factory of their own, linked to this one on the master graph.")
                    .clicked();
            }
            if !only_knots {
                separator(ui);
                pick = crate::menus::colours(ui, assets.as_mut(), current, &mut custom, &mut swatches);
            }
        });
        self.user.assets = assets;
        self.custom_color = custom;
        #[cfg(test)]
        {
            self.menu_swatches = swatches;
        }
        #[cfg(not(test))]
        let _ = swatches;
        if let Some(color) = pick {
            for id in self.context_targets.clone() {
                if self.state.graph.nodes.contains_key(id) {
                    self.state.graph[id].user_data.color = color;
                }
            }
            self.changed();
        }
        if let (Some((c, p)), [one]) = (set_extractor.clone(), &self.context_targets[..]) {
            if let NodeKind::Extractor { class, purity, .. } = &mut self.state.graph[*one].user_data.kind {
                (*class, *purity) = (c, p);
                self.changed();
            }
        }
        if let Some((pole, t)) = set_tier {
            if let Some(p) = self.planner.power_net.poles.iter_mut().find(|p| p.id == pole) {
                p.tier = t;
                self.changed();
            }
        }
        let chose = do_duplicate || do_delete || do_move || do_frame || do_open || do_rename || set_tier.is_some() || set_extractor.is_some();
        if chose || crate::menus::dismissed(ctx, &menu) {
            self.context_menu = None;
        }
        if let Some(f) = factory {
            if do_open {
                self.switch_to = Some(f);
            }
        }
        if do_rename {
            self.node_rename = Some((node, self.state.graph[node].label.clone()));
        }
        if do_duplicate {
            self.duplicate_selected();
        }
        if do_move {
            self.move_selection_to_new_factory(ctx);
        }
        if do_frame {
            self.add_frame_around_selection();
        }
        if do_delete {
            self.delete_selected();
        }
    }

    /// Remembers the View menu's toggles.
    pub(crate) fn save_view_settings(&mut self) {
        if let Some(s) = self.status.as_mut() {
            s.settings.view = fb_model::ViewSettings {
                snap_to_grid: self.snap,
                wire_labels: self.wire_labels,
                minimap: self.minimap,
                power_links: self.power_links,
                flow_arrows: self.flow_arrows,
                map_power: !self.map.hide_power,
                map_belt_flow: !self.map.hide_flow,
            };
            let settings = s.settings.clone();
            self.backend.call("set_settings", json!({ "settings": settings }), Msg::Status);
        }
    }

    /// F2: rename the selected node, or the selected comment frame.
    pub(crate) fn start_rename(&mut self) {
        if let [n] = self.state.selected_nodes[..] {
            if self.state.graph.nodes.contains_key(n) && self.state.graph[n].user_data.knot_size().is_none() {
                self.node_rename = Some((n, self.state.graph[n].label.clone()));
            }
        } else if self.state.selected_nodes.is_empty() {
            if let Some(f) = self.frames.selected.and_then(|id| self.planner.frames.iter().find(|f| f.id == id)) {
                self.frames.editing = Some((f.id, f.title.clone()));
            }
        }
    }

    /// Renaming a node: a text field over its title. Enter keeps it, Esc
    /// cancels; an empty name goes back to the recipe's or item's. On the
    /// master graph it renames the factory.
    fn node_title_editor(&mut self, ctx: &egui::Context) {
        let Some((node, mut text)) = self.node_rename.clone() else { return };
        let (Some(r), true) = (self.node_rects.get(&node).copied(), self.state.graph.nodes.contains_key(node)) else {
            self.node_rename = None;
            return;
        };
        let z = self.state.pan_zoom.zoom.max(0.4);
        let mut done = None;
        egui::Area::new(egui::Id::new(("node_title", node))).order(egui::Order::Foreground).fixed_pos(r.min + vec2(6.0, 6.0) * z).show(ctx, |ui| {
            let edit = ui.add(egui::TextEdit::singleline(&mut text).desired_width((r.width() - 12.0 * z).max(120.0)).hint_text("Name"));
            if !edit.has_focus() && !edit.lost_focus() {
                edit.request_focus();
            }
            if edit.lost_focus() {
                done = Some(!ui.input(|i| i.key_pressed(Key::Escape)));
            }
        });
        match done {
            None => self.node_rename = Some((node, text)),
            Some(keep) => {
                self.node_rename = None;
                let name: String = text.trim().chars().take(200).collect();
                if !keep || name == self.state.graph[node].label {
                    return;
                }
                if let Some(f) = crate::project::factory_of(&self.state.graph, node) {
                    self.rename_factory(f, &name);
                    return;
                }
                let game = self.user.game.clone();
                let label = if name.is_empty() { crate::app_files::default_label(&game, &self.state.graph[node].user_data.kind) } else { name };
                self.state.graph[node].label = label;
                self.changed();
            }
        }
    }

    /// Add node for a wire let go on empty canvas from `port` of `node`.
    pub(crate) fn open_finder_for_wire(&mut self, node: NodeId, port: egui_node_graph2::AnyParameterId, at: egui::Pos2) {
        let Some(item) = self.state.graph.any_param_type(port).ok().map(|t| t.0.clone()) else { return };
        if item.is_empty() {
            return;
        }
        let item_name = self.user.game.item_name(&item).to_string();
        let from_output = matches!(port, egui_node_graph2::AnyParameterId::Output(_));
        let mut f = crate::finder::FinderState::new(at);
        f.wire = Some(crate::finder::WireDrop { node, port, item, item_name, from_output, extras: Vec::new() });
        self.finder = Some(f);
        self.context_menu = None;
    }

    /// Joins a node added from a dropped wire to where the wire came from:
    /// its first port of the same item, on the other side.
    fn connect_dropped_wire(&mut self, w: &crate::finder::WireDrop, new: NodeId) {
        use egui_node_graph2::AnyParameterId as P;
        let g = &mut self.state.graph;
        if !g.nodes.contains_key(w.node) {
            return;
        }
        let same = |t: &crate::graph::ItemType| t.0 == w.item;
        match w.port {
            P::Output(output) => {
                let input = g[new].inputs.iter().map(|(_, id)| *id).find(|id| same(&g[*id].typ));
                let origin = self.state.graph[w.node].user_data.clone();
                let takes = origin.accepts_wire(w.node, P::Output(output), &self.state.graph, &mut self.user);
                let g = &mut self.state.graph;
                if let (Some(input), true) = (input, takes) {
                    let at = g.connections(input).len();
                    g.add_connection(output, input, at);
                    // The other selected nodes' wires, as far as it takes them.
                    let live: Vec<_> = w.extras.iter().copied().filter(|x| self.state.graph.try_get_output(*x).is_some()).collect();
                    for x in &live {
                        let data = self.state.graph[new].user_data.clone();
                        if data.accepts_wire(new, P::Input(input), &self.state.graph, &mut self.user) {
                            let at = self.state.graph.connections(input).len();
                            self.state.graph.add_connection(*x, input, at);
                        }
                    }
                }
            }
            P::Input(input) => {
                let output = g[new].outputs.iter().map(|(_, id)| *id).find(|id| same(&g[*id].typ));
                let origin = self.state.graph[w.node].user_data.clone();
                let takes = origin.accepts_wire(w.node, P::Input(input), &self.state.graph, &mut self.user);
                let g = &mut self.state.graph;
                if let (Some(output), true) = (output, takes) {
                    let at = g.connections(input).len();
                    g.add_connection(output, input, at);
                }
            }
        }
    }

    /// The Add node menu (right-click on empty canvas).
    fn add_node_menu(&mut self, ctx: &egui::Context) {
        let (Some(state), Some(catalog)) = (self.finder.as_mut(), self.catalog.as_ref()) else {
            return;
        };
        let assets = self.user.assets.as_mut().expect("assets");
        match crate::finder::show(ctx, state, catalog, assets) {
            crate::finder::FinderResult::Open => {}
            crate::finder::FinderResult::Close => self.finder = None,
            crate::finder::FinderResult::Add(template) => {
                let at = state.pos;
                let wire = state.wire.take();
                self.finder = None;
                let graph_pos = at - self.state.pan_zoom.pan - self.editor_rect.min.to_vec2();
                let id = self.add_node(template, graph_pos);
                if let Some(w) = wire {
                    self.connect_dropped_wire(&w, id);
                }
                self.state.selected_nodes = vec![id];
                self.changed();
            }
        }
    }

    // ------------------------------------------------------------- rooms

    pub(crate) fn user_name(&self) -> String {
        self.status.as_ref().map(|s| s.settings.user_name.clone()).filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "Pioneer".into())
    }

    pub(crate) fn room_port(&self) -> u16 {
        self.status.as_ref().map(|s| s.settings.room_port).unwrap_or(fb_model::DEFAULT_ROOM_PORT)
    }

    /// Remember the name for next time.
    pub(crate) fn save_user_name(&mut self, name: &str) {
        if let Some(s) = self.status.as_mut() {
            if s.settings.user_name != name {
                s.settings.user_name = name.to_string();
                let settings = s.settings.clone();
                self.backend.call("set_settings", json!({ "settings": settings }), Msg::Status);
            }
        }
    }

    pub(crate) fn open_room(&mut self, name: String) {
        self.save_user_name(&name);
        self.room.mode = crate::room::Mode::Joining;
        self.backend.call("room_host", json!({ "name": name }), Msg::RoomHosted);
    }

    pub(crate) fn join_room(&mut self, name: String, code: String) {
        self.save_user_name(&name);
        self.room.mode = crate::room::Mode::Joining;
        self.backend.call("room_join", json!({ "name": name, "code": code }), Msg::RoomJoined);
    }

    pub(crate) fn leave_room(&mut self, ctx: &egui::Context) {
        self.backend.call::<()>("room_leave", json!({}), |_| Msg::RoomEvents(Ok(Vec::new())));
        let was_host = matches!(self.room.mode, crate::room::Mode::Hosting(_));
        self.room.reset();
        self.say(ctx, if was_host { "Room closed." } else { "Left the room." }, false);
    }

    /// The shared document: every factory's plan, graph and unzoomed node
    /// positions, and the master graph.
    fn shared_doc_json(&self) -> String {
        let docs = self
            .docs
            .iter()
            .map(|d| {
                let (state, planner) = match &d.body {
                    Some(b) => (&b.state, &b.planner),
                    None => (&self.state, &self.planner),
                };
                let z = state.pan_zoom.zoom.max(0.01);
                let positions = state.node_positions.iter().map(|(id, p)| (id, [p.x / z, p.y / z])).collect();
                crate::room::SharedDocRef { id: d.id, name: &d.name, planner, graph: &state.graph, positions, order: &state.node_order }
            })
            .collect();
        serde_json::to_string(&crate::room::SharedProjectRef { docs }).unwrap_or_default()
    }

    fn share_doc(&mut self) {
        if self.room.active() {
            let json = self.shared_doc_json();
            self.backend.call::<()>("room_send", json!({ "wire": fb_model::room::Wire::Doc { from: self.room.me, rev: 0, json } }), |_| Msg::RoomEvents(Ok(Vec::new())));
        }
    }

    /// Someone else's edit: take their project, keep my view and tabs.
    fn apply_shared_doc(&mut self, json: &str) {
        let Ok(project) = serde_json::from_str::<crate::room::SharedProject>(json) else { return };
        let incoming: BTreeSet<u32> = project.docs.iter().map(|d| d.id).collect();
        if !incoming.contains(&crate::project::MASTER) {
            return;
        }
        if !incoming.contains(&self.active) {
            self.activate(crate::project::MASTER);
        }
        self.docs.retain(|d| incoming.contains(&d.id));
        self.tabs.retain(|t| incoming.contains(t));
        let mut order = Vec::new();
        for doc in project.docs {
            order.push(doc.id);
            if doc.id == self.active {
                apply_shared_graph(&mut self.state, doc.graph, doc.positions, doc.order);
                self.context_menu = None;
                let filter = std::mem::take(&mut self.planner.recipe_filter);
                self.planner = doc.planner;
                self.planner.recipe_filter = filter;
                // Undo history is per person and would undo other people's work.
                self.undo.clear();
                self.redo.clear();
                self.committed = self.project_json();
                self.pending_commit = false;
                if let Some(d) = self.doc_mut(doc.id) {
                    d.name = doc.name;
                }
                continue;
            }
            if self.doc(doc.id).is_none() {
                self.docs.push(crate::project::Doc { id: doc.id, name: String::new(), body: Some(Box::new(crate::project::DocBody::new(Planner::default()))), info: None });
            }
            if let Some(d) = self.doc_mut(doc.id) {
                d.name = doc.name;
                d.info = None;
                if let Some(b) = d.body.as_mut() {
                    apply_shared_graph(&mut b.state, doc.graph, doc.positions, doc.order);
                    b.planner = doc.planner;
                    b.undo.clear();
                    b.redo.clear();
                    b.committed.clear();
                }
            }
        }
        // Everyone's factories in the same order.
        self.docs.sort_by_key(|d| order.iter().position(|o| *o == d.id).unwrap_or(usize::MAX));
        self.dirty = true;
        self.rev += 1;
    }

    fn room_event(&mut self, ctx: &egui::Context, e: fb_model::room::RoomEvent) {
        use fb_model::room::{RoomEvent, Wire};
        match e {
            RoomEvent::Message(Wire::Welcome { you, peers, doc, .. }) => {
                self.room.me = you;
                self.room.peers = peers.into_iter().map(|p| (p.id, p)).collect();
                let host = self.room.peers.get(&0).map(|p| p.name.clone()).unwrap_or_default();
                if let Some(doc) = doc {
                    self.apply_shared_doc(&doc);
                    self.frame_all = true;
                }
                self.say(ctx, format!("Joined {host}'s room."), false);
            }
            RoomEvent::Message(Wire::Joined { peer }) => {
                if peer.id != self.room.me {
                    self.say(ctx, format!("{} joined the room.", peer.name), false);
                }
                self.room.peers.insert(peer.id, peer);
                // They need my view and selection: send them again.
                self.room.sent_view = None;
                self.room.sent_selection = None;
            }
            RoomEvent::Message(Wire::Left { id }) => {
                if let Some(p) = self.room.peers.remove(&id) {
                    self.say(ctx, format!("{} left the room.", p.name), false);
                }
                self.room.forget_peer(id);
            }
            RoomEvent::Message(w @ (Wire::Cursor { .. } | Wire::Ping { .. } | Wire::Chat { .. } | Wire::View { .. } | Wire::Selection { .. })) => {
                self.peer_message(ctx, w);
            }
            RoomEvent::Message(Wire::Doc { from, json, .. }) => {
                if from != self.room.me {
                    self.apply_shared_doc(&json);
                }
            }
            RoomEvent::Message(Wire::Denied { reason }) => {
                self.room.reset();
                self.say(ctx, reason, true);
            }
            RoomEvent::Message(Wire::Hello { .. }) => {}
            RoomEvent::Closed(reason) => {
                if self.room.mode != crate::room::Mode::Off {
                    self.room.reset();
                    self.say(ctx, format!("{reason} Your copy of the factory is kept."), true);
                }
            }
        }
    }

    /// Fetches room messages as soon as the backend says something arrived
    /// (the "room-event" push), with a slow poll as a safety net.
    fn poll_room(&mut self, ctx: &egui::Context) {
        if !self.room.active() {
            return;
        }
        if self.backend.take_room_wake() {
            self.room.wake_pending = true;
        }
        let now = ctx.input(|i| i.time);
        let fallback = now - self.room.last_poll > 0.25 || cfg!(not(target_arch = "wasm32"));
        if !self.room.poll_in_flight && (self.room.wake_pending || fallback) {
            self.room.wake_pending = false;
            self.room.poll_in_flight = true;
            self.room.last_poll = now;
            self.backend.call("room_poll", json!({}), Msg::RoomEvents);
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }

    fn send_cursor(&mut self, ctx: &egui::Context, rect: egui::Rect) {
        if !self.room.active() {
            return;
        }
        let Some(p) = ctx.pointer_hover_pos().filter(|p| rect.contains(*p)) else { return };
        let z = self.state.pan_zoom.zoom.max(0.01);
        let g = ((p - rect.min - self.state.pan_zoom.pan) / z).to_pos2();
        let now = ctx.input(|i| i.time);
        let due = match self.room.last_cursor {
            Some((last, t)) => (last - g).length() > 0.5 && now - t > 0.015,
            None => true,
        };
        if due {
            self.room.last_cursor = Some((g, now));
            self.backend.call::<()>("room_send", json!({ "wire": fb_model::room::Wire::Cursor { id: self.room.me, x: g.x, y: g.y, doc: self.active } }), |_| Msg::RoomEvents(Ok(Vec::new())));
        }
    }

    /// Everyone's cursors on the canvas: others ghosted with their names and
    /// eased between updates; mine is a native cursor in my colour (drawn by
    /// the system, so it never trails the mouse) where the plain arrow would be.
    fn paint_room_cursors(&mut self, ctx: &egui::Context) {
        if !self.room.active() {
            crate::room::set_own_cursor(None);
            return;
        }
        let rect = self.editor_rect;
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("room_cursors"))).with_clip_rect(rect);
        let z = self.state.pan_zoom.zoom;
        let pan = self.state.pan_zoom.pan;
        let dt = ctx.input(|i| i.stable_dt).clamp(0.0, 0.1);
        let ease = 1.0 - (-dt / 0.025).exp();
        let dialog_open = !matches!(self.dialog, Dialog::None) || self.wizard.is_some() || self.view == crate::map::View::Map;
        if !dialog_open {
            self.paint_room_marks(ctx);
        }
        let mut moving = false;
        for (id, target) in self.room.cursors.clone() {
            let shown = self.room.cursor_shown.entry(id).or_insert(target);
            *shown += (target - *shown) * ease;
            if (target - *shown).length() > 0.2 {
                moving = true;
            } else {
                *shown = target;
            }
            if dialog_open || self.room.cursor_doc.get(&id).copied().unwrap_or(0) != self.active {
                continue; // dialogs sit above the canvas; others are on another graph
            }
            if let Some(peer) = self.room.peers.get(&id) {
                let screen = rect.min + pan + shown.to_vec2() * z;
                let bubble = self.room.bubbles.get(&id).map(|(text, _)| text.as_str());
                crate::room::paint_cursor(&painter, screen, crate::room::rgb(peer.color), Some(&peer.name), bubble);
            }
        }
        if moving {
            ctx.request_repaint();
        }
        let plain_arrow = ctx.output(|o| o.cursor_icon) == egui::CursorIcon::Default;
        let over_canvas = ctx.pointer_hover_pos().is_some_and(|p| rect.contains(p));
        let own = over_canvas && plain_arrow && !dialog_open;
        crate::room::set_own_cursor(own.then(|| self.room.my_color()));
    }

    /// Self-test only: expose what the UI computed to the backend's probe.
    fn publish_selftest(&mut self) {
        #[cfg(target_arch = "wasm32")]
        {
            let s = &self.summary;
            let report = json!({
                "nodes": self.state.graph.nodes.len(),
                "links": self.state.graph.iter_connections().count(),
                "min_efficiency": self.user.nodes.values().map(|n| n.efficiency).fold(1.0, f64::min),
                "power_mw": s.power_mw,
                "generated_mw": s.generation,
                "outputs": s.outputs,
                "issues": s.issues.iter().map(|i| i.1.clone()).collect::<Vec<_>>(),
                "assets": self.user.assets.as_ref().map(|a| a.counts()),
                "fonts_installed": self.user.assets.as_ref().map(|a| a.fonts_installed),
                "solve_notes": self.solve_notes,
            });
            if let Some(w) = web_sys::window() {
                let _ = js_sys::Reflect::set(&w, &"__fbSummary".into(), &report.to_string().into());
            }
        }
    }

    // ---------------------------------------------------------- test hooks
    #[cfg(test)]
    pub(crate) fn node_count(&self) -> usize {
        self.state.graph.nodes.len()
    }
    #[cfg(test)]
    pub(crate) fn planner_mut(&mut self) -> &mut Planner {
        &mut self.planner
    }
    #[cfg(test)]
    pub(crate) fn press_plan(&mut self) {
        self.request_plan();
    }
    #[cfg(test)]
    pub(crate) fn outputs(&self) -> Vec<(String, f64, f64)> {
        self.summary.outputs.clone()
    }
    #[cfg(test)]
    pub(crate) fn min_efficiency(&self) -> f64 {
        self.user
            .nodes
            .values()
            .map(|n| n.efficiency)
            .fold(1.0, f64::min)
    }
    #[cfg(test)]
    pub(crate) fn save_as(&mut self, path: &str) {
        self.save_to_path(path.into());
    }
    #[cfg(test)]
    pub(crate) fn open(&mut self, name: &str) {
        self.run_pending(Pending::OpenPath(name.into()));
    }
    #[cfg(test)]
    pub(crate) fn new_project(&mut self) {
        self.run_pending(Pending::New);
    }
    #[cfg(test)]
    pub(crate) fn first_node(&self) -> Option<(NodeId, egui::Rect, egui::Pos2)> {
        let (id, r) = self.node_rects.iter().next()?;
        Some((*id, *r, self.state.node_positions[*id]))
    }
    #[cfg(test)]
    pub(crate) fn node_pos(&self, id: NodeId) -> egui::Pos2 {
        self.state.node_positions[id]
    }
    #[cfg(test)]
    pub(crate) fn overlapping_nodes(&self) -> Vec<(NodeId, NodeId)> {
        let rects: Vec<_> = self.node_rects.iter().collect();
        let mut out = Vec::new();
        for i in 0..rects.len() {
            for j in i + 1..rects.len() {
                if rects[i].1.shrink(1.0).intersects(rects[j].1.shrink(1.0)) {
                    out.push((*rects[i].0, *rects[j].0));
                }
            }
        }
        out
    }
    #[cfg(test)]
    pub(crate) fn add_test_node(&mut self, t: Template, at: egui::Pos2) -> NodeId {
        let id = self.add_node(t, at);
        self.changed();
        id
    }
    #[cfg(test)]
    pub(crate) fn node_pos_unzoomed(&self, id: NodeId) -> egui::Pos2 {
        (self.state.node_positions[id].to_vec2() / self.state.pan_zoom.zoom).to_pos2()
    }
    #[cfg(test)]
    pub(crate) fn screen_to_graph_unzoomed(&self, p: egui::Pos2) -> egui::Pos2 {
        ((p - self.editor_rect.min - self.state.pan_zoom.pan) / self.state.pan_zoom.zoom).to_pos2()
    }
    #[cfg(test)]
    pub(crate) fn screen_to_graph(&self, p: egui::Pos2) -> egui::Pos2 {
        p - self.state.pan_zoom.pan - self.editor_rect.min.to_vec2()
    }
    #[cfg(test)]
    pub(crate) fn node_y(&self, id: NodeId) -> f32 {
        self.state.node_positions[id].y
    }
    #[cfg(test)]
    pub(crate) fn graph(&self) -> &FactoryGraph {
        &self.state.graph
    }
    #[cfg(test)]
    pub(crate) fn selected(&self) -> Vec<NodeId> {
        self.state.selected_nodes.clone()
    }
    #[cfg(test)]
    pub(crate) fn finder_open(&self) -> bool {
        self.finder.is_some()
    }
    #[cfg(test)]
    pub(crate) fn node_labels(&self) -> Vec<String> {
        self.state
            .graph
            .iter_nodes()
            .map(|id| self.state.graph[id].label.clone())
            .collect()
    }
    #[cfg(test)]
    pub(crate) fn set_color_all(&mut self, c: Option<[u8; 3]>) {
        for id in self.state.graph.iter_nodes().collect::<Vec<_>>() {
            self.state.graph[id].user_data.color = c;
        }
        self.changed();
    }
    #[cfg(test)]
    pub(crate) fn colors(&self) -> Vec<Option<[u8; 3]>> {
        self.state
            .graph
            .iter_nodes()
            .map(|id| self.state.graph[id].user_data.color)
            .collect()
    }
    #[cfg(test)]
    pub(crate) fn tallest_node(&self) -> f32 {
        self.node_heights.values().copied().fold(0.0, f32::max)
    }
    #[cfg(test)]
    pub(crate) fn power(&self) -> f64 {
        self.summary.power_mw
    }
    #[cfg(test)]
    pub(crate) fn generation(&self) -> Option<f64> {
        self.summary.generation
    }
    #[cfg(test)]
    pub(crate) fn undo_steps(&self) -> usize {
        self.undo.len()
    }
    #[cfg(test)]
    pub(crate) fn is_dirty(&self) -> bool {
        self.dirty
    }
    #[cfg(test)]
    pub(crate) fn dialog(&self) -> &Dialog {
        &self.dialog
    }
}

pub(crate) type SimCache = Option<(Vec<SimNode>, Vec<SimLink>, SimResult)>;

/// What a graph's flow simulation gives the UI.
#[derive(Default)]
pub(crate) struct Evaluation {
    pub summary: Summary,
    pub ports: std::collections::HashMap<(NodeId, String), PortStat>,
    pub nodes: std::collections::HashMap<NodeId, NodeStat>,
    pub wire_flow: std::collections::HashMap<(egui_node_graph2::InputId, egui_node_graph2::OutputId), f64>,
}

/// Runs the flow simulation over a factory graph (or the master graph of
/// factory blocks) and works out the stats the nodes and overview show.
/// `cache` skips the simulation when nothing that affects it changed.
pub(crate) fn evaluate_graph(game: &GameData, g: &FactoryGraph, cache: &mut SimCache) -> Evaluation {
    let ids: Vec<NodeId> = g.iter_nodes().collect();
    let index: BTreeMap<NodeId, usize> = ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    let item_inputs = |id: NodeId| -> Vec<egui_node_graph2::InputId> {
        g[id].inputs.iter().filter(|(_, i)| matches!(g[*i].value, PortValue::Item(_))).map(|(_, i)| *i).collect()
    };
    let mut links = Vec::new();
    let mut link_wires = Vec::new();
    let router = |n: NodeId| crate::routers::is_router(&g[n].user_data.kind);
    let sim_link = |output: egui_node_graph2::OutputId, input: egui_node_graph2::InputId| {
        let (from, to) = (g[output].node, g[input].node);
        let to_port = item_inputs(to).iter().position(|i| *i == input)?;
        let from_port = g[from].outputs.iter().position(|(_, o)| *o == output)?;
        Some(SimLink { from_node: index[&from], from_port, to_node: index[&to], to_port })
    };
    for (input, outputs) in g.iter_connection_groups() {
        if g.try_get_input(input).is_none() {
            continue;
        }
        for output in outputs {
            // Wires through mergers and splitters go in below, joined up.
            if router(g[output].node) || router(g[input].node) {
                continue;
            }
            if let Some(l) = sim_link(output, input) {
                links.push(l);
                link_wires.push((input, output));
            }
        }
    }
    let routes = crate::routers::route(g);
    let first_route = links.len();
    let mut routed = Vec::new();
    for (k, (o, i)) in routes.pairs.iter().enumerate() {
        if let Some(l) = sim_link(*o, *i) {
            links.push(l);
            routed.push(k);
        }
    }
    let mut connected_in = BTreeSet::new();
    let mut connected_out = BTreeSet::new();
    for l in &links {
        connected_in.insert((l.to_node, l.to_port));
        connected_out.insert((l.from_node, l.from_port));
    }
    let mut rates = Vec::with_capacity(ids.len());
    let mut sim_nodes = Vec::with_capacity(ids.len());
    for (i, &id) in ids.iter().enumerate() {
        let (ins, outs) = node_rates(game, g, id);
        let factory = matches!(g[id].user_data.kind, NodeKind::Factory { .. });
        sim_nodes.push(SimNode {
            // A factory block's unlinked inputs come from outside the
            // project, so they never hold it up.
            inputs: ins.iter().enumerate().map(|(p, x)| if factory && !connected_in.contains(&(i, p)) { 0.0 } else { x.1 }).collect(),
            outputs: outs.iter().map(|x| x.1).collect(),
            throttles: matches!(g[id].user_data.kind, NodeKind::Recipe { .. } | NodeKind::Extractor { .. }),
            sink: matches!(g[id].user_data.kind, NodeKind::Sink { .. }),
        });
        rates.push((ins, outs));
    }
    // A factory's surplus goes to its own storage: each linked output gets
    // an overflow, so sending more than the others take never slows it.
    let mut sim_links = links.clone();
    for (i, &id) in ids.iter().enumerate() {
        if matches!(g[id].user_data.kind, NodeKind::Factory { .. }) {
            for p in 0..rates[i].1.len() {
                if connected_out.contains(&(i, p)) {
                    sim_links.push(SimLink { from_node: i, from_port: p, to_node: sim_nodes.len(), to_port: 0 });
                    sim_nodes.push(SimNode { inputs: vec![0.0], outputs: vec![], throttles: false, sink: true });
                }
            }
        }
    }
    let result = match cache {
        Some((n, l, r)) if *n == sim_nodes && *l == sim_links => r.clone(),
        _ => {
            let r = simulate(&sim_nodes, &sim_links);
            *cache = Some((sim_nodes, sim_links, r.clone()));
            r
        }
    };

    let mut e = Evaluation { wire_flow: link_wires.into_iter().zip(result.link_flow.iter().copied()).collect(), ..Default::default() };
    // Wires through routers carry the flows of the links they stand for.
    let mut pair_flow = vec![0.0; routes.pairs.len()];
    for (n, k) in routed.iter().enumerate() {
        pair_flow[*k] = result.link_flow.get(first_route + n).copied().unwrap_or(0.0);
    }
    for (wire, carried) in &routes.carries {
        e.wire_flow.insert(*wire, carried.iter().map(|k| pair_flow[*k]).sum());
    }
    for &id in &ids {
        if router(id) {
            let through: f64 = g[id].inputs.first().map(|(_, i)| g.connections(*i).iter().map(|o| e.wire_flow.get(&(*i, *o)).copied().unwrap_or(0.0)).sum()).unwrap_or(0.0);
            e.ports.insert((id, crate::routers::ROUTED.into()), PortStat { have: through, want: through, connected: true });
        }
    }
    let mut sum = Summary::default();
    for (i, &id) in ids.iter().enumerate() {
        let (ins, outs) = &rates[i];
        for (p, (name, want)) in ins.iter().enumerate() {
            e.ports.insert((id, name.clone()), PortStat { have: result.received[i][p], want: *want, connected: connected_in.contains(&(i, p)) });
        }
        for (p, (name, want)) in outs.iter().enumerate() {
            e.ports.insert((id, name.clone()), PortStat { have: result.shipped[i][p], want: *want, connected: connected_out.contains(&(i, p)) });
        }
        let eff = result.efficiency[i];
        let mut power = 0.0;
        match &g[id].user_data.kind {
            NodeKind::Recipe { recipe } => {
                if let Some(r) = game.recipes.get(recipe) {
                    let machines = input_number(g, id, MACHINES);
                    let setup = crate::graph::machine_setup(game, g, id);
                    if let Some(b) = game.buildings.get(&r.building) {
                        // Whole machines as built, each at the spread clock speed.
                        power = setup.count as f64 * b.power_with(game.recipe_power_mw(r), setup.clock, setup.sloops_each);
                        let e = sum.buildings.entry(b.class.clone()).or_default();
                        e.0 += setup.count as f64;
                        e.1 += machines;
                        sum.power_shards += setup.shards();
                        sum.somersloops += setup.sloops();
                    }
                    if eff < 0.995 {
                        sum.issues.push((id, format!("{} runs at {:.0}%", r.name, eff * 100.0)));
                    }
                    for (p, (name, want)) in outs.iter().enumerate() {
                        if *want > 1e-9 && !connected_out.contains(&(i, p)) {
                            sum.issues.push((id, format!("{}: {} output isn't connected", r.name, name)));
                        }
                    }
                }
            }
            NodeKind::Resource { item } => *sum.resources.entry(item.clone()).or_default() += result.shipped[i].iter().sum::<f64>(),
            NodeKind::Input { item } => *sum.inputs.entry(item.clone()).or_default() += result.shipped[i].iter().sum::<f64>(),
            NodeKind::Output { item } => {
                let got: f64 = result.received[i].iter().sum();
                let want: f64 = ins.iter().map(|x| x.1).sum();
                if got < want * 0.995 {
                    sum.issues.push((id, format!("Only {} of {} {} delivered", crate::fmt::fmt_rate(got), crate::fmt::fmt_rate(want), game.item_name(item))));
                }
                sum.outputs.push((item.clone(), got, want));
            }
            NodeKind::Sink { item } => {
                let got: f64 = result.received[i].iter().sum();
                *sum.sunk.entry(item.clone()).or_default() += got;
                sum.sink_points += got * game.items.get(item).map(|it| it.sink_points).unwrap_or(0) as f64;
            }
            NodeKind::Pole { .. } | NodeKind::Merger { .. } | NodeKind::Splitter { .. } => {}
            NodeKind::Pump { class, .. } => power = game.buildable_power.get(class).copied().unwrap_or(0.0),
            NodeKind::Extractor { item, class, purity } => {
                if let Some(ex) = crate::extractors::find(game, class) {
                    let setup = crate::graph::machine_setup(game, g, id);
                    power = crate::extractors::power(ex, setup.count, setup.clock);
                    let e = sum.extractors.entry(class.clone()).or_default();
                    e.0 += setup.count as f64;
                    e.1 += input_number(g, id, MACHINES);
                    sum.power_shards += setup.shards();
                    sum.extracted.entry(item.clone()).or_default().push(crate::extractors::line(game, class, *purity, &setup));
                    if eff < 0.995 {
                        sum.issues.push((id, format!("{} runs at {:.0}%", ex.name, eff * 100.0)));
                    }
                }
            }
            NodeKind::Factory { .. } => {
                let name = &g[id].label;
                for (p, (port, want)) in ins.iter().enumerate() {
                    let got = result.received[i][p];
                    if connected_in.contains(&(i, p)) && got < want * 0.995 {
                        sum.issues.push((id, format!("{name} gets only {} of {} {port}", crate::fmt::fmt_rate(got), crate::fmt::fmt_rate(*want))));
                    }
                }
            }
        }
        sum.power_mw += power;
        if let Some(mw) = crate::power_ui::generated_mw(game, g, id, eff) {
            *sum.generation.get_or_insert(0.0) += mw;
        }
        e.nodes.insert(id, NodeStat { efficiency: eff, power_mw: power });
    }
    crate::power_net::evaluate(g, &mut e.ports, &mut sum.issues);
    e.summary = sum;
    e
}

/// Takes a shared graph and its unzoomed positions into `state`, keeping
/// its pan, zoom and (where the nodes still exist) selection.
fn apply_shared_graph(state: &mut EditorState, graph: FactoryGraph, positions: Vec<(NodeId, [f32; 2])>, order: Vec<NodeId>) {
    let z = state.pan_zoom.zoom;
    state.graph = graph;
    state.node_positions.clear();
    for (id, [x, y]) in positions {
        if state.graph.nodes.contains_key(id) {
            state.node_positions.insert(id, egui::pos2(x * z, y * z));
        }
    }
    let mut order: Vec<NodeId> = order.into_iter().filter(|id| state.graph.nodes.contains_key(*id)).collect();
    for id in state.graph.iter_nodes() {
        if !order.contains(&id) {
            order.push(id);
        }
        if !state.node_positions.contains_key(id) {
            state.node_positions.insert(id, egui::Pos2::ZERO);
        }
    }
    state.node_order = order;
    let graph = &state.graph;
    state.selected_nodes.retain(|id| graph.nodes.contains_key(*id));
    state.connection_in_progress = None;
}

/// The game's colour presets (BP_GameState.mStaticGlobalColorPresets).
pub(crate) const GAME_COLORS: [(&str, [u8; 3]); 5] = [
    ("FICSIT primary", [0xFA, 0x95, 0x49]),
    ("FICSIT secondary", [0x5F, 0x66, 0x8C]),
    ("Foundation", [0x5D, 0x5D, 0x5D]),
    ("Concrete", [0xFF, 0xFF, 0xFF]),
    ("Pioneer overalls", [0xD9, 0xCB, 0xB3]),
];

/// A colour chip for the node menu; `None` is "no colour".
pub(crate) fn swatch(
    ui: &mut egui::Ui,
    assets: Option<&mut Assets>,
    rgb: Option<[u8; 3]>,
    selected: bool,
    tip: &str,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(24.0, 24.0), egui::Sense::click());
    let inner = rect.shrink(3.0);
    match rgb {
        Some([r, g, b]) => {
            ui.painter()
                .rect_filled(inner, 0.0, egui::Color32::from_rgb(r, g, b));
        }
        None => {
            ui.painter().rect_filled(inner, 0.0, INPUT);
            if let Some(a) = assets {
                a.paint_ui(
                    ui,
                    crate::assets::UiIcon::Close,
                    inner.shrink(3.0),
                    TEXT_DIM,
                );
            }
        }
    }
    let stroke = if selected {
        egui::Stroke::new(2.0_f32, ACCENT)
    } else if resp.hovered() {
        egui::Stroke::new(1.0_f32, TEXT)
    } else {
        egui::Stroke::new(1.0_f32, DIVIDER)
    };
    ui.painter().rect_stroke(rect.shrink(1.0), 0.0, stroke);
    resp.on_hover_text(tip)
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

#[cfg(target_arch = "wasm32")]
impl eframe::App for FactoryApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.ui(ctx);
    }
}
