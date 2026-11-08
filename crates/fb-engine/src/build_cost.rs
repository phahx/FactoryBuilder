//! What the Build Gun charges to place each building, from the Docs.
//!
//! Build recipes are `FGRecipe`s with `mProducedIn` = `BP_BuildGun_C`; their
//! single product is the building's *descriptor* (`Desc_X_C`), which pairs
//! with the building class `Build_X_C`. Pair by product, never by recipe
//! name: `Recipe_SmelterMk1_C` builds the Foundry, `Recipe_SmelterBasicMk1_C`
//! the Smelter.

use crate::docs::{parse_amounts, parse_class_list, str_field};
use fb_model::{Amount, GameData};
use serde_json::Value;
use std::collections::BTreeMap;

pub(crate) fn parse_build_costs(by_native: &BTreeMap<&str, Vec<&Value>>, data: &mut GameData) {
    for c in by_native.get("FGRecipe").into_iter().flatten().copied() {
        if !parse_class_list(str_field(c, "mProducedIn")).iter().any(|p| p == "BP_BuildGun_C") {
            continue;
        }
        let products = parse_amounts(str_field(c, "mProduct"));
        let [(desc, 1.0)] = products.as_slice() else { continue };
        let Some(stem) = desc.strip_prefix("Desc_") else { continue };
        let building = format!("Build_{stem}");
        let cost: Option<Vec<Amount>> = parse_amounts(str_field(c, "mIngredients"))
            .into_iter()
            .map(|(item, amount)| {
                let form = data.items.get(&item)?.form;
                (amount > 0.0 && !form.is_fluid()).then_some(Amount { item, amount })
            })
            .collect();
        if let Some(cost) = cost.filter(|c| !c.is_empty()) {
            data.build_costs.entry(building).or_insert(cost);
        }
    }
}

#[cfg(test)]
mod tests {
    fn game() -> Option<fb_model::GameData> {
        let dir = std::env::var("SATISFACTORY_DIR")
            .unwrap_or_else(|_| r"C:\Program Files (x86)\Steam\steamapps\common\Satisfactory".into());
        crate::docs::load_game_data(std::path::Path::new(&dir)).ok()
    }

    fn cost(g: &fb_model::GameData, b: &str) -> Vec<(String, f64)> {
        g.build_costs[b].iter().map(|a| (a.item.clone(), a.amount)).collect()
    }

    #[test]
    fn build_costs_come_from_build_gun_recipes() {
        let Some(g) = game() else { return };
        // Recipe_ConstructorMk1_C: 2 Reinforced Iron Plate + 8 Cable.
        assert_eq!(
            cost(&g, "Build_ConstructorMk1_C"),
            vec![("Desc_IronPlateReinforced_C".to_string(), 2.0), ("Desc_Cable_C".to_string(), 8.0)]
        );
        // Names cross over: the Foundry's recipe is Recipe_SmelterMk1_C.
        assert_eq!(cost(&g, "Build_FoundryMk1_C")[0], ("Desc_ModularFrame_C".to_string(), 10.0));
        assert_eq!(cost(&g, "Build_SmelterMk1_C")[0], ("Desc_IronRod_C".to_string(), 5.0));
        assert_eq!(
            cost(&g, "Build_MinerMk1_C"),
            vec![
                ("BP_ItemDescriptorPortableMiner_C".to_string(), 1.0),
                ("Desc_IronPlate_C".to_string(), 10.0),
                ("Desc_Cement_C".to_string(), 10.0)
            ]
        );
        // Every machine and extractor has a cost.
        for b in g.buildings.keys().chain(g.extraction.extractors.iter().map(|e| &e.class)) {
            assert!(g.build_costs.contains_key(b), "no build cost for {b}");
        }
    }
}
