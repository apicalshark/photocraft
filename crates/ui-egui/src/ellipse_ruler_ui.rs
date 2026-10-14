//! Ruler overlay: guides, handles and drag gestures for every ruler kind.
//!
//! Handles share the guides' y-up convention (angles are counter-clockwise
//! degrees, like brush angles): what is drawn is what the snap hits, by
//! construction. A drag previews in the gesture and commits one journaled
//! `ruler.*.set` on release, so the journal stays clean. (The module keeps
//! its ellipse-era name; it serves all kinds.)

use crate::{
    PhotocraftApp,
    canvas::{ToolEvent, ViewXform},
    state::Tool,
    theme::Tokens,
};
use egui::Stroke;
use photocraft_engine::ruler_cmds::{ActiveRuler, EllipseParams, RulerKind};
use serde::Serialize;
use serde_json::{Value, json};

const SEGMENTS: usize = 128;

/// A ruler drag: the committed ruler, its preview and the active handle.
#[derive(Clone, Debug, Serialize)]
pub struct Drag {
    pub original: ActiveRuler,
    pub preview: ActiveRuler,
    document: usize,
    tool: Tool,
    #[serde(skip)]
    gesture: Option<Gesture>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Gesture {
    OriginMove { start: [f64; 2], center: [f64; 2] },
    EllipseRadX { sign: f64 },
    EllipseRadY { sign: f64 },
    AngleDrag { pointer: f64, value: f64, center: [f64; 2], target: AngleTarget },
    SpacingDrag,
    PointDrag { index: usize, target: PointTarget },
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum PointTarget {
    Vp,
    Vertex,
    Cubic,
    Quad,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum AngleTarget {
    EllipseRotation,
    ParallelAngle,
    IsoAxis(usize),
}

/// A grabbable handle and its document position.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Handle {
    Origin,
    EllipseRadX { sign: f64 },
    EllipseRadY { sign: f64 },
    EllipseRot,
    Angle,
    Spacing,
    Vp(usize),
    Vertex(usize),
    Cubic(usize),
    Quad(usize),
    Axis(usize),
}

impl PartialEq for Drag {
    fn eq(&self, other: &Self) -> bool {
        self.original == other.original
            && self.preview == other.preview
            && self.document == other.document
            && self.tool == other.tool
            && self.gesture == other.gesture
    }
}

fn ruler(app: &PhotocraftApp) -> Option<ActiveRuler> {
    app.session.active()?.ruler.clone()
}

fn brushlike(app: &PhotocraftApp) -> bool {
    matches!(app.ui.tool, Tool::Brush | Tool::Pencil | Tool::Eraser)
}

fn dot(a: [f64; 2], b: [f64; 2]) -> f64 {
    a[0] * b[0] + a[1] * b[1]
}

fn dist(a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

/// Direction unit of a counter-clockwise degree angle (y-up convention).
fn dir_of(angle: f64) -> [f64; 2] {
    let (sin, cos) = angle.to_radians().sin_cos();
    [cos, sin]
}

/// Frame axes of the ellipse guide in document pixels.
fn axes(r: &EllipseParams) -> ([f64; 2], [f64; 2]) {
    let (sin, cos) = r.rotation.to_radians().sin_cos();
    ([cos, sin], [-sin, cos])
}

/// Ring point at parametric angle `t` (document pixels, guide convention).
fn ring_point(r: &EllipseParams, t: f64) -> [f64; 2] {
    let (ux, uy) = axes(r);
    let (s, c) = t.sin_cos();
    let (lx, ly) = (r.radii[0] * c, r.radii[1] * s);
    [r.center[0] + lx * ux[0] + ly * uy[0], r.center[1] + lx * ux[1] + ly * uy[1]]
}

/// Rotation handle: the angle-0 ring point pushed outward (document pixels).
fn rotation_handle(r: &EllipseParams, pad: f64) -> [f64; 2] {
    let (ux, _) = axes(r);
    let k = r.radii[0] + pad;
    [r.center[0] + k * ux[0], r.center[1] + k * ux[1]]
}

fn tol(xf: &ViewXform) -> f64 {
    8.0 / f64::from(xf.zoom.max(0.001))
}

/// Handles of the active ruler: (role, document position).
fn handles(active: &ActiveRuler, zoom: f32) -> Vec<(Handle, [f64; 2])> {
    let pad = 48.0 / f64::from(zoom.max(0.001));
    match &active.kind {
        RulerKind::Ellipse(r) => {
            let rp = 12.0 / f64::from(zoom.max(0.001));
            vec![
                (Handle::Origin, r.center),
                (Handle::EllipseRadX { sign: 1.0 }, ring_point(r, 0.0)),
                (Handle::EllipseRadX { sign: -1.0 }, ring_point(r, std::f64::consts::PI)),
                (Handle::EllipseRadY { sign: 1.0 }, ring_point(r, std::f64::consts::FRAC_PI_2)),
                (Handle::EllipseRadY { sign: -1.0 }, ring_point(r, -std::f64::consts::FRAC_PI_2)),
                (Handle::EllipseRot, rotation_handle(r, rp)),
            ]
        }
        RulerKind::Parallel(p) => {
            let d = dir_of(p.angle);
            let n = [-d[1], d[0]];
            vec![
                (Handle::Origin, p.origin),
                (Handle::Angle, [p.origin[0] + pad * d[0], p.origin[1] + pad * d[1]]),
                (Handle::Spacing, [p.origin[0] + p.spacing * n[0], p.origin[1] + p.spacing * n[1]]),
            ]
        }
        RulerKind::Radial(r) => vec![(Handle::Origin, r.center)],
        RulerKind::Vanishing(v) => v.vps.iter().enumerate().map(|(i, vp)| (Handle::Vp(i), *vp)).collect(),
        RulerKind::Fisheye(f) => vec![(Handle::Origin, f.center)],
        RulerKind::Iso(g) => {
            let mut out = vec![(Handle::Origin, g.origin)];
            for (i, a) in g.angles.iter().enumerate() {
                let d = dir_of(*a);
                out.push((Handle::Axis(i), [g.origin[0] + pad * d[0], g.origin[1] + pad * d[1]]));
            }
            out.push((Handle::Spacing, {
                let d = dir_of(g.angles[0]);
                let n = [-d[1], d[0]];
                [g.origin[0] + g.spacing * n[0], g.origin[1] + g.spacing * n[1]]
            }));
            out
        }
        RulerKind::Connected(c) => c.points.iter().enumerate().map(|(i, p)| (Handle::Vertex(i), *p)).collect(),
        RulerKind::Bezier(b) => b.points.iter().enumerate().map(|(i, p)| (Handle::Cubic(i), *p)).collect(),
        RulerKind::QuadBezier(q) => q.points.iter().enumerate().map(|(i, p)| (Handle::Quad(i), *p)).collect(),
        RulerKind::Spiral(s) => vec![(Handle::Origin, s.center)],
    }
}

/// Center-like anchor of a kind (moved by OriginMove).
fn origin_of(kind: &RulerKind) -> Option<[f64; 2]> {
    match kind {
        RulerKind::Ellipse(r) => Some(r.center),
        RulerKind::Parallel(p) => Some(p.origin),
        RulerKind::Radial(r) => Some(r.center),
        RulerKind::Vanishing(_) | RulerKind::Connected(_) => None,
        RulerKind::Fisheye(f) => Some(f.center),
        RulerKind::Iso(g) => Some(g.origin),
        RulerKind::Bezier(_) | RulerKind::QuadBezier(_) => None,
        RulerKind::Spiral(s) => Some(s.center),
    }
}

fn move_origin(kind: &mut RulerKind, to: [f64; 2]) {
    match kind {
        RulerKind::Ellipse(r) => r.center = to,
        RulerKind::Parallel(p) => p.origin = to,
        RulerKind::Radial(r) => r.center = to,
        RulerKind::Vanishing(_) | RulerKind::Connected(_) => {}
        RulerKind::Fisheye(f) => f.center = to,
        RulerKind::Iso(g) => g.origin = to,
        RulerKind::Bezier(_) | RulerKind::QuadBezier(_) => {},
        RulerKind::Spiral(s) => s.center = to,
    }
}

/// Screen-clockwise pointer delta negated into the counter-clockwise guide
/// convention, with optional Shift 15° snapping.
fn drag_angle(pointer_at_grab: f64, value_at_grab: f64, center: [f64; 2], p: [f64; 2], shift: bool) -> f64 {
    let mut v = value_at_grab + (pointer_at_grab - (p[1] - center[1]).atan2(p[0] - center[0])).to_degrees();
    if shift {
        v = (v / 15.0).round() * 15.0;
    }
    v.rem_euclid(360.0)
}

fn valid(app: &PhotocraftApp, d: &Drag) -> bool {
    app.session.active_index() == Some(d.document)
        && app.ui.tool == d.tool
        && brushlike(app)
        && ruler(app).as_ref() == Some(&d.original)
}

/// The set command replaying the preview (id + params).
fn commit_command(preview: &ActiveRuler) -> (&'static str, Value) {
    match &preview.kind {
        RulerKind::Ellipse(e) => (
            "ruler.ellipse.set",
            json!({"center": e.center, "radii": e.radii, "rotation": e.rotation, "arc": e.arc.map(|(a, b)| [a, b]), "accuracy": preview.accuracy}),
        ),
        RulerKind::Parallel(p) => (
            "ruler.parallel.set",
            json!({"center": p.origin, "angle": p.angle, "spacing": p.spacing, "accuracy": preview.accuracy}),
        ),
        RulerKind::Radial(r) => (
            "ruler.radial.set",
            json!({"center": r.center, "tick": r.tick, "curve": r.curve, "accuracy": preview.accuracy}),
        ),
        RulerKind::Vanishing(v) => (
            "ruler.vanishing.set",
            json!({"vps": v.vps, "tick": v.tick, "accuracy": preview.accuracy}),
        ),
        RulerKind::Fisheye(f) => (
            "ruler.fisheye.set",
            json!({"center": f.center, "mode": match f.mode {
                photocraft_engine::ruler_cmds::FisheyeKind::Horizontal => "horizontal",
                photocraft_engine::ruler_cmds::FisheyeKind::Vertical => "vertical",
                photocraft_engine::ruler_cmds::FisheyeKind::Radial => "radial",
            }, "accuracy": preview.accuracy}),
        ),
        RulerKind::Iso(g) => (
            "ruler.iso.set",
            json!({"center": g.origin, "angles": g.angles, "spacing": g.spacing, "accuracy": preview.accuracy}),
        ),
        RulerKind::Connected(c) => (
            "ruler.connected.set",
            json!({"points": c.points, "accuracy": preview.accuracy}),
        ),
        RulerKind::Bezier(b) => (
            "ruler.bezier.set",
            json!({"points": b.points, "accuracy": preview.accuracy}),
        ),
        RulerKind::QuadBezier(q) => (
            "ruler.quadBezier.set",
            json!({"points": q.points, "accuracy": preview.accuracy}),
        ),
        RulerKind::Spiral(s) => (
            "ruler.spiral.set",
            json!({"center": s.center, "a": s.a, "b": s.b, "turns": s.turns, "accuracy": preview.accuracy}),
        ),
    }
}

/// Commit the preview as one journaled command.
fn commit(app: &mut PhotocraftApp, d: &Drag) {
    let (id, params) = commit_command(&d.preview);
    let _ = app.run(id, params);
}

pub fn track(app: &mut PhotocraftApp, _ctx: &egui::Context) {
    let drop = app.ui.ruler_drag.as_ref().is_some_and(|d| !valid(app, d));
    if drop {
        app.ui.ruler_drag = None;
    }
}

pub fn pointer(app: &mut PhotocraftApp, ev: ToolEvent, mods: egui::Modifiers) -> bool {
    if app.ui.ruler_drag.as_ref().is_some_and(|d| !valid(app, d)) {
        app.ui.ruler_drag = None;
    }
    let Some(xf) = ViewXform::active(app) else {
        return app.ui.ruler_drag.is_some();
    };
    match ev {
        ToolEvent::Down { x, y, .. } => {
            if !brushlike(app) {
                return false;
            }
            let Some(active) = ruler(app) else {
                return false;
            };
            let p = [x, y];
            let t = tol(&xf);
            let mut best: Option<(Handle, f64)> = None;
            for (h, q) in handles(&active, xf.zoom) {
                let d = dist(p, q);
                if d <= t && best.is_none_or(|(_, bd)| d < bd) {
                    best = Some((h, d));
                }
            }
            let Some((handle, _)) = best else {
                return false;
            };
            let gesture = match handle {
                Handle::Origin => Gesture::OriginMove { start: p, center: origin_of(&active.kind).unwrap_or(p) },
                Handle::EllipseRadX { sign } => Gesture::EllipseRadX { sign },
                Handle::EllipseRadY { sign } => Gesture::EllipseRadY { sign },
                Handle::EllipseRot => {
                    let c = origin_of(&active.kind).unwrap_or(p);
                    let RulerKind::Ellipse(e) = &active.kind else {
                        return false;
                    };
                    Gesture::AngleDrag { pointer: (y - c[1]).atan2(x - c[0]), value: e.rotation, center: c, target: AngleTarget::EllipseRotation }
                }
                Handle::Angle => {
                    let (c, v) = match &active.kind {
                        RulerKind::Parallel(p) => (p.origin, p.angle),
                        _ => return false,
                    };
                    Gesture::AngleDrag { pointer: (y - c[1]).atan2(x - c[0]), value: v, center: c, target: AngleTarget::ParallelAngle }
                }
                Handle::Spacing => Gesture::SpacingDrag,
                Handle::Vp(i) => Gesture::PointDrag { index: i, target: PointTarget::Vp },
                Handle::Vertex(i) => Gesture::PointDrag { index: i, target: PointTarget::Vertex },
                Handle::Cubic(i) => Gesture::PointDrag { index: i, target: PointTarget::Cubic },
                Handle::Quad(i) => Gesture::PointDrag { index: i, target: PointTarget::Quad },
                Handle::Axis(i) => {
                    let RulerKind::Iso(g) = &active.kind else {
                        return false;
                    };
                    let c = g.origin;
                    Gesture::AngleDrag { pointer: (y - c[1]).atan2(x - c[0]), value: g.angles[i], center: c, target: AngleTarget::IsoAxis(i) }
                }
            };
            app.ui.ruler_drag = Some(Drag {
                original: active.clone(),
                preview: active,
                document: app.session.active_index().unwrap_or(usize::MAX),
                tool: app.ui.tool,
                gesture: Some(gesture),
            });
            true
        }
        ToolEvent::Move { x, y, .. } => {
            let Some(d) = &mut app.ui.ruler_drag else {
                return false;
            };
            let g = d.gesture;
            match g {
                Some(Gesture::OriginMove { start, center }) => {
                    move_origin(&mut d.preview.kind, [center[0] + x - start[0], center[1] + y - start[1]]);
                }
                Some(Gesture::EllipseRadX { sign }) | Some(Gesture::EllipseRadY { sign }) => {
                    let RulerKind::Ellipse(p) = &mut d.preview.kind else {
                        return true;
                    };
                    let (ux, uy) = axes(p);
                    let axis = if matches!(g, Some(Gesture::EllipseRadX { .. })) { ux } else { uy };
                    let v = dot([x - p.center[0], y - p.center[1]], axis) * sign;
                    if matches!(g, Some(Gesture::EllipseRadX { .. })) {
                        p.radii[0] = v.max(1.0);
                    } else {
                        p.radii[1] = v.max(1.0);
                    }
                }
                Some(Gesture::AngleDrag { pointer, value, center, target }) => {
                    let v = drag_angle(pointer, value, center, [x, y], mods.shift);
                    match (&mut d.preview.kind, target) {
                        (RulerKind::Ellipse(p), AngleTarget::EllipseRotation) => p.rotation = v,
                        (RulerKind::Parallel(p), AngleTarget::ParallelAngle) => p.angle = v,
                        (RulerKind::Iso(g), AngleTarget::IsoAxis(i)) => {
                            if let Some(a) = g.angles.get_mut(i) {
                                *a = v;
                            }
                        }
                        _ => {}
                    }
                }
                Some(Gesture::SpacingDrag) => {
                    let c = origin_of(&d.preview.kind).unwrap_or([x, y]);
                    let v = match &mut d.preview.kind {
                        RulerKind::Parallel(p) => {
                            let n = [-dir_of(p.angle)[1], dir_of(p.angle)[0]];
                            Some((dot([x - c[0], y - c[1]], n), true))
                        }
                        RulerKind::Iso(g) => {
                            let n = [-dir_of(g.angles[0])[1], dir_of(g.angles[0])[0]];
                            Some((dot([x - c[0], y - c[1]], n), false))
                        }
                        _ => None,
                    };
                    if let Some((v, parallel)) = v {
                        let next = v.abs().max(1.0);
                        match &mut d.preview.kind {
                            RulerKind::Parallel(p) if parallel => p.spacing = next,
                            RulerKind::Iso(g) if !parallel => g.spacing = next,
                            _ => {}
                        }
                    }
                }
                Some(Gesture::PointDrag { index, target }) => {
                    let q = match (&mut d.preview.kind, target) {
                        (RulerKind::Vanishing(v), PointTarget::Vp) => v.vps.get_mut(index),
                        (RulerKind::Connected(c), PointTarget::Vertex) => c.points.get_mut(index),
                        (RulerKind::Bezier(b), PointTarget::Cubic) => b.points.get_mut(index),
                        (RulerKind::QuadBezier(q), PointTarget::Quad) => q.points.get_mut(index),
                        _ => None,
                    };
                    if let Some(q) = q {
                        *q = [x, y];
                    }
                }
                None => return true,
            }
            true
        }
        ToolEvent::Up { .. } => {
            if let Some(d) = app.ui.ruler_drag.take() {
                commit(app, &d);
                return true;
            }
            false
        }
    }
}

/// Guide polylines to draw: (segments, anchors, handle rays).
/// Cubic Bernstein evaluation (draw-only; snapping lives in the guide).
fn eval_cubic(p: &[[f64; 2]; 4], t: f64) -> [f64; 2] {
    let u = 1.0 - t;
    let (b0, b1, b2, b3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    [
        b0 * p[0][0] + b1 * p[1][0] + b2 * p[2][0] + b3 * p[3][0],
        b0 * p[0][1] + b1 * p[1][1] + b2 * p[2][1] + b3 * p[3][1],
    ]
}

/// Degree-elevate a quadratic to cubic (same curve).
fn elevate_quad(q: &[[f64; 2]; 3]) -> [[f64; 2]; 4] {
    let (q0, q1, q2) = (q[0], q[1], q[2]);
    [q0, [(q0[0] + 2.0 * q1[0]) / 3.0, (q0[1] + 2.0 * q1[1]) / 3.0], [(2.0 * q1[0] + q2[0]) / 3.0, (2.0 * q1[1] + q2[1]) / 3.0], q2]
}

/// Log-spiral evaluation (draw-only).
fn eval_spiral(s: &photocraft_engine::ruler_cmds::SpiralParams, t: f64) -> [f64; 2] {
    let r = s.a * (s.b * t).exp();
    [s.center[0] + r * t.cos(), s.center[1] + r * t.sin()]
}

/// Guide polylines, anchors and handle rays (document pixels).
type Artwork = (Vec<([f64; 2], [f64; 2])>, Vec<[f64; 2]>, Vec<([f64; 2], [f64; 2])>);

fn artwork(active: &ActiveRuler, doc: [f64; 2], zoom: f32) -> Artwork {
    let pad = 12.0 / f64::from(zoom.max(0.001));
    let diag = doc[0].hypot(doc[1]).max(64.0);
    let mut segs = Vec::new();
    let mut anchors = Vec::new();
    let mut rays = Vec::new();
    match &active.kind {
        RulerKind::Ellipse(e) => {
            let two_pi = std::f64::consts::TAU;
            let (start, span) = match e.arc {
                Some((a, b)) => {
                    let a = a.to_radians();
                    let mut span = (b.to_radians() - a).rem_euclid(two_pi);
                    if span <= 1e-9 {
                        span = two_pi;
                    }
                    (a, span)
                }
                None => (0.0, two_pi),
            };
            let mut prev = ring_point(e, start);
            for i in 1..=SEGMENTS {
                let next = ring_point(e, start + span * i as f64 / SEGMENTS as f64);
                segs.push((prev, next));
                prev = next;
            }
            anchors.push(e.center);
            for u in [0.0, std::f64::consts::PI, std::f64::consts::FRAC_PI_2, -std::f64::consts::FRAC_PI_2] {
                anchors.push(ring_point(e, u));
            }
            rays.push((e.center, rotation_handle(e, pad)));
        }
        RulerKind::Parallel(p) => {
            let d = dir_of(p.angle);
            let n = [-d[1], d[0]];
            let m = ((diag / p.spacing) as usize).min(20);
            for k in -(m as i32)..=m as i32 {
                let o = [p.origin[0] + k as f64 * p.spacing * n[0], p.origin[1] + k as f64 * p.spacing * n[1]];
                segs.push(([o[0] - diag * d[0], o[1] - diag * d[1]], [o[0] + diag * d[0], o[1] + diag * d[1]]));
            }
            anchors.push(p.origin);
            rays.push((p.origin, [p.origin[0] + 48.0 * d[0], p.origin[1] + 48.0 * d[1]]));
            rays.push((p.origin, [p.origin[0] + p.spacing * n[0], p.origin[1] + p.spacing * n[1]]));
        }
        RulerKind::Radial(r) => {
            let n = ((360.0 / r.tick).round() as usize).clamp(1, 72);
            for i in 0..n {
                let d = dir_of(i as f64 * r.tick);
                segs.push((r.center, [r.center[0] + diag * d[0], r.center[1] + diag * d[1]]));
            }
            anchors.push(r.center);
        }
        RulerKind::Vanishing(v) => {
            for vp in &v.vps {
                anchors.push(*vp);
                let n = ((360.0 / v.tick).round() as usize).clamp(1, 24);
                for i in 0..n {
                    let d = dir_of(i as f64 * v.tick);
                    segs.push((*vp, [vp[0] + diag * d[0], vp[1] + diag * d[1]]));
                }
            }
        }
        RulerKind::Fisheye(f) => {
            use photocraft_engine::ruler_cmds::FisheyeKind;
            match f.mode {
                FisheyeKind::Horizontal => segs.push(([f.center[0] - diag, f.center[1]], [f.center[0] + diag, f.center[1]])),
                FisheyeKind::Vertical => segs.push(([f.center[0], f.center[1] - diag], [f.center[0], f.center[1] + diag])),
                FisheyeKind::Radial => {
                    for i in 0..24 {
                        let d = dir_of(i as f64 * 15.0);
                        segs.push((f.center, [f.center[0] + diag * d[0], f.center[1] + diag * d[1]]));
                    }
                }
            }
            anchors.push(f.center);
        }
        RulerKind::Iso(g) => {
            for a in g.angles {
                let d = dir_of(a);
                let n = [-d[1], d[0]];
                let m = ((diag / g.spacing) as usize).min(6);
                for k in -(m as i32)..=m as i32 {
                    let o = [g.origin[0] + k as f64 * g.spacing * n[0], g.origin[1] + k as f64 * g.spacing * n[1]];
                    segs.push(([o[0] - diag * d[0], o[1] - diag * d[1]], [o[0] + diag * d[0], o[1] + diag * d[1]]));
                }
                rays.push((g.origin, [g.origin[0] + 48.0 * d[0], g.origin[1] + 48.0 * d[1]]));
            }
            anchors.push(g.origin);
        }
        RulerKind::Connected(c) => {
            for w in c.points.windows(2) {
                segs.push((w[0], w[1]));
            }
            anchors.extend(c.points.iter().copied());
        }
        RulerKind::Bezier(b) => {
            let mut prev = eval_cubic(&b.points, 0.0);
            for i in 1..=64 {
                let next = eval_cubic(&b.points, i as f64 / 64.0);
                segs.push((prev, next));
                prev = next;
            }
            anchors.extend(b.points.iter().copied());
        }
        RulerKind::QuadBezier(q) => {
            let elevated = elevate_quad(&q.points);
            let mut prev = eval_cubic(&elevated, 0.0);
            for i in 1..=64 {
                let next = eval_cubic(&elevated, i as f64 / 64.0);
                segs.push((prev, next));
                prev = next;
            }
            anchors.extend(q.points.iter().copied());
        }
        RulerKind::Spiral(s) => {
            let max_t = s.turns * std::f64::consts::TAU;
            let mut prev = eval_spiral(s, 0.0);
            for i in 1..=SEGMENTS {
                let next = eval_spiral(s, max_t * i as f64 / SEGMENTS as f64);
                segs.push((prev, next));
                prev = next;
            }
            anchors.push(s.center);
        }
    }
    (segs, anchors, rays)
}

pub fn draw(app: &PhotocraftApp, painter: &egui::Painter, xf: &ViewXform) {
    let dragging = app.ui.ruler_drag.as_ref();
    if !brushlike(app) && dragging.is_none() {
        return;
    }
    let Some(st) = app.session.active() else {
        return;
    };
    let preview = dragging.map(|d| d.preview.clone()).or_else(|| ruler(app));
    let Some(active) = preview else {
        return;
    };
    let painter = painter.with_clip_rect(painter.clip_rect().intersect(xf.rect));
    let screen = |p: [f64; 2]| xf.to_screen(p[0] as f32, p[1] as f32);
    let t = Tokens::get(painter.ctx());
    let size = [f64::from(st.doc.size.width), f64::from(st.doc.size.height)];
    let (segs, anchors, rays) = artwork(&active, size, xf.zoom);
    for (a, b) in segs {
        painter.line_segment([screen(a), screen(b)], Stroke::new(1.0, t.accent));
    }
    for (a, b) in rays {
        painter.line_segment([screen(a), screen(b)], Stroke::new(1.0, t.accent));
        painter.circle_filled(screen(b), 3.0, t.accent);
    }
    for (i, a) in anchors.iter().enumerate() {
        crate::vector_ui::draw_anchor(&painter, screen(*a), i == 0, t.accent);
    }
}

#[cfg(test)]
mod tests;
