//! "Import into planner": machines selected on the map become a factory,
//! grouped by recipe (one node per recipe with its machine count and average
//! clock speed, like the planner's own nodes). Extractors are grouped the
//! same way (by building and node purity), fed by a resource node for what
//! they take from the ground; flows from outside the selection become
//! inputs, and products that leave it become outputs. The result is a normal factory file, so the
//! existing import (beside the current graph, undoable) does the rest.
//!
//! "Make project from save" does that for every group of machines on the
//! map at once: each is a factory, linked on the master graph by the belts
//! and pipes between them, and power poles wired the way each power
//! circuit joins them.

use crate::portable::{FactoryFile, Kind, Link, Node, ProjectFactory, ProjectFile, Wire};
use crate::power_net::{End, Pole, PowerLink};
use fb_model::world::{MapInfo, Machine, World};
use fb_model::{GameData, Purity};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// What came out of an import, for the message to the user.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ImportReport {
    pub machines: usize,
    pub nodes: usize,
    /// Machines that couldn't be brought in (no recipe, generators...).
    pub left_out: usize,
    /// Groups whose machines had different Somersloop counts.
    pub mixed_sloops: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Group {
    Recipe(String),
    /// The nodes of one resource and purity (0 to 2) extractors stand on.
    Resource(String, u8),
    /// Extractors: resource, building class, node purity (0 to 2).
    Extractor(String, String, u8),
    /// Storage: building class, item.
    Storage(String, String),
}

/// What a node of an imported graph stands for on the map.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Members {
    #[default]
    None,
    /// Machine ids.
    Machines(Vec<u32>),
    /// Storage: indexes in `World::structures`.
    Stores(Vec<u32>),
    /// One of the HUB's burners (index in `World::hub_burners`).
    Burner(usize),
}

fn purity_code(p: Purity) -> u8 {
    Purity::ALL.iter().position(|x| *x == p).unwrap_or(1) as u8
}

fn per_min(r: &fb_model::Recipe, item: &str, list: &[fb_model::Amount]) -> f64 {
    list.iter().filter(|a| a.item == item).map(|a| r.per_min(a)).sum()
}

/// Items per minute an extractor produces on its node.
#[cfg(test)]
fn extractor_rate(game: &GameData, map: Option<&MapInfo>, world: &World, m: &Machine) -> Option<(String, f64)> {
    extractor_info(game, map, world, m).map(|(r, _, rate)| (r, rate))
}

/// What an extractor takes, the purity of its node, and per minute.
fn extractor_info(game: &GameData, map: Option<&MapInfo>, world: &World, m: &Machine) -> Option<(String, Purity, f64)> {
    let ex = game.extraction.extractors.iter().find(|e| e.class == m.class)?;
    if ex.kind == fb_model::ExtractorKind::Water {
        // Water extractors pump from any water; no node, no purity.
        return Some(("Desc_Water_C".to_string(), Purity::Normal, ex.per_min(Purity::Normal, m.clock as f64)));
    }
    let id = m.node.as_deref()?;
    let site = map.and_then(|mi| mi.nodes.iter().find(|n| n.id == id));
    let over = world.node_overrides.iter().find(|o| o.id == id);
    let resource = over.and_then(|o| o.resource.clone()).or_else(|| site.and_then(|s| s.resource.clone()))?;
    let purity = over.and_then(|o| o.purity).or(site.map(|s| s.purity)).unwrap_or(Purity::Normal);
    Some((resource, purity, ex.per_min(purity, m.clock as f64)))
}

pub fn import(world: &World, selected: &BTreeSet<u32>, game: &GameData, map: Option<&MapInfo>) -> (FactoryFile, ImportReport) {
    let (f, report, _) = import_with(world, selected, &BTreeSet::new(), &HashMap::new(), game, map, false);
    (f, report)
}

/// A live factory's graph (live_factory.rs): machines `selected` and
/// storage `stores` (structure indexes), each storage holding the item
/// `holds` says when no belt or pipe tells. Also what each node stands for.
pub fn import_live(world: &World, selected: &BTreeSet<u32>, stores: &BTreeSet<u32>, holds: &HashMap<u32, String>, game: &GameData, map: Option<&MapInfo>) -> (FactoryFile, Vec<Members>) {
    let (f, _, members) = import_with(world, selected, stores, holds, game, map, false);
    (f, members)
}

/// `from_factories`: what comes in from outside the selection comes from
/// another factory (an input), even raw resources (otherwise those are
/// resource nodes, mined for this factory).
fn import_with(world: &World, selected: &BTreeSet<u32>, stores: &BTreeSet<u32>, holds: &HashMap<u32, String>, game: &GameData, map: Option<&MapInfo>, from_factories: bool) -> (FactoryFile, ImportReport, Vec<Members>) {
    let mut report = ImportReport::default();
    let mut groups: BTreeMap<Group, Vec<&Machine>> = BTreeMap::new();
    let mut group_of: BTreeMap<u32, Group> = BTreeMap::new();
    let mut resource_rate: BTreeMap<(String, u8), f64> = BTreeMap::new();
    let mut group_rate: BTreeMap<Group, f64> = BTreeMap::new();
    // A generator holding no fuel, fed by nothing that says which: its
    // building's first fuel (it shows as idle), so it isn't left out.
    let first_fuel = |class: &str| {
        let mut rs: Vec<&String> = game.recipes.values().filter(|r| r.is_generator() && r.building == class).map(|r| &r.class).collect();
        rs.sort();
        rs.first().map(|r| (*r).clone())
    };
    for m in world.machines.iter().filter(|m| selected.contains(&m.id)) {
        let known = m.recipe.clone().filter(|r| game.recipes.contains_key(r)).or_else(|| first_fuel(&m.class));
        let g = match &known {
            Some(r) if game.recipes.contains_key(r) => Group::Recipe(r.clone()),
            _ => match extractor_info(game, map, world, m) {
                Some((res, purity, rate)) => {
                    *resource_rate.entry((res.clone(), purity_code(purity))).or_default() += rate;
                    let g = Group::Extractor(res, m.class.clone(), purity_code(purity));
                    *group_rate.entry(g.clone()).or_default() += rate;
                    g
                }
                None => {
                    report.left_out += 1;
                    continue;
                }
            },
        };
        report.machines += 1;
        group_of.insert(m.id, g.clone());
        groups.entry(g).or_default().push(m);
    }
    // A resource node for each resource and purity extracted.
    for (res, p) in resource_rate.keys() {
        groups.entry(Group::Resource(res.clone(), *p)).or_default();
    }
    // Storage: one node per building and item, from what its belts and
    // pipes carry, else what it holds.
    let mut store_group: BTreeMap<u32, Vec<Group>> = BTreeMap::new();
    let mut store_members: BTreeMap<Group, BTreeSet<u32>> = BTreeMap::new();
    let class_of = |s: u32| world.structures.get(s as usize).map(|x| x.class.clone());
    for l in &world.store_links {
        let (Some(item), true) = (&l.item, stores.contains(&l.store)) else { continue };
        let Some(class) = class_of(l.store) else { continue };
        let g = Group::Storage(class, item.clone());
        let list = store_group.entry(l.store).or_default();
        if !list.contains(&g) {
            list.push(g.clone());
        }
        store_members.entry(g).or_default().insert(l.store);
    }
    for &s in stores {
        if store_group.contains_key(&s) {
            continue;
        }
        let (Some(class), Some(item)) = (class_of(s), holds.get(&s)) else { continue };
        if !game.items.contains_key(item) {
            continue;
        }
        let g = Group::Storage(class, item.clone());
        store_group.entry(s).or_default().push(g.clone());
        store_members.entry(g).or_default().insert(s);
    }
    for g in store_members.keys() {
        groups.entry(g.clone()).or_default();
    }

    let mut f = FactoryFile::new(Some("Imported factory".into()));
    let mut index: BTreeMap<Group, usize> = BTreeMap::new();
    for (g, ms) in &groups {
        let node = match g {
            Group::Recipe(r) => {
                let recipe = &game.recipes[r];
                let count = ms.len() as f64;
                let clock = ms.iter().map(|m| m.clock as f64).sum::<f64>() / count;
                // Somersloops from the boost: boost = 1 + sloops x per-sloop boost.
                let per = game.buildings.get(&recipe.building).map(|b| b.sloop_boost).unwrap_or(0.0);
                let sloops: Vec<u32> = ms.iter().map(|m| if per > 0.0 { ((m.boost as f64 - 1.0) / per).round().max(0.0) as u32 } else { 0 }).collect();
                let mut counts: BTreeMap<u32, usize> = BTreeMap::new();
                for s in &sloops {
                    *counts.entry(*s).or_default() += 1;
                }
                if counts.len() > 1 {
                    report.mixed_sloops += 1;
                }
                let common = counts.iter().max_by_key(|(_, n)| **n).map(|(s, _)| *s).unwrap_or(0);
                Node { kind: Kind::Recipe, class: r.clone(), machines: Some(count), clock: Some((clock * 100.0 * 1000.0).round() / 1000.0), sloops: Some(common).filter(|s| *s > 0), rate: None, name: None, color: None, item: None, purity: None, x: 0.0, y: 0.0, planned: false }
            }
            Group::Resource(res, p) => Node { kind: Kind::Resource, class: res.clone(), machines: None, clock: None, sloops: None, rate: Some((resource_rate[&(res.clone(), *p)] * 1000.0).round() / 1000.0), name: None, color: None, item: None, purity: Purity::ALL.get(*p as usize).copied(), x: 0.0, y: 0.0, planned: false },
            Group::Extractor(res, class, p) => {
                let count = ms.len() as f64;
                let clock = ms.iter().map(|m| m.clock as f64).sum::<f64>() / count;
                Node {
                    kind: Kind::Extractor,
                    class: class.clone(),
                    machines: Some(count),
                    clock: Some((clock * 100.0 * 1000.0).round() / 1000.0),
                    sloops: None,
                    rate: None,
                    name: None,
                    color: None,
                    item: Some(res.clone()),
                    purity: Purity::ALL.get(*p as usize).copied(),
                    x: 0.0,
                    y: 0.0,
                    planned: false,
                }
            }
            // Its supply rate is set below, from what it feeds.
            Group::Storage(class, item) => Node { kind: Kind::Storage, class: class.clone(), machines: None, clock: None, sloops: None, rate: Some(0.0), name: None, color: None, item: Some(item.clone()), purity: None, x: 0.0, y: 0.0, planned: false },
        };
        index.insert(g.clone(), f.nodes.len());
        f.nodes.push(node);
    }

    // Item ports: a recipe node's inputs are its ingredients and its outputs
    // its products, in recipe order; resource/input nodes have one output and
    // output nodes one input.
    let in_port = |g: &Group, item: &str| -> Option<usize> {
        match g {
            Group::Recipe(r) => game.recipes[r].ingredients.iter().position(|a| a.item == item),
            Group::Resource(..) | Group::Extractor(..) => None,
            Group::Storage(_, held) => (held == item).then_some(0),
        }
    };
    let out_port = |g: &Group, item: &str| -> Option<usize> {
        match g {
            Group::Recipe(r) => game.recipes[r].products.iter().position(|a| a.item == item),
            Group::Resource(res, _) | Group::Extractor(res, ..) => (res == item).then_some(0),
            Group::Storage(_, held) => (held == item).then_some(0),
        }
    };

    // Wires between groups, and flows crossing the selection; each
    // extractor group takes from its resource's deposit.
    let mut wires: BTreeSet<(usize, usize, usize, usize)> = BTreeSet::new();
    for g in groups.keys() {
        if let Group::Extractor(res, _, p) = g {
            wires.insert((index[&Group::Resource(res.clone(), *p)], 0, index[g], 0));
        }
    }
    let mut from_outside: BTreeSet<(Group, String)> = BTreeSet::new();
    let mut to_outside: BTreeSet<(Group, String)> = BTreeSet::new();
    for l in &world.links {
        let Some(item) = &l.item else { continue };
        match (group_of.get(&l.from), group_of.get(&l.to)) {
            (Some(a), Some(b)) if a != b => {
                if let (Some(o), Some(i)) = (out_port(a, item), in_port(b, item)) {
                    wires.insert((index[a], o, index[b], i));
                }
            }
            (None, Some(b)) => {
                from_outside.insert((b.clone(), item.clone()));
            }
            (Some(a), None) => {
                to_outside.insert((a.clone(), item.clone()));
            }
            _ => {}
        }
    }

    // Belts and pipes to and from storage: wires when the storage is in
    // the factory, else what comes in or goes out.
    let mut storage_feeds: Vec<(Group, Group, String)> = Vec::new();
    // Storage here filled from (or feeding) machines elsewhere: per minute.
    let mut store_from_outside: BTreeMap<Group, f64> = BTreeMap::new();
    let mut store_to_outside: BTreeMap<Group, f64> = BTreeMap::new();
    for l in &world.store_links {
        let Some(item) = &l.item else { continue };
        if group_of.contains_key(&l.machine) {
            continue;
        }
        let Some(s) = store_group.get(&l.store).and_then(|gs| gs.iter().find(|g| matches!(g, Group::Storage(_, i) if i == item)).cloned()) else { continue };
        let Some(m) = world.machines.get(l.machine as usize) else { continue };
        let r = machine_rate(world, game, map, m, item, !l.into);
        *(if l.into { &mut store_from_outside } else { &mut store_to_outside }).entry(s).or_default() += r;
    }
    for l in &world.store_links {
        let (Some(item), Some(m)) = (&l.item, group_of.get(&l.machine)) else { continue };
        let inside = store_group.get(&l.store).and_then(|gs| gs.iter().find(|g| matches!(g, Group::Storage(_, i) if i == item)).cloned());
        match (inside, l.into) {
            (Some(s), true) => {
                if let (Some(o), Some(i)) = (out_port(m, item), in_port(&s, item)) {
                    wires.insert((index[m], o, index[&s], i));
                }
            }
            (Some(s), false) => {
                if let (Some(o), Some(i)) = (out_port(&s, item), in_port(m, item)) {
                    wires.insert((index[&s], o, index[m], i));
                    storage_feeds.push((s, m.clone(), item.clone()));
                }
            }
            (None, true) => {
                to_outside.insert((m.clone(), item.clone()));
            }
            (None, false) => {
                from_outside.insert((m.clone(), item.clone()));
            }
        }
    }

    // Rates per group, to size inputs and outputs.
    let rate = |g: &Group, item: &str, inputs: bool| -> f64 {
        match g {
            Group::Recipe(r) => {
                let recipe = &game.recipes[r];
                let n = &f.nodes[index[g]];
                let k = n.machines.unwrap_or(0.0) * n.clock.unwrap_or(100.0) / 100.0;
                if inputs {
                    per_min(recipe, item, &recipe.ingredients) * k
                } else {
                    let boost = game.buildings.get(&recipe.building).map(|b| b.amplification(n.sloops.unwrap_or(0))).unwrap_or(1.0);
                    per_min(recipe, item, &recipe.products) * k * boost
                }
            }
            Group::Resource(res, p) => {
                if !inputs && res == item {
                    resource_rate[&(res.clone(), *p)]
                } else {
                    0.0
                }
            }
            Group::Extractor(res, ..) => {
                if !inputs && res == item {
                    group_rate[g]
                } else {
                    0.0
                }
            }
            Group::Storage(..) => 0.0,
        }
    };
    // A storage supplies what the machines it feeds take.
    let mut supply: BTreeMap<Group, f64> = BTreeMap::new();
    let mut fed: BTreeSet<(Group, Group)> = BTreeSet::new();
    for (s, m, item) in &storage_feeds {
        if fed.insert((s.clone(), m.clone())) {
            *supply.entry(s.clone()).or_default() += rate(m, item, true);
        }
    }
    let mut extra: Vec<(Node, Option<(usize, usize, bool)>)> = Vec::new();
    for (g, item) in &from_outside {
        if let Some(i) = in_port(g, item) {
            let kind = if !from_factories && game.items.get(item).is_some_and(|it| it.raw) { Kind::Resource } else { Kind::Input };
            let r = rate(g, item, true);
            extra.push((Node { kind, class: item.clone(), machines: None, clock: None, sloops: None, rate: Some((r * 1000.0).round() / 1000.0), name: None, color: None, item: None, purity: None, x: 0.0, y: 0.0, planned: false }, Some((index[g], i, true))));
        }
    }
    for (g, item) in &to_outside {
        if let Some(o) = out_port(g, item) {
            let r = rate(g, item, false);
            extra.push((Node { kind: Kind::Output, class: item.clone(), machines: None, clock: None, sloops: None, rate: Some((r * 1000.0).round() / 1000.0), name: None, color: None, item: None, purity: None, x: 0.0, y: 0.0, planned: false }, Some((index[g], o, false))));
        }
    }
    for (g, r) in &store_from_outside {
        if let Group::Storage(_, item) = g {
            extra.push((Node { kind: Kind::Input, class: item.clone(), machines: None, clock: None, sloops: None, rate: Some((r * 1000.0).round() / 1000.0), name: None, color: None, item: None, purity: None, x: 0.0, y: 0.0, planned: false }, Some((index[g], 0, true))));
        }
    }
    for (g, r) in &store_to_outside {
        if let Group::Storage(_, item) = g {
            *supply.entry(g.clone()).or_default() += r;
            extra.push((Node { kind: Kind::Output, class: item.clone(), machines: None, clock: None, sloops: None, rate: Some((r * 1000.0).round() / 1000.0), name: None, color: None, item: None, purity: None, x: 0.0, y: 0.0, planned: false }, Some((index[g], 0, false))));
        }
    }
    for (node, link) in extra {
        let id = f.nodes.len();
        f.nodes.push(node);
        match link {
            Some((g, port, true)) => {
                wires.insert((id, 0, g, port));
            }
            Some((g, port, false)) => {
                wires.insert((g, port, id, 0));
            }
            None => {}
        }
    }

    for (g, r) in &supply {
        f.nodes[index[g]].rate = Some((r * 1000.0).round() / 1000.0);
    }
    // Layered layout, left to right in flow order.
    let edges: Vec<(usize, usize)> = wires.iter().map(|w| (w.0, w.2)).collect();
    let heights: Vec<f32> = f.nodes.iter().map(|n| if matches!(n.kind, Kind::Recipe | Kind::Extractor | Kind::Storage) { 230.0 } else { 120.0 }).collect();
    let pos = fb_engine::layout::layered_sized(f.nodes.len(), &edges, 300.0, &heights);
    for (n, p) in f.nodes.iter_mut().zip(pos) {
        n.x = p[0];
        n.y = p[1];
    }
    // Bubble order: one wire per pair of ports.
    let mut slots: BTreeMap<(usize, usize), usize> = BTreeMap::new();
    for (from, output, to, input) in wires {
        let slot = slots.entry((from, output)).or_default();
        f.wires.push(Wire { from, output, to, input, slot: *slot, belt: None });
        *slot += 1;
    }
    report.nodes = f.nodes.len();
    let mut members = vec![Members::None; f.nodes.len()];
    for (g, i) in &index {
        members[*i] = match g {
            Group::Recipe(_) | Group::Extractor(..) => Members::Machines(groups.get(g).map(|ms| ms.iter().map(|m| m.id).collect()).unwrap_or_default()),
            Group::Storage(..) => Members::Stores(store_members.get(g).map(|s| s.iter().copied().collect()).unwrap_or_default()),
            Group::Resource(..) => Members::None,
        };
    }
    (f, report, members)
}

// ------------------------------------------------- a project from a save

/// Groups of machines (ids) that stand together: each machine is within
/// `gap_cm` of another in its group. Found on a grid, so it stays quick on
/// big saves. Groups are in order of their first machine.
pub fn sites(world: &World, gap_cm: f32) -> Vec<Vec<u32>> {
    fn find(p: &mut [usize], mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    let gap = gap_cm.max(1.0);
    let ms = &world.machines;
    let mut parent: Vec<usize> = (0..ms.len()).collect();
    let cell = |v: f32| (v / gap).floor() as i64;
    let mut grid: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (i, m) in ms.iter().enumerate() {
        grid.entry((cell(m.x), cell(m.y))).or_default().push(i);
    }
    for (i, m) in ms.iter().enumerate() {
        let (cx, cy) = (cell(m.x), cell(m.y));
        for dx in -1..=1 {
            for dy in -1..=1 {
                for &j in grid.get(&(cx + dx, cy + dy)).into_iter().flatten() {
                    if j > i && (m.x - ms[j].x).hypot(m.y - ms[j].y) < gap {
                        let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                        parent[a] = b;
                    }
                }
            }
        }
    }
    let mut groups: BTreeMap<usize, Vec<u32>> = BTreeMap::new();
    let mut first: HashMap<usize, usize> = HashMap::new();
    for i in 0..ms.len() {
        let root = find(&mut parent, i);
        let key = *first.entry(root).or_insert(i);
        groups.entry(key).or_default().push(ms[i].id);
    }
    groups.into_values().collect()
}

/// About how tall a factory's block on the master graph is: a row per item
/// it takes in, per item it sends out (storage counts) and for its power,
/// stacked, under its header and above its footer.
fn block_height(f: &FactoryFile) -> f32 {
    let items = |kinds: &[Kind]| f.nodes.iter().filter(|n| kinds.contains(&n.kind)).map(|n| n.item.clone().unwrap_or_else(|| n.class.clone())).collect::<BTreeSet<_>>().len();
    let rows = items(&[Kind::Input]) + items(&[Kind::Output, Kind::Storage]) + 1;
    110.0 + 36.0 * rows as f32
}

/// What came out of making a project from a save.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct WorldReport {
    pub factories: usize,
    /// Links between factories on the master graph.
    pub links: usize,
    /// Power circuits joining two or more factories.
    pub circuits: usize,
    /// Machines brought in.
    pub machines: usize,
    /// Machines that couldn't be (no recipe set, generators...).
    pub left_out: usize,
}


/// A project of the whole save: every group of machines standing within
/// `gap_cm` of each other is a factory (as Import into planner makes it),
/// and the belts and pipes running between groups are links on the master
/// graph. Linked factories sit together (see [`arrange_blocks`]). Power
/// circuits that join two or more factories are wired through power poles
/// (see [`power_poles`]; a circuit inside one factory joins nothing).
pub fn world_project(world: &World, game: &GameData, map: Option<&MapInfo>, gap_cm: f32) -> (ProjectFile, WorldReport) {
    let mut report = WorldReport::default();
    let mut p = ProjectFile::new(Some(world.save.name.clone()));
    let mut factory_of: HashMap<u32, u32> = HashMap::new();
    let mut names: BTreeMap<String, usize> = BTreeMap::new();
    // Each block: where the factory is (world cm) and the block's size.
    let mut boxes: Vec<(f32, f32, f32, f32)> = Vec::new();
    for site in sites(world, gap_cm) {
        let set: BTreeSet<u32> = site.iter().copied().collect();
        let (mut f, r, _) = import_with(world, &set, &BTreeSet::new(), &HashMap::new(), game, map, true);
        report.left_out += r.left_out;
        if r.machines == 0 {
            continue;
        }
        report.machines += r.machines;
        let id = p.factories.len() as u32 + 1;
        for m in &site {
            factory_of.insert(*m, id);
        }
        let base = site_name(game, &f);
        let n = names.entry(base.clone()).or_default();
        *n += 1;
        f.name = Some(if *n == 1 { base } else { format!("{base} {n}") });
        let ms: Vec<&Machine> = site.iter().filter_map(|m| world.machines.get(*m as usize)).collect();
        let cx = ms.iter().map(|m| m.x).sum::<f32>() / ms.len() as f32;
        let cy = ms.iter().map(|m| m.y).sum::<f32>() / ms.len() as f32;
        boxes.push((cx, cy, 320.0, block_height(&f)));
        p.factories.push(ProjectFactory { id, x: 0.0, y: 0.0, color: None, factory: f, hub: false });
    }
    // Links: an item belted or piped from one factory to another, when the
    // first sends it out and the second takes it in.
    let has = |id: u32, kind: Kind, item: &str| p.factories[id as usize - 1].factory.nodes.iter().any(|n| n.kind == kind && n.class == item);
    let mut pairs: BTreeSet<(u32, u32, String)> = BTreeSet::new();
    for l in &world.links {
        let (Some(item), Some(&a), Some(&b)) = (&l.item, factory_of.get(&l.from), factory_of.get(&l.to)) else { continue };
        if a != b && has(a, Kind::Output, item) && has(b, Kind::Input, item) {
            pairs.insert((a, b, item.clone()));
        }
    }
    let mut slots: BTreeMap<(u32, String), usize> = BTreeMap::new();
    for (from, to, item) in pairs {
        let slot = slots.entry((from, item.clone())).or_default();
        p.links.push(Link { from, to, item, slot: *slot });
        *slot += 1;
    }
    let linked: Vec<(usize, usize)> = p.links.iter().map(|l| (l.from as usize - 1, l.to as usize - 1)).collect();
    for (f, (x, y)) in p.factories.iter_mut().zip(arrange_blocks(&boxes, &linked)) {
        (f.x, f.y) = (x, y);
    }
    (p.poles, p.power_links, report.circuits) = power_poles(world, &factory_of, &p.factories, &boxes);
    report.factories = p.factories.len();
    report.links = p.links.len();
    (p, report)
}

/// Per minute a machine makes (`takes`: uses) of `item`, at its clock
/// speed and Somersloops (extractors: what they extract).
fn machine_rate(world: &World, game: &GameData, map: Option<&MapInfo>, m: &Machine, item: &str, takes: bool) -> f64 {
    if let Some(r) = m.recipe.as_ref().and_then(|r| game.recipes.get(r)) {
        let k = m.clock as f64;
        return if takes { per_min(r, item, &r.ingredients) * k } else { per_min(r, item, &r.products) * k * m.boost as f64 };
    }
    match extractor_info(game, map, world, m) {
        Some((res, _, rate)) if !takes && res == item => rate,
        _ => 0.0,
    }
}

/// The live project (live_factory.rs): what each factory node stands for,
/// per factory (by its id in the file).
pub struct LiveProject {
    pub file: ProjectFile,
    pub members: BTreeMap<u32, Vec<Members>>,
}

/// The live project: each live factory (a region or picked buildings) is a
/// factory, its graph as Import into planner makes it, with its storage;
/// a building in two regions belongs to the first. Belts, pipes and
/// storage running between factories are links on the master graph, power
/// circuits joining them are wired through poles, and the save's HUB is the
/// project's HUB, on its circuit. Blocks sit as `world_project` puts them.
/// `holds`: what each storage holds, for storage no belt tells.
pub fn region_project(world: &World, defs: &[fb_model::LiveFactoryDef], holds: &HashMap<u32, String>, game: &GameData, map: Option<&MapInfo>) -> LiveProject {
    let mut p = ProjectFile::new(Some("Live".into()));
    let mut members_of: BTreeMap<u32, Vec<Members>> = BTreeMap::new();
    let mut factory_of: HashMap<u32, u32> = HashMap::new();
    let mut store_factory: HashMap<u32, u32> = HashMap::new();
    let mut boxes: Vec<(f32, f32, f32, f32)> = Vec::new();
    for def in defs.iter().filter(|d| d.valid()) {
        let (ms, ss) = crate::live_factory::members(def, world);
        let ms: BTreeSet<u32> = ms.into_iter().filter(|m| !factory_of.contains_key(m)).collect();
        let ss: BTreeSet<u32> = ss.into_iter().filter(|s| !store_factory.contains_key(s)).collect();
        let id = p.factories.len() as u32 + 1;
        for m in &ms {
            factory_of.insert(*m, id);
        }
        for s in &ss {
            store_factory.insert(*s, id);
        }
        let (mut f, _, members) = import_with(world, &ms, &ss, holds, game, map, true);
        f.name = Some(def.name.clone());
        let pts: Vec<[f32; 2]> = if def.region.len() >= 3 {
            def.region.clone()
        } else {
            ms.iter().filter_map(|m| world.machines.get(*m as usize)).map(|m| [m.x, m.y]).chain(ss.iter().filter_map(|s| world.structures.get(*s as usize)).map(|s| [s.x, s.y])).collect()
        };
        let n = pts.len().max(1) as f32;
        let (cx, cy) = (pts.iter().map(|p| p[0]).sum::<f32>() / n, pts.iter().map(|p| p[1]).sum::<f32>() / n);
        boxes.push((cx, cy, 320.0, block_height(&f)));
        members_of.insert(id, members);
        p.factories.push(ProjectFactory { id, x: 0.0, y: 0.0, color: def.color.map(crate::portable::color_to_hex), factory: f, hub: false });
    }
    // The HUB, where the save has it, on its circuit.
    let mut extra = Vec::new();
    if let Some(h) = world.structures.iter().find(|s| s.class.starts_with("Build_TradingPost")) {
        let id = p.factories.len() as u32 + 1;
        boxes.push((h.x, h.y, 320.0, 150.0));
        // On its burners' circuit (the HUB building itself may have none).
        if let Some(c) = world.hub_burners.iter().find_map(|b| b.circuit).or(h.circuit) {
            extra.push((c, id));
        }
        // Its two burners, burning what they burn now, fed it by hand.
        let mut f = FactoryFile::new(Some(crate::hub::NAME.into()));
        let mut members = Vec::new();
        for (k, b) in world.hub_burners.iter().take(fb_model::HUB_BURNERS).enumerate() {
            let burning = b.fuel.clone().filter(|f| crate::hub::is_fuel(game, f));
            let Some(fuel) = burning.clone().or_else(|| crate::hub::default_fuel(game)) else { continue };
            let y = k as f32 * 260.0;
            let at = f.nodes.len();
            f.nodes.push(Node { kind: Kind::Burner, class: fuel.clone(), machines: None, clock: None, sloops: None, rate: None, name: None, color: None, item: None, purity: None, x: 420.0, y, planned: false });
            members.push(Members::Burner(k));
            // Its output follows demand from moment to moment; the live
            // figures show that. Here: fuelled, it can make its full output.
            let share = if burning.is_some() { 1.0 } else { 0.0 };
            let rate = crate::hub::fuel_per_min(game, &fuel) * share;
            f.nodes.push(Node { kind: Kind::Input, class: fuel, machines: None, clock: None, sloops: None, rate: Some((rate * 1000.0).round() / 1000.0), name: None, color: None, item: None, purity: None, x: 0.0, y, planned: false });
            members.push(Members::None);
            f.wires.push(Wire { from: at + 1, output: 0, to: at, input: 0, slot: 0, belt: None });
        }
        members_of.insert(id, members);
        p.factories.push(ProjectFactory { id, x: 0.0, y: 0.0, color: None, factory: f, hub: true });
    }
    // Links: an item belted or piped (or stored) from one factory to another,
    // when the first sends it out and the second takes it in.
    let has = |id: u32, kind: Kind, item: &str| p.factories[id as usize - 1].factory.nodes.iter().any(|n| n.kind == kind && n.class == item);
    let mut pairs: BTreeSet<(u32, u32, String)> = BTreeSet::new();
    let mut pair = |a: u32, b: u32, item: &str| {
        if a != b && has(a, Kind::Output, item) && has(b, Kind::Input, item) {
            pairs.insert((a, b, item.to_string()));
        }
    };
    for l in &world.links {
        if let (Some(item), Some(&a), Some(&b)) = (&l.item, factory_of.get(&l.from), factory_of.get(&l.to)) {
            pair(a, b, item);
        }
    }
    for l in &world.store_links {
        if let (Some(item), Some(&m), Some(&s)) = (&l.item, factory_of.get(&l.machine), store_factory.get(&l.store)) {
            if l.into {
                pair(m, s, item);
            } else {
                pair(s, m, item);
            }
        }
    }
    let mut slots: BTreeMap<(u32, String), usize> = BTreeMap::new();
    for (from, to, item) in pairs {
        let slot = slots.entry((from, item.clone())).or_default();
        p.links.push(Link { from, to, item, slot: *slot });
        *slot += 1;
    }
    let linked: Vec<(usize, usize)> = p.links.iter().map(|l| (l.from as usize - 1, l.to as usize - 1)).collect();
    for (f, (x, y)) in p.factories.iter_mut().zip(arrange_blocks(&boxes, &linked)) {
        (f.x, f.y) = (x, y);
    }
    (p.poles, p.power_links, _) = power_poles_with(world, &factory_of, &extra, &p.factories, &boxes);
    LiveProject { file: p, members: members_of }
}

/// Poles for each power circuit that joins two or more factories with
/// machines (`factory_of`: machine id -> factory id). Every one gets a pole at
/// a corner of its block, in the gaps between blocks: top right for one
/// that makes more power than it uses (in the save), where its Power output
/// is, top left for the rest. Factories whose corners meet share a pole.
/// Each circuit's poles are joined pole to nearest pole (a minimum spanning
/// tree), so its wires run short, along the gaps. Returns the poles, the
/// wires and how many circuits.
fn power_poles(world: &World, factory_of: &HashMap<u32, u32>, factories: &[ProjectFactory], boxes: &[(f32, f32, f32, f32)]) -> (Vec<Pole>, Vec<PowerLink>, usize) {
    power_poles_with(world, factory_of, &[], factories, boxes)
}

/// As `power_poles`, with more factories on circuits (circuit, factory):
/// the HUB, whose burners aren't machines.
fn power_poles_with(world: &World, factory_of: &HashMap<u32, u32>, extra: &[(u32, u32)], factories: &[ProjectFactory], boxes: &[(f32, f32, f32, f32)]) -> (Vec<Pole>, Vec<PowerLink>, usize) {
    let mut on: BTreeMap<u32, BTreeSet<u32>> = BTreeMap::new();
    let mut balance: HashMap<u32, f32> = HashMap::new();
    for (c, f) in extra {
        on.entry(*c).or_default().insert(*f);
        *balance.entry(*f).or_default() += 1.0;
    }
    // Only factories with machines or extractors (anything that uses power).
    let planned = |f: u32| factories[f as usize - 1].factory.nodes.iter().any(|n| matches!(n.kind, Kind::Recipe | Kind::Extractor));
    for (m, c) in &world.machine_circuit {
        if let Some(&f) = factory_of.get(m).filter(|f| planned(**f)) {
            on.entry(*c).or_default().insert(f);
        }
    }
    for (m, u) in &world.machine_power {
        if let Some(&f) = factory_of.get(m) {
            *balance.entry(f).or_default() += u.makes - u.max;
        }
    }
    let (mut poles, mut links) = (Vec::new(), Vec::new());
    let mut circuits = 0;
    for members in on.into_values().filter(|fs| fs.len() >= 2) {
        circuits += 1;
        // This circuit's poles: (id, centre).
        let mut here: Vec<(u32, (f32, f32))> = Vec::new();
        for f in members {
            let (b, w) = (&factories[f as usize - 1], boxes[f as usize - 1].2);
            let makes = balance.get(&f).is_some_and(|v| *v > 0.0);
            let spot = (if makes { b.x + w + BLOCK_GAP / 2.0 } else { b.x - BLOCK_GAP / 2.0 }, b.y - BLOCK_GAP / 2.0);
            let near = here.iter().find(|(_, c)| (c.0 - spot.0).hypot(c.1 - spot.1) < POLE_SHARE).map(|(id, _)| *id);
            let pole = near.unwrap_or_else(|| {
                let id = poles.len() as u32 + 1;
                let half = crate::power_net::POLE_SIZE / 2.0;
                poles.push(Pole { id, x: spot.0 - half.x, y: spot.1 - half.y, tier: 1 });
                here.push((id, spot));
                id
            });
            links.push(PowerLink { from: End::Factory(f), to: End::Pole(pole) });
        }
        // Prim's tree over the poles, each wire left to right.
        let n = here.len();
        let mut joined = vec![false; n];
        let mut best: Vec<(f32, usize)> = vec![(f32::INFINITY, 0); n];
        let dist = |a: usize, b: usize| (here[a].1 .0 - here[b].1 .0).hypot(here[a].1 .1 - here[b].1 .1);
        if n > 0 {
            joined[0] = true;
            for k in 1..n {
                best[k] = (dist(0, k), 0);
            }
        }
        for _ in 1..n {
            let Some(k) = (0..n).filter(|k| !joined[*k]).min_by(|a, b| best[*a].0.total_cmp(&best[*b].0)) else { break };
            joined[k] = true;
            let j = best[k].1;
            let (l, r) = if here[j].1 .0 <= here[k].1 .0 { (j, k) } else { (k, j) };
            links.push(PowerLink { from: End::Pole(here[l].0), to: End::Pole(here[r].0) });
            for m in 0..n {
                if !joined[m] && dist(k, m) < best[m].0 {
                    best[m] = (dist(k, m), k);
                }
            }
        }
    }
    // Each pole the lowest tier that takes its wires.
    for p in &mut poles {
        let wires = links.iter().filter(|l| l.from == End::Pole(p.id) || l.to == End::Pole(p.id)).count();
        p.tier = crate::power_net::tier_for(wires);
    }
    (poles, links, circuits)
}

/// Corners closer than this (graph units) share a pole.
const POLE_SHARE: f32 = 60.0;

/// A factory's name: the most advanced thing it makes (by sink points;
/// fluids have none, so then what most machines make). Power plants are
/// named for their generators, and sites with only miners and pumps are
/// "<resource> extraction".
fn site_name(game: &GameData, f: &FactoryFile) -> String {
    let points = |c: &str| game.items.get(c).map(|i| i.sink_points).unwrap_or(0);
    let recipes: Vec<(&fb_model::Recipe, f64)> =
        f.nodes.iter().filter(|n| n.kind == Kind::Recipe).filter_map(|n| game.recipes.get(&n.class).map(|r| (r, n.machines.unwrap_or(0.0)))).collect();
    let made = recipes
        .iter()
        .filter(|(r, _)| !r.is_generator())
        .flat_map(|(r, machines)| r.products.iter().map(move |a| (a.item.as_str(), *machines)))
        .max_by(|a, b| points(a.0).cmp(&points(b.0)).then(a.1.total_cmp(&b.1)));
    if let Some((item, _)) = made {
        return game.item_name(item).to_string();
    }
    let power = recipes.iter().filter(|(r, _)| r.is_generator()).max_by(|a, b| a.1.total_cmp(&b.1));
    if let Some(b) = power.and_then(|(r, _)| game.buildings.get(&r.building)) {
        let count: f64 = recipes.iter().filter(|(r, _)| r.building == b.class).map(|x| x.1).sum();
        return if count > 1.0 { format!("{}s", b.name) } else { b.name.clone() };
    }
    let rate = |n: &Node| n.rate.unwrap_or(0.0);
    match f.nodes.iter().filter(|n| matches!(n.kind, Kind::Resource | Kind::Output)).max_by(|a, b| rate(a).total_cmp(&rate(b))) {
        Some(n) => format!("{} extraction", game.item_name(&n.class)),
        None => "Factory".into(),
    }
}

/// Places blocks (world x, y, block width, height) in a compact grid that
/// keeps the map's order: rows from north to south, each from west to east.
/// The whole world fits on screen; neighbours on the map stay near.
/// Returns each block's top-left corner (graph units).
/// Space between factory blocks and between groups of them.
const BLOCK_GAP: f32 = 70.0;

/// Places factory blocks (world x, y, block width, height) on the master
/// graph. Factories linked to each other sit together, laid out left to
/// right like a factory's graph so their links run short and level; the
/// groups go in rows in map order (north to south, then west to east), and
/// factories without links follow in a grid, in map order too.
fn arrange_blocks(boxes: &[(f32, f32, f32, f32)], links: &[(usize, usize)]) -> Vec<(f32, f32)> {
    fn find(p: &mut [usize], mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    let n = boxes.len();
    let mut parent: Vec<usize> = (0..n).collect();
    for &(a, b) in links {
        if a < n && b < n {
            let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
            parent[ra] = rb;
        }
    }
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..n {
        let root = find(&mut parent, i);
        groups.entry(root).or_default().push(i);
    }
    let (linked, single): (Vec<Vec<usize>>, Vec<Vec<usize>>) = groups.into_values().partition(|g| g.len() > 1);

    // Each linked group laid out on its own: members' places from the
    // group's top left, its size, and where it is on the map.
    let mut placed: Vec<BlockGroup> = linked
        .into_iter()
        .map(|members| {
            let index: HashMap<usize, usize> = members.iter().enumerate().map(|(k, &i)| (i, k)).collect();
            let edges: Vec<(usize, usize)> = links.iter().filter_map(|(a, b)| Some((*index.get(a)?, *index.get(b)?))).collect();
            let heights: Vec<f32> = members.iter().map(|&i| boxes[i].3).collect();
            let width = members.iter().map(|&i| boxes[i].2).fold(0.0, f32::max);
            let pos = fb_engine::layout::layered_sized(members.len(), &edges, width, &heights);
            let left = pos.iter().map(|p| p[0]).fold(f32::INFINITY, f32::min);
            let top = pos.iter().map(|p| p[1]).fold(f32::INFINITY, f32::min);
            let at: Vec<(f32, f32)> = pos.iter().map(|p| (p[0] - left, p[1] - top)).collect();
            let w = members.iter().zip(&at).map(|(&i, a)| a.0 + boxes[i].2).fold(0.0, f32::max);
            let h = members.iter().zip(&at).map(|(&i, a)| a.1 + boxes[i].3).fold(0.0, f32::max);
            let count = members.len() as f32;
            let x = members.iter().map(|&i| boxes[i].0).sum::<f32>() / count;
            let y = members.iter().map(|&i| boxes[i].1).sum::<f32>() / count;
            BlockGroup { members, at, w, h, x, y }
        })
        .collect();

    // Try a few row widths and keep the layout whose longer side is
    // shortest: it frames at the largest zoom.
    let area: f32 = placed.iter().map(|g| (g.w + BLOCK_GAP) * (g.h + BLOCK_GAP)).sum::<f32>()
        + single.iter().flatten().map(|&i| (boxes[i].2 + BLOCK_GAP) * (boxes[i].3 + BLOCK_GAP)).sum::<f32>();
    let widest = placed.iter().map(|g| g.w).fold(0.0, f32::max);
    let single: Vec<usize> = single.into_iter().flatten().collect();
    placed.sort_by(|a, b| a.y.total_cmp(&b.y));
    [1.0, 1.25, 1.5, 2.0, 2.5, 3.0]
        .iter()
        .map(|k| pack(boxes, &placed, &single, (area.sqrt() * k).max(widest)))
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(out, _)| out)
        .unwrap_or_default()
}

/// A linked group of factory blocks, laid out on its own.
struct BlockGroup {
    members: Vec<usize>,
    /// Each member's place from the group's top left.
    at: Vec<(f32, f32)>,
    w: f32,
    h: f32,
    /// Where the group is on the map.
    x: f32,
    y: f32,
}

/// Packs the groups (north to south) into rows up to `row_width` wide, each
/// row west to east, then the unlinked blocks in a grid below. Returns
/// every block's place and the longer side of the whole.
fn pack(boxes: &[(f32, f32, f32, f32)], groups: &[BlockGroup], single: &[usize], row_width: f32) -> (Vec<(f32, f32)>, f32) {
    let mut out = vec![(0.0, 0.0); boxes.len()];
    let mut top = 0.0;
    let mut right: f32 = 0.0;
    let mut rest: Vec<&BlockGroup> = groups.iter().collect();
    while !rest.is_empty() {
        let mut take = 1;
        let mut used = rest[0].w;
        while take < rest.len() && used + BLOCK_GAP + rest[take].w <= row_width {
            used += BLOCK_GAP + rest[take].w;
            take += 1;
        }
        let mut row: Vec<&BlockGroup> = rest.drain(..take).collect();
        row.sort_by(|a, b| a.x.total_cmp(&b.x));
        let mut left = 0.0;
        for g in &row {
            for (&i, a) in g.members.iter().zip(&g.at) {
                out[i] = (left + a.0, top + a.1);
            }
            left += g.w + BLOCK_GAP;
        }
        right = right.max(left - BLOCK_GAP);
        top += row.iter().map(|g| g.h).fold(0.0, f32::max) + BLOCK_GAP;
    }
    let block_w = single.iter().map(|&i| boxes[i].2).fold(1.0, f32::max);
    let cols = ((row_width + BLOCK_GAP) / (block_w + BLOCK_GAP)).floor().max(1.0) as usize;
    let sub: Vec<(f32, f32, f32, f32)> = single.iter().map(|&i| boxes[i]).collect();
    let mut bottom: f32 = top - BLOCK_GAP;
    for ((&i, (x, y)), b) in single.iter().zip(grid(&sub, cols)).zip(&sub) {
        out[i] = (x, top + y);
        right = right.max(x + b.2);
        bottom = bottom.max(top + y + b.3);
    }
    (out, right.max(bottom))
}

/// Blocks in a grid of `cols` columns, in map order: rows north to south,
/// each row west to east.
fn grid(boxes: &[(f32, f32, f32, f32)], cols: usize) -> Vec<(f32, f32)> {
    const GAP: f32 = BLOCK_GAP;
    let cols = cols.max(1);
    let mut order: Vec<usize> = (0..boxes.len()).collect();
    order.sort_by(|a, b| boxes[*a].1.total_cmp(&boxes[*b].1));
    let mut out = vec![(0.0, 0.0); boxes.len()];
    let mut top = 0.0;
    for row in order.chunks_mut(cols) {
        row.sort_by(|a, b| boxes[*a].0.total_cmp(&boxes[*b].0));
        let mut left = 0.0;
        for &i in row.iter() {
            out[i] = (left, top);
            left += boxes[i].2 + GAP;
        }
        top += row.iter().map(|i| boxes[*i].3).fold(0.0, f32::max) + GAP;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use fb_model::world::{Link as WLink, Machine};

    fn machine(id: u32, x: f32, recipe: &str) -> Machine {
        Machine { id, class: "Build_ConstructorMk1_C".into(), recipe: Some(recipe.into()), clock: 1.0, boost: 1.0, x, y: 0.0, z: 0.0, yaw: 0.0, node: None }
    }

    #[test]
    fn machines_standing_together_are_one_site() {
        let mut w = World::default();
        // 0-1-2 chained 30 m apart, 3 far away.
        w.machines = vec![machine(0, 0.0, "a"), machine(1, 3000.0, "a"), machine(2, 6000.0, "a"), machine(3, 50_000.0, "a")];
        assert_eq!(sites(&w, 5000.0), vec![vec![0, 1, 2], vec![3]]);
        assert_eq!(sites(&w, 2000.0), vec![vec![0], vec![1], vec![2], vec![3]]);
    }

    #[test]
    fn blocks_keep_the_maps_order() {
        // Four factories: two in the north (west, east), two in the south.
        let b = [(900.0, 10.0, 300.0, 200.0), (-500.0, 0.0, 300.0, 150.0), (100.0, 5000.0, 300.0, 200.0), (-900.0, 4000.0, 300.0, 200.0)];
        let at = grid(&b, 2);
        assert_eq!(at, vec![(370.0, 0.0), (0.0, 0.0), (370.0, 270.0), (0.0, 270.0)]);
    }

    #[test]
    fn linked_factories_sit_together() {
        // 0 -> 2 linked though far apart on the map; 1 and 3 have no links.
        let b = [(0.0, 0.0, 300.0, 200.0), (500.0, 100.0, 300.0, 200.0), (90_000.0, 90_000.0, 300.0, 200.0), (1000.0, 200.0, 300.0, 200.0)];
        let at = arrange_blocks(&b, &[(0, 2)]);
        // Side by side, level, the supplier on the left.
        assert_eq!(at[0].1, at[2].1, "{at:?}");
        assert!(at[2].0 > at[0].0 && at[2].0 - at[0].0 < 300.0 + 200.0, "{at:?}");
        // The others below them, apart from each other.
        assert!(at[1].1 >= at[0].1 + 200.0 && at[3].1 >= at[0].1 + 200.0, "{at:?}");
        assert_ne!(at[1], at[3]);
    }

    #[test]
    fn belts_between_sites_are_links() {
        let mut game = GameData::default();
        let item = |c: &str| fb_model::Item { class: c.into(), name: c.into(), description: String::new(), form: fb_model::Form::Solid, sink_points: 1, raw: false, color: [0; 4] };
        for c in ["Ingot", "Plate"] {
            game.items.insert(c.into(), item(c));
        }
        let recipe = |c: &str, i: Vec<&str>, o: &str| fb_model::Recipe {
            class: c.into(),
            name: c.into(),
            alternate: false,
            duration_s: 6.0,
            ingredients: i.into_iter().map(|x| fb_model::Amount { item: x.into(), amount: 1.0 }).collect(),
            products: vec![fb_model::Amount { item: o.into(), amount: 1.0 }],
            building: "Build_ConstructorMk1_C".into(),
            variable_power_mw: None,
            generates_mw: 0.0,
        };
        game.recipes.insert("MakeIngot".into(), recipe("MakeIngot", vec![], "Ingot"));
        game.recipes.insert("MakePlate".into(), recipe("MakePlate", vec!["Ingot"], "Plate"));
        let mut w = World::default();
        w.machines = vec![machine(0, 0.0, "MakeIngot"), machine(1, 100_000.0, "MakePlate")];
        w.links = vec![WLink { from: 0, to: 1, item: Some("Ingot".into()) }];
        let (p, r) = world_project(&w, &game, None, 5000.0);
        assert_eq!((r.factories, r.links, r.machines), (2, 1, 2));
        let names: Vec<_> = p.factories.iter().map(|f| f.factory.name.clone().unwrap()).collect();
        assert_eq!(names, ["Ingot", "Plate"], "named for what they make");
        assert_eq!(p.links, vec![Link { from: 1, to: 2, item: "Ingot".into(), slot: 0 }]);
        // The plate factory's ingots come from another factory: an input,
        // not a resource.
        assert!(p.factories[1].factory.nodes.iter().any(|n| n.kind == Kind::Input && n.class == "Ingot"));
        assert!(p.factories[0].x < p.factories[1].x, "blocks sit the way the factories do");
    }

    #[test]
    fn circuits_joining_factories_get_poles() {
        let mut game = GameData::default();
        game.items.insert("Ingot".into(), fb_model::Item { class: "Ingot".into(), name: "Ingot".into(), description: String::new(), form: fb_model::Form::Solid, sink_points: 1, raw: false, color: [0; 4] });
        game.recipes.insert(
            "MakeIngot".into(),
            fb_model::Recipe { class: "MakeIngot".into(), name: "MakeIngot".into(), alternate: false, duration_s: 6.0, ingredients: vec![], products: vec![fb_model::Amount { item: "Ingot".into(), amount: 1.0 }], building: "Build_ConstructorMk1_C".into(), variable_power_mw: None, generates_mw: 0.0 },
        );
        // Three sites far apart. Circuit 5 joins the first two; circuit 9
        // is the third's alone; circuit 7 has two machines of the first.
        let mut w = World::default();
        w.machines = vec![machine(0, 0.0, "MakeIngot"), machine(1, 100.0, "MakeIngot"), machine(2, 100_000.0, "MakeIngot"), machine(3, 200_000.0, "MakeIngot")];
        w.machine_circuit = [(0, 5), (1, 7), (2, 5), (3, 9)].into_iter().collect();
        // The first makes power (so its pole is on its right), the second uses it.
        w.machine_power = [(0, fb_model::world::PowerUse { now: 0.0, max: 0.0, makes: 75.0 }), (2, fb_model::world::PowerUse { now: 4.0, max: 4.0, makes: 0.0 })].into_iter().collect();
        let (p, r) = world_project(&w, &game, None, 5000.0);
        assert_eq!(r.factories, 3);
        assert_eq!(r.circuits, 1, "only circuit 5 joins two factories");
        // Each factory's pole sits at its corner: top right for the plant,
        // top left for the other.
        let centre = |id: u32| {
            let q = p.poles.iter().find(|q| q.id == id).unwrap();
            (q.x + crate::power_net::POLE_SIZE.x / 2.0, q.y + crate::power_net::POLE_SIZE.y / 2.0)
        };
        let pole_of = |f: u32| {
            p.power_links.iter().find_map(|l| match (l.from, l.to) {
                (End::Factory(x), End::Pole(q)) | (End::Pole(q), End::Factory(x)) if x == f => Some(q),
                _ => None,
            })
        };
        let (a, b) = (&p.factories[0], &p.factories[1]);
        let near = |c: (f32, f32), x: f32, y: f32| (c.0 - x).abs() < 1e-3 && (c.1 - y).abs() < 1e-3;
        let ca = centre(pole_of(1).unwrap());
        assert!(near(ca, a.x + 320.0 + BLOCK_GAP / 2.0, a.y - BLOCK_GAP / 2.0), "top right of the plant: {ca:?}");
        let cb = centre(pole_of(2).unwrap());
        assert!(near(cb, b.x - BLOCK_GAP / 2.0, b.y - BLOCK_GAP / 2.0), "top left of the other: {cb:?}");
        assert!(pole_of(3).is_none(), "the third is on a circuit of its own");
        // Two poles are wired to each other, left to right.
        if p.poles.len() == 2 {
            let (l, r) = if ca.0 <= cb.0 { (pole_of(1).unwrap(), pole_of(2).unwrap()) } else { (pole_of(2).unwrap(), pole_of(1).unwrap()) };
            assert!(p.power_links.contains(&PowerLink { from: End::Pole(l), to: End::Pole(r) }), "{:?}", p.power_links);
        } else {
            assert_eq!(p.poles.len(), 1, "corners that meet share a pole");
        }
        // It survives the file format.
        let back = ProjectFile::parse(&p.to_json(false)).unwrap();
        assert_eq!((back.poles, back.power_links), (p.poles.clone(), p.power_links.clone()));
    }

    /// The late-game save (FB_TEST_SAVES), with the extracted map's resource
    /// nodes when FB_TEST_MAP points at a map.json.
    #[test]
    fn real_save_project() {
        let Ok(dir) = std::env::var("FB_TEST_SAVES") else { return };
        let game_dir = std::env::var("SATISFACTORY_DIR").unwrap_or_else(|_| r"C:\Program Files (x86)\Steam\steamapps\common\Satisfactory".into());
        let Ok(game) = fb_engine::docs::load_game_data(std::path::Path::new(&game_dir)) else { return };
        let Ok(bytes) = std::fs::read(std::path::Path::new(&dir).join("late.sav")) else { return };
        let mut w = fb_engine::world::read_world(&bytes, &game).unwrap();
        let map: Option<MapInfo> = std::env::var("FB_TEST_MAP").ok().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str(&t).ok());
        if let Some(m) = &map {
            fb_engine::world::apply_map(&mut w, m, &game);
        }
        let (p, r) = world_project(&w, &game, map.as_ref(), 5000.0);
        eprintln!("{r:?} (map: {})", map.is_some());
        for f in p.factories.iter().take(60) {
            let kinds = |k: Kind| f.factory.nodes.iter().filter(|n| n.kind == k).count();
            eprintln!("  {:>3} {:<32} at ({:>7.0}, {:>7.0}) recipes {:>2} in {:>2} out {:>2}", f.id, f.factory.name.clone().unwrap_or_default(), f.x, f.y, kinds(Kind::Recipe), kinds(Kind::Input), kinds(Kind::Output));
        }
        let mut out: BTreeMap<String, (usize, usize)> = BTreeMap::new();
        for m in &w.machines {
            let planned = m.recipe.as_ref().is_some_and(|r| game.recipes.contains_key(r));
            if !planned && extractor_rate(&game, map.as_ref(), &w, m).is_none() {
                let e = out.entry(m.class.clone()).or_default();
                e.0 += 1;
                e.1 += m.recipe.is_some() as usize;
            }
        }
        eprintln!("left out (class: count, with a recipe set): {out:?}");
        assert!(r.factories > 10 && r.links > 10, "{r:?}");
        eprintln!("poles {}, power wires {}", p.poles.len(), p.power_links.len());
        let mut tiers = [0; 3];
        for q in &p.poles {
            let wires = p.power_links.iter().filter(|l| l.from == End::Pole(q.id) || l.to == End::Pole(q.id)).count();
            assert!(wires <= crate::power_net::max_wires(q.tier), "pole {} Mk.{} has {wires} wires", q.id, q.tier);
            tiers[q.tier as usize - 1] += 1;
        }
        eprintln!("poles by tier (Mk.1, Mk.2, Mk.3): {tiers:?}");
        let poles: BTreeSet<u32> = p.poles.iter().map(|q| q.id).collect();
        assert!(p.power_links.iter().all(|l| [l.from, l.to].iter().all(|e| match e {
            End::Factory(f) => *f as usize <= p.factories.len(),
            End::Pole(q) => poles.contains(q),
        })));
        assert_eq!(p.valid_links().count(), p.links.len());
        let names: BTreeSet<_> = p.factories.iter().map(|f| f.factory.name.clone()).collect();
        assert_eq!(names.len(), p.factories.len(), "names are unique");
    }
}
