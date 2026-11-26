//! Core conversion pipeline for raw2uhdr.
//!
//! Organized around trait boundaries (DESIGN.md §6): a [`develop::Developer`]
//! produces a scene-referred linear image, [`render::render`] turns it into
//! the SDR base and HDR renditions, and an [`encode::HdrEncoder`] emits the
//! Ultra HDR JPEG. [`pipeline::Pipeline`] wires these together for one file.

pub mod develop;
pub mod encode;
pub mod format;
pub mod pipeline;
pub mod render;

pub use develop::{Developer, Highlight, LibRawDeveloper, WhiteBalance};
pub use encode::{HdrEncoder, UltraHdrEncoder};
pub use format::{is_supported_extension, SUPPORTED_EXTENSIONS};
pub use libraw_dev::{
    ColorSpace, DevelopError, DevelopOptions, DevelopedImage, RawMetadata, DEFAULT_BLACK_CLIP,
};
pub use pipeline::{Error, Pipeline};
pub use render::{render, Headroom, RenderOptions, Renditions, ToneMode, DEFAULT_MAX_HEADROOM};
