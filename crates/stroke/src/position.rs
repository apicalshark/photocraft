//! Position smoothing modes (`positionMode 0..3`).
//!
//! Clean-room point-stream implementations. Names and parameter ranges come
//! from the observed preset facts and the public smoothing tutorial (SPEC
//! Part A); the maths below is standard stabilization practice, **not** a
//! claim about any proprietary formula.
//!
//! Feed inputs with [`PositionSmoother::push`]; a `None` return means the brush
//! holds (string not yet taut, or a paused pointer with catch-up off). When
//! the pointer pauses or the stroke ends, call [`PositionSmoother::finish`] to
//! collect the catch-up tail.
//!
//! Host mapping (PhotoCraft `paint::dynamics::Smoother`, read 2026-10-14):
//! pulled-string length `amount * 100px`, exponential step `a = 1 - amount`
//! per 16ms step. [`PositionConfig::from_host_amount`] encodes that mapping;
//! device-feel parity still needs real-stroke fixtures
//! (`host_stroke_fixture_parity`, ignored until then).

/// 2D point in document pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pt {
    pub x: f32,
    pub y: f32,
}

impl Pt {
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }
}

/// Which smoothing law [`PositionSmoother`] applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PositionMode {
    /// Lazy-mouse: the brush follows only once the pointer has pulled a
    /// string of [`PositionConfig::string_length`] taut.
    PulledString = 0,
    /// Mean of the last [`PositionConfig::num_samples`] inputs.
    MovingAverage = 1,
    /// Full-history exponential approach with
    /// [`PositionConfig::smoothing_factor`] as the amount.
    Exponential = 2,
    /// Spring physics over a velocity state (`mass` / `drag`); suits fast
    /// lines only and never snaps on [`PositionSmoother::finish`].
    Inertia = 3,
}

/// Parameters for [`PositionSmoother`]. Field names mirror the preset facts.
#[derive(Clone, Debug, PartialEq)]
pub struct PositionConfig {
    pub mode: PositionMode,
    /// Taut-string length in px (mode 0). Observed presets: 23..40.
    pub string_length: f32,
    /// Moving-average window (mode 1). Observed presets: 5..30.
    pub num_samples: u32,
    /// Exponential amount 0..1 (mode 2). 0 follows the pointer directly.
    pub smoothing_factor: f32,
    /// Spring mass (mode 3). Observed presets: 30..31.25.
    pub mass: f32,
    /// Velocity retained per push, 0..1 (mode 3). Observed presets: 0.2.
    pub drag: f32,
    /// Keep approaching a paused pointer (modes 1/2).
    pub catch_up: bool,
    /// Snap to the last pointer position on [`PositionSmoother::finish`].
    pub catch_up_on_end: bool,
}

impl Default for PositionConfig {
    /// Mirrors the host brush default behaviour: smoothing effectively off
    /// (exponential amount 0 follows the pointer), catch-up on.
    fn default() -> Self {
        Self {
            mode: PositionMode::Exponential,
            string_length: 40.0,
            num_samples: 7,
            smoothing_factor: 0.0,
            mass: 30.0,
            drag: 0.2,
            catch_up: true,
            catch_up_on_end: true,
        }
    }
}

impl PositionConfig {
    /// Map a host-style `amount` (0..1) onto this config. Pulled-string mode
    /// uses `amount * 100px`; exponential mode uses `amount` as the factor
    /// (host step `a = 1 - amount` per 16ms). Out-of-range amounts clamp.
    pub fn from_host_amount(mode: PositionMode, amount: f32) -> Self {
        let amount = amount.clamp(0.0, 1.0);
        Self { mode, string_length: amount * 100.0, smoothing_factor: amount, ..Self::default() }
    }
}

/// One step of the 16ms time base shared with the host smoother.
pub const SMOOTHING_STEP_MS: f32 = 16.0;
/// Cap for the moving-average window (observed presets need at most 30).
pub const MAX_WINDOW: usize = 32;

/// Incremental position smoother over a point stream.
#[derive(Clone, Debug)]
pub struct PositionSmoother {
    cfg: PositionConfig,
    pos: Option<Pt>,
    last_input: Option<Pt>,
    last_out: Option<Pt>,
    window: [Pt; MAX_WINDOW],
    win_len: usize,
    win_idx: usize,
    vel: Pt,
}

impl PositionSmoother {
    pub fn new(cfg: PositionConfig) -> Self {
        Self {
            cfg,
            pos: None,
            last_input: None,
            last_out: None,
            window: [Pt { x: 0.0, y: 0.0 }; MAX_WINDOW],
            win_len: 0,
            win_idx: 0,
            vel: Pt { x: 0.0, y: 0.0 },
        }
    }

    /// Feed one pointer sample; `dt_ms` is the stroke time since the previous
    /// sample (`<= 0` or non-finite means one smoothing step). Returns the
    /// brush position, or `None` when the brush holds. Non-finite inputs are
    /// dropped without poisoning the state.
    pub fn push(&mut self, p: Pt, dt_ms: f32) -> Option<Pt> {
        if !p.is_finite() {
            return None;
        }
        let still = self.last_input.is_some_and(|q| q.x == p.x && q.y == p.y);
        self.last_input = Some(p);
        let out = match self.cfg.mode {
            PositionMode::PulledString => self.push_pulled(p),
            PositionMode::MovingAverage => self.push_ma(p, still),
            PositionMode::Exponential => self.push_exp(p, dt_ms, still),
            PositionMode::Inertia => self.push_inertia(p),
        };
        if out.is_some() {
            self.last_out = out;
        }
        out
    }

    /// End of stroke: with `catch_up_on_end` the brush finishes at the last
    /// pointer position (inertia mode never snaps: it has no catch-up).
    pub fn finish(&mut self) -> Option<Pt> {
        if !self.cfg.catch_up_on_end || self.cfg.mode == PositionMode::Inertia {
            return None;
        }
        match (self.last_out, self.last_input) {
            (Some(o), Some(last)) if o.x != last.x || o.y != last.y => {
                self.pos = Some(last);
                self.last_out = Some(last);
                Some(last)
            }
            _ => None,
        }
    }

    /// Is catch-up still pulling the brush towards a stopped pointer?
    pub fn lagging(&self) -> bool {
        if !self.cfg.catch_up || self.cfg.mode == PositionMode::Inertia {
            return false;
        }
        match (self.last_out, self.last_input) {
            (Some(o), Some(last)) => (o.x - last.x).abs() + (o.y - last.y).abs() > 0.01,
            _ => false,
        }
    }

    /// Guide-line endpoints `(brush, pointer)` while smoothing holds the
    /// brush back, `None` when they coincide (or before the first sample).
    pub fn string(&self) -> Option<(Pt, Pt)> {
        let (o, last) = (self.last_out?, self.last_input?);
        (o.x != last.x || o.y != last.y).then_some((o, last))
    }

    fn push_pulled(&mut self, p: Pt) -> Option<Pt> {
        let Some(pos) = self.pos else {
            self.pos = Some(p);
            return Some(p);
        };
        let len = self.cfg.string_length.max(0.0);
        let (dx, dy) = (p.x - pos.x, p.y - pos.y);
        let d = (dx * dx + dy * dy).sqrt();
        if d > len && d > 0.0 {
            let k = (d - len) / d;
            let np = Pt { x: pos.x + dx * k, y: pos.y + dy * k };
            self.pos = Some(np);
            Some(np)
        } else {
            None
        }
    }

    fn push_ma(&mut self, p: Pt, still: bool) -> Option<Pt> {
        if still && !self.cfg.catch_up {
            return None;
        }
        let n = self.cfg.num_samples.clamp(1, MAX_WINDOW as u32) as usize;
        self.window[self.win_idx % MAX_WINDOW] = p;
        self.win_idx = self.win_idx.wrapping_add(1);
        self.win_len = (self.win_len + 1).min(n);
        // Mean over the live window: the oldest slots when fewer than `n`
        // samples have arrived, the last `n` slots once full.
        let mut sx = 0.0f32;
        let mut sy = 0.0f32;
        for i in 0..self.win_len {
            let slot = (self.win_idx + MAX_WINDOW - 1 - i) % MAX_WINDOW;
            let q = self.window[slot];
            sx += q.x;
            sy += q.y;
        }
        let len = self.win_len as f32;
        let mean = Pt { x: sx / len, y: sy / len };
        self.pos = Some(mean);
        Some(mean)
    }

    fn push_exp(&mut self, p: Pt, dt_ms: f32, still: bool) -> Option<Pt> {
        if still && !self.cfg.catch_up {
            return None;
        }
        let Some(mut cur) = self.pos else {
            self.pos = Some(p);
            return Some(p);
        };
        // Host law: each 16ms step moves `a` of the way towards the pointer.
        let a = 1.0 - self.cfg.smoothing_factor.clamp(0.0, 0.95);
        let steps = if dt_ms > 0.0 && dt_ms.is_finite() { dt_ms / SMOOTHING_STEP_MS } else { 1.0 };
        // Closed form of the host's sub-step loop: 1 - (1-a)^steps.
        // (The host emits each sub-step as a paint point; the point stream
        // here carries only the final position -- see SPEC §5.)
        let t = 1.0 - (1.0 - a).powf(steps.min(64.0));
        cur.x += (p.x - cur.x) * t;
        cur.y += (p.y - cur.y) * t;
        self.pos = Some(cur);
        Some(cur)
    }

    fn push_inertia(&mut self, p: Pt) -> Option<Pt> {
        let Some(mut cur) = self.pos else {
            self.pos = Some(p);
            self.vel = Pt { x: 0.0, y: 0.0 };
            return Some(p);
        };
        if self.cfg.mass <= 0.0 || !self.cfg.mass.is_finite() {
            self.vel = Pt { x: 0.0, y: 0.0 };
            self.pos = Some(p);
            return Some(p);
        }
        let retain = (1.0 - self.cfg.drag.clamp(0.0, 1.0)).max(0.0);
        self.vel.x = (self.vel.x + (p.x - cur.x) / self.cfg.mass) * retain;
        self.vel.y = (self.vel.y + (p.y - cur.y) / self.cfg.mass) * retain;
        cur.x += self.vel.x;
        cur.y += self.vel.y;
        self.pos = Some(cur);
        Some(cur)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(x: f32, y: f32) -> Pt {
        Pt { x, y }
    }

    fn cfg(mode: PositionMode) -> PositionConfig {
        PositionConfig { mode, ..PositionConfig::default() }
    }

    fn near(a: Pt, b: Pt) -> bool {
        (a.x - b.x).abs() < 1e-3 && (a.y - b.y).abs() < 1e-3
    }

    #[test]
    fn pulled_string_holds_until_taut() {
        let mut c = cfg(PositionMode::PulledString);
        c.string_length = 40.0;
        c.catch_up = false;
        let mut s = PositionSmoother::new(c);
        assert_eq!(s.push(pt(0.0, 0.0), 0.0), Some(pt(0.0, 0.0)));
        assert_eq!(s.push(pt(10.0, 0.0), 16.0), None);
        assert_eq!(s.push(pt(39.9, 0.0), 16.0), None);
        // d = 50, k = (50-40)/50 = 0.2 -> (10, 0).
        assert_eq!(s.push(pt(50.0, 0.0), 16.0), Some(pt(10.0, 0.0)));
        assert!(!s.lagging(), "pulled mode has no catch-up");
    }

    #[test]
    fn pulled_string_diagonal_matches_host_law() {
        let mut c = cfg(PositionMode::PulledString);
        c.string_length = 5.0;
        let mut s = PositionSmoother::new(c);
        s.push(pt(0.0, 0.0), 0.0);
        // d = 10, k = 0.5 -> (3, 4).
        let out = s.push(pt(6.0, 8.0), 16.0).expect("taut");
        assert!(near(out, pt(3.0, 4.0)), "got {out:?}");
        // Guide line spans brush -> pointer.
        assert_eq!(s.string(), Some((pt(3.0, 4.0), pt(6.0, 8.0))));
    }

    #[test]
    fn pulled_zero_length_follows() {
        let mut c = cfg(PositionMode::PulledString);
        c.string_length = 0.0;
        let mut s = PositionSmoother::new(c);
        s.push(pt(0.0, 0.0), 0.0);
        assert_eq!(s.push(pt(7.0, 1.0), 16.0), Some(pt(7.0, 1.0)));
    }

    #[test]
    fn moving_average_means_last_n() {
        let mut c = cfg(PositionMode::MovingAverage);
        c.num_samples = 3;
        let mut s = PositionSmoother::new(c);
        assert_eq!(s.push(pt(0.0, 0.0), 0.0), Some(pt(0.0, 0.0)));
        assert!(near(s.push(pt(3.0, 0.0), 16.0).expect("mean"), pt(1.5, 0.0)));
        assert!(near(s.push(pt(6.0, 0.0), 16.0).expect("mean"), pt(3.0, 0.0)));
        // Window slides: mean(3, 6, 9) = 6.
        assert!(near(s.push(pt(9.0, 0.0), 16.0).expect("mean"), pt(6.0, 0.0)));
    }

    #[test]
    fn moving_average_still_holds_without_catch_up() {
        let mut c = cfg(PositionMode::MovingAverage);
        c.num_samples = 4;
        c.catch_up = false;
        let mut s = PositionSmoother::new(c);
        s.push(pt(0.0, 0.0), 0.0);
        s.push(pt(8.0, 0.0), 16.0);
        assert_eq!(s.push(pt(8.0, 0.0), 16.0), None, "brush stops with the pen");
        assert!(!s.lagging());
    }

    #[test]
    fn moving_average_finish_snaps_to_pen() {
        let mut c = cfg(PositionMode::MovingAverage);
        c.num_samples = 30;
        c.catch_up_on_end = true;
        let mut s = PositionSmoother::new(c);
        s.push(pt(0.0, 0.0), 0.0);
        s.push(pt(30.0, 0.0), 16.0);
        assert!(s.lagging());
        assert_eq!(s.finish(), Some(pt(30.0, 0.0)));
        assert_eq!(s.finish(), None, "second finish is a no-op");
        assert!(!s.lagging());
    }

    #[test]
    fn exponential_zero_amount_follows() {
        let mut c = cfg(PositionMode::Exponential);
        c.smoothing_factor = 0.0;
        let mut s = PositionSmoother::new(c);
        s.push(pt(5.0, 5.0), 0.0);
        assert_eq!(s.push(pt(9.0, 1.0), 16.0), Some(pt(9.0, 1.0)));
        assert_eq!(s.finish(), None, "already at the pen");
    }

    #[test]
    fn exponential_step_matches_host_single_step() {
        // Host: a = 1 - amount; one step moves `a` of the way.
        let mut c = cfg(PositionMode::Exponential);
        c.smoothing_factor = 0.4;
        let mut s = PositionSmoother::new(c);
        s.push(pt(0.0, 0.0), 0.0);
        // dt <= 0 counts as one step: 0 + (10 - 0) * 0.6 = 6.
        let out = s.push(pt(10.0, 0.0), 0.0).expect("step");
        assert!(near(out, pt(6.0, 0.0)), "got {out:?}");
    }

    #[test]
    fn exponential_dt_accumulates_like_host_substeps() {
        // 32ms = two 16ms steps of a=0.5: 0 -> 5 -> 7.5.
        let mut c = cfg(PositionMode::Exponential);
        c.smoothing_factor = 0.5;
        let mut s = PositionSmoother::new(c);
        s.push(pt(0.0, 0.0), 0.0);
        let out = s.push(pt(10.0, 0.0), 32.0).expect("steps");
        assert!(near(out, pt(7.5, 0.0)), "got {out:?}");
    }

    #[test]
    fn exponential_still_holds_without_catch_up() {
        let mut c = cfg(PositionMode::Exponential);
        c.smoothing_factor = 0.5;
        c.catch_up = false;
        let mut s = PositionSmoother::new(c);
        s.push(pt(0.0, 0.0), 0.0);
        s.push(pt(10.0, 0.0), 16.0);
        assert_eq!(s.push(pt(10.0, 0.0), 16.0), None);
    }

    #[test]
    fn inertia_moves_toward_target_without_snapping() {
        let mut c = cfg(PositionMode::Inertia);
        c.mass = 30.0;
        c.drag = 0.2;
        let mut s = PositionSmoother::new(c);
        assert_eq!(s.push(pt(0.0, 0.0), 0.0), Some(pt(0.0, 0.0)));
        let out = s.push(pt(30.0, 0.0), 16.0).expect("spring");
        // v = (30/30) * 0.8 = 0.8 -> x = 0.8: moves, far from snapping.
        assert!(near(out, pt(0.8, 0.0)), "got {out:?}");
        assert_eq!(s.finish(), None, "inertia never snaps");
    }

    #[test]
    fn inertia_guards_degenerate_params() {
        let mut c = cfg(PositionMode::Inertia);
        c.mass = 0.0;
        let mut s = PositionSmoother::new(c);
        s.push(pt(0.0, 0.0), 0.0);
        assert_eq!(s.push(pt(4.0, 2.0), 16.0), Some(pt(4.0, 2.0)));
    }

    #[test]
    fn non_finite_input_drops_without_poisoning() {
        for mode in [
            PositionMode::PulledString,
            PositionMode::MovingAverage,
            PositionMode::Exponential,
            PositionMode::Inertia,
        ] {
            let mut s = PositionSmoother::new(cfg(mode));
            s.push(pt(1.0, 1.0), 0.0);
            assert_eq!(s.push(pt(f32::NAN, 0.0), 16.0), None);
            assert_eq!(s.push(pt(f32::INFINITY, 0.0), 16.0), None);
            // Pulled-string holds near inputs by design; probe far away.
            let probe = if mode == PositionMode::PulledString { pt(200.0, 0.0) } else { pt(2.0, 2.0) };
            assert!(s.push(probe, 16.0).is_some(), "state alive for {mode:?}");
        }
    }

    #[test]
    fn batch_equals_incremental() {
        // Determinism: one call per sample vs the same stream gives the same path.
        let inputs = [pt(0.0, 0.0), pt(4.0, 1.0), pt(9.0, 3.0), pt(9.0, 3.0), pt(15.0, 8.0)];
        for mode in [PositionMode::MovingAverage, PositionMode::Exponential, PositionMode::Inertia] {
            let mut a = PositionSmoother::new(cfg(mode));
            let mut b = PositionSmoother::new(cfg(mode));
            for p in inputs {
                let ra = a.push(p, 16.0);
                let rb = b.push(p, 16.0);
                assert_eq!(ra, rb, "mode {mode:?} diverged at {p:?}");
            }
            assert_eq!(a.finish(), b.finish());
        }
    }

    #[test]
    fn host_amount_mapping() {
        let c = PositionConfig::from_host_amount(PositionMode::PulledString, 0.4);
        assert_eq!(c.string_length, 40.0, "amount * 100px");
        let c = PositionConfig::from_host_amount(PositionMode::Exponential, 0.4);
        assert_eq!(c.smoothing_factor, 0.4, "host step a = 1 - amount");
        let c = PositionConfig::from_host_amount(PositionMode::PulledString, 99.0);
        assert_eq!(c.string_length, 100.0, "out-of-range clamps");
    }

    #[test]
    fn default_config_is_effectively_off() {
        // Host default (enabled, amount 0) paints through the pointer.
        let mut s = PositionSmoother::new(PositionConfig::default());
        for p in [pt(3.0, 4.0), pt(10.0, -2.0), pt(10.0, -2.0)] {
            assert_eq!(s.push(p, 16.0), Some(p));
        }
    }

    /// Real-device parity: recorded host `paint::dynamics::Smoother` outputs
    /// (generated in-tree via a throwaway `paint` test, 2026-10-15) replayed
    /// through [`PositionSmoother`]. Vectors are data, not code.
    #[test]
    fn host_stroke_fixture_parity() {
        // (in_x, in_y, host_out_x, host_out_y) at amount 0.4, pulled-string.
        const PULLED: &[[f32; 4]] = &[
            [0.0, 0.0, 0.0, 0.0],
            [50.0, 0.0, 10.0, 0.0],
            [100.0, 10.0, 60.2446, 5.5827],
            [150.0, -10.0, 110.59, -3.1578],
            [200.0, 5.0, 160.165, 1.3655],
        ];
        let mut s = PositionSmoother::new(PositionConfig::from_host_amount(
            PositionMode::PulledString,
            0.4,
        ));
        for row in PULLED {
            let out = s.push(pt(row[0], row[1]), SMOOTHING_STEP_MS);
            assert!(out.is_some_and(|o| near(o, pt(row[2], row[3]))), "row {row:?} gave {out:?}");
        }
        // Same, exponential at amount 0.4 (timeless pushes = one step each).
        const EXP: &[[f32; 4]] = &[
            [0.0, 0.0, 0.0, 0.0],
            [10.0, 4.0, 6.0, 2.4],
            [20.0, -3.0, 14.4, -0.84],
            [30.0, 5.0, 23.76, 2.664],
        ];
        let mut s = PositionSmoother::new(PositionConfig::from_host_amount(
            PositionMode::Exponential,
            0.4,
        ));
        for row in EXP {
            let out = s.push(pt(row[0], row[1]), 0.0);
            assert!(out.is_some_and(|o| near(o, pt(row[2], row[3]))), "row {row:?} gave {out:?}");
        }
    }
}
