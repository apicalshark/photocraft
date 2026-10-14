//! Ellipse ruler guide: snap a pointer to the nearest point on an ellipse.
//!
//! Pure geometry for slice (a) of the ellipse ruler (see
//! `docs/stroke-lab-todo.md` P2). Param names mirror the shipped preset tags
//! (`ellipseDegree`, `ellipseRotation`, ...); the maths is original: a Newton
//! solve on the eccentric angle, seeded by the radial direction.
//!
//! Coordinates are document pixels, y-up math convention; rotation is
//! counter-clockwise degrees (like brush angles). The canvas overlay converts
//! at its boundary. Everything is stateless per point (no chunking concerns)
//! and deterministic.

use alloc::vec::Vec;

/// Ellipse guide: `center`, semi-axes `radii` (`rx`, `ry`), `rotation` and an
/// optional `arc` clamp in parametric radians (`None` = whole ellipse).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EllipseGuide {
    pub center: [f64; 2],
    pub radii: [f64; 2],
    pub rotation: f64,
    pub arc: Option<(f64, f64)>,
}

/// Newton iterations from the radial seed (more than enough for pixels).
const ITERATIONS: usize = 8;

impl EllipseGuide {
    /// `None` for degenerate guides (non-positive/non-finite axes or center).
    pub fn new(center: [f64; 2], radii: [f64; 2], rotation: f64, arc: Option<(f64, f64)>) -> Option<Self> {
        if !center.iter().all(|v| v.is_finite()) || !radii.iter().all(|v| v.is_finite() && *v > 0.0) {
            return None;
        }
        if !rotation.is_finite() {
            return None;
        }
        if let Some((a, b)) = arc
            && (!a.is_finite() || !b.is_finite())
        {
            return None;
        }
        Some(Self { center, radii, rotation, arc })
    }

    /// Nearest guide point to `(x, y)` in document pixels, or `None` for
    /// non-finite input.
    pub fn snap(&self, x: f64, y: f64) -> Option<[f64; 2]> {
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        let (rx, ry) = (self.radii[0], self.radii[1]);
        let phi = self.rotation.to_radians();
        let (sin, cos) = phi.sin_cos();
        // To the ellipse frame: translate, un-rotate.
        let (dx, dy) = (x - self.center[0], y - self.center[1]);
        let q = [dx * cos + dy * sin, -dx * sin + dy * cos];
        // Radial seed; the center maps to angle 0 (any ring point is nearest).
        let mut t = q[1].atan2(q[0]);
        if !t.is_finite() {
            t = 0.0;
        }
        for _ in 0..ITERATIONS {
            let (s, c) = t.sin_cos();
            // E(t), E'(t), E''(t) with the axes folded in.
            let ex = rx * c;
            let ey = ry * s;
            let dx1 = -rx * s;
            let dy1 = ry * c;
            let f1 = (ex - q[0]) * dx1 + (ey - q[1]) * dy1;
            let f2 = dx1 * dx1 + dy1 * dy1 + (ex - q[0]) * -ex + (ey - q[1]) * -ey;
            if f2.abs() < 1e-12 {
                break;
            }
            t -= f1 / f2;
            if !t.is_finite() {
                t = q[1].atan2(q[0]);
                break;
            }
        }
        // Arc clamp: outside the span, the nearer endpoint wins. A span whose
        // end wraps below its start covers both sides of angle 0.
        if let Some((a, b)) = self.arc {
            let two_pi = core::f64::consts::TAU;
            let wrap = |v: f64| v.rem_euclid(two_pi);
            let (lo, hi) = (wrap(a), wrap(b));
            let w = wrap(t);
            let inside = if hi < lo { w >= lo || w <= hi } else { w >= lo && w <= hi };
            if !inside {
                let ring = |u: f64| {
                    let d = (u - w).abs();
                    d.min(two_pi - d)
                };
                t = if ring(lo) <= ring(hi) { lo } else { hi };
            }
        }
        let (s, c) = t.sin_cos();
        let local = [rx * c, ry * s];
        // Back to document pixels.
        let wx = self.center[0] + local[0] * cos - local[1] * sin;
        let wy = self.center[1] + local[0] * sin + local[1] * cos;
        (wx.is_finite() && wy.is_finite()).then_some([wx, wy])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guide() -> EllipseGuide {
        EllipseGuide::new([10.0, 20.0], [5.0, 5.0], 0.0, None).expect("valid")
    }

    fn near(a: [f64; 2], b: [f64; 2]) -> bool {
        (a[0] - b[0]).abs() < 1e-6 && (a[1] - b[1]).abs() < 1e-6
    }

    #[test]
    fn circle_projects_radially() {
        let g = guide();
        assert!(near(g.snap(16.0, 28.0).expect("snap"), [13.0, 24.0]));
        // On the ring: identity.
        assert!(near(g.snap(13.0, 24.0).expect("snap"), [13.0, 24.0]));
        // Axis point outside.
        assert!(near(g.snap(20.0, 20.0).expect("snap"), [15.0, 20.0]));
    }

    #[test]
    fn rotation_turns_the_axes() {
        let g = EllipseGuide::new([0.0, 0.0], [2.0, 1.0], 90.0, None).expect("valid");
        assert!(near(g.snap(0.0, 5.0).expect("snap"), [0.0, 2.0]));
        assert!(near(g.snap(5.0, 0.0).expect("snap"), [1.0, 0.0]));
    }

    #[test]
    fn arc_clamps_to_the_nearer_end() {
        let g = EllipseGuide::new([0.0, 0.0], [4.0, 3.0], 0.0, Some((0.0, core::f64::consts::FRAC_PI_2))).expect("valid");
        // Inside the span: the ring point itself.
        assert!(near(g.snap(4.0, 0.0).expect("snap"), [4.0, 0.0]));
        // (-4, 0) is outside: nearer to (0, 3) than to (4, 0).
        assert!(near(g.snap(-4.0, 0.0).expect("snap"), [0.0, 3.0]));
    }

    #[test]
    fn degenerate_and_hostile_guides_refuse() {
        assert!(EllipseGuide::new([0.0, 0.0], [0.0, 1.0], 0.0, None).is_none());
        assert!(EllipseGuide::new([0.0, 0.0], [-2.0, 1.0], 0.0, None).is_none());
        assert!(EllipseGuide::new([f64::NAN, 0.0], [2.0, 1.0], 0.0, None).is_none());
        assert!(EllipseGuide::new([0.0, 0.0], [2.0, 1.0], f64::INFINITY, None).is_none());
        assert!(guide().snap(f64::NAN, 0.0).is_none());
        assert!(guide().snap(0.0, f64::INFINITY).is_none());
    }

    #[test]
    fn snap_is_deterministic() {
        let g = EllipseGuide::new([3.0, -7.0], [9.0, 4.0], 37.0, None).expect("valid");
        assert_eq!(g.snap(11.0, 2.0), g.snap(11.0, 2.0));
    }
}

// ---------------------------------------------------------------------------
// Line families (P2-2): parallel direction + tick spacing, radial spokes with
// angular ticks and quadratic bend, multi-VP bundles, fisheye line modes,
// isometric three-axis lattice, connected segment chains. Stateless per point;
// angles are counter-clockwise degrees in the y-up document convention.

/// Parallel ticked lines: direction `angle`, lines through `origin` spaced
/// `spacing` apart along the normal (the reference's tick spacing).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParallelGuide {
    pub origin: [f64; 2],
    pub angle: f64,
    pub spacing: f64,
}

impl ParallelGuide {
    pub fn new(origin: [f64; 2], angle: f64, spacing: f64) -> Option<Self> {
        if !origin.iter().all(|v| v.is_finite()) || !angle.is_finite() || !(spacing.is_finite() && spacing > 0.0) {
            return None;
        }
        Some(Self { origin, angle, spacing })
    }

    pub fn snap(&self, x: f64, y: f64) -> Option<[f64; 2]> {
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        let (sin, cos) = self.angle.to_radians().sin_cos();
        let (dx, dy) = (x - self.origin[0], y - self.origin[1]);
        let along = dx * cos + dy * sin;
        let across = dx * -sin + dy * cos;
        let k = (across / self.spacing).round();
        Some([self.origin[0] + along * cos - k * self.spacing * sin, self.origin[1] + along * sin + k * self.spacing * cos])
    }
}

/// Radial spokes: angles snap to `tick` multiples around `center`, radius is
/// free; `curve` bends spokes quadratically (perpendicular offset
/// `curve * r^2 / 1000`, 0 = straight, the only shipped value).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RadialGuide {
    pub center: [f64; 2],
    pub tick: f64,
    pub curve: f64,
}

impl RadialGuide {
    pub fn new(center: [f64; 2], tick: f64, curve: f64) -> Option<Self> {
        if !center.iter().all(|v| v.is_finite()) || !(tick.is_finite() && tick > 0.0) || !curve.is_finite() {
            return None;
        }
        Some(Self { center, tick, curve })
    }

    pub fn snap(&self, x: f64, y: f64) -> Option<[f64; 2]> {
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        let (dx, dy) = (x - self.center[0], y - self.center[1]);
        let r = dx.hypot(dy);
        if r <= 0.0 {
            return Some(self.center);
        }
        let tick_rad = self.tick.to_radians();
        let t = (dy.atan2(dx) / tick_rad).round() * tick_rad;
        let (s, c) = t.sin_cos();
        // Perpendicular bend (y-up frame).
        let o = self.curve * r * r / 1000.0;
        let out = [self.center[0] + r * c - o * s, self.center[1] + r * s + o * c];
        (out[0].is_finite() && out[1].is_finite()).then_some(out)
    }
}

/// Vanishing/perspective bundle: one to six vanishing points sharing one
/// angular tick; each pointer snaps to the nearest ticked spoke of the
/// nearest VP (two VPs = two-point perspective).
#[derive(Clone, Debug, PartialEq)]
pub struct BundleGuide {
    pub vps: Vec<[f64; 2]>,
    pub tick: f64,
}

impl BundleGuide {
    pub fn new(vps: Vec<[f64; 2]>, tick: f64) -> Option<Self> {
        if vps.is_empty() || vps.len() > 6 || !vps.iter().all(|p| p.iter().all(|v| v.is_finite())) {
            return None;
        }
        if !(tick.is_finite() && tick > 0.0) {
            return None;
        }
        Some(Self { vps, tick })
    }

    pub fn snap(&self, x: f64, y: f64) -> Option<[f64; 2]> {
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        let mut best: Option<([f64; 2], f64)> = None;
        for vp in &self.vps {
            let g = RadialGuide { center: *vp, tick: self.tick, curve: 0.0 };
            if let Some(q) = g.snap(x, y) {
                let d = (q[0] - x).hypot(q[1] - y);
                if best.is_none_or(|(_, bd)| d < bd) {
                    best = Some((q, d));
                }
            }
        }
        best.map(|(q, _)| q)
    }
}

/// Fisheye's three line modes (fixed 180° view): horizontal / vertical guide
/// through the center, or ticked radial spokes from it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FisheyeMode {
    Horizontal,
    Vertical,
    Radial,
}

/// Fisheye line guide (radial mode ticks at 15°, host-defined).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FisheyeGuide {
    pub center: [f64; 2],
    pub mode: FisheyeMode,
}

impl FisheyeGuide {
    pub fn new(center: [f64; 2], mode: FisheyeMode) -> Option<Self> {
        if !center.iter().all(|v| v.is_finite()) {
            return None;
        }
        Some(Self { center, mode })
    }

    pub fn snap(&self, x: f64, y: f64) -> Option<[f64; 2]> {
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        match self.mode {
            FisheyeMode::Horizontal => Some([x, self.center[1]]),
            FisheyeMode::Vertical => Some([self.center[0], y]),
            FisheyeMode::Radial => {
                let g = RadialGuide { center: self.center, tick: 15.0, curve: 0.0 };
                g.snap(x, y)
            }
        }
    }
}

/// Isometric three-axis lattice: axis `angles` (defaults 30/-30/90) through
/// `origin`, lines spaced `spacing` apart; snaps to the nearest axis line.
#[derive(Clone, Debug, PartialEq)]
pub struct IsoGuide {
    pub origin: [f64; 2],
    pub angles: [f64; 3],
    pub spacing: f64,
}

impl IsoGuide {
    pub fn new(origin: [f64; 2], angles: [f64; 3], spacing: f64) -> Option<Self> {
        if !origin.iter().all(|v| v.is_finite())
            || !angles.iter().all(|v| v.is_finite())
            || !(spacing.is_finite() && spacing > 0.0)
        {
            return None;
        }
        Some(Self { origin, angles, spacing })
    }

    pub fn snap(&self, x: f64, y: f64) -> Option<[f64; 2]> {
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        let mut best: Option<([f64; 2], f64)> = None;
        for a in self.angles {
            let g = ParallelGuide { origin: self.origin, angle: a, spacing: self.spacing };
            if let Some(q) = g.snap(x, y) {
                let d = (q[0] - x).hypot(q[1] - y);
                if best.is_none_or(|(_, bd)| d < bd) {
                    best = Some((q, d));
                }
            }
        }
        best.map(|(q, _)| q)
    }
}

/// Connected segment chain: snaps to the nearest clamped segment (clamping is
/// the anti-overshoot stop at junctions).
#[derive(Clone, Debug, PartialEq)]
pub struct PolyGuide {
    pub points: Vec<[f64; 2]>,
}

impl PolyGuide {
    pub fn new(points: Vec<[f64; 2]>) -> Option<Self> {
        if points.len() < 2 || !points.iter().all(|p| p.iter().all(|v| v.is_finite())) {
            return None;
        }
        Some(Self { points })
    }

    pub fn snap(&self, x: f64, y: f64) -> Option<[f64; 2]> {
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        let mut best: Option<([f64; 2], f64)> = None;
        for w in self.points.windows(2) {
            let (a, b) = (w[0], w[1]);
            let (abx, aby) = (b[0] - a[0], b[1] - a[1]);
            let len2 = abx * abx + aby * aby;
            if len2 <= 0.0 {
                continue;
            }
            let t = (((x - a[0]) * abx + (y - a[1]) * aby) / len2).clamp(0.0, 1.0);
            let q = [a[0] + abx * t, a[1] + aby * t];
            let d = (q[0] - x).hypot(q[1] - y);
            if best.is_none_or(|(_, bd)| d < bd) {
                best = Some((q, d));
            }
        }
        best.map(|(q, _)| q)
    }
}

#[cfg(test)]
mod line_tests {
    use super::*;

    fn near(a: [f64; 2], b: [f64; 2]) -> bool {
        (a[0] - b[0]).abs() < 1e-6 && (a[1] - b[1]).abs() < 1e-6
    }

    #[test]
    fn parallel_snaps_to_tick_lines() {
        let g = ParallelGuide::new([0.0, 0.0], 0.0, 10.0).expect("valid");
        assert!(near(g.snap(3.0, 14.0).expect("snap"), [3.0, 10.0]));
        assert!(near(g.snap(3.0, 16.0).expect("snap"), [3.0, 20.0]));
        // 45° keeps the along-component, snaps across to the nearest tick line.
        let g = ParallelGuide::new([0.0, 0.0], 45.0, 10.0).expect("valid");
        let q = g.snap(10.0, 0.0).expect("snap");
        let (sin, cos) = 45.0_f64.to_radians().sin_cos();
        let along = q[0] * cos + q[1] * sin;
        let across = q[0] * -sin + q[1] * cos;
        assert!((along - 10.0 * cos).abs() < 1e-6 && (across + 10.0).abs() < 1e-6, "{q:?}");
        assert!(ParallelGuide::new([0.0, 0.0], 0.0, 0.0).is_none());
        assert!(g.snap(f64::NAN, 0.0).is_none());
    }

    #[test]
    fn radial_ticks_angles_and_bends() {
        let g = RadialGuide::new([0.0, 0.0], 90.0, 0.0).expect("valid");
        // 80° at radius 9 snaps to the 90° spoke, radius kept.
        let a = 80.0_f64.to_radians();
        let q = g.snap(9.0 * a.cos(), 9.0 * a.sin()).expect("snap");
        assert!(q[0].abs() < 1e-6 && (q[1] - 9.0).abs() < 1e-6, "{q:?}");
        // Center maps to itself.
        assert_eq!(g.snap(0.0, 0.0), Some([0.0, 0.0]));
        // Curve bends (documented formula).
        let g = RadialGuide::new([0.0, 0.0], 360.0, 10.0).expect("valid");
        let q = g.snap(100.0, 0.0).expect("snap");
        assert!((q[0] - 100.0).abs() < 1e-6 && (q[1] - 100.0).abs() < 1e-6, "{q:?}");
        assert!(RadialGuide::new([0.0, 0.0], 0.0, 0.0).is_none());
    }

    #[test]
    fn bundle_picks_the_nearer_vp() {
        let g = BundleGuide::new(vec![[0.0, 0.0], [100.0, 0.0]], 90.0).expect("valid");
        // Near the right VP: the vertical spoke through it, radius kept.
        let q = g.snap(102.0, 30.0).expect("snap");
        assert!(near(q, [100.0, 904.0_f64.sqrt()]), "{q:?}");
        assert!(BundleGuide::new(vec![], 90.0).is_none());
        assert!(BundleGuide::new(vec![[0.0, 0.0]; 7], 90.0).is_none());
    }

    #[test]
    fn fisheye_modes() {
        let h = FisheyeGuide::new([5.0, 5.0], FisheyeMode::Horizontal).expect("valid");
        assert_eq!(h.snap(99.0, -99.0), Some([99.0, 5.0]));
        let v = FisheyeGuide::new([5.0, 5.0], FisheyeMode::Vertical).expect("valid");
        assert_eq!(v.snap(99.0, -99.0), Some([5.0, -99.0]));
        let r = FisheyeGuide::new([0.0, 0.0], FisheyeMode::Radial).expect("valid");
        let q = r.snap(10.0, 0.0).expect("snap");
        assert!(near(q, [10.0, 0.0]), "{q:?}");
    }

    #[test]
    fn iso_snaps_to_nearest_axis_line() {
        let g = IsoGuide::new([0.0, 0.0], [30.0, -30.0, 90.0], 10.0).expect("valid");
        // Near the vertical lattice line x = 10 (nearer than any 30° line).
        assert!(near(g.snap(9.5, 100.0).expect("snap"), [10.0, 100.0]));
        // Sane and resting near the origin (ulp-tolerant).
        let q = g.snap(0.5, 0.5).expect("snap");
        assert!(q[0].is_finite() && q[1].is_finite());
        let q2 = g.snap(q[0], q[1]).expect("snap");
        assert!(near(q, q2), "snapped points rest");
    }

    #[test]
    fn poly_clamps_to_segments() {
        let g = PolyGuide::new(vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]]).expect("valid");
        assert!(near(g.snap(5.0, 3.0).expect("snap"), [5.0, 0.0]));
        // Past the end: clamped (anti-overshoot), not projected further.
        assert!(near(g.snap(10.0, 99.0).expect("snap"), [10.0, 10.0]));
        assert!(PolyGuide::new(vec![[0.0, 0.0]]).is_none());
    }
}

// ---------------------------------------------------------------------------
// Curves (P2-4): cubic/quadratic Bezier closest-point by subdivision, golden
// log-spiral snap by Newton in theta. Stateless per point.

// de Casteljau split of a cubic at t = 0.5.
fn split_cubic(p: &[[f64; 2]; 4]) -> ([[f64; 2]; 4], [[f64; 2]; 4]) {
    let mid = |a: [f64; 2], b: [f64; 2]| [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0];
    let q0 = mid(p[0], p[1]);
    let q1 = mid(p[1], p[2]);
    let q2 = mid(p[2], p[3]);
    let r0 = mid(q0, q1);
    let r1 = mid(q1, q2);
    let s = mid(r0, r1);
    ([p[0], q0, r0, s], [s, r1, q2, p[3]])
}

/// Flatness: max distance of the inner controls from the end chord.
fn flatness(p: &[[f64; 2]; 4]) -> f64 {
    let (ax, ay) = (p[3][0] - p[0][0], p[3][1] - p[0][1]);
    let len = ax.hypot(ay).max(1e-12);
    [(p[1][0] - p[0][0]) * ay - (p[1][1] - p[0][1]) * ax, (p[2][0] - p[0][0]) * ay - (p[2][1] - p[0][1]) * ax]
        .iter()
        .map(|v| v.abs() / len)
        .fold(0.0, f64::max)
}

/// Closest point on a cubic: subdivide to near-straight leaves, keep the best
/// clamped segment projection. `tol` bounds the leaf deviation; `depth` caps
/// the recursion.
fn closest_cubic(p: &[[f64; 2]; 4], x: f64, y: f64, tol: f64, depth: u32) -> [f64; 2] {
    if depth == 0 || flatness(p) <= tol {
        let (ax, ay) = (p[3][0] - p[0][0], p[3][1] - p[0][1]);
        let len2 = ax * ax + ay * ay;
        if len2 <= 0.0 {
            return p[0];
        }
        let t = (((x - p[0][0]) * ax + (y - p[0][1]) * ay) / len2).clamp(0.0, 1.0);
        return [p[0][0] + ax * t, p[0][1] + ay * t];
    }
    let (l, r) = split_cubic(p);
    let a = closest_cubic(&l, x, y, tol, depth - 1);
    let b = closest_cubic(&r, x, y, tol, depth - 1);
    if (a[0] - x).hypot(a[1] - y) <= (b[0] - x).hypot(b[1] - y) { a } else { b }
}

/// Cubic Bezier ruler: snaps to the closest curve point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BezierGuide {
    pub points: [[f64; 2]; 4],
}

impl BezierGuide {
    pub fn new(points: [[f64; 2]; 4]) -> Option<Self> {
        if !points.iter().all(|p| p.iter().all(|v| v.is_finite())) {
            return None;
        }
        Some(Self { points })
    }

    pub fn snap(&self, x: f64, y: f64) -> Option<[f64; 2]> {
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        Some(closest_cubic(&self.points, x, y, 1e-6, 12))
    }
}

/// Quadratic Bezier ruler (degree-elevated to cubic internally).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct QuadGuide {
    pub points: [[f64; 2]; 3],
}

impl QuadGuide {
    pub fn new(points: [[f64; 2]; 3]) -> Option<Self> {
        if !points.iter().all(|p| p.iter().all(|v| v.is_finite())) {
            return None;
        }
        Some(Self { points })
    }

    pub fn snap(&self, x: f64, y: f64) -> Option<[f64; 2]> {
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        let (q0, q1, q2) = (self.points[0], self.points[1], self.points[2]);
        let elevated = [q0, [(q0[0] + 2.0 * q1[0]) / 3.0, (q0[1] + 2.0 * q1[1]) / 3.0], [(2.0 * q1[0] + q2[0]) / 3.0, (2.0 * q1[1] + q2[1]) / 3.0], q2];
        Some(closest_cubic(&elevated, x, y, 1e-6, 12))
    }
}

/// Golden log spiral: `r = a * e^(b*theta)` over `turns` turns from theta 0.
/// `b = ln(phi) / (pi/2)` is the golden spiral (a quarter turn grows by phi).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpiralGuide {
    pub center: [f64; 2],
    pub a: f64,
    pub b: f64,
    pub turns: f64,
}

impl SpiralGuide {
    pub fn new(center: [f64; 2], a: f64, b: f64, turns: f64) -> Option<Self> {
        if !center.iter().all(|v| v.is_finite()) || !(a.is_finite() && a > 0.0) || !b.is_finite() || !(turns.is_finite() && turns >= 0.5) || turns > 8.0 {
            return None;
        }
        Some(Self { center, a, b, turns })
    }

    fn point(&self, t: f64) -> [f64; 2] {
        let r = self.a * (self.b * t).exp();
        [self.center[0] + r * t.cos(), self.center[1] + r * t.sin()]
    }

    pub fn snap(&self, x: f64, y: f64) -> Option<[f64; 2]> {
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        let (dx, dy) = (x - self.center[0], y - self.center[1]);
        let max_t = self.turns * core::f64::consts::TAU;
        // Seed at the pointer's angle; Newton on |S(t) - q|^2.
        let mut t = dy.atan2(dx).rem_euclid(core::f64::consts::TAU);
        if !t.is_finite() {
            t = 0.0;
        }
        // Try the seed turn and its neighbors (multi-turn spirals revisit angles).
        let mut best_t = t.clamp(0.0, max_t);
        let mut best_d = f64::INFINITY;
        let base_turn = (t / core::f64::consts::TAU).floor() as i32;
        for k in base_turn - 1..=base_turn + 1 {
            let mut u = t + f64::from(k) * core::f64::consts::TAU;
            u = u.clamp(0.0, max_t);
            for _ in 0..16 {
                let r = self.a * (self.b * u).exp();
                if !r.is_finite() {
                    break;
                }
                let (s, c) = u.sin_cos();
                // S, S', S'' with r' = b*r, r'' = b^2*r.
                let ex = r * c;
                let ey = r * s;
                let dx1 = self.b * ex - r * s;
                let dy1 = self.b * ey + r * c;
                let rb = self.b * self.b * r;
                let dx2 = rb * c - 2.0 * self.b * r * s - ex;
                let dy2 = rb * s + 2.0 * self.b * r * c - ey;
                let f1 = (self.center[0] + ex - x) * dx1 + (self.center[1] + ey - y) * dy1;
                let f2 = dx1 * dx1 + dy1 * dy1 + (self.center[0] + ex - x) * dx2 + (self.center[1] + ey - y) * dy2;
                if f2.abs() < 1e-12 {
                    break;
                }
                u -= f1 / f2;
                if !u.is_finite() {
                    u = t;
                    break;
                }
                u = u.clamp(0.0, max_t);
            }
            let q = self.point(u);
            let d = (q[0] - x).hypot(q[1] - y);
            if d < best_d {
                best_d = d;
                best_t = u;
            }
        }
        let q = self.point(best_t);
        (q[0].is_finite() && q[1].is_finite()).then_some(q)
    }
}

#[cfg(test)]
mod curve_tests {
    use super::*;

    fn near(a: [f64; 2], b: [f64; 2]) -> bool {
        (a[0] - b[0]).abs() < 1e-4 && (a[1] - b[1]).abs() < 1e-4
    }

    #[test]
    fn cubic_snaps_to_curve() {
        // Straight cubic: snaps like a segment.
        let g = BezierGuide::new([[0.0, 0.0], [3.0, 0.0], [7.0, 0.0], [10.0, 0.0]]).expect("valid");
        assert!(near(g.snap(5.0, 3.0).expect("snap"), [5.0, 0.0]));
        // Symmetric hump: apex snaps down.
        let g = BezierGuide::new([[0.0, 0.0], [0.0, 10.0], [10.0, 10.0], [10.0, 0.0]]).expect("valid");
        let q = g.snap(5.0, 20.0).expect("snap");
        assert!((q[0] - 5.0).abs() < 1e-4 && q[1] > 7.0 && q[1] <= 7.51, "{q:?}");
        // Endpoints rest.
        assert!(near(g.snap(0.0, 0.0).expect("snap"), [0.0, 0.0]));
        assert!(g.snap(f64::NAN, 0.0).is_none());
    }

    #[test]
    fn quad_matches_elevated_cubic() {
        let q = QuadGuide::new([[0.0, 0.0], [5.0, 10.0], [10.0, 0.0]]).expect("valid");
        let c = BezierGuide::new([[0.0, 0.0], [(0.0 + 10.0) / 3.0, 20.0 / 3.0], [(10.0 + 10.0) / 3.0, 20.0 / 3.0], [10.0, 0.0]]).expect("valid");
        for (x, y) in [(5.0, 20.0), (2.0, 3.0), (9.0, 9.0)] {
            assert!(near(q.snap(x, y).expect("q"), c.snap(x, y).expect("c")));
        }
    }

    #[test]
    fn spiral_snaps_near_its_ring() {
        // Circle-like spiral (b = 0): radial projection at radius a.
        let g = SpiralGuide::new([0.0, 0.0], 5.0, 0.0, 2.0).expect("valid");
        assert!(near(g.snap(10.0, 0.0).expect("snap"), [5.0, 0.0]));
        // Golden growth: a point on turn 1 stays near it.
        let phi = (1.0 + 5.0_f64.sqrt()) / 2.0;
        let b = phi.ln() / core::f64::consts::FRAC_PI_2;
        let g = SpiralGuide::new([0.0, 0.0], 2.0, b, 3.0).expect("valid");
        let r1 = 2.0 * (b * core::f64::consts::TAU).exp();
        let q = g.snap(r1 + 1.0, 0.0).expect("snap");
        assert!((q[0] - r1).abs() < 0.5 && q[1].abs() < 0.5, "{q:?} r1={r1}");
        assert!(SpiralGuide::new([0.0, 0.0], 0.0, b, 3.0).is_none());
        assert!(SpiralGuide::new([0.0, 0.0], 2.0, b, 99.0).is_none());
    }
}
