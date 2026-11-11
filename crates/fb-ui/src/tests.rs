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
                Ok(serde_json::to_value(Status { icons_found: 747, ui_assets_found: 17, map_found: true, settings, ..Default::default() }).unwrap())
            }
            // Set up, with icons present, so setup and notices stay closed.
            "status" => Ok(serde_json::to_value(Status {
                icons_found: 747,
                ui_assets_found: 17,
                map_found: true,
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
    let mut app = FactoryApp::with_backend(&ctx, backend);
    // A new project starts with the HUB on the master graph (hub_tests);
    // most tests work in one plain factory, as a factory file opens.
    app.reset_to(None, None);
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
    // ore -> miners -> smelters -> constructors -> output
    assert_eq!(app.node_count(), 5, "nodes");
    assert!(
        (app.min_efficiency() - 1.0).abs() < 1e-6,
        "every machine at 100%: {}",
        app.min_efficiency()
    );
    let outs = app.outputs();
    assert_eq!(outs.len(), 1);
    assert!((outs[0].1 - 60.0).abs() < 1e-6, "delivered {:?}", outs);
    // 24 MW of machines, plus the Miners Mk.1 (the default extractor
    // settings: Normal nodes, 100%) for the ore, from the game's data.
    let game = app.user.game.clone();
    let ore = app.summary.resources["Desc_OreIron_C"];
    let miner = crate::extractors::find(&game, "Build_MinerMk1_C").unwrap();
    // Whole miners at 100%, as Plan factory builds them.
    let setup = fb_model::MachineSetup::new((ore / miner.per_min(fb_model::Purity::Normal, 1.0) - 1e-6).ceil(), 1.0, 0);
    let miners = crate::extractors::power(miner, setup.count, setup.clock);
    assert!(miners > 0.0);
    assert!((app.power() - (24.0 + miners)).abs() < 1e-3, "power {} = 24 + {miners}", app.power());
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
    assert!(app.on_master() && app.node_count() == 1, "a new project: the HUB on the master graph");
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
    assert_eq!(app.node_count(), 5);
    assert!(app.is_dirty());
    assert!(app.undo_steps() >= 1);

    app.undo();
    frames(&ctx, &mut app, 1);
    assert_eq!(app.node_count(), 0, "undo removes the planned graph");
    app.redo();
    frames(&ctx, &mut app, 1);
    assert_eq!(app.node_count(), 5, "redo brings it back");

    // New with unsaved changes asks first instead of discarding.
    app.guard(crate::dialogs::Pending::New);
    assert!(matches!(
        app.dialog(),
        crate::dialogs::Dialog::Unsaved { .. }
    ));
    assert_eq!(app.node_count(), 5);
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
    assert_eq!(app.node_count(), 10, "duplicated all five nodes");
    // The copies are wired like the originals, so the copy also delivers 60.
    let delivered: f64 = app.outputs().iter().map(|o| o.1).sum();
    assert!((delivered - 120.0).abs() < 1e-3, "delivered {delivered}");
    app.delete_selected();
    frames(&ctx, &mut app, 2);
    assert_eq!(app.node_count(), 5, "deleted the selected copies");
    app.undo();
    frames(&ctx, &mut app, 1);
    assert_eq!(app.node_count(), 10, "delete is undoable");
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

/// Wired by hand (plans route shared belts through splitters and mergers):
/// ore feeding two smelters, and a second ore node into the first smelter
/// too. Returns the output with two wires and the input with two.
fn fan_graph(ctx: &egui::Context, app: &mut FactoryApp) -> (egui_node_graph2::OutputId, egui_node_graph2::InputId) {
    use crate::graph::Template;
    frames(ctx, app, 2);
    app.minimap = false;
    let at = |app: &FactoryApp, x: f32, y: f32| app.screen_to_graph(egui::pos2(x, y));
    let p = at(app, 420.0, 150.0);
    let ore = app.add_test_node(Template::Resource("Desc_OreIron_C".into()), p);
    let p = at(app, 420.0, 450.0);
    let ore2 = app.add_test_node(Template::Resource("Desc_OreIron_C".into()), p);
    let p = at(app, 820.0, 120.0);
    let a = app.add_test_node(Template::Recipe("Recipe_IngotIron_C".into()), p);
    let p = at(app, 820.0, 420.0);
    let b = app.add_test_node(Template::Recipe("Recipe_IngotIron_C".into()), p);
    let (input, output) = connect(app, ore, a, "Desc_OreIron_C");
    connect(app, ore, b, "Desc_OreIron_C");
    connect(app, ore2, a, "Desc_OreIron_C");
    frames(ctx, app, 3);
    (output, input)
}

#[test]
fn each_output_bubble_pulls_its_own_wire() {
    use egui_node_graph2::AnyParameterId;
    let Some((ctx, mut app, _)) = harness() else {
        return;
    };
    let (output, _) = fan_graph(&ctx, &mut app);
    let wires = app.graph().output_connections(output);
    assert_eq!(wires.len(), 2);
    let hooks = app.port_hooks[&AnyParameterId::Output(output)].clone();
    // A resource takes any number of wires (it's the ground, not a belt).
    assert_eq!(hooks.len(), wires.len() + 1, "one bubble per wire plus a free ring");
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
fn a_wire_let_go_on_the_canvas_offers_what_fits_and_connects_it() {
    use egui_node_graph2::AnyParameterId;
    let Some((ctx, mut app, _)) = harness() else {
        return;
    };
    frames(&ctx, &mut app, 2);
    app.minimap = false;
    let p = app.screen_to_graph(egui::pos2(500.0, 200.0));
    let smelter = app.add_test_node(crate::graph::Template::Recipe("Recipe_IngotIron_C".into()), p);
    frames(&ctx, &mut app, 3);
    let g = app.graph();
    let output = g[smelter].outputs[0].1;
    let nodes_before = g.nodes.len();
    let ring = *app.port_hooks[&AnyParameterId::Output(output)].last().unwrap();
    drag(&ctx, &mut app, ring, egui::pos2(ring.x + 60.0, 920.0));
    let wire = app.finder.as_ref().and_then(|f| f.wire.clone()).expect("Add node opened for the wire");
    assert_eq!((wire.item.as_str(), wire.from_output), ("Desc_IronIngot_C", true));
    // Only what takes Iron Ingot in.
    let catalog = app.catalog.as_ref().unwrap();
    let hits = catalog.search("", Some(&wire));
    assert!(hits.len() > 3, "{}", hits.len());
    let game = app.user.game.clone();
    for &i in &hits {
        let ok = match &catalog.entries[i].template {
            crate::graph::Template::Recipe(r) => game.recipes[r].ingredients.iter().any(|a| a.item == "Desc_IronIngot_C"),
            crate::graph::Template::Output(x) | crate::graph::Template::Sink(x) => x == "Desc_IronIngot_C",
            crate::graph::Template::Merger(x) | crate::graph::Template::Splitter(x) | crate::graph::Template::PriorityMerger(x) => x == "Desc_IronIngot_C",
            crate::graph::Template::SmartSplitter { item, .. } => item == "Desc_IronIngot_C",
            crate::graph::Template::Storage { item, .. } => item == "Desc_IronIngot_C",
            _ => false,
        };
        assert!(ok, "{} doesn't take Iron Ingot", catalog.entries[i].title);
    }
    // Enter adds the first one, wired to the ingots.
    let wires_before = app.graph().output_connections(output).len();
    press_key(&ctx, &mut app, egui::Key::Enter);
    frames(&ctx, &mut app, 2);
    assert!(app.finder.is_none());
    let g = app.graph();
    assert_eq!(g.nodes.len(), nodes_before + 1);
    let wires = g.output_connections(output);
    assert_eq!(wires.len(), wires_before + 1, "the new node is wired to the ingots");
    let new = g[*wires.last().unwrap()].node;
    assert!(app.state.selected_nodes == vec![new]);
}

#[test]
fn input_bubble_pulls_its_own_wire() {
    use egui_node_graph2::AnyParameterId;
    let Some((ctx, mut app, _)) = harness() else {
        return;
    };
    let (_, input) = fan_graph(&ctx, &mut app);
    let outs = app.graph().connections(input);
    assert_eq!(outs.len(), 2);
    let hooks = app.port_hooks[&AnyParameterId::Input(input)].clone();
    assert_eq!(hooks.len(), outs.len(), "no free ring on a full port");
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
    // The menu opens at the click, with its rows the menu's full width.
    let menu = ctx.memory(|m| m.area_rect(egui::Id::new("node_menu"))).expect("node menu open");
    let rows: Vec<egui::Rect> = crate::widgets::TEST_BUTTONS.with(|b| b.borrow().iter().filter(|(l, r, _)| menu.contains_rect(*r) && ["Duplicate", "Delete", "Add comment frame", "Move to a new factory"].contains(&l.as_str())).map(|(_, r, _)| *r).collect());
    assert_eq!(rows.len(), 4);
    assert!(rows.iter().all(|r| (r.width() - rows[0].width()).abs() < 0.5 && r.width() > menu.width() - 4.0), "every row as wide as the menu: {rows:?}");
    let delete = crate::widgets::TEST_BUTTONS.with(|b| b.borrow().iter().find(|(l, r, _)| l == "Delete" && menu.contains_rect(*r)).map(|(_, r, _)| *r)).expect("Delete row");
    click(&ctx, &mut app, delete.center(), egui::PointerButton::Primary);
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
fn a_full_port_takes_no_more_wires() {
    use crate::graph::Template;
    use egui_node_graph2::AnyParameterId;
    let Some((ctx, mut app, _)) = harness() else {
        return;
    };
    frames(&ctx, &mut app, 2);
    app.minimap = false;
    // A smelter sending its ingots to an Output: its one belt is taken.
    let at = |app: &FactoryApp, x: f32, y: f32| app.screen_to_graph(egui::pos2(x, y));
    let p = at(&app, 420.0, 150.0);
    let smelter = app.add_test_node(Template::Recipe("Recipe_IngotIron_C".into()), p);
    let p = at(&app, 820.0, 150.0);
    let output = app.add_test_node(Template::Output("Desc_IronIngot_C".into()), p);
    let p = at(&app, 820.0, 600.0);
    let sink = app.add_test_node(Template::Sink("Desc_IronIngot_C".into()), p);
    connect(&mut app, smelter, output, "Desc_IronIngot_C");
    frames(&ctx, &mut app, 3);
    let out = app.graph()[smelter].outputs[0].1;
    assert_eq!(app.port_hooks[&AnyParameterId::Output(out)].len(), 1, "no free ring");
    let sink_in = app.graph()[sink].inputs[0].1;
    // Dropped on the smelter's bubble, or on the smelter: no second belt.
    let sink_ring = app.port_hooks[&AnyParameterId::Input(sink_in)][0];
    let bubble = app.port_hooks[&AnyParameterId::Output(out)][0];
    drag(&ctx, &mut app, sink_ring, bubble);
    let sink_ring = app.port_hooks[&AnyParameterId::Input(sink_in)][0];
    let body = app.node_rects[&smelter].center();
    drag(&ctx, &mut app, sink_ring, body);
    let g = app.graph();
    assert_eq!(g.output_connections(out).len(), 1, "still one belt out of the smelter");
    assert!(g.connections(sink_in).is_empty(), "the sink got none");
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
    /// Everything relayed, in order (tests check what went out).
    sent: Vec<room::Wire>,
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
                    Wire::Doc { json, .. } => {
                        n.rev += 1;
                        n.doc = Some(json.clone());
                        Wire::Doc { from: me, rev: n.rev, json }
                    }
                    // Cursor, ping, chat, view, selection: stamped and checked like the host does.
                    other => match other.from_peer(me) {
                        Some(w) => w,
                        None => return Some(Ok(Value::Null)),
                    },
                };
                n.sent.push(out.clone());
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
    assert_eq!(host.node_count(), 5);

    // Guest joins and gets the host's factory.
    guest.join_room("Guesty".into(), test_code());
    frames(&gctx, &mut guest, 6);
    assert_eq!(guest.node_count(), 5, "guest received the host's factory");
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
    assert_eq!(host.node_count(), 10, "host received the guest's edit");

    // The guest's cursor shows on the host's canvas, in graph coordinates.
    let at = egui::pos2(900.0, 500.0);
    frame_with(&gctx, &mut guest, vec![egui::Event::PointerMoved(at)]);
    frames(&gctx, &mut guest, 1);
    frames(&hctx, &mut host, 3);
    let seen = *host.room.cursors.get(&1).expect("host has the guest's cursor");
    let expected = guest.screen_to_graph_unzoomed(at);
    assert!((seen - expected).length() < 0.5, "cursor {seen:?} vs {expected:?}");
}

/// A host who planned iron plates and a guest who joined, over a fake relay.
fn room_pair() -> Option<(Rc<RefCell<FakeNet>>, egui::Context, FactoryApp, egui::Context, FactoryApp)> {
    let net: Rc<RefCell<FakeNet>> = Default::default();
    let (hctx, mut host, _) = harness_with(Some(room_hook(net.clone(), true)))?;
    let (gctx, mut guest, _) = harness_with(Some(room_hook(net.clone(), false))).unwrap();
    frames(&hctx, &mut host, 2);
    frames(&gctx, &mut guest, 2);
    host.open_room("Hoster".into());
    frames(&hctx, &mut host, 2);
    host.dialog = crate::dialogs::Dialog::None;
    host.planner_mut().targets = vec![Target { item: "Desc_IronPlate_C".into(), rate: 60.0 }];
    host.press_plan();
    frames(&hctx, &mut host, 8);
    guest.join_room("Guesty".into(), test_code());
    frames(&gctx, &mut guest, 6);
    frames(&hctx, &mut host, 3);
    assert_eq!(guest.node_count(), host.node_count());
    Some((net, hctx, host, gctx, guest))
}

/// One frame with these events and modifier keys held.
fn frame_mods(ctx: &egui::Context, app: &mut FactoryApp, events: Vec<egui::Event>, modifiers: egui::Modifiers) {
    crate::widgets::TEST_BUTTONS.with(|b| b.borrow_mut().clear());
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 960.0))),
        events,
        modifiers,
        focused: true,
        ..Default::default()
    };
    let _ = ctx.run(input, |ctx| app.ui(ctx));
}

fn alt_click(ctx: &egui::Context, app: &mut FactoryApp, at: egui::Pos2) {
    let alt = egui::Modifiers::ALT;
    let ev = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: alt };
    frame_mods(ctx, app, vec![egui::Event::PointerMoved(at)], alt);
    frame_mods(ctx, app, vec![ev(true)], alt);
    frame_mods(ctx, app, vec![ev(false)], alt);
    frames(ctx, app, 2);
}

fn key(k: egui::Key) -> Vec<egui::Event> {
    vec![
        egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() },
        egui::Event::Key { key: k, physical_key: None, pressed: false, repeat: false, modifiers: Default::default() },
    ]
}

/// A point on the canvas, well inside it, that no node covers.
fn empty_spot(app: &FactoryApp) -> egui::Pos2 {
    let r = app.editor_rect.shrink(60.0);
    let mut y = r.top();
    while y < r.bottom() {
        let mut x = r.left();
        while x < r.right() {
            let p = egui::pos2(x, y);
            if !app.node_rects.values().any(|n| n.expand(20.0).contains(p)) {
                return p;
            }
            x += 40.0;
        }
        y += 40.0;
    }
    panic!("no empty canvas");
}

/// Every shape drawn on one frame (flattened).
fn drawn_shapes(ctx: &egui::Context, app: &mut FactoryApp) -> Vec<egui::Shape> {
    fn walk(shape: &egui::Shape, out: &mut Vec<egui::Shape>) {
        match shape {
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
            s => out.push(s.clone()),
        }
    }
    crate::widgets::TEST_BUTTONS.with(|b| b.borrow_mut().clear());
    let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 960.0))), focused: true, ..Default::default() };
    let out = ctx.run(input, |ctx| app.ui(ctx));
    let mut shapes = Vec::new();
    for s in &out.shapes {
        walk(&s.shape, &mut shapes);
    }
    shapes
}

fn graph_centre(app: &FactoryApp) -> egui::Pos2 {
    app.screen_to_graph_unzoomed(app.editor_rect.center())
}

#[test]
fn room_pings_and_messages_arrive_and_show() {
    let Some((net, hctx, mut host, gctx, mut guest)) = room_pair() else { return };
    // Alt+click on empty canvas pings, and doesn't drop the guest's selection.
    let node = guest.graph().iter_nodes().next().unwrap();
    guest.state.selected_nodes = vec![node];
    let spot = empty_spot(&guest);
    alt_click(&gctx, &mut guest, spot);
    assert_eq!(guest.selected(), vec![node], "Alt+click is a ping, not a deselect");
    let at = guest.screen_to_graph_unzoomed(spot);
    let mine = guest.room.pings.last().expect("my own ping shows too").clone();
    assert_eq!((mine.id, mine.doc), (1, guest.active));
    frames(&hctx, &mut host, 2);
    let ping = host.room.pings.last().expect("the host got the ping").clone();
    assert_eq!((ping.id, ping.doc), (1, host.active));
    assert!((ping.pos - at).length() < 0.5, "{:?} vs {at:?}", ping.pos);
    // Drawn on the host's canvas where the guest clicked (same view here).
    let expected = host.editor_rect.min + host.state.pan_zoom.pan + ping.pos.to_vec2() * host.state.pan_zoom.zoom;
    let rings = drawn_shapes(&hctx, &mut host).into_iter().filter(|s| matches!(s, egui::Shape::Circle(c) if (c.center - expected).length() < 1.0)).count();
    assert!(rings >= 2, "a ring and a dot at the ping");
    // Gone after about 2 s (frames are 1/60 s apart here).
    frames(&hctx, &mut host, 130);
    assert!(host.room.pings.is_empty(), "pings fade after 2 s");

    // / opens a box at the pointer (without typing the /); Enter sends.
    frame_with(&gctx, &mut guest, vec![egui::Event::PointerMoved(spot)]);
    frame_with(&gctx, &mut guest, vec![egui::Event::Text("/".into())]);
    frames(&gctx, &mut guest, 1);
    let draft = guest.room.chat.clone().expect("the message box opened");
    assert_eq!(draft.text, "", "the / isn't typed into the box");
    assert_eq!(draft.at, spot);
    frame_with(&gctx, &mut guest, vec![egui::Event::Text("Over here".into())]);
    frame_with(&gctx, &mut guest, key(egui::Key::Enter));
    frames(&gctx, &mut guest, 1);
    assert!(guest.room.chat.is_none(), "Enter sent it and closed the box");
    frames(&hctx, &mut host, 2);
    let last = host.room.messages.back().expect("the host got the message").clone();
    assert_eq!((last.id, last.name.as_str(), last.text.as_str()), (1, "Guesty", "Over here"));
    assert!(host.room.bubbles.contains_key(&1));
    // Shown beside the guest's cursor and in the overview's Messages list.
    let texts = drawn_texts(&hctx, &mut host);
    assert!(texts.iter().filter(|t| t.contains("Over here")).count() >= 2, "bubble and list: {texts:?}");
    assert!(texts.iter().any(|t| t == "Messages"));
    // Esc cancels without sending.
    let before = net.borrow().sent.len();
    frame_with(&gctx, &mut guest, vec![egui::Event::Text("/".into())]);
    frames(&gctx, &mut guest, 1);
    frame_with(&gctx, &mut guest, vec![egui::Event::Text("never mind".into())]);
    frame_with(&gctx, &mut guest, key(egui::Key::Escape));
    frames(&gctx, &mut guest, 1);
    assert!(guest.room.chat.is_none());
    assert!(!net.borrow().sent[before..].iter().any(|w| matches!(w, room::Wire::Chat { .. })), "Esc sent nothing");
    // The bubble goes after about 6 s; the list keeps it.
    frames(&hctx, &mut host, 370);
    assert!(!host.room.bubbles.contains_key(&1));
    assert_eq!(host.room.messages.len(), 1);
}

#[test]
fn chat_text_is_cleaned_and_capped() {
    let Some((net, hctx, mut host, gctx, mut guest)) = room_pair() else { return };
    // Sending: control characters out, at most 200 characters.
    let mid = guest.editor_rect.center();
    frame_with(&gctx, &mut guest, vec![egui::Event::PointerMoved(mid)]);
    frame_with(&gctx, &mut guest, vec![egui::Event::Text("/".into())]);
    frames(&gctx, &mut guest, 1);
    guest.room.chat.as_mut().unwrap().text = format!("hi\u{0007}\u{001B}[31m{}", "y".repeat(300));
    frame_with(&gctx, &mut guest, key(egui::Key::Enter));
    frames(&gctx, &mut guest, 1);
    let sent = net.borrow().sent.iter().rev().find_map(|w| match w {
        room::Wire::Chat { text, .. } => Some(text.clone()),
        _ => None,
    });
    let sent = sent.expect("sent");
    assert!(sent.starts_with("hi[31myyy"), "{sent}");
    assert_eq!(sent.chars().count(), room::MAX_CHAT);
    frames(&hctx, &mut host, 2);
    assert_eq!(host.room.messages.back().unwrap().text, sent);

    // Receiving: whatever a (hostile) host sends is cleaned and capped too,
    // and an empty one is dropped.
    let raw = format!("\u{202E}evil\r\n{}", "z".repeat(1000));
    net.borrow_mut().guest.push_back(room::RoomEvent::Message(room::Wire::Chat { id: 0, text: raw }));
    net.borrow_mut().guest.push_back(room::RoomEvent::Message(room::Wire::Chat { id: 0, text: "\u{0000}\u{0008}".into() }));
    frames(&gctx, &mut guest, 2);
    let got: Vec<String> = guest.room.messages.iter().filter(|m| m.id == 0).map(|m| m.text.clone()).collect();
    assert_eq!(got.len(), 1, "the empty one was dropped");
    assert!(got[0].starts_with("evilzzz"), "{}", got[0]);
    assert_eq!(got[0].chars().count(), room::MAX_CHAT);
    // The list keeps the last 20.
    for k in 0..25 {
        net.borrow_mut().guest.push_back(room::RoomEvent::Message(room::Wire::Chat { id: 0, text: format!("m{k}") }));
    }
    frames(&gctx, &mut guest, 2);
    assert_eq!(guest.room.messages.len(), crate::room_extras::MESSAGES_KEPT);
    assert_eq!(guest.room.messages.back().unwrap().text, "m24");
}

#[test]
fn following_switches_document_tracks_the_view_and_stops_when_i_pan() {
    let Some((_net, hctx, mut host, gctx, mut guest)) = room_pair() else { return };
    // The host opens a second factory; the guest stays on the first.
    let first = host.active;
    host.add_factory_file(&hctx, &ingot_factory("Smelting", 2.0));
    let second = host.active;
    assert_ne!(first, second);
    frames(&hctx, &mut host, 8);
    frames(&gctx, &mut guest, 4);
    assert!(guest.doc(second).is_some(), "the guest has the new factory");
    assert_eq!(guest.active, first);

    // The host zooms in somewhere.
    let wheel = |dy: f32| egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, dy), modifiers: Default::default() };
    let at = host.editor_rect.center() + egui::vec2(150.0, 40.0);
    frame_with(&hctx, &mut host, vec![egui::Event::PointerMoved(at)]);
    frame_with(&hctx, &mut host, vec![wheel(120.0)]);
    frames(&hctx, &mut host, 60); // smooth zooming settles, the last view goes out

    // The guest clicks the host in "In this room" (their own row isn't clickable).
    frames(&gctx, &mut guest, 1); // buttons are per thread: the guest's frame last
    assert!(button("Follow Guesty").is_none(), "no following myself");
    press(&gctx, &mut guest, "Follow Hoster");
    assert_eq!(guest.room.following, Some(0));
    frames(&gctx, &mut guest, 3);
    assert_eq!(guest.active, second, "switched to the host's document");
    let close = |a: &FactoryApp, b: &FactoryApp| (graph_centre(a) - graph_centre(b)).length() < 0.5 && (a.state.pan_zoom.zoom - b.state.pan_zoom.zoom).abs() < 1e-4;
    assert!(close(&host, &guest), "same view: host {:?} @ {}, guest {:?} @ {}", graph_centre(&host), host.state.pan_zoom.zoom, graph_centre(&guest), guest.state.pan_zoom.zoom);
    // A bar names who I'm following.
    let texts = drawn_texts(&gctx, &mut guest);
    assert!(texts.iter().any(|t| t == "Following Hoster"), "{texts:?}");
    assert!(button("Stop following").is_some());

    // The host moves on: zooms out and pans (Ctrl+drag); the guest keeps up.
    frame_with(&hctx, &mut host, vec![wheel(-200.0)]);
    frames(&hctx, &mut host, 60); // smooth zooming settles, the last view goes out
    let from = empty_spot(&host);
    let ctrl = egui::Modifiers::COMMAND;
    frame_mods(&hctx, &mut host, vec![egui::Event::PointerMoved(from)], ctrl);
    frame_mods(&hctx, &mut host, vec![egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: ctrl }], ctrl);
    for k in 1..=6 {
        frame_mods(&hctx, &mut host, vec![egui::Event::PointerMoved(from + egui::vec2(20.0 * k as f32, 10.0 * k as f32))], ctrl);
    }
    frame_mods(&hctx, &mut host, vec![egui::Event::PointerButton { pos: from + egui::vec2(120.0, 60.0), button: egui::PointerButton::Primary, pressed: false, modifiers: ctrl }], ctrl);
    frames(&hctx, &mut host, 60); // smooth zooming settles, the last view goes out
    frames(&gctx, &mut guest, 3);
    assert!(close(&host, &guest), "still the same view: host {:?} @ {}, guest {:?} @ {}", graph_centre(&host), host.state.pan_zoom.zoom, graph_centre(&guest), guest.state.pan_zoom.zoom);

    // The guest scrolls on the canvas: following stops and the view stays theirs.
    let mid = guest.editor_rect.center();
    frame_with(&gctx, &mut guest, vec![egui::Event::PointerMoved(mid)]);
    frame_with(&gctx, &mut guest, vec![wheel(60.0)]);
    frames(&gctx, &mut guest, 3);
    assert_eq!(guest.room.following, None, "my own zoom stops following");
    frames(&gctx, &mut guest, 60);
    let mine = (graph_centre(&guest), guest.state.pan_zoom.zoom);
    let mid = host.editor_rect.center();
    frame_with(&hctx, &mut host, vec![egui::Event::PointerMoved(mid)]);
    frame_with(&hctx, &mut host, vec![wheel(150.0)]);
    frames(&hctx, &mut host, 60); // smooth zooming settles, the last view goes out
    frames(&gctx, &mut guest, 3);
    assert!((guest.state.pan_zoom.zoom - mine.1).abs() < 1e-4, "no longer tracking");
    assert!(!drawn_texts(&gctx, &mut guest).iter().any(|t| t.starts_with("Following")));

    // Following again, then a click on the canvas or Esc stops it.
    frames(&gctx, &mut guest, 1); // buttons are per thread: the guest's frame last
    press(&gctx, &mut guest, "Follow Hoster");
    frames(&gctx, &mut guest, 2);
    assert_eq!(guest.room.following, Some(0));
    let spot = empty_spot(&guest);
    click(&gctx, &mut guest, spot, egui::PointerButton::Primary);
    assert_eq!(guest.room.following, None, "a click on the canvas stops it");
    frames(&gctx, &mut guest, 1); // buttons are per thread: the guest's frame last
    press(&gctx, &mut guest, "Follow Hoster");
    frames(&gctx, &mut guest, 2);
    frame_with(&gctx, &mut guest, key(egui::Key::Escape));
    assert_eq!(guest.room.following, None, "Esc stops it");
    // And so does the person leaving.
    frames(&gctx, &mut guest, 1); // buttons are per thread: the guest's frame last
    press(&gctx, &mut guest, "Follow Hoster");
    assert_eq!(guest.room.following, Some(0));
    _net.borrow_mut().guest.push_back(room::RoomEvent::Message(room::Wire::Left { id: 0 }));
    frames(&gctx, &mut guest, 2);
    assert_eq!(guest.room.following, None);
}

#[test]
fn others_selections_are_outlined_with_name_chips() {
    let Some((_net, hctx, mut host, gctx, mut guest)) = room_pair() else { return };
    let nodes: Vec<_> = host.graph().iter_nodes().collect();
    host.state.selected_nodes = vec![nodes[0], nodes[1]];
    frames(&hctx, &mut host, 8);
    frames(&gctx, &mut guest, 2);
    let (doc, got) = guest.room.selections.get(&0).cloned().expect("the guest knows the host's selection");
    assert_eq!(doc, guest.active);
    assert_eq!(got, vec![nodes[0], nodes[1]]);

    let host_color = egui::Color32::from_rgb(0xFA, 0x95, 0x49);
    let shapes = drawn_shapes(&gctx, &mut guest);
    for n in [nodes[0], nodes[1]] {
        let r = guest.node_rects[&n];
        let outline = shapes.iter().any(|s| matches!(s, egui::Shape::Rect(x) if x.stroke.width == 2.0 && x.stroke.color == host_color && (x.rect.min - (r.min - egui::vec2(1.0, 1.0))).length() < 0.01));
        assert!(outline, "a 2 px outline in the host's colour round {n:?}");
        let chip = shapes.iter().any(|s| matches!(s, egui::Shape::Rect(x) if x.fill == host_color && (x.rect.left_bottom() - (r.left_top() - egui::vec2(2.0, 2.0))).length() < 0.01));
        assert!(chip, "a name chip sitting on {n:?}'s top-left corner");
    }
    assert!(!shapes.iter().any(|s| matches!(s, egui::Shape::Rect(x) if x.stroke.color == host_color && x.rect.contains_rect(guest.node_rects[&nodes[2]]))), "unselected nodes have no outline");

    // A node two people have selected: both chips, side by side.
    net_select(&_net, 5, nodes[0], guest.active);
    guest.room.peers.insert(5, room::Peer { id: 5, name: "Ada".into(), color: [0x86, 0xC7, 0xF2] });
    frames(&gctx, &mut guest, 2);
    let shapes = drawn_shapes(&gctx, &mut guest);
    let r = guest.node_rects[&nodes[0]];
    let chips: Vec<egui::Rect> = shapes.iter().filter_map(|s| match s {
        egui::Shape::Rect(x) if (x.fill == host_color || x.fill == egui::Color32::from_rgb(0x86, 0xC7, 0xF2)) && (x.rect.bottom() - (r.top() - 4.0)).abs() < 0.01 => Some(x.rect),
        _ => None,
    }).collect();
    assert_eq!(chips.len(), 2, "{chips:?}");
    assert!((chips[1].left() - chips[0].right() - 2.0).abs() < 0.01 || (chips[0].left() - chips[1].right() - 2.0).abs() < 0.01, "side by side: {chips:?}");

    // Deselecting clears it.
    host.state.selected_nodes.clear();
    frames(&hctx, &mut host, 8);
    frames(&gctx, &mut guest, 2);
    assert_eq!(guest.room.selections.get(&0).map(|s| s.1.len()), Some(0));
}

/// Someone (by id) selecting `node` on `doc`, arriving at the guest.
fn net_select(net: &Rc<RefCell<FakeNet>>, id: u32, node: egui_node_graph2::NodeId, doc: u32) {
    let r = crate::room_extras::node_ref(node).unwrap();
    net.borrow_mut().guest.push_back(room::RoomEvent::Message(room::Wire::Selection { id, doc, nodes: vec![r] }));
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
    app.planner_mut().enabled = g.recipes.values().filter(|r| !r.is_generator()).map(|r| r.class.clone()).collect();
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
    press(&ctx, &mut app, "Extract now");
    let w = app.wizard.as_ref().expect("icons step opens");
    assert!(w.icons_only && w.step() == crate::setup::Step::Icons);
    timed_frames(&ctx, &mut app, 8);
    press(&ctx, &mut app, "Done");
    assert!(app.wizard.is_none());
    assert!(button("Extract now").is_none(), "the notice goes away");
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

// ------------------------------------------------------------ progress

#[test]
fn recipes_from_a_save_replace_the_choices_and_undo() {
    let hook: RoomHook = Rc::new(|cmd: &str, _args: &Value| match cmd {
        "list_saves" => Some(Ok(json!([]))),
        "pick_save_game" => Some(Ok(json!("D:/Saves/early.sav"))),
        "read_save_unlocks" => Some(Ok(serde_json::to_value(fb_model::saves::SaveUnlocks {
            save: fb_model::saves::SaveSummary { session: "speedrun gods".into(), ..Default::default() },
            schematics: vec![
                "/Game/FactoryGame/Schematics/Schematic_StartingRecipes.Schematic_StartingRecipes_C".into(),
                "/Game/FactoryGame/Schematics/Alternate/Parts/Schematic_Alternate_Screw.Schematic_Alternate_Screw_C".into(),
            ],
        })
        .unwrap())),
        _ => None,
    });
    let Some((ctx, mut app, _)) = harness_with(Some(hook)) else { return };
    frames(&ctx, &mut app, 3);
    let before = app.planner_mut().enabled.clone();
    app.open_saves_dialog();
    frames(&ctx, &mut app, 2);
    assert!(matches!(app.dialog(), crate::dialogs::Dialog::Saves(_)));
    press(&ctx, &mut app, "Browse…");
    frames(&ctx, &mut app, 2);
    assert!(matches!(app.dialog(), crate::dialogs::Dialog::None), "closes once applied");
    let enabled = app.planner_mut().enabled.clone();
    assert!(enabled.contains("Recipe_IngotIron_C") && enabled.contains("Recipe_Alternate_Screw_C"));
    assert!(!enabled.contains("Recipe_Motor_C"), "nothing the save hasn't unlocked");
    app.undo();
    frames(&ctx, &mut app, 1);
    assert_eq!(app.planner_mut().enabled, before, "undo brings the old choices back");
}

// ------------------------------------------------------------------ power

/// Every piece of text drawn on one frame.
fn drawn_texts(ctx: &egui::Context, app: &mut FactoryApp) -> Vec<String> {
    fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
        match shape {
            egui::Shape::Text(t) => out.push(t.galley.text().to_string()),
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
            _ => {}
        }
    }
    crate::widgets::TEST_BUTTONS.with(|b| b.borrow_mut().clear());
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 960.0))),
        focused: true,
        ..Default::default()
    };
    let out = ctx.run(input, |ctx| app.ui(ctx));
    let mut texts = Vec::new();
    for s in &out.shapes {
        walk(&s.shape, &mut texts);
    }
    texts
}

#[test]
fn generate_power_plans_coal_generators() {
    let Some((ctx, mut app, store)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    assert_eq!(app.planner_mut().power.mode, PowerMode::Grid, "new factories use the grid");
    app.planner_mut().targets = vec![Target { item: "Desc_IronPlate_C".into(), rate: 60.0 }];
    press(&ctx, &mut app, "Generate it here");
    assert_eq!(app.planner_mut().power.mode, PowerMode::Generate);
    assert_eq!(app.planner_mut().power.generators, vec![DEFAULT_GENERATOR.to_string()]);
    press(&ctx, &mut app, "Plan factory");
    frames(&ctx, &mut app, 4);
    // Ore, smelters, constructors, output + generators, coal, water, and
    // an extractor for each of the three resources.
    assert_eq!(app.node_count(), 10, "{:?}", app.node_labels());
    assert!(app.node_labels().contains(&"Coal-Powered Generator · Coal".to_string()), "{:?}", app.node_labels());
    assert!((app.min_efficiency() - 1.0).abs() < 1e-6, "generators get all their coal and water");
    // 3 Smelters + 3 Constructors at 4 MW = 24 MW, plus the extractors; the
    // generators make at least all of it.
    let made = app.generation().expect("the graph has generators");
    assert!(app.power() > 24.0, "the extractors' power counts: {}", app.power());
    assert!(made >= app.power() - 1e-6, "generated {made} for {}", app.power());
    let texts = drawn_texts(&ctx, &mut app);
    assert!(texts.iter().any(|t| *t == format!("Produces {} MW", crate::fmt::fmt_num(made))), "generator node shows its MW");
    assert!(texts.iter().any(|t| t.starts_with("Power production")), "overview shows production");

    // Saved with the project and in the portable file (only what differs).
    app.save_as("power.fbfactory");
    frames(&ctx, &mut app, 2);
    let text = store.borrow().get("power.fbfactory").cloned().expect("written");
    let f = crate::portable::FactoryFile::parse(&text).unwrap();
    assert!(f.plan.generate_power);
    assert_eq!(f.plan.generators, None, "default generator list isn't stored");
    let project: serde_json::Value = serde_json::from_str(&app.project_json()).unwrap();
    assert_eq!(project["planner"]["power"]["mode"], "Generate");

    let (ctx2, mut other, _) = harness().unwrap();
    frames(&ctx2, &mut other, 2);
    other.open_factory(&ctx2, f, None);
    frames(&ctx2, &mut other, 3);
    assert_eq!(other.planner_mut().power, app.planner_mut().power);
    assert!((other.generation().unwrap_or(0.0) - made).abs() < 1e-6);
}

#[test]
fn power_settings_round_trip_and_old_files_use_the_grid() {
    let Some((ctx, mut app, _)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    let nuclear = "Generator__Build_GeneratorNuclear_C__Desc_NuclearFuelRod_C".to_string();
    app.planner_mut().power = PowerSettings { mode: PowerMode::Generate, generators: vec![nuclear.clone(), "Not_A_Generator".into()] };
    let f = app.to_portable();
    assert_eq!(f.plan.generators.as_deref(), Some(&[nuclear.clone(), "Not_A_Generator".to_string()][..]));
    let (ctx2, mut other, _) = harness().unwrap();
    frames(&ctx2, &mut other, 2);
    other.open_factory(&ctx2, f, None);
    frames(&ctx2, &mut other, 1);
    assert_eq!(other.planner_mut().power.generators, vec![nuclear], "unknown classes are dropped");

    // A version 1 file from before power plants.
    let old = r#"{"format":"factory-builder","version":1,"plan":{"products":[{"item":"Desc_IronPlate_C","rate":10}],"optimize":"Resources"},"nodes":[],"wires":[]}"#;
    let f = crate::portable::FactoryFile::parse(old).unwrap();
    other.open_factory(&ctx2, f, None);
    frames(&ctx2, &mut other, 1);
    assert_eq!(other.planner_mut().power, PowerSettings::default());
    // An old project (no "power" in the planner) loads too.
    let planner: crate::app::Planner = serde_json::from_str(r#"{"targets":[],"inputs":[],"objective":"Power","enabled":[],"limits":{}}"#).unwrap();
    assert_eq!(planner.power.mode, PowerMode::Grid);
}

/// Where `label` was drawn on one frame.
fn text_rect(ctx: &egui::Context, app: &mut FactoryApp, label: &str) -> Option<egui::Rect> {
    fn walk(shape: &egui::Shape, label: &str, out: &mut Option<egui::Rect>) {
        match shape {
            egui::Shape::Text(t) if t.galley.text() == label => *out = Some(t.visual_bounding_rect()),
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, label, out)),
            _ => {}
        }
    }
    crate::widgets::TEST_BUTTONS.with(|b| b.borrow_mut().clear());
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 960.0))),
        focused: true,
        ..Default::default()
    };
    let out = ctx.run(input, |ctx| app.ui(ctx));
    let mut found = None;
    for s in &out.shapes {
        walk(&s.shape, label, &mut found);
    }
    found
}

#[test]
fn generator_list_picks_fuels() {
    let Some((ctx, mut app, _)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    assert!(text_rect(&ctx, &mut app, "Generators").is_none(), "no generator list while using the grid");
    press(&ctx, &mut app, "Generate it here");
    let header = text_rect(&ctx, &mut app, "Generators").expect("generator list");
    assert!(text_rect(&ctx, &mut app, "Coal-Powered Generator · Coal").is_some(), "closed list sums up the choice");
    click(&ctx, &mut app, header.center(), egui::PointerButton::Primary);
    assert!(text_rect(&ctx, &mut app, "Nuclear Power Plant").is_some(), "generators are listed by building");
    let row = text_rect(&ctx, &mut app, "Compacted Coal").expect("fuel row");
    click(&ctx, &mut app, row.center(), egui::PointerButton::Primary);
    let mut want = vec![DEFAULT_GENERATOR.to_string(), "Generator__Build_GeneratorCoal_C__Desc_CompactedCoal_C".to_string()];
    want.sort();
    assert_eq!(app.planner_mut().power.generators, want);
    let g = fb_engine::docs::load_game_data(std::path::Path::new(&game_dir())).unwrap();
    let total = g.recipes.values().filter(|r| r.is_generator() && !g.hand_fed_generators.contains(&r.class)).count();
    assert!(text_rect(&ctx, &mut app, &format!("2 of {total} allowed")).is_some());
    press(&ctx, &mut app, "From the grid");
    assert_eq!(app.planner_mut().power.mode, PowerMode::Grid);
    assert_eq!(app.planner_mut().power.generators.len(), 2, "the choice is kept for next time");
}

// ------------------------------------------------------- belts and shards

fn plan_plates(ctx: &egui::Context, app: &mut FactoryApp, rate: f64) {
    app.planner_mut().targets = vec![Target { item: "Desc_IronPlate_C".into(), rate }];
    app.press_plan();
    frames(ctx, app, 8);
}

#[test]
fn arrows_move_along_wires_that_carry_something() {
    let Some((ctx, mut app, _)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    plan_plates(&ctx, &mut app, 30.0);
    let arrow = |s: &egui::Shape| matches!(s, egui::Shape::Path(p) if p.closed && p.points.len() == 3 && p.fill == egui::Color32::from_white_alpha(190));
    let first: Vec<egui::Pos2> = drawn_shapes(&ctx, &mut app).iter().filter(|s| arrow(s)).filter_map(|s| if let egui::Shape::Path(p) = s { Some(p.points[0]) } else { None }).collect();
    assert!(!first.is_empty(), "arrows on the plan's wires");
    // Off in the View menu: none.
    app.flow_arrows = false;
    assert!(!drawn_shapes(&ctx, &mut app).iter().any(arrow));
}

#[test]
fn wires_name_the_belt_they_need() {
    let Some((ctx, mut app, _)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    plan_plates(&ctx, &mut app, 60.0);
    // 90 ingots/min need a Mk.2 (120); 60 plates/min fit a Mk.1 (60). Each
    // label gives the belt's speed under it.
    let texts = app.wire_label_texts.clone();
    assert!(texts.contains(&"Mk.2\n120/min".to_string()), "{texts:?}");
    assert!(texts.contains(&"Mk.1\n60/min".to_string()), "{texts:?}");
    plan_plates(&ctx, &mut app, 3000.0);
    // 4500 ingots/min is more than one Mk.6 (1200) carries: 4 in parallel
    // (zoomed out this far, the label has room for the tier only).
    assert!(app.wire_label_texts.iter().any(|t| t.starts_with("4× Mk.6")), "{:?}", app.wire_label_texts);
    app.wire_labels = false;
    frames(&ctx, &mut app, 1);
    assert!(app.wire_label_texts.is_empty() || !app.wire_labels);
}

#[test]
fn somersloops_and_shards_change_output_power_and_totals() {
    let Some((ctx, mut app, _)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    plan_plates(&ctx, &mut app, 60.0);
    let plates = app
        .graph()
        .iter_nodes()
        .find(|id| matches!(&app.graph()[*id].user_data.kind, crate::graph::NodeKind::Recipe { recipe } if recipe == "Recipe_IronPlate_C"))
        .unwrap();
    let game = app.user.game.clone();
    let constructor = &game.buildings["Build_ConstructorMk1_C"];
    assert_eq!((constructor.sloop_slots, constructor.sloop_boost), (1, 1.0), "from the Docs");
    assert_eq!(game.buildings["Build_SmelterMk1_C"].sloop_slots, 1, "the Smelter takes one (wiki)");
    let out_before = crate::graph::node_rates(&game, app.graph(), plates).1[0].1;
    let power_before = app.power();
    let set = |app: &mut FactoryApp, name: &str, v: f64| {
        let input = app.state.graph[plates].get_input(name).unwrap();
        app.state.graph[input].value.set_number(v);
    };
    set(&mut app, crate::graph::SLOOPS, 1.0);
    frames(&ctx, &mut app, 2);
    let (ins, outs) = crate::graph::node_rates(&game, app.graph(), plates);
    assert!((outs[0].1 - 2.0 * out_before).abs() < 1e-9, "one Somersloop doubles a Constructor's output");
    assert!((ins[0].1 - 90.0).abs() < 1e-9, "inputs stay the same");
    let machines = crate::graph::input_number(app.graph(), plates, crate::graph::MACHINES);
    let whole = (machines - 1e-6).ceil();
    assert_eq!(app.summary.somersloops as f64, whole);
    // Power: x (1 + 1)^2 = 4 for those Constructors (4 MW each at 100%).
    let added = app.power() - power_before;
    assert!((added - whole * 4.0 * 3.0).abs() < 1e-6, "added {added} MW");
    set(&mut app, crate::graph::CLOCK, 250.0);
    set(&mut app, crate::graph::MACHINES, 1.0);
    frames(&ctx, &mut app, 2);
    assert_eq!(app.summary.power_shards, 3, "one machine at 250% takes 3 Power Shards");
}

// ------------------------------------------------------------------ map

fn sample_world() -> (fb_model::world::World, fb_model::world::MapInfo) {
    use fb_model::world::*;
    let m = |id: u32, class: &str, recipe: Option<&str>, x: f32, node: Option<&str>| Machine {
        id,
        class: class.into(),
        recipe: recipe.map(Into::into),
        clock: 1.0,
        boost: 1.0,
        x,
        y: 50_000.0,
        z: 0.0,
        yaw: 0.0,
        node: node.map(Into::into),
    };
    let world = World {
        save: fb_model::saves::SaveSummary { path: "D:/Saves/test.sav".into(), session: "Test".into(), name: "Test".into(), ..Default::default() },
        machines: vec![
            m(0, "Build_MinerMk1_C", None, 40_000.0, Some("BP_ResourceNode1")),
            m(1, "Build_SmelterMk1_C", Some("Recipe_IngotIron_C"), 45_000.0, None),
            m(2, "Build_SmelterMk1_C", Some("Recipe_IngotIron_C"), 46_000.0, None),
            m(3, "Build_ConstructorMk1_C", Some("Recipe_IronPlate_C"), 50_000.0, None),
            m(4, "Build_ConstructorMk1_C", Some("Recipe_IronPlate_C"), 51_000.0, None),
            m(5, "Build_ConstructorMk1_C", Some("Recipe_IronPlate_C"), 52_000.0, None),
            // Outside the selection in the import test.
            m(6, "Build_AssemblerMk1_C", Some("Recipe_IronPlateReinforced_C"), 90_000.0, None),
        ],
        links: vec![
            Link { from: 0, to: 1, item: Some("Desc_OreIron_C".into()) },
            Link { from: 0, to: 2, item: Some("Desc_OreIron_C".into()) },
            Link { from: 1, to: 3, item: Some("Desc_IronIngot_C".into()) },
            Link { from: 2, to: 4, item: Some("Desc_IronIngot_C".into()) },
            Link { from: 2, to: 5, item: Some("Desc_IronIngot_C".into()) },
            Link { from: 5, to: 6, item: Some("Desc_IronPlate_C".into()) },
        ],
        ..Default::default()
    };
    let map = MapInfo {
        x_min: 0.0,
        y_min: 0.0,
        size: 100_000.0,
        tile_px: 1024,
        levels: 1,
        nodes: vec![MapNode { id: "BP_ResourceNode1".into(), kind: SiteKind::Node, resource: Some("Desc_OreIron_C".into()), purity: fb_model::Purity::Pure, x: 40_000.0, y: 50_000.0, z: 0.0 }],
    };
    (world, map)
}

#[test]
fn importing_groups_machines_by_recipe() {
    let Some((ctx, mut app, _)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    let game = app.user.game.clone();
    let (world, map) = sample_world();
    let selected: std::collections::BTreeSet<u32> = (0..=5).collect();
    let (f, report) = crate::map_import::import(&world, &selected, &game, Some(&map));
    assert_eq!(report.machines, 6);
    let find = |kind: crate::portable::Kind, class: &str| f.nodes.iter().position(|n| n.kind == kind && n.class == class).unwrap_or_else(|| panic!("{class}"));
    let ingots = find(crate::portable::Kind::Recipe, "Recipe_IngotIron_C");
    let plates = find(crate::portable::Kind::Recipe, "Recipe_IronPlate_C");
    let ore = find(crate::portable::Kind::Resource, "Desc_OreIron_C");
    let out = find(crate::portable::Kind::Output, "Desc_IronPlate_C");
    assert_eq!(f.nodes[ingots].machines, Some(2.0));
    assert_eq!(f.nodes[plates].machines, Some(3.0));
    // Miner Mk.1 (60/min from the Docs) on a Pure node (x2) at 100%.
    assert_eq!(f.nodes[ore].rate, Some(120.0));
    // The miner itself: one, on a Pure node, at its clock speed.
    let miner = find(crate::portable::Kind::Extractor, "Build_MinerMk1_C");
    let m = &f.nodes[miner];
    assert_eq!((m.item.as_deref(), m.purity, m.machines, m.clock), (Some("Desc_OreIron_C"), Some(fb_model::Purity::Pure), Some(1.0), Some(100.0)));
    assert_eq!(f.nodes.len(), 5, "ore, its miner, ingots, plates, and the plates leaving the selection");
    let wire = |from: usize, to: usize| f.wires.iter().any(|w| w.from == from && w.to == to);
    assert!(wire(ore, miner) && wire(miner, ingots) && wire(ingots, plates) && wire(plates, out));
}

#[test]
fn map_tab_selects_and_imports() {
    let (world, map) = sample_world();
    let hook: RoomHook = Rc::new(move |cmd: &str, _args: &Value| match cmd {
        "map_info" => Some(Ok(serde_json::to_value(&map).unwrap())),
        "map_tile" => Some(Err("no tiles in tests".into())),
        "world_sessions" => Some(Ok(json!([{ "session": "Test", "saves": [world.save.clone()] }]))),
        "world_load" => Some(Ok(serde_json::to_value(&world).unwrap())),
        "world_latest" => Some(Ok(serde_json::to_value(&world.save).unwrap())),
        _ => None,
    });
    let Some((ctx, mut app, _)) = harness_with(Some(hook)) else { return };
    frames(&ctx, &mut app, 2);
    press(&ctx, &mut app, "Map");
    frames(&ctx, &mut app, 3);
    assert_eq!(app.view, crate::map::View::Map);
    assert!(app.map.world.is_some(), "the newest save of the first session loads");
    assert!(!button("Import into planner").unwrap().1, "nothing selected yet");
    // Drag across the whole map area (below the tabs and toolbar).
    let r = app.map.rect.expect("map drawn");
    drag(&ctx, &mut app, r.min + egui::vec2(5.0, 5.0), r.max - egui::vec2(5.0, 5.0));
    assert_eq!(app.map.selected.len(), 7, "every machine inside the box");
    let before = app.node_count();
    press(&ctx, &mut app, "Import into planner");
    assert_eq!(app.view, crate::map::View::Graph, "back to the graph to see the import");
    assert!(app.node_count() > before);
}

#[test]
fn a_missing_map_is_offered_for_extraction() {
    let calls: Calls = Default::default();
    let mut status = first_launch_status(true, false);
    status.settings.setup_done = true;
    status.icons_found = 747;
    status.ui_assets_found = 17;
    status.map_found = false;
    let Some((ctx, mut app, _)) = harness_with(Some(setup_hook(status, calls))) else { return };
    frames(&ctx, &mut app, 3);
    press(&ctx, &mut app, "Extract now");
    let w = app.wizard.as_ref().expect("the extraction step opens");
    assert!(w.icons_only && w.step() == crate::setup::Step::Icons);
}

// ------------------------------------------------------------ zoom space

/// Room cursors and shared node positions use graph coordinates
/// (screen = editor min + pan + graph x zoom). Zooming must keep every
/// node's graph position, or people at different zooms disagree.
#[test]
fn zooming_keeps_graph_coordinates_stable() {
    let Some((ctx, mut app, _)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    plan_plates(&ctx, &mut app, 60.0);
    let graph_pos = |app: &FactoryApp| -> Vec<(egui_node_graph2::NodeId, egui::Pos2)> {
        let z = app.state.pan_zoom.zoom;
        app.state.node_positions.iter().map(|(id, p)| (id, (p.to_vec2() / z).to_pos2())).collect()
    };
    let before = graph_pos(&app);
    let z0 = app.state.pan_zoom.zoom;
    let rect = app.editor_rect;
    let centre_before = (rect.center() - rect.min - app.state.pan_zoom.pan) / z0;
    let wheel = |dy: f32| egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, dy), modifiers: Default::default() };
    frame_with(&ctx, &mut app, vec![egui::Event::PointerMoved(rect.center())]);
    for dy in [120.0, 120.0, -60.0] {
        frame_with(&ctx, &mut app, vec![wheel(dy)]);
        frames(&ctx, &mut app, 4);
    }
    let z1 = app.state.pan_zoom.zoom;
    assert!((z1 - z0).abs() > 0.05, "the wheel zoomed ({z0} -> {z1})");
    for ((id, a), (_, b)) in before.iter().zip(graph_pos(&app)) {
        assert!((*a - b).length() < 0.01, "{id:?} moved in graph space: {a:?} -> {b:?}");
    }
    let centre_after = (rect.center() - rect.min - app.state.pan_zoom.pan) / z1;
    assert!((centre_before - centre_after).length() < 0.5, "zoomed about the centre of the view");
    // A cursor sent at a node's corner lands on that node on screen.
    let (id, g) = graph_pos(&app)[0];
    let screen = rect.min + app.state.pan_zoom.pan + g.to_vec2() * z1;
    let drawn = app.node_rects[&id].min;
    assert!((screen - drawn).length() < 1.0, "{screen:?} vs {drawn:?}");
}


// ------------------------------------------------------------ projects

/// Smelter(s) making Iron Ingots from ore, delivered as an output.
fn ingot_factory(name: &str, smelters: f64) -> crate::portable::FactoryFile {
    use crate::portable::{FactoryFile, Kind, Node, Wire};
    let node = |kind, class: &str, machines: Option<f64>, rate: Option<f64>, x: f32| Node { kind, class: class.into(), machines, clock: machines.map(|_| 100.0), sloops: None, rate, name: None, color: None, item: None, purity: None, x, y: 0.0, planned: false, router: None };
    let mut f = FactoryFile::new(Some(name.into()));
    f.nodes.push(node(Kind::Resource, "Desc_OreIron_C", None, Some(30.0 * smelters), 0.0));
    f.nodes.push(node(Kind::Recipe, "Recipe_IngotIron_C", Some(smelters), None, 400.0));
    f.nodes.push(node(Kind::Output, "Desc_IronIngot_C", None, Some(30.0 * smelters), 800.0));
    f.wires.push(Wire { from: 0, output: 0, to: 1, input: 0, slot: 0, belt: None });
    f.wires.push(Wire { from: 1, output: 0, to: 2, input: 0, slot: 0, belt: None });
    f
}

/// Constructors turning 60 Iron Ingots from elsewhere into 60 Iron Rods.
fn rod_factory(name: &str) -> crate::portable::FactoryFile {
    use crate::portable::{FactoryFile, Kind, Node, Wire};
    let node = |kind, class: &str, machines: Option<f64>, rate: Option<f64>, x: f32| Node { kind, class: class.into(), machines, clock: machines.map(|_| 100.0), sloops: None, rate, name: None, color: None, item: None, purity: None, x, y: 0.0, planned: false, router: None };
    let mut f = FactoryFile::new(Some(name.into()));
    f.nodes.push(node(Kind::Input, "Desc_IronIngot_C", None, Some(60.0), 0.0));
    f.nodes.push(node(Kind::Recipe, "Recipe_IronRod_C", Some(4.0), None, 400.0));
    f.nodes.push(node(Kind::Output, "Desc_IronRod_C", None, Some(60.0), 800.0));
    f.wires.push(Wire { from: 0, output: 0, to: 1, input: 0, slot: 0, belt: None });
    f.wires.push(Wire { from: 1, output: 0, to: 2, input: 0, slot: 0, belt: None });
    f
}

/// Links factory `from`'s `item` output to factory `to`'s input on the
/// master graph (which must be open).
fn link(app: &mut FactoryApp, from: u32, to: u32, item: &str) {
    let g = &app.state.graph;
    let a = crate::project::block_of(g, from).expect("block for the source");
    let b = crate::project::block_of(g, to).expect("block for the target");
    let out = g[a].outputs.iter().map(|(_, o)| *o).find(|o| g[*o].typ.0 == item).expect("output port");
    let inp = g[b].inputs.iter().map(|(_, i)| *i).find(|i| g[*i].typ.0 == item).expect("input port");
    app.state.graph.add_connection(out, inp, 0);
    app.changed();
}

#[test]
fn master_graph_links_factories_and_flags_shortfalls() {
    use crate::project::MASTER;
    let Some((ctx, mut app, store)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    assert_eq!(app.factory_count(), 1, "a new document is a project with one factory");
    let first = app.active;
    app.add_factory_file(&ctx, &ingot_factory("Smelting", 1.0));
    let smelting = app.active;
    app.add_factory_file(&ctx, &rod_factory("Rods"));
    let rods = app.active;
    frames(&ctx, &mut app, 2);
    assert_eq!(app.factory_count(), 3);
    assert_eq!(app.tabs, vec![first, smelting, rods], "each added factory opens in a tab");
    assert_eq!(app.title(), "Untitled project*");

    press(&ctx, &mut app, "Project");
    frames(&ctx, &mut app, 2);
    assert_eq!(app.active, MASTER);
    assert_eq!(app.node_count(), 3, "one block per factory");
    assert!(app.node_labels().contains(&"Smelting".to_string()));
    // Unlinked, the rod factory's ingots come from outside the project.
    let t = app.project_totals();
    assert!((t.from_outside["Desc_IronIngot_C"] - 60.0).abs() < 1e-6);
    assert!(app.summary.issues.is_empty(), "{:?}", app.summary.issues);

    // In the rod factory, its Input isn't brought in from anywhere yet.
    let warning = |app: &mut FactoryApp| {
        use egui_node_graph2::NodeDataTrait;
        let g = &app.state.graph;
        let n = g.iter_nodes().find(|n| matches!(g[*n].user_data.kind, crate::graph::NodeKind::Input { .. })).unwrap();
        let data = g[n].user_data.clone();
        data.warning(n, &app.state.graph, &mut app.user).map(|w| w.1)
    };
    let input_warned = |app: &mut FactoryApp| warning(app).is_some();
    app.activate(rods);
    frames(&ctx, &mut app, 2);
    assert_eq!(warning(&mut app).as_deref(), Some("Nothing brings this in: on the master graph, link Iron Ingot into this factory from another one."));
    app.activate(MASTER);
    frames(&ctx, &mut app, 2);

    // One smelter sends 30 ingots; the rod factory needs 60.
    link(&mut app, smelting, rods, "Desc_IronIngot_C");
    frames(&ctx, &mut app, 2);
    let issues: Vec<String> = app.summary.issues.iter().map(|i| i.1.clone()).collect();
    assert!(issues.iter().any(|i| i.contains("Rods gets only 30 of 60")), "{issues:?}");
    let block = crate::project::block_of(&app.state.graph, rods).unwrap();
    assert!((app.user.nodes[&block].efficiency - 0.5).abs() < 1e-6, "short of half its ingots, it runs at half speed");
    let t = app.project_totals();
    assert!(!t.from_outside.contains_key("Desc_IronIngot_C"));
    assert!((t.products["Desc_IronRod_C"] - 30.0).abs() < 1e-6, "{:?}", t.products);
    app.activate(rods);
    frames(&ctx, &mut app, 2);
    assert_eq!(warning(&mut app).as_deref(), Some("Not enough comes in: 30 of 60 per min arrives from other factories."), "linked, but only half its ingots arrive");
    app.activate(MASTER);
    frames(&ctx, &mut app, 2);
    assert!((t.resources["Desc_OreIron_C"] - 30.0).abs() < 1e-6);

    // Two smelters cover it; edits in the factory flow back to its block.
    app.activate(smelting);
    let find = |app: &FactoryApp, f: fn(&crate::graph::NodeKind) -> bool| app.state.graph.iter_nodes().find(|n| f(&app.state.graph[*n].user_data.kind)).unwrap();
    let recipe = find(&app, |k| matches!(k, crate::graph::NodeKind::Recipe { .. }));
    // (The ore's miner was added when the factory came in: twice as many.)
    let miner = find(&app, |k| matches!(k, crate::graph::NodeKind::Extractor { .. }));
    let miners = crate::graph::input_number(&app.state.graph, miner, crate::graph::MACHINES);
    for (node, name, v) in [(recipe, crate::graph::MACHINES, 2.0), (miner, crate::graph::MACHINES, 2.0 * miners)] {
        let i = app.state.graph[node].get_input(name).unwrap();
        app.state.graph[i].value.set_number(v);
    }
    app.changed();
    frames(&ctx, &mut app, 2);
    press(&ctx, &mut app, "Project");
    frames(&ctx, &mut app, 2);
    assert!(app.summary.issues.is_empty(), "{:?}", app.summary.issues);
    assert!((app.project_totals().products["Desc_IronRod_C"] - 60.0).abs() < 1e-6);
    app.activate(rods);
    frames(&ctx, &mut app, 2);
    assert!(!input_warned(&mut app), "all 60 arrive: no warning");
    app.activate(MASTER);
    frames(&ctx, &mut app, 2);

    // Saved as a project (there's more than one factory) and opened again.
    let path = "C:/Factories/Base.fbproject";
    assert_eq!(app.save_kind(), crate::app_files::SavePurpose::Project);
    app.save_as(path);
    frames(&ctx, &mut app, 2);
    let text = store.borrow().get(path).cloned().expect("written");
    let p = crate::portable::ProjectFile::parse(&text).expect("a project file");
    assert_eq!(p.factories.len(), 3);
    assert_eq!(p.links.len(), 1);
    assert_eq!(p.links[0].item, "Desc_IronIngot_C");
    assert_eq!(app.title(), "Base");
    app.new_project();
    frames(&ctx, &mut app, 1);
    assert_eq!(app.factory_count(), 1);
    app.open(path);
    frames(&ctx, &mut app, 3);
    assert_eq!(app.factory_count(), 3);
    assert_eq!(app.active, MASTER, "a project opens on its master graph");
    assert_eq!(app.state.graph.iter_connections().count(), 1, "the link came back");
    assert!(app.summary.issues.is_empty(), "{:?}", app.summary.issues);
    assert!(!app.is_dirty());
}

/// `machines` Coal-Powered Generators fed coal and water brought in from
/// elsewhere (Input nodes; rates from the game's own recipe).
fn power_plant(app: &FactoryApp, name: &str, machines: f64) -> crate::portable::FactoryFile {
    use crate::portable::{FactoryFile, Kind, Node, Wire};
    let gen = fb_model::DEFAULT_GENERATOR;
    let r = &app.user.game.recipes[gen];
    let mut f = FactoryFile::new(Some(name.into()));
    f.nodes.push(Node { kind: Kind::Recipe, class: gen.into(), machines: Some(machines), clock: Some(100.0), sloops: None, rate: None, name: None, color: None, item: None, purity: None, x: 400.0, y: 0.0, planned: false, router: None });
    for (k, a) in r.ingredients.iter().enumerate() {
        f.nodes.push(Node { kind: Kind::Input, class: a.item.clone(), machines: None, clock: None, sloops: None, rate: Some(r.per_min(a) * machines), name: None, color: None, item: None, purity: None, x: 0.0, y: 200.0 * k as f32, planned: false, router: None });
        f.wires.push(Wire { from: k + 1, output: 0, to: 0, input: k, slot: 0, belt: None });
    }
    f
}

/// Power wires on the master graph (any direction), as (from node, to node).
fn power_wires(app: &FactoryApp) -> Vec<(egui_node_graph2::NodeId, egui_node_graph2::NodeId)> {
    let g = &app.state.graph;
    g.iter_connections().filter(|(i, _)| g[*i].typ.0 == crate::power_net::POWER).map(|(i, o)| (g[o].node, g[i].node)).collect()
}

/// Factory `factory`'s block's power input (`input`) or output, if shown.
fn power_port(app: &FactoryApp, factory: u32, input: bool) -> bool {
    let g = &app.state.graph;
    let b = crate::project::block_of(g, factory).unwrap();
    let is = |t: &String| t == crate::power_net::POWER;
    if input {
        g[b].inputs.iter().any(|(_, i)| is(&g[*i].typ.0))
    } else {
        g[b].outputs.iter().any(|(_, o)| is(&g[*o].typ.0))
    }
}

/// Wires factory `factory`'s power port to pole `pole` (whichever way it
/// goes), as dragging a wire does.
fn wire_to_pole(app: &mut FactoryApp, factory: u32, pole: u32) {
    let g = &app.state.graph;
    let b = crate::project::block_of(g, factory).unwrap();
    let p = crate::power_net::pole_node(g, pole).unwrap();
    let out = |n: egui_node_graph2::NodeId| g[n].outputs.iter().map(|(_, o)| *o).find(|o| g[*o].typ.0 == crate::power_net::POWER);
    let inp = |n: egui_node_graph2::NodeId| g[n].inputs.iter().map(|(_, i)| *i).find(|i| g[*i].typ.0 == crate::power_net::POWER);
    let (o, i) = match out(b) {
        Some(o) => (o, inp(p).unwrap()),
        None => (out(p).unwrap(), inp(b).unwrap()),
    };
    let pos = app.state.graph.connections(i).len();
    app.state.graph.add_connection(o, i, pos);
    app.changed();
}

#[test]
fn power_poles_show_which_factories_power_which() {
    use crate::power_net::{pole_node, MADE, PORT, USED};
    let Some((ctx, mut app, store)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    let plant_file = power_plant(&app, "Coal power", 2.0);
    app.add_factory_file(&ctx, &plant_file);
    let plant = app.active;
    app.add_factory_file(&ctx, &ingot_factory("Smelting", 1.0));
    let smelting = app.active;
    app.add_factory_file(&ctx, &rod_factory("Rods"));
    let rods = app.active;
    press(&ctx, &mut app, "Project");
    frames(&ctx, &mut app, 2);
    let made_mw = app.factory_info(plant).generation.expect("the plant makes power");
    let smelting_mw = app.factory_info(smelting).power_mw;
    let rods_mw = app.factory_info(rods).power_mw;
    assert!(made_mw > 0.0 && smelting_mw > 0.0 && rods_mw > 0.0);

    // Power ports: out of the plant (it makes more than it uses), into the others.
    assert!(power_port(&app, plant, false) && !power_port(&app, plant, true));
    assert!(power_port(&app, smelting, true) && !power_port(&app, smelting, false));
    assert!(power_wires(&app).is_empty(), "nothing wired yet");
    // Not getting power: outlined red.
    let outline = |app: &mut FactoryApp, f: u32| {
        use egui_node_graph2::NodeDataTrait;
        let b = crate::project::block_of(&app.state.graph, f).unwrap();
        let data = app.state.graph[b].user_data.clone();
        data.warning(b, &app.state.graph, &mut app.user).map(|w| w.0)
    };
    assert_eq!(outline(&mut app, smelting), Some(crate::theme::DANGER));
    {
        use egui_node_graph2::NodeDataTrait;
        let b = crate::project::block_of(&app.state.graph, smelting).unwrap();
        let data = app.state.graph[b].user_data.clone();
        let text = data.warning(b, &app.state.graph, &mut app.user).unwrap().1;
        assert_eq!(text, "Not getting power: its Power input isn't wired to a pole or a power plant.");
        // Hovering the block shows it.
        app.request_frame_all();
        frames(&ctx, &mut app, 2);
        let r = app.node_rects[&b];
        frame_with(&ctx, &mut app, vec![egui::Event::PointerMoved(r.center())]);
        let texts = drawn_texts(&ctx, &mut app);
        assert!(texts.iter().any(|t| *t == text), "the tip shows on hover");
    }
    assert_eq!(outline(&mut app, plant), None, "the plant needs none");

    // A pole from the canvas menu; the plant and the smelters wired to it.
    app.new_pole_at(egui::pos2(0.0, -400.0));
    frames(&ctx, &mut app, 2);
    assert_eq!(app.planner.power_net.poles.len(), 1);
    let pole = app.planner.power_net.poles[0].id;
    let node = pole_node(&app.state.graph, pole).expect("its node");
    assert_eq!(app.state.selected_nodes, vec![node]);
    wire_to_pole(&mut app, plant, pole);
    wire_to_pole(&mut app, smelting, pole);
    frames(&ctx, &mut app, 2);
    assert_eq!(app.planner.power_net.links.len(), 2);
    let stat = |app: &FactoryApp, n, name: &str| app.user.ports[&(n, name.to_string())];
    let node = pole_node(&app.state.graph, pole).unwrap();
    assert!((stat(&app, node, MADE).have - made_mw).abs() < 1e-6);
    assert!((stat(&app, node, USED).have - smelting_mw).abs() < 1e-6);
    let sb = crate::project::block_of(&app.state.graph, smelting).unwrap();
    let s = stat(&app, sb, PORT);
    assert!(s.connected && (s.have - s.want).abs() < 1e-9, "fully powered: {s:?}");
    assert_eq!(outline(&mut app, smelting), None, "powered: no warning");
    assert!(app.summary.issues.is_empty(), "{:?}", app.summary.issues);
    // Item flows don't see power wires.
    assert!(app.project_totals().from_outside.contains_key("Desc_IronIngot_C"));

    // View > Power connections off: no pole, no power ports, no power
    // wires; they're kept, and come back when it's turned on again.
    app.power_links = false;
    frames(&ctx, &mut app, 2);
    assert!(pole_node(&app.state.graph, pole).is_none());
    assert!(!power_port(&app, plant, false) && !power_port(&app, smelting, true));
    assert!(power_wires(&app).is_empty());
    assert_eq!(app.planner.power_net.links.len(), 2);
    app.power_links = true;
    frames(&ctx, &mut app, 2);
    assert_eq!(power_wires(&app).len(), 2);

    // Disconnecting takes the wire away; Undo puts it back.
    let g = &app.state.graph;
    let sb = crate::project::block_of(g, smelting).unwrap();
    let i = g[sb].inputs.iter().map(|(_, i)| *i).find(|i| g[*i].typ.0 == crate::power_net::POWER).unwrap();
    let o = g.connections(i)[0];
    app.state.graph.remove_connection(i, o);
    app.changed();
    frames(&ctx, &mut app, 2);
    assert_eq!(app.planner.power_net.links.len(), 1);
    app.undo();
    frames(&ctx, &mut app, 2);
    assert_eq!(app.planner.power_net.links.len(), 2);
    assert_eq!(power_wires(&app).len(), 2);

    // Short: the rods join, and the plant runs a fifth of one generator.
    wire_to_pole(&mut app, rods, pole);
    frames(&ctx, &mut app, 2);
    app.activate(plant);
    let gen = app.state.graph.iter_nodes().find(|n| matches!(app.state.graph[*n].user_data.kind, crate::graph::NodeKind::Recipe { .. })).unwrap();
    let i = app.state.graph[gen].get_input(crate::graph::MACHINES).unwrap();
    app.state.graph[i].value.set_number(0.2);
    app.changed();
    frames(&ctx, &mut app, 2);
    let small = app.factory_info(plant).generation.unwrap();
    assert!(small < smelting_mw + rods_mw, "{small} MW for {} MW", smelting_mw + rods_mw);
    press(&ctx, &mut app, "Project");
    frames(&ctx, &mut app, 2);
    let short = crate::fmt::fmt_num(smelting_mw + rods_mw - small);
    let issues: Vec<String> = app.summary.issues.iter().map(|i| i.1.clone()).collect();
    assert!(issues.iter().any(|i| *i == format!("Power for Rods and 1 more is {short} MW short")), "{issues:?}");
    let rods_tip = {
        use egui_node_graph2::NodeDataTrait;
        let b = crate::project::block_of(&app.state.graph, rods).unwrap();
        let data = app.state.graph[b].user_data.clone();
        data.warning(b, &app.state.graph, &mut app.user).map(|w| w.1)
    };
    assert!(rods_tip.as_deref().is_some_and(|t| t.starts_with("Short of power: gets ")), "{rods_tip:?}");
    let rb = crate::project::block_of(&app.state.graph, rods).unwrap();
    let s = stat(&app, rb, PORT);
    assert!((s.have - rods_mw * small / (smelting_mw + rods_mw)).abs() < 1e-6, "its share of what's made: {s:?}");
    // However many wires, a pole has one bubble each side.
    let g = &app.state.graph;
    let node = pole_node(g, pole).unwrap();
    let out = g[node].outputs[0].1;
    assert_eq!(g.output_connections(out).len(), 2);
    app.request_frame_all();
    frames(&ctx, &mut app, 2);
    assert_eq!(app.port_hooks[&egui_node_graph2::AnyParameterId::Output(out)].len(), 1);
    // Selecting a factory lights its whole circuit.
    app.state.selected_nodes = vec![rb];
    frames(&ctx, &mut app, 1);
    assert_eq!(app.user.power_lit.len(), 3);
    app.state.selected_nodes.clear();
    frames(&ctx, &mut app, 1);
    assert!(app.user.power_lit.is_empty());

    // Saved and opened again: the pole and the wires come back.
    let path = "C:/Factories/Power.fbproject";
    app.save_as(path);
    frames(&ctx, &mut app, 2);
    let text = store.borrow().get(path).cloned().expect("written");
    let p = crate::portable::ProjectFile::parse(&text).unwrap();
    assert_eq!((p.poles.len(), p.power_links.len()), (1, 3));
    app.new_project();
    frames(&ctx, &mut app, 1);
    app.open(path);
    frames(&ctx, &mut app, 3);
    assert_eq!(power_wires(&app).len(), 3);
    assert!(!app.is_dirty(), "opening isn't an edit");

    // Deleting the pole deletes its wires (no question asked; Undo).
    let pole = app.planner.power_net.poles[0].id;
    let node = pole_node(&app.state.graph, pole).unwrap();
    app.state.selected_nodes = vec![node];
    app.delete_selected();
    frames(&ctx, &mut app, 2);
    assert!(app.planner.power_net.poles.is_empty() && app.planner.power_net.links.is_empty());
    assert!(power_wires(&app).is_empty());
    assert!(matches!(app.dialog, crate::dialogs::Dialog::None));
    app.undo();
    frames(&ctx, &mut app, 2);
    assert_eq!(power_wires(&app).len(), 3);
}

/// Wires `from`'s `item` output to `to`'s `item` input (as dragging would).
fn connect(app: &mut FactoryApp, from: egui_node_graph2::NodeId, to: egui_node_graph2::NodeId, item: &str) -> (egui_node_graph2::InputId, egui_node_graph2::OutputId) {
    let g = &app.state.graph;
    let o = g[from].outputs.iter().map(|(_, o)| *o).find(|o| g[*o].typ.0 == item).expect("output");
    let i = g[to].inputs.iter().map(|(_, i)| *i).find(|i| g[*i].typ.0 == item).expect("input");
    let pos = g.connections(i).len();
    app.state.graph.add_connection(o, i, pos);
    app.changed();
    (i, o)
}

#[test]
fn mergers_and_splitters_route_wires_without_changing_flows() {
    use crate::graph::{NodeKind, Template, MACHINES, RATE};
    let Some((ctx, mut app, _)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    let (ore, ingot) = ("Desc_OreIron_C", "Desc_IronIngot_C");
    let per_smelter = {
        let r = &app.user.game.recipes["Recipe_IngotIron_C"];
        (r.per_min(&r.ingredients[0]), r.per_min(&r.products[0]))
    };
    // Ore -> splitter -> two smelters -> merger -> output.
    let src = app.add_test_node(Template::Resource(ore.into()), egui::pos2(0.0, 0.0));
    let split = app.add_test_node(Template::Splitter(ore.into()), egui::pos2(300.0, 0.0));
    let s1 = app.add_test_node(Template::Recipe("Recipe_IngotIron_C".into()), egui::pos2(450.0, -200.0));
    let s2 = app.add_test_node(Template::Recipe("Recipe_IngotIron_C".into()), egui::pos2(450.0, 200.0));
    let merge = app.add_test_node(Template::Merger(ingot.into()), egui::pos2(850.0, 0.0));
    let out = app.add_test_node(Template::Output(ingot.into()), egui::pos2(1000.0, 0.0));
    let set = |app: &mut FactoryApp, n: egui_node_graph2::NodeId, name: &str, v: f64| {
        let i = app.state.graph[n].get_input(name).unwrap();
        app.state.graph[i].value.set_number(v);
    };
    set(&mut app, src, RATE, 2.0 * per_smelter.0);
    set(&mut app, out, RATE, 2.0 * per_smelter.1);
    set(&mut app, s1, MACHINES, 1.0);
    set(&mut app, s2, MACHINES, 1.0);
    let w_in = connect(&mut app, src, split, ore);
    let w_s1 = connect(&mut app, split, s1, ore);
    connect(&mut app, split, s2, ore);
    let w_m1 = connect(&mut app, s1, merge, ingot);
    connect(&mut app, s2, merge, ingot);
    let w_out = connect(&mut app, merge, out, ingot);
    frames(&ctx, &mut app, 2);
    assert!(app.summary.issues.is_empty(), "{:?}", app.summary.issues);
    for s in [s1, s2] {
        assert!((app.user.nodes[&s].efficiency - 1.0).abs() < 1e-4, "each smelter gets its ore through the splitter");
    }
    let got = app.outputs().iter().find(|o| o.0 == ingot).map(|o| o.1).unwrap();
    assert!((got - 2.0 * per_smelter.1).abs() < 1e-3, "both smelters' ingots arrive through the merger: {got}");
    let flow = |w: (egui_node_graph2::InputId, egui_node_graph2::OutputId)| app.wire_flow.get(&w).copied().unwrap_or(-1.0);
    assert!((flow(w_in) - 2.0 * per_smelter.0).abs() < 1e-3, "{}", flow(w_in));
    assert!((flow(w_s1) - per_smelter.0).abs() < 1e-3, "{}", flow(w_s1));
    assert!((flow(w_m1) - per_smelter.1).abs() < 1e-3, "{}", flow(w_m1));
    assert!((flow(w_out) - 2.0 * per_smelter.1).abs() < 1e-3, "{}", flow(w_out));

    // The game's limits: a merger joins three belts into one, a splitter
    // shares one out to three.
    use egui_node_graph2::{AnyParameterId as P, NodeDataTrait};
    let takes = |app: &mut FactoryApp, n: egui_node_graph2::NodeId, input: bool| {
        let g = &app.state.graph;
        let port = if input { P::Input(g[n].inputs[0].1) } else { P::Output(g[n].outputs[0].1) };
        let data = g[n].user_data.clone();
        data.accepts_wire(n, port, &app.state.graph, &mut app.user)
    };
    assert!(takes(&mut app, merge, true), "two of three belts in");
    assert!(!takes(&mut app, merge, false), "its one belt out is taken");
    assert!(!takes(&mut app, split, true) && takes(&mut app, split, false), "one belt in, two of three out");
    let s3 = app.add_test_node(Template::Recipe("Recipe_IngotIron_C".into()), egui::pos2(450.0, 600.0));
    connect(&mut app, split, s3, ore);
    assert!(!takes(&mut app, split, false), "three out is all");

    // The file format keeps them.
    let f = app.to_portable();
    let kinds: Vec<_> = f.nodes.iter().map(|n| n.kind).collect();
    assert!(kinds.contains(&crate::portable::Kind::Merger) && kinds.contains(&crate::portable::Kind::Splitter), "{kinds:?}");
    let back = crate::portable::FactoryFile::parse(&f.to_json(false)).unwrap();
    assert_eq!(back, f);
    // A fluid gets the game's one Pipeline Junction.
    let game = app.user.game.clone();
    let water = NodeKind::Merger { item: "Desc_Water_C".into() };
    assert_eq!((crate::routers::name(&game, &water), crate::routers::icon(&game, &water)), ("Pipeline Junction", "Desc_PipelineJunction_Cross_C"));
}

#[test]
fn plans_share_belts_and_pipes_through_mergers_splitters_and_junctions() {
    use crate::graph::NodeKind;
    let Some((ctx, mut app, _)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    // Heavy Modular Frames and Plastic with every recipe: belts and pipes
    // shared by several machines.
    app.planner_mut().targets = vec![Target { item: "Desc_ModularFrameHeavy_C".into(), rate: 4.0 }, Target { item: "Desc_Plastic_C".into(), rate: 30.0 }];
    app.planner_mut().enabled = app.user.game.recipes.keys().cloned().collect();
    app.press_plan();
    frames(&ctx, &mut app, 8);
    let g = app.graph();
    let game = app.user.game.clone();
    let router = |n: egui_node_graph2::NodeId| crate::routers::is_router(&g[n].user_data.kind);
    let (mut mergers, mut splitters, mut junctions) = (0, 0, 0);
    for n in g.iter_nodes() {
        let kind = &g[n].user_data.kind;
        let ins: usize = g[n].inputs.iter().map(|(_, i)| g.connections(*i).len()).sum();
        let outs: usize = g[n].outputs.iter().map(|(_, o)| g.output_connections(*o).len()).sum();
        match kind {
            NodeKind::Merger { item } | NodeKind::Splitter { item } if game.item_form(item).is_fluid() => {
                junctions += 1;
                assert!(ins + outs <= crate::routers::JUNCTION_PIPES && ins >= 1 && outs >= 1, "junction {ins} in {outs} out");
            }
            NodeKind::Merger { .. } => {
                mergers += 1;
                assert!((2..=crate::routers::MERGER_INPUTS).contains(&ins) && outs == 1, "merger {ins} in {outs} out");
            }
            NodeKind::Splitter { .. } => {
                splitters += 1;
                assert!(ins == 1 && (2..=crate::routers::SPLITTER_OUTPUTS).contains(&outs), "splitter {ins} in {outs} out");
            }
            _ => {
                // A machine's port never shares a wire with another machine's now.
                for (_, i) in &g[n].inputs {
                    assert!(g.connections(*i).len() <= 1 || g.connections(*i).iter().all(|o| router(g[*o].node)), "{} has a shared input", g[n].label);
                }
                for (_, o) in &g[n].outputs {
                    assert!(g.output_connections(*o).len() <= 1, "{} has a shared output", g[n].label);
                }
            }
        }
    }
    eprintln!("mergers {mergers}, splitters {splitters}, junctions {junctions}");
    assert!(mergers + splitters > 0 && junctions > 0, "belts and pipes both shared in this plan");
    // Same factory as before: everything delivered, nothing short.
    for (item, got, want) in app.outputs() {
        assert!((got - want).abs() < want * 1e-3 + 1e-6, "{item}: {got} of {want}");
    }
    assert!(app.summary.issues.is_empty(), "{:?}", app.summary.issues);
}

#[test]
fn dragging_from_one_selected_node_pulls_wires_from_all_of_them() {
    use crate::graph::Template;
    use egui_node_graph2::AnyParameterId;
    let Some((ctx, mut app, _)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    app.minimap = false;
    let at = |app: &FactoryApp, x: f32, y: f32| app.screen_to_graph(egui::pos2(x, y));
    let p = at(&app, 420.0, 100.0);
    let a = app.add_test_node(Template::Recipe("Recipe_IngotIron_C".into()), p);
    let p = at(&app, 420.0, 420.0);
    let b = app.add_test_node(Template::Recipe("Recipe_IngotIron_C".into()), p);
    let p = at(&app, 950.0, 300.0);
    let merger = app.add_test_node(Template::Merger("Desc_IronIngot_C".into()), p);
    frames(&ctx, &mut app, 3);
    app.state.selected_nodes = vec![a, b];
    let ingots = |app: &FactoryApp, n: egui_node_graph2::NodeId| app.graph()[n].outputs[0].1;
    let ring = *app.port_hooks[&AnyParameterId::Output(ingots(&app, a))].last().unwrap();
    // Onto a merger: both smelters' ingots go in (it takes three belts).
    let body = app.node_rects[&merger].center();
    drag(&ctx, &mut app, ring, body);
    let g = app.graph();
    let input = g[merger].inputs[0].1;
    let mut got = g.connections(input);
    got.sort();
    let mut want = vec![ingots(&app, a), ingots(&app, b)];
    want.sort();
    assert_eq!(got, want, "a wire from each selected smelter");

    // Onto empty canvas: Add node, and the new node gets both wires.
    for o in want {
        app.state.graph.remove_connection(input, o);
    }
    frames(&ctx, &mut app, 2);
    app.state.selected_nodes = vec![a, b];
    frames(&ctx, &mut app, 1);
    let ring = *app.port_hooks[&AnyParameterId::Output(ingots(&app, a))].last().unwrap();
    drag(&ctx, &mut app, ring, egui::pos2(ring.x + 60.0, 900.0));
    let wire = app.finder.as_ref().and_then(|f| f.wire.clone()).expect("Add node opened");
    assert_eq!(wire.extras, vec![ingots(&app, b)]);
    app.finder.as_mut().unwrap().query = "Iron Ingot merger".into();
    frames(&ctx, &mut app, 1);
    press_key(&ctx, &mut app, egui::Key::Enter);
    frames(&ctx, &mut app, 2);
    let g = app.graph();
    let new = *app.selected().first().expect("the new node is selected");
    assert!(matches!(g[new].user_data.kind, crate::graph::NodeKind::Merger { .. }), "{:?}", g[new].user_data.kind);
    assert_eq!(g.connections(g[new].inputs[0].1).len(), 2, "both smelters wired to it");
}

#[test]
fn extractors_and_pumps_are_buildings_with_power() {
    use crate::graph::{NodeKind, Template, RATE};
    let Some((ctx, mut app, _)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    let texts = drawn_texts(&ctx, &mut app);
    assert!(texts.iter().any(|t| t == "Outputs") && texts.iter().any(|t| t == "Inputs"), "the plan's sections are named like the nodes");
    assert!(!texts.iter().any(|t| t == "What to make" || t == "Already available"));
    app.planner_mut().targets = vec![Target { item: "Desc_IronPlate_C".into(), rate: 60.0 }];
    app.press_plan();
    frames(&ctx, &mut app, 4);
    let game = app.user.game.clone();
    let g = app.graph();
    let miner = g.iter_nodes().find(|n| matches!(g[*n].user_data.kind, NodeKind::Extractor { .. })).expect("the plan has an extractor");
    let NodeKind::Extractor { item, class, purity } = g[miner].user_data.kind.clone() else { unreachable!() };
    assert_eq!((item.as_str(), class.as_str(), purity), ("Desc_OreIron_C", "Build_MinerMk1_C", fb_model::Purity::Normal), "from the default settings");
    let power_before = app.power();

    // Its menu: a Miner Mk.2 instead (twice the output each, so the same
    // ore needs half the work), then a Pure node.
    app.context_menu = Some((miner, egui::pos2(600.0, 300.0)));
    app.context_targets = vec![miner];
    frames(&ctx, &mut app, 1);
    press(&ctx, &mut app, "    Miner Mk.2");
    frames(&ctx, &mut app, 2);
    assert!(matches!(&app.graph()[miner].user_data.kind, NodeKind::Extractor { class, .. } if class == "Build_MinerMk2_C"));
    app.context_menu = Some((miner, egui::pos2(600.0, 300.0)));
    app.context_targets = vec![miner];
    frames(&ctx, &mut app, 1);
    // Purity is the resource node's (the miner's menu no longer has it).
    assert!(!crate::widgets::TEST_BUTTONS.with(|b| b.borrow().iter().any(|x| x.0 == "    Pure")));
    app.context_menu = None;
    frames(&ctx, &mut app, 1);
    press(&ctx, &mut app, "Pure");
    frames(&ctx, &mut app, 2);
    assert!(app.graph().iter_nodes().any(|n| matches!(app.graph()[n].user_data.kind, NodeKind::Resource { purity: fb_model::Purity::Pure, .. })));
    assert_eq!(crate::extractors::purity_of(app.graph(), miner), fb_model::Purity::Pure, "the miner uses its resource's purity");
    assert!(app.power() != power_before, "the extractor's power follows its building");
    // The resource's wire into the miner isn't a belt: no belt label on it,
    // and the resource takes more extractors without a splitter.
    let g = app.graph();
    let ore = g.iter_nodes().find(|n| matches!(g[*n].user_data.kind, NodeKind::Resource { .. })).unwrap();
    let out = g[ore].outputs[0].1;
    let into_miner = g.output_connections(out)[0];
    assert!(crate::extractors::from_the_ground(g, out, into_miner));
    let after_miner = g.output_connections(g[miner].outputs[0].1)[0];
    assert!(!crate::extractors::from_the_ground(g, g[miner].outputs[0].1, after_miner), "what it puts out is a belt");
    {
        use egui_node_graph2::{AnyParameterId as P, NodeDataTrait};
        let data = app.state.graph[ore].user_data.clone();
        assert!(data.accepts_wire(ore, P::Output(out), &app.state.graph, &mut app.user));
    }
    app.request_frame_all();
    frames(&ctx, &mut app, 2);
    let flow = app.wire_flow[&(into_miner, out)];
    let belt = game.extraction.carrier(false, flow).unwrap().kind.name.clone();
    let short = belt.rsplit(' ').next().unwrap().to_string();
    let labels = app.wire_label_texts.clone();
    let belt_out = app.wire_flow[&(after_miner, app.graph()[miner].outputs[0].1)];
    eprintln!("ore {flow} needs {short}; labels {labels:?}; out of the miner {belt_out}");
    assert_eq!(labels.len(), app.graph().iter_connections().count() - 1, "every wire but the resource's has a label: {labels:?}");
    assert!((app.outputs()[0].1 - 60.0).abs() < 1e-6, "it still extracts all the ore needed");

    // A Pipeline Pump: water passes through, and it draws the Docs' power.
    let pump_mw = game.buildable_power["Build_PipelinePump_C"];
    assert!(pump_mw > 0.0);
    app.run_pending(crate::dialogs::Pending::New);
    app.new_factory();
    frames(&ctx, &mut app, 2);
    let water = app.add_test_node(Template::Resource("Desc_Water_C".into()), egui::pos2(0.0, 0.0));
    let pump = app.add_test_node(Template::Pump { item: "Desc_Water_C".into(), class: "Build_PipelinePump_C".into() }, egui::pos2(300.0, 0.0));
    let out = app.add_test_node(Template::Output("Desc_Water_C".into()), egui::pos2(500.0, 0.0));
    for (n, v) in [(water, 60.0), (out, 60.0)] {
        let i = app.state.graph[n].get_input(RATE).unwrap();
        app.state.graph[i].value.set_number(v);
    }
    connect(&mut app, water, pump, "Desc_Water_C");
    connect(&mut app, pump, out, "Desc_Water_C");
    frames(&ctx, &mut app, 2);
    assert!((app.outputs()[0].1 - 60.0).abs() < 1e-6, "{:?}", app.outputs());
    assert!((app.power() - pump_mw).abs() < 1e-9, "power {}", app.power());
    use egui_node_graph2::{AnyParameterId as P, NodeDataTrait};
    let i = app.state.graph[pump].inputs[0].1;
    let data = app.state.graph[pump].user_data.clone();
    assert!(!data.accepts_wire(pump, P::Input(i), &app.state.graph, &mut app.user), "a pump sits in one pipe");
    assert_eq!(crate::routers::name(&game, &app.state.graph[pump].user_data.kind), "Pipeline Pump Mk.1");
}

#[test]
fn extractors_extract_by_themselves_and_their_resource_shows_it() {
    use crate::graph::{NodeKind, Template, MACHINES, RATE};
    let Some((ctx, mut app, _)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    let (ore, item) = (app.add_test_node(Template::Resource("Desc_OreIron_C".into()), egui::pos2(0.0, 0.0)), "Desc_OreIron_C");
    let miner = app.add_test_node(Template::Extractor { item: item.into(), class: "Build_MinerMk1_C".into(), purity: fb_model::Purity::Normal }, egui::pos2(350.0, 0.0));
    // A smelter running a sixth of one: it takes 5 ore a minute.
    let out = app.add_test_node(Template::Recipe("Recipe_IngotIron_C".into()), egui::pos2(750.0, 0.0));
    let set = |app: &mut FactoryApp, n: egui_node_graph2::NodeId, name: &str, v: f64| {
        let i = app.state.graph[n].get_input(name).unwrap();
        app.state.graph[i].value.set_number(v);
    };
    set(&mut app, ore, RATE, 30.0);
    set(&mut app, miner, MACHINES, 1.0);
    set(&mut app, out, MACHINES, 1.0 / 6.0);
    let (ground, _) = connect(&mut app, ore, miner, item);
    let (smelter_in, _) = connect(&mut app, miner, out, item);
    frames(&ctx, &mut app, 2);
    // What comes after takes 5: the miner extracts 5 (backed up, as in
    // game), not starved by its resource node.
    let stat = |app: &FactoryApp, n, name: &str| app.user.ports[&(n, name.to_string())];
    let port = app.graph()[miner].inputs.iter().find(|(_, i)| *i == ground).map(|(n, _)| n.clone()).unwrap();
    let s = stat(&app, miner, &port);
    assert!((s.have - 5.0).abs() < 1e-6 && (s.want - 5.0).abs() < 1e-6, "takes what it extracts: {s:?}");
    assert!((app.user.ground[&ore] - 5.0).abs() < 1e-6, "the resource shows what's extracted");
    let miner_mk1 = crate::extractors::per_machine(&app.user.game, "Build_MinerMk1_C", fb_model::Purity::Normal);
    assert_eq!(miner_mk1, 60.0, "a Miner Mk.1 on a Normal node: 60 per minute (Docs)");
    assert!((app.user.nodes[&miner].efficiency - 5.0 / 60.0).abs() < 1e-4);
    let texts = drawn_texts(&ctx, &mut app);
    assert!(texts.iter().any(|t| t == "Extracted"), "the resource's rate is shown, not edited");
    // The resource node's old Rate doesn't limit anything.
    set(&mut app, ore, RATE, 1.0);
    set(&mut app, out, MACHINES, 3.0);
    app.changed();
    frames(&ctx, &mut app, 2);
    let port = app.graph()[out].inputs.iter().find(|(_, i)| *i == smelter_in).map(|(n, _)| n.clone()).unwrap();
    let got = stat(&app, out, &port).have;
    assert!((got - 60.0).abs() < 1e-6, "one Miner Mk.1 makes its 60, whatever the resource said: {got}");
    assert!((app.summary.resources[item] - 60.0).abs() < 1e-6, "Raw resources counts what's extracted");
    assert!(matches!(app.graph()[miner].user_data.kind, NodeKind::Extractor { .. }));
}

#[test]
fn a_small_plan_builds_whole_machines_and_shows_what_its_miner_could_do() {
    use crate::graph::{input_number, NodeKind, CLOCK, MACHINES};
    let Some((ctx, mut app, _)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    app.planner_mut().targets = vec![Target { item: "Desc_IronIngot_C".into(), rate: 5.0 }];
    app.press_plan();
    frames(&ctx, &mut app, 4);
    let g = app.graph();
    let find = |f: fn(&NodeKind) -> bool| g.iter_nodes().find(|n| f(&g[*n].user_data.kind)).unwrap();
    let smelter = find(|k| matches!(k, NodeKind::Recipe { .. }));
    let miner = find(|k| matches!(k, NodeKind::Extractor { .. }));
    let ore = find(|k| matches!(k, NodeKind::Resource { .. }));
    // 5 ingots: a sixth of a Smelter's 30, built as 1 Smelter at 16.67%.
    assert_eq!(input_number(g, smelter, MACHINES), 1.0);
    assert!((input_number(g, smelter, CLOCK) - 100.0 / 6.0).abs() < 1e-4, "{}", input_number(g, smelter, CLOCK));
    // One whole Miner Mk.1 at 100%, as the extractor settings say.
    assert_eq!((input_number(g, miner, MACHINES), input_number(g, miner, CLOCK)), (1.0, 100.0));
    let texts = drawn_texts(&ctx, &mut app);
    assert!(texts.iter().any(|t| t == "5 of 60"), "Extracted: 5 taken of the 60 it can");
    for (p, can) in [("Pure", "5 of 120"), ("Impure", "5 of 30")] {
        let _ = ore;
        app.request_frame_all();
        frames(&ctx, &mut app, 2);
        press(&ctx, &mut app, p);
        frames(&ctx, &mut app, 2);
        let texts = drawn_texts(&ctx, &mut app);
        assert!(texts.iter().any(|t| t == can), "{p}: {can}; {:?}", texts.iter().filter(|t| t.contains(" of ")).collect::<Vec<_>>());
    }
    // The Overview's raw resources are just what's extracted.
    assert!(!texts.iter().any(|t| t.contains("Miner Mk.1 on Normal nodes")), "no extractor plan in the Overview any more");
}

#[test]
fn water_extractors_have_no_purity() {
    use crate::graph::{Template, CLOCK, MACHINES};
    use egui_node_graph2::{AnyParameterId as P, NodeDataTrait};
    let Some((ctx, mut app, _)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    let game = app.user.game.clone();
    // From the game's Docs: 120 m³/min at 100% on any water, 300 at 250%, 20 MW.
    let w = crate::extractors::find(&game, "Build_WaterPump_C").expect("the Water Extractor");
    for p in fb_model::Purity::ALL {
        assert_eq!(w.per_min(p, 1.0), 120.0, "{p:?}");
    }
    assert_eq!(w.per_min(fb_model::Purity::Normal, 2.5), 300.0);
    assert_eq!(crate::extractors::power(w, 1, 1.0), 20.0);
    assert!(!crate::extractors::has_purity(&game, "Build_WaterPump_C"));
    // On the graph: water's resource node has no purity switch with a Water
    // Extractor on it, and the extractor puts out one pipe.
    let water = "Desc_Water_C";
    let p = app.screen_to_graph(egui::pos2(450.0, 300.0));
    let res = app.add_test_node(Template::Resource(water.into()), p);
    let p = app.screen_to_graph(egui::pos2(850.0, 300.0));
    let x = app.add_test_node(Template::Extractor { item: water.into(), class: "Build_WaterPump_C".into(), purity: fb_model::Purity::Pure }, p);
    let p = app.screen_to_graph(egui::pos2(1150.0, 300.0));
    let sink = app.add_test_node(Template::Sink(water.into()), p);
    for (n, v) in [(MACHINES, 1.0), (CLOCK, 250.0)] {
        let i = app.state.graph[x].get_input(n).unwrap();
        app.state.graph[i].value.set_number(v);
    }
    connect(&mut app, res, x, water);
    let pipe = connect(&mut app, x, sink, water);
    frames(&ctx, &mut app, 3);
    assert!((app.wire_flow[&pipe] - 300.0).abs() < 1e-6, "300 m³/min at 250%, purity or not");
    assert!(!crate::widgets::TEST_BUTTONS.with(|b| b.borrow().iter().any(|x| x.0 == "Impure")), "no purity on water");
    let out = app.state.graph[x].outputs[0].1;
    let data = app.state.graph[x].user_data.clone();
    assert!(!data.accepts_wire(x, P::Output(out), &app.state.graph, &mut app.user), "one pipe out");
}

#[test]
fn a_chosen_belt_limits_its_wire() {
    use crate::graph::{Template, MACHINES};
    let Some((ctx, mut app, _)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    app.minimap = false;
    let item = "Desc_OreIron_C";
    let at = |app: &FactoryApp, x: f32, y: f32| app.screen_to_graph(egui::pos2(x, y));
    let p = at(&app, 380.0, 200.0);
    let miner = app.add_test_node(Template::Extractor { item: item.into(), class: "Build_MinerMk1_C".into(), purity: fb_model::Purity::Normal }, p);
    let p = at(&app, 850.0, 200.0);
    let smelter = app.add_test_node(Template::Recipe("Recipe_IngotIron_C".into()), p);
    for (n, v) in [(miner, 2.0), (smelter, 4.0)] {
        let i = app.state.graph[n].get_input(MACHINES).unwrap();
        app.state.graph[i].value.set_number(v);
    }
    // Standing on a Normal node.
    let p = at(&app, 380.0, 520.0);
    let ore = app.add_test_node(Template::Resource(item.into()), p);
    connect(&mut app, ore, miner, item);
    let wire = connect(&mut app, miner, smelter, item);
    frames(&ctx, &mut app, 3);
    // 2 Miners Mk.1 (60 each): 120 a minute, on the slowest belt that
    // carries it, a Mk.2 (120).
    assert!((app.wire_flow[&wire] - 120.0).abs() < 1e-6);
    assert!(app.wire_label_texts.contains(&"Mk.2\n120/min".to_string()), "{:?}", app.wire_label_texts);
    // Its menu: a Mk.1 (60) instead, which holds the line to 60.
    app.belt_menu = Some((wire.0, wire.1, egui::pos2(600.0, 300.0)));
    frames(&ctx, &mut app, 1);
    press(&ctx, &mut app, "    Conveyor Belt Mk.1");
    frames(&ctx, &mut app, 3);
    assert_eq!(crate::wires::chosen(&app.planner.belts, wire.0, wire.1), Some(1));
    assert!((app.wire_flow[&wire] - 60.0).abs() < 1e-6, "{}", app.wire_flow[&wire]);
    assert!((app.user.nodes[&smelter].efficiency - 0.5).abs() < 1e-6, "the smelters get half their ore");
    assert!(app.wire_label_texts.contains(&"Mk.1\n60/min".to_string()), "{:?}", app.wire_label_texts);
    // Kept in the factory file, and it limits there too.
    let json = app.to_portable().to_json(false);
    let f = crate::portable::FactoryFile::parse(&json).unwrap();
    assert_eq!(f.wires.iter().filter_map(|w| w.belt).collect::<Vec<_>>(), vec![1]);
    let (ctx2, mut other, _) = harness().unwrap();
    frames(&ctx2, &mut other, 2);
    other.open_factory(&ctx2, f, None);
    frames(&ctx2, &mut other, 3);
    assert_eq!(other.planner.belts.len(), 1);
    assert!(other.wire_flow.values().any(|v| (v - 60.0).abs() < 1e-6), "{:?}", other.wire_flow);
    // Automatic again: back to 120.
    app.belt_menu = Some((wire.0, wire.1, egui::pos2(600.0, 300.0)));
    frames(&ctx, &mut app, 1);
    press(&ctx, &mut app, "    Automatic");
    frames(&ctx, &mut app, 3);
    assert!(app.planner.belts.is_empty());
    assert!((app.wire_flow[&wire] - 120.0).abs() < 1e-6);
}

#[test]
fn a_wire_dropped_on_a_node_connects_to_the_port_that_fits() {
    use crate::graph::Template;
    use egui_node_graph2::AnyParameterId;
    let Some((ctx, mut app, _)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    app.minimap = false;
    let src = app.add_test_node(Template::Resource("Desc_OreIron_C".into()), egui::pos2(100.0, 100.0));
    let smelter = app.add_test_node(Template::Recipe("Recipe_IngotIron_C".into()), egui::pos2(700.0, 100.0));
    frames(&ctx, &mut app, 3);
    let g = app.graph();
    let out = g[src].outputs[0].1;
    let ring = *app.port_hooks[&AnyParameterId::Output(out)].last().unwrap();
    // Let go over the middle of the smelter (not on its bubble).
    let body = app.node_rects[&smelter].center();
    drag(&ctx, &mut app, ring, body);
    let g = app.graph();
    let input = g[smelter].inputs.iter().map(|(_, i)| *i).find(|i| g[*i].typ.0 == "Desc_OreIron_C").unwrap();
    assert_eq!(g.connections(input), vec![out], "the ore went to the smelter's ore input");
    assert!(app.finder.is_none(), "not a drop on empty canvas");
}

#[test]
fn factory_tabs_keep_their_own_graph_and_history() {
    let Some((ctx, mut app, store)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    let first = app.active;
    app.add_test_node(crate::graph::Template::Resource("Desc_OreIron_C".into()), egui::pos2(0.0, 0.0));
    frames(&ctx, &mut app, 2);
    assert_eq!(app.undo_steps(), 1);
    press(&ctx, &mut app, "New factory");
    frames(&ctx, &mut app, 2);
    let second = app.active;
    assert_ne!(first, second);
    assert_eq!(app.node_count(), 0, "a new factory starts empty");
    assert_eq!(app.undo_steps(), 0, "and with its own history");
    app.add_test_node(crate::graph::Template::Resource("Desc_Coal_C".into()), egui::pos2(0.0, 0.0));
    app.add_test_node(crate::graph::Template::Resource("Desc_Stone_C".into()), egui::pos2(0.0, 300.0));
    frames(&ctx, &mut app, 2);
    press(&ctx, &mut app, "Factory");
    frames(&ctx, &mut app, 2);
    assert_eq!(app.active, first);
    assert_eq!(app.node_count(), 1);
    app.undo();
    frames(&ctx, &mut app, 1);
    assert_eq!(app.node_count(), 0, "undo acts on the open factory only");
    press(&ctx, &mut app, "Factory 2");
    frames(&ctx, &mut app, 2);
    assert_eq!(app.node_count(), 2, "the other factory is untouched");

    // Closing a tab keeps the factory in the project.
    press(&ctx, &mut app, "Close Factory 2");
    frames(&ctx, &mut app, 2);
    assert_eq!(app.tabs, vec![first]);
    assert_eq!(app.factory_count(), 2);
    assert_eq!(app.active, first);

    // The autosave has every factory, and restores them.
    let t = ctx.input(|i| i.time);
    for k in 0..4 {
        let input = egui::RawInput { time: Some(t + 3.0 * (k + 1) as f64), screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 960.0))), ..Default::default() };
        let _ = ctx.run(input, |ctx| app.ui(ctx));
    }
    let saved = store.borrow().get(crate::app::AUTOSAVE).cloned().expect("autosaved");
    let w = crate::project::Workspace::parse(&saved).expect("a whole-project autosave");
    assert_eq!(w.docs.len(), 3, "master graph and both factories");
    app.new_project();
    frames(&ctx, &mut app, 1);
    app.restore_workspace(w);
    frames(&ctx, &mut app, 2);
    assert_eq!(app.factory_count(), 2);
    app.activate(second);
    frames(&ctx, &mut app, 1);
    assert_eq!(app.node_count(), 2);
}

#[test]
fn deleting_a_block_deletes_its_factory_after_asking() {
    use crate::project::MASTER;
    let Some((ctx, mut app, _)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    app.add_factory_file(&ctx, &ingot_factory("Smelting", 1.0));
    let smelting = app.active;
    app.activate(MASTER);
    frames(&ctx, &mut app, 2);
    let block = crate::project::block_of(&app.state.graph, smelting).unwrap();
    app.state.selected_nodes = vec![block];
    app.delete_selected();
    assert!(matches!(app.dialog(), crate::dialogs::Dialog::DeleteFactories { ids } if ids == &vec![smelting]));
    frames(&ctx, &mut app, 3);
    press(&ctx, &mut app, "Delete");
    frames(&ctx, &mut app, 2);
    assert_eq!(app.factory_count(), 1);
    assert!(crate::project::block_of(&app.state.graph, smelting).is_none());
    assert!(!app.tabs.contains(&smelting));

    // Duplicating a block copies the factory.
    let only = app.factory_ids()[0];
    app.state.selected_nodes = vec![crate::project::block_of(&app.state.graph, only).unwrap()];
    app.duplicate_selected();
    frames(&ctx, &mut app, 2);
    assert_eq!(app.factory_count(), 2);
    assert_eq!(app.node_count(), 2);
}

#[test]
fn project_share_codes_open_whole_projects() {
    let Some((ctx, mut app, _)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    app.add_factory_file(&ctx, &ingot_factory("Smelting", 2.0));
    app.add_factory_file(&ctx, &rod_factory("Rods"));
    app.activate(crate::project::MASTER);
    frames(&ctx, &mut app, 2);
    let (smelting, rods) = (app.factory_ids()[1], app.factory_ids()[2]);
    link(&mut app, smelting, rods, "Desc_IronIngot_C");
    frames(&ctx, &mut app, 2);
    let code = app.project_file().to_share_code();
    assert!(code.starts_with("FBP1-"));
    app.new_project();
    frames(&ctx, &mut app, 1);
    match crate::portable::Shared::from_share_code(&code).unwrap() {
        crate::portable::Shared::Project(p) => {
            let ctx2 = ctx.clone();
            app.open_project(&ctx2, p, None);
        }
        other => panic!("expected a project, got {other:?}"),
    }
    frames(&ctx, &mut app, 2);
    assert_eq!(app.factory_count(), 3);
    assert_eq!(app.state.graph.iter_connections().count(), 1);
    assert!(app.summary.issues.is_empty(), "{:?}", app.summary.issues);
}

#[test]
fn moving_a_selection_to_a_new_factory_keeps_the_totals() {
    use crate::graph::NodeKind;
    use crate::portable::{FactoryFile, Kind, Node, Wire};
    let Some((ctx, mut app, _)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    let node = |kind, class: &str, machines: Option<f64>, rate: Option<f64>, x: f32| Node { kind, class: class.into(), machines, clock: machines.map(|_| 100.0), sloops: None, rate, name: None, color: None, item: None, purity: None, x, y: 0.0, planned: false, router: None };
    let mut f = FactoryFile::new(Some("Rods".into()));
    f.nodes = vec![
        node(Kind::Resource, "Desc_OreIron_C", None, Some(30.0), 0.0),
        node(Kind::Recipe, "Recipe_IngotIron_C", Some(1.0), None, 400.0),
        node(Kind::Recipe, "Recipe_IronRod_C", Some(2.0), None, 800.0),
        node(Kind::Output, "Desc_IronRod_C", None, Some(30.0), 1200.0),
    ];
    f.wires = (0..3).map(|k| Wire { from: k, output: 0, to: k + 1, input: 0, slot: 0, belt: None }).collect();
    app.add_factory_file(&ctx, &f);
    let here = app.active;
    frames(&ctx, &mut app, 2);
    assert!(app.summary.issues.is_empty(), "{:?}", app.summary.issues);
    // Move the rod constructors and the output.
    let g = &app.state.graph;
    let picked: Vec<_> = g.iter_nodes().filter(|n| matches!(&g[*n].user_data.kind, NodeKind::Output { .. }) || matches!(&g[*n].user_data.kind, NodeKind::Recipe { recipe } if recipe == "Recipe_IronRod_C")).collect();
    assert_eq!(picked.len(), 2);
    app.state.selected_nodes = picked;
    app.move_selection_to_new_factory(&ctx);
    frames(&ctx, &mut app, 2);
    assert_eq!(app.active, here, "stays in the factory it was moved from");
    assert_eq!(app.factory_count(), 3);
    let kinds: Vec<String> = app.state.graph.iter_nodes().map(|n| format!("{:?}", app.state.graph[n].user_data.kind)).collect();
    assert_eq!(app.node_count(), 4, "ore, its miner, smelter and an Output for the ingots: {kinds:?}");
    assert!(kinds.iter().any(|k| k.contains("Output") && k.contains("Desc_IronIngot_C")), "{kinds:?}");
    assert!(app.summary.issues.is_empty(), "{:?}", app.summary.issues);
    let new = *app.factory_ids().last().unwrap();
    assert_eq!(app.doc_name(new), "Iron Rod factory");

    // The new factory gets the ingots as an input and makes the rods.
    app.activate(new);
    frames(&ctx, &mut app, 2);
    assert_eq!(app.node_count(), 3, "an Input, the constructors and the output");
    assert!(app.summary.issues.is_empty(), "{:?}", app.summary.issues);
    assert!((app.summary.inputs["Desc_IronIngot_C"] - 30.0).abs() < 1e-6);
    assert_eq!(app.planner.targets.first().map(|t| t.item.as_str()), Some("Desc_IronRod_C"), "its plan makes what it outputs");

    // Linked on the master graph: nothing short, the same rods leave the project.
    app.activate(crate::project::MASTER);
    frames(&ctx, &mut app, 2);
    assert_eq!(app.state.graph.iter_connections().count(), 1);
    assert!(app.summary.issues.is_empty(), "{:?}", app.summary.issues);
    let t = app.project_totals();
    assert!((t.products["Desc_IronRod_C"] - 30.0).abs() < 1e-6, "{:?}", t.products);
    assert!(!t.from_outside.contains_key("Desc_IronIngot_C"));
}

// ------------------------------------------------------ frames, find, minimap

fn drag_by(ctx: &egui::Context, app: &mut FactoryApp, from: egui::Pos2, by: egui::Vec2) {
    let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
    frame_with(ctx, app, vec![egui::Event::PointerMoved(from)]);
    frame_with(ctx, app, vec![button(from, true)]);
    let steps = 6;
    for k in 1..=steps {
        frame_with(ctx, app, vec![egui::Event::PointerMoved(from + by * (k as f32 / steps as f32))]);
    }
    frame_with(ctx, app, vec![button(from + by, false)]);
    frames(ctx, app, 2);
}

/// A planned factory (iron plates: ore, smelters, constructors, output).
fn planned(ctx: &egui::Context, app: &mut FactoryApp) {
    frames(ctx, app, 2);
    app.planner_mut().targets = vec![Target { item: "Desc_IronPlate_C".into(), rate: 60.0 }];
    app.press_plan();
    frames(ctx, app, 8);
}

#[test]
fn comment_frames_wrap_move_and_save_with_their_nodes() {
    let Some((ctx, mut app, store)) = harness() else { return };
    planned(&ctx, &mut app);
    let g = &app.state.graph;
    let smelters: Vec<_> = g.iter_nodes().filter(|n| matches!(&g[*n].user_data.kind, crate::graph::NodeKind::Recipe { recipe } if recipe == "Recipe_IngotIron_C")).collect();
    let others: Vec<_> = g.iter_nodes().filter(|n| !smelters.contains(n)).collect();
    app.state.selected_nodes = smelters.clone();
    // C frames the selection and starts naming it.
    frame_with(&ctx, &mut app, vec![egui::Event::Key { key: egui::Key::C, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }]);
    frames(&ctx, &mut app, 2);
    assert_eq!(app.planner.frames.len(), 1);
    assert!(app.frames.editing.is_some(), "a new frame asks for its name");
    app.frames.editing = Some((app.planner.frames[0].id, "Smelting".into()));
    frame_with(&ctx, &mut app, vec![egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }]);
    frames(&ctx, &mut app, 2);
    assert_eq!(app.planner.frames[0].title, "Smelting");
    assert!(app.frames.editing.is_none());
    // The frame encloses its nodes.
    let fr = app.planner.frames[0].rect();
    for n in &smelters {
        let p = app.node_pos_unzoomed(*n);
        assert!(fr.contains(p), "node at {p:?} inside {fr:?}");
    }

    // Dragging the title bar moves the frame and what's inside, nothing else.
    let before: Vec<_> = smelters.iter().chain(&others).map(|n| app.node_pos(*n)).collect();
    let frame_before = app.planner.frames[0].clone();
    let (header, _) = button("Frame Smelting").expect("frame title bar");
    drag_by(&ctx, &mut app, header.center(), egui::vec2(120.0, 60.0));
    let after: Vec<_> = smelters.iter().chain(&others).map(|n| app.node_pos(*n)).collect();
    for (k, n) in smelters.iter().enumerate() {
        let d = after[k] - before[k];
        assert!((d - egui::vec2(120.0, 60.0)).length() < 1.0, "inside node {n:?} moved by {d:?}");
    }
    for k in smelters.len()..before.len() {
        assert!((after[k] - before[k]).length() < 0.01, "outside nodes stay put");
    }
    let z = app.state.pan_zoom.zoom;
    let moved = app.planner.frames[0].x - frame_before.x;
    assert!((moved - 120.0 / z).abs() < 1.0, "frame moved {moved}");

    // It's one undo step, and it comes back with the file.
    let steps = app.undo_steps();
    app.undo();
    frames(&ctx, &mut app, 1);
    assert!((app.planner.frames[0].x - frame_before.x).abs() < 0.01, "undo puts it back");
    assert_eq!(app.undo_steps(), steps - 1);
    let path = "C:/Factories/Framed.fbfactory";
    app.save_as(path);
    frames(&ctx, &mut app, 2);
    assert!(store.borrow()[path].contains("\"frames\""));
    app.new_project();
    frames(&ctx, &mut app, 1);
    assert!(app.planner.frames.is_empty());
    app.open(path);
    frames(&ctx, &mut app, 3);
    assert_eq!(app.planner.frames.len(), 1);
    assert_eq!(app.planner.frames[0].title, "Smelting");

    // Deleting the frame keeps its nodes.
    let n = app.node_count();
    let id = app.planner.frames[0].id;
    app.state.selected_nodes.clear();
    app.frames.selected = Some(id);
    frame_with(&ctx, &mut app, vec![egui::Event::Key { key: egui::Key::Delete, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }]);
    frames(&ctx, &mut app, 1);
    assert!(app.planner.frames.is_empty());
    assert_eq!(app.node_count(), n);
}

#[test]
fn find_steps_through_matches_and_the_minimap_moves_the_view() {
    let Some((ctx, mut app, _)) = harness() else { return };
    planned(&ctx, &mut app);
    let ctrl_f = egui::Event::Key { key: egui::Key::F, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND };
    frame_with(&ctx, &mut app, vec![ctrl_f]);
    frames(&ctx, &mut app, 1);
    assert!(app.find.is_some(), "Ctrl+F opens Find");
    // Machines are found by their building's name too.
    let hits = app.find_matches("constructor");
    assert!(!hits.is_empty());
    app.find.as_mut().unwrap().query = "constructor".into();
    frames(&ctx, &mut app, 3);
    let first = match hits[0] {
        crate::navigate::Hit::Node(n) => n,
        other => panic!("{other:?}"),
    };
    assert_eq!(app.selected(), vec![first], "the first match is shown and selected");
    assert!(app.find_matches("no such thing here").is_empty());
    assert_eq!(app.find_matches("iron plate").len(), app.find_matches("PLATE iron").len(), "words in any order, any case");
    frame_with(&ctx, &mut app, vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }]);
    frames(&ctx, &mut app, 1);
    assert!(app.find.is_none(), "Esc closes it");

    // Clicking the minimap's corner moves the view there.
    let (map, _) = button("Minimap").expect("minimap shown");
    let pan = app.state.pan_zoom.pan;
    click(&ctx, &mut app, map.left_top() + egui::vec2(8.0, 8.0), egui::PointerButton::Primary);
    assert!((app.state.pan_zoom.pan - pan).length() > 1.0, "view moved");
    assert!(app.selected().len() <= 1, "no box selection from the click");
    app.minimap = false;
    frames(&ctx, &mut app, 1);
    assert!(button("Minimap").is_none(), "View › Minimap hides it");
}

#[test]
fn right_click_on_the_master_graph_adds_a_factory_there() {
    let Some((ctx, mut app, store)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    press(&ctx, &mut app, "Project");
    frames(&ctx, &mut app, 3);
    assert_eq!(app.factory_count(), 1);
    // Empty canvas: right of the one block.
    let only = app.node_rects.values().next().copied().expect("one block");
    let spot = egui::pos2(only.right() + 150.0, only.top() + 20.0);
    assert!(!app.node_rects.values().any(|r| r.contains(spot)), "the spot is empty canvas");
    click(&ctx, &mut app, spot, egui::PointerButton::Secondary);
    assert!(app.master_menu.is_some(), "the canvas menu opened");
    assert!(app.finder.is_none(), "not the Add node menu");
    // The menu's item (the tab strip's + has the same name).
    let item = crate::widgets::TEST_BUTTONS.with(|b| b.borrow().iter().find(|(l, r, _)| l == "New factory" && r.top() >= spot.y - 1.0).map(|(_, r, _)| *r)).expect("menu item");
    click(&ctx, &mut app, item.center(), egui::PointerButton::Primary);
    assert!(app.master_menu.is_none(), "menu closed");
    assert_eq!(app.factory_count(), 2);
    assert_eq!(app.active, crate::project::MASTER, "stays on the master graph");
    let new = *app.factory_ids().last().unwrap();
    let block = crate::project::block_of(&app.state.graph, new).expect("its block");
    assert_eq!(app.selected(), vec![block], "the new block is selected");
    let at = app.screen_to_graph_unzoomed(spot);
    assert!((app.node_pos_unzoomed(block) - at).length() < 1.0, "placed where the menu was opened");
    assert!(app.is_dirty());

    // From a file: the block goes to the menu's spot too, once the file is read.
    let mut f = crate::portable::FactoryFile::new(Some("Smelting".into()));
    f.nodes.push(crate::portable::Node { kind: crate::portable::Kind::Resource, class: "Desc_OreIron_C".into(), machines: None, clock: None, sloops: None, rate: Some(60.0), name: None, color: None, item: None, purity: None, x: 0.0, y: 0.0, planned: false, router: None });
    store.borrow_mut().insert("C:/Factories/Smelting.fbfactory".into(), f.to_json(false));
    let spot2 = egui::pos2(only.right() + 150.0, only.bottom() + 200.0);
    assert!(!app.node_rects.values().any(|r| r.contains(spot2)));
    click(&ctx, &mut app, spot2, egui::PointerButton::Secondary);
    let at2 = app.master_menu.expect("menu").1;
    app.master_menu = None;
    app.block_spot = Some(at2);
    app.read_file(crate::app_files::OpenPurpose::AddFactory, "C:/Factories/Smelting.fbfactory".into());
    frames(&ctx, &mut app, 3);
    assert_eq!(app.factory_count(), 3);
    assert_eq!(app.active, crate::project::MASTER);
    let smelting = *app.factory_ids().last().unwrap();
    assert_eq!(app.doc_name(smelting), "Smelting");
    let b2 = crate::project::block_of(&app.state.graph, smelting).unwrap();
    assert!((app.node_pos_unzoomed(b2) - at2).length() < 1.0);
    assert!(app.block_spot.is_none());
}

fn press_key(ctx: &egui::Context, app: &mut FactoryApp, key: egui::Key) {
    frame_with(ctx, app, vec![egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }]);
}

#[test]
fn f2_renames_nodes_and_factory_blocks() {
    let Some((ctx, mut app, store)) = harness() else { return };
    planned(&ctx, &mut app);
    let g = &app.state.graph;
    let smelter = g.iter_nodes().find(|n| matches!(&g[*n].user_data.kind, crate::graph::NodeKind::Recipe { recipe } if recipe == "Recipe_IngotIron_C")).unwrap();
    let original = g[smelter].label.clone();
    app.state.selected_nodes = vec![smelter];
    press_key(&ctx, &mut app, egui::Key::F2);
    frames(&ctx, &mut app, 1);
    assert_eq!(app.node_rename.as_ref().map(|r| r.0), Some(smelter), "F2 starts renaming the selected node");
    app.node_rename = Some((smelter, "  Main smelters ".into()));
    frames(&ctx, &mut app, 1);
    press_key(&ctx, &mut app, egui::Key::Enter);
    frames(&ctx, &mut app, 2);
    assert!(app.node_rename.is_none());
    assert_eq!(app.state.graph[smelter].label, "Main smelters");
    assert!(app.find_matches("main smelters").len() == 1, "found by its new name");

    // The name is saved; the recipe still is what it was.
    let path = "C:/Factories/Named.fbfactory";
    app.save_as(path);
    frames(&ctx, &mut app, 2);
    assert!(store.borrow()[path].contains("\"name\": \"Main smelters\""));
    app.new_project();
    frames(&ctx, &mut app, 1);
    app.open(path);
    frames(&ctx, &mut app, 3);
    let g = &app.state.graph;
    let back = g.iter_nodes().find(|n| g[*n].label == "Main smelters").expect("name came back");
    assert!(matches!(&g[back].user_data.kind, crate::graph::NodeKind::Recipe { recipe } if recipe == "Recipe_IngotIron_C"));
    let unnamed = store.borrow()[path].matches("\"name\"").count();
    assert_eq!(unnamed, 2, "only the renamed node (and the factory) carry a name");

    // An empty name goes back to the recipe's; Esc keeps the old one.
    app.state.selected_nodes = vec![back];
    press_key(&ctx, &mut app, egui::Key::F2);
    frames(&ctx, &mut app, 1);
    app.node_rename = Some((back, "Throwaway".into()));
    frames(&ctx, &mut app, 1);
    press_key(&ctx, &mut app, egui::Key::Escape);
    frames(&ctx, &mut app, 1);
    assert_eq!(app.state.graph[back].label, "Main smelters", "Esc cancels");
    press_key(&ctx, &mut app, egui::Key::F2);
    frames(&ctx, &mut app, 1);
    app.node_rename = Some((back, String::new()));
    frames(&ctx, &mut app, 1);
    press_key(&ctx, &mut app, egui::Key::Enter);
    frames(&ctx, &mut app, 1);
    assert_eq!(app.state.graph[back].label, original);

    // On the master graph, F2 on a block renames the factory.
    app.activate(crate::project::MASTER);
    frames(&ctx, &mut app, 2);
    let id = app.factory_ids()[0];
    let block = crate::project::block_of(&app.state.graph, id).unwrap();
    app.state.selected_nodes = vec![block];
    press_key(&ctx, &mut app, egui::Key::F2);
    frames(&ctx, &mut app, 1);
    app.node_rename = Some((block, "Plates".into()));
    frames(&ctx, &mut app, 1);
    press_key(&ctx, &mut app, egui::Key::Enter);
    frames(&ctx, &mut app, 2);
    assert_eq!(app.doc_name(id), "Plates");
    assert_eq!(app.state.graph[block].label, "Plates");
}

#[test]
fn snap_to_grid_moves_nodes_and_frames_in_grid_steps() {
    let Some((ctx, mut app, _)) = harness() else { return };
    planned(&ctx, &mut app);
    assert!(!app.snap, "off unless turned on");
    app.snap = true;
    app.save_view_settings();
    frames(&ctx, &mut app, 2);
    assert!(app.status.as_ref().unwrap().settings.view.snap_to_grid, "remembered in the settings");

    let (id, rect, _) = app.first_node().unwrap();
    app.state.selected_nodes.clear();
    let grab = rect.left_top() + egui::vec2(40.0, 14.0);
    drag(&ctx, &mut app, grab, grab + egui::vec2(37.0, 13.0));
    let p = app.node_pos_unzoomed(id);
    let on_grid = |v: f32| ((v / crate::app::GRID).round() * crate::app::GRID - v).abs() < 0.01;
    assert!(on_grid(p.x) && on_grid(p.y), "node at {p:?} is on the grid");

    // Frames too, and what's inside them stays where it is in the frame.
    let g = &app.state.graph;
    let all: Vec<_> = g.iter_nodes().collect();
    app.state.selected_nodes = all[..2].to_vec();
    app.add_frame_around_selection();
    app.frames.editing = None;
    frames(&ctx, &mut app, 2);
    let title = app.planner.frames[0].title.clone();
    let (header, _) = button(&format!("Frame {title}")).expect("frame title bar");
    drag_by(&ctx, &mut app, header.center() + egui::vec2(-40.0, 0.0), egui::vec2(51.0, 22.0));
    let f = &app.planner.frames[0];
    assert!(on_grid(f.x) && on_grid(f.y), "frame at {}, {}", f.x, f.y);

    // Off again: free movement.
    app.snap = false;
    let (id, rect, _) = app.first_node().unwrap();
    let before = app.node_pos_unzoomed(id);
    let grab = rect.left_top() + egui::vec2(40.0, 14.0);
    drag(&ctx, &mut app, grab, grab + egui::vec2(7.0, 5.0));
    // (egui starts a drag after the first few pixels, so it moves a bit less.)
    let after = app.node_pos_unzoomed(id);
    assert!((after - before).length() > 1.0, "it moved");
    assert!(!(on_grid(after.x) && on_grid(after.y)), "and isn't held to the grid: {after:?}");
}

#[test]
fn power_sections_name_their_ends_and_what_crosses() {
    use crate::map::{compass, crossing};
    use fb_model::world::PowerSide;
    // Screen up is north (world -Y).
    assert_eq!(compass(1.0, 0.0), "east");
    assert_eq!(compass(0.0, -1.0), "north");
    assert_eq!(compass(-1.0, 1.0), "south-west");
    let side = |makes: f32, uses: f32| PowerSide { makes, uses, buildings: 1 };
    // The b side uses 300 MW more than it makes: it comes from a.
    assert_eq!(crossing([side(1000.0, 200.0), side(100.0, 400.0)]), Some((300.0, true)));
    assert_eq!(crossing([side(0.0, 50.0), side(500.0, 10.0)]), Some((50.0, false)));
    assert_eq!(crossing([side(100.0, 50.0), side(500.0, 10.0)]), None, "each side covers itself");
}

#[test]
fn an_extractor_extracts_only_on_a_resource_node() {
    use crate::graph::NodeKind;
    use egui_node_graph2::NodeDataTrait;
    let Some((ctx, mut app, _)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    app.planner_mut().targets = vec![Target { item: "Desc_IronIngot_C".into(), rate: 30.0 }];
    app.press_plan();
    frames(&ctx, &mut app, 4);
    let g = app.graph();
    let miner = g.iter_nodes().find(|n| matches!(g[*n].user_data.kind, NodeKind::Extractor { .. })).unwrap();
    let ore = g.iter_nodes().find(|n| matches!(g[*n].user_data.kind, NodeKind::Resource { .. })).unwrap();
    let warning = |app: &mut FactoryApp| {
        let data = app.state.graph[miner].user_data.clone();
        data.warning(miner, &app.state.graph, &mut app.user).map(|w| w.1)
    };
    assert_eq!(warning(&mut app), None);
    assert!((app.summary.resources["Desc_OreIron_C"] - 30.0).abs() < 1e-6);
    // Take the miner off its node: nothing comes out, and it says why.
    let g = &app.state.graph;
    let on = g[miner].inputs.iter().map(|x| x.1).find(|x| !g.connections(*x).is_empty()).unwrap();
    for o in g.connections(on) {
        app.state.graph.remove_connection(on, o);
    }
    app.changed();
    frames(&ctx, &mut app, 3);
    assert_eq!(app.summary.resources.get("Desc_OreIron_C").copied().unwrap_or(0.0), 0.0, "{:?}", app.summary.resources);
    let out = app.state.graph[miner].outputs[0].0.clone();
    assert_eq!(app.user.ports[&(miner, out)].have, 0.0, "the miner puts nothing out");
    assert_eq!(warning(&mut app).as_deref(), Some("Not on a resource node: it extracts nothing until a Iron Ore resource node is wired into it."));
    // The deposit, with nothing on it, gives nothing either.
    let r_out = app.state.graph[ore].outputs[0].0.clone();
    let s = app.user.ports.get(&(ore, r_out)).copied().unwrap_or_default();
    assert_eq!((s.have, s.want), (0.0, 0.0), "{s:?}");
}

#[test]
fn a_new_project_starts_with_the_hub_and_its_two_burners() {
    use crate::graph::{NodeKind, Template, RATE};
    use crate::project::{block_of, MASTER};
    let Some((ctx, mut app, store)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    app.new_project();
    frames(&ctx, &mut app, 2);
    // The master graph, with the HUB's block: not a factory.
    assert!(app.on_master());
    let hub = app.hub_id().expect("a HUB");
    assert_eq!(app.doc_name(hub), "The HUB");
    let b = block_of(&app.state.graph, hub).unwrap();
    assert!(matches!(app.state.graph[b].user_data.kind, NodeKind::Factory { hub: true, .. }));
    assert_eq!(app.state.graph[b].user_data.kind.icon_item(&app.user.game.clone()), Some(fb_model::HUB_BUILDING));
    // One per project: no copy, no rename, no second one.
    assert_eq!(app.duplicate_factory(hub), None);
    app.rename_factory(hub, "Base");
    assert_eq!(app.doc_name(hub), "The HUB");
    app.place_hub_at(egui::pos2(400.0, 0.0));
    assert_eq!(app.factory_count(), 1);

    // Inside: its two Biomass Burners, nothing else.
    app.activate(hub);
    frames(&ctx, &mut app, 2);
    let burners = |app: &FactoryApp| app.state.graph.iter_nodes().filter(|n| matches!(app.state.graph[*n].user_data.kind, NodeKind::Burner { .. })).collect::<Vec<_>>();
    assert_eq!(burners(&app).len(), 2);
    assert_eq!(app.state.graph.nodes.len(), 2);
    // They stay: not deleted, not copied.
    app.select_all();
    app.delete_selected();
    app.select_all();
    app.duplicate_selected();
    frames(&ctx, &mut app, 1);
    assert_eq!(burners(&app).len(), 2);
    assert_eq!(app.state.graph.nodes.len(), 2);
    // Only their fuels can be added: by hand or from another factory.
    let game = app.user.game.clone();
    let catalog = app.hub_catalog.as_ref().unwrap();
    assert_eq!(catalog.entries.len(), 2 * game.burner_fuels.len());
    assert!(catalog.entries.iter().all(|e| matches!(&e.template, Template::Resource(i) | Template::Input(i) if crate::hub::is_fuel(&game, i))));
    // Nothing to plan here.
    app.planner_mut().targets = vec![Target { item: "Desc_IronIngot_C".into(), rate: 30.0 }];
    app.press_plan();
    frames(&ctx, &mut app, 3);
    assert_eq!(app.state.graph.nodes.len(), 2, "no plan in the HUB");

    // 80 Leaves a minute keep one burner at its 20 MW; 40 at half.
    let [one, two] = burners(&app)[..] else { unreachable!() };
    let leaves = app.add_test_node(Template::Resource("Desc_Leaves_C".into()), egui::pos2(-300.0, 0.0));
    let rate = app.state.graph[leaves].get_input(RATE).unwrap();
    app.state.graph[rate].value.set_number(80.0);
    connect(&mut app, leaves, one, "Desc_Leaves_C");
    frames(&ctx, &mut app, 3);
    assert!((app.summary.generation.unwrap() - 20.0).abs() < 1e-9, "{:?}", app.summary.generation);
    app.state.graph[rate].value.set_number(40.0);
    app.changed();
    frames(&ctx, &mut app, 3);
    assert!((app.summary.generation.unwrap() - 10.0).abs() < 1e-9, "{:?}", app.summary.generation);
    // Leaves come by hand: no miner for them, nothing to build.
    assert!(crate::overview_extra::plan_resource(&game, "Desc_Leaves_C", 40.0, &app.planner.extraction).is_none());
    assert!(app.summary.extractors.is_empty() && app.summary.buildings.is_empty());
    // The other burns Solid Biofuel: 2.67 a minute for its 20 MW.
    crate::hub::set_fuel(&game, &mut app.state.graph, two, "Desc_Biofuel_C");
    let bio = app.add_test_node(Template::Input("Desc_Biofuel_C".into()), egui::pos2(-300.0, 200.0));
    let rate = app.state.graph[bio].get_input(RATE).unwrap();
    app.state.graph[rate].value.set_number(8.0 / 3.0);
    connect(&mut app, bio, two, "Desc_Biofuel_C");
    frames(&ctx, &mut app, 3);
    assert!((app.summary.generation.unwrap() - 30.0).abs() < 1e-6, "{:?}", app.summary.generation);
    // Ore isn't fuel: a recipe or an ore node pasted in goes.
    app.add_test_node(Template::Recipe("Recipe_IngotIron_C".into()), egui::pos2(300.0, 0.0));
    app.add_test_node(Template::Resource("Desc_OreIron_C".into()), egui::pos2(300.0, 200.0));
    app.tidy_hubs();
    assert_eq!(app.state.graph.nodes.len(), 4);

    // On the master graph, its block makes 30 MW and takes Solid Biofuel in.
    app.activate(MASTER);
    frames(&ctx, &mut app, 2);
    let b = block_of(&app.state.graph, hub).unwrap();
    assert!(matches!(app.state.graph[b].user_data.kind, NodeKind::Factory { makes_mw, .. } if (makes_mw - 30.0).abs() < 1e-6));
    assert!(app.state.graph[b].inputs.iter().any(|(_, i)| app.state.graph[*i].typ.0 == "Desc_Biofuel_C"));

    // Saved and opened again: still the HUB, with its burners and fuel.
    let path = "C:/Factories/Base.fbproject";
    app.save_as(path);
    frames(&ctx, &mut app, 2);
    assert!(store.borrow()[path].contains("\"hub\": true"));
    app.new_project();
    app.open(path);
    frames(&ctx, &mut app, 3);
    let hub = app.hub_id().expect("the HUB again");
    app.activate(hub);
    frames(&ctx, &mut app, 2);
    assert_eq!(burners(&app).len(), 2);
    assert!((app.summary.generation.unwrap() - 30.0).abs() < 1e-6);

    // Deleted, it can be placed again (once).
    app.activate(MASTER);
    app.place_hub_at(egui::pos2(0.0, 0.0));
    app.new_factory_at(egui::pos2(400.0, 0.0));
    app.delete_factories(&[hub]);
    assert_eq!(app.hub_id(), None);
    app.place_hub_at(egui::pos2(0.0, 0.0));
    app.place_hub_at(egui::pos2(400.0, 0.0));
    let hubs = app.factory_ids().into_iter().filter(|id| app.is_hub(*id)).count();
    assert_eq!(hubs, 1);
}

#[test]
fn enter_opens_the_selected_factory_and_confirms_a_dialog() {
    use crate::project::{block_of, MASTER};
    let Some((ctx, mut app, _)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    let first = app.active;
    let second = app.add_factory("Rods", None);
    app.activate(MASTER);
    frames(&ctx, &mut app, 2);
    // Select a block, Enter: it opens, as a double-click does.
    app.state.selected_nodes = vec![block_of(&app.state.graph, second).unwrap()];
    press_key(&ctx, &mut app, egui::Key::Enter);
    frames(&ctx, &mut app, 1);
    assert_eq!(app.active, second);
    // Select one, Delete, Enter: deleted, as with the dialog's Delete button.
    app.activate(MASTER);
    frames(&ctx, &mut app, 2);
    app.state.selected_nodes = vec![block_of(&app.state.graph, second).unwrap()];
    press_key(&ctx, &mut app, egui::Key::Delete);
    frames(&ctx, &mut app, 1);
    assert!(matches!(app.dialog, crate::dialogs::Dialog::DeleteFactories { .. }));
    press_key(&ctx, &mut app, egui::Key::Enter);
    frames(&ctx, &mut app, 1);
    assert!(matches!(app.dialog, crate::dialogs::Dialog::None));
    assert_eq!(app.factory_ids(), vec![first]);
    assert_eq!(app.active, MASTER, "deleting doesn't open anything");
    // Escape still cancels.
    app.state.selected_nodes = vec![block_of(&app.state.graph, first).unwrap()];
    press_key(&ctx, &mut app, egui::Key::Delete);
    frames(&ctx, &mut app, 1);
    press_key(&ctx, &mut app, egui::Key::Escape);
    frames(&ctx, &mut app, 1);
    assert_eq!(app.factory_ids(), vec![first]);
}

#[test]
fn the_live_map_add_on_shows_players_only_when_turned_on() {
    let (mut world, map) = sample_world();
    let m = world.machines[0].clone();
    world.players = vec![fb_model::world::PlayerSpot { name: None, x: m.x, y: m.y + 500.0, z: m.z, yaw: 90.0, online: None }];
    let hook: RoomHook = Rc::new(move |cmd: &str, _args: &Value| match cmd {
        "map_info" => Some(Ok(serde_json::to_value(&map).unwrap())),
        "map_tile" => Some(Err("no tiles in tests".into())),
        "world_sessions" => Some(Ok(json!([{ "session": "Test", "saves": [world.save.clone()] }]))),
        "world_load" => Some(Ok(serde_json::to_value(&world).unwrap())),
        "world_latest" => Some(Ok(serde_json::to_value(&world.save).unwrap())),
        _ => None,
    });
    let Some((ctx, mut app, _)) = harness_with(Some(hook)) else { return };
    frames(&ctx, &mut app, 2);
    press(&ctx, &mut app, "Map");
    frames(&ctx, &mut app, 3);
    // Off by default: nothing new on the map.
    assert!(!app.live_on());
    assert!(!drawn_texts(&ctx, &mut app).iter().any(|t| t == "Player"));
    // On: the save's player, where it left them.
    app.status.as_mut().unwrap().settings.live.enabled = true;
    frames(&ctx, &mut app, 1);
    assert!(drawn_texts(&ctx, &mut app).iter().any(|t| t == "Player"));
    // Several are numbered; a name (from a mod, later) is used as is.
    let two = vec![fb_model::world::PlayerSpot::default(), fb_model::world::PlayerSpot { name: Some("Ada".into()), ..Default::default() }];
    assert_eq!((crate::live::label(&two, 0), crate::live::label(&two, 1)), ("Player 1".to_string(), "Ada".to_string()));
}

#[test]
fn live_map_follows_a_dedicated_server_after_its_certificate_is_trusted() {
    let (mut world, map) = sample_world();
    world.save.path = r"C:\data\live\server.sav".into();
    world.save.name = "Base_autosave_2".into();
    let fp = "AB:CD:EF:01";
    let calls: Calls = Default::default();
    let log = calls.clone();
    let trusted = Rc::new(RefCell::new(false));
    let t = trusted.clone();
    let save = world.save.clone();
    let hook: RoomHook = Rc::new(move |cmd: &str, args: &Value| {
        log.borrow_mut().push((cmd.to_string(), args.clone()));
        match cmd {
            "status" => {
                let mut s = Settings { setup_done: true, ..Default::default() };
                s.live.enabled = true;
                Some(Ok(serde_json::to_value(Status { icons_found: 747, ui_assets_found: 17, map_found: true, settings: s, ..Default::default() }).unwrap()))
            }
            "map_info" => Some(Ok(serde_json::to_value(&map).unwrap())),
            "map_tile" => Some(Err("no tiles in tests".into())),
            "world_sessions" => Some(Ok(json!([]))),
            "world_load" => Some(Ok(serde_json::to_value(&world).unwrap())),
            "live_server_check" => Some(Ok(if *t.borrow() {
                json!({ "reachable": true, "health": "healthy", "token_set": true, "token_ok": true, "session": "Base", "players": 2, "player_limit": 4 })
            } else {
                json!({ "reachable": true, "untrusted_cert": fp, "token_set": true })
            })),
            "live_server_trust" => {
                assert_eq!(args["fingerprint"], fp, "only what the server showed");
                *t.borrow_mut() = true;
                let mut s = Settings { setup_done: true, ..Default::default() };
                s.live.enabled = true;
                s.live.server_host = "10.0.0.5".into();
                s.live.server_cert = Some(fp.into());
                Some(Ok(serde_json::to_value(Status { settings: s, ..Default::default() }).unwrap()))
            }
            "live_server_sync" => Some(Ok(serde_json::to_value(&save).unwrap())),
            "live_players" => Some(Ok(json!([{ "name": "Ada", "x": 0.0, "y": 0.0, "z": 0.0, "yaw": 0.0, "online": true }]))),
            "live_buildings" => Some(Ok(json!({ "machines": [{ "class": "Build_ConstructorMk1_C", "recipe": "Recipe_IronPlate_C", "x": 9.9e6, "y": 9.9e6, "z": 0.0, "yaw": 0.0, "clock": 1.0, "productivity": 50.0, "producing": true, "paused": false }], "carriers": null, "errors": [] }))),
            _ => None,
        }
    });
    let Some((ctx, mut app, _)) = harness_with(Some(hook)) else { return };
    frames(&ctx, &mut app, 2);
    press(&ctx, &mut app, "Map");
    frames(&ctx, &mut app, 2);
    // The add-on's own button; its dialog: an address, then Check server.
    press(&ctx, &mut app, "Live map…");
    frames(&ctx, &mut app, 1);
    if let crate::dialogs::Dialog::Live { draft } = &mut app.dialog {
        draft.server_host = "10.0.0.5".into();
    }
    frames(&ctx, &mut app, 1);
    frames(&ctx, &mut app, 2);
    press(&ctx, &mut app, "Check server");
    frames(&ctx, &mut app, 3);
    // Unknown certificate: shown to compare, trusted only on request.
    let texts = drawn_texts(&ctx, &mut app);
    assert!(texts.iter().any(|t| t.contains(fp)), "the fingerprint is shown");
    assert!(!calls.borrow().iter().any(|(c, _)| c == "live_server_trust"));
    press(&ctx, &mut app, "Trust this certificate");
    frames(&ctx, &mut app, 3);
    let texts = drawn_texts(&ctx, &mut app);
    assert!(texts.iter().any(|t| t.contains("Running “Base”, 2 of 4 players on")), "{:?}", texts.iter().filter(|t| t.contains("Base")).collect::<Vec<_>>());
    press(&ctx, &mut app, "Close");
    frames(&ctx, &mut app, 1);
    assert!(matches!(app.dialog, crate::dialogs::Dialog::None));
    // Following it: its newest save loads.
    app.choose_session(crate::live::SERVER_SESSION.into());
    frames(&ctx, &mut app, 4);
    assert!(calls.borrow().iter().any(|(c, _)| c == "live_server_sync"));
    assert_eq!(app.map.world.as_ref().map(|w| w.save.name.clone()).as_deref(), Some("Base_autosave_2"));
    // Live players from the mod, once it's turned on: named.
    if let Some(s) = app.status.as_mut() {
        s.settings.live.frm = true;
    }
    frames(&ctx, &mut app, 3);
    assert!(drawn_texts(&ctx, &mut app).iter().any(|t| t == "Ada"));
    // And what's built now, over the server's save: the one machine the
    // mod reports is new; the save's are taken down.
    assert!(calls.borrow().iter().any(|(c, _)| c == "live_buildings"));
    let saved = app.live.base.as_ref().map(|w| w.machines.len()).unwrap();
    assert_eq!(app.live.changes, (1, saved));
    assert_eq!(app.map.world.as_ref().unwrap().machines.len(), 1);
    // Turned off: the save as it is.
    if let Some(s) = app.status.as_mut() {
        s.settings.live.frm = false;
    }
    frames(&ctx, &mut app, 2);
    assert_eq!(app.map.world.as_ref().unwrap().machines.len(), saved);
}

#[test]
fn live_buildings_are_laid_over_the_save() {
    use fb_model::world::{Carrier, Link, Machine, PowerUse, World};
    use fb_model::{LiveBuildings, LiveCarrier, LiveMachine};
    let Some((_, app, _)) = harness() else { return };
    let game = fb_engine::docs::load_game_data(std::path::Path::new(&game_dir())).unwrap();
    let _ = app;
    let m = |id: u32, x: f32, recipe: &str| Machine { id, class: "Build_ConstructorMk1_C".into(), recipe: Some(recipe.into()), clock: 1.0, boost: 1.0, x, y: 0.0, z: 0.0, yaw: 0.0, node: None };
    let belt = |class: &str, a: [f32; 2], b: [f32; 2]| Carrier { class: class.into(), fluid: false, points: vec![a, b], yaw: 0.0 };
    let mut base = World::default();
    base.machines = vec![m(0, 0.0, "Recipe_IronPlate_C"), m(1, 1000.0, "Recipe_IronRod_C"), m(2, 2000.0, "Recipe_Screw_C")];
    base.links = vec![Link { from: 0, to: 1, item: None }, Link { from: 0, to: 2, item: None }];
    base.machine_power.insert(2, PowerUse { now: 4.0, max: 4.0, makes: 0.0 });
    base.carriers = vec![
        belt("Build_ConveyorBeltMk1_C", [0.0, 0.0], [500.0, 0.0]),
        belt("Build_ConveyorBeltMk1_C", [0.0, 900.0], [500.0, 900.0]),
        belt("Build_ConveyorLiftMk1_C", [9000.0, 0.0], [9000.0, 10.0]),
    ];
    let lm = |x: f32, recipe: &str| LiveMachine { class: "Build_ConstructorMk1_C".into(), recipe: Some(recipe.into()), x, y: 40.0, producing: Some(true), productivity: Some(80.0), ..Default::default() };
    let live = LiveBuildings {
        // The first moved a little and makes rods now; the second is gone;
        // the third is there; one more is new.
        machines: Some(vec![lm(2000.0, "Recipe_Screw_C"), lm(30.0, "Recipe_IronRod_C"), lm(5000.0, "Recipe_Wire_C")]),
        // The first belt, reported the other way round; a new one.
        carriers: Some(vec![
            LiveCarrier { class: "Build_ConveyorBeltMk1_C".into(), fluid: false, points: vec![[480.0, 20.0], [10.0, 0.0]] },
            LiveCarrier { class: "Build_ConveyorBeltMk2_C".into(), fluid: false, points: vec![[0.0, 5000.0], [800.0, 5000.0]] },
        ]),
        errors: vec![],
    };
    let out = crate::live::merge(&base, &live, None, &game);
    let w = &out.world;
    let ids: Vec<u32> = w.machines.iter().map(|m| m.id).collect();
    assert_eq!(ids, vec![0, 1, 2], "ids stay indexes");
    assert_eq!(w.machines[0].recipe.as_deref(), Some("Recipe_IronRod_C"), "its recipe as it is now");
    assert_eq!(w.machines[1].x, 2000.0);
    assert_eq!(w.machines[2].recipe.as_deref(), Some("Recipe_Wire_C"));
    assert_eq!((out.added, out.removed, out.new_from), (1, 1, 2));
    assert_eq!(w.links, vec![Link { from: 0, to: 1, item: None }], "the link to the one taken down goes; the other follows its machine");
    assert!(w.machine_power.contains_key(&1) && w.machine_power.len() == 1);
    assert_eq!(out.status[0].as_ref().unwrap().productivity, Some(80.0));
    let classes: Vec<&str> = w.carriers.iter().map(|c| c.class.as_str()).collect();
    assert_eq!(classes, vec!["Build_ConveyorBeltMk1_C", "Build_ConveyorLiftMk1_C", "Build_ConveyorBeltMk2_C"], "matched either way round; lifts stay; the gone one goes");
    // Without the mod's machines, the save's stay as they are.
    let partial = LiveBuildings { machines: None, carriers: None, errors: vec!["no answer".into()] };
    assert_eq!(crate::live::merge(&base, &partial, None, &game).world.machines.len(), 3);
}

/// The Live view: a live factory's graph from a region over the save. Its
/// nodes move, settings stay as built; in planning mode pieces can be added
/// and wired, and planned pieces have their menu.
#[test]
fn live_view_moves_nodes_and_plans() {
    let (world, map) = sample_world();
    let region = fb_model::LiveFactoryDef { name: "Plates".into(), region: vec![[39_000.0, 40_000.0], [60_000.0, 40_000.0], [60_000.0, 60_000.0], [39_000.0, 60_000.0]], ..Default::default() };
    let r2 = region.clone();
    let hook: RoomHook = Rc::new(move |cmd: &str, _args: &Value| {
        let settings = || {
            let mut s = Settings { setup_done: true, ..Default::default() };
            s.live.enabled = true;
            s.live.factories = vec![r2.clone()];
            s
        };
        match cmd {
            "status" => Some(Ok(serde_json::to_value(Status { icons_found: 747, ui_assets_found: 17, map_found: true, settings: settings(), ..Default::default() }).unwrap())),
            "map_info" => Some(Ok(serde_json::to_value(&map).unwrap())),
            "map_tile" => Some(Err("no tiles in tests".into())),
            "world_sessions" => Some(Ok(json!([]))),
            _ => None,
        }
    });
    let Some((ctx, mut app, _)) = harness_with(Some(hook)) else { return };
    frames(&ctx, &mut app, 2);
    app.map.set_world(world);
    app.open_live("Plates");
    frames(&ctx, &mut app, 4);
    eprintln!("live on {} active {} name {} nodes {} docs {:?}", app.live_view.on, app.active, app.doc_name(app.active), app.node_count(), app.docs.iter().map(|d| (d.id, d.name.clone())).collect::<Vec<_>>());
    assert!(app.live_view.on && app.doc_name(app.active) == "Plates", "the live factory is open");
    let planned: Vec<bool> = app.graph().nodes.iter().map(|(_, n)| n.user_data.planned).collect();
    eprintln!("planned flags {planned:?}");
    assert!(planned.iter().all(|p| !p), "nothing built is planned");
    // Move a node by its title.
    let (id, rect, before) = app.first_node().expect("a node");
    let start = rect.left_top() + egui::vec2(30.0, 12.0);
    drag(&ctx, &mut app, start, start + egui::vec2(80.0, 0.0));
    let moved = app.node_pos(id) - before;
    eprintln!("moved by {moved:?}, toast {:?}", app.toast);
    assert!(moved.x > 40.0, "the node moved: {moved:?}");
    // Kept: the live project built again from nothing (as after reconnecting
    // or restarting) puts it back where it was moved to.
    let kind = app.graph()[id].user_data.kind.clone();
    let at = app.node_pos_unzoomed(id);
    assert!(app.live_settings().layouts.contains_key("Test|Plates"), "the layout is saved, for this session: {:?}", app.live_settings().layouts.keys().collect::<Vec<_>>());
    assert_eq!(app.live_settings().factories[0].world, "Test", "the region belongs to the session it was seen with");
    app.leave_live();
    app.live_view = Default::default();
    app.open_live("Plates");
    frames(&ctx, &mut app, 4);
    let again = app.graph().nodes.iter().find(|(_, n)| n.user_data.kind == kind).map(|(i, _)| i).expect("the node again");
    let back = app.node_pos_unzoomed(again);
    assert!((back - at).length() < 1.0, "same place after a rebuild: {back:?} vs {at:?}");
    let (id, rect, _) = app.first_node().expect("a node");
    let _ = (id, rect);
    // Right-click a built node: no editing menu; right-click the canvas
    // in planning mode: Add node.
    app.set_planning(true);
    frames(&ctx, &mut app, 3);
    let spot = empty_spot(&app);
    click(&ctx, &mut app, spot, egui::PointerButton::Secondary);
    eprintln!("finder open {}", app.finder.is_some());
    assert!(app.finder.is_some(), "Add node opens in planning mode");
    // Add a planned Iron Ingot smelter: marked planned, kept.
    frame_with(&ctx, &mut app, vec![egui::Event::Text("iron ingot".into())]);
    frames(&ctx, &mut app, 1);
    frame_with(&ctx, &mut app, vec![egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }]);
    frames(&ctx, &mut app, 3);
    let planned: Vec<egui_node_graph2::NodeId> = app.graph().nodes.iter().filter(|(_, n)| n.user_data.planned).map(|(id, _)| id).collect();
    eprintln!("planned nodes {} toast {:?}", planned.len(), app.toast);
    assert_eq!(planned.len(), 1, "one planned piece");
    // Wire the built iron ore deposit (any number of wires) into the plan.
    use egui_node_graph2::AnyParameterId;
    let g = app.graph();
    let plate_out = g.nodes.iter().filter(|(_, n)| !n.user_data.planned).flat_map(|(_, n)| n.outputs.iter().map(|(_, o)| *o)).find(|o| g[*o].typ.0 == "Desc_OreIron_C").expect("a built ore output");
    let plate_in = g[planned[0]].inputs.iter().map(|(_, i)| *i).find(|i| g[*i].typ.0 == "Desc_OreIron_C").expect("planned ore input");
    frames(&ctx, &mut app, 1);
    let from = *app.port_hooks[&AnyParameterId::Output(plate_out)].last().unwrap();
    let to = app.port_hooks[&AnyParameterId::Input(plate_in)][0];
    drag(&ctx, &mut app, from, to);
    frames(&ctx, &mut app, 2);
    let wired = app.graph().connections(plate_in).contains(&plate_out);
    eprintln!("planned wire made {wired} toast {:?}", app.toast);
    assert!(wired, "a planned wire from what's built to the plan");
    // Kept with the live factory.
    let plan = app.live_defs()[0].plan.clone();
    eprintln!("plan saved {} chars", plan.len());
    assert!(plan.contains("Recipe_IngotIron_C") && plan.contains("Desc_OreIron_C"), "the plan and its wire are saved: {plan}");
    // Right-click a built node: its menu, without Delete or Duplicate.
    let built = app.graph().nodes.iter().find(|(_, n)| !n.user_data.planned).map(|(id, _)| id).unwrap();
    let rect = app.node_rects[&built];
    click(&ctx, &mut app, rect.left_top() + egui::vec2(30.0, 12.0), egui::PointerButton::Secondary);
    let labels: Vec<String> = crate::widgets::TEST_BUTTONS.with(|b| b.borrow().iter().map(|(l, _, _)| l.clone()).collect());
    eprintln!("menu open {} labels {:?}", app.context_menu.is_some(), labels.iter().filter(|l| ["Delete", "Duplicate", "Add comment frame", "Rename"].contains(&l.as_str())).collect::<Vec<_>>());
    assert!(app.context_menu.is_some(), "the menu opens on a built node");
    assert!(!labels.iter().any(|l| l == "Delete" || l == "Duplicate"), "nothing that changes it");
    // Planning mode off: only what's built; on again: the plan is back, wired.
    frame_with(&ctx, &mut app, key(egui::Key::Escape));
    app.set_planning(false);
    frames(&ctx, &mut app, 3);
    assert!(app.graph().nodes.iter().all(|(_, n)| !n.user_data.planned), "no planned pieces with planning mode off");
    app.set_planning(true);
    frames(&ctx, &mut app, 3);
    let g = app.graph();
    let back: Vec<_> = g.nodes.iter().filter(|(_, n)| n.user_data.planned).map(|(id, _)| id).collect();
    assert_eq!(back.len(), 1, "the planned piece is back");
    let wired_back = g[back[0]].inputs.iter().any(|(_, i)| !g.connections(*i).is_empty());
    assert!(wired_back, "with its wire");
}

/// Generators in a region count, even a Biomass Burner holding no fuel.
#[test]
fn live_regions_take_their_generators() {
    let Some((ctx, mut app, _)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    let (mut world, _) = sample_world();
    let id = world.machines.len() as u32;
    world.machines.push(fb_model::world::Machine { id, class: "Build_GeneratorBiomass_Automated_C".into(), recipe: None, clock: 1.0, boost: 1.0, x: 41_000.0, y: 50_000.0, z: 0.0, yaw: 0.0, node: None });
    let def = fb_model::LiveFactoryDef { name: "Plates".into(), region: vec![[39_000.0, 40_000.0], [60_000.0, 40_000.0], [60_000.0, 60_000.0], [39_000.0, 60_000.0]], ..Default::default() };
    let lp = crate::map_import::region_project(&world, &[def], &Default::default(), &app.user.game, None);
    let f = &lp.file.factories[0].factory;
    eprintln!("GEN count {}", app.user.game.recipes.keys().filter(|k| k.starts_with(fb_model::GENERATOR_PREFIX)).count()); for r in app.user.game.recipes.values().filter(|r| r.class.starts_with(fb_model::GENERATOR_PREFIX)).take(3) { eprintln!("RECIPE {} building {} gen {}", r.class, r.building, r.is_generator()); }
    let gens: Vec<&str> = f.nodes.iter().filter(|n| n.class.starts_with(fb_model::GENERATOR_PREFIX)).map(|n| n.class.as_str()).collect();
    assert_eq!(gens.len(), 1, "the burner is in the factory: {:?}", f.nodes.iter().map(|n| &n.class).collect::<Vec<_>>());
    assert!(gens[0].contains("Build_GeneratorBiomass_Automated_C"));
}

/// The save's HUB on the live master graph: its burners burning what they
/// burn in game, and on their power circuit with a factory.
#[test]
fn live_hub_has_the_saves_burners_and_power() {
    let Some((ctx, mut app, _)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    let (mut world, _) = sample_world();
    world.structures.push(fb_model::world::Structure { class: "Build_TradingPost_C".into(), x: 10_000.0, y: 10_000.0, yaw: 0.0, circuit: None, power: None });
    world.hub_burners = vec![
        fb_model::world::HubBurner { x: 10_000.0, y: 10_100.0, circuit: Some(0), makes: 12.0, fuel: Some("Desc_Leaves_C".into()), fuel_amount: None },
        fb_model::world::HubBurner { x: 10_000.0, y: 10_200.0, circuit: Some(0), makes: 0.0, fuel: None, fuel_amount: None },
    ];
    world.circuits = vec![fb_model::world::Circuit::default()];
    world.machine_circuit.insert(1, 0);
    let def = fb_model::LiveFactoryDef { name: "Plates".into(), region: vec![[39_000.0, 40_000.0], [60_000.0, 40_000.0], [60_000.0, 60_000.0], [39_000.0, 60_000.0]], ..Default::default() };
    let lp = crate::map_import::region_project(&world, &[def], &Default::default(), &app.user.game, None);
    let hub = lp.file.factories.iter().find(|f| f.hub).expect("the HUB");
    let burners: Vec<&str> = hub.factory.nodes.iter().filter(|n| n.kind == crate::portable::Kind::Burner).map(|n| n.class.as_str()).collect();
    assert_eq!(burners.len(), 2, "both burners");
    assert!(burners.contains(&"Desc_Leaves_C"), "burning leaves, as in game: {burners:?}");
    assert!(hub.factory.wires.len() == 2, "each fed its fuel");
    use crate::power_net::End;
    let linked = lp.file.power_links.iter().any(|l| l.from == End::Factory(hub.id) || l.to == End::Factory(hub.id));
    assert!(linked, "the HUB is on the factory's circuit: {:?}", lp.file.power_links);
}

/// A live factory shows only with the save session it was drawn on.
#[test]
fn live_regions_belong_to_their_session() {
    let Some((ctx, mut app, _)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    let (mut world, _) = sample_world();
    let region = fb_model::LiveFactoryDef { name: "Plates".into(), world: "Test".into(), region: vec![[39_000.0, 40_000.0], [60_000.0, 40_000.0], [60_000.0, 60_000.0], [39_000.0, 60_000.0]], ..Default::default() };
    if let Some(s) = app.status.as_mut() {
        s.settings.live.enabled = true;
        s.settings.live.factories = vec![region];
    }
    app.map.set_world(world.clone());
    assert_eq!(app.world_defs().len(), 1, "its own session");
    app.live_outlines_refresh();
    assert_eq!(app.live_map.outlines.len(), 1);
    world.save.session = "Another".into();
    app.map.set_world(world);
    assert!(app.world_defs().is_empty(), "not on another session's map");
    app.live_outlines_refresh();
    assert!(app.live_map.outlines.is_empty(), "no outline there");
}

/// A newer build: a notice offers it, and only the button installs it.
#[test]
fn an_update_is_offered_and_installed_only_when_asked() {
    let calls: Calls = Default::default();
    let log = calls.clone();
    let hook: RoomHook = Rc::new(move |cmd: &str, args: &Value| {
        log.borrow_mut().push((cmd.to_string(), args.clone()));
        match cmd {
            "update_check" => Some(Ok(json!({ "build": 9, "current": 7, "title": "Factory Builder 0.1.0 (build 9)", "size": 50_000_000 }))),
            "update_apply" => Some(Ok(Value::Null)),
            _ => None,
        }
    });
    let Some((ctx, mut app, _)) = harness_with(Some(hook)) else { return };
    frames(&ctx, &mut app, 4);
    let texts = drawn_texts(&ctx, &mut app);
    assert!(texts.iter().any(|t| t.contains("build 9 (you have build 7)")), "the notice shows");
    assert!(!calls.borrow().iter().any(|(c, _)| c == "update_apply"), "nothing installed without asking");
    press(&ctx, &mut app, "Update and restart");
    frames(&ctx, &mut app, 2);
    assert_eq!(calls.borrow().iter().filter(|(c, _)| c == "update_check").count(), 1, "checked once");
    assert!(calls.borrow().iter().any(|(c, _)| c == "update_apply"), "installed when asked");
}

#[test]
fn routers_keep_belts_apart() {
    use crate::graph::{Template, MACHINES, RATE};
    let Some((ctx, mut app, _)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    let (ore, recipe) = ("Desc_OreIron_C", "Recipe_IngotIron_C");
    let per = { let r = &app.user.game.recipes[recipe]; r.per_min(&r.ingredients[0]) };
    // A -> splitter -> X (two smelters) and -> merger; C -> merger -> Y (one smelter).
    // X can only ever get A's ore.
    let a = app.add_test_node(Template::Resource(ore.into()), egui::pos2(0.0, 0.0));
    let c = app.add_test_node(Template::Resource(ore.into()), egui::pos2(0.0, 400.0));
    let split = app.add_test_node(Template::Splitter(ore.into()), egui::pos2(300.0, 0.0));
    let merge = app.add_test_node(Template::Merger(ore.into()), egui::pos2(300.0, 400.0));
    let x = app.add_test_node(Template::Recipe(recipe.into()), egui::pos2(600.0, -200.0));
    let y = app.add_test_node(Template::Recipe(recipe.into()), egui::pos2(600.0, 400.0));
    let set = |app: &mut FactoryApp, n: egui_node_graph2::NodeId, name: &str, v: f64| {
        let i = app.state.graph[n].get_input(name).unwrap();
        app.state.graph[i].value.set_number(v);
    };
    set(&mut app, a, RATE, per);
    set(&mut app, c, RATE, per);
    set(&mut app, x, MACHINES, 2.0);
    set(&mut app, y, MACHINES, 1.0);
    connect(&mut app, a, split, ore);
    connect(&mut app, split, x, ore);
    connect(&mut app, split, merge, ore);
    connect(&mut app, c, merge, ore);
    connect(&mut app, merge, y, ore);
    frames(&ctx, &mut app, 2);
    // As in game: the merger takes equally from its two inputs (15 each),
    // so the splitter's belt to it backs up and X gets the other 15.
    let eff = |n| app.user.nodes[&n].efficiency;
    assert!((eff(x) - 0.25).abs() < 1e-4, "X: {:.4}", eff(x));
    assert!((eff(y) - 1.0).abs() < 1e-4, "Y: {:.4}", eff(y));
}

/// Wires output `out` of `from` to input `inp` of `to` (by index among
/// their item ports).
fn connect_ports(app: &mut FactoryApp, from: egui_node_graph2::NodeId, out: usize, to: egui_node_graph2::NodeId, inp: usize) -> (egui_node_graph2::InputId, egui_node_graph2::OutputId) {
    let g = &app.state.graph;
    let o = g[from].outputs[out].1;
    let items: Vec<egui_node_graph2::InputId> = g[to].inputs.iter().map(|(_, i)| *i).filter(|i| matches!(g[*i].value, crate::graph::PortValue::Item(_))).collect();
    let i = items[inp];
    let pos = g.connections(i).len();
    app.state.graph.add_connection(o, i, pos);
    app.changed();
    (i, o)
}

#[test]
fn smart_splitters_priority_mergers_and_valves_work_like_the_game() {
    use crate::graph::{NodeKind, Template, MACHINES, RATE};
    use crate::routers::{Priority, Rule};
    let Some((ctx, mut app, _)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    let (ore, recipe) = ("Desc_OreIron_C", "Recipe_IngotIron_C");
    let per = { let r = &app.user.game.recipes[recipe]; r.per_min(&r.ingredients[0]) };
    let set = |app: &mut FactoryApp, n: egui_node_graph2::NodeId, name: &str, v: f64| {
        let i = app.state.graph[n].get_input(name).unwrap();
        app.state.graph[i].value.set_number(v);
    };
    let flow = |app: &FactoryApp, w: (egui_node_graph2::InputId, egui_node_graph2::OutputId)| app.wire_flow.get(&w).copied().unwrap_or(-1.0);

    // Ore (1.5 smelters' worth) -> Smart Splitter: Left to a smelter, Right
    // to a sink. Passing everything, it shares equally, as in game.
    let src = app.add_test_node(Template::Resource(ore.into()), egui::pos2(0.0, 0.0));
    let smart = app.add_test_node(Template::SmartSplitter { item: ore.into(), programmable: false }, egui::pos2(300.0, 0.0));
    let smelter = app.add_test_node(Template::Recipe(recipe.into()), egui::pos2(650.0, -200.0));
    let sink = app.add_test_node(Template::Sink(ore.into()), egui::pos2(650.0, 200.0));
    set(&mut app, src, RATE, 1.5 * per);
    set(&mut app, smelter, MACHINES, 1.0);
    connect(&mut app, src, smart, ore);
    let left = connect_ports(&mut app, smart, 0, smelter, 0);
    let right = connect_ports(&mut app, smart, 2, sink, 0);
    frames(&ctx, &mut app, 2);
    assert!((flow(&app, left) - 0.75 * per).abs() < 1e-3 && (flow(&app, right) - 0.75 * per).abs() < 1e-3, "{} {}", flow(&app, left), flow(&app, right));

    // Right Output set to Overflow from its menu: the smelter gets all it
    // takes, the sink the rest.
    frames(&ctx, &mut app, 2);
    press(&ctx, &mut app, "Right Output setting");
    frames(&ctx, &mut app, 2);
    press(&ctx, &mut app, "    Overflow");
    frames(&ctx, &mut app, 3);
    assert!(matches!(&app.state.graph[smart].user_data.kind, NodeKind::SmartSplitter { rules, .. } if rules[2] == vec![Rule::Overflow]));
    assert!((flow(&app, left) - per).abs() < 1e-3 && (flow(&app, right) - 0.5 * per).abs() < 1e-3, "{} {}", flow(&app, left), flow(&app, right));
    assert!((app.user.nodes[&smelter].efficiency - 1.0).abs() < 1e-4);
    // None on the Left: nothing passes there; the overflow gets it all.
    if let NodeKind::SmartSplitter { rules, .. } = &mut app.state.graph[smart].user_data.kind {
        rules[0] = vec![Rule::None];
    }
    app.changed();
    frames(&ctx, &mut app, 2);
    assert!(flow(&app, left).abs() < 1e-6 && (flow(&app, right) - 1.5 * per).abs() < 1e-3, "{} {}", flow(&app, left), flow(&app, right));

    // A Priority Merger: two sources of one smelter's worth into a smelter
    // pair that takes 1.5 of them; High is merged before Low.
    let a = app.add_test_node(Template::Resource(ore.into()), egui::pos2(0.0, 800.0));
    let b = app.add_test_node(Template::Resource(ore.into()), egui::pos2(0.0, 1000.0));
    let pm = app.add_test_node(Template::PriorityMerger(ore.into()), egui::pos2(300.0, 900.0));
    let pair = app.add_test_node(Template::Recipe(recipe.into()), egui::pos2(650.0, 900.0));
    set(&mut app, a, RATE, per);
    set(&mut app, b, RATE, per);
    set(&mut app, pair, MACHINES, 1.0);
    set(&mut app, pair, crate::graph::CLOCK, 150.0);
    let low = connect_ports(&mut app, a, 0, pm, 0);
    let high = connect_ports(&mut app, b, 0, pm, 2);
    connect(&mut app, pm, pair, ore);
    frames(&ctx, &mut app, 2);
    // Equal (all Low): half and half.
    assert!((flow(&app, low) - 0.75 * per).abs() < 1e-3 && (flow(&app, high) - 0.75 * per).abs() < 1e-3, "{} {}", flow(&app, low), flow(&app, high));
    app.frame_all = true;
    frames(&ctx, &mut app, 3);
    press(&ctx, &mut app, "Right Input setting");
    frames(&ctx, &mut app, 2);
    press(&ctx, &mut app, "    High");
    frames(&ctx, &mut app, 3);
    assert!(matches!(&app.state.graph[pm].user_data.kind, NodeKind::PriorityMerger { priority, .. } if priority[2] == Priority::High));
    assert!((flow(&app, high) - per).abs() < 1e-3 && (flow(&app, low) - 0.5 * per).abs() < 1e-3, "{} {}", flow(&app, low), flow(&app, high));

    // A Valve holds a pipe to its limit.
    let water = "Desc_Water_C";
    let w = app.add_test_node(Template::Resource(water.into()), egui::pos2(0.0, 1600.0));
    let valve = app.add_test_node(Template::Valve(water.into()), egui::pos2(300.0, 1600.0));
    let out = app.add_test_node(Template::Sink(water.into()), egui::pos2(650.0, 1600.0));
    set(&mut app, w, RATE, 120.0);
    set(&mut app, valve, crate::routers::VALVE_LIMIT, 50.0);
    connect(&mut app, w, valve, water);
    let piped = connect(&mut app, valve, out, water);
    frames(&ctx, &mut app, 2);
    assert!((flow(&app, piped) - 50.0).abs() < 1e-3, "{}", flow(&app, piped));

    // All of it survives the file format.
    let f = app.to_portable();
    let back = crate::portable::FactoryFile::parse(&f.to_json(false)).unwrap();
    assert_eq!(back, f);
    let kinds: Vec<_> = back.nodes.iter().map(|n| n.kind).collect();
    for k in [crate::portable::Kind::SmartSplitter, crate::portable::Kind::PriorityMerger, crate::portable::Kind::Valve] {
        assert!(kinds.contains(&k), "{k:?}");
    }
    let before: Vec<NodeKind> = [smart, pm, valve].iter().map(|n| app.state.graph[*n].user_data.kind.clone()).collect();
    let (ids, skipped) = app.insert_portable(&back, egui::vec2(0.0, 3000.0));
    assert_eq!(skipped, 0);
    for k in before {
        assert!(ids.iter().any(|n| app.state.graph[*n].user_data.kind == k), "{k:?} back from the file");
    }
    let v = ids.iter().find(|n| matches!(app.state.graph[**n].user_data.kind, NodeKind::Valve { .. })).unwrap();
    assert_eq!(crate::graph::input_number(&app.state.graph, *v, crate::routers::VALVE_LIMIT), 50.0);
}

#[test]
fn plans_send_leftovers_through_a_smart_splitters_overflow() {
    use crate::graph::NodeKind;
    use crate::routers::Rule;
    let Some((ctx, mut app, _)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    // Plastic is both a product and used inside (Recycled Rubber): its belt
    // feeds machines and the Output, so the Output takes the overflow.
    plan_hmf(&ctx, &mut app);
    let g = &app.state.graph;
    let smart: Vec<_> = g.iter_nodes().filter(|n| matches!(&g[*n].user_data.kind, NodeKind::SmartSplitter { rules, .. } if rules.iter().any(|r| r.contains(&Rule::Overflow)))).collect();
    assert!(!smart.is_empty(), "a Smart Splitter with an Overflow output");
    assert!((app.min_efficiency() - 1.0).abs() < 1e-4, "every machine at 100%: {}", app.min_efficiency());
}

#[test]
fn longer_messages_show_longer() {
    use crate::room_extras::bubble_secs;
    assert_eq!(bubble_secs("ok"), 4.0);
    assert!(bubble_secs(&"x".repeat(60)) > bubble_secs(&"x".repeat(30)));
    assert_eq!(bubble_secs(&"x".repeat(fb_model::room::MAX_CHAT)), 13.0);
}

#[test]
fn a_motor_plan_delivers() {
    let Some((ctx, mut app, _)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    app.planner_mut().targets = vec![Target { item: "Desc_Motor_C".into(), rate: 5.0 }];
    app.planner_mut().enabled = app.user.game.recipes.values().filter(|r| !r.alternate).map(|r| r.class.clone()).collect();
    app.press_plan();
    frames(&ctx, &mut app, 3);
    let outs = app.outputs();
    assert!(outs.iter().any(|o| o.0 == "Desc_Motor_C" && (o.1 - 5.0).abs() < 1e-3), "{outs:?}");
}

#[test]
fn storage_passes_on_what_arrives_and_reads_both_sides() {
    use crate::graph::Template;
    let Some((ctx, mut app, _)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    let ore = "Desc_OreIron_C";
    let res = app.add_test_node(Template::Resource(ore.into()), egui::pos2(0.0, 0.0));
    let miner = app.add_test_node(Template::Extractor { item: ore.into(), class: "Build_MinerMk1_C".into(), purity: fb_model::Purity::Normal }, egui::pos2(300.0, 0.0));
    let st = app.add_test_node(Template::Storage { item: ore.into(), class: "Build_StorageContainerMk1_C".into() }, egui::pos2(600.0, 0.0));
    let sm = app.add_test_node(Template::Recipe("Recipe_IngotIron_C".into()), egui::pos2(900.0, 0.0));
    connect(&mut app, res, miner, ore);
    connect(&mut app, miner, st, ore);
    connect(&mut app, st, sm, ore);
    frames(&ctx, &mut app, 2);
    assert!(app.state.graph[st].get_input(crate::graph::RATE).is_err(), "no Rate field");
    let port = |name: &str| app.user.ports[&(st, name.to_string())];
    // The miner's 60 go in; the smelter takes its 30; the rest fills it.
    assert!((port(crate::storage::INPUT).have - 60.0).abs() < 1e-3);
    assert!((port(crate::storage::OUTPUT).have - 30.0).abs() < 1e-3);
    assert!((app.user.nodes[&sm].efficiency - 1.0).abs() < 1e-4);
    assert!(app.summary.outputs.iter().any(|o| o.0 == ore && (o.1 - 30.0).abs() < 1e-3), "{:?}", app.summary.outputs);
}

#[test]
fn a_backed_up_miner_gets_a_hint() {
    use crate::graph::Template;
    let Some((ctx, mut app, _)) = harness() else { return };
    frames(&ctx, &mut app, 2);
    let ore = "Desc_OreIron_C";
    let res = app.add_test_node(Template::Resource(ore.into()), egui::pos2(0.0, 0.0));
    let miner = app.add_test_node(Template::Extractor { item: ore.into(), class: "Build_MinerMk1_C".into(), purity: fb_model::Purity::Normal }, egui::pos2(300.0, 0.0));
    let sm = app.add_test_node(Template::Recipe("Recipe_IngotIron_C".into()), egui::pos2(600.0, 0.0));
    let out = app.add_test_node(Template::Output("Desc_IronIngot_C".into()), egui::pos2(900.0, 0.0));
    connect(&mut app, res, miner, ore);
    connect(&mut app, miner, sm, ore);
    connect(&mut app, sm, out, "Desc_IronIngot_C");
    frames(&ctx, &mut app, 2);
    // The miner could do 60; the smelter takes 30.
    let held = app.user.nodes[&miner].held.expect("the miner is held back");
    assert!((held.0 - 30.0).abs() < 1e-3 && (held.1 - 60.0).abs() < 1e-3, "{held:?}");
    assert!(app.user.nodes[&sm].held.is_none(), "the smelter runs flat out");
}
