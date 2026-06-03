//! Core conversion pipeline for dng2uhdr.
//!
//! Organized around trait boundaries (DESIGN.md §6): a [`develop::Developer`]
//! produces a developed linear image, [`image::hdr_from_developed`] converts it
//! to an HDR rendition, and an [`encode::HdrEncoder`] emits the Ultra HDR JPEG.
//! [`pipeline::Pipeline`] wires these together for a single file.

pub mod develop;
pub mod encode;
pub mod image;
pub mod pipeline;

pub use develop::{Developer, Highlight, LibRawDeveloper, WhiteBalance};
pub use encode::{HdrEncoder, UltraHdrEncoder};
pub use image::{hdr_from_developed, HdrImage};
pub use libraw_dev::{DevelopError, DevelopOptions, DevelopedImage};
pub use pipeline::{Error, Pipeline, DEFAULT_HDR_HEADROOM};
