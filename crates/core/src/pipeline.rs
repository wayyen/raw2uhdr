//! Orchestrates develop -> convert -> encode for a single file.

use std::path::Path;

use thiserror::Error;

use crate::develop::{Developer, LibRawDeveloper};
use crate::encode::{HdrEncoder, UltraHdrEncoder};
use crate::image::hdr_from_developed;

/// Default HDR headroom (OQ-1): full-scale maps to 4x SDR white (2 stops).
pub const DEFAULT_HDR_HEADROOM: f32 = 4.0;

/// Errors spanning the whole conversion.
#[derive(Debug, Error)]
pub enum Error {
    #[error("develop stage failed: {0}")]
    Develop(#[from] crate::develop::DevelopError),
    #[error("encode stage failed: {0}")]
    Encode(#[from] uhdr::Error),
}

/// A conversion pipeline: a developer, an encoder, and the HDR anchor that
/// bridges them. Generic over the trait boundaries so backends can be swapped.
pub struct Pipeline<D: Developer, E: HdrEncoder> {
    pub developer: D,
    pub encoder: E,
    pub hdr_headroom: f32,
}

impl Default for Pipeline<LibRawDeveloper, UltraHdrEncoder> {
    fn default() -> Self {
        Pipeline {
            developer: LibRawDeveloper::default(),
            encoder: UltraHdrEncoder::default(),
            hdr_headroom: DEFAULT_HDR_HEADROOM,
        }
    }
}

impl<D: Developer, E: HdrEncoder> Pipeline<D, E> {
    /// Run the full pipeline on a raw file, returning the Ultra HDR JPEG bytes.
    pub fn run(&self, input: &Path) -> Result<Vec<u8>, Error> {
        tracing::debug!(?input, "developing raw");
        let developed = self.developer.develop(input)?;
        tracing::debug!(width = developed.width, height = developed.height, "developed");

        let hdr = hdr_from_developed(&developed, self.hdr_headroom);

        tracing::debug!("encoding ultra hdr");
        let jpeg = self.encoder.encode(&hdr)?;
        tracing::debug!(bytes = jpeg.len(), "encoded");
        Ok(jpeg)
    }
}
