//! Right-click menus, all in the style of the Add node menu (finder.rs):
//! a header bar with what the menu is for, then full-width rows that light
//! up across the whole menu on hover, shortcuts right-aligned, hairlines
//! between groups.

use crate::assets::Assets;
use crate::theme::*;
use crate::widgets;
use egui::{pos2, vec2, Align2, CursorIcon, Pos2, Rect, RichText, Sense, TextStyle, Ui};

pub const WIDTH: f32 = 260.0;
const ROW: f32 = 32.0;
const PAD: f32 = 12.0;

/// A menu at `pos` (kept on screen). Returns the area's response, for
/// "clicked elsewhere" checks.
pub fn popup(ctx: &egui::Context, id: &str, pos: Pos2, title: &str, body: impl FnOnce(&mut Ui)) -> egui::Response {
    let screen = ctx.screen_rect();
    // The height of last frame's menu, to keep it above the bottom edge.
    let h = ctx.memory(|m| m.area_rect(egui::Id::new(id))).map(|r| r.height()).unwrap_or(0.0);
    let at = pos2(pos.x.min(screen.right() - WIDTH - 8.0).max(8.0), pos.y.min(screen.bottom() - h - 8.0).max(8.0));
    egui::Area::new(egui::Id::new(id))
        .order(egui::Order::Foreground)
        .fixed_pos(at)
        .show(ctx, |ui| {
            egui::Frame::none().fill(CONTENT).stroke(ui.visuals().window_stroke).shadow(ui.visuals().popup_shadow).show(ui, |ui| {
                ui.set_width(WIDTH);
                ui.spacing_mut().item_spacing.y = 0.0;
                widgets::panel_header(ui, title);
                ui.add_space(4.0);
                body(ui);
                ui.add_space(4.0);
            });
        })
        .response
}

/// One row: label, right-aligned shortcut.
pub fn item(ui: &mut Ui, label: &str, shortcut: &str, enabled: bool) -> egui::Response {
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), sense);
    #[cfg(test)]
    widgets::TEST_BUTTONS.with(|b| b.borrow_mut().push((label.to_string(), rect, enabled)));
    if enabled && (resp.hovered() || resp.is_pointer_button_down_on()) {
        ui.painter().rect_filled(rect, 0.0, if resp.is_pointer_button_down_on() { ACCENT } else { ROW_HOVER });
    }
    let fg = if enabled { TEXT } else { TEXT_DISABLED };
    let x = rect.left() + PAD;
    let short_w = if shortcut.is_empty() {
        0.0
    } else {
        let g = ui.painter().layout_no_wrap(shortcut.to_string(), egui::FontId::proportional(13.0), TEXT_DIM);
        let w = g.size().x;
        ui.painter().galley(pos2(rect.right() - PAD - w, rect.center().y - g.size().y / 2.0), g, TEXT_DIM);
        w + 12.0
    };
    let g = egui::WidgetText::from(RichText::new(label).color(fg)).into_galley(ui, Some(egui::TextWrapMode::Truncate), rect.right() - PAD - short_w - x, TextStyle::Body);
    ui.painter().galley(pos2(x, rect.center().y - g.size().y / 2.0), g, fg);
    if enabled {
        resp.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        resp
    }
}

/// Hairline between groups of rows.
pub fn separator(ui: &mut Ui) {
    ui.add_space(4.0);
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(r, 0.0, DIVIDER);
    ui.add_space(4.0);
}

/// A small caption over a group (like "Colour").
pub fn caption(ui: &mut Ui, text: &str) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
    ui.painter().text(pos2(r.left() + PAD, r.center().y), Align2::LEFT_CENTER, text, egui::FontId::proportional(12.0), TEXT_DIM);
}

/// The colour group of a menu: "No colour", the game's presets and a custom
/// colour. Returns the pick, if one was made; `swatches` collects their
/// rects (tests).
pub fn colours(ui: &mut Ui, mut assets: Option<&mut Assets>, current: Option<[u8; 3]>, custom: &mut Option<[u8; 3]>, swatches: &mut Vec<Rect>) -> Option<Option<[u8; 3]>> {
    let mut pick = None;
    caption(ui, "Colour");
    ui.horizontal(|ui| {
        ui.add_space(PAD - 3.0);
        ui.spacing_mut().item_spacing.x = 4.0;
        let r = crate::app::swatch(ui, assets.as_deref_mut(), None, current.is_none(), "No colour");
        swatches.push(r.rect);
        if r.clicked() {
            pick = Some(None);
        }
        for (name, rgb) in crate::app::GAME_COLORS {
            let r = crate::app::swatch(ui, assets.as_deref_mut(), Some(rgb), current == Some(rgb), name);
            swatches.push(r.rect);
            if r.clicked() {
                pick = Some(Some(rgb));
            }
        }
    });
    ui.add_space(4.0);
    match *custom {
        None => {
            if item(ui, "Custom colour…", "", true).clicked() {
                *custom = Some(current.unwrap_or([0xE5, 0x93, 0x44]));
            }
        }
        Some(rgb) => {
            egui::Frame::none().inner_margin(egui::Margin::symmetric(PAD, 4.0)).show(ui, |ui| {
                let mut c = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                if egui::color_picker::color_picker_color32(ui, &mut c, egui::color_picker::Alpha::Opaque) {
                    *custom = Some([c.r(), c.g(), c.b()]);
                    pick = Some(Some([c.r(), c.g(), c.b()]));
                }
            });
        }
    }
    pick
}

/// Closes on a click outside the menu or Esc.
pub fn dismissed(ctx: &egui::Context, menu: &egui::Response) -> bool {
    (ctx.input(|i| i.pointer.any_pressed()) && !menu.contains_pointer()) || ctx.input(|i| i.key_pressed(egui::Key::Escape))
}
