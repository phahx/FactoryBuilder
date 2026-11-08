//! Open, save, import, export and share.
//!
//! A project with one factory is saved as a `.fbfactory` file, one with more
//! as a `.fbproject` (both in the portable format, portable.rs), wherever the
//! user picks with the native dialogs. Opening replaces the project (after
//! offering to save); importing adds a factory's nodes beside the open
//! factory's; "Add factory" adds it to the project as its own factory.
//! Exports: a PNG of the graph on screen, factory JSON, and a text summary.
//! Share codes are the portable format compressed.

use crate::app::{FactoryApp, Planner};
use crate::backend::Msg;
use crate::dialogs::{Dialog, Pending};
use crate::graph::*;
use crate::portable::{self, FactoryFile, ProjectFile, Shared};
use fb_model::{PowerMode, PowerSettings};
use egui::{vec2, Pos2, Rect, Vec2};
use egui_node_graph2::{InputId, NodeId, OutputId};
use serde_json::json;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenPurpose {
    Open,
    /// Into the open factory's graph.
    Import,
    /// Into the project as a factory of its own.
    AddFactory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SavePurpose {
    Factory,
    Project,
    Png,
    Json,
    Text,
}

impl SavePurpose {
    fn kind(self) -> &'static str {
        match self {
            SavePurpose::Factory => "factory",
            SavePurpose::Project => "project",
            SavePurpose::Png => "png",
            SavePurpose::Json => "json",
            SavePurpose::Text => "txt",
        }
    }
}

const RECENT_MAX: usize = 10;
const NODE_WIDTH: f32 = 300.0;
const MAX_IMAGE_SIDE: f32 = 16384.0;

pub fn file_name(path: &str) -> String {
    std::path::Path::new(path).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string())
}

pub fn is_project_path(path: &str) -> bool {
    std::path::Path::new(path).extension().is_some_and(|e| e.eq_ignore_ascii_case("fbproject"))
}

/// The order nodes are written in a factory file (draw order, then the
/// rest); a file's wires refer to nodes by their place in it.
pub(crate) fn portable_order(state: &EditorState) -> Vec<NodeId> {
    let g = &state.graph;
    let plain = |id: &NodeId| g.nodes.contains_key(*id) && !matches!(g[*id].user_data.kind, NodeKind::Factory { .. });
    let mut ids: Vec<NodeId> = state.node_order.iter().copied().filter(plain).collect();
    for id in g.iter_nodes() {
        if plain(&id) && !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

pub fn file_stem(path: &str) -> String {
    std::path::Path::new(path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string())
}

/// A PNG export in progress: the graph is drawn tile by tile at full detail
/// and each finished frame is read back from the canvas.
pub struct ImageExport {
    pub path: String,
    tiles: Vec<Pos2>,
    current: usize,
    frames_left: u8,
    bounds: Rect,
    scale: f32,
    ppp: f32,
    image: image::RgbaImage,
    saved_zoom: f32,
    saved_pan: Vec2,
    saved_selection: Vec<NodeId>,
}

/// Tile origins (graph units) covering `bounds` with tiles of `tile` size.
pub fn tile_origins(bounds: Rect, tile: Vec2) -> Vec<Pos2> {
    let cols = (bounds.width() / tile.x).ceil().max(1.0) as usize;
    let rows = (bounds.height() / tile.y).ceil().max(1.0) as usize;
    let mut v = Vec::with_capacity(cols * rows);
    for r in 0..rows {
        for c in 0..cols {
            v.push(bounds.min + vec2(c as f32 * tile.x, r as f32 * tile.y));
        }
    }
    v
}

impl FactoryApp {
    // ------------------------------------------------------- portable form

    /// The open factory in the portable file format.
    pub(crate) fn to_portable(&self) -> FactoryFile {
        self.portable_of(&self.state, &self.planner, Some(self.doc_name(self.active)))
    }

    /// A factory's graph and plan in the portable file format.
    pub(crate) fn portable_of(&self, state: &EditorState, planner: &Planner, name: Option<String>) -> FactoryFile {
        let g = &state.graph;
        let game = &self.user.game;
        let z = state.pan_zoom.zoom.max(0.01);
        let ids = portable_order(state);
        let index: BTreeMap<NodeId, usize> = ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
        let mut f = FactoryFile::new(name);
        for &id in &ids {
            let n = &g[id];
            let pos = state.node_positions.get(id).map(|p| (p.to_vec2() / z).to_pos2()).unwrap_or(Pos2::ZERO);
            let (kind, class) = match &n.user_data.kind {
                NodeKind::Recipe { recipe } => (portable::Kind::Recipe, recipe.clone()),
                NodeKind::Resource { item } => (portable::Kind::Resource, item.clone()),
                NodeKind::Input { item } => (portable::Kind::Input, item.clone()),
                NodeKind::Output { item } => (portable::Kind::Output, item.clone()),
                NodeKind::Sink { item } => (portable::Kind::Sink, item.clone()),
                NodeKind::Factory { .. } => continue, // only on the master graph
            };
            let is_recipe = kind == portable::Kind::Recipe;
            let has_rate = matches!(kind, portable::Kind::Resource | portable::Kind::Input | portable::Kind::Output);
            f.nodes.push(portable::Node {
                kind,
                class,
                machines: is_recipe.then(|| input_number(g, id, MACHINES)),
                clock: is_recipe.then(|| input_number(g, id, CLOCK)),
                sloops: Some(crate::graph::sloops(game, g, id)).filter(|n| *n > 0),
                rate: has_rate.then(|| input_number(g, id, RATE)),
                color: n.user_data.color.map(portable::color_to_hex),
                x: (pos.x * 10.0).round() / 10.0,
                y: (pos.y * 10.0).round() / 10.0,
            });
        }
        for (to, &id) in ids.iter().enumerate() {
            let item_inputs: Vec<InputId> = g[id].inputs.iter().filter(|(_, i)| matches!(g[*i].value, PortValue::Item(_))).map(|(_, i)| *i).collect();
            for (input, &iid) in item_inputs.iter().enumerate() {
                for out in g.connections(iid) {
                    let from_node = g[out].node;
                    let (Some(&from), Some(output)) = (index.get(&from_node), g[from_node].outputs.iter().position(|(_, o)| *o == out)) else { continue };
                    let slot = g.output_connections(out).iter().position(|i| *i == iid).unwrap_or(0);
                    f.wires.push(portable::Wire { from, output, to, input, slot });
                }
            }
        }
        let p = planner;
        let alts_on: Vec<String> = game.recipes.values().filter(|r| r.alternate && p.enabled.contains(&r.class)).map(|r| r.class.clone()).collect();
        let alts_off: Vec<String> = game.recipes.values().filter(|r| r.alternate && !p.enabled.contains(&r.class)).map(|r| r.class.clone()).collect();
        // Whichever list is shorter keeps files and share codes small.
        let most_on = alts_off.len() < alts_on.len();
        f.plan = portable::Plan {
            products: p.targets.clone(),
            available: p.inputs.clone(),
            optimize: p.objective,
            alternates_on: if most_on { Vec::new() } else { alts_on },
            all_alternates: most_on,
            alternates_off: if most_on { alts_off } else { Vec::new() },
            standard_off: game.recipes.values().filter(|r| !r.alternate && !r.is_generator() && !p.enabled.contains(&r.class)).map(|r| r.class.clone()).collect(),
            limits: p.limits.iter().filter(|(k, v)| game.resource_limits.get(*k) != Some(*v)).map(|(k, v)| (k.clone(), *v)).collect(),
            generate_power: p.power.mode == PowerMode::Generate,
            generators: (p.power.generators != PowerSettings::default().generators).then(|| p.power.generators.clone()),
        };
        f
    }

    pub(crate) fn planner_from_portable(&self, plan: &portable::Plan) -> Planner {
        let game = &self.user.game;
        let mut enabled: std::collections::BTreeSet<String> =
            game.recipes.values().filter(|r| !r.alternate && !r.is_generator() && !plan.standard_off.contains(&r.class)).map(|r| r.class.clone()).collect();
        if plan.all_alternates {
            enabled.extend(game.recipes.values().filter(|r| r.alternate && !plan.alternates_off.contains(&r.class)).map(|r| r.class.clone()));
        }
        enabled.extend(plan.alternates_on.iter().filter(|c| game.recipes.contains_key(*c)).cloned());
        let mut limits = game.resource_limits.clone();
        for (k, v) in &plan.limits {
            if limits.contains_key(k) && v.is_finite() && *v >= 0.0 {
                limits.insert(k.clone(), *v);
            }
        }
        Planner {
            targets: plan.products.iter().filter(|t| game.items.contains_key(&t.item)).cloned().collect(),
            inputs: plan.available.iter().filter(|t| game.items.contains_key(&t.item)).cloned().collect(),
            objective: plan.optimize,
            enabled,
            limits,
            extraction: self.planner.extraction.clone(),
            recipe_filter: self.planner.recipe_filter.clone(),
            power: PowerSettings {
                mode: if plan.generate_power { PowerMode::Generate } else { PowerMode::Grid },
                generators: match &plan.generators {
                    Some(list) => list.iter().filter(|c| game.recipes.get(*c).is_some_and(|r| r.is_generator())).cloned().collect(),
                    None => PowerSettings::default().generators,
                },
            },
        }
    }

    /// Builds the file's nodes and wires into the graph, shifted by `offset`
    /// (graph units). Returns the new nodes and how many entries were skipped
    /// because this game version doesn't know them.
    pub(crate) fn insert_portable(&mut self, f: &FactoryFile, offset: Vec2) -> (Vec<NodeId>, usize) {
        let game = self.user.game.clone();
        let z = self.state.pan_zoom.zoom;
        let mut ids: Vec<Option<NodeId>> = vec![None; f.nodes.len()];
        let mut skipped = 0;
        for (i, n) in f.nodes.iter().enumerate() {
            let known_item = game.items.contains_key(&n.class);
            let template = match n.kind {
                portable::Kind::Recipe if game.recipes.contains_key(&n.class) => Template::Recipe(n.class.clone()),
                portable::Kind::Resource if known_item => Template::Resource(n.class.clone()),
                portable::Kind::Input if known_item => Template::Input(n.class.clone()),
                portable::Kind::Output if known_item => Template::Output(n.class.clone()),
                portable::Kind::Sink if known_item => Template::Sink(n.class.clone()),
                _ => {
                    skipped += 1;
                    continue;
                }
            };
            let pos = ((egui::pos2(n.x, n.y) + offset).to_vec2() * z).to_pos2();
            let id = self.add_node_at(template, pos);
            let set = |app: &mut FactoryApp, name: &str, v: Option<f64>| {
                if let Some(v) = v.filter(|v| v.is_finite()) {
                    if let Ok(input) = app.state.graph[id].get_input(name) {
                        app.state.graph[input].value.set_number(v);
                    }
                }
            };
            set(self, MACHINES, n.machines);
            set(self, CLOCK, n.clock);
            set(self, crate::graph::SLOOPS, n.sloops.map(f64::from));
            set(self, RATE, n.rate);
            self.state.graph[id].user_data.color = n.color.as_deref().and_then(portable::color_from_hex);
            ids[i] = Some(id);
        }
        // Wires: inputs in file order, then each output's bubble order.
        let mut wires: Vec<(usize, &portable::Wire)> = f.valid_wires().enumerate().collect();
        wires.sort_by_key(|(k, w)| (w.to, w.input, *k));
        let mut slots: BTreeMap<OutputId, Vec<(usize, InputId)>> = BTreeMap::new();
        for (_, w) in wires {
            let (Some(from), Some(to)) = (ids[w.from], ids[w.to]) else { continue };
            let g = &self.state.graph;
            let item_inputs: Vec<InputId> = g[to].inputs.iter().filter(|(_, i)| matches!(g[*i].value, PortValue::Item(_))).map(|(_, i)| *i).collect();
            let (Some(&iid), Some(&(_, oid))) = (item_inputs.get(w.input), g[from].outputs.get(w.output)) else { continue };
            if g[oid].typ != g[iid].typ {
                continue; // a different item: the recipe changed between game versions
            }
            let pos = g.connections(iid).len();
            self.state.graph.add_connection(oid, iid, pos);
            slots.entry(oid).or_default().push((w.slot, iid));
        }
        for (oid, mut list) in slots {
            list.sort_by_key(|(slot, _)| *slot);
            self.state.graph.set_output_order(oid, list.into_iter().map(|(_, i)| i).collect());
        }
        (ids.into_iter().flatten().collect(), skipped)
    }

    /// Replaces the project with project file `p`.
    pub(crate) fn open_project(&mut self, ctx: &egui::Context, p: ProjectFile, path: Option<String>) {
        let skipped = self.load_project_file(&p);
        self.project_name = path.as_deref().map(file_stem).or_else(|| p.name.clone());
        self.current_path = path;
        self.reset_history();
        self.share_doc_now();
        let what = self.project_name.clone().unwrap_or_else(|| "the project".into());
        if skipped > 0 {
            self.say(ctx, format!("Opened “{what}”. {skipped} parts weren't loaded: this game version doesn't have them."), true);
        } else {
            self.say(ctx, format!("Opened “{what}”."), false);
        }
    }

    /// Replaces the project with one factory, `f`.
    pub(crate) fn open_factory(&mut self, ctx: &egui::Context, f: FactoryFile, path: Option<String>) {
        let name = path.as_deref().map(file_stem).or_else(|| f.name.clone());
        self.reset_to(None, name.clone());
        self.planner = self.planner_from_portable(&f.plan);
        let (_, skipped) = self.insert_portable(&f, Vec2::ZERO);
        self.current_path = path;
        self.reset_history();
        self.frame_all = true;
        self.share_doc_now();
        let what = name.unwrap_or_else(|| "the factory".into());
        if skipped > 0 {
            self.say(ctx, format!("Opened “{what}”. {skipped} parts weren't loaded: this game version doesn't have them."), true);
        } else {
            self.say(ctx, format!("Opened “{what}”."), false);
        }
    }

    /// Adds `f` beside the open factory's graph and selects it.
    pub(crate) fn import_factory(&mut self, ctx: &egui::Context, f: FactoryFile) {
        if self.on_master() {
            // The master graph only has factories: add it as one.
            self.add_factory_file(ctx, &f);
            return;
        }
        let z = self.state.pan_zoom.zoom.max(0.01);
        let mut right = f32::NEG_INFINITY;
        let mut top = f32::INFINITY;
        for (id, p) in self.state.node_positions.iter() {
            let g = (p.to_vec2() / z).to_pos2();
            right = right.max(g.x + NODE_WIDTH);
            top = top.min(g.y);
            let _ = id;
        }
        let (fx, fy) = f.nodes.iter().fold((f32::INFINITY, f32::INFINITY), |(x, y), n| (x.min(n.x), y.min(n.y)));
        let offset = if right.is_finite() && fx.is_finite() { vec2(right + 120.0 - fx, top - fy) } else { Vec2::ZERO };
        let (ids, skipped) = self.insert_portable(&f, offset);
        self.state.selected_nodes = ids.clone();
        self.changed();
        self.frame_all = true;
        let mut msg = format!("Imported {} nodes{}.", ids.len(), f.name.as_deref().map(|n| format!(" from “{n}”")).unwrap_or_default());
        if skipped > 0 {
            msg.push_str(&format!(" {skipped} parts were skipped: this game version doesn't have them."));
        }
        self.say(ctx, msg, skipped > 0);
    }

    /// A file's text: a factory or project in the portable format, or an
    /// older Factory Builder project.
    fn open_text(&mut self, ctx: &egui::Context, path: String, text: &str) {
        match Shared::parse(text) {
            Ok(Shared::Factory(f)) => {
                self.open_factory(ctx, f, Some(path.clone()));
                self.add_recent(&path);
            }
            Ok(Shared::Project(p)) => {
                self.open_project(ctx, p, Some(path.clone()));
                self.add_recent(&path);
            }
            Err(e) => {
                if serde_json::from_str::<crate::app::Project>(text).is_ok() {
                    self.reset_to(Some(text), Some(file_stem(&path)));
                    self.current_path = Some(path.clone());
                    self.add_recent(&path);
                    self.say(ctx, format!("Opened “{}”.", file_stem(&path)), false);
                } else {
                    self.say(ctx, format!("Couldn't open {}: {e}", file_name(&path)), true);
                }
            }
        }
    }

    // ------------------------------------------------------------ recent

    pub(crate) fn recent(&self) -> Vec<String> {
        self.status.as_ref().map(|s| s.settings.recent.clone()).unwrap_or_default()
    }

    fn set_recent(&mut self, list: Vec<String>) {
        if let Some(s) = self.status.as_mut() {
            s.settings.recent = list;
            let settings = s.settings.clone();
            self.backend.call("set_settings", json!({ "settings": settings }), Msg::Status);
        }
    }

    pub(crate) fn add_recent(&mut self, path: &str) {
        let mut list = self.recent();
        list.retain(|p| !p.eq_ignore_ascii_case(path));
        list.insert(0, path.to_string());
        list.truncate(RECENT_MAX);
        self.set_recent(list);
    }

    fn remove_recent(&mut self, path: &str) {
        let mut list = self.recent();
        list.retain(|p| !p.eq_ignore_ascii_case(path));
        self.set_recent(list);
    }

    pub(crate) fn clear_recent(&mut self) {
        self.set_recent(Vec::new());
    }

    // ------------------------------------------------------- dialogs + io

    pub(crate) fn pick_open(&self, purpose: OpenPurpose) {
        self.backend.call("pick_open_file", json!({}), move |r| Msg::PickedOpen { purpose, path: r });
    }

    pub(crate) fn read_file(&self, purpose: OpenPurpose, path: String) {
        let p = path.clone();
        self.backend.call("read_factory_file", json!({ "path": path }), move |r| Msg::FileRead { purpose, path: p, result: r });
    }

    pub(crate) fn pick_save(&self, purpose: SavePurpose) {
        let suggested = match purpose {
            SavePurpose::Factory | SavePurpose::Project => self.project_name.clone(),
            _ => None,
        };
        let suggested = suggested.unwrap_or_else(|| if self.on_master() { "Project".into() } else { self.doc_name(self.active) });
        self.backend.call("pick_save_file", json!({ "kind": purpose.kind(), "suggested": suggested }), move |r| Msg::PickedSave { purpose, path: r });
    }

    fn write(&self, purpose: SavePurpose, path: String, text: Option<String>, base64: Option<String>) {
        let p = path.clone();
        self.backend.call::<()>("write_file", json!({ "path": path, "kind": purpose.kind(), "text": text, "base64": base64 }), move |r| Msg::FileWritten {
            purpose,
            path: p,
            result: r,
        });
    }

    /// What File › Save writes: a factory file while there's one factory
    /// (unless it's already a project file), a project file otherwise.
    pub(crate) fn save_kind(&self) -> SavePurpose {
        if self.factory_count() > 1 || self.current_path.as_deref().is_some_and(is_project_path) {
            SavePurpose::Project
        } else {
            SavePurpose::Factory
        }
    }

    /// File › Save: to the open file, or ask where. A one-factory file that
    /// has become a project asks for a new (.fbproject) file.
    pub(crate) fn save(&mut self) {
        let kind = self.save_kind();
        match self.current_path.clone() {
            Some(path) if is_project_path(&path) == (kind == SavePurpose::Project) => self.write_doc(kind, path),
            _ => self.pick_save(kind),
        }
    }

    /// Saves straight to `path` (tests; the menu goes through the dialog).
    #[cfg(test)]
    pub(crate) fn save_to_path(&mut self, path: String) {
        let kind = if is_project_path(&path) { SavePurpose::Project } else { SavePurpose::Factory };
        self.write_doc(kind, path);
    }

    fn write_doc(&mut self, kind: SavePurpose, path: String) {
        let text = if kind == SavePurpose::Project {
            let mut p = self.project_file();
            p.name = Some(file_stem(&path));
            p.to_json(true)
        } else {
            let id = self.factory_ids().first().copied().unwrap_or(self.active);
            let Some(mut f) = self.factory_file(id) else { return };
            f.name = Some(file_stem(&path));
            f.to_json(true)
        };
        self.write(kind, path, Some(text), None);
    }

    pub(crate) fn copy_share_code(&mut self, ctx: &egui::Context) {
        let (code, what) = if self.on_master() {
            (self.project_file().to_share_code(), "the whole project")
        } else {
            (self.to_portable().to_share_code(), "this factory")
        };
        let n = code.len();
        ctx.copy_text(code);
        self.say(ctx, format!("Share code for {what} copied ({n} characters). Paste it anywhere; others open it with File › Open share code."), false);
    }

    pub(crate) fn summary_text(&mut self) -> String {
        if self.on_master() {
            return self.project_summary_text();
        }
        let game = &self.user.game;
        let s = &self.summary;
        let unit = |item: &str| if game.item_form(item).is_fluid() { "m³/min" } else { "per min" };
        let n = crate::fmt::fmt_rate;
        let mut out = String::new();
        out.push_str(&format!("{}\n", self.doc_name(self.active)));
        out.push_str(&format!("Power: {} MW\n", crate::fmt::fmt_num(s.power_mw)));
        if let Some(mw) = s.generation {
            out.push_str(&format!("Power production: {} MW\n", crate::fmt::fmt_num(mw)));
        }
        if !s.outputs.is_empty() {
            out.push_str("\nProducts\n");
            for (item, got, want) in &s.outputs {
                out.push_str(&format!("  {}: {} / {} {}\n", game.item_name(item), n(*got), n(*want), unit(item)));
            }
        }
        if !s.buildings.is_empty() {
            out.push_str("\nBuildings\n");
            for (b, (whole, _)) in &s.buildings {
                if let Some(bd) = game.buildings.get(b) {
                    out.push_str(&format!("  {whole} × {}\n", bd.name));
                }
            }
        }
        for (title, m) in [("Raw resources", &s.resources), ("Inputs", &s.inputs), ("Byproducts", &s.sunk)] {
            if !m.is_empty() {
                out.push_str(&format!("\n{title}\n"));
                for (item, rate) in m {
                    out.push_str(&format!("  {}: {} {}\n", game.item_name(item), n(*rate), unit(item)));
                }
            }
        }
        out.push_str("\nMade with Factory Builder\n");
        out
    }

    // ------------------------------------------------------------ messages

    pub(crate) fn handle_file_msg(&mut self, ctx: &egui::Context, msg: Msg) {
        match msg {
            Msg::PickedOpen { purpose, path } => match path {
                Ok(Some(p)) => self.read_file(purpose, p),
                Ok(None) => {}
                Err(e) => self.say(ctx, e, true),
            },
            Msg::FileRead { purpose, path, result } => match result {
                Ok(text) => match purpose {
                    OpenPurpose::Open => self.open_text(ctx, path, &text),
                    OpenPurpose::Import | OpenPurpose::AddFactory => match Shared::parse(&text) {
                        Ok(Shared::Factory(f)) if purpose == OpenPurpose::Import => self.import_factory(ctx, f),
                        Ok(Shared::Factory(f)) => self.add_factory_file(ctx, &f),
                        Ok(Shared::Project(_)) => self.say(ctx, format!("{} is a whole project. Open it with File › Open.", file_name(&path)), true),
                        Err(e) => self.say(ctx, format!("Couldn't import {}: {e}", file_name(&path)), true),
                    },
                },
                Err(e) => {
                    if self.recent().iter().any(|r| r.eq_ignore_ascii_case(&path)) && !e.contains("chosen") {
                        self.remove_recent(&path);
                    }
                    self.say(ctx, e, true);
                }
            },
            Msg::PickedSave { purpose, path } => match path {
                Ok(Some(p)) => match purpose {
                    SavePurpose::Factory | SavePurpose::Project => self.write_doc(purpose, p),
                    SavePurpose::Json => self.write(purpose, p, Some(self.to_portable().to_json(true)), None),
                    SavePurpose::Text => {
                        let text = self.summary_text();
                        self.write(purpose, p, Some(text), None);
                    }
                    SavePurpose::Png => self.start_image_export(ctx, p),
                },
                Ok(None) => self.after_save = None,
                Err(e) => {
                    self.after_save = None;
                    self.say(ctx, e, true);
                }
            },
            Msg::FileWritten { purpose, path, result } => match result {
                Ok(()) => {
                    if matches!(purpose, SavePurpose::Factory | SavePurpose::Project) {
                        self.current_path = Some(path.clone());
                        self.project_name = Some(file_stem(&path));
                        if purpose == SavePurpose::Factory {
                            // The file's name is the factory's.
                            if let Some(id) = self.factory_ids().first().copied() {
                                if let Some(d) = self.doc_mut(id) {
                                    d.name = file_stem(&path);
                                }
                            }
                        }
                        self.mark_saved();
                        self.add_recent(&path);
                        self.say(ctx, format!("Saved {}.", file_name(&path)), false);
                        if let Some(p) = self.after_save.take() {
                            self.run_pending(p);
                        }
                    } else {
                        self.say(ctx, format!("Exported {}.", file_name(&path)), false);
                    }
                }
                Err(e) => {
                    self.after_save = None;
                    self.say(ctx, e, true);
                }
            },
            Msg::StartupFile(r) => {
                self.startup_checked = true;
                match r {
                    Ok(Some(path)) => {
                        self.tried_autoload = true;
                        self.read_file(OpenPurpose::Open, path);
                    }
                    _ => self.try_autoload(),
                }
            }
            _ => {}
        }
    }

    // ------------------------------------------------------- image export

    pub(crate) fn start_image_export_for_selftest(&mut self, ctx: &egui::Context) {
        self.start_image_export(ctx, crate::app::SELFTEST_EXPORT.into());
    }

    fn start_image_export(&mut self, ctx: &egui::Context, path: String) {
        if self.state.graph.nodes.is_empty() {
            self.say(ctx, "There's nothing to export yet.", true);
            return;
        }
        let z = self.state.pan_zoom.zoom.max(0.01);
        let mut bounds = Rect::NOTHING;
        for (id, p) in self.state.node_positions.iter() {
            let h = self.node_heights.get(&id).copied().unwrap_or(260.0);
            bounds = bounds.union(Rect::from_min_size((p.to_vec2() / z).to_pos2(), vec2(NODE_WIDTH, h)));
        }
        let bounds = bounds.expand(40.0);
        let ppp = ctx.pixels_per_point();
        let scale = 1.0_f32.min(MAX_IMAGE_SIDE / (bounds.width() * ppp)).min(MAX_IMAGE_SIDE / (bounds.height() * ppp));
        let screen = ctx.screen_rect().size();
        let tiles = tile_origins(bounds, screen / scale);
        let w = (bounds.width() * scale * ppp).ceil() as u32;
        let h = (bounds.height() * scale * ppp).ceil() as u32;
        let bg = crate::theme::BACKDROP;
        let image = image::RgbaImage::from_pixel(w, h, image::Rgba([bg.r(), bg.g(), bg.b(), 255]));
        let saved = (self.state.pan_zoom.zoom, self.state.pan_zoom.pan, std::mem::take(&mut self.state.selected_nodes));
        self.set_zoom(ctx, scale);
        self.context_menu = None;
        self.finder = None;
        self.export = Some(ImageExport {
            path,
            tiles,
            current: 0,
            frames_left: 2,
            bounds,
            scale,
            ppp,
            image,
            saved_zoom: saved.0,
            saved_pan: saved.1,
            saved_selection: saved.2,
        });
        ctx.request_repaint();
    }

    /// Changes zoom keeping node positions consistent (they're stored zoomed).
    pub(crate) fn set_zoom(&mut self, ctx: &egui::Context, zoom: f32) {
        let k = zoom / self.state.pan_zoom.zoom.max(0.01);
        for (_, p) in self.state.node_positions.iter_mut() {
            *p = (p.to_vec2() * k).to_pos2();
        }
        self.state.pan_zoom.zoom(ctx.screen_rect(), &ctx.style(), k);
    }

    /// One frame of an image export. Returns true while it's running.
    pub(crate) fn export_frame(&mut self, ctx: &egui::Context) -> bool {
        let Some(ex) = self.export.as_mut() else { return false };
        if ex.frames_left == 0 {
            match capture_canvas() {
                Some(tile) => {
                    let origin = ex.tiles[ex.current];
                    let dx = ((origin.x - ex.bounds.min.x) * ex.scale * ex.ppp).round() as i64;
                    let dy = ((origin.y - ex.bounds.min.y) * ex.scale * ex.ppp).round() as i64;
                    image::imageops::replace(&mut ex.image, &tile, dx, dy);
                }
                None => {
                    self.finish_image_export(ctx, Err("Image export isn't available here.".into()));
                    return false;
                }
            }
            ex.current += 1;
            if ex.current >= ex.tiles.len() {
                let mut png = Vec::new();
                let result = image::codecs::png::PngEncoder::new(&mut png)
                    .write_image(ex.image.as_raw(), ex.image.width(), ex.image.height(), image::ExtendedColorType::Rgba8)
                    .map(|_| png)
                    .map_err(|e| e.to_string());
                self.finish_image_export(ctx, result);
                return false;
            }
            ex.frames_left = 2;
        } else {
            ex.frames_left -= 1;
        }
        let ex = self.export.as_ref().unwrap();
        self.state.pan_zoom.pan = -(ex.tiles[ex.current].to_vec2() * ex.scale);
        ctx.request_repaint();
        true
    }

    fn finish_image_export(&mut self, ctx: &egui::Context, png: Result<Vec<u8>, String>) {
        let Some(ex) = self.export.take() else { return };
        self.set_zoom(ctx, ex.saved_zoom);
        self.state.pan_zoom.pan = ex.saved_pan;
        self.state.selected_nodes = ex.saved_selection;
        match png {
            Ok(bytes) => {
                use base64::Engine;
                let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                if ex.path == crate::app::SELFTEST_EXPORT {
                    self.backend.call::<()>("selftest_screenshot", json!({ "dataUrl": format!("data:image/png;base64,{b64}"), "name": "export_full" }), |_| Msg::RoomEvents(Ok(Vec::new())));
                } else {
                    self.write(SavePurpose::Png, ex.path, None, Some(b64));
                }
            }
            Err(e) => self.say(ctx, e, true),
        }
    }
}

use image::ImageEncoder;

/// The last frame drawn on the canvas (its WebGL buffer is preserved, see
/// lib.rs), as RGBA.
fn capture_canvas() -> Option<image::RgbaImage> {
    #[cfg(target_arch = "wasm32")]
    {
        use base64::Engine;
        use wasm_bindgen::JsCast;
        let canvas = web_sys::window()?.document()?.get_element_by_id("app")?.dyn_into::<web_sys::HtmlCanvasElement>().ok()?;
        let url = canvas.to_data_url().ok()?;
        let b64 = url.split_once(',')?.1;
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
        Some(image::load_from_memory_with_format(&bytes, image::ImageFormat::Png).ok()?.to_rgba8())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        None
    }
}

impl Dialog {
    pub fn share_code(mode: crate::dialogs::ShareMode) -> Self {
        Dialog::ShareCode { code: String::new(), error: None, mode }
    }
}

impl Pending {
    pub fn open_factory(f: FactoryFile) -> Self {
        Pending::OpenFactory(Box::new(f))
    }
}
