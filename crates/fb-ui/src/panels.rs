//! Side panels, each laid out like a vanilla window: header bar, content.

use crate::app::FactoryApp;
use crate::assets::{Assets, UiIcon};
use crate::fmt::{fmt_num, fmt_rate, fmt_rate_unit};
use crate::theme::{self, *};
use crate::widgets;
use egui::{vec2, Align, Id, Layout, RichText, ScrollArea, Ui};
use fb_model::{GameData, Objective, Recipe, Target};

fn padded(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::Frame::none()
                .inner_margin(egui::Margin {
                    left: 12.0,
                    right: 12.0,
                    top: 8.0,
                    bottom: 16.0,
                })
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    add(ui);
                });
        });
}

/// Items that can be made or mined, sorted by name, for the pickers.
fn item_choices(game: &GameData, producible_only: bool) -> Vec<(String, String)> {
    let made: std::collections::BTreeSet<&str> = game
        .recipes
        .values()
        .flat_map(|r| r.products.iter().map(|p| p.item.as_str()))
        .collect();
    let mut v: Vec<(String, String)> = game
        .items
        .values()
        .filter(|i| !producible_only || i.raw || made.contains(i.class.as_str()))
        .map(|i| (i.class.clone(), i.name.clone()))
        .collect();
    v.sort_by(|a, b| a.1.cmp(&b.1));
    v
}

/// Rows of item + rate + remove. Returns true if anything changed.
fn target_rows(
    ui: &mut Ui,
    assets: &mut Assets,
    game: &GameData,
    id: &str,
    list: &mut Vec<Target>,
    choices: &[(String, String)],
    empty: &str,
    first: &str,
    rate: f64,
) -> bool {
    let mut changed = false;
    let mut remove = None;
    if list.is_empty() && widgets::add_row(ui, assets, empty).clicked() {
        let item = choices
            .iter()
            .find(|(c, _)| c == first)
            .or(choices.first())
            .map(|x| x.0.clone())
            .unwrap_or_default();
        list.push(Target { item, rate });
        changed = true;
    }
    for (i, t) in list.iter_mut().enumerate() {
        // Right to left: the fixed-size controls first, then the item picker
        // takes whatever width is left, so the row never outgrows the panel.
        let row_h = ui.spacing().interact_size.y;
        ui.allocate_ui_with_layout(
            vec2(ui.available_width(), row_h),
            Layout::right_to_left(Align::Center),
            |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                if widgets::icon_button(ui, assets, UiIcon::Trash, "Remove this item").clicked() {
                    remove = Some(i);
                }
                let unit = if game.item_form(&t.item).is_fluid() {
                    "m³/min"
                } else {
                    "per min"
                };
                ui.allocate_ui_with_layout(
                    vec2(48.0, row_h),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        ui.label(RichText::new(unit).small().color(TEXT_DIM));
                    },
                );
                changed |= widgets::num_field(ui, Id::new((id, i, "rate")), &mut t.rate, 56.0);
                let combo_w = ui.available_width().max(80.0);
                changed |= widgets::item_combo(
                    ui,
                    assets,
                    Id::new((id, i, "item")),
                    &mut t.item,
                    choices,
                    combo_w,
                );
            },
        );
    }
    if let Some(i) = remove {
        list.remove(i);
        changed = true;
    }
    changed
}

pub fn planner(app: &mut FactoryApp, ui: &mut Ui) {
    ui.spacing_mut().item_spacing.y = 0.0;
    widgets::panel_header(ui, "Production plan");
    if !app.game_loaded {
        padded(ui, |ui| {
            ui.label(
                RichText::new(if app.game_error.is_some() {
                    "Game data didn't load. Check the folders in File › Settings."
                } else {
                    "Reading game data…"
                })
                .color(TEXT_DIM),
            );
        });
        return;
    }
    let game = app.user.game.clone();
    let producible = item_choices(&game, true);
    let all = item_choices(&game, false);
    let has_graph = !app.state.graph.nodes.is_empty();
    let mut changed = false;
    let mut plan = false;
    let mut open_saves = false;
    let mut said: Option<String> = None;
    {
        let FactoryApp {
            planner,
            user,
            solving,
            solve_notes,
            ..
        } = app;
        let assets = user.assets.as_mut().expect("assets");
        padded(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;

            if let Some(r) = widgets::section(
                ui,
                "What to make",
                Some((assets, UiIcon::Add, "Add another item to make")),
            ) {
                if r.clicked() {
                    let item = producible
                        .iter()
                        .find(|(c, _)| c == "Desc_IronPlate_C")
                        .or(producible.first())
                        .map(|x| x.0.clone())
                        .unwrap_or_default();
                    planner.targets.push(Target { item, rate: 10.0 });
                    changed = true;
                }
            }
            widgets::caption(
                ui,
                "The items this factory should produce, and how many per minute.",
            );
            changed |= target_rows(
                ui,
                assets,
                &game,
                "targets",
                &mut planner.targets,
                &producible,
                "Add an item to make",
                "Desc_IronPlate_C",
                10.0,
            );

            widgets::divider(ui);
            if let Some(r) = widgets::section(
                ui,
                "Already available",
                Some((assets, UiIcon::Add, "Add another item you already have")),
            ) {
                if r.clicked() {
                    let item = all
                        .iter()
                        .find(|(c, _)| c == "Desc_IronIngot_C")
                        .or(all.first())
                        .map(|x| x.0.clone())
                        .unwrap_or_default();
                    planner.inputs.push(Target { item, rate: 30.0 });
                    changed = true;
                }
            }
            widgets::caption(ui, "Items you already bring in from another factory. The plan uses these before mining more. Optional.");
            changed |= target_rows(
                ui,
                assets,
                &game,
                "inputs",
                &mut planner.inputs,
                &all,
                "Add an item you already have",
                "Desc_IronIngot_C",
                30.0,
            );

            widgets::divider(ui);
            widgets::section(ui, "Optimize for", None);
            widgets::caption(ui, "What the plan should use as little of as possible.");
            changed |= widgets::tabs(
                ui,
                &mut planner.objective,
                &[
                    (
                        Objective::Resources,
                        "Resources",
                        "Use the fewest raw resources. Scarce ones, like Uranium, count for more.",
                    ),
                    (Objective::Power, "Power", "Use the least power."),
                    (
                        Objective::Buildings,
                        "Buildings",
                        "Use the fewest machines.",
                    ),
                ],
            );
            ui.add_space(6.0);
            let label = if *solving {
                "Planning…"
            } else {
                "Plan factory"
            };
            let resp = widgets::button_ex_full_enabled(
                ui,
                label,
                !*solving && !planner.targets.is_empty(),
            );
            let resp = if has_graph {
                resp.on_hover_text("Replaces the current graph. Undo (Ctrl+Z) brings it back.")
            } else {
                resp
            };
            if resp.clicked() {
                plan = true;
            }
            for note in solve_notes.iter() {
                ui.horizontal_wrapped(|ui| {
                    assets.ui(ui, UiIcon::Warning, 14.0, DANGER);
                    ui.label(RichText::new(note).color(TEXT));
                });
            }

            widgets::divider(ui);
            changed |= recipes(ui, assets, &game, planner, &mut open_saves, &mut said);
            widgets::divider(ui);
            changed |= limits(ui, assets, &game, planner);
        });
    }
    if changed {
        app.changed();
    }
    if let Some(text) = said {
        app.say(ui.ctx(), text, false);
    }
    if open_saves {
        app.open_saves_dialog();
    }
    if plan {
        app.request_plan();
    }
}

fn recipes(
    ui: &mut Ui,
    assets: &mut Assets,
    game: &GameData,
    planner: &mut crate::app::Planner,
    open_saves: &mut bool,
    said: &mut Option<String>,
) -> bool {
    let mut changed = false;
    let alts: Vec<&Recipe> = game.recipes.values().filter(|r| r.alternate).collect();
    let on = alts
        .iter()
        .filter(|r| planner.enabled.contains(&r.class))
        .count();
    let std_off = game
        .recipes
        .values()
        .filter(|r| !r.alternate && !planner.enabled.contains(&r.class))
        .count();
    let summary = match (on, std_off) {
        (0, 0) => "Standard recipes only".to_string(),
        (n, 0) => format!("Standard recipes + {n} of {} alternates", alts.len()),
        (n, off) => format!("{off} standard off · {n} of {} alternates on", alts.len()),
    };
    widgets::collapsible(ui, assets, "recipes", "Recipes", &summary, |ui, assets| {
        widgets::caption(ui, "Which recipes the plan may use. Match them to your game from a save or a HUB tier, or tick them by hand; hover a recipe to see what it takes.");
        if let Some(text) = crate::progress_ui::unlock_controls(ui, game, planner, open_saves) {
            *said = Some(text);
            changed = true;
        }
        ui.horizontal(|ui| {
            if widgets::button(ui, "Enable all alternates").clicked() {
                planner.enabled.extend(alts.iter().map(|r| r.class.clone()));
                changed = true;
            }
            if widgets::button(ui, "Disable all").clicked() {
                for r in &alts {
                    planner.enabled.remove(&r.class);
                }
                changed = true;
            }
        });
        ui.horizontal(|ui| {
            assets.ui(ui, UiIcon::Search, 16.0, TEXT_DIM);
            ui.add(
                egui::TextEdit::singleline(&mut planner.recipe_filter)
                    .hint_text("Search recipes")
                    .desired_width(f32::INFINITY),
            );
        });
        let filter = planner.recipe_filter.to_lowercase();
        let mut list: Vec<&Recipe> = game
            .recipes
            .values()
            .filter(|r| filter.is_empty() || r.name.to_lowercase().contains(&filter))
            .collect();
        list.sort_by(|a, b| b.alternate.cmp(&a.alternate).then(a.name.cmp(&b.name)));
        ScrollArea::vertical()
            .id_salt("recipe_list")
            .max_height(360.0)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for r in list {
                    let enabled = planner.enabled.contains(&r.class);
                    let mut box_rect = egui::Rect::NOTHING;
                    let row = widgets::list_row(ui, 30.0, |ui| {
                        let (rect, _) =
                            ui.allocate_exact_size(vec2(20.0, 20.0), egui::Sense::hover());
                        box_rect = rect;
                        if let Some(p) = r.products.first() {
                            assets.item(ui, &p.item, 22.0);
                        }
                        ui.label(RichText::new(&r.name).color(if enabled {
                            TEXT
                        } else {
                            TEXT_DIM
                        }));
                    });
                    widgets::checkbox_box(ui, assets, box_rect, enabled, row.hovered());
                    let row = row.on_hover_ui(|ui| recipe_tooltip(ui, game, r));
                    if row.clicked() {
                        if enabled {
                            planner.enabled.remove(&r.class);
                        } else {
                            planner.enabled.insert(r.class.clone());
                        }
                        changed = true;
                    }
                }
            });
    });
    changed
}

fn limits(
    ui: &mut Ui,
    assets: &mut Assets,
    game: &GameData,
    planner: &mut crate::app::Planner,
) -> bool {
    let mut changed = false;
    let custom = planner
        .limits
        .iter()
        .filter(|(k, v)| game.resource_limits.get(*k) != Some(*v))
        .count();
    let summary = if custom == 0 {
        "Whole map (default)".to_string()
    } else {
        format!("{custom} changed from the whole-map default")
    };
    widgets::collapsible(
        ui,
        assets,
        "limits",
        "Resource limits",
        &summary,
        |ui, assets| {
            widgets::caption(ui, "The most of each raw resource the plan may use per minute. The defaults are every node on the map at 250% clock speed; lower them to what you have tapped. Water is unlimited.");
            for (item, limit) in planner.limits.iter_mut() {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    assets.item(ui, item, 24.0);
                    ui.label(RichText::new(game.item_name(item)).color(TEXT));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        changed |=
                            widgets::num_field(ui, Id::new(("limit", item.as_str())), limit, 80.0);
                    });
                });
            }
            if widgets::button(ui, "Reset to defaults").clicked() {
                planner.limits = game.resource_limits.clone();
                changed = true;
            }
        },
    );
    changed
}

pub fn recipe_tooltip(ui: &mut Ui, game: &GameData, r: &Recipe) {
    widgets::tooltip_title(ui, &r.name);
    let b = game
        .buildings
        .get(&r.building)
        .map(|b| b.name.as_str())
        .unwrap_or(&r.building);
    ui.label(
        RichText::new(format!("{b} · {} seconds per cycle", fmt_num(r.duration_s))).color(TEXT_DIM),
    );
    for (label, list) in [("Ingredients", &r.ingredients), ("Products", &r.products)] {
        ui.label(RichText::new(label).color(TEXT));
        for a in list {
            ui.label(
                RichText::new(format!(
                    "  {} · {}",
                    game.item_name(&a.item),
                    fmt_rate_unit(r.per_min(a), game.item_form(&a.item))
                ))
                .color(TEXT_DIM),
            );
        }
    }
}

pub fn overview(app: &mut FactoryApp, ui: &mut Ui) {
    ui.spacing_mut().item_spacing.y = 0.0;
    widgets::panel_header(ui, "Overview");
    if !app.game_loaded {
        return;
    }
    let game = app.user.game.clone();
    let empty = app.state.graph.nodes.is_empty();
    let mut focus = None;
    let mut extraction_changed = false;
    {
        let FactoryApp { summary: s, user, room, planner, .. } = app;
        let assets = user.assets.as_mut().expect("assets");
        padded(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            if room.active() {
                widgets::section(ui, "In this room", None);
                for p in room.peers.values() {
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(12.0, 12.0), egui::Sense::hover());
                        ui.painter().rect_filled(r, 0.0, crate::room::rgb(p.color));
                        ui.label(RichText::new(&p.name).color(TEXT));
                        let mut tags = Vec::new();
                        if p.id == 0 {
                            tags.push("host");
                        }
                        if p.id == room.me {
                            tags.push("you");
                        }
                        if !tags.is_empty() {
                            ui.label(RichText::new(tags.join(", ")).small().color(TEXT_DIM));
                        }
                    });
                }
                widgets::divider(ui);
            }
            if empty {
                ui.label(
                    RichText::new("Totals appear here once the graph has machines.")
                        .color(TEXT_DIM),
                );
                return;
            }
            ui.horizontal(|ui| {
                assets.ui(ui, UiIcon::Power, 22.0, ACCENT);
                ui.label(
                    RichText::new(format!("{} MW", fmt_num(s.power_mw)))
                        .font(theme::semibold(20.0))
                        .color(TEXT),
                );
            });
            ui.label(RichText::new("Power consumption").small().color(TEXT_DIM));

            if !s.outputs.is_empty() {
                widgets::divider(ui);
                widgets::section(ui, "Products", None);
                for (item, got, want) in &s.outputs {
                    let unit = if game.item_form(item).is_fluid() {
                        "m³/min"
                    } else {
                        "per min"
                    };
                    let text =
                        RichText::new(format!("{} / {} {unit}", fmt_rate(*got), fmt_rate(*want)))
                            .color(supply_color(*got, *want));
                    widgets::info_row(ui, assets, item, game.item_name(item), text);
                }
            }
            if !s.buildings.is_empty() {
                widgets::divider(ui);
                widgets::section(ui, "Buildings", None);
                for (b, (whole, machines)) in &s.buildings {
                    if let Some(bd) = game.buildings.get(b) {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            assets.item(ui, &bd.icon, 24.0);
                            ui.label(RichText::new(&bd.name).color(TEXT));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.label(RichText::new(format!("{whole}")).color(TEXT))
                                    .on_hover_text(format!(
                                        "{} machines' worth of work at 100% clock speed",
                                        fmt_num(*machines)
                                    ));
                            });
                        });
                    }
                }
            }
            let flows = |ui: &mut Ui,
                         assets: &mut Assets,
                         title: &str,
                         m: &std::collections::BTreeMap<String, f64>| {
                if m.is_empty() {
                    return;
                }
                widgets::divider(ui);
                widgets::section(ui, title, None);
                for (item, rate) in m {
                    widgets::info_row(
                        ui,
                        assets,
                        item,
                        game.item_name(item),
                        RichText::new(fmt_rate_unit(*rate, game.item_form(item))).color(TEXT),
                    );
                }
            };
            extraction_changed |=
                crate::overview_extra::extraction(ui, assets, &game, &s.resources, &mut planner.extraction);
            crate::overview_extra::build_cost(ui, assets, &game, &s.buildings, &s.resources, &planner.extraction);
            flows(ui, assets, "Inputs", &s.inputs);
            flows(ui, assets, "Byproducts", &s.sunk);
            if s.sink_points > 0.0 {
                ui.label(
                    RichText::new(format!(
                        "{} AWESOME Sink points per min if sunk",
                        fmt_num(s.sink_points)
                    ))
                    .small()
                    .color(TEXT_DIM),
                );
            }
            if !s.issues.is_empty() {
                widgets::divider(ui);
                ui.horizontal(|ui| {
                    assets.ui(ui, UiIcon::Warning, 16.0, ACCENT);
                    ui.label(
                        RichText::new(format!("Issues ({})", s.issues.len()))
                            .font(theme::semibold(14.0))
                            .color(ACCENT),
                    );
                });
                ui.spacing_mut().item_spacing.y = 0.0;
                for (node, text) in &s.issues {
                    let row = widgets::list_row(ui, 28.0, |ui| {
                        ui.add(
                            egui::Label::new(RichText::new(text).small().color(TEXT)).truncate(),
                        );
                    });
                    if row
                        .on_hover_text(format!("{text}\nClick to show it in the graph."))
                        .clicked()
                    {
                        focus = Some(*node);
                    }
                }
            }
        });
    }
    if extraction_changed {
        app.changed();
    }
    if focus.is_some() {
        app.focus_node = focus;
    }
}
