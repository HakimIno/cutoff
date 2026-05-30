//! Local-only configuration and history persistence.

pub mod config;
pub mod history;
pub mod storage;

use thiserror::Error;

pub use config::{AppConfig, Theme};
pub use history::{ExportRecord, History};
pub use storage::Storage;

#[derive(Debug, Error)]
pub enum PersistenceError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml decode error: {0}")]
    TomlDe(#[from] toml::de::Error),
    #[error("toml encode error: {0}")]
    TomlSer(#[from] toml::ser::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("no platform config directory available")]
    NoConfigDir,
}

pub type PersistenceResult<T> = Result<T, PersistenceError>;
