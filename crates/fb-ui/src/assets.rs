//! Images and fonts from the player's own game files, fetched lazily from
//! the backend's cache (written by tools/IconExtractor):
//! item/building icons (`<Class>.png`), the game's monochrome UI icons and
//! its Open Sans font (`ui/<name>`).

use crate::backend::{Backend, Msg};
use crate::theme;
use egui::{Color32, Image, Sense, Ui, Vec2};
use std::collections::HashMap;

/// The game's UI icons (white masks, tinted when drawn).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UiIcon {
    Add,
    Trash,
    Settings,
    Warning,
    Power,
    Factory,
    Close,
    Recycle,
    Search,
    Reset,
    Copy,
    Check,
    Caret,
    Arrow,
}

impl UiIcon {
    pub fn file(self) -> &'static str {
        match self {
            UiIcon::Add => "TXUI_AddTolist_32.png",
            UiIcon::Trash => "TXUI_MIcon_Trash.png",
            UiIcon::Settings => "TXUI_MIcon_Cogwheel.png",
            UiIcon::Warning => "TXUI_MIcon_Warning.png",
            UiIcon::Power => "TXUI_MIcon_Power.png",
            UiIcon::Factory => "TXUI_MIcon_Factory.png",
            UiIcon::Close => "TXUI_MIcon_Stop_X.png",
            UiIcon::Recycle => "TXUI_MIcon_Recycle.png",
            UiIcon::Search => "SearchIcon.png",
            UiIcon::Reset => "Reset_Icon.png",
            UiIcon::Copy => "TXUI_Icon_Copy.png",
            UiIcon::Check => "ficsit_checkmark_256.png",
            UiIcon::Caret => "Caret.png",
            UiIcon::Arrow => "TXUI_MIcon_Arrow_Right.png",
        }
    }
}

pub const FONT_FILES: [&str; 3] = [
    "OpenSans-Regular.ttf",
    "OpenSans-SemiBold.ttf",
    "OpenSans-Bold.ttf",
];

enum State {
    Requested,
    Ready(String),
    Missing,
}

pub struct Assets {
    backend: Backend,
    states: HashMap<String, State>,
    fonts: [Option<Vec<u8>>; 3],
    fonts_failed: bool,
    pub fonts_installed: bool,
}

impl Assets {
    pub fn new(backend: Backend) -> Self {
        let assets = Self {
            backend,
            states: HashMap::new(),
            fonts: [None, None, None],
            fonts_failed: false,
            fonts_installed: false,
        };
        assets.request_fonts();
        assets
    }

    fn request_fonts(&self) {
        for (i, f) in FONT_FILES.iter().enumerate() {
            self.backend
                .call_bytes("ui_asset", serde_json::json!({ "name": f }), move |r| {
                    Msg::Font(i, r)
                });
        }
    }

    /// Forget everything (after the icon folder changed) and fetch again.
    pub fn reload(&mut self, ctx: &egui::Context) {
        for s in self.states.values() {
            if let State::Ready(uri) = s {
                ctx.forget_image(uri);
            }
        }
        self.states.clear();
        if !self.fonts_installed {
            self.fonts = [None, None, None];
            self.fonts_failed = false;
            self.request_fonts();
        }
    }

    pub fn on_font(&mut self, ctx: &egui::Context, index: usize, result: Result<Vec<u8>, String>) {
        match result {
            Ok(bytes) => self.fonts[index] = Some(bytes),
            Err(_) => self.fonts_failed = true,
        }
        if self.fonts.iter().all(Option::is_some) && !self.fonts_installed {
            let [a, b, c] = std::mem::take(&mut self.fonts);
            theme::install_fonts(ctx, a.unwrap(), b.unwrap(), c.unwrap());
            self.fonts_installed = true;
        }
    }

    pub fn on_image(&mut self, ctx: &egui::Context, key: String, result: Result<Vec<u8>, String>) {
        let state = match result {
            Ok(bytes) => {
                let uri = format!("bytes://{key}");
                ctx.include_bytes(uri.clone(), bytes);
                State::Ready(uri)
            }
            Err(_) => State::Missing,
        };
        self.states.insert(key, state);
    }

    fn uri(&mut self, key: &str, cmd: &'static str, arg: serde_json::Value) -> Option<String> {
        match self.states.get(key) {
            Some(State::Ready(uri)) => Some(uri.clone()),
            Some(_) => None,
            None => {
                self.states.insert(key.to_string(), State::Requested);
                let k = key.to_string();
                self.backend.call_bytes(cmd, arg, move |r| Msg::Image(k, r));
                None
            }
        }
    }

    fn draw(ui: &mut Ui, uri: Option<String>, size: f32, tint: Option<Color32>) -> egui::Response {
        let size = Vec2::splat(size);
        match uri {
            Some(uri) => {
                let mut image = Image::new(uri).fit_to_exact_size(size);
                if let Some(t) = tint {
                    image = image.tint(t);
                }
                ui.add(image)
            }
            None => ui.allocate_exact_size(size, Sense::hover()).1,
        }
    }

    /// Item or building icon by class name (`Desc_IronIngot_C`).
    pub fn item(&mut self, ui: &mut Ui, class: &str, size: f32) -> egui::Response {
        let uri = self.uri(
            &format!("icon/{class}.png"),
            "icon",
            serde_json::json!({ "class": class }),
        );
        Self::draw(ui, uri, size, None)
    }

    /// One of the game's monochrome UI icons, tinted.
    pub fn ui(&mut self, ui: &mut Ui, icon: UiIcon, size: f32, tint: Color32) -> egui::Response {
        let uri = self.ui_uri(icon);
        Self::draw(ui, uri, size, Some(tint))
    }

    pub fn ui_uri(&mut self, icon: UiIcon) -> Option<String> {
        self.uri(
            &format!("ui/{}", icon.file()),
            "ui_asset",
            serde_json::json!({ "name": icon.file() }),
        )
    }

    /// Paints a UI icon into `rect` (for custom-drawn widgets).
    pub fn paint_ui(&mut self, ui: &Ui, icon: UiIcon, rect: egui::Rect, tint: Color32) {
        if let Some(uri) = self.ui_uri(icon) {
            Image::new(uri).tint(tint).paint_at(ui, rect);
        }
    }

    /// (requested, ready, missing), for diagnostics.
    pub fn counts(&self) -> (usize, usize, usize) {
        let mut c = (0, 0, 0);
        for s in self.states.values() {
            match s {
                State::Requested => c.0 += 1,
                State::Ready(_) => c.1 += 1,
                State::Missing => c.2 += 1,
            }
        }
        c
    }

    pub fn fonts_failed(&self) -> bool {
        self.fonts_failed
    }
}
