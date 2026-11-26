//! Safe LibRaw integration. Develops raw files — any format LibRaw supports
//! (DNG, CR2/CR3, NEF, ARW, RAF, ORF, RW2, …) — into a scene-referred, linear,
//! wide-gamut floating-point image ready for tone rendering.
//!
//! LibRaw does the decode, white balance and demosaic, emitting camera-native
//! RGB. Colour is then handled here so that DNGs render the way their embedded
//! camera profile intends (DESIGN.md §5.1): ForwardMatrix/ColorMatrix
//! interpolation for the scene white, OpcodeList2 lens-shading gain maps,
//! HueSatMap, ProfileGainTableMap (local tone mapping), BaselineExposure and
//! the LookTable — none of which LibRaw applies without the Adobe DNG SDK.
//! Non-DNG raws (or DNGs without a usable profile) fall back to LibRaw's
//! built-in camera matrix.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::path::Path;

use libraw_sys as sys;
use rayon::prelude::*;
use thiserror::Error;

pub mod color;
pub mod dng;
pub mod exif;
pub mod profile;

pub use color::ColorSpace;
pub use exif::{build_exif, RawMetadata};

use dng::{DngProfile, GainMapOpcode};
use profile::{ExposureRamp, GainTable};

/// Exposure (stops) applied to raws without a DNG BaselineExposure. Camera
/// raws are typically metered ~0.5 EV under to protect highlights; Adobe's
/// per-camera baselines cluster around this value.
pub const DEFAULT_BASELINE_EXPOSURE: f64 = 0.5;

/// Default shadow black clip (Adobe's default "Shadows" of 5). Applied even
/// when a DNG says `DefaultBlackRender = None`: measured against Pixel camera
/// JPEGs, whose shadows match this clip (5th-percentile L* within ~0.6), while
/// skipping it renders blacks visibly lifted.
pub const DEFAULT_BLACK_CLIP: f32 = 5.0;

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
#[derive(Debug, Clone, Default)]
pub enum WhiteBalance {
    /// As-shot camera white balance.
    #[default]
    Camera,
    /// LibRaw automatic white balance.
    Auto,
    /// Explicit RGBG multipliers.
    Custom([f32; 4]),
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

/// Development options. Defaults target the camera's intended rendering.
#[derive(Debug, Clone)]
pub struct DevelopOptions {
    pub white_balance: WhiteBalance,
    pub highlight: Highlight,
    /// LibRaw demosaic quality (`user_qual`); 3 = AHD.
    pub demosaic: i32,
    /// Exposure compensation in stops, on top of the camera's baseline.
    pub exposure_ev: f32,
    /// Apply the DNG's embedded camera profile (look table, local tone map,
    /// baseline exposure, lens shading). Off = neutral LibRaw colour.
    pub use_profile: bool,
    /// Shadow black clip in Adobe "Shadows" units (black = value * 0.001 of
    /// raw white, before exposure; scaled by DNG ShadowScale). 0 = none.
    pub black_clip: f32,
}

impl Default for DevelopOptions {
    fn default() -> Self {
        DevelopOptions {
            white_balance: WhiteBalance::Camera,
            highlight: Highlight::Clip,
            demosaic: 3,
            exposure_ev: 0.0,
            use_profile: true,
            black_clip: DEFAULT_BLACK_CLIP,
        }
    }
}

/// A developed image: scene-referred linear ProPhoto RGB (ROMM, D50).
///
/// `data` holds `width * height * 3` `f32` samples, row-major RGBRGB..., with
/// exposure applied: 1.0 is the level the tone curve maps to display white,
/// and values above it are highlight detail beyond SDR white (HDR headroom).
pub struct DevelopedImage {
    pub width: u32,
    pub height: u32,
    /// Camera metadata parsed from the raw file (for EXIF carry-over).
    pub metadata: RawMetadata,
    pub data: Vec<f32>,
    /// Per-pixel scene brightness — max(RGB) after exposure but *before* the
    /// profile's local tone mapping (ProfileGainTableMap), which compresses
    /// highlights into the SDR range. Drives the HDR highlight lift.
    pub scene_max: Vec<f32>,
    /// The profile's display tone curve as `(input, output)` pairs on [0, 1],
    /// when the raw carries one. `None` = renderer default.
    pub tone_curve: Option<Vec<(f32, f32)>>,
}

impl DevelopedImage {
    /// Number of color channels (always 3 for the develop path).
    pub const CHANNELS: usize = 3;
}

/// Develop a raw file at `path` into scene-linear ProPhoto RGB.
pub fn develop_file(path: &Path, opts: &DevelopOptions) -> Result<DevelopedImage> {
    let cpath = path
        .to_str()
        .and_then(|s| CString::new(s).ok())
        .ok_or(DevelopError::BadPath)?;

    let dng = opts
        .use_profile
        .then(|| dng::read_profile(path))
        .flatten()
        .filter(DngProfile::has_color);

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
        let dng = dng.filter(|_| (*ctx).idata.colors == 3);
        let flip = (*ctx).sizes.flip;
        // LibRaw's camera -> linear sRGB matrix, for the non-profile path.
        let rgb_cam = (*ctx).color.rgb_cam;

        // As-shot white from the profile (DNG semantics, incl. AsShotWhiteXY
        // and triple-illuminant interpolation) rather than LibRaw's
        // single-matrix approximation.
        let mut user_mul = None;
        if let (Some(p), WhiteBalance::Camera) = (&dng, &opts.white_balance) {
            if let Some(cc) = p.camera_color(p.white_xy()) {
                let cw = cc.camera_white;
                user_mul = Some([1.0 / cw[0], 1.0 / cw[1], 1.0 / cw[2], 1.0 / cw[1]].map(|m| m as f32));
            }
        }

        let gain_maps: &[GainMapOpcode] = match &dng {
            Some(p) if (*ctx).idata.filters != 0 => &p.opcode_gain_maps,
            _ => &[],
        };
        let mut scale = 1.0f64;
        if !gain_maps.is_empty() {
            if !matches!(opts.highlight, Highlight::Clip) {
                tracing::warn!("--highlight blend/rebuild has no effect with lens-shading gain maps; clipping");
            }
            let wb_spread = match (&user_mul, &opts.white_balance) {
                (Some(m), _) | (None, WhiteBalance::Custom(m)) => {
                    let (lo, hi) = (m.iter().cloned().fold(f32::MAX, f32::min), m.iter().cloned().fold(0.0, f32::max));
                    (hi / lo.max(1e-6)) as f64
                }
                _ => 4.0,
            };
            scale = apply_gain_maps(ctx, gain_maps, wb_spread);
        }

        configure(&mut (*ctx).params, opts, user_mul, !gain_maps.is_empty());

        check(sys::libraw_dcraw_process(ctx))?;

        // Effective (normalized) WB multipliers -> camera white for the matrix.
        let pre_mul = (*ctx).color.pre_mul;

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

        let (w, h) = (img.width as usize, img.height as usize);
        let count = w * h * 3;
        let bytes = std::slice::from_raw_parts(img.data.as_ptr(), img.data_size as usize);
        debug_assert!(bytes.len() >= count * 2);
        let raw: Vec<u16> =
            bytes[..count * 2].chunks_exact(2).map(|b| u16::from_ne_bytes([b[0], b[1]])).collect();

        let render = Render::new(dng.as_ref(), opts, pre_mul, rgb_cam, gain_maps, scale);
        let (data, scene_max) = render.run(&raw, w, h);
        let (data, ow, oh) = orient(data, w, h, 3, flip);
        let (scene_max, _, _) = orient(scene_max, w, h, 1, flip);
        let (w, h) = (ow, oh);

        Ok(DevelopedImage {
            width: w as u32,
            height: h as u32,
            metadata,
            data,
            scene_max,
            tone_curve: dng.as_ref().and_then(|p| p.tone_curve.clone()),
        })
    }
}

/// Apply OpcodeList2 `GainMap`s (lens/colour shading) to the raw CFA samples
/// in place. LibRaw's 16-bit pipeline would clip the brightened corners at
/// the white level, so samples are rescaled by `k` and LibRaw's maximum is
/// raised to leave room for the gains and the white-balance multipliers.
/// Returns the factor that maps LibRaw's output back to "1.0 = raw white".
unsafe fn apply_gain_maps(ctx: *mut sys::libraw_data_t, maps: &[GainMapOpcode], wb_spread: f64) -> f64 {
    let rd = &mut (*ctx).rawdata;
    if rd.raw_image.is_null() {
        return 1.0;
    }
    let s = rd.sizes;
    let (h, w) = (s.height as usize, s.width as usize);
    let stride = s.raw_pitch as usize / 2;
    let (top, left) = (s.top_margin as usize, s.left_margin as usize);
    let cblack = rd.color.cblack;
    let black_at = |row: usize, col: usize| -> f64 {
        let mut b = rd.color.black as f64;
        let (pr, pc) = (cblack[4] as usize, cblack[5] as usize);
        if pr > 0 && pc > 0 {
            b += cblack[6 + (row % pr) * pc + col % pc] as f64;
        }
        b
    };
    let white = rd.color.maximum as f64;
    let black0 = black_at(0, 0);
    let gmax = maps
        .iter()
        .flat_map(|m| m.gains.iter())
        .cloned()
        .fold(1.0f32, f32::max) as f64;
    let k = (65535.0 / ((white - black0).max(1.0) * gmax)).min(1.0);

    let raw = std::slice::from_raw_parts_mut(rd.raw_image, stride * s.raw_height as usize);
    for m in maps.iter().filter(|m| m.planes >= 1 && m.plane == 0) {
        let rows = (m.top as usize..(m.bottom as usize).min(h)).step_by(m.row_pitch as usize);
        for row in rows {
            let v = (row as f64 + 0.5) / h as f64;
            let line = &mut raw[(row + top) * stride..];
            for col in (m.left as usize..(m.right as usize).min(w)).step_by(m.col_pitch as usize) {
                let g = m.gain_at(v, (col as f64 + 0.5) / w as f64) as f64;
                let b = black_at(row, col);
                let px = &mut line[col + left];
                *px = (b + (*px as f64 - b).max(0.0) * g * k).round().min(65535.0) as u16;
            }
        }
    }
    // Leave room above the shaded white for the WB multipliers so LibRaw's
    // scale_colors does not clip; we clip neutrally ourselves (Render).
    let new_max = black0 + (white - black0) * k * gmax * wb_spread.max(1.0);
    rd.color.maximum = new_max.min(u32::MAX as f64) as u32;
    (new_max - black0) / ((white - black0) * k)
}

/// The scene-referred colour pipeline, per pixel.
struct Render<'a> {
    /// Camera (WB'd, 1.0 = white) -> linear ProPhoto.
    matrix: [[f32; 3]; 3],
    /// LibRaw output (0..65535) -> camera units where 1.0 = raw white.
    norm: f32,
    hue_sat: Option<dng::HsvTable>,
    look: Option<&'a dng::HsvTable>,
    gain_table: Option<GainTable<'a>>,
    ramp: ExposureRamp,
    /// Shading maps, for the per-pixel neutral clip level.
    shading: &'a [GainMapOpcode],
    /// Neutral highlight clip (camera units) in clip mode; `None` keeps
    /// LibRaw's blended/rebuilt highlights.
    clip: Option<f32>,
}

impl<'a> Render<'a> {
    fn new(
        dng: Option<&'a DngProfile>,
        opts: &DevelopOptions,
        pre_mul: [f32; 4],
        rgb_cam: [[f32; 4]; 3],
        shading: &'a [GainMapOpcode],
        scale: f64,
    ) -> Self {
        let user_ev = opts.exposure_ev as f64;

        // Camera white from the multipliers LibRaw actually used (covers
        // camera, auto and custom WB uniformly). LibRaw normalizes them by
        // their min in clip mode and by their max otherwise; dividing by the
        // min puts a clipping neutral at 1.0 either way.
        let pm = [pre_mul[0], pre_mul[1], pre_mul[2]].map(|v| v.max(1e-6) as f64);
        let pmin = pm.iter().cloned().fold(f64::MAX, f64::min);
        let camera_white = pm.map(|v| pmin / v);
        let norm = (scale / 65535.0 / pmin) as f32;
        let clip = matches!(opts.highlight, Highlight::Clip).then_some(1.0f32);

        if let Some(p) = dng {
            let white = p.neutral_to_xy(camera_white);
            if let Some(cc) = p.camera_color(white) {
                let m = color::mul(&cc.camera_to_prophoto, &color::diag(cc.camera_white));
                let be = p.baseline_exposure + p.baseline_exposure_offset;
                let black = opts.black_clip as f64 * 0.001 * p.shadow_scale;
                tracing::debug!(white_xy = ?white, baseline_ev = be, weights = ?cc.weights, "dng profile");
                return Render {
                    matrix: to_f32(&m),
                    norm,
                    hue_sat: p.hue_sat_map_for(&cc.weights),
                    look: p.look_table.as_ref(),
                    gain_table: p.gain_table_map.as_ref().map(|g| GainTable::new(g, be)),
                    ramp: ExposureRamp::new(be + user_ev, black),
                    shading,
                    clip,
                };
            }
        }

        // Fallback: LibRaw's camera -> sRGB matrix, re-expressed in ProPhoto.
        let srgb_to_prophoto = color::invert(&ColorSpace::Srgb.from_prophoto()).unwrap_or(color::IDENTITY);
        let cam: color::Mat3 = std::array::from_fn(|i| std::array::from_fn(|j| rgb_cam[i][j] as f64));
        let cam = if cam.iter().all(|r| r.iter().all(|v| *v == 0.0)) { color::IDENTITY } else { cam };
        Render {
            matrix: to_f32(&color::mul(&srgb_to_prophoto, &cam)),
            norm,
            hue_sat: None,
            look: None,
            gain_table: None,
            ramp: ExposureRamp::new(DEFAULT_BASELINE_EXPOSURE + user_ev, opts.black_clip as f64 * 0.001),
            shading: &[],
            clip,
        }
    }

    fn run(&self, raw: &[u16], w: usize, h: usize) -> (Vec<f32>, Vec<f32>) {
        let mut out = vec![0f32; w * h * 3];
        let mut peak = vec![0f32; w * h];
        out.par_chunks_mut(w * 3).zip(peak.par_chunks_mut(w)).enumerate().for_each(|(row, (line, pline))| {
            let v = (row as f64 + 0.5) / h as f64;
            let grow = self.gain_table.as_ref().map(|g| g.row(v));
            let src = &raw[row * w * 3..(row + 1) * w * 3];
            for col in 0..w {
                let u = (col as f64 + 0.5) / w as f64;
                // Neutral highlight clip: the lowest channel saturation level
                // (raw white scaled by the weakest shading gain here).
                let clip = match self.clip {
                    Some(base) if !self.shading.is_empty() => {
                        self.shading.iter().map(|m| m.gain_at(v, u)).fold(f32::MAX, f32::min).max(1.0) * base
                    }
                    Some(base) => base,
                    None => f32::MAX,
                };
                let s = &src[col * 3..col * 3 + 3];
                let cam = [0, 1, 2].map(|c| (s[c] as f32 * self.norm).min(clip));
                let m = &self.matrix;
                let mut p = [0, 1, 2].map(|i| (m[i][0] * cam[0] + m[i][1] * cam[1] + m[i][2] * cam[2]).max(0.0));
                if let Some(t) = &self.hue_sat {
                    p = profile::apply_hsv_table(t, p);
                }
                let mut local = 1.0;
                if let (Some(g), Some(gr)) = (&self.gain_table, grow) {
                    local = g.gain(gr, g.col(u), p);
                    p = p.map(|x| x * local);
                }
                p = p.map(|x| self.ramp.apply(x));
                // Undo the local gain to recover the scene brightness the
                // camera recorded (the ramp is linear above its black point).
                pline[col] = p[0].max(p[1]).max(p[2]) / local.max(1e-6);
                if let Some(t) = self.look {
                    p = profile::apply_hsv_table(t, p);
                }
                line[col * 3..col * 3 + 3].copy_from_slice(&p);
            }
        });
        (out, peak)
    }
}

fn to_f32(m: &color::Mat3) -> [[f32; 3]; 3] {
    m.map(|r| r.map(|v| v as f32))
}

/// Rotate/flip an interleaved f32 buffer of `n` channels by a LibRaw/dcraw
/// flip code (bit 4 = transpose, 2 = flip rows, 1 = flip columns), matching
/// `flip_index`.
fn orient(data: Vec<f32>, w: usize, h: usize, n: usize, flip: i32) -> (Vec<f32>, usize, usize) {
    if flip & 7 == 0 {
        return (data, w, h);
    }
    let (ow, oh) = if flip & 4 != 0 { (h, w) } else { (w, h) };
    let mut out = vec![0f32; data.len()];
    out.par_chunks_mut(ow * n).enumerate().for_each(|(orow, line)| {
        for ocol in 0..ow {
            let (mut r, mut c) = (orow, ocol);
            if flip & 4 != 0 {
                std::mem::swap(&mut r, &mut c);
            }
            if flip & 2 != 0 {
                r = h - 1 - r;
            }
            if flip & 1 != 0 {
                c = w - 1 - c;
            }
            let i = (r * w + c) * n;
            line[ocol * n..ocol * n + n].copy_from_slice(&data[i..i + n]);
        }
    });
    (out, ow, oh)
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
unsafe fn configure(
    params: &mut sys::libraw_output_params_t,
    opts: &DevelopOptions,
    profile_mul: Option<[f32; 4]>,
    raised_maximum: bool,
) {
    // Fixed policy: linear, 16-bit, camera-native colour (we do colour
    // ourselves), unrotated (spatial profile maps are in sensor orientation;
    // we rotate at the end), no auto-bright.
    params.output_bps = 16;
    params.gamm[0] = 1.0;
    params.gamm[1] = 1.0;
    params.no_auto_bright = 1;
    params.output_color = 0;
    params.user_flip = 0;
    params.user_qual = opts.demosaic;
    params.highlight = opts.highlight.to_libraw();
    if raised_maximum {
        // We raised `maximum` deliberately; don't let LibRaw pull it back.
        params.adjust_maximum_thr = 0.0;
    }

    params.use_camera_wb = 0;
    params.use_auto_wb = 0;
    match (&opts.white_balance, profile_mul) {
        (WhiteBalance::Camera, Some(mul)) => params.user_mul = mul,
        (WhiteBalance::Camera, None) => params.use_camera_wb = 1,
        (WhiteBalance::Auto, _) => params.use_auto_wb = 1,
        (WhiteBalance::Custom(mul), _) => params.user_mul = *mul,
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
