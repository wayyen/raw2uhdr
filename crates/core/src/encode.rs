//! The encode stage: turning an HDR rendition into an Ultra HDR JPEG.
//!
//! `HdrEncoder` is the trait boundary (DESIGN.md §6); `UltraHdrEncoder` is the
//! v1 implementation using libultrahdr's API-0.

use uhdr::{ColorGamut, ColorRange, ColorTransfer, Encoder, ImgFmt, ImgLabel, RawImage};

use crate::image::HdrImage;

/// Encodes an HDR rendition into a final compressed container.
pub trait HdrEncoder {
    fn encode(&self, img: &HdrImage) -> Result<Vec<u8>, uhdr::Error>;
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
    fn encode(&self, img: &HdrImage) -> Result<Vec<u8>, uhdr::Error> {
        let raw = RawImage {
            width: img.width,
            height: img.height,
            fmt: ImgFmt::UHDR_IMG_FMT_64bppRGBAHalfFloat,
            gamut: ColorGamut::UHDR_CG_BT_2100,
            transfer: ColorTransfer::UHDR_CT_LINEAR,
            range: ColorRange::UHDR_CR_FULL_RANGE,
            stride_pixels: img.width,
            data: &img.data,
        };

        let mut enc = Encoder::new()?;
        enc.set_raw_image(&raw, ImgLabel::UHDR_HDR_IMG)?;
        enc.set_quality(self.quality, ImgLabel::UHDR_BASE_IMG)?;
        if self.gainmap_scale > 1 {
            enc.set_gainmap_scale_factor(self.gainmap_scale)?;
        }
        enc.encode()
    }
}
