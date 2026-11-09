//! Overview sections for extraction ("Raw resources" as extractors) and the
//! total build cost. Pure planning functions first, then the egui parts.
//!
//! Sources: extractor stats and build costs come from the game's Docs (see
//! `fb-engine/src/extraction.rs` and `build_cost.rs`). Power Shards: each
//! one raises the maximum clock speed by 50%, up to 3, so 250% needs 3
//! (https://satisfactory.wiki.gg/wiki/Clock_speed). Node purity multipliers:
//! see `fb_model::Purity`.

use crate::assets::Assets;
use crate::fmt::{fmt_num, fmt_rate_unit};
use crate::theme::*;
use crate::widgets;
use egui::{Id, RichText, Ui};
use fb_model::{Extractor, ExtractorKind, Form, GameData, Purity};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MIN_CLOCK_PCT: f64 = 1.0;
pub const MAX_CLOCK_PCT: f64 = 250.0;

/// The user's extractor choices; saved with the project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExtractionSettings {
    /// Miner building class, e.g. `Build_MinerMk1_C`.
    pub miner: String,
    pub purity: Purity,
    /// Miner clock speed in percent, 1 to 250.
    pub clock_pct: f64,
}

impl Default for ExtractionSettings {
    fn default() -> Self {
        Self { miner: "Build_MinerMk1_C".into(), purity: Purity::Normal, clock_pct: 100.0 }
    }
}

impl ExtractionSettings {
    pub fn clock(&self) -> f64 {
        if self.clock_pct.is_finite() {
            self.clock_pct.clamp(MIN_CLOCK_PCT, MAX_CLOCK_PCT) / 100.0
        } else {
            1.0
        }
    }
}

/// Power Shards one machine needs to run at `clock` (1.0 = 100%):
/// 1 per 50% above 100%.
pub fn shards_per_machine(clock: f64) -> u64 {
    (((clock - 1.0) * 100.0 / 50.0) - 1e-9).ceil().max(0.0) as u64
}

#[derive(Debug, Clone, PartialEq)]
pub enum BeltNote {
    /// Not a belt resource (fluids go in pipes).
    None,
    /// One extractor's output fits on this belt tier at the smallest.
    Fits(String),
    /// One extractor's output is more than the fastest belt carries.
    TooFast { belt: String, belt_per_min: f64 },
}

#[derive(Debug, Clone)]
pub struct ExtractPlan {
    pub item: String,
    pub rate: f64,
    pub class: String,
    pub name: String,
    pub count: u64,
    /// None for Water Extractors.
    pub purity: Option<Purity>,
    pub clock: f64,
    pub per_each: f64,
    pub shards_total: u64,
    pub belt: BeltNote,
    /// Other miner tiers at the same purity and clock: (name, count).
    pub tiers: Vec<(String, u64)>,
}

fn count_for(rate: f64, per_each: f64) -> u64 {
    if per_each <= 0.0 {
        return 0;
    }
    ((rate / per_each) - 1e-6).ceil().max(1.0) as u64
}

fn pick_extractor<'a>(game: &'a GameData, item: &str, form: Form, s: &ExtractionSettings) -> Option<&'a Extractor> {
    let all = &game.extraction.extractors;
    if form == Form::Solid {
        let chosen = all.iter().find(|e| e.class == s.miner && e.can_extract(item, form));
        return chosen.or_else(|| all.iter().find(|e| e.kind == ExtractorKind::Node && e.can_extract(item, form)));
    }
    [ExtractorKind::Water, ExtractorKind::Node, ExtractorKind::Well]
        .into_iter()
        .find_map(|k| all.iter().find(|e| e.kind == k && e.can_extract(item, form)))
}

/// How to extract `rate` per minute of `item`, or None if nothing can.
pub fn plan_resource(game: &GameData, item: &str, rate: f64, s: &ExtractionSettings) -> Option<ExtractPlan> {
    if !(rate > 0.0) || !rate.is_finite() {
        return None;
    }
    let form = game.item_form(item);
    let e = pick_extractor(game, item, form, s)?;
    let solid = form == Form::Solid;
    // Only miners are clocked here; fluid extractors run at 100%.
    let clock = if solid { s.clock() } else { 1.0 };
    let per_each = e.per_min(s.purity, clock);
    let count = count_for(rate, per_each);
    let belt = if solid {
        let belts = &game.extraction.belts;
        match belts.iter().find(|b| b.per_min + 1e-9 >= per_each) {
            Some(b) => BeltNote::Fits(b.name.clone()),
            None => match belts.last() {
                Some(b) => BeltNote::TooFast { belt: b.name.clone(), belt_per_min: b.per_min },
                None => BeltNote::None,
            },
        }
    } else {
        BeltNote::None
    };
    let tiers = if solid {
        game.extraction
            .extractors
            .iter()
            .filter(|x| x.kind == ExtractorKind::Node && x.can_extract(item, form))
            .map(|x| (x.name.clone(), count_for(rate, x.per_min(s.purity, clock))))
            .collect()
    } else {
        Vec::new()
    };
    Some(ExtractPlan {
        item: item.to_string(),
        rate,
        class: e.class.clone(),
        name: e.name.clone(),
        count,
        purity: (e.kind != ExtractorKind::Water).then_some(s.purity),
        clock,
        per_each,
        shards_total: if solid { count * shards_per_machine(clock) } else { 0 },
        belt,
        tiers,
    })
}

pub fn plan_all(game: &GameData, resources: &BTreeMap<String, f64>, s: &ExtractionSettings) -> Vec<ExtractPlan> {
    resources.iter().filter_map(|(item, rate)| plan_resource(game, item, *rate, s)).collect()
}

/// "4 Miner Mk.2 on Normal nodes at 100%"
pub fn plan_text(p: &ExtractPlan, solid: bool) -> String {
    let mut t = format!("{} {}", fmt_num(p.count as f64), p.name);
    if let Some(purity) = p.purity {
        t.push_str(&format!(" on {} nodes", purity.label()));
    }
    if solid {
        t.push_str(&format!(" at {}%", fmt_num((p.clock * 100.0).round())));
    }
    if p.shards_total > 0 {
        t.push_str(&format!(", {} Power Shard{}", p.shards_total, if p.shards_total == 1 { "" } else { "s" }));
    }
    t
}

/// Whole machines in `buildings` plus the extractors for `resources`, as
/// item -> amount, using the Build Gun costs from the Docs.
pub fn build_cost_totals(
    game: &GameData,
    buildings: &BTreeMap<String, (f64, f64)>,
    plans: &[ExtractPlan],
) -> BTreeMap<String, f64> {
    let mut out: BTreeMap<String, f64> = BTreeMap::new();
    let mut add = |class: &str, n: f64| {
        if let Some(cost) = game.build_costs.get(class) {
            for a in cost {
                *out.entry(a.item.clone()).or_default() += a.amount * n;
            }
        }
    };
    for (class, (whole, _)) in buildings {
        add(class, whole.max(0.0));
    }
    for p in plans {
        add(&p.class, p.count as f64);
    }
    out
}

// ------------------------------------------------------------------ egui

fn short_tier(name: &str) -> &str {
    name.strip_prefix("Miner ").unwrap_or(name)
}

/// "Raw resources" as extractors, with the settings that drive them.
/// Returns true when a setting changed.
pub fn extraction(
    ui: &mut Ui,
    assets: &mut Assets,
    game: &GameData,
    resources: &BTreeMap<String, f64>,
    extracted: &BTreeMap<String, Vec<String>>,
    settings: &mut ExtractionSettings,
) -> bool {
    let mut changed = false;
    if resources.is_empty() {
        return false;
    }
    widgets::divider(ui);
    widgets::section(ui, "Raw resources", None);
    let miners: Vec<&Extractor> = game
        .extraction
        .extractors
        .iter()
        .filter(|e| e.kind == ExtractorKind::Node && e.forms.contains(&Form::Solid))
        .collect();
    if !miners.is_empty() {
        if !miners.iter().any(|m| m.class == settings.miner) {
            settings.miner = miners[0].class.clone();
        }
        let current = miners.iter().find(|m| m.class == settings.miner).map(|m| m.name.as_str()).unwrap_or("");
        let summary = format!("{current}, {}, {}%", settings.purity.label(), fmt_num(settings.clock_pct));
        widgets::collapsible(ui, assets, "extractor-settings", "Extractors", &summary, |ui, _| {
            widgets::caption(ui, "What Plan factory mines with. Purity applies to every resource except Water.");
            ui.add_space(4.0);
            ui.label(RichText::new("Miner").color(TEXT));
            let mut idx = miners.iter().position(|m| m.class == settings.miner).unwrap_or(0);
            let labels: Vec<String> = miners.iter().map(|m| short_tier(&m.name).to_string()).collect();
            let tips: Vec<String> = miners
                .iter()
                .map(|m| format!("{} makes {} per min on a Normal node at 100% clock speed.", m.name, fmt_num(m.base_per_min())))
                .collect();
            let opts: Vec<(usize, &str, &str)> =
                (0..miners.len()).map(|i| (i, labels[i].as_str(), tips[i].as_str())).collect();
            if widgets::tabs(ui, &mut idx, &opts) {
                settings.miner = miners[idx].class.clone();
                changed = true;
            }
            ui.add_space(6.0);
            ui.label(RichText::new("Node purity").color(TEXT));
            let tip = |p: Purity| format!("A {} node gives {}x the output.", p.label(), fmt_num(p.multiplier()));
            let (ti, tn, tp) = (tip(Purity::Impure), tip(Purity::Normal), tip(Purity::Pure));
            if widgets::tabs(
                ui,
                &mut settings.purity,
                &[(Purity::Impure, "Impure", &ti), (Purity::Normal, "Normal", &tn), (Purity::Pure, "Pure", &tp)],
            ) {
                changed = true;
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Clock Speed").color(TEXT));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new("%").color(TEXT_DIM));
                    let mut v = settings.clock_pct;
                    if widgets::num_field(ui, Id::new("extractor-clock"), &mut v, 56.0) {
                        settings.clock_pct = v.clamp(MIN_CLOCK_PCT, MAX_CLOCK_PCT);
                        changed = true;
                    }
                });
            });
            let shards = shards_per_machine(settings.clock());
            widgets::caption(
                ui,
                &if shards == 0 {
                    "1 to 250%. Above 100% each miner needs Power Shards: 1 per 50%. Oil and Nitrogen Gas extractors stay at 100%.".to_string()
                } else {
                    format!(
                        "Each miner needs {shards} Power Shard{}. 1 to 250%; 1 Power Shard per 50% above 100%. Oil and Nitrogen Gas extractors stay at 100%.",
                        if shards == 1 { "" } else { "s" }
                    )
                },
            );
        });
    }
    let plans: BTreeMap<String, ExtractPlan> =
        plan_all(game, &not_extracted(resources, extracted), settings).into_iter().map(|p| (p.item.clone(), p)).collect();
    for (item, rate) in resources {
        let rate_text = RichText::new(fmt_rate_unit(*rate, game.item_form(item))).color(TEXT);
        widgets::info_row(ui, assets, item, game.item_name(item), rate_text);
        // Extractor nodes in the graph say what extracts it.
        if let Some(lines) = extracted.get(item) {
            ui.horizontal(|ui| {
                ui.add_space(30.0);
                ui.vertical(|ui| {
                    for l in lines {
                        ui.add(egui::Label::new(RichText::new(l).small().color(TEXT)).wrap());
                    }
                });
            });
            ui.add_space(4.0);
            continue;
        }
        let Some(p) = plans.get(item) else { continue };
        let solid = game.item_form(item) == Form::Solid;
        ui.horizontal(|ui| {
            ui.add_space(30.0);
            ui.vertical(|ui| {
                let mut tip = format!(
                    "One {} makes {}.",
                    p.name,
                    fmt_rate_unit(p.per_each, game.item_form(item))
                );
                match &p.belt {
                    BeltNote::Fits(b) => tip.push_str(&format!(" That fits a {b}.")),
                    BeltNote::TooFast { belt, belt_per_min } => tip.push_str(&format!(
                        " That is more than a {belt} carries ({} per min).",
                        fmt_num(*belt_per_min)
                    )),
                    BeltNote::None => {}
                }
                ui.add(egui::Label::new(RichText::new(plan_text(p, solid)).small().color(TEXT)).wrap())
                    .on_hover_text(tip);
                if let BeltNote::TooFast { belt, .. } = &p.belt {
                    ui.add(
                        egui::Label::new(
                            RichText::new(format!("One extractor outputs more than a {belt} carries."))
                                .small()
                                .color(ACCENT),
                        )
                        .wrap(),
                    );
                }
                if p.tiers.len() > 1 {
                    let list: Vec<String> =
                        p.tiers.iter().map(|(n, c)| format!("{} {}", fmt_num(*c as f64), short_tier(n))).collect();
                    ui.add(
                        egui::Label::new(RichText::new(format!("Or {}", list.join(" · "))).small().color(TEXT_DIM))
                            .wrap(),
                    );
                }
            });
        });
        ui.add_space(4.0);
    }
    changed
}

/// Resources the graph has no extractor nodes for.
fn not_extracted(resources: &BTreeMap<String, f64>, extracted: &BTreeMap<String, Vec<String>>) -> BTreeMap<String, f64> {
    resources.iter().filter(|(i, _)| !extracted.contains_key(*i)).map(|(i, r)| (i.clone(), *r)).collect()
}

/// Total materials to place every machine, as a collapsible Overview section.
#[allow(clippy::too_many_arguments)]
pub fn build_cost(
    ui: &mut Ui,
    assets: &mut Assets,
    game: &GameData,
    buildings: &BTreeMap<String, (f64, f64)>,
    extractors: &BTreeMap<String, (f64, f64)>,
    resources: &BTreeMap<String, f64>,
    extracted: &BTreeMap<String, Vec<String>>,
    settings: &ExtractionSettings,
) {
    let plans = plan_all(game, &not_extracted(resources, extracted), settings);
    let mut all = buildings.clone();
    all.extend(extractors.iter().map(|(k, v)| (k.clone(), *v)));
    let buildings = &all;
    let totals = build_cost_totals(game, buildings, &plans);
    if totals.is_empty() {
        return;
    }
    let machines: f64 = buildings.values().map(|(w, _)| w).sum::<f64>() + plans.iter().map(|p| p.count as f64).sum::<f64>();
    let mut rows: Vec<(&String, &f64)> = totals.iter().collect();
    rows.sort_by(|a, b| b.1.total_cmp(a.1).then(a.0.cmp(b.0)));
    widgets::divider(ui);
    let summary = format!("{} machines, {} kinds of item", fmt_num(machines), rows.len());
    widgets::collapsible(ui, assets, "build-cost", "Build cost", &summary, |ui, assets| {
        widgets::caption(
            ui,
            "Every machine in the graph, rounded up to whole machines, plus the extractors under Raw resources. Belts, pipes, power poles, foundations and Resource Well Pressurizers are not included.",
        );
        for (item, amount) in rows {
            widgets::info_row(ui, assets, item, game.item_name(item), RichText::new(fmt_num(*amount)).color(TEXT));
        }
    });
}
