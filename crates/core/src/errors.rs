use std::path::PathBuf;
use thiserror::Error;

pub type CoreResult<T> = Result<T, CoreError>;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("clip not found: {0}")]
    ClipNotFound(uuid::Uuid),

    #[error("invalid reorder: index {index} out of bounds (len {len})")]
    InvalidReorder { index: usize, len: usize },

    #[error("empty playlist cannot be merged")]
    EmptyPlaylist,

    #[error("unsupported file: {0}")]
    UnsupportedFile(PathBuf),

    #[error("invalid trim: {0}")]
    InvalidTrim(String),
}
