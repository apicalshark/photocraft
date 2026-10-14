//! Stroke Lab: Smooth Lines (Advanced), a PhotoCraft filter plug-in (ABI v1,
//! through [`stroke_lab_photocraft`]).
//!
//! The same post-process line cleanup as Smooth Lines, with every control:
//! radius, edge preservation, alpha handling, and the blend amount. Same
//! honesty clause as the basic plug-in: a per-band after-the-fact filter,
//! **not** real-time stabilization while drawing.
#![no_std]

use stroke_lab_photocraft::{params, pixels::PixelParams};
use stroke_lab_photocraft::{Band, OK, plugin};

/// The manifest PhotoCraft reads at install (`pc_manifest`); parameters in dialog order. See
/// `smooth` for the naming and version conventions.
const MANIFEST: &str = concat!(
    r#"{
  "id": "io.github.storytold.stroke-lab.smooth-advanced",
  "name": "Stroke Lab: Smooth Lines (Advanced)",
  "version": ""#,
    env!("CARGO_PKG_VERSION"),
    r#"",
  "kind": "filter",
  "author": "stroke-lab",
  "description": "Cleans up shaky rasterized line art after the fact, with full controls: smooth radius, edge preservation, alpha handling and blend amount. A post-process filter, not real-time stabilization while drawing.",
  "params": {
    "radius":       {"type": "int", "min": 0, "max": 8, "default": 2},
    "preserveEdges": {"type": "number", "min": 0, "max": 100, "default": 50},
    "smoothAlpha":  {"type": "bool", "default": false},
    "amount":       {"type": "number", "min": 0, "max": 100, "default": 50}
  },
  "overlap": 8,
  "area": "content"
}"#
);

plugin!(MANIFEST, filter);

fn filter(mut band: Band) -> i32 {
    let json = band.json;
    let radius = libm::roundf(params::number_or(json, "radius", 2.0)).clamp(0.0, 8.0) as u32;
    let p = PixelParams {
        radius,
        preserve_edges: params::number_or(json, "preserveEdges", 50.0).clamp(0.0, 100.0),
        amount: params::number_or(json, "amount", 50.0).clamp(0.0, 100.0),
        smooth_alpha: params::boolean_or(json, "smoothAlpha", false),
    };
    band.smooth_lines(p);
    OK
}
