//! Which recipes a player has: from the schematics bought in a save, or
//! everything standard up to a HUB tier.

use fb_model::{ClassName, GameData, SchematicKind};
use std::collections::BTreeSet;

/// `/Game/.../Schematic_X.Schematic_X_C` -> `Schematic_X_C`.
pub fn schematic_class(path: &str) -> &str {
    path.rsplit('.').next().unwrap_or(path)
}

/// Production recipes unlocked by these schematics (classes or paths).
pub fn recipes_from_schematics<'a>(data: &GameData, schematics: impl IntoIterator<Item = &'a str>) -> BTreeSet<ClassName> {
    let mut out = BTreeSet::new();
    for s in schematics {
        if let Some(s) = data.schematics.get(schematic_class(s)) {
            out.extend(s.recipes.iter().cloned());
        }
    }
    out
}

/// The highest HUB tier in the game data.
pub fn max_tier(data: &GameData) -> u32 {
    data.schematics.values().filter(|s| s.kind == SchematicKind::Milestone).map(|s| s.tier).max().unwrap_or(0)
}

/// Standard recipes available once every HUB milestone up to `tier` is done:
/// those milestones, the tutorial, the starting recipes and the game's own
/// tier bundles, plus MAM research made in a machine unlocked by then.
/// Alternates (hard drives) are never included.
pub fn recipes_up_to_tier(data: &GameData, tier: u32) -> BTreeSet<ClassName> {
    let in_tier = |k: SchematicKind, t: u32| matches!(k, SchematicKind::Milestone | SchematicKind::Tutorial | SchematicKind::Custom) && t <= tier;
    let mut recipes = BTreeSet::new();
    let mut buildings = BTreeSet::new();
    for s in data.schematics.values().filter(|s| in_tier(s.kind, s.tier)) {
        recipes.extend(s.recipes.iter().cloned());
        buildings.extend(s.buildings.iter().cloned());
    }
    for s in data.schematics.values().filter(|s| s.kind == SchematicKind::Mam) {
        for r in &s.recipes {
            if data.recipes.get(r).is_some_and(|r| buildings.contains(&r.building)) {
                recipes.insert(r.clone());
            }
        }
    }
    recipes.retain(|r| data.recipes.get(r).is_some_and(|r| !r.alternate));
    recipes
}

#[cfg(all(test, feature = "solver"))]
mod tests {
    use super::*;

    fn game() -> Option<GameData> {
        let dir = std::env::var("SATISFACTORY_DIR").unwrap_or_else(|_| r"C:\Program Files (x86)\Steam\steamapps\common\Satisfactory".into());
        crate::docs::load_game_data(std::path::Path::new(&dir)).ok()
    }

    #[test]
    fn every_production_recipe_has_an_unlock() {
        let Some(g) = game() else { return };
        let unlocked: BTreeSet<&String> = g.schematics.values().flat_map(|s| s.recipes.iter()).collect();
        let missing: Vec<&String> = g.recipes.keys().filter(|r| !unlocked.contains(r)).collect();
        assert!(missing.is_empty(), "{missing:?}");
        let s = &g.schematics["Schematic_Alternate_Screw_C"];
        assert_eq!(s.kind, SchematicKind::Alternate);
        assert_eq!(s.recipes, vec!["Recipe_Alternate_Screw_C".to_string()]);
    }

    #[test]
    fn tiers_grow_and_gate_research_by_machine() {
        let Some(g) = game() else { return };
        assert_eq!(max_tier(&g), 9);
        let t0 = recipes_up_to_tier(&g, 0);
        assert!(t0.contains("Recipe_IngotIron_C") && t0.contains("Recipe_IronPlate_C"), "starting recipes");
        assert!(!t0.contains("Recipe_Motor_C"), "motors need a later tier");
        let mut last = 0;
        for t in 0..=9 {
            let n = recipes_up_to_tier(&g, t).len();
            assert!(n >= last, "tier {t} has fewer recipes than the one before");
            last = n;
        }
        let all = recipes_up_to_tier(&g, 9);
        let standard = g.recipes.values().filter(|r| !r.alternate).count();
        eprintln!("standard recipes reachable by tier 9: {} of {standard}", all.len());
        assert!(all.iter().all(|r| !g.recipes[r].alternate));
        // Caterium Ingot is MAM research, made in a Smelter (tier 0).
        assert!(recipes_up_to_tier(&g, 2).contains("Recipe_IngotCaterium_C"));
    }
}
