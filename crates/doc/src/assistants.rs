//! Drawing assistants: snapping guides stored with a document (resolving #2293).
//!
//! An assistant constrains brush strokes to a geometric guide: a straight line at an angle
//! ([`StraightLineAssistant`]) or the rays of vanishing points around a horizon line
//! ([`PerspectiveAssistant`]). Every assistant implements [`DrawingAssistant`], so later kinds
//! (Bézier curves, curvilinear and fisheye grids, #4408) plug into the same stroke pipeline and
//! overlay renderer without touching them: add a variant to [`DrawingAssistantKind`], implement
//! the trait, and extend the engine commands.
//!
//! All coordinates are document pixels. Everything here is pure data plus pure `f64` geometry:
//! no pixels, no UI toolkit, safe on wasm. Invalid inputs (non-finite numbers, out-of-range
//! indices, degenerate lines) snap to nothing and draw nothing rather than failing.

use serde::{Deserialize, Serialize};

use crate::{LabelColor, next_id};

/// Identity of one drawing assistant (see [`DrawingAssistantItem`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AssistantId(pub u64);

impl AssistantId {
    pub fn fresh() -> Self {
        Self(next_id())
    }
}

/// Largest absolute coordinate an assistant stores (mirrors the brush pipeline's `MAX_COORD`:
/// beyond it dab generation would explode).
pub const MAX_COORDINATE: f64 = 1_000_000.0;
/// How many assistants one document holds (bounds guide-drag hit tests and overlay work).
pub const MAX_ASSISTANTS: usize = 64;
/// How many vanishing points one perspective assistant holds (up to 6-point compositions
/// with headroom for auxiliary points).
pub const MAX_VANISHING_POINTS: usize = 8;
/// How many explicit rays one perspective assistant holds.
pub const MAX_RAYS: usize = 360;
/// Default snapping radius in document pixels (Photoshop-like magnetism at 100 % zoom).
pub const DEFAULT_SNAP_RADIUS: f64 = 8.0;
/// Largest snapping radius accepted (a whole-canvas magnet is meaningless and hides mistakes).
pub const MAX_SNAP_RADIUS: f64 = 500.0;

/// Whether a stored snapping radius is finite and within the command and bundle limits.
pub fn is_valid_snap_radius(radius: f64) -> bool {
    radius.is_finite() && (0.0..=MAX_SNAP_RADIUS).contains(&radius)
}

fn finite2(p: [f64; 2]) -> bool {
    p[0].is_finite() && p[1].is_finite()
}

fn in_range(p: [f64; 2]) -> bool {
    finite2(p) && p[0].abs() <= MAX_COORDINATE && p[1].abs() <= MAX_COORDINATE
}

/// Unit direction of `angle_deg` (counter-clockwise, document Y pointing down like the rest of
/// the view geometry); `None` for a non-finite angle.
fn dir_from_deg(angle_deg: f64) -> Option<[f64; 2]> {
    if !angle_deg.is_finite() {
        return None;
    }
    let a = angle_deg.to_radians();
    Some([a.cos(), a.sin()])
}

/// `p` projected onto the line through `a` along unit `d`, plus its distance to the line.
fn project(p: [f64; 2], a: [f64; 2], d: [f64; 2]) -> ([f64; 2], f64) {
    let t = (p[0] - a[0]) * d[0] + (p[1] - a[1]) * d[1];
    let q = [a[0] + d[0] * t, a[1] + d[1] * t];
    (q, (p[0] - q[0]).hypot(p[1] - q[1]))
}

/// Clip the infinite line through `a` along unit `d` to `viewport` (`[x0, y0, x1, y1]`).
/// `None` when either input is degenerate or the line misses the viewport.
fn clip_line(a: [f64; 2], d: [f64; 2], viewport: [f64; 4]) -> Option<[[f64; 2]; 2]> {
    if !in_range(a) || !finite2(d) || d[0].hypot(d[1]) <= 1e-12 {
        return None;
    }
    let [x0, y0, x1, y1] = viewport;
    if !(x0.is_finite() && y0.is_finite() && x1.is_finite() && y1.is_finite() && x0 < x1 && y0 < y1) {
        return None;
    }
    // Slab method over the parameter t.
    let (mut lo, mut hi) = (f64::NEG_INFINITY, f64::INFINITY);
    for (origin, dir, lo_edge, hi_edge) in [(a[0], d[0], x0, x1), (a[1], d[1], y0, y1)] {
        if dir.abs() < 1e-12 {
            if origin < lo_edge || origin > hi_edge {
                return None;
            }
        } else {
            let (t0, t1) = ((lo_edge - origin) / dir, (hi_edge - origin) / dir);
            lo = lo.max(t0.min(t1));
            hi = hi.min(t0.max(t1));
        }
    }
    if !(lo.is_finite() && hi.is_finite() && hi >= lo) {
        return None;
    }
    Some([[a[0] + d[0] * lo, a[1] + d[1] * lo], [a[0] + d[0] * hi, a[1] + d[1] * hi]])
}

/// Clip the ray from `origin` along unit `d` (`t >= 0`) to `viewport`. `None` as in
/// [`clip_line`].
fn clip_ray(origin: [f64; 2], d: [f64; 2], viewport: [f64; 4]) -> Option<[[f64; 2]; 2]> {
    let seg = clip_line(origin, d, viewport)?;
    // Parameter of each endpoint along the ray.
    let param = |p: [f64; 2]| (p[0] - origin[0]) * d[0] + (p[1] - origin[1]) * d[1];
    let (t0, t1) = (param(seg[0]).max(0.0), param(seg[1]).max(0.0));
    let (lo, hi) = (t0.min(t1), t0.max(t1));
    if hi <= 1e-9 {
        return None;
    }
    Some([[origin[0] + d[0] * lo, origin[1] + d[1] * lo], [origin[0] + d[0] * hi, origin[1] + d[1] * hi]])
}

/// Whether a snapping radius is usable: finite and non-negative (0 snaps exact hits only).
fn usable_radius(radius: f64) -> bool {
    radius.is_finite() && radius >= 0.0
}

/// A straight-line (parallel/isometric angle) ruler: strokes snap onto the line through
/// `anchor` at `angle_deg` degrees. `length` bounds the segment (0 or negative keeps the
/// legacy infinite line, so older bundles still open); new rulers are always finite.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StraightLineAssistant {
    pub anchor: [f64; 2],
    pub angle_deg: f64,
    pub length: f64,
}

impl Default for StraightLineAssistant {
    fn default() -> Self {
        Self { anchor: [0.0, 0.0], angle_deg: 0.0, length: 0.0 }
    }
}

impl StraightLineAssistant {
    /// `None` for a non-finite angle or an out-of-range anchor.
    pub fn new(anchor: [f64; 2], angle_deg: f64) -> Option<Self> {
        if !in_range(anchor) || dir_from_deg(angle_deg).is_none() {
            return None;
        }
        Some(Self { anchor, angle_deg, length: 0.0 })
    }

    /// Half-extent of the segment, or `None` for the legacy infinite line.
    fn half(self) -> Option<f64> {
        (self.length.is_finite() && self.length > 0.0).then_some(0.5 * self.length)
    }

    fn direction(self) -> Option<[f64; 2]> {
        if !in_range(self.anchor) { None } else { dir_from_deg(self.angle_deg) }
    }
}

/// One explicit guide ray of a perspective assistant: the line through vanishing point `vp`
/// at `angle_deg` degrees.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PerspectiveRay {
    pub vp: usize,
    pub angle_deg: f64,
}

/// A 1- or 2-point perspective guide: strokes snap onto the nearest explicit ray through a
/// vanishing point, or onto the horizon line.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PerspectiveAssistant {
    pub vps: Vec<[f64; 2]>,
    pub horizon_y: Option<f64>,
    pub rays: Vec<PerspectiveRay>,
    /// Ray length from each vanishing point (0 or negative keeps the legacy infinite rays).
    pub length: f64,
}

impl PerspectiveAssistant {
    /// Ray extent from each vanishing point, or `None` for infinite rays.
    fn ray_len(&self) -> Option<f64> {
        (self.length.is_finite() && self.length > 0.0).then_some(self.length)
    }

    /// `None` when there are no (or too many) vanishing points, a point is out of range, the
    /// horizon is non-finite, a ray is invalid, or a ray names no vanishing point.
    pub fn new(vps: Vec<[f64; 2]>, horizon_y: Option<f64>, rays: Vec<PerspectiveRay>) -> Option<Self> {
        if vps.is_empty() || vps.len() > MAX_VANISHING_POINTS || !vps.iter().all(|p| in_range(*p)) {
            return None;
        }
        if horizon_y.is_some_and(|h| !h.is_finite() || h.abs() > MAX_COORDINATE) {
            return None;
        }
        if rays.len() > MAX_RAYS || rays.iter().any(|r| r.vp >= vps.len() || dir_from_deg(r.angle_deg).is_none()) {
            return None;
        }
        Some(Self { vps, horizon_y, rays, length: 0.0 })
    }

    /// Vanishing points with a fan of rays every `step_deg` degrees plus the horizon through
    /// the first point (1-point) or through both points' mean (2-point). `step_deg` must be a
    /// finite positive angle; `None` on any invalid input.
    pub fn with_default_rays(vps: Vec<[f64; 2]>, horizon_y: Option<f64>, step_deg: f64) -> Option<Self> {
        if !step_deg.is_finite() || step_deg <= 0.0 || step_deg > 180.0 {
            return None;
        }
        if vps.is_empty() || vps.len() > MAX_VANISHING_POINTS || !vps.iter().all(|p| in_range(*p)) {
            return None;
        }
        let horizon = match horizon_y {
            Some(h) if h.is_finite() && h.abs() <= MAX_COORDINATE => Some(h),
            Some(_) => return None,
            None => {
                let mean = vps.iter().map(|p| p[1]).sum::<f64>() / vps.len() as f64;
                mean.is_finite().then_some(mean)
            }
        };
        let mut rays = Vec::new();
        let mut a = 0.0;
        while a < 180.0 {
            for vp in 0..vps.len() {
                rays.push(PerspectiveRay { vp, angle_deg: a });
            }
            a += step_deg;
        }
        Self::new(vps, horizon, rays)
    }
}

/// A usable guide spacing: finite, positive, and bounded (a planet-sized comb is a mistake).
fn valid_spacing(s: f64) -> bool {
    s.is_finite() && s > 0.0 && s <= MAX_COORDINATE
}

/// Nearest snap of `point` onto explicit perspective rays plus an optional horizon.
/// Returns the snapped point and its distance; the caller applies the radius.
fn snap_to_rays(vps: &[[f64; 2]], rays: &[PerspectiveRay], horizon_y: Option<f64>, point: [f64; 2], ray_len: Option<f64>) -> Option<([f64; 2], f64)> {
    let mut best: Option<([f64; 2], f64)> = None;
    let mut consider = |q: [f64; 2], dist: f64| {
        if finite2(q) && best.is_none_or(|(_, bd)| dist < bd) {
            best = Some((q, dist));
        }
    };
    for ray in rays {
        let (Some(vp), Some(d)) = (vps.get(ray.vp).copied(), dir_from_deg(ray.angle_deg)) else { continue };
        if !in_range(vp) {
            continue;
        }
        if let Some(len) = ray_len {
            let t = (point[0] - vp[0]) * d[0] + (point[1] - vp[1]) * d[1];
            if t < 0.0 || t > len {
                continue;
            }
        }
        let (q, dist) = project(point, vp, d);
        consider(q, dist);
    }
    if let Some(h) = horizon_y.filter(|h| h.is_finite() && h.abs() <= MAX_COORDINATE) {
        consider([point[0], h], (point[1] - h).abs());
    }
    best
}

/// A cubic Bézier curve ruler through 4 control points: strokes snap onto the curve.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BezierAssistant {
    pub points: [[f64; 2]; 4],
}

impl Default for BezierAssistant {
    fn default() -> Self {
        Self { points: [[-90.0, 0.0], [-30.0, 0.0], [30.0, 0.0], [90.0, 0.0]] }
    }
}

fn bezier_eval(pts: &[[f64; 2]; 4], t: f64) -> [f64; 2] {
    let (u, t2, u2) = (1.0 - t, t * t, (1.0 - t) * (1.0 - t));
    [
        u2 * u * pts[0][0] + 3.0 * u2 * t * pts[1][0] + 3.0 * u * t2 * pts[2][0] + t2 * t * pts[3][0],
        u2 * u * pts[0][1] + 3.0 * u2 * t * pts[1][1] + 3.0 * u * t2 * pts[2][1] + t2 * t * pts[3][1],
    ]
}

fn bezier_deriv(pts: &[[f64; 2]; 4], t: f64) -> [f64; 2] {
    let u = 1.0 - t;
    [
        3.0 * u * u * (pts[1][0] - pts[0][0]) + 6.0 * u * t * (pts[2][0] - pts[1][0]) + 3.0 * t * t * (pts[3][0] - pts[2][0]),
        3.0 * u * u * (pts[1][1] - pts[0][1]) + 6.0 * u * t * (pts[2][1] - pts[1][1]) + 3.0 * t * t * (pts[3][1] - pts[2][1]),
    ]
}

/// Nearest point on the cubic and its distance: coarse sampling plus guarded Newton steps.
/// Total for any finite input; degenerate (coincident) points still return a best sample.
fn nearest_on_cubic(pts: &[[f64; 2]; 4], p: [f64; 2]) -> ([f64; 2], f64) {
    let mut best = (bezier_eval(pts, 0.0), f64::INFINITY);
    let mut best_t = 0.0;
    for i in 0..=32 {
        let t = f64::from(i) / 32.0;
        let q = bezier_eval(pts, t);
        let d = (p[0] - q[0]).hypot(p[1] - q[1]);
        if d < best.1 {
            best = (q, d);
            best_t = t;
        }
    }
    let mut t = best_t;
    for _ in 0..10 {
        let q = bezier_eval(pts, t);
        let dv = bezier_deriv(pts, t);
        let denom = dv[0] * dv[0] + dv[1] * dv[1];
        if !(denom > 1e-12) {
            break;
        }
        let step = ((q[0] - p[0]) * dv[0] + (q[1] - p[1]) * dv[1]) / denom;
        if !step.is_finite() {
            break;
        }
        t = (t - step).clamp(0.0, 1.0);
        let q = bezier_eval(pts, t);
        let d = (p[0] - q[0]).hypot(p[1] - q[1]);
        if d < best.1 {
            best = (q, d);
        }
    }
    best
}

impl BezierAssistant {
    /// `None` unless all four control points are in range.
    pub fn new(points: [[f64; 2]; 4]) -> Option<Self> {
        points.iter().all(|p| in_range(*p)).then_some(Self { points })
    }
}

/// A parallel-line ruler through `anchor` at `angle_deg`: strokes snap onto the nearest of
/// the infinite family of parallel lines `spacing` apart, anywhere on the canvas.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ParallelAssistant {
    pub anchor: [f64; 2],
    pub angle_deg: f64,
    pub spacing: f64,
    /// Segment length like [`StraightLineAssistant::length`]: 0 or negative is infinite.
    pub length: f64,
}

impl Default for ParallelAssistant {
    fn default() -> Self {
        Self { anchor: [0.0, 0.0], angle_deg: 0.0, spacing: 20.0, length: 0.0 }
    }
}

impl ParallelAssistant {
    /// `None` for a bad anchor, a non-finite angle, or an unusable spacing.
    pub fn new(anchor: [f64; 2], angle_deg: f64, spacing: f64) -> Option<Self> {
        if !in_range(anchor) || dir_from_deg(angle_deg).is_none() || !valid_spacing(spacing) {
            return None;
        }
        Some(Self { anchor, angle_deg, spacing, length: 0.0 })
    }

    /// Half-extent of each family segment, or `None` for infinite lines.
    fn half(self) -> Option<f64> {
        (self.length.is_finite() && self.length > 0.0).then_some(0.5 * self.length)
    }

    fn direction(self) -> Option<[f64; 2]> {
        if !in_range(self.anchor) || !valid_spacing(self.spacing) { None } else { dir_from_deg(self.angle_deg) }
    }
}

/// An ellipse ruler: strokes snap onto the rotated ellipse with radii `radii`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EllipseAssistant {
    pub center: [f64; 2],
    pub radii: [f64; 2],
    pub rotation_deg: f64,
}

impl Default for EllipseAssistant {
    fn default() -> Self {
        Self { center: [0.0, 0.0], radii: [60.0, 40.0], rotation_deg: 0.0 }
    }
}

impl EllipseAssistant {
    /// `None` for a bad center, non-positive or unbounded radii, or a non-finite rotation.
    pub fn new(center: [f64; 2], radii: [f64; 2], rotation_deg: f64) -> Option<Self> {
        if !in_range(center) || !rotation_deg.is_finite() || radii.iter().any(|r| !r.is_finite() || *r <= 0.0 || *r > MAX_COORDINATE) {
            return None;
        }
        Some(Self { center, radii, rotation_deg })
    }

    /// Nearest point on the ellipse and its distance: parametric guess plus guarded Newton
    /// steps in the ellipse's local frame. `None` for degenerate stored geometry.
    fn nearest(&self, p: [f64; 2]) -> Option<([f64; 2], f64)> {
        let [rx, ry] = self.radii;
        if !(rx > 0.0 && ry > 0.0 && rx <= MAX_COORDINATE && ry <= MAX_COORDINATE) || !in_range(self.center) || !self.rotation_deg.is_finite() {
            return None;
        }
        let a = self.rotation_deg.to_radians();
        let (ca, sa) = (a.cos(), a.sin());
        // Into the local frame (rotation by -a, translate).
        let dx = p[0] - self.center[0];
        let dy = p[1] - self.center[1];
        let lx = ca * dx + sa * dy;
        let ly = -sa * dx + ca * dy;
        let mut t = (ly / ry).atan2(lx / rx);
        let eval = |t: f64| [rx * t.cos(), ry * t.sin()];
        let mut best = None;
        for _ in 0..12 {
            let e = eval(t);
            let ex = rx * t.sin();
            let ey = ry * t.cos();
            // f(t) = (E - L)·E'; f'(t) = |E'|² + (E - L)·E''.
            let f = (e[0] - lx) * -ex + (e[1] - ly) * ey;
            let fp = ex * ex + ey * ey + (e[0] - lx) * -e[0] + (e[1] - ly) * -e[1];
            if !(fp.abs() > 1e-9) {
                break;
            }
            let step = f / fp;
            if !step.is_finite() {
                break;
            }
            t -= step;
            let e = eval(t);
            let d = (e[0] - lx).hypot(e[1] - ly);
            if best.is_none_or(|(_, bd)| d < bd) {
                // Back into document space (rotation by +a, translate).
                best = Some(([self.center[0] + ca * e[0] - sa * e[1], self.center[1] + sa * e[0] + ca * e[1]], d));
            }
            if step.abs() < 1e-9 {
                break;
            }
        }
        // The parametric guess itself is the fallback when Newton never improves.
        if best.is_none() {
            let e = eval(t);
            best = Some(([self.center[0] + ca * e[0] - sa * e[1], self.center[1] + sa * e[0] + ca * e[1]], (e[0] - lx).hypot(e[1] - ly)));
        }
        best
    }
}

/// Concentric circles about `center`, one ring every `spacing`: strokes snap onto the
/// nearest ring.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConcentricAssistant {
    pub center: [f64; 2],
    pub spacing: f64,
}

impl Default for ConcentricAssistant {
    fn default() -> Self {
        Self { center: [0.0, 0.0], spacing: 20.0 }
    }
}

impl ConcentricAssistant {
    /// `None` for a bad center or an unusable spacing.
    pub fn new(center: [f64; 2], spacing: f64) -> Option<Self> {
        if !in_range(center) || !valid_spacing(spacing) {
            return None;
        }
        Some(Self { center, spacing })
    }
}

/// A regular polygon ruler: strokes snap onto its edges.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PolygonAssistant {
    pub center: [f64; 2],
    pub radius: f64,
    pub sides: u32,
    pub rotation_deg: f64,
}

impl Default for PolygonAssistant {
    fn default() -> Self {
        Self { center: [0.0, 0.0], radius: 60.0, sides: 5, rotation_deg: -90.0 }
    }
}

impl PolygonAssistant {
    /// `None` for a bad center, a non-positive radius, fewer than 3 (or more than 64) sides,
    /// or a non-finite rotation.
    pub fn new(center: [f64; 2], radius: f64, sides: u32, rotation_deg: f64) -> Option<Self> {
        if !in_range(center) || !radius.is_finite() || radius <= 0.0 || radius > MAX_COORDINATE || !(3..=64).contains(&sides) || !rotation_deg.is_finite() {
            return None;
        }
        Some(Self { center, radius, sides, rotation_deg })
    }

    /// Vertices in document space; `None` for degenerate stored geometry.
    fn vertices(&self) -> Option<Vec<[f64; 2]>> {
        if !in_range(self.center) || !(self.radius > 0.0) || !(3..=64).contains(&self.sides) || !self.rotation_deg.is_finite() {
            return None;
        }
        let a0 = self.rotation_deg.to_radians();
        Some(
            (0..self.sides)
                .map(|i| {
                    let a = a0 + f64::from(i) * std::f64::consts::TAU / f64::from(self.sides);
                    [self.center[0] + self.radius * a.cos(), self.center[1] + self.radius * a.sin()]
                })
                .collect(),
        )
    }
}

/// Nearest point on segment `ab` and its distance.
fn nearest_on_segment(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> ([f64; 2], f64) {
    let ab = [b[0] - a[0], b[1] - a[1]];
    let denom = ab[0] * ab[0] + ab[1] * ab[1];
    let t = if denom > 1e-12 { ((p[0] - a[0]) * ab[0] + (p[1] - a[1]) * ab[1]) / denom } else { 0.0 }.clamp(0.0, 1.0);
    let q = [a[0] + ab[0] * t, a[1] + ab[1] * t];
    (q, (p[0] - q[0]).hypot(p[1] - q[1]))
}

/// A fisheye (curvilinear) perspective ruler: plate-space perspective rays bent through an
/// equidistant azimuthal projection about `center`, inside the lens circle of `radius`.
/// `strength` 0 is linear perspective; 1 is full fisheye. Outside the lens nothing snaps,
/// as in Clip Studio Paint.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FisheyeAssistant {
    pub center: [f64; 2],
    pub radius: f64,
    pub strength: f64,
    pub vps: Vec<[f64; 2]>,
    pub horizon_y: Option<f64>,
    pub rays: Vec<PerspectiveRay>,
}

impl FisheyeAssistant {
    /// `None` for a bad center, a non-positive radius, a strength outside 0–1, bad vanishing
    /// points, or invalid rays (same ray rules as perspective).
    pub fn new(center: [f64; 2], radius: f64, strength: f64, vps: Vec<[f64; 2]>, horizon_y: Option<f64>, rays: Vec<PerspectiveRay>) -> Option<Self> {
        if !in_range(center) || !radius.is_finite() || radius <= 0.0 || radius > MAX_COORDINATE || !strength.is_finite() || strength < 0.0 || strength > 1.0 {
            return None;
        }
        if vps.is_empty() || vps.len() > MAX_VANISHING_POINTS || !vps.iter().all(|p| in_range(*p)) {
            return None;
        }
        if horizon_y.is_some_and(|h| !h.is_finite() || h.abs() > MAX_COORDINATE) {
            return None;
        }
        if rays.len() > MAX_RAYS || rays.iter().any(|r| r.vp >= vps.len() || dir_from_deg(r.angle_deg).is_none()) {
            return None;
        }
        Some(Self { center, radius, strength, vps, horizon_y, rays })
    }

    /// Distort plate point `q` into lens space; `None` outside the lens.
    fn forward(&self, q: [f64; 2]) -> Option<[f64; 2]> {
        let dx = (q[0] - self.center[0]) / self.radius;
        let dy = (q[1] - self.center[1]) / self.radius;
        let r = dx.hypot(dy);
        if !(r <= 1.0) || !r.is_finite() {
            return None;
        }
        let m = (1.0 - self.strength) + self.strength * r * r;
        Some([self.center[0] + self.radius * dx * m, self.center[1] + self.radius * dy * m])
    }

    /// Undistort lens point `p` into plate space; `None` outside the lens. The radial map
    /// `r * m(r)` is strictly increasing on [0, 1] for strengths in 0–1, so bisection lands.
    fn backward(&self, p: [f64; 2]) -> Option<[f64; 2]> {
        let dx = (p[0] - self.center[0]) / self.radius;
        let dy = (p[1] - self.center[1]) / self.radius;
        let rv = dx.hypot(dy);
        if !(rv <= 1.0) || !rv.is_finite() {
            return None;
        }
        if rv < 1e-12 {
            return Some(self.center);
        }
        let (mut lo, mut hi) = (0.0, 1.0);
        for _ in 0..40 {
            let mid = 0.5 * (lo + hi);
            if mid * ((1.0 - self.strength) + self.strength * mid * mid) < rv {
                lo = mid
            } else {
                hi = mid
            };
        }
        let r = 0.5 * (lo + hi);
        Some([self.center[0] + self.radius * dx / rv * r, self.center[1] + self.radius * dy / rv * r])
    }
}

/// The guide geometry of one assistant. New kinds only add a variant plus its trait impl:
/// the stroke pipeline, overlay renderer, commands, persistence and UI need no changes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum DrawingAssistantKind {
    StraightLine(StraightLineAssistant),
    Perspective(PerspectiveAssistant),
    Bezier(BezierAssistant),
    Parallel(ParallelAssistant),
    Ellipse(EllipseAssistant),
    Concentric(ConcentricAssistant),
    Polygon(PolygonAssistant),
    Fisheye(FisheyeAssistant),
}

/// A visual primitive for the canvas overlay renderer (toolkit-agnostic data, like the tool
/// overlay seam: the UI draws these, it never computes guide geometry itself).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GuidePrimitive {
    /// A guide segment in document pixels. `major` marks the primary guides (the straight
    /// line itself, the horizon): the UI draws those solid and the secondary ray fan dotted.
    Segment { a: [f64; 2], b: [f64; 2], major: bool },
    /// An interactive drag handle in document pixels.
    Handle { pos: [f64; 2], kind: HandleKind },
}

/// What a [`GuidePrimitive::Handle`] drags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandleKind {
    /// The straight-line anchor (moves the line).
    Anchor,
    /// The straight-line angle handle (rotates the line about its anchor).
    Angle,
    /// Vanishing point `usize` (moves the point and its rays).
    VanishingPoint(usize),
    /// A kind-specific control point by index (Bézier control points, ellipse/polygon/fisheye
    /// handles); the command validates the index against the kind.
    ControlPoint(usize),
}

/// The extensible assistant interface: snapping plus overlay, in document pixels.
///
/// Implementors must be total: any input — non-finite points, degenerate geometry loaded from
/// an older or hand-edited file — snaps to nothing and draws nothing, never panics.
pub trait DrawingAssistant: Send + Sync {
    /// The snapped coordinate when `point` is within `snap_radius` of the guide, else `None`.
    fn snap_point(&self, point: [f64; 2], snap_radius: f64) -> Option<[f64; 2]>;
    /// Visual primitives clipped to `viewport` (`[x0, y0, x1, y1]`, document pixels).
    fn overlay_primitives(&self, viewport: [f64; 4]) -> Vec<GuidePrimitive>;
}

impl DrawingAssistant for StraightLineAssistant {
    fn snap_point(&self, point: [f64; 2], snap_radius: f64) -> Option<[f64; 2]> {
        if !finite2(point) || !usable_radius(snap_radius) {
            return None;
        }
        let d = self.direction()?;
        let (q, dist) = project(point, self.anchor, d);
        if let Some(half) = self.half() {
            let t = (point[0] - self.anchor[0]) * d[0] + (point[1] - self.anchor[1]) * d[1];
            if t.abs() > half {
                return None;
            }
        }
        (dist <= snap_radius && finite2(q)).then_some(q)
    }

    fn overlay_primitives(&self, viewport: [f64; 4]) -> Vec<GuidePrimitive> {
        let mut out = Vec::new();
        let Some(d) = self.direction() else { return out };
        if let Some(half) = self.half() {
            out.push(GuidePrimitive::Segment {
                a: [self.anchor[0] - d[0] * half, self.anchor[1] - d[1] * half],
                b: [self.anchor[0] + d[0] * half, self.anchor[1] + d[1] * half],
                major: true,
            });
        } else if let Some([a, b]) = clip_line(self.anchor, d, viewport) {
            out.push(GuidePrimitive::Segment { a, b, major: true });
        }
        out.push(GuidePrimitive::Handle { pos: self.anchor, kind: HandleKind::Anchor });
        // Angle handle at a fixed document distance along the line (viewport-independent, so it
        // stays grabbable at any zoom).
        const ANGLE_HANDLE_DIST: f64 = 60.0;
        let angle_pos = [self.anchor[0] + d[0] * ANGLE_HANDLE_DIST, self.anchor[1] + d[1] * ANGLE_HANDLE_DIST];
        if finite2(angle_pos) {
            out.push(GuidePrimitive::Handle { pos: angle_pos, kind: HandleKind::Angle });
        }
        out
    }
}

impl DrawingAssistant for PerspectiveAssistant {
    fn snap_point(&self, point: [f64; 2], snap_radius: f64) -> Option<[f64; 2]> {
        if !finite2(point) || !usable_radius(snap_radius) {
            return None;
        }
        let (q, dist) = snap_to_rays(&self.vps, &self.rays, self.horizon_y, point, self.ray_len())?;
        (dist <= snap_radius).then_some(q)
    }

    fn overlay_primitives(&self, viewport: [f64; 4]) -> Vec<GuidePrimitive> {
        let mut out = Vec::new();
        for ray in &self.rays {
            let (Some(vp), Some(d)) = (self.vps.get(ray.vp).copied(), dir_from_deg(ray.angle_deg)) else { continue };
            if !in_range(vp) {
                continue;
            }
            if let Some([a, b]) = clip_ray(vp, d, viewport) {
                let (a, b) = match self.ray_len() {
                    None => (a, b),
                    Some(len) => {
                        let t = |p: [f64; 2]| (p[0] - vp[0]) * d[0] + (p[1] - vp[1]) * d[1];
                        let (lo, hi) = (t(a).max(0.0).min(len), t(b).max(0.0).min(len));
                        if hi <= lo + 1e-9 {
                            continue;
                        }
                        ([vp[0] + d[0] * lo, vp[1] + d[1] * lo], [vp[0] + d[0] * hi, vp[1] + d[1] * hi])
                    }
                };
                out.push(GuidePrimitive::Segment { a, b, major: false });
            }
        }
        let [x0, _, x1, _] = viewport;
        if let Some(h) = self.horizon_y.filter(|h| h.is_finite() && h.abs() <= MAX_COORDINATE) {
            let [_, y0, _, y1] = viewport;
            if h >= y0 && h <= y1 && x0 < x1 {
                out.push(GuidePrimitive::Segment { a: [x0, h], b: [x1, h], major: true });
            }
        }
        for (i, vp) in self.vps.iter().enumerate() {
            if in_range(*vp) {
                out.push(GuidePrimitive::Handle { pos: *vp, kind: HandleKind::VanishingPoint(i) });
            }
        }
        out
    }
}

impl DrawingAssistant for BezierAssistant {
    fn snap_point(&self, point: [f64; 2], snap_radius: f64) -> Option<[f64; 2]> {
        if !finite2(point) || !usable_radius(snap_radius) || !self.points.iter().all(|p| in_range(*p)) {
            return None;
        }
        let (q, dist) = nearest_on_cubic(&self.points, point);
        (dist <= snap_radius && finite2(q)).then_some(q)
    }

    fn overlay_primitives(&self, viewport: [f64; 4]) -> Vec<GuidePrimitive> {
        let mut out = Vec::new();
        if !self.points.iter().all(|p| in_range(*p)) {
            return out;
        }
        // The curve as short solid majors, the control polygon as dotted minors.
        const STEPS: usize = 24;
        let mut prev = bezier_eval(&self.points, 0.0);
        for i in 1..=STEPS {
            let q = bezier_eval(&self.points, i as f64 / STEPS as f64);
            out.push(GuidePrimitive::Segment { a: prev, b: q, major: true });
            prev = q;
        }
        for w in self.points.windows(2) {
            out.push(GuidePrimitive::Segment { a: w[0], b: w[1], major: false });
        }
        let _ = viewport;
        for (i, p) in self.points.iter().enumerate() {
            out.push(GuidePrimitive::Handle { pos: *p, kind: HandleKind::ControlPoint(i) });
        }
        out
    }
}

impl DrawingAssistant for ParallelAssistant {
    fn snap_point(&self, point: [f64; 2], snap_radius: f64) -> Option<[f64; 2]> {
        if !finite2(point) || !usable_radius(snap_radius) {
            return None;
        }
        let d = self.direction()?;
        let n = [-d[1], d[0]];
        let s = (point[0] - self.anchor[0]) * n[0] + (point[1] - self.anchor[1]) * n[1];
        let k = (s / self.spacing).round();
        let off = s - k * self.spacing;
        if let Some(half) = self.half() {
            let c = [self.anchor[0] + n[0] * k * self.spacing, self.anchor[1] + n[1] * k * self.spacing];
            let t = (point[0] - c[0]) * d[0] + (point[1] - c[1]) * d[1];
            if t.abs() > half {
                return None;
            }
        }
        (off.abs() <= snap_radius).then(|| [point[0] - n[0] * off, point[1] - n[1] * off]).filter(|q| finite2(*q))
    }

    fn overlay_primitives(&self, viewport: [f64; 4]) -> Vec<GuidePrimitive> {
        let mut out = Vec::new();
        let Some(d) = self.direction() else { return out };
        // One segment per family line: finite around its center when `length` is set,
        // else clipped to the viewport like the legacy infinite ruler.
        let seg = |c: [f64; 2]| -> Option<[[f64; 2]; 2]> {
            if let Some(half) = self.half() {
                Some([[c[0] - d[0] * half, c[1] - d[1] * half], [c[0] + d[0] * half, c[1] + d[1] * half]])
            } else {
                clip_line(c, d, viewport)
            }
        };
        if let Some([a, b]) = seg(self.anchor) {
            out.push(GuidePrimitive::Segment { a, b, major: true });
        }
        // Family lines covering the viewport, strided so a dense comb stays bounded.
        let n = [-d[1], d[0]];
        let corners = [[viewport[0], viewport[1]], [viewport[2], viewport[1]], [viewport[2], viewport[3]], [viewport[0], viewport[3]]];
        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        for c in corners {
            let s = (c[0] - self.anchor[0]) * n[0] + (c[1] - self.anchor[1]) * n[1];
            lo = lo.min(s);
            hi = hi.max(s);
        }
        if !(lo.is_finite() && hi.is_finite()) {
            return out;
        }
        // Family lines covering the viewport; a sub-pixel comb draws as the base line.
        if (hi - lo) / self.spacing <= 256.0 {
            let start = (lo / self.spacing).floor();
            let count = ((hi - lo) / self.spacing).ceil() as usize;
            for j in 0..=count.min(256) {
                let k = start + j as f64;
                if k == 0.0 {
                    continue;
                }
                let a = [self.anchor[0] + n[0] * k * self.spacing, self.anchor[1] + n[1] * k * self.spacing];
                if let Some([p, q]) = seg(a) {
                    out.push(GuidePrimitive::Segment { a: p, b: q, major: false });
                }
            }
        }
        out.push(GuidePrimitive::Handle { pos: self.anchor, kind: HandleKind::Anchor });
        const ANGLE_HANDLE_DIST: f64 = 60.0;
        let angle_pos = [self.anchor[0] + d[0] * ANGLE_HANDLE_DIST, self.anchor[1] + d[1] * ANGLE_HANDLE_DIST];
        if finite2(angle_pos) {
            out.push(GuidePrimitive::Handle { pos: angle_pos, kind: HandleKind::Angle });
        }
        out
    }
}

impl DrawingAssistant for EllipseAssistant {
    fn snap_point(&self, point: [f64; 2], snap_radius: f64) -> Option<[f64; 2]> {
        if !finite2(point) || !usable_radius(snap_radius) {
            return None;
        }
        self.nearest(point).and_then(|(q, dist)| (dist <= snap_radius && finite2(q)).then_some(q))
    }

    fn overlay_primitives(&self, viewport: [f64; 4]) -> Vec<GuidePrimitive> {
        let mut out = Vec::new();
        let [rx, ry] = self.radii;
        if !(rx > 0.0 && ry > 0.0) || !in_range(self.center) || !self.rotation_deg.is_finite() {
            return out;
        }
        let a = self.rotation_deg.to_radians();
        let (ca, sa) = (a.cos(), a.sin());
        let at = |t: f64| [self.center[0] + ca * rx * t.cos() - sa * ry * t.sin(), self.center[1] + sa * rx * t.cos() + ca * ry * t.sin()];
        const STEPS: usize = 64;
        let mut prev = at(0.0);
        for i in 1..=STEPS {
            let q = at(i as f64 / STEPS as f64 * std::f64::consts::TAU);
            out.push(GuidePrimitive::Segment { a: prev, b: q, major: true });
            prev = q;
        }
        let _ = viewport;
        out.push(GuidePrimitive::Handle { pos: self.center, kind: HandleKind::ControlPoint(0) });
        out.push(GuidePrimitive::Handle { pos: at(0.0), kind: HandleKind::ControlPoint(1) });
        out.push(GuidePrimitive::Handle { pos: at(std::f64::consts::FRAC_PI_2), kind: HandleKind::ControlPoint(2) });
        out
    }
}

impl DrawingAssistant for ConcentricAssistant {
    fn snap_point(&self, point: [f64; 2], snap_radius: f64) -> Option<[f64; 2]> {
        if !finite2(point) || !usable_radius(snap_radius) || !in_range(self.center) || !valid_spacing(self.spacing) {
            return None;
        }
        let dx = [point[0] - self.center[0], point[1] - self.center[1]];
        let r = dx[0].hypot(dx[1]);
        if !(r > 1e-9) {
            return None;
        }
        let k = (r / self.spacing).round().max(1.0);
        let off = r - k * self.spacing;
        (off.abs() <= snap_radius)
            .then(|| [self.center[0] + dx[0] / r * k * self.spacing, self.center[1] + dx[1] / r * k * self.spacing])
            .filter(|q| finite2(*q))
    }

    fn overlay_primitives(&self, viewport: [f64; 4]) -> Vec<GuidePrimitive> {
        let mut out = Vec::new();
        if !in_range(self.center) || !valid_spacing(self.spacing) {
            return out;
        }
        // Rings reaching the viewport, capped so a fine comb stays bounded.
        let cx = 0.5 * (viewport[0] + viewport[2]);
        let cy = 0.5 * (viewport[1] + viewport[3]);
        let far = (self.center[0] - cx).hypot(self.center[1] - cy) + 0.5 * (viewport[2] - viewport[0]).hypot(viewport[3] - viewport[1]);
        if !far.is_finite() {
            return out;
        }
        let rings = ((far / self.spacing).ceil() as usize).min(32).max(1);
        const STEPS: usize = 48;
        for k in 1..=rings {
            let r = k as f64 * self.spacing;
            let mut prev = [self.center[0] + r, self.center[1]];
            for i in 1..=STEPS {
                let t = i as f64 / STEPS as f64 * std::f64::consts::TAU;
                let q = [self.center[0] + r * t.cos(), self.center[1] + r * t.sin()];
                out.push(GuidePrimitive::Segment { a: prev, b: q, major: false });
                prev = q;
            }
        }
        out.push(GuidePrimitive::Handle { pos: self.center, kind: HandleKind::Anchor });
        out
    }
}

impl DrawingAssistant for PolygonAssistant {
    fn snap_point(&self, point: [f64; 2], snap_radius: f64) -> Option<[f64; 2]> {
        if !finite2(point) || !usable_radius(snap_radius) {
            return None;
        }
        let v = self.vertices()?;
        let mut best: Option<([f64; 2], f64)> = None;
        for i in 0..v.len() {
            let (q, dist) = nearest_on_segment(point, v[i], v[(i + 1) % v.len()]);
            if finite2(q) && best.is_none_or(|(_, bd)| dist < bd) {
                best = Some((q, dist));
            }
        }
        best.and_then(|(q, dist)| (dist <= snap_radius).then_some(q))
    }

    fn overlay_primitives(&self, viewport: [f64; 4]) -> Vec<GuidePrimitive> {
        let mut out = Vec::new();
        let Some(v) = self.vertices() else { return out };
        let _ = viewport;
        for i in 0..v.len() {
            out.push(GuidePrimitive::Segment { a: v[i], b: v[(i + 1) % v.len()], major: true });
        }
        out.push(GuidePrimitive::Handle { pos: self.center, kind: HandleKind::ControlPoint(0) });
        out.push(GuidePrimitive::Handle { pos: v[0], kind: HandleKind::ControlPoint(1) });
        out
    }
}

impl DrawingAssistant for FisheyeAssistant {
    fn snap_point(&self, point: [f64; 2], snap_radius: f64) -> Option<[f64; 2]> {
        if !finite2(point) || !usable_radius(snap_radius) || !valid_spacing(self.radius) || !in_range(self.center) {
            return None;
        }
        if !(self.strength.is_finite() && (0.0..=1.0).contains(&self.strength)) {
            return None;
        }
        let plate = self.backward(point)?;
        let (q, _) = snap_to_rays(&self.vps, &self.rays, self.horizon_y, plate, None)?;
        let out = self.forward(q)?;
        let dist = (point[0] - out[0]).hypot(point[1] - out[1]);
        (dist <= snap_radius && finite2(out)).then_some(out)
    }

    fn overlay_primitives(&self, viewport: [f64; 4]) -> Vec<GuidePrimitive> {
        let mut out = Vec::new();
        if !valid_spacing(self.radius) || !in_range(self.center) || !(self.strength.is_finite() && (0.0..=1.0).contains(&self.strength)) {
            return out;
        }
        // The lens circle itself, so the snapping range is visible.
        const STEPS: usize = 64;
        let mut prev = [self.center[0] + self.radius, self.center[1]];
        for i in 1..=STEPS {
            let t = i as f64 / STEPS as f64 * std::f64::consts::TAU;
            let q = [self.center[0] + self.radius * t.cos(), self.center[1] + self.radius * t.sin()];
            out.push(GuidePrimitive::Segment { a: prev, b: q, major: true });
            prev = q;
        }
        // Each ray bent through the lens, sampled and clipped to the disc.
        const SAMPLES: usize = 17;
        for ray in &self.rays {
            let (Some(vp), Some(d)) = (self.vps.get(ray.vp).copied(), dir_from_deg(ray.angle_deg)) else { continue };
            if !in_range(vp) {
                continue;
            }
            // Plate-space extent reaching the lens.
            let reach = (vp[0] - self.center[0]).hypot(vp[1] - self.center[1]) + self.radius + 1.0;
            if !reach.is_finite() {
                continue;
            }
            let mut prev: Option<[f64; 2]> = None;
            for i in 0..=SAMPLES {
                let t = reach * i as f64 / SAMPLES as f64;
                let bent = self.forward([vp[0] + d[0] * t, vp[1] + d[1] * t]);
                if let (Some(a), Some(b)) = (prev, bent) {
                    out.push(GuidePrimitive::Segment { a, b, major: false });
                }
                prev = bent;
            }
        }
        if let Some(h) = self.horizon_y.filter(|h| h.is_finite() && h.abs() <= MAX_COORDINATE) {
            // The horizon bent through the lens.
            let [x0, _, x1, _] = viewport;
            const HSAMPLES: usize = 25;
            let mut prev: Option<[f64; 2]> = None;
            for i in 0..=HSAMPLES {
                let x = x0 + (x1 - x0) * i as f64 / HSAMPLES as f64;
                let bent = self.forward([x, h]);
                if let (Some(a), Some(b)) = (prev, bent) {
                    out.push(GuidePrimitive::Segment { a, b, major: true });
                }
                prev = bent;
            }
        }
        out.push(GuidePrimitive::Handle { pos: self.center, kind: HandleKind::Anchor });
        let radius_pos = [self.center[0] + self.radius, self.center[1]];
        if finite2(radius_pos) {
            out.push(GuidePrimitive::Handle { pos: radius_pos, kind: HandleKind::ControlPoint(0) });
        }
        for (i, vp) in self.vps.iter().enumerate() {
            if in_range(*vp) {
                out.push(GuidePrimitive::Handle { pos: *vp, kind: HandleKind::VanishingPoint(i) });
            }
        }
        out
    }
}

impl DrawingAssistant for DrawingAssistantKind {
    fn snap_point(&self, point: [f64; 2], snap_radius: f64) -> Option<[f64; 2]> {
        match self {
            Self::StraightLine(a) => a.snap_point(point, snap_radius),
            Self::Perspective(a) => a.snap_point(point, snap_radius),
            Self::Bezier(a) => a.snap_point(point, snap_radius),
            Self::Parallel(a) => a.snap_point(point, snap_radius),
            Self::Ellipse(a) => a.snap_point(point, snap_radius),
            Self::Concentric(a) => a.snap_point(point, snap_radius),
            Self::Polygon(a) => a.snap_point(point, snap_radius),
            Self::Fisheye(a) => a.snap_point(point, snap_radius),
        }
    }

    fn overlay_primitives(&self, viewport: [f64; 4]) -> Vec<GuidePrimitive> {
        match self {
            Self::StraightLine(a) => a.overlay_primitives(viewport),
            Self::Perspective(a) => a.overlay_primitives(viewport),
            Self::Bezier(a) => a.overlay_primitives(viewport),
            Self::Parallel(a) => a.overlay_primitives(viewport),
            Self::Ellipse(a) => a.overlay_primitives(viewport),
            Self::Concentric(a) => a.overlay_primitives(viewport),
            Self::Polygon(a) => a.overlay_primitives(viewport),
            Self::Fisheye(a) => a.overlay_primitives(viewport),
        }
    }
}

/// One assistant on the canvas: an id for the commands, a display name, visibility, whether
/// its guides snap strokes (off draws the overlay dimmed), a label colour (`None` uses the
/// theme's assistant colour), and its geometry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DrawingAssistantItem {
    pub id: AssistantId,
    pub name: String,
    pub visible: bool,
    pub snapping: bool,
    pub color: LabelColor,
    pub kind: DrawingAssistantKind,
}

impl DrawingAssistantItem {
    pub fn new(name: String, kind: DrawingAssistantKind) -> Self {
        Self { id: AssistantId::fresh(), name, visible: true, snapping: true, color: LabelColor::None, kind }
    }
}

impl Default for DrawingAssistantItem {
    fn default() -> Self {
        Self {
            id: AssistantId::fresh(),
            name: String::new(),
            visible: true,
            snapping: true,
            color: LabelColor::None,
            kind: DrawingAssistantKind::StraightLine(StraightLineAssistant::default()),
        }
    }
}

/// Where a snapped point came from (for UI feedback and tests).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AssistSnap {
    pub point: [f64; 2],
    pub assistant: AssistantId,
}

/// The document's assistant set: a master switch, a shared magnetism radius, and the guides.
/// Saved with the document (`.pcraft`); PSD has no equivalent, so assistants stay document-local
/// on PSD round trips, like the file encoding.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DrawingAssistants {
    pub enabled: bool,
    pub snap_radius: f64,
    pub items: Vec<DrawingAssistantItem>,
}

impl Default for DrawingAssistants {
    fn default() -> Self {
        Self { enabled: true, snap_radius: DEFAULT_SNAP_RADIUS, items: Vec::new() }
    }
}

impl DrawingAssistants {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn get(&self, id: AssistantId) -> Option<&DrawingAssistantItem> {
        self.items.iter().find(|a| a.id == id)
    }

    pub fn get_mut(&mut self, id: AssistantId) -> Option<&mut DrawingAssistantItem> {
        self.items.iter_mut().find(|a| a.id == id)
    }

    /// The nearest snap across every visible, snapping assistant, or `None` when snapping is
    /// off, the radius is unusable, or nothing is close enough. First in document order wins ties.
    pub fn snap_point(&self, point: [f64; 2]) -> Option<AssistSnap> {
        self.snap_point_with_radius(point, self.snap_radius)
    }

    /// [`Self::snap_point`] with an explicit radius (the UI passes `8 px / zoom`).
    pub fn snap_point_with_radius(&self, point: [f64; 2], radius: f64) -> Option<AssistSnap> {
        if !self.enabled || !finite2(point) || !usable_radius(radius) {
            return None;
        }
        let mut best: Option<(AssistSnap, f64)> = None;
        for item in self.items.iter().filter(|a| a.visible && a.snapping) {
            if let Some(q) = item.kind.snap_point(point, radius) {
                let dist = (point[0] - q[0]).hypot(point[1] - q[1]);
                if best.is_none_or(|(_, bd)| dist < bd) {
                    best = Some((AssistSnap { point: q, assistant: item.id }, dist));
                }
            }
        }
        best.map(|(hit, _)| hit)
    }

    /// Overlay primitives of every visible assistant, in document order.
    pub fn overlay_primitives(&self, viewport: [f64; 4]) -> Vec<GuidePrimitive> {
        if !self.enabled {
            return Vec::new();
        }
        self.items.iter().filter(|a| a.visible).flat_map(|a| a.kind.overlay_primitives(viewport)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_snap_radius_limits() {
        for radius in [0.0, DEFAULT_SNAP_RADIUS, MAX_SNAP_RADIUS] {
            assert!(is_valid_snap_radius(radius), "{radius}");
        }
        for radius in [-1.0, MAX_SNAP_RADIUS + 1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(!is_valid_snap_radius(radius), "{radius}");
        }
    }

    #[test]
    fn perspective_horizon_overlay_matches_coordinate_limits() {
        let viewport = [0.0, -MAX_COORDINATE - 1.0, 100.0, MAX_COORDINATE + 1.0];
        for horizon in [-MAX_COORDINATE, 0.0, MAX_COORDINATE] {
            let p = PerspectiveAssistant { horizon_y: Some(horizon), ..Default::default() };
            assert_eq!(p.overlay_primitives(viewport), vec![GuidePrimitive::Segment { a: [0.0, horizon], b: [100.0, horizon], major: true }]);
            assert_eq!(p.snap_point([50.0, horizon], 0.0), Some([50.0, horizon]));
        }
        for horizon in [-MAX_COORDINATE - 1.0, MAX_COORDINATE + 1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let p = PerspectiveAssistant { horizon_y: Some(horizon), ..Default::default() };
            assert!(p.overlay_primitives(viewport).is_empty(), "{horizon}");
            assert_eq!(p.snap_point([50.0, horizon], 0.0), None);
        }
    }

    fn straight() -> StraightLineAssistant {
        StraightLineAssistant::new([100.0, 100.0], 0.0).expect("valid horizontal line")
    }

    #[test]
    fn test_straight_line_snapping_within_threshold() {
        let a = straight();
        // Directly above the line: projects straight down.
        assert_eq!(a.snap_point([150.0, 104.0], 8.0), Some([150.0, 100.0]));
        // Exactly on the line snaps to itself.
        assert_eq!(a.snap_point([40.0, 100.0], 8.0), Some([40.0, 100.0]));
        // On the radius boundary still snaps.
        assert_eq!(a.snap_point([150.0, 108.0], 8.0), Some([150.0, 100.0]));
        // A 45° line projects diagonally.
        let d = StraightLineAssistant::new([0.0, 0.0], 45.0).expect("valid diagonal");
        let q = d.snap_point([10.0, 0.0], 8.0).expect("snaps");
        assert!((q[0] - 5.0).abs() < 1e-9 && (q[1] - 5.0).abs() < 1e-9, "{q:?}");
    }

    #[test]
    fn test_straight_line_ignores_points_outside_radius() {
        let a = straight();
        assert_eq!(a.snap_point([150.0, 108.1], 8.0), None);
        assert_eq!(a.snap_point([150.0, 300.0], 8.0), None);
        // Zero radius only takes exact hits.
        assert_eq!(a.snap_point([150.0, 100.0], 0.0), Some([150.0, 100.0]));
        assert_eq!(a.snap_point([150.0, 100.1], 0.0), None);
    }

    #[test]
    fn test_perspective_snapping_to_nearest_vanishing_ray() {
        // Two vanishing points on the horizon y = 200, with a horizontal ray each.
        let p = PerspectiveAssistant::new(
            vec![[100.0, 200.0], [700.0, 200.0]],
            Some(200.0),
            vec![PerspectiveRay { vp: 0, angle_deg: 0.0 }, PerspectiveRay { vp: 1, angle_deg: 0.0 }],
        )
        .expect("valid perspective");
        // Near the left VP's ray: snaps onto y = 200.
        assert_eq!(p.snap_point([150.0, 203.0], 8.0), Some([150.0, 200.0]));
        // Between the VPs but closer to the right ray's line: still y = 200 either way, and the
        // projection keeps x.
        assert_eq!(p.snap_point([650.0, 196.0], 8.0), Some([650.0, 200.0]));
        // A diagonal ray wins over the horizon when nearer: ray from VP0 at 45° through (100,200).
        let p = PerspectiveAssistant::new(
            vec![[100.0, 200.0]],
            Some(200.0),
            vec![PerspectiveRay { vp: 0, angle_deg: 45.0 }, PerspectiveRay { vp: 0, angle_deg: 0.0 }],
        )
        .expect("valid perspective");
        // (130, 225) is ~3.5 px from the 45° ray, 25 px from the horizon.
        let q = p.snap_point([130.0, 225.0], 8.0).expect("snaps to the diagonal");
        assert!((q[0] - 127.5).abs() < 1e-9 && (q[1] - 227.5).abs() < 1e-6, "{q:?}");
        // (400, 206) is 6 px from the horizon, far from any ray: horizon wins.
        assert_eq!(p.snap_point([400.0, 206.0], 8.0), Some([400.0, 200.0]));
    }

    #[test]
    fn test_zero_length_line_and_collinear_edge_cases() {
        // Non-finite angles never validate, and loaded garbage draws/snaps nothing.
        assert!(StraightLineAssistant::new([0.0, 0.0], f64::NAN).is_none());
        assert!(StraightLineAssistant::new([0.0, 0.0], f64::INFINITY).is_none());
        assert!(StraightLineAssistant::new([f64::NAN, 0.0], 0.0).is_none());
        assert!(StraightLineAssistant::new([MAX_COORDINATE + 1.0, 0.0], 0.0).is_none());
        let bad = StraightLineAssistant { anchor: [f64::NAN, 0.0], angle_deg: 0.0, length: 0.0 };
        assert_eq!(bad.snap_point([10.0, 0.0], 8.0), None);
        assert!(bad.overlay_primitives([0.0, 0.0, 100.0, 100.0]).is_empty());
        let bad_angle = StraightLineAssistant { anchor: [0.0, 0.0], angle_deg: f64::NAN, length: 0.0 };
        assert_eq!(bad_angle.snap_point([10.0, 0.0], 8.0), None);
        // Non-finite points and radii never snap.
        let a = straight();
        assert_eq!(a.snap_point([f64::NAN, 100.0], 8.0), None);
        assert_eq!(a.snap_point([150.0, 104.0], f64::NAN), None);
        assert_eq!(a.snap_point([150.0, 104.0], -1.0), None);
        assert_eq!(a.snap_point([150.0, 104.0], f64::INFINITY), None);
        // A point collinear with the anchor but far along the line still projects onto it.
        assert_eq!(a.snap_point([10_000.0, 100.0], 8.0), Some([10_000.0, 100.0]));
        // Perspective edge cases: no VPs, too many VPs, dangling ray indices.
        assert!(PerspectiveAssistant::new(vec![], None, vec![]).is_none());
        assert!(PerspectiveAssistant::new(vec![[0.0, 0.0]; MAX_VANISHING_POINTS + 1], None, vec![]).is_none());
        assert!(PerspectiveAssistant::new(vec![[0.0, 0.0]], None, vec![PerspectiveRay { vp: 7, angle_deg: 0.0 }]).is_none());
        let dangling = PerspectiveAssistant { vps: vec![[50.0, 50.0]], horizon_y: None, rays: vec![PerspectiveRay { vp: 3, angle_deg: 0.0 }], length: 0.0 };
        assert_eq!(dangling.snap_point([50.0, 53.0], 8.0), None);
        // The dangling ray draws no segment, but the valid vanishing point keeps its handle.
        assert!(dangling.overlay_primitives([0.0, 0.0, 100.0, 100.0]).iter().all(|p| matches!(p, GuidePrimitive::Handle { .. })));
        // Empty perspective (valid shape, no guides) snaps to nothing.
        let empty = PerspectiveAssistant { vps: vec![[50.0, 50.0]], horizon_y: None, rays: vec![], length: 0.0 };
        assert_eq!(empty.snap_point([50.0, 53.0], 8.0), None);
    }

    #[test]
    fn default_rays_cover_the_compass_and_horizon() {
        let p = PerspectiveAssistant::with_default_rays(vec![[400.0, 300.0]], None, 90.0).expect("1-point guide");
        assert_eq!(p.horizon_y, Some(300.0), "horizon defaults through the vanishing point");
        assert_eq!(p.rays.len(), 2, "0° and 90° rays");
        assert!(PerspectiveAssistant::with_default_rays(vec![[0.0, 0.0], [800.0, 0.0]], None, 45.0).expect("2-point").rays.len() == 8);
        assert!(PerspectiveAssistant::with_default_rays(vec![[0.0, 0.0]], None, 0.0).is_none(), "zero step rejected");
        assert!(PerspectiveAssistant::with_default_rays(vec![[0.0, 0.0]], None, f64::NAN).is_none());
        assert!(PerspectiveAssistant::with_default_rays(vec![], None, 15.0).is_none(), "no VP rejected");
        assert!(PerspectiveAssistant::with_default_rays(vec![[f64::INFINITY, 0.0]], None, 15.0).is_none());
    }

    #[test]
    fn set_snaps_to_nearest_visible_assistant() {
        let mut set = DrawingAssistants::default();
        let h = DrawingAssistantItem::new("h".into(), DrawingAssistantKind::StraightLine(StraightLineAssistant::new([0.0, 100.0], 0.0).expect("h")));
        let v = DrawingAssistantItem::new("v".into(), DrawingAssistantKind::StraightLine(StraightLineAssistant::new([100.0, 0.0], 90.0).expect("v")));
        let (hid, vid) = (h.id, v.id);
        set.items.extend([h, v]);
        // (103, 103): 3 px from each line; document order wins the tie.
        let hit = set.snap_point([103.0, 103.0]).expect("snaps");
        assert_eq!(hit.assistant, hid);
        assert_eq!(hit.point, [103.0, 100.0]);
        // Hiding the horizontal leaves the vertical.
        set.get_mut(hid).expect("exists").visible = false;
        let hit = set.snap_point([103.0, 103.0]).expect("snaps");
        assert_eq!((hit.assistant, hit.point), (vid, [100.0, 103.0]));
        // Disabled set, or nothing close, snaps nothing.
        set.enabled = false;
        assert_eq!(set.snap_point([103.0, 103.0]), None);
        set.enabled = true;
        assert_eq!(set.snap_point([500.0, 500.0]), None);
        // Zoom-scaled radius: a 2 px document radius misses, 8 px hits.
        assert_eq!(set.snap_point_with_radius([103.0, 103.0], 2.0), None);
        assert_eq!(set.snap_point_with_radius([103.0, 103.0], 8.0).map(|h| h.point), Some([100.0, 103.0]));
    }

    #[test]
    fn non_snapping_assistant_guides_but_never_snaps() {
        let mut set = DrawingAssistants::default();
        let mut h = DrawingAssistantItem::new("h".into(), DrawingAssistantKind::StraightLine(StraightLineAssistant::new([0.0, 100.0], 0.0).expect("h")));
        h.snapping = false;
        set.items.push(h);
        assert_eq!(set.snap_point([50.0, 104.0]), None, "visible but not snapping");
        assert!(!set.overlay_primitives([0.0, 0.0, 400.0, 300.0]).is_empty(), "still drawn (dimmed by the UI)");
        set.items[0].snapping = true;
        assert_eq!(set.snap_point([50.0, 104.0]).map(|h| h.point), Some([50.0, 100.0]));
    }

    #[test]
    fn overlay_marks_major_and_minor_segments() {
        let a = straight();
        let prims = a.overlay_primitives([0.0, 0.0, 400.0, 300.0]);
        assert!(prims.iter().any(|p| matches!(p, GuidePrimitive::Segment { major: true, .. })), "the line itself is major");
        let p = PerspectiveAssistant::with_default_rays(vec![[200.0, 150.0]], None, 90.0).expect("guide");
        let prims = p.overlay_primitives([0.0, 0.0, 400.0, 300.0]);
        assert!(prims.iter().any(|p| matches!(p, GuidePrimitive::Segment { major: true, .. })), "the horizon is major");
        assert!(prims.iter().any(|p| matches!(p, GuidePrimitive::Segment { major: false, .. })), "the ray fan is minor");
    }

    #[test]
    fn overlay_clips_to_viewport() {
        let a = straight();
        let prims = a.overlay_primitives([0.0, 0.0, 400.0, 300.0]);
        let seg = prims
            .iter()
            .find_map(|p| match *p {
                GuidePrimitive::Segment { a, b, .. } => Some((a, b)),
                _ => None,
            })
            .expect("a segment");
        assert_eq!(seg, ([0.0, 100.0], [400.0, 100.0]));
        // A line missing the viewport draws no segment, but its handles remain.
        let far = StraightLineAssistant::new([0.0, 5000.0], 0.0).expect("valid");
        let prims = far.overlay_primitives([0.0, 0.0, 400.0, 300.0]);
        assert!(prims.iter().all(|p| matches!(p, GuidePrimitive::Handle { .. })));
        // Degenerate viewports draw nothing.
        assert!(a.overlay_primitives([10.0, 10.0, 10.0, 50.0]).iter().all(|p| matches!(p, GuidePrimitive::Handle { .. })));
        assert!(a.overlay_primitives([f64::NAN, 0.0, 400.0, 300.0]).iter().all(|p| matches!(p, GuidePrimitive::Handle { .. })));
        // Perspective: rays leave the VP towards the viewport, VPs get handles.
        let p = PerspectiveAssistant::with_default_rays(vec![[200.0, 150.0]], None, 90.0).expect("guide");
        let prims = p.overlay_primitives([0.0, 0.0, 400.0, 300.0]);
        assert!(prims.iter().any(|p| matches!(p, GuidePrimitive::Segment { .. })));
        assert!(prims.iter().any(|p| matches!(p, GuidePrimitive::Handle { kind: HandleKind::VanishingPoint(0), .. })));
        // A disabled set draws nothing at all.
        let mut set = DrawingAssistants::default();
        set.items.push(DrawingAssistantItem::new("h".into(), DrawingAssistantKind::StraightLine(a)));
        assert!(!set.overlay_primitives([0.0, 0.0, 400.0, 300.0]).is_empty());
        set.enabled = false;
        assert!(set.overlay_primitives([0.0, 0.0, 400.0, 300.0]).is_empty());
    }

    #[test]
    fn assistants_serialize_stably() {
        let mut set = DrawingAssistants::default();
        set.items
            .push(DrawingAssistantItem::new("iso".into(), DrawingAssistantKind::StraightLine(StraightLineAssistant::new([10.0, 20.0], 30.0).expect("valid"))));
        set.items.push(DrawingAssistantItem::new(
            "2pt".into(),
            DrawingAssistantKind::Perspective(PerspectiveAssistant::with_default_rays(vec![[0.0, 300.0], [800.0, 300.0]], None, 45.0).expect("valid")),
        ));
        let json = serde_json::to_value(&set).expect("serializes");
        let back: DrawingAssistants = serde_json::from_value(json).expect("round-trips");
        assert_eq!(back, set);
        // Older files without the field load the default.
        let old = serde_json::json!({"enabled": true, "snap_radius": 8.0, "items": []});
        assert_eq!(serde_json::from_value::<DrawingAssistants>(old).expect("loads"), DrawingAssistants::default());
        // Assistant ids are unique.
        assert_ne!(set.items[0].id, set.items[1].id);
    }
}
