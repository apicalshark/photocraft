//! Deterministic wave/noise/mix helpers (`f32`, seeded; no clock, no RNG).
//!
//! Pure functions for the companion-side script evaluator and for distortion
//! synthesis. Wave inputs are in cycles (period 1) with range [-1, 1]; the
//! phase conventions below are ours, documented and tested -- **not** a claim
//! about any proprietary function table. Name collisions with the public
//! script reference are resolved in docs, not here.

/// `sin(2πx)`. Non-finite input yields 0.
pub fn sine(x: f32) -> f32 {
    if !x.is_finite() {
        return 0.0;
    }
    (x * 2.0 * core::f32::consts::PI).sin()
}

/// Bipolar square: `1` while `frac(x) < pulse_width`, else `-1`.
/// `pulse_width` clamps to 0..1. Non-finite input yields -1.
pub fn square(x: f32, pulse_width: f32) -> f32 {
    if !x.is_finite() {
        return -1.0;
    }
    if frac(x) < pulse_width.clamp(0.0, 1.0) { 1.0 } else { -1.0 }
}

/// Triangle through (0,1) (1/4,0) (1/2,-1) (3/4,0), period 1.
/// Non-finite input yields 0.
pub fn triangle(x: f32) -> f32 {
    if !x.is_finite() {
        return 0.0;
    }
    2.0 * (2.0 * frac(x) - 1.0).abs() - 1.0
}

/// Rising sawtooth `2·frac(x) − 1` in [-1, 1). Non-finite input yields -1.
pub fn saw(x: f32) -> f32 {
    if !x.is_finite() {
        return -1.0;
    }
    2.0 * frac(x) - 1.0
}

/// Bipolar sigmoid `2/(1+e^(−x)) − 1`: R -> (-1, 1), `sigmoid(0) = 0`.
/// Non-finite input yields ±1 (sign) or 0 for NaN.
pub fn sigmoid(x: f32) -> f32 {
    if x.is_nan() {
        return 0.0;
    }
    if !x.is_finite() {
        return x.signum();
    }
    2.0 / (1.0 + (-x).exp()) - 1.0
}

/// Integer-lattice hash in 0..1. The sole entropy source: wrapping
/// xorshift-multiply, identical on every platform.
pub fn hash01(i: i32) -> f32 {
    let mut h = (i as u32).wrapping_mul(0x9E37_79B1);
    h ^= h >> 15;
    h = h.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 13;
    // 2^32 -> [0, 1). The division is exact for u32 numerators.
    (h as f32) / 4294967296.0
}

/// Continuous value noise in [-1, 1]: hashed lattice + smootherstep
/// interpolation. Same input always gives the same output.
pub fn noise(x: f32) -> f32 {
    if !x.is_finite() {
        return 0.0;
    }
    let ix = x.floor() as i32;
    let t = x - x.floor();
    let u = t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
    hash01(ix) * 2.0 - 1.0 + ((hash01(ix.wrapping_add(1)) - hash01(ix)) * 2.0 * u)
}

/// Stepwise cell noise in [-1, 1]: constant within each integer cell.
pub fn cell_noise(x: f32) -> f32 {
    if !x.is_finite() {
        return 0.0;
    }
    hash01(x.floor() as i32) * 2.0 - 1.0
}

/// Linear blend with `t` clamped to 0..1.
pub fn mix(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * clamp(t, 0.0, 1.0)
}

/// Clamp to `[lo, hi]` (swapped bounds are corrected, never NaN from order).
pub fn clamp(v: f32, lo: f32, hi: f32) -> f32 {
    if lo <= hi { v.clamp(lo, hi) } else { v.clamp(hi, lo) }
}

/// Smoothstep: 0 below `a`, 1 above `b`, smooth in between. `a == b`
/// degrades to a hard step (no division by zero).
pub fn smooth_step(a: f32, b: f32, x: f32) -> f32 {
    if !(a.is_finite() && b.is_finite() && x.is_finite()) {
        return 0.0;
    }
    if a == b {
        return if x < a { 0.0 } else { 1.0 };
    }
    let (lo, hi) = if a < b { (a, b) } else { (b, a) };
    let t = clamp((x - lo) / (hi - lo), 0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn frac(x: f32) -> f32 {
    x - x.floor()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn waves_have_period_one_and_stay_in_range() {
        for i in 0..64 {
            let x = i as f32 * 0.137;
            for f in [sine(x), triangle(x), saw(x), noise(x), cell_noise(x)] {
                assert!((-1.0..=1.0).contains(&f), "out of range: {f}");
            }
            assert!(near(sine(x + 1.0), sine(x)), "sine period");
            assert!(near(triangle(x + 1.0), triangle(x)), "triangle period");
            assert!(near(saw(x + 2.0), saw(x)), "saw period");
        }
        // Square is period-1 away from pulse edges (float-safe landmarks).
        assert_eq!(square(0.1 + 1.0, 0.3), square(0.1, 0.3));
        assert_eq!(square(0.7 + 1.0, 0.3), square(0.7, 0.3));
        // Cell noise is NOT periodic: each cell hashes differently (by design).
    }

    #[test]
    fn wave_landmarks() {
        assert!(near(sine(0.0), 0.0) && near(sine(0.25), 1.0) && near(sine(0.5), 0.0));
        assert!(near(triangle(0.0), 1.0) && near(triangle(0.5), -1.0) && near(triangle(0.25), 0.0));
        assert!(near(saw(0.0), -1.0) && near(saw(0.5), 0.0));
        assert_eq!(square(0.1, 0.5), 1.0);
        assert_eq!(square(0.7, 0.5), -1.0);
        assert_eq!(square(0.1, 0.0), -1.0, "empty pulse never fires");
    }

    #[test]
    fn sigmoid_shape() {
        assert!(near(sigmoid(0.0), 0.0));
        assert!(sigmoid(2.0) > sigmoid(1.0) && sigmoid(1.0) > 0.0, "monotonic rise");
        assert!(sigmoid(-3.0) < 0.0);
        assert!(sigmoid(100.0) > 0.999 && sigmoid(-100.0) < -0.999, "saturates");
    }

    #[test]
    fn noise_is_deterministic_and_varied() {
        assert_eq!(noise(3.7), noise(3.7));
        assert_eq!(cell_noise(3.7), cell_noise(3.7));
        // Lattice points interpolate exactly to their hashes.
        assert!(near(noise(4.0), cell_noise(4.0)));
        // Neighbouring cells usually differ (hash avalanches).
        assert!((cell_noise(4.0) - cell_noise(5.0)).abs() > 1e-3);
        // Cell noise is flat inside a cell.
        assert_eq!(cell_noise(4.2), cell_noise(4.9));
    }

    #[test]
    fn mix_clamp_smooth_step() {
        assert!(near(mix(10.0, 20.0, 0.25), 12.5));
        assert!(near(mix(10.0, 20.0, 99.0), 20.0), "t clamps");
        assert_eq!(clamp(5.0, 1.0, 0.0), 1.0, "swapped bounds corrected");
        assert!(near(smooth_step(0.0, 1.0, 0.5), 0.5));
        assert_eq!(smooth_step(0.0, 1.0, -2.0), 0.0);
        assert_eq!(smooth_step(0.0, 1.0, 2.0), 1.0);
        assert_eq!(smooth_step(2.0, 2.0, 1.9), 0.0, "degenerate step, no div-by-zero");
        assert_eq!(smooth_step(2.0, 2.0, 2.0), 1.0);
    }

    #[test]
    fn non_finite_inputs_have_defined_outputs() {
        let nan = f32::NAN;
        let inf = f32::INFINITY;
        assert_eq!(sine(nan), 0.0);
        assert_eq!(triangle(inf), 0.0);
        assert_eq!(saw(nan), -1.0);
        assert_eq!(square(nan, 0.5), -1.0);
        assert_eq!(sigmoid(nan), 0.0);
        assert_eq!(sigmoid(inf), 1.0);
        assert_eq!(noise(nan), 0.0);
        assert_eq!(cell_noise(inf), 0.0);
        assert_eq!(smooth_step(0.0, 1.0, nan), 0.0);
    }
}
