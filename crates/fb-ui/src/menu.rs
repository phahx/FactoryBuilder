//! Menu bar (File / Edit / View / Help) on the vanilla header colour, with
//! the project name on the right.

use crate::app::FactoryApp;
use crate::app_files::{OpenPurpose, SavePurpose};
use crate::dialogs::{Dialog, Pending};
use crate::theme::*;
use egui::{Align, Button, Layout, RichText};

pub fn menu_bar(app: &mut FactoryApp, ctx: &egui::Context) {
    egui::TopBottomPanel::top("menu")
        .exact_height(30.0)
        .frame(
            egui::Frame::none()
                .fill(HEADER)
                .inner_margin(egui::Margin::symmetric(6.0, 0.0)),
        )
        .show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.style_mut().spacing.button_padding = egui::vec2(10.0, 5.0);
                ui.menu_button("File", |ui| {
                    ui.set_min_width(240.0);
                    if ui.add(Button::new("New").shortcut_text("Ctrl+N")).clicked() {
                        ui.close_menu();
                        app.guard(Pending::New);
                    }
                    if ui.add(Button::new("Open…").shortcut_text("Ctrl+O")).clicked() {
                        ui.close_menu();
                        app.guard(Pending::OpenDialog);
                    }
                    let recent = app.recent();
                    ui.add_enabled_ui(!recent.is_empty(), |ui| {
                        ui.menu_button("Open recent", |ui| {
                            ui.set_min_width(260.0);
                            for path in &recent {
                                let name = crate::app_files::file_stem(path);
                                if ui.button(name).on_hover_text(path).clicked() {
                                    ui.close_menu();
                                    app.guard(Pending::OpenPath(path.clone()));
                                }
                            }
                            ui.separator();
                            if ui.button("Clear list").clicked() {
                                ui.close_menu();
                                app.clear_recent();
                            }
                        });
                    });
                    if ui.button("Open share code…").clicked() {
                        ui.close_menu();
                        app.dialog = Dialog::share_code(false);
                    }
                    ui.separator();
                    if ui.add(Button::new("Save").shortcut_text("Ctrl+S")).clicked() {
                        ui.close_menu();
                        app.save();
                    }
                    if ui.add(Button::new("Save as…").shortcut_text("Ctrl+Shift+S")).clicked() {
                        ui.close_menu();
                        app.pick_save(SavePurpose::Factory);
                    }
                    ui.separator();
                    ui.menu_button("Import", |ui| {
                        ui.set_min_width(200.0);
                        if ui.button("Factory file…").clicked() {
                            ui.close_menu();
                            app.pick_open(OpenPurpose::Import);
                        }
                        if ui.button("Share code…").clicked() {
                            ui.close_menu();
                            app.dialog = Dialog::share_code(true);
                        }
                    });
                    let has_nodes = !app.state.graph.nodes.is_empty();
                    ui.add_enabled_ui(has_nodes, |ui| {
                        ui.menu_button("Export", |ui| {
                            ui.set_min_width(220.0);
                            if ui.button("Image (PNG)…").clicked() {
                                ui.close_menu();
                                app.pick_save(SavePurpose::Png);
                            }
                            if ui.button("Factory JSON…").clicked() {
                                ui.close_menu();
                                app.pick_save(SavePurpose::Json);
                            }
                            ui.separator();
                            if ui.button("Copy text summary").clicked() {
                                ui.close_menu();
                                let text = app.summary_text();
                                ui.ctx().copy_text(text);
                                app.say(ui.ctx(), "Summary copied.", false);
                            }
                            if ui.button("Save text summary…").clicked() {
                                ui.close_menu();
                                app.pick_save(SavePurpose::Text);
                            }
                        });
                    });
                    if ui.add_enabled(has_nodes, Button::new("Copy share code")).clicked() {
                        ui.close_menu();
                        app.copy_share_code(ui.ctx());
                    }
                    ui.separator();
                    if ui.button("Settings…").clicked() {
                        ui.close_menu();
                        let s = app.status.as_ref().map(|s| s.settings.clone()).unwrap_or_default();
                        app.dialog = Dialog::settings(&s);
                    }
                });
                ui.menu_button("Edit", |ui| {
                    ui.set_min_width(220.0);
                    if ui
                        .add_enabled(app.can_undo(), Button::new("Undo").shortcut_text("Ctrl+Z"))
                        .clicked()
                    {
                        ui.close_menu();
                        app.undo();
                    }
                    if ui
                        .add_enabled(app.can_redo(), Button::new("Redo").shortcut_text("Ctrl+Y"))
                        .clicked()
                    {
                        ui.close_menu();
                        app.redo();
                    }
                    ui.separator();
                    let sel = app.has_selection();
                    if ui
                        .add_enabled(sel, Button::new("Duplicate").shortcut_text("Ctrl+D"))
                        .clicked()
                    {
                        ui.close_menu();
                        app.duplicate_selected();
                    }
                    if ui
                        .add_enabled(sel, Button::new("Delete").shortcut_text("Del"))
                        .clicked()
                    {
                        ui.close_menu();
                        app.delete_selected();
                    }
                    if ui
                        .add(Button::new("Select all").shortcut_text("Ctrl+A"))
                        .clicked()
                    {
                        ui.close_menu();
                        app.select_all();
                    }
                });
                ui.menu_button("View", |ui| {
                    ui.set_min_width(220.0);
                    if ui
                        .add(Button::new("Frame all").shortcut_text("Home"))
                        .clicked()
                    {
                        ui.close_menu();
                        app.request_frame_all();
                    }
                    if ui.button("Arrange nodes").clicked() {
                        ui.close_menu();
                        app.request_arrange();
                    }
                });
                ui.menu_button("Room", |ui| {
                    ui.set_min_width(260.0);
                    let off = app.room.mode == crate::room::Mode::Off;
                    let hosting = matches!(app.room.mode, crate::room::Mode::Hosting(_));
                    if ui.add_enabled(off, Button::new("Open a room…")).clicked() {
                        ui.close_menu();
                        app.dialog = Dialog::OpenRoom { name: app.user_name(), error: None };
                    }
                    if ui.add_enabled(off, Button::new("Join a room…")).clicked() {
                        ui.close_menu();
                        app.dialog = Dialog::JoinRoom { name: app.user_name(), code: String::new(), error: None, busy: false };
                    }
                    ui.separator();
                    if ui.add_enabled(hosting, Button::new("Room code…")).clicked() {
                        ui.close_menu();
                        app.dialog = Dialog::RoomInfo;
                    }
                    let leave = if hosting { "Close room" } else { "Leave room" };
                    if ui.add_enabled(app.room.active(), Button::new(leave)).clicked() {
                        ui.close_menu();
                        app.leave_room(ui.ctx());
                    }
                });
                ui.menu_button("Help", |ui| {
                    ui.set_min_width(220.0);
                    if ui.button("Keyboard and mouse…").clicked() {
                        ui.close_menu();
                        app.dialog = Dialog::Shortcuts;
                    }
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new(app.title()).color(TEXT_DIM));
                });
            });
        });
}
