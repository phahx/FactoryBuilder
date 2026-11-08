//! Bridge from the egui UI to the Tauri backend.
//!
//! In the webview (wasm32) every call goes through
//! `window.__TAURI__.core.invoke` (enabled by `withGlobalTauri`) and its
//! result lands in an inbox that the app drains each frame. On native
//! targets (tests) a plain function stands in for the backend.

use fb_model::{GameData, SolveOutcome, Status};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::cell::RefCell;
use std::rc::Rc;

pub enum Msg {
    Status(Result<Status, String>),
    Game(Result<GameData, String>),
    /// An item icon or UI image, keyed like "icon/<Class>.png" / "ui/<file>".
    Image(String, Result<Vec<u8>, String>),
    /// One of the game's Open Sans faces (index into assets::FONT_FILES).
    Font(usize, Result<Vec<u8>, String>),
    Solved(Result<SolveOutcome, String>),
    Projects(Result<Vec<String>, String>),
    Saved {
        name: String,
        autosave: bool,
        result: Result<(), String>,
    },
    Loaded {
        name: String,
        result: Result<String, String>,
    },
    Deleted {
        name: String,
        result: Result<(), String>,
    },
    RoomHosted(Result<fb_model::room::HostInfo, String>),
    PickedOpen {
        purpose: crate::app_files::OpenPurpose,
        path: Result<Option<String>, String>,
    },
    FileRead {
        purpose: crate::app_files::OpenPurpose,
        path: String,
        result: Result<String, String>,
    },
    PickedSave {
        purpose: crate::app_files::SavePurpose,
        path: Result<Option<String>, String>,
    },
    FileWritten {
        purpose: crate::app_files::SavePurpose,
        path: String,
        result: Result<(), String>,
    },
    StartupFile(Result<Option<String>, String>),
    RoomJoined(Result<(), String>),
    RoomEvents(Result<Vec<fb_model::room::RoomEvent>, String>),
    // First-launch setup (setup.rs).
    GameDirs(Result<Vec<String>, String>),
    GameChecked {
        dir: String,
        result: Result<fb_model::setup::GameCheck, String>,
    },
    PickedFolder {
        purpose: crate::setup::FolderPurpose,
        path: Result<Option<String>, String>,
    },
    InstallDirFor(Result<String, String>),
    Installed(Result<fb_model::setup::InstallReport, String>),
    Restarted(Result<(), String>),
    ExtractStarted(Result<(), String>),
    Extract(Result<fb_model::setup::ExtractProgress, String>),
    /// A Developer settings action finished (its message).
    DevDone(Result<String, String>),
    // Recipes from a save (progress_ui.rs).
    Saves(Result<Vec<fb_model::saves::SaveSummary>, String>),
    PickedSaveGame(Result<Option<String>, String>),
    SaveUnlocks(Result<fb_model::saves::SaveUnlocks, String>),
    // World map (map.rs).
    MapInfo(Result<fb_model::world::MapInfo, String>),
    WorldSessions(Result<Vec<fb_model::world::Session>, String>),
    WorldLoaded {
        path: String,
        result: Result<fb_model::world::World, String>,
    },
    WorldLatest(Result<Option<fb_model::saves::SaveSummary>, String>),
    MapSprites(Result<fb_model::world::Sprites, String>),
    /// Self-test: the sample save to show on the Map tab.
    SelftestSave(Result<Option<String>, String>),
}

#[cfg(not(target_arch = "wasm32"))]
pub type NativeHandler = Rc<dyn Fn(&str, Value) -> Result<Value, String>>;

#[derive(Clone)]
pub struct Backend {
    inbox: Rc<RefCell<Vec<Msg>>>,
    ctx: egui::Context,
    /// Set by the backend's "room-event" push: room messages are waiting.
    room_wake: Rc<std::cell::Cell<bool>>,
    #[cfg(not(target_arch = "wasm32"))]
    handler: NativeHandler,
}

impl Backend {
    #[cfg(target_arch = "wasm32")]
    pub fn new(ctx: egui::Context) -> Self {
        let b = Self {
            inbox: Default::default(),
            ctx,
            room_wake: Default::default(),
        };
        // The backend emits "room-event" whenever a room message arrives.
        let (wake, ctx) = (b.room_wake.clone(), b.ctx.clone());
        web::listen("room-event", move || {
            wake.set(true);
            ctx.request_repaint();
        });
        b
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn native(ctx: egui::Context, handler: NativeHandler) -> Self {
        Self {
            inbox: Default::default(),
            ctx,
            room_wake: Default::default(),
            handler,
        }
    }

    pub fn ctx(&self) -> egui::Context {
        self.ctx.clone()
    }

    /// True once after the backend said room messages are waiting.
    pub fn take_room_wake(&self) -> bool {
        self.room_wake.replace(false)
    }

    pub fn drain(&self) -> Vec<Msg> {
        std::mem::take(&mut *self.inbox.borrow_mut())
    }

    /// Calls `cmd` with JSON `args`; the decoded result is wrapped into a
    /// [`Msg`] and delivered on a later frame.
    pub fn call<T: DeserializeOwned + 'static>(
        &self,
        cmd: &'static str,
        args: Value,
        wrap: impl FnOnce(Result<T, String>) -> Msg + 'static,
    ) {
        let inbox = self.inbox.clone();
        let ctx = self.ctx.clone();
        let deliver = move |r: Result<Value, String>| {
            let decoded = r.and_then(|v| {
                serde_json::from_value::<T>(v).map_err(|e| format!("{cmd}: bad reply: {e}"))
            });
            inbox.borrow_mut().push(wrap(decoded));
            ctx.request_repaint();
        };
        #[cfg(target_arch = "wasm32")]
        wasm_bindgen_futures::spawn_local(async move {
            deliver(web::invoke_json(cmd, &args).await);
        });
        #[cfg(not(target_arch = "wasm32"))]
        deliver((self.handler)(cmd, args));
    }

    /// Like [`Backend::call`] for commands that answer with raw bytes.
    pub fn call_bytes(
        &self,
        cmd: &'static str,
        args: Value,
        wrap: impl FnOnce(Result<Vec<u8>, String>) -> Msg + 'static,
    ) {
        let inbox = self.inbox.clone();
        let ctx = self.ctx.clone();
        let deliver = move |r: Result<Vec<u8>, String>| {
            inbox.borrow_mut().push(wrap(r));
            ctx.request_repaint();
        };
        #[cfg(target_arch = "wasm32")]
        wasm_bindgen_futures::spawn_local(async move {
            deliver(web::invoke_bytes(cmd, &args).await);
        });
        #[cfg(not(target_arch = "wasm32"))]
        deliver(
            (self.handler)(cmd, args)
                .and_then(|v| serde_json::from_value::<Vec<u8>>(v).map_err(|e| e.to_string())),
        );
    }
}

#[cfg(target_arch = "wasm32")]
mod web {
    use serde::Serialize;
    use serde_json::Value;
    use wasm_bindgen::prelude::*;

    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "core"], js_name = invoke, catch)]
        async fn tauri_invoke(cmd: &str, args: JsValue) -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "event"], js_name = listen)]
        fn tauri_listen(event: &str, handler: &Closure<dyn FnMut(JsValue)>) -> js_sys::Promise;
    }

    /// Calls `f` every time the backend emits `event` (for the app's lifetime).
    pub fn listen(event: &str, mut f: impl FnMut() + 'static) {
        let cb = Closure::<dyn FnMut(JsValue)>::new(move |_| f());
        let _ = tauri_listen(event, &cb);
        cb.forget();
    }

    fn js_error(e: JsValue) -> String {
        e.as_string().unwrap_or_else(|| format!("{e:?}"))
    }

    async fn invoke(cmd: &str, args: &Value) -> Result<JsValue, String> {
        // json_compatible: maps become plain JS objects, which is what Tauri expects.
        let ser = serde_wasm_bindgen::Serializer::json_compatible();
        let args = args.serialize(&ser).map_err(|e| e.to_string())?;
        tauri_invoke(cmd, args).await.map_err(js_error)
    }

    pub async fn invoke_json(cmd: &str, args: &Value) -> Result<Value, String> {
        let v = invoke(cmd, args).await?;
        if v.is_undefined() || v.is_null() {
            return Ok(Value::Null);
        }
        serde_wasm_bindgen::from_value(v).map_err(|e| e.to_string())
    }

    pub async fn invoke_bytes(cmd: &str, args: &Value) -> Result<Vec<u8>, String> {
        let v = invoke(cmd, args).await?;
        // tauri::ipc::Response arrives as an ArrayBuffer.
        if v.is_instance_of::<js_sys::ArrayBuffer>() {
            Ok(js_sys::Uint8Array::new(&v).to_vec())
        } else if v.is_instance_of::<js_sys::Uint8Array>() {
            Ok(js_sys::Uint8Array::from(v).to_vec())
        } else {
            serde_wasm_bindgen::from_value::<Vec<u8>>(v).map_err(|e| e.to_string())
        }
    }
}
