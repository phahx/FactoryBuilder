//! The Live map add-on (Settings › Add-ons, off by default). While it's
//! off, nothing here runs or shows. On the Map tab:
//! - players: from the save it follows (a save keeps where each player
//!   stood, not their name), or live from the Ficsit Remote Monitoring mod;
//! - a dedicated server: its newest save, downloaded when it changes
//!   (src-tauri live.rs does the talking, with a pinned certificate);
//! - the Live map dialog to set those up.

use crate::app::FactoryApp;
use crate::backend::Msg;
use crate::map_draw::View;
use crate::theme::*;
use crate::widgets;
use egui::{pos2, vec2, Color32, Pos2, RichText, Stroke};
use fb_model::world::{Carrier, Machine, PlayerSpot, World};
use fb_model::{GameData, LiveBuildings, LiveMachine, LivePower, LivePowered, LiveSettings, LiveStructure, ServerCheck};
use std::collections::{BTreeMap, HashMap};
use serde_json::json;

/// Marker radius, screen pixels.
const MARKER: f32 = 7.0;
/// How often to ask the mod where players are. Its getPlayer answer is
/// small (1.3 KB for one player) and quick (median 37 ms on a local
/// dedicated server, measured), so four times a second.
const PLAYERS_SECONDS: f64 = 0.25;
/// Markers glide to each new position over about this long (time
/// constant), instead of jumping every poll.
const PLAYERS_EASE: f64 = 0.12;
/// A player who moved further than this (cm) between polls jumped
/// (respawned, teleported): no gliding.
const PLAYERS_JUMP_CM: f32 = 20_000.0;
/// Live players older than this aren't shown (the mod stopped answering).
const PLAYERS_STALE: f64 = 10.0;
/// How often to ask a dedicated server whether it has a newer save.
pub const SERVER_SECONDS: f64 = 30.0;
/// The Map tab's session list entry for the dedicated server.
pub const SERVER_SESSION: &str = "Dedicated server";
/// How often to ask the mod what's built (its docs warn that big requests
/// such as getFactory can make a server hitch).
const BUILDINGS_SECONDS: f64 = 15.0;
/// How often to ask the mod about power, poles, storage and the like
/// (smaller answers, so asked more often).
const POWER_SECONDS: f64 = 3.0;
/// How far from a power line's end its pole or building can be, cm (a
/// building's connector sits off its middle).
const LINE_END_CM: f32 = 2000.0;
/// A live building of the same class this close (cm) is the save's.
const SAME_CM: f32 = 100.0;

#[derive(Default)]
pub struct LiveState {
    /// The last server check, and one in progress.
    pub check: Option<ServerCheck>,
    pub checking: bool,
    pub error: Option<String>,
    /// The token being typed (sent once, then cleared).
    pub token: String,
    /// Live players from the mod, when they arrived, and the last problem.
    pub players: Vec<PlayerSpot>,
    pub players_at: Option<f64>,
    pub players_busy: bool,
    pub players_error: Option<String>,
    next_players: f64,
    /// Where the markers are drawn now (gliding towards `players`).
    shown: Vec<PlayerSpot>,
    shown_at: f64,
    /// Following the dedicated server.
    pub server_busy: bool,
    pub next_server: f64,
    /// Live buildings: the save they're laid over, what the mod reported,
    /// and per machine of the map's world (by id), what it's doing now.
    pub base: Option<World>,
    pub buildings: Option<LiveBuildings>,
    pub buildings_busy: bool,
    next_buildings: f64,
    pub status: Vec<Option<LiveMachine>>,
    /// Machines from this index on were built since the save.
    pub new_from: usize,
    /// Built and taken down since the save.
    pub changes: (usize, usize),
    /// Power, poles, storage and the like from the mod, and per structure
    /// of the map's world (by index), what the mod says about it now.
    pub power: Option<LivePower>,
    pub power_busy: bool,
    next_power: f64,
    pub structure_status: Vec<Option<LiveStructure>>,
}

impl FactoryApp {
    /// Whether the Live map add-on is on.
    pub(crate) fn live_on(&self) -> bool {
        self.status.as_ref().is_some_and(|s| s.settings.live.enabled)
    }

    pub(crate) fn live_settings(&self) -> LiveSettings {
        self.status.as_ref().map(|s| s.settings.live.clone()).unwrap_or_default()
    }

    /// Whether a dedicated server is set up to follow.
    pub(crate) fn live_server(&self) -> bool {
        let l = self.live_settings();
        l.enabled && !l.server_host.trim().is_empty()
    }

    /// Players to show on the map: live ones from the mod while it
    /// answers, otherwise where the save left them.
    pub(crate) fn live_players(&self) -> (Vec<PlayerSpot>, bool) {
        if !self.live_on() {
            return (Vec::new(), false);
        }
        let now = self.backend_ctx().input(|i| i.time);
        if self.live_settings().frm && self.live.players_at.is_some_and(|t| now - t < PLAYERS_STALE) {
            let shown = if self.live.shown.len() == self.live.players.len() { &self.live.shown } else { &self.live.players };
            return (shown.clone(), true);
        }
        (self.map.world.as_ref().map(|w| w.players.clone()).unwrap_or_default(), false)
    }

    /// Map tab: asks the mod where players are, about once a second.
    pub(crate) fn live_poll(&mut self, ctx: &egui::Context) {
        if !self.live_on() || !self.live_settings().frm {
            // Back to the save as it is.
            if let Some(base) = self.live.base.take() {
                self.live.status.clear();
                self.live.buildings = None;
                self.live.power = None;
                self.live.structure_status.clear();
                self.live.changes = (0, 0);
                self.map.selected.clear();
                self.map.set_world(base);
            }
            return;
        }
        let now = ctx.input(|i| i.time);
        if !self.live.buildings_busy && now >= self.live.next_buildings && (self.live.base.is_some() || self.map.world.is_some()) {
            if self.live.base.is_none() {
                self.live.base = self.map.world.clone();
            }
            self.live.buildings_busy = true;
            self.live.next_buildings = now + BUILDINGS_SECONDS;
            self.backend.call("live_buildings", json!({}), Msg::LiveBuildings);
        }
        if !self.live.power_busy && now >= self.live.next_power && (self.live.base.is_some() || self.map.world.is_some()) {
            if self.live.base.is_none() {
                self.live.base = self.map.world.clone();
            }
            self.live.power_busy = true;
            self.live.next_power = now + POWER_SECONDS;
            self.backend.call("live_power", json!({}), Msg::LivePower);
        }
        if !self.live.players_busy && now >= self.live.next_players {
            self.live.players_busy = true;
            self.live.next_players = now + PLAYERS_SECONDS;
            self.backend.call("live_players", json!({}), Msg::LivePlayers);
        }
        if self.ease_players(now) {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(PLAYERS_SECONDS));
        }
    }

    /// Moves the drawn markers towards where the mod last said players are.
    /// Returns whether they're still moving.
    fn ease_players(&mut self, now: f64) -> bool {
        let dt = (now - self.live.shown_at).clamp(0.0, 0.5);
        self.live.shown_at = now;
        let target = &self.live.players;
        let same = self.live.shown.len() == target.len() && self.live.shown.iter().zip(target).all(|(a, b)| a.name == b.name);
        if !same {
            self.live.shown = target.clone();
            return false;
        }
        let k = (1.0 - (-dt / PLAYERS_EASE).exp()) as f32;
        let mut moving = false;
        for (s, t) in self.live.shown.iter_mut().zip(target) {
            let far = (t.x - s.x).hypot(t.y - s.y);
            if far > PLAYERS_JUMP_CM {
                *s = t.clone();
                continue;
            }
            s.x += (t.x - s.x) * k;
            s.y += (t.y - s.y) * k;
            s.z += (t.z - s.z) * k;
            let turn = (t.yaw - s.yaw + 540.0).rem_euclid(360.0) - 180.0;
            s.yaw = (s.yaw + turn * k).rem_euclid(360.0);
            s.online = t.online;
            if far > 5.0 || turn.abs() > 0.5 {
                moving = true;
            }
        }
        moving
    }

    /// Map tab, following the dedicated server: asks for its newest save.
    pub(crate) fn live_server_follow(&mut self, now: f64) {
        if self.live.server_busy || now < self.live.next_server {
            return;
        }
        self.live.server_busy = true;
        self.live.next_server = now + SERVER_SECONDS;
        self.backend.call("live_server_sync", json!({}), Msg::LiveServerSave);
    }

    /// Saves the add-on's settings, then (when asked) checks the server.
    pub(crate) fn live_save(&mut self, l: LiveSettings, then_check: bool) {
        let Some(mut s) = self.status.as_ref().map(|s| s.settings.clone()) else { return };
        s.live = l;
        if let Some(st) = self.status.as_mut() {
            st.settings = s.clone();
        }
        self.backend.call("set_settings", json!({ "settings": s }), move |r| Msg::LiveSaved { result: r, then_check });
    }

    pub(crate) fn on_live_msg(&mut self, msg: Msg) {
        match msg {
            Msg::LiveSaved { result, then_check } => {
                if let Ok(s) = &result {
                    self.status = Some(s.clone());
                }
                if then_check {
                    self.live.checking = true;
                    self.backend.call("live_server_check", json!({}), Msg::LiveCheck);
                }
            }
            Msg::LiveCheck(r) => {
                self.live.checking = false;
                match r {
                    Ok(c) => {
                        self.live.error = None;
                        self.live.check = Some(c);
                    }
                    Err(e) => {
                        self.live.check = None;
                        self.live.error = Some(e);
                    }
                }
            }
            Msg::LiveTrusted(r) => match r {
                Ok(s) => {
                    self.status = Some(s);
                    self.live.checking = true;
                    self.backend.call("live_server_check", json!({}), Msg::LiveCheck);
                }
                Err(e) => self.live.error = Some(e),
            },
            Msg::LiveToken(r) => {
                if let Err(e) = r {
                    self.live.error = Some(e);
                } else {
                    self.live.checking = true;
                    self.backend.call("live_server_check", json!({}), Msg::LiveCheck);
                }
            }
            Msg::LivePlayers(r) => {
                self.live.players_busy = false;
                match r {
                    Ok(p) => {
                        self.live.players = p;
                        self.live.players_at = Some(self.backend_ctx().input(|i| i.time));
                        self.live.players_error = None;
                    }
                    Err(e) => self.live.players_error = Some(e),
                }
            }
            Msg::LiveBuildings(r) => {
                self.live.buildings_busy = false;
                match r {
                    Ok(b) => {
                        self.live.players_error = b.errors.first().cloned();
                        self.live.buildings = Some(b);
                        self.live_apply();
                    }
                    Err(e) => self.live.players_error = Some(e),
                }
            }
            Msg::LivePower(r) => {
                self.live.power_busy = false;
                match r {
                    Ok(p) => {
                        if let Some(e) = p.errors.first() {
                            self.live.players_error = Some(e.clone());
                        }
                        self.live.power = Some(p);
                        self.live_apply();
                    }
                    Err(e) => self.live.players_error = Some(e),
                }
            }
            Msg::LiveServerSave(r) => {
                self.live.server_busy = false;
                if self.map.session.as_deref() != Some(SERVER_SESSION) {
                    return;
                }
                match r {
                    Ok(Some(save)) => {
                        let current = self.map.world.as_ref().map(|w| (w.save.path.clone(), w.save.saved_unix, w.save.name.clone()));
                        if current != Some((save.path.clone(), save.saved_unix, save.name.clone())) && self.map.loading.is_none() {
                            self.load_world(save.path);
                        }
                    }
                    Ok(None) => self.map.world_error = Some("The server has no save of the session it's running yet.".into()),
                    Err(e) => self.map.world_error = Some(e),
                }
            }
            _ => {}
        }
    }
}

impl FactoryApp {
    /// A save was read for the Map tab: with live buildings on, they're
    /// laid over it.
    pub(crate) fn live_world_loaded(&mut self, w: World) {
        if self.live_on() && self.live_settings().frm {
            self.live.base = Some(w);
            if self.live.buildings.is_some() || self.live.power.is_some() {
                self.live_apply();
                return;
            }
            let w = self.live.base.clone().unwrap_or_default();
            self.map.set_world(w);
        } else {
            self.map.set_world(w);
        }
    }

    /// The map's world: the save with what the mod says is built now.
    fn live_apply(&mut self) {
        let Some(base) = &self.live.base else { return };
        if self.live.buildings.is_none() && self.live.power.is_none() {
            return;
        }
        let none = LiveBuildings::default();
        let game = self.user.game.clone();
        let m = merge(base, self.live.buildings.as_ref().unwrap_or(&none), self.live.power.as_ref(), &game);
        let before = self.map.world.as_ref().map(|w| w.machines.len());
        if before != Some(m.world.machines.len()) || self.live.changes != (m.added, m.removed) {
            self.map.selected.clear();
        }
        self.live.status = m.status;
        self.live.structure_status = m.structure_status;
        self.live.new_from = m.new_from;
        self.live.changes = (m.added, m.removed);
        self.map.set_world(m.world);
    }
}

/// The save with live buildings laid over it.
pub struct Merged {
    pub world: World,
    /// Per machine (by id): what it's doing now.
    pub status: Vec<Option<LiveMachine>>,
    /// Machines from this id on weren't in the save.
    pub new_from: usize,
    /// Per structure (by index): what the mod says about it now.
    pub structure_status: Vec<Option<LiveStructure>>,
    /// Buildings built and taken down since the save (machines and the
    /// smaller buildings the mod reports).
    pub added: usize,
    pub removed: usize,
}

/// Cells of `SAME_CM`, for finding what's near.
fn cell(x: f32, y: f32) -> (i64, i64) {
    ((x / SAME_CM).floor() as i64, (y / SAME_CM).floor() as i64)
}

fn near(a: [f32; 2], b: [f32; 2]) -> bool {
    (a[0] - b[0]).hypot(a[1] - b[1]) <= SAME_CM
}

/// Lays what's built now over the save: a machine of the same class within
/// a metre is the save's (kept, with its links, and its recipe as it is
/// now); the save's with none was taken down; the rest are new. Belts and
/// pipes the same, by their ends. A part the mod didn't report is left as
/// the save has it. Then power and the smaller buildings (`merge_power`).
pub fn merge(base: &World, live: &LiveBuildings, power: Option<&LivePower>, game: &GameData) -> Merged {
    let mut w = base.clone();
    let mut out = Merged { world: World::default(), status: vec![None; base.machines.len()], new_from: base.machines.len(), structure_status: vec![None; base.structures.len()], added: 0, removed: 0 };
    if let Some(lm) = &live.machines {
        let mut grid: HashMap<(&str, (i64, i64)), Vec<usize>> = HashMap::new();
        for (j, m) in lm.iter().enumerate() {
            grid.entry((m.class.as_str(), cell(m.x, m.y))).or_default().push(j);
        }
        let mut used = vec![false; lm.len()];
        let mut new_id: Vec<Option<u32>> = vec![None; base.machines.len()];
        let mut machines: Vec<Machine> = Vec::new();
        let mut status = Vec::new();
        // A generator's recipe is its fuel; the mod says what it holds.
        let recipe_of = |l: &LiveMachine| {
            l.recipe.clone().filter(|r| game.recipes.contains_key(r)).or_else(|| {
                let g = format!("{}{}__{}", fb_model::GENERATOR_PREFIX, l.class, l.fuel.as_deref()?);
                game.recipes.contains_key(&g).then_some(g)
            })
        };
        for m in &base.machines {
            let (cx, cy) = cell(m.x, m.y);
            let found = (-1..=1)
                .flat_map(|dx| (-1..=1).map(move |dy| (cx + dx, cy + dy)))
                .flat_map(|c| grid.get(&(m.class.as_str(), c)).into_iter().flatten().copied())
                .find(|j| !used[*j] && near([lm[*j].x, lm[*j].y], [m.x, m.y]));
            let Some(j) = found else { continue };
            used[j] = true;
            let mut kept = m.clone();
            kept.id = machines.len() as u32;
            if let Some(r) = recipe_of(&lm[j]) {
                kept.recipe = Some(r);
            }
            if let Some(c) = lm[j].clock.filter(|c| *c > 0.0) {
                kept.clock = c;
            }
            new_id[m.id as usize] = Some(kept.id);
            machines.push(kept);
            status.push(Some(lm[j].clone()));
        }
        out.removed = base.machines.len() - machines.len();
        out.new_from = machines.len();
        for (j, l) in lm.iter().enumerate().filter(|(j, _)| !used[*j]) {
            let _ = j;
            machines.push(Machine { id: machines.len() as u32, class: l.class.clone(), recipe: recipe_of(l), clock: l.clock.filter(|c| *c > 0.0).unwrap_or(1.0), boost: 1.0, x: l.x, y: l.y, z: l.z, yaw: l.yaw, node: None });
            status.push(Some(l.clone()));
        }
        out.added = machines.len() - out.new_from;
        let remap = |id: u32| new_id.get(id as usize).copied().flatten();
        w.links = base.links.iter().filter_map(|l| Some(fb_model::world::Link { from: remap(l.from)?, to: remap(l.to)?, item: l.item.clone() })).collect();
        w.store_links = base.store_links.iter().filter_map(|l| Some(fb_model::world::StoreLink { machine: remap(l.machine)?, ..l.clone() })).collect();
        w.machine_circuit = base.machine_circuit.iter().filter_map(|(k, v)| Some((remap(*k)?, *v))).collect::<BTreeMap<_, _>>();
        w.machine_power = base.machine_power.iter().filter_map(|(k, v)| Some((remap(*k)?, *v))).collect::<BTreeMap<_, _>>();
        w.machines = machines;
        out.status = status;
    }
    if let Some(lc) = &live.carriers {
        let mergeable = |c: &Carrier| if c.fluid { true } else { c.class.starts_with("Build_ConveyorBelt") };
        let ends = |p: &[[f32; 2]]| (p[0], p[p.len() - 1]);
        let mut grid: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
        for (j, c) in lc.iter().enumerate() {
            let (a, _) = ends(&c.points);
            grid.entry(cell(a[0], a[1])).or_default().push(j);
        }
        let mut used = vec![false; lc.len()];
        let mut carriers = Vec::new();
        for c in &base.carriers {
            if !mergeable(c) || c.points.len() < 2 {
                carriers.push(c.clone());
                continue;
            }
            let (a, b) = ends(&c.points);
            let mut found = None;
            for (start, end) in [(a, b), (b, a)] {
                let (cx, cy) = cell(start[0], start[1]);
                found = (-1..=1)
                    .flat_map(|dx| (-1..=1).map(move |dy| (cx + dx, cy + dy)))
                    .flat_map(|k| grid.get(&k).into_iter().flatten().copied())
                    .find(|j| !used[*j] && lc[*j].class == c.class && near(lc[*j].points[0], start) && near(*lc[*j].points.last().unwrap(), end));
                if found.is_some() {
                    break;
                }
            }
            if let Some(j) = found {
                used[j] = true;
                carriers.push(c.clone());
            }
        }
        for (j, c) in lc.iter().enumerate() {
            if !used[j] {
                carriers.push(Carrier { class: c.class.clone(), fluid: c.fluid, points: c.points.clone(), yaw: 0.0 });
            }
        }
        w.carriers = carriers;
    }
    if let Some(p) = power {
        let (status, added, removed) = merge_power(&mut w, &out.status, p);
        out.structure_status = status;
        out.added += added;
        out.removed += removed;
    }
    out.world = w;
    out
}

/// Finds, near (x, y), an item of `class` in a grid of `SAME_CM` cells.
fn find_near<'a, T>(grid: &HashMap<(&str, (i64, i64)), Vec<usize>>, items: &'a [T], pos: impl Fn(&T) -> [f32; 2], class: &str, x: f32, y: f32, used: Option<&[bool]>) -> Option<usize> {
    let (cx, cy) = cell(x, y);
    (-1..=1)
        .flat_map(|dx| (-1..=1).map(move |dy| (cx + dx, cy + dy)))
        .flat_map(|c| grid.get(&(class, c)).into_iter().flatten().copied())
        .find(|j| used.is_none_or(|u| !u[*j]) && near(pos(&items[*j]), [x, y]))
}

/// Power and the smaller buildings now, laid over `w` (whose machines are
/// already merged; `status` per machine):
/// - each family the mod answered for replaces the save's buildings of its
///   classes (same class within a metre is the save's; the rest are new);
/// - circuits are the game's own, live; every building's circuit and power
///   come from the mod (by its id, else class and place);
/// - power lines are the mod's, each on the circuit of the pole or building
///   at an end. (Which line splits its circuit, and what crosses it, is
///   worked out from saves only, so live lines don't say.)
///
/// Returns per structure what the mod says about it, and how many of the
/// smaller buildings were built and taken down.
fn merge_power(w: &mut World, status: &[Option<LiveMachine>], p: &LivePower) -> (Vec<Option<LiveStructure>>, usize, usize) {
    use fb_model::world::{Circuit, PowerLine, PowerSource, PowerUse, Structure};
    let (mut added, mut removed) = (0, 0);

    // The smaller buildings, family by family.
    let family_of = |class: &str| p.families.iter().position(|f| f.prefixes.iter().any(|x| class.starts_with(x.as_str())));
    let grids: Vec<HashMap<(&str, (i64, i64)), Vec<usize>>> = p
        .families
        .iter()
        .map(|f| {
            let mut g: HashMap<(&str, (i64, i64)), Vec<usize>> = HashMap::new();
            for (j, s) in f.items.iter().enumerate() {
                g.entry((s.class.as_str(), cell(s.x, s.y))).or_default().push(j);
            }
            g
        })
        .collect();
    let mut used: Vec<Vec<bool>> = p.families.iter().map(|f| vec![false; f.items.len()]).collect();
    let mut structures = Vec::new();
    let mut st_status = Vec::new();
    // The save's structure index -> the merged one (taken down: none).
    let mut new_index: Vec<Option<u32>> = vec![None; w.structures.len()];
    for (k, s) in w.structures.iter().enumerate() {
        let Some(f) = family_of(&s.class) else {
            new_index[k] = Some(structures.len() as u32);
            structures.push(s.clone());
            st_status.push(None);
            continue;
        };
        let items = &p.families[f].items;
        match find_near(&grids[f], items, |i| [i.x, i.y], &s.class, s.x, s.y, Some(&used[f])) {
            Some(j) => {
                used[f][j] = true;
                new_index[k] = Some(structures.len() as u32);
                structures.push(s.clone());
                st_status.push(Some(items[j].clone()));
            }
            None => removed += 1,
        }
    }
    w.store_links = std::mem::take(&mut w.store_links)
        .into_iter()
        .filter_map(|l| Some(fb_model::world::StoreLink { store: new_index.get(l.store as usize).copied().flatten()?, ..l }))
        .collect();
    for (f, fam) in p.families.iter().enumerate() {
        for (j, it) in fam.items.iter().enumerate().filter(|(j, _)| !used[f][*j]) {
            let _ = j;
            structures.push(Structure { class: it.class.clone(), x: it.x, y: it.y, yaw: it.yaw, circuit: None, power: None });
            st_status.push(Some(it.clone()));
            added += 1;
        }
    }
    w.structures = structures;

    // Circuits, and what's on them.
    let (Some(circuits), Some(powered)) = (&p.circuits, &p.powered) else { return (st_status, added, removed) };
    let mut index: HashMap<i64, u32> = HashMap::new();
    for (i, c) in circuits.iter().enumerate() {
        for id in &c.ids {
            index.insert(*id, i as u32);
        }
    }
    let of = |id: Option<i64>| id.and_then(|i| index.get(&i).copied());
    w.circuits = circuits
        .iter()
        .map(|c| Circuit {
            consumption: c.consumption,
            max_consumption: c.max_consumption,
            capacity: c.capacity,
            production: c.production,
            fuse: c.fuse,
            stored_mwh: c.stored_mwh,
            battery_mwh: c.battery_mwh,
            ..Default::default()
        })
        .collect();
    for q in powered {
        let Some(c) = of(q.circuit) else { continue };
        let cir = &mut w.circuits[c as usize];
        if q.max > 0.0 {
            cir.consumers += 1;
        }
        if q.class.starts_with("Build_PowerStorage") {
            cir.batteries += 1;
        }
        if q.class.starts_with("Build_Generator") {
            match cir.sources.iter_mut().find(|s| s.class == q.class) {
                Some(s) => {
                    s.count += 1;
                    s.mw += q.can_make;
                }
                None => cir.sources.push(PowerSource { class: q.class.clone(), count: 1, mw: q.can_make }),
            }
        }
    }
    for c in &mut w.circuits {
        c.sources.sort_by(|a, b| b.mw.total_cmp(&a.mw).then(a.class.cmp(&b.class)));
    }
    let by_id: HashMap<&str, &LivePowered> = powered.iter().map(|q| (q.id.as_str(), q)).collect();
    let mut grid: HashMap<(&str, (i64, i64)), Vec<usize>> = HashMap::new();
    for (j, q) in powered.iter().enumerate() {
        grid.entry((q.class.as_str(), cell(q.x, q.y))).or_default().push(j);
    }
    let find = |class: &str, x: f32, y: f32| find_near(&grid, powered, |q| [q.x, q.y], class, x, y, None).map(|j| &powered[j]);
    let use_of = |q: &LivePowered| PowerUse { now: q.now, max: q.max, makes: q.makes };
    w.machine_circuit.clear();
    w.machine_power.clear();
    for m in &w.machines {
        let by_mod_id = status.get(m.id as usize).and_then(|s| s.as_ref()).and_then(|s| s.id.as_deref()).and_then(|id| by_id.get(id).copied());
        let Some(q) = by_mod_id.or_else(|| find(&m.class, m.x, m.y)) else { continue };
        if let Some(c) = of(q.circuit) {
            w.machine_circuit.insert(m.id, c);
        }
        if q.max > 0.0 || q.makes > 0.0 || q.can_make > 0.0 {
            w.machine_power.insert(m.id, use_of(q));
        }
    }
    // The HUB's burners, as the mod reports them.
    let burners: Vec<&LivePowered> = powered.iter().filter(|q| q.class.starts_with(fb_model::HUB_BURNER_CLASS)).collect();
    if !burners.is_empty() {
        w.hub_burners = burners
            .iter()
            .map(|q| fb_model::world::HubBurner { x: q.x, y: q.y, circuit: of(q.circuit), makes: q.makes, fuel: q.fuel.as_ref().map(|f| f.0.clone()), fuel_amount: q.fuel.as_ref().map(|f| f.1) })
            .collect();
    }
    for (s, st) in w.structures.iter_mut().zip(&st_status) {
        let q = find(&s.class, s.x, s.y);
        s.circuit = of(st.as_ref().and_then(|l| l.circuit).or(q.and_then(|q| q.circuit)));
        s.power = q.map(use_of).filter(|u| u.max > 0.0 || u.makes > 0.0);
    }

    // Power lines: on the circuit of the pole or building at an end.
    if let Some(lines) = &p.lines {
        let big = |x: f32, y: f32| ((x / LINE_END_CM).floor() as i64, (y / LINE_END_CM).floor() as i64);
        let mut ends: HashMap<(i64, i64), Vec<([f32; 2], u32)>> = HashMap::new();
        for s in &w.structures {
            if let Some(c) = s.circuit {
                ends.entry(big(s.x, s.y)).or_default().push(([s.x, s.y], c));
            }
        }
        for q in powered {
            if let Some(c) = of(q.circuit) {
                ends.entry(big(q.x, q.y)).or_default().push(([q.x, q.y], c));
            }
        }
        let circuit_at = |e: [f32; 2]| -> Option<(f32, u32)> {
            let (cx, cy) = big(e[0], e[1]);
            (-1..=1)
                .flat_map(|dx| (-1..=1).map(move |dy| (cx + dx, cy + dy)))
                .flat_map(|k| ends.get(&k).into_iter().flatten())
                .map(|(p, c)| ((p[0] - e[0]).hypot(p[1] - e[1]), *c))
                .filter(|(d, _)| *d <= LINE_END_CM)
                .min_by(|a, b| a.0.total_cmp(&b.0))
        };
        w.power_lines = lines
            .iter()
            .map(|[a, b]| {
                let circuit = match (circuit_at(*a), circuit_at(*b)) {
                    (Some(x), Some(y)) => Some(if x.0 <= y.0 { x.1 } else { y.1 }),
                    (x, y) => x.or(y).map(|v| v.1),
                };
                PowerLine { a: *a, b: *b, circuit, sides: None }
            })
            .collect();
    }
    (st_status, added, removed)
}

/// An item list in a tooltip: icon, name and amount (fluids in m³).
fn item_lines(ui: &mut egui::Ui, assets: &mut crate::assets::Assets, game: &GameData, items: &[(String, f32)]) {
    for (item, n) in items.iter().take(12) {
        let amount = if game.item_form(item).is_fluid() { format!("{} m³", crate::fmt::fmt_num((*n as f64 * 10.0).round() / 10.0)) } else { crate::fmt::fmt_num(n.round() as f64) };
        widgets::tip_item(ui, assets, item, game.item_name(item), &amount);
    }
    if items.len() > 12 {
        widgets::tip_line(ui, RichText::new(format!("and {} more", items.len() - 12)).small().color(TEXT_DIM));
    }
}

/// A machine's input and output slots now, from the mod.
pub fn machine_stock(ui: &mut egui::Ui, assets: &mut crate::assets::Assets, game: &GameData, status: Option<&LiveMachine>) {
    let Some(s) = status else { return };
    for (label, items) in [("In its input slots", &s.inputs), ("In its output slots", &s.outputs)] {
        if !items.is_empty() {
            ui.add_space(4.0);
            widgets::tip_line(ui, RichText::new(label).small().color(TEXT_DIM));
            item_lines(ui, assets, game, items);
        }
    }
}

/// A storage container's or fluid tank's contents now, from the mod.
pub fn structure_lines(ui: &mut egui::Ui, assets: &mut crate::assets::Assets, game: &GameData, status: Option<&LiveStructure>) {
    let Some(s) = status else { return };
    if let Some((used, all)) = s.slots {
        widgets::tip_line(ui, RichText::new(format!("Now: {used} of {all} slots used")).color(ACCENT));
        item_lines(ui, assets, game, &s.items);
    }
    if let Some((fluid, content, capacity)) = &s.fluid {
        let text = format!("Now: {} of {} m³", crate::fmt::fmt_num((*content as f64 * 10.0).round() / 10.0), crate::fmt::fmt_num(capacity.round() as f64));
        match fluid {
            Some(f) if *content > 0.0 => widgets::tip_item(ui, assets, f, game.item_name(f), &text),
            _ => {
                widgets::tip_line(ui, RichText::new(format!("{text} (empty)")).color(ACCENT));
            }
        }
    }
}

/// A machine's tooltip lines from the mod: what it's doing now.
pub fn machine_lines(ui: &mut egui::Ui, status: Option<&LiveMachine>, new: bool) {
    let Some(s) = status else { return };
    let now = match (s.paused, s.producing, s.productivity) {
        (Some(true), _, _) => "Now: paused".to_string(),
        (_, Some(true), Some(p)) => format!("Now: producing, {}% productivity", crate::fmt::fmt_num(p as f64)),
        (_, Some(true), None) => "Now: producing".to_string(),
        (_, Some(false), _) => "Now: idle".to_string(),
        (_, None, Some(p)) => format!("Now: {}% productivity", crate::fmt::fmt_num(p as f64)),
        _ => return,
    };
    ui.add_space(4.0);
    widgets::tip_line(ui, RichText::new(now).color(ACCENT));
    if new {
        widgets::tip_line(ui, RichText::new("Built since the save (Ficsit Remote Monitoring).").small().color(TEXT_DIM));
    }
}

/// What a player's marker is called: their name, or "Player n".
pub fn label(players: &[PlayerSpot], i: usize) -> String {
    match players[i].name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => n.to_string(),
        None if players.len() == 1 => "Player".to_string(),
        None => format!("Player {}", i + 1),
    }
}

/// Draws the players (a dot with an arrow the way they face, and their
/// name; dimmed while offline). Returns the one under `pointer`.
pub fn draw_players(painter: &egui::Painter, view: &View, players: &[PlayerSpot], pointer: Option<Pos2>) -> Option<usize> {
    let mut hovered = None;
    for (i, p) in players.iter().enumerate() {
        let at = view.screen(p.x as f64, p.y as f64);
        if !view.rect.expand(40.0).contains(at) {
            continue;
        }
        let color = if p.online == Some(false) { TEXT_DIM } else { ACCENT };
        // Unreal yaw turns +X (east, right) towards +Y (south, down).
        let a = p.yaw.to_radians();
        let dir = vec2(a.cos(), a.sin());
        let side = vec2(-dir.y, dir.x);
        let tip = at + dir * (MARKER + 7.0);
        let shadow = Color32::from_black_alpha(200);
        painter.add(egui::Shape::convex_polygon(vec![tip, at + dir * (MARKER - 1.0) + side * 5.0, at + dir * (MARKER - 1.0) - side * 5.0], color, Stroke::new(1.5_f32, shadow)));
        painter.circle(at, MARKER, color, Stroke::new(2.0_f32, shadow));
        painter.circle_filled(at, 2.5, CONTENT);
        let name = label(players, i);
        let g = painter.layout_no_wrap(name, egui::FontId::proportional(12.0), TEXT);
        let chip = egui::Rect::from_center_size(at + vec2(0.0, MARKER + 14.0), g.size() + vec2(10.0, 4.0));
        painter.rect_filled(chip, 3.0, CONTENT.gamma_multiply(0.9));
        painter.galley(pos2(chip.left() + 5.0, chip.top() + 2.0), g, TEXT);
        if pointer.is_some_and(|q| q.distance(at) <= MARKER + 4.0 || chip.contains(q)) {
            hovered = Some(i);
        }
    }
    hovered
}

/// A player marker's tooltip.
pub fn player_tip(ui: &mut egui::Ui, players: &[PlayerSpot], i: usize, live: bool, saved: &str) {
    widgets::tooltip_title(ui, &label(players, i));
    if live {
        let state = match players[i].online {
            Some(false) => "Offline: where they logged out.",
            _ => "In the game now (Ficsit Remote Monitoring).",
        };
        widgets::tip_line(ui, RichText::new(state).color(TEXT));
    } else {
        widgets::tip_line(ui, RichText::new(format!("Where the save left them ({saved}).")).color(TEXT));
        widgets::tip_line(ui, RichText::new("Saves don't keep player names, only where each one stood.").small().color(TEXT_DIM));
    }
}

/// Settings › Add-ons.
pub fn settings_section(ui: &mut egui::Ui, assets: &mut crate::assets::Assets, draft: &mut fb_model::Settings) {
    widgets::divider(ui);
    ui.label(RichText::new("Add-ons").font(crate::theme::semibold(14.0)).color(ACCENT));
    widgets::checkbox(ui, assets, &mut draft.live.enabled, "Live map");
    widgets::caption(ui, "Shows players on the Map tab, and can follow a dedicated server and the Ficsit Remote Monitoring mod for live updates. Set those up with the Map tab's Live map button.");
}

/// The Live map dialog: the dedicated server and the mod.
pub fn dialog(app: &mut FactoryApp, ctx: &egui::Context, draft: &mut LiveSettings) -> bool {
    let check = app.live.check.clone();
    let checking = app.live.checking;
    let error = app.live.error.clone();
    let players_error = app.live.players_error.clone();
    let now = ctx.input(|i| i.time);
    let live_age = app.live.players_at.map(|t| now - t);
    let live_count = app.live.players.len();
    let saved = app.live_settings();
    let mut token = std::mem::take(&mut app.live.token);
    let (mut do_check, mut trust, mut save_token, mut forget_token, mut frm_changed) = (false, None, false, false, false);
    let Some(assets) = app.user.assets.as_mut() else { return false };
    let clicked = crate::dialogs::modal(
        ctx,
        "Live map",
        600.0,
        |ui| {
            widgets::caption(ui, "Players and what's built, kept up to date on the Map tab.");
            widgets::divider(ui);
            ui.label(RichText::new("Dedicated server").font(crate::theme::semibold(14.0)).color(ACCENT));
            widgets::caption(ui, "Follows the newest save of the session the server is running. Leave the address empty to follow saves on this PC.");
            ui.horizontal(|ui| {
                ui.label(RichText::new("Address").color(TEXT_DIM));
                ui.add(egui::TextEdit::singleline(&mut draft.server_host).hint_text("e.g. 192.168.1.20").desired_width(260.0).char_limit(253).margin(vec2(8.0, 6.0)));
                ui.label(RichText::new("Port").color(TEXT_DIM));
                let mut v = draft.server_port as f64;
                if widgets::num_field(ui, egui::Id::new("live_server_port"), &mut v, 80.0) {
                    draft.server_port = v.clamp(1.0, 65535.0) as u16;
                }
            });
            let token_set = check.as_ref().is_some_and(|c| c.token_set);
            ui.horizontal(|ui| {
                ui.label(RichText::new("API token").color(TEXT_DIM));
                if token_set {
                    ui.label(RichText::new("Saved, encrypted for your Windows account.").color(TEXT));
                    forget_token = widgets::button(ui, "Remove").clicked();
                } else {
                    ui.add(egui::TextEdit::singleline(&mut token).password(true).hint_text("paste it here").desired_width(300.0).char_limit(4096).margin(vec2(8.0, 6.0)));
                    save_token = widgets::button_enabled(ui, "Save token", !token.trim().is_empty()).clicked();
                }
            });
            widgets::caption(ui, "To make one: in Satisfactory, open Server Manager, pick this server, sign in as admin, and in its Console tab type server.GenerateAPIToken. Copy what it prints and paste it here. This works for rented servers too (BisectHosting and the like): commands only run from that Console tab, not from a hosting panel. The token is only ever sent to this server.");
            ui.horizontal(|ui| {
                do_check = widgets::button_enabled(ui, if checking { "Checking…" } else { "Check server" }, !checking && !draft.server_host.trim().is_empty()).clicked();
            });
            if let Some(e) = &error {
                crate::dialogs::status_line(ui, assets, false, e);
            }
            if let Some(c) = &check {
                if let Some(fp) = &c.untrusted_cert {
                    let first = saved.server_cert.is_none();
                    crate::dialogs::status_line(
                        ui,
                        assets,
                        false,
                        if first { "The server's certificate isn't trusted yet. Make sure this is your server before trusting it:" } else { "The server's certificate has changed since you trusted it. Only trust it if you changed it on the server:" },
                    );
                    ui.label(RichText::new(format!("SHA-256  {fp}")).small().monospace().color(TEXT));
                    if widgets::button(ui, "Trust this certificate").clicked() {
                        trust = Some(fp.clone());
                    }
                } else if let Some(e) = &c.error {
                    crate::dialogs::status_line(ui, assets, false, e);
                }
                if c.reachable && c.untrusted_cert.is_none() {
                    let health = c.health.as_deref().unwrap_or("answering");
                    crate::dialogs::status_line(ui, assets, true, &format!("Connected securely. The server is {health}."));
                    match (c.token_ok, &c.session) {
                        (Some(true), Some(s)) => {
                            let players = match (c.players, c.player_limit) {
                                (Some(n), Some(m)) => format!(", {n} of {m} players on"),
                                (Some(n), None) => format!(", {n} players on"),
                                _ => String::new(),
                            };
                            crate::dialogs::status_line(ui, assets, true, &format!("Running “{s}”{players}. The Map tab follows it (and only it)."));
                        }
                        (None, _) => crate::dialogs::status_line(ui, assets, false, "Add the API token to follow its saves."),
                        _ => {}
                    }
                }
            }
            widgets::divider(ui);
            ui.label(RichText::new("Ficsit Remote Monitoring").font(crate::theme::semibold(14.0)).color(ACCENT));
            frm_changed |= widgets::checkbox(ui, assets, &mut draft.frm, "Live players from the mod").changed();
            let via_server = !draft.server_host.trim().is_empty();
            if via_server {
                widgets::caption(ui, "Install the mod on the dedicated server with the Satisfactory Mod Manager. It's asked through the server above; if the server doesn't pass that on, the mod's own web server on the same machine is used, at this port (turn on its autostart in the mod's options).");
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Web server port").color(TEXT_DIM));
                    let mut v = draft.frm_port as f64;
                    if widgets::num_field(ui, egui::Id::new("live_frm_port"), &mut v, 80.0) {
                        draft.frm_port = v.clamp(1.0, 65535.0) as u16;
                        frm_changed = true;
                    }
                });
            } else {
                widgets::caption(ui, "Install the mod on the PC hosting the game with the Satisfactory Mod Manager, then type /frm http start in the game's chat (or turn on its web server's autostart in the mod's options).");
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Address").color(TEXT_DIM));
                    frm_changed |= ui.add(egui::TextEdit::singleline(&mut draft.frm_host).desired_width(260.0).char_limit(253).margin(vec2(8.0, 6.0))).lost_focus();
                    ui.label(RichText::new("Port").color(TEXT_DIM));
                    let mut v = draft.frm_port as f64;
                    if widgets::num_field(ui, egui::Id::new("live_frm_port"), &mut v, 80.0) {
                        draft.frm_port = v.clamp(1.0, 65535.0) as u16;
                        frm_changed = true;
                    }
                });
            }
            if saved.frm {
                match (live_age, &players_error) {
                    (_, Some(e)) => crate::dialogs::status_line(ui, assets, false, e),
                    (Some(age), None) if age < PLAYERS_STALE => {
                        crate::dialogs::status_line(ui, assets, true, &format!("Live: {live_count} {} reported.", if live_count == 1 { "player" } else { "players" }))
                    }
                    _ => widgets::caption(ui, "Open the Map tab to see it."),
                }
            }
        },
        &[("Close", true)],
    );
    app.live.token = token;
    if let Some(fp) = trust {
        app.backend.call("live_server_trust", json!({ "fingerprint": fp }), Msg::LiveTrusted);
    }
    if save_token {
        let t = std::mem::take(&mut app.live.token);
        app.backend.call("live_server_token", json!({ "token": t.trim() }), Msg::LiveToken);
    }
    if forget_token {
        app.backend.call("live_server_token", json!({ "token": null }), Msg::LiveToken);
    }
    if do_check {
        app.live.checking = true;
        app.live.error = None;
        app.live_save(draft.clone(), true);
    } else if frm_changed || clicked.is_some() {
        // The mod's settings take effect right away; the server's when
        // it's checked (or now, on closing).
        if *draft != saved {
            app.live_save(draft.clone(), false);
        }
    }
    clicked.is_none()
}
