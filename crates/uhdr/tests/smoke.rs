//! M0 FFI smoke test: build a synthetic linear half-float HDR gradient and
//! encode it to an Ultra HDR JPEG via libultrahdr's API-0, then verify the
//! output is a valid Ultra HDR image.

use half::f16;
use uhdr::{ColorGamut, ColorRange, ColorTransfer, Encoder, ImgFmt, ImgLabel, RawImage};

/// Build a WxH RGBA half-float image, linear, with a horizontal gradient that
/// runs from SDR (1.0) up into the HDR range so a non-trivial gain map results.
fn synthetic_hdr_rgba_f16(w: usize, h: usize) -> Vec<u8> {
    // Nominal half-float range per the libultrahdr API is [0.0 .. 10000/203].
    const HDR_MAX: f32 = 10000.0 / 203.0;
    let mut buf = Vec::with_capacity(w * h * 4 * 2);
    for y in 0..h {
        for x in 0..w {
            let fx = x as f32 / (w.max(2) - 1) as f32; // 0..1 across width
            let fy = y as f32 / (h.max(2) - 1) as f32; // 0..1 down height
            // Base near SDR white scaling up toward HDR highlights on the right.
            let lum = 0.25 + fx * (HDR_MAX - 0.25);
            let r = lum;
            let g = lum * (0.5 + 0.5 * fy);
            let b = lum * 0.5;
            for c in [r, g, b, 1.0f32] {
                buf.extend_from_slice(&f16::from_f32(c).to_le_bytes());
            }
        }
    }
    buf
}

#[test]
fn encodes_synthetic_hdr_to_valid_ultrahdr() {
    let (w, h) = (64usize, 48usize);
    let data = synthetic_hdr_rgba_f16(w, h);

    let img = RawImage {
        width: w as u32,
        height: h as u32,
        fmt: ImgFmt::UHDR_IMG_FMT_64bppRGBAHalfFloat,
        gamut: ColorGamut::UHDR_CG_BT_2100,
        transfer: ColorTransfer::UHDR_CT_LINEAR,
        range: ColorRange::UHDR_CR_FULL_RANGE,
        stride_pixels: w as u32,
        data: &data,
    };

    let mut enc = Encoder::new().expect("create encoder");
    enc.set_raw_image(&img, ImgLabel::UHDR_HDR_IMG)
        .expect("set hdr raw image");
    enc.set_quality(95, ImgLabel::UHDR_BASE_IMG)
        .expect("set base quality");

    let jpeg = enc.encode().expect("encode ultra hdr");

    // Backward-compatible JPEG: starts with the SOI marker.
    assert!(jpeg.len() > 1000, "output suspiciously small: {} bytes", jpeg.len());
    assert_eq!(&jpeg[0..2], &[0xFF, 0xD8], "missing JPEG SOI marker");

    // And it must be a valid Ultra HDR image (primary + gain map + metadata).
    assert!(
        uhdr::is_ultrahdr(&jpeg),
        "encoded stream is not recognized as a valid Ultra HDR image"
    );
}
