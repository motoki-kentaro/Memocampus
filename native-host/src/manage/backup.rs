//! Backups B, C and D (Issue #51, Slice C). Format A (a plain vault's normal backup, a plain SQLite
//! file) is `vault::backup_vault`, unchanged.
//!
//! | Vault | Normal | Password-protected |
//! |---|---|---|
//! | plain | A | C |
//! | encrypted | B | D |
//!
//! # Steps (all under the vault binding of `Session::read_for`, see `manage::backup`)
//!
//! 1. B and D: the key from this user's key store and the recovery envelope from the sidecar,
//!    both checked against the vault file's salt and id, **and the Recovery Spell the user
//!    entered** (review finding C-03): it must open that envelope to exactly the key store's key. A
//!    wrong spell, another vault's spell, altered metadata or a 12-word (version 1) sidecar refuses
//!    the backup before any file exists.
//! 2. Snapshot.
//!    - B, D: the SQLCipher Backup API, under one read transaction on the source, into a work file
//!      `<app dir>\backup-work\<uuid>.sqlite3` keyed with the vault key: ciphertext with a new
//!      salt. Checked: `integrity_check`, schema, row counts, first bytes (never plain SQLite).
//!    - C (review finding C-01): **no work file**. Inside one read transaction (SHARED lock: no
//!      writer can commit) the plain vault is checked (`integrity_check`, schema, counts) and its
//!      pages are read one at a time from its own file and encrypted as they are read. A vault in
//!      WAL mode is refused (`backup_wal_unsupported`). The Backup API cannot write into an
//!      encrypted target from a plain source, and this build has no `sqlite_dbpage`; see
//!      `docs/secure-vault/slice-c.md` §3.1 for the PoC.
//! 3. The container is written to `<folder>\.memocampus-backup-<uuid>.partial` (created new),
//!    flushed to disk.
//! 4. Verification: the container is read back in full with the key or password just used; every
//!    MAC/AEAD tag must pass and the vault bytes inside must hash to what was written.
//! 5. `rename_no_replace` to `memocampus-backup-<UTC>.mcbackup` (`-1`, `-2`… if taken). Nothing is
//!    ever replaced.
//!
//! A failure removes this backup's `.partial` and work file; nothing else is touched. A process
//! that dies leaves at most a `.partial` (ciphertext for B, C and D) in the folder and, for B and D,
//! a work file (SQLCipher ciphertext under the vault key) in the app directory; work files of dead
//! processes are removed by the next backup (each has a lock file held open, without sharing, by
//! its owner).
//!
//! What a backup cannot prove: that the medium it is written to will keep it, or that a future
//! version will read it. Restore tests remain the user's proof.

use super::{display, Failure};
use crate::vault::{self, VaultError};
use memocampus_native_host::active_vault::VaultMode;
use memocampus_native_host::policy::Policy;
use memocampus_native_host::policy_store::{self, PolicyStore};
use memocampus_native_host::secure_vault::container::{self, Kind, PasswordReader, PasswordWriter};
use memocampus_native_host::secure_vault::recovery::{self, Envelope};
use memocampus_native_host::secure_vault::secret::{CipherSalt, RecoverySpell, VaultId, VaultKey};
use memocampus_native_host::secure_vault::{cipher, key_store, sidecar};
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use zeroize::Zeroizing;

pub const WORK_DIR: &str = "backup-work";
pub const EXTENSION: &str = "mcbackup";
const POLICY_COPY_FORMAT: &str = "memocampus-policy-copy";

/// How the user asked for the backup to be protected.
pub enum Protection {
    Normal,
    Password(Zeroizing<String>),
}

/// `protection` / `password` of a `vault_backup` request, checked before any dialog is shown.
pub fn protection(message: &Value) -> Result<Protection, Failure> {
    match message.get("protection").and_then(Value::as_str) {
        None | Some("normal") => Ok(Protection::Normal),
        Some("password") => {
            let password = message
                .get("password")
                .and_then(Value::as_str)
                .ok_or_else(|| Failure::with("backup_password_required", json!({})))?;
            if password.chars().count() < container::PASSWORD_MIN_CHARS {
                return Err(Failure::with(
                    "backup_password_too_short",
                    json!({ "minimum": container::PASSWORD_MIN_CHARS }),
                ));
            }
            if password.len() > container::PASSWORD_MAX_BYTES {
                return Err(Failure::with("backup_password_too_long", json!({})));
            }
            Ok(Protection::Password(Zeroizing::new(password.to_owned())))
        }
        Some(_) => Err(Failure::with("invalid_request", json!({}))),
    }
}

/// The format a backup of a vault in `mode` gets.
pub fn kind_for(mode: VaultMode, protection: &Protection) -> Option<Kind> {
    match (mode, protection) {
        (VaultMode::Plain, Protection::Normal) => None,
        (VaultMode::Encrypted, Protection::Normal) => Some(Kind::B),
        (VaultMode::Plain, Protection::Password(_)) => Some(Kind::C),
        (VaultMode::Encrypted, Protection::Password(_)) => Some(Kind::D),
    }
}

// --- Fault injection (tests only) --------------------------------------------------------------

/// The points at which tests inject a failure or a simulated process death.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    Metadata,
    Snapshot,
    Write,
    Verify,
    BeforeRename,
    AfterRename,
    RestoreWrite,
    RestoreVerify,
    RestoreKeys,
    RestorePromote,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fault {
    /// An I/O error; cleanup runs.
    Error,
    /// A full disk (`StorageFull`); cleanup runs.
    DiskFull,
    /// The process dies here: an error, and no cleanup runs (what is on disk stays).
    Crash,
}

/// Tests only: what runs at every checkpoint.
#[cfg(test)]
pub(crate) type StepHook = Box<dyn FnMut(Step)>;

#[cfg(test)]
thread_local! {
    pub(crate) static FAULT: std::cell::Cell<Option<(Step, Fault)>> =
        const { std::cell::Cell::new(None) };
    static CRASHED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Runs at every checkpoint (e.g. to scan the disk while a backup is in progress).
    pub(crate) static ON_STEP: std::cell::RefCell<Option<StepHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn reset_faults() {
    FAULT.set(None);
    CRASHED.set(false);
    ON_STEP.with(|hook| hook.borrow_mut().take());
}

/// Passes in production; fails at the injected step in tests.
pub(crate) fn checkpoint(step: Step) -> io::Result<()> {
    #[cfg(test)]
    ON_STEP.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook(step);
        }
    });
    #[cfg(test)]
    if let Some((at, fault)) = FAULT.get() {
        if at == step {
            return Err(match fault {
                Fault::Error => io::Error::other("injected"),
                Fault::DiskFull => io::Error::from(io::ErrorKind::StorageFull),
                Fault::Crash => {
                    CRASHED.set(true);
                    io::Error::other("injected crash")
                }
            });
        }
    }
    let _ = step;
    Ok(())
}

/// True after a simulated process death: no cleanup may run.
#[cfg(test)]
pub(crate) fn crashed() -> bool {
    CRASHED.get()
}

#[cfg(not(test))]
pub(crate) fn crashed() -> bool {
    false
}

/// A writer that passes `checkpoint(step)` before every write (a full disk in tests).
pub(crate) struct Guarded<W> {
    pub inner: W,
    pub step: Step,
}

impl<W: Write> Write for Guarded<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        checkpoint(self.step)?;
        self.inner.write(data)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// An I/O failure while writing: a full disk is told apart.
pub(crate) fn output_failure(error: &io::Error, otherwise: &'static str) -> Failure {
    if error.kind() == io::ErrorKind::StorageFull {
        Failure::with("disk_full", json!({}))
    } else {
        Failure::with(otherwise, json!({}))
    }
}

// --- Work files --------------------------------------------------------------------------------

/// A snapshot work file in `<app dir>\backup-work`, with a lock file held open without sharing for
/// as long as this value lives. Removed on drop.
struct WorkFile {
    lock: Option<File>,
    lock_path: PathBuf,
    db: PathBuf,
}

impl WorkFile {
    fn new(app: &Path) -> Result<Self, Failure> {
        let dir = app.join(WORK_DIR);
        fs::create_dir_all(&dir).map_err(|_| VaultError::Io)?;
        clean_stale(&dir);
        let id = uuid::Uuid::new_v4().simple().to_string();
        let lock_path = dir.join(format!("{id}.lock"));
        let lock = open_lock(&lock_path, true).map_err(|_| VaultError::Io)?;
        Ok(Self {
            lock: Some(lock),
            lock_path,
            db: dir.join(format!("{id}.sqlite3")),
        })
    }

    /// A simulated process death: the lock is released, the files stay.
    fn abandon(mut self) {
        self.lock = None;
        std::mem::forget(self);
    }
}

impl Drop for WorkFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.db);
        let _ = fs::remove_file(fsutil_suffix(&self.db, "-journal"));
        self.lock = None;
        let _ = fs::remove_file(&self.lock_path);
    }
}

fn fsutil_suffix(path: &Path, suffix: &str) -> PathBuf {
    memocampus_native_host::secure_vault::fsutil::with_suffix(path, suffix)
}

#[cfg(windows)]
fn open_lock(path: &Path, new: bool) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    let mut options = OpenOptions::new();
    options.read(true).write(true).share_mode(0);
    if new {
        options.create_new(true);
    }
    options.open(path)
}

#[cfg(not(windows))]
fn open_lock(path: &Path, new: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    if new {
        options.create_new(true);
    }
    options.open(path)
}

/// Removes the work files of backups whose process is gone: their lock file can be opened
/// without sharing. Only names this module creates are considered.
fn clean_stale(dir: &Path) {
    if !cfg!(windows) {
        return; // no share modes: a live owner cannot be told from a dead one
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(id) = name.to_str().and_then(|name| name.strip_suffix(".lock")) else {
            continue;
        };
        if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        if let Ok(lock) = open_lock(&entry.path(), false) {
            let db = dir.join(format!("{id}.sqlite3"));
            let _ = fs::remove_file(fsutil_suffix(&db, "-journal"));
            let _ = fs::remove_file(&db);
            drop(lock);
            let _ = fs::remove_file(entry.path());
        }
    }
}

// --- Writing -----------------------------------------------------------------------------------

/// The vault being backed up, as the config named it when the request arrived.
pub struct Source {
    pub path: PathBuf,
    pub vault_id: Option<VaultId>,
    pub app: PathBuf,
    /// The file identity the panel showed (`vault_token`), checked again on the raw file handle.
    pub token: String,
    /// B and D (review finding C-03): the Recovery Spell the user holds, proven against this vault
    /// before anything is written. Never stored.
    pub spell: Option<RecoverySpell>,
}

/// The key and recovery metadata of an encrypted vault.
struct Material {
    key: VaultKey,
    vault_id: VaultId,
    recovery_salt: CipherSalt,
    recovery: Envelope,
}

/// The key and envelope of the encrypted vault, and proof that the user's spell recovers it: the
/// spell must open the sidecar's envelope (bound to the vault id and the file's salt) to exactly
/// the key this user's key store holds (itself checked by its key check value against the same id
/// and salt). A well-formed spell of another vault, or altered metadata, is refused.
fn material(source: &Source) -> Result<Material, Failure> {
    let vault_id = source.vault_id.ok_or(VaultError::ConfigInvalid)?;
    let salt = match cipher::read_head(&source.path) {
        Ok(cipher::Head::Salt(salt)) => salt,
        Ok(_) => return Err(VaultError::ModeMismatch.into()),
        Err(_) => return Err(VaultError::Io.into()),
    };
    let key = key_store::unlock(&source.app, &vault_id, &salt)
        .map_err(|_| Failure::from(VaultError::KeyUnavailable))?;
    let found = sidecar::read(&sidecar::path_for(&source.path)).map_err(|error| match error {
        sidecar::SidecarError::Missing => Failure::with("recovery_metadata_missing", json!({})),
        sidecar::SidecarError::Invalid => Failure::with("recovery_metadata_damaged", json!({})),
        sidecar::SidecarError::Unsupported => {
            Failure::with("recovery_metadata_unsupported", json!({}))
        }
        sidecar::SidecarError::Io => VaultError::Io.into(),
    })?;
    if found.vault_id != vault_id || found.cipher_salt != salt {
        return Err(Failure::with("recovery_metadata_mismatch", json!({})));
    }
    let secret = source.spell.as_ref().ok_or_else(|| {
        Failure::with(
            "recovery_spell_invalid",
            json!({ "reason": "length", "count": 0 }),
        )
    })?;
    let unwrapped = recovery::unwrap(&found.recovery, secret, &vault_id, &found.cipher_salt)
        .map_err(|error| match error {
            recovery::UnwrapError::SpellRejected => {
                Failure::with("recovery_spell_rejected", json!({}))
            }
            _ => Failure::with("recovery_metadata_damaged", json!({})),
        })?;
    if !unwrapped.same_as(&key) {
        // The envelope opens, but to another key than this vault's: the sidecar does not belong to
        // the key the vault is encrypted with.
        return Err(Failure::with("recovery_key_mismatch", json!({})));
    }
    drop(unwrapped);
    Ok(Material {
        key,
        vault_id,
        recovery_salt: found.cipher_salt,
        recovery: found.recovery,
    })
}

/// The global policy and approved domains as a copy for a backup, or None if there is no usable
/// policy (the backup is made without one and says so).
fn policy_copy(app: &Path) -> Option<Zeroizing<Vec<u8>>> {
    let store = PolicyStore::new(app.to_path_buf()).ok()?;
    let snapshot = store.load().ok()?;
    let approved: Vec<String> = store
        .list_approvals()
        .map(|(approvals, _)| approvals.into_iter().map(|a| a.domain).collect())
        .unwrap_or_default();
    let bytes = serde_json::to_vec(&json!({
        "format": POLICY_COPY_FORMAT,
        "version": 1,
        "policy": snapshot.policy,
        "approvedDomains": approved,
    }))
    .ok()?;
    (bytes.len() as u64 <= container::MAX_POLICY_COPY).then(|| Zeroizing::new(bytes))
}

/// What a restore shows of a backup's policy copy. Never applied anywhere (I3).
pub fn describe_policy_copy(bytes: Option<&[u8]>) -> Value {
    let Some(bytes) = bytes else {
        return json!({ "state": "absent" });
    };
    let parsed: Option<Value> = serde_json::from_slice(bytes).ok();
    let valid = parsed.as_ref().and_then(|copy| {
        let fields_ok = copy.get("format").and_then(Value::as_str) == Some(POLICY_COPY_FORMAT)
            && copy.get("version").and_then(Value::as_u64) == Some(1);
        let policy: Policy = serde_json::from_value(copy.get("policy")?.clone()).ok()?;
        let domains: Vec<String> =
            serde_json::from_value(copy.get("approvedDomains")?.clone()).ok()?;
        (fields_ok && policy_store::fits(&policy)).then_some((policy, domains))
    });
    match valid {
        Some((policy, domains)) => json!({
            "state": "valid",
            "revision": policy.revision,
            "policy": policy,
            "approvedDomains": domains,
        }),
        None => json!({ "state": "invalid" }),
    }
}

/// A finished backup.
pub struct Written {
    pub path: PathBuf,
    pub kind: Kind,
    pub policy_included: bool,
}

/// Writes a backup of kind B, C or D of `db` (the open source vault) into `dir`.
pub fn write(
    db: &Connection,
    source: &Source,
    dir: &Path,
    kind: Kind,
    protection: &Protection,
    now: SystemTime,
) -> Result<Written, Failure> {
    if !dir.is_dir() {
        return Err(VaultError::ParentMissing.into());
    }
    let material = match kind {
        Kind::B | Kind::D => Some(material(source)?),
        Kind::C => None,
    };
    checkpoint(Step::Metadata).map_err(|_| VaultError::BackupFailed)?;
    let policy = policy_copy(&source.app);
    let policy = policy.as_deref().map(Vec::as_slice);
    let temp = dir.join(format!(
        ".memocampus-backup-{}.partial",
        uuid::Uuid::new_v4().simple()
    ));
    // B and D: a SQLCipher snapshot (keyed with the vault key) in a work file. C: no work file at
    // all; the pages of the plain vault are encrypted as they are read (review finding C-01).
    let work = match kind {
        Kind::C => None,
        _ => Some(WorkFile::new(&source.app)?),
    };
    let written = match (&work, &material) {
        (None, _) => stream_plain_pages(db, source, &temp, policy, protection),
        (Some(work), Some(material)) => {
            snapshot(db, &work.db, &material.key).and_then(|(len, salt)| {
                let mut file = BufReader::new(
                    File::open(&work.db)
                        .map_err(|error| output_failure(&error, "backup_failed"))?,
                );
                write_container(
                    &temp,
                    kind,
                    &mut file,
                    len,
                    Some(salt),
                    Some(material),
                    policy,
                    protection,
                )
            })
        }
        (Some(_), None) => Err(VaultError::BackupFailed.into()),
    };
    let result = written.and_then(|digest| {
        verify_container(&temp, kind, material.as_ref(), protection, digest)?;
        checkpoint(Step::BeforeRename).map_err(|_| VaultError::BackupFailed)?;
        let path = publish(&temp, dir, now)?;
        checkpoint(Step::AfterRename).map_err(|_| VaultError::BackupFailed)?;
        Ok(path)
    });
    if crashed() {
        if let Some(work) = work {
            work.abandon();
        }
        return result.map(|path| Written {
            path,
            kind,
            policy_included: policy.is_some(),
        });
    }
    drop(work);
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map(|path| Written {
        path,
        kind,
        policy_included: policy.is_some(),
    })
}

/// The pages of a plain vault file, read while the source connection holds its read transaction
/// (SHARED lock: no writer can commit, so the file is one consistent snapshot). The page at 1 GiB
/// holds SQLite's lock bytes on Windows and never data: it is produced as zeros, never read.
/// One page at a time is in memory.
struct PageReader {
    file: File,
    page_size: u64,
    pages: u64,
    lock_page: u64,
    next: u64,
    page: Zeroizing<Vec<u8>>,
    pos: usize,
}

impl io::Read for PageReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        use std::io::{Seek, SeekFrom};
        if self.pos == self.page.len() {
            if self.next > self.pages {
                return Ok(0);
            }
            if self.next == self.lock_page {
                self.page.fill(0);
            } else {
                self.file
                    .seek(SeekFrom::Start((self.next - 1) * self.page_size))?;
                self.file.read_exact(&mut self.page)?;
            }
            self.next += 1;
            self.pos = 0;
        }
        let n = out.len().min(self.page.len() - self.pos);
        out[..n].copy_from_slice(&self.page[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// Format C: checks the plain vault inside one read transaction (integrity, schema, counts), then
/// streams its pages straight into the password container. Nothing but ciphertext reaches the
/// disk. A vault in WAL mode is refused: its committed pages may be in the `-wal` file, which a
/// page reader of the main file would miss.
fn stream_plain_pages(
    db: &Connection,
    source: &Source,
    temp: &Path,
    policy: Option<&[u8]>,
    protection: &Protection,
) -> Result<[u8; 32], Failure> {
    fn failed<E>(_: E) -> Failure {
        VaultError::BackupFailed.into()
    }
    db.execute_batch("BEGIN").map_err(failed)?;
    let result = (|| {
        let journal: String = db
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .map_err(failed)?;
        if journal.eq_ignore_ascii_case("wal") {
            return Err(Failure::with("backup_wal_unsupported", json!({})));
        }
        // The first read takes the SHARED lock that pins the file until COMMIT.
        let integrity: String = db
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .map_err(failed)?;
        memocampus_native_host::schema::check(db).map_err(failed)?;
        vault::content_counts(db).map_err(failed)?;
        if integrity != "ok" {
            return Err(VaultError::BackupFailed.into());
        }
        let page_size: u64 = db
            .query_row("PRAGMA page_size", [], |row| row.get(0))
            .map_err(failed)?;
        let pages: u64 = db
            .query_row("PRAGMA page_count", [], |row| row.get(0))
            .map_err(failed)?;
        let file = File::open(&source.path).map_err(failed)?;
        // The very file the connection holds (Windows: it cannot be replaced while SQLite has it
        // open), and the one the panel showed.
        if memocampus_native_host::active_vault::vault_token_of(&file).map_err(failed)?
            != source.token
        {
            return Err(VaultError::Switched.into());
        }
        if file.metadata().map_err(failed)?.len() < page_size * pages || pages == 0 {
            return Err(VaultError::BackupFailed.into());
        }
        checkpoint(Step::Snapshot).map_err(|error| output_failure(&error, "backup_failed"))?;
        let mut reader = PageReader {
            file,
            page_size,
            pages,
            lock_page: (1u64 << 30) / page_size + 1,
            next: 1,
            page: Zeroizing::new(vec![0u8; page_size as usize]),
            pos: page_size as usize,
        };
        write_container(
            temp,
            Kind::C,
            &mut reader,
            page_size * pages,
            None,
            None,
            policy,
            protection,
        )
    })();
    let _ = db.execute_batch("COMMIT");
    result
}

/// The Backup API copy of an encrypted vault into the work file, keyed with the vault key, checked;
/// returns its length and its (new) salt.
fn snapshot(
    source: &Connection,
    work: &Path,
    key: &VaultKey,
) -> Result<(u64, CipherSalt), Failure> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(work)
        .map_err(|error| output_failure(&error, "backup_failed"))?;
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let mut target = cipher::open(work, key, flags).map_err(|_| VaultError::BackupFailed)?;
    let expected = vault::snapshot_into(source, &mut target)?;
    checkpoint(Step::Snapshot).map_err(|error| output_failure(&error, "backup_failed"))?;
    vault::verify_copy(&target, &expected)?;
    target
        .close()
        .map_err(|_| Failure::from(VaultError::BackupFailed))?;
    let file = OpenOptions::new()
        .write(true)
        .open(work)
        .map_err(|_| VaultError::BackupFailed)?;
    file.sync_all()
        .map_err(|error| output_failure(&error, "backup_failed"))?;
    let len = file.metadata().map_err(|_| VaultError::BackupFailed)?.len();
    drop(file);
    // An encrypted vault's snapshot is SQLCipher, never plain SQLite.
    match cipher::read_head(work) {
        Ok(cipher::Head::Salt(salt)) => Ok((len, salt)),
        _ => Err(VaultError::BackupFailed.into()),
    }
}

fn prepared_b(
    material: &Material,
    salt: CipherSalt,
    len: u64,
    policy: Option<&[u8]>,
) -> Result<container::PreparedB, Failure> {
    container::PreparedB::new(
        &material.key,
        container::VaultFacts {
            schema_version: memocampus_native_host::schema::SCHEMA_VERSION,
            vault_id: material.vault_id,
            cipher_salt: salt,
            recovery_salt: material.recovery_salt,
            recovery: material.recovery.clone(),
        },
        len,
        policy,
    )
    .map_err(|_| Failure::with("random_unavailable", json!({})))
}

fn password_of(protection: &Protection) -> &[u8] {
    match protection {
        Protection::Password(password) => password.as_bytes(),
        Protection::Normal => b"",
    }
}

#[allow(clippy::too_many_arguments)]
fn write_container(
    temp: &Path,
    kind: Kind,
    snapshot: &mut dyn io::Read,
    len: u64,
    salt: Option<CipherSalt>,
    material: Option<&Material>,
    policy: Option<&[u8]>,
    protection: &Protection,
) -> Result<[u8; 32], Failure> {
    let failed = |error: io::Error| output_failure(&error, "backup_failed");
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp)
        .map_err(failed)?;
    let mut out = BufWriter::with_capacity(
        1 << 20,
        Guarded {
            inner: file,
            step: Step::Write,
        },
    );
    let mut source = container::Hashing::new(snapshot);
    let password = password_of(protection);
    let package = |material: Option<&Material>| -> Result<container::PreparedB, Failure> {
        let material = material.ok_or(VaultError::BackupFailed)?;
        prepared_b(material, salt.ok_or(VaultError::BackupFailed)?, len, policy)
    };
    match kind {
        Kind::B => package(material)?
            .write_to(&mut out, &mut source)
            .map_err(failed)?,
        Kind::C => {
            let meta = serde_json::to_vec(&container::Meta::plain()).expect("meta serializes");
            let mut sections = vec![container::Section {
                kind: container::SECTION_META.into(),
                len: meta.len() as u64,
            }];
            if let Some(policy) = policy {
                sections.push(container::Section {
                    kind: container::SECTION_POLICY.into(),
                    len: policy.len() as u64,
                });
            }
            sections.push(container::Section {
                kind: container::SECTION_VAULT.into(),
                len,
            });
            let kdf = container::Kdf::new_default()
                .map_err(|_| Failure::with("random_unavailable", json!({})))?;
            let mut writer = PasswordWriter::start(&mut out, password, Kind::C, kdf, sections)
                .map_err(failed)?;
            writer.write_all(&meta).map_err(failed)?;
            if let Some(policy) = policy {
                writer.write_all(policy).map_err(failed)?;
            }
            io::copy(&mut source, &mut writer).map_err(failed)?;
            writer.finish().map_err(failed)?;
        }
        Kind::D => {
            let prepared = package(material)?;
            let kdf = container::Kdf::new_default()
                .map_err(|_| Failure::with("random_unavailable", json!({})))?;
            let sections = vec![container::Section {
                kind: container::SECTION_PACKAGE.into(),
                len: prepared.total_len(),
            }];
            let mut writer = PasswordWriter::start(&mut out, password, Kind::D, kdf, sections)
                .map_err(failed)?;
            prepared
                .write_to(&mut writer, &mut source)
                .map_err(failed)?;
            writer.finish().map_err(failed)?;
        }
    }
    let guarded = out
        .into_inner()
        .map_err(|error| failed(error.into_error()))?;
    guarded.inner.sync_all().map_err(failed)?;
    Ok(source.digest())
}

/// Reads the new container back in full with the secrets just used: every tag must pass and the
/// vault bytes inside must be the snapshot's.
fn verify_container(
    temp: &Path,
    kind: Kind,
    material: Option<&Material>,
    protection: &Protection,
    digest: [u8; 32],
) -> Result<(), Failure> {
    checkpoint(Step::Verify).map_err(|_| VaultError::BackupFailed)?;
    fn failed<E>(_: E) -> Failure {
        VaultError::BackupFailed.into()
    }
    let len = fs::metadata(temp).map_err(failed)?.len();
    let mut file = BufReader::new(File::open(temp).map_err(failed)?);
    let mut sink = container::Hashing::new(io::sink());
    let with_key = |_: &container::HeaderB| -> Result<VaultKey, container::FormatError> {
        material
            .map(|material| material.key.duplicate())
            .ok_or(container::FormatError::Malformed)
    };
    let password = password_of(protection);
    match kind {
        Kind::B => {
            container::read_b(&mut file, len, with_key, &mut sink).map_err(failed)?;
        }
        Kind::C => {
            let (header, mut reader) =
                PasswordReader::open(file, len, password, Kind::C).map_err(failed)?;
            for section in &header.sections {
                match section.kind.as_str() {
                    container::SECTION_META => {
                        container::Meta::parse(
                            &container::read_small(&mut reader, section.len).map_err(failed)?,
                        )
                        .map_err(failed)?;
                    }
                    container::SECTION_POLICY => {
                        container::read_small(&mut reader, section.len).map_err(failed)?;
                    }
                    _ => container::copy_exact(&mut reader, section.len, &mut sink)
                        .map_err(failed)?,
                }
            }
            if !reader.is_complete() {
                return Err(VaultError::BackupFailed.into());
            }
        }
        Kind::D => {
            let (header, mut reader) =
                PasswordReader::open(file, len, password, Kind::D).map_err(failed)?;
            container::read_b(&mut reader, header.sections[0].len, with_key, &mut sink)
                .map_err(failed)?;
            if !reader.is_complete() {
                return Err(VaultError::BackupFailed.into());
            }
        }
    }
    if sink.digest() != digest {
        return Err(VaultError::BackupFailed.into());
    }
    Ok(())
}

/// Renames the verified container to a fresh name; never replaces a file.
fn publish(temp: &Path, dir: &Path, now: SystemTime) -> Result<PathBuf, Failure> {
    let name = vault::backup_file_name(now);
    let stem = name.trim_end_matches(".sqlite3");
    for attempt in 0..100 {
        let candidate = if attempt == 0 {
            dir.join(format!("{stem}.{EXTENSION}"))
        } else {
            dir.join(format!("{stem}-{attempt}.{EXTENSION}"))
        };
        match vault::rename_no_replace(temp, &candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(VaultError::BackupFailed.into()),
        }
    }
    Err(VaultError::FileExists.into())
}

/// The response fields of a finished backup.
pub fn response(written: &Written, vault_path: &str, vault_token: &str) -> Value {
    json!({
        "backupPath": display(&written.path),
        "vaultPath": vault_path,
        "vaultToken": vault_token,
        "format": written.kind.code(),
        "policyCopy": written.policy_included,
    })
}

#[cfg(test)]
pub(crate) fn work_dir_entries(app: &Path) -> Vec<String> {
    fs::read_dir(app.join(WORK_DIR))
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}
