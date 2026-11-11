// Factory Builder: Tauri backend.
//
// The UI is egui compiled to wasm and runs in this app's webview (see
// crates/fb-ui). It calls the commands below through `window.__TAURI__`.
// Game data comes from the player's own install
// (CommunityResources/Docs/en-US.json) and icons from a local cache that the
// bundled helper (tools/IconExtractor.exe) extracts on first launch. Nothing
// from the game ships with the app.
//
// Command line:
//   <file.fbfactory|.fbproject>  open it
//   --uninstall [--quiet]   remove the installed copy (Apps & features runs this)
//   --test-profile <dir>    development: a throwaway profile in <dir> that
//                           installs as "Factory Builder (Test)"

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use fb_model::{GameData, Settings, SolveOutcome, SolveRequest, Status};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{ipc::Response, AppHandle, Manager, State};

mod files;
mod live;
mod install;
mod room;
mod saves;
mod selftest;
mod setup;
mod update;
mod world;

/// Development build (`--features dev`): Developer settings, and it never
/// hands off to an installed copy (except in a test profile).
const DEV: bool = cfg!(feature = "dev");
const TEST_PROFILE_DIR: &str = "FactoryBuilder-test-profile";

struct AppState {
    settings: Mutex<Settings>,
    game: Mutex<Option<GameData>>,
    /// Command names in call order (read by the hidden-window self-test).
    calls: Mutex<Vec<String>>,
    /// The planning room this app hosts or has joined, if any.
    room: Mutex<Option<room::Room>>,
    /// Files the window may read/write (see files.rs).
    allowed: files::Allowed,
    /// A .fbfactory or .fbproject given on the command line.
    startup_file: Mutex<Option<String>>,
    /// Settings, icon cache and autosave live here.
    data_dir: PathBuf,
    identity: install::Identity,
    extract: setup::Progress,
    /// Live map: the certificate the dedicated server last showed that
    /// isn't trusted yet (only that one can be trusted), and the server
    /// save downloaded last (name, play time).
    live_seen: Mutex<Option<String>>,
    live_save: Mutex<Option<(String, i64)>>,
    /// A dedicated server (host:port) that said it has no `frm` function,
    /// and when: FRM's own web server is asked straight away until then
    /// plus `NO_FRM_RECHECK` (each try through the server costs a TLS
    /// handshake, measured ~90 ms locally).
    live_no_frm: Mutex<Option<(String, std::time::Instant)>>,
    /// The newer release found, ready to install when asked.
    update: Mutex<Option<update::Release>>,
}

impl AppState {
    fn log(&self, cmd: &str) {
        self.calls.lock().unwrap().push(cmd.to_string());
    }
}

type CmdResult<T> = Result<T, String>;

fn config_path(data_dir: &Path) -> PathBuf {
    data_dir.join("settings.json")
}

fn projects_dir(state: &AppState) -> CmdResult<PathBuf> {
    let dir = state.data_dir.join("projects");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

fn default_settings(data_dir: &Path) -> Settings {
    Settings {
        // The first Satisfactory install found; setup lets the user change it.
        game_dir: setup::detect_game_dirs().into_iter().next().unwrap_or_default(),
        icons_dir: data_dir.join("icons").to_string_lossy().into_owned(),
        ..Settings::default()
    }
}

fn load_settings(data_dir: &Path) -> Settings {
    let mut s: Settings = std::fs::read_to_string(config_path(data_dir))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| default_settings(data_dir));
    if s.icons_dir.is_empty() {
        s.icons_dir = data_dir.join("icons").to_string_lossy().into_owned();
    }
    s
}

/// The window's settings, with what only the backend sets kept: the icon
/// folder and stamp, and the trusted server certificate (live_server_trust),
/// which another server address or port clears.
fn keep_backend_owned(old: &Settings, mut new: Settings) -> Settings {
    new.icons_stamp = old.icons_stamp.clone();
    new.icons_dir = old.icons_dir.clone();
    let same_server = old.live.server_host.trim().eq_ignore_ascii_case(new.live.server_host.trim()) && old.live.server_port == new.live.server_port;
    new.live.server_cert = if same_server { old.live.server_cert.clone() } else { None };
    new
}

fn save_settings(data_dir: &Path, settings: &Settings) -> CmdResult<()> {
    let path = config_path(data_dir);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(settings).unwrap()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

/// Project names become file names: keep them boring.
fn project_file(state: &AppState, name: &str) -> CmdResult<PathBuf> {
    let clean: String = name
        .trim()
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'))
        .collect();
    let clean = clean.trim_matches('.').trim();
    if clean.is_empty() {
        return Err("Project name must contain letters or digits.".into());
    }
    Ok(projects_dir(state)?.join(format!("{clean}.fbproj.json")))
}

fn make_status(state: &AppState) -> CmdResult<Status> {
    let settings = state.settings.lock().unwrap().clone();
    let (icons_found, ui_assets_found) = setup::count_icons(Path::new(&settings.icons_dir));
    // Only compared once icons were extracted by this version of the app.
    let game_changed = icons_found > 0
        && settings
            .icons_stamp
            .as_ref()
            .is_some_and(|s| setup::game_stamp(Path::new(&settings.game_dir)).is_some_and(|now| &now != s));
    let map_found = Path::new(&settings.icons_dir).join("map").join("map.json").is_file();
    Ok(Status {
        icons_found,
        ui_assets_found,
        map_found,
        projects_dir: projects_dir(state)?.to_string_lossy().into_owned(),
        game_changed,
        setup: state.identity.info(DEV),
        settings,
    })
}

#[tauri::command]
fn status(state: State<AppState>) -> CmdResult<Status> {
    state.log("status");
    make_status(&state)
}

#[tauri::command]
fn set_settings(state: State<AppState>, settings: Settings) -> CmdResult<Status> {
    state.log("set_settings");
    let settings = keep_backend_owned(&state.settings.lock().unwrap(), settings);
    save_settings(&state.data_dir, &settings)?;
    let changed_game = state.settings.lock().unwrap().game_dir != settings.game_dir;
    *state.settings.lock().unwrap() = settings;
    if changed_game {
        *state.game.lock().unwrap() = None;
    }
    make_status(&state)
}

#[tauri::command]
async fn load_game(state: State<'_, AppState>) -> CmdResult<GameData> {
    state.log("load_game");
    if let Some(g) = state.game.lock().unwrap().clone() {
        return Ok(g);
    }
    let dir = state.settings.lock().unwrap().game_dir.clone();
    if dir.is_empty() {
        return Err("Set your Satisfactory folder in Settings.".into());
    }
    let data = fb_engine::docs::load_game_data(Path::new(&dir)).map_err(|e| {
        format!("{e}\n(looked for {})", fb_engine::docs::docs_path(Path::new(&dir)).display())
    })?;
    *state.game.lock().unwrap() = Some(data.clone());
    Ok(data)
}

/// PNG bytes of `<class>.png` from the icon cache, sent as raw binary.
#[tauri::command]
fn icon(state: State<AppState>, class: String) -> CmdResult<Response> {
    state.log("icon");
    if !class.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err("bad icon name".into());
    }
    let dir = state.settings.lock().unwrap().icons_dir.clone();
    let bytes = std::fs::read(Path::new(&dir).join(format!("{class}.png"))).map_err(|e| e.to_string())?;
    Ok(Response::new(bytes))
}

/// Largest map.json the app will read (the real one is about 90 KB).
const MAP_JSON_MAX: u64 = 8 << 20;

/// Reads and checks `<icons>/map/map.json`.
fn read_map_info(icons_dir: &str) -> Result<fb_model::world::MapInfo, String> {
    let path = Path::new(icons_dir).join("map").join("map.json");
    let len = std::fs::metadata(&path).map_err(|e| format!("The map is not extracted yet ({e})."))?.len();
    if len > MAP_JSON_MAX {
        return Err("map.json is too large.".into());
    }
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let info: fb_model::world::MapInfo = serde_json::from_str(&text).map_err(|e| format!("map.json is not valid: {e}"))?;
    let finite = |v: f64| v.is_finite();
    if !(finite(info.x_min) && finite(info.y_min) && finite(info.size) && info.size > 0.0) {
        return Err("map.json has bad world bounds.".into());
    }
    if !(1..=8192).contains(&info.tile_px) || !(1..=10).contains(&info.levels) {
        return Err("map.json has a bad tile size or level count.".into());
    }
    if info.nodes.len() > 100_000 || info.nodes.iter().any(|n| !(finite(n.x) && finite(n.y) && finite(n.z))) {
        return Err("map.json has bad resource sites.".into());
    }
    Ok(info)
}

/// `<icons>/map/<z>/<x>_<y>.png`, built from the three integers only. Level z
/// is a 2^z x 2^z grid, and z must be below the map's level count.
fn map_tile_path(icons_dir: &Path, levels: u32, z: u32, x: u32, y: u32) -> Result<PathBuf, String> {
    if z >= levels || z >= 31 {
        return Err("bad map level".into());
    }
    let n = 1u32 << z;
    if x >= n || y >= n {
        return Err("bad map tile".into());
    }
    Ok(icons_dir.join("map").join(z.to_string()).join(format!("{x}_{y}.png")))
}

/// The extracted world map: bounds and resource sites.
#[tauri::command]
fn map_info(state: State<AppState>) -> CmdResult<fb_model::world::MapInfo> {
    state.log("map_info");
    let dir = state.settings.lock().unwrap().icons_dir.clone();
    read_map_info(&dir)
}

/// PNG bytes of one map tile, sent as raw binary.
#[tauri::command]
fn map_tile(state: State<AppState>, z: u32, x: u32, y: u32) -> CmdResult<Response> {
    state.log("map_tile");
    let dir = state.settings.lock().unwrap().icons_dir.clone();
    let levels = read_map_info(&dir)?.levels;
    let path = map_tile_path(Path::new(&dir), levels, z, x, y)?;
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    Ok(Response::new(bytes))
}

/// Largest sprites.json the app will read (the real one is about 10 KB).
const SPRITES_JSON_MAX: u64 = 1 << 20;

/// A sprite image name as the helper writes them: `[A-Za-z0-9_]+\.png`, so
/// it can't name anything outside `<icons>/map/sprites`.
fn valid_sprite_file(name: &str) -> bool {
    match name.strip_suffix(".png") {
        Some(stem) => !stem.is_empty() && stem.len() <= 128 && stem.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'),
        None => false,
    }
}

/// Reads and checks `<icons>/map/sprites/sprites.json` (top-down renders of
/// the buildings, belts and pipes).
fn read_sprites(icons_dir: &str) -> Result<fb_model::world::Sprites, String> {
    if icons_dir.is_empty() {
        return Err("The map sprites are not extracted yet.".into());
    }
    let path = Path::new(icons_dir).join("map").join("sprites").join("sprites.json");
    let len = std::fs::metadata(&path).map_err(|e| format!("The map sprites are not extracted yet ({e})."))?.len();
    if len > SPRITES_JSON_MAX {
        return Err("sprites.json is too large.".into());
    }
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let s: fb_model::world::Sprites = serde_json::from_str(&text).map_err(|e| format!("sprites.json is not valid: {e}"))?;
    let ok = |v: f32| v.is_finite() && v.abs() <= 1.0e6;
    if !(s.cm_per_px.is_finite() && s.cm_per_px > 0.0 && s.cm_per_px <= 100.0) {
        return Err("sprites.json has a bad resolution.".into());
    }
    if s.buildings.len() > 2000 || s.strips.len() > 2000 {
        return Err("sprites.json lists too many sprites.".into());
    }
    for (class, b) in &s.buildings {
        let sane = valid_sprite_file(&b.file) && [b.min_x, b.min_y, b.max_x, b.max_y].into_iter().all(ok) && b.min_x < b.max_x && b.min_y < b.max_y;
        if !sane {
            return Err(format!("sprites.json has a bad entry for {class}."));
        }
    }
    for (class, t) in &s.strips {
        let sane = valid_sprite_file(&t.file) && ok(t.length_cm) && ok(t.width_cm) && t.length_cm > 0.0 && t.width_cm > 0.0;
        if !sane {
            return Err(format!("sprites.json has a bad entry for {class}."));
        }
    }
    Ok(s)
}

/// `<icons>/map/sprites/<file>`, only for a well-formed name that sprites.json lists.
fn sprite_path(icons_dir: &Path, sprites: &fb_model::world::Sprites, file: &str) -> Result<PathBuf, String> {
    let listed = sprites.buildings.values().any(|b| b.file == file) || sprites.strips.values().any(|t| t.file == file);
    if !valid_sprite_file(file) || !listed {
        return Err("bad sprite name".into());
    }
    Ok(icons_dir.join("map").join("sprites").join(file))
}

/// Where each building, belt and pipe sprite is and how it is placed.
#[tauri::command]
fn map_sprites(state: State<AppState>) -> CmdResult<fb_model::world::Sprites> {
    state.log("map_sprites");
    let dir = state.settings.lock().unwrap().icons_dir.clone();
    read_sprites(&dir)
}

/// PNG bytes of one sprite listed in sprites.json, sent as raw binary.
#[tauri::command]
fn map_sprite(state: State<AppState>, file: String) -> CmdResult<Response> {
    state.log("map_sprite");
    let dir = state.settings.lock().unwrap().icons_dir.clone();
    let sprites = read_sprites(&dir)?;
    let path = sprite_path(Path::new(&dir), &sprites, &file)?;
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    Ok(Response::new(bytes))
}

/// A file from `<icons>/ui`: the game's Open Sans faces and UI icon PNGs.
#[tauri::command]
fn ui_asset(state: State<AppState>, name: String) -> CmdResult<Response> {
    state.log("ui_asset");
    let ok_name = !name.is_empty()
        && !name.starts_with('.')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
    if !ok_name {
        return Err("bad asset name".into());
    }
    let dir = state.settings.lock().unwrap().icons_dir.clone();
    let bytes = std::fs::read(Path::new(&dir).join("ui").join(&name)).map_err(|e| e.to_string())?;
    Ok(Response::new(bytes))
}

#[tauri::command]
async fn solve(state: State<'_, AppState>, request: SolveRequest) -> CmdResult<SolveOutcome> {
    state.log("solve");
    let game = state.game.lock().unwrap().clone().ok_or("Game data is not loaded.")?;
    Ok(fb_engine::solver::solve(&game, &request))
}

#[tauri::command]
fn list_projects(state: State<AppState>) -> CmdResult<Vec<String>> {
    state.log("list_projects");
    let mut names: Vec<String> = std::fs::read_dir(projects_dir(&state)?)
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().to_string_lossy().strip_suffix(".fbproj.json").map(str::to_string))
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    Ok(names)
}

#[tauri::command]
fn save_project(state: State<AppState>, name: String, json: String) -> CmdResult<()> {
    state.log("save_project");
    let path = project_file(&state, &name)?;
    // Write to a temp file then rename, so a crash never leaves half a project.
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

#[tauri::command]
fn load_project(state: State<AppState>, name: String) -> CmdResult<String> {
    state.log("load_project");
    std::fs::read_to_string(project_file(&state, &name)?).map_err(|e| e.to_string())
}

#[tauri::command]
fn delete_project(state: State<AppState>, name: String) -> CmdResult<()> {
    state.log("delete_project");
    std::fs::remove_file(project_file(&state, &name)?).map_err(|e| e.to_string())
}

// --------------------------------------------------------------------- files

#[tauri::command]
async fn pick_open_file(app: AppHandle) -> CmdResult<Option<String>> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        files::pick_open(&app, &state.allowed)
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn pick_save_file(app: AppHandle, kind: files::Kind, suggested: String) -> CmdResult<Option<String>> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        files::pick_save(&app, &state.allowed, kind, &suggested)
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn read_factory_file(state: State<AppState>, path: String) -> CmdResult<String> {
    state.log("read_factory_file");
    let recent = state.settings.lock().unwrap().recent.clone();
    files::read_factory(&state.allowed, &recent, &path)
}

#[tauri::command]
fn write_file(state: State<AppState>, path: String, kind: files::Kind, text: Option<String>, base64: Option<String>) -> CmdResult<()> {
    state.log("write_file");
    files::write(&state.allowed, &path, kind, text, base64)
}

/// The webview is the app's window, not a browser: its own shortcuts (Ctrl+F
/// find bar, Ctrl+P print, F5 / Ctrl+R reload, Ctrl+plus/minus page zoom,
/// F12 devtools) are turned off. The keys still reach the app (Ctrl+F is its
/// own Find). WebView2 setting AreBrowserAcceleratorKeysEnabled; Tauri doesn't
/// expose it, so it's set on the native webview.
fn desktop_keys(window: &tauri::WebviewWindow) {
    #[cfg(windows)]
    let _ = window.with_webview(|wv| {
        use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Settings3;
        use windows::core::Interface;
        // SAFETY: plain COM calls on the webview's own controller, on the
        // thread Tauri runs this closure on.
        unsafe {
            let Ok(core) = wv.controller().CoreWebView2() else { return };
            let Ok(settings) = core.Settings() else { return };
            // Ctrl+wheel and Ctrl+plus/minus would zoom the page under egui.
            let _ = settings.SetIsZoomControlEnabled(false);
            if let Ok(s3) = settings.cast::<ICoreWebView2Settings3>() {
                let _ = s3.SetAreBrowserAcceleratorKeysEnabled(false);
                if std::env::var_os("FB_SELFTEST").is_some() {
                    let mut on = windows::core::BOOL::default();
                    let _ = s3.AreBrowserAcceleratorKeysEnabled(&mut on);
                    selftest::note("browser_accelerator_keys", on.as_bool());
                }
            }
        }
    });
}

/// The .fbfactory or .fbproject passed on the command line, once.
#[tauri::command]
fn startup_file(state: State<AppState>) -> Option<String> {
    state.startup_file.lock().unwrap().take()
}

// --------------------------------------------------------------------- setup

/// Reads the game's data from `dir` (without changing settings) so setup can
/// show what it found.
#[tauri::command]
async fn check_game(state: State<'_, AppState>, dir: String) -> CmdResult<fb_model::setup::GameCheck> {
    state.log("check_game");
    let dir = dir.trim().trim_matches('"').to_string();
    tauri::async_runtime::spawn_blocking(move || {
        let path = PathBuf::from(&dir);
        if dir.is_empty() || !path.is_absolute() {
            return Err("Pick the folder Satisfactory is installed in.".to_string());
        }
        if !setup::is_game_dir(&path) {
            return Err("That folder doesn't look like Satisfactory (there's no CommunityResources\\Docs\\en-US.json in it). Pick the folder that has FactoryGame.exe.".into());
        }
        let g = fb_engine::docs::load_game_data(&path).map_err(|e| format!("The game's data couldn't be read: {e}"))?;
        Ok(fb_model::setup::GameCheck { dir, recipes: g.recipes.len(), items: g.items.len(), buildings: g.buildings.len() })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn detect_game_dirs(state: State<AppState>) -> Vec<String> {
    state.log("detect_game_dirs");
    setup::detect_game_dirs()
}

#[tauri::command]
async fn pick_folder(app: AppHandle, title: String, start: Option<String>) -> CmdResult<Option<String>> {
    tauri::async_runtime::spawn_blocking(move || {
        use tauri_plugin_dialog::DialogExt;
        let mut d = app.dialog().file().set_title(title);
        if let Some(s) = start.filter(|s| Path::new(s).is_dir()) {
            d = d.set_directory(s);
        }
        d.blocking_pick_folder().and_then(|f| f.into_path().ok()).map(|p| p.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| e.to_string())
}

/// Starts extracting icons and the font from the game in settings.
#[tauri::command]
fn extract_start(app: AppHandle, state: State<AppState>) -> CmdResult<()> {
    state.log("extract_start");
    let (game_dir, icons_dir) = {
        let s = state.settings.lock().unwrap();
        (PathBuf::from(&s.game_dir), PathBuf::from(&s.icons_dir))
    };
    if !setup::is_game_dir(&game_dir) {
        return Err("Pick your Satisfactory folder first.".into());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let (helper, shim) = install::helper_paths(&exe)
        .ok_or("The icon helper is missing (tools\\IconExtractor.exe next to Factory Builder). Reinstall Factory Builder to get it back.")?;
    let handle = app.clone();
    setup::start_extract(&helper, &shim, &game_dir, &icons_dir, state.extract.clone(), move |ok| {
        if ok {
            let state = handle.state::<AppState>();
            let mut s = state.settings.lock().unwrap();
            s.icons_stamp = setup::game_stamp(Path::new(&s.game_dir));
            let _ = save_settings(&state.data_dir, &s);
        }
    })
}

#[tauri::command]
fn extract_status(state: State<AppState>) -> fb_model::setup::ExtractProgress {
    state.extract.lock().unwrap().clone()
}

/// The folder an install into `chosen` would use.
#[tauri::command]
fn install_dir_for(state: State<AppState>, chosen: String) -> String {
    state.identity.resolve_dir(Path::new(&chosen)).to_string_lossy().into_owned()
}

#[tauri::command]
async fn install_run(state: State<'_, AppState>, dir: String, desktop: bool) -> CmdResult<fb_model::setup::InstallReport> {
    state.log("install_run");
    let identity = state.identity.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        identity.install(Path::new(&dir), &exe, desktop)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Starts the installed copy and closes this one.
#[tauri::command]
fn install_restart(app: AppHandle, state: State<AppState>) -> CmdResult<()> {
    state.log("install_restart");
    state.identity.start_installed()?;
    app.exit(0);
    Ok(())
}

fn test_profile_dir() -> PathBuf {
    std::env::temp_dir().join(TEST_PROFILE_DIR)
}

/// Development: removes the test install and profile, then starts this build
/// in a fresh test profile, as on a PC that has never run Factory Builder.
#[tauri::command]
fn dev_test_first_launch(state: State<AppState>) -> CmdResult<()> {
    state.log("dev_test_first_launch");
    if !DEV {
        return Err("Only in development builds.".into());
    }
    let profile = test_profile_dir();
    let test = install::Identity::new(Some(profile.clone()), install::user_places());
    if test.installed().is_some() {
        test.uninstall().map_err(|e| format!("Couldn't remove the old test install first: {e}"))?;
    }
    if profile.exists() {
        std::fs::remove_dir_all(&profile).map_err(|e| format!("Couldn't clear the test profile (is a test copy still open?): {e}"))?;
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    std::process::Command::new(exe)
        .arg("--test-profile")
        .arg(&profile)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Development: removes "Factory Builder (Test)" and its profile.
#[tauri::command]
fn dev_remove_test_install(state: State<AppState>) -> CmdResult<String> {
    state.log("dev_remove_test_install");
    if !DEV {
        return Err("Only in development builds.".into());
    }
    if state.identity.is_test() {
        return Err("This is the test copy. Close it, then remove it from a normal launch.".into());
    }
    let profile = test_profile_dir();
    let test = install::Identity::new(Some(profile.clone()), install::user_places());
    let mut said = Vec::new();
    if test.installed().is_some() {
        test.uninstall().map_err(|e| format!("Couldn't remove the test install: {e}"))?;
        said.push("Removed the test install.");
    }
    if profile.exists() {
        std::fs::remove_dir_all(&profile).map_err(|e| format!("Couldn't clear the test profile (is a test copy still open?): {e}"))?;
        said.push("Cleared the test profile.");
    }
    Ok(if said.is_empty() { "There was no test install.".into() } else { said.join(" ") })
}

// --------------------------------------------------------------------- saves

#[tauri::command]
async fn list_saves(state: State<'_, AppState>) -> CmdResult<Vec<fb_model::saves::SaveSummary>> {
    state.log("list_saves");
    tauri::async_runtime::spawn_blocking(saves::list).await.map_err(|e| e.to_string())
}

/// Native Open dialog for a save file somewhere else.
#[tauri::command]
async fn pick_save_game(app: AppHandle) -> CmdResult<Option<String>> {
    tauri::async_runtime::spawn_blocking(move || {
        use tauri_plugin_dialog::DialogExt;
        let mut d = app.dialog().file().set_title("Open a Satisfactory save").add_filter("Satisfactory saves", &["sav"]);
        if let Some(root) = saves::saves_root().filter(|r| r.is_dir()) {
            d = d.set_directory(root);
        }
        let path = d.blocking_pick_file().and_then(|f| f.into_path().ok())?;
        app.state::<AppState>().allowed.allow(&path);
        Some(path.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn read_save_unlocks(app: AppHandle, path: String) -> CmdResult<fb_model::saves::SaveUnlocks> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        state.log("read_save_unlocks");
        saves::unlocks(&state.allowed, &path)
    })
    .await
    .map_err(|e| e.to_string())?
}

// ----------------------------------------------------------------- world map

/// The player's saves grouped by session, newest first.
#[tauri::command]
async fn world_sessions(app: AppHandle) -> CmdResult<Vec<fb_model::world::Session>> {
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<AppState>().log("world_sessions");
        world::sessions()
    })
    .await
    .map_err(|e| e.to_string())
}

/// What a save has built (machines, belts and pipes, item flow).
#[tauri::command]
async fn world_load(app: AppHandle, path: String) -> CmdResult<fb_model::world::World> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        state.log("world_load");
        world::load_for(&state, &path)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The newest save of a session (headers only), so the window can follow it.
#[tauri::command]
async fn world_latest(app: AppHandle, session: String) -> CmdResult<Option<fb_model::saves::SaveSummary>> {
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<AppState>().log("world_latest");
        world::latest(&session)
    })
    .await
    .map_err(|e| e.to_string())
}

// ------------------------------------------------------------- live map

/// The Live map add-on's settings, or why it can't run.
fn live_settings(state: &AppState) -> CmdResult<fb_model::LiveSettings> {
    let l = state.settings.lock().unwrap().live.clone();
    if !l.enabled {
        return Err("The Live map add-on is off (File › Settings).".into());
    }
    Ok(l)
}

fn live_error(f: live::Fail) -> String {
    match f {
        live::Fail::Untrusted(_) => "The server's certificate isn't the one you trusted. Check the server in Live map and confirm it again.".into(),
        live::Fail::Other(e) => e,
    }
}

/// Is the dedicated server there, is its certificate trusted, does it take
/// the token, and what's it running.
#[tauri::command]
async fn live_server_check(state: State<'_, AppState>) -> CmdResult<fb_model::ServerCheck> {
    state.log("live_server_check");
    let l = live_settings(&state)?;
    if l.server_host.trim().is_empty() {
        return Err("Enter the server's address first.".into());
    }
    let host = l.server_host.trim().to_string();
    let token = live::load_token(&state.data_dir);
    let mut out = fb_model::ServerCheck { token_set: token.is_some(), ..Default::default() };
    match live::call(&host, l.server_port, l.server_cert.as_deref(), None, "HealthCheck", serde_json::json!({ "ClientCustomData": "" })).await {
        Ok(v) => {
            out.reachable = true;
            out.health = live::field(&v, "Health").and_then(|h| h.as_str()).map(|h| h.chars().take(32).collect());
        }
        Err(live::Fail::Untrusted(fp)) => {
            out.reachable = true;
            out.untrusted_cert = fp.clone();
            *state.live_seen.lock().unwrap() = fp;
            return Ok(out);
        }
        Err(live::Fail::Other(e)) => {
            out.error = Some(e);
            return Ok(out);
        }
    }
    if let (Some(t), Some(pin)) = (token, l.server_cert.as_deref()) {
        match live::call(&host, l.server_port, Some(pin), Some(&t), "QueryServerState", serde_json::json!({})).await {
            Ok(v) => {
                let g = live::field(&v, "ServerGameState").cloned().unwrap_or_default();
                out.token_ok = Some(true);
                out.session = live::field(&g, "ActiveSessionName").and_then(|s| s.as_str()).map(|s| s.chars().take(64).collect());
                out.players = live::field(&g, "NumConnectedPlayers").and_then(|n| n.as_i64());
                out.player_limit = live::field(&g, "PlayerLimit").and_then(|n| n.as_i64());
            }
            Err(f) => {
                out.token_ok = Some(false);
                out.error = Some(live_error(f));
            }
        }
    }
    Ok(out)
}

/// Trusts the certificate the server just showed (and only that one).
#[tauri::command]
fn live_server_trust(state: State<AppState>, fingerprint: String) -> CmdResult<Status> {
    state.log("live_server_trust");
    let seen = state.live_seen.lock().unwrap().clone();
    if seen.as_deref() != Some(fingerprint.as_str()) {
        return Err("That isn't the certificate the server showed. Check the server again.".into());
    }
    let settings = {
        let mut s = state.settings.lock().unwrap();
        s.live.server_cert = Some(fingerprint);
        s.clone()
    };
    save_settings(&state.data_dir, &settings)?;
    *state.live_seen.lock().unwrap() = None;
    make_status(&state)
}

/// Keeps (or with None, forgets) the server's API token, encrypted.
#[tauri::command]
fn live_server_token(state: State<AppState>, token: Option<String>) -> CmdResult<()> {
    state.log("live_server_token");
    match token {
        Some(t) => live::save_token(&state.data_dir, t.trim()),
        None => {
            live::forget_token(&state.data_dir);
            Ok(())
        }
    }
}

/// The server's newest save (of the session it's running), downloaded
/// when it's changed. The Map tab then reads it like any save.
#[tauri::command]
async fn live_server_sync(state: State<'_, AppState>) -> CmdResult<Option<fb_model::saves::SaveSummary>> {
    state.log("live_server_sync");
    let l = live_settings(&state)?;
    let host = l.server_host.trim().to_string();
    let pin = l.server_cert.clone().ok_or("Check the server in Live map and trust its certificate first.")?;
    let token = live::load_token(&state.data_dir).ok_or("Add the server's API token in Live map first.")?;
    let sessions = live::call(&host, l.server_port, Some(&pin), Some(&token), "EnumerateSessions", serde_json::json!({})).await.map_err(live_error)?;
    let Some((name, played)) = live::newest_save(&sessions) else { return Ok(None) };
    let dir = state.data_dir.join("live");
    let path = dir.join("server.sav");
    let fresh = state.live_save.lock().unwrap().as_ref() == Some(&(name.clone(), played)) && path.exists();
    if !fresh {
        let bytes = live::download(&host, l.server_port, &pin, &token, &name).await.map_err(live_error)?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let tmp = dir.join("server.sav.part");
        std::fs::write(&tmp, &bytes).map_err(|e| format!("Couldn't keep the server's save: {e}"))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("Couldn't keep the server's save: {e}"))?;
        *state.live_save.lock().unwrap() = Some((name, played));
    }
    state.allowed.allow(&path);
    Ok(saves::summary_of(&path))
}

/// How long to go straight to FRM's web server once a dedicated server
/// said it has no `frm` function, before asking the server again.
const NO_FRM_RECHECK: std::time::Duration = std::time::Duration::from_secs(60);

/// One read endpoint of the Ficsit Remote Monitoring mod: through the
/// dedicated server when there is one (falling back to the mod's own web
/// server on the same machine when the server has no `frm` function),
/// otherwise its own web server.
async fn frm_get(state: &AppState, endpoint: &str) -> CmdResult<serde_json::Value> {
    let l = live_settings(state)?;
    if !l.frm {
        return Err("Ficsit Remote Monitoring isn't turned on in Live map.".into());
    }
    let host = l.server_host.trim();
    match l.server_cert.as_deref() {
        Some(pin) if !host.is_empty() => {
            let key = format!("{host}:{}", l.server_port);
            let known_without = state.live_no_frm.lock().unwrap().as_ref().is_some_and(|(k, at)| *k == key && at.elapsed() < NO_FRM_RECHECK);
            if !known_without {
                match live::frm_via_server(host, l.server_port, pin, endpoint).await.map_err(live_error)? {
                    Some(v) => {
                        *state.live_no_frm.lock().unwrap() = None;
                        return Ok(v);
                    }
                    None => *state.live_no_frm.lock().unwrap() = Some((key, std::time::Instant::now())),
                }
            }
            live::frm_direct(host, l.frm_port, endpoint).await.map_err(|e| {
                format!("The server doesn't pass requests on to Ficsit Remote Monitoring, and its web server didn't answer either: {e} If port {} is taken on the server, change the mod's Web Server Port and set the same port here.", l.frm_port)
            })
        }
        _ if !host.is_empty() => live::frm_direct(host, l.frm_port, endpoint).await,
        _ => live::frm_direct(l.frm_host.trim(), l.frm_port, endpoint).await,
    }
}

/// Players now, from the Ficsit Remote Monitoring mod.
#[tauri::command]
async fn live_players(state: State<'_, AppState>) -> CmdResult<Vec<fb_model::world::PlayerSpot>> {
    Ok(live::players(&frm_get(&state, "getPlayer").await?))
}

/// What's built now, from the Ficsit Remote Monitoring mod: machines,
/// extractors and generators; belts and pipes.
#[tauri::command]
async fn live_buildings(state: State<'_, AppState>) -> CmdResult<fb_model::LiveBuildings> {
    state.log("live_buildings");
    let mut out = fb_model::LiveBuildings::default();
    let mut machines = Vec::new();
    let mut ok = true;
    for ep in ["getFactory", "getExtractor", "getGenerators"] {
        match frm_get(&state, ep).await {
            Ok(v) => machines.extend(live::machines(&v)),
            Err(e) => {
                ok = false;
                out.errors.push(e);
                break;
            }
        }
    }
    out.machines = ok.then_some(machines);
    let mut carriers = Vec::new();
    let mut ok = true;
    for (ep, fluid) in [("getBelts", false), ("getPipes", true)] {
        match frm_get(&state, ep).await {
            Ok(v) => carriers.extend(live::carriers(&v, fluid)),
            Err(e) => {
                ok = false;
                out.errors.push(e);
                break;
            }
        }
    }
    out.carriers = ok.then_some(carriers);
    Ok(out)
}

/// Power and the map's smaller buildings now, from the Ficsit Remote
/// Monitoring mod: circuits, what every building draws or makes, power
/// lines, and poles, switches, storage (with what's in it), tanks,
/// splitters, mergers, junctions and pumps. Stops at the first endpoint
/// that fails (the mod isn't answering), keeping what came before.
#[tauri::command]
async fn live_power(state: State<'_, AppState>) -> CmdResult<fb_model::LivePower> {
    let mut out = fb_model::LivePower::default();
    let power = async {
        let circuits = frm_get(&state, "getPower").await?;
        let usage = frm_get(&state, "getPowerUsage").await?;
        let generators = frm_get(&state, "getGenerators").await?;
        Ok::<_, String>((live::circuits(&circuits), live::powered(&usage, &generators), live::structures(&usage, &[live::BATTERIES])))
    };
    match power.await {
        Ok((circuits, powered, batteries)) => {
            out.circuits = Some(circuits);
            out.powered = Some(powered);
            out.families.push(fb_model::LiveFamily { prefixes: vec![live::BATTERIES.to_string()], items: batteries });
        }
        Err(e) => {
            out.errors.push(e);
            return Ok(out);
        }
    }
    match frm_get(&state, "getCables").await {
        Ok(v) => out.lines = Some(live::lines(&v)),
        Err(e) => {
            out.errors.push(e);
            return Ok(out);
        }
    }
    for (ep, prefixes) in live::FAMILIES {
        match frm_get(&state, ep).await {
            Ok(v) => out.families.push(fb_model::LiveFamily { prefixes: prefixes.iter().map(|p| p.to_string()).collect(), items: live::structures(&v, prefixes) }),
            Err(e) => {
                out.errors.push(e);
                break;
            }
        }
    }
    Ok(out)
}

// ------------------------------------------------------------------ updates

/// A newer build of the app on GitHub, if there is one. Only packaged builds
/// running as the installed copy look (development builds don't update).
#[tauri::command]
async fn update_check(state: State<'_, AppState>) -> CmdResult<Option<fb_model::update::UpdateInfo>> {
    let Some(current) = update::build() else { return Ok(None) };
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    if !state.identity.is_installed_exe(&exe) || state.identity.is_test() {
        return Ok(None);
    }
    let found = update::latest(current).await?;
    let info = found.as_ref().map(|r| r.info.clone());
    *state.update.lock().unwrap() = found;
    Ok(info)
}

/// Installs the newer build found by `update_check`: downloads it, checks
/// its signature, copies it over this install and starts it; this copy then
/// exits.
#[tauri::command]
async fn update_apply(app: AppHandle, state: State<'_, AppState>) -> CmdResult<()> {
    let release = state.update.lock().unwrap().clone().ok_or("There's no update to install.")?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    if !state.identity.is_installed_exe(&exe) {
        return Err("Updates install over the installed copy, and this isn't it.".into());
    }
    let dir = exe.parent().ok_or("The app's folder isn't known.")?.to_path_buf();
    let work = state.data_dir.join("update");
    let new_app = update::fetch(&release, &work).await?;
    update::swap_in(&dir, &new_app)?;
    let _ = std::fs::remove_dir_all(&work);
    std::process::Command::new(dir.join(install::EXE_NAME))
        .args(state.identity.launch_args())
        .current_dir(&dir)
        .spawn()
        .map_err(|e| format!("The update is in, but it didn't start: {e}. Start Factory Builder again."))?;
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        app.exit(0);
    });
    Ok(())
}

// --------------------------------------------------------------------- rooms

/// Tells the window something arrived, so it fetches it right away instead
/// of waiting for its next poll.
fn room_notify(app: &AppHandle) -> room::Notify {
    let app = app.clone();
    std::sync::Arc::new(move || {
        use tauri::Emitter;
        let _ = app.emit("room-event", ());
    })
}

/// Opens a room on the configured TCP port and returns its code.
#[tauri::command]
async fn room_host(app: AppHandle, state: State<'_, AppState>, name: String) -> CmdResult<fb_model::room::HostInfo> {
    state.log("room_host");
    let port = state.settings.lock().unwrap().room_port;
    *state.room.lock().unwrap() = None;
    let host = room::Host::open(&name, port, room_notify(&app)).await?;
    let info = host.info.clone();
    *state.room.lock().unwrap() = Some(room::Room::Host(host));
    Ok(info)
}

#[tauri::command]
async fn room_join(app: AppHandle, state: State<'_, AppState>, name: String, code: String) -> CmdResult<()> {
    state.log("room_join");
    let addr = fb_model::room::decode_code(&code).ok_or("That room code isn't valid. It looks like XXXX-XXXX-XXXX-XXXX.")?;
    *state.room.lock().unwrap() = None;
    let guest = room::Guest::join(addr, &name, room_notify(&app)).await?;
    *state.room.lock().unwrap() = Some(room::Room::Guest(guest));
    Ok(())
}

#[tauri::command]
fn room_leave(state: State<AppState>) {
    state.log("room_leave");
    *state.room.lock().unwrap() = None;
}

#[tauri::command]
fn room_send(state: State<AppState>, wire: fb_model::room::Wire) {
    if let Some(r) = state.room.lock().unwrap().as_ref() {
        r.send(wire);
    }
}

#[tauri::command]
fn room_poll(state: State<AppState>) -> Vec<fb_model::room::RoomEvent> {
    state.room.lock().unwrap().as_ref().map(|r| r.drain()).unwrap_or_default()
}

/// The value after `--name` on the command line.
fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let selftest_out = std::env::var_os("FB_SELFTEST").map(PathBuf::from);
    // Test profiles are a development feature, and must be a full path.
    let test_profile = arg_value(&args, "--test-profile").map(PathBuf::from).filter(|p| p.is_absolute() && DEV);
    let identity = install::Identity::new(test_profile.clone(), install::user_places());

    if args.iter().any(|a| a == "--uninstall") {
        install::run_uninstaller(&identity, args.iter().any(|a| a == "--quiet"));
        return;
    }
    // After an update: the exe it replaced goes.
    update::cleanup(&std::env::current_exe().unwrap_or_default());
    // Hand off to the installed copy, so there's one app no matter which
    // copy was started. Development builds only do this in a test profile.
    if selftest_out.is_none() && (!DEV || identity.is_test()) && identity.hand_off(&args) {
        return;
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            let data_dir = match &test_profile {
                Some(p) => p.clone(),
                None => app.path().app_data_dir()?,
            };
            let settings = load_settings(&data_dir);
            let allowed = files::Allowed::default();
            // Self-test: the sample save it shows on the Map tab may be read.
            if let (Some(_), Some(save)) = (&selftest_out, std::env::var_os("FB_SELFTEST_SAVE")) {
                allowed.allow(Path::new(&save));
            }
            let startup = files::startup_file_from_args().map(|p| {
                allowed.allow(&p);
                p.to_string_lossy().into_owned()
            });
            app.manage(AppState {
                settings: Mutex::new(settings),
                game: Mutex::new(None),
                calls: Mutex::new(Vec::new()),
                room: Mutex::new(None),
                allowed,
                startup_file: Mutex::new(startup),
                data_dir,
                identity: identity.clone(),
                extract: Default::default(),
                live_seen: Mutex::new(None),
                live_save: Mutex::new(None),
                live_no_frm: Mutex::new(None),
                update: Mutex::new(None),
            });
            let mut window = tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::App("index.html".into()))
                .title(if test_profile.is_some() { "Factory Builder (Test)" } else { "Factory Builder" })
                .inner_size(1600.0, 960.0)
                .min_inner_size(900.0, 600.0)
                // The self-test runs the real UI in a window nobody sees.
                .visible(selftest_out.is_none());
            if selftest_out.is_some() {
                window = window.initialization_script(selftest::CAPTURE_JS);
            }
            let window = window.build()?;
            desktop_keys(&window);
            if let Some(out) = selftest_out.clone() {
                selftest::start(app.handle().clone(), out);
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            status,
            set_settings,
            live_server_check,
            live_server_trust,
            live_server_token,
            live_server_sync,
            live_players,
            live_buildings,
            live_power,
            update_check,
            update_apply,
            load_game,
            icon,
            ui_asset,
            map_info,
            map_tile,
            map_sprites,
            map_sprite,
            solve,
            list_projects,
            save_project,
            load_project,
            delete_project,
            pick_open_file,
            pick_save_file,
            read_factory_file,
            write_file,
            startup_file,
            check_game,
            detect_game_dirs,
            pick_folder,
            extract_start,
            extract_status,
            install_dir_for,
            install_run,
            install_restart,
            dev_test_first_launch,
            dev_remove_test_install,
            list_saves,
            pick_save_game,
            read_save_unlocks,
            world_sessions,
            world_load,
            world_latest,
            room_host,
            room_join,
            room_leave,
            room_send,
            room_poll,
            selftest::selftest_report,
            selftest::selftest_save,
            selftest::selftest_screenshot
        ])
        .run(tauri::generate_context!())
        .expect("error while running Factory Builder");
}

#[cfg(test)]
mod settings_tests {
    use super::*;

    #[test]
    fn the_window_cant_set_the_trusted_certificate() {
        let mut old = Settings::default();
        old.live.server_host = "10.0.0.5".into();
        old.live.server_cert = Some("AA:BB".into());
        let mut new = old.clone();
        new.live.server_cert = Some("EVIL".into());
        assert_eq!(keep_backend_owned(&old, new.clone()).live.server_cert.as_deref(), Some("AA:BB"));
        new.live.server_host = "10.0.0.6".into();
        assert_eq!(keep_backend_owned(&old, new).live.server_cert, None, "another server: confirm again");
    }
}

#[cfg(test)]
mod map_tests {
    use super::*;

    #[test]
    fn map_tile_path_validation() {
        let dir = Path::new("icons");
        let p = map_tile_path(dir, 4, 3, 7, 0).unwrap();
        assert_eq!(p, dir.join("map").join("3").join("7_0.png"));
        assert!(map_tile_path(dir, 4, 0, 0, 0).is_ok());
        // z at or past the level count, or past what a u32 shift can express
        assert!(map_tile_path(dir, 4, 4, 0, 0).is_err());
        assert!(map_tile_path(dir, 40, 32, 0, 0).is_err());
        // x or y outside the 2^z grid
        assert!(map_tile_path(dir, 4, 0, 1, 0).is_err());
        assert!(map_tile_path(dir, 4, 2, 4, 0).is_err());
        assert!(map_tile_path(dir, 4, 2, 0, 4).is_err());
        assert!(map_tile_path(dir, 4, 3, u32::MAX, 0).is_err());
    }

    #[test]
    fn map_info_is_validated() {
        let dir = std::env::temp_dir().join(format!("fb-map-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("map")).unwrap();
        let write = |s: &str| std::fs::write(dir.join("map").join("map.json"), s).unwrap();
        let d = dir.to_string_lossy().into_owned();
        write(r#"{"x_min":-1.5,"y_min":-2,"size":10,"tile_px":1024,"levels":4,"nodes":[{"id":"BP_ResourceNode1","kind":"Node","resource":"Desc_Coal_C","purity":"Pure","x":1,"y":2,"z":3},{"id":"G","kind":"Geyser","resource":null,"purity":"Normal","x":1,"y":2,"z":3}]}"#);
        assert_eq!(read_map_info(&d).unwrap().nodes.len(), 2);
        write(r#"{"x_min":0,"y_min":0,"size":0,"tile_px":1024,"levels":4,"nodes":[]}"#);
        assert!(read_map_info(&d).is_err());
        write(r#"{"x_min":0,"y_min":0,"size":1,"tile_px":1024,"levels":99,"nodes":[]}"#);
        assert!(read_map_info(&d).is_err());
        write("not json");
        assert!(read_map_info(&d).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sprite_names_are_strict() {
        for good in ["Build_ConstructorMk1_C.png", "a.png", "X_9.png"] {
            assert!(valid_sprite_file(good), "{good}");
        }
        let long = format!("{}.png", "a".repeat(129));
        for bad in ["", ".png", "a.PNG", "a.png.png", "../a.png", "..\\a.png", "a/b.png", "a\\b.png", "C:a.png", "a b.png", "a-b.png", "a.jpg", "a", "é.png", "a.png\0", long.as_str()] {
            assert!(!valid_sprite_file(bad), "{bad:?}");
        }
        let mut s = fb_model::world::Sprites { cm_per_px: 4.0, ..Default::default() };
        s.buildings.insert("Build_A_C".into(), fb_model::world::Sprite { file: "Build_A_C.png".into(), min_x: -1.0, min_y: -1.0, max_x: 1.0, max_y: 1.0 });
        s.strips.insert("Build_B_C".into(), fb_model::world::Strip { file: "Build_B_C.png".into(), length_cm: 200.0, width_cm: 184.0 });
        let dir = Path::new("icons");
        assert_eq!(sprite_path(dir, &s, "Build_A_C.png").unwrap(), dir.join("map").join("sprites").join("Build_A_C.png"));
        assert!(sprite_path(dir, &s, "Build_B_C.png").is_ok());
        // well formed but not listed, or not well formed
        assert!(sprite_path(dir, &s, "Build_C_C.png").is_err());
        assert!(sprite_path(dir, &s, "sprites.png").is_err());
        assert!(sprite_path(dir, &s, "../Build_A_C.png").is_err());
    }

    #[test]
    fn sprites_json_is_validated() {
        let dir = std::env::temp_dir().join(format!("fb-sprites-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("map").join("sprites")).unwrap();
        let file = dir.join("map").join("sprites").join("sprites.json");
        let write = |s: &str| std::fs::write(&file, s).unwrap();
        let d = dir.to_string_lossy().into_owned();
        assert!(read_sprites(&d).is_err(), "missing");
        assert!(read_sprites("").is_err(), "no icon folder");
        write(r#"{"cm_per_px":4.0,"buildings":{"Build_ConstructorMk1_C":{"file":"Build_ConstructorMk1_C.png","min_x":-452,"min_y":-504,"max_x":452,"max_y":472}},"strips":{"Build_ConveyorBeltMk1_C":{"file":"Build_ConveyorBeltMk1_C.png","length_cm":200,"width_cm":184}}}"#);
        let s = read_sprites(&d).unwrap();
        assert_eq!(s.buildings["Build_ConstructorMk1_C"].max_y, 472.0);
        assert_eq!(s.strips["Build_ConveyorBeltMk1_C"].width_cm, 184.0);
        for bad in [
            r#"{"cm_per_px":0,"buildings":{},"strips":{}}"#,
            r#"{"cm_per_px":4,"buildings":{"A":{"file":"../x.png","min_x":0,"min_y":0,"max_x":1,"max_y":1}},"strips":{}}"#,
            r#"{"cm_per_px":4,"buildings":{"A":{"file":"A.png","min_x":1,"min_y":0,"max_x":1,"max_y":1}},"strips":{}}"#,
            r#"{"cm_per_px":4,"buildings":{"A":{"file":"A.png","min_x":0,"min_y":0,"max_x":1e30,"max_y":1}},"strips":{}}"#,
            r#"{"cm_per_px":4,"buildings":{},"strips":{"B":{"file":"B.png","length_cm":0,"width_cm":10}}}"#,
            r#"{"cm_per_px":4,"buildings":{},"strips":{"B":{"file":"B.exe","length_cm":200,"width_cm":10}}}"#,
            "not json",
        ] {
            write(bad);
            assert!(read_sprites(&d).is_err(), "{bad}");
        }
        std::fs::write(&file, vec![b' '; (SPRITES_JSON_MAX + 1) as usize]).unwrap();
        assert!(read_sprites(&d).unwrap_err().contains("too large"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// With FB_TEST_ICONS (an icon folder the helper extracted into): the real
    /// sprites.json passes the checks and every image it lists is a PNG.
    #[test]
    fn real_sprites_are_valid() {
        let Ok(dir) = std::env::var("FB_TEST_ICONS") else { return };
        let s = read_sprites(&dir).unwrap();
        assert!(!s.buildings.is_empty() && !s.strips.is_empty());
        for file in s.buildings.values().map(|b| &b.file).chain(s.strips.values().map(|t| &t.file)) {
            let bytes = std::fs::read(sprite_path(Path::new(&dir), &s, file).unwrap()).unwrap();
            assert!(bytes.starts_with(b"\x89PNG"), "{file}");
        }
    }
}
