//! The in-memory HDR rendition handed to the encoder, plus the conversion from
//! a developed linear RGB16 image to packed RGBA half-float.

use half::f16;
use libraw_dev::{ColorSpace, DevelopedImage};
use uhdr::ColorGamut;

/// libultrahdr's nominal half-float ceiling: 10000 nits / 203 nits (SDR white).
pub const HDR_MAX: f32 = 10000.0 / 203.0;

/// Maximum value of a developed 16-bit sample.
const SAMPLE_MAX: f32 = 65535.0;

/// Map a develop color space to the encoder's gamut tag.
pub fn gamut_for(color_space: ColorSpace) -> ColorGamut {
    match color_space {
        ColorSpace::Srgb => ColorGamut::UHDR_CG_BT_709,
        ColorSpace::DisplayP3 => ColorGamut::UHDR_CG_DISPLAY_P3,
        ColorSpace::Rec2020 => ColorGamut::UHDR_CG_BT_2100,
    }
}

/// An HDR rendition: packed RGBA half-float, linear, in `gamut`.
///
/// `data` is `width * height` pixels of four little-endian `f16` channels
/// (R, G, B, A) — the exact layout libultrahdr expects for
/// `UHDR_IMG_FMT_64bppRGBAHalfFloat`.
pub struct HdrImage {
    pub width: u32,
    pub height: u32,
    pub gamut: ColorGamut,
    pub data: Vec<u8>,
}

/// Convert a developed linear Rec.2020 RGB16 image into an HDR rendition.
///
/// `hdr_headroom` is the OQ-1 normalization anchor (DESIGN.md §5.2): the linear
/// multiplier between SDR diffuse white (mapped to 1.0) and sensor saturation.
/// A value of `h` maps a full-scale 16-bit sample to `h`, so samples brighter
/// than `1/h` of full scale become HDR highlights (> 1.0). Larger values grant
/// more headroom above SDR white.
pub fn hdr_from_developed(dev: &DevelopedImage, hdr_headroom: f32) -> HdrImage {
    let scale = hdr_headroom / SAMPLE_MAX;
    let pixels = dev.width as usize * dev.height as usize;

    let mut data = Vec::with_capacity(pixels * 4 * 2);
    let alpha = f16::from_f32(1.0).to_le_bytes();

    for rgb in dev.data.chunks_exact(DevelopedImage::CHANNELS) {
        for &sample in rgb {
            let v = (sample as f32 * scale).min(HDR_MAX);
            data.extend_from_slice(&f16::from_f32(v).to_le_bytes());
        }
        data.extend_from_slice(&alpha);
    }

    HdrImage {
        width: dev.width,
        height: dev.height,
        gamut: gamut_for(dev.color_space),
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_f16(bytes: &[u8], idx: usize) -> f32 {
        f16::from_le_bytes([bytes[idx * 2], bytes[idx * 2 + 1]]).to_f32()
    }

    #[test]
    fn maps_white_balance_anchor_and_alpha() {
        // 2x1 image: a mid sample and a full-scale (saturated) sample.
        let dev = DevelopedImage {
            width: 2,
            height: 1,
            color_space: ColorSpace::Rec2020,
            data: vec![16384, 16384, 16384, 65535, 65535, 65535],
        };
        let headroom = 4.0;
        let hdr = hdr_from_developed(&dev, headroom);

        assert_eq!(hdr.width, 2);
        assert_eq!(hdr.data.len(), 2 * 4 * 2); // 2 px * RGBA * f16

        // 16384/65535 * 4 ≈ 1.0 -> diffuse white near the SDR anchor.
        let mid_r = read_f16(&hdr.data, 0);
        assert!((mid_r - 1.0).abs() < 0.05, "mid sample ~1.0, got {mid_r}");

        // Saturated sample scales to the full headroom -> an HDR highlight.
        let hi_r = read_f16(&hdr.data, 4); // second pixel, R channel
        assert!((hi_r - headroom).abs() < 0.05, "hi sample ~{headroom}, got {hi_r}");

        // Alpha is 1.0 for both pixels.
        assert_eq!(read_f16(&hdr.data, 3), 1.0);
        assert_eq!(read_f16(&hdr.data, 7), 1.0);
    }

    #[test]
    fn clamps_to_hdr_max() {
        let dev = DevelopedImage {
            width: 1,
            height: 1,
            color_space: ColorSpace::Rec2020,
            data: vec![65535, 65535, 65535],
        };
        // Absurd headroom would exceed the nominal ceiling; must be clamped.
        let hdr = hdr_from_developed(&dev, 1000.0);
        assert!((read_f16(&hdr.data, 0) - HDR_MAX).abs() < 0.5);
    }
}
