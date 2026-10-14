//! Ellipse ruler overlay tests: headless drags through `tool_event`
//! (mirrors `symmetry_ui/tests.rs` — no GPU needed).

use super::*;
use photocraft_engine::ruler_cmds::RulerKind;
use egui::{Modifiers, Pos2, Rect, vec2};
use serde_json::json;

use crate::PhotocraftApp;
use crate::canvas::{ToolEvent, ViewXform};

fn app() -> PhotocraftApp {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("file.new", json!({"width":100,"height":80})).unwrap();
    app.sync_views();
    app.ui.tool = Tool::Brush;
    app.ui.views[0].zoom = 1.0;
    app.last_canvas_rect = Rect::from_min_size(Pos2::ZERO, vec2(500.0, 400.0));
    app
}

fn ruler(app: &mut PhotocraftApp) {
    app.run("ruler.ellipse.set", json!({"center": [50, 40], "radii": [20, 10]})).unwrap();
}

fn down(app: &mut PhotocraftApp, p: [f64; 2]) {
    crate::canvas::tool_event(app, ToolEvent::Down { x: p[0], y: p[1], pressure: 1.0 }, Modifiers::NONE);
}

fn move_to(app: &mut PhotocraftApp, p: [f64; 2], mods: Modifiers) {
    crate::canvas::tool_event(app, ToolEvent::Move { x: p[0], y: p[1], pressure: 1.0 }, mods);
}

fn up(app: &mut PhotocraftApp, p: [f64; 2]) {
    crate::canvas::tool_event(app, ToolEvent::Up { x: p[0], y: p[1] }, Modifiers::NONE);
}

fn state(app: &PhotocraftApp) -> EllipseParams {
    app.session
        .active()
        .unwrap()
        .ruler
        .as_ref()
        .and_then(|r| r.ellipse())
        .cloned()
        .unwrap_or(EllipseParams { center: [0.0, 0.0], radii: [1.0, 1.0], rotation: 0.0, arc: None })
}

#[test]
fn center_drag_moves_and_commits_once() {
    let mut app = app();
    ruler(&mut app);
    let sets_before = app.session.journal.iter().filter(|(id, _)| id == "ruler.ellipse.set").count();
    down(&mut app, [50.0, 40.0]);
    assert!(app.ui.ruler_drag.is_some(), "center handle grabs");
    move_to(&mut app, [60.0, 45.0], Modifiers::NONE);
    up(&mut app, [60.0, 45.0]);
    assert_eq!(app.ui.ruler_drag, None);
    let r = state(&app);
    assert_eq!((r.center, r.radii), ([60.0, 45.0], [20.0, 10.0]));
    let sets_after = app.session.journal.iter().filter(|(id, _)| id == "ruler.ellipse.set").count();
    assert_eq!(sets_after, sets_before + 1, "one journal entry per gesture");
}

#[test]
fn ring_handles_resize_and_rotation_turns_with_shift_snap() {
    let mut app = app();
    ruler(&mut app);
    // +rx handle at (70, 40): drag out to rx 30.
    down(&mut app, [70.0, 40.0]);
    move_to(&mut app, [80.0, 40.0], Modifiers::NONE);
    up(&mut app, [80.0, 40.0]);
    assert_eq!(state(&app).radii, [30.0, 10.0]);
    // Rotation handle at (50 + 30 + 12, 40) = (92, 40): drag straight up.
    // Screen -90° negates into +90° guide rotation.
    down(&mut app, [92.0, 40.0]);
    move_to(&mut app, [50.0, 8.0], Modifiers::NONE);
    up(&mut app, [50.0, 8.0]);
    assert!((state(&app).rotation - 90.0).abs() < 1e-6, "{:?}", state(&app));
    // Shift snaps to 15°.
    let handle = rotation_handle_pos(&app);
    down(&mut app, handle);
    move_to(&mut app, [60.0, 10.0], Modifiers::SHIFT);
    up(&mut app, [60.0, 10.0]);
    assert!((state(&app).rotation / 15.0).fract() < 1e-6, "{:?}", state(&app));
}

fn rotation_handle_pos(app: &PhotocraftApp) -> [f64; 2] {
    let r = state(app);
    let (ux, _) = axes(&r);
    [r.center[0] + (r.radii[0] + 12.0) * ux[0], r.center[1] + (r.radii[0] + 12.0) * ux[1]]
}

#[test]
fn empty_press_paints_and_arc_survives_a_drag() {
    let mut app = app();
    app.run("ruler.ellipse.set", json!({"center": [50, 40], "radii": [20, 10], "arc": [0, 90]})).unwrap();
    down(&mut app, [5.0, 5.0]);
    assert_eq!(app.ui.ruler_drag, None, "empty press is not a handle grab");
    // A center drag keeps the arc.
    down(&mut app, [50.0, 40.0]);
    move_to(&mut app, [55.0, 40.0], Modifiers::NONE);
    up(&mut app, [55.0, 40.0]);
    let r = state(&app);
    assert_eq!((r.center, r.arc), ([55.0, 40.0], Some((0.0, 90.0))));
}

#[test]
fn overlay_draws_without_panicking() {
    let mut app = app();
    ruler(&mut app);
    let xf = ViewXform::active(&app).expect("view");
    let ctx = egui::Context::default();
    let painter = ctx.layer_painter(egui::LayerId::background());
    // Ring, arc span, and a mid-drag preview all draw (exercises the ring map
    // end to end; pixel shape is pinned in `stroke::ruler` tests).
    draw(&app, &painter, &xf);
    app.run("ruler.ellipse.set", json!({"center": [50, 40], "radii": [20, 10], "arc": [10, 200]})).unwrap();
    draw(&app, &painter, &xf);
    down(&mut app, [50.0, 40.0]);
    draw(&app, &painter, &xf);
}

#[test]
fn parallel_origin_and_angle_drag() {
    let mut app = app();
    app.run("ruler.parallel.set", json!({"angle": 0, "spacing": 10})).unwrap();
    // Origin handle at the doc middle (50, 40): move it.
    down(&mut app, [50.0, 40.0]);
    assert!(app.ui.ruler_drag.is_some());
    move_to(&mut app, [60.0, 40.0], Modifiers::NONE);
    up(&mut app, [60.0, 40.0]);
    // Angle handle 48px along 0°: (108, 40) is off-canvas; use a closer VP-free
    // check instead: rotate via the handle by recomputing from state.
    let kind = app.session.active().unwrap().ruler.clone().expect("ruler").kind;
    assert!(matches!(kind, RulerKind::Parallel(_)), "expected a parallel ruler");
    let RulerKind::Parallel(p) = kind else {
        return;
    };
    assert_eq!((p.origin, p.angle), ([60.0, 40.0], 0.0));
    let origin = p.origin;
    // Angle handle at origin + 48x: drag straight up for 90°.
    down(&mut app, [origin[0] + 48.0, origin[1]]);
    move_to(&mut app, [origin[0], origin[1] - 48.0], Modifiers::NONE);
    up(&mut app, [origin[0], origin[1] - 48.0]);
    let kind = app.session.active().unwrap().ruler.clone().expect("ruler").kind;
    assert!(matches!(kind, RulerKind::Parallel(_)), "expected a parallel ruler");
    let RulerKind::Parallel(p) = kind else {
        return;
    };
    assert!((p.angle - 90.0).abs() < 1e-6, "{p:?}");
}

#[test]
fn vanishing_vp_and_connected_vertex_drag() {
    let mut app = app();
    app.run("ruler.vanishing.set", json!({"vps": [[10, 10], [90, 10]], "tick": 90})).unwrap();
    down(&mut app, [10.0, 10.0]);
    assert!(app.ui.ruler_drag.is_some(), "VP grabs");
    move_to(&mut app, [20.0, 20.0], Modifiers::NONE);
    up(&mut app, [20.0, 20.0]);
    let kind = app.session.active().unwrap().ruler.clone().expect("ruler").kind;
    assert!(matches!(kind, RulerKind::Vanishing(_)), "expected a vanishing ruler");
    let RulerKind::Vanishing(v) = kind else {
        return;
    };
    assert_eq!(v.vps[0], [20.0, 20.0]);
    app.run("ruler.connected.set", json!({"points": [[0, 0], [10, 0], [10, 10]]})).unwrap();
    down(&mut app, [10.0, 10.0]);
    move_to(&mut app, [20.0, 10.0], Modifiers::NONE);
    up(&mut app, [20.0, 10.0]);
    let kind = app.session.active().unwrap().ruler.clone().expect("ruler").kind;
    assert!(matches!(kind, RulerKind::Connected(_)), "expected a connected ruler");
    let RulerKind::Connected(c) = kind else {
        return;
    };
    assert_eq!(c.points[2], [20.0, 10.0]);
}

#[test]
fn bezier_control_and_spiral_center_drag() {
    let mut app = app();
    app.run("ruler.bezier.set", json!({"points": [[10, 10], [30, 10], [30, 30], [10, 30]]})).unwrap();
    // Drag the second control point.
    down(&mut app, [30.0, 10.0]);
    assert!(app.ui.ruler_drag.is_some(), "control grabs");
    move_to(&mut app, [40.0, 10.0], Modifiers::NONE);
    up(&mut app, [40.0, 10.0]);
    let kind = app.session.active().unwrap().ruler.clone().expect("ruler").kind;
    assert!(matches!(kind, RulerKind::Bezier(_)), "still bezier");
    let RulerKind::Bezier(b) = kind else {
        return;
    };
    assert_eq!(b.points[1], [40.0, 10.0]);
    // Spiral: center drag keeps the growth.
    app.run("ruler.spiral.set", json!({"center": [50, 40]})).unwrap();
    down(&mut app, [50.0, 40.0]);
    move_to(&mut app, [55.0, 42.0], Modifiers::NONE);
    up(&mut app, [55.0, 42.0]);
    let kind = app.session.active().unwrap().ruler.clone().expect("ruler").kind;
    let RulerKind::Spiral(s) = kind else {
        return;
    };
    assert_eq!((s.center, s.turns), ([55.0, 42.0], 3.0));
}
