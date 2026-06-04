//! Safe LibRaw integration. Develops raw files — any format LibRaw supports
//! (DNG, CR2/CR3, NEF, ARW, RAF, ORF, RW2, …) — into a linear, wide-gamut,
//! high-bit-depth image suitable for HDR encoding.
//!
//! The development settings follow `DESIGN.md` §5.1: linear gamma, Rec.2020
//! output primaries, 16-bit output, no auto-brightening — so the result is
//! scene-linear data in a wide gamut, ready to be normalized to half-float.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::path::Path;

use libraw_sys as sys;
use thiserror::Error;

pub mod exif;
pub use exif::{build_exif, RawMetadata};

/// Errors from the develop stage.
#[derive(Debug, Error)]
pub enum DevelopError {
    #[error("path is not valid for LibRaw (contains interior NUL or invalid UTF-8)")]
    BadPath,
    #[error("LibRaw error {code}: {msg}")]
    LibRaw { code: i32, msg: String },
    #[error("LibRaw returned a null pointer")]
    Null,
    #[error("unexpected developed image format: colors={colors}, bits={bits} (want 3x16)")]
    BadFormat { colors: u16, bits: u16 },
}

type Result<T> = std::result::Result<T, DevelopError>;

/// White-balance selection for development.
#[derive(Debug, Clone)]
pub enum WhiteBalance {
    /// As-shot camera white balance.
    Camera,
    /// LibRaw automatic white balance.
    Auto,
    /// Explicit RGBG multipliers.
    Custom([f32; 4]),
}

impl Default for WhiteBalance {
    fn default() -> Self {
        WhiteBalance::Camera
    }
}

/// Highlight handling mode (maps to LibRaw's `highlight` parameter).
#[derive(Debug, Clone, Copy)]
pub enum Highlight {
    /// Clip highlights (LibRaw 0).
    Clip,
    /// Blend clipped and unclipped (LibRaw 2).
    Blend,
    /// Reconstruct highlights (LibRaw 5).
    Rebuild,
}

impl Highlight {
    fn to_libraw(self) -> i32 {
        match self {
            Highlight::Clip => 0,
            Highlight::Blend => 2,
            Highlight::Rebuild => 5,
        }
    }
}

/// Output color space (primaries). The develop path always emits linear data;
/// this only selects the gamut. Downstream maps it to the encoder's gamut tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorSpace {
    /// sRGB / BT.709 primaries.
    Srgb,
    /// Display-P3 primaries (LibRaw DCI-P3).
    DisplayP3,
    /// Rec.2020 / BT.2020 primaries.
    Rec2020,
}

impl ColorSpace {
    /// LibRaw `output_color` code.
    fn to_libraw(self) -> i32 {
        match self {
            ColorSpace::Srgb => 1,
            ColorSpace::DisplayP3 => 7, // DCI-P3
            ColorSpace::Rec2020 => 8,
        }
    }
}

impl Default for ColorSpace {
    fn default() -> Self {
        ColorSpace::Rec2020
    }
}

/// Development options. Defaults target faithful, near-default development.
#[derive(Debug, Clone)]
pub struct DevelopOptions {
    pub white_balance: WhiteBalance,
    pub highlight: Highlight,
    /// Output gamut (primaries). Data stays linear regardless.
    pub color_space: ColorSpace,
    /// LibRaw demosaic quality (`user_qual`); 3 = AHD.
    pub demosaic: i32,
    /// Exposure compensation in stops (0.0 = none). Applied in scene-linear
    /// space before tone/output handling.
    pub exposure_ev: f32,
}

impl Default for DevelopOptions {
    fn default() -> Self {
        DevelopOptions {
            white_balance: WhiteBalance::Camera,
            highlight: Highlight::Clip,
            color_space: ColorSpace::Rec2020,
            demosaic: 3,
            exposure_ev: 0.0,
        }
    }
}

/// A developed image: interleaved RGB, 16-bit, linear, in `color_space`'s
/// primaries.
///
/// `data` holds `width * height * 3` samples in row-major RGBRGB... order,
/// host-endian, scene-linear with no tone curve applied.
pub struct DevelopedImage {
    pub width: u32,
    pub height: u32,
    pub color_space: ColorSpace,
    /// Camera metadata parsed from the raw file (for EXIF carry-over).
    pub metadata: RawMetadata,
    pub data: Vec<u16>,
}

impl DevelopedImage {
    /// Number of color channels (always 3 for the develop path).
    pub const CHANNELS: usize = 3;
}

/// Develop a raw file at `path` into a linear Rec.2020 16-bit image.
pub fn develop_file(path: &Path, opts: &DevelopOptions) -> Result<DevelopedImage> {
    let cpath = path
        .to_str()
        .and_then(|s| CString::new(s).ok())
        .ok_or(DevelopError::BadPath)?;

    // SAFETY: all pointers below are checked for null and freed via guards.
    unsafe {
        let ctx = sys::libraw_init(0);
        if ctx.is_null() {
            return Err(DevelopError::Null);
        }
        let _ctx = CtxGuard(ctx);

        check(sys::libraw_open_file(ctx, cpath.as_ptr()))?;
        check(sys::libraw_unpack(ctx))?;

        let metadata = extract_metadata(&*ctx);

        configure(&mut (*ctx).params, opts);

        check(sys::libraw_dcraw_process(ctx))?;

        let mut errc: i32 = 0;
        let img = sys::libraw_dcraw_make_mem_image(ctx, &mut errc);
        check(errc)?;
        if img.is_null() {
            return Err(DevelopError::Null);
        }
        let _img = MemImageGuard(img);
        let img = &*img;

        if img.colors != 3 || img.bits != 16 {
            return Err(DevelopError::BadFormat { colors: img.colors, bits: img.bits });
        }

        let count = img.width as usize * img.height as usize * img.colors as usize;
        let bytes = std::slice::from_raw_parts(img.data.as_ptr(), img.data_size as usize);
        debug_assert!(bytes.len() >= count * 2);

        let mut data = vec![0u16; count];
        for (i, sample) in data.iter_mut().enumerate() {
            *sample = u16::from_ne_bytes([bytes[2 * i], bytes[2 * i + 1]]);
        }

        Ok(DevelopedImage {
            width: img.width as u32,
            height: img.height as u32,
            color_space: opts.color_space,
            metadata,
            data,
        })
    }
}

/// Read curated camera metadata from a LibRaw context (after `unpack`).
unsafe fn extract_metadata(ctx: &sys::libraw_data_t) -> RawMetadata {
    let pos = |v: f32| (v > 0.0).then_some(v);
    RawMetadata {
        make: c_array_to_string(ctx.idata.make.as_ptr(), ctx.idata.make.len()),
        model: c_array_to_string(ctx.idata.model.as_ptr(), ctx.idata.model.len()),
        software: c_array_to_string(ctx.idata.software.as_ptr(), ctx.idata.software.len()),
        timestamp: (ctx.other.timestamp != 0).then_some(ctx.other.timestamp as i64),
        exposure_time: pos(ctx.other.shutter),
        fnumber: pos(ctx.other.aperture),
        iso: pos(ctx.other.iso_speed),
        focal_len: pos(ctx.other.focal_len),
    }
}

/// Convert a fixed-size C char array (NUL-terminated) to a `String`.
unsafe fn c_array_to_string(ptr: *const c_char, max: usize) -> String {
    let mut len = 0;
    while len < max && *ptr.add(len) != 0 {
        len += 1;
    }
    let bytes = std::slice::from_raw_parts(ptr as *const u8, len);
    String::from_utf8_lossy(bytes).trim().to_string()
}

/// Apply our fixed development policy plus user options to LibRaw params.
unsafe fn configure(params: &mut sys::libraw_output_params_t, opts: &DevelopOptions) {
    // Fixed policy: linear, 16-bit, no auto-bright (DESIGN.md §5.1).
    params.output_bps = 16;
    params.gamm[0] = 1.0; // linear power
    params.gamm[1] = 1.0; // linear toe slope
    params.no_auto_bright = 1;
    params.output_color = opts.color_space.to_libraw();
    params.user_qual = opts.demosaic;
    params.highlight = opts.highlight.to_libraw();

    // Exposure compensation in scene-linear space (2^EV), applied before
    // highlight/output handling. exp_shift==1.0 is a no-op.
    if opts.exposure_ev != 0.0 {
        params.exp_correc = 1;
        params.exp_shift = 2.0f32.powf(opts.exposure_ev);
        params.exp_preser = 1.0; // preserve highlights while shifting exposure
    }

    // White balance.
    params.use_camera_wb = 0;
    params.use_auto_wb = 0;
    match &opts.white_balance {
        WhiteBalance::Camera => params.use_camera_wb = 1,
        WhiteBalance::Auto => params.use_auto_wb = 1,
        WhiteBalance::Custom(mul) => params.user_mul = *mul,
    }
}

/// Translate a LibRaw return code into a `Result`.
fn check(code: i32) -> Result<()> {
    if code == 0 {
        return Ok(());
    }
    // SAFETY: libraw_strerror returns a static C string for any code.
    let msg = unsafe {
        CStr::from_ptr(sys::libraw_strerror(code)).to_string_lossy().into_owned()
    };
    Err(DevelopError::LibRaw { code, msg })
}

/// RAII guard that closes the LibRaw context.
struct CtxGuard(*mut sys::libraw_data_t);
impl Drop for CtxGuard {
    fn drop(&mut self) {
        unsafe { sys::libraw_close(self.0) };
    }
}

/// RAII guard that frees a processed memory image.
struct MemImageGuard(*mut sys::libraw_processed_image_t);
impl Drop for MemImageGuard {
    fn drop(&mut self) {
        unsafe { sys::libraw_dcraw_clear_mem(self.0) };
    }
}
