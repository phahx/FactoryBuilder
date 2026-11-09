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
    import_with(world, selected, game, map, false)
}

/// `from_factories`: what comes in from outside the selection comes from
/// another factory (an input), even raw resources (otherwise those are
/// resource nodes, mined for this factory).
fn import_with(world: &World, selected: &BTreeSet<u32>, game: &GameData, map: Option<&MapInfo>, from_factories: bool) -> (FactoryFile, ImportReport) {
    let mut report = ImportReport::default();
    let mut groups: BTreeMap<Group, Vec<&Machine>> = BTreeMap::new();
    let mut group_of: BTreeMap<u32, Group> = BTreeMap::new();
    let mut resource_rate: BTreeMap<(String, u8), f64> = BTreeMap::new();
    let mut group_rate: BTreeMap<Group, f64> = BTreeMap::new();
    for m in world.machines.iter().filter(|m| selected.contains(&m.id)) {
        let g = match &m.recipe {
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
                Node { kind: Kind::Recipe, class: r.clone(), machines: Some(count), clock: Some((clock * 100.0 * 1000.0).round() / 1000.0), sloops: Some(common).filter(|s| *s > 0), rate: None, name: None, color: None, item: None, purity: None, x: 0.0, y: 0.0 }
            }
            Group::Resource(res, p) => Node { kind: Kind::Resource, class: res.clone(), machines: None, clock: None, sloops: None, rate: Some((resource_rate[&(res.clone(), *p)] * 1000.0).round() / 1000.0), name: None, color: None, item: None, purity: Purity::ALL.get(*p as usize).copied(), x: 0.0, y: 0.0 },
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
                }
            }
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
        }
    };
    let out_port = |g: &Group, item: &str| -> Option<usize> {
        match g {
            Group::Recipe(r) => game.recipes[r].products.iter().position(|a| a.item == item),
            Group::Resource(res, _) | Group::Extractor(res, ..) => (res == item).then_some(0),
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
        }
    };
    let mut extra: Vec<(Node, Option<(usize, usize, bool)>)> = Vec::new();
    for (g, item) in &from_outside {
        if let Some(i) = in_port(g, item) {
            let kind = if !from_factories && game.items.get(item).is_some_and(|it| it.raw) { Kind::Resource } else { Kind::Input };
            let r = rate(g, item, true);
            extra.push((Node { kind, class: item.clone(), machines: None, clock: None, sloops: None, rate: Some((r * 1000.0).round() / 1000.0), name: None, color: None, item: None, purity: None, x: 0.0, y: 0.0 }, Some((index[g], i, true))));
        }
    }
    for (g, item) in &to_outside {
        if let Some(o) = out_port(g, item) {
            let r = rate(g, item, false);
            extra.push((Node { kind: Kind::Output, class: item.clone(), machines: None, clock: None, sloops: None, rate: Some((r * 1000.0).round() / 1000.0), name: None, color: None, item: None, purity: None, x: 0.0, y: 0.0 }, Some((index[g], o, false))));
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

    // Layered layout, left to right in flow order.
    let edges: Vec<(usize, usize)> = wires.iter().map(|w| (w.0, w.2)).collect();
    let heights: Vec<f32> = f.nodes.iter().map(|n| if matches!(n.kind, Kind::Recipe | Kind::Extractor) { 230.0 } else { 120.0 }).collect();
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
    (f, report)
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
        let (mut f, r) = import_with(world, &set, game, map, true);
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
        let ports = f.nodes.iter().filter(|n| n.kind == Kind::Input).count().max(f.nodes.iter().filter(|n| n.kind == Kind::Output).count());
        boxes.push((cx, cy, 320.0, 130.0 + 34.0 * ports as f32));
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

/// Poles for each power circuit that joins two or more factories with
/// machines (`factory_of`: machine id -> factory id). Every one gets a pole at
/// a corner of its block, in the gaps between blocks: top right for one
/// that makes more power than it uses (in the save), where its Power output
/// is, top left for the rest. Factories whose corners meet share a pole.
/// Each circuit's poles are joined pole to nearest pole (a minimum spanning
/// tree), so its wires run short, along the gaps. Returns the poles, the
/// wires and how many circuits.
fn power_poles(world: &World, factory_of: &HashMap<u32, u32>, factories: &[ProjectFactory], boxes: &[(f32, f32, f32, f32)]) -> (Vec<Pole>, Vec<PowerLink>, usize) {
    let mut on: BTreeMap<u32, BTreeSet<u32>> = BTreeMap::new();
    let mut balance: HashMap<u32, f32> = HashMap::new();
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
