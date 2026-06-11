//! Durable storage for the offline server.
//!
//! The server must survive both a device power-cycle and a host restart, and
//! re-seed a valid retained `/state` before the device's first post-restart
//! reading. This crate gives it that memory: a [`Store`] trait and a JSON-file
//! implementation that persists the rolling reading history, the `lstEmpty`
//! baseline, the last computed state, and a cached copy of calibration.
//!
//! The file is written **atomically** (temp file + fsync + rename) so a crash
//! mid-write can never leave a half-written, unparseable state file — a reload
//! sees either the previous good state or the new one. The format is
//! human-readable JSON for debugging (PRD: Persistence).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use aquilo_core::history::ReadingRecord;
use aquilo_core::{Calibration, SensorState};

/// Everything the server persists across restarts.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PersistedState {
    /// Rolling reading history the `daysLeft` projection runs over.
    #[serde(default)]
    pub history: Vec<ReadingRecord>,
    /// Last detected pump-out timestamp (RFC3339); the `lstEmpty` baseline.
    #[serde(default)]
    pub lst_empty: String,
    /// The last computed `/state`, re-seeded retained on startup so the device
    /// gets a valid state immediately after a host reboot.
    #[serde(default)]
    pub last_state: Option<SensorState>,
    /// Cached calibration, so the persisted history can be reasoned about even if
    /// the live config later changes.
    #[serde(default)]
    pub calibration: Calibration,
}

/// Durable storage abstraction. The server depends on this trait, not the
/// concrete file impl, so the persistence backend stays swappable and testable.
pub trait Store {
    /// Loads the persisted state, or `None` when nothing has been stored yet
    /// (first run). Errors only on an unreadable or corrupt file.
    fn load(&self) -> Result<Option<PersistedState>>;
    /// Persists the state atomically.
    fn save(&self, state: &PersistedState) -> Result<()>;
}

/// A [`Store`] backed by a single JSON file, written atomically.
pub struct JsonFileStore {
    path: PathBuf,
    tmp: PathBuf,
}

impl JsonFileStore {
    /// Stores at the given file path. The temp file used for the atomic write
    /// sits beside it (same directory, so the rename stays on one filesystem).
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let mut tmp: OsString = path.clone().into_os_string();
        tmp.push(".tmp");
        JsonFileStore {
            path,
            tmp: PathBuf::from(tmp),
        }
    }

    /// Stores at `state.json` inside the given data directory (the add-on
    /// `/data` volume in the HA deployment).
    pub fn in_dir(dir: impl AsRef<Path>) -> Self {
        Self::new(dir.as_ref().join("state.json"))
    }
}

impl Store for JsonFileStore {
    fn load(&self) -> Result<Option<PersistedState>> {
        match std::fs::read(&self.path) {
            Ok(bytes) => {
                let state = serde_json::from_slice(&bytes)
                    .with_context(|| format!("parsing state file {}", self.path.display()))?;
                Ok(Some(state))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => {
                Err(e).with_context(|| format!("reading state file {}", self.path.display()))
            }
        }
    }

    fn save(&self, state: &PersistedState) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating data dir {}", parent.display()))?;
            }
        }

        let json = serde_json::to_vec_pretty(state).context("serializing state")?;

        // Atomic replace: write the new contents to a temp file, flush them to
        // disk, then rename over the target. The rename is atomic on the same
        // filesystem, so a reader never observes a partial file.
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&self.tmp)
                .with_context(|| format!("creating temp state file {}", self.tmp.display()))?;
            f.write_all(&json)?;
            f.sync_all()?;
        }
        std::fs::rename(&self.tmp, &self.path).with_context(|| {
            format!(
                "renaming {} -> {}",
                self.tmp.display(),
                self.path.display()
            )
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aquilo_core::history::ReadingRecord;

    fn sample_state() -> PersistedState {
        PersistedState {
            history: vec![
                ReadingRecord {
                    ts: "2026-06-01T00:00:00+02:00".into(),
                    lvl: 150.0,
                    bat: 83,
                },
                ReadingRecord {
                    ts: "2026-06-02T00:00:00+02:00".into(),
                    lvl: 140.0,
                    bat: 82,
                },
            ],
            lst_empty: "2026-05-30T00:17:19+02:00".into(),
            last_state: Some(SensorState {
                id: "ae5058".into(),
                name: "ae5058".into(),
                lvl: 140.0,
                pct: 28,
                bat: 82,
                lst_read: "2026-06-02T00:00:00+02:00".into(),
                lst_empty: "2026-05-30T00:17:19+02:00".into(),
                days_left: 12,
                lvl_to_full: 100,
                from: "node-4".into(),
            }),
            calibration: Calibration::default(),
        }
    }

    #[test]
    fn round_trips_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = JsonFileStore::in_dir(dir.path());

        assert_eq!(store.load().unwrap(), None, "no file yet");

        let state = sample_state();
        store.save(&state).unwrap();

        // A fresh store over the same path reloads the identical state — the
        // survives-restart guarantee at the storage layer.
        let reloaded = JsonFileStore::in_dir(dir.path()).load().unwrap();
        assert_eq!(reloaded, Some(state));
    }

    #[test]
    fn reload_reproduces_the_seeded_state_json() {
        let dir = tempfile::tempdir().unwrap();
        let store = JsonFileStore::in_dir(dir.path());

        let state = sample_state();
        store.save(&state).unwrap();

        // After a restart the server seeds the retained `/state` from the
        // persisted last_state; that JSON must match what was last published.
        let original = state.last_state.as_ref().unwrap().to_json();
        let reloaded = JsonFileStore::in_dir(dir.path())
            .load()
            .unwrap()
            .unwrap();
        let seeded = reloaded.last_state.unwrap().to_json();
        assert_eq!(seeded, original);
    }

    #[test]
    fn save_overwrites_atomically_leaving_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = JsonFileStore::in_dir(dir.path());

        store.save(&PersistedState::default()).unwrap();
        let mut updated = sample_state();
        updated.lst_empty = "2026-06-09T10:00:00+02:00".into();
        store.save(&updated).unwrap();

        assert_eq!(store.load().unwrap(), Some(updated));
        // The temp file is renamed away on success, never left behind.
        assert!(!dir.path().join("state.json.tmp").exists());
    }
}
