//! The encode stage: turning an HDR rendition into an Ultra HDR JPEG.
//!
//! `HdrEncoder` is the trait boundary (DESIGN.md §6); `UltraHdrEncoder` is the
//! v1 implementation using libultrahdr's API-0.

use uhdr::{ColorRange, ColorTransfer, Encoder, ImgFmt, ImgLabel, RawImage};

use crate::image::{HdrImage, SDR_WHITE_NITS};

/// Encodes an HDR rendition into a final compressed container.
///
/// `exif`, when present, is an EXIF payload (`Exif\0\0` + TIFF) to embed.
pub trait HdrEncoder {
    fn encode(&self, img: &HdrImage, exif: Option<&[u8]>) -> Result<Vec<u8>, uhdr::Error>;
}

/// Ultra HDR encoder (API-0): libultrahdr derives the SDR base and gain map
/// from the HDR rendition.
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
    fn encode(&self, img: &HdrImage, exif: Option<&[u8]>) -> Result<Vec<u8>, uhdr::Error> {
        let raw = RawImage {
            width: img.width,
            height: img.height,
            fmt: ImgFmt::UHDR_IMG_FMT_64bppRGBAHalfFloat,
            gamut: img.gamut,
            transfer: ColorTransfer::UHDR_CT_LINEAR,
            range: ColorRange::UHDR_CR_FULL_RANGE,
            stride_pixels: img.width,
            data: &img.data,
        };

        let mut enc = Encoder::new()?;
        enc.set_raw_image(&raw, ImgLabel::UHDR_HDR_IMG)?;
        enc.set_quality(self.quality, ImgLabel::UHDR_BASE_IMG)?;

        // Tie the gain map's HDR capacity to the headroom we actually produced.
        // For a linear HDR intent libultrahdr otherwise leaves hdr_capacity_max
        // at the 10000-nit (~49x) encoding ceiling, which makes displays apply
        // only a fraction of the intended boost (DESIGN.md §5.2). The per-pixel
        // gains already reflect our headroom; this aligns the capacity signal.
        if img.headroom > 1.0 {
            let nits = (img.headroom * SDR_WHITE_NITS).clamp(SDR_WHITE_NITS, 10_000.0);
            enc.set_target_display_peak_brightness(nits)?;
        }

        if self.gainmap_scale > 1 {
            enc.set_gainmap_scale_factor(self.gainmap_scale)?;
        }
        if let Some(exif) = exif {
            enc.set_exif(exif)?;
        }
        enc.encode()
    }
}
