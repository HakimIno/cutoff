use std::path::PathBuf;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::{PersistenceResult, Storage};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportRecord {
    pub output_path: PathBuf,
    pub input_count: usize,
    pub finished_at: SystemTime,
    pub duration_secs: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct History {
    pub records: Vec<ExportRecord>,
}

impl History {
    pub fn load(storage: &Storage) -> PersistenceResult<Self> {
        let path = storage.history_file();
        if !path.exists() {
            return Ok(Self::default());
        }
        let bytes = std::fs::read(&path)?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub fn save(&self, storage: &Storage) -> PersistenceResult<()> {
        let path = storage.history_file();
        let bytes = serde_json::to_vec_pretty(self)?;
        std::fs::write(path, bytes)?;
        Ok(())
    }

    pub fn push(&mut self, record: ExportRecord) {
        self.records.insert(0, record);
        self.records.truncate(50);
    }
}
