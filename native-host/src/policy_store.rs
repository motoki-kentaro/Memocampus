//! Native Host is the sole writer. Construction and reads never initialize policy implicitly.
use crate::policy::{Decision, Policy, MAX_REVISION};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const MAX_BYTES: u64 = 1_048_576;
const MARKER: &[u8] = b"Memocampus policy initialization v1\n";
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreError {
    Unavailable,
    Missing,
    Invalid,
    AlreadyInitialized,
    Busy,
    Conflict,
    Io,
}
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub policy: Policy,
    /// Hash of exact bytes, also detects unsupported external edits without a revision bump.
    pub etag: String,
}
pub struct PolicyStore {
    dir: PathBuf,
}
impl PolicyStore {
    pub fn from_process() -> Result<Self, StoreError> {
        let nonempty = |key| std::env::var_os(key).filter(|v| !v.is_empty());
        let dir = nonempty("MEMOCAMPUS_APP_DIR")
            .map(PathBuf::from)
            .or_else(|| nonempty("LOCALAPPDATA").map(|p| PathBuf::from(p).join("Memocampus")))
            .ok_or(StoreError::Unavailable)?;
        Self::new(dir)
    }
    /// Supply a dedicated temporary application directory in tests. Never supply a Vault path.
    pub fn new(app_dir: PathBuf) -> Result<Self, StoreError> {
        if !app_dir.is_absolute() {
            return Err(StoreError::Unavailable);
        }
        Ok(Self { dir: app_dir })
    }
    pub fn policy_path(&self) -> PathBuf {
        self.dir.join("policy.json")
    }
    fn marker_path(&self) -> PathBuf {
        self.dir.join("policy.initialized")
    }
    fn lock(&self) -> Result<File, StoreError> {
        fs::create_dir_all(&self.dir).map_err(|_| StoreError::Io)?;
        let path = self.dir.join("policy.lock");
        if fs::symlink_metadata(&path).is_ok_and(|m| !m.is_file()) {
            return Err(StoreError::Invalid);
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|_| StoreError::Io)?;
        file.try_lock().map_err(|_| StoreError::Busy)?;
        Ok(file)
    }
    pub fn load(&self) -> Result<Snapshot, StoreError> {
        // Readers only open an existing lock. No directory or file is created by a read.
        // Sharing this OS lock avoids Windows delete-pending races during replacement.
        let path = self.dir.join("policy.lock");
        let metadata = fs::symlink_metadata(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                StoreError::Missing
            } else {
                StoreError::Io
            }
        })?;
        if !metadata.is_file() {
            return Err(StoreError::Invalid);
        }
        let lock = File::open(path).map_err(|_| StoreError::Io)?;
        lock.try_lock_shared().map_err(|_| StoreError::Busy)?;
        self.load_unlocked()
    }
    fn load_unlocked(&self) -> Result<Snapshot, StoreError> {
        if read_regular(&self.marker_path())? != MARKER {
            return Err(StoreError::Invalid);
        }
        let bytes = read_regular(&self.policy_path())?;
        snapshot(&bytes)
    }
    /// True only before any initialization attempt. A lost policy or marker is not first use, and
    /// a lone `policy.lock` proves nothing either way.
    pub fn never_initialized(&self) -> bool {
        let absent = |path: PathBuf| {
            fs::symlink_metadata(path).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        };
        absent(self.marker_path()) && absent(self.policy_path())
    }
    pub fn evaluate(&self, url: &str) -> Decision {
        self.load()
            .map_or(Decision::NeverSave, |s| s.policy.evaluate(url))
    }
    /// Explicit first-run operation only. An interrupted initialization requires manual recovery;
    /// the marker is written and flushed first and is NEVER rolled back on an error.
    pub fn initialize(&self) -> Result<Snapshot, StoreError> {
        let _lock = self.lock()?;
        for path in [self.marker_path(), self.policy_path()] {
            match fs::symlink_metadata(path) {
                Ok(_) => return Err(StoreError::AlreadyInitialized),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(StoreError::Io),
            }
        }
        let mut marker = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.marker_path())
            .map_err(|_| StoreError::Io)?;
        marker.write_all(MARKER).map_err(|_| StoreError::Io)?;
        marker.sync_all().map_err(|_| StoreError::Io)?;
        drop(marker);
        sync_dir(&self.dir)?;
        self.persist(&Policy::default())?;
        self.load_unlocked()
    }
    /// Callers edit the loaded policy without changing revision. Host increments it under lock.
    pub fn update(
        &self,
        expected: &Snapshot,
        mut candidate: Policy,
    ) -> Result<Snapshot, StoreError> {
        candidate.validate().map_err(|_| StoreError::Invalid)?;
        let _lock = self.lock()?;
        let current = self.load_unlocked()?;
        if current.policy.revision != expected.policy.revision
            || current.etag != expected.etag
            || candidate.revision != expected.policy.revision
        {
            return Err(StoreError::Conflict);
        }
        if current.policy.revision == MAX_REVISION {
            return Err(StoreError::Invalid);
        }
        candidate.revision = current.policy.revision + 1;
        self.persist(&candidate)?;
        self.load_unlocked()
    }
    fn persist(&self, policy: &Policy) -> Result<(), StoreError> {
        let bytes = serde_json::to_vec_pretty(policy).map_err(|_| StoreError::Invalid)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(StoreError::Invalid);
        }
        let temp = self
            .dir
            .join(format!("policy.{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)
                .map_err(|_| StoreError::Io)?;
            file.write_all(&bytes).map_err(|_| StoreError::Io)?;
            file.sync_all().map_err(|_| StoreError::Io)?;
            drop(file);
            atomic_replace(&temp, &self.policy_path()).map_err(|_| StoreError::Io)?;
            sync_dir(&self.dir)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }
}
fn read_regular(path: &Path) -> Result<Vec<u8>, StoreError> {
    let metadata = fs::symlink_metadata(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            StoreError::Missing
        } else {
            StoreError::Io
        }
    })?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        return Err(StoreError::Invalid);
    }
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|_| StoreError::Io)?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| StoreError::Io)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(StoreError::Invalid);
    }
    Ok(bytes)
}
fn snapshot(bytes: &[u8]) -> Result<Snapshot, StoreError> {
    Ok(Snapshot {
        policy: Policy::parse(bytes).map_err(|_| StoreError::Invalid)?,
        etag: format!("{:x}", Sha256::digest(bytes)),
    })
}
#[cfg(windows)]
fn atomic_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    // Both paths are in the same directory/volume; never delete the destination first.
    if unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}
#[cfg(not(windows))]
fn atomic_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::rename(from, to)
}
#[cfg(windows)]
fn sync_dir(_: &Path) -> Result<(), StoreError> {
    Ok(())
}
#[cfg(not(windows))]
fn sync_dir(path: &Path) -> Result<(), StoreError> {
    File::open(path)
        .and_then(|f| f.sync_all())
        .map_err(|_| StoreError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!("memocampus-policy-{}", uuid::Uuid::new_v4())))
        }
        fn store(&self) -> PolicyStore {
            PolicyStore::new(self.0.clone()).unwrap()
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn marker_loss_damage_and_oversized_policy_fail_closed_without_repair() {
        let dir = Scratch::new();
        let store = dir.store();
        let first = store.initialize().unwrap();
        fs::remove_file(store.marker_path()).unwrap();
        assert_eq!(store.load().unwrap_err(), StoreError::Missing);
        assert_eq!(
            store.initialize().unwrap_err(),
            StoreError::AlreadyInitialized
        );
        fs::write(store.marker_path(), b"unfinished").unwrap();
        assert_eq!(store.load().unwrap_err(), StoreError::Invalid);
        assert_eq!(
            store.update(&first, first.policy.clone()).unwrap_err(),
            StoreError::Invalid
        );
        fs::write(store.marker_path(), MARKER).unwrap();
        assert_eq!(store.load().unwrap().etag, first.etag);
        let oversized = vec![b' '; MAX_BYTES as usize + 1];
        fs::write(store.policy_path(), &oversized).unwrap();
        assert_eq!(store.load().unwrap_err(), StoreError::Invalid);
        assert_eq!(store.evaluate("https://example.com"), Decision::NeverSave);
        assert_eq!(fs::read(store.policy_path()).unwrap(), oversized);
    }
    #[test]
    fn readers_only_observe_complete_policy_during_replacements() {
        let dir = Scratch::new();
        let store = dir.store();
        let first = store.initialize().unwrap();
        let reader = dir.store();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let reader_barrier = barrier.clone();
        let worker = std::thread::spawn(move || {
            reader_barrier.wait();
            for _ in 0..100 {
                match reader.load() {
                    Ok(snapshot) => assert!((1..=21).contains(&snapshot.policy.revision)),
                    Err(StoreError::Busy) => {}
                    Err(error) => panic!("unexpected read failure: {error:?}"),
                }
            }
        });
        barrier.wait();
        let mut previous = first;
        for _ in 0..20 {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                match store.update(&previous, previous.policy.clone()) {
                    Ok(next) => {
                        previous = next;
                        break;
                    }
                    Err(StoreError::Busy) if std::time::Instant::now() < deadline => {
                        std::thread::yield_now()
                    }
                    Err(error) => panic!("unexpected update failure: {error:?}"),
                }
            }
        }
        worker.join().unwrap();
        assert_eq!(store.load().unwrap().policy.revision, 21);
    }
    #[test]
    fn explicit_initialization_missing_corrupt_and_unknown_versions() {
        let dir = Scratch::new();
        let store = dir.store();
        assert_eq!(store.evaluate("https://example.com"), Decision::NeverSave);
        assert!(!dir.0.exists());
        let first = store.initialize().unwrap();
        assert_eq!(first.policy.revision, 1);
        assert_eq!(store.evaluate("https://example.com"), Decision::None);
        assert_eq!(
            store.initialize().unwrap_err(),
            StoreError::AlreadyInitialized
        );
        for bytes in [b"{broken".to_vec(), b"{}".to_vec(), {
            let mut p = first.policy.clone();
            p.engine_version = 2;
            serde_json::to_vec(&p).unwrap()
        }] {
            fs::write(store.policy_path(), &bytes).unwrap();
            assert_eq!(store.load().unwrap_err(), StoreError::Invalid);
            assert_eq!(store.evaluate("https://example.com"), Decision::NeverSave);
            assert_eq!(
                store.initialize().unwrap_err(),
                StoreError::AlreadyInitialized
            );
            assert_eq!(fs::read(store.policy_path()).unwrap(), bytes);
        }
        fs::remove_file(store.policy_path()).unwrap();
        assert_eq!(store.load().unwrap_err(), StoreError::Missing);
        assert_eq!(
            store.initialize().unwrap_err(),
            StoreError::AlreadyInitialized
        );
    }
    #[test]
    fn update_conflicts_and_external_edits_preserve_bytes() {
        let dir = Scratch::new();
        let store = dir.store();
        let first = store.initialize().unwrap();
        let mut edited = first.policy.clone();
        edited.layers.never_save.rules.push("private.test".into());
        let second = store.update(&first, edited.clone()).unwrap();
        assert_eq!(second.policy.revision, 2);
        assert_eq!(store.evaluate("https://private.test/"), Decision::NeverSave);
        assert_eq!(
            store.update(&first, edited).unwrap_err(),
            StoreError::Conflict
        );
        let mut external = fs::read(store.policy_path()).unwrap();
        external.push(b' ');
        fs::write(store.policy_path(), &external).unwrap();
        assert_eq!(
            store.update(&second, second.policy.clone()).unwrap_err(),
            StoreError::Conflict
        );
        assert_eq!(fs::read(store.policy_path()).unwrap(), external);
    }
    #[test]
    fn interrupted_init_and_interrupted_update_do_not_reset_policy() {
        let dir = Scratch::new();
        let store = dir.store();
        fs::create_dir_all(&dir.0).unwrap();
        fs::write(store.marker_path(), MARKER).unwrap();
        fs::write(dir.0.join("policy.interrupted.tmp"), b"{partial").unwrap();
        assert_eq!(
            store.initialize().unwrap_err(),
            StoreError::AlreadyInitialized
        );
        assert_eq!(store.evaluate("https://example.com"), Decision::NeverSave);
        let other = Scratch::new();
        let store = other.store();
        let first = store.initialize().unwrap();
        fs::write(other.0.join("policy.interrupted.tmp"), b"{partial").unwrap();
        assert_eq!(store.load().unwrap().etag, first.etag);
    }
    #[test]
    fn concurrent_writers_have_exactly_one_winner() {
        let dir = Scratch::new();
        let first = dir.store().initialize().unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let workers: Vec<_> = (0..2)
            .map(|_| {
                let store = dir.store();
                let first = first.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    store.update(&first, first.policy.clone())
                })
            })
            .collect();
        let results: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert!(results
            .iter()
            .filter_map(|r| r.as_ref().err())
            .all(|e| matches!(e, StoreError::Busy | StoreError::Conflict)));
        assert_eq!(dir.store().load().unwrap().policy.revision, 2);
    }
    #[test]
    fn invalid_candidate_and_exhausted_revision_do_not_write() {
        let dir = Scratch::new();
        let store = dir.store();
        let first = store.initialize().unwrap();
        let mut invalid = first.policy.clone();
        invalid.layers.never_save.rules.push("*".into());
        assert_eq!(
            store.update(&first, invalid).unwrap_err(),
            StoreError::Invalid
        );
        assert_eq!(store.load().unwrap().etag, first.etag);
        let mut last = first.policy;
        last.revision = MAX_REVISION;
        fs::write(store.policy_path(), serde_json::to_vec(&last).unwrap()).unwrap();
        let last = store.load().unwrap();
        assert_eq!(
            store.update(&last, last.policy.clone()).unwrap_err(),
            StoreError::Invalid
        );
        assert_eq!(store.load().unwrap().etag, last.etag);
    }
    #[cfg(windows)]
    #[test]
    fn failed_replace_preserves_original_and_releases_lock() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = Scratch::new();
        let store = dir.store();
        let first = store.initialize().unwrap();
        let held = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(store.policy_path())
            .unwrap();
        assert_eq!(
            store.update(&first, first.policy.clone()).unwrap_err(),
            StoreError::Io
        );
        assert_eq!(store.load().unwrap().etag, first.etag);
        assert!(!fs::read_dir(&dir.0).unwrap().any(|e| e
            .unwrap()
            .path()
            .extension()
            .is_some_and(|e| e == "tmp")));
        drop(held);
        assert_eq!(
            store
                .update(&first, first.policy.clone())
                .unwrap()
                .policy
                .revision,
            2
        );
    }
}
