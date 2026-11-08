//! Power generators from the Docs file, as synthesized "generator recipes"
//! so the planner, graph, flow simulation and save format treat them like
//! any other machine.
//!
//! One recipe per (generator building, fuel), class
//! `Generator__<building>__<fuel>` ([`fb_model::GENERATOR_PREFIX`]). Its
//! cycle is the time one fuel item (or one m³ of fluid fuel) lasts:
//!
//! * `mPowerProduction` (generator, MW) and `mEnergyValue` (fuel item, MJ;
//!   per litre for fluids, so x1000 per m³; FGItemDescriptor.h: "Energy
//!   value for this resource if used as fuel. In megawatt seconds (MWs)")
//!   give `duration_s = energy / MW`, so fuel per min = MW / energy x 60.
//! * The supplemental resource (water) comes from the fuel entry's
//!   `mSupplementalResourceClass` and the generator's
//!   `mSupplementalToPowerRatio`, "the quantity of supplemental resource to
//!   consume per megawatt of power produced" (FGBuildableGeneratorFuel.h),
//!   i.e. litres per MJ: litres per cycle = MW x ratio x duration_s.
//! * The byproduct (nuclear waste) is the fuel entry's `mByproduct`, with
//!   `mByproductAmount` items per fuel item burned.
//!
//! Geothermal Generators are left out: they burn no fuel and their output
//! depends on the geyser's purity, which the Docs file doesn't list (it has
//! only `mVariablePowerProductionConstant`/`Factor`), and the planner doesn't
//! model geyser nodes.
//!
//! Generators whose fuel can't be made at all from raw resources with the
//! game's recipes (the Biomass Burner's leaves, wood, mycelia, ...) are left
//! out too, since the planner could never fuel them.

use fb_model::{Amount, Building, GameData, Recipe, GENERATOR_PREFIX};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Fuel-burning generator classes (the nuclear plant is a fuel generator
/// with a byproduct). Geothermal is deliberately absent (see module docs).
const GENERATOR_CLASSES: &[&str] = &["FGBuildableGeneratorFuel", "FGBuildableGeneratorNuclear"];
const WATER: &str = "Desc_Water_C";

fn native_name(s: &str) -> &str {
    let s = s.trim_end_matches('\'');
    s.rsplit('.').next().unwrap_or(s)
}

fn str_field<'a>(c: &'a Value, key: &str) -> &'a str {
    c.get(key).and_then(Value::as_str).unwrap_or("")
}

/// A finite number, or None for empty/unparseable values.
fn num_field(c: &Value, key: &str) -> Option<f64> {
    str_field(c, key).trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Class name of the generator recipe for `building` burning `fuel`.
pub fn generator_class(building: &str, fuel: &str) -> String {
    format!("{GENERATOR_PREFIX}{building}__{fuel}")
}

/// Adds the generator buildings and their (building, fuel) recipes to `data`.
/// Expects items, recipes and resource limits to be parsed already.
pub fn add_generators(root: &Value, data: &mut GameData) {
    let Some(natives) = root.as_array() else { return };
    let mut energy: BTreeMap<&str, f64> = BTreeMap::new();
    let mut generators: Vec<&Value> = Vec::new();
    for n in natives {
        let native = native_name(str_field(n, "NativeClass"));
        let Some(classes) = n.get("Classes").and_then(Value::as_array) else { continue };
        for c in classes {
            if let Some(e) = num_field(c, "mEnergyValue") {
                let class = str_field(c, "ClassName");
                if e > 0.0 && data.items.contains_key(class) {
                    energy.insert(class, e);
                }
            }
        }
        if GENERATOR_CLASSES.contains(&native) {
            generators.extend(classes.iter());
        }
    }

    let mut candidates: Vec<(Building, Recipe)> = Vec::new();
    for g in generators {
        let class = str_field(g, "ClassName");
        let mw = num_field(g, "mPowerProduction").unwrap_or(0.0);
        if class.is_empty() || mw <= 0.0 {
            continue;
        }
        let building = Building {
            class: class.to_string(),
            name: str_field(g, "mDisplayName").to_string(),
            power_mw: num_field(g, "mPowerConsumption").unwrap_or(0.0).max(0.0),
            power_exponent: num_field(g, "mPowerConsumptionExponent").unwrap_or(1.0),
            icon: class.strip_prefix("Build_").map(|s| format!("Desc_{s}")).unwrap_or_else(|| class.to_string()),
        };
        let ratio = num_field(g, "mSupplementalToPowerRatio").unwrap_or(0.0).max(0.0);
        let Some(fuels) = g.get("mFuel").and_then(Value::as_array) else { continue };
        for f in fuels {
            let fuel = str_field(f, "mFuelClass");
            let (Some(item), Some(&e)) = (data.items.get(fuel), energy.get(fuel)) else { continue };
            // MJ per item, or per m³ for fluids (the Docs value is per litre).
            let per_unit = if item.form.is_fluid() { e * 1000.0 } else { e };
            let duration_s = per_unit / mw;
            if !duration_s.is_finite() || duration_s <= 0.0 {
                continue;
            }
            let mut ingredients = vec![Amount { item: fuel.to_string(), amount: 1.0 }];
            let supplemental = str_field(f, "mSupplementalResourceClass");
            if !supplemental.is_empty() && ratio > 0.0 {
                let Some(s) = data.items.get(supplemental) else { continue };
                let litres = mw * ratio * duration_s;
                let amount = if s.form.is_fluid() { litres / 1000.0 } else { litres };
                ingredients.push(Amount { item: supplemental.to_string(), amount });
            }
            let mut products = Vec::new();
            let byproduct = str_field(f, "mByproduct");
            if !byproduct.is_empty() {
                let (Some(b), Some(n)) = (data.items.get(byproduct), num_field(f, "mByproductAmount")) else { continue };
                // Per fuel item; no fluid fuel has a byproduct, and what it
                // would mean per litre isn't documented, so don't guess.
                if item.form.is_fluid() || b.form.is_fluid() || n <= 0.0 {
                    continue;
                }
                products.push(Amount { item: byproduct.to_string(), amount: n });
            }
            let recipe = Recipe {
                class: generator_class(class, fuel),
                name: format!("{} · {}", building.name, item.name),
                alternate: false,
                duration_s,
                ingredients,
                products,
                building: class.to_string(),
                variable_power_mw: None,
                generates_mw: mw,
            };
            candidates.push((building.clone(), recipe));
        }
    }

    // Keep only generators whose fuel can be made from raw resources.
    let mut makeable: BTreeSet<String> = BTreeSet::new();
    makeable.insert(WATER.to_string());
    for (class, item) in &data.items {
        if item.raw && data.resource_limits.get(class).copied().unwrap_or(0.0) > 0.0 {
            makeable.insert(class.clone());
        }
    }
    let all: Vec<&Recipe> = data.recipes.values().chain(candidates.iter().map(|c| &c.1)).collect();
    loop {
        let before = makeable.len();
        for r in &all {
            if r.ingredients.iter().all(|a| makeable.contains(&a.item)) {
                for p in &r.products {
                    makeable.insert(p.item.clone());
                }
            }
        }
        if makeable.len() == before {
            break;
        }
    }
    for (building, recipe) in candidates {
        if recipe.ingredients.iter().all(|a| makeable.contains(&a.item)) {
            data.buildings.entry(building.class.clone()).or_insert(building);
            data.recipes.insert(recipe.class.clone(), recipe);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn docs() -> Option<(Value, GameData)> {
        let dir = std::env::var("SATISFACTORY_DIR")
            .unwrap_or_else(|_| r"C:\Program Files (x86)\Steam\steamapps\common\Satisfactory".into());
        let bytes = std::fs::read(crate::docs::docs_path(std::path::Path::new(&dir))).ok()?;
        let body = bytes.strip_prefix(&[0xFF, 0xFE]).unwrap_or(&bytes);
        let units: Vec<u16> = body.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let root: Value = serde_json::from_str(&String::from_utf16(&units).ok()?).ok()?;
        let data = crate::docs::parse_docs(&root).ok()?;
        Some((root, data))
    }

    /// A raw Docs class by name.
    fn class<'a>(root: &'a Value, name: &str) -> &'a Value {
        root.as_array()
            .unwrap()
            .iter()
            .flat_map(|n| n["Classes"].as_array().unwrap())
            .find(|c| c["ClassName"] == name)
            .unwrap_or_else(|| panic!("{name} not in Docs"))
    }

    fn f(c: &Value, key: &str) -> f64 {
        c[key].as_str().unwrap().parse().unwrap()
    }

    fn per_min(r: &Recipe, item: &str) -> f64 {
        r.ingredients.iter().chain(&r.products).filter(|a| a.item == item).map(|a| r.per_min(a)).sum()
    }

    #[test]
    fn coal_generator_rates_come_from_the_docs() {
        let Some((root, data)) = docs() else {
            eprintln!("Satisfactory not found; skipping");
            return;
        };
        let gen = class(&root, "Build_GeneratorCoal_C");
        let mw = f(gen, "mPowerProduction");
        let ratio = f(gen, "mSupplementalToPowerRatio");
        let coal_mj = f(class(&root, "Desc_Coal_C"), "mEnergyValue");
        assert_eq!((mw, ratio, coal_mj), (75.0, 10.0, 300.0), "Docs values this test was written against");
        let r = &data.recipes["Generator__Build_GeneratorCoal_C__Desc_Coal_C"];
        assert_eq!(r.name, "Coal-Powered Generator · Coal");
        assert!(r.is_generator() && r.products.is_empty());
        assert_eq!(r.generates_mw, mw);
        // 75 MW / 300 MJ per coal = 0.25 coal/s = 15 per min.
        assert!((per_min(r, "Desc_Coal_C") - mw / coal_mj * 60.0).abs() < 1e-9);
        assert!((per_min(r, "Desc_Coal_C") - 15.0).abs() < 1e-9);
        // 75 MW x 10 L/MJ = 750 L/s = 45 m³/min.
        assert!((per_min(r, "Desc_Water_C") - mw * ratio * 60.0 / 1000.0).abs() < 1e-9);
        assert!((per_min(r, "Desc_Water_C") - 45.0).abs() < 1e-9);
        assert_eq!(data.buildings["Build_GeneratorCoal_C"].power_mw, 0.0, "generators draw nothing");
        assert_eq!(data.buildings["Build_GeneratorCoal_C"].icon, "Desc_GeneratorCoal_C");
    }

    #[test]
    fn fuel_and_nuclear_generators() {
        let Some((root, data)) = docs() else { return };
        // Fluid fuel: mEnergyValue is per litre. 250 MW / (0.75 MJ/L x 1000) = 1/3 m³/s = 20 m³/min.
        let fuel_mw = f(class(&root, "Build_GeneratorFuel_C"), "mPowerProduction");
        let fuel_mj = f(class(&root, "Desc_LiquidFuel_C"), "mEnergyValue");
        assert_eq!((fuel_mw, fuel_mj), (250.0, 0.75));
        let r = &data.recipes["Generator__Build_GeneratorFuel_C__Desc_LiquidFuel_C"];
        assert!((per_min(r, "Desc_LiquidFuel_C") - 20.0).abs() < 1e-9);
        assert_eq!(r.ingredients.len(), 1, "no water for fuel generators");

        // Nuclear: 2500 MW / 750000 MJ = 0.2 rods/min, 50 waste per rod = 10/min,
        // 2500 x 1.6 L/MJ = 4000 L/s = 240 m³/min.
        let gen = class(&root, "Build_GeneratorNuclear_C");
        let (mw, ratio) = (f(gen, "mPowerProduction"), f(gen, "mSupplementalToPowerRatio"));
        let rod_mj = f(class(&root, "Desc_NuclearFuelRod_C"), "mEnergyValue");
        assert_eq!((mw, ratio, rod_mj), (2500.0, 1.6, 750000.0));
        let r = &data.recipes["Generator__Build_GeneratorNuclear_C__Desc_NuclearFuelRod_C"];
        assert!((per_min(r, "Desc_NuclearFuelRod_C") - 0.2).abs() < 1e-9);
        assert!((per_min(r, "Desc_NuclearWaste_C") - 10.0).abs() < 1e-9);
        assert!((per_min(r, "Desc_Water_C") - 240.0).abs() < 1e-9);
        assert!(data.recipes.contains_key("Generator__Build_GeneratorNuclear_C__Desc_PlutoniumFuelRod_C"), "plutonium rods are made from uranium waste");
    }

    #[test]
    fn unfuelable_and_geothermal_are_left_out() {
        let Some((_, data)) = docs() else { return };
        let gens: Vec<&str> = data.recipes.keys().filter(|k| k.starts_with(GENERATOR_PREFIX)).map(|k| k.as_str()).collect();
        eprintln!("{} generator options: {gens:?}", gens.len());
        assert!(!gens.iter().any(|g| g.contains("GeoThermal")));
        assert!(!gens.contains(&"Generator__Build_GeneratorBiomass_Automated_C__Desc_Leaves_C"), "leaves can't be made");
        assert!(gens.contains(&"Generator__Build_GeneratorCoal_C__Desc_PetroleumCoke_C"));
        for g in &gens {
            assert!(!data.recipes[*g].alternate);
        }
    }
}
