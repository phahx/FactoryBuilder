//! The Map tab's save side: the player's sessions, what a save has built
//! (fb_engine::world), and following a session's newest save. Saves are only
//! read, and only from the places saves.rs allows.

use crate::files::Allowed;
use crate::AppState;
use fb_model::saves::SaveSummary;
use fb_model::world::{MapInfo, Session, World};
use fb_model::GameData;
use std::path::Path;

/// `<icons>/map/map.json` is a few hundred KB; anything this big isn't it.
const MAX_MAP_JSON: u64 = 32 * 1024 * 1024;

/// Saves grouped by session, each newest first; the session with the newest
/// save comes first.
pub fn group_sessions(mut saves: Vec<SaveSummary>) -> Vec<Session> {
    saves.sort_by_key(|s| std::cmp::Reverse(s.saved_unix));
    let mut out: Vec<Session> = Vec::new();
    for s in saves {
        match out.iter_mut().find(|g| g.session == s.session) {
            Some(g) => g.saves.push(s),
            None => out.push(Session { session: s.session.clone(), saves: vec![s] }),
        }
    }
    out
}

/// The player's sessions (headers only).
pub fn sessions() -> Vec<Session> {
    group_sessions(crate::saves::list())
}

/// The newest save of `session`, from the headers only, so the window can
/// poll it cheaply and load the save when it changes.
pub fn latest(session: &str) -> Option<SaveSummary> {
    newest_of(crate::saves::list(), session)
}

fn newest_of(saves: Vec<SaveSummary>, session: &str) -> Option<SaveSummary> {
    saves.into_iter().filter(|s| s.session == session).max_by_key(|s| s.saved_unix)
}

/// The world map file the icon helper writes, if it is there and sound.
fn map_info(icons_dir: &str) -> Option<MapInfo> {
    if icons_dir.is_empty() {
        return None;
    }
    let path = Path::new(icons_dir).join("map").join("map.json");
    if std::fs::metadata(&path).ok()?.len() > MAX_MAP_JSON {
        return None;
    }
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// What the save at `path` has built. The map's resource nodes (when the
/// world map has been extracted) tell what miners send down their belts.
pub fn load(allowed: &Allowed, path: &str, game: &GameData, icons_dir: &str) -> Result<World, String> {
    let (bytes, save) = crate::saves::read_allowed(allowed, path)?;
    let mut world = fb_engine::world::read_world(&bytes, game).map_err(|e| e.to_string())?;
    drop(bytes);
    world.save = save;
    if let Some(map) = map_info(icons_dir) {
        fb_engine::world::apply_map(&mut world, &map, game);
    }
    Ok(world)
}

/// `load` with the app's game data (loaded now if the window hasn't yet).
pub fn load_for(state: &AppState, path: &str) -> Result<World, String> {
    let (game_dir, icons_dir) = {
        let s = state.settings.lock().unwrap();
        (s.game_dir.clone(), s.icons_dir.clone())
    };
    let cached = state.game.lock().unwrap().clone();
    let game = match cached {
        Some(g) => g,
        None => {
            if game_dir.is_empty() {
                return Err("Set your Satisfactory folder in Settings.".into());
            }
            let g = fb_engine::docs::load_game_data(Path::new(&game_dir)).map_err(|e| e.to_string())?;
            *state.game.lock().unwrap() = Some(g.clone());
            g
        }
    };
    load(&state.allowed, path, &game, &icons_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn save(session: &str, name: &str, t: i64) -> SaveSummary {
        SaveSummary { path: format!(r"C:\saves\{name}.sav"), session: session.into(), name: name.into(), saved_unix: t, ..Default::default() }
    }

    #[test]
    fn sessions_group_newest_first() {
        let list = vec![save("A", "a1", 10), save("B", "b1", 30), save("A", "a2", 20), save("B", "b0", 5), save("C", "c1", 1)];
        let g = group_sessions(list.clone());
        let names: Vec<(&str, Vec<&str>)> = g.iter().map(|s| (s.session.as_str(), s.saves.iter().map(|x| x.name.as_str()).collect())).collect();
        assert_eq!(names, vec![("B", vec!["b1", "b0"]), ("A", vec!["a2", "a1"]), ("C", vec!["c1"])]);
        assert_eq!(newest_of(list.clone(), "A").unwrap().name, "a2");
        assert_eq!(newest_of(list, "nope"), None);
    }

    #[test]
    fn only_saves_from_allowed_places() {
        let allowed = Allowed::default();
        let game = GameData::default();
        let tmp = std::env::temp_dir().join(format!("fb-world-test-{}.sav", std::process::id()));
        std::fs::write(&tmp, b"not a save").unwrap();
        let p = tmp.to_string_lossy().into_owned();
        assert!(load(&allowed, &p, &game, "").unwrap_err().contains("can't be opened"));
        allowed.allow(&tmp);
        assert!(load(&allowed, &p, &game, "").unwrap_err().contains("isn't a Satisfactory save"));
        assert!(load(&allowed, r"C:\Windows\win.ini", &game, "").is_err());
        let _ = std::fs::remove_file(tmp);
    }

    #[test]
    fn a_bad_map_file_is_ignored() {
        let dir = std::env::temp_dir().join(format!("fb-world-map-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("map")).unwrap();
        let d = dir.to_string_lossy().into_owned();
        assert!(map_info(&d).is_none(), "missing");
        std::fs::write(dir.join("map").join("map.json"), b"{ not json").unwrap();
        assert!(map_info(&d).is_none(), "broken");
        let info = MapInfo { x_min: -1.0, y_min: -1.0, size: 2.0, tile_px: 256, levels: 1, nodes: vec![] };
        std::fs::write(dir.join("map").join("map.json"), serde_json::to_vec(&info).unwrap()).unwrap();
        assert_eq!(map_info(&d), Some(info));
        assert!(map_info("").is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// With FB_TEST_SAVES (a folder holding late.sav) and the game's Docs.
    #[test]
    fn loads_a_real_save() {
        let Ok(dir) = std::env::var("FB_TEST_SAVES") else { return };
        let game_dir = std::env::var("SATISFACTORY_DIR").unwrap_or_else(|_| r"C:\Program Files (x86)\Steam\steamapps\common\Satisfactory".into());
        let Ok(game) = fb_engine::docs::load_game_data(Path::new(&game_dir)) else { return };
        for file in ["early.sav", "late.sav"] {
            let path = Path::new(&dir).join(file);
            if !path.exists() {
                continue;
            }
            let allowed = Allowed::default();
            allowed.allow(&path);
            let w = load(&allowed, &path.to_string_lossy(), &game, "").unwrap();
            assert_eq!(w.save.path, path.to_string_lossy());
            assert!(!w.machines.is_empty() && !w.carriers.is_empty() && !w.links.is_empty());
            let json = serde_json::to_string(&w).unwrap();
            let back: World = serde_json::from_str(&json).unwrap();
            assert_eq!(back, w, "survives the trip to the window");
        }
    }
}
