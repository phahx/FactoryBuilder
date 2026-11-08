//! Vanilla Satisfactory look, taken from the game's own UI assets
//! (UE 5.6 paks, read with tools/IconExtractor --json):
//!
//! - Widget_Window_DarkMode: header + bottom bar #3F3F3F, tab divider #575757,
//!   loading backdrop #373737
//! - Widget_Window_ContentBackground_DarkMode: content #0C0C0C at 89%
//! - Widget_StandardButton: flat square #666667, hover/pressed #E59344, white text
//! - Widget_ListButton: row hover #575757; secondary text #CCCBCB
//! - Widget_SlidingTabs_Button: #575757 / hover #E59345 / selected #CCCBCB
//! - Widget_Scrollbox: track + thumb #575757, thumb hover/drag #E59345
//! - BPW_ComboboxSearch: #7F7F7F / hover #E59344 / pressed #212121
//! - Widget_Tooltip: header text #E59345, divider #666667
//! - Rich text styles: text #FDFDFF, orange #E59344, red #BD2525
//! - Font: Open Sans Regular / SemiBold / Bold (Font'DescriptionText')

use egui::{Color32, FontFamily, FontId, Rounding, Stroke, TextStyle};

pub const HEADER: Color32 = Color32::from_rgb(0x3F, 0x3F, 0x3F);
pub const DIVIDER: Color32 = Color32::from_rgb(0x57, 0x57, 0x57);
pub const BACKDROP: Color32 = Color32::from_rgb(0x37, 0x37, 0x37);
/// #0C0C0C at 89% over the #373737 backdrop.
pub const CONTENT: Color32 = Color32::from_rgb(0x11, 0x11, 0x11);
pub const INPUT: Color32 = Color32::from_rgb(0x0C, 0x0C, 0x0C);
pub const BUTTON: Color32 = Color32::from_rgb(0x66, 0x66, 0x67);
pub const ACCENT: Color32 = Color32::from_rgb(0xE5, 0x93, 0x44);
pub const ROW_HOVER: Color32 = Color32::from_rgb(0x57, 0x57, 0x57);
pub const TAB: Color32 = Color32::from_rgb(0x57, 0x57, 0x57);
pub const TAB_SELECTED: Color32 = Color32::from_rgb(0xCC, 0xCB, 0xCB);
pub const COMBO: Color32 = Color32::from_rgb(0x7F, 0x7F, 0x7F);
pub const COMBO_OPEN: Color32 = Color32::from_rgb(0x21, 0x21, 0x21);
pub const DISABLED: Color32 = Color32::from_rgb(0x50, 0x50, 0x50);
pub const TEXT: Color32 = Color32::from_rgb(0xFD, 0xFD, 0xFF);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0xCC, 0xCB, 0xCB);
pub const TEXT_DARK: Color32 = Color32::from_rgb(0x0C, 0x0C, 0x0C);
pub const TEXT_DISABLED: Color32 = Color32::from_rgb(0x78, 0x78, 0x78);
pub const DANGER: Color32 = Color32::from_rgb(0xBD, 0x25, 0x25);

pub const SEMIBOLD: &str = "semibold";
pub const BOLD: &str = "bold";

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
}

/// Colour for "have of want": white when met, orange when short, red at zero.
pub fn supply_color(have: f64, want: f64) -> Color32 {
    if want <= 1e-9 || have >= want * 0.999 {
        TEXT
    } else if have > 1e-9 {
        ACCENT
    } else {
        DANGER
    }
}

/// Installs the vanilla visuals. Fonts are added separately once the game's
/// Open Sans files arrive (see [`install_fonts`]); until then the semibold
/// and bold families fall back to egui's default font.
pub fn apply(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let fallback = fonts.families[&FontFamily::Proportional].clone();
    fonts
        .families
        .insert(FontFamily::Name(SEMIBOLD.into()), fallback.clone());
    fonts
        .families
        .insert(FontFamily::Name(BOLD.into()), fallback);
    ctx.set_fonts(fonts);

    ctx.style_mut(|style| {
        style.text_styles = [
            (TextStyle::Small, FontId::proportional(12.0)),
            (TextStyle::Body, FontId::proportional(14.0)),
            (TextStyle::Button, FontId::proportional(14.0)),
            (TextStyle::Heading, semibold(18.0)),
            (TextStyle::Monospace, FontId::monospace(13.0)),
        ]
        .into();

        // Labels are plain text, not selectable: selectable labels grab
        // drags (so nodes couldn't be moved by their text) and steal clicks
        // and hover from the rows they sit in.
        style.interaction.selectable_labels = false;

        let s = &mut style.spacing;
        s.item_spacing = egui::vec2(8.0, 6.0);
        s.button_padding = egui::vec2(12.0, 7.0);
        s.interact_size = egui::vec2(40.0, 28.0);
        // Tooltips wrap at a readable width (egui's default is 500 px).
        s.tooltip_width = 340.0;
        s.menu_margin = egui::Margin::same(4.0);
        s.window_margin = egui::Margin::same(0.0);
        s.indent = 16.0;
        s.scroll = egui::style::ScrollStyle::solid();
        s.scroll.bar_width = 8.0;

        let v = &mut style.visuals;
        *v = egui::Visuals::dark();
        v.override_text_color = None;
        v.panel_fill = CONTENT;
        v.window_fill = HEADER;
        v.window_stroke = Stroke::new(1.0_f32, DIVIDER);
        v.window_rounding = Rounding::ZERO;
        v.menu_rounding = Rounding::ZERO;
        v.window_shadow = egui::epaint::Shadow {
            offset: egui::vec2(0.0, 6.0),
            blur: 18.0,
            spread: 0.0,
            color: Color32::from_black_alpha(140),
        };
        v.popup_shadow = egui::epaint::Shadow {
            offset: egui::vec2(0.0, 4.0),
            blur: 12.0,
            spread: 0.0,
            color: Color32::from_black_alpha(120),
        };
        v.extreme_bg_color = INPUT;
        v.faint_bg_color = HEADER;
        v.code_bg_color = INPUT;
        v.hyperlink_color = ACCENT;
        v.warn_fg_color = ACCENT;
        v.error_fg_color = DANGER;
        v.selection.bg_fill = ACCENT;
        v.selection.stroke = Stroke::new(1.0_f32, TEXT);
        v.text_cursor.stroke = Stroke::new(2.0_f32, ACCENT);
        v.striped = false;
        v.slider_trailing_fill = true;
        v.handle_shape = egui::style::HandleShape::Rect { aspect_ratio: 0.5 };

        let w = &mut v.widgets;
        // Static text and frames.
        w.noninteractive.bg_fill = CONTENT;
        w.noninteractive.weak_bg_fill = CONTENT;
        w.noninteractive.bg_stroke = Stroke::new(1.0_f32, DIVIDER);
        w.noninteractive.fg_stroke = Stroke::new(1.0_f32, TEXT_DIM);
        w.noninteractive.rounding = Rounding::ZERO;
        // egui's own widgets (menu items, selectable rows, collapsing
        // headers, text fields). Our buttons are drawn by `widgets::button`.
        // Rows highlight #575757 like Widget_ListButton; scrollbar thumbs
        // use bg_fill: #575757, hover/drag orange like Widget_Scrollbox.
        w.inactive.bg_fill = TAB;
        w.inactive.weak_bg_fill = Color32::TRANSPARENT;
        w.inactive.bg_stroke = Stroke::NONE;
        w.inactive.fg_stroke = Stroke::new(1.0_f32, TEXT);
        w.inactive.rounding = Rounding::ZERO;
        w.inactive.expansion = 0.0;
        w.hovered.bg_fill = ACCENT;
        w.hovered.weak_bg_fill = ROW_HOVER;
        w.hovered.bg_stroke = Stroke::new(1.0_f32, DIVIDER);
        w.hovered.fg_stroke = Stroke::new(1.0_f32, TEXT);
        w.hovered.rounding = Rounding::ZERO;
        w.hovered.expansion = 0.0;
        w.active.bg_fill = ACCENT;
        w.active.weak_bg_fill = ROW_HOVER;
        w.active.bg_stroke = Stroke::new(1.0_f32, ACCENT);
        w.active.fg_stroke = Stroke::new(1.0_f32, TEXT);
        w.active.rounding = Rounding::ZERO;
        w.active.expansion = 0.0;
        w.open.bg_fill = COMBO_OPEN;
        w.open.weak_bg_fill = ROW_HOVER;
        w.open.bg_stroke = Stroke::NONE;
        w.open.fg_stroke = Stroke::new(1.0_f32, TEXT);
        w.open.rounding = Rounding::ZERO;
    });
}

/// Puts the game's Open Sans in place once its three faces have loaded.
pub fn install_fonts(ctx: &egui::Context, regular: Vec<u8>, semibold: Vec<u8>, bold: Vec<u8>) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "OpenSans-Regular".into(),
        egui::FontData::from_owned(regular),
    );
    fonts.font_data.insert(
        "OpenSans-SemiBold".into(),
        egui::FontData::from_owned(semibold),
    );
    fonts
        .font_data
        .insert("OpenSans-Bold".into(), egui::FontData::from_owned(bold));
    // egui's default fonts stay behind as fallbacks for symbols Open Sans lacks.
    let defaults = fonts.families[&FontFamily::Proportional].clone();
    let with = |first: &str| {
        let mut v = vec![first.to_string()];
        v.extend(defaults.iter().cloned());
        v
    };
    fonts
        .families
        .insert(FontFamily::Proportional, with("OpenSans-Regular"));
    fonts
        .families
        .insert(FontFamily::Name(SEMIBOLD.into()), with("OpenSans-SemiBold"));
    fonts
        .families
        .insert(FontFamily::Name(BOLD.into()), with("OpenSans-Bold"));
    ctx.set_fonts(fonts);
}

/// Node editor colours: nodes are vanilla windows (header bar over content).
pub fn graph_style() -> egui_node_graph2::GraphStyle {
    egui_node_graph2::GraphStyle {
        node_width: 300.0,
        node_fill: CONTENT,
        title_fill: HEADER,
        title_text: TEXT,
        text: TEXT,
        selection: ACCENT,
        rounding: 0.0,
        port_radius: 5.0,
        wire_width: 3.0,
        finder_fill: HEADER,
        finder_text: TEXT,
    }
}
