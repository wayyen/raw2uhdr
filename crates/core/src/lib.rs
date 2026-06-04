//! Core conversion pipeline for raw2uhdr.
//!
//! Organized around trait boundaries (DESIGN.md §6): a [`develop::Developer`]
//! produces a developed linear image, [`image::hdr_from_developed`] converts it
//! to an HDR rendition, and an [`encode::HdrEncoder`] emits the Ultra HDR JPEG.
//! [`pipeline::Pipeline`] wires these together for a single file.

pub mod develop;
pub mod encode;
pub mod format;
pub mod image;
pub mod pipeline;

pub use develop::{Developer, Highlight, LibRawDeveloper, WhiteBalance};
pub use encode::{HdrEncoder, UltraHdrEncoder};
pub use format::{is_supported_extension, SUPPORTED_EXTENSIONS};
pub use image::{hdr_from_developed, resolve_headroom, HdrAnchor, HdrImage};
pub use libraw_dev::{ColorSpace, DevelopError, DevelopOptions, DevelopedImage, RawMetadata};
pub use pipeline::{
    Error, Pipeline, DEFAULT_AUTO_PERCENTILE, DEFAULT_HDR_ANCHOR, DEFAULT_HDR_HEADROOM,
    DEFAULT_MAX_HEADROOM,
};
