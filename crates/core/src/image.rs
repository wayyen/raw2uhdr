//! The in-memory HDR rendition handed to the encoder, plus the conversion from
//! a developed linear RGB16 image to packed RGBA half-float.

use half::f16;
use libraw_dev::{ColorSpace, DevelopedImage};
use uhdr::ColorGamut;

/// libultrahdr's nominal half-float ceiling: 10000 nits / 203 nits (SDR white).
pub const HDR_MAX: f32 = 10000.0 / 203.0;

/// Maximum value of a developed 16-bit sample.
const SAMPLE_MAX: f32 = 65535.0;

/// How to choose the HDR normalization anchor (OQ-1, DESIGN.md §5.2): the
/// linear value that full-scale (16-bit 65535) maps to. Larger = more HDR
/// headroom above SDR white (1.0).
#[derive(Debug, Clone, Copy)]
pub enum HdrAnchor {
    /// Fixed headroom: full-scale maps to this multiple of SDR white.
    Fixed(f32),
    /// Content-aware: anchor SDR white at the given luminance percentile of the
    /// image, so pixels above it become HDR highlights. The derived headroom is
    /// capped at `max_headroom` (and floored at 1.0).
    AutoPercentile { percentile: f32, max_headroom: f32 },
}

/// Resolve an anchor to the scalar headroom consumed by [`hdr_from_developed`].
pub fn resolve_headroom(dev: &DevelopedImage, anchor: HdrAnchor) -> f32 {
    match anchor {
        HdrAnchor::Fixed(h) => h.max(1.0),
        HdrAnchor::AutoPercentile { percentile, max_headroom } => {
            let anchor_val = luminance_percentile(dev, percentile).max(1.0);
            (SAMPLE_MAX / anchor_val).clamp(1.0, max_headroom)
        }
    }
}

/// Rec.2020/709/P3 luma weights for the developed gamut.
fn luma_weights(cs: ColorSpace) -> [f32; 3] {
    match cs {
        ColorSpace::Rec2020 => [0.2627, 0.6780, 0.0593],
        ColorSpace::Srgb => [0.2126, 0.7152, 0.0722],
        ColorSpace::DisplayP3 => [0.2290, 0.6917, 0.0793],
    }
}

/// The luminance value (0..=65535) at the given percentile (0..100) of the
/// developed image, via a 16-bit histogram.
fn luminance_percentile(dev: &DevelopedImage, percentile: f32) -> f32 {
    let [wr, wg, wb] = luma_weights(dev.color_space);
    let mut hist = vec![0u32; 65536];
    let mut total = 0u64;
    for rgb in dev.data.chunks_exact(DevelopedImage::CHANNELS) {
        let y = wr * rgb[0] as f32 + wg * rgb[1] as f32 + wb * rgb[2] as f32;
        hist[(y.round() as usize).min(65535)] += 1;
        total += 1;
    }
    if total == 0 {
        return SAMPLE_MAX;
    }
    let target = (total as f64 * (percentile as f64 / 100.0)).ceil() as u64;
    let mut cum = 0u64;
    for (value, &count) in hist.iter().enumerate() {
        cum += count as u64;
        if cum >= target {
            return value as f32;
        }
    }
    SAMPLE_MAX
}

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
    /// The headroom used during conversion (full-scale -> this linear value).
    /// The encoder uses it to set the target HDR peak brightness.
    pub headroom: f32,
    pub data: Vec<u8>,
}

/// SDR diffuse-white reference luminance in nits (per the Ultra HDR model).
pub const SDR_WHITE_NITS: f32 = 203.0;

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
        headroom: hdr_headroom,
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
            metadata: Default::default(),
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
    fn auto_anchor_maps_percentile_to_white() {
        // 99 mid-grey pixels + 1 full-scale highlight. With luma weights summing
        // to 1, grey luminance == 16384, so the 99th percentile is 16384 and the
        // derived headroom is 65535/16384 ≈ 4.0 (within the cap).
        let mut data = Vec::new();
        for _ in 0..99 {
            data.extend_from_slice(&[16384, 16384, 16384]);
        }
        data.extend_from_slice(&[65535, 65535, 65535]);
        let dev = DevelopedImage {
            width: 100,
            height: 1,
            color_space: ColorSpace::Rec2020,
            metadata: Default::default(),
            data,
        };
        let h = resolve_headroom(&dev, HdrAnchor::AutoPercentile { percentile: 99.0, max_headroom: 8.0 });
        assert!((h - 4.0).abs() < 0.1, "expected ~4.0, got {h}");

        // The cap is honored.
        let capped = resolve_headroom(&dev, HdrAnchor::AutoPercentile { percentile: 99.0, max_headroom: 2.0 });
        assert!((capped - 2.0).abs() < 1e-3, "expected cap 2.0, got {capped}");
    }

    #[test]
    fn clamps_to_hdr_max() {
        let dev = DevelopedImage {
            width: 1,
            height: 1,
            color_space: ColorSpace::Rec2020,
            metadata: Default::default(),
            data: vec![65535, 65535, 65535],
        };
        // Absurd headroom would exceed the nominal ceiling; must be clamped.
        let hdr = hdr_from_developed(&dev, 1000.0);
        assert!((read_f16(&hdr.data, 0) - HDR_MAX).abs() < 0.5);
    }
}
