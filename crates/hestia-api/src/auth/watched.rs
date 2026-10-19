use std::path::{Path, PathBuf};
use std::sync::{PoisonError, RwLock};
use std::time::SystemTime;

use hestia_hub::storage::{load_json, save_json};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// A JSON file re-read whenever it changes on disk, so command-line edits
/// (`hestia user …`, `hestia mcp token …`) apply to a running server.
pub(crate) struct Watched<T> {
    path: PathBuf,
    cache: RwLock<(T, Option<SystemTime>)>,
}

impl<T: Clone + Default + Serialize + DeserializeOwned> Watched<T> {
    pub(crate) fn open(path: PathBuf) -> anyhow::Result<Self> {
        let value = load_json(&path)?;
        let modified = modified(&path);
        Ok(Self {
            path,
            cache: RwLock::new((value, modified)),
        })
    }

    pub(crate) fn current(&self) -> T {
        let on_disk = modified(&self.path);
        {
            let cache = self.cache.read().unwrap_or_else(PoisonError::into_inner);
            if cache.1 == on_disk {
                return cache.0.clone();
            }
        }
        let mut cache = self.cache.write().unwrap_or_else(PoisonError::into_inner);
        match load_json(&self.path) {
            Ok(value) => *cache = (value, on_disk),
            Err(error) => {
                tracing::warn!(%error, path = %self.path.display(), "keeping previous content");
            }
        }
        cache.0.clone()
    }

    pub(crate) fn save(&self, value: T) -> anyhow::Result<()> {
        save_json(&self.path, &value)?;
        *self.cache.write().unwrap_or_else(PoisonError::into_inner) = (value, modified(&self.path));
        Ok(())
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
}
