//! Power plants in the UI: the planner's "Power" section (grid or generate,
//! and which generators may be used), the footer of generator nodes
//! ("Produces 75 MW"), and the overview's consumption vs generation.
//!
//! Generators are the synthesized generator recipes from
//! `fb_engine::power` (`Recipe::is_generator`).

use crate::assets::{Assets, UiIcon};
use crate::fmt::{fmt_num, fmt_rate_unit};
use crate::graph::{input_number, FactoryGraph, NodeKind, UserState, CLOCK, MACHINES};
use crate::theme::{self, *};
use crate::widgets;
use egui::{vec2, RichText, Ui};
use egui_node_graph2::NodeId;
use fb_model::{GameData, PowerMode, PowerSettings, Recipe};

/// Generator recipes grouped by building, smallest generator first; fuels
/// by name.
fn generator_groups(game: &GameData) -> Vec<(&str, Vec<&Recipe>)> {
    let mut groups: Vec<(&str, Vec<&Recipe>)> = Vec::new();
    for r in game.recipes.values().filter(|r| r.is_generator()) {
        match groups.iter_mut().find(|(b, _)| *b == r.building) {
            Some((_, list)) => list.push(r),
            None => groups.push((&r.building, vec![r])),
        }
    }
    let fuel_name = |r: &Recipe| r.icon_item().map(|i| game.item_name(i).to_string()).unwrap_or_default();
    for (_, list) in &mut groups {
        list.sort_by_key(|r| fuel_name(r));
    }
    groups.sort_by(|a, b| a.1[0].generates_mw.total_cmp(&b.1[0].generates_mw).then(a.0.cmp(b.0)));
    groups
}

/// The planner panel's "Power" section. Returns true when the settings changed.
pub fn power_section(ui: &mut Ui, assets: &mut Assets, game: &GameData, power: &mut PowerSettings) -> bool {
    let mut changed = false;
    widgets::divider(ui);
    widgets::section(ui, "Power", None);
    widgets::caption(ui, "Where this factory's power comes from.");
    changed |= widgets::tabs(
        ui,
        &mut power.mode,
        &[
            (PowerMode::Grid, "From the grid", "Your power grid supplies it. The plan only shows how many MW the machines use."),
            (PowerMode::Generate, "Generate it here", "The plan adds generators for the MW its machines use, and makes their fuel."),
        ],
    );
    if power.mode != PowerMode::Generate {
        return changed;
    }
    let groups = generator_groups(game);
    let total: usize = groups.iter().map(|g| g.1.len()).sum();
    let allowed: Vec<&Recipe> = groups.iter().flat_map(|g| g.1.iter().copied()).filter(|r| power.generators.contains(&r.class)).collect();
    let summary = match allowed.len() {
        0 => "None allowed. Pick at least one.".to_string(),
        1 => allowed[0].name.clone(),
        n => format!("{n} of {total} allowed"),
    };
    widgets::collapsible(ui, assets, "generators", "Generators", &summary, |ui, assets| {
        widgets::caption(ui, "Which generators and fuels the plan may use. Hover one to see what it burns.");
        for (building, list) in &groups {
            let Some(b) = game.buildings.get(*building) else { continue };
            widgets::split_row(ui, 26.0, |ui| {
                ui.label(RichText::new(format!("{} MW each", fmt_num(list[0].generates_mw))).small().color(TEXT_DIM));
            }, |ui| {
                assets.item(ui, &b.icon, 24.0);
                widgets::name_label(ui, RichText::new(&b.name).color(TEXT));
            });
            ui.spacing_mut().item_spacing.y = 0.0;
            for r in list {
                let on = power.generators.contains(&r.class);
                let fuel = r.icon_item().unwrap_or_default();
                let mut box_rect = egui::Rect::NOTHING;
                let row = widgets::list_row(ui, 30.0, |ui| {
                    let (rect, _) = ui.allocate_exact_size(vec2(20.0, 20.0), egui::Sense::hover());
                    box_rect = rect;
                    assets.item(ui, fuel, 22.0);
                    ui.label(RichText::new(game.item_name(fuel)).color(if on { TEXT } else { TEXT_DIM }));
                });
                widgets::checkbox_box(ui, assets, box_rect, on, row.hovered());
                if row.on_hover_ui(|ui| generator_tooltip(ui, assets, game, r)).clicked() {
                    if on {
                        power.generators.retain(|c| c != &r.class);
                    } else {
                        power.generators.push(r.class.clone());
                        power.generators.sort();
                    }
                    changed = true;
                }
            }
            ui.spacing_mut().item_spacing.y = 6.0;
        }
    });
    changed
}

/// What one generator burns and makes, per minute at 100% clock speed.
pub fn generator_tooltip(ui: &mut Ui, assets: &mut Assets, game: &GameData, r: &Recipe) {
    widgets::tooltip_title(ui, &r.name);
    ui.label(RichText::new(format!("Produces {} MW per generator", fmt_num(r.generates_mw))).color(TEXT));
    for (label, list) in [("Uses", &r.ingredients), ("Byproduct", &r.products)] {
        if list.is_empty() {
            continue;
        }
        ui.add_space(2.0);
        ui.label(RichText::new(label).small().color(TEXT_DIM));
        for a in list.iter() {
            let amount = fmt_rate_unit(r.per_min(a), game.item_form(&a.item));
            widgets::tip_item(ui, assets, &a.item, game.item_name(&a.item), &amount);
        }
    }
}

/// MW a graph node generates as it runs now (generators only): machines x
/// clock x running speed x the generator's MW. Fuel use scales the same way,
/// so this is also fuel burned x its energy.
pub fn generated_mw(game: &GameData, graph: &FactoryGraph, id: NodeId, efficiency: f64) -> Option<f64> {
    let NodeKind::Recipe { recipe } = &graph[id].user_data.kind else { return None };
    let r = game.recipes.get(recipe).filter(|r| r.is_generator())?;
    let machines = input_number(graph, id, MACHINES);
    let clock = input_number(graph, id, CLOCK) / 100.0;
    Some(r.generates_mw * machines * clock * efficiency.clamp(0.0, 1.0))
}

/// Footer of a generator node: the building, how many to build, and the MW
/// it produces.
pub fn generator_footer(ui: &mut Ui, s: &mut UserState, graph: &FactoryGraph, node_id: NodeId, r: &Recipe) {
    let game = s.game.clone();
    let efficiency = s.nodes.get(&node_id).map(|n| n.efficiency).unwrap_or(1.0);
    let produced = generated_mw(&game, graph, node_id, efficiency).unwrap_or(0.0);
    let setup = crate::graph::machine_setup(&game, graph, node_id);
    let (r_div, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().rect_filled(r_div, 0.0, DIVIDER);
    let b = game.buildings.get(&r.building);
    let extra = crate::graph::extras_hint(&setup);
    widgets::split_row_with(ui, if extra.is_some() { 48.0 } else { 36.0 }, s, |ui, s| {
        ui.label(RichText::new(format!("Produces {} MW", fmt_num(produced))).small().color(TEXT));
        s.ui_icon(ui, UiIcon::Power, 14.0, TEXT);
    }, |ui, s| {
        if let Some(b) = b {
            s.item_icon(ui, &b.icon, 32.0);
        }
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            widgets::name_label(ui, RichText::new(b.map(|b| b.name.as_str()).unwrap_or(&r.building)).color(TEXT));
            if let Some(line) = crate::graph::build_line(graph, node_id, &setup) {
                widgets::name_label(ui, RichText::new(line).small().color(TEXT_DIM));
            }
            if let Some(extra) = &extra {
                widgets::name_label(ui, RichText::new(extra).small().color(ACCENT));
            }
        });
    });
}

/// The overview's power figures: what the machines use and, when the graph
/// has generators, what they produce and the difference.
pub fn overview_power(ui: &mut Ui, assets: &mut Assets, used: f64, generated: Option<f64>) {
    let big = |ui: &mut Ui, assets: &mut Assets, mw: f64| {
        ui.horizontal(|ui| {
            assets.ui(ui, UiIcon::Power, 22.0, ACCENT);
            ui.label(RichText::new(format!("{} MW", fmt_num(mw))).font(theme::semibold(20.0)).color(TEXT));
        });
    };
    big(ui, assets, used);
    ui.label(RichText::new("Power consumption").small().color(TEXT_DIM));
    let Some(made) = generated else { return };
    ui.add_space(4.0);
    big(ui, assets, made);
    let diff = made - used;
    let (note, color) = if diff.abs() < 1e-6 {
        ("Power production".to_string(), TEXT_DIM)
    } else if diff > 0.0 {
        (format!("Power production · {} MW spare", fmt_num(diff)), TEXT_DIM)
    } else {
        (format!("Power production · {} MW short", fmt_num(-diff)), supply_color(made, used))
    };
    ui.label(RichText::new(note).small().color(color));
}
