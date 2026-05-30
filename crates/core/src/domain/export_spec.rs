use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportSpec {
    pub output_path: PathBuf,
    pub format: ContainerFormat,
    pub quality: Quality,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContainerFormat {
    Mp4,
    Mkv,
    Mov,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Quality {
    Lossless,
    High,
    Balanced,
}
