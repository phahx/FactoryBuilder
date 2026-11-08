//! First-launch setup, in the same window style as the other dialogs:
//!
//! 1. Install (only when this isn't the installed copy): folder, shortcuts,
//!    then restart from the installed copy, which carries on at step 2.
//! 2. Game folder: found automatically, checked by reading its recipes.
//! 3. Icons and font: the bundled helper extracts them, with progress.
//! 4. Your name (for rooms; optional).
//!
//! The icons step also runs on its own: Settings › Extract again, and when
//! the game has updated since the icons were extracted.

use crate::app::FactoryApp;
use crate::backend::Msg;
use crate::dialogs::{modal, status_line};
use crate::theme::{self, *};
use crate::widgets;
use egui::{vec2, RichText};
use fb_model::setup::{ExtractProgress, GameCheck, InstallReport};
use fb_model::Status;
use serde_json::json;

pub const TITLE: &str = "Factory Builder setup";
const WIDTH: f32 = 600.0;
/// How often to ask the backend how extraction is going.
const POLL_SECONDS: f64 = 0.15;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Install,
    Game,
    Icons,
    Name,
}

impl Step {
    fn label(self) -> &'static str {
        match self {
            Step::Install => "Install",
            Step::Game => "Game folder",
            Step::Icons => "Icons and map",
            Step::Name => "Your name",
        }
    }
}

/// Which folder a native folder picker was opened for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderPurpose {
    InstallDir,
    GameDir,
    SettingsGameDir,
}

#[derive(Debug, Clone, Default)]
pub struct Wizard {
    pub steps: Vec<Step>,
    pub at: usize,
    // Install
    pub install_dir: String,
    pub desktop: bool,
    pub installing: bool,
    pub installed: Option<InstallReport>,
    pub install_error: Option<String>,
    pub restarting: bool,
    // Game folder
    pub game_dir: String,
    pub found: Vec<String>,
    pub checking: Option<String>,
    pub check: Option<(String, Result<GameCheck, String>)>,
    // Icons
    pub extract: Option<ExtractProgress>,
    pub extract_error: Option<String>,
    pub next_poll: f64,
    // Name
    pub name: String,
    /// Just the icons step (Settings › Extract again, or after a game update).
    pub icons_only: bool,
}

impl Wizard {
    pub fn first_launch(status: &Status) -> Self {
        let mut steps = vec![Step::Game, Step::Icons, Step::Name];
        if !status.setup.installed_copy {
            steps.insert(0, Step::Install);
        }
        let install_dir = status.setup.installed.as_ref().map(|c| c.dir.clone()).unwrap_or_else(|| status.setup.default_dir.clone());
        Self {
            steps,
            install_dir,
            desktop: true,
            game_dir: status.settings.game_dir.clone(),
            name: status.settings.user_name.clone(),
            ..Default::default()
        }
    }

    pub fn icons_only(status: &Status) -> Self {
        Self { steps: vec![Step::Icons], game_dir: status.settings.game_dir.clone(), icons_only: true, ..Default::default() }
    }

    pub fn step(&self) -> Step {
        self.steps[self.at.min(self.steps.len() - 1)]
    }

    fn game_ok(&self) -> Option<&GameCheck> {
        match &self.check {
            Some((dir, Ok(c))) if *dir == self.game_dir && self.checking.is_none() => Some(c),
            _ => None,
        }
    }

    fn extract_done(&self) -> bool {
        self.extract.as_ref().is_some_and(|p| p.finished && p.error.is_none())
    }

    fn extract_running(&self) -> bool {
        self.extract_error.is_none() && self.extract.as_ref().is_none_or(|p| !p.finished)
    }
}

impl FactoryApp {
    /// Opens setup at its first step.
    pub(crate) fn open_setup(&mut self, wizard: Wizard) {
        self.dialog = crate::dialogs::Dialog::None;
        self.wizard = Some(wizard);
        self.enter_step();
    }

    /// Kicks off whatever the current step needs.
    fn enter_step(&mut self) {
        let Some(w) = self.wizard.as_mut() else { return };
        match w.step() {
            Step::Install | Step::Name => {}
            Step::Game => {
                self.backend.call("detect_game_dirs", json!({}), Msg::GameDirs);
                let dir = w.game_dir.clone();
                self.check_game_dir(dir);
            }
            Step::Icons => {
                w.extract = None;
                w.extract_error = None;
                self.backend.call("extract_start", json!({}), Msg::ExtractStarted);
            }
        }
    }

    fn check_game_dir(&mut self, dir: String) {
        let Some(w) = self.wizard.as_mut() else { return };
        if dir.trim().is_empty() {
            w.checking = None;
            w.check = None;
            return;
        }
        w.checking = Some(dir.clone());
        let d = dir.clone();
        self.backend.call("check_game", json!({ "dir": dir }), move |r| Msg::GameChecked { dir: d, result: r });
    }

    fn next_step(&mut self, ctx: &egui::Context) {
        let Some(w) = self.wizard.as_mut() else { return };
        if w.at + 1 < w.steps.len() {
            w.at += 1;
            self.enter_step();
        } else {
            self.finish_setup(ctx);
        }
    }

    fn finish_setup(&mut self, ctx: &egui::Context) {
        let Some(w) = self.wizard.take() else { return };
        if w.icons_only {
            if w.extract_done() {
                self.say(ctx, "Icons and font updated.", false);
            }
        } else {
            let mut s = self.status.as_ref().map(|s| s.settings.clone()).unwrap_or_default();
            s.game_dir = w.game_dir.trim().to_string();
            s.user_name = w.name.trim().to_string();
            s.setup_done = true;
            self.backend.call("set_settings", json!({ "settings": s }), Msg::Status);
            self.say(ctx, "Setup complete.", false);
        }
        self.notice_dismissed = true;
        self.reload_after_settings(ctx);
    }

    /// Saves the chosen game folder so the helper extracts from it.
    fn save_game_dir(&mut self, dir: &str) {
        let mut s = self.status.as_ref().map(|s| s.settings.clone()).unwrap_or_default();
        s.game_dir = dir.trim().to_string();
        self.backend.call("set_settings", json!({ "settings": s }), Msg::Status);
    }

    pub(crate) fn pick_folder(&mut self, purpose: FolderPurpose, title: &str, start: &str) {
        self.backend.call("pick_folder", json!({ "title": title, "start": start }), move |r| Msg::PickedFolder { purpose, path: r });
    }

    /// Setup's replies from the backend.
    pub(crate) fn on_setup_msg(&mut self, ctx: &egui::Context, msg: Msg) {
        match msg {
            Msg::GameDirs(r) => {
                if let (Some(w), Ok(found)) = (self.wizard.as_mut(), r) {
                    if w.game_dir.trim().is_empty() {
                        if let Some(first) = found.first().cloned() {
                            w.game_dir = first.clone();
                            w.found = found;
                            self.check_game_dir(first);
                            return;
                        }
                    }
                    w.found = found;
                }
            }
            Msg::GameChecked { dir, result } => {
                if let Some(w) = self.wizard.as_mut() {
                    if w.checking.as_deref() == Some(dir.as_str()) {
                        w.checking = None;
                    }
                    w.check = Some((dir, result));
                }
            }
            Msg::PickedFolder { purpose, path } => match (purpose, path) {
                (_, Err(e)) => self.say(ctx, e, true),
                (_, Ok(None)) => {}
                (FolderPurpose::InstallDir, Ok(Some(p))) => {
                    self.backend.call("install_dir_for", json!({ "chosen": p }), Msg::InstallDirFor);
                }
                (FolderPurpose::GameDir, Ok(Some(p))) => {
                    if let Some(w) = self.wizard.as_mut() {
                        w.game_dir = p.clone();
                    }
                    self.check_game_dir(p);
                }
                (FolderPurpose::SettingsGameDir, Ok(Some(p))) => {
                    if let crate::dialogs::Dialog::Settings { draft } = &mut self.dialog {
                        draft.game_dir = p;
                    }
                }
            },
            Msg::InstallDirFor(r) => match (self.wizard.as_mut(), r) {
                (Some(w), Ok(dir)) => {
                    w.install_dir = dir;
                    w.install_error = None;
                }
                (_, Err(e)) => self.say(ctx, e, true),
                _ => {}
            },
            Msg::Installed(r) => {
                if let Some(w) = self.wizard.as_mut() {
                    w.installing = false;
                    match r {
                        Ok(report) => w.installed = Some(report),
                        Err(e) => w.install_error = Some(e),
                    }
                }
            }
            Msg::Restarted(r) => {
                if let (Some(w), Err(e)) = (self.wizard.as_mut(), r) {
                    w.restarting = false;
                    w.install_error = Some(e);
                }
            }
            Msg::ExtractStarted(r) => {
                if let Some(w) = self.wizard.as_mut() {
                    match r {
                        Ok(()) => w.next_poll = 0.0,
                        Err(e) => w.extract_error = Some(e),
                    }
                }
            }
            Msg::Extract(r) => {
                let mut done = false;
                if let Some(w) = self.wizard.as_mut() {
                    match r {
                        Ok(p) => {
                            done = p.finished && p.error.is_none();
                            w.extract = Some(p);
                        }
                        Err(e) => w.extract_error = Some(e),
                    }
                }
                if done {
                    // The new icons and font show up straight away.
                    self.backend.call("status", json!({}), Msg::Status);
                    self.assets().reload(ctx);
                }
            }
            Msg::DevDone(r) => match r {
                Ok(text) => self.say(ctx, text, false),
                Err(e) => self.say(ctx, e, true),
            },
            _ => {}
        }
    }

    /// Asks how extraction is going while it runs.
    fn poll_extract(&mut self, ctx: &egui::Context) {
        if self.selftest {
            return; // its setup screens show made-up progress
        }
        let now = ctx.input(|i| i.time);
        let Some(w) = self.wizard.as_mut() else { return };
        if w.step() != Step::Icons || w.extract_error.is_some() || w.extract.as_ref().is_some_and(|p| p.finished) {
            return;
        }
        if now >= w.next_poll {
            w.next_poll = now + POLL_SECONDS;
            self.backend.call("extract_status", json!({}), Msg::Extract);
        }
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(POLL_SECONDS));
    }
}

/// Draws setup if it's open.
pub fn show(app: &mut FactoryApp, ctx: &egui::Context) {
    app.poll_extract(ctx);
    let Some(mut w) = app.wizard.clone() else { return };
    let status = app.status.clone().unwrap_or_default();
    let dev = status.setup.dev;
    let step = w.step();
    let assets = app.user.assets.as_mut().expect("assets");

    let mut pick: Option<(FolderPurpose, &str, String)> = None;
    let mut recheck = false;
    let buttons: Vec<(&str, bool)>;
    let mut actions: Vec<Action> = Vec::new();

    match step {
        Step::Install => {
            let done = w.installed.is_some();
            buttons = if done {
                vec![(if w.restarting { "Restarting…" } else { "Restart and continue" }, !w.restarting)]
            } else if dev {
                vec![("Skip (development build)", !w.installing), (if w.installing { "Installing…" } else { "Install" }, !w.installing)]
            } else {
                vec![(if w.installing { "Installing…" } else { "Install" }, !w.installing)]
            };
            actions = if done { vec![Action::Restart] } else if dev { vec![Action::Skip, Action::Install] } else { vec![Action::Install] };
        }
        Step::Game => {
            let back = w.at > 0 && w.steps[w.at - 1] != Step::Install;
            let mut b = Vec::new();
            if back {
                b.push(("Back", true));
                actions.push(Action::Back);
            }
            b.push(("Next", w.game_ok().is_some()));
            actions.push(Action::NextGame);
            buttons = b;
        }
        Step::Icons => {
            let mut b = Vec::new();
            if !w.icons_only {
                b.push(("Back", !w.extract_running()));
                actions.push(Action::Back);
            }
            let failed = w.extract_error.is_some() || w.extract.as_ref().is_some_and(|p| p.error.is_some());
            if failed {
                b.push(("Try again", true));
                actions.push(Action::Retry);
                b.push((if w.icons_only { "Close" } else { "Skip for now" }, true));
                actions.push(Action::Next);
            } else {
                b.push((if w.icons_only { "Done" } else { "Next" }, w.extract_done()));
                actions.push(Action::Next);
            }
            buttons = b;
        }
        Step::Name => {
            buttons = vec![("Back", true), ("Finish", true)];
            actions = vec![Action::Back, Action::Next];
        }
    }

    let clicked = modal(
        ctx,
        TITLE,
        WIDTH,
        |ui| {
            if w.steps.len() > 1 {
                step_strip(ui, &w);
                ui.add_space(4.0);
            }
            if let Some(p) = &status.setup.test_profile {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(format!("Test profile ({}): installs as “{}”, separate from your real settings.", p, status.setup.product)).small().color(ACCENT));
                });
            }
            match step {
                Step::Install => {
                    heading(ui, "Install Factory Builder");
                    if let Some(report) = &w.installed {
                        for line in &report.done {
                            status_line(ui, assets, true, line);
                        }
                        for line in &report.problems {
                            status_line(ui, assets, false, line);
                        }
                        widgets::caption(ui, "Factory Builder restarts from its install folder and carries on with setup.");
                    } else {
                        widgets::caption(ui, "It installs for your Windows account only, so it doesn't need administrator rights.");
                        if let Some(c) = &status.setup.installed {
                            let note = if fb_model::setup::version_cmp(&c.version, &status.setup.version).is_lt() {
                                format!("Version {} is installed. Installing updates it to {}.", c.version, status.setup.version)
                            } else {
                                format!("Version {} is already installed here. Installing again repairs it.", c.version)
                            };
                            status_line(ui, assets, true, &note);
                        }
                        ui.label(RichText::new("Install folder").color(TEXT_DIM));
                        let mut shown = w.install_dir.clone();
                        if widgets::field_with_button(ui, &mut shown, false, "Change location…", !w.installing).1 {
                            pick = Some((FolderPurpose::InstallDir, "Install Factory Builder in", w.install_dir.clone()));
                        }
                        widgets::checkbox(ui, assets, &mut w.desktop, "Create a desktop shortcut");
                        widgets::caption(ui, "A Start menu shortcut is always added. Factory Builder appears in Apps & features, where you can uninstall it. Double-clicking a .fbfactory file opens it here. Your factories and settings are kept separately and stay if you uninstall.");
                        if let Some(e) = &w.install_error {
                            error_text(ui, e);
                        }
                    }
                }
                Step::Game => {
                    heading(ui, "Find Satisfactory");
                    widgets::caption(ui, "Recipes, items and buildings are read from your own copy of the game, so they always match your version.");
                    ui.label(RichText::new("Satisfactory folder").color(TEXT_DIM));
                    let (r, browse) = widgets::field_with_button(ui, &mut w.game_dir, true, "Browse…", true);
                    if r.lost_focus() && w.check.as_ref().is_none_or(|(d, _)| *d != w.game_dir) {
                        recheck = true;
                    }
                    if browse {
                        pick = Some((FolderPurpose::GameDir, "Your Satisfactory folder", w.game_dir.clone()));
                    }
                    let others: Vec<String> = w.found.iter().filter(|f| !f.eq_ignore_ascii_case(w.game_dir.trim())).cloned().collect();
                    if !others.is_empty() {
                        ui.label(RichText::new("Also found on this PC").small().color(TEXT_DIM));
                        for f in others {
                            if widgets::list_row(ui, 26.0, |ui| {
                                ui.label(RichText::new(&f).small().color(TEXT));
                            })
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .clicked()
                            {
                                w.game_dir = f.clone();
                                recheck = true;
                            }
                        }
                    }
                    if w.checking.is_some() {
                        ui.label(RichText::new("Checking…").small().color(TEXT_DIM));
                    } else if let Some(c) = w.game_ok() {
                        status_line(ui, assets, true, &format!("Satisfactory found: {} recipes, {} items and {} buildings.", c.recipes, c.items, c.buildings));
                    } else if let Some((dir, Err(e))) = &w.check {
                        if *dir == w.game_dir {
                            status_line(ui, assets, false, e);
                        }
                    } else if w.game_dir.trim().is_empty() {
                        status_line(ui, assets, false, "Satisfactory wasn't found automatically. Pick the folder it's installed in (the one with FactoryGame.exe).");
                    }
                }
                Step::Icons => {
                    heading(ui, if w.icons_only { "Update icons, font and map" } else { "Icons, font and map" });
                    widgets::caption(ui, "Factory Builder shows the game's own item icons, font and world map. They're read from your Satisfactory files and stay on this PC.");
                    ui.add_space(4.0);
                    let p = w.extract.clone().unwrap_or_default();
                    if let Some(e) = w.extract_error.clone().or(p.error.clone()) {
                        status_line(ui, assets, false, &e);
                    } else if p.finished {
                        progress_bar(ui, 1.0, "Done");
                        status_line(ui, assets, true, &format!("{} item icons and {} interface files are ready.", p.icons, p.ui));
                    } else {
                        let text = match p.stage.as_str() {
                            "icons" => format!("Item icons… {:.0}%", p.fraction() * 100.0),
                            "ui" => format!("Font and interface icons… {:.0}%", p.fraction() * 100.0),
                            "map" => format!("World map… {:.0}%", p.fraction() * 100.0),
                            "sprites" => format!("Buildings for the map… {:.0}%", p.fraction() * 100.0),
                            "mount" => "Opening the game files…".to_string(),
                            _ => "Starting…".to_string(),
                        };
                        progress_bar(ui, p.fraction(), &text);
                        widgets::caption(ui, "This takes a few seconds.");
                    }
                }
                Step::Name => {
                    heading(ui, "Your name");
                    widgets::caption(ui, "Shown under your cursor when you plan together in a room. You can leave it empty and change it later in Settings.");
                    let r = ui.add(egui::TextEdit::singleline(&mut w.name).hint_text("Pioneer").desired_width(f32::INFINITY).char_limit(24).margin(vec2(8.0, 6.0)));
                    if w.name.is_empty() {
                        r.request_focus();
                    }
                }
            }
        },
        &buttons,
    );

    // Edits made this frame.
    if let Some(cur) = app.wizard.as_mut() {
        cur.install_dir = w.install_dir.clone();
        cur.desktop = w.desktop;
        cur.game_dir = w.game_dir.clone();
        cur.name = w.name.clone();
    }
    if recheck {
        app.check_game_dir(w.game_dir.clone());
    }
    if let Some((purpose, title, start)) = pick {
        app.pick_folder(purpose, title, &start);
    }
    let Some(action) = clicked.and_then(|i| actions.get(i).copied()) else { return };
    match action {
        Action::Back => {
            if let Some(cur) = app.wizard.as_mut() {
                cur.at = cur.at.saturating_sub(1);
            }
            app.enter_step();
        }
        Action::Skip | Action::Next => app.next_step(ctx),
        Action::NextGame => {
            app.save_game_dir(&w.game_dir);
            app.next_step(ctx);
        }
        Action::Retry => app.enter_step(),
        Action::Install => {
            if let Some(cur) = app.wizard.as_mut() {
                cur.installing = true;
                cur.install_error = None;
            }
            app.backend.call("install_run", json!({ "dir": w.install_dir, "desktop": w.desktop }), Msg::Installed);
        }
        Action::Restart => {
            if let Some(cur) = app.wizard.as_mut() {
                cur.restarting = true;
            }
            app.backend.call("install_restart", json!({}), Msg::Restarted);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Action {
    Back,
    Next,
    NextGame,
    Skip,
    Retry,
    Install,
    Restart,
}

fn heading(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).font(theme::semibold(18.0)).color(TEXT));
}

fn error_text(ui: &mut egui::Ui, text: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(text).color(DANGER));
    });
}

/// "1 Install   2 Game folder   3 Icons and font   4 Your name": where you
/// are (orange), what's done (white) and what's next (dim). Not clickable.
fn step_strip(ui: &mut egui::Ui, w: &Wizard) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 18.0;
        for (i, s) in w.steps.iter().enumerate() {
            let color = if i == w.at {
                ACCENT
            } else if i < w.at {
                TEXT
            } else {
                TEXT_DISABLED
            };
            let text = RichText::new(format!("{}  {}", i + 1, s.label())).color(color);
            ui.label(if i == w.at { text.font(theme::semibold(14.0)) } else { text });
        }
    });
}

/// The game's progress bar look: dark track, orange fill, label on top.
fn progress_bar(ui: &mut egui::Ui, fraction: f32, text: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 0.0, INPUT);
    let mut fill = rect;
    fill.set_width(rect.width() * fraction.clamp(0.0, 1.0));
    ui.painter().rect_filled(fill, 0.0, ACCENT);
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(13.0), TEXT);
}
