use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{PersistenceResult, Storage};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppConfig {
    pub theme: Theme,
    pub default_output_dir: Option<PathBuf>,
    pub ffmpeg_path: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

impl AppConfig {
    pub fn load(storage: &Storage) -> PersistenceResult<Self> {
        let path = storage.config_file();
        if !path.exists() {
            return Ok(Self::default());
        }
        let bytes = std::fs::read_to_string(&path)?;
        Ok(toml::from_str(&bytes)?)
    }

    pub fn save(&self, storage: &Storage) -> PersistenceResult<()> {
        let path = storage.config_file();
        let text = toml::to_string_pretty(self)?;
        std::fs::write(path, text)?;
        Ok(())
    }
}
