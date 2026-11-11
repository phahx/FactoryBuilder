//! The HUB: where every project starts. It isn't a factory: one per
//! project, on the master graph like a factory's block, and inside it only
//! its two built-in Biomass Burners (20 MW each; see `fb_model::HUB_BURNERS`)
//! and what feeds them, the fuels a Biomass Burner burns (from the Docs).
//! Its burners can't be removed or copied; nothing else can be added.

use crate::fmt::fmt_num;
use crate::graph::*;
use crate::theme::*;
use crate::widgets;
use egui::{vec2, RichText, Ui};
use egui_node_graph2::NodeId;
use fb_model::{GameData, HUB_BURNERS, HUB_BURNER_MW};

pub const NAME: &str = "The HUB";
pub const BURNER_NAME: &str = "Biomass Burner";

/// Items per minute a HUB burner burns of `fuel` at full output.
pub fn fuel_per_min(game: &GameData, fuel: &str) -> f64 {
    game.burner_fuels.iter().find(|f| f.item == fuel).map_or(0.0, |f| f.hub_per_min())
}

/// What a new burner burns: the first fuel the Docs list (Leaves).
pub fn default_fuel(game: &GameData) -> Option<String> {
    game.burner_fuels.first().map(|f| f.item.clone())
}

/// Whether an item can go into the HUB (a burner fuel).
pub fn is_fuel(game: &GameData, item: &str) -> bool {
    game.burner_fuels.iter().any(|f| f.item == item)
}

/// What may stand in the HUB's graph: its burners, and fuel coming in by
/// hand or from other factories.
pub fn allowed(game: &GameData, kind: &NodeKind) -> bool {
    match kind {
        NodeKind::Burner { fuel } => is_fuel(game, fuel),
        NodeKind::Resource { item, .. } | NodeKind::Input { item } => is_fuel(game, item),
        _ => false,
    }
}

/// Makes a HUB's graph what it may be: exactly its two burners, and no
/// nodes it can't have (pasted or from an older file). Elsewhere, takes out
/// any burners. Returns whether it changed anything.
pub fn tidy(state: &mut EditorState, user: &mut UserState, hub: bool) -> bool {
    let game = user.game.clone();
    if game.burner_fuels.is_empty() {
        return false;
    }
    let g = &state.graph;
    let mut burners = 0;
    let mut remove = Vec::new();
    for n in g.iter_nodes() {
        let kind = &g[n].user_data.kind;
        let keep = if !hub {
            !matches!(kind, NodeKind::Burner { .. })
        } else if matches!(kind, NodeKind::Burner { .. }) {
            burners += 1;
            burners <= HUB_BURNERS && allowed(&game, kind)
        } else {
            allowed(&game, kind)
        };
        if !keep {
            remove.push(n);
        }
    }
    let mut changed = !remove.is_empty();
    for n in remove {
        if matches!(state.graph[n].user_data.kind, NodeKind::Burner { .. }) {
            burners -= 1;
        }
        state.graph.remove_node(n);
        state.node_positions.remove(n);
        state.node_order.retain(|x| *x != n);
        state.selected_nodes.retain(|x| *x != n);
    }
    if hub {
        let fuel = default_fuel(&game).unwrap_or_default();
        let z = state.pan_zoom.zoom.max(0.01);
        while burners < HUB_BURNERS {
            // One above the other.
            let at = egui::pos2(0.0, burners as f32 * 200.0);
            crate::project::add_node_to(state, user, Template::Burner(fuel.clone()), (at.to_vec2() * z).to_pos2());
            burners += 1;
            changed = true;
        }
    }
    changed
}

/// Switches burner `node` to `fuel`: a new input port for it (the old
/// fuel's wires go, as they carry something else).
pub fn set_fuel(game: &GameData, g: &mut FactoryGraph, node: NodeId, fuel: &str) {
    if !is_fuel(game, fuel) || matches!(&g[node].user_data.kind, NodeKind::Burner { fuel: f } if f == fuel) {
        return;
    }
    g[node].user_data.kind = NodeKind::Burner { fuel: fuel.to_string() };
    for i in g[node].inputs.iter().map(|(_, i)| *i).collect::<Vec<_>>() {
        g.remove_input_param(i);
    }
    add_item_input(game, g, node, fuel);
}

/// MW a burner makes, fed `efficiency` of its fuel.
pub fn burner_mw(efficiency: f64) -> f64 {
    HUB_BURNER_MW * efficiency.clamp(0.0, 1.0)
}

/// Under a burner's fuel port: what it makes, and what it burns at full output.
pub fn burner_footer(ui: &mut Ui, s: &mut UserState, _graph: &FactoryGraph, node_id: NodeId, fuel: &str) {
    let game = s.game.clone();
    let eff = s.nodes.get(&node_id).map(|n| n.efficiency).unwrap_or(1.0);
    // Live: what it makes now.
    let mw = s.live.get(&node_id).and_then(|l| l.makes_mw).map(f64::from).unwrap_or_else(|| burner_mw(eff));
    let (r_div, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().rect_filled(r_div, 0.0, DIVIDER);
    let burns = format!("{} {} per min at {} MW", fmt_num(fuel_per_min(&game, fuel)), game.item_name(fuel), fmt_num(HUB_BURNER_MW));
    widgets::split_row_with(ui, 36.0, s, |ui, s| {
        ui.label(RichText::new(format!("Produces {} MW", fmt_num(mw))).small().color(TEXT));
        s.ui_icon(ui, crate::assets::UiIcon::Power, 14.0, TEXT);
    }, |ui, s| {
        s.item_icon(ui, fb_model::BIOMASS_BURNER_ICON, 32.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            widgets::name_label(ui, RichText::new(BURNER_NAME).color(TEXT));
            widgets::name_label(ui, RichText::new(burns).small().color(TEXT_DIM));
            crate::live_factory::status_line(ui, s, node_id);
        });
    });
}

/// Under the HUB's block on the master graph: what its burners make.
pub fn block_footer(ui: &mut Ui, s: &mut UserState, f: &crate::project::FactoryInfo) {
    let (r_div, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().rect_filled(r_div, 0.0, DIVIDER);
    let made = f.generation.unwrap_or(0.0);
    widgets::split_row_with(ui, 24.0, s, |ui, s| {
        ui.label(RichText::new(format!("{} of {} MW", fmt_num(made), fmt_num(HUB_BURNER_MW * HUB_BURNERS as f64))).small().color(TEXT_DIM));
        s.ui_icon(ui, crate::assets::UiIcon::Power, 14.0, TEXT_DIM);
    }, |ui, _| {
        widgets::name_label(ui, RichText::new(format!("{HUB_BURNERS} Biomass Burners")).small().color(TEXT_DIM));
    });
}
