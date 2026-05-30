use std::path::PathBuf;

use directories::ProjectDirs;

use crate::{PersistenceError, PersistenceResult};

pub struct Storage {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
}

impl Storage {
    pub fn for_app() -> PersistenceResult<Self> {
        let dirs = ProjectDirs::from("dev", "videomerger", "video-merger")
            .ok_or(PersistenceError::NoConfigDir)?;
        std::fs::create_dir_all(dirs.config_dir())?;
        std::fs::create_dir_all(dirs.data_dir())?;
        Ok(Self {
            config_dir: dirs.config_dir().to_path_buf(),
            data_dir: dirs.data_dir().to_path_buf(),
        })
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    pub fn history_file(&self) -> PathBuf {
        self.data_dir.join("history.json")
    }
}
