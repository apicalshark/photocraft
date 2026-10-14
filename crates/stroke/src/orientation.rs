//! Tilt/rotation smoothing.
//!
//! The shipped presets carry `orientationSmoothingFactor` at 0 throughout
//! (feature off); this module is the increment for when a host wants it.
//! Tilt interpolates linearly; rotation interpolates along the shortest arc
//! so 350° -> 10° passes through 0° instead of swinging back through 180°.
//! Angles are in degrees.

/// Tilt/rotation pose in degrees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    /// Elevation angle.
    pub tilt: f32,
    /// Compass angle, normalized to 0..360 on output.
    pub rotation: f32,
}

/// Parameters. Field name mirrors the preset fact.
#[derive(Clone, Debug, PartialEq)]
pub struct OrientationConfig {
    pub enabled: bool,
    /// Amount 0..1 (0 follows the input directly).
    pub smoothing_factor: f32,
}

impl Default for OrientationConfig {
    /// Identity: passes poses through unchanged.
    fn default() -> Self {
        Self { enabled: true, smoothing_factor: 0.0 }
    }
}

/// Incremental tilt/rotation smoother.
#[derive(Clone, Debug)]
pub struct OrientationSmoother {
    cfg: OrientationConfig,
    state: Option<Pose>,
}

impl OrientationSmoother {
    pub fn new(cfg: OrientationConfig) -> Self {
        Self { cfg, state: None }
    }

    /// Push one pose sample. Non-finite components hold the last output
    /// (or a zeroed pose before the first sample) without poisoning state.
    pub fn push(&mut self, p: Pose) -> Pose {
        if !self.cfg.enabled {
            return sanitize(p);
        }
        if !p.tilt.is_finite() || !p.rotation.is_finite() {
            return self.state.unwrap_or(Pose { tilt: 0.0, rotation: 0.0 });
        }
        let next = match self.state {
            None => Pose { tilt: p.tilt, rotation: norm360(p.rotation) },
            Some(s) => {
                let a = 1.0 - self.cfg.smoothing_factor.clamp(0.0, 0.95);
                Pose {
                    tilt: s.tilt + (p.tilt - s.tilt) * a,
                    rotation: norm360(s.rotation + short_arc(s.rotation, p.rotation) * a),
                }
            }
        };
        self.state = Some(next);
        next
    }
}

fn sanitize(p: Pose) -> Pose {
    Pose {
        tilt: if p.tilt.is_finite() { p.tilt } else { 0.0 },
        rotation: if p.rotation.is_finite() { norm360(p.rotation) } else { 0.0 },
    }
}

/// Normalize degrees to 0..360.
pub fn norm360(d: f32) -> f32 {
    let mut r = d % 360.0;
    if r < 0.0 {
        r += 360.0;
    }
    // `%` can return -0.0; normalize so outputs compare cleanly.
    r + 0.0
}

/// Signed shortest arc from `from` to `to`, in -180..180 degrees.
pub fn short_arc(from: f32, to: f32) -> f32 {
    let mut d = (to - from) % 360.0;
    if d > 180.0 {
        d -= 360.0;
    } else if d < -180.0 {
        d += 360.0;
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn default_passes_through() {
        let mut s = OrientationSmoother::new(OrientationConfig::default());
        let out = s.push(Pose { tilt: 12.0, rotation: 370.0 });
        assert!(near(out.tilt, 12.0) && near(out.rotation, 10.0));
    }

    #[test]
    fn disabled_returns_sanitized_input() {
        let mut s = OrientationSmoother::new(OrientationConfig { enabled: false, smoothing_factor: 0.9 });
        let out = s.push(Pose { tilt: 5.0, rotation: -90.0 });
        assert!(near(out.tilt, 5.0) && near(out.rotation, 270.0));
    }

    #[test]
    fn smoothing_converges_on_tilt() {
        let mut s = OrientationSmoother::new(OrientationConfig { enabled: true, smoothing_factor: 0.5 });
        s.push(Pose { tilt: 0.0, rotation: 0.0 });
        let out = s.push(Pose { tilt: 10.0, rotation: 0.0 });
        assert!(near(out.tilt, 5.0), "got {out:?}");
        let out = s.push(Pose { tilt: 10.0, rotation: 0.0 });
        assert!(near(out.tilt, 7.5), "got {out:?}");
    }

    #[test]
    fn rotation_takes_short_arc() {
        let mut s = OrientationSmoother::new(OrientationConfig { enabled: true, smoothing_factor: 0.5 });
        s.push(Pose { tilt: 0.0, rotation: 350.0 });
        // 350 -> 10 is +20 along the short arc; half step lands on 0.
        let out = s.push(Pose { tilt: 0.0, rotation: 10.0 });
        assert!(near(out.rotation, 0.0) || near(out.rotation, 360.0), "got {out:?}");
        assert!(short_arc(350.0, 10.0) > 0.0, "arc goes forward through 0");
        assert!(near(short_arc(10.0, 350.0), -20.0));
    }

    #[test]
    fn non_finite_holds_without_poisoning() {
        let mut s = OrientationSmoother::new(OrientationConfig { enabled: true, smoothing_factor: 0.5 });
        // Before any sample: zeroed pose.
        let out = s.push(Pose { tilt: f32::NAN, rotation: 0.0 });
        assert_eq!(out, Pose { tilt: 0.0, rotation: 0.0 });
        s.push(Pose { tilt: 8.0, rotation: 90.0 });
        let out = s.push(Pose { tilt: 8.0, rotation: f32::INFINITY });
        assert!(near(out.tilt, 8.0) && near(out.rotation, 90.0), "got {out:?}");
        // State still alive afterwards.
        let out = s.push(Pose { tilt: 8.0, rotation: 90.0 });
        assert!(near(out.tilt, 8.0) && near(out.rotation, 90.0));
    }

    #[test]
    fn batch_equals_incremental() {
        let inputs = [
            Pose { tilt: 0.0, rotation: 350.0 },
            Pose { tilt: 5.0, rotation: 10.0 },
            Pose { tilt: -3.0, rotation: 720.0 },
        ];
        let (mut a, mut b) = (
            OrientationSmoother::new(OrientationConfig { enabled: true, smoothing_factor: 0.7 }),
            OrientationSmoother::new(OrientationConfig { enabled: true, smoothing_factor: 0.7 }),
        );
        for p in inputs {
            assert_eq!(a.push(p), b.push(p));
        }
    }
}
