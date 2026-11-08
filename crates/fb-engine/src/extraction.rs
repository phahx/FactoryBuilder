//! Extractor stats and belt speeds from the Docs.
//!
//! Verified against the game's en-US.json:
//! - `FGBuildableResourceExtractor` (Miner Mk.1-3, Oil Extractor),
//!   `FGBuildableWaterPump` (Water Extractor) and
//!   `FGBuildableFrackingExtractor` (Resource Well Extractor) carry
//!   `mExtractCycleTime`, `mItemsPerCycle` (fluids in litres, so /1000 for m³),
//!   `mAllowedResourceForms`, `mOnlyAllowCertainResources`,
//!   `mAllowedResources`, `mPowerConsumption`, `mPowerConsumptionExponent`.
//! - Belts: `FGBuildableConveyorBelt.mSpeed` is half-items per minute
//!   (Mk.1 = 120 -> 60 items/min).

use crate::docs::{f64_field, parse_class_list, str_field};
use fb_model::{Belt, Extractor, ExtractorKind, Form, GameData};
use serde_json::Value;
use std::collections::BTreeMap;

const EXTRACTOR_NATIVES: &[(&str, ExtractorKind)] = &[
    ("FGBuildableResourceExtractor", ExtractorKind::Node),
    ("FGBuildableWaterPump", ExtractorKind::Water),
    ("FGBuildableFrackingExtractor", ExtractorKind::Well),
];

fn parse_forms(s: &str) -> Vec<Form> {
    let mut out = Vec::new();
    for (tag, form) in [("RF_SOLID", Form::Solid), ("RF_LIQUID", Form::Liquid), ("RF_GAS", Form::Gas)] {
        if s.contains(tag) {
            out.push(form);
        }
    }
    out
}

pub(crate) fn parse_extraction(by_native: &BTreeMap<&str, Vec<&Value>>, data: &mut GameData) {
    let mut extractors = Vec::new();
    for (native, kind) in EXTRACTOR_NATIVES {
        for c in by_native.get(native).into_iter().flatten().copied() {
            let class = str_field(c, "ClassName").to_string();
            let forms = parse_forms(str_field(c, "mAllowedResourceForms"));
            let cycle_s = f64_field(c, "mExtractCycleTime");
            let mut per_cycle = f64_field(c, "mItemsPerCycle");
            if class.is_empty() || forms.is_empty() || !(cycle_s > 0.0) || !(per_cycle > 0.0) {
                continue;
            }
            if forms.iter().any(|f| f.is_fluid()) {
                per_cycle /= 1000.0;
            }
            let resources = if str_field(c, "mOnlyAllowCertainResources") == "True" {
                parse_class_list(str_field(c, "mAllowedResources"))
            } else {
                Vec::new()
            };
            let icon = class.strip_prefix("Build_").map(|s| format!("Desc_{s}")).unwrap_or_else(|| class.clone());
            extractors.push(Extractor {
                name: str_field(c, "mDisplayName").to_string(),
                kind: *kind,
                cycle_s,
                per_cycle,
                forms,
                resources,
                power_mw: f64_field(c, "mPowerConsumption"),
                power_exponent: f64_field(c, "mPowerConsumptionExponent"),
                icon,
                class,
            });
        }
    }
    extractors.sort_by(|a, b| a.base_per_min().total_cmp(&b.base_per_min()).then(a.class.cmp(&b.class)));
    data.extraction.extractors = extractors;

    let mut belts: Vec<Belt> = by_native
        .get("FGBuildableConveyorBelt")
        .into_iter()
        .flatten()
        .copied()
        .filter_map(|c| {
            let speed = f64_field(c, "mSpeed");
            (speed > 0.0).then(|| Belt { name: str_field(c, "mDisplayName").to_string(), per_min: speed / 2.0 })
        })
        .collect();
    belts.sort_by(|a, b| a.per_min.total_cmp(&b.per_min));
    data.extraction.belts = belts;
}

#[cfg(test)]
mod tests {
    use fb_model::{ExtractorKind, Form, GameData, Purity};

    fn game() -> Option<GameData> {
        let dir = std::env::var("SATISFACTORY_DIR")
            .unwrap_or_else(|_| r"C:\Program Files (x86)\Steam\steamapps\common\Satisfactory".into());
        crate::docs::load_game_data(std::path::Path::new(&dir)).ok()
    }

    /// Rates derived from the Docs fields, cross-checked with the wiki tables
    /// (satisfactory.wiki.gg/wiki/Miner: 60/120/240 at Normal;
    /// /wiki/Resource_well: 30/60/120 m3/min).
    #[test]
    fn extractor_rates_derive_from_docs() {
        let Some(g) = game() else { return };
        let ex = |c: &str| g.extraction.extractors.iter().find(|e| e.class == c).unwrap_or_else(|| panic!("{c}"));
        // Miner Mk.1: 1 item per 1 s cycle = 60/min.
        let m1 = ex("Build_MinerMk1_C");
        assert_eq!((m1.cycle_s, m1.per_cycle), (1.0, 1.0));
        assert_eq!(m1.per_min(Purity::Normal, 1.0), 60.0);
        assert_eq!(m1.per_min(Purity::Impure, 1.0), 30.0);
        assert_eq!(m1.per_min(Purity::Pure, 2.5), 300.0);
        // Mk.2: 0.5 s cycle, Mk.3: 0.25 s cycle.
        assert_eq!(ex("Build_MinerMk2_C").per_min(Purity::Normal, 1.0), 120.0);
        assert_eq!(ex("Build_MinerMk3_C").per_min(Purity::Pure, 2.5), 1200.0);
        assert!(m1.can_extract("Desc_OreIron_C", Form::Solid));
        assert!(!m1.can_extract("Desc_Water_C", Form::Liquid));
        // Oil Extractor: 2000 L per 1 s = 120 m3/min on a normal node.
        let oil = ex("Build_OilPump_C");
        assert_eq!(oil.per_min(Purity::Normal, 1.0), 120.0);
        assert!(oil.can_extract("Desc_LiquidOil_C", Form::Liquid));
        assert!(!oil.can_extract("Desc_NitrogenGas_C", Form::Gas));
        // Water Extractor ignores purity: 120 m3/min.
        let w = ex("Build_WaterPump_C");
        assert_eq!(w.kind, ExtractorKind::Water);
        assert_eq!(w.per_min(Purity::Impure, 1.0), 120.0);
        // Resource Well Extractor: 1000 L per 1 s = 60 m3/min Normal.
        let well = ex("Build_FrackingExtractor_C");
        assert_eq!(well.per_min(Purity::Impure, 1.0), 30.0);
        assert_eq!(well.per_min(Purity::Pure, 1.0), 120.0);
        assert!(well.can_extract("Desc_NitrogenGas_C", Form::Gas));
        // Belts: Mk.1 = 60/min ... Mk.6 = 1200/min.
        let belts = &g.extraction.belts;
        assert_eq!(belts.len(), 6);
        assert_eq!(belts[0].per_min, 60.0);
        assert_eq!(belts[5].per_min, 1200.0);
    }
}
