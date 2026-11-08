//! Parser for the game's own data dump:
//! `<Satisfactory>/CommunityResources/Docs/en-US.json` (UTF-16 LE with BOM).
//!
//! Values in that file are Unreal text-exported strings, e.g.
//! `((ItemClass="/Script/Engine.BlueprintGeneratedClass'/Game/.../Desc_OreIron.Desc_OreIron_C'",Amount=1))`

use fb_model::{Amount, Building, Form, GameData, Item, Recipe};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug)]
pub enum DocsError {
    Io(std::io::Error),
    Encoding,
    Json(serde_json::Error),
    Shape(&'static str),
}

impl std::fmt::Display for DocsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DocsError::Io(e) => write!(f, "could not read Docs file: {e}"),
            DocsError::Encoding => write!(f, "Docs file is not UTF-16 LE"),
            DocsError::Json(e) => write!(f, "Docs file is not valid JSON: {e}"),
            DocsError::Shape(s) => write!(f, "unexpected Docs layout: {s}"),
        }
    }
}
impl std::error::Error for DocsError {}

/// Docs path inside a Satisfactory install folder.
pub fn docs_path(game_dir: &Path) -> std::path::PathBuf {
    game_dir.join("CommunityResources").join("Docs").join("en-US.json")
}

pub fn load_game_data(game_dir: &Path) -> Result<GameData, DocsError> {
    let bytes = std::fs::read(docs_path(game_dir)).map_err(DocsError::Io)?;
    parse_docs_bytes(&bytes)
}

pub fn parse_docs_bytes(bytes: &[u8]) -> Result<GameData, DocsError> {
    let text = decode_utf16le(bytes)?;
    let root: Value = serde_json::from_str(&text).map_err(DocsError::Json)?;
    parse_docs(&root)
}

fn decode_utf16le(bytes: &[u8]) -> Result<String, DocsError> {
    let body = bytes.strip_prefix(&[0xFF, 0xFE]).unwrap_or(bytes);
    if body.len() % 2 != 0 {
        return Err(DocsError::Encoding);
    }
    let units: Vec<u16> = body.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    String::from_utf16(&units).map_err(|_| DocsError::Encoding)
}

/// World extraction limits per minute: every node on the map with a
/// Miner Mk.3 / extractor at 250%. From satisfactory.wiki.gg (Resource node
/// and Nitrogen Gas pages, patch 1.2.x). These are not in the Docs file.
/// Water is unlimited (Water Extractors can go anywhere there's water).
pub const WORLD_LIMITS: &[(&str, f64)] = &[
    ("Desc_OreIron_C", 92_100.0),
    ("Desc_OreCopper_C", 36_900.0),
    ("Desc_Stone_C", 69_300.0),
    ("Desc_Coal_C", 42_300.0),
    ("Desc_OreGold_C", 15_000.0),
    ("Desc_RawQuartz_C", 13_500.0),
    ("Desc_Sulfur_C", 10_800.0),
    ("Desc_OreBauxite_C", 12_300.0),
    ("Desc_OreUranium_C", 2_100.0),
    ("Desc_SAM_C", 10_200.0),
    ("Desc_LiquidOil_C", 12_600.0),
    ("Desc_NitrogenGas_C", 12_000.0),
];

const ITEM_CLASSES: &[&str] = &[
    "FGItemDescriptor",
    "FGResourceDescriptor",
    "FGItemDescriptorBiomass",
    "FGItemDescriptorNuclearFuel",
    "FGItemDescriptorPowerBoosterFuel",
    "FGConsumableDescriptor",
    "FGEquipmentDescriptor",
    "FGPowerShardDescriptor",
    "FGAmmoTypeProjectile",
    "FGAmmoTypeInstantHit",
    "FGAmmoTypeSpreadshot",
];

const MANUFACTURER_CLASSES: &[&str] = &["FGBuildableManufacturer", "FGBuildableManufacturerVariablePower"];

/// `/Script/CoreUObject.Class'/Script/FactoryGame.FGRecipe'` -> `FGRecipe`
fn native_name(s: &str) -> &str {
    let s = s.trim_end_matches('\'');
    s.rsplit('.').next().unwrap_or(s)
}

pub(crate) fn str_field<'a>(c: &'a Value, key: &str) -> &'a str {
    c.get(key).and_then(Value::as_str).unwrap_or("")
}

pub(crate) fn f64_field(c: &Value, key: &str) -> f64 {
    str_field(c, key).trim().parse().unwrap_or(0.0)
}

/// Parses `((ItemClass="...Desc_X.Desc_X_C'",Amount=3),(...))`.
pub fn parse_amounts(s: &str) -> Vec<(String, f64)> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(i) = rest.find("ItemClass=") {
        rest = &rest[i + "ItemClass=".len()..];
        // The class path ends at the first `"` after the opening one, or `,`.
        let end = rest[1..].find('"').map(|e| e + 1).or_else(|| rest.find(',')).unwrap_or(rest.len());
        let path = rest[..end].trim_matches(|c| c == '"' || c == '\'');
        let class = class_from_path(path);
        rest = &rest[end..];
        let amount = rest
            .find("Amount=")
            .map(|a| {
                let num = &rest[a + "Amount=".len()..];
                let n_end = num.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-')).unwrap_or(num.len());
                num[..n_end].parse().unwrap_or(0.0)
            })
            .unwrap_or(0.0);
        out.push((class.to_string(), amount));
    }
    out
}

/// `/Game/A/B/Desc_X.Desc_X_C'` -> `Desc_X_C`
fn class_from_path(path: &str) -> &str {
    let path = path.trim_matches(|c| c == '"' || c == '\'' || c == '(' || c == ')');
    path.rsplit('.').next().unwrap_or(path)
}

/// Parses `("/Game/.../Build_X.Build_X_C","/Script/FactoryGame.Y")` into class names.
pub fn parse_class_list(s: &str) -> Vec<String> {
    s.trim_matches(|c| c == '(' || c == ')')
        .split(',')
        .map(class_from_path)
        .filter(|c| !c.is_empty())
        .map(str::to_string)
        .collect()
}

/// `(B=21,G=125,R=235,A=255)` -> [r, g, b, a]
fn parse_color(s: &str) -> [u8; 4] {
    let mut rgba = [0u8; 4];
    for part in s.trim_matches(|c| c == '(' || c == ')').split(',') {
        if let Some((k, v)) = part.split_once('=') {
            let v = v.parse::<u8>().unwrap_or(0);
            match k {
                "R" => rgba[0] = v,
                "G" => rgba[1] = v,
                "B" => rgba[2] = v,
                "A" => rgba[3] = v,
                _ => {}
            }
        }
    }
    rgba
}

pub fn parse_docs(root: &Value) -> Result<GameData, DocsError> {
    let natives = root.as_array().ok_or(DocsError::Shape("root is not an array"))?;
    let mut by_native: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();
    for n in natives {
        let name = native_name(str_field(n, "NativeClass"));
        let classes = n.get("Classes").and_then(Value::as_array).ok_or(DocsError::Shape("missing Classes"))?;
        by_native.entry(name).or_default().extend(classes.iter());
    }
    let classes_of = |n: &str| by_native.get(n).cloned().unwrap_or_default();

    let mut data = GameData::default();

    for native in ITEM_CLASSES {
        let raw = *native == "FGResourceDescriptor";
        for c in classes_of(native) {
            let form = match str_field(c, "mForm") {
                "RF_SOLID" => Form::Solid,
                "RF_LIQUID" => Form::Liquid,
                "RF_GAS" => Form::Gas,
                _ => continue,
            };
            let class = str_field(c, "ClassName").to_string();
            data.items.insert(
                class.clone(),
                Item {
                    class,
                    name: str_field(c, "mDisplayName").to_string(),
                    description: str_field(c, "mDescription").to_string(),
                    form,
                    sink_points: str_field(c, "mResourceSinkPoints").parse().unwrap_or(0),
                    raw,
                    color: parse_color(str_field(c, "mFluidColor")),
                },
            );
        }
    }

    for native in MANUFACTURER_CLASSES {
        for c in classes_of(native) {
            let class = str_field(c, "ClassName").to_string();
            let icon = class.strip_prefix("Build_").map(|s| format!("Desc_{s}")).unwrap_or_else(|| class.clone());
            // Somersloop slots. The Docs list the Smelter with 0 slots although
            // it can be boosted (mCanChangeProductionBoost) and takes one
            // Somersloop at +100% in the game
            // (https://satisfactory.wiki.gg/wiki/Production_amplifier).
            let boostable = str_field(c, "mCanChangeProductionBoost") == "True";
            let mut sloop_slots = str_field(c, "mProductionShardSlotSize").trim().parse::<u32>().unwrap_or(0);
            let mut sloop_boost = f64_field(c, "mProductionShardBoostMultiplier");
            if !boostable {
                sloop_slots = 0;
            } else if sloop_slots == 0 {
                sloop_slots = 1;
                sloop_boost = 1.0;
            }
            data.buildings.insert(
                class.clone(),
                Building {
                    class,
                    name: str_field(c, "mDisplayName").to_string(),
                    power_mw: f64_field(c, "mPowerConsumption"),
                    power_exponent: f64_field(c, "mPowerConsumptionExponent"),
                    icon,
                    sloop_slots,
                    sloop_boost,
                    sloop_power_exponent: Some(f64_field(c, "mProductionBoostPowerConsumptionExponent")).filter(|e| *e > 0.0).unwrap_or(2.0),
                },
            );
        }
    }

    for c in classes_of("FGRecipe") {
        let produced_in = parse_class_list(str_field(c, "mProducedIn"));
        let Some(building) = produced_in.into_iter().find(|b| data.buildings.contains_key(b)) else {
            continue; // build-gun, workbench-only, equipment workshop recipes
        };
        let convert = |list: Vec<(String, f64)>| -> Option<Vec<Amount>> {
            list.into_iter()
                .map(|(item, amount)| {
                    let form = data.items.get(&item)?.form;
                    let amount = if form.is_fluid() { amount / 1000.0 } else { amount };
                    Some(Amount { item, amount })
                })
                .collect()
        };
        let (Some(ingredients), Some(products)) = (
            convert(parse_amounts(str_field(c, "mIngredients"))),
            convert(parse_amounts(str_field(c, "mProduct"))),
        ) else {
            continue; // produces something that isn't an item (buildings etc.)
        };
        let duration_s = f64_field(c, "mManufactoringDuration");
        if duration_s <= 0.0 || products.is_empty() {
            continue;
        }
        let constant = f64_field(c, "mVariablePowerConsumptionConstant");
        let factor = f64_field(c, "mVariablePowerConsumptionFactor");
        let variable = data.buildings[&building].power_mw == 0.0;
        let class = str_field(c, "ClassName").to_string();
        let name = str_field(c, "mDisplayName").to_string();
        data.recipes.insert(
            class.clone(),
            Recipe {
                alternate: class.starts_with("Recipe_Alternate_") || name.starts_with("Alternate:"),
                class,
                name,
                duration_s,
                ingredients,
                products,
                building,
                // Power ramps linearly from `constant` to `constant + factor`
                // over each cycle, so the average is the midpoint.
                variable_power_mw: variable.then_some(constant + factor / 2.0),
                generates_mw: 0.0,
            },
        );
    }

    for (item, limit) in WORLD_LIMITS {
        if data.items.contains_key(*item) {
            data.resource_limits.insert(item.to_string(), *limit);
        }
    }
    crate::power::add_generators(root, &mut data);

    crate::schematics::parse(&classes_of("FGSchematic"), &classes_of("FGRecipe"), &mut data);
    crate::extraction::parse_extraction(&by_native, &mut data);
    crate::build_cost::parse_build_costs(&by_native, &mut data);

    if data.recipes.is_empty() || data.items.is_empty() {
        return Err(DocsError::Shape("no recipes or items found"));
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts_parse() {
        let s = r#"((ItemClass="/Script/Engine.BlueprintGeneratedClass'/Game/FactoryGame/Resource/RawResources/OreIron/Desc_OreIron.Desc_OreIron_C'",Amount=7),(ItemClass="/Script/Engine.BlueprintGeneratedClass'/Game/FactoryGame/Resource/RawResources/Water/Desc_Water.Desc_Water_C'",Amount=4000))"#;
        assert_eq!(
            parse_amounts(s),
            vec![("Desc_OreIron_C".to_string(), 7.0), ("Desc_Water_C".to_string(), 4000.0)]
        );
    }

    #[test]
    fn class_list_parse() {
        let s = r#"("/Game/FactoryGame/Buildable/Factory/SmelterMk1/Build_SmelterMk1.Build_SmelterMk1_C","/Script/FactoryGame.FGBuildableAutomatedWorkBench")"#;
        assert_eq!(parse_class_list(s), vec!["Build_SmelterMk1_C", "FGBuildableAutomatedWorkBench"]);
    }

    #[test]
    fn color_parse() {
        assert_eq!(parse_color("(B=21,G=125,R=235,A=255)"), [235, 125, 21, 255]);
    }
}
