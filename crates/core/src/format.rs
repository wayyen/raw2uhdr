//! Raw input-format recognition.
//!
//! LibRaw identifies raw files by *content*, not extension, and the develop path
//! ([`crate::develop`]) hands any file straight to it. This module exists only so
//! directory scans can pick raw files out of a folder, and so the set of formats
//! we advertise lives in one place. The list reflects the formats the pinned
//! LibRaw (0.22.1) decodes; explicitly-named files are still attempted by LibRaw
//! regardless of their extension.

use std::path::Path;

/// Raw file extensions (lowercase) LibRaw 0.22.1 decodes, grouped by vendor.
///
/// Used to select raw files when scanning directories. This is *not* a gate on
/// single, explicitly-named inputs — those are passed to LibRaw as-is, and
/// LibRaw is the authority on whether it can decode a given file.
pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    "dng", // Adobe / generic
    "cr2", "cr3", "crw", // Canon
    "nef", "nrw", // Nikon
    "arw", "srf", "sr2", // Sony
    "raf", // Fujifilm
    "orf", // Olympus / OM System
    "rw2", "raw", "rwl", // Panasonic / Leica
    "pef", "ptx", // Pentax
    "srw", // Samsung
    "x3f", // Sigma (Foveon)
    "3fr", "fff", // Hasselblad
    "iiq", "cap", "eip", // Phase One
    "mef", // Mamiya
    "mos", // Leaf
    "mrw", // Minolta
    "erf", // Epson
    "dcr", "kdc", "k25", "dcs", "drf", // Kodak
    "mdc", // Minolta / Agfa
    "gpr", // GoPro
    "bay", // Casio
    "ari", // ARRI
];

/// Whether `path`'s extension is a known raw format (case-insensitive).
pub fn is_supported_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| SUPPORTED_EXTENSIONS.iter().any(|s| e.eq_ignore_ascii_case(s)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn matches_known_extensions() {
        for ext in ["dng", "cr2", "cr3", "arw", "nef", "raf", "orf", "rw2", "x3f"] {
            assert!(
                is_supported_extension(Path::new(&format!("photo.{ext}"))),
                "{ext} should be recognized"
            );
        }
    }

    #[test]
    fn is_case_insensitive() {
        assert!(is_supported_extension(Path::new("IMG.ARW")));
        assert!(is_supported_extension(Path::new("IMG.Cr2")));
        assert!(is_supported_extension(Path::new("a/b/c.DNG")));
    }

    #[test]
    fn rejects_non_raw_and_missing_extension() {
        assert!(!is_supported_extension(Path::new("photo.jpg")));
        assert!(!is_supported_extension(Path::new("photo.png")));
        assert!(!is_supported_extension(Path::new("README")));
        assert!(!is_supported_extension(Path::new("noext")));
    }

    #[test]
    fn registry_is_lowercase_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for &ext in SUPPORTED_EXTENSIONS {
            assert_eq!(ext, ext.to_ascii_lowercase(), "{ext} must be lowercase");
            assert!(seen.insert(ext), "{ext} is duplicated");
        }
    }
}
