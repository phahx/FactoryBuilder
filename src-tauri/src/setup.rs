//! Finding the game, noticing when it updates, and running the icon helper.

use fb_model::setup::ExtractProgress;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const STEAM_DEFAULT: &str = r"C:\Program Files (x86)\Steam\steamapps\common\Satisfactory";

/// Whether `dir` is a Satisfactory install (it has the game's Docs file).
pub fn is_game_dir(dir: &Path) -> bool {
    fb_engine::docs::docs_path(dir).is_file()
}

/// Satisfactory installs on this PC: Steam libraries (from Steam's own
/// library list) and the Epic Games launcher's install manifests.
pub fn detect_game_dirs() -> Vec<String> {
    let mut found: Vec<PathBuf> = vec![PathBuf::from(STEAM_DEFAULT)];
    if let Some(steam) = steam_dir() {
        found.push(steam.join(r"steamapps\common\Satisfactory"));
        if let Ok(vdf) = std::fs::read_to_string(steam.join(r"steamapps\libraryfolders.vdf")) {
            for lib in vdf_paths(&vdf) {
                found.push(PathBuf::from(lib).join(r"steamapps\common\Satisfactory"));
            }
        }
    }
    let program_data = std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
    if let Ok(rd) = std::fs::read_dir(program_data.join(r"Epic\EpicGamesLauncher\Data\Manifests")) {
        for e in rd.filter_map(Result::ok).filter(|e| e.path().extension().is_some_and(|x| x == "item")) {
            if let Some(dir) = std::fs::read_to_string(e.path()).ok().and_then(|s| epic_install(&s)) {
                found.push(dir);
            }
        }
    }
    let mut out: Vec<String> = Vec::new();
    for d in found {
        let s = d.to_string_lossy().into_owned();
        if is_game_dir(&d) && !out.iter().any(|o| o.eq_ignore_ascii_case(&s)) {
            out.push(s);
        }
    }
    out
}

fn steam_dir() -> Option<PathBuf> {
    let key = windows_registry::CURRENT_USER.open(r"Software\Valve\Steam").ok()?;
    key.get_string("SteamPath").ok().map(|p| PathBuf::from(p.replace('/', "\\")))
}

/// The `"path"` entries of Steam's libraryfolders.vdf.
fn vdf_paths(vdf: &str) -> Vec<String> {
    vdf.lines()
        .filter_map(|l| {
            let l = l.trim();
            let rest = l.strip_prefix("\"path\"")?.trim();
            let v = rest.strip_prefix('"')?.strip_suffix('"')?;
            Some(v.replace("\\\\", "\\"))
        })
        .collect()
}

/// An Epic manifest's install folder, if it is Satisfactory.
fn epic_install(json: &str) -> Option<PathBuf> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let name = v.get("DisplayName")?.as_str()?;
    if !name.starts_with("Satisfactory") {
        return None;
    }
    v.get("InstallLocation")?.as_str().map(PathBuf::from)
}

/// A fingerprint of the game files icons come from (sizes and dates of the
/// Docs file and the paks). It changes when the game updates.
pub fn game_stamp(dir: &Path) -> Option<String> {
    let line = |p: &Path| -> Option<String> {
        let m = p.metadata().ok()?;
        let t = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
        Some(format!("{}:{}:{t};", p.file_name()?.to_string_lossy(), m.len()))
    };
    let mut text = line(&fb_engine::docs::docs_path(dir))?;
    let mut paks: Vec<PathBuf> = std::fs::read_dir(dir.join(r"FactoryGame\Content\Paks"))
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| ["pak", "utoc", "ucas"].iter().any(|e| x.eq_ignore_ascii_case(e))))
        .collect();
    paks.sort();
    for p in paks {
        text += &line(&p)?;
    }
    // FNV-1a: small, and the same in every build (unlike std's hasher).
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h = (h ^ b as u64).wrapping_mul(0x0100_0000_01b3);
    }
    Some(format!("{h:016x}"))
}

/// Files in an icon folder: (item icons, UI files).
pub fn count_icons(dir: &Path) -> (usize, usize) {
    let count = |d: &Path, png_only: bool| {
        std::fs::read_dir(d)
            .map(|rd| rd.filter_map(Result::ok).filter(|e| e.path().is_file() && (!png_only || e.path().extension().is_some_and(|x| x == "png"))).count())
            .unwrap_or(0)
    };
    (count(dir, true), count(&dir.join("ui"), false))
}

pub type Progress = Arc<Mutex<ExtractProgress>>;

/// Runs the icon helper in the background; `progress` follows it.
/// `on_done(ok)` runs on the helper's thread when it ends.
pub fn start_extract(
    helper: &Path,
    shim: &Path,
    game_dir: &Path,
    out_dir: &Path,
    progress: Progress,
    on_done: impl FnOnce(bool) + Send + 'static,
) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    {
        let mut p = progress.lock().unwrap();
        if p.running {
            return Err("Icons are already being extracted.".into());
        }
        *p = ExtractProgress { running: true, stage: "starting".into(), ..Default::default() };
    }
    let child = std::process::Command::new(helper)
        .arg("--game")
        .arg(game_dir)
        .arg("--out")
        .arg(out_dir)
        .arg("--oodle")
        .arg(shim)
        .arg("--progress")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .creation_flags(crate::install::CREATE_NO_WINDOW)
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            let mut p = progress.lock().unwrap();
            p.running = false;
            p.finished = true;
            p.error = Some(format!("Couldn't start the icon helper: {e}"));
            return Err(p.error.clone().unwrap());
        }
    };
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    // The helper reports per-icon problems on stderr; keep the last few for
    // the error message.
    let errors = std::thread::spawn(move || {
        let mut last: Vec<String> = Vec::new();
        for l in BufReader::new(stderr).lines().map_while(Result::ok) {
            if !l.trim().is_empty() {
                last.push(l.trim().to_string());
                if last.len() > 5 {
                    last.remove(0);
                }
            }
        }
        last
    });
    let out_dir = out_dir.to_path_buf();
    std::thread::spawn(move || {
        for l in BufReader::new(stdout).lines().map_while(Result::ok) {
            let mut p = progress.lock().unwrap();
            if let Some(stage) = l.strip_prefix("@stage ") {
                p.stage = stage.trim().to_string();
            } else if let Some(nums) = l.strip_prefix("@progress ") {
                let mut it = nums.split_whitespace().filter_map(|n| n.parse::<u32>().ok());
                if let (Some(d), Some(t)) = (it.next(), it.next()) {
                    p.done = d;
                    p.total = t;
                }
            }
        }
        let status = child.wait();
        let last_errors = errors.join().unwrap_or_default();
        let (icons, ui) = count_icons(&out_dir);
        let ok = matches!(&status, Ok(s) if s.success()) && icons > 0;
        {
            let mut p = progress.lock().unwrap();
            p.running = false;
            p.finished = true;
            p.icons = icons;
            p.ui = ui;
            if !ok {
                let why = last_errors.last().cloned().unwrap_or_else(|| match &status {
                    Ok(s) => format!("it stopped with code {}", s.code().unwrap_or(-1)),
                    Err(e) => e.to_string(),
                });
                p.error = Some(format!("Extracting icons failed: {why}"));
            }
        }
        on_done(ok);
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_steam_library_paths() {
        let vdf = "\"libraryfolders\"\n{\n\t\"0\"\n\t{\n\t\t\"path\"\t\t\"C:\\\\Program Files (x86)\\\\Steam\"\n\t}\n\t\"1\"\n\t{\n\t\t\"path\"\t\t\"D:\\\\SteamLibrary\"\n\t\t\"label\"\t\t\"\"\n\t}\n}\n";
        assert_eq!(vdf_paths(vdf), vec![r"C:\Program Files (x86)\Steam".to_string(), r"D:\SteamLibrary".to_string()]);
    }

    #[test]
    fn reads_epic_manifests() {
        let m = r#"{"DisplayName": "Satisfactory", "InstallLocation": "E:\\Epic\\Satisfactory"}"#;
        assert_eq!(epic_install(m), Some(PathBuf::from(r"E:\Epic\Satisfactory")));
        assert_eq!(epic_install(r#"{"DisplayName": "Fortnite", "InstallLocation": "E:\\F"}"#), None);
    }

    #[test]
    fn stamp_follows_the_game_files() {
        let root = std::env::temp_dir().join(format!("fb-stamp-{}", std::process::id()));
        let docs = root.join(r"CommunityResources\Docs");
        let paks = root.join(r"FactoryGame\Content\Paks");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::create_dir_all(&paks).unwrap();
        std::fs::write(docs.join("en-US.json"), b"[]").unwrap();
        std::fs::write(paks.join("FactoryGame-Windows.utoc"), b"1234").unwrap();
        let a = game_stamp(&root).unwrap();
        assert_eq!(game_stamp(&root).unwrap(), a, "stable");
        std::fs::write(paks.join("FactoryGame-Windows.utoc"), b"12345").unwrap();
        assert_ne!(game_stamp(&root).unwrap(), a, "an updated pak changes it");
        assert!(game_stamp(&root.join("nope")).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn finds_this_pcs_game() {
        // Informational on machines without the game.
        let dirs = detect_game_dirs();
        eprintln!("found: {dirs:?}");
        if Path::new(STEAM_DEFAULT).join(r"CommunityResources\Docs\en-US.json").is_file() {
            assert!(dirs.iter().any(|d| d.eq_ignore_ascii_case(STEAM_DEFAULT)));
        }
    }
}
