//! Schematics from the Docs file: what each HUB milestone, MAM research,
//! hard-drive alternate and so on unlocks (production recipes and buildings).

use crate::docs::parse_class_list;
use fb_model::{GameData, Schematic, SchematicKind};
use serde_json::Value;
use std::collections::BTreeMap;

/// Build-gun recipes produce a building's descriptor (`Desc_X_C`); the
/// building itself is `Build_X_C`.
const BUILD_GUN: &str = "BuildGun";

fn str_field<'a>(c: &'a Value, key: &str) -> &'a str {
    c.get(key).and_then(Value::as_str).unwrap_or("")
}

fn kind(t: &str) -> SchematicKind {
    match t {
        "EST_Milestone" => SchematicKind::Milestone,
        "EST_MAM" => SchematicKind::Mam,
        "EST_Alternate" => SchematicKind::Alternate,
        "EST_Tutorial" => SchematicKind::Tutorial,
        "EST_Custom" => SchematicKind::Custom,
        _ => SchematicKind::Other,
    }
}

/// Fills `data.schematics`; `recipes` are all `FGRecipe` classes (production
/// recipes are already in `data.recipes`).
pub fn parse(schematics: &[&Value], recipes: &[&Value], data: &mut GameData) {
    // Build recipe -> building it places.
    let mut builds: BTreeMap<&str, String> = BTreeMap::new();
    for r in recipes {
        if !str_field(r, "mProducedIn").contains(BUILD_GUN) {
            continue;
        }
        let product = crate::docs::parse_amounts(str_field(r, "mProduct"));
        if let Some((desc, _)) = product.first() {
            if let Some(rest) = desc.strip_prefix("Desc_") {
                builds.insert(str_field(r, "ClassName"), format!("Build_{rest}"));
            }
        }
    }
    for s in schematics {
        let class = str_field(s, "ClassName");
        if class.is_empty() {
            continue;
        }
        let mut out = Schematic {
            class: class.to_string(),
            name: str_field(s, "mDisplayName").to_string(),
            kind: kind(str_field(s, "mType")),
            tier: str_field(s, "mTechTier").trim().parse().unwrap_or(0),
            recipes: Vec::new(),
            buildings: Vec::new(),
        };
        for unlock in s.get("mUnlocks").and_then(Value::as_array).into_iter().flatten() {
            for r in parse_class_list(str_field(unlock, "mRecipes")) {
                if data.recipes.contains_key(&r) {
                    out.recipes.push(r);
                } else if let Some(b) = builds.get(r.as_str()) {
                    out.buildings.push(b.clone());
                }
            }
        }
        data.schematics.insert(out.class.clone(), out);
    }
}
