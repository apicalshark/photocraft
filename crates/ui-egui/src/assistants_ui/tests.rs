use super::*;
use crate::canvas::{ToolEvent, ViewXform};
use crate::state::Tool;
use egui::{Pos2, Rect, vec2};
use photocraft_engine::Session;
use serde_json::{Value, json};

fn app() -> PhotocraftApp {
    let mut app = PhotocraftApp::new(Session::new(), crate::Services::default());
    app.run("file.new", json!({"width": 200, "height": 150, "background": "white"})).unwrap();
    app.sync_views();
    app.ui.tool = Tool::Brush;
    app.ui.views[0].zoom = 1.0;
    app.ui.views[0].fit_pending = false;
    app.last_canvas_rect = Rect::from_min_size(Pos2::ZERO, vec2(500.0, 400.0));
    app
}

fn down(app: &mut PhotocraftApp, p: [f64; 2]) {
    crate::canvas::tool_event(app, ToolEvent::Down { x: p[0], y: p[1], pressure: 1.0 }, egui::Modifiers::NONE);
}

fn move_to(app: &mut PhotocraftApp, p: [f64; 2], mods: egui::Modifiers) {
    crate::canvas::tool_event(app, ToolEvent::Move { x: p[0], y: p[1], pressure: 1.0 }, mods);
}

fn up(app: &mut PhotocraftApp, p: [f64; 2]) {
    crate::canvas::tool_event(app, ToolEvent::Up { x: p[0], y: p[1] }, egui::Modifiers::NONE);
}

fn strokes(app: &PhotocraftApp) -> usize {
    app.session.journal.iter().filter(|(id, _)| *id == "paint.stroke").count()
}

fn anchor(app: &PhotocraftApp, id: u64) -> [f64; 2] {
    let st = app.session.active().unwrap();
    let a = st.doc.assistants.get(AssistantId(id)).unwrap();
    match &a.kind {
        DrawingAssistantKind::StraightLine(l) => l.anchor,
        _ => panic!("not a line"),
    }
}

/// How many shapes the assistants overlay paints this frame.
fn overlay_shapes(app: &PhotocraftApp, ctx: &egui::Context) -> usize {
    let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(400.0, 400.0));
        let painter = ui.ctx().layer_painter(egui::LayerId::background()).with_clip_rect(rect);
        let xf = ViewXform { rect, zoom: 1.0, center: [100.0, 75.0], flip: false, rotation: 0.0, aspect: 1.0 };
        draw(app, &painter, &xf);
    });
    out.textures_delta.clear();
    out.shapes.len()
}

#[test]
fn overlay_draws_guides_and_handles_and_respects_show_flags() {
    let ctx = egui::Context::default();
    let mut app = app();
    assert_eq!(overlay_shapes(&app, &ctx), 0, "no assistants, nothing drawn");
    app.run("assist.addStraightLine", json!({"anchor": [100.0, 75.0], "angleDeg": 0.0})).unwrap();
    app.run("assist.addPerspective", json!({"points": [[150.0, 75.0]], "rayStep": 90.0})).unwrap();
    assert!(overlay_shapes(&app, &ctx) > 0, "segments and handles draw");
    let toggle = |app: &mut PhotocraftApp, id: &str| crate::menus::invoke(app, &ctx, id, Value::Null).unwrap();
    toggle(&mut app, "view.show.assistants");
    assert!(!app.ui.view.show.assistants);
    assert_eq!(overlay_shapes(&app, &ctx), 0, "Show flag off hides the overlay");
    toggle(&mut app, "view.show.assistants");
    toggle(&mut app, "view.extras");
    assert!(!app.ui.view.extras);
    assert_eq!(overlay_shapes(&app, &ctx), 0, "Extras off hides the overlay");
    toggle(&mut app, "view.extras");
    assert!(overlay_shapes(&app, &ctx) > 0, "shown again");
    // Disabling the set in the document hides it too.
    app.run("assist.setSnap", json!({"enabled": false})).unwrap();
    assert_eq!(overlay_shapes(&app, &ctx), 0);
}

#[test]
fn handle_grabs_require_an_active_brushlike_tool() {
    let mut app = app();
    app.run("assist.addStraightLine", json!({"anchor": [50.0, 60.0], "angleDeg": 0.0})).unwrap();
    let press = ToolEvent::Down { x: 50.0, y: 60.0, pressure: 1.0 };
    for tool in [Tool::Move, Tool::RectMarquee, Tool::Lasso, Tool::Pen, Tool::Type, Tool::Crop] {
        app.ui.tool = tool;
        assert!(!pointer(&mut app, press, egui::Modifiers::NONE), "{tool:?}");
        assert!(app.ui.assistant_drag.is_none());
    }
    app.ui.tool = Tool::Brush;
    app.tool_override = Some(Tool::Move);
    assert!(!pointer(&mut app, press, egui::Modifiers::NONE), "temporary Move tool owns the pointer");
    app.ui.tool = Tool::Move;
    app.tool_override = Some(Tool::Brush);
    assert!(pointer(&mut app, press, egui::Modifiers::NONE), "temporary Brush can grab handles");
    assert!(app.ui.assistant_drag.is_some());
    assert!(pointer(&mut app, ToolEvent::Up { x: 50.0, y: 60.0 }, egui::Modifiers::NONE));
    assert!(app.ui.assistant_drag.is_none());
    for tool in [Tool::Brush, Tool::Pencil, Tool::Eraser, Tool::CloneStamp] {
        app.tool_override = None;
        app.ui.tool = tool;
        assert!(pointer(&mut app, press, egui::Modifiers::NONE), "{tool:?}");
        assert!(app.ui.assistant_drag.is_some());
        assert!(pointer(&mut app, ToolEvent::Up { x: 50.0, y: 60.0 }, egui::Modifiers::NONE));
    }
}

#[test]
fn anchor_drag_moves_the_line_in_one_undo_step_without_painting() {
    let mut app = app();
    let id = app.run("assist.addStraightLine", json!({"anchor": [50.0, 60.0], "angleDeg": 0.0})).unwrap()["id"].as_u64().unwrap();
    let steps = app.session.active().unwrap().history.past_len();
    let before = strokes(&app);
    down(&mut app, [50.0, 60.0]);
    assert!(app.ui.assistant_drag.is_some(), "the anchor grab starts a drag");
    move_to(&mut app, [60.0, 70.0], egui::Modifiers::NONE);
    move_to(&mut app, [70.0, 80.0], egui::Modifiers::NONE);
    up(&mut app, [70.0, 80.0]);
    assert!(app.ui.assistant_drag.is_none());
    assert_eq!(anchor(&app, id), [70.0, 80.0]);
    assert_eq!(app.session.active().unwrap().history.past_len(), steps + 1, "one undo step for the drag");
    assert_eq!(strokes(&app), before, "dragging a handle never paints");
    app.session.undo();
    assert_eq!(anchor(&app, id), [50.0, 60.0]);
}

#[test]
fn angle_handle_rotates_with_shift_snap() {
    let mut app = app();
    let id = app.run("assist.addStraightLine", json!({"anchor": [50.0, 60.0], "angleDeg": 0.0})).unwrap()["id"].as_u64().unwrap();
    // Angle handle 60 px along the line.
    down(&mut app, [110.0, 60.0]);
    assert!(app.ui.assistant_drag.is_some());
    // Straight down from the anchor is 90°; ⇧ keeps the 15° snap (already exact here).
    move_to(&mut app, [50.0, 120.0], egui::Modifiers::SHIFT);
    up(&mut app, [50.0, 120.0]);
    let st = app.session.active().unwrap();
    let angle = match &st.doc.assistants.get(AssistantId(id)).unwrap().kind {
        DrawingAssistantKind::StraightLine(l) => l.angle_deg,
        _ => panic!("not a line"),
    };
    assert!((angle - 90.0).abs() < 1e-9, "{angle}");
    // Free rotation without ⇧: ~45° towards bottom-right.
    down(&mut app, [50.0, 120.0]);
    move_to(&mut app, [100.0, 110.0], egui::Modifiers::NONE);
    up(&mut app, [100.0, 110.0]);
    let st = app.session.active().unwrap();
    let angle = match &st.doc.assistants.get(AssistantId(id)).unwrap().kind {
        DrawingAssistantKind::StraightLine(l) => l.angle_deg,
        _ => panic!("not a line"),
    };
    assert!((angle - 45.0).abs() < 1e-6, "{angle}");
}

#[test]
fn vanishing_point_drag_moves_point_and_rays() {
    let mut app = app();
    app.run("assist.addPerspective", json!({"points": [[150.0, 75.0]], "rayStep": 90.0})).unwrap();
    down(&mut app, [150.0, 75.0]);
    assert!(app.ui.assistant_drag.is_some(), "the VP grab starts a drag");
    move_to(&mut app, [150.0, 90.0], egui::Modifiers::NONE);
    up(&mut app, [150.0, 90.0]);
    let st = app.session.active().unwrap();
    let vp = match &st.doc.assistants.items[0].kind {
        DrawingAssistantKind::Perspective(p) => p.vps[0],
        _ => panic!("not perspective"),
    };
    assert_eq!(vp, [150.0, 90.0]);
    // The moved horizontal ray snaps strokes: just below y = 90 lands on it.
    app.ui.tool = Tool::Brush;
    down(&mut app, [20.0, 93.0]);
    move_to(&mut app, [40.0, 93.0], egui::Modifiers::NONE);
    up(&mut app, [40.0, 93.0]);
    let (_, params) = app.session.journal.iter().rev().find(|(id, _)| *id == "paint.stroke").unwrap();
    let ys: Vec<f64> = params["points"].as_array().unwrap().iter().map(|q| q[1].as_f64().unwrap()).collect();
    assert!(ys.iter().all(|y| (*y - 93.0).abs() < 1e-9), "journal keeps raw points: {ys:?}");
}

#[test]
fn press_away_from_handles_paints_normally() {
    let mut app = app();
    app.run("assist.addStraightLine", json!({"anchor": [150.0, 120.0], "angleDeg": 0.0})).unwrap();
    down(&mut app, [10.0, 10.0]);
    assert!(app.ui.assistant_drag.is_none(), "no handle nearby: no drag");
    assert!(app.drag.is_some(), "a paint drag starts instead");
    move_to(&mut app, [20.0, 10.0], egui::Modifiers::NONE);
    up(&mut app, [20.0, 10.0]);
    assert!(app.session.journal.iter().any(|(id, _)| *id == "paint.stroke"), "the stroke commits");
}

#[test]
fn non_snapping_assistant_still_draws_dimmed_but_pipeline_skips() {
    let ctx = egui::Context::default();
    let mut app = app();
    let id = app.run("assist.addStraightLine", json!({"anchor": [100.0, 75.0], "angleDeg": 0.0})).unwrap()["id"].as_u64().unwrap();
    let drawn_snapping = overlay_shapes(&app, &ctx);
    assert!(drawn_snapping > 0);
    app.run("assist.setSnapping", json!({"id": id, "snapping": false})).unwrap();
    assert_eq!(overlay_shapes(&app, &ctx), drawn_snapping, "visible but inert: same primitives, dimmed paint");
    // The stroke pipeline no longer snaps onto it.
    down(&mut app, [20.0, 78.0]);
    move_to(&mut app, [40.0, 78.0], egui::Modifiers::NONE);
    up(&mut app, [40.0, 78.0]);
    let (_, params) = app.session.journal.iter().rev().find(|(id, _)| *id == "paint.stroke").unwrap();
    let ys: Vec<f64> = params["points"].as_array().unwrap().iter().map(|q| q[1].as_f64().unwrap()).collect();
    assert!(ys.iter().all(|y| (*y - 78.0).abs() < 1e-9), "raw points pass through: {ys:?}");
}

#[test]
fn ray_step_slider_reads_the_fan_density() {
    let mut app = app();
    let id = app.run("assist.addPerspective", json!({"points": [[100.0, 75.0]], "rayStep": 45.0})).unwrap()["id"].as_u64().unwrap();
    assert_eq!(super::ray_step_of(&app, id), Some(45.0));
    app.run("assist.setRayStep", json!({"id": id, "rayStep": 30.0})).unwrap();
    assert_eq!(super::ray_step_of(&app, id), Some(30.0));
    let lid = app.run("assist.addStraightLine", json!({})).unwrap()["id"].as_u64().unwrap();
    assert_eq!(super::ray_step_of(&app, lid), None, "straight lines have no fan");
    assert_eq!(super::ray_step_of(&app, 9999), None, "unknown id");
}
