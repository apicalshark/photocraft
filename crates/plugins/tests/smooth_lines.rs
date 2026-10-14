//! Smooth Lines example plug-ins end to end (ported from the out-of-tree wasmi
//! harness, now through the host `Plugin` API): manifests, identity, wobble
//! reduction, radius ordering, alpha opt-in, hostile params.

use photocraft_color::{PixelFormat, SampleType};
use photocraft_geom::Rect;
use photocraft_plugins::{Kind, Limits, ParamSpec, Plugin};
use photocraft_raster::Surface;
use serde_json::json;

const SMOOTH: &[u8] = include_bytes!("fixtures/smooth-lines.wasm");
const ADVANCED: &[u8] = include_bytes!("fixtures/smooth-lines-advanced.wasm");

fn load(bytes: &[u8]) -> Plugin {
    Plugin::load(bytes, Limits::default()).unwrap()
}

/// A shaky dark line on a light opaque ground: smoothing moves luminance.
fn shaky(w: i32, h: i32) -> Surface {
    let mut s = Surface::new(PixelFormat::RGBA8);
    for x in 0..w {
        let c = h / 2 + ((x * 37) % 11) - 5 + if x % 2 == 0 { 2 } else { -2 };
        for y in 0..h {
            let d = (y - c).abs();
            let v = if d <= 1 { 0.05 } else if d == 2 { 0.5 } else { 1.0 };
            s.write_pixel(x, y, &[v, v, v, 1.0]);
        }
    }
    s
}

/// Centroid roughness: RMS of second differences of per-column darkness
/// centroids (what the harness called wobble RMS).
fn roughness(s: &Surface, w: i32, h: i32) -> f64 {
    let mut c = Vec::new();
    for x in 0..w {
        let (mut m, mut tot) = (0.0, 0.0);
        for y in 0..h {
            let a = 1.0 - f64::from(s.rgba(x, y)[0]);
            m += a * f64::from(y);
            tot += a;
        }
        if tot > 0.0 {
            c.push(m / tot);
        }
    }
    if c.len() < 3 {
        return 0.0;
    }
    let n = c.len() as f64;
    (c.windows(3).map(|w| (w[0] - 2.0 * w[1] + w[2]).powi(2)).sum::<f64>() / n).sqrt()
}

fn apply(p: &Plugin, s: &Surface, w: i32, h: i32, params: &serde_json::Value) -> Surface {
    p.apply(s, Rect::new(0, 0, w, h), None, params).unwrap()
}

fn param(params: &[(String, ParamSpec)], name: &str) -> ParamSpec {
    params.iter().find(|(k, _)| k == name).unwrap_or_else(|| panic!("no param `{name}`")).1.clone()
}

#[test]
fn manifests_describe_both_modules() {
    let m = load(SMOOTH).manifest().clone();
    assert_eq!(m.id, "io.github.storytold.stroke-lab.smooth");
    assert!(matches!(param(&m.params, "amount"), ParamSpec::Number { default, .. } if default == 30.0));
    assert!(matches!(param(&m.params, "preset"), ParamSpec::Choice { default, .. } if default.as_deref() == Some("Standard")));
    let m = load(ADVANCED).manifest().clone();
    assert_eq!(m.id, "io.github.storytold.stroke-lab.smooth-advanced");
    assert!(matches!(param(&m.params, "radius"), ParamSpec::Int { default, .. } if default == 2));
    assert!(matches!(param(&m.params, "smoothAlpha"), ParamSpec::Bool { default } if !default));
    for m in [load(SMOOTH).manifest(), load(ADVANCED).manifest()] {
        assert_eq!(m.kind, Kind::Filter);
        assert!(m.description.contains("post-process"), "honesty clause: {}", m.description);
    }
}

#[test]
fn amount_zero_is_identity_at_every_depth() {
    for bytes in [SMOOTH, ADVANCED] {
        let p = load(bytes);
        for sample in SampleType::ALL {
            let fmt = PixelFormat { sample, ..PixelFormat::RGBA8 };
            let mut s = Surface::new(fmt);
            for y in 0..16 {
                for x in 0..24 {
                    s.write_pixel(x, y, &[0.2, 0.4, 0.6, 0.8]);
                }
            }
            let out = apply(&p, &s, 24, 16, &json!({"amount": 0}));
            for y in 0..16 {
                for x in 0..24 {
                    assert_eq!(s.pixel(x, y), out.pixel(x, y), "{sample:?} ({x},{y})");
                }
            }
        }
    }
}

#[test]
fn standard_reduces_and_strong_halves_the_wobble() {
    let p = load(SMOOTH);
    let s = shaky(120, 40);
    let before = roughness(&s, 120, 40);
    let std = roughness(&apply(&p, &s, 120, 40, &json!({"amount": 100, "preset": "Standard"})), 120, 40);
    assert!(std < before * 0.85, "{before} -> {std}");
    let strong = roughness(&apply(&p, &s, 120, 40, &json!({"amount": 100, "preset": "Strong"})), 120, 40);
    assert!(strong < before * 0.65, "{before} -> {strong}");
    let gentle = roughness(&apply(&p, &s, 120, 40, &json!({"amount": 100, "preset": "Gentle"})), 120, 40);
    assert!(gentle < before && strong <= std, "{before} -> {gentle} -> {std} -> {strong}");
}

#[test]
fn advanced_radius_orders_smoothing() {
    let p = load(ADVANCED);
    let s = shaky(120, 40);
    let r2 = roughness(&apply(&p, &s, 120, 40, &json!({"radius": 2, "amount": 100})), 120, 40);
    let r8 = roughness(&apply(&p, &s, 120, 40, &json!({"radius": 8, "amount": 100})), 120, 40);
    assert!(r8 < r2, "{r2} -> {r8}");
}

#[test]
fn transparent_pixels_stay_without_smooth_alpha() {
    for bytes in [SMOOTH, ADVANCED] {
        let p = load(bytes);
        let s = shaky(60, 30);
        let out = apply(&p, &s, 60, 30, &json!({"amount": 100}));
        for y in 0..30 {
            for x in 0..60 {
                if s.rgba(x, y)[3] <= 0.0 {
                    assert_eq!(out.rgba(x, y)[3], 0.0, "({x},{y})");
                }
            }
        }
    }
}

#[test]
fn hostile_params_fall_back_without_panicking() {
    let p = load(SMOOTH);
    let s = shaky(40, 24);
    // Absurd amounts clamp and missing params take defaults. Re-running
    // agrees (deterministic). Wrong types and unknown choices are host
    // Params errors, not silent fallbacks.
    for params in [json!({"amount": 9999}), json!({"amount": -5}), json!({})] {
        let out = apply(&p, &s, 40, 24, &params);
        let again = apply(&p, &s, 40, 24, &params);
        for y in 0..24 {
            for x in 0..40 {
                assert_eq!(out.pixel(x, y), again.pixel(x, y), "deterministic {params}");
            }
        }
    }
    for params in [json!({"amount": "lots"}), json!({"preset": 42}), json!({"preset": "Turbo"})] {
        assert!(p.apply(&s, Rect::new(0, 0, 40, 24), None, &params).is_err(), "rejected: {params}");
    }
}
