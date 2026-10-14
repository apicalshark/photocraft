//! What the Stroke Lab PhotoCraft plug-ins share: the PhotoCraft WebAssembly plug-in ABI v1
//! (`photocraft/docs/plugins.md`; built like `photocraft/examples/plugins/invert-rs`).
//!
//! `no_std`, no allocator, no imports. An ABI v1 module is one manifest and one menu entry, so
//! each plug-in is its own `cdylib` crate: its manifest, and a function that reads its parameters
//! into [`photocraft_stroke`] settings. [`plugin!`] writes the four exports around them; [`Band`] is
//! the layer band the host hands over, and smooths it in place.
#![no_std]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod params;
pub mod pixels;


#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}

/// Writes the four ABI v1 exports of a plug-in crate: `pc_abi_version`, `pc_manifest` (the
/// `&'static str` given), `pc_alloc`, and `pc_filter`, which hands the band to the
/// `fn(Band) -> i32` given and returns its code ([`OK`], or a code the host reports as an error).
#[macro_export]
macro_rules! plugin {
    ($manifest:expr, $filter:path) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn pc_abi_version() -> u32 {
            1
        }

        /// `len << 32 | ptr` of the UTF-8 manifest JSON.
        #[unsafe(no_mangle)]
        pub extern "C" fn pc_manifest() -> u64 {
            let m: &'static str = $manifest;
            ((m.len() as u64) << 32) | m.as_ptr() as u64
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn pc_alloc(size: u32) -> u32 {
            $crate::alloc(size)
        }

        /// Smooths `width × height` pixels of `channels` interleaved `f32` samples at `buf` in place.
        ///
        /// # Safety
        /// The host guarantees `buf..buf + len` is a block it got from `pc_alloc` holding `len / 4`
        /// samples, and `params..params + params_len` the UTF-8 parameter JSON.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn pc_filter(
            buf: *mut f32,
            len: u32,
            width: u32,
            height: u32,
            channels: u32,
            format: u32,
            params: *const u8,
            params_len: u32,
        ) -> i32 {
            // SAFETY: the host's contract above is `Band::new`'s.
            match unsafe { $crate::Band::new(buf, len, width, height, channels, format, params, params_len) } {
                Ok(band) => $filter(band),
                Err(code) => code,
            }
        }
    };
}

/// Return codes of `pc_filter` (anything but 0 is reported by the host as an error).
pub const OK: i32 = 0;
pub const ERR_NO_CHANNELS: i32 = 1;
pub const ERR_UNSUPPORTED_MODE: i32 = 2;

/// Bit 8 of the `format` argument: the last channel is alpha.
const HAS_ALPHA: u32 = 1 << 8;

/// A block of `size` bytes: fresh pages from `memory.grow` (each run gets a fresh instance, so
/// nothing is ever freed). Returns 0 when the host's memory cap is reached.
pub fn alloc(size: u32) -> u32 {
    let pages = (size as usize).div_ceil(65536);
    let old = core::arch::wasm32::memory_grow(0, pages);
    if old == usize::MAX { 0 } else { (old * 65536) as u32 }
}

/// One band of the layer, as the host hands it to `pc_filter`.
pub struct Band<'a> {
    /// `width × height × channels` interleaved samples, margins included.
    pub px: &'a mut [f32],
    /// The parameter JSON: the manifest's parameters, validated, and `_image`.
    pub json: &'a [u8],
    pub width: usize,
    pub height: usize,
    /// Samples per pixel.
    pub channels: usize,
    /// Colour samples per pixel, 1 (grey) or 3 (RGB); the rest is alpha.
    pub colour: usize,
    pub alpha: bool,
}

impl Band<'_> {
    /// The band `pc_filter` was called with, or the code to return: [`ERR_NO_CHANNELS`] for an
    /// alpha-only band, [`ERR_UNSUPPORTED_MODE`] for CMYK and multichannel documents.
    ///
    /// # Safety
    /// `buf..buf + len` holds `len / 4` samples and `params..params + params_len` the parameter
    /// JSON, both valid for the rest of the call and referenced by nothing else.
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn new(
        buf: *mut f32,
        len: u32,
        width: u32,
        height: u32,
        channels: u32,
        format: u32,
        params: *const u8,
        params_len: u32,
    ) -> Result<Self, i32> {
        let n = channels as usize;
        let alpha = format & HAS_ALPHA != 0;
        let colour = if alpha { n.saturating_sub(1) } else { n };
        if colour == 0 {
            return Err(ERR_NO_CHANNELS);
        }
        if colour != 1 && colour != 3 {
            // CMYK / multichannel documents: no line art to clean up here.
            return Err(ERR_UNSUPPORTED_MODE);
        }
        // SAFETY: see the function's contract.
        let px = unsafe { core::slice::from_raw_parts_mut(buf, len as usize / 4) };
        // SAFETY: see the function's contract.
        let json = unsafe { core::slice::from_raw_parts(params, params_len as usize) };
        Ok(Self { px, json, width: width as usize, height: height as usize, channels: n, colour, alpha })
    }

    /// A `choice` parameter through `table` (the option's text as the manifest lists it, and
    /// what it stands for), or `None` when unset or not listed. The host only passes listed
    /// options, so the dialog's text can be words for people rather than the core's names.
    pub fn choice<T: Copy>(&self, key: &str, table: &[(&str, T)]) -> Option<T> {
        let v = params::string(self.json, key)?;
        table.iter().find(|(text, _)| *text == v).map(|(_, t)| *t)
    }

    /// Whether a pixel is smoothed: alpha 0 means nothing is there, and its values stay.
    fn opaque(alpha: bool, n: usize, pix: &[f32]) -> bool {
        !alpha || pix.get(n - 1).is_some_and(|a| *a > 0.0)
    }

    /// Smooths the band in place with `p` (the manifest then needs an
    /// `overlap` of at least the radius, for seamless bands), over scratch
    /// blocks grown for it; left unsmoothed when the host's memory cap is
    /// reached. Returns the code for `pc_filter`.
    pub fn smooth_lines(&mut self, p: pixels::PixelParams) -> i32 {
        let Some(count) = self.width.checked_mul(self.height) else { return OK };
        let Ok(lum_bytes) = u32::try_from(count.saturating_mul(4)) else { return OK };
        let tmp_samples = count.saturating_mul(self.channels);
        let Ok(tmp_bytes) = u32::try_from(tmp_samples.saturating_mul(4)) else { return OK };
        let lum_at = alloc(lum_bytes);
        let tmp_at = alloc(tmp_bytes);
        if lum_at == 0 || tmp_at == 0 {
            return OK;
        }
        // SAFETY: `alloc` returned fresh blocks of this module's memory that
        // nothing else references; `f32` has no invalid bit patterns and new pages are zeroed.
        let lum = unsafe { core::slice::from_raw_parts_mut(lum_at as *mut f32, count) };
        let tmp = unsafe { core::slice::from_raw_parts_mut(tmp_at as *mut f32, tmp_samples) };
        let (n, alpha) = (self.channels, self.alpha);
        let layout = pixels::Layout { width: self.width, height: self.height, channels: n, colour: self.colour };
        pixels::smooth(self.px, lum, tmp, layout, p, |pix| Self::opaque(alpha, n, pix));
        OK
    }
}
