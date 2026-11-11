//! Storage nodes: a Storage Container, Industrial Storage Container, Fluid
//! Buffer or the like, holding one item. In the simulation one takes in
//! whatever arrives (as a sink does) and supplies up to its rate (as an
//! input does). In a live factory (live_factory.rs) it stands for the
//! containers of that building and item on the map, and shows what's in
//! them now.

use crate::graph::UserState;
use crate::theme::*;
use crate::widgets;
use egui::{vec2, RichText, Ui};
use egui_node_graph2::NodeId;
use fb_model::GameData;

/// Fluid buffers hold fluids; the rest hold items.
const FLUID_STORAGE: &[&str] = &["Build_IndustrialTank", "Build_PipeStorageTank"];

pub fn is_storage(class: &str) -> bool {
    fb_model::is_storage(class)
}

pub fn holds_fluid(class: &str) -> bool {
    FLUID_STORAGE.iter().any(|p| class.starts_with(p))
}

/// The building's name in game ("Storage Container").
pub fn name(game: &GameData, class: &str) -> String {
    game.building_names.get(class).cloned().unwrap_or_else(|| if holds_fluid(class) { "Fluid Buffer".into() } else { "Storage Container".into() })
}

/// The building's icon (its descriptor).
pub fn icon(class: &str) -> String {
    class.strip_prefix("Build_").map(|x| format!("Desc_{x}")).unwrap_or_default()
}

/// What a new storage node for `item` is: a Storage Container, or for
/// fluids a Fluid Buffer.
pub fn default_for(game: &GameData, item: &str) -> &'static str {
    if game.item_form(item).is_fluid() {
        "Build_PipeStorageTank_C"
    } else {
        "Build_StorageContainerMk1_C"
    }
}

/// The node's footer: the building (× how many, live), then what's in
/// them now as the game shows a storage's inventory: icons with counts.
pub fn footer(ui: &mut Ui, s: &mut UserState, node_id: NodeId, _item: &str, class: &str) {
    let game = s.game.clone();
    let live = s.live.get(&node_id).cloned();
    let (r_div, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().rect_filled(r_div, 0.0, DIVIDER);
    ui.horizontal(|ui| {
        s.item_icon(ui, &icon(class), 32.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let count = live.as_ref().map(|l| l.buildings).unwrap_or(0);
            let title = if count > 1 { format!("{} ×{count}", name(&game, class)) } else { name(&game, class) };
            widgets::name_label(ui, RichText::new(title).color(TEXT));
            if let Some((fill, full)) = live.as_ref().and_then(|l| l.fill.clone()) {
                widgets::name_label(ui, RichText::new(fill).small().color(if full { ACCENT } else { TEXT_DIM }));
            }
        });
    });
    let Some(l) = live.filter(|l| !l.held.is_empty()) else { return };
    ui.add_space(2.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
        for (item, n) in l.held.iter().take(16) {
            let fluid = game.item_form(item).is_fluid();
            let text = crate::live_factory::count_text(*n, fluid);
            let unit = if fluid { " m³" } else { "" };
            s.item_icon_count(ui, item, 30.0, Some(&text)).on_hover_text(format!("{} {}{unit}", crate::fmt::fmt_num((*n as f64 * 10.0).round() / 10.0), game.item_name(item)));
        }
    });
    if l.held.len() > 16 {
        widgets::name_label(ui, RichText::new(format!("and {} more", l.held.len() - 16)).small().color(TEXT_DIM));
    }
}
