//! Modal dialogs in the vanilla window layout: header bar (#3F3F3F), content,
//! bottom bar with the buttons on the right (cancel first, action last).

use crate::app::FactoryApp;
use crate::assets::UiIcon;
use crate::backend::Msg;
use crate::theme::{self, *};
use crate::widgets;
use egui::{vec2, Align, Align2, Layout, RichText};
use fb_model::Settings;
use serde_json::json;

/// What to do once unsaved changes are dealt with.
#[derive(Debug, Clone, PartialEq)]
pub enum Pending {
    New,
    /// Show the native Open dialog.
    OpenDialog,
    /// Open this file (Open Recent).
    OpenPath(String),
    /// Open a factory from a share code.
    OpenFactory(Box<crate::portable::FactoryFile>),
    /// Open a project from a share code.
    OpenProject(Box<crate::portable::ProjectFile>),
}

/// What a pasted share code is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareMode {
    /// Replace what's open.
    Open,
    /// Add its nodes to the open factory.
    Import,
    /// Add it to the project as a factory of its own.
    AddFactory,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Dialog {
    None,
    Unsaved { then: Pending },
    /// Paste a share code to open it (or import it).
    ShareCode { code: String, error: Option<String>, mode: ShareMode },
    /// Confirm removing factories from the project.
    DeleteFactories { ids: Vec<u32> },
    RenameFactory { id: u32, name: String },
    Settings { draft: Settings },
    Shortcuts,
    OpenRoom { name: String, error: Option<String> },
    JoinRoom { name: String, code: String, error: Option<String>, busy: bool },
    /// Host: the room code and how to let people in from outside.
    RoomInfo,
    /// Pick a save to use the recipes it has unlocked.
    Saves(crate::progress_ui::SavesDialog),
    /// Make a project of every factory in the save on the Map tab.
    WorldProject(WorldProject),
}

/// The Make project from save dialog: how close machines stand to be one
/// factory, and what that makes (worked out again when it changes).
#[derive(Debug, Clone, PartialEq)]
pub struct WorldProject {
    pub gap_m: f64,
    pub preview: Option<(f64, crate::map_import::WorldReport)>,
}

impl Dialog {
    pub fn settings(s: &Settings) -> Self {
        Dialog::Settings { draft: s.clone() }
    }
}

/// Draws the modal frame; `body` fills the content area and returns the
/// buttons to show (label, is_enabled) in order; returns the index clicked.
pub(crate) fn modal(
    ctx: &egui::Context,
    title: &str,
    width: f32,
    body: impl FnOnce(&mut egui::Ui),
    buttons: &[(&str, bool)],
) -> Option<usize> {
    // Enter takes the action (the last button), as dialogs do; a field
    // that takes Enter itself (it had focus coming in) keeps it.
    let enter = ctx.memory(|m| m.focused().is_none()) && ctx.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.is_none());
    // Dim the app and swallow clicks behind the dialog.
    let screen = ctx.screen_rect();
    egui::Area::new(egui::Id::new("modal_backdrop"))
        .order(egui::Order::Middle)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            let (rect, _) = ui.allocate_exact_size(screen.size(), egui::Sense::click_and_drag());
            ui.painter()
                .rect_filled(rect, 0.0, egui::Color32::from_black_alpha(150));
        });
    let mut clicked = None;
    egui::Area::new(egui::Id::new(("modal", title)))
        .order(egui::Order::Foreground)
        .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
        .show(ctx, |ui| {
            egui::Frame::none()
                .fill(CONTENT)
                .stroke(ui.visuals().window_stroke)
                .shadow(ui.visuals().window_shadow)
                .show(ui, |ui| {
                    ui.set_width(width);
                    ui.spacing_mut().item_spacing.y = 0.0;
                    widgets::panel_header(ui, title);
                    egui::Frame::none()
                        .inner_margin(egui::Margin::same(16.0))
                        .show(ui, |ui| {
                            // Keep content (and its text wrapping) inside the dialog.
                            ui.set_max_width(width - 32.0);
                            ui.spacing_mut().item_spacing.y = 8.0;
                            body(ui);
                        });
                    let (bar, _) = ui.allocate_exact_size(vec2(width, 52.0), egui::Sense::hover());
                    ui.painter().rect_filled(bar, 0.0, HEADER);
                    let mut child = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(bar.shrink2(vec2(12.0, 0.0)))
                            .layout(Layout::right_to_left(Align::Center)),
                    );
                    // Right-to-left: the last button (the action) sits at the far right.
                    for (i, (label, enabled)) in buttons.iter().enumerate().rev() {
                        if widgets::button_enabled(&mut child, label, *enabled).clicked() {
                            clicked = Some(i);
                        }
                    }
                });
        });
    if clicked.is_none() && enter {
        clicked = buttons.iter().rposition(|_| true).filter(|i| buttons[*i].1);
    }
    clicked
}

pub fn show(app: &mut FactoryApp, ctx: &egui::Context) {
    let dialog = app.dialog.clone();
    match dialog {
        Dialog::None => {}
        Dialog::Unsaved { then } => {
            let name = app.title().trim_end_matches('*').to_string();
            let clicked = modal(
                ctx,
                "Unsaved changes",
                440.0,
                |ui| {
                    ui.label(RichText::new(format!("Save changes to “{name}” first?")).color(TEXT));
                    ui.label(
                        RichText::new("If you don't save, your changes will be lost.")
                            .color(TEXT_DIM),
                    );
                },
                &[("Cancel", true), ("Don't save", true), ("Save", true)],
            );
            match clicked {
                Some(0) => app.dialog = Dialog::None,
                Some(1) => {
                    app.dialog = Dialog::None;
                    app.run_pending(then);
                }
                Some(2) => {
                    app.dialog = Dialog::None;
                    app.after_save = Some(then);
                    app.save();
                }
                _ => {}
            }
        }
        Dialog::ShareCode { mut code, error, mode } => {
            let mut submit = false;
            let (title, action) = match mode {
                ShareMode::Open => ("Open share code", "Open"),
                ShareMode::Import => ("Import share code", "Import"),
                ShareMode::AddFactory => ("Add factory from a share code", "Add"),
            };
            let has_code = !code.trim().is_empty();
            let clicked = modal(
                ctx,
                title,
                520.0,
                |ui| {
                    ui.label(RichText::new("Share code").color(TEXT_DIM));
                    let r = ui.add(
                        egui::TextEdit::multiline(&mut code)
                            .hint_text(if mode == ShareMode::Open { "FB1-… or FBP1-…" } else { "FB1-…" })
                            .font(egui::TextStyle::Monospace)
                            .desired_rows(5)
                            .desired_width(f32::INFINITY),
                    );
                    if code.is_empty() {
                        r.request_focus();
                    }
                    submit = r.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.command);
                    widgets::caption(
                        ui,
                        match mode {
                            ShareMode::Import => "The shared factory is added beside yours and selected.",
                            ShareMode::AddFactory => "The shared factory is added to this project as a factory of its own.",
                            ShareMode::Open => "The shared factory or project replaces what you have open (you'll be asked to save first if there are changes).",
                        },
                    );
                    if let Some(e) = &error {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(e).color(DANGER));
                        });
                    }
                },
                &[("Cancel", true), (action, has_code)],
            );
            match clicked {
                Some(0) => {
                    app.dialog = Dialog::None;
                    app.block_spot = None;
                    return;
                }
                Some(1) => submit = true,
                _ => {}
            }
            app.dialog = Dialog::ShareCode { code: code.clone(), error: error.clone(), mode };
            if submit && !code.trim().is_empty() {
                use crate::portable::Shared;
                match (crate::portable::Shared::from_share_code(&code), mode) {
                    (Ok(Shared::Factory(f)), ShareMode::Open) => {
                        app.dialog = Dialog::None;
                        app.guard(Pending::open_factory(f));
                    }
                    (Ok(Shared::Project(p)), ShareMode::Open) => {
                        app.dialog = Dialog::None;
                        app.guard(Pending::OpenProject(Box::new(p)));
                    }
                    (Ok(Shared::Factory(f)), ShareMode::Import) => {
                        app.dialog = Dialog::None;
                        app.import_factory(ctx, f);
                    }
                    (Ok(Shared::Factory(f)), ShareMode::AddFactory) => {
                        app.dialog = Dialog::None;
                        app.add_factory_file(ctx, &f);
                    }
                    (Ok(Shared::Project(_)), _) => {
                        let e = "That's the share code of a whole project. Open it with File › Open share code.".to_string();
                        app.dialog = Dialog::ShareCode { code, error: Some(e), mode };
                    }
                    (Err(e), _) => app.dialog = Dialog::ShareCode { code, error: Some(e), mode },
                }
            }
        }
        Dialog::DeleteFactories { ids } => {
            let names: Vec<String> = ids.iter().map(|id| format!("“{}”", app.doc_name(*id))).collect();
            let what = if names.len() == 1 { names[0].clone() } else { format!("{} factories", names.len()) };
            let clicked = modal(
                ctx,
                "Delete factory",
                440.0,
                |ui| {
                    ui.label(RichText::new(format!("Delete {what} from the project?")).color(TEXT));
                    if names.len() > 1 {
                        ui.label(RichText::new(names.join(", ")).color(TEXT_DIM));
                    }
                    ui.label(RichText::new("Its graph, plan and links go with it. This can't be undone.").color(TEXT_DIM));
                },
                &[("Cancel", true), ("Delete", true)],
            );
            match clicked {
                Some(0) => app.dialog = Dialog::None,
                Some(1) => {
                    app.dialog = Dialog::None;
                    app.delete_factories(&ids);
                }
                _ => {}
            }
        }
        Dialog::WorldProject(mut d) => {
            let Some(world) = app.map.world.clone() else {
                app.dialog = Dialog::None;
                return;
            };
            let game = app.user.game.clone();
            let map = app.map.info.clone();
            let make = |gap_m: f64| crate::map_import::world_project(&world, &game, map.as_ref(), (gap_m * 100.0) as f32);
            if d.preview.as_ref().is_none_or(|p| p.0 != d.gap_m) {
                d.preview = Some((d.gap_m, make(d.gap_m).1));
            }
            let r = d.preview.clone().map(|p| p.1).unwrap_or_default();
            let clicked = modal(
                ctx,
                "Make a project from this save",
                520.0,
                |ui| {
                    ui.label(RichText::new("Every group of machines becomes a factory, like Import into planner makes it. Belts and pipes running from one group to another become links on the master graph. Trains, trucks and drones aren't followed.").color(TEXT_DIM));
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Machines within").color(TEXT));
                        let mut v = d.gap_m;
                        if widgets::num_field(ui, egui::Id::new("world_gap"), &mut v, 64.0) {
                            d.gap_m = v.clamp(5.0, 2000.0);
                        }
                        ui.label(RichText::new("m of each other are one factory").color(TEXT));
                    });
                    ui.add_space(12.0);
                    let text = format!(
                        "{} factories · {} links between them · {} power circuits · {} machines",
                        r.factories, r.links, r.circuits, r.machines
                    );
                    ui.label(RichText::new(text).color(TEXT));
                    if r.left_out > 0 {
                        ui.label(RichText::new(format!("{} machines left out: ones with no recipe set, generators with no fuel in them, and geothermal generators.", r.left_out)).small().color(TEXT_DIM));
                    }
                    ui.label(RichText::new("It opens as a new project; the one open now is closed (you're asked to save it first).").small().color(TEXT_DIM));
                },
                &[("Cancel", true), ("Make project", r.factories > 0)],
            );
            match clicked {
                Some(0) => app.dialog = Dialog::None,
                Some(1) => {
                    app.dialog = Dialog::None;
                    let (p, _) = make(d.gap_m);
                    app.guard(Pending::OpenProject(Box::new(p)));
                }
                _ => app.dialog = Dialog::WorldProject(d),
            }
        }
        Dialog::RenameFactory { id, mut name } => {
            let mut submit = false;
            let named = !name.trim().is_empty();
            let clicked = modal(
                ctx,
                "Rename factory",
                400.0,
                |ui| {
                    ui.label(RichText::new("Name").color(TEXT_DIM));
                    let r = ui.add(egui::TextEdit::singleline(&mut name).desired_width(f32::INFINITY));
                    if !r.has_focus() && !r.lost_focus() {
                        r.request_focus();
                    }
                    submit = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                },
                &[("Cancel", true), ("Rename", named)],
            );
            match clicked {
                Some(0) => app.dialog = Dialog::None,
                Some(1) => submit = true,
                _ => app.dialog = Dialog::RenameFactory { id, name: name.clone() },
            }
            if submit && !name.trim().is_empty() {
                app.dialog = Dialog::None;
                app.rename_factory(id, &name);
            }
        }
        Dialog::Settings { mut draft } => {
            let status = app.status.clone().unwrap_or_default();
            let game_ok = app.game_loaded && app.game_error.is_none();
            let recipes = app.user.game.recipes.len();
            let game_error = app.game_error.clone();
            let assets = app.user.assets.as_mut().expect("assets");
            let mut browse = false;
            let mut extract = false;
            let mut dev_action: Option<&'static str> = None;
            let clicked = modal(
                ctx,
                "Settings",
                620.0,
                |ui| {
                    ui.label(RichText::new("Satisfactory folder").color(TEXT_DIM));
                    browse = widgets::field_with_button(ui, &mut draft.game_dir, true, "Browse…", true).1;
                    status_line(
                        ui,
                        assets,
                        game_ok,
                        &if game_ok {
                            format!("Game data loaded: {recipes} recipes.")
                        } else {
                            game_error
                                .clone()
                                .unwrap_or_else(|| "Game data not loaded.".into())
                        },
                    );
                    widgets::divider(ui);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Icons, font and map").color(TEXT_DIM));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            extract = widgets::button_enabled(ui, "Extract again", status.setup.helper_found).clicked();
                        });
                    });
                    let icons_ok = status.icons_found > 0;
                    status_line(
                        ui,
                        assets,
                        icons_ok,
                        &format!("{} item icons found.", status.icons_found),
                    );
                    let ui_ok = status.ui_assets_found > 0;
                    status_line(
                        ui,
                        assets,
                        ui_ok,
                        if ui_ok {
                            "Game font and interface icons found."
                        } else {
                            "Game font and interface icons not found."
                        },
                    );
                    status_line(
                        ui,
                        assets,
                        status.map_found,
                        if status.map_found { "World map found." } else { "World map not extracted yet." },
                    );
                    if status.game_changed {
                        status_line(ui, assets, false, "Satisfactory has updated since they were extracted. Extract again to match.");
                    }
                    if !status.setup.helper_found {
                        status_line(ui, assets, false, "The icon helper is missing (tools\\IconExtractor.exe next to Factory Builder). Reinstall Factory Builder to get it back.");
                    }
                    widgets::caption(ui, "They're read from your own Satisfactory files and stay on this PC.");
                    widgets::divider(ui);
                    ui.label(RichText::new("Your name in rooms").color(TEXT_DIM));
                    ui.add(egui::TextEdit::singleline(&mut draft.user_name).desired_width(f32::INFINITY).char_limit(24).margin(vec2(8.0, 6.0)));
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Port for rooms you open (TCP)").color(TEXT_DIM));
                        let mut v = draft.room_port as f64;
                        if widgets::num_field(ui, egui::Id::new("settings_room_port"), &mut v, 80.0) {
                            draft.room_port = v.clamp(1024.0, 65535.0) as u16;
                        }
                    });
                    crate::live::settings_section(ui, assets, &mut draft);
                    if status.setup.dev {
                        widgets::divider(ui);
                        ui.label(RichText::new("Developer").font(theme::semibold(14.0)).color(ACCENT));
                        widgets::caption(ui, &match &status.setup.test_profile {
                            Some(p) => format!("Development build, running in the test profile at {p}. It installs as “{}”.", status.setup.product),
                            None => "Development build. These are for testing setup; your real settings and install aren't touched.".to_string(),
                        });
                        ui.horizontal_wrapped(|ui| {
                            if widgets::button(ui, "Test first launch").on_hover_text("Opens this build in a new window with a fresh test profile, as on a PC that has never run Factory Builder. Its install is “Factory Builder (Test)”, with its own folder, shortcuts and Apps & features entry. Any earlier test install is removed first.").clicked() {
                                dev_action = Some("first_launch");
                            }
                            if widgets::button(ui, "Run setup here").on_hover_text("Opens setup in this window and profile.").clicked() {
                                dev_action = Some("setup_here");
                            }
                            if widgets::button_enabled(ui, "Remove test install", status.setup.test_profile.is_none()).on_hover_text("Uninstalls “Factory Builder (Test)” and deletes the test profile.").clicked() {
                                dev_action = Some("remove_test");
                            }
                        });
                    }
                },
                &[("Cancel", true), ("Apply", true)],
            );
            if browse {
                let start = draft.game_dir.clone();
                app.dialog = Dialog::Settings { draft: draft.clone() };
                app.pick_folder(crate::setup::FolderPurpose::SettingsGameDir, "Your Satisfactory folder", &start);
                return;
            }
            if extract {
                app.open_setup(crate::setup::Wizard::icons_only(&status));
                return;
            }
            match dev_action {
                Some("first_launch") => {
                    app.backend.call("dev_test_first_launch", json!({}), |r: Result<(), String>| {
                        Msg::DevDone(r.map(|_| "Started a test first launch in a new window.".to_string()))
                    });
                    return;
                }
                Some("setup_here") => {
                    app.open_setup(crate::setup::Wizard::first_launch(&status));
                    return;
                }
                Some("remove_test") => {
                    app.backend.call("dev_remove_test_install", json!({}), Msg::DevDone);
                    return;
                }
                _ => {}
            }
            match clicked {
                Some(0) => app.dialog = Dialog::None,
                Some(1) => {
                    app.backend
                        .call("set_settings", json!({ "settings": draft }), Msg::Status);
                    app.reload_after_settings(ctx);
                    app.dialog = Dialog::None;
                }
                _ => app.dialog = Dialog::Settings { draft },
            }
        }
        Dialog::OpenRoom { mut name, error } => {
            let port = app.room_port();
            let busy = app.room.mode == crate::room::Mode::Joining;
            let mut submit = false;
            let valid = !name.trim().is_empty();
            let clicked = modal(
                ctx,
                "Open a room",
                480.0,
                |ui| {
                    ui.label(RichText::new("Your name").color(TEXT_DIM));
                    let r = ui.add(egui::TextEdit::singleline(&mut name).desired_width(f32::INFINITY).char_limit(24).margin(vec2(8.0, 6.0)));
                    r.request_focus();
                    submit = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    widgets::caption(ui, &format!("Others plan with you live: everyone sees the same factory and each other's cursors. They connect to this PC on TCP port {port}. Factory Builder asks your router to forward that port and finds your public address, so one code works for everyone; the next screen shows what worked."));
                    if let Some(e) = &error {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(e).color(DANGER));
                        });
                    }
                },
                &[("Cancel", true), (if busy { "Opening…" } else { "Open room" }, valid && !busy)],
            );
            match clicked {
                Some(0) => app.dialog = Dialog::None,
                Some(1) => submit = true,
                _ => app.dialog = Dialog::OpenRoom { name: name.clone(), error: error.clone() },
            }
            if submit && valid && !busy {
                app.dialog = Dialog::OpenRoom { name: name.clone(), error: None };
                app.open_room(name.trim().to_string());
            }
        }
        Dialog::JoinRoom { mut name, mut code, error, busy } => {
            let parsed = fb_model::room::decode_code(&code);
            let valid = parsed.is_some() && !name.trim().is_empty();
            let replaces = !app.state.graph.nodes.is_empty();
            let mut submit = false;
            let clicked = modal(
                ctx,
                "Join a room",
                480.0,
                |ui| {
                    ui.label(RichText::new("Your name").color(TEXT_DIM));
                    ui.add(egui::TextEdit::singleline(&mut name).desired_width(f32::INFINITY).char_limit(24).margin(vec2(8.0, 6.0)));
                    ui.label(RichText::new("Room code").color(TEXT_DIM));
                    let r = ui.add(egui::TextEdit::singleline(&mut code).hint_text("XXXX-XXXX-XXXX-XXXX").desired_width(f32::INFINITY).char_limit(24).margin(vec2(8.0, 6.0)));
                    if code.is_empty() {
                        r.request_focus();
                    }
                    submit = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if !code.trim().is_empty() && parsed.is_none() {
                        ui.label(RichText::new("That doesn't look like a room code. It has 16 letters and digits, like 60N0-0HFE-QM6B-01RG.").small().color(ACCENT));
                    }
                    widgets::caption(ui, "Ask the host for their room code.");
                    if replaces {
                        widgets::caption(ui, "Joining shows the host's factory instead of yours. Save yours first if you want to keep it.");
                    }
                    if let Some(e) = &error {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(e).color(DANGER));
                        });
                    }
                },
                &[("Cancel", true), (if busy { "Joining…" } else { "Join" }, valid && !busy)],
            );
            match clicked {
                Some(0) => {
                    if busy {
                        app.leave_room(ctx);
                    }
                    app.dialog = Dialog::None;
                    return;
                }
                Some(1) => submit = true,
                _ => {}
            }
            app.dialog = Dialog::JoinRoom { name: name.clone(), code: code.clone(), error: error.clone(), busy };
            if submit && valid && !busy {
                app.dialog = Dialog::JoinRoom { name: name.clone(), code: code.clone(), error: None, busy: true };
                app.join_room(name.trim().to_string(), code.trim().to_string());
            }
        }
        Dialog::RoomInfo => {
            let crate::room::Mode::Hosting(info) = app.room.mode.clone() else {
                app.dialog = Dialog::None;
                return;
            };
            let assets = app.user.assets.as_mut().expect("assets");
            let mut copy = None;
            let clicked = modal(
                ctx,
                "Room open",
                560.0,
                |ui| {
                    ui.label(RichText::new("Room code").color(TEXT_DIM));
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&info.code).font(theme::semibold(24.0)).color(ACCENT));
                        if widgets::icon_button(ui, assets, UiIcon::Copy, "Copy room code").clicked() {
                            copy = Some(info.code.clone());
                        }
                    });
                    widgets::caption(ui, "One code for everyone: people on your network and people over the internet use the same one.");
                    widgets::divider(ui);
                    status_line(ui, assets, true, "Works for people on your network.");
                    match (&info.public_ip, info.forwarded) {
                        (Some(_), true) => {
                            status_line(ui, assets, true, &format!("Works over the internet: your router forwarded TCP port {} to this PC while the room is open.", info.port));
                            if info.shared_address {
                                status_line(ui, assets, false, "Your router's own internet address is shared by your provider, so people outside your network may still not get in.");
                            }
                        }
                        (Some(_), false) => {
                            let why = info.forward_problem.clone().unwrap_or_else(|| "it didn't say why".into());
                            status_line(ui, assets, false, &format!("For people outside your network: on your router, forward TCP port {} to this PC ({}). It couldn't be done automatically: {why}.", info.port, info.local_ip));
                        }
                        (None, _) => {
                            status_line(ui, assets, false, "Your public address couldn't be found (no answer from your router or the internet), so this code only works on your network.");
                        }
                    }
                    status_line(ui, assets, true, "If Windows asks, allow Factory Builder through the firewall.");
                    if let (Some(ip), Some(src)) = (&info.public_ip, &info.public_source) {
                        let from = if src == "router" { "your router".to_string() } else { src.clone() };
                        widgets::caption(ui, &format!("Public address {ip}, from {from}. The code contains your addresses, so share it only with people you want in the room. The room stays open until you close it from the Room menu or quit."));
                    } else {
                        widgets::caption(ui, "The code contains your address, so share it only with people you want in the room. The room stays open until you close it from the Room menu or quit.");
                    }
                },
                &[("Close", true)],
            );
            if let Some(c) = copy {
                ctx.copy_text(c);
                app.say(ctx, "Room code copied.", false);
            }
            if clicked.is_some() {
                app.dialog = Dialog::None;
            }
        }
        Dialog::Saves(d) => crate::progress_ui::show(app, ctx, d),
        Dialog::Shortcuts => {
            let rows = [
                ("New project (it starts with the HUB)", "Ctrl+N"),
                ("Open", "Ctrl+O"),
                ("Save / Save as", "Ctrl+S / Ctrl+Shift+S"),
                ("Undo / Redo", "Ctrl+Z / Ctrl+Y"),
                ("Add a node (a factory, on the master graph)", "Right-click the canvas"),
                ("Node menu", "Right-click a node"),
                ("Connect", "Drag from a port to a matching port"),
                ("Select several", "Drag on empty canvas"),
                ("Select all", "Ctrl+A"),
                ("Duplicate / Delete", "Ctrl+D / Del"),
                ("Rename a node or frame", "F2"),
                ("Open the selected factory (master graph)", "Enter, or double-click"),
                ("A dialog's action / Cancel", "Enter / Esc"),
                ("Comment frame around selection", "C"),
                ("Find", "Ctrl+F"),
                ("Pan", "Middle-drag, or Ctrl+drag"),
                ("Zoom", "Mouse wheel"),
                ("Frame all", "Home"),
                ("Ping a spot (in a room)", "Alt+click empty canvas"),
                ("Message the room", "/"),
                ("Follow someone's view", "Click them under In this room"),
                ("Stop following", "Esc, or pan or click yourself"),
            ];
            let clicked = modal(
                ctx,
                "Keyboard and mouse",
                460.0,
                |ui| {
                    egui::Grid::new("shortcuts")
                        .num_columns(2)
                        .spacing(vec2(24.0, 8.0))
                        .show(ui, |ui| {
                            for (what, keys) in rows {
                                ui.label(RichText::new(what).color(TEXT));
                                ui.label(RichText::new(keys).color(TEXT_DIM));
                                ui.end_row();
                            }
                        });
                },
                &[("Close", true)],
            );
            if clicked.is_some() {
                app.dialog = Dialog::None;
            }
        }
    }
}

pub(crate) fn status_line(ui: &mut egui::Ui, assets: &mut crate::assets::Assets, ok: bool, text: &str) {
    ui.horizontal_top(|ui| {
        if ok {
            assets.ui(ui, UiIcon::Check, 14.0, ACCENT);
        } else {
            assets.ui(ui, UiIcon::Warning, 14.0, DANGER);
        }
        ui.add(egui::Label::new(RichText::new(text).small().color(if ok { TEXT_DIM } else { TEXT })).wrap());
    });
    let _ = theme::semibold;
}
