//! Vanilla-styled widgets (see theme.rs for where each colour comes from).
//! Only interactive things get hover/press states.

use crate::assets::{Assets, UiIcon};
use crate::theme::{self, *};
use egui::{
    vec2, Align, Align2, Color32, CursorIcon, Id, Layout, Rect, Response, RichText, Sense, Stroke,
    TextStyle, Ui, WidgetText,
};

fn galley(
    ui: &Ui,
    text: impl Into<WidgetText>,
    style: TextStyle,
    color: Color32,
) -> std::sync::Arc<egui::Galley> {
    let t: WidgetText = text.into();
    t.color(color)
        .into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, style)
}

#[cfg(test)]
thread_local! {
    /// Tests find buttons by label: (label, rect, enabled) drawn this frame.
    pub static TEST_BUTTONS: std::cell::RefCell<Vec<(String, Rect, bool)>> = Default::default();
}

/// Widget_StandardButton: flat #666667, orange on hover and press.
pub fn button(ui: &mut Ui, text: &str) -> Response {
    button_ex(ui, text, false, true)
}

pub fn button_full(ui: &mut Ui, text: &str) -> Response {
    button_ex(ui, text, true, true)
}

pub fn button_enabled(ui: &mut Ui, text: &str, enabled: bool) -> Response {
    button_ex(ui, text, false, enabled)
}

pub fn button_ex_full_enabled(ui: &mut Ui, text: &str, enabled: bool) -> Response {
    button_ex(ui, text, true, enabled)
}

fn button_ex(ui: &mut Ui, text: &str, full: bool, enabled: bool) -> Response {
    let pad = ui.spacing().button_padding;
    let fg = if enabled { TEXT } else { TEXT_DISABLED };
    let g = galley(ui, text, TextStyle::Button, fg);
    let width = if full {
        ui.available_width()
    } else {
        g.size().x + pad.x * 2.0
    };
    let size = vec2(
        width,
        (g.size().y + pad.y * 2.0).max(ui.spacing().interact_size.y),
    );
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    #[cfg(test)]
    TEST_BUTTONS.with(|b| b.borrow_mut().push((text.to_string(), rect, enabled)));
    if ui.is_rect_visible(rect) {
        let fill = if !enabled {
            DISABLED
        } else if resp.hovered() || resp.is_pointer_button_down_on() {
            ACCENT
        } else {
            BUTTON
        };
        ui.painter().rect_filled(rect, 0.0, fill);
        ui.painter().galley(rect.center() - g.size() / 2.0, g, fg);
    }
    if enabled {
        resp.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        resp
    }
}

/// Square icon-only button with a tooltip (toolbar / row actions).
pub fn icon_button(ui: &mut Ui, assets: &mut Assets, icon: UiIcon, tooltip: &str) -> Response {
    let side = ui.spacing().interact_size.y;
    let (rect, resp) = ui.allocate_exact_size(vec2(side, side), Sense::click());
    if ui.is_rect_visible(rect) {
        let (fill, tint) = if resp.is_pointer_button_down_on() {
            (ACCENT, TEXT)
        } else if resp.hovered() {
            (ROW_HOVER, TEXT)
        } else {
            (Color32::TRANSPARENT, TEXT_DIM)
        };
        ui.painter().rect_filled(rect, 0.0, fill);
        assets.paint_ui(
            ui,
            icon,
            Rect::from_center_size(rect.center(), vec2(16.0, 16.0)),
            tint,
        );
    }
    resp.on_hover_text(tooltip)
        .on_hover_cursor(CursorIcon::PointingHand)
}

/// Widget_SlidingTabs: #575757, orange on hover, selected #CCCBCB with dark text.
pub fn tabs<T: PartialEq + Copy>(ui: &mut Ui, value: &mut T, options: &[(T, &str, &str)]) -> bool {
    let mut changed = false;
    let width = ui.available_width();
    let gap = 2.0;
    let each = (width - gap * (options.len() as f32 - 1.0)) / options.len() as f32;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for (v, label, tip) in options {
            let selected = *value == *v;
            let (rect, resp) =
                ui.allocate_exact_size(vec2(each, ui.spacing().interact_size.y), Sense::click());
            let (fill, fg) = if selected {
                (TAB_SELECTED, TEXT_DARK)
            } else if resp.hovered() {
                (ACCENT, TEXT)
            } else {
                (TAB, TEXT)
            };
            ui.painter().rect_filled(rect, 0.0, fill);
            let g = galley(ui, *label, TextStyle::Button, fg);
            ui.painter().galley(rect.center() - g.size() / 2.0, g, fg);
            let resp = if tip.is_empty() {
                resp
            } else {
                resp.on_hover_text(*tip)
            };
            if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() && !selected {
                *value = *v;
                changed = true;
            }
        }
    });
    changed
}

/// Square checkbox with the game's FICSIT checkmark.
pub fn checkbox_box(ui: &mut Ui, assets: &mut Assets, rect: Rect, on: bool, hovered: bool) {
    let r = Rect::from_center_size(rect.center(), vec2(16.0, 16.0));
    if on {
        ui.painter().rect_filled(r, 0.0, ACCENT);
        assets.paint_ui(ui, UiIcon::Check, r.shrink(2.0), TEXT_DARK);
    } else {
        ui.painter().rect_filled(r, 0.0, INPUT);
        ui.painter().rect_stroke(
            r,
            0.0,
            Stroke::new(1.0_f32, if hovered { TEXT_DIM } else { DIVIDER }),
        );
    }
}

/// A text field with a button at its right, filling the row exactly (the
/// button is laid out first so the field takes what's left). Returns the
/// field's response and whether the button was clicked.
pub fn field_with_button(ui: &mut Ui, text: &mut String, editable: bool, button_label: &str, can_click: bool) -> (Response, bool) {
    let mut clicked = false;
    let height = ui.spacing().interact_size.y.max(30.0);
    let inner = ui.allocate_ui_with_layout(vec2(ui.available_width(), height), Layout::right_to_left(Align::Center), |ui| {
        clicked = button_enabled(ui, button_label, can_click).clicked();
        let edit = egui::TextEdit::singleline(text).desired_width(ui.available_width()).margin(vec2(8.0, 6.0));
        if editable {
            ui.add(edit)
        } else {
            ui.add_enabled(false, edit)
        }
    });
    (inner.inner, clicked)
}

/// A checkbox with its label; the whole row toggles it.
pub fn checkbox(ui: &mut Ui, assets: &mut Assets, on: &mut bool, label: &str) -> Response {
    let g = galley(ui, label, TextStyle::Body, TEXT);
    let size = vec2(16.0 + 8.0 + g.size().x, g.size().y.max(ui.spacing().interact_size.y));
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.clicked() {
        *on = !*on;
    }
    let box_rect = Rect::from_min_size(rect.min, vec2(16.0, rect.height()));
    checkbox_box(ui, assets, box_rect, *on, resp.hovered());
    let text_pos = egui::pos2(box_rect.right() + 8.0, rect.center().y - g.size().y / 2.0);
    ui.painter().galley(text_pos, g, TEXT);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Number box that commits on Enter or when focus leaves. Returns true when
/// the value changed.
pub fn num_field(ui: &mut Ui, id: Id, value: &mut f64, width: f32) -> bool {
    let text_id = id.with("text");
    let mut text: String = ui
        .data_mut(|d| d.get_temp::<String>(text_id))
        .unwrap_or_else(|| crate::fmt::fmt_plain(*value));
    let edit = egui::TextEdit::singleline(&mut text)
        .id(id)
        .desired_width(width)
        .horizontal_align(Align::Max)
        // From the (possibly zoomed) style, so fields scale inside nodes.
        .margin(ui.spacing().button_padding * vec2(0.5, 0.7));
    let resp = ui.add(edit);
    let mut changed = false;
    if resp.has_focus() {
        ui.data_mut(|d| d.insert_temp(text_id, text.clone()));
    } else if resp.lost_focus() {
        if let Ok(v) = text.trim().replace(',', "").parse::<f64>() {
            if v.is_finite() && v >= 0.0 && (v - *value).abs() > 1e-12 {
                *value = v;
                changed = true;
            }
        }
        ui.data_mut(|d| d.remove::<String>(text_id));
    } else {
        ui.data_mut(|d| d.remove::<String>(text_id));
    }
    changed
}

/// Panel title bar: the vanilla window header (#3F3F3F).
pub fn panel_header(ui: &mut Ui, title: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, HEADER);
    ui.painter().text(
        rect.left_center() + vec2(12.0, 0.0),
        Align2::LEFT_CENTER,
        title,
        theme::semibold(15.0),
        TEXT,
    );
}

/// Section title in the tooltip-header orange, with an optional action
/// icon button at the right. Returns the action's response.
pub fn section(
    ui: &mut Ui,
    title: &str,
    action: Option<(&mut Assets, UiIcon, &str)>,
) -> Option<Response> {
    let mut out = None;
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(title)
                .font(theme::semibold(14.0))
                .color(ACCENT),
        );
        if let Some((assets, icon, tip)) = action {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                out = Some(icon_button(ui, assets, icon, tip));
            });
        }
    });
    out
}

/// A short explanation under a section title.
pub fn caption(ui: &mut Ui, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).small().color(TEXT_DIM)).wrap());
}

/// Empty-list row that adds the first entry: the add icon and a label.
pub fn add_row(ui: &mut Ui, assets: &mut Assets, text: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
    let fill = if resp.is_pointer_button_down_on() {
        ACCENT
    } else if resp.hovered() {
        ROW_HOVER
    } else {
        INPUT
    };
    ui.painter().rect_filled(rect, 0.0, fill);
    assets.paint_ui(
        ui,
        UiIcon::Add,
        Rect::from_center_size(rect.left_center() + vec2(18.0, 0.0), vec2(16.0, 16.0)),
        TEXT_DIM,
    );
    ui.painter().text(
        rect.left_center() + vec2(34.0, 0.0),
        Align2::LEFT_CENTER,
        text,
        egui::FontId::proportional(14.0),
        TEXT_DIM,
    );
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Collapsible section: caret, orange title and a one-line summary that
/// stays visible while collapsed, so the closed header says what's inside.
pub fn collapsible(
    ui: &mut Ui,
    assets: &mut Assets,
    id: &str,
    title: &str,
    summary: &str,
    body: impl FnOnce(&mut Ui, &mut Assets),
) {
    let id = ui.make_persistent_id(id);
    let mut state =
        egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 42.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 0.0, ROW_HOVER);
    }
    let caret = Rect::from_center_size(rect.left_center() + vec2(8.0, -7.0), vec2(9.0, 9.0));
    if state.is_open() {
        assets.paint_ui(ui, UiIcon::Caret, caret, TEXT_DIM);
    } else {
        assets.paint_ui(ui, UiIcon::Arrow, caret.expand(2.0), TEXT_DIM);
    }
    ui.painter().text(
        rect.left_top() + vec2(20.0, 4.0),
        Align2::LEFT_TOP,
        title,
        theme::semibold(14.0),
        ACCENT,
    );
    ui.painter().text(
        rect.left_top() + vec2(20.0, 24.0),
        Align2::LEFT_TOP,
        summary,
        egui::FontId::proportional(12.0),
        TEXT_DIM,
    );
    if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
        state.toggle(ui);
    }
    state.store(ui.ctx());
    if state.is_open() {
        body(ui, assets);
    }
}

/// Hairline between sections (tab divider #575757).
pub fn divider(ui: &mut Ui) {
    ui.add_space(6.0);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, DIVIDER);
    ui.add_space(6.0);
}

/// A full-width clickable list row (Widget_ListButton: hover #575757).
pub fn list_row(ui: &mut Ui, height: f32, add: impl FnOnce(&mut Ui)) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 0.0, ROW_HOVER);
    }
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(vec2(6.0, 0.0)))
            .layout(Layout::left_to_right(Align::Center)),
    );
    add(&mut child);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// A non-interactive row: icon, label, value on the right.
pub fn info_row(ui: &mut Ui, assets: &mut Assets, class: &str, label: &str, value: RichText) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        assets.item(ui, class, 24.0);
        ui.label(RichText::new(label).color(TEXT));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(value));
    });
}

/// BPW_ComboboxSearch: an item picker with icons and a search field.
/// Returns true when the selection changed.
pub fn item_combo(
    ui: &mut Ui,
    assets: &mut Assets,
    id: Id,
    current: &mut String,
    items: &[(String, String)],
    width: f32,
) -> bool {
    let popup_id = id.with("popup");
    let open = ui.memory(|m| m.is_popup_open(popup_id));
    let (rect, resp) =
        ui.allocate_exact_size(vec2(width, ui.spacing().interact_size.y), Sense::click());
    let fill = if open {
        COMBO_OPEN
    } else if resp.hovered() {
        ACCENT
    } else {
        COMBO
    };
    ui.painter().rect_filled(rect, 0.0, fill);
    let icon_rect = Rect::from_min_size(rect.left_top() + vec2(4.0, 2.0), vec2(24.0, 24.0));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(icon_rect));
    assets.item(&mut child, current, 24.0);
    let name = items
        .iter()
        .find(|(c, _)| c == current)
        .map(|(_, n)| n.as_str())
        .unwrap_or(current.as_str());
    let text_rect = Rect::from_min_max(
        rect.left_top() + vec2(34.0, 0.0),
        rect.right_bottom() - vec2(24.0, 0.0),
    );
    let g = egui::WidgetText::from(name).color(TEXT).into_galley(
        ui,
        Some(egui::TextWrapMode::Truncate),
        text_rect.width(),
        TextStyle::Button,
    );
    ui.painter().galley(
        egui::pos2(text_rect.left(), rect.center().y - g.size().y / 2.0),
        g,
        TEXT,
    );
    assets.paint_ui(
        ui,
        UiIcon::Caret,
        Rect::from_center_size(rect.right_center() - vec2(12.0, 0.0), vec2(10.0, 10.0)),
        TEXT,
    );
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    if resp.clicked() {
        ui.memory_mut(|m| m.toggle_popup(popup_id));
    }

    let mut changed = false;
    egui::popup_below_widget(
        ui,
        popup_id,
        &resp,
        egui::PopupCloseBehavior::CloseOnClickOutside,
        |ui| {
            ui.set_min_width(width.max(280.0));
            let qid = id.with("query");
            let mut query: String = ui.data_mut(|d| d.get_temp(qid).unwrap_or_default());
            ui.horizontal(|ui| {
                assets.ui(ui, UiIcon::Search, 16.0, TEXT_DIM);
                let r = ui.add(
                    egui::TextEdit::singleline(&mut query)
                        .hint_text("Search")
                        .desired_width(f32::INFINITY),
                );
                r.request_focus();
            });
            let q = query.to_lowercase();
            let matches: Vec<&(String, String)> = items
                .iter()
                .filter(|(_, n)| q.is_empty() || n.to_lowercase().contains(&q))
                .collect();
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            egui::ScrollArea::vertical()
                .max_height(360.0)
                .show(ui, |ui| {
                    if matches.is_empty() {
                        ui.label(RichText::new("No matches").color(TEXT_DIM));
                    }
                    for (i, (class, name)) in matches.iter().enumerate() {
                        let row = list_row(ui, 30.0, |ui| {
                            assets.item(ui, class, 24.0);
                            ui.label(RichText::new(name).color(if class == current {
                                ACCENT
                            } else {
                                TEXT
                            }));
                        });
                        if row.clicked() || (enter && i == 0) {
                            *current = class.clone();
                            changed = true;
                            ui.memory_mut(|m| m.close_popup());
                        }
                    }
                });
            ui.data_mut(|d| d.insert_temp(qid, query));
        },
    );
    changed
}

/// Vanilla tooltip body: orange title, then white lines.
pub fn tooltip_title(ui: &mut Ui, title: &str) {
    ui.label(
        RichText::new(title)
            .font(theme::semibold(14.0))
            .color(ACCENT),
    );
}
