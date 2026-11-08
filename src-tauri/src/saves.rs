//! The player's Satisfactory saves: listing them and reading what they've
//! unlocked (see fb_engine::save). Saves are only read, never written.

use crate::files::Allowed;
use fb_model::saves::{SaveSummary, SaveUnlocks};
use std::io::Read;
use std::path::{Path, PathBuf};

/// Big enough for any header; the world data after it isn't needed to list.
const HEADER_BYTES: u64 = 16 * 1024;
/// Late-game saves are tens of MB; anything this big isn't a save.
const MAX_SAVE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_LISTED: usize = 300;

/// `%LOCALAPPDATA%\FactoryGame\Saved\SaveGames` (one folder per account inside).
pub fn saves_root() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join(r"FactoryGame\Saved\SaveGames"))
}

fn summary(path: &Path) -> Option<SaveSummary> {
    let mut head = Vec::new();
    std::fs::File::open(path).ok()?.take(HEADER_BYTES).read_to_end(&mut head).ok()?;
    let (mut s, _) = fb_engine::save::read_header(&head).ok()?;
    s.path = path.to_string_lossy().into_owned();
    if s.name.is_empty() {
        s.name = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    }
    Some(s)
}

/// Saves on this PC, newest first.
pub fn list() -> Vec<SaveSummary> {
    let Some(root) = saves_root() else { return Vec::new() };
    let mut out = Vec::new();
    for account in std::fs::read_dir(&root).into_iter().flatten().filter_map(Result::ok) {
        if !account.path().is_dir() {
            continue;
        }
        for f in std::fs::read_dir(account.path()).into_iter().flatten().filter_map(Result::ok) {
            let p = f.path();
            if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("sav")) {
                if let Some(s) = summary(&p) {
                    out.push(s);
                }
            }
        }
    }
    out.sort_by(|a, b| b.saved_unix.cmp(&a.saved_unix));
    out.truncate(MAX_LISTED);
    out
}

/// Only saves in the game's save folder, or ones picked in the Open dialog.
fn may_read(allowed: &Allowed, path: &Path) -> bool {
    if allowed.has(path) {
        return true;
    }
    match (saves_root().and_then(|r| r.canonicalize().ok()), path.canonicalize()) {
        (Some(root), Ok(p)) => p.starts_with(root),
        _ => false,
    }
}

pub fn unlocks(allowed: &Allowed, path: &str) -> Result<SaveUnlocks, String> {
    let p = PathBuf::from(path);
    if !p.extension().is_some_and(|x| x.eq_ignore_ascii_case("sav")) || !may_read(allowed, &p) {
        return Err("That file can't be opened here.".into());
    }
    let size = std::fs::metadata(&p).map_err(|e| format!("Couldn't open the save: {e}"))?.len();
    if size > MAX_SAVE_BYTES {
        return Err("That file is too big to be a save.".into());
    }
    let bytes = std::fs::read(&p).map_err(|e| format!("Couldn't read the save: {e}"))?;
    let schematics = fb_engine::save::purchased_schematics(&bytes).map_err(|e| e.to_string())?;
    let save = summary(&p).ok_or("That isn't a Satisfactory save file.")?;
    Ok(SaveUnlocks { save, schematics })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_save_files_from_allowed_places() {
        let allowed = Allowed::default();
        let tmp = std::env::temp_dir().join(format!("fb-saves-test-{}.sav", std::process::id()));
        std::fs::write(&tmp, b"not a save").unwrap();
        let p = tmp.to_string_lossy().into_owned();
        assert!(unlocks(&allowed, &p).unwrap_err().contains("can't be opened"), "outside the save folder and not picked");
        allowed.allow(&tmp);
        assert!(unlocks(&allowed, &p).unwrap_err().contains("isn't a Satisfactory save"), "picked, but not a save");
        assert!(unlocks(&allowed, r"C:\Windows\win.ini").is_err());
        let _ = std::fs::remove_file(tmp);
    }

    #[test]
    fn lists_this_pcs_saves() {
        for s in list().iter().take(3) {
            eprintln!("{} | {} | {}s", s.name, s.session, s.play_seconds);
            assert!(s.path.to_lowercase().ends_with(".sav"));
        }
    }
}
