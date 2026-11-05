//! Native Host is the sole writer. Construction and reads never initialize policy implicitly.
//!
//! The domains approved for restricted domain wildcards (`domain_approvals`) live beside the
//! policy, and every policy is compiled with the approvals read under the same lock. Approving and
//! revoking take the policy's write lock, so an apply and a revocation never interleave: a domain
//! the stored policy needs is never revoked, and a policy is never stored needing a domain that is
//! not approved.
use crate::domain_approvals::{self, Approval, ApprovalError};
use crate::policy::{CompiledPolicy, Decision, DomainApprovals, Error, Policy, MAX_REVISION};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

const MAX_BYTES: u64 = 1_048_576;
const MARKER: &[u8] = b"Memocampus policy initialization v1\n";
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreError {
    Unavailable,
    Missing,
    Invalid,
    /// Valid, but holds a rule this version does not evaluate.
    Unsupported,
    /// `migrate` was asked for anything but a version 1 policy.
    NotMigratable,
    AlreadyInitialized,
    Busy,
    Conflict,
    Io,
    /// The stored policy needs the approval that was to be revoked.
    InUse,
    /// The approvals could not be read or written.
    Approvals,
}
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub policy: Policy,
    /// Hash of exact bytes, also detects unsupported external edits without a revision bump.
    pub etag: String,
    /// `policy` compiled once while it was loaded, for the one request that loaded it. Nothing
    /// keeps it longer: the next request reads and compiles the file again.
    pub compiled: Arc<CompiledPolicy>,
}
/// The one place a replacement may change the policy's version.
#[derive(Clone, Copy)]
enum Change {
    SameVersion,
    Migration,
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
    fn approvals_path(&self) -> PathBuf {
        self.dir.join(domain_approvals::FILE_NAME)
    }
    /// The approved domains as a policy is judged with them: none if they cannot be read.
    pub fn approvals(&self) -> DomainApprovals {
        domain_approvals::read(&self.approvals_path()).map_or_else(
            |_| DomainApprovals::none(),
            |approvals| DomainApprovals::of(approvals.into_iter().map(|a| a.domain)),
        )
    }
    /// Every approval, for the settings page, and the domains the stored policy needs (None if the
    /// stored policy cannot be read as a policy document).
    pub fn list_approvals(&self) -> Result<(Vec<Approval>, Option<HashSet<String>>), StoreError> {
        let approvals =
            domain_approvals::read(&self.approvals_path()).map_err(|_| StoreError::Approvals)?;
        Ok((approvals, self.dependencies().ok()))
    }
    /// Approves a canonical domain. Under the write lock, committed before it returns.
    pub fn approve_domain(&self, domain: &str) -> Result<Approval, StoreError> {
        let _lock = self.lock()?;
        domain_approvals::write(&self.approvals_path(), domain, SystemTime::now())
            .map_err(|_| StoreError::Approvals)
    }
    /// Revokes an approval, unless the stored policy needs it: under the write lock, so no apply
    /// comes between the check and the removal. False if the domain was not approved.
    pub fn revoke_domain(&self, domain: &str) -> Result<bool, StoreError> {
        let _lock = self.lock()?;
        if self.dependencies()?.contains(domain) {
            return Err(StoreError::InUse);
        }
        domain_approvals::remove(&self.approvals_path(), domain).map_err(|error| match error {
            ApprovalError::Unreadable | ApprovalError::WriteFailed => StoreError::Approvals,
        })
    }
    /// The domains the stored policy needs approved, read as a document, not compiled: a policy
    /// that does not compile may still need them. None stored: none needed.
    fn dependencies(&self) -> Result<HashSet<String>, StoreError> {
        match read_regular(&self.policy_path()) {
            Err(StoreError::Missing) => Ok(HashSet::new()),
            Err(error) => Err(error),
            Ok(bytes) => serde_json::from_slice::<Policy>(&bytes)
                .map(|policy| policy.approval_dependencies())
                .map_err(|_| StoreError::Invalid),
        }
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
        let _lock = self.read_lock()?;
        self.load_unlocked()
    }
    /// Readers only open an existing lock. No directory or file is created by a read.
    /// Sharing this OS lock avoids Windows delete-pending races during replacement.
    fn read_lock(&self) -> Result<File, StoreError> {
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
        Ok(lock)
    }
    fn load_unlocked(&self) -> Result<Snapshot, StoreError> {
        if read_regular(&self.marker_path())? != MARKER {
            return Err(StoreError::Invalid);
        }
        let bytes = read_regular(&self.policy_path())?;
        snapshot(&bytes, || self.approvals())
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
            .map_or(Decision::NeverSave, |s| s.compiled.evaluate(url))
    }
    /// The ETag of the stored policy, read like `load` but not parsed: enough to tell that the
    /// policy a reader started with is still the stored one, which was valid when it was loaded.
    pub fn current_etag(&self) -> Result<String, StoreError> {
        let _lock = self.read_lock()?;
        if read_regular(&self.marker_path())? != MARKER {
            return Err(StoreError::Invalid);
        }
        Ok(etag_of(&read_regular(&self.policy_path())?))
    }
    /// Explicit first-run operation only. An interrupted initialization requires manual recovery;
    /// the marker is written and flushed first and is NEVER rolled back on an error.
    pub fn initialize(&self) -> Result<Snapshot, StoreError> {
        self.initialize_with(&Policy::default())
    }
    /// `initialize` with a policy of the caller's, at revision 1. It is checked whole, and its
    /// size, before anything is written, since the marker is never rolled back.
    pub fn initialize_with(&self, policy: &Policy) -> Result<Snapshot, StoreError> {
        let mut first = policy.clone();
        first.revision = 1;
        if !fits(&first) {
            return Err(StoreError::Invalid);
        }
        let _lock = self.lock()?;
        first
            .validate_with(&self.approvals())
            .map_err(|error| match error {
                Error::UnsupportedRule => StoreError::Unsupported,
                _ => StoreError::Invalid,
            })?;
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
        self.persist(&first)?;
        self.load_unlocked()
    }
    /// Callers edit the loaded policy without changing revision. Host increments it under lock.
    /// An update keeps the policy's version: only `migrate` changes it.
    pub fn update(&self, expected: &Snapshot, candidate: Policy) -> Result<Snapshot, StoreError> {
        self.replace(expected, candidate, Change::SameVersion)
    }
    fn replace(
        &self,
        expected: &Snapshot,
        mut candidate: Policy,
        change: Change,
    ) -> Result<Snapshot, StoreError> {
        let _lock = self.lock()?;
        // Judged with the approvals as they are now, under the lock a revocation also takes.
        candidate
            .validate_with(&self.approvals())
            .map_err(|_| StoreError::Invalid)?;
        let current = self.load_unlocked()?;
        if current.policy.revision != expected.policy.revision
            || current.etag != expected.etag
            || candidate.revision != expected.policy.revision
        {
            return Err(StoreError::Conflict);
        }
        let versions = |policy: &Policy| (policy.schema_version, policy.engine_version);
        let allowed = match change {
            Change::SameVersion => versions(&current.policy) == versions(&candidate),
            Change::Migration => {
                versions(&current.policy) == (1, 1) && versions(&candidate) == (2, 2)
            }
        };
        if !allowed {
            return Err(StoreError::Invalid);
        }
        if current.policy.revision == MAX_REVISION {
            return Err(StoreError::Invalid);
        }
        candidate.revision = current.policy.revision + 1;
        self.persist(&candidate)?;
        self.load_unlocked()
    }
    /// Explicit only: nothing calls it on a load or a decision. Converts the version 1 policy of
    /// `expected` to version 2 and stores it only if `expected` is still the stored policy, so a
    /// newer policy is never replaced by a conversion of an older one. All or nothing: on any
    /// failure the file, revision and ETag stay as they were.
    pub fn migrate(&self, expected: &Snapshot) -> Result<Snapshot, StoreError> {
        let candidate = expected.policy.migrated().map_err(|error| match error {
            Error::NotMigratable => StoreError::NotMigratable,
            _ => StoreError::Invalid,
        })?;
        CompiledPolicy::compile_with(&candidate, &self.approvals())
            .map_err(|_| StoreError::Invalid)?;
        self.replace(expected, candidate, Change::Migration)
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
/// The approvals are read only for a policy that needs one, so a policy without a restricted
/// domain wildcard never opens the approvals file.
fn snapshot(
    bytes: &[u8],
    approvals: impl FnOnce() -> DomainApprovals,
) -> Result<Snapshot, StoreError> {
    let refused = |error| match error {
        Error::UnsupportedRule => StoreError::Unsupported,
        _ => StoreError::Invalid,
    };
    let policy: Policy = serde_json::from_slice(bytes).map_err(|_| StoreError::Invalid)?;
    let approvals = if policy.approval_dependencies().is_empty() {
        DomainApprovals::none()
    } else {
        approvals()
    };
    let compiled = CompiledPolicy::compile_with(&policy, &approvals).map_err(refused)?;
    Ok(Snapshot {
        policy,
        etag: etag_of(bytes),
        compiled: Arc::new(compiled),
    })
}
fn etag_of(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
/// Whether the policy fits the file as `persist` writes it.
pub fn fits(policy: &Policy) -> bool {
    serde_json::to_vec_pretty(policy).is_ok_and(|bytes| bytes.len() as u64 <= MAX_BYTES)
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
            p.engine_version = 3;
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
    /// A version 1 policy as an older Memocampus wrote it, rules as strings.
    fn write_v1(store: &PolicyStore, never_save: &[&str]) -> Snapshot {
        let mut policy = Policy {
            schema_version: 1,
            engine_version: 1,
            ..Policy::default()
        };
        policy.layers.never_save.rules = never_save
            .iter()
            .map(|&rule| crate::policy::RuleSpec::Text(rule.into()))
            .collect();
        fs::write(
            store.policy_path(),
            serde_json::to_vec_pretty(&policy).unwrap(),
        )
        .unwrap();
        store.load().unwrap()
    }
    #[test]
    fn a_new_policy_is_version_2_and_an_update_never_changes_the_version() {
        let dir = Scratch::new();
        let store = dir.store();
        let first = store.initialize().unwrap();
        assert_eq!(
            (first.policy.schema_version, first.policy.engine_version),
            (2, 2)
        );
        let as_v1 = Policy {
            schema_version: 1,
            engine_version: 1,
            ..first.policy.clone()
        };
        assert_eq!(
            store.update(&first, as_v1).unwrap_err(),
            StoreError::Invalid
        );
        let engine_only = Policy {
            engine_version: 1,
            ..first.policy.clone()
        };
        assert_eq!(
            store.update(&first, engine_only).unwrap_err(),
            StoreError::Invalid
        );
        assert_eq!(store.load().unwrap().etag, first.etag);
        let v1 = write_v1(&dir.store(), &["a.test"]);
        let as_v2 = v1.policy.migrated().unwrap();
        assert_eq!(
            store.update(&v1, as_v2).unwrap_err(),
            StoreError::Invalid,
            "only migrate"
        );
        let engine_only = Policy {
            engine_version: 2,
            ..v1.policy.clone()
        };
        assert_eq!(
            store.update(&v1, engine_only).unwrap_err(),
            StoreError::Invalid
        );
        assert_eq!(store.load().unwrap().etag, v1.etag);
        let mut kept = v1.policy.clone();
        kept.layers
            .never_save
            .rules
            .push(crate::policy::RuleSpec::Text("b.test".into()));
        assert_eq!(store.update(&v1, kept).unwrap().policy.schema_version, 1);
    }
    #[test]
    fn the_migration_path_itself_accepts_only_version_1_to_2() {
        let dir = Scratch::new();
        let store = dir.store();
        let v2 = store.initialize().unwrap();
        let same = v2.policy.clone();
        assert_eq!(
            store.replace(&v2, same, Change::Migration).unwrap_err(),
            StoreError::Invalid
        );
        let v1 = write_v1(&store, &["a.test"]);
        let unchanged = v1.policy.clone();
        assert_eq!(
            store
                .replace(&v1, unchanged, Change::Migration)
                .unwrap_err(),
            StoreError::Invalid
        );
        assert_eq!(store.load().unwrap().etag, v1.etag);
    }
    #[test]
    fn migration_is_all_or_nothing_and_only_from_the_current_version_1() {
        let dir = Scratch::new();
        let store = dir.store();
        store.initialize().unwrap();
        let first = write_v1(&store, &["b.test"]);
        let mut edited = first.policy.clone();
        edited
            .layers
            .never_save
            .rules
            .push(crate::policy::RuleSpec::Text("*.a.test".into()));
        let current = store.update(&first, edited).unwrap();
        assert_eq!(
            store.migrate(&first).unwrap_err(),
            StoreError::Conflict,
            "an older snapshot"
        );
        assert_eq!(store.load().unwrap().etag, current.etag);
        let migrated = store.migrate(&current).unwrap();
        assert_eq!(migrated.policy.schema_version, 2);
        assert_eq!(migrated.policy.revision, current.policy.revision + 1);
        assert_ne!(migrated.etag, current.etag);
        for url in [
            "https://b.test/",
            "https://x.a.test/",
            "https://a.test/",
            "https://c.test/",
        ] {
            assert_eq!(
                migrated.policy.evaluate(url),
                current.policy.evaluate(url),
                "{url}"
            );
        }
        assert_eq!(
            store.migrate(&migrated).unwrap_err(),
            StoreError::NotMigratable
        );
        assert_eq!(store.migrate(&current).unwrap_err(), StoreError::Conflict);
        assert_eq!(store.load().unwrap().etag, migrated.etag);
    }
    #[cfg(windows)]
    #[test]
    fn a_migration_that_cannot_be_written_leaves_version_1_in_place() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = Scratch::new();
        let store = dir.store();
        store.initialize().unwrap();
        let first = write_v1(&store, &["a.test"]);
        let bytes = fs::read(store.policy_path()).unwrap();
        let held = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(store.policy_path())
            .unwrap();
        assert_eq!(store.migrate(&first).unwrap_err(), StoreError::Io);
        drop(held);
        let after = store.load().unwrap();
        assert_eq!(
            (after.etag, after.policy.schema_version),
            (first.etag.clone(), 1)
        );
        assert_eq!(fs::read(store.policy_path()).unwrap(), bytes);
        assert_eq!(store.migrate(&first).unwrap().policy.schema_version, 2);
    }
    #[test]
    fn reading_and_judging_a_version_1_policy_never_rewrites_it() {
        let dir = Scratch::new();
        let store = dir.store();
        store.initialize().unwrap();
        write_v1(&store, &["a.test"]);
        let bytes = fs::read(store.policy_path()).unwrap();
        for _ in 0..3 {
            store.load().unwrap();
            store.evaluate("https://example.com/");
        }
        assert_eq!(fs::read(store.policy_path()).unwrap(), bytes);
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

    #[test]
    fn a_first_policy_is_checked_whole_before_anything_is_written() {
        let dir = Scratch::new();
        let store = dir.store();
        let mut invalid = Policy::default();
        invalid.layers.never_save.rules = vec!["bad/*/x".into()];
        let mut huge = Policy::default();
        huge.layers.never_save.rules = vec![format!("a.test/{}/*", "x".repeat(2000)).into(); 600];
        for draft in [invalid, huge] {
            assert_eq!(
                store.initialize_with(&draft).unwrap_err(),
                StoreError::Invalid
            );
            assert!(store.never_initialized(), "no marker, no file");
        }
        let mut first = Policy {
            revision: 9,
            ..Policy::default()
        };
        first.layers.never_save.rules = vec!["kept.test".into()];
        let started = store.initialize_with(&first).unwrap();
        assert_eq!(started.policy.revision, 1);
        assert_eq!(started.policy.layers, first.layers);
        assert_eq!(
            store.initialize_with(&first).unwrap_err(),
            StoreError::AlreadyInitialized
        );
        assert_eq!(store.current_etag().unwrap(), started.etag);
    }

    #[test]
    fn the_current_etag_is_that_of_the_bytes_and_fails_like_a_load() {
        let dir = Scratch::new();
        let store = dir.store();
        assert_eq!(store.current_etag().unwrap_err(), StoreError::Missing);
        let first = store.initialize().unwrap();
        assert_eq!(store.current_etag().unwrap(), first.etag);
        let second = store.update(&first, first.policy.clone()).unwrap();
        assert_eq!(store.current_etag().unwrap(), second.etag);
        let _held = OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.0.join("policy.lock"))
            .unwrap();
        _held.try_lock().unwrap();
        assert_eq!(store.current_etag().unwrap_err(), StoreError::Busy);
    }

    fn with_domain(policy: &Policy, pattern: &str, enabled: bool) -> Policy {
        use crate::policy::{RuleSpec, TypedRule};
        let mut next = policy.clone();
        next.layers.save_but_no_mcp.enabled = enabled;
        next.layers
            .save_but_no_mcp
            .rules
            .push(RuleSpec::Typed(TypedRule::DomainWildcard {
                pattern: pattern.into(),
            }));
        next
    }

    #[test]
    fn a_policy_needing_an_approval_is_stored_and_loaded_only_while_it_is_approved() {
        let dir = Scratch::new();
        let store = dir.store();
        let first = store.initialize().unwrap();
        let needing = with_domain(&first.policy, "*.co.jp", true);
        assert_eq!(
            store.update(&first, needing.clone()).unwrap_err(),
            StoreError::Invalid,
            "not approved: refused, nothing written"
        );
        assert_eq!(store.load().unwrap().etag, first.etag);

        let before = fs::read(store.policy_path()).unwrap();
        let approval = store.approve_domain("co.jp").unwrap();
        assert_eq!(approval.domain, "co.jp");
        assert_eq!(
            fs::read(store.policy_path()).unwrap(),
            before,
            "approving changes no policy"
        );
        assert_eq!(store.load().unwrap().etag, first.etag);
        let applied = store.update(&first, needing).unwrap();
        assert_eq!(
            applied.compiled.evaluate("https://x.co.jp/"),
            Decision::SaveButNoMcp
        );
        assert_eq!(
            applied.compiled.evaluate("https://x.ne.jp/"),
            Decision::None
        );

        // The approvals are lost: the stored policy no longer loads, and every URL is refused.
        fs::remove_file(store.approvals_path()).unwrap();
        assert_eq!(store.load().unwrap_err(), StoreError::Invalid);
        assert_eq!(store.evaluate("https://public.test/"), Decision::NeverSave);
        // An unreadable store is the same as none.
        fs::write(store.approvals_path(), b"garbage").unwrap();
        assert_eq!(store.load().unwrap_err(), StoreError::Invalid);
        assert_eq!(store.evaluate("https://public.test/"), Decision::NeverSave);
        assert_eq!(
            store.approve_domain("co.jp").unwrap_err(),
            StoreError::Approvals
        );
        assert_eq!(
            fs::read(store.approvals_path()).unwrap(),
            b"garbage",
            "never written over"
        );
        fs::remove_file(store.approvals_path()).unwrap();
        // Approved again, the same policy loads again, unchanged.
        store.approve_domain("co.jp").unwrap();
        assert_eq!(store.load().unwrap().etag, applied.etag);
    }

    #[test]
    fn an_approval_the_stored_policy_needs_is_never_revoked() {
        let dir = Scratch::new();
        let store = dir.store();
        let first = store.initialize().unwrap();
        store.approve_domain("co.jp").unwrap();
        store.approve_domain("example.newtld").unwrap();
        // Needed even in a disabled layer, and even when written in another case.
        let applied = store
            .update(&first, with_domain(&first.policy, "*.CO.JP", false))
            .unwrap();
        assert_eq!(store.revoke_domain("co.jp").unwrap_err(), StoreError::InUse);
        assert!(store.approvals().contains("co.jp"));
        assert_eq!(store.load().unwrap().etag, applied.etag);
        assert_eq!(
            store.revoke_domain("example.newtld"),
            Ok(true),
            "not used: revoked"
        );
        assert_eq!(store.revoke_domain("example.newtld"), Ok(false));
        assert!(!store.approvals().contains("example.newtld"));
        let (listed, used) = store.list_approvals().unwrap();
        assert_eq!(
            listed.iter().map(|a| a.domain.as_str()).collect::<Vec<_>>(),
            ["co.jp"]
        );
        assert_eq!(used, Some(HashSet::from(["co.jp".to_owned()])));

        // Once a policy without the rule is applied, the approval may go.
        let removed = store
            .update(
                &applied,
                Policy {
                    revision: 2,
                    ..first.policy.clone()
                },
            )
            .unwrap();
        assert_eq!(store.revoke_domain("co.jp"), Ok(true));
        assert_eq!(store.load().unwrap().etag, removed.etag);
        // And a policy needing it is refused again.
        assert_eq!(
            store
                .update(&removed, with_domain(&removed.policy, "*.co.jp", true))
                .unwrap_err(),
            StoreError::Invalid
        );
    }

    #[test]
    fn a_revocation_waits_for_the_lock_an_apply_holds() {
        let dir = Scratch::new();
        let store = dir.store();
        store.initialize().unwrap();
        store.approve_domain("co.jp").unwrap();
        let held = store.lock().unwrap();
        assert_eq!(store.revoke_domain("co.jp").unwrap_err(), StoreError::Busy);
        assert_eq!(store.approve_domain("ne.jp").unwrap_err(), StoreError::Busy);
        drop(held);
        assert!(
            store.approvals().contains("co.jp"),
            "nothing changed while held"
        );
        assert!(!store.approvals().contains("ne.jp"));
    }

    #[test]
    fn a_first_policy_needing_an_approval_is_checked_with_the_approvals() {
        let dir = Scratch::new();
        let store = dir.store();
        let draft = with_domain(&Policy::default(), "*.github.io", true);
        assert_eq!(
            store.initialize_with(&draft).unwrap_err(),
            StoreError::Invalid
        );
        assert!(store.never_initialized());
        assert_eq!(
            store.revoke_domain("github.io"),
            Ok(false),
            "no policy: nothing depends"
        );
        store.approve_domain("github.io").unwrap();
        let started = store.initialize_with(&draft).unwrap();
        assert_eq!(
            started.compiled.evaluate("https://a.github.io/"),
            Decision::SaveButNoMcp
        );
    }

    #[test]
    fn an_unreadable_policy_document_blocks_a_revocation() {
        let dir = Scratch::new();
        let store = dir.store();
        store.initialize().unwrap();
        store.approve_domain("co.jp").unwrap();
        fs::write(store.policy_path(), b"{ not json").unwrap();
        assert_eq!(
            store.revoke_domain("co.jp").unwrap_err(),
            StoreError::Invalid
        );
        assert!(store.approvals().contains("co.jp"));
        let (_, used) = store.list_approvals().unwrap();
        assert_eq!(used, None, "what the policy uses is unknown");
    }
}
