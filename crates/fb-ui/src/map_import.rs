//! "Import into planner": machines selected on the map become a factory,
//! grouped by recipe (one node per recipe with its machine count and average
//! clock speed, like the planner's own nodes). Extractors become resource
//! nodes, flows from outside the selection become inputs, and products that
//! leave it become outputs. The result is a normal factory file, so the
//! existing import (beside the current graph, undoable) does the rest.

use crate::portable::{FactoryFile, Kind, Node, Wire};
use fb_model::world::{MapInfo, Machine, World};
use fb_model::{GameData, Purity};
use std::collections::{BTreeMap, BTreeSet};

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
    /// Extractors of one resource.
    Resource(String),
}

fn per_min(r: &fb_model::Recipe, item: &str, list: &[fb_model::Amount]) -> f64 {
    list.iter().filter(|a| a.item == item).map(|a| r.per_min(a)).sum()
}

/// Items per minute an extractor produces on its node.
fn extractor_rate(game: &GameData, map: Option<&MapInfo>, world: &World, m: &Machine) -> Option<(String, f64)> {
    let ex = game.extraction.extractors.iter().find(|e| e.class == m.class)?;
    if ex.kind == fb_model::ExtractorKind::Water {
        // Water extractors pump from any water; no node, no purity.
        return Some(("Desc_Water_C".to_string(), ex.per_min(Purity::Normal, m.clock as f64)));
    }
    let id = m.node.as_deref()?;
    let site = map.and_then(|mi| mi.nodes.iter().find(|n| n.id == id));
    let over = world.node_overrides.iter().find(|o| o.id == id);
    let resource = over.and_then(|o| o.resource.clone()).or_else(|| site.and_then(|s| s.resource.clone()))?;
    let purity = over.and_then(|o| o.purity).or(site.map(|s| s.purity)).unwrap_or(Purity::Normal);
    Some((resource, ex.per_min(purity, m.clock as f64)))
}

pub fn import(world: &World, selected: &BTreeSet<u32>, game: &GameData, map: Option<&MapInfo>) -> (FactoryFile, ImportReport) {
    let mut report = ImportReport::default();
    let mut groups: BTreeMap<Group, Vec<&Machine>> = BTreeMap::new();
    let mut group_of: BTreeMap<u32, Group> = BTreeMap::new();
    let mut resource_rate: BTreeMap<String, f64> = BTreeMap::new();
    for m in world.machines.iter().filter(|m| selected.contains(&m.id)) {
        let g = match &m.recipe {
            Some(r) if game.recipes.contains_key(r) => Group::Recipe(r.clone()),
            _ => match extractor_rate(game, map, world, m) {
                Some((res, rate)) => {
                    *resource_rate.entry(res.clone()).or_default() += rate;
                    Group::Resource(res)
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
                Node { kind: Kind::Recipe, class: r.clone(), machines: Some(count), clock: Some((clock * 100.0 * 1000.0).round() / 1000.0), sloops: Some(common).filter(|s| *s > 0), rate: None, color: None, x: 0.0, y: 0.0 }
            }
            Group::Resource(res) => Node { kind: Kind::Resource, class: res.clone(), machines: None, clock: None, sloops: None, rate: Some((resource_rate[res] * 1000.0).round() / 1000.0), color: None, x: 0.0, y: 0.0 },
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
            Group::Resource(_) => None,
        }
    };
    let out_port = |g: &Group, item: &str| -> Option<usize> {
        match g {
            Group::Recipe(r) => game.recipes[r].products.iter().position(|a| a.item == item),
            Group::Resource(res) => (res == item).then_some(0),
        }
    };

    // Wires between groups, and flows crossing the selection.
    let mut wires: BTreeSet<(usize, usize, usize, usize)> = BTreeSet::new();
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
            Group::Resource(res) => {
                if !inputs && res == item {
                    resource_rate[res]
                } else {
                    0.0
                }
            }
        }
    };
    let mut extra: Vec<(Node, Option<(usize, usize, bool)>)> = Vec::new();
    for (g, item) in &from_outside {
        if let Some(i) = in_port(g, item) {
            let kind = if game.items.get(item).is_some_and(|it| it.raw) { Kind::Resource } else { Kind::Input };
            let r = rate(g, item, true);
            extra.push((Node { kind, class: item.clone(), machines: None, clock: None, sloops: None, rate: Some((r * 1000.0).round() / 1000.0), color: None, x: 0.0, y: 0.0 }, Some((index[g], i, true))));
        }
    }
    for (g, item) in &to_outside {
        if let Some(o) = out_port(g, item) {
            let r = rate(g, item, false);
            extra.push((Node { kind: Kind::Output, class: item.clone(), machines: None, clock: None, sloops: None, rate: Some((r * 1000.0).round() / 1000.0), color: None, x: 0.0, y: 0.0 }, Some((index[g], o, false))));
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
    let heights: Vec<f32> = f.nodes.iter().map(|n| if n.kind == Kind::Recipe { 230.0 } else { 120.0 }).collect();
    let pos = fb_engine::layout::layered_sized(f.nodes.len(), &edges, 300.0, &heights);
    for (n, p) in f.nodes.iter_mut().zip(pos) {
        n.x = p[0];
        n.y = p[1];
    }
    // Bubble order: one wire per pair of ports.
    let mut slots: BTreeMap<(usize, usize), usize> = BTreeMap::new();
    for (from, output, to, input) in wires {
        let slot = slots.entry((from, output)).or_default();
        f.wires.push(Wire { from, output, to, input, slot: *slot });
        *slot += 1;
    }
    report.nodes = f.nodes.len();
    (f, report)
}
