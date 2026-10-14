//! Host ↔ `photocraft-stroke` parity: the `Smoother` laws must track the
//! ported point-stream laws they mirror (P0-2..P0-5). Timeless pushes (one
//! sub-step each) so both sides emit one point per push; `finish` tails are
//! excluded (host-only snap semantics). `f32` vs `f64` dust stays far below
//! the 1e-3 bound.

use photocraft_stroke::orientation::{OrientationConfig, OrientationSmoother, Pose};
use photocraft_stroke::position::{PositionConfig, PositionMode, PositionSmoother, Pt};
use photocraft_stroke::pressure::{PressureConfig, PressureMode, PressurePipe};

use crate::brush::Smoothing;
use crate::dynamics::Smoother;
use crate::StrokePoint;

fn near(a: f64, b: f32) -> bool {
    (a - f64::from(b)).abs() < 1e-3
}

fn drive_position(cfg: &Smoothing, pts: &[(f64, f64)]) -> Vec<StrokePoint> {
    let inputs: Vec<StrokePoint> = pts.iter().map(|&(x, y)| StrokePoint::new(x, y, 1.0)).collect();
    smooth_points_no_finish(cfg, &inputs)
}

/// Like [`smooth_points`] but without the end snap, matching the crate's
/// per-push laws (whose `finish` rules differ by mode on purpose).
fn smooth_points_no_finish(cfg: &Smoothing, pts: &[StrokePoint]) -> Vec<StrokePoint> {
    let mut s = Smoother::new(cfg, 1.0);
    let mut out = Vec::with_capacity(pts.len());
    for p in pts {
        s.push(*p, &mut out);
    }
    out
}

#[test]
fn pulled_string_tracks_the_crate() {
    let host = Smoothing { amount: 0.4, pulled_string: true, catch_up_on_end: false, ..Default::default() };
    let mut law = PositionSmoother::new(PositionConfig::from_host_amount(PositionMode::PulledString, 0.4));
    let pts = [(0.0, 0.0), (50.0, 0.0), (100.0, 10.0), (150.0, -10.0), (200.0, 5.0)];
    let got = drive_position(&host, &pts);
    assert_eq!(got.len(), pts.len());
    for (g, &(x, y)) in got.iter().zip(pts.iter()) {
        match law.push(Pt { x: x as f32, y: y as f32 }, 16.0) {
            Some(want) => {
                assert!(near(g.x, want.x) && near(g.y, want.y), "at ({x}, {y}): got ({}, {}), want {want:?}", g.x, g.y);
            }
            None => panic!("crate held at ({x}, {y}) while the host emitted ({}, {})", g.x, g.y),
        }
    }
}

#[test]
fn moving_average_tracks_the_crate() {
    let host = Smoothing { amount: 0.6, position_mode: 1, position_window: 5, catch_up_on_end: false, ..Default::default() };
    let mut law = PositionSmoother::new(PositionConfig {
        mode: PositionMode::MovingAverage,
        num_samples: 5,
        ..PositionConfig::default()
    });
    let pts: Vec<(f64, f64)> = (0..12).map(|i| (i as f64 * 7.0, if i % 2 == 0 { 0.0 } else { 5.0 })).collect();
    let got = drive_position(&host, &pts);
    assert_eq!(got.len(), pts.len());
    for (g, &(x, y)) in got.iter().zip(pts.iter()) {
        let want = law.push(Pt { x: x as f32, y: y as f32 }, 16.0).expect("MA always emits");
        assert!(near(g.x, want.x) && near(g.y, want.y), "at ({x}, {y}): got ({}, {}), want {want:?}", g.x, g.y);
    }
}

#[test]
fn exponential_tracks_the_crate() {
    let host = Smoothing { amount: 0.4, catch_up_on_end: false, ..Default::default() };
    let mut law = PositionSmoother::new(PositionConfig::from_host_amount(PositionMode::Exponential, 0.4));
    let pts = [(0.0, 0.0), (10.0, 4.0), (20.0, -3.0), (30.0, 5.0)];
    let got = drive_position(&host, &pts);
    assert_eq!(got.len(), pts.len());
    for (g, &(x, y)) in got.iter().zip(pts.iter()) {
        // Timeless pushes: one host sub-step == the crate's closed form.
        let want = law.push(Pt { x: x as f32, y: y as f32 }, 0.0).expect("exp always emits");
        assert!(near(g.x, want.x) && near(g.y, want.y), "at ({x}, {y}): got ({}, {}), want {want:?}", g.x, g.y);
    }
}

#[test]
fn inertia_tracks_the_crate() {
    let host = Smoothing { amount: 0.6, position_mode: 3, catch_up_on_end: false, ..Default::default() };
    let mut law = PositionSmoother::new(PositionConfig {
        mode: PositionMode::Inertia,
        mass: 30.0,
        drag: 0.2,
        ..PositionConfig::default()
    });
    let pts = [(0.0, 0.0), (30.0, 0.0), (60.0, 5.0), (90.0, -5.0)];
    let got = drive_position(&host, &pts);
    assert_eq!(got.len(), pts.len());
    for (g, &(x, y)) in got.iter().zip(pts.iter()) {
        let want = law.push(Pt { x: x as f32, y: y as f32 }, 16.0).expect("spring always emits");
        assert!(near(g.x, want.x) && near(g.y, want.y), "at ({x}, {y}): got ({}, {}), want {want:?}", g.x, g.y);
    }
}

#[test]
fn pressure_chain_tracks_the_crate() {
    // Windowed MA over an identity curve, no speed scale, on both sides.
    let host = Smoothing { amount: 0.6, pressure_smoothing: 3, pressure_mode: 2, ..Default::default() };
    let mut pipe = PressurePipe::new(PressureConfig {
        mode: PressureMode::MovingAverage,
        num_samples: 3,
        ..PressureConfig::default()
    });
    let qs = [0.5f32, 0.7, 0.6, 0.9, 0.8];
    // No pen-up sample here: the host restarts its window on exact 0 while the
    // crate pipe assumes a fresh pipe per stroke (both rules are tested on
    // their own sides).
    let inputs: Vec<StrokePoint> = qs.iter().map(|&q| StrokePoint::new(0.0, 0.0, q)).collect();
    let got = smooth_points_no_finish(&host, &inputs);
    assert_eq!(got.len(), qs.len());
    for (g, &q) in got.iter().zip(qs.iter()) {
        let want = pipe.push(q, 0.0);
        assert!((g.pressure - want).abs() < 1e-3, "at {q}: got {}, want {want}", g.pressure);
    }
}

#[test]
fn orientation_tracks_the_crate() {
    // Host `t = amount` == crate `a = 1 - factor` at factor `1 - t`.
    let host = Smoothing { amount: 0.6, orientation_smoothing: 0.5, ..Default::default() };
    let mut law = OrientationSmoother::new(OrientationConfig { enabled: true, smoothing_factor: 0.5 });
    let poses = [(10.0f32, 350.0f32), (12.0, 10.0), (14.0, 30.0)];
    let inputs: Vec<StrokePoint> = poses
        .iter()
        .map(|&(tx, r)| StrokePoint { tilt_x: tx, rotation: r, ..StrokePoint::new(0.0, 0.0, 1.0) })
        .collect();
    let got = smooth_points_no_finish(&host, &inputs);
    assert_eq!(got.len(), poses.len());
    for (g, &(tx, r)) in got.iter().zip(poses.iter()) {
        let want = law.push(Pose { tilt: tx, rotation: r });
        assert!((g.tilt_x - want.tilt).abs() < 1e-3 && (g.rotation - want.rotation).abs() < 1e-3,
            "at ({tx}, {r}): got ({}, {}), want {want:?}", g.tilt_x, g.rotation);
    }
}
