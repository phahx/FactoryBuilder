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
//   <file.fbfactory>        open it
//   --uninstall [--quiet]   remove the installed copy (Apps & features runs this)
//   --test-profile <dir>    development: a throwaway profile in <dir> that
//                           installs as "Factory Builder (Test)"

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use fb_model::{GameData, Settings, SolveOutcome, SolveRequest, Status};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{ipc::Response, AppHandle, Manager, State};

mod files;
mod install;
mod room;
mod saves;
mod selftest;
mod setup;

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
    /// A .fbfactory given on the command line.
    startup_file: Mutex<Option<String>>,
    /// Settings, icon cache and autosave live here.
    data_dir: PathBuf,
    identity: install::Identity,
    extract: setup::Progress,
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
    let mut settings = settings;
    {
        let old = state.settings.lock().unwrap();
        // Owned by the backend: the window's copy may be stale.
        settings.icons_stamp = old.icons_stamp.clone();
        settings.icons_dir = old.icons_dir.clone();
    }
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

/// The .fbfactory passed on the command line, once.
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
            window.build()?;
            if let Some(out) = selftest_out.clone() {
                selftest::start(app.handle().clone(), out);
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            status,
            set_settings,
            load_game,
            icon,
            ui_asset,
            map_info,
            map_tile,
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
            room_host,
            room_join,
            room_leave,
            room_send,
            room_poll,
            selftest::selftest_report,
            selftest::selftest_screenshot
        ])
        .run(tauri::generate_context!())
        .expect("error while running Factory Builder");
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
}
