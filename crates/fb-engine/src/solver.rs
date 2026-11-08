//! Linear-programming planner (good_lp, pure-Rust microlp backend).
//!
//! Variables, all >= 0, per minute:
//!   r_j  machines (100%-clock equivalents) running enabled recipe j
//!   e_i  raw extraction of resource i (bounded by the resource limit)
//!   u_i  externally supplied item i (bounded by the given input rate)
//!   s_i  surplus of item i (byproducts / overflow)
//! For every item:  sum_j net_ij * r_j + e_i + u_i - s_i = target_i
//! The objective picks which of the many feasible plans is "best".

use crate::layout;
use fb_model::*;
use good_lp::{microlp, variable, Expression, ProblemVariables, ResolutionError, Solution, SolverModel, Variable};
use std::collections::{BTreeMap, BTreeSet};

const EPS: f64 = 1e-7;
/// Weight per m³ of water: unlimited, but don't use it for nothing.
const WATER_WEIGHT: f64 = 1e-4;
const WATER: &str = "Desc_Water_C";

pub fn solve(data: &GameData, req: &SolveRequest) -> SolveOutcome {
    let targets: Vec<&Target> = req.targets.iter().filter(|t| t.rate > EPS).collect();
    if targets.is_empty() {
        return SolveOutcome::Infeasible(vec!["Add at least one product with a rate above 0.".into()]);
    }

    let recipes: Vec<&Recipe> = req
        .enabled_recipes
        .iter()
        .filter_map(|c| data.recipes.get(c))
        .collect();

    // Items that appear anywhere in the problem.
    let mut items: BTreeSet<&str> = BTreeSet::new();
    for r in &recipes {
        for a in r.ingredients.iter().chain(&r.products) {
            items.insert(&a.item);
        }
    }
    for t in targets.iter().copied().chain(&req.inputs) {
        items.insert(&t.item);
    }

    let mut vars = ProblemVariables::new();
    let r_vars: Vec<Variable> = recipes.iter().map(|_| vars.add(variable().min(0.0))).collect();

    let limit_of = |item: &str| -> Option<f64> {
        if item == WATER {
            return Some(f64::INFINITY);
        }
        req.resource_limits.get(item).copied().or_else(|| data.resource_limits.get(item).copied())
    };

    let mut e_vars: BTreeMap<&str, Variable> = BTreeMap::new();
    for &item in &items {
        if data.items.get(item).is_some_and(|i| i.raw) {
            let limit = limit_of(item).unwrap_or(0.0);
            if limit > 0.0 {
                let def = if limit.is_finite() { variable().min(0.0).max(limit) } else { variable().min(0.0) };
                e_vars.insert(item, vars.add(def));
            }
        }
    }
    let mut u_vars: BTreeMap<&str, Variable> = BTreeMap::new();
    for t in req.inputs.iter().filter(|t| t.rate > EPS) {
        u_vars.insert(&t.item, vars.add(variable().min(0.0).max(t.rate)));
    }
    let s_vars: BTreeMap<&str, Variable> = items.iter().map(|&i| (i, vars.add(variable().min(0.0)))).collect();

    // ---- objective
    let max_limit = data.resource_limits.values().copied().fold(1.0, f64::max);
    let resource_cost = |item: &str| -> f64 {
        if item == WATER {
            WATER_WEIGHT
        } else {
            // Scarce resources cost more: uranium ~44x iron.
            max_limit / limit_of(item).unwrap_or(max_limit).max(1.0) * 1e-3
        }
    };
    let mut resources = Expression::with_capacity(e_vars.len());
    for (&item, &v) in &e_vars {
        resources.add_mul(resource_cost(item), v);
    }
    let mut power = Expression::with_capacity(recipes.len());
    let mut machines = Expression::with_capacity(recipes.len());
    for (r, &v) in recipes.iter().zip(&r_vars) {
        power.add_mul(data.recipe_power_mw(r).max(0.1), v);
        machines.add_mul(1.0, v);
    }
    let mut surplus = Expression::with_capacity(s_vars.len());
    for &v in s_vars.values() {
        surplus.add_mul(1.0, v);
    }
    // Small tie-breakers keep zero-cost loops (package/unpackage, ...) and
    // pointless overproduction out of the answer.
    let objective = match req.objective {
        Objective::Resources => resources + 1e-6 * power + 1e-5 * machines + 1e-6 * surplus,
        Objective::Power => power + 1e-3 * resources + 1e-6 * surplus,
        Objective::Buildings => machines + 1e-3 * resources + 1e-6 * power + 1e-6 * surplus,
    };

    // ---- balance constraints
    let mut balance: BTreeMap<&str, Expression> = items.iter().map(|&i| (i, Expression::default())).collect();
    for (r, &v) in recipes.iter().zip(&r_vars) {
        for a in &r.products {
            balance.get_mut(a.item.as_str()).unwrap().add_mul(r.per_min(a), v);
        }
        for a in &r.ingredients {
            balance.get_mut(a.item.as_str()).unwrap().add_mul(-r.per_min(a), v);
        }
    }
    let mut problem = vars.minimise(objective.clone()).using(microlp);
    for (&item, expr) in &balance {
        let mut e = expr.clone();
        if let Some(&v) = e_vars.get(item) {
            e.add_mul(1.0, v);
        }
        if let Some(&v) = u_vars.get(item) {
            e.add_mul(1.0, v);
        }
        e.add_mul(-1.0, s_vars[item]);
        let target: f64 = targets.iter().filter(|t| t.item == item).map(|t| t.rate).sum();
        problem.add_constraint(e.eq(target));
    }

    let solution = match problem.solve() {
        Ok(s) => s,
        Err(ResolutionError::Infeasible) => return SolveOutcome::Infeasible(explain_infeasible(data, req, &recipes)),
        Err(e) => return SolveOutcome::Infeasible(vec![format!("Solver error: {e}")]),
    };

    // ---- turn the solution into plan nodes
    let mut nodes: Vec<PlanNodeKind> = Vec::new();
    for (r, &v) in recipes.iter().zip(&r_vars) {
        let m = solution.value(v);
        if m > EPS {
            nodes.push(PlanNodeKind::Recipe { recipe: r.class.clone(), machines: clean(m) });
        }
    }
    for (&item, &v) in &e_vars {
        let x = solution.value(v);
        if x > EPS {
            nodes.push(PlanNodeKind::Resource { item: item.into(), rate: clean(x) });
        }
    }
    for (&item, &v) in &u_vars {
        let x = solution.value(v);
        if x > EPS {
            nodes.push(PlanNodeKind::Input { item: item.into(), rate: clean(x) });
        }
    }
    for t in &targets {
        nodes.push(PlanNodeKind::Output { item: t.item.clone(), rate: t.rate });
    }
    for (&item, &v) in &s_vars {
        let x = solution.value(v);
        if x > 1e-5 {
            nodes.push(PlanNodeKind::Surplus { item: item.into(), rate: clean(x) });
        }
    }

    let edges = distribute(data, &nodes);
    let power_mw = nodes
        .iter()
        .map(|n| match n {
            PlanNodeKind::Recipe { recipe, machines } => data.recipe_power_mw(&data.recipes[recipe]) * machines,
            _ => 0.0,
        })
        .sum();
    let positions = layout::layered(nodes.len(), &edges.iter().map(|e| (e.from, e.to)).collect::<Vec<_>>());
    SolveOutcome::Solved(Plan {
        nodes: nodes.into_iter().zip(positions).map(|(kind, pos)| PlanNode { kind, pos }).collect(),
        edges,
        power_mw,
    })
}

fn clean(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

/// Per-item production and consumption of a plan node, per minute.
pub fn node_flows(data: &GameData, n: &PlanNodeKind) -> (Vec<(String, f64)>, Vec<(String, f64)>) {
    match n {
        PlanNodeKind::Recipe { recipe, machines } => {
            let r = &data.recipes[recipe];
            (
                r.products.iter().map(|a| (a.item.clone(), r.per_min(a) * machines)).collect(),
                r.ingredients.iter().map(|a| (a.item.clone(), r.per_min(a) * machines)).collect(),
            )
        }
        PlanNodeKind::Resource { item, rate } | PlanNodeKind::Input { item, rate } => {
            (vec![(item.clone(), *rate)], vec![])
        }
        PlanNodeKind::Output { item, rate } | PlanNodeKind::Surplus { item, rate } => {
            (vec![], vec![(item.clone(), *rate)])
        }
    }
}

/// Splits each item's production across its consumers in proportion,
/// giving one edge per (producer, consumer, item).
fn distribute(data: &GameData, nodes: &[PlanNodeKind]) -> Vec<PlanEdge> {
    let mut producers: BTreeMap<String, Vec<(usize, f64)>> = BTreeMap::new();
    let mut consumers: BTreeMap<String, Vec<(usize, f64)>> = BTreeMap::new();
    for (i, n) in nodes.iter().enumerate() {
        let (out, inp) = node_flows(data, n);
        for (item, rate) in out {
            producers.entry(item).or_default().push((i, rate));
        }
        for (item, rate) in inp {
            consumers.entry(item).or_default().push((i, rate));
        }
    }
    let mut edges = Vec::new();
    for (item, prods) in &producers {
        let Some(cons) = consumers.get(item) else { continue };
        let total: f64 = prods.iter().map(|p| p.1).sum();
        if total <= EPS {
            continue;
        }
        for &(p, prate) in prods {
            for &(c, crate_) in cons {
                let rate = prate * crate_ / total;
                if rate > 1e-6 && p != c {
                    edges.push(PlanEdge { from: p, to: c, item: item.clone(), rate: clean(rate) });
                }
            }
        }
    }
    edges
}

/// Explains why no plan exists, in terms the player can act on.
fn explain_infeasible(data: &GameData, req: &SolveRequest, recipes: &[&Recipe]) -> Vec<String> {
    let mut makeable: BTreeSet<&str> = BTreeSet::new();
    makeable.insert(WATER);
    for (item, it) in &data.items {
        if it.raw && req.resource_limits.get(item).or(data.resource_limits.get(item)).copied().unwrap_or(0.0) > 0.0 {
            makeable.insert(item);
        }
    }
    for t in &req.inputs {
        makeable.insert(&t.item);
    }
    loop {
        let before = makeable.len();
        for r in recipes {
            if r.ingredients.iter().all(|a| makeable.contains(a.item.as_str())) {
                for p in &r.products {
                    makeable.insert(&p.item);
                }
            }
        }
        if makeable.len() == before {
            break;
        }
    }
    let mut why: Vec<String> = req
        .targets
        .iter()
        .filter(|t| !makeable.contains(t.item.as_str()))
        .map(|t| format!("{} can't be made with the enabled recipes and resources.", data.item_name(&t.item)))
        .collect();
    if why.is_empty() {
        why.push("Not enough raw resources for these rates. Lower the targets, raise resource limits, or enable alternate recipes.".into());
    }
    why
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game() -> Option<GameData> {
        let dir = std::env::var("SATISFACTORY_DIR")
            .unwrap_or_else(|_| r"C:\Program Files (x86)\Steam\steamapps\common\Satisfactory".into());
        crate::docs::load_game_data(std::path::Path::new(&dir)).ok()
    }

    fn standard(data: &GameData) -> Vec<String> {
        data.recipes.values().filter(|r| !r.alternate).map(|r| r.class.clone()).collect()
    }

    fn rate_of(plan: &Plan, f: impl Fn(&PlanNodeKind) -> Option<f64>) -> f64 {
        plan.nodes.iter().filter_map(|n| f(&n.kind)).sum()
    }

    #[test]
    fn iron_plates_from_real_docs() {
        let Some(data) = game() else {
            eprintln!("Satisfactory not found; skipping");
            return;
        };
        // Iron Ingot: 1 ore -> 1 ingot / 2 s; Iron Plate: 3 ingot -> 2 plate / 6 s.
        let req = SolveRequest {
            targets: vec![Target { item: "Desc_IronPlate_C".into(), rate: 60.0 }],
            enabled_recipes: standard(&data),
            ..Default::default()
        };
        let SolveOutcome::Solved(plan) = solve(&data, &req) else { panic!("infeasible") };
        let ore = rate_of(&plan, |k| match k {
            PlanNodeKind::Resource { item, rate } if item == "Desc_OreIron_C" => Some(*rate),
            _ => None,
        });
        assert!((ore - 90.0).abs() < 1e-6, "ore {ore}");
        let plates_machines = rate_of(&plan, |k| match k {
            PlanNodeKind::Recipe { recipe, machines } if recipe == "Recipe_IronPlate_C" => Some(*machines),
            _ => None,
        });
        assert!((plates_machines - 3.0).abs() < 1e-6, "constructors {plates_machines}");
        // 3 smelters (4 MW) + 3 constructors (4 MW)
        assert!((plan.power_mw - 24.0).abs() < 1e-6, "power {}", plan.power_mw);
    }

    #[test]
    fn plastic_has_byproduct_surplus() {
        let Some(data) = game() else { return };
        let req = SolveRequest {
            targets: vec![Target { item: "Desc_Plastic_C".into(), rate: 20.0 }],
            enabled_recipes: vec!["Recipe_Plastic_C".into()],
            ..Default::default()
        };
        let SolveOutcome::Solved(plan) = solve(&data, &req) else { panic!("infeasible") };
        // Plastic: 3 oil -> 2 plastic + 1 heavy oil residue per 6 s.
        let residue = rate_of(&plan, |k| match k {
            PlanNodeKind::Surplus { item, rate } if item == "Desc_HeavyOilResidue_C" => Some(*rate),
            _ => None,
        });
        assert!((residue - 10.0).abs() < 1e-6, "residue {residue}");
    }

    #[test]
    fn missing_recipe_is_explained() {
        let Some(data) = game() else { return };
        let req = SolveRequest {
            targets: vec![Target { item: "Desc_IronPlate_C".into(), rate: 10.0 }],
            enabled_recipes: vec!["Recipe_IronPlate_C".into()],
            ..Default::default()
        };
        let SolveOutcome::Infeasible(why) = solve(&data, &req) else { panic!("should be infeasible") };
        assert!(why[0].contains("Iron Plate"), "{why:?}");
    }

    #[test]
    fn heavy_modular_frames_solve_with_all_alternates() {
        let Some(data) = game() else { return };
        let req = SolveRequest {
            targets: vec![Target { item: "Desc_ModularFrameHeavy_C".into(), rate: 10.0 }],
            enabled_recipes: data.recipes.keys().cloned().collect(),
            ..Default::default()
        };
        let t = std::time::Instant::now();
        let SolveOutcome::Solved(plan) = solve(&data, &req) else { panic!("infeasible") };
        eprintln!("HMF: {} nodes, {} edges, {:.1} MW in {:?}", plan.nodes.len(), plan.edges.len(), plan.power_mw, t.elapsed());
        assert!(plan.nodes.len() > 3);
    }
}
