//! Linear-programming planner (good_lp, pure-Rust microlp backend).
//!
//! Variables, all >= 0, per minute:
//!   r_j  machines (100%-clock equivalents) running enabled recipe j
//!   e_i  raw extraction of resource i (bounded by the resource limit)
//!   u_i  externally supplied item i (bounded by the given input rate)
//!   s_i  surplus of item i (byproducts / overflow)
//! For every item:  sum_j net_ij * r_j + e_i + u_i - s_i = target_i
//! When the plan generates its own power (PowerMode::Generate), the allowed
//! generator recipes join the recipes and
//!   sum_j consumed_MW_j * r_j <= sum_g generated_MW_g * r_g
//! (machines at 100% clock), so their fuel chain is planned too.
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

    // Generators only come from the power settings, never from the recipe list.
    let mut recipes: Vec<&Recipe> = req
        .enabled_recipes
        .iter()
        .filter_map(|c| data.recipes.get(c))
        .filter(|r| !r.is_generator())
        .collect();
    let generate = req.power.mode == PowerMode::Generate;
    if generate {
        let generators: BTreeSet<&str> = req
            .power
            .generators
            .iter()
            .filter(|c| data.recipes.get(*c).is_some_and(|r| r.is_generator()))
            .map(|c| c.as_str())
            .collect();
        if generators.is_empty() {
            return SolveOutcome::Infeasible(vec!["Pick at least one generator under Power, or take the power from the grid.".into()]);
        }
        recipes.extend(generators.into_iter().map(|c| &data.recipes[c]));
    }

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
    // Consumed minus generated MW at 100% clock: must be <= 0 in Generate mode.
    let mut net_power = Expression::with_capacity(recipes.len());
    let mut generated = Expression::default();
    for (r, &v) in recipes.iter().zip(&r_vars) {
        power.add_mul(data.recipe_power_mw(r).max(0.1), v);
        machines.add_mul(1.0, v);
        net_power.add_mul(data.recipe_power_mw(r) - r.generates_mw, v);
        if r.is_generator() {
            generated.add_mul(r.generates_mw, v);
        }
    }
    // The extractors' power, per unit of each resource taken.
    for (&item, &v) in &e_vars {
        if let Some(&mw) = req.extraction_mw.get(item).filter(|mw| mw.is_finite() && **mw > 0.0) {
            power.add_mul(mw, v);
            net_power.add_mul(mw, v);
        }
    }
    // Generating its own power, the plan always covers what it uses, so
    // "least power" means the least generating capacity to build (and so the
    // least fuel); resources break ties between generators.
    if generate && req.objective == Objective::Power {
        power = generated;
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
    if generate {
        problem.add_constraint(net_power.leq(0.0));
    }
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
    let (power_mw, generated_mw) = nodes
        .iter()
        .map(|n| match n {
            PlanNodeKind::Recipe { recipe, machines } => {
                let r = &data.recipes[recipe];
                (data.recipe_power_mw(r) * machines, r.generates_mw * machines)
            }
            PlanNodeKind::Resource { item, rate } => (req.extraction_mw.get(item).copied().filter(|m| m.is_finite()).unwrap_or(0.0) * rate, 0.0),
            _ => (0.0, 0.0),
        })
        .fold((0.0, 0.0), |a, b| (a.0 + b.0, a.1 + b.1));
    let positions = layout::layered(nodes.len(), &edges.iter().map(|e| (e.from, e.to)).collect::<Vec<_>>());
    SolveOutcome::Solved(Plan {
        nodes: nodes.into_iter().zip(positions).map(|(kind, pos)| PlanNode { kind, pos }).collect(),
        edges,
        power_mw,
        generated_mw,
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
    if why.is_empty() && req.power.mode == PowerMode::Generate {
        let fuelable = recipes
            .iter()
            .any(|r| r.is_generator() && r.ingredients.iter().all(|a| makeable.contains(a.item.as_str())));
        if !fuelable {
            why.push("None of the allowed generators can get fuel with the enabled recipes and resources. Allow another generator or enable the recipes for its fuel.".into());
        }
    }
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

    const COAL_GEN: &str = "Generator__Build_GeneratorCoal_C__Desc_Coal_C";

    fn machines_of(plan: &Plan, class: &str) -> f64 {
        rate_of(plan, |k| match k {
            PlanNodeKind::Recipe { recipe, machines } if recipe == class => Some(*machines),
            _ => None,
        })
    }

    fn resource(plan: &Plan, class: &str) -> f64 {
        rate_of(plan, |k| match k {
            PlanNodeKind::Resource { item, rate } if item == class => Some(*rate),
            _ => None,
        })
    }

    fn generate(generators: &[&str]) -> PowerSettings {
        PowerSettings { mode: PowerMode::Generate, generators: generators.iter().map(|g| g.to_string()).collect() }
    }

    #[test]
    fn iron_plates_with_their_own_coal_power() {
        let Some(data) = game() else { return };
        let req = SolveRequest {
            targets: vec![Target { item: "Desc_IronPlate_C".into(), rate: 60.0 }],
            enabled_recipes: standard(&data),
            power: generate(&[COAL_GEN]),
            ..Default::default()
        };
        let SolveOutcome::Solved(plan) = solve(&data, &req) else { panic!("infeasible") };
        // Consumption (Docs mPowerConsumption): 3 Smelters x 4 MW + 3 Constructors x 4 MW.
        let smelter = data.buildings["Build_SmelterMk1_C"].power_mw;
        let constructor = data.buildings["Build_ConstructorMk1_C"].power_mw;
        let used = 3.0 * smelter + 3.0 * constructor;
        assert_eq!(used, 24.0);
        assert!((plan.power_mw - used).abs() < 1e-6, "machines still use {} MW", plan.power_mw);
        // Coal-Powered Generator (Docs): mPowerProduction 75 MW; Coal mEnergyValue 300 MJ;
        // mSupplementalToPowerRatio 10 L of water per MJ.
        let gen = &data.recipes[COAL_GEN];
        assert_eq!(gen.generates_mw, 75.0);
        let generators = used / gen.generates_mw; // 24 / 75 = 0.32
        assert!((machines_of(&plan, COAL_GEN) - generators).abs() < 1e-6, "generators {}", machines_of(&plan, COAL_GEN));
        assert!((plan.generated_mw - used).abs() < 1e-6, "generates exactly what it uses: {}", plan.generated_mw);
        // Coal: 0.32 x 75 MW / 300 MJ x 60 = 4.8 per min.
        assert!((resource(&plan, "Desc_Coal_C") - generators * 75.0 / 300.0 * 60.0).abs() < 1e-6);
        assert!((resource(&plan, "Desc_Coal_C") - 4.8).abs() < 1e-6, "coal {}", resource(&plan, "Desc_Coal_C"));
        // Water: 0.32 x 75 MW x 10 L/MJ x 60 s / 1000 = 14.4 m³/min.
        assert!((resource(&plan, "Desc_Water_C") - 14.4).abs() < 1e-6, "water {}", resource(&plan, "Desc_Water_C"));
        assert!((resource(&plan, "Desc_OreIron_C") - 90.0).abs() < 1e-6, "the product side is unchanged");
        // Coal and water flow into the generators.
        let gen_node = plan
            .nodes
            .iter()
            .position(|n| matches!(&n.kind, PlanNodeKind::Recipe { recipe, .. } if recipe == COAL_GEN))
            .unwrap();
        let fed: Vec<&str> = plan.edges.iter().filter(|e| e.to == gen_node).map(|e| e.item.as_str()).collect();
        assert!(fed.contains(&"Desc_Coal_C") && fed.contains(&"Desc_Water_C"), "{fed:?}");
    }

    #[test]
    fn generating_plans_cover_their_extractors_too() {
        let Some(data) = game() else { return };
        // 5 MW per 60 ore (a Miner Mk.1 at 100% on a Normal node).
        let req = SolveRequest {
            targets: vec![Target { item: "Desc_IronPlate_C".into(), rate: 60.0 }],
            enabled_recipes: standard(&data),
            power: generate(&[COAL_GEN]),
            extraction_mw: [("Desc_OreIron_C".to_string(), 5.0 / 60.0)].into_iter().collect(),
            ..Default::default()
        };
        let SolveOutcome::Solved(plan) = solve(&data, &req) else { panic!("infeasible") };
        let ore = rate_of(&plan, |k| match k {
            PlanNodeKind::Resource { item, rate } if item == "Desc_OreIron_C" => Some(*rate),
            _ => None,
        });
        // The coal for the generators is mined too, at no cost here.
        let machines = 24.0;
        assert!((plan.power_mw - (machines + ore * 5.0 / 60.0)).abs() < 1e-6, "power {}", plan.power_mw);
        assert!(plan.generated_mw >= plan.power_mw - 1e-6, "generated {} for {}", plan.generated_mw, plan.power_mw);
    }

    #[test]
    fn grid_mode_never_plans_generators() {
        let Some(data) = game() else { return };
        // Even with every recipe (generators included) in the recipe list.
        let req = SolveRequest {
            targets: vec![Target { item: "Desc_IronPlate_C".into(), rate: 60.0 }],
            enabled_recipes: data.recipes.keys().cloned().collect(),
            ..Default::default()
        };
        let SolveOutcome::Solved(plan) = solve(&data, &req) else { panic!("infeasible") };
        assert!(plan
            .nodes
            .iter()
            .all(|n| !matches!(&n.kind, PlanNodeKind::Recipe { recipe, .. } if data.recipes[recipe].is_generator())));
        assert_eq!(plan.generated_mw, 0.0);
    }

    #[test]
    fn fuel_generators_plan_the_refinery_chain() {
        let Some(data) = game() else { return };
        let fuel_gen = "Generator__Build_GeneratorFuel_C__Desc_LiquidFuel_C";
        let req = SolveRequest {
            targets: vec![Target { item: "Desc_IronPlate_C".into(), rate: 60.0 }],
            enabled_recipes: standard(&data),
            power: generate(&[fuel_gen]),
            ..Default::default()
        };
        let SolveOutcome::Solved(plan) = solve(&data, &req) else { panic!("infeasible") };
        // The refineries making the fuel need power too, so generation covers them as well.
        assert!(plan.power_mw > 24.0, "refineries add to the load: {}", plan.power_mw);
        assert!(plan.generated_mw >= plan.power_mw - 1e-6);
        assert!(resource(&plan, "Desc_LiquidOil_C") > 0.0);
        assert!((machines_of(&plan, fuel_gen) * 250.0 - plan.generated_mw).abs() < 1e-6);
    }

    #[test]
    fn nuclear_waste_is_surplus() {
        let Some(data) = game() else { return };
        let nuclear = "Generator__Build_GeneratorNuclear_C__Desc_NuclearFuelRod_C";
        let req = SolveRequest {
            targets: vec![Target { item: "Desc_IronPlate_C".into(), rate: 600.0 }],
            enabled_recipes: standard(&data),
            power: generate(&[nuclear]),
            ..Default::default()
        };
        let SolveOutcome::Solved(plan) = solve(&data, &req) else { panic!("infeasible") };
        let plants = machines_of(&plan, nuclear);
        assert!(plants > 0.0);
        // 50 Uranium Waste per rod (mByproductAmount), 0.2 rods/min per plant: 10 per min per plant.
        let waste = rate_of(&plan, |k| match k {
            PlanNodeKind::Surplus { item, rate } if item == "Desc_NuclearWaste_C" => Some(*rate),
            _ => None,
        });
        assert!((waste - plants * 10.0).abs() < 1e-4, "waste {waste} for {plants} plants");
        // The fuel rods are made in the plan (from uranium, mined or converted).
        let rods = &data.recipes["Recipe_NuclearFuelRod_C"];
        let rod_makers = plants * 0.2 / rods.per_min(&rods.products[0]);
        assert!((machines_of(&plan, "Recipe_NuclearFuelRod_C") - rod_makers).abs() < 1e-4);
    }

    #[test]
    fn power_objective_builds_the_least_generation() {
        let Some(data) = game() else { return };
        let req = SolveRequest {
            targets: vec![Target { item: "Desc_IronPlate_C".into(), rate: 60.0 }],
            enabled_recipes: standard(&data),
            objective: Objective::Power,
            power: generate(&[COAL_GEN, "Generator__Build_GeneratorFuel_C__Desc_LiquidFuel_C"]),
            ..Default::default()
        };
        let SolveOutcome::Solved(plan) = solve(&data, &req) else { panic!("infeasible") };
        // Coal needs no extra machines, Fuel needs refineries: coal is the least power.
        assert!((plan.generated_mw - 24.0).abs() < 1e-6, "generated {}", plan.generated_mw);
        assert!((machines_of(&plan, COAL_GEN) - 0.32).abs() < 1e-6);
    }

    #[test]
    fn generating_without_generators_is_explained() {
        let Some(data) = game() else { return };
        let mut req = SolveRequest {
            targets: vec![Target { item: "Desc_IronPlate_C".into(), rate: 60.0 }],
            enabled_recipes: standard(&data),
            power: generate(&[]),
            ..Default::default()
        };
        let SolveOutcome::Infeasible(why) = solve(&data, &req) else { panic!("should be infeasible") };
        assert!(why[0].contains("generator"), "{why:?}");
        // Fuel generators, but no recipe that makes Fuel.
        req.power = generate(&["Generator__Build_GeneratorFuel_C__Desc_LiquidFuel_C"]);
        req.enabled_recipes.retain(|r| !r.contains("Fuel"));
        let SolveOutcome::Infeasible(why) = solve(&data, &req) else { panic!("should be infeasible") };
        assert!(why[0].contains("fuel"), "{why:?}");
    }

    #[test]
    fn old_requests_default_to_the_grid() {
        let json = r#"{"targets":[],"inputs":[],"enabled_recipes":[],"resource_limits":{},"objective":"Power"}"#;
        let req: SolveRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.power.mode, PowerMode::Grid);
        assert_eq!(req.power.generators, vec![COAL_GEN.to_string()]);
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
