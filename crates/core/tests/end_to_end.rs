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
}
