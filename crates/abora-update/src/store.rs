//! Persistent update state: history, last check time and pending reboot.
//!
//! One small JSON file (default location `/var/lib/abora/updates.json`, chosen
//! by the caller). Rules the store keeps so a crash or a bad disk never loses or
//! corrupts more than it has to:
//!
//! * **Atomic writes**: the new file is written next to the old one, synced,
//!   then renamed over it. A reader sees the old file or the new one, never half.
//! * **Private**: created with mode `0600` on Unix.
//! * **Nothing half-applied**: a change is made on a copy and only becomes
//!   visible in memory once it is safely on disk. If saving fails the caller
//!   gets the error and the state is unchanged.
//! * **Bounded**: only the newest [`MAX_HISTORY`] entries are kept.
//! * **Careful with damage**: a file that does not parse is moved aside to
//!   `<file>.corrupt` and the store starts empty, and says so ([`Opened::Recovered`]).
//!   A file written by a *newer* schema is refused rather than overwritten.
//!
//! Timestamps are passed in by the caller (RFC 3339 strings), so the store has
//! no clock and is trivial to test.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use crate::{RebootStatus, UpdateHistoryEntry};

/// Version of the on-disk format.
pub const SCHEMA_VERSION: u32 = 1;
/// How many history entries are kept (the newest ones).
pub const MAX_HISTORY: usize = 500;

/// What is saved to disk. History is stored oldest first.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StoreData {
    pub schema: u32,
    /// When the update source was last checked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_check: Option<String>,
    #[serde(default)]
    pub reboot: RebootStatus,
    #[serde(default)]
    pub history: Vec<UpdateHistoryEntry>,
}

impl Default for StoreData {
    fn default() -> Self {
        Self { schema: SCHEMA_VERSION, last_check: None, reboot: RebootStatus::default(), history: Vec::new() }
    }
}

/// What [`UpdateStore::open`] found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Opened {
    /// No file yet: starting empty.
    Fresh,
    /// An existing file was loaded.
    Loaded,
    /// The file did not parse. It was moved to `backup` and the store starts empty.
    Recovered { backup: PathBuf },
}

/// Store failures. Messages never include file contents.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    #[error("update store I/O error at {path}: {message}")]
    Io { path: String, message: String },
    #[error("update store {path} was written by a newer version (schema {found}, this build understands {supported}); refusing to overwrite it")]
    NewerSchema { path: String, found: u32, supported: u32 },
}

fn io_err(path: &Path, e: std::io::Error) -> StoreError {
    StoreError::Io { path: path.display().to_string(), message: e.to_string() }
}

/// The persistent store. Cheap to share behind an `Arc`.
pub struct UpdateStore {
    /// `None` for an in-memory store (tests, or a daemon with no state directory).
    path: Option<PathBuf>,
    data: Mutex<StoreData>,
}

impl UpdateStore {
    /// A store that never touches the disk.
    pub fn in_memory() -> Self {
        Self { path: None, data: Mutex::new(StoreData::default()) }
    }

    /// Open (or start) the store at `path`. See the module docs for how damage is handled.
    pub fn open(path: impl Into<PathBuf>) -> Result<(Self, Opened), StoreError> {
        let path = path.into();
        let (data, opened) = match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<StoreData>(&bytes) {
                Ok(data) if data.schema > SCHEMA_VERSION => {
                    return Err(StoreError::NewerSchema {
                        path: path.display().to_string(),
                        found: data.schema,
                        supported: SCHEMA_VERSION,
                    })
                }
                Ok(data) => (data, Opened::Loaded),
                Err(_) => {
                    let mut backup = path.clone().into_os_string();
                    backup.push(".corrupt");
                    let backup = PathBuf::from(backup);
                    fs::rename(&path, &backup).map_err(|e| io_err(&path, e))?;
                    (StoreData::default(), Opened::Recovered { backup })
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (StoreData::default(), Opened::Fresh),
            Err(e) => return Err(io_err(&path, e)),
        };
        let mut data = data;
        trim(&mut data);
        Ok((Self { path: Some(path), data: Mutex::new(data) }, opened))
    }

    fn lock(&self) -> MutexGuard<'_, StoreData> {
        // A panic while holding the lock cannot leave the data half-written
        // (changes are made on a copy), so a poisoned lock is safe to reuse.
        self.data.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Everything currently stored (history oldest first).
    pub fn snapshot(&self) -> StoreData {
        self.lock().clone()
    }

    /// History, newest first, as the API presents it.
    pub fn history(&self) -> Vec<UpdateHistoryEntry> {
        let mut h = self.lock().history.clone();
        h.reverse();
        h
    }

    pub fn last_check(&self) -> Option<String> {
        self.lock().last_check.clone()
    }

    pub fn reboot(&self) -> RebootStatus {
        self.lock().reboot.clone()
    }

    /// Note that the update source was checked at `when` (RFC 3339).
    pub fn record_check(&self, when: impl Into<String>) -> Result<(), StoreError> {
        let when = when.into();
        self.change(|d| d.last_check = Some(when))
    }

    /// Append one applied (or failed) update to the history.
    pub fn record_update(&self, entry: UpdateHistoryEntry) -> Result<(), StoreError> {
        self.change(|d| d.history.push(entry))
    }

    /// Set whether a reboot is pending.
    pub fn set_reboot(&self, status: RebootStatus) -> Result<(), StoreError> {
        self.change(|d| d.reboot = status)
    }

    /// Apply `edit` to a copy, save it, and only then make it the live state.
    fn change(&self, edit: impl FnOnce(&mut StoreData)) -> Result<(), StoreError> {
        let mut guard = self.lock();
        let mut next = guard.clone();
        edit(&mut next);
        trim(&mut next);
        if let Some(path) = &self.path {
            save(path, &next)?;
        }
        *guard = next;
        Ok(())
    }
}

fn trim(data: &mut StoreData) {
    if data.history.len() > MAX_HISTORY {
        let excess = data.history.len() - MAX_HISTORY;
        data.history.drain(..excess);
    }
}

fn save(path: &Path, data: &StoreData) -> Result<(), StoreError> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
    }
    let mut tmp = path.to_path_buf().into_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);

    let json = serde_json::to_vec_pretty(data).expect("store data always serializes");
    let result = write_synced(&tmp, &json).and_then(|()| fs::rename(&tmp, path));
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(io_err(path, e));
    }
    // Make the rename itself durable. Best effort: not every filesystem allows opening a directory.
    if let Some(dir) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        if let Ok(dir) = File::open(dir) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
}

fn write_synced(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Channel;
    use abora_core::Version;

    /// A unique scratch directory, removed on drop.
    struct Scratch(PathBuf);
    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("abora-store-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn file(&self) -> PathBuf {
            self.0.join("updates.json")
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn entry(n: u64, ok: bool) -> UpdateHistoryEntry {
        UpdateHistoryEntry {
            applied_at: format!("2026-09-21T00:00:{:02}Z", n % 60),
            from_version: Version { major: 0, minor: 1, patch: n, prerelease: None, build: None },
            to_version: Version { major: 0, minor: 1, patch: n + 1, prerelease: None, build: None },
            channel: Channel::Stable,
            component: "framework".into(),
            succeeded: ok,
            note: None,
        }
    }

    #[test]
    fn state_survives_a_reopen() {
        let dir = Scratch::new("reopen");
        let (store, opened) = UpdateStore::open(dir.file()).unwrap();
        assert_eq!(opened, Opened::Fresh);
        store.record_check("2026-09-21T01:00:00Z").unwrap();
        store.record_update(entry(1, true)).unwrap();
        store.record_update(entry(2, false)).unwrap();
        store
            .set_reboot(RebootStatus { required: true, reason: Some("kernel".into()), pending_since: Some("2026-09-21T01:05:00Z".into()) })
            .unwrap();

        let (again, opened) = UpdateStore::open(dir.file()).unwrap();
        assert_eq!(opened, Opened::Loaded);
        assert_eq!(again.last_check().as_deref(), Some("2026-09-21T01:00:00Z"));
        assert!(again.reboot().required);
        let history = again.history();
        assert_eq!(history.len(), 2);
        assert!(!history[0].succeeded, "newest first");
        assert_eq!(history[1].from_version.patch, 1);
    }

    #[test]
    fn history_is_bounded_to_the_newest_entries() {
        let store = UpdateStore::in_memory();
        for n in 0..(MAX_HISTORY as u64 + 25) {
            store.record_update(entry(n, true)).unwrap();
        }
        let history = store.history();
        assert_eq!(history.len(), MAX_HISTORY);
        assert_eq!(history[0].from_version.patch, MAX_HISTORY as u64 + 24, "newest kept");
        assert_eq!(history.last().unwrap().from_version.patch, 25, "oldest 25 dropped");
    }

    #[test]
    fn a_corrupt_file_is_moved_aside_not_deleted() {
        let dir = Scratch::new("corrupt");
        fs::write(dir.file(), b"{ this is not json").unwrap();
        let (store, opened) = UpdateStore::open(dir.file()).unwrap();
        let Opened::Recovered { backup } = opened else { panic!("expected Recovered, got {opened:?}") };
        assert_eq!(fs::read(&backup).unwrap(), b"{ this is not json");
        assert!(store.history().is_empty());
        store.record_check("2026-09-21T02:00:00Z").unwrap();
        assert!(matches!(UpdateStore::open(dir.file()).unwrap().1, Opened::Loaded));
    }

    #[test]
    fn a_newer_schema_is_refused_and_left_alone() {
        let dir = Scratch::new("newer");
        let body = br#"{"schema": 99, "history": []}"#;
        fs::write(dir.file(), body).unwrap();
        let err = UpdateStore::open(dir.file()).err().expect("must refuse");
        assert!(matches!(err, StoreError::NewerSchema { found: 99, .. }));
        assert_eq!(fs::read(dir.file()).unwrap(), body, "file untouched");
    }

    #[test]
    fn a_failed_save_leaves_the_state_unchanged() {
        let dir = Scratch::new("failsave");
        let sub = dir.0.join("state");
        fs::create_dir_all(&sub).unwrap();
        let (store, _) = UpdateStore::open(sub.join("updates.json")).unwrap();
        // Pull the directory out from under the store: a file now sits where it was.
        fs::remove_dir_all(&sub).unwrap();
        fs::write(&sub, b"x").unwrap();
        assert!(store.record_update(entry(1, true)).is_err());
        assert!(store.history().is_empty(), "nothing became visible");
        assert_eq!(store.last_check(), None);
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_private_and_no_temp_file_is_left() {
        use std::os::unix::fs::PermissionsExt;
        let dir = Scratch::new("mode");
        let (store, _) = UpdateStore::open(dir.file()).unwrap();
        store.record_check("2026-09-21T03:00:00Z").unwrap();
        let mode = fs::metadata(dir.file()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let names: Vec<_> = fs::read_dir(&dir.0).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 1, "only updates.json remains: {names:?}");
    }

    #[test]
    fn in_memory_store_works_without_a_path() {
        let store = UpdateStore::in_memory();
        store.record_check("t").unwrap();
        assert_eq!(store.last_check().as_deref(), Some("t"));
    }
}
