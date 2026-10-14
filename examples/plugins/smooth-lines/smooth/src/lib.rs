//! Stroke Lab: Smooth Lines, a PhotoCraft filter plug-in (ABI v1, through
//! [`stroke_lab_photocraft`]).
//!
//! Post-process cleanup for shaky rasterized lines: a tent-weighted mean with
//! edge preservation, blended back over the original by `amount`. This is a
//! per-band after-the-fact filter, **not** real-time stabilization while
//! drawing (ABI v1 modules cannot intercept strokes); for that, see the host
//! integration proposal in `docs/stroke-lab-integration.md`.
//!
//! # The dialog
//!
//! One strength slider and three named modes. Everything else is internal;
//! the advanced plug-in (`advanced/`) exposes every control.
#![no_std]

use stroke_lab_photocraft::{params, pixels::PixelParams};
use stroke_lab_photocraft::{Band, OK, plugin};

/// The manifest PhotoCraft reads at install (`pc_manifest`); parameters in dialog order. No
/// parameter is named like a key of the host's `_image` object (see `params`): `_image`
/// carries `"mode"` (the colour mode), so the strength dropdown is `preset`, not `mode`. The
/// version is the workspace's, so a release tag only has to match `Cargo.toml`. The name has no
/// "…": PhotoCraft adds one to the menu entry of a plug-in with parameters, and shows the name
/// as it is in the dialog's title and the History panel.
const MANIFEST: &str = concat!(
    r#"{
  "id": "io.github.storytold.stroke-lab.smooth",
  "name": "Stroke Lab: Smooth Lines",
  "version": ""#,
    env!("CARGO_PKG_VERSION"),
    r#"",
  "kind": "filter",
  "author": "stroke-lab",
  "description": "Cleans up shaky rasterized line art after the fact: a small edge-preserving smooth blended back over the original. A post-process filter, not real-time stabilization while drawing.",
  "params": {
    "amount": {"type": "number", "min": 0, "max": 100, "default": 30},
    "preset": {"type": "choice", "options": ["Gentle", "Standard", "Strong"], "default": "Standard"}
  },
  "overlap": 2,
  "area": "content"
}"#
);

plugin!(MANIFEST, filter);

/// Strength presets: (radius, preserve_edges). Measured on the harness
/// jitter fixture (wobble RMS 1.156): Gentle 0.92, Standard 0.76,
/// Strong 0.51 -- Strong halves the wobble.
const MODES: &[(&str, (u32, f32))] =
    &[("Gentle", (1, 70.0)), ("Standard", (2, 50.0)), ("Strong", (3, 10.0))];

fn filter(mut band: Band) -> i32 {
    let json = band.json;
    let amount = params::number_or(json, "amount", 30.0).clamp(0.0, 100.0);
    let (radius, preserve_edges) =
        band.choice::<(u32, f32)>("preset", MODES).unwrap_or((2, 50.0));
    band.smooth_lines(PixelParams { radius, preserve_edges, amount, smooth_alpha: false });
    OK
}
