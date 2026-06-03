//! End-to-end develop -> encode validation on a real DNG.
//!
//! Bring-your-own-DNG: set `DNG2UHDR_TEST_DNG` to a raw file, or drop a `*.dng`
//! into the workspace `samples/` directory. The test auto-skips when no sample
//! is available (e.g. in CI), since sample DNGs are not committed.

use std::path::PathBuf;

use dng2uhdr_core::Pipeline;

fn sample_dng() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("DNG2UHDR_TEST_DNG") {
        let p = PathBuf::from(p);
        return p.exists().then_some(p);
    }
    let samples = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../samples");
    std::fs::read_dir(samples)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("dng")))
}

#[test]
fn dng_converts_to_valid_ultrahdr() {
    let Some(dng) = sample_dng() else {
        eprintln!("SKIP: no sample DNG (set DNG2UHDR_TEST_DNG or add samples/*.dng)");
        return;
    };
    eprintln!("converting {}", dng.display());

    let jpeg = Pipeline::default().run(&dng).expect("conversion failed");

    assert!(jpeg.len() > 1000, "output suspiciously small: {} bytes", jpeg.len());
    assert_eq!(&jpeg[0..2], &[0xFF, 0xD8], "missing JPEG SOI marker");
    assert!(
        uhdr::is_ultrahdr(&jpeg),
        "output is a JPEG but not a valid Ultra HDR image (no gain map)"
    );

    // EXIF must round-trip out of the produced JPEG (Pipeline::default keeps it).
    let exif = exif::Reader::new()
        .read_from_container(&mut std::io::Cursor::new(&jpeg))
        .expect("output has no parseable EXIF");
    let make = exif
        .get_field(exif::Tag::Make, exif::In::PRIMARY)
        .expect("EXIF Make missing");
    eprintln!("EXIF Make = {}", make.display_value());
    // Orientation must be normal (1): LibRaw bakes the camera flip into pixels.
    if let Some(o) = exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY) {
        assert_eq!(o.value.get_uint(0), Some(1), "output orientation should be 1");
    }

    // Gain-map metadata sanity (M4 validation): the HDR capacity must reflect
    // our content-aware headroom — above SDR (1.0) and within the cap.
    let info = uhdr::probe(&jpeg).expect("probe gain-map metadata");
    eprintln!(
        "base {}x{}, gainmap {}x{}, hdr_capacity_max {:.3}, max_content_boost {:.3}",
        info.base_width, info.base_height, info.gainmap_width, info.gainmap_height,
        info.hdr_capacity_max, info.max_content_boost[0],
    );
    assert!(
        info.hdr_capacity_max > 1.0 && info.hdr_capacity_max <= dng2uhdr_core::DEFAULT_MAX_HEADROOM + 0.01,
        "hdr_capacity_max {} out of expected (1, {}] range",
        info.hdr_capacity_max, dng2uhdr_core::DEFAULT_MAX_HEADROOM
    );
    // Gain map is full-resolution (scale factor 1) and matches the base size.
    assert_eq!((info.gainmap_width, info.gainmap_height), (info.base_width, info.base_height));
}
