//! Headless UI tests: the real app code runs frames in an egui Context with
//! no window, against the real game data and solver behind a native stand-in
//! for the Tauri backend.

use crate::app::FactoryApp;
use crate::backend::Backend;
use fb_model::*;
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

fn game_dir() -> String {
    std::env::var("SATISFACTORY_DIR")
        .unwrap_or_else(|_| r"C:\Program Files (x86)\Steam\steamapps\common\Satisfactory".into())
}

type RoomHook = Rc<dyn Fn(&str, &Value) -> Option<Result<Value, String>>>;

fn harness() -> Option<(
    egui::Context,
    FactoryApp,
    Rc<RefCell<HashMap<String, String>>>,
)> {
    harness_with(None)
}

fn harness_with(
    room: Option<RoomHook>,
) -> Option<(
    egui::Context,
    FactoryApp,
    Rc<RefCell<HashMap<String, String>>>,
)> {
    let game = fb_engine::docs::load_game_data(std::path::Path::new(&game_dir())).ok()?;
    let projects: Rc<RefCell<HashMap<String, String>>> = Default::default();
    let store = projects.clone();
    let ctx = egui::Context::default();
    let handler = Rc::new(move |cmd: &str, args: Value| -> Result<Value, String> {
        if let Some(hook) = &room {
            if let Some(r) = hook(cmd, &args) {
                return r;
            }
        }
        match cmd {
            "set_settings" => {
                let settings: Settings = serde_json::from_value(args["settings"].clone()).map_err(|e| e.to_string())?;
                Ok(serde_json::to_value(Status { icons_found: 747, ui_assets_found: 17, settings, ..Default::default() }).unwrap())
            }
            // Set up, with icons present, so setup and notices stay closed.
            "status" => Ok(serde_json::to_value(Status {
                icons_found: 747,
                ui_assets_found: 17,
                settings: Settings { setup_done: true, ..Default::default() },
                ..Default::default()
            })
            .unwrap()),
            "ui_asset" => Err("no ui assets in tests".into()),
            "load_game" => Ok(serde_json::to_value(&game).unwrap()),
            "solve" => {
                let req: SolveRequest =
                    serde_json::from_value(args["request"].clone()).map_err(|e| e.to_string())?;
                Ok(serde_json::to_value(fb_engine::solver::solve(&game, &req)).unwrap())
            }
            "list_projects" => Ok(json!(store.borrow().keys().cloned().collect::<Vec<_>>())),
            "save_project" => {
                store.borrow_mut().insert(
                    args["name"].as_str().unwrap().into(),
                    args["json"].as_str().unwrap().into(),
                );
                Ok(Value::Null)
            }
            "load_project" => store
                .borrow()
                .get(args["name"].as_str().unwrap())
                .map(|s| json!(s))
                .ok_or("missing".into()),
            "icon" => Err("no icons in tests".into()),
            "startup_file" => Ok(Value::Null),
            "write_file" => {
                let path = args["path"].as_str().unwrap().to_string();
                store.borrow_mut().insert(path, args["text"].as_str().unwrap_or_default().to_string());
                Ok(Value::Null)
            }
            "read_factory_file" => store
                .borrow()
                .get(args["path"].as_str().unwrap())
                .map(|s| json!(s))
                .ok_or("missing".into()),
            other => Err(format!("unexpected command {other}")),
        }
    });
    let backend = Backend::native(ctx.clone(), handler);
    let app = FactoryApp::with_backend(&ctx, backend);
    Some((ctx, app, projects))
}

fn frames(ctx: &egui::Context, app: &mut FactoryApp, n: usize) {
    for _ in 0..n {
        frame_with(ctx, app, Vec::new());
    }
}

fn frame_with(ctx: &egui::Context, app: &mut FactoryApp, events: Vec<egui::Event>) {
    crate::widgets::TEST_BUTTONS.with(|b| b.borrow_mut().clear());
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1600.0, 960.0),
        )),
        events,
        focused: true,
        ..Default::default()
    };
    let _ = ctx.run(input, |ctx| app.ui(ctx));
}

#[test]
fn plan_builds_a_balanced_graph() {
    let Some((ctx, mut app, _)) = harness() else {
        eprintln!("Satisfactory not found; skipping");
        return;
    };
    frames(&ctx, &mut app, 2);
    app.planner_mut().targets = vec![Target {
        item: "Desc_IronPlate_C".into(),
        rate: 60.0,
    }];
    app.press_plan();
    frames(&ctx, &mut app, 3);
    // ore -> smelters -> constructors -> output
    assert_eq!(app.node_count(), 4, "nodes");
    assert!(
        (app.min_efficiency() - 1.0).abs() < 1e-6,
        "every machine at 100%: {}",
        app.min_efficiency()
    );
    let outs = app.outputs();
    assert_eq!(outs.len(), 1);
    assert!((outs[0].1 - 60.0).abs() < 1e-6, "delivered {:?}", outs);
    assert!((app.power() - 24.0).abs() < 1e-6, "power {}", app.power());
}

#[test]
fn plan_with_byproducts_and_alternates_stays_balanced() {
    let Some((ctx, mut app, _)) = harness() else {
        return;
    };
    frames(&ctx, &mut app, 2);
    app.planner_mut().targets = vec![
        Target {
            item: "Desc_ModularFrameHeavy_C".into(),
            rate: 4.0,
        },
        Target {
            item: "Desc_Plastic_C".into(),
            rate: 30.0,
        },
    ];
    let all: Vec<String> = {
        let g = fb_engine::docs::load_game_data(std::path::Path::new(&game_dir())).unwrap();
        g.recipes.keys().cloned().collect()
    };
    app.planner_mut().enabled = all.into_iter().collect();
    app.press_plan();
    frames(&ctx, &mut app, 1); // solve reply arrives; graph is built
    let t = std::time::Instant::now();
    frames(&ctx, &mut app, 1); // first frame simulates the new graph
    let first = t.elapsed();
    let t = std::time::Instant::now();
    frames(&ctx, &mut app, 10); // cached
    eprintln!(
        "{} nodes: first frame {:?}, cached frame {:?}",
        app.node_count(),
        first,
        t.elapsed() / 10
    );
    assert!(app.node_count() > 5);
    for (item, got, want) in app.outputs() {
        assert!((got - want).abs() < 1e-4, "{item}: got {got}, want {want}");
    }
    assert!(
        app.min_efficiency() > 0.999,
        "min efficiency {}",
        app.min_efficiency()
    );
}

#[test]
fn projects_round_trip() {
    let Some((ctx, mut app, store)) = harness() else {
        return;
    };
    frames(&ctx, &mut app, 2);
    app.planner_mut().targets = vec![Target {
        item: "Desc_IronRod_C".into(),
        rate: 15.0,
    }];
    app.press_plan();
    frames(&ctx, &mut app, 2);
    let n = app.node_count();
    let path = "C:/Factories/Rods.fbfactory";
    app.save_as(path);
    frames(&ctx, &mut app, 2);
    let saved = store.borrow().get(path).cloned().expect("written");
    assert!(saved.contains("\"format\": \"factory-builder\""), "saved in the portable format");
    assert!(!app.is_dirty());
    assert_eq!(app.title(), "Rods");
    assert_eq!(app.recent(), vec![path.to_string()], "added to Open Recent");
    app.new_project();
    frames(&ctx, &mut app, 1);
    assert_eq!(app.node_count(), 0);
    app.open(path);
    frames(&ctx, &mut app, 3);
    assert_eq!(app.node_count(), n);
    assert!((app.outputs()[0].1 - 15.0).abs() < 1e-6);
    assert_eq!(app.title(), "Rods");
    assert!(!app.is_dirty(), "a freshly opened file has no unsaved changes");
}

#[test]
fn undo_redo_and_unsaved_guard() {
    let Some((ctx, mut app, _)) = harness() else {
        return;
    };
    frames(&ctx, &mut app, 2);
    assert!(!app.is_dirty(), "fresh app is clean");
    app.planner_mut().targets = vec![Target {
        item: "Desc_IronPlate_C".into(),
        rate: 60.0,
    }];
    app.press_plan();
    frames(&ctx, &mut app, 3);
    assert_eq!(app.node_count(), 4);
    assert!(app.is_dirty());
    assert!(app.undo_steps() >= 1);

    app.undo();
    frames(&ctx, &mut app, 1);
    assert_eq!(app.node_count(), 0, "undo removes the planned graph");
    app.redo();
    frames(&ctx, &mut app, 1);
    assert_eq!(app.node_count(), 4, "redo brings it back");

    // New with unsaved changes asks first instead of discarding.
    app.guard(crate::dialogs::Pending::New);
    assert!(matches!(
        app.dialog(),
        crate::dialogs::Dialog::Unsaved { .. }
    ));
    assert_eq!(app.node_count(), 4);
}

#[test]
fn duplicate_and_delete_selection() {
    let Some((ctx, mut app, _)) = harness() else {
        return;
    };
    frames(&ctx, &mut app, 2);
    app.planner_mut().targets = vec![Target {
        item: "Desc_IronPlate_C".into(),
        rate: 60.0,
    }];
    app.press_plan();
    frames(&ctx, &mut app, 3);
    app.select_all();
    app.duplicate_selected();
    frames(&ctx, &mut app, 2);
    assert_eq!(app.node_count(), 8, "duplicated all four nodes");
    // The copies are wired like the originals, so the copy also delivers 60.
    let delivered: f64 = app.outputs().iter().map(|o| o.1).sum();
    assert!((delivered - 120.0).abs() < 1e-3, "delivered {delivered}");
    app.delete_selected();
    frames(&ctx, &mut app, 2);
    assert_eq!(app.node_count(), 4, "deleted the selected copies");
    app.undo();
    frames(&ctx, &mut app, 1);
    assert_eq!(app.node_count(), 8, "delete is undoable");
}

#[test]
fn side_panels_keep_their_width() {
    let Some((ctx, mut app, _)) = harness() else {
        return;
    };
    app.planner_mut().targets = vec![
        Target {
            item: "Desc_ModularFrameHeavy_C".into(),
            rate: 4.0,
        },
        Target {
            item: "Desc_Plastic_C".into(),
            rate: 30.0,
        },
    ];
    frames(&ctx, &mut app, 40);
    for (id, default) in [("planner", 340.0), ("overview", 300.0)] {
        let w = egui::containers::panel::PanelState::load(&ctx, egui::Id::new(id))
            .map(|p| p.rect.width())
            .unwrap_or(0.0);
        assert!(
            w > 0.0 && w <= default + 1.0,
            "{id} panel is {w} px wide after 40 frames (default {default})"
        );
    }
}

#[test]
fn nodes_are_sized_to_their_content() {
    let Some((ctx, mut app, _)) = harness() else {
        return;
    };
    frames(&ctx, &mut app, 2);
    app.planner_mut().targets = vec![Target {
        item: "Desc_ModularFrameHeavy_C".into(),
        rate: 4.0,
    }];
    app.press_plan();
    frames(&ctx, &mut app, 5);
    let tallest = app.tallest_node();
    // The biggest machine here has 4 inputs: a few hundred px, not the 4000 px canvas cap.
    assert!(
        tallest > 100.0 && tallest < 600.0,
        "tallest node is {tallest} px"
    );
}

#[test]
fn nodes_drag_by_their_title_text() {
    let Some((ctx, mut app, _)) = harness() else {
        return;
    };
    frames(&ctx, &mut app, 2);
    app.planner_mut().targets = vec![Target {
        item: "Desc_IronPlate_C".into(),
        rate: 60.0,
    }];
    app.press_plan();
    frames(&ctx, &mut app, 6);
    let (id, rect, before) = app.first_node().expect("a node was drawn");
    // Grab the title text (left part of the header), drag 60 px right.
    let start = rect.left_top() + egui::vec2(30.0, 12.0);
    let end = start + egui::vec2(60.0, 0.0);
    let button = |pressed, pos| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    };
    frame_with(&ctx, &mut app, vec![egui::Event::PointerMoved(start)]);
    frame_with(&ctx, &mut app, vec![button(true, start)]);
    for k in 1..=6 {
        let p = start + egui::vec2(10.0 * k as f32, 0.0);
        frame_with(&ctx, &mut app, vec![egui::Event::PointerMoved(p)]);
    }
    frame_with(&ctx, &mut app, vec![button(false, end)]);
    frames(&ctx, &mut app, 1);
    let moved = app.node_pos(id) - before;
    assert!(
        moved.x > 30.0,
        "dragging the title text moved the node by {moved:?}"
    );
}

#[test]
fn arranged_nodes_do_not_overlap() {
    let Some((ctx, mut app, _)) = harness() else {
        return;
    };
    frames(&ctx, &mut app, 2);
    app.planner_mut().targets = vec![
        Target {
            item: "Desc_ModularFrameHeavy_C".into(),
            rate: 4.0,
        },
        Target {
            item: "Desc_Plastic_C".into(),
            rate: 30.0,
        },
    ];
    let g = fb_engine::docs::load_game_data(std::path::Path::new(&game_dir())).unwrap();
    app.planner_mut().enabled = g.recipes.keys().cloned().collect();
    app.press_plan();
    frames(&ctx, &mut app, 8);
    let overlaps = app.overlapping_nodes();
    assert!(
        overlaps.is_empty(),
        "{} overlapping node pairs",
        overlaps.len()
    );
}

#[test]
fn add_node_menu_searches_and_adds() {
    let Some((ctx, mut app, _)) = harness() else {
        return;
    };
    frames(&ctx, &mut app, 3);
    let at = egui::pos2(800.0, 480.0); // empty canvas
    let button = |pressed| egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Secondary,
        pressed,
        modifiers: Default::default(),
    };
    frame_with(&ctx, &mut app, vec![egui::Event::PointerMoved(at)]);
    frame_with(&ctx, &mut app, vec![button(true)]);
    frame_with(&ctx, &mut app, vec![button(false)]);
    frames(&ctx, &mut app, 2);
    assert!(
        app.finder_open(),
        "right-click on empty canvas opens Add node"
    );
    frame_with(
        &ctx,
        &mut app,
        vec![egui::Event::Text("reinforced iron plate".into())],
    );
    frames(&ctx, &mut app, 1);
    let enter = egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Default::default(),
    };
    frame_with(&ctx, &mut app, vec![enter]);
    frames(&ctx, &mut app, 2);
    assert!(!app.finder_open(), "menu closes after adding");
    assert_eq!(
        app.node_labels(),
        vec!["Reinforced Iron Plate".to_string()],
        "the best match (standard recipe) was added"
    );
}

#[test]
fn node_colours_survive_save_and_load() {
    let Some((ctx, mut app, _)) = harness() else {
        return;
    };
    frames(&ctx, &mut app, 2);
    app.planner_mut().targets = vec![Target {
        item: "Desc_IronRod_C".into(),
        rate: 15.0,
    }];
    app.press_plan();
    frames(&ctx, &mut app, 3);
    app.set_color_all(Some([0xFA, 0x95, 0x49]));
    frames(&ctx, &mut app, 1);
    app.save_as("C:/Factories/Coloured.fbfactory");
    frames(&ctx, &mut app, 2);
    app.new_project();
    frames(&ctx, &mut app, 1);
    app.open("C:/Factories/Coloured.fbfactory");
    frames(&ctx, &mut app, 3);
    let colors = app.colors();
    assert!(
        !colors.is_empty() && colors.iter().all(|c| *c == Some([0xFA, 0x95, 0x49])),
        "{colors:?}"
    );
}

fn drag(ctx: &egui::Context, app: &mut FactoryApp, from: egui::Pos2, to: egui::Pos2) {
    let button = |pressed, pos| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    };
    frame_with(ctx, app, vec![egui::Event::PointerMoved(from)]);
    frame_with(ctx, app, vec![button(true, from)]);
    for k in 1..=8 {
        frame_with(
            ctx,
            app,
            vec![egui::Event::PointerMoved(
                from + (to - from) * (k as f32 / 8.0),
            )],
        );
    }
    frame_with(ctx, app, vec![button(false, to)]);
    frames(ctx, app, 2);
}

#[test]
fn each_output_bubble_pulls_its_own_wire() {
    use egui_node_graph2::AnyParameterId;
    let Some((ctx, mut app, _)) = harness() else {
        return;
    };
    frames(&ctx, &mut app, 2);
    // Reinforced Iron Plate: the smelters' ingots feed both plates and rods,
    // so one output carries two wires.
    app.planner_mut().targets = vec![Target {
        item: "Desc_IronPlateReinforced_C".into(),
        rate: 5.0,
    }];
    app.press_plan();
    frames(&ctx, &mut app, 8);
    let g = app.graph();
    let (output, wires): (_, Vec<_>) = g
        .outputs
        .keys()
        .map(|o| (o, g.output_connections(o)))
        .find(|(_, w)| w.len() >= 2)
        .expect("an output with two wires");
    let hooks = app.port_hooks[&AnyParameterId::Output(output)].clone();
    assert_eq!(
        hooks.len(),
        wires.len() + 1,
        "one bubble per wire plus a free ring"
    );
    // Pull the second bubble's wire off and drop it on empty canvas.
    drag(
        &ctx,
        &mut app,
        hooks[1],
        egui::pos2(hooks[1].x + 40.0, 920.0),
    );
    let g = app.graph();
    let still = g.output_connections(output);
    assert_eq!(still, vec![wires[0]], "only the second wire came out");
}

#[test]
fn input_bubble_pulls_its_own_wire() {
    use egui_node_graph2::AnyParameterId;
    let Some((ctx, mut app, _)) = harness() else {
        return;
    };
    frames(&ctx, &mut app, 2);
    app.planner_mut().targets = vec![Target {
        item: "Desc_IronPlateReinforced_C".into(),
        rate: 5.0,
    }];
    app.press_plan();
    frames(&ctx, &mut app, 8);
    // Find an input fed by two outputs, if the plan has one; otherwise
    // dragging the single wire must still take exactly that wire.
    let g = app.graph();
    let (input, outs) = g
        .iter_connection_groups()
        .max_by_key(|(_, o)| o.len())
        .expect("a connected input");
    let hooks = app.port_hooks[&AnyParameterId::Input(input)].clone();
    assert_eq!(hooks.len(), outs.len() + 1);
    let last = outs.len() - 1;
    drag(
        &ctx,
        &mut app,
        hooks[last],
        egui::pos2(hooks[last].x - 40.0, 920.0),
    );
    let remaining = app.graph().connections(input);
    let mut expected = outs.clone();
    expected.remove(last);
    assert_eq!(remaining, expected, "only the dragged wire came out");
}

fn click(ctx: &egui::Context, app: &mut FactoryApp, at: egui::Pos2, button: egui::PointerButton) {
    let ev = |pressed| egui::Event::PointerButton {
        pos: at,
        button,
        pressed,
        modifiers: Default::default(),
    };
    frame_with(ctx, app, vec![egui::Event::PointerMoved(at)]);
    frame_with(ctx, app, vec![ev(true)]);
    frame_with(ctx, app, vec![ev(false)]);
    frames(ctx, app, 2);
}

#[test]
fn node_menu_delete_removes_the_node() {
    let Some((ctx, mut app, _)) = harness() else {
        return;
    };
    frames(&ctx, &mut app, 2);
    app.planner_mut().targets = vec![Target { item: "Desc_IronPlate_C".into(), rate: 60.0 }];
    app.press_plan();
    frames(&ctx, &mut app, 8);
    let n = app.node_count();
    let (id, rect, _) = app.first_node().unwrap();
    click(&ctx, &mut app, rect.center(), egui::PointerButton::Secondary);
    // The menu opens at the click; "Delete" is its second row.
    let menu = ctx.memory(|m| m.area_rect(egui::Id::new("node_menu"))).expect("node menu open");
    let row = ctx.style().spacing.interact_size.y + ctx.style().spacing.item_spacing.y;
    let delete = egui::pos2(menu.center().x, menu.top() + ctx.style().spacing.window_margin.top + row * 1.5);
    click(&ctx, &mut app, delete, egui::PointerButton::Primary);
    assert!(!app.graph().nodes.contains_key(id), "deleted");
    assert_eq!(app.node_count(), n - 1);
    assert!(app.context_menu.is_none(), "menu closed");
}

#[test]
fn colour_chip_in_node_menu_colours_the_node() {
    let Some((ctx, mut app, _)) = harness() else {
        return;
    };
    frames(&ctx, &mut app, 2);
    app.planner_mut().targets = vec![Target {
        item: "Desc_IronPlate_C".into(),
        rate: 60.0,
    }];
    app.press_plan();
    frames(&ctx, &mut app, 8);
    let (id, rect, _) = app.first_node().unwrap();
    // Right-click the node body, then click the first game colour chip.
    click(
        &ctx,
        &mut app,
        rect.center(),
        egui::PointerButton::Secondary,
    );
    assert_eq!(app.menu_swatches.len(), 6, "no colour + 5 game presets");
    let chip = app.menu_swatches[1].center();
    click(&ctx, &mut app, chip, egui::PointerButton::Primary);
    let color = app.graph()[id].user_data.color;
    assert_eq!(
        color,
        Some(crate::app::GAME_COLORS[0].1),
        "clicking the chip coloured the node"
    );
}

#[test]
fn plan_orders_wires_top_to_bottom() {
    let Some((ctx, mut app, _)) = harness() else {
        return;
    };
    frames(&ctx, &mut app, 2);
    app.planner_mut().targets = vec![
        Target {
            item: "Desc_ModularFrameHeavy_C".into(),
            rate: 4.0,
        },
        Target {
            item: "Desc_Plastic_C".into(),
            rate: 30.0,
        },
    ];
    let g = fb_engine::docs::load_game_data(std::path::Path::new(&game_dir())).unwrap();
    app.planner_mut().enabled = g.recipes.keys().cloned().collect();
    app.press_plan();
    frames(&ctx, &mut app, 8);
    let g = app.graph();
    for o in g.outputs.keys() {
        let ys: Vec<f32> = g
            .output_connections(o)
            .iter()
            .map(|i| app.node_y(g[*i].node))
            .collect();
        assert!(
            ys.windows(2).all(|w| w[0] <= w[1]),
            "output wires not top-to-bottom: {ys:?}"
        );
    }
    for (input, outs) in g.iter_connection_groups() {
        let ys: Vec<f32> = outs.iter().map(|o| app.node_y(g[*o].node)).collect();
        assert!(
            ys.windows(2).all(|w| w[0] <= w[1]),
            "input {input:?} wires not top-to-bottom: {ys:?}"
        );
    }
}

#[test]
fn connecting_by_hand_keeps_existing_wire_order() {
    use crate::graph::Template;
    use egui_node_graph2::AnyParameterId;
    let Some((ctx, mut app, _)) = harness() else {
        return;
    };
    frames(&ctx, &mut app, 2);
    app.planner_mut().targets = vec![Target {
        item: "Desc_IronPlateReinforced_C".into(),
        rate: 5.0,
    }];
    app.press_plan();
    frames(&ctx, &mut app, 8);
    let g = app.graph();
    let output = g
        .outputs
        .keys()
        .find(|o| g.output_connections(*o).len() >= 2)
        .expect("an output with two wires");
    let item = g[output].typ.0.clone();
    let before = g.output_connections(output);
    // A sink for the same item, placed below everything.
    // Sinks go in the visible canvas, bottom right, clear of the plan.
    let at = app.screen_to_graph(egui::pos2(1150.0, 760.0));
    let sink = app.add_test_node(Template::Sink(item.clone()), at);
    frames(&ctx, &mut app, 3);
    let sink_in = app.graph()[sink].inputs[0].1;
    let ring = *app.port_hooks[&AnyParameterId::Output(output)]
        .last()
        .unwrap();
    let sink_ring = app.port_hooks[&AnyParameterId::Input(sink_in)][0];
    drag(&ctx, &mut app, ring, sink_ring);
    let after = app.graph().output_connections(output);
    let mut expected = before.clone();
    expected.push(sink_in);
    assert_eq!(
        after, expected,
        "existing wires stayed on their bubbles; the new one went last"
    );

    // Dropping onto the first filled bubble puts a new wire there.
    let at2 = app.screen_to_graph(egui::pos2(1150.0, 860.0));
    let sink2 = app.add_test_node(Template::Sink(item), at2);
    frames(&ctx, &mut app, 3);
    let sink2_in = app.graph()[sink2].inputs[0].1;
    let sink2_ring = app.port_hooks[&AnyParameterId::Input(sink2_in)][0];
    let first_bubble = app.port_hooks[&AnyParameterId::Output(output)][0];
    drag(&ctx, &mut app, sink2_ring, first_bubble);
    let after2 = app.graph().output_connections(output);
    assert_eq!(after2[0], sink2_in, "dropped on bubble 0, so it went first");
    assert_eq!(&after2[1..], &expected[..], "the others kept their order");
}

/// In-memory stand-in for the backend's room relay (the socket version is
/// tested in src-tauri/src/room.rs), following the same rules: the host
/// stamps revisions and relays to everyone but the sender.
#[derive(Default)]
struct FakeNet {
    host: std::collections::VecDeque<room::RoomEvent>,
    guest: std::collections::VecDeque<room::RoomEvent>,
    doc: Option<String>,
    rev: u64,
}

fn test_code() -> String {
    room::encode_code(room::RoomAddress { local: "192.168.1.2".parse().unwrap(), public: Some("203.0.113.7".parse().unwrap()), port: 47860 })
}

fn room_hook(net: Rc<RefCell<FakeNet>>, is_host: bool) -> RoomHook {
    use room::{Peer, RoomEvent, Wire};
    let host_peer = Peer { id: 0, name: "Hoster".into(), color: [0xFA, 0x95, 0x49] };
    let guest_peer = Peer { id: 1, name: "Guesty".into(), color: [0x80, 0xC0, 0xF0] };
    Rc::new(move |cmd: &str, args: &Value| {
        let mut n = net.borrow_mut();
        Some(match cmd {
            "room_host" if is_host => Ok(serde_json::to_value(room::HostInfo {
                code: test_code(),
                local_ip: "192.168.1.2".into(),
                port: 47860,
                public_ip: Some("203.0.113.7".into()),
                public_source: Some("router".into()),
                forwarded: true,
                forward_problem: None,
                shared_address: false,
            })
            .unwrap()),
            "room_join" if !is_host => {
                let welcome = Wire::Welcome { you: 1, peers: vec![host_peer.clone(), guest_peer.clone()], doc: n.doc.clone(), rev: n.rev };
                n.guest.push_back(RoomEvent::Message(welcome));
                n.host.push_back(RoomEvent::Message(Wire::Joined { peer: guest_peer.clone() }));
                Ok(Value::Null)
            }
            "room_send" => {
                let wire: Wire = serde_json::from_value(args["wire"].clone()).unwrap();
                let me = if is_host { 0 } else { 1 };
                let out = match wire {
                    Wire::Cursor { x, y, .. } => Wire::Cursor { id: me, x, y },
                    Wire::Doc { json, .. } => {
                        n.rev += 1;
                        n.doc = Some(json.clone());
                        Wire::Doc { from: me, rev: n.rev, json }
                    }
                    other => other,
                };
                if is_host { n.guest.push_back(RoomEvent::Message(out)) } else { n.host.push_back(RoomEvent::Message(out)) }
                Ok(Value::Null)
            }
            "room_poll" => {
                let q = if is_host { &mut n.host } else { &mut n.guest };
                Ok(serde_json::to_value(q.drain(..).collect::<Vec<_>>()).unwrap())
            }
            "room_leave" => Ok(Value::Null),
            _ => return None,
        })
    })
}

#[test]
fn room_shares_the_factory_and_cursors() {
    let net: Rc<RefCell<FakeNet>> = Default::default();
    let Some((hctx, mut host, _)) = harness_with(Some(room_hook(net.clone(), true))) else { return };
    let (gctx, mut guest, _) = harness_with(Some(room_hook(net.clone(), false))).unwrap();
    frames(&hctx, &mut host, 2);
    frames(&gctx, &mut guest, 2);

    // Host opens a room, sees the code and port-forwarding dialog, and plans.
    host.open_room("Hoster".into());
    frames(&hctx, &mut host, 2);
    assert!(matches!(host.dialog(), crate::dialogs::Dialog::RoomInfo { .. }), "host is shown the code and port forwarding");
    host.planner_mut().targets = vec![Target { item: "Desc_IronPlate_C".into(), rate: 60.0 }];
    host.press_plan();
    frames(&hctx, &mut host, 8);
    assert_eq!(host.node_count(), 4);

    // Guest joins and gets the host's factory.
    guest.join_room("Guesty".into(), test_code());
    frames(&gctx, &mut guest, 6);
    assert_eq!(guest.node_count(), 4, "guest received the host's factory");
    assert_eq!(guest.room.peers.len(), 2);
    frames(&hctx, &mut host, 3);
    assert!(host.room.peers.contains_key(&1), "host sees the guest");

    // Same layout on both, whatever each person's zoom.
    for id in host.graph().iter_nodes() {
        let (hp, gp) = (host.node_pos_unzoomed(id), guest.node_pos_unzoomed(id));
        assert!((hp - gp).length() < 0.5, "node {id:?}: host {hp:?} guest {gp:?}");
    }

    // A guest edit reaches the host.
    guest.select_all();
    guest.duplicate_selected();
    frames(&gctx, &mut guest, 3);
    frames(&hctx, &mut host, 4);
    assert_eq!(host.node_count(), 8, "host received the guest's edit");

    // The guest's cursor shows on the host's canvas, in graph coordinates.
    let at = egui::pos2(900.0, 500.0);
    frame_with(&gctx, &mut guest, vec![egui::Event::PointerMoved(at)]);
    frames(&gctx, &mut guest, 1);
    frames(&hctx, &mut host, 3);
    let seen = *host.room.cursors.get(&1).expect("host has the guest's cursor");
    let expected = guest.screen_to_graph_unzoomed(at);
    assert!((seen - expected).length() < 0.5, "cursor {seen:?} vs {expected:?}");
}

/// Structure of a graph that doesn't depend on ids: per node (kind, numbers,
/// colour), and wires as (from, output, to, input, slot) by node order.
fn shape(app: &FactoryApp) -> (Vec<String>, Vec<(usize, usize, usize, usize, usize)>) {
    let f = app.to_portable();
    let nodes = f.nodes.iter().map(|n| format!("{:?} {} {:?} {:?} {:?} {:?}", n.kind, n.class, n.machines, n.clock, n.rate, n.color)).collect();
    let mut wires: Vec<_> = f.wires.iter().map(|w| (w.from, w.output, w.to, w.input, w.slot)).collect();
    wires.sort();
    (nodes, wires)
}

fn plan_hmf(ctx: &egui::Context, app: &mut FactoryApp) {
    app.planner_mut().targets = vec![
        Target { item: "Desc_ModularFrameHeavy_C".into(), rate: 4.0 },
        Target { item: "Desc_Plastic_C".into(), rate: 30.0 },
    ];
    let g = fb_engine::docs::load_game_data(std::path::Path::new(&game_dir())).unwrap();
    app.planner_mut().enabled = g.recipes.keys().cloned().collect();
    app.press_plan();
    frames(ctx, app, 8);
}

#[test]
fn portable_format_keeps_everything() {
    let Some((ctx, mut app, _)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    plan_hmf(&ctx, &mut app);
    app.set_color_all(Some([0x5F, 0x66, 0x8C]));
    frames(&ctx, &mut app, 1);
    let before = shape(&app);
    let outputs_before = app.outputs();
    let json = app.to_portable().to_json(true);

    let (ctx2, mut other, _) = harness().unwrap();
    frames(&ctx2, &mut other, 2);
    let f = crate::portable::FactoryFile::parse(&json).unwrap();
    other.open_factory(&ctx2, f, None);
    frames(&ctx2, &mut other, 4);
    assert_eq!(shape(&other), before, "nodes, numbers, colours, wires and bubble order survive");
    assert_eq!(other.planner_mut().enabled.len(), app.planner_mut().enabled.len(), "recipe choices survive");
    for ((a, got_a, _), (b, got_b, _)) in outputs_before.iter().zip(other.outputs().iter()) {
        assert_eq!(a, b);
        assert!((got_a - got_b).abs() < 1e-3, "{a}: {got_a} vs {got_b}");
    }
}

#[test]
fn share_codes_are_short_and_import_beside() {
    let Some((ctx, mut app, _)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    plan_hmf(&ctx, &mut app);
    let n = app.node_count();
    let code = app.to_portable().to_share_code();
    eprintln!("share code for a {n}-node factory: {} characters", code.len());
    assert!(code.len() < 2000, "fits a Discord message: {} chars", code.len());
    // A typical standard-recipe factory.
    let (c2, mut small, _) = harness().unwrap();
    frames(&c2, &mut small, 2);
    small.planner_mut().targets = vec![Target { item: "Desc_IronPlateReinforced_C".into(), rate: 10.0 }];
    small.press_plan();
    frames(&c2, &mut small, 6);
    eprintln!("share code for a {}-node standard factory: {} characters", small.node_count(), small.to_portable().to_share_code().len());

    let f = crate::portable::FactoryFile::from_share_code(&code).unwrap();
    let right_edge = app.graph().iter_nodes().map(|id| app.node_pos_unzoomed(id).x).fold(f32::MIN, f32::max);
    app.import_factory(&ctx, f);
    frames(&ctx, &mut app, 3);
    assert_eq!(app.node_count(), 2 * n, "imported alongside, not replacing");
    let imported = app.selected();
    assert_eq!(imported.len(), n, "the imported nodes are selected");
    let min_x = imported.iter().map(|id| app.node_pos_unzoomed(*id).x).fold(f32::MAX, f32::min);
    assert!(min_x > right_edge, "placed to the right of the existing factory");
    assert!(app.is_dirty() && app.undo_steps() >= 1, "import is an undoable change");
}

#[test]
fn opening_a_share_code_asks_to_save_first() {
    let Some((ctx, mut app, _)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    plan_hmf(&ctx, &mut app);
    let code = app.to_portable().to_share_code();
    let f = crate::portable::FactoryFile::from_share_code(&code).unwrap();
    app.guard(crate::dialogs::Pending::open_factory(f));
    assert!(matches!(app.dialog(), crate::dialogs::Dialog::Unsaved { .. }));
}

#[test]
fn image_tiles_cover_the_graph() {
    let b = egui::Rect::from_min_size(egui::pos2(-40.0, -40.0), egui::vec2(3000.0, 1000.0));
    let tiles = crate::app_files::tile_origins(b, egui::vec2(1600.0, 960.0));
    assert_eq!(tiles.len(), 4, "2 columns x 2 rows");
    assert_eq!(tiles[0], b.min);
    assert_eq!(tiles[3], b.min + egui::vec2(1600.0, 960.0));
}

// ------------------------------------------------------------------ setup

/// A button drawn on the last frame: (rect, enabled).
fn button(label: &str) -> Option<(egui::Rect, bool)> {
    crate::widgets::TEST_BUTTONS.with(|b| b.borrow().iter().find(|(l, _, _)| l == label).map(|(_, r, e)| (*r, *e)))
}

fn press(ctx: &egui::Context, app: &mut FactoryApp, label: &str) {
    let (rect, enabled) = button(label).unwrap_or_else(|| {
        let shown: Vec<String> = crate::widgets::TEST_BUTTONS.with(|b| b.borrow().iter().map(|(l, _, _)| l.clone()).collect());
        panic!("no “{label}” button; shown: {shown:?}")
    });
    assert!(enabled, "“{label}” is disabled");
    click(ctx, app, rect.center(), egui::PointerButton::Primary);
}

/// Frames with the clock moving, so polling (every 0.15 s) happens.
fn timed_frames(ctx: &egui::Context, app: &mut FactoryApp, n: usize) {
    for _ in 0..n {
        let t = ctx.input(|i| i.time) + 0.2;
        crate::widgets::TEST_BUTTONS.with(|b| b.borrow_mut().clear());
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 960.0))),
            time: Some(t),
            focused: true,
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| app.ui(ctx));
    }
}

type Calls = Rc<RefCell<Vec<(String, Value)>>>;

/// A backend that answers setup's commands. `status` describes this copy.
fn setup_hook(status: Status, calls: Calls) -> RoomHook {
    let polls = Rc::new(std::cell::Cell::new(0u32));
    Rc::new(move |cmd: &str, args: &Value| {
        let setup_cmd = matches!(
            cmd,
            "status" | "detect_game_dirs" | "check_game" | "extract_start" | "extract_status" | "install_run" | "install_restart" | "install_dir_for" | "set_settings" | "dev_test_first_launch" | "dev_remove_test_install"
        );
        if !setup_cmd {
            return None;
        }
        calls.borrow_mut().push((cmd.to_string(), args.clone()));
        Some(match cmd {
            "status" => Ok(serde_json::to_value(&status).unwrap()),
            "set_settings" => {
                let mut s = status.clone();
                s.settings = serde_json::from_value(args["settings"].clone()).unwrap();
                Ok(serde_json::to_value(s).unwrap())
            }
            "detect_game_dirs" => Ok(json!([game_dir(), r"D:\SteamLibrary\steamapps\common\Satisfactory"])),
            "check_game" => {
                let dir = args["dir"].as_str().unwrap();
                if dir == game_dir() {
                    Ok(serde_json::to_value(fb_model::setup::GameCheck { dir: dir.into(), recipes: 1203, items: 168, buildings: 52 }).unwrap())
                } else {
                    Err("That folder doesn't look like Satisfactory.".into())
                }
            }
            "extract_start" => Ok(Value::Null),
            "extract_status" => {
                polls.set(polls.get() + 1);
                let p = match polls.get() {
                    1 => fb_model::setup::ExtractProgress { running: true, stage: "mount".into(), ..Default::default() },
                    2 => fb_model::setup::ExtractProgress { running: true, stage: "icons".into(), done: 400, total: 764, ..Default::default() },
                    _ => fb_model::setup::ExtractProgress { finished: true, stage: "ui".into(), done: 764, total: 764, icons: 747, ui: 17, ..Default::default() },
                };
                Ok(serde_json::to_value(p).unwrap())
            }
            "install_run" => Ok(serde_json::to_value(fb_model::setup::InstallReport {
                dir: args["dir"].as_str().unwrap().into(),
                done: vec!["Copied Factory Builder".into(), "Added a Start menu shortcut".into()],
                problems: vec![],
            })
            .unwrap()),
            "install_dir_for" => Ok(json!(format!(r"{}\Factory Builder", args["chosen"].as_str().unwrap()))),
            "dev_remove_test_install" => Ok(json!("There was no test install.")),
            _ => Ok(Value::Null),
        })
    })
}

fn first_launch_status(installed_copy: bool, dev: bool) -> Status {
    Status {
        settings: Settings { game_dir: String::new(), setup_done: false, ..Default::default() },
        setup: fb_model::setup::InstallInfo {
            product: "Factory Builder".into(),
            version: "0.1.0".into(),
            installed_copy,
            default_dir: r"C:\Users\Pioneer\AppData\Local\Programs\Factory Builder".into(),
            helper_found: true,
            dev,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn names(calls: &Calls) -> Vec<String> {
    calls.borrow().iter().map(|(c, _)| c.clone()).collect()
}

#[test]
fn first_launch_installs_then_restarts_from_the_install() {
    let calls: Calls = Default::default();
    let Some((ctx, mut app, _)) = harness_with(Some(setup_hook(first_launch_status(false, false), calls.clone()))) else { return };
    frames(&ctx, &mut app, 3);
    let w = app.wizard.as_ref().expect("setup opens on first launch");
    assert_eq!(w.step(), crate::setup::Step::Install);
    assert!(button("Skip (development build)").is_none(), "no skipping in a real build");
    assert!(button("Change location…").is_some());
    press(&ctx, &mut app, "Install");
    frames(&ctx, &mut app, 2);
    let (_, args) = calls.borrow().iter().find(|(c, _)| c == "install_run").cloned().expect("installed");
    assert_eq!(args["dir"], json!(r"C:\Users\Pioneer\AppData\Local\Programs\Factory Builder"));
    assert_eq!(args["desktop"], json!(true), "desktop shortcut ticked by default");
    press(&ctx, &mut app, "Restart and continue");
    assert!(names(&calls).contains(&"install_restart".to_string()));
}

#[test]
fn installed_copy_finds_the_game_extracts_icons_and_finishes() {
    let calls: Calls = Default::default();
    let Some((ctx, mut app, _)) = harness_with(Some(setup_hook(first_launch_status(true, false), calls.clone()))) else { return };
    frames(&ctx, &mut app, 3);
    assert_eq!(app.wizard.as_ref().unwrap().step(), crate::setup::Step::Game, "no install step in the installed copy");
    frames(&ctx, &mut app, 2);
    assert_eq!(app.wizard.as_ref().unwrap().game_dir, game_dir(), "the found install is filled in");
    assert!(button("Back").is_none());
    press(&ctx, &mut app, "Next");
    let saved = calls.borrow().iter().filter(|(c, _)| c == "set_settings").last().cloned().expect("game folder saved before extracting");
    assert_eq!(saved.1["settings"]["game_dir"], json!(game_dir()));
    assert_eq!(app.wizard.as_ref().unwrap().step(), crate::setup::Step::Icons);
    assert!(names(&calls).contains(&"extract_start".to_string()));
    assert!(!button("Next").unwrap().1, "Next waits for the icons");
    timed_frames(&ctx, &mut app, 8);
    let p = app.wizard.as_ref().unwrap().extract.clone().unwrap();
    assert!(p.finished && p.icons == 747);
    press(&ctx, &mut app, "Next");
    assert_eq!(app.wizard.as_ref().unwrap().step(), crate::setup::Step::Name);
    frame_with(&ctx, &mut app, vec![egui::Event::Text("Ada".into())]);
    frames(&ctx, &mut app, 1);
    press(&ctx, &mut app, "Finish");
    assert!(app.wizard.is_none(), "setup closes");
    let last = calls.borrow().iter().filter(|(c, _)| c == "set_settings").last().cloned().unwrap();
    assert_eq!(last.1["settings"]["setup_done"], json!(true));
    assert_eq!(last.1["settings"]["user_name"], json!("Ada"));
    assert_eq!(last.1["settings"]["game_dir"], json!(game_dir()));
}

#[test]
fn a_wrong_game_folder_blocks_next() {
    let calls: Calls = Default::default();
    let mut status = first_launch_status(true, false);
    status.settings.game_dir = r"C:\Games\NotSatisfactory".into();
    let Some((ctx, mut app, _)) = harness_with(Some(setup_hook(status, calls.clone()))) else { return };
    frames(&ctx, &mut app, 4);
    assert!(!button("Next").unwrap().1, "can't go on with a folder that isn't the game");
    let w = app.wizard.as_ref().unwrap();
    assert!(matches!(&w.check, Some((_, Err(e))) if e.contains("doesn't look like")));
    assert_eq!(w.found.len(), 2, "installs found on this PC are offered");
}

#[test]
fn game_update_offers_to_extract_again() {
    let calls: Calls = Default::default();
    let mut status = first_launch_status(true, false);
    status.settings.setup_done = true;
    status.settings.game_dir = game_dir();
    status.icons_found = 747;
    status.ui_assets_found = 17;
    status.game_changed = true;
    let Some((ctx, mut app, _)) = harness_with(Some(setup_hook(status, calls.clone()))) else { return };
    frames(&ctx, &mut app, 3);
    assert!(app.wizard.is_none());
    press(&ctx, &mut app, "Extract icons and font");
    let w = app.wizard.as_ref().expect("icons step opens");
    assert!(w.icons_only && w.step() == crate::setup::Step::Icons);
    timed_frames(&ctx, &mut app, 8);
    press(&ctx, &mut app, "Done");
    assert!(app.wizard.is_none());
    assert!(button("Extract icons and font").is_none(), "the notice goes away");
}

#[test]
fn developer_settings_only_in_development_builds() {
    for dev in [false, true] {
        let calls: Calls = Default::default();
        let mut status = first_launch_status(true, dev);
        status.settings.setup_done = true;
        status.icons_found = 747;
        status.ui_assets_found = 17;
        let Some((ctx, mut app, _)) = harness_with(Some(setup_hook(status.clone(), calls.clone()))) else { return };
        frames(&ctx, &mut app, 3);
        app.dialog = crate::dialogs::Dialog::settings(&status.settings);
        frames(&ctx, &mut app, 2);
        assert_eq!(button("Test first launch").is_some(), dev, "dev = {dev}");
        assert!(button("Extract again").is_some());
        if dev {
            press(&ctx, &mut app, "Test first launch");
            assert!(names(&calls).contains(&"dev_test_first_launch".to_string()));
            app.dialog = crate::dialogs::Dialog::settings(&status.settings);
            frames(&ctx, &mut app, 2);
            press(&ctx, &mut app, "Run setup here");
            assert_eq!(app.wizard.as_ref().unwrap().step(), crate::setup::Step::Game);
        }
    }
}

// ---------------------------------------------------- extraction and build cost

#[test]
fn overview_extraction_and_build_cost() {
    use crate::overview_extra::*;
    let Some((ctx, mut app, _)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    plan_hmf(&ctx, &mut app);
    let game = app.user.game.clone();
    let res = app.summary.resources.clone();
    let iron = *res.get("Desc_OreIron_C").expect("the plan mines iron");
    assert!(iron > 0.0);

    // Default: Miner Mk.1 on Normal nodes at 100% = 60 per min each.
    let s = app.planner.extraction.clone();
    assert_eq!(s, ExtractionSettings::default());
    let p = plan_resource(&game, "Desc_OreIron_C", iron, &s).unwrap();
    assert_eq!((p.class.as_str(), p.per_each), ("Build_MinerMk1_C", 60.0));
    assert_eq!(p.count, (iron / 60.0 - 1e-6).ceil() as u64);
    assert!(plan_text(&p, true).contains("Miner Mk.1 on Normal nodes at 100%"));

    // Mk.3, Pure, 250%: 240 * 2 * 2.5 = 1200 per min each; 3 shards each.
    let fast = ExtractionSettings { miner: "Build_MinerMk3_C".into(), purity: Purity::Pure, clock_pct: 250.0 };
    let q = plan_resource(&game, "Desc_OreIron_C", iron, &fast).unwrap();
    assert_eq!(q.per_each, 1200.0);
    assert_eq!(q.shards_total, q.count * 3);
    assert_eq!(q.belt, BeltNote::Fits("Conveyor Belt Mk.6".into()));
    assert_eq!(shards_per_machine(1.0), 0);
    assert_eq!(shards_per_machine(1.01), 1);
    assert_eq!(shards_per_machine(2.0), 2);
    assert_eq!(shards_per_machine(2.5), 3);

    // Fluids use their own extractors.
    let water = plan_resource(&game, "Desc_Water_C", 250.0, &s).unwrap();
    assert_eq!((water.class.as_str(), water.count, water.purity), ("Build_WaterPump_C", 3, None));
    let oil = plan_resource(&game, "Desc_LiquidOil_C", 250.0, &s).unwrap();
    assert_eq!((oil.class.as_str(), oil.count), ("Build_OilPump_C", 3));
    let n2 = plan_resource(&game, "Desc_NitrogenGas_C", 250.0, &s).unwrap();
    assert_eq!((n2.class.as_str(), n2.count), ("Build_FrackingExtractor_C", 5));

    // Build cost: machines (whole) plus extractors, from the Docs recipes.
    let plans = plan_all(&game, &res, &s);
    let totals = build_cost_totals(&game, &app.summary.buildings, &plans);
    assert!(!app.summary.buildings.is_empty() && !totals.is_empty());
    let mut expect: std::collections::BTreeMap<String, f64> = Default::default();
    let mut machines: Vec<(String, f64)> = app.summary.buildings.iter().map(|(b, (w, _))| (b.clone(), *w)).collect();
    machines.extend(plans.iter().map(|p| (p.class.clone(), p.count as f64)));
    for (b, n) in machines {
        for a in &game.build_costs[&b] {
            *expect.entry(a.item.clone()).or_default() += a.amount * n;
        }
    }
    assert_eq!(totals, expect);
    // One Constructor costs 2 Reinforced Iron Plate + 8 Cable.
    let only = std::collections::BTreeMap::from([("Build_ConstructorMk1_C".to_string(), (3.0, 2.2))]);
    let t = build_cost_totals(&game, &only, &[]);
    assert_eq!(t["Desc_IronPlateReinforced_C"], 6.0);
    assert_eq!(t["Desc_Cable_C"], 24.0);

    // The sections render (closed and with settings changed) without panics.
    frames(&ctx, &mut app, 2);
    app.planner_mut().extraction = fast;
    frames(&ctx, &mut app, 2);
}

#[test]
fn extraction_settings_save_with_the_project_and_old_files_load() {
    let Some((ctx, mut app, _)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    app.planner_mut().extraction.clock_pct = 150.0;
    app.planner_mut().extraction.purity = Purity::Pure;
    let json = serde_json::to_string(&app.planner).unwrap();
    let back: crate::app::Planner = serde_json::from_str(&json).unwrap();
    assert_eq!(back.extraction, app.planner.extraction);
    // A planner saved before this feature has no "extraction" key.
    let mut v: Value = serde_json::from_str(&json).unwrap();
    v.as_object_mut().unwrap().remove("extraction");
    let old: crate::app::Planner = serde_json::from_value(v).unwrap();
    assert_eq!(old.extraction, crate::overview_extra::ExtractionSettings::default());
}
