//! The develop stage: turning a raw file into a developed linear image.
//!
//! `Developer` is the trait boundary (DESIGN.md §6); `LibRawDeveloper` is the
//! v1 implementation. Future raw backends implement the same trait.

use std::path::Path;

use libraw_dev::{develop_file, DevelopOptions, DevelopedImage};

pub use libraw_dev::{DevelopError, Highlight, WhiteBalance};

/// Develops a raw file into a linear, wide-gamut, high-bit-depth image.
pub trait Developer {
    fn develop(&self, path: &Path) -> Result<DevelopedImage, DevelopError>;
}

/// LibRaw-backed developer (DNG and other raw formats LibRaw supports).
#[derive(Debug, Clone, Default)]
pub struct LibRawDeveloper {
    pub options: DevelopOptions,
}

impl LibRawDeveloper {
    pub fn new(options: DevelopOptions) -> Self {
        LibRawDeveloper { options }
    }
}

impl Developer for LibRawDeveloper {
    fn develop(&self, path: &Path) -> Result<DevelopedImage, DevelopError> {
        develop_file(path, &self.options)
    }
}
