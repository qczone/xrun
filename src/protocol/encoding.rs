//! Local bandwidth policy; protocol 5 endpoints understand both bounded codecs.
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
/// Compression of independent file chunks and output batches.
pub enum Compression {
    #[default]
    /// Compress only when at least one eighth of the payload is saved.
    Auto,
    /// Send uncompressed bytes.
    None,
    /// Try zstd for every object, still avoid expanding the wire payload.
    Zstd,
}
