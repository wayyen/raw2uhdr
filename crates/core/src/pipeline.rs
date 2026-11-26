//! Orchestrates develop -> render -> encode for a single file.

use std::path::Path;

use thiserror::Error;

use crate::develop::{Developer, LibRawDeveloper};
use crate::encode::{HdrEncoder, UltraHdrEncoder};
use crate::render::{render, RenderOptions};

/// Errors spanning the whole conversion. The source error carries the detail;
/// these messages name the stage (avoids duplicating the cause in the chain).
#[derive(Debug, Error)]
pub enum Error {
    #[error("develop stage failed")]
    Develop(#[from] crate::develop::DevelopError),
    #[error("encode stage failed")]
    Encode(#[from] uhdr::Error),
}

/// A conversion pipeline: a developer, the renderer settings, and an encoder.
/// Generic over the trait boundaries so backends can be swapped.
pub struct Pipeline<D: Developer, E: HdrEncoder> {
    pub developer: D,
    pub render: RenderOptions,
    pub encoder: E,
    /// Carry the source's camera metadata into the output as EXIF.
    pub keep_exif: bool,
}

impl Default for Pipeline<LibRawDeveloper, UltraHdrEncoder> {
    fn default() -> Self {
        Pipeline {
            developer: LibRawDeveloper::default(),
            render: RenderOptions::default(),
            encoder: UltraHdrEncoder::default(),
            keep_exif: true,
        }
    }
}

impl<D: Developer, E: HdrEncoder> Pipeline<D, E> {
    /// Run the full pipeline on a raw file, returning the Ultra HDR JPEG bytes.
    pub fn run(&self, input: &Path) -> Result<Vec<u8>, Error> {
        tracing::debug!(?input, "developing raw");
        let developed = self.developer.develop(input)?;
        tracing::debug!(
            width = developed.width,
            height = developed.height,
            profile_curve = developed.tone_curve.is_some(),
            "developed"
        );

        let exif = self
            .keep_exif
            .then(|| libraw_dev::build_exif(&developed.metadata));

        let renditions = render(&developed, &self.render);
        drop(developed);
        tracing::debug!(headroom = renditions.headroom, "rendered");

        let jpeg = self.encoder.encode(&renditions, exif.as_deref())?;
        tracing::debug!(bytes = jpeg.len(), "encoded");
        Ok(jpeg)
    }
}
