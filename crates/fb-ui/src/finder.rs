//! "Add node" menu (right-click on empty canvas). Replaces the node graph's
//! built-in finder: every entry has an icon, a name and a one-line
//! description; groups show their size; typing gives one flat, ranked list;
//! Up/Down + Enter work, Escape closes. Opened by letting go of a wire on
//! empty canvas, it lists only what fits that wire, and connects it.

use crate::assets::{Assets, UiIcon};
use crate::fmt::fmt_num;
use crate::graph::Template;
use crate::theme::{self, *};
use crate::widgets;
use egui::{vec2, Align2, Color32, Key, Pos2, Rect, RichText, Sense, Ui};
use fb_model::GameData;

pub struct Entry {
    pub template: Template,
    pub title: String,
    pub subtitle: String,
    pub group: String,
    /// Item (or building) whose icon represents the entry.
    pub icon: String,
    alternate: bool,
    search: String,
    /// Items it takes in, and puts out (for wires dropped on the canvas).
    takes: Vec<String>,
    makes: Vec<String>,
}

pub struct Group {
    pub name: String,
    pub icon: GroupIcon,
    pub entries: Vec<usize>,
}

pub enum GroupIcon {
    Item(String),
    Ui(UiIcon),
}

pub struct Catalog {
    pub entries: Vec<Entry>,
    pub groups: Vec<Group>,
}

impl Catalog {
    pub fn new(game: &GameData) -> Self {
        let mut entries = Vec::new();
        let names = |list: &[fb_model::Amount]| {
            list.iter()
                .map(|a| game.item_name(&a.item).to_string())
                .collect::<Vec<_>>()
                .join(", ")
        };
        for r in game.recipes.values() {
            let Some(b) = game.buildings.get(&r.building) else {
                continue;
            };
            let Some(icon) = r.icon_item().map(str::to_string) else {
                continue;
            };
            // Generators make power, not (only) items.
            let made = if r.is_generator() {
                let mw = format!("{} MW", fmt_num(r.generates_mw));
                if r.products.is_empty() { mw } else { format!("{mw}, {}", names(&r.products)) }
            } else {
                names(&r.products)
            };
            entries.push(Entry {
                template: Template::Recipe(r.class.clone()),
                title: r.name.clone(),
                // Open Sans (the game font) has no arrows; › reads the same.
                subtitle: format!(
                    "{} · {} › {}",
                    b.name,
                    names(&r.ingredients),
                    made
                ),
                group: b.name.clone(),
                icon,
                alternate: r.alternate,
                search: String::new(),
                takes: r.ingredients.iter().map(|a| a.item.clone()).collect(),
                makes: r.products.iter().map(|a| a.item.clone()).collect(),
            });
        }
        for (class, item) in &game.items {
            if item.raw {
                let limit = game
                    .resource_limits
                    .get(class)
                    .map(|l| format!("up to {} per min on the map", fmt_num(*l)))
                    .unwrap_or_else(|| "unlimited".into());
                entries.push(Entry {
                    template: Template::Resource(class.clone()),
                    title: item.name.clone(),
                    subtitle: format!("Raw resource, {limit}"),
                    group: "Resources".into(),
                    icon: class.clone(),
                    alternate: false,
                    search: String::new(),
                    takes: vec![],
                    makes: vec![class.clone()],
                });
            }
            for (template, group, subtitle) in [
                (
                    Template::Input(class.clone()),
                    "Inputs",
                    "Brought in from another factory",
                ),
                (
                    Template::Output(class.clone()),
                    "Outputs",
                    "Delivered by this factory",
                ),
                (
                    Template::Sink(class.clone()),
                    "Sinks",
                    "Leftovers, sunk or stored",
                ),
            ] {
                let (takes, makes) = match template {
                    Template::Input(_) => (vec![], vec![class.clone()]),
                    _ => (vec![class.clone()], vec![]),
                };
                entries.push(Entry {
                    template,
                    title: item.name.clone(),
                    subtitle: subtitle.into(),
                    group: group.into(),
                    icon: class.clone(),
                    alternate: false,
                    search: String::new(),
                    takes,
                    makes,
                });
            }
        }
        // Mergers and splitters for every item (one junction for fluids,
        // as in game), named for the item, with the game's icons.
        for (class, item) in &game.items {
            let kinds: &[(Template, &str, &str)] = if item.form.is_fluid() {
                &[(Template::Merger(class.clone()), "Pipeline Junctions", "Pipeline Junction · joins and shares out pipes")]
            } else {
                &[
                    (Template::Merger(class.clone()), "Mergers", "Merger · joins belts into one"),
                    (Template::Splitter(class.clone()), "Splitters", "Splitter · shares a belt out over several"),
                ]
            };
            for (template, group, subtitle) in kinds {
                let icon = crate::routers::icon(game, &template.kind()).to_string();
                entries.push(Entry {
                    template: template.clone(),
                    title: item.name.clone(),
                    subtitle: (*subtitle).into(),
                    group: (*group).into(),
                    icon,
                    alternate: false,
                    search: String::new(),
                    takes: vec![class.clone()],
                    makes: vec![class.clone()],
                });
            }
        }
        for e in &mut entries {
            e.search = format!("{} {}", e.title, e.subtitle).to_lowercase();
        }
        entries.sort_by(|a, b| a.alternate.cmp(&b.alternate).then(a.title.cmp(&b.title)));

        // Machine groups (by building, A–Z) first, then the item groups.
        let mut buildings: Vec<&fb_model::Building> = game.buildings.values().collect();
        buildings.sort_by(|a, b| a.name.cmp(&b.name));
        let mut groups: Vec<Group> = buildings
            .iter()
            .map(|b| Group {
                name: b.name.clone(),
                icon: GroupIcon::Item(b.icon.clone()),
                entries: vec![],
            })
            .collect();
        for (name, icon) in [
            ("Resources", UiIcon::Factory),
            ("Inputs", UiIcon::Arrow),
            ("Outputs", UiIcon::Arrow),
            ("Sinks", UiIcon::Recycle),
        ] {
            groups.push(Group {
                name: name.into(),
                icon: GroupIcon::Ui(icon),
                entries: vec![],
            });
        }
        for (name, icon) in [("Mergers", "Desc_ConveyorAttachmentMerger_C"), ("Splitters", "Desc_ConveyorAttachmentSplitter_C"), ("Pipeline Junctions", "Desc_PipelineJunction_Cross_C")] {
            groups.push(Group { name: name.into(), icon: GroupIcon::Item(icon.into()), entries: vec![] });
        }
        for (i, e) in entries.iter().enumerate() {
            if let Some(g) = groups.iter_mut().find(|g| g.name == e.group) {
                g.entries.push(i);
            }
        }
        groups.retain(|g| !g.entries.is_empty());
        Self { entries, groups }
    }

    /// Ranked matches: every word must appear; title prefix matches first,
    /// standard recipes before alternates. With a wire, only what fits it.
    pub fn search(&self, query: &str, wire: Option<&WireDrop>) -> Vec<usize> {
        let words: Vec<String> = query
            .to_lowercase()
            .split_whitespace()
            .map(str::to_string)
            .collect();
        let q = query.trim().to_lowercase();
        let mut hits: Vec<(u8, usize)> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| words.iter().all(|w| e.search.contains(w.as_str())))
            .filter(|(_, e)| wire.is_none_or(|w| w.fits(e)))
            .map(|(i, e)| {
                let t = e.title.to_lowercase();
                let rank = if t == q {
                    0
                } else if t.starts_with(&q) {
                    1
                } else if t.contains(&q) {
                    2
                } else {
                    3
                };
                (rank, i)
            })
            .collect();
        hits.sort_by_key(|&(rank, i)| (rank, i));
        hits.into_iter().map(|h| h.1).take(150).collect()
    }
}

/// A wire let go on empty canvas: the item on it, and whether it comes from
/// an output (so the new node takes the item in) or an input (it makes it).
#[derive(Clone)]
pub struct WireDrop {
    pub node: egui_node_graph2::NodeId,
    pub port: egui_node_graph2::AnyParameterId,
    pub item: String,
    pub item_name: String,
    pub from_output: bool,
    /// Outputs of other selected nodes pulled along with it.
    pub extras: Vec<egui_node_graph2::OutputId>,
}

impl WireDrop {
    fn fits(&self, e: &Entry) -> bool {
        let list = if self.from_output { &e.takes } else { &e.makes };
        list.contains(&self.item)
    }
}

pub struct FinderState {
    /// Screen position the menu opened at (also where the node goes).
    pub pos: Pos2,
    pub query: String,
    pub highlight: usize,
    pub just_opened: bool,
    pub wire: Option<WireDrop>,
}

impl FinderState {
    pub fn new(pos: Pos2) -> Self {
        Self {
            pos,
            query: String::new(),
            highlight: 0,
            just_opened: true,
            wire: None,
        }
    }
}

pub enum FinderResult {
    Open,
    Close,
    Add(Template),
}

const WIDTH: f32 = 440.0;
const ROW: f32 = 44.0;

fn entry_row(
    ui: &mut Ui,
    assets: &mut Assets,
    e: &Entry,
    highlighted: bool,
    show_group: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
    if highlighted || resp.hovered() {
        ui.painter().rect_filled(rect, 0.0, ROW_HOVER);
    }
    let icon_rect = Rect::from_min_size(rect.left_top() + vec2(8.0, 7.0), vec2(30.0, 30.0));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(icon_rect));
    assets.item(&mut child, &e.icon, 30.0);
    let text_left = rect.left() + 48.0;
    let right_pad = if show_group { 110.0 } else { 8.0 };
    let width = rect.right() - right_pad - text_left;
    let title = egui::WidgetText::from(RichText::new(&e.title).color(TEXT)).into_galley(
        ui,
        Some(egui::TextWrapMode::Truncate),
        width,
        egui::TextStyle::Body,
    );
    let sub = egui::WidgetText::from(RichText::new(&e.subtitle).small().color(TEXT_DIM))
        .into_galley(
            ui,
            Some(egui::TextWrapMode::Truncate),
            width,
            egui::TextStyle::Small,
        );
    ui.painter()
        .galley(egui::pos2(text_left, rect.top() + 5.0), title, TEXT);
    ui.painter()
        .galley(egui::pos2(text_left, rect.top() + 25.0), sub, TEXT_DIM);
    if show_group {
        ui.painter().text(
            rect.right_center() - vec2(8.0, 0.0),
            Align2::RIGHT_CENTER,
            &e.group,
            egui::FontId::proportional(12.0),
            TEXT_DIM,
        );
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub fn show(
    ctx: &egui::Context,
    state: &mut FinderState,
    catalog: &Catalog,
    assets: &mut Assets,
) -> FinderResult {
    let mut result = FinderResult::Open;
    let screen = ctx.screen_rect();
    let height = 520.0_f32.min(screen.height() - 40.0);
    let pos = egui::pos2(
        state.pos.x.min(screen.right() - WIDTH - 8.0).max(8.0),
        state.pos.y.min(screen.bottom() - height - 8.0).max(8.0),
    );
    // A flat list when searching, or for a dropped wire (only what fits).
    let searching = !state.query.trim().is_empty() || state.wire.is_some();
    let hits = if searching {
        catalog.search(&state.query, state.wire.as_ref())
    } else {
        Vec::new()
    };

    // Keyboard navigation over the flat result list.
    let (up, down, enter, escape) = ctx.input(|i| {
        (
            i.key_pressed(Key::ArrowUp),
            i.key_pressed(Key::ArrowDown),
            i.key_pressed(Key::Enter),
            i.key_pressed(Key::Escape),
        )
    });
    if escape {
        return FinderResult::Close;
    }
    if searching && !hits.is_empty() {
        if down {
            state.highlight = (state.highlight + 1).min(hits.len() - 1);
        }
        if up {
            state.highlight = state.highlight.saturating_sub(1);
        }
        state.highlight = state.highlight.min(hits.len() - 1);
        if enter {
            return FinderResult::Add(catalog.entries[hits[state.highlight]].template.clone());
        }
    }

    let area = egui::Area::new(egui::Id::new("add_node_menu")).order(egui::Order::Foreground).fixed_pos(pos).show(ctx, |ui| {
        egui::Frame::none().fill(CONTENT).stroke(ui.visuals().window_stroke).shadow(ui.visuals().popup_shadow).show(ui, |ui| {
            ui.set_width(WIDTH);
            ui.spacing_mut().item_spacing.y = 0.0;
            match &state.wire {
                Some(w) if w.from_output => widgets::panel_header(ui, &format!("Add node that takes {}", w.item_name)),
                Some(w) => widgets::panel_header(ui, &format!("Add node that makes {}", w.item_name)),
                None => widgets::panel_header(ui, "Add node"),
            }
            egui::Frame::none().inner_margin(egui::Margin::same(8.0)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    assets.ui(ui, UiIcon::Search, 16.0, TEXT_DIM);
                    let before = state.query.clone();
                    let r = ui.add(egui::TextEdit::singleline(&mut state.query).hint_text("Search machines, recipes and items").desired_width(f32::INFINITY));
                    if state.just_opened {
                        r.request_focus();
                        state.just_opened = false;
                    }
                    if state.query != before {
                        state.highlight = 0;
                    }
                });
            });
            let (line, _) = ui.allocate_exact_size(vec2(WIDTH, 1.0), Sense::hover());
            ui.painter().rect_filled(line, 0.0, DIVIDER);
            egui::ScrollArea::vertical().max_height(height - 90.0).auto_shrink([false, true]).show(ui, |ui| {
                ui.set_width(WIDTH);
                if searching {
                    if hits.is_empty() {
                        let text = match &state.wire {
                            Some(_) if state.query.trim().is_empty() => "Nothing in the game fits this wire.",
                            Some(_) => "Nothing that fits this wire matches.",
                            None => "Nothing matches. Try an item name, like “Iron Plate”.",
                        };
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            ui.add_space(12.0);
                            ui.label(RichText::new(text).color(TEXT_DIM));
                        });
                        ui.add_space(12.0);
                    }
                    for (n, &i) in hits.iter().enumerate() {
                        let e = &catalog.entries[i];
                        let row = entry_row(ui, assets, e, n == state.highlight, true);
                        if n == state.highlight && (up || down) {
                            row.scroll_to_me(None);
                        }
                        if row.clicked() {
                            result = FinderResult::Add(e.template.clone());
                        }
                    }
                } else {
                    for g in &catalog.groups {
                        let id = ui.make_persistent_id(("add_node_group", &g.name));
                        let state_c = egui::collapsing_header::CollapsingState::load_with_default_open(ctx, id, false);
                        let open = state_c.is_open();
                        let (rect, header) = ui.allocate_exact_size(vec2(WIDTH, 34.0), Sense::click());
                        if header.hovered() {
                            ui.painter().rect_filled(rect, 0.0, ROW_HOVER);
                        }
                        let caret = Rect::from_center_size(rect.left_center() + vec2(14.0, 0.0), vec2(9.0, 9.0));
                        if open {
                            assets.paint_ui(ui, UiIcon::Caret, caret, TEXT_DIM);
                        } else {
                            assets.paint_ui(ui, UiIcon::Arrow, caret.expand(2.0), TEXT_DIM);
                        }
                        let icon_rect = Rect::from_min_size(rect.left_top() + vec2(28.0, 5.0), vec2(24.0, 24.0));
                        match &g.icon {
                            GroupIcon::Item(class) => {
                                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(icon_rect));
                                assets.item(&mut child, class, 24.0);
                            }
                            GroupIcon::Ui(icon) => assets.paint_ui(ui, *icon, icon_rect.shrink(3.0), TEXT_DIM),
                        }
                        ui.painter().text(rect.left_center() + vec2(60.0, 0.0), Align2::LEFT_CENTER, &g.name, theme::semibold(14.0), TEXT);
                        ui.painter().text(rect.right_center() - vec2(12.0, 0.0), Align2::RIGHT_CENTER, g.entries.len().to_string(), egui::FontId::proportional(12.0), TEXT_DIM);
                        if header.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                            let mut s = state_c;
                            s.toggle(ui);
                            s.store(ctx);
                        }
                        if open {
                            for &i in &g.entries {
                                let e = &catalog.entries[i];
                                if entry_row(ui, assets, e, false, false).clicked() {
                                    result = FinderResult::Add(e.template.clone());
                                }
                            }
                        }
                    }
                }
            });
            let (bar, _) = ui.allocate_exact_size(vec2(WIDTH, 26.0), Sense::hover());
            ui.painter().rect_filled(bar, 0.0, HEADER);
            ui.painter().text(
                bar.left_center() + vec2(10.0, 0.0),
                Align2::LEFT_CENTER,
                if searching { "Up/Down to choose · Enter to add · Esc to close" } else { "Type to search · Esc to close" },
                egui::FontId::proportional(12.0),
                TEXT_DIM,
            );
        });
    });
    let outside = ctx.input(|i| i.pointer.any_pressed())
        && !area
            .response
            .rect
            .contains(ctx.input(|i| i.pointer.interact_pos().unwrap_or_default()));
    if outside && !matches!(result, FinderResult::Add(_)) {
        return FinderResult::Close;
    }
    let _: Color32 = TEXT;
    result
}
