//! Core conversion pipeline for dng2uhdr.
//!
//! The pipeline is organized around trait boundaries (see `DESIGN.md` §6):
//! `RawSource` -> `Developer` -> `HdrEncoder` -> `OutputSink`. These are
//! fleshed out across M1-M3; this is the workspace skeleton.
