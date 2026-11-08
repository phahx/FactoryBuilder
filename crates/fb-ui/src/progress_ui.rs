//! "Use what I've unlocked": recipes from a save file, or every standard
//! recipe up to a HUB tier. Both replace the recipe choices (undoable).

use crate::app::FactoryApp;
use crate::backend::Msg;
use crate::dialogs::{modal, Dialog};
use crate::theme::*;
use crate::widgets;
use egui::{RichText, Ui};
use fb_engine::progress;
use fb_model::saves::{SaveSummary, SaveUnlocks};
use fb_model::GameData;
use serde_json::json;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SavesDialog {
    /// None while the list is loading.
    pub saves: Option<Vec<SaveSummary>>,
    /// The save being read.
    pub reading: Option<String>,
    pub error: Option<String>,
}

/// The controls at the top of the Recipes section. Returns a message when
/// they changed the recipe choices; sets `open_saves` to show the save picker.
pub fn unlock_controls(ui: &mut Ui, game: &GameData, planner: &mut crate::app::Planner, open_saves: &mut bool) -> Option<String> {
    let mut said = None;
    let max = progress::max_tier(game);
    ui.horizontal(|ui| {
        if widgets::button(ui, "From my save…")
            .on_hover_text("Use exactly the recipes a Satisfactory save has unlocked, alternates included.")
            .clicked()
        {
            *open_saves = true;
        }
        let mut pick: Option<u32> = None;
        egui::ComboBox::from_id_salt("tier_pick")
            .selected_text("Up to tier…")
            .width(130.0)
            .show_ui(ui, |ui| {
                for t in 0..=max {
                    let label = if t == 0 { "Tier 0 (onboarding)".to_string() } else { format!("Tier {t}") };
                    if ui.selectable_label(false, label).clicked() {
                        pick = Some(t);
                    }
                }
            })
            .response
            .on_hover_text("Use the standard recipes you'd have once every HUB milestone up to that tier is done (MAM research included, no alternates).");
        if let Some(t) = pick {
            planner.enabled = progress::recipes_up_to_tier(game, t);
            said = Some(format!("Using the {} standard recipes up to Tier {t}. Undo (Ctrl+Z) to go back.", planner.enabled.len()));
        }
    });
    said
}

impl FactoryApp {
    pub(crate) fn open_saves_dialog(&mut self) {
        self.dialog = Dialog::Saves(SavesDialog::default());
        self.backend.call("list_saves", json!({}), Msg::Saves);
    }

    fn read_save(&mut self, path: String) {
        if let Dialog::Saves(d) = &mut self.dialog {
            d.reading = Some(path.clone());
            d.error = None;
        }
        self.backend.call("read_save_unlocks", json!({ "path": path }), Msg::SaveUnlocks);
    }

    /// Replies for the save picker.
    pub(crate) fn on_progress_msg(&mut self, ctx: &egui::Context, msg: Msg) {
        match msg {
            Msg::Saves(r) => {
                if let Dialog::Saves(d) = &mut self.dialog {
                    match r {
                        Ok(list) => d.saves = Some(list),
                        Err(e) => {
                            d.saves = Some(Vec::new());
                            d.error = Some(e);
                        }
                    }
                }
            }
            Msg::PickedSaveGame(r) => match r {
                Ok(Some(path)) => self.read_save(path),
                Ok(None) => {}
                Err(e) => self.say(ctx, e, true),
            },
            Msg::SaveUnlocks(r) => match r {
                Ok(u) => self.apply_save(ctx, u),
                Err(e) => {
                    if let Dialog::Saves(d) = &mut self.dialog {
                        d.reading = None;
                        d.error = Some(e);
                    }
                }
            },
            _ => {}
        }
    }

    fn apply_save(&mut self, ctx: &egui::Context, u: SaveUnlocks) {
        let game = &self.user.game;
        let enabled = progress::recipes_from_schematics(game, u.schematics.iter().map(String::as_str));
        if enabled.is_empty() {
            if let Dialog::Saves(d) = &mut self.dialog {
                d.reading = None;
                d.error = Some("That save hasn't unlocked any production recipes yet.".into());
            }
            return;
        }
        let alts = enabled.iter().filter(|r| game.recipes.get(*r).is_some_and(|r| r.alternate)).count();
        let n = enabled.len();
        self.planner.enabled = enabled;
        self.dialog = Dialog::None;
        self.changed();
        let who = if u.save.session.is_empty() { u.save.name.clone() } else { u.save.session.clone() };
        self.say(ctx, format!("Using the {n} recipes unlocked in “{who}” ({alts} alternates). Undo (Ctrl+Z) to go back."), false);
    }
}

/// "2026-11-07" (UTC) from Unix seconds.
pub(crate) fn date_text(unix: i64) -> String {
    date(unix)
}

fn date(unix: i64) -> String {
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = unix.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y}-{m:02}-{d:02}")
}

fn play_time(s: i64) -> String {
    let h = s / 3600;
    let m = (s % 3600) / 60;
    if h > 0 {
        format!("{h} h {m} min played")
    } else {
        format!("{m} min played")
    }
}

pub fn show(app: &mut FactoryApp, ctx: &egui::Context, d: SavesDialog) {
    let mut pick: Option<String> = None;
    let busy = d.reading.is_some();
    let clicked = modal(
        ctx,
        "Recipes from a save",
        560.0,
        |ui| {
            widgets::caption(ui, "Pick a save to plan with exactly the recipes it has unlocked: HUB milestones, MAM research and the alternates from hard drives. The save is only read, never changed.");
            match &d.saves {
                None => {
                    ui.label(RichText::new("Looking for saves…").color(TEXT_DIM));
                }
                Some(list) if list.is_empty() => {
                    ui.label(RichText::new("No saves were found in the game's save folder. Use Browse to pick one.").color(TEXT_DIM));
                }
                Some(list) => {
                    egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        for s in list {
                            let reading = d.reading.as_deref() == Some(s.path.as_str());
                            let row = widgets::list_row(ui, 44.0, |ui| {
                                ui.vertical(|ui| {
                                    ui.add_space(4.0);
                                    let title = if s.session.is_empty() || s.session == s.name { s.name.clone() } else { format!("{} · {}", s.session, s.name) };
                                    ui.label(RichText::new(title).color(TEXT));
                                    let detail = if reading { "Reading…".to_string() } else { format!("{} · {}", date(s.saved_unix), play_time(s.play_seconds)) };
                                    ui.label(RichText::new(detail).small().color(TEXT_DIM));
                                });
                            });
                            if row.clicked() && !busy {
                                pick = Some(s.path.clone());
                            }
                        }
                    });
                }
            }
            if let Some(e) = &d.error {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(e).color(DANGER));
                });
            }
            ui.add_space(2.0);
        },
        &[("Cancel", true), ("Browse…", !busy)],
    );
    match clicked {
        Some(0) => app.dialog = Dialog::None,
        Some(1) => app.backend.call("pick_save_game", json!({}), Msg::PickedSaveGame),
        _ => {}
    }
    if let Some(p) = pick {
        app.read_save(p);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn dates_and_play_time() {
        assert_eq!(super::date(0), "1970-01-01");
        assert_eq!(super::date(1_700_000_000), "2023-11-14");
        assert_eq!(super::play_time(799_915), "222 h 11 min played");
        assert_eq!(super::play_time(59), "0 min played");
    }
}
