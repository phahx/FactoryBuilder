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
use egui_node_graph2::{NodeId, NodeResponse, NodeTemplateTrait};
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
    #[serde(skip)]
    pub recipe_filter: String,
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
    /// building class -> (whole machines, machines at 100%)
    pub buildings: BTreeMap<String, (f64, f64)>,
    pub resources: BTreeMap<String, f64>,
    pub inputs: BTreeMap<String, f64>,
    /// item, delivered, wanted
    pub outputs: Vec<(String, f64, f64)>,
    pub sunk: BTreeMap<String, f64>,
    pub sink_points: f64,
    pub issues: Vec<(NodeId, String)>,
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
    pub(crate) autosaved: String,
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
    pub(crate) sim_cache: Option<(Vec<SimNode>, Vec<SimLink>, SimResult)>,
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
    /// "Not now" on the icons notice, for this session.
    pub(crate) notice_dismissed: bool,
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
            autosaved: String::new(),
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
        };
        app.committed = app.project_json();
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
        let name = self.project_name.as_deref().unwrap_or("Untitled factory");
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
        self.pending_commit = false;
        let now = self.project_json();
        if now != self.committed {
            self.undo.push(std::mem::replace(&mut self.committed, now));
            if self.undo.len() > HISTORY_LIMIT {
                self.undo.remove(0);
            }
            self.redo.clear();
            self.dirty = true;
            self.last_change = ctx.input(|i| i.time);
            self.share_doc();
        }
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
            self.share_doc();
        }
    }

    pub(crate) fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            self.undo
                .push(std::mem::replace(&mut self.committed, next.clone()));
            self.restore(&next);
            self.dirty = true;
            self.share_doc();
        }
    }

    /// Start over from `json` (or empty) with a fresh history.
    pub(crate) fn reset_to(&mut self, json: Option<&str>, name: Option<String>) {
        self.current_path = None;
        match json {
            Some(j) => self.restore(j),
            None => {
                self.state = EditorState::new(1.0);
                self.state.style = theme::graph_style();
                if let Some(g) = self.game_loaded.then(|| self.user.game.clone()) {
                    self.planner.targets = vec![Target {
                        item: "Desc_IronPlateReinforced_C".into(),
                        rate: 5.0,
                    }];
                    self.planner.inputs.clear();
                    self.planner.enabled = g
                        .recipes
                        .values()
                        .filter(|r| !r.alternate)
                        .map(|r| r.class.clone())
                        .collect();
                    self.planner.limits = g.resource_limits.clone();
                    self.planner.objective = Objective::Resources;
                }
            }
        }
        self.project_name = name;
        self.undo.clear();
        self.redo.clear();
        self.committed = self.project_json();
        self.pending_commit = false;
        self.dirty = false;
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
        }
    }

    pub(crate) fn backend_ctx(&self) -> egui::Context {
        self.backend.ctx()
    }

    pub(crate) fn reset_history(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.committed = self.project_json();
        self.pending_commit = false;
        self.dirty = false;
    }

    pub(crate) fn mark_saved(&mut self) {
        self.dirty = false;
        self.committed = self.project_json();
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
        if self.committed != self.autosaved {
            self.autosaved = self.committed.clone();
            self.autosave_in_flight = true;
            let json = self.committed.clone();
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
                Msg::Solved(r) => {
                    self.solving = false;
                    match r {
                        Ok(SolveOutcome::Solved(plan)) => {
                            self.solve_notes.clear();
                            self.apply_plan(&plan);
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
                                self.autosaved = self.committed.clone();
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
        if self.planner.enabled.is_empty() {
            self.planner.enabled = game
                .recipes
                .values()
                .filter(|r| !r.alternate)
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
        };
        self.backend
            .call("solve", json!({ "request": req }), Msg::Solved);
    }

    // --------------------------------------------------------------- graph

    fn add_node(&mut self, template: Template, pos: egui::Pos2) -> NodeId {
        let label = template.node_graph_label(&mut self.user);
        let data = template.user_data(&mut self.user);
        let user = &mut self.user;
        let id = self
            .state
            .graph
            .add_node(label, data, |g, id| template.build_node(g, user, id));
        self.state.node_positions.insert(id, pos);
        self.state.node_order.push(id);
        id
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

    fn arrange(&mut self) {
        let ids: Vec<NodeId> = self.state.graph.iter_nodes().collect();
        let index: BTreeMap<NodeId, usize> =
            ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
        let g = &self.state.graph;
        let edges: Vec<(usize, usize)> = g
            .iter_connections()
            .map(|(i, o)| (index[&g[o].node], index[&g[i].node]))
            .collect();
        let heights: Vec<f32> = ids
            .iter()
            .map(|id| self.node_heights.get(id).copied().unwrap_or(260.0))
            .collect();
        let pos = fb_engine::layout::layered_sized(
            ids.len(),
            &edges,
            theme::graph_style().node_width,
            &heights,
        );
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

    /// Runs the flow simulation over the current graph and fills the stats
    /// the nodes and the overview show.
    fn evaluate(&mut self) {
        let game = self.user.game.clone();
        let g = &self.state.graph;
        let ids: Vec<NodeId> = g.iter_nodes().collect();
        let index: BTreeMap<NodeId, usize> =
            ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
        let mut rates = Vec::with_capacity(ids.len());
        let mut sim_nodes = Vec::with_capacity(ids.len());
        for &id in &ids {
            let (ins, outs) = node_rates(&game, g, id);
            sim_nodes.push(SimNode {
                inputs: ins.iter().map(|x| x.1).collect(),
                outputs: outs.iter().map(|x| x.1).collect(),
                throttles: matches!(g[id].user_data.kind, NodeKind::Recipe { .. }),
                sink: matches!(g[id].user_data.kind, NodeKind::Sink { .. }),
            });
            rates.push((ins, outs));
        }
        let item_inputs = |id: NodeId| -> Vec<egui_node_graph2::InputId> {
            g[id]
                .inputs
                .iter()
                .filter(|(_, i)| matches!(g[*i].value, PortValue::Item(_)))
                .map(|(_, i)| *i)
                .collect()
        };
        let mut links = Vec::new();
        for (input, outputs) in g.iter_connection_groups() {
            let Some(to) = g.try_get_input(input).map(|i| i.node) else {
                continue;
            };
            let Some(to_port) = item_inputs(to).iter().position(|i| *i == input) else {
                continue;
            };
            for output in outputs {
                let from = g[output].node;
                let Some(from_port) = g[from].outputs.iter().position(|(_, o)| *o == output) else {
                    continue;
                };
                links.push(SimLink {
                    from_node: index[&from],
                    from_port,
                    to_node: index[&to],
                    to_port,
                });
            }
        }
        let result = match &self.sim_cache {
            Some((n, l, r)) if *n == sim_nodes && *l == links => r.clone(),
            _ => {
                let r = simulate(&sim_nodes, &links);
                self.sim_cache = Some((sim_nodes, links.clone(), r.clone()));
                r
            }
        };

        self.user.ports.clear();
        self.user.nodes.clear();
        let mut connected_in = BTreeSet::new();
        let mut connected_out = BTreeSet::new();
        for l in &links {
            connected_in.insert((l.to_node, l.to_port));
            connected_out.insert((l.from_node, l.from_port));
        }
        let mut sum = Summary::default();
        for (i, &id) in ids.iter().enumerate() {
            let (ins, outs) = &rates[i];
            for (p, (name, want)) in ins.iter().enumerate() {
                self.user.ports.insert(
                    (id, name.clone()),
                    PortStat {
                        have: result.received[i][p],
                        want: *want,
                        connected: connected_in.contains(&(i, p)),
                    },
                );
            }
            for (p, (name, want)) in outs.iter().enumerate() {
                self.user.ports.insert(
                    (id, name.clone()),
                    PortStat {
                        have: result.shipped[i][p],
                        want: *want,
                        connected: connected_out.contains(&(i, p)),
                    },
                );
            }
            let eff = result.efficiency[i];
            let mut power = 0.0;
            match &g[id].user_data.kind {
                NodeKind::Recipe { recipe } => {
                    if let Some(r) = game.recipes.get(recipe) {
                        let machines = input_number(g, id, MACHINES);
                        let clock = input_number(g, id, CLOCK) / 100.0;
                        if let Some(b) = game.buildings.get(&r.building) {
                            power = machines * b.power_at(game.recipe_power_mw(r), clock);
                            let e = sum.buildings.entry(b.class.clone()).or_default();
                            let throughput = machines * clock;
                            e.0 += (throughput - 1e-6).ceil().max(0.0);
                            e.1 += machines;
                        }
                        if eff < 0.995 {
                            sum.issues
                                .push((id, format!("{} runs at {:.0}%", r.name, eff * 100.0)));
                        }
                        for (p, (name, want)) in outs.iter().enumerate() {
                            if *want > 1e-9 && !connected_out.contains(&(i, p)) {
                                sum.issues.push((
                                    id,
                                    format!("{}: {} output isn't connected", r.name, name),
                                ));
                            }
                        }
                    }
                }
                NodeKind::Resource { item } => {
                    *sum.resources.entry(item.clone()).or_default() +=
                        result.shipped[i].iter().sum::<f64>()
                }
                NodeKind::Input { item } => {
                    *sum.inputs.entry(item.clone()).or_default() +=
                        result.shipped[i].iter().sum::<f64>()
                }
                NodeKind::Output { item } => {
                    let got: f64 = result.received[i].iter().sum();
                    let want: f64 = ins.iter().map(|x| x.1).sum();
                    if got < want * 0.995 {
                        sum.issues.push((
                            id,
                            format!(
                                "Only {} of {} {} delivered",
                                crate::fmt::fmt_rate(got),
                                crate::fmt::fmt_rate(want),
                                game.item_name(item)
                            ),
                        ));
                    }
                    sum.outputs.push((item.clone(), got, want));
                }
                NodeKind::Sink { item } => {
                    let got: f64 = result.received[i].iter().sum();
                    *sum.sunk.entry(item.clone()).or_default() += got;
                    sum.sink_points +=
                        got * game.items.get(item).map(|it| it.sink_points).unwrap_or(0) as f64;
                }
            }
            sum.power_mw += power;
            self.user.nodes.insert(
                id,
                NodeStat {
                    efficiency: eff,
                    power_mw: power,
                },
            );
        }
        self.summary = sum;
    }

    // ------------------------------------------------------------------ UI

    pub fn ui(&mut self, ctx: &egui::Context) {
        self.handle_messages(ctx);
        self.poll_room(ctx);
        if self.game_loaded {
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
            .show(ctx, |ui| crate::panels::planner(self, ui));
        egui::SidePanel::right("overview")
            .resizable(true)
            .default_width(300.0)
            .min_width(260.0)
            .frame(egui::Frame::none().fill(CONTENT))
            .show(ctx, |ui| crate::panels::overview(self, ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(BACKDROP))
            .show(ctx, |ui| self.editor_ui(ui));
        self.node_menu(ctx);
        self.add_node_menu(ctx);
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
                "settings_dev" => {
                    let s = self.status.as_ref().map(|s| s.settings.clone()).unwrap_or_default();
                    self.dialog = Dialog::settings(&s);
                }
                "export" => {
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
                    if view == "room_info" {
                        self.dialog = Dialog::RoomInfo;
                    } else {
                        self.dialog = Dialog::None;
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
            }
            return;
        }
        let cmd = |key| KeyboardShortcut::new(Modifiers::COMMAND, key);
        let cmd_shift = |key| KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, key);
        let hit = |s: KeyboardShortcut| ctx.input_mut(|i| i.consume_shortcut(&s));
        if hit(cmd_shift(Key::S)) {
            self.pick_save(crate::app_files::SavePurpose::Factory);
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
        if ctx.wants_keyboard_input() {
            return; // typing in a field: leave editing keys to the field
        }
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
        if ctx.input(|i| i.key_pressed(Key::Delete)) && self.has_selection() {
            self.delete_selected();
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
            "Satisfactory has updated. Update the icons and font to match?"
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
                        if crate::widgets::button(ui, "Extract icons and font").clicked() {
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
        if self.frame_all {
            self.frame_all = false;
            self.fit_view(ui);
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
        if self.state.graph.nodes.is_empty() {
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                "Plan a factory from the panel on the left,\nor right-click here to place machines yourself.",
                egui::FontId::proportional(15.0),
                TEXT_DIM,
            );
        }
        let resp = self
            .state
            .draw_graph_editor(ui, &self.templates, &mut self.user, Vec::new());
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
        if let Some(menu) = resp.node_context_menu {
            self.context_menu = Some(menu);
            self.context_targets = self.state.selected_nodes.clone();
            if !self.context_targets.contains(&menu.0) {
                self.context_targets = vec![menu.0];
            }
            self.custom_color = None;
        }
        // Right-click on empty canvas: show our Add node menu instead of the
        // library's finder.
        if let Some(f) = self.state.node_finder.take() {
            let at = f.position.unwrap_or(rect.center());
            self.finder = Some(crate::finder::FinderState::new(at));
            self.context_menu = None;
        }
        self.editor_rect = rect;
        self.send_cursor(ui.ctx(), rect);
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
            self.arrange();
            self.frame_all = true;
            self.changed();
            ui.ctx().request_repaint();
        }
    }

    /// Fit every node into view (View > Frame all, Home).
    fn fit_view(&mut self, ui: &egui::Ui) {
        let rect = ui.max_rect();
        let z = self.state.pan_zoom.zoom;
        let w = theme::graph_style().node_width;
        let size = |id: NodeId| vec2(w, self.node_heights.get(&id).copied().unwrap_or(260.0));
        let mut bounds = egui::Rect::NOTHING;
        for (id, p) in self.state.node_positions.iter() {
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
        for (id, p) in self.state.node_positions.iter() {
            b = b.union(egui::Rect::from_min_size(*p, size(id) * target));
        }
        self.state.pan_zoom.pan = rect.size() / 2.0 - b.center().to_vec2();
    }

    fn draw_grid(&self, ui: &egui::Ui, rect: egui::Rect) {
        let step = 32.0 * self.state.pan_zoom.zoom;
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
        let mut close = false;
        // Run after the menu is drawn: the menu reads the node it's for, and
        // deleting it mid-draw would index a node that no longer exists.
        let (mut do_duplicate, mut do_delete) = (false, false);
        #[cfg(test)]
        self.menu_swatches.clear();
        let area = egui::Area::new(egui::Id::new("node_menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(180.0);
                    if ui
                        .add(egui::Button::new("Duplicate").shortcut_text("Ctrl+D"))
                        .clicked()
                    {
                        do_duplicate = true;
                        close = true;
                    }
                    if ui
                        .add(egui::Button::new("Delete").shortcut_text("Del"))
                        .clicked()
                    {
                        do_delete = true;
                        close = true;
                    }
                    ui.separator();
                    ui.label(RichText::new("Colour").small().color(TEXT_DIM));
                    let current = self.state.graph[node].user_data.color;
                    let mut pick: Option<Option<[u8; 3]>> = None;
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        let r = swatch(
                            ui,
                            self.user.assets.as_mut(),
                            None,
                            current.is_none(),
                            "No colour",
                        );
                        #[cfg(test)]
                        self.menu_swatches.push(r.rect);
                        if r.clicked() {
                            pick = Some(None);
                        }
                        for (name, rgb) in GAME_COLORS {
                            let r = swatch(
                                ui,
                                self.user.assets.as_mut(),
                                Some(rgb),
                                current == Some(rgb),
                                name,
                            );
                            #[cfg(test)]
                            self.menu_swatches.push(r.rect);
                            if r.clicked() {
                                pick = Some(Some(rgb));
                            }
                        }
                    });
                    match self.custom_color {
                        None => {
                            if ui.button("Custom colour…").clicked() {
                                self.custom_color = Some(current.unwrap_or([0xE5, 0x93, 0x44]));
                            }
                        }
                        Some(rgb) => {
                            let mut c = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                            if egui::color_picker::color_picker_color32(
                                ui,
                                &mut c,
                                egui::color_picker::Alpha::Opaque,
                            ) {
                                self.custom_color = Some([c.r(), c.g(), c.b()]);
                                pick = Some(Some([c.r(), c.g(), c.b()]));
                            }
                        }
                    }
                    if let Some(color) = pick {
                        for id in self.context_targets.clone() {
                            if self.state.graph.nodes.contains_key(id) {
                                self.state.graph[id].user_data.color = color;
                            }
                        }
                        self.changed();
                    }
                });
            });
        let clicked_elsewhere = ctx.input(|i| i.pointer.any_pressed())
            && !area.response.contains_pointer()
            && !area.response.hovered();
        if close || clicked_elsewhere || ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.context_menu = None;
        }
        if do_duplicate {
            self.duplicate_selected();
        }
        if do_delete {
            self.delete_selected();
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
                self.finder = None;
                let graph_pos = at - self.state.pan_zoom.pan - self.editor_rect.min.to_vec2();
                let id = self.add_node(template, graph_pos);
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

    /// The shared document: plan, graph and unzoomed node positions.
    fn shared_doc_json(&self) -> String {
        let z = self.state.pan_zoom.zoom.max(0.01);
        let positions = self.state.node_positions.iter().map(|(id, p)| (id, [p.x / z, p.y / z])).collect();
        serde_json::to_string(&crate::room::SharedDocRef { planner: &self.planner, graph: &self.state.graph, positions, order: &self.state.node_order }).unwrap_or_default()
    }

    fn share_doc(&mut self) {
        if self.room.active() {
            let json = self.shared_doc_json();
            self.backend.call::<()>("room_send", json!({ "wire": fb_model::room::Wire::Doc { from: self.room.me, rev: 0, json } }), |_| Msg::RoomEvents(Ok(Vec::new())));
        }
    }

    /// Someone else's edit: take their document, keep my view.
    fn apply_shared_doc(&mut self, json: &str) {
        let Ok(doc) = serde_json::from_str::<crate::room::SharedDoc>(json) else { return };
        let z = self.state.pan_zoom.zoom;
        self.state.graph = doc.graph;
        self.state.node_positions.clear();
        for (id, [x, y]) in doc.positions {
            if self.state.graph.nodes.contains_key(id) {
                self.state.node_positions.insert(id, egui::pos2(x * z, y * z));
            }
        }
        let mut order: Vec<NodeId> = doc.order.into_iter().filter(|id| self.state.graph.nodes.contains_key(*id)).collect();
        for id in self.state.graph.iter_nodes() {
            if !order.contains(&id) {
                order.push(id);
            }
            if !self.state.node_positions.contains_key(id) {
                self.state.node_positions.insert(id, egui::Pos2::ZERO);
            }
        }
        self.state.node_order = order;
        let graph = &self.state.graph;
        self.state.selected_nodes.retain(|id| graph.nodes.contains_key(*id));
        self.state.connection_in_progress = None;
        self.context_menu = None;
        let filter = std::mem::take(&mut self.planner.recipe_filter);
        self.planner = doc.planner;
        self.planner.recipe_filter = filter;
        // Undo history is per person and would undo other people's work.
        self.undo.clear();
        self.redo.clear();
        self.committed = self.project_json();
        self.pending_commit = false;
        self.dirty = true;
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
            }
            RoomEvent::Message(Wire::Left { id }) => {
                if let Some(p) = self.room.peers.remove(&id) {
                    self.say(ctx, format!("{} left the room.", p.name), false);
                }
                self.room.cursors.remove(&id);
            }
            RoomEvent::Message(Wire::Cursor { id, x, y }) => {
                if id != self.room.me {
                    self.room.cursors.insert(id, egui::pos2(x, y));
                }
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
            self.backend.call::<()>("room_send", json!({ "wire": fb_model::room::Wire::Cursor { id: self.room.me, x: g.x, y: g.y } }), |_| Msg::RoomEvents(Ok(Vec::new())));
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
        let dialog_open = !matches!(self.dialog, Dialog::None) || self.wizard.is_some();
        let mut moving = false;
        for (id, target) in self.room.cursors.clone() {
            let shown = self.room.cursor_shown.entry(id).or_insert(target);
            *shown += (target - *shown) * ease;
            if (target - *shown).length() > 0.2 {
                moving = true;
            } else {
                *shown = target;
            }
            if dialog_open {
                continue; // dialogs sit above the canvas
            }
            if let Some(peer) = self.room.peers.get(&id) {
                let screen = rect.min + pan + shown.to_vec2() * z;
                crate::room::paint_cursor(&painter, screen, crate::room::rgb(peer.color), Some(&peer.name));
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

/// The game's colour presets (BP_GameState.mStaticGlobalColorPresets).
pub(crate) const GAME_COLORS: [(&str, [u8; 3]); 5] = [
    ("FICSIT primary", [0xFA, 0x95, 0x49]),
    ("FICSIT secondary", [0x5F, 0x66, 0x8C]),
    ("Foundation", [0x5D, 0x5D, 0x5D]),
    ("Concrete", [0xFF, 0xFF, 0xFF]),
    ("Pioneer overalls", [0xD9, 0xCB, 0xB3]),
];

/// A colour chip for the node menu; `None` is "no colour".
fn swatch(
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
