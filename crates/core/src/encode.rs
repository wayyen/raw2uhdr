//! The encode stage: turning the renditions into an Ultra HDR JPEG.
//!
//! `HdrEncoder` is the trait boundary (DESIGN.md §6); `UltraHdrEncoder` is the
//! implementation using libultrahdr's API-1: we supply both the HDR rendition
//! and our own SDR base, and libultrahdr computes the gain map between them
//! and muxes the JPEG. (API-0 — letting libultrahdr tonemap the SDR itself —
//! produced a flat base that never reached white; see DESIGN.md OQ-5.)

use uhdr::{ColorRange, ColorTransfer, Encoder, ImgFmt, ImgLabel, RawImage};

use crate::render::{Renditions, SDR_WHITE_NITS};

/// Encodes renditions into a final compressed container.
///
/// `exif`, when present, is an EXIF payload (`Exif\0\0` + TIFF) to embed.
pub trait HdrEncoder {
    fn encode(&self, img: &Renditions, exif: Option<&[u8]>) -> Result<Vec<u8>, uhdr::Error>;
}

/// Ultra HDR encoder (API-1).
#[derive(Debug, Clone)]
pub struct UltraHdrEncoder {
    /// Base-image JPEG quality (0-100).
    pub quality: i32,
    /// Gain-map downscale factor (1 = full resolution).
    pub gainmap_scale: i32,
}

impl Default for UltraHdrEncoder {
    fn default() -> Self {
        UltraHdrEncoder { quality: 95, gainmap_scale: 1 }
    }
}

impl HdrEncoder for UltraHdrEncoder {
    fn encode(&self, img: &Renditions, exif: Option<&[u8]>) -> Result<Vec<u8>, uhdr::Error> {
        let hdr = RawImage {
            width: img.width,
            height: img.height,
            fmt: ImgFmt::UHDR_IMG_FMT_64bppRGBAHalfFloat,
            gamut: img.gamut,
            transfer: ColorTransfer::UHDR_CT_LINEAR,
            range: ColorRange::UHDR_CR_FULL_RANGE,
            stride_pixels: img.width,
            data: &img.hdr,
        };
        let sdr = RawImage {
            width: img.width,
            height: img.height,
            fmt: ImgFmt::UHDR_IMG_FMT_32bppRGBA8888,
            gamut: img.gamut,
            transfer: ColorTransfer::UHDR_CT_SRGB,
            range: ColorRange::UHDR_CR_FULL_RANGE,
            stride_pixels: img.width,
            data: &img.sdr,
        };

        let mut enc = Encoder::new()?;
        enc.set_raw_image(&hdr, ImgLabel::UHDR_HDR_IMG)?;
        enc.set_raw_image(&sdr, ImgLabel::UHDR_SDR_IMG)?;
        enc.set_quality(self.quality, ImgLabel::UHDR_BASE_IMG)?;
        // The HDR rendition is a scalar multiple of the SDR base (same hue and
        // saturation), so one gain channel carries everything at a third of
        // the size.
        enc.set_multi_channel_gainmap(false)?;

        // Tie the gain map's HDR capacity to the headroom we actually rendered.
        // For a linear HDR intent libultrahdr otherwise assumes a 10000-nit
        // (~49x) capacity, so displays would apply only a fraction of the
        // boost (DESIGN.md §5.2).
        let nits = (img.headroom * SDR_WHITE_NITS).clamp(SDR_WHITE_NITS, 10_000.0);
        enc.set_target_display_peak_brightness(nits)?;
        // By construction HDR = SDR x boost with boost in [1, headroom]. Pin the
        // gain range to that so 8-bit rounding noise in near-black pixels
        // (HDR/SDR ratios of 0.5x-5x at code values 0-2) doesn't stretch the
        // gain map's range and cost it precision.
        enc.set_min_max_content_boost(1.0, img.headroom.max(1.01))?;

        if self.gainmap_scale > 1 {
            enc.set_gainmap_scale_factor(self.gainmap_scale)?;
        }
        if let Some(exif) = exif {
            enc.set_exif(exif)?;
        }
        enc.encode()
    }
}
