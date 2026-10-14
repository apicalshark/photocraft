//! Per-document ellipse ruler: snap Brush/Pencil/Eraser input points onto an
//! ellipse before the smoother sees them (slice (b) of P2-1).
//!
//! Like symmetry, the ruler is tool state, not document pixels: it lives on
//! the document state, applies to both the live preview and the commit
//! through one helper, and journals like any command.

use photocraft_paint::StrokePoint;
use photocraft_stroke::ruler::{BezierGuide, BundleGuide, EllipseGuide, FisheyeGuide, FisheyeMode, IsoGuide, ParallelGuide, PolyGuide, QuadGuide, RadialGuide, SpiralGuide};
use serde_json::{Value, json};

use crate::{EngineError, Result, Session, commands::CommandSpec};

/// Largest center/radius coordinate, like symmetry's bound.
const MAX_COORDINATE: f64 = 1_000_000.0;

/// Ellipse ruler params (user units: degrees). The guide conversion (arc to
/// radians) happens at the snap boundary.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EllipseParams {
    pub center: [f64; 2],
    pub radii: [f64; 2],
    pub rotation: f64,
    pub arc: Option<(f64, f64)>,
}

/// Any ruler kind. Variants grow here (P2-2..P2-4); the snap seam and the
/// accuracy mix stay shared.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum RulerKind {
    Ellipse(EllipseParams),
    /// Ticked parallel lines (direction + spacing).
    Parallel(ParallelParams),
    /// Ticked spokes from one center, optional quadratic bend.
    Radial(RadialParams),
    /// One to six vanishing points sharing one angular tick (two = two-point
    /// perspective; the perspective grids and scene match stay out).
    Vanishing(VanishingParams),
    /// Fisheye's three line modes through the center.
    Fisheye(FisheyeParams),
    /// Isometric three-axis lattice.
    Iso(IsoParams),
    /// Connected segment chain (clamped = anti-overshoot).
    Connected(ConnectedParams),
    /// Cubic Bezier curve (closest-point snap).
    Bezier(BezierParams),
    /// Quadratic Bezier curve.
    QuadBezier(QuadParams),
    /// Golden log spiral (angle solve in theta).
    Spiral(SpiralParams),
}

/// Ticked parallel lines (user units: degrees).
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ParallelParams {
    pub origin: [f64; 2],
    pub angle: f64,
    pub spacing: f64,
}

/// Ticked spokes from one center (user units: degrees).
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RadialParams {
    pub center: [f64; 2],
    pub tick: f64,
    pub curve: f64,
}

/// Vanishing-point bundle (user units: degrees).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VanishingParams {
    pub vps: Vec<[f64; 2]>,
    pub tick: f64,
}

/// Fisheye line mode through the center.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FisheyeParams {
    pub center: [f64; 2],
    pub mode: FisheyeKind,
}

/// Fisheye's three line modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FisheyeKind {
    Horizontal,
    Vertical,
    Radial,
}

/// Isometric three-axis lattice (user units: degrees).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct IsoParams {
    pub origin: [f64; 2],
    pub angles: [f64; 3],
    pub spacing: f64,
}

/// Connected segment chain.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConnectedParams {
    pub points: Vec<[f64; 2]>,
}

/// Cubic Bezier control points.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BezierParams {
    pub points: [[f64; 2]; 4],
}

/// Quadratic Bezier control points.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct QuadParams {
    pub points: [[f64; 2]; 3],
}

/// Golden log spiral (user units: `b` per radian, `turns` count).
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SpiralParams {
    pub center: [f64; 2],
    pub a: f64,
    pub b: f64,
    pub turns: f64,
}

/// Active ruler: kind plus the universal accuracy mix (the reference's
/// `rulerAccuracy`: 1 = full projection, 0 = show the guide without snapping).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ActiveRuler {
    pub kind: RulerKind,
    pub accuracy: f32,
}

/// Apply a point snapper over a stroke, keeping pressure/tilt/time.
fn map_pts(pts: &[StrokePoint], snap: impl Fn(f64, f64) -> Option<[f64; 2]>) -> Vec<StrokePoint> {
    pts.iter()
        .map(|p| match snap(p.x, p.y) {
            Some([x, y]) => StrokePoint { x, y, ..*p },
            None => *p,
        })
        .collect()
}

impl ActiveRuler {
    /// The ellipse params, if this ruler is one.
    pub fn ellipse(&self) -> Option<&EllipseParams> {
        match &self.kind {
            RulerKind::Ellipse(e) => Some(e),
            _ => None,
        }
    }
    /// Blend raw input toward the snapped ring by accuracy (clamped 0..1).
    pub fn snap_points(&self, pts: &[StrokePoint]) -> Vec<StrokePoint> {
        let t = self.accuracy.clamp(0.0, 1.0);
        if t <= 0.0 {
            return pts.to_vec();
        }
        let snapped = match &self.kind {
            RulerKind::Ellipse(e) => e.snap_points(pts),
            RulerKind::Parallel(p) => match ParallelGuide::new(p.origin, p.angle, p.spacing) {
                Some(g) => map_pts(pts, |x, y| g.snap(x, y)),
                None => pts.to_vec(),
            },
            RulerKind::Radial(r) => match RadialGuide::new(r.center, r.tick, r.curve) {
                Some(g) => map_pts(pts, |x, y| g.snap(x, y)),
                None => pts.to_vec(),
            },
            RulerKind::Vanishing(v) => match BundleGuide::new(v.vps.clone(), v.tick) {
                Some(g) => map_pts(pts, |x, y| g.snap(x, y)),
                None => pts.to_vec(),
            },
            RulerKind::Fisheye(f) => {
                let mode = match f.mode {
                    FisheyeKind::Horizontal => FisheyeMode::Horizontal,
                    FisheyeKind::Vertical => FisheyeMode::Vertical,
                    FisheyeKind::Radial => FisheyeMode::Radial,
                };
                match FisheyeGuide::new(f.center, mode) {
                    Some(g) => map_pts(pts, |x, y| g.snap(x, y)),
                    None => pts.to_vec(),
                }
            }
            RulerKind::Iso(g) => match IsoGuide::new(g.origin, g.angles, g.spacing) {
                Some(g) => map_pts(pts, |x, y| g.snap(x, y)),
                None => pts.to_vec(),
            },
            RulerKind::Connected(c) => match PolyGuide::new(c.points.clone()) {
                Some(g) => map_pts(pts, |x, y| g.snap(x, y)),
                None => pts.to_vec(),
            },
            RulerKind::Bezier(b) => match BezierGuide::new(b.points) {
                Some(g) => map_pts(pts, |x, y| g.snap(x, y)),
                None => pts.to_vec(),
            },
            RulerKind::QuadBezier(q) => match QuadGuide::new(q.points) {
                Some(g) => map_pts(pts, |x, y| g.snap(x, y)),
                None => pts.to_vec(),
            },
            RulerKind::Spiral(sp) => match SpiralGuide::new(sp.center, sp.a, sp.b, sp.turns) {
                Some(g) => map_pts(pts, |x, y| g.snap(x, y)),
                None => pts.to_vec(),
            },
        };
        if t >= 1.0 {
            return snapped;
        }
        pts.iter()
            .zip(snapped.iter())
            .map(|(p, q)| StrokePoint {
                x: p.x + (q.x - p.x) * f64::from(t),
                y: p.y + (q.y - p.y) * f64::from(t),
                ..*p
            })
            .collect()
    }
}

impl EllipseParams {
    /// Snap input points onto the ring, keeping pressure/tilt/time. Points
    /// that miss the guide pass through (unreachable after `check_coords`,
    /// kept total so the commit path cannot stall).
    pub fn snap_points(&self, pts: &[StrokePoint]) -> Vec<StrokePoint> {
        let guide = EllipseGuide::new(
            self.center,
            self.radii,
            self.rotation,
            self.arc.map(|(a, b)| (a.to_radians(), b.to_radians())),
        );
        let Some(guide) = guide else { return pts.to_vec() };
        pts.iter()
            .map(|p| match guide.snap(p.x, p.y) {
                Some([x, y]) => StrokePoint { x, y, ..*p },
                None => *p,
            })
            .collect()
    }
}

/// Snap through the document's ruler, if any. One seam for the live preview
/// (`LiveStroke::push`) and the commit (`stroke_with`), so the live-commit
/// fast path keeps matching.
pub fn snap_points(s: &Session, pts: Vec<StrokePoint>) -> Vec<StrokePoint> {
    match s.active().and_then(|st| st.ruler.clone()) {
        Some(r) => r.snap_points(&pts),
        None => pts,
    }
}
fn pair(v: &Value, fail: &impl Fn() -> EngineError) -> Result<[f64; 2]> {
    let a = v.as_array().filter(|v| v.len() == 2).ok_or_else(fail)?;
    Ok([a.first().and_then(Value::as_f64).ok_or_else(fail)?, a.get(1).and_then(Value::as_f64).ok_or_else(fail)?])
}

fn set_ellipse(s: &mut Session, p: &Value) -> Result<Value> {
    let fail = || {
        EngineError::Other(
            "ruler.ellipse.set needs a finite center [x, y] (|v| <= 1e6), positive finite radii [rx, ry] (<= 1e6), a finite rotation, a finite arc and an accuracy in 0..1".into(),
        )
    };
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let center = match p.get("center") {
        None => [f64::from(st.doc.size.width) / 2.0, f64::from(st.doc.size.height) / 2.0],
        Some(v) => pair(v, &fail)?,
    };
    if !center.iter().all(|v| v.is_finite() && v.abs() <= MAX_COORDINATE) {
        return Err(fail());
    }
    let radii = pair(p.get("radii").ok_or_else(fail)?, &fail)?;
    if !radii.iter().all(|v| v.is_finite() && *v > 0.0 && *v <= MAX_COORDINATE) {
        return Err(fail());
    }
    let rotation = p.get("rotation").map_or(Ok(0.0), |v| v.as_f64().ok_or_else(fail))?;
    if !rotation.is_finite() {
        return Err(fail());
    }
    let arc = match p.get("arc").filter(|v| !v.is_null()) {
        None => None,
        Some(v) => {
            let [a, b] = pair(v, &fail)?;
            if !a.is_finite() || !b.is_finite() {
                return Err(fail());
            }
            Some((a, b))
        }
    };
    let accuracy = p.get("accuracy").map_or(Ok(1.0), |v| v.as_f64().ok_or_else(fail))?;
    if !(0.0..=1.0).contains(&accuracy) {
        return Err(fail());
    }
    let ruler = ActiveRuler {
        kind: RulerKind::Ellipse(EllipseParams { center, radii, rotation: rotation.rem_euclid(360.0), arc }),
        accuracy: accuracy as f32,
    };
    s.active_mut().ok_or(EngineError::NoDocument)?.ruler = Some(ruler.clone());
    let RulerKind::Ellipse(e) = &ruler.kind else {
        return Err(EngineError::Other("unreachable: just built an ellipse".into()));
    };
    Ok(json!({ "center": e.center, "radii": e.radii, "rotation": e.rotation, "arc": e.arc.map(|(a, b)| [a, b]), "accuracy": ruler.accuracy }))
}

fn disable(s: &mut Session, _p: &Value) -> Result<Value> {
    s.active_mut().ok_or(EngineError::NoDocument)?.ruler = None;
    Ok(json!({ "disabled": true }))
}

/// Optional center defaulting to the document middle, validated like `set_ellipse`.
fn opt_middle(p: &Value, st: &crate::DocState, fail: &impl Fn() -> EngineError) -> Result<[f64; 2]> {
    match p.get("center").filter(|v| !v.is_null()) {
        None => Ok([f64::from(st.doc.size.width) / 2.0, f64::from(st.doc.size.height) / 2.0]),
        Some(v) => {
            let c = pair(v, fail)?;
            if c.iter().all(|x| x.is_finite() && x.abs() <= MAX_COORDINATE) { Ok(c) } else { Err(fail()) }
        }
    }
}

fn opt_angle(p: &Value, key: &str, default: f64, fail: &impl Fn() -> EngineError) -> Result<f64> {
    match p.get(key).filter(|v| !v.is_null()) {
        None => Ok(default),
        Some(v) => {
            let a = v.as_f64().ok_or_else(fail)?;
            if a.is_finite() { Ok(a.rem_euclid(360.0)) } else { Err(fail()) }
        }
    }
}

fn opt_tick(p: &Value, default: f64, fail: &impl Fn() -> EngineError) -> Result<f64> {
    match p.get("tick").filter(|v| !v.is_null()) {
        None => Ok(default),
        Some(v) => {
            let t = v.as_f64().ok_or_else(fail)?;
            if t.is_finite() && t > 0.0 && t <= 360.0 { Ok(t) } else { Err(fail()) }
        }
    }
}

fn opt_spacing(p: &Value, default: f64, fail: &impl Fn() -> EngineError) -> Result<f64> {
    match p.get("spacing").filter(|v| !v.is_null()) {
        None => Ok(default),
        Some(v) => {
            let s = v.as_f64().ok_or_else(fail)?;
            if s.is_finite() && s > 0.0 && s <= MAX_COORDINATE { Ok(s) } else { Err(fail()) }
        }
    }
}

fn opt_accuracy(p: &Value, fail: &impl Fn() -> EngineError) -> Result<f32> {
    match p.get("accuracy").filter(|v| !v.is_null()) {
        None => Ok(1.0),
        Some(v) => {
            let a = v.as_f64().ok_or_else(fail)?;
            if (0.0..=1.0).contains(&a) { Ok(a as f32) } else { Err(fail()) }
        }
    }
}

fn set_kind(s: &mut Session, kind: RulerKind, accuracy: f32) -> Result<Value> {
    s.active_mut().ok_or(EngineError::NoDocument)?.ruler = Some(ActiveRuler { kind, accuracy });
    Ok(json!({ "accuracy": accuracy }))
}

fn set_parallel(s: &mut Session, p: &Value) -> Result<Value> {
    let fail = || EngineError::Other("ruler.parallel.set needs a finite angle, a finite center and a positive finite spacing".into());
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let (center, angle, spacing, accuracy) =
        (opt_middle(p, st, &fail)?, opt_angle(p, "angle", 0.0, &fail)?, opt_spacing(p, 20.0, &fail)?, opt_accuracy(p, &fail)?);
    set_kind(s, RulerKind::Parallel(ParallelParams { origin: center, angle, spacing }), accuracy)
}

fn set_radial(s: &mut Session, p: &Value) -> Result<Value> {
    let fail = || EngineError::Other("ruler.radial.set needs a finite center, a tick in 0..360 and a finite curve".into());
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let (center, tick, accuracy) = (opt_middle(p, st, &fail)?, opt_tick(p, 15.0, &fail)?, opt_accuracy(p, &fail)?);
    let curve = match p.get("curve").filter(|v| !v.is_null()) {
        None => 0.0,
        Some(v) => {
            let c = v.as_f64().ok_or_else(fail)?;
            if c.is_finite() { c } else { return Err(fail()); }
        }
    };
    set_kind(s, RulerKind::Radial(RadialParams { center, tick, curve }), accuracy)
}

fn vp_list(p: &Value, key: &str, min: usize, max: usize, fail: &impl Fn() -> EngineError) -> Result<Vec<[f64; 2]>> {
    let a = p.get(key).and_then(Value::as_array).ok_or_else(fail)?;
    if a.len() < min || a.len() > max {
        return Err(fail());
    }
    a.iter()
        .map(|v| {
            let q = pair(v, fail)?;
            if q.iter().all(|x| x.is_finite() && x.abs() <= MAX_COORDINATE) { Ok(q) } else { Err(fail()) }
        })
        .collect()
}

fn set_vanishing(s: &mut Session, p: &Value) -> Result<Value> {
    let fail = || EngineError::Other("ruler.vanishing.set needs 1..6 finite vanishing points and a tick in 0..360".into());
    s.active().ok_or(EngineError::NoDocument)?;
    let (vps, tick, accuracy) = (vp_list(p, "vps", 1, 6, &fail)?, opt_tick(p, 15.0, &fail)?, opt_accuracy(p, &fail)?);
    set_kind(s, RulerKind::Vanishing(VanishingParams { vps, tick }), accuracy)
}

fn set_perspective(s: &mut Session, p: &Value) -> Result<Value> {
    let fail = || EngineError::Other("ruler.perspective.set needs two finite vanishing points vp1/vp2 and a tick in 0..360".into());
    s.active().ok_or(EngineError::NoDocument)?;
    let vp = |key: &str| {
        let q = pair(p.get(key).ok_or_else(fail)?, &fail)?;
        if q.iter().all(|x| x.is_finite() && x.abs() <= MAX_COORDINATE) { Ok(q) } else { Err(fail()) }
    };
    let (vps, tick, accuracy) = (vec![vp("vp1")?, vp("vp2")?], opt_tick(p, 15.0, &fail)?, opt_accuracy(p, &fail)?);
    set_kind(s, RulerKind::Vanishing(VanishingParams { vps, tick }), accuracy)
}

fn set_fisheye(s: &mut Session, p: &Value) -> Result<Value> {
    let fail = || EngineError::Other("ruler.fisheye.set needs a finite center and a mode horizontal|vertical|radial".into());
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let (center, accuracy) = (opt_middle(p, st, &fail)?, opt_accuracy(p, &fail)?);
    let mode = match p.get("mode").filter(|v| !v.is_null()) {
        None => FisheyeKind::Horizontal,
        Some(v) => match v.as_str() {
            Some("horizontal") => FisheyeKind::Horizontal,
            Some("vertical") => FisheyeKind::Vertical,
            Some("radial") => FisheyeKind::Radial,
            _ => return Err(fail()),
        },
    };
    set_kind(s, RulerKind::Fisheye(FisheyeParams { center, mode }), accuracy)
}

fn set_iso(s: &mut Session, p: &Value) -> Result<Value> {
    let fail = || EngineError::Other("ruler.iso.set needs a finite origin, three finite axis angles and a positive finite spacing".into());
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let (origin, spacing, accuracy) = (opt_middle(p, st, &fail)?, opt_spacing(p, 20.0, &fail)?, opt_accuracy(p, &fail)?);
    let angles = match p.get("angles").filter(|v| !v.is_null()) {
        None => [30.0, -30.0, 90.0],
        Some(v) => {
            let a = v.as_array().filter(|v| v.len() == 3).ok_or_else(fail)?;
            let mut out = [0.0; 3];
            for (i, v) in a.iter().enumerate() {
                out[i] = v.as_f64().filter(|x| x.is_finite()).ok_or_else(fail)?;
            }
            out
        }
    };
    set_kind(s, RulerKind::Iso(IsoParams { origin, angles, spacing }), accuracy)
}

fn set_connected(s: &mut Session, p: &Value) -> Result<Value> {
    let fail = || EngineError::Other("ruler.connected.set needs 2..1024 finite points".into());
    s.active().ok_or(EngineError::NoDocument)?;
    let accuracy = opt_accuracy(p, &fail)?;
    set_kind(s, RulerKind::Connected(ConnectedParams { points: vp_list(p, "points", 2, 1024, &fail)? }), accuracy)
}

fn ctrl_points<const N: usize>(p: &Value, key: &str, fail: &impl Fn() -> EngineError) -> Result<[[f64; 2]; N]> {
    let a = p.get(key).and_then(Value::as_array).ok_or_else(fail)?;
    if a.len() != N {
        return Err(fail());
    }
    let mut out = Vec::with_capacity(N);
    for v in a {
        let q = pair(v, fail)?;
        if !q.iter().all(|x| x.is_finite() && x.abs() <= MAX_COORDINATE) {
            return Err(fail());
        }
        out.push(q);
    }
    out.try_into().map_err(|_| fail())
}

fn set_bezier(s: &mut Session, p: &Value) -> Result<Value> {
    let fail = || EngineError::Other("ruler.bezier.set needs four finite control points p0..p3".into());
    s.active().ok_or(EngineError::NoDocument)?;
    let accuracy = opt_accuracy(p, &fail)?;
    set_kind(s, RulerKind::Bezier(BezierParams { points: ctrl_points(p, "points", &fail)? }), accuracy)
}

fn set_quad_bezier(s: &mut Session, p: &Value) -> Result<Value> {
    let fail = || EngineError::Other("ruler.quadBezier.set needs three finite control points p0..p2".into());
    s.active().ok_or(EngineError::NoDocument)?;
    let accuracy = opt_accuracy(p, &fail)?;
    set_kind(s, RulerKind::QuadBezier(QuadParams { points: ctrl_points(p, "points", &fail)? }), accuracy)
}

fn set_spiral(s: &mut Session, p: &Value) -> Result<Value> {
    let fail = || EngineError::Other("ruler.spiral.set needs a finite center, a positive scale a, a finite growth b and turns in 0.5..8".into());
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let (center, accuracy) = (opt_middle(p, st, &fail)?, opt_accuracy(p, &fail)?);
    let num = |key: &str, default: f64| match p.get(key).filter(|v| !v.is_null()) {
        None => Ok(default),
        Some(v) => v.as_f64().filter(|x| x.is_finite()).ok_or_else(fail),
    };
    // Golden growth default: a quarter turn grows by phi.
    let (a, b, turns) = (num("a", 5.0)?, num("b", 0.306_349)?, num("turns", 3.0)?);
    if !(a.is_finite() && a > 0.0) || !(0.5..=8.0).contains(&turns) {
        return Err(fail());
    }
    set_kind(s, RulerKind::Spiral(SpiralParams { center, a, b, turns }), accuracy)
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "ruler.ellipse.set",
            label: "Set Ellipse Ruler",
            menu: &[],
            shortcut: None,
            params: r#"{"center":[x,y]?=document middle,"radii":[rx,ry],"rotation":degrees=0,"arc":[start,end]?=whole ring in degrees,"accuracy":0..1=1} — snap Brush, Pencil and Eraser strokes onto the ellipse"#,
            enabled: |s| s.active().map(|_| ()).ok_or_else(|| "no document open".into()),
            run: set_ellipse,
            journal: true,
        },
        CommandSpec {
            id: "ruler.parallel.set",
            label: "Set Parallel Ruler",
            menu: &[],
            shortcut: None,
            params: r#"{"angle":degrees=0,"center":[x,y]?=document middle,"spacing":px=20,"accuracy":0..1=1} — snap strokes onto ticked parallel lines"#,
            enabled: |s| s.active().map(|_| ()).ok_or_else(|| "no document open".into()),
            run: set_parallel,
            journal: true,
        },
        CommandSpec {
            id: "ruler.radial.set",
            label: "Set Radial Ruler",
            menu: &[],
            shortcut: None,
            params: r#"{"center":[x,y]?=document middle,"tick":degrees=15,"curve":number=0,"accuracy":0..1=1} — snap strokes onto ticked spokes from the center"#,
            enabled: |s| s.active().map(|_| ()).ok_or_else(|| "no document open".into()),
            run: set_radial,
            journal: true,
        },
        CommandSpec {
            id: "ruler.vanishing.set",
            label: "Set Vanishing Ruler",
            menu: &[],
            shortcut: None,
            params: r#"{"vps":[[x,y]×1..6],"tick":degrees=15,"accuracy":0..1=1} — snap strokes onto ticked lines through the nearest vanishing point"#,
            enabled: |s| s.active().map(|_| ()).ok_or_else(|| "no document open".into()),
            run: set_vanishing,
            journal: true,
        },
        CommandSpec {
            id: "ruler.perspective.set",
            label: "Set Perspective Ruler",
            menu: &[],
            shortcut: None,
            params: r#"{"vp1":[x,y],"vp2":[x,y],"tick":degrees=15,"accuracy":0..1=1} — two-point perspective bundle (scene match stays out)"#,
            enabled: |s| s.active().map(|_| ()).ok_or_else(|| "no document open".into()),
            run: set_perspective,
            journal: true,
        },
        CommandSpec {
            id: "ruler.fisheye.set",
            label: "Set Fisheye Ruler",
            menu: &[],
            shortcut: None,
            params: r#"{"center":[x,y]?=document middle,"mode":"horizontal|vertical|radial"="horizontal","accuracy":0..1=1} — snap strokes onto one fisheye line"#,
            enabled: |s| s.active().map(|_| ()).ok_or_else(|| "no document open".into()),
            run: set_fisheye,
            journal: true,
        },
        CommandSpec {
            id: "ruler.iso.set",
            label: "Set Isometric Ruler",
            menu: &[],
            shortcut: None,
            params: r#"{"origin":[x,y]?=document middle,"angles":[a,b,c]?=[30,-30,90],"spacing":px=20,"accuracy":0..1=1} — snap strokes onto the three-axis lattice"#,
            enabled: |s| s.active().map(|_| ()).ok_or_else(|| "no document open".into()),
            run: set_iso,
            journal: true,
        },
        CommandSpec {
            id: "ruler.connected.set",
            label: "Set Connected Ruler",
            menu: &[],
            shortcut: None,
            params: r#"{"points":[[x,y]×2..1024],"accuracy":0..1=1} — snap strokes onto the clamped segment chain"#,
            enabled: |s| s.active().map(|_| ()).ok_or_else(|| "no document open".into()),
            run: set_connected,
            journal: true,
        },
        CommandSpec {
            id: "ruler.bezier.set",
            label: "Set Bezier Ruler",
            menu: &[],
            shortcut: None,
            params: r#"{"points":[p0,p1,p2,p3],"accuracy":0..1=1} — snap strokes onto the cubic curve"#,
            enabled: |s| s.active().map(|_| ()).ok_or_else(|| "no document open".into()),
            run: set_bezier,
            journal: true,
        },
        CommandSpec {
            id: "ruler.quadBezier.set",
            label: "Set Quadratic Ruler",
            menu: &[],
            shortcut: None,
            params: r#"{"points":[p0,p1,p2],"accuracy":0..1=1} — snap strokes onto the quadratic curve"#,
            enabled: |s| s.active().map(|_| ()).ok_or_else(|| "no document open".into()),
            run: set_quad_bezier,
            journal: true,
        },
        CommandSpec {
            id: "ruler.spiral.set",
            label: "Set Spiral Ruler",
            menu: &[],
            shortcut: None,
            params: r#"{"center":[x,y]?=document middle,"a":scale=5,"b":growth=0.306349 (golden),"turns":0.5..8=3,"accuracy":0..1=1} — snap strokes onto the log spiral"#,
            enabled: |s| s.active().map(|_| ()).ok_or_else(|| "no document open".into()),
            run: set_spiral,
            journal: true,
        },
        CommandSpec {
            id: "ruler.disable",
            label: "Disable Ruler",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: |s| s.active().filter(|st| st.ruler.is_some()).map(|_| ()).ok_or_else(|| "no active ruler".into()),
            run: disable,
            journal: true,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(w: u32, h: u32) -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": w, "height": h})).unwrap();
        s
    }

    #[test]
    fn set_validates_and_disables() {
        let mut s = session(64, 64);
        // No document state needed beyond an open doc; hostile shapes error.
        assert!(s.execute("ruler.disable", json!({})).is_err(), "nothing active");
        let r = s
            .execute("ruler.ellipse.set", json!({"center": [32, 32], "radii": [10, 6], "rotation": 30}))
            .unwrap();
        assert_eq!((r["radii"][0].as_f64().unwrap(), r["rotation"].as_f64().unwrap()), (10.0, 30.0));
        assert!(r["arc"].is_null());
        // Center defaults to the document middle.
        s.execute("ruler.ellipse.set", json!({"radii": [5, 5]})).unwrap();
        assert_eq!(s.active().unwrap().ruler.as_ref().unwrap().ellipse().unwrap().center, [32.0, 32.0]);
        s.execute("ruler.disable", json!({})).unwrap();
        assert_eq!(s.active().unwrap().ruler, None);
        // Explicit null arc (what the overlay commit sends) means whole ring.
        s.execute("ruler.ellipse.set", json!({"center": [10, 10], "radii": [5, 5], "arc": null})).unwrap();
        assert_eq!(s.active().unwrap().ruler.as_ref().unwrap().ellipse().unwrap().arc, None);
        for p in [
            json!({}),
            json!({"radii": [10, 6], "center": [0]}),
            json!({"radii": [0, 6]}),
            json!({"radii": [-1, 6]}),
            json!({"radii": [1e7, 6]}),
            json!({"radii": ["x", 6]}),
            json!({"center": [f64::NAN, 0], "radii": [10, 6]}),
            json!({"radii": [10, 6], "rotation": "fast"}),
            json!({"radii": [10, 6], "arc": [0]}),
        ] {
            assert!(s.execute("ruler.ellipse.set", p).is_err());
        }
    }

    #[test]
    fn snap_keeps_non_position_fields() {
        let r = EllipseParams { center: [0.0, 0.0], radii: [4.0, 4.0], rotation: 0.0, arc: None };
        let mut p = StrokePoint::new(8.0, 0.0, 0.5);
        p.tilt_x = 3.0;
        p.time = 12.0;
        let got = r.snap_points(&[p]);
        assert_eq!(got.len(), 1);
        assert!((got[0].x - 4.0).abs() < 1e-9 && (got[0].pressure - 0.5).abs() < 1e-12);
        assert_eq!((got[0].tilt_x, got[0].time), (3.0, 12.0));
    }

    #[test]
    fn accuracy_mixes_raw_and_snapped() {
        let pts = vec![StrokePoint::new(8.0, 0.0, 1.0)];
        let full = ActiveRuler { kind: RulerKind::Ellipse(EllipseParams { center: [0.0, 0.0], radii: [4.0, 4.0], rotation: 0.0, arc: None }), accuracy: 1.0 };
        assert!((full.snap_points(&pts)[0].x - 4.0).abs() < 1e-9);
        let off = ActiveRuler { accuracy: 0.0, ..full.clone() };
        assert!((off.snap_points(&pts)[0].x - 8.0).abs() < 1e-12, "show-only");
        let half = ActiveRuler { accuracy: 0.5, ..full };
        assert!((half.snap_points(&pts)[0].x - 6.0).abs() < 1e-9);
    }

    fn kind_of(s: &Session) -> RulerKind {
        s.active().unwrap().ruler.clone().expect("ruler").kind
    }

    #[test]
    fn curve_commands_set_and_snap() {
        let mut s = session(100, 100);
        s.execute("ruler.bezier.set", json!({"points": [[0, 0], [3, 0], [7, 0], [10, 0]]})).unwrap();
        assert!(matches!(kind_of(&s), RulerKind::Bezier(_)));
        let got = snap_points(&s, vec![StrokePoint::new(5.0, 3.0, 1.0)]);
        assert!((got[0].x - 5.0).abs() < 1e-4 && got[0].y.abs() < 1e-4, "{got:?}");
        s.execute("ruler.quadBezier.set", json!({"points": [[0, 0], [5, 10], [10, 0]]})).unwrap();
        assert!(matches!(kind_of(&s), RulerKind::QuadBezier(_)));
        s.execute("ruler.spiral.set", json!({"center": [0, 0], "a": 5, "b": 0, "turns": 2})).unwrap();
        assert!(matches!(kind_of(&s), RulerKind::Spiral(_)));
        let got = snap_points(&s, vec![StrokePoint::new(10.0, 0.0, 1.0)]);
        assert!((got[0].x - 5.0).abs() < 1e-4 && got[0].y.abs() < 1e-4, "{got:?}");
    }

    #[test]
    fn line_commands_set_validate_and_snap() {
        let mut s = session(100, 100);
        // Parallel: horizontal ticks every 10 through the middle.
        s.execute("ruler.parallel.set", json!({"angle": 0, "spacing": 10})).unwrap();
        assert!(matches!(kind_of(&s), RulerKind::Parallel(_)));
        let pts = vec![StrokePoint::new(3.0, 14.0, 1.0)];
        let got = snap_points(&s, pts);
        assert!((got[0].x - 3.0).abs() < 1e-9 && (got[0].y - 10.0).abs() < 1e-9, "{got:?}");
        // Radial: 90° spokes from the middle.
        s.execute("ruler.radial.set", json!({"tick": 90})).unwrap();
        let pts = vec![StrokePoint::new(52.0, 59.0, 1.0)];
        let got = snap_points(&s, pts);
        assert!((got[0].x - 50.0).abs() < 1e-9 && (got[0].y - (50.0 + 85.0_f64.sqrt())).abs() < 1e-6, "{got:?}");
        // Vanishing + perspective share the bundle math.
        s.execute("ruler.vanishing.set", json!({"vps": [[0, 0], [100, 0]], "tick": 90})).unwrap();
        let pts = vec![StrokePoint::new(102.0, 30.0, 1.0)];
        let got = snap_points(&s, pts);
        assert!((got[0].x - 100.0).abs() < 1e-6, "{got:?}");
        s.execute("ruler.perspective.set", json!({"vp1": [0, 0], "vp2": [100, 0], "tick": 90})).unwrap();
        assert!(matches!(kind_of(&s), RulerKind::Vanishing(_)));
        // Fisheye horizontal pins y; iso takes the vertical lattice line.
        s.execute("ruler.fisheye.set", json!({"center": [5, 5], "mode": "horizontal"})).unwrap();
        let got = snap_points(&s, vec![StrokePoint::new(99.0, -99.0, 1.0)]);
        assert_eq!((got[0].x, got[0].y), (99.0, 5.0));
        s.execute("ruler.iso.set", json!({})).unwrap();
        let got = snap_points(&s, vec![StrokePoint::new(69.5, 100.0, 1.0)]);
        assert!((got[0].x - 70.0).abs() < 1e-6 && (got[0].y - 100.0).abs() < 1e-6, "{got:?}");
        // Connected chain clamps to its segments.
        s.execute("ruler.connected.set", json!({"points": [[0, 0], [10, 0], [10, 10]]})).unwrap();
        let got = snap_points(&s, vec![StrokePoint::new(10.0, 99.0, 1.0)]);
        assert!((got[0].x - 10.0).abs() < 1e-6 && (got[0].y - 10.0).abs() < 1e-6, "{got:?}");
        // Hostile input errors, never panics (each command at least once).
        for (id, p) in [
            ("ruler.ellipse.set", json!({"radii": [0, 6]})),
            ("ruler.parallel.set", json!({"spacing": 0})),
            ("ruler.parallel.set", json!({"angle": "flat"})),
            ("ruler.radial.set", json!({"tick": 0})),
            ("ruler.radial.set", json!({"tick": 361})),
            ("ruler.vanishing.set", json!({"vps": []})),
            ("ruler.vanishing.set", json!({"vps": [[0, 0], [1, 1], [2, 2], [3, 3], [4, 4], [5, 5], [6, 6]]})),
            ("ruler.perspective.set", json!({"vp1": [0, 0]})),
            ("ruler.fisheye.set", json!({"mode": "diagonal"})),
            ("ruler.iso.set", json!({"angles": [0, 0]})),
            ("ruler.iso.set", json!({"spacing": -5})),
            ("ruler.connected.set", json!({"points": [[0, 0]]})),
            ("ruler.connected.set", json!({"accuracy": 2})),
            ("ruler.bezier.set", json!({})),
            ("ruler.bezier.set", json!({"points": [[0, 0], [1, 1]]})),
            ("ruler.bezier.set", json!({"points": [[0, 0], [1, 1], [2, 2], [3, "x"]]})),
            ("ruler.quadBezier.set", json!({"points": [[0, 0], [1, 1]]})),
            ("ruler.spiral.set", json!({"a": 0})),
            ("ruler.spiral.set", json!({"turns": 99})),
            ("ruler.spiral.set", json!({"b": "golden"})),
        ] {
            assert!(s.execute(id, p).is_err(), "{id}");
        }
    }
}
