//! Safe, idiomatic Rust wrapper over libultrahdr (`uhdr-sys`).
//!
//! This crate exposes a small RAII surface around the libultrahdr encoder. The
//! design intent (see `DESIGN.md`) is API-0: hand libultrahdr a high-fidelity
//! HDR rendition and let it derive the SDR base image and gain map.

use std::ffi::c_void;
use std::os::raw::c_char;

use thiserror::Error;

// Re-export the libultrahdr enums as the public color/format vocabulary.
pub use uhdr_sys::{
    uhdr_color_gamut as ColorGamut, uhdr_color_range as ColorRange,
    uhdr_color_transfer as ColorTransfer, uhdr_img_fmt as ImgFmt, uhdr_img_label as ImgLabel,
};

/// Error returned by libultrahdr, carrying its detail string when present.
#[derive(Debug, Error)]
#[error("libultrahdr error {code:?}: {detail}")]
pub struct Error {
    pub code: uhdr_sys::uhdr_codec_err,
    pub detail: String,
}

impl Error {
    fn from_info(info: uhdr_sys::uhdr_error_info_t) -> Self {
        let detail = if info.has_detail != 0 {
            c_str_to_string(info.detail.as_ptr(), info.detail.len())
        } else {
            String::new()
        };
        Error { code: info.error_code, detail }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

fn check(info: uhdr_sys::uhdr_error_info_t) -> Result<()> {
    if matches!(info.error_code, uhdr_sys::uhdr_codec_err::UHDR_CODEC_OK) {
        Ok(())
    } else {
        Err(Error::from_info(info))
    }
}

fn c_str_to_string(ptr: *const c_char, max: usize) -> String {
    let mut bytes = Vec::new();
    for i in 0..max {
        let b = unsafe { *ptr.add(i) } as u8;
        if b == 0 {
            break;
        }
        bytes.push(b);
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// A borrowed, packed raw image handed to the encoder for a given intent.
///
/// For the v1 pipeline this describes a 64bpp RGBA half-float, linear,
/// BT.2100-gamut HDR rendition (a single packed plane).
pub struct RawImage<'a> {
    pub width: u32,
    pub height: u32,
    pub fmt: ImgFmt,
    pub gamut: ColorGamut,
    pub transfer: ColorTransfer,
    pub range: ColorRange,
    /// Stride between rows, in pixels (not bytes), for the packed plane.
    pub stride_pixels: u32,
    /// Tightly/row-packed pixel data for plane 0.
    pub data: &'a [u8],
}

/// RAII handle around a libultrahdr encoder instance.
pub struct Encoder {
    ptr: *mut uhdr_sys::uhdr_codec_private_t,
}

impl Encoder {
    /// Create a new encoder initialized with libultrahdr defaults.
    pub fn new() -> Result<Self> {
        let ptr = unsafe { uhdr_sys::uhdr_create_encoder() };
        if ptr.is_null() {
            return Err(Error {
                code: uhdr_sys::uhdr_codec_err::UHDR_CODEC_MEM_ERROR,
                detail: "uhdr_create_encoder returned null".into(),
            });
        }
        Ok(Encoder { ptr })
    }

    /// Register a raw image for the given intent (e.g. `ImgLabel::UHDR_HDR_IMG`).
    ///
    /// The pixel data must remain valid until [`Encoder::encode`] returns; the
    /// borrow checker enforces this through the `'_` lifetime on `img`.
    pub fn set_raw_image(&mut self, img: &RawImage<'_>, intent: ImgLabel) -> Result<()> {
        // Validate the buffer is large enough for the declared geometry.
        let bytes_per_pixel = bytes_per_pixel(img.fmt);
        let needed = img.stride_pixels as usize * img.height as usize * bytes_per_pixel;
        assert!(
            img.data.len() >= needed,
            "raw image buffer too small: have {}, need {}",
            img.data.len(),
            needed
        );

        let mut raw: uhdr_sys::uhdr_raw_image_t = unsafe { std::mem::zeroed() };
        raw.fmt = img.fmt;
        raw.cg = img.gamut;
        raw.ct = img.transfer;
        raw.range = img.range;
        raw.w = img.width;
        raw.h = img.height;
        raw.planes[0] = img.data.as_ptr() as *mut c_void;
        raw.stride[0] = img.stride_pixels;

        let info = unsafe { uhdr_sys::uhdr_enc_set_raw_image(self.ptr, &mut raw, intent) };
        check(info)
    }

    /// Set the JPEG quality factor (0-100) for the given intent.
    pub fn set_quality(&mut self, quality: i32, intent: ImgLabel) -> Result<()> {
        let info = unsafe { uhdr_sys::uhdr_enc_set_quality(self.ptr, quality, intent) };
        check(info)
    }

    /// Set the gain-map downscale factor (1 = full resolution).
    pub fn set_gainmap_scale_factor(&mut self, factor: i32) -> Result<()> {
        let info = unsafe { uhdr_sys::uhdr_enc_set_gainmap_scale_factor(self.ptr, factor) };
        check(info)
    }

    /// Set the target display peak brightness in nits (range [203, 10000]).
    ///
    /// For a linear/PQ HDR intent libultrahdr otherwise assumes a 10000-nit
    /// peak, which pins the gain map's HDR capacity to its maximum regardless of
    /// content. Setting this to `203 * headroom` ties the encoded HDR capacity
    /// to the headroom we actually produced.
    pub fn set_target_display_peak_brightness(&mut self, nits: f32) -> Result<()> {
        let info =
            unsafe { uhdr_sys::uhdr_enc_set_target_display_peak_brightness(self.ptr, nits) };
        check(info)
    }

    /// Choose a three-channel (per R/G/B) or single-channel gain map.
    pub fn set_multi_channel_gainmap(&mut self, multi: bool) -> Result<()> {
        let info =
            unsafe { uhdr_sys::uhdr_enc_set_using_multi_channel_gainmap(self.ptr, multi as i32) };
        check(info)
    }

    /// Recommend the min/max content boost (linear) for the gain map.
    pub fn set_min_max_content_boost(&mut self, min: f32, max: f32) -> Result<()> {
        let info = unsafe { uhdr_sys::uhdr_enc_set_min_max_content_boost(self.ptr, min, max) };
        check(info)
    }

    /// Provide EXIF data to embed in the output.
    ///
    /// `data` is the EXIF payload as it appears after the APP1 marker+length,
    /// i.e. it must begin with the `Exif\0\0` identifier followed by the TIFF
    /// structure. libultrahdr writes the APP1 marker and length itself and
    /// places the segment correctly relative to its gain-map metadata.
    ///
    /// The data must remain valid until [`Encoder::encode`] returns.
    pub fn set_exif(&mut self, data: &[u8]) -> Result<()> {
        let mut block = uhdr_sys::uhdr_mem_block_t {
            data: data.as_ptr() as *mut c_void,
            data_sz: data.len(),
            capacity: data.len(),
        };
        let info = unsafe { uhdr_sys::uhdr_enc_set_exif_data(self.ptr, &mut block) };
        check(info)
    }

    /// Run the encode and return an owned copy of the Ultra HDR JPEG bytes.
    pub fn encode(&mut self) -> Result<Vec<u8>> {
        check(unsafe { uhdr_sys::uhdr_encode(self.ptr) })?;
        let stream = unsafe { uhdr_sys::uhdr_get_encoded_stream(self.ptr) };
        if stream.is_null() {
            return Err(Error {
                code: uhdr_sys::uhdr_codec_err::UHDR_CODEC_UNKNOWN_ERROR,
                detail: "uhdr_get_encoded_stream returned null".into(),
            });
        }
        let stream = unsafe { &*stream };
        let bytes =
            unsafe { std::slice::from_raw_parts(stream.data as *const u8, stream.data_sz) };
        Ok(bytes.to_vec())
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe { uhdr_sys::uhdr_release_encoder(self.ptr) };
    }
}

/// Returns true if `data` is a valid Ultra HDR image (has primary + gain map +
/// gain-map metadata).
pub fn is_ultrahdr(data: &[u8]) -> bool {
    unsafe { uhdr_sys::is_uhdr_image(data.as_ptr() as *mut c_void, data.len() as i32) == 1 }
}

/// Gain-map metadata and image geometry, read back from an Ultra HDR stream.
#[derive(Debug, Clone)]
pub struct GainMapInfo {
    pub base_width: i32,
    pub base_height: i32,
    pub gainmap_width: i32,
    pub gainmap_height: i32,
    /// Per-channel maximum content boost (linear).
    pub max_content_boost: [f32; 3],
    /// Per-channel minimum content boost (linear).
    pub min_content_boost: [f32; 3],
    pub gamma: [f32; 3],
    pub hdr_capacity_min: f32,
    pub hdr_capacity_max: f32,
}

/// Parse (without fully decoding pixels) an Ultra HDR stream and return its
/// gain-map metadata and geometry. Useful for validating encoder output.
pub fn probe(data: &[u8]) -> Result<GainMapInfo> {
    unsafe {
        let dec = uhdr_sys::uhdr_create_decoder();
        if dec.is_null() {
            return Err(Error {
                code: uhdr_sys::uhdr_codec_err::UHDR_CODEC_MEM_ERROR,
                detail: "uhdr_create_decoder returned null".into(),
            });
        }
        let _dec = DecoderHandle(dec);

        let mut img = uhdr_sys::uhdr_compressed_image_t {
            data: data.as_ptr() as *mut c_void,
            data_sz: data.len(),
            capacity: data.len(),
            cg: uhdr_sys::uhdr_color_gamut::UHDR_CG_UNSPECIFIED,
            ct: uhdr_sys::uhdr_color_transfer::UHDR_CT_UNSPECIFIED,
            range: uhdr_sys::uhdr_color_range::UHDR_CR_UNSPECIFIED,
        };
        check(uhdr_sys::uhdr_dec_set_image(dec, &mut img))?;
        check(uhdr_sys::uhdr_dec_probe(dec))?;

        let meta = uhdr_sys::uhdr_dec_get_gainmap_metadata(dec);
        if meta.is_null() {
            return Err(Error {
                code: uhdr_sys::uhdr_codec_err::UHDR_CODEC_ERROR,
                detail: "no gain-map metadata".into(),
            });
        }
        let meta = &*meta;

        Ok(GainMapInfo {
            base_width: uhdr_sys::uhdr_dec_get_image_width(dec),
            base_height: uhdr_sys::uhdr_dec_get_image_height(dec),
            gainmap_width: uhdr_sys::uhdr_dec_get_gainmap_width(dec),
            gainmap_height: uhdr_sys::uhdr_dec_get_gainmap_height(dec),
            max_content_boost: meta.max_content_boost,
            min_content_boost: meta.min_content_boost,
            gamma: meta.gamma,
            hdr_capacity_min: meta.hdr_capacity_min,
            hdr_capacity_max: meta.hdr_capacity_max,
        })
    }
}

/// RAII handle for a decoder instance.
struct DecoderHandle(*mut uhdr_sys::uhdr_codec_private_t);
impl Drop for DecoderHandle {
    fn drop(&mut self) {
        unsafe { uhdr_sys::uhdr_release_decoder(self.0) };
    }
}

fn bytes_per_pixel(fmt: ImgFmt) -> usize {
    match fmt {
        ImgFmt::UHDR_IMG_FMT_64bppRGBAHalfFloat => 8,
        ImgFmt::UHDR_IMG_FMT_32bppRGBA8888 | ImgFmt::UHDR_IMG_FMT_32bppRGBA1010102 => 4,
        ImgFmt::UHDR_IMG_FMT_24bppRGB888 => 3,
        // Conservative default for packed formats not yet used by the pipeline.
        _ => 4,
    }
}
