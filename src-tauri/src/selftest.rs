//! Hidden-window self-test: `FB_SELFTEST=<report.json> factory-builder.exe`
//!
//! Runs the real wasm UI in an invisible window, waits for it to start, then
//! checks the IPC bridge from JavaScript and writes a JSON report with:
//! which commands the wasm UI itself called, console errors and panics, and
//! the results of direct invoke() calls. Then the app exits. Nothing is
//! shown on screen and no input focus is taken.

use crate::AppState;
use std::path::PathBuf;
use std::time::Duration;
use tauri::{AppHandle, Manager, State};

/// Installed before any page script: records console errors and uncaught
/// exceptions (wasm panics land in console.error via eframe's handler).
pub const CAPTURE_JS: &str = r#"
window.__fbErrors = [];
window.__fbSelftest = true;
(function () {
  const orig = console.error.bind(console);
  console.error = function (...args) { window.__fbErrors.push(args.map(String).join(' ')); orig(...args); };
  window.addEventListener('error', (e) => window.__fbErrors.push('error: ' + e.message));
  window.addEventListener('unhandledrejection', (e) => window.__fbErrors.push('rejection: ' + e.reason));
})();
"#;

const PROBE_JS: &str = r#"
(async () => {
  const r = {};
  const inv = window.__TAURI__.core.invoke;
  const canvas = document.getElementById('app');
  r.loading_gone = !document.getElementById('loading');
  r.canvas = canvas ? [canvas.width, canvas.height] : null;
  r.raf = await new Promise((res) => { let done = false; requestAnimationFrame(() => { done = true; res(true); }); setTimeout(() => { if (!done) res(false); }, 2000); });
  try { const s = await inv('status'); r.icons_found = s.icons_found; r.game_dir = s.settings.game_dir; } catch (e) { r.status_err = String(e); }
  try { const g = await inv('load_game'); r.recipes = Object.keys(g.recipes).length; r.items = Object.keys(g.items).length; } catch (e) { r.game_err = String(e); }
  try { const b = await inv('icon', { class: 'Desc_IronIngot_C' }); r.icon_type = Object.prototype.toString.call(b); r.icon_bytes = b.byteLength; } catch (e) { r.icon_err = String(e); }
  try {
    const req = { targets: [{ item: 'Desc_IronPlate_C', rate: 60 }], inputs: [], enabled_recipes: ['Recipe_IngotIron_C', 'Recipe_IronPlate_C'], resource_limits: {}, objective: 'Resources' };
    const s = await inv('solve', { request: req });
    r.solve = s.Solved ? { nodes: s.Solved.nodes.length, power_mw: s.Solved.power_mw } : s;
  } catch (e) { r.solve_err = String(e); }
  r.ui = window.__fbSummary ? JSON.parse(window.__fbSummary) : null;
  // Screenshots: switch the UI into each view, poke egui into repainting,
  // then read the canvas in a later animation frame (before it clears).
  const sleep = (ms) => new Promise((res) => setTimeout(res, ms));
  r.screenshots = [];
  for (const view of ['graph', 'zoomed', 'finder', 'finder_search', 'menu', 'room_cursors', 'room_info', 'setup_install', 'setup_game', 'setup_icons', 'setup_name', 'settings_dev', 'map', 'map_zoom', 'map_close', 'map_import', 'export']) {
    try {
      window.__fbSelftestView = view;
      for (let i = 0; i < 6; i++) {
        canvas.dispatchEvent(new PointerEvent('pointermove', { clientX: 1590, clientY: 950 - i, bubbles: true }));
        await sleep(120);
      }
      // A fresh event right before reading makes egui paint in the next
      // frame; read in that frame and the one after, keep the non-blank one.
      canvas.dispatchEvent(new PointerEvent('pointermove', { clientX: 1580, clientY: 940, bubbles: true }));
      const shots = await new Promise((res) => requestAnimationFrame(() => {
        const a = canvas.toDataURL('image/png');
        requestAnimationFrame(() => res([a, canvas.toDataURL('image/png')]));
      }));
      const png = shots[0].length >= shots[1].length ? shots[0] : shots[1];
      await inv('selftest_screenshot', { dataUrl: png, name: view });
      r.screenshots.push(view);
    } catch (e) { r.screenshot_err = String(e); }
  }
  await sleep(3000);
  // Hover tips: put the pointer on something, wait past the tooltip delay,
  // then read (no further pointer events, so the tip stays up).
  const hovers = [
    ['zoomed', 'tip_product', 1400, 202], ['zoomed', 'tip_building', 1400, 327],
    ['zoomed', 'tip_raw', 1400, 636], ['zoomed', 'tip_extractor', 1440, 664],
    ['zoomed', 'tip_node_icon', 903, 398], ['zoomed', 'tip_node_title', 760, 407],
    ['zoomed', 'tip_footer', 703, 690], ['zoomed', 'tip_port', 697, 553],
    ['zoomed', 'tip_wire_label', 625, 610], ['zoomed', 'tip_target', 90, 164],
    ['map_zoom', 'tip_map_node', 853, 251],
    ['map_close', 'tip_map_machine', 540, 300], ['map_close', 'tip_map_splitter', 463, 145],
  ];
  const hit = document.elementFromPoint(900, 400);
  r.hit_test = hit ? (hit.tagName + '#' + hit.id) : null;
  r.viewport = [window.innerWidth, window.innerHeight, document.visibilityState];
  for (const [view, name, x, y] of hovers) {
    try {
      window.__fbSelftestView = view;
      await sleep(400);
      // eframe listens for mousemove (not pointermove).
      canvas.dispatchEvent(new MouseEvent('mousemove', { clientX: x, clientY: y, bubbles: true }));
      await sleep(1400);
      const png = await new Promise((res) => requestAnimationFrame(() => res(canvas.toDataURL('image/png'))));
      await inv('selftest_screenshot', { dataUrl: png, name });
      r.screenshots.push(name);
    } catch (e) { r.screenshot_err = String(e); }
  }
  r.errors = window.__fbErrors;
  await inv('selftest_report', { report: JSON.stringify(r) });
})();
"#;

pub fn start(app: AppHandle, out: PathBuf) {
    std::thread::spawn(move || {
        // Give the wasm UI time to load, run frames and request icons.
        std::thread::sleep(Duration::from_secs(10));
        if let Some(w) = app.get_webview_window("main") {
            if let Err(e) = w.eval(PROBE_JS) {
                write(&app, &out, &format!("{{\"eval_error\":{:?}}}", e.to_string()));
            }
        }
        // Safety net if the probe never reports back.
        std::thread::sleep(Duration::from_secs(45));
        write(&app, &out, "{\"timeout\":true}");
    });
}

fn write(app: &AppHandle, out: &PathBuf, probe: &str) {
    let calls = app.state::<AppState>().calls.lock().unwrap().clone();
    let report = format!("{{\"calls_before_probe\":{},\"probe\":{}}}", serde_json::to_string(&calls).unwrap(), probe);
    let _ = std::fs::write(out, report);
    app.exit(0);
}

/// Self-test only: a save to show on the Map tab (`FB_SELFTEST_SAVE`), so
/// the map can be checked with a real 1.x save without touching the game's
/// own save folder. None outside the self-test.
#[tauri::command]
pub fn selftest_save() -> Option<String> {
    std::env::var_os("FB_SELFTEST")?;
    std::env::var("FB_SELFTEST_SAVE").ok()
}

#[tauri::command]
pub fn selftest_screenshot(data_url: String, name: String) -> Result<(), String> {
    let Ok(out) = std::env::var("FB_SELFTEST") else { return Ok(()) };
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err("bad name".into());
    }
    let b64 = data_url.split_once(',').map(|x| x.1).ok_or("not a data URL")?;
    std::fs::write(PathBuf::from(out).with_extension(format!("{name}.b64")), b64).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn selftest_report(app: AppHandle, state: State<AppState>, report: String) {
    // Calls made by the probe itself come after the UI's own.
    let ui_calls: Vec<String> = {
        let calls = state.calls.lock().unwrap();
        let n = calls.len().saturating_sub(4); // status, load_game, icon, solve from the probe
        calls[..n].to_vec()
    };
    let Ok(out) = std::env::var("FB_SELFTEST") else { return };
    let text = format!("{{\"calls_before_probe\":{},\"probe\":{}}}", serde_json::to_string(&ui_calls).unwrap(), report);
    let _ = std::fs::write(out, text);
    app.exit(0);
}
