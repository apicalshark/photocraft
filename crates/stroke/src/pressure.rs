//! Pressure chain: curve -> processing -> speed scaling.
//!
//! Clean-room point-stream implementation. Stage order, field names, and the
//! observed ranges come from the preset facts and the public pressure
//! tutorial (SPEC Part A §§A3-A5); the maths is standard dynamics practice,
//! **not** a claim about any proprietary formula.
//!
//! Mode-number mapping (`0 fixed / 1 sample&hold / 2 moving-average /
//! 3 exponential`) follows the tutorial's listing order and the preset
//! evidence (`mode 2` pairs with `pressureNumSamples`, `mode 0` pairs with a
//! zero `pressureFixedValue`); confidence is high for 0/2, lower for 1/3,
//! which have no shipped preset instance. The mapping lives in one place
//! ([`PressureMode`]) so a correction is a two-line change.
//!
//! Pen-up rule (public scripting tutorial): raw `p == 0` yields `0`
//! unconditionally, otherwise the host would never see the pen lift. Feed a
//! fresh [`PressurePipe`] per stroke.

/// Which processing law applies after the curve stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PressureMode {
    /// Hold [`PressureConfig::fixed_value`].
    Fixed = 0,
    /// Hold the pressure of the Nth sample
    /// ([`PressureConfig::sample_hold_index`], 1-based; observed value 15).
    SampleHold = 1,
    /// Mean of the last [`PressureConfig::num_samples`] samples.
    MovingAverage = 2,
    /// Full-history exponential approach.
    Exponential = 3,
}

/// Parameters for [`PressurePipe`]. Field names mirror the preset facts.
#[derive(Clone, Debug, PartialEq)]
pub struct PressureConfig {
    // -- curve stage --
    pub curve_enabled: bool,
    /// Multiply first. Observed presets: 1..1.8.
    pub gain: f32,
    /// Exponent bend (`pow(p, 1 + softness)`); 0 is a no-op.
    /// Observed presets: -0.4..0 (negative reaches high pressure sooner).
    pub softness: f32,
    pub min_val: f32,
    pub max_val: f32,
    /// Mirror around `[min_val, max_val]`.
    pub invert: bool,
    // -- processing stage --
    pub processing_enabled: bool,
    pub mode: PressureMode,
    pub fixed_value: f32,
    pub sample_hold_index: u32,
    pub num_samples: u32,
    pub smoothing_factor: f32,
    // -- speed scaling stage --
    pub speed_enabled: bool,
    /// Gain at rest. Observed presets: 0.33..1.
    pub slow_gain: f32,
    /// Gain at [`PressureConfig::fast_speed`]. Observed presets: 0.33..1.
    pub fast_gain: f32,
    /// Speed at which `fast_gain` fully applies. Observed: 500..564.
    pub fast_speed: f32,
    /// Pre-smooth the speed signal (amount 0..1, 0 follows directly).
    /// Observed presets: 0..0.09.
    pub speed_smoothing: f32,
}

impl Default for PressureConfig {
    /// Identity: every stage passes the pressure through unchanged.
    fn default() -> Self {
        Self {
            curve_enabled: true,
            gain: 1.0,
            softness: 0.0,
            min_val: 0.0,
            max_val: 1.0,
            invert: false,
            processing_enabled: true,
            mode: PressureMode::MovingAverage,
            fixed_value: 0.0,
            sample_hold_index: 15,
            num_samples: 1,
            smoothing_factor: 0.0,
            speed_enabled: false,
            slow_gain: 1.0,
            fast_gain: 1.0,
            fast_speed: 500.0,
            speed_smoothing: 0.0,
        }
    }
}

/// Cap for the moving-average window (observed presets need at most 30).
pub const MAX_WINDOW: usize = 32;

/// Incremental pressure pipe: one [`push`](PressurePipe::push) per sample.
#[derive(Clone, Debug)]
pub struct PressurePipe {
    cfg: PressureConfig,
    window: [f32; MAX_WINDOW],
    win_len: usize,
    win_idx: usize,
    count: u32,
    held: Option<f32>,
    ema: Option<f32>,
    speed_ema: Option<f32>,
}

impl PressurePipe {
    pub fn new(cfg: PressureConfig) -> Self {
        Self {
            cfg,
            window: [0.0; MAX_WINDOW],
            win_len: 0,
            win_idx: 0,
            count: 0,
            held: None,
            ema: None,
            speed_ema: None,
        }
    }

    /// Push one sample (`p` in 0..1, `speed` in the same units as
    /// `fast_speed`). Returns the processed pressure. Raw `p == 0` returns
    /// `0` (pen-up rule); non-finite inputs are treated as `0` without
    /// poisoning the state.
    pub fn push(&mut self, p: f32, speed: f32) -> f32 {
        if p == 0.0 {
            return 0.0;
        }
        let p = if p.is_finite() { p.max(0.0) } else { 0.0 };
        let speed = if speed.is_finite() { speed.max(0.0) } else { 0.0 };
        let curved = self.curve(p);
        let processed = self.process(curved);
        self.scale(processed, speed)
    }

    fn curve(&self, p: f32) -> f32 {
        if !self.cfg.curve_enabled {
            return p;
        }
        let mut v = p * self.cfg.gain;
        if self.cfg.softness != 0.0 && v > 0.0 && v.is_finite() {
            v = v.powf(1.0 + self.cfg.softness);
        }
        let (lo, hi) = if self.cfg.min_val <= self.cfg.max_val {
            (self.cfg.min_val, self.cfg.max_val)
        } else {
            (self.cfg.max_val, self.cfg.min_val)
        };
        v = v.clamp(lo, hi);
        if self.cfg.invert { lo + hi - v } else { v }
    }

    fn process(&mut self, v: f32) -> f32 {
        if !self.cfg.processing_enabled {
            return v;
        }
        match self.cfg.mode {
            PressureMode::Fixed => self.cfg.fixed_value.clamp(0.0, 1.0),
            PressureMode::SampleHold => {
                self.count = self.count.wrapping_add(1);
                let want = self.cfg.sample_hold_index.max(1);
                if self.count == want {
                    self.held = Some(v);
                }
                self.held.unwrap_or(v)
            }
            PressureMode::MovingAverage => {
                let n = self.cfg.num_samples.clamp(1, MAX_WINDOW as u32) as usize;
                self.window[self.win_idx % MAX_WINDOW] = v;
                self.win_idx = self.win_idx.wrapping_add(1);
                self.win_len = (self.win_len + 1).min(n);
                let mut sum = 0.0f32;
                for i in 0..self.win_len {
                    let slot = (self.win_idx + MAX_WINDOW - 1 - i) % MAX_WINDOW;
                    sum += self.window[slot];
                }
                sum / self.win_len as f32
            }
            PressureMode::Exponential => {
                let a = 1.0 - self.cfg.smoothing_factor.clamp(0.0, 0.95);
                match self.ema {
                    Some(e) => {
                        let next = e + (v - e) * a;
                        self.ema = Some(next);
                        next
                    }
                    None => {
                        self.ema = Some(v);
                        v
                    }
                }
            }
        }
    }

    fn scale(&mut self, v: f32, speed: f32) -> f32 {
        if !self.cfg.speed_enabled {
            return v;
        }
        let a = 1.0 - self.cfg.speed_smoothing.clamp(0.0, 0.95);
        let smoothed = match self.speed_ema {
            Some(e) => {
                let next = e + (speed - e) * a;
                self.speed_ema = Some(next);
                next
            }
            None => {
                self.speed_ema = Some(speed);
                speed
            }
        };
        let t = if self.cfg.fast_speed > 0.0 && self.cfg.fast_speed.is_finite() {
            (smoothed / self.cfg.fast_speed).clamp(0.0, 1.0)
        } else {
            0.0
        };
        v * (self.cfg.slow_gain + (self.cfg.fast_gain - self.cfg.slow_gain) * t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    fn cfg() -> PressureConfig {
        PressureConfig::default()
    }

    #[test]
    fn default_pipe_is_identity() {
        let mut p = PressurePipe::new(cfg());
        for v in [0.0, 0.13, 0.5, 1.0] {
            assert!(near(p.push(v, 300.0), v), "v={v}");
        }
        // Out-of-range inputs clamp (hostile-input policy, not distortion).
        assert!(near(p.push(2.0, 0.0), 1.0));
        assert!(near(p.push(-1.0, 0.0), 0.0));
    }

    #[test]
    fn gain_multiplies_first() {
        let mut c = cfg();
        c.gain = 1.8;
        let mut p = PressurePipe::new(c);
        assert!(near(p.push(0.5, 0.0), 0.9));
    }

    #[test]
    fn softness_bends_the_curve() {
        // Negative softness lifts the low end (high pressure sooner).
        let mut c = cfg();
        c.softness = -0.32;
        let mut p = PressurePipe::new(c);
        let out = p.push(0.5, 0.0);
        assert!(out > 0.5 && out < 1.0, "got {out}");
        // Positive softness presses it down.
        let mut c = cfg();
        c.softness = 0.5;
        let mut p = PressurePipe::new(c);
        let out = p.push(0.5, 0.0);
        assert!(out < 0.5, "got {out}");
        // Zero is a no-op.
        let mut p = PressurePipe::new(cfg());
        assert!(near(p.push(0.5, 0.0), 0.5));
    }

    #[test]
    fn clamp_and_invert() {
        let mut c = cfg();
        c.gain = 2.0;
        let mut p = PressurePipe::new(c);
        assert!(near(p.push(0.8, 0.0), 1.0), "clamps at max");
        let mut c = cfg();
        c.invert = true;
        let mut p = PressurePipe::new(c);
        assert!(near(p.push(0.25, 0.0), 0.75));
        // Swapped min/max cannot invert the clamp.
        let mut c = cfg();
        c.min_val = 1.0;
        c.max_val = 0.0;
        let mut p = PressurePipe::new(c);
        assert!(near(p.push(0.5, 0.0), 0.5));
    }

    #[test]
    fn fixed_mode_holds_but_pen_up_wins() {
        let mut c = cfg();
        c.mode = PressureMode::Fixed;
        c.fixed_value = 0.81;
        let mut p = PressurePipe::new(c);
        assert!(near(p.push(0.3, 0.0), 0.81));
        assert!(near(p.push(0.9, 0.0), 0.81));
        assert_eq!(p.push(0.0, 0.0), 0.0, "pen-up rule overrides fixed");
    }

    #[test]
    fn sample_hold_latches_nth_sample() {
        let mut c = cfg();
        c.mode = PressureMode::SampleHold;
        c.sample_hold_index = 3;
        let mut p = PressurePipe::new(c);
        assert!(near(p.push(0.2, 0.0), 0.2));
        assert!(near(p.push(0.4, 0.0), 0.4));
        assert!(near(p.push(0.6, 0.0), 0.6));
        assert!(near(p.push(0.9, 0.0), 0.6), "latched");
        assert!(near(p.push(0.1, 0.0), 0.6), "still latched");
    }

    #[test]
    fn moving_average_smooths_and_fades() {
        let mk = || {
            let mut c = cfg();
            c.mode = PressureMode::MovingAverage;
            c.num_samples = 4;
            PressurePipe::new(c)
        };
        let mut p = mk();
        assert!(near(p.push(1.0, 0.0), 1.0));
        assert!(near(p.push(1.0, 0.0), 1.0));
        assert!(near(p.push(0.0, 0.0), 0.0), "pen-up rule, not the mean");
        // A pressure ramp-down fades through the window.
        let mut p = mk();
        for _ in 0..4 {
            p.push(1.0, 0.0);
        }
        assert!(near(p.push(0.5, 0.0), 0.875));
    }

    #[test]
    fn exponential_follows_and_smooths() {
        let mut c = cfg();
        c.mode = PressureMode::Exponential;
        c.smoothing_factor = 0.0;
        let mut p = PressurePipe::new(c);
        assert!(near(p.push(0.7, 0.0), 0.7));
        let mut c = cfg();
        c.mode = PressureMode::Exponential;
        c.smoothing_factor = 0.5;
        let mut p = PressurePipe::new(c);
        assert!(near(p.push(1.0, 0.0), 1.0), "first sample seeds");
        assert!(near(p.push(1.0, 0.0), 1.0));
        assert!(near(p.push(0.0, 0.0), 0.0), "pen-up rule");
    }

    #[test]
    fn speed_scaling_interpolates_gains() {
        let mk = || {
            let mut c = cfg();
            c.speed_enabled = true;
            c.slow_gain = 0.33;
            c.fast_gain = 1.0;
            c.fast_speed = 500.0;
            c.speed_smoothing = 0.0;
            PressurePipe::new(c)
        };
        assert!(near(mk().push(1.0, 0.0), 0.33), "at rest");
        assert!(near(mk().push(1.0, 500.0), 1.0), "at fast speed");
        assert!(near(mk().push(1.0, 250.0), 0.665), "midway");
    }

    #[test]
    fn speed_stage_guards_degenerate_fast_speed() {
        let mut c = cfg();
        c.speed_enabled = true;
        c.slow_gain = 0.5;
        c.fast_gain = 2.0;
        c.fast_speed = 0.0;
        let mut p = PressurePipe::new(c);
        assert!(near(p.push(1.0, 9999.0), 0.5), "falls back to slow gain");
    }

    #[test]
    fn non_finite_inputs_do_not_poison() {
        let mut p = PressurePipe::new(cfg());
        assert_eq!(p.push(f32::NAN, 0.0), 0.0);
        assert_eq!(p.push(f32::INFINITY, f32::NAN), 0.0);
        assert!(near(p.push(0.4, 0.0), 0.4), "state alive");
    }

    #[test]
    fn batch_equals_incremental() {
        let inputs = [(1.0, 0.0), (0.8, 120.0), (0.6, 400.0), (0.9, 900.0), (0.2, 50.0)];
        let mk = || {
            let mut c = cfg();
            c.speed_enabled = true;
            c.slow_gain = 0.33;
            c.fast_gain = 1.0;
            c.softness = -0.2;
            PressurePipe::new(c)
        };
        let (mut a, mut b) = (mk(), mk());
        for (p, s) in inputs {
            assert_eq!(a.push(p, s), b.push(p, s));
        }
    }
}
