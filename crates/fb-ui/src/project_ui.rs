//! The project's UI: the tab strip (master graph, open factories, map), the
//! factory list beside the master graph, and the project's totals.

use crate::app::FactoryApp;
use crate::assets::UiIcon;
use crate::dialogs::Dialog;
use crate::fmt::{fmt_num, fmt_rate_unit};
use crate::graph::NodeKind;
use crate::map::View;
use crate::project::MASTER;
use crate::theme::{self, *};
use crate::widgets;
use egui::{vec2, CursorIcon, Rect, RichText, Sense, Ui};
use std::collections::BTreeMap;

const TAB_HEIGHT: f32 = 26.0;
const TAB_MAX: f32 = 200.0;
const TAB_PAD: f32 = 12.0;
const CLOSE: f32 = 18.0;

enum TabAction {
    Open(u32),
    Close(u32),
    Rename(u32),
    Map,
    New,
}

fn tab_width(ui: &Ui, label: &str, closable: bool) -> f32 {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let full = ui.painter().layout_no_wrap(label.to_string(), font, TEXT);
    let extra = if closable { CLOSE + 4.0 } else { 0.0 };
    (full.size().x + TAB_PAD * 2.0 + extra).min(TAB_MAX).max(64.0)
}

/// One tab: its label (truncated to fit), selected/hover fills, and an
/// optional close button. Returns the tab's and the close button's responses.
fn tab(ui: &mut Ui, at: egui::Pos2, label: &str, selected: bool, closable: bool, id: egui::Id) -> (egui::Response, Option<egui::Response>, f32) {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let extra = if closable { CLOSE + 4.0 } else { 0.0 };
    let width = tab_width(ui, label, closable);
    let rect = Rect::from_min_size(at, vec2(width, TAB_HEIGHT));
    let resp = ui.interact(rect, id, Sense::click());
    let close_rect = Rect::from_center_size(egui::pos2(rect.right() - TAB_PAD / 2.0 - CLOSE / 2.0, rect.center().y), vec2(CLOSE, CLOSE));
    let close = closable.then(|| ui.interact(close_rect, id.with("close"), Sense::click()));
    let close_hovered = close.as_ref().is_some_and(|c| c.hovered());
    let (fill, fg) = if selected {
        (TAB_SELECTED, TEXT_DARK)
    } else if resp.hovered() && !close_hovered {
        (ACCENT, TEXT)
    } else {
        (TAB, TEXT)
    };
    ui.painter().rect_filled(rect, 0.0, fill);
    let room = width - TAB_PAD * 2.0 - extra;
    let mut job = egui::text::LayoutJob::simple_singleline(label.to_string(), font, fg);
    job.wrap = egui::text::TextWrapping::truncate_at_width(room);
    let g = ui.painter().layout_job(job);
    let text_at = egui::pos2(rect.left() + TAB_PAD, rect.center().y - g.size().y / 2.0);
    ui.painter().galley(text_at, g, fg);
    #[cfg(test)]
    widgets::TEST_BUTTONS.with(|b| b.borrow_mut().push((label.to_string(), rect, true)));
    (resp, close, width)
}

/// Project, factory and Map tabs across the top of the central area.
pub fn tab_bar(app: &mut FactoryApp, ui: &mut Ui, bar: Rect) {
    ui.painter().rect_filled(bar, 0.0, CONTENT);
    let mut action = None;
    let mut x = bar.left() + 8.0;
    let y = bar.top() + 4.0;
    let graph_view = app.view == View::Graph;
    // The master graph.
    let (r, _, w) = tab(ui, egui::pos2(x, y), "Project", graph_view && app.active == MASTER, false, egui::Id::new("tab_master"));
    if r.on_hover_text("Every factory in the project and how they feed each other").on_hover_cursor(CursorIcon::PointingHand).clicked() {
        action = Some(TabAction::Open(MASTER));
    }
    x += w + 2.0;
    // Room for the factory tabs: everything but the + and Map.
    let map_w = 64.0;
    let limit = bar.right() - 8.0 - map_w - 8.0 - TAB_HEIGHT - 4.0;
    // More tabs than fit: start late enough that the open one shows (the
    // rest are in the factory list beside the master graph).
    let widths: Vec<f32> = app.tabs.iter().map(|id| tab_width(ui, &app.doc_name(*id), true) + 2.0).collect();
    let fits = |from: usize, to: usize| x + widths[from..=to].iter().sum::<f32>() <= limit;
    let mut first = 0;
    if let Some(k) = app.tabs.iter().position(|t| *t == app.active) {
        while first < k && !fits(first, k) {
            first += 1;
        }
    }
    for id in app.tabs.clone().into_iter().skip(first) {
        let name = app.doc_name(id);
        if x + tab_width(ui, &name, true) > limit {
            break;
        }
        let tab_id = egui::Id::new(("tab_factory", id));
        let (r, close, w) = tab(ui, egui::pos2(x, y), &name, graph_view && app.active == id, true, tab_id);
        if let Some(c) = close {
            let hovered = c.hovered();
            let tint = if graph_view && app.active == id { TEXT_DARK } else { TEXT };
            if hovered {
                ui.painter().rect_filled(c.rect, 2.0, if graph_view && app.active == id { DIVIDER } else { BACKDROP });
            }
            if let Some(a) = app.user.assets.as_mut() {
                a.paint_ui(ui, UiIcon::Close, c.rect.shrink(4.0), if hovered { TEXT } else { tint });
            }
            #[cfg(test)]
            widgets::TEST_BUTTONS.with(|b| b.borrow_mut().push((format!("Close {name}"), c.rect, true)));
            if c.on_hover_text("Close this tab (the factory stays in the project)").clicked() {
                action = Some(TabAction::Close(id));
            }
        }
        let r = r.on_hover_text(&name).on_hover_cursor(CursorIcon::PointingHand);
        if r.clicked() {
            action = Some(TabAction::Open(id));
        }
        if r.middle_clicked() {
            action = Some(TabAction::Close(id));
        }
        r.context_menu(|ui| {
            if ui.button("Rename…").clicked() {
                action = Some(TabAction::Rename(id));
                ui.close_menu();
            }
            if ui.button("Close tab").clicked() {
                action = Some(TabAction::Close(id));
                ui.close_menu();
            }
        });
        x += w + 2.0;
    }
    // New factory.
    let add = Rect::from_min_size(egui::pos2(x + 2.0, y), vec2(TAB_HEIGHT, TAB_HEIGHT));
    let r = ui.interact(add, egui::Id::new("tab_add"), Sense::click());
    ui.painter().rect_filled(add, 0.0, if r.hovered() { ACCENT } else { TAB });
    let (c, arm) = (add.center(), 6.0);
    let stroke = egui::Stroke::new(2.0_f32, TEXT);
    ui.painter().line_segment([c - vec2(arm, 0.0), c + vec2(arm, 0.0)], stroke);
    ui.painter().line_segment([c - vec2(0.0, arm), c + vec2(0.0, arm)], stroke);
    #[cfg(test)]
    widgets::TEST_BUTTONS.with(|b| b.borrow_mut().push(("New factory".into(), add, true)));
    if r.on_hover_text("New factory").on_hover_cursor(CursorIcon::PointingHand).clicked() {
        action = Some(TabAction::New);
    }
    // The map, at the right.
    let (r, _, _) = tab(ui, egui::pos2(bar.right() - 8.0 - map_w, y), "Map", app.view == View::Map, false, egui::Id::new("tab_map"));
    if r.on_hover_text("Your save on the game's map").on_hover_cursor(CursorIcon::PointingHand).clicked() {
        action = Some(TabAction::Map);
    }
    // Picking a tab myself takes my view back from whoever I was following.
    if matches!(action, Some(TabAction::Open(_) | TabAction::Map | TabAction::New)) {
        app.room.following = None;
    }
    match action {
        Some(TabAction::Open(id)) => {
            app.view = View::Graph;
            app.switch_to = Some(id);
        }
        Some(TabAction::Close(id)) => app.close_tab(id),
        Some(TabAction::Rename(id)) => app.dialog = Dialog::RenameFactory { id, name: app.doc_name(id) },
        Some(TabAction::Map) => app.view = View::Map,
        Some(TabAction::New) => app.new_factory(),
        None => {}
    }
}

fn padded(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        egui::Frame::none().inner_margin(egui::Margin { left: 12.0, right: 12.0, top: 8.0, bottom: 16.0 }).show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
    });
}

enum ListAction {
    Open(u32),
    Rename(u32),
    Duplicate(u32),
    Delete(u32),
    New,
    AddFile,
    AddCode,
}

/// Beside the master graph: the factories, to open, rename, copy or delete.
pub fn factories_panel(app: &mut FactoryApp, ui: &mut Ui) {
    ui.spacing_mut().item_spacing.y = 0.0;
    widgets::panel_header(ui, "Factories");
    if !app.game_loaded {
        return;
    }
    let infos = app.factory_infos();
    let mut action = None;
    let Some(assets) = app.user.assets.as_mut() else { return };
    padded(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        widgets::caption(ui, "Each factory is its own graph and plan. Click one to open it; wire their blocks together on the master graph to send one factory's products to another.");
        ui.add_space(8.0);
        for (id, f) in &infos {
            let row = widgets::list_row(ui, 44.0, |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                match f.outs.first() {
                    Some((item, _)) => {
                        assets.item(ui, item, 28.0);
                    }
                    None => {
                        assets.ui(ui, UiIcon::Factory, 28.0, TEXT_DIM);
                    }
                }
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.add_space(4.0);
                    widgets::name_label(ui, RichText::new(&f.name).color(TEXT));
                    let n = f.machines;
                    widgets::name_label(ui, RichText::new(format!("{n} {} · {} MW", if n == 1 { "building" } else { "buildings" }, fmt_num(f.power_mw))).small().color(TEXT_DIM));
                });
            });
            #[cfg(test)]
            widgets::TEST_BUTTONS.with(|b| b.borrow_mut().push((format!("Factory {}", f.name), row.rect, true)));
            if row.clicked() {
                action = Some(ListAction::Open(*id));
            }
            row.context_menu(|ui| {
                for (label, a) in [("Open", ListAction::Open(*id)), ("Rename…", ListAction::Rename(*id)), ("Duplicate", ListAction::Duplicate(*id)), ("Delete…", ListAction::Delete(*id))] {
                    if ui.button(label).clicked() {
                        action = Some(a);
                        ui.close_menu();
                    }
                }
            });
        }
        ui.add_space(6.0);
        if widgets::add_row(ui, assets, "New factory").clicked() {
            action = Some(ListAction::New);
        }
        ui.add_space(8.0);
        ui.spacing_mut().item_spacing.y = 6.0;
        if widgets::button_full(ui, "Add factory file…").clicked() {
            action = Some(ListAction::AddFile);
        }
        if widgets::button_full(ui, "Add factory from a share code…").clicked() {
            action = Some(ListAction::AddCode);
        }
    });
    if matches!(action, Some(ListAction::Open(_) | ListAction::New)) {
        app.room.following = None;
    }
    match action {
        Some(ListAction::Open(id)) => app.switch_to = Some(id),
        Some(ListAction::Rename(id)) => app.dialog = Dialog::RenameFactory { id, name: app.doc_name(id) },
        Some(ListAction::Duplicate(id)) => {
            app.duplicate_factory(id);
        }
        Some(ListAction::Delete(id)) => app.dialog = Dialog::DeleteFactories { ids: vec![id] },
        Some(ListAction::New) => app.new_factory(),
        Some(ListAction::AddFile) => app.pick_open(crate::app_files::OpenPurpose::AddFactory),
        Some(ListAction::AddCode) => app.dialog = Dialog::share_code(crate::dialogs::ShareMode::AddFactory),
        None => {}
    }
}

/// The whole project's numbers.
#[derive(Default)]
pub(crate) struct ProjectTotals {
    pub factories: usize,
    pub power_mw: f64,
    pub generation: Option<f64>,
    pub power_shards: u32,
    pub somersloops: u32,
    pub buildings: BTreeMap<String, (f64, f64)>,
    pub resources: BTreeMap<String, f64>,
    /// What leaves the project: factory outputs no other factory takes.
    pub products: BTreeMap<String, f64>,
    /// What the project needs from outside: unlinked factory inputs.
    pub from_outside: BTreeMap<String, f64>,
}

impl FactoryApp {
    /// Totals over every factory; flows between them come from the master
    /// graph (only while it's open, which is when they're shown).
    pub(crate) fn project_totals(&mut self) -> ProjectTotals {
        let mut t = ProjectTotals::default();
        for (_, f) in self.factory_infos() {
            t.factories += 1;
            t.power_mw += f.power_mw;
            if let Some(mw) = f.generation {
                *t.generation.get_or_insert(0.0) += mw;
            }
            t.power_shards += f.power_shards;
            t.somersloops += f.somersloops;
            for (b, (whole, m)) in &f.buildings {
                let e = t.buildings.entry(b.clone()).or_default();
                e.0 += whole;
                e.1 += m;
            }
            for (item, r) in &f.resources {
                *t.resources.entry(item.clone()).or_default() += r;
            }
        }
        if self.on_master() {
            let g = &self.state.graph;
            for node in g.iter_nodes() {
                let NodeKind::Factory { ins, outs, .. } = &g[node].user_data.kind else { continue };
                let eff = self.user.nodes.get(&node).map(|n| n.efficiency).unwrap_or(1.0);
                for (_, o) in &g[node].outputs {
                    let item = &g[*o].typ.0;
                    let made: f64 = outs.iter().filter(|(i, _)| i == item).map(|(_, r)| r).sum::<f64>() * eff;
                    let taken: f64 = self.wire_flow.iter().filter(|((_, out), _)| out == o).map(|(_, f)| f).sum();
                    let left = made - taken;
                    if left > 1e-6 {
                        *t.products.entry(item.clone()).or_default() += left;
                    }
                }
                for (_, i) in &g[node].inputs {
                    if g.connections(*i).is_empty() {
                        let item = &g[*i].typ.0;
                        let need: f64 = ins.iter().filter(|(x, _)| x == item).map(|(_, r)| r).sum();
                        if need > 1e-6 {
                            *t.from_outside.entry(item.clone()).or_default() += need;
                        }
                    }
                }
            }
        }
        t
    }

    pub(crate) fn project_summary_text(&mut self) -> String {
        let t = self.project_totals();
        let infos = self.factory_infos();
        let game = self.user.game.clone();
        let rate = |item: &str, r: f64| fmt_rate_unit(r, game.item_form(item));
        let mut out = format!("{}\n", self.project_name.clone().unwrap_or_else(|| "Untitled project".into()));
        out.push_str(&format!("Power: {} MW\n", fmt_num(t.power_mw)));
        if let Some(mw) = t.generation {
            out.push_str(&format!("Power production: {} MW\n", fmt_num(mw)));
        }
        out.push_str("\nFactories\n");
        for (_, f) in &infos {
            out.push_str(&format!("  {}: {} buildings, {} MW\n", f.name, f.machines, fmt_num(f.power_mw)));
        }
        for (title, m) in [("Products", &t.products), ("From outside", &t.from_outside), ("Raw resources", &t.resources)] {
            if !m.is_empty() {
                out.push_str(&format!("\n{title}\n"));
                for (item, r) in m {
                    out.push_str(&format!("  {}: {}\n", game.item_name(item), rate(item, *r)));
                }
            }
        }
        out.push_str("\nMade with Factory Builder\n");
        out
    }
}

/// Beside the master graph: the whole project's totals and problems.
pub fn project_overview(app: &mut FactoryApp, ui: &mut Ui) {
    ui.spacing_mut().item_spacing.y = 0.0;
    widgets::panel_header(ui, "Project");
    if !app.game_loaded {
        return;
    }
    let game = app.user.game.clone();
    let t = app.project_totals();
    let issues = app.summary.issues.clone();
    let extraction = app.planner.extraction.clone();
    let mut focus = None;
    let mut follow = None;
    let room = &app.room;
    let Some(assets) = app.user.assets.as_mut() else { return };
    padded(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        if room.active() {
            follow = crate::room_extras::room_section(ui, room);
        }
        ui.label(RichText::new(format!("{} {}", t.factories, if t.factories == 1 { "factory" } else { "factories" })).color(TEXT_DIM));
        crate::power_ui::overview_power(ui, assets, t.power_mw, t.generation);
        if t.power_shards > 0 {
            widgets::info_row(ui, assets, fb_model::POWER_SHARD, "Power Shards", RichText::new(t.power_shards.to_string()).color(TEXT));
        }
        if t.somersloops > 0 {
            widgets::info_row(ui, assets, fb_model::SOMERSLOOP, "Somersloops", RichText::new(t.somersloops.to_string()).color(TEXT));
        }
        let flows = |ui: &mut Ui, assets: &mut crate::assets::Assets, title: &str, note: &str, m: &BTreeMap<String, f64>| {
            if m.is_empty() {
                return;
            }
            widgets::divider(ui);
            widgets::section(ui, title, None);
            widgets::caption(ui, note);
            for (item, r) in m {
                widgets::info_row(ui, assets, item, game.item_name(item), RichText::new(fmt_rate_unit(*r, game.item_form(item))).color(TEXT));
            }
        };
        flows(ui, assets, "Products", "What the factories make that no other factory in the project takes.", &t.products);
        flows(ui, assets, "From outside", "Factory inputs no other factory supplies.", &t.from_outside);
        if !t.buildings.is_empty() {
            widgets::divider(ui);
            widgets::section(ui, "Buildings", None);
            for (b, (whole, machines)) in &t.buildings {
                if let Some(bd) = game.buildings.get(b) {
                    widgets::split_row(ui, 26.0, |ui| {
                        ui.label(RichText::new(format!("{whole}")).color(TEXT)).on_hover_text(format!("{} machines' worth of work at 100% clock speed", fmt_num(*machines)));
                    }, |ui| {
                        assets.item(ui, &bd.icon, 24.0);
                        widgets::name_label(ui, RichText::new(&bd.name).color(TEXT));
                    });
                }
            }
        }
        flows(ui, assets, "Raw resources", "Mined across all the factories.", &t.resources);
        crate::overview_extra::build_cost(ui, assets, &game, &t.buildings, &t.resources, &extraction);
        if !issues.is_empty() {
            widgets::divider(ui);
            ui.horizontal(|ui| {
                assets.ui(ui, UiIcon::Warning, 16.0, ACCENT);
                ui.label(RichText::new(format!("Issues ({})", issues.len())).font(theme::semibold(14.0)).color(ACCENT));
            });
            ui.spacing_mut().item_spacing.y = 0.0;
            for (node, text) in &issues {
                let row = widgets::list_row(ui, 28.0, |ui| {
                    ui.add(egui::Label::new(RichText::new(text).small().color(TEXT)).truncate());
                });
                if row.on_hover_text(format!("{text}\nClick to show it on the master graph.")).clicked() {
                    focus = Some(*node);
                }
            }
        }
    });
    if focus.is_some() {
        app.focus_node = focus;
    }
    if let Some(id) = follow {
        app.toggle_follow(id);
    }
}
