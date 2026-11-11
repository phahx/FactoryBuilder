//! Factory Builder UI: egui + egui_node_graph2, compiled to wasm and run
//! inside the Tauri webview.

pub mod app;
pub mod app_files;
pub mod assets;
pub mod backend;
pub mod dialogs;
pub mod extractors;
pub mod finder;
pub mod frames;
pub mod navigate;
pub mod live;
pub mod live_factory;
pub mod live_map;
pub mod demo;
pub mod storage;
pub mod map;
pub mod map_draw;
pub mod map_import;
pub mod fmt;
pub mod hub;
pub mod graph;
pub mod menu;
pub mod menus;
pub mod overview_extra;
pub mod panels;
pub mod portable;
pub mod progress_ui;
pub mod project;
pub mod project_ui;
pub mod power_net;
pub mod power_ui;
pub mod room;
pub mod routers;
pub mod room_extras;
pub mod setup;
pub mod theme;
pub mod widgets;
pub mod wires;

#[cfg(test)]
mod tests;

#[cfg(target_arch = "wasm32")]
mod web_entry {
    use wasm_bindgen::prelude::*;

    #[wasm_bindgen(start)]
    pub fn start() {
        wasm_bindgen_futures::spawn_local(async {
            let document = web_sys::window()
                .expect("no window")
                .document()
                .expect("no document");
            let canvas = document
                .get_element_by_id("app")
                .expect("index.html needs <canvas id=\"app\">")
                .dyn_into::<web_sys::HtmlCanvasElement>()
                .expect("#app is not a canvas");
            // Create the WebGL context ourselves first, keeping the drawing
            // buffer: eframe's later getContext("webgl2") returns this same
            // context, and finished frames stay readable for image export.
            let opts = js_sys::Object::new();
            let _ = js_sys::Reflect::set(&opts, &"preserveDrawingBuffer".into(), &true.into());
            let _ = canvas.get_context_with_context_options("webgl2", &opts);
            let result = eframe::WebRunner::new()
                .start(
                    canvas,
                    eframe::WebOptions::default(),
                    Box::new(|cc| Ok(Box::new(crate::app::FactoryApp::new(cc)))),
                )
                .await;
            if let Some(loading) = document.get_element_by_id("loading") {
                match result {
                    Ok(()) => loading.remove(),
                    Err(e) => loading
                        .set_text_content(Some(&format!("Factory Builder failed to start: {e:?}"))),
                }
            }
        });
    }
}
