//! Persistent dedup state: which fills (tx-hash:log-index) we've already acted
//! on, so a reconnect or restart never double-submits. Stored as a small JSON
//! file. WS subscriptions only deliver new events, so we never replay history.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};

/// Keep at most this many recent keys to bound the file size.
const MAX_KEYS: usize = 50_000;

#[derive(Debug, Default, Serialize, Deserialize)]
struct StateFile {
    seen: VecDeque<String>,
}

pub struct State {
    path: PathBuf,
    set: HashSet<String>,
    order: VecDeque<String>,
    dirty: bool,
}

impl State {
    pub fn load(path: &Path) -> Result<State> {
        let file = match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str::<StateFile>(&text)
                .with_context(|| format!("parsing state file {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => StateFile::default(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };

        let set: HashSet<String> = file.seen.iter().cloned().collect();
        Ok(State {
            path: path.to_path_buf(),
            set,
            order: file.seen,
            dirty: false,
        })
    }

    pub fn has_seen(&self, key: &str) -> bool {
        self.set.contains(key)
    }

    pub fn mark_seen(&mut self, key: String) {
        if self.set.insert(key.clone()) {
            self.order.push_back(key);
            while self.order.len() > MAX_KEYS {
                if let Some(old) = self.order.pop_front() {
                    self.set.remove(&old);
                }
            }
            self.dirty = true;
        }
    }

    /// Write to disk only if something changed. Atomic via temp-file + rename.
    pub fn save(&mut self) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).ok();
            }
        }
        let file = StateFile {
            seen: self.order.clone(),
        };
        let json = serde_json::to_string(&file)?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, &json).with_context(|| format!("writing {}", tmp.display()))?;
        // Prefer atomic rename; if that fails (some filesystems), fall back to a
        // direct write so state still persists.
        if std::fs::rename(&tmp, &self.path).is_err() {
            std::fs::write(&self.path, &json)
                .with_context(|| format!("writing {}", self.path.display()))?;
            let _ = std::fs::remove_file(&tmp);
        }
        self.dirty = false;
        Ok(())
    }
}
