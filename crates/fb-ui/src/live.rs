//! The Live map add-on (Settings › Add-ons, off by default). While it's
//! off, nothing here runs or shows. On the Map tab:
//! - players: from the save it follows (a save keeps where each player
//!   stood, not their name), or live from the Ficsit Remote Monitoring mod;
//! - a dedicated server: its newest save, downloaded when it changes
//!   (src-tauri live.rs does the talking, with a pinned certificate);
//! - the Live map dialog to set those up.

use crate::app::FactoryApp;
use crate::backend::Msg;
use crate::map_draw::View;
use crate::theme::*;
use crate::widgets;
use egui::{pos2, vec2, Color32, Pos2, RichText, Stroke};
use fb_model::world::PlayerSpot;
use fb_model::{LiveSettings, ServerCheck};
use serde_json::json;

/// Marker radius, screen pixels.
const MARKER: f32 = 7.0;
/// How often to ask the mod where players are (its own map page asks
/// every second).
const PLAYERS_SECONDS: f64 = 1.0;
/// Live players older than this aren't shown (the mod stopped answering).
const PLAYERS_STALE: f64 = 10.0;
/// How often to ask a dedicated server whether it has a newer save.
pub const SERVER_SECONDS: f64 = 30.0;
/// The Map tab's session list entry for the dedicated server.
pub const SERVER_SESSION: &str = "Dedicated server";

#[derive(Default)]
pub struct LiveState {
    /// The last server check, and one in progress.
    pub check: Option<ServerCheck>,
    pub checking: bool,
    pub error: Option<String>,
    /// The token being typed (sent once, then cleared).
    pub token: String,
    /// Live players from the mod, when they arrived, and the last problem.
    pub players: Vec<PlayerSpot>,
    pub players_at: Option<f64>,
    pub players_busy: bool,
    pub players_error: Option<String>,
    next_players: f64,
    /// Following the dedicated server.
    pub server_busy: bool,
    pub next_server: f64,
}

impl FactoryApp {
    /// Whether the Live map add-on is on.
    pub(crate) fn live_on(&self) -> bool {
        self.status.as_ref().is_some_and(|s| s.settings.live.enabled)
    }

    pub(crate) fn live_settings(&self) -> LiveSettings {
        self.status.as_ref().map(|s| s.settings.live.clone()).unwrap_or_default()
    }

    /// Whether a dedicated server is set up to follow.
    pub(crate) fn live_server(&self) -> bool {
        let l = self.live_settings();
        l.enabled && !l.server_host.trim().is_empty()
    }

    /// Players to show on the map: live ones from the mod while it
    /// answers, otherwise where the save left them.
    pub(crate) fn live_players(&self) -> (Vec<PlayerSpot>, bool) {
        if !self.live_on() {
            return (Vec::new(), false);
        }
        let now = self.backend_ctx().input(|i| i.time);
        if self.live_settings().frm && self.live.players_at.is_some_and(|t| now - t < PLAYERS_STALE) {
            return (self.live.players.clone(), true);
        }
        (self.map.world.as_ref().map(|w| w.players.clone()).unwrap_or_default(), false)
    }

    /// Map tab: asks the mod where players are, about once a second.
    pub(crate) fn live_poll(&mut self, ctx: &egui::Context) {
        if !self.live_on() || !self.live_settings().frm {
            return;
        }
        let now = ctx.input(|i| i.time);
        if !self.live.players_busy && now >= self.live.next_players {
            self.live.players_busy = true;
            self.live.next_players = now + PLAYERS_SECONDS;
            self.backend.call("live_players", json!({}), Msg::LivePlayers);
        }
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(PLAYERS_SECONDS));
    }

    /// Map tab, following the dedicated server: asks for its newest save.
    pub(crate) fn live_server_follow(&mut self, now: f64) {
        if self.live.server_busy || now < self.live.next_server {
            return;
        }
        self.live.server_busy = true;
        self.live.next_server = now + SERVER_SECONDS;
        self.backend.call("live_server_sync", json!({}), Msg::LiveServerSave);
    }

    /// Saves the add-on's settings, then (when asked) checks the server.
    fn live_save(&mut self, l: LiveSettings, then_check: bool) {
        let Some(mut s) = self.status.as_ref().map(|s| s.settings.clone()) else { return };
        s.live = l;
        if let Some(st) = self.status.as_mut() {
            st.settings = s.clone();
        }
        self.backend.call("set_settings", json!({ "settings": s }), move |r| Msg::LiveSaved { result: r, then_check });
    }

    pub(crate) fn on_live_msg(&mut self, msg: Msg) {
        match msg {
            Msg::LiveSaved { result, then_check } => {
                if let Ok(s) = &result {
                    self.status = Some(s.clone());
                }
                if then_check {
                    self.live.checking = true;
                    self.backend.call("live_server_check", json!({}), Msg::LiveCheck);
                }
            }
            Msg::LiveCheck(r) => {
                self.live.checking = false;
                match r {
                    Ok(c) => {
                        self.live.error = None;
                        self.live.check = Some(c);
                    }
                    Err(e) => {
                        self.live.check = None;
                        self.live.error = Some(e);
                    }
                }
            }
            Msg::LiveTrusted(r) => match r {
                Ok(s) => {
                    self.status = Some(s);
                    self.live.checking = true;
                    self.backend.call("live_server_check", json!({}), Msg::LiveCheck);
                }
                Err(e) => self.live.error = Some(e),
            },
            Msg::LiveToken(r) => {
                if let Err(e) = r {
                    self.live.error = Some(e);
                } else {
                    self.live.checking = true;
                    self.backend.call("live_server_check", json!({}), Msg::LiveCheck);
                }
            }
            Msg::LivePlayers(r) => {
                self.live.players_busy = false;
                match r {
                    Ok(p) => {
                        self.live.players = p;
                        self.live.players_at = Some(self.backend_ctx().input(|i| i.time));
                        self.live.players_error = None;
                    }
                    Err(e) => self.live.players_error = Some(e),
                }
            }
            Msg::LiveServerSave(r) => {
                self.live.server_busy = false;
                if self.map.session.as_deref() != Some(SERVER_SESSION) {
                    return;
                }
                match r {
                    Ok(Some(save)) => {
                        let current = self.map.world.as_ref().map(|w| (w.save.path.clone(), w.save.saved_unix, w.save.name.clone()));
                        if current != Some((save.path.clone(), save.saved_unix, save.name.clone())) && self.map.loading.is_none() {
                            self.load_world(save.path);
                        }
                    }
                    Ok(None) => self.map.world_error = Some("The server has no save of the session it's running yet.".into()),
                    Err(e) => self.map.world_error = Some(e),
                }
            }
            _ => {}
        }
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
/// name; dimmed while offline). Returns the one under `pointer`.
pub fn draw_players(painter: &egui::Painter, view: &View, players: &[PlayerSpot], pointer: Option<Pos2>) -> Option<usize> {
    let mut hovered = None;
    for (i, p) in players.iter().enumerate() {
        let at = view.screen(p.x as f64, p.y as f64);
        if !view.rect.expand(40.0).contains(at) {
            continue;
        }
        let color = if p.online == Some(false) { TEXT_DIM } else { ACCENT };
        // Unreal yaw turns +X (east, right) towards +Y (south, down).
        let a = p.yaw.to_radians();
        let dir = vec2(a.cos(), a.sin());
        let side = vec2(-dir.y, dir.x);
        let tip = at + dir * (MARKER + 7.0);
        let shadow = Color32::from_black_alpha(200);
        painter.add(egui::Shape::convex_polygon(vec![tip, at + dir * (MARKER - 1.0) + side * 5.0, at + dir * (MARKER - 1.0) - side * 5.0], color, Stroke::new(1.5_f32, shadow)));
        painter.circle(at, MARKER, color, Stroke::new(2.0_f32, shadow));
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
pub fn player_tip(ui: &mut egui::Ui, players: &[PlayerSpot], i: usize, live: bool, saved: &str) {
    widgets::tooltip_title(ui, &label(players, i));
    if live {
        let state = match players[i].online {
            Some(false) => "Offline: where they logged out.",
            _ => "In the game now (Ficsit Remote Monitoring).",
        };
        widgets::tip_line(ui, RichText::new(state).color(TEXT));
    } else {
        widgets::tip_line(ui, RichText::new(format!("Where the save left them ({saved}).")).color(TEXT));
        widgets::tip_line(ui, RichText::new("Saves don't keep player names, only where each one stood.").small().color(TEXT_DIM));
    }
}

/// Settings › Add-ons.
pub fn settings_section(ui: &mut egui::Ui, assets: &mut crate::assets::Assets, draft: &mut fb_model::Settings) {
    widgets::divider(ui);
    ui.label(RichText::new("Add-ons").font(crate::theme::semibold(14.0)).color(ACCENT));
    widgets::checkbox(ui, assets, &mut draft.live.enabled, "Live map");
    widgets::caption(ui, "Shows players on the Map tab, and can follow a dedicated server and the Ficsit Remote Monitoring mod for live updates. Set those up with the Map tab's Live map button.");
}

/// The Live map dialog: the dedicated server and the mod.
pub fn dialog(app: &mut FactoryApp, ctx: &egui::Context, draft: &mut LiveSettings) -> bool {
    let check = app.live.check.clone();
    let checking = app.live.checking;
    let error = app.live.error.clone();
    let players_error = app.live.players_error.clone();
    let now = ctx.input(|i| i.time);
    let live_age = app.live.players_at.map(|t| now - t);
    let live_count = app.live.players.len();
    let saved = app.live_settings();
    let mut token = std::mem::take(&mut app.live.token);
    let (mut do_check, mut trust, mut save_token, mut forget_token, mut frm_changed) = (false, None, false, false, false);
    let Some(assets) = app.user.assets.as_mut() else { return false };
    let clicked = crate::dialogs::modal(
        ctx,
        "Live map",
        600.0,
        |ui| {
            widgets::caption(ui, "Players and what's built, kept up to date on the Map tab.");
            widgets::divider(ui);
            ui.label(RichText::new("Dedicated server").font(crate::theme::semibold(14.0)).color(ACCENT));
            widgets::caption(ui, "Follows the newest save of the session the server is running. Leave the address empty to follow saves on this PC.");
            ui.horizontal(|ui| {
                ui.label(RichText::new("Address").color(TEXT_DIM));
                ui.add(egui::TextEdit::singleline(&mut draft.server_host).hint_text("e.g. 192.168.1.20").desired_width(260.0).char_limit(253).margin(vec2(8.0, 6.0)));
                ui.label(RichText::new("Port").color(TEXT_DIM));
                let mut v = draft.server_port as f64;
                if widgets::num_field(ui, egui::Id::new("live_server_port"), &mut v, 80.0) {
                    draft.server_port = v.clamp(1.0, 65535.0) as u16;
                }
            });
            let token_set = check.as_ref().is_some_and(|c| c.token_set);
            ui.horizontal(|ui| {
                ui.label(RichText::new("API token").color(TEXT_DIM));
                if token_set {
                    ui.label(RichText::new("Saved, encrypted for your Windows account.").color(TEXT));
                    forget_token = widgets::button(ui, "Remove").clicked();
                } else {
                    ui.add(egui::TextEdit::singleline(&mut token).password(true).hint_text("paste it here").desired_width(300.0).char_limit(4096).margin(vec2(8.0, 6.0)));
                    save_token = widgets::button_enabled(ui, "Save token", !token.trim().is_empty()).clicked();
                }
            });
            widgets::caption(ui, "Make one by typing server.GenerateAPIToken in the server's console. It's only ever sent to this server.");
            ui.horizontal(|ui| {
                do_check = widgets::button_enabled(ui, if checking { "Checking…" } else { "Check server" }, !checking && !draft.server_host.trim().is_empty()).clicked();
            });
            if let Some(e) = &error {
                crate::dialogs::status_line(ui, assets, false, e);
            }
            if let Some(c) = &check {
                if let Some(fp) = &c.untrusted_cert {
                    let first = saved.server_cert.is_none();
                    crate::dialogs::status_line(
                        ui,
                        assets,
                        false,
                        if first { "The server's certificate isn't trusted yet. Make sure this is your server before trusting it:" } else { "The server's certificate has changed since you trusted it. Only trust it if you changed it on the server:" },
                    );
                    ui.label(RichText::new(format!("SHA-256  {fp}")).small().monospace().color(TEXT));
                    if widgets::button(ui, "Trust this certificate").clicked() {
                        trust = Some(fp.clone());
                    }
                } else if let Some(e) = &c.error {
                    crate::dialogs::status_line(ui, assets, false, e);
                }
                if c.reachable && c.untrusted_cert.is_none() {
                    let health = c.health.as_deref().unwrap_or("answering");
                    crate::dialogs::status_line(ui, assets, true, &format!("Connected securely. The server is {health}."));
                    match (c.token_ok, &c.session) {
                        (Some(true), Some(s)) => {
                            let players = match (c.players, c.player_limit) {
                                (Some(n), Some(m)) => format!(", {n} of {m} players on"),
                                (Some(n), None) => format!(", {n} players on"),
                                _ => String::new(),
                            };
                            crate::dialogs::status_line(ui, assets, true, &format!("Running “{s}”{players}. Pick “{SERVER_SESSION}” on the Map tab to follow it."));
                        }
                        (None, _) => crate::dialogs::status_line(ui, assets, false, "Add the API token to follow its saves."),
                        _ => {}
                    }
                }
            }
            widgets::divider(ui);
            ui.label(RichText::new("Ficsit Remote Monitoring").font(crate::theme::semibold(14.0)).color(ACCENT));
            frm_changed |= widgets::checkbox(ui, assets, &mut draft.frm, "Live players from the mod").changed();
            let via_server = !draft.server_host.trim().is_empty();
            if via_server {
                widgets::caption(ui, "Install the mod on the dedicated server with the Satisfactory Mod Manager. It's reached through the server above, so nothing else needs setting up.");
            } else {
                widgets::caption(ui, "Install the mod on the PC hosting the game with the Satisfactory Mod Manager, then type /frm http start in the game's chat (or turn on its web server's autostart in the mod's options).");
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Address").color(TEXT_DIM));
                    frm_changed |= ui.add(egui::TextEdit::singleline(&mut draft.frm_host).desired_width(260.0).char_limit(253).margin(vec2(8.0, 6.0))).lost_focus();
                    ui.label(RichText::new("Port").color(TEXT_DIM));
                    let mut v = draft.frm_port as f64;
                    if widgets::num_field(ui, egui::Id::new("live_frm_port"), &mut v, 80.0) {
                        draft.frm_port = v.clamp(1.0, 65535.0) as u16;
                        frm_changed = true;
                    }
                });
            }
            if saved.frm {
                match (live_age, &players_error) {
                    (_, Some(e)) => crate::dialogs::status_line(ui, assets, false, e),
                    (Some(age), None) if age < PLAYERS_STALE => {
                        crate::dialogs::status_line(ui, assets, true, &format!("Live: {live_count} {} reported.", if live_count == 1 { "player" } else { "players" }))
                    }
                    _ => widgets::caption(ui, "Open the Map tab to see it."),
                }
            }
        },
        &[("Close", true)],
    );
    app.live.token = token;
    if let Some(fp) = trust {
        app.backend.call("live_server_trust", json!({ "fingerprint": fp }), Msg::LiveTrusted);
    }
    if save_token {
        let t = std::mem::take(&mut app.live.token);
        app.backend.call("live_server_token", json!({ "token": t.trim() }), Msg::LiveToken);
    }
    if forget_token {
        app.backend.call("live_server_token", json!({ "token": null }), Msg::LiveToken);
    }
    if do_check {
        app.live.checking = true;
        app.live.error = None;
        app.live_save(draft.clone(), true);
    } else if frm_changed || clicked.is_some() {
        // The mod's settings take effect right away; the server's when
        // it's checked (or now, on closing).
        if *draft != saved {
            app.live_save(draft.clone(), false);
        }
    }
    clicked.is_none()
}
