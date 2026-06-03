//! Orchestrates develop -> convert -> encode for a single file.

use std::path::Path;

use thiserror::Error;

use crate::develop::{Developer, LibRawDeveloper};
use crate::encode::{HdrEncoder, UltraHdrEncoder};
use crate::image::{hdr_from_developed, resolve_headroom, HdrAnchor};

/// Default fixed HDR headroom when a fixed anchor is requested (2 stops).
pub const DEFAULT_HDR_HEADROOM: f32 = 4.0;

/// Default luminance percentile for the content-aware anchor.
pub const DEFAULT_AUTO_PERCENTILE: f32 = 99.0;

/// Default cap on auto-derived headroom (3 stops).
pub const DEFAULT_MAX_HEADROOM: f32 = 8.0;

/// The default anchor: content-aware (OQ-1 resolution).
pub const DEFAULT_HDR_ANCHOR: HdrAnchor = HdrAnchor::AutoPercentile {
    percentile: DEFAULT_AUTO_PERCENTILE,
    max_headroom: DEFAULT_MAX_HEADROOM,
};

/// Errors spanning the whole conversion. The source error carries the detail;
/// these messages name the stage (avoids duplicating the cause in the chain).
#[derive(Debug, Error)]
pub enum Error {
    #[error("develop stage failed")]
    Develop(#[from] crate::develop::DevelopError),
    #[error("encode stage failed")]
    Encode(#[from] uhdr::Error),
}

/// A conversion pipeline: a developer, an encoder, and the HDR anchor that
/// bridges them. Generic over the trait boundaries so backends can be swapped.
pub struct Pipeline<D: Developer, E: HdrEncoder> {
    pub developer: D,
    pub encoder: E,
    pub hdr_anchor: HdrAnchor,
    /// Carry the source's camera metadata into the output as EXIF.
    pub keep_exif: bool,
}

impl Default for Pipeline<LibRawDeveloper, UltraHdrEncoder> {
    fn default() -> Self {
        Pipeline {
            developer: LibRawDeveloper::default(),
            encoder: UltraHdrEncoder::default(),
            hdr_anchor: DEFAULT_HDR_ANCHOR,
            keep_exif: true,
        }
    }
}

impl<D: Developer, E: HdrEncoder> Pipeline<D, E> {
    /// Run the full pipeline on a raw file, returning the Ultra HDR JPEG bytes.
    pub fn run(&self, input: &Path) -> Result<Vec<u8>, Error> {
        tracing::debug!(?input, "developing raw");
        let developed = self.developer.develop(input)?;
        tracing::debug!(width = developed.width, height = developed.height, "developed");

        let exif = self
            .keep_exif
            .then(|| libraw_dev::build_exif(&developed.metadata));

        let headroom = resolve_headroom(&developed, self.hdr_anchor);
        tracing::debug!(headroom, "resolved hdr headroom");
        let hdr = hdr_from_developed(&developed, headroom);

        tracing::debug!("encoding ultra hdr");
        let jpeg = self.encoder.encode(&hdr, exif.as_deref())?;
        tracing::debug!(bytes = jpeg.len(), "encoded");
        Ok(jpeg)
    }
}
