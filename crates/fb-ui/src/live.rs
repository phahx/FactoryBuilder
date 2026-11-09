//! The Live map add-on (Settings › Add-ons, off by default): on the Map
//! tab, where players are. While it's off, nothing here runs or shows.
//!
//! Players come from the save the Map tab follows: a save keeps each
//! player character's place but not their name (only an account id), so
//! they're numbered.

use crate::app::FactoryApp;
use crate::map_draw::View;
use crate::theme::*;
use crate::widgets;
use egui::{pos2, vec2, Color32, Pos2, RichText, Stroke};
use fb_model::world::PlayerSpot;

/// Marker radius, screen pixels.
const MARKER: f32 = 7.0;

impl FactoryApp {
    /// Whether the Live map add-on is on.
    pub(crate) fn live_on(&self) -> bool {
        self.status.as_ref().is_some_and(|s| s.settings.live.enabled)
    }

    /// Players to show on the map, and where they come from.
    pub(crate) fn live_players(&self) -> Vec<PlayerSpot> {
        if !self.live_on() {
            return Vec::new();
        }
        self.map.world.as_ref().map(|w| w.players.clone()).unwrap_or_default()
    }
}

/// What a player's marker is called: their name, or "Player n".
pub fn label(players: &[PlayerSpot], i: usize) -> String {
    match players[i].name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => n.to_string(),
        None if players.len() == 1 => "Player".to_string(),
        None => format!("Player {}", i + 1),
    }
}

/// Draws the players (a dot with an arrow the way they face, and their
/// name). Returns the one under `pointer`.
pub fn draw_players(painter: &egui::Painter, view: &View, players: &[PlayerSpot], pointer: Option<Pos2>) -> Option<usize> {
    let mut hovered = None;
    for (i, p) in players.iter().enumerate() {
        let at = view.screen(p.x as f64, p.y as f64);
        if !view.rect.expand(40.0).contains(at) {
            continue;
        }
        // Unreal yaw turns +X (east, right) towards +Y (south, down).
        let a = p.yaw.to_radians();
        let dir = vec2(a.cos(), a.sin());
        let side = vec2(-dir.y, dir.x);
        let tip = at + dir * (MARKER + 7.0);
        let shadow = Color32::from_black_alpha(200);
        painter.add(egui::Shape::convex_polygon(vec![tip, at + dir * (MARKER - 1.0) + side * 5.0, at + dir * (MARKER - 1.0) - side * 5.0], ACCENT, Stroke::new(1.5, shadow)));
        painter.circle(at, MARKER, ACCENT, Stroke::new(2.0, shadow));
        painter.circle_filled(at, 2.5, CONTENT);
        let name = label(players, i);
        let g = painter.layout_no_wrap(name, egui::FontId::proportional(12.0), TEXT);
        let chip = egui::Rect::from_center_size(at + vec2(0.0, MARKER + 14.0), g.size() + vec2(10.0, 4.0));
        painter.rect_filled(chip, 3.0, CONTENT.gamma_multiply(0.9));
        painter.galley(pos2(chip.left() + 5.0, chip.top() + 2.0), g, TEXT);
        if pointer.is_some_and(|q| q.distance(at) <= MARKER + 4.0 || chip.contains(q)) {
            hovered = Some(i);
        }
    }
    hovered
}

/// A player marker's tooltip.
pub fn player_tip(ui: &mut egui::Ui, players: &[PlayerSpot], i: usize, saved: &str) {
    widgets::tooltip_title(ui, &label(players, i));
    widgets::tip_line(ui, RichText::new(format!("Where the save left them ({saved}).")).color(TEXT));
    widgets::tip_line(ui, RichText::new("Saves don't keep player names, only where each one stood.").small().color(TEXT_DIM));
}

/// Settings › Add-ons. Returns nothing; edits `draft`.
pub fn settings_section(ui: &mut egui::Ui, assets: &mut crate::assets::Assets, draft: &mut fb_model::Settings) {
    widgets::divider(ui);
    ui.label(RichText::new("Add-ons").font(crate::theme::semibold(14.0)).color(ACCENT));
    widgets::checkbox(ui, assets, &mut draft.live.enabled, "Live map");
    widgets::caption(ui, "Shows players on the Map tab, where the save it follows left them. Off: the Map tab shows saves as before.");
}
