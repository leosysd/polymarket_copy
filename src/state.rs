//! Persistent dedup state: which target trades we've already processed, so a
//! restart never replays history. Stored as a small JSON file.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};

/// Keep at most this many recent keys to bound the file size.
const MAX_KEYS: usize = 50_000;

#[derive(Debug, Default, Serialize, Deserialize)]
struct StateFile {
    seen: VecDeque<String>,
    bootstrapped: bool,
}

pub struct State {
    path: PathBuf,
    set: HashSet<String>,
    order: VecDeque<String>,
    pub bootstrapped: bool,
    dirty: bool,
}

impl State {
    pub fn load(path: &Path) -> Result<State> {
        let (file, fresh) = match std::fs::read_to_string(path) {
            Ok(text) => (
                serde_json::from_str::<StateFile>(&text)
                    .with_context(|| format!("parsing state file {}", path.display()))?,
                false,
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (StateFile::default(), true),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };

        let set: HashSet<String> = file.seen.iter().cloned().collect();
        Ok(State {
            path: path.to_path_buf(),
            set,
            order: file.seen,
            // A brand-new state file means first run -> needs a bootstrap pass.
            bootstrapped: if fresh { false } else { file.bootstrapped },
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

    pub fn set_bootstrapped(&mut self) {
        if !self.bootstrapped {
            self.bootstrapped = true;
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
            bootstrapped: self.bootstrapped,
        };
        let json = serde_json::to_string(&file)?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, json).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("renaming into {}", self.path.display()))?;
        self.dirty = false;
        Ok(())
    }
}
