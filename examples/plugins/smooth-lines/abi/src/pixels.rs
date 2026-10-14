//! Pixel post-process kernel for the WASM filters: a tent-weighted mean with
//! edge preservation, for cleaning up rasterized line art after the fact.
//!
//! This is **not** real-time stabilization (ABI v1 modules are stateless
//! per-band filters; see SPEC §6). The band's outer `radius` rows/columns
//! keep their values; a host that pads bands with an `overlap` gets seamless
//! results (same contract as the film-lab sharpen kernel this mirrors).
//!
//! - Only opaque pixels are touched; fully transparent pixels keep their
//!   values, and transparent neighbours lend no weight (no halos).
//! - The alpha channel itself is smoothed only when `smooth_alpha` is set
//!   (default off: alpha shape is preserved).
//! - Non-finite samples never propagate: a non-finite centre pixel is left
//!   alone, non-finite neighbours lend no weight.

/// How a band's samples are laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub width: usize,
    pub height: usize,
    /// Interleaved samples per pixel.
    pub channels: usize,
    /// How many of them are colour (the rest is alpha).
    pub colour: usize,
}

/// Smoothing parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PixelParams {
    /// Tent radius 0..8 px.
    pub radius: u32,
    /// 0..100: how strongly a luminance step resists blending (bilateral).
    pub preserve_edges: f32,
    /// 0..100: blend between the original pixel and the smoothed value.
    pub amount: f32,
    /// Also smooth the alpha channel (morphs soft shapes; default false).
    pub smooth_alpha: bool,
}

impl Default for PixelParams {
    fn default() -> Self {
        Self { radius: 2, preserve_edges: 50.0, amount: 50.0, smooth_alpha: false }
    }
}

/// Largest tent radius the kernel applies.
pub const MAX_RADIUS: u32 = 8;

/// Smooths `px` in place: a band laid out as `l` says, colour samples
/// encoded 0..1. `lum` must hold at least `width × height` values (the
/// kernel fills it with the input's luminance); `tmp` must hold at least
/// `width × height` values (one channel's horizontal pass, reused). A pixel
/// is skipped when `opaque` says so (alpha 0). Returns false (and changes
/// nothing) when the buffers are too small or the layout is invalid.
///
/// Two separable tent passes (horizontal, then vertical): `2 × (2r+1)` tap
/// visits per pixel instead of `(2r+1)²`, every channel riding along in one
/// visit, so the largest radius stays inside the host's per-sample
/// instruction budget. The edge term reads the *input* luminance in both
/// passes (a true bilateral filter would re-derive it after the first pass);
/// the difference is second-order next to the tent.
pub fn smooth(
    px: &mut [f32],
    lum: &mut [f32],
    tmp: &mut [f32],
    l: Layout,
    p: PixelParams,
    opaque: impl Fn(&[f32]) -> bool,
) -> bool {
    let Layout { width, height, channels, colour } = l;
    let amount = if p.amount.is_finite() { p.amount.clamp(0.0, 100.0) / 100.0 } else { 0.0 };
    if amount <= 0.0 {
        return true;
    }
    let radius = (p.radius.min(MAX_RADIUS)) as usize;
    if colour == 0 || colour > channels || channels == 0 || channels > MAX_CHANNELS {
        // The ABI admits grey (1) and RGB (3) only; the fixed lane buffers
        // below hold at most MAX_CHANNELS channels.
        return false;
    }
    let Some(n) = width.checked_mul(height) else { return false };
    let np = n.saturating_mul(channels);
    if px.len() < np || lum.len() < n || tmp.len() < np {
        return false;
    }
    if width < 2 * radius + 1 || height < 2 * radius + 1 {
        // No interior pixel: not an error.
        return true;
    }
    // Luminance of the input (mean of the colour channels) for the edge term.
    for (i, pix) in px.chunks_exact(channels).take(n).enumerate() {
        let mut sum = 0.0f32;
        let mut ok = true;
        for v in pix.iter().take(colour) {
            if !v.is_finite() {
                ok = false;
                break;
            }
            sum += *v;
        }
        if let Some(s) = lum.get_mut(i) {
            *s = if ok { sum / colour as f32 } else { f32::NAN };
        }
    }
    let edge_k = if p.preserve_edges.is_finite() { p.preserve_edges.clamp(0.0, 100.0) / 10.0 } else { 0.0 };
    // Alpha rides the same passes (plain tent, no edge term) but is written
    // back only when asked, last, so `opaque` keeps seeing the input alpha
    // throughout the colour work.
    pass(px, lum, tmp, l, radius, edge_k, amount, p.smooth_alpha, &opaque);
    true
}

/// Lane buffers for one tap visit: colour lanes plus the alpha lane.
const MAX_CHANNELS: usize = 4;

/// One separable all-channel pass: horizontal tent into `tmp` for every
/// pixel, then vertical tent from `tmp` back into `px`, interior pixels only.
#[allow(clippy::too_many_arguments)]
fn pass(
    px: &mut [f32],
    lum: &[f32],
    tmp: &mut [f32],
    l: Layout,
    radius: usize,
    edge_k: f32,
    amount: f32,
    smooth_alpha: bool,
    opaque: &impl Fn(&[f32]) -> bool,
) {
    let Layout { width, height, channels, colour } = l;
    let r = radius as isize;
    let lanes = channels.min(MAX_CHANNELS);
    let lum_at = |x: usize, y: usize| lum.get(y * width + x).copied().unwrap_or(f32::NAN);
    // Tap weight with the edge term; plain tent when preservation is off.
    let weight = |tent: f32, dl: f32| if edge_k == 0.0 { tent } else { tent / (1.0 + edge_k * dl) };
    // Horizontal pass over the whole band (margins included: the vertical
    // pass reads `radius` rows past the interior on each side).
    for y in 0..height {
        for x in 0..width {
            let lc = lum_at(x, y);
            let mut wsum = 0.0f32;
            let mut acc = [0.0f32; MAX_CHANNELS];
            let eligible = lc.is_finite() && pix_at(px, x, y, width, channels).is_some_and(opaque);
            if eligible {
                for dx in -r..=r {
                    let nx = (x as isize + dx).clamp(0, width as isize - 1) as usize;
                    let nl = lum_at(nx, y);
                    if !nl.is_finite() {
                        continue;
                    }
                    let Some(npix) = pix_at(px, nx, y, width, channels) else { continue };
                    if !opaque(npix) {
                        continue;
                    }
                    let w = weight(tent(radius, dx), (nl - lc).abs());
                    wsum += w;
                    for (lane, a) in acc.iter_mut().enumerate().take(lanes) {
                        *a += npix.get(lane).copied().unwrap_or(0.0) * w;
                    }
                }
            }
            let tb = (y * width + x) * channels;
            if eligible && wsum > 0.0 {
                if let Some(tslice) = tmp.get_mut(tb..tb + channels) {
                    for (t, a) in tslice.iter_mut().zip(&acc) {
                        *t = *a / wsum;
                    }
                }
            } else if let (Some(tslice), Some(cpix)) =
                (tmp.get_mut(tb..tb + channels), pix_at(px, x, y, width, channels))
            {
                // Transparent or non-finite centres pass through untouched.
                tslice.copy_from_slice(&cpix[..channels.min(cpix.len())]);
                for t in tslice.iter_mut().skip(cpix.len()) {
                    *t = 0.0;
                }
            }
        }
    }
    // Vertical pass, interior pixels only; the band's outer `radius` rows
    // keep their values for the host's overlap margins.
    for y in radius..height - radius {
        for x in radius..width - radius {
            let lc = lum_at(x, y);
            if !lc.is_finite() {
                continue;
            }
            let base = (y * width + x) * channels;
            let Some(centre) = px.get(base..base + channels) else { continue };
            if !opaque(centre) {
                continue;
            }
            let mut wsum = 0.0f32;
            let mut acc = [0.0f32; MAX_CHANNELS];
            for dy in -r..=r {
                let ny = (y as isize + dy).clamp(0, height as isize - 1) as usize;
                let nl = lum_at(x, ny);
                if !nl.is_finite() {
                    continue;
                }
                // Transparency still comes from the input band (alpha is
                // written back last, so it is intact while colours run).
                if !pix_at(px, x, ny, width, channels).is_some_and(opaque) {
                    continue;
                }
                let w = weight(tent(radius, dy), (nl - lc).abs());
                wsum += w;
                let tb = (ny * width + x) * channels;
                if let Some(tslice) = tmp.get(tb..tb + channels) {
                    for (a, t) in acc.iter_mut().zip(tslice).take(lanes) {
                        *a += *t * w;
                    }
                }
            }
            if wsum <= 0.0 {
                continue;
            }
            let Some(cpix) = px.get_mut(base..base + channels) else { continue };
            for (c, v) in cpix.iter_mut().take(colour).enumerate() {
                if let Some(a) = acc.get(c) {
                    *v = *v + (*a / wsum - *v) * amount;
                }
            }
            if smooth_alpha && channels > colour {
                // The alpha lane is `acc[colour]` (`colour == channels - 1`
                // whenever an alpha channel exists).
                if let (Some(dst), Some(a)) = (cpix.get_mut(channels - 1), acc.get(colour)) {
                    *dst = *dst + (*a / wsum - *dst) * amount;
                }
            }
        }
    }
}


/// One pixel's channel slice, or `None` when the arithmetic overflows the buffer.
fn pix_at(px: &[f32], x: usize, y: usize, width: usize, channels: usize) -> Option<&[f32]> {
    let b = (y.saturating_mul(width).saturating_add(x)).saturating_mul(channels);
    px.get(b..b.saturating_add(channels))
}

/// 1D tent weight at offset `d` for the given radius.
fn tent(radius: usize, d: isize) -> f32 {
    (radius + 1 - d.unsigned_abs()) as f32
}

#[cfg(test)]
mod tests {
    use std::vec;
    use std::vec::Vec;

    use super::*;

    fn layout(channels: usize, colour: usize) -> Layout {
        Layout { width: 5, height: 5, channels, colour }
    }

    fn params(radius: u32, preserve: f32, amount: f32) -> PixelParams {
        PixelParams { radius, preserve_edges: preserve, amount, smooth_alpha: false }
    }

    /// Scratch buffers for a 5 × 5 band: luminance plus a band-sized pass buffer.
    fn scratch(channels: usize) -> (Vec<f32>, Vec<f32>) {
        (vec![0.0; 25], vec![0.0; 25 * channels])
    }

    /// A 5 × 5 grey band, flat 0.5 with a bright centre pixel.
    fn band(centre: f32) -> Vec<f32> {
        let mut px = vec![0.5; 25];
        px[12] = centre;
        px
    }

    #[test]
    fn amount_zero_and_radius_zero_change_nothing() {
        let (mut lum, mut tmp) = scratch(1);
        let mut px = band(0.9);
        let before = px.clone();
        assert!(smooth(&mut px, &mut lum, &mut tmp, layout(1, 1), params(2, 0.0, 0.0), |_| true));
        assert_eq!(px, before);
        assert!(smooth(&mut px, &mut lum, &mut tmp, layout(1, 1), params(0, 0.0, 100.0), |_| true));
        assert_eq!(px, before, "a 1x1 window is the pixel itself");
    }

    #[test]
    fn spike_spreads_and_borders_hold() {
        let (mut lum, mut tmp) = scratch(1);
        let mut px = band(1.0);
        assert!(smooth(&mut px, &mut lum, &mut tmp, layout(1, 1), params(1, 0.0, 100.0), |_| true));
        assert!(px[12] < 1.0, "centre relaxes: {px:?}");
        assert!(px[11] > 0.5 && px[7] > 0.5, "tent shares with neighbours: {px:?}");
        assert_eq!(px[0], 0.5, "band edge untouched");
        assert_eq!(px[4], 0.5);
    }

    #[test]
    fn preserve_edges_resists_a_step() {
        // Left half 0.0, right half 1.0: pixel (2,2) sits on the step.
        let mut step: Vec<f32> = (0..25).map(|i| if i % 5 < 2 { 0.0 } else { 1.0 }).collect();
        let (mut lum, mut tmp) = scratch(1);
        let mut soft = step.clone();
        assert!(smooth(&mut soft, &mut lum, &mut tmp, layout(1, 1), params(1, 0.0, 100.0), |_| true));
        let mut kept = step.clone();
        assert!(smooth(&mut kept, &mut lum, &mut tmp, layout(1, 1), params(1, 100.0, 100.0), |_| true));
        assert!((soft[12] - 0.5).abs() < (kept[12] - 1.0).abs() || (kept[12] - 1.0).abs() < 0.35,
            "edge held: soft={} kept={}", soft[12], kept[12]);
        assert!(soft[12] < kept[12], "no preservation blends further: {} vs {}", soft[12], kept[12]);
        let _ = &mut step;
    }

    #[test]
    fn transparent_pixels_are_untouched_and_lend_no_weight() {
        // Opaque centre surrounded by transparent pixels: nothing to borrow.
        let (mut lum, mut tmp) = scratch(2);
        let mut px: Vec<f32> = (0..25).flat_map(|_| [0.9, 0.0]).collect();
        px[12 * 2] = 0.9;
        px[12 * 2 + 1] = 1.0;
        assert!(smooth(&mut px, &mut lum, &mut tmp, layout(2, 1), params(2, 0.0, 100.0), |p| p[1] > 0.0));
        assert!((px[12 * 2] - 0.9).abs() < 1e-5, "opaque centre with no opaque neighbours stays");
        // A transparent centre is never written.
        let mut px: Vec<f32> = (0..25).flat_map(|_| [0.9, 1.0]).collect();
        px[12 * 2 + 1] = 0.0;
        assert!(smooth(&mut px, &mut lum, &mut tmp, layout(2, 1), params(2, 0.0, 100.0), |p| p[1] > 0.0));
        assert_eq!(&px[24..26], &[0.9, 0.0]);
    }

    #[test]
    fn alpha_semantics_follow_smooth_alpha() {
        // RGBA: opaque bright centre, half-alpha neighbour.
        let mut px: Vec<f32> = (0..25).flat_map(|i| if i == 12 { [1.0, 1.0, 1.0, 1.0] } else { [0.5, 0.5, 0.5, 1.0] }).collect();
        px[11 * 4 + 3] = 0.5;
        let (mut lum, mut tmp) = scratch(4);
        let mut no_alpha = px.clone();
        assert!(smooth(&mut no_alpha, &mut lum, &mut tmp, layout(4, 3), params(1, 0.0, 100.0), |p| p[3] > 0.0));
        assert_eq!(no_alpha[12 * 4 + 3], 1.0, "alpha channel untouched by default");
        assert!(no_alpha[12 * 4] < 1.0, "colour still smoothed");
        let mut with_alpha = px.clone();
        let mut p = params(1, 0.0, 100.0);
        p.smooth_alpha = true;
        assert!(smooth(&mut with_alpha, &mut lum, &mut tmp, layout(4, 3), p, |p| p[3] > 0.0));
        assert!(with_alpha[12 * 4 + 3] < 1.0, "alpha smoothed when asked");
    }

    #[test]
    fn hostile_layouts_and_samples() {
        let (mut lum, mut tmp) = scratch(1);
        let mut px = band(0.9);
        assert!(!smooth(&mut px, &mut lum[..10], &mut tmp, layout(1, 1), params(1, 0.0, 100.0), |_| true), "short scratch");
        assert!(!smooth(&mut px[..20], &mut lum, &mut tmp, layout(1, 1), params(1, 0.0, 100.0), |_| true), "short pixels");
        assert!(!smooth(&mut px, &mut lum, &mut tmp, Layout { width: 5, height: 5, channels: 1, colour: 0 }, params(1, 0.0, 100.0), |_| true), "no colour");
        assert!(!smooth(&mut px, &mut lum, &mut tmp, Layout { width: 5, height: 5, channels: 1, colour: 5 }, params(1, 0.0, 100.0), |_| true), "too many colour");
        let mut tiny = vec![0.5; 4];
        assert!(smooth(&mut tiny, &mut lum, &mut tmp, Layout { width: 2, height: 2, channels: 1, colour: 1 }, params(1, 0.0, 100.0), |_| true),
            "no interior pixel is not an error");
        assert_eq!(tiny, vec![0.5; 4]);
        // A NaN centre is left alone and never propagates.
        let mut px = band(0.9);
        px[12] = f32::NAN;
        assert!(smooth(&mut px, &mut lum, &mut tmp, layout(1, 1), params(1, 0.0, 100.0), |_| true));
        assert!(px[12].is_nan());
        assert!(px.iter().enumerate().all(|(i, v)| i == 12 || *v == 0.5), "{px:?}");
    }

    #[test]
    fn amount_blends_old_and_new() {
        let (mut lum, mut tmp) = scratch(1);
        let mut full = band(1.0);
        assert!(smooth(&mut full, &mut lum, &mut tmp, layout(1, 1), params(1, 0.0, 100.0), |_| true));
        let mut half = band(1.0);
        assert!(smooth(&mut half, &mut lum, &mut tmp, layout(1, 1), params(1, 0.0, 50.0), |_| true));
        // Separable passes read fixed buffers, so amount blends exactly:
        // amount=50 lands on the midpoint of the full-strength result.
        assert!((half[12] - (1.0 + full[12]) / 2.0).abs() < 1e-4, "halfway: {} vs {}", half[12], full[12]);
    }
}
