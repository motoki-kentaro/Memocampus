//! Active vault resolution and lifecycle.
//!
//! The vault (SQLite file) is independent of the host executable. Its path is persisted in
//! `%LOCALAPPDATA%\Memocampus\config.json`. Vaults are only ever created by an explicit
//! request, opened without SQLite's create flag, and existing files are never overwritten.
//!
//! Issue #51 (Slice B0): the config records the vault's mode (`plain` / `encrypted`) and whether
//! it is published to MCP (see `active_vault`). Slice B: an encrypted vault is opened with its key from
//! this Windows user's key store (`secure_vault`); a key or mode error never counts as proof that
//! nobody writes to it.

use rusqlite::backup::{Backup, StepResult};
use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use serde::Serialize;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::images;
use memocampus_native_host::active_vault::{self, Publication, PublicationState, VaultMode};
use memocampus_native_host::schema::{self, SchemaError};

pub use memocampus_native_host::active_vault::{Config, CONFIG_VERSION};
const DEFAULT_VAULT_NAME: &str = "memocampus.sqlite3";
const BUSY_TIMEOUT: Duration = Duration::from_secs(3);
const LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const SIDECAR_SUFFIXES: [&str; 3] = ["-journal", "-wal", "-shm"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaultError {
    AppDirUnavailable,
    NotConfigured,
    ConfigInvalid,
    Missing,
    Invalid,
    SchemaMismatch,
    Switched,
    Busy,
    ManagementDisabled,
    FileExists,
    SidecarExists,
    ParentMissing,
    InvalidPath,
    Cancelled,
    BackupFailed,
    Io,
    Db,
    /// The vault's key could not be obtained (no key store entry, protector failure, mismatch).
    KeyUnavailable,
    /// The key exists but is not unlocked in this process (needs the user, e.g. the Recovery Spell).
    Locked,
    /// A key was applied but the vault refuses it: never proof that the file is unusable.
    DecryptFailed,
    /// The file contradicts the mode the config records (e.g. plain SQLite in encrypted mode).
    ModeMismatch,
    /// The engine or platform cannot open encrypted vaults (no SQLCipher, no key protector).
    EncryptionUnsupported,
    /// An encrypted vault of an earlier scheme (Slices B–D: a random key in a `.mckey` envelope),
    /// unsupported since Slice E. Nothing about it is changed.
    LegacyUnsupported,
}

impl VaultError {
    pub fn code(self) -> &'static str {
        match self {
            Self::AppDirUnavailable => "app_dir_unavailable",
            Self::NotConfigured => "vault_not_configured",
            Self::ConfigInvalid => "config_invalid",
            Self::Missing => "vault_missing",
            Self::Invalid => "vault_invalid",
            Self::SchemaMismatch => "vault_schema_mismatch",
            Self::Switched => "vault_switched",
            Self::Busy => "vault_busy",
            Self::ManagementDisabled => "management_disabled",
            Self::FileExists => "file_exists",
            Self::SidecarExists => "sidecar_exists",
            Self::ParentMissing => "parent_missing",
            Self::InvalidPath => "invalid_path",
            Self::Cancelled => "cancelled",
            Self::BackupFailed => "backup_failed",
            Self::Io => "io_error",
            Self::Db => "db_error",
            Self::KeyUnavailable => "key_unavailable",
            Self::Locked => "vault_locked",
            Self::DecryptFailed => "vault_decrypt_failed",
            Self::ModeMismatch => "vault_mode_mismatch",
            Self::EncryptionUnsupported => "vault_encryption_unsupported",
            Self::LegacyUnsupported => "vault_legacy_unsupported",
        }
    }
}

/// Process-level inputs to path resolution, separated so tests can supply their own.
#[derive(Debug, Clone)]
pub struct Environment {
    /// `%LOCALAPPDATA%\Memocampus` (or `MEMOCAMPUS_APP_DIR` for tests).
    pub app_dir: Option<PathBuf>,
    /// Development override `MEMOCAMPUS_DB`; disables vault management while set.
    pub db_override: Option<PathBuf>,
    pub exe_dir: Option<PathBuf>,
}

impl Environment {
    pub fn from_process() -> Self {
        let non_empty = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
        let app_dir = non_empty("MEMOCAMPUS_APP_DIR")
            .map(PathBuf::from)
            .or_else(|| non_empty("LOCALAPPDATA").map(|dir| PathBuf::from(dir).join("Memocampus")));
        Self {
            app_dir,
            db_override: non_empty("MEMOCAMPUS_DB").map(PathBuf::from),
            exe_dir: std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(Path::to_path_buf)),
        }
    }

    fn app_dir(&self) -> Result<&Path, VaultError> {
        self.app_dir.as_deref().ok_or(VaultError::AppDirUnavailable)
    }

    pub fn config_path(&self) -> Result<PathBuf, VaultError> {
        Ok(self.app_dir()?.join("config.json"))
    }

    pub fn default_vault_path(&self) -> Result<PathBuf, VaultError> {
        Ok(self.app_dir()?.join(DEFAULT_VAULT_NAME))
    }

    pub fn management_enabled(&self) -> bool {
        self.db_override.is_none() && self.app_dir.is_some()
    }
}

#[derive(Debug, PartialEq)]
pub enum Active {
    Override(PathBuf),
    Configured(Config),
    NotConfigured,
}

/// Resolves the active vault path without touching the vault itself. A version 1 config is read
/// as a plain vault with no publication decision (see `migrate_legacy_config`).
pub fn resolve(env: &Environment) -> Result<Active, VaultError> {
    if let Some(path) = &env.db_override {
        return Ok(Active::Override(path.clone()));
    }
    match active_vault::read_config(env.app_dir()?) {
        Ok(Some(config)) => Ok(Active::Configured(config)),
        Ok(None) => Ok(Active::NotConfigured),
        Err(active_vault::ConfigError::Io) => Err(VaultError::Io),
        Err(active_vault::ConfigError::Invalid) => Err(VaultError::ConfigInvalid),
    }
}

/// The steps of `write_config`, for fault injection in tests.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigStep {
    Create,
    Write,
    Sync,
    /// The process dies after the temp file is complete, before the replace: the temp file stays.
    CrashBeforeReplace,
    Replace,
}

#[cfg(test)]
thread_local! {
    pub(crate) static CONFIG_FAULT: std::cell::Cell<Option<ConfigStep>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn inject(step: ConfigStep) -> io::Result<()> {
    if CONFIG_FAULT.with(std::cell::Cell::get) == Some(step) {
        return Err(io::Error::other("injected"));
    }
    Ok(())
}

/// Replaces `config.json` as one step: a unique temp file is written and flushed, renamed over
/// the old file through its own handle, and flushed again after the rename. Readers see the old or
/// the new file, never a mixture, and on any failure the old file stays in place. Always writes
/// the current version.
///
/// On Windows the rename uses POSIX semantics (`FileRenameInfoEx`): the name switches atomically
/// even while readers (the MCP server, other host processes) have the old file open. A classic
/// `MoveFileExW(MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)` fails with
/// `ERROR_ACCESS_DENIED` in that case (measured in Slice B0: most replaces failed under concurrent
/// readers), and `MOVEFILE_WRITE_THROUGH` only matters for moves done as copy and delete.
///
/// Durability: the flush after the rename asks NTFS to commit the file, including the journaled
/// rename. Whether a given disk honours that across a power cut is outside the program's control
/// (write caches without power-loss protection); this is not verified by an actual power cut.
pub fn write_config(env: &Environment, config: &Config) -> Result<(), VaultError> {
    let target = env.config_path()?;
    let dir = env.app_dir()?;
    fs::create_dir_all(dir).map_err(|_| VaultError::Io)?;
    let config = Config {
        version: CONFIG_VERSION,
        ..config.clone()
    };
    let temp = dir.join(format!("config.json.{}.tmp", uuid::Uuid::new_v4()));
    #[allow(unused_mut)]
    let mut crashed = false;
    let result = (|| {
        #[cfg(test)]
        inject(ConfigStep::Create)?;
        let mut file = create_renamable(&temp)?;
        #[cfg(test)]
        inject(ConfigStep::Write)?;
        file.write_all(&config.to_bytes())?;
        #[cfg(test)]
        inject(ConfigStep::Sync)?;
        file.sync_all()?;
        #[cfg(test)]
        if inject(ConfigStep::CrashBeforeReplace).is_err() {
            crashed = true;
            return Err(io::Error::other("injected crash"));
        }
        #[cfg(test)]
        inject(ConfigStep::Replace)?;
        replace_by_handle(file, &temp, &target)
    })();
    if result.is_err() && !crashed {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(|_| VaultError::Io)
}

/// A new file opened so that it can later be renamed through its own handle.
#[cfg(windows)]
fn create_renamable(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    const DELETE: u32 = 0x0001_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .access_mode(GENERIC_WRITE | DELETE)
        .open(path)
}

#[cfg(not(windows))]
fn create_renamable(path: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

/// Renames the open `file` (at `from`) to `to`, replacing it atomically, then flushes it.
#[cfg(windows)]
fn replace_by_handle(file: File, from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FileRenameInfoEx, SetFileInformationByHandle, FILE_RENAME_INFO,
    };
    const FILE_RENAME_FLAG_REPLACE_IF_EXISTS: u32 = 0x1;
    const FILE_RENAME_FLAG_POSIX_SEMANTICS: u32 = 0x2;
    const ERROR_INVALID_FUNCTION: i32 = 1;
    const ERROR_NOT_SUPPORTED: i32 = 50;
    const ERROR_INVALID_PARAMETER: i32 = 87;
    let name: Vec<u16> = to.as_os_str().encode_wide().collect();
    let offset = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
    let size = offset + (name.len() + 1) * 2;
    // u64 storage keeps the struct's alignment; zeroed, so the name is NUL-terminated.
    let mut buffer = vec![0u64; size.div_ceil(8)];
    let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    // SAFETY: `buffer` holds `size` bytes, enough for the struct and the name with its NUL.
    let renamed = unsafe {
        (*info).Anonymous.Flags =
            FILE_RENAME_FLAG_REPLACE_IF_EXISTS | FILE_RENAME_FLAG_POSIX_SEMANTICS;
        (*info).RootDirectory = std::ptr::null_mut();
        (*info).FileNameLength = (name.len() * 2) as u32;
        std::ptr::copy_nonoverlapping(
            name.as_ptr(),
            std::ptr::addr_of_mut!((*info).FileName).cast::<u16>(),
            name.len(),
        );
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileRenameInfoEx,
            info.cast(),
            size as u32,
        )
    };
    if renamed == 0 {
        let error = io::Error::last_os_error();
        // File systems without POSIX rename semantics (FAT, older Windows): the standard replace.
        match error.raw_os_error() {
            Some(ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED | ERROR_INVALID_PARAMETER) => {
                fs::rename(from, to)?
            }
            _ => return Err(error),
        }
    }
    file.sync_all()
}

#[cfg(not(windows))]
fn replace_by_handle(file: File, from: &Path, to: &Path) -> io::Result<()> {
    drop(file);
    fs::rename(from, to)?;
    if let Some(dir) = to.parent() {
        File::open(dir)?.sync_all()?;
    }
    Ok(())
}

/// Serializes vault management across the short-lived host processes. The OS releases the
/// lock when the holding process exits, so a crash cannot leave it stuck.
pub struct ConfigLock {
    _file: File,
}

impl ConfigLock {
    pub fn acquire(env: &Environment) -> Result<Self, VaultError> {
        Self::acquire_within(env, LOCK_TIMEOUT)
    }

    pub fn acquire_within(env: &Environment, timeout: Duration) -> Result<Self, VaultError> {
        let dir = env.app_dir()?;
        fs::create_dir_all(dir).map_err(|_| VaultError::Io)?;
        let path = dir.join("config.lock");
        let deadline = Instant::now() + timeout;
        loop {
            match open_exclusive(&path) {
                Ok(file) => return Ok(Self { _file: file }),
                Err(error) if is_sharing_violation(&error) && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(error) if is_sharing_violation(&error) => return Err(VaultError::Busy),
                Err(_) => return Err(VaultError::Io),
            }
        }
    }
}

#[cfg(windows)]
fn open_exclusive(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(path)
}

#[cfg(not(windows))]
fn open_exclusive(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

fn is_sharing_violation(error: &io::Error) -> bool {
    const ERROR_SHARING_VIOLATION: i32 = 32;
    error.raw_os_error() == Some(ERROR_SHARING_VIOLATION)
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// The underlying reason a vault could not be opened, with the original SQLite or OS code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenCause {
    NotFound,
    NotAFile,
    /// `std::io::Error::raw_os_error` (a Win32 error code on Windows).
    Os(Option<i32>),
    /// Primary and extended SQLite result codes.
    Sqlite {
        code: rusqlite::ErrorCode,
        extended: i32,
    },
    /// A rusqlite error that carries no SQLite result code.
    Driver,
    /// SQLite fell back to read-only access (e.g. read-only file attribute, missing write permission).
    ReadOnly,
    ForeignApplicationId(i64),
    /// Untagged, or a `user_version` other than the supported one.
    UnsupportedVersion(i64),
    /// The named object is missing or differs from the schema definition.
    SchemaMismatch(&'static str),
    IntegrityCheckFailed,
    /// The key could not be obtained.
    Key(KeyError),
    /// The first bytes of the file contradict the recorded mode.
    ModeMismatch,
}

pub use memocampus_native_host::secure_vault::secret::{CipherSalt, VaultId, VaultKey};
pub use memocampus_native_host::secure_vault::KeyError;
use memocampus_native_host::secure_vault::{cipher, key_store};

/// The vault error a key failure is reported as. None of them proves anything about the file.
pub fn key_kind(error: KeyError) -> VaultError {
    match error {
        KeyError::NotSupported => VaultError::EncryptionUnsupported,
        KeyError::Missing | KeyError::ProtectorFailed | KeyError::Mismatch | KeyError::Io => {
            VaultError::KeyUnavailable
        }
        KeyError::Locked => VaultError::Locked,
        KeyError::Legacy => VaultError::LegacyUnsupported,
    }
}

/// What a key source is asked: the key of the encrypted vault `vault_id` whose file starts with
/// `salt` (read from the file itself, never from the config).
pub struct KeyRequest {
    pub vault_id: Option<VaultId>,
    pub salt: CipherSalt,
}

/// Where the key of an encrypted vault comes from.
pub trait KeySource {
    fn key_for(&self, request: &KeyRequest) -> Result<VaultKey, KeyError>;
}

/// No keys at all: encrypted vaults cannot be unlocked (tests, and the key-independent paths).
pub struct NoKeys;

impl KeySource for NoKeys {
    fn key_for(&self, _: &KeyRequest) -> Result<VaultKey, KeyError> {
        Err(KeyError::NotSupported)
    }
}

/// The production key source: this Windows user's DPAPI key store in the app directory
/// (`secure_vault::key_store`). A missing or unusable entry is an error; a key is never created
/// here.
pub struct KeyStore {
    app_dir: Option<PathBuf>,
}

impl KeyStore {
    pub fn of(env: &Environment) -> Self {
        Self {
            app_dir: env.app_dir.clone(),
        }
    }
}

impl KeySource for KeyStore {
    fn key_for(&self, request: &KeyRequest) -> Result<VaultKey, KeyError> {
        let (Some(app_dir), Some(vault_id)) = (&self.app_dir, &request.vault_id) else {
            return Err(KeyError::Missing);
        };
        key_store::unlock(app_dir, vault_id, &request.salt)
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenFailure {
    pub kind: VaultError,
    pub cause: OpenCause,
    /// The mode the open was attempted in (from the config).
    pub mode: VaultMode,
}

impl OpenFailure {
    fn new(kind: VaultError, cause: OpenCause) -> Self {
        Self {
            kind,
            cause,
            mode: VaultMode::Plain,
        }
    }

    fn in_mode(self, mode: VaultMode) -> Self {
        Self { mode, ..self }
    }

    fn key(error: KeyError) -> Self {
        Self::new(key_kind(error), OpenCause::Key(error)).in_mode(VaultMode::Encrypted)
    }

    /// The key-independent cases in which a switch away from this vault must hold an exclusive
    /// handle on the file instead of its write lock (see `switch_active_with`): this process
    /// cannot open the vault the way a writer holding the key would.
    pub fn needs_handle_fence(&self) -> bool {
        matches!(
            self.kind,
            VaultError::KeyUnavailable
                | VaultError::Locked
                | VaultError::DecryptFailed
                | VaultError::ModeMismatch
                | VaultError::EncryptionUnsupported
                | VaultError::LegacyUnsupported
        )
    }

    fn from_io(error: &io::Error) -> Self {
        if error.kind() == io::ErrorKind::NotFound {
            Self::new(VaultError::Missing, OpenCause::NotFound)
        } else {
            Self::new(VaultError::Io, OpenCause::Os(error.raw_os_error()))
        }
    }

    /// Maps a rusqlite error, keeping its codes. Only "not a database" and "corrupt" mean the file
    /// is definitively unusable; lock contention is `Busy`, permission and I/O problems are `Io`,
    /// and anything else is an unexplained `Db` error.
    fn from_sqlite(error: &rusqlite::Error) -> Self {
        Self::from_sqlite_in(error, VaultMode::Plain)
    }

    /// `from_sqlite` for a connection opened in `mode`. With a key applied, "not a database" means
    /// the key was refused (`DecryptFailed`), not that the file is unusable: SQLCipher answers a
    /// wrong key and a missing key alike with `SQLITE_NOTADB` (PoC A-8.2).
    fn from_sqlite_in(error: &rusqlite::Error, mode: VaultMode) -> Self {
        use rusqlite::ErrorCode;
        let rusqlite::Error::SqliteFailure(inner, _) = error else {
            return Self::new(VaultError::Db, OpenCause::Driver).in_mode(mode);
        };
        let kind = match inner.code {
            ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked => VaultError::Busy,
            ErrorCode::NotADatabase if mode == VaultMode::Encrypted => VaultError::DecryptFailed,
            ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt => VaultError::Invalid,
            ErrorCode::CannotOpen
            | ErrorCode::SystemIoFailure
            | ErrorCode::PermissionDenied
            | ErrorCode::ReadOnly => VaultError::Io,
            _ => VaultError::Db,
        };
        Self::new(
            kind,
            OpenCause::Sqlite {
                code: inner.code,
                extended: inner.extended_code,
            },
        )
        .in_mode(mode)
    }

    /// True only when no writer can possibly be using the vault: it does not exist, or its own
    /// contents prove it is not a Memocampus vault (writers run the same checks and are refused).
    /// Both the classification and the cause must agree, so a misclassified transient error can
    /// never qualify.
    ///
    /// Only a plain vault's contents can prove anything: a process that cannot key an encrypted
    /// vault learns nothing about processes that can (HIGH-4). There, only absence qualifies.
    pub fn proves_unwritable(&self) -> bool {
        use rusqlite::ErrorCode;
        if self.mode != VaultMode::Plain {
            return matches!(
                (&self.kind, &self.cause),
                (VaultError::Missing, OpenCause::NotFound)
            );
        }
        match (&self.kind, &self.cause) {
            (VaultError::Missing, OpenCause::NotFound) => true,
            (
                VaultError::SchemaMismatch,
                OpenCause::UnsupportedVersion(_) | OpenCause::SchemaMismatch(_),
            ) => true,
            (VaultError::Invalid, cause) => matches!(
                cause,
                OpenCause::NotAFile
                    | OpenCause::ForeignApplicationId(_)
                    | OpenCause::Sqlite {
                        code: ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt,
                        ..
                    }
            ),
            _ => false,
        }
    }
}

impl From<OpenFailure> for VaultError {
    fn from(failure: OpenFailure) -> Self {
        failure.kind
    }
}

/// Only schema 4 is ever used, for every operation. Older vaults are refused here, unchanged.
fn validate_schema(db: &Connection) -> Result<(), OpenFailure> {
    validate_schema_in(db, VaultMode::Plain)
}

/// `validate_schema` on a connection opened in `mode`: on a keyed connection, the first read is
/// what tells a refused key (`DecryptFailed`) from success.
fn validate_schema_in(db: &Connection, mode: VaultMode) -> Result<(), OpenFailure> {
    let in_mode = |failure: OpenFailure| failure.in_mode(mode);
    schema::check(db)
        .map_err(|error| match error {
            SchemaError::ForeignApplicationId(id) => {
                OpenFailure::new(VaultError::Invalid, OpenCause::ForeignApplicationId(id))
            }
            SchemaError::UnsupportedVersion(version) => OpenFailure::new(
                VaultError::SchemaMismatch,
                OpenCause::UnsupportedVersion(version),
            ),
            SchemaError::Shape(name) => {
                OpenFailure::new(VaultError::SchemaMismatch, OpenCause::SchemaMismatch(name))
            }
            SchemaError::Sqlite(error) => OpenFailure::from_sqlite_in(&error, mode),
        })
        .map_err(in_mode)
}

/// Opens an existing vault. Never creates a file: SQLite's create flag is not passed.
#[cfg_attr(not(test), allow(dead_code))]
pub fn open_vault(path: &Path) -> Result<Connection, VaultError> {
    open_vault_detailed(path).map_err(VaultError::from)
}

/// `open_vault` with the full failure cause.
pub fn open_vault_detailed(path: &Path) -> Result<Connection, OpenFailure> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return Err(OpenFailure::new(VaultError::Invalid, OpenCause::NotAFile)),
        Err(error) => return Err(OpenFailure::from_io(&error)),
    }
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| match fs::metadata(path) {
        // Only a confirmed absence is "missing"; an unreadable path keeps the SQLite cause.
        Err(io) if io.kind() == io::ErrorKind::NotFound => OpenFailure::from_io(&io),
        _ => OpenFailure::from_sqlite(&error),
    })?;
    db.busy_timeout(BUSY_TIMEOUT)
        .map_err(|error| OpenFailure::from_sqlite(&error))?;
    db.execute_batch("PRAGMA foreign_keys = ON;")
        .map_err(|error| OpenFailure::from_sqlite(&error))?;
    validate_schema(&db)?;
    // SQLite silently opens read-only when read-write access is denied; such a vault cannot
    // take writes or a write fence, which is a permission problem, not a format problem.
    if db
        .is_readonly(rusqlite::DatabaseName::Main)
        .map_err(|error| OpenFailure::from_sqlite(&error))?
    {
        return Err(OpenFailure::new(VaultError::Io, OpenCause::ReadOnly));
    }
    Ok(db)
}

/// Opens the vault in the mode its config records. Plain: `open_vault_detailed`. Encrypted: the
/// file must not be plain SQLite, its key must be obtainable for `vault_id` and the salt the file
/// starts with, and the keyed connection must pass every check a plain one passes. A key that
/// SQLCipher refuses is `DecryptFailed`, never "not a vault". The file is never created or
/// initialized here.
pub fn open_vault_in(
    path: &Path,
    mode: VaultMode,
    vault_id: Option<VaultId>,
    keys: &dyn KeySource,
) -> Result<Connection, OpenFailure> {
    match mode {
        VaultMode::Plain => open_vault_detailed(path),
        VaultMode::Encrypted => open_encrypted(path, vault_id, keys),
    }
}

fn open_encrypted(
    path: &Path,
    vault_id: Option<VaultId>,
    keys: &dyn KeySource,
) -> Result<Connection, OpenFailure> {
    let encrypted = |failure: OpenFailure| failure.in_mode(VaultMode::Encrypted);
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => {
            return Err(encrypted(OpenFailure::new(
                VaultError::Invalid,
                OpenCause::NotAFile,
            )))
        }
        Err(error) => return Err(encrypted(OpenFailure::from_io(&error))),
    }
    // A SQLCipher file starts with its random salt; plain SQLite (or a file too short to hold a
    // salt) contradicts the recorded mode. Never resolved by switching modes.
    let salt = match cipher::read_head(path) {
        Ok(cipher::Head::Salt(salt)) => salt,
        Ok(_) => {
            return Err(encrypted(OpenFailure::new(
                VaultError::ModeMismatch,
                OpenCause::ModeMismatch,
            )))
        }
        Err(error) => return Err(encrypted(OpenFailure::from_io(&error))),
    };
    let key = keys
        .key_for(&KeyRequest { vault_id, salt })
        .map_err(OpenFailure::key)?;
    let db = cipher::open(
        path,
        &key,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| match error {
        cipher::KeyingError::Unavailable => OpenFailure::key(KeyError::NotSupported),
        cipher::KeyingError::Sqlite(error) => match fs::metadata(path) {
            Err(io) if io.kind() == io::ErrorKind::NotFound => encrypted(OpenFailure::from_io(&io)),
            _ => OpenFailure::from_sqlite_in(&error, VaultMode::Encrypted),
        },
    })?;
    drop(key);
    let sqlite = |error: rusqlite::Error| OpenFailure::from_sqlite_in(&error, VaultMode::Encrypted);
    db.busy_timeout(BUSY_TIMEOUT).map_err(sqlite)?;
    db.execute_batch("PRAGMA foreign_keys = ON;")
        .map_err(sqlite)?;
    validate_schema_in(&db, VaultMode::Encrypted)?;
    if db
        .is_readonly(rusqlite::DatabaseName::Main)
        .map_err(sqlite)?
    {
        return Err(encrypted(OpenFailure::new(
            VaultError::Io,
            OpenCause::ReadOnly,
        )));
    }
    Ok(db)
}
/// `open_vault` plus a full structural check, for when the user picks a file to open.
pub fn open_vault_checked(path: &Path) -> Result<Connection, VaultError> {
    open_checked_in(path, VaultMode::Plain, None, &NoKeys).map_err(VaultError::from)
}

/// `open_vault_in` plus `PRAGMA quick_check`, for a file the user picks or a vault just made.
pub fn open_checked_in(
    path: &Path,
    mode: VaultMode,
    vault_id: Option<VaultId>,
    keys: &dyn KeySource,
) -> Result<Connection, OpenFailure> {
    let db = open_vault_in(path, mode, vault_id, keys)?;
    let result: String = db
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(|error| OpenFailure::from_sqlite_in(&error, mode))?;
    if result != "ok" {
        return Err(
            OpenFailure::new(VaultError::Invalid, OpenCause::IntegrityCheckFailed).in_mode(mode),
        );
    }
    Ok(db)
}

/// Creates a brand-new vault at `path` with exclusive file creation; an existing file is
/// never touched. A leftover journal/WAL beside the target is refused because SQLite would
/// apply it to the new database.
pub fn create_vault(path: &Path) -> Result<(), VaultError> {
    if SIDECAR_SUFFIXES
        .iter()
        .any(|suffix| sidecar(path, suffix).exists())
    {
        return Err(VaultError::SidecarExists);
    }
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(file) => drop(file),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            return Err(VaultError::FileExists)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(VaultError::ParentMissing)
        }
        Err(_) => return Err(VaultError::Io),
    }
    let result = (|| {
        let db = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|_| VaultError::Db)?;
        db.execute_batch(&format!("BEGIN; {} COMMIT;", schema::create_sql()))
            .map_err(|_| VaultError::Db)?;
        validate_schema(&db).map_err(VaultError::from)
    })();
    if result.is_err() {
        // Only the empty file this call created exclusively is removed.
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(sidecar(path, "-journal"));
    }
    result
}

/// Points the configuration at `new_path`. While the config is rewritten, the previously
/// active vault is write-locked, so every write to it either commits before the switch or
/// observes the new configuration (see `Session::write`).
///
/// The fence is skipped only when no writer can be writing to the old vault: it does not
/// exist, or it is definitively not a vault (writers open vaults the same way and would be
/// refused). Any other failure to open it (lock contention, I/O) aborts the switch with the
/// configuration unchanged; the caller may retry.
pub fn switch_active(
    env: &Environment,
    lock: &ConfigLock,
    new_path: &Path,
    location_approved: bool,
) -> Result<(), VaultError> {
    switch_active_with(env, lock, new_path, location_approved, &KeyStore::of(env))
}

/// The vault a switch makes active.
#[derive(Debug, Clone, Copy)]
pub struct NewActive<'a> {
    pub path: &'a Path,
    pub mode: VaultMode,
    /// Required for an encrypted vault (its key store entry), None for a plain one.
    pub vault_id: Option<VaultId>,
}

impl<'a> NewActive<'a> {
    pub fn plain(path: &'a Path) -> Self {
        Self {
            path,
            mode: VaultMode::Plain,
            vault_id: None,
        }
    }

    pub fn encrypted(path: &'a Path, vault_id: VaultId) -> Self {
        Self {
            path,
            mode: VaultMode::Encrypted,
            vault_id: Some(vault_id),
        }
    }
}

/// What keeps writers off the old vault while the config is replaced.
enum Fence {
    /// Nothing can be writing to the old vault.
    None,
    /// Its write lock (`BEGIN IMMEDIATE`): every writer is waited for and then blocked.
    WriteLock(Connection),
    /// An open handle without any sharing: it can only be taken while no process has the file
    /// open, and while it is held nobody can open it. It needs no key (Slice A, PoC A-7.1).
    Handle(#[allow(dead_code)] File),
}

/// Takes the exclusive handle. A file that vanished needs no fence (nothing recreates vaults).
fn handle_fence(path: &Path) -> Result<Fence, VaultError> {
    match open_unshared(path) {
        Ok(file) => Ok(Fence::Handle(file)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Fence::None),
        Err(error) if is_sharing_violation(&error) => Err(VaultError::Busy),
        Err(_) => Err(VaultError::Io),
    }
}

#[cfg(windows)]
fn open_unshared(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    // Read access only, never created: the file is not changed in any way.
    OpenOptions::new().read(true).share_mode(0).open(path)
}

/// Without Windows share modes there is no key-independent fence: refuse rather than switch
/// unfenced (non-Windows is out of scope, residual risk R10).
#[cfg(not(windows))]
fn open_unshared(_: &Path) -> io::Result<File> {
    Err(io::Error::from_raw_os_error(32))
}

#[cfg(test)]
thread_local! {
    /// Tests only: runs while the switch holds its fence, before the config is replaced.
    pub(crate) static WHILE_FENCED: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        const { std::cell::RefCell::new(None) };
}

/// `switch_active` with the key source used to open an encrypted old vault.
///
/// The fence over the old vault (see `switch_active`) is chosen by what opening it shows:
/// - opened: its write lock;
/// - proven unwritable (absent, or a plain file that is no vault): none;
/// - not openable by this process for a key or mode reason: the exclusive handle, which fails
///   while any process (with or without the key) has the file open;
/// - anything else (lock contention, I/O): the switch is refused, config unchanged.
///
/// The new config names `new_path` as a plain vault. Its publication is pending, unless the new
/// vault is the very file (same identity) whose publication was already confirmed.
pub fn switch_active_with(
    env: &Environment,
    lock: &ConfigLock,
    new_path: &Path,
    location_approved: bool,
    keys: &dyn KeySource,
) -> Result<(), VaultError> {
    switch_active_to(
        env,
        lock,
        &NewActive::plain(new_path),
        location_approved,
        keys,
        false,
    )
}

/// `switch_active_with` for a vault of either mode. `fresh_publication`: the new vault starts
/// pending even if it is the very file whose publication was confirmed (a recovered vault).
pub fn switch_active_to(
    env: &Environment,
    lock: &ConfigLock,
    new: &NewActive,
    location_approved: bool,
    keys: &dyn KeySource,
    fresh_publication: bool,
) -> Result<(), VaultError> {
    switch_active_fenced(
        env,
        lock,
        new,
        location_approved,
        keys,
        fresh_publication,
        &mut || Ok(()),
    )
}

/// `switch_active_to` that runs `while_fenced` while the old vault's writers are fenced off, just
/// before the config is replaced (Issue #51 Slice D: the last copy of a converted vault, so that no
/// write to the old vault can be missing from the new one). If it fails, the config is not changed.
pub fn switch_active_fenced(
    env: &Environment,
    _lock: &ConfigLock,
    new: &NewActive,
    location_approved: bool,
    keys: &dyn KeySource,
    fresh_publication: bool,
    while_fenced: &mut dyn FnMut() -> Result<(), VaultError>,
) -> Result<(), VaultError> {
    let new_path = new.path;
    if (new.mode == VaultMode::Encrypted) != new.vault_id.is_some() {
        return Err(VaultError::InvalidPath);
    }
    let (fence, previous) = match resolve(env) {
        Ok(Active::Configured(config)) => {
            let path = PathBuf::from(&config.active_vault);
            let fence = match open_vault_in(&path, config.mode, config.vault_id, keys) {
                Ok(db) => Fence::WriteLock(db),
                Err(failure) if failure.proves_unwritable() => Fence::None,
                Err(failure) if failure.needs_handle_fence() => handle_fence(&path)?,
                Err(failure) => return Err(failure.kind),
            };
            (fence, Some(config))
        }
        // Nothing resolvable is active, so no writer can be using an old vault.
        Ok(Active::NotConfigured) | Err(VaultError::ConfigInvalid) => (Fence::None, None),
        Ok(Active::Override(_)) => return Err(VaultError::ManagementDisabled),
        Err(error) => return Err(error),
    };
    if let Fence::WriteLock(db) = &fence {
        db.execute_batch("BEGIN IMMEDIATE")
            .map_err(|_| VaultError::Busy)?;
    }
    #[cfg(test)]
    WHILE_FENCED.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook();
        }
    });
    if let Err(error) = while_fenced() {
        if let Fence::WriteLock(db) = &fence {
            let _ = db.execute_batch("ROLLBACK");
        }
        return Err(error);
    }
    let token = active_vault::vault_token(new_path).ok();
    let publication = token.map(
        |token| match previous.and_then(|config| config.publication) {
            Some(kept) if kept.vault_token == token && !fresh_publication => kept,
            _ => Publication {
                vault_token: token,
                state: PublicationState::Pending,
                policy_revision: None,
            },
        },
    );
    let config = Config {
        publication,
        vault_id: new.vault_id,
        ..Config::new(
            new_path.to_string_lossy().into_owned(),
            location_approved,
            new.mode,
        )
    };
    let result = write_config(env, &config);
    if let Fence::WriteLock(db) = &fence {
        let _ = db.execute_batch("ROLLBACK");
    }
    drop(fence);
    result
}

/// A publication decision by the user (`vault_publication_confirm` / `_revoke`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicationChange {
    /// Publish to MCP, recording the policy revision the user saw.
    Confirm { policy_revision: u64 },
    /// Back to pending: MCP reads nothing from it.
    Revoke,
}

/// Records a publication decision for the active vault, which must be the vault the user saw
/// (`expected_token`, from `vault_status`); otherwise `Switched` and nothing is written. Runs under
/// the config lock, like every switch, so a decision can never land on another vault.
///
/// Confirming needs the vault to open (a plain vault passing every check) and takes its identity
/// while it is open. Revoking only needs its identity: making a vault pending is always safe.
pub fn set_publication(
    env: &Environment,
    _lock: &ConfigLock,
    expected_token: &str,
    change: PublicationChange,
) -> Result<Config, VaultError> {
    let config = match resolve(env)? {
        Active::Configured(config) => config,
        Active::NotConfigured => return Err(VaultError::NotConfigured),
        Active::Override(_) => return Err(VaultError::ManagementDisabled),
    };
    let path = PathBuf::from(&config.active_vault);
    let token = match change {
        PublicationChange::Confirm { .. } => {
            let db = open_vault_in(&path, config.mode, config.vault_id, &KeyStore::of(env))
                .map_err(VaultError::from)?;
            let token = vault_token(&path)?;
            drop(db);
            token
        }
        PublicationChange::Revoke => vault_token(&path)?,
    };
    if token != expected_token {
        return Err(VaultError::Switched);
    }
    let (state, policy_revision) = match change {
        PublicationChange::Confirm { policy_revision } => {
            (PublicationState::Confirmed, Some(policy_revision))
        }
        PublicationChange::Revoke => (PublicationState::Pending, None),
    };
    let updated = Config {
        publication: Some(Publication {
            vault_token: token,
            state,
            policy_revision,
        }),
        ..config
    };
    write_config(env, &updated)?;
    Ok(updated)
}

/// What `withdraw_publication` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Withdrawal {
    /// Nothing was published: no config, no decision, already pending, an invalid config (which
    /// MCP refuses anyway) or vault management disabled.
    NothingPublished,
    /// A confirmed publication was set back to pending.
    Withdrawn,
    /// A version 1 config, which a later start would have migrated as confirmed, was rewritten as
    /// version 2 with no publication decision.
    LegacyClosed,
}

/// Sets the active vault's publication back to pending, whatever vault it is (Issue #51, F-02):
/// run before a policy is (re)initialized, so an approval given under one policy never carries
/// over to a new one. Only one publication is ever stored, and it is the active vault's, so no
/// other vault's decision can be touched; its token is kept as recorded.
///
/// Takes the config lock (waiting as a switch does). Any failure to read or write the config is
/// an error, and the caller must then not initialize.
pub fn withdraw_publication(env: &Environment) -> Result<Withdrawal, VaultError> {
    if !env.management_enabled() {
        return Ok(Withdrawal::NothingPublished);
    }
    let _lock = ConfigLock::acquire(env)?;
    let config = match resolve(env) {
        Ok(Active::Configured(config)) => config,
        Ok(_) | Err(VaultError::ConfigInvalid) => return Ok(Withdrawal::NothingPublished),
        Err(error) => return Err(error),
    };
    let (updated, outcome) = if config.is_legacy() {
        let updated = Config::new(
            config.active_vault,
            config.location_approved,
            VaultMode::Plain,
        );
        (updated, Withdrawal::LegacyClosed)
    } else {
        match &config.publication {
            Some(publication) if publication.state == PublicationState::Confirmed => {
                let publication = Publication {
                    vault_token: publication.vault_token.clone(),
                    state: PublicationState::Pending,
                    policy_revision: None,
                };
                let updated = Config {
                    publication: Some(publication),
                    ..config
                };
                (updated, Withdrawal::Withdrawn)
            }
            _ => return Ok(Withdrawal::NothingPublished),
        }
    };
    write_config(env, &updated)?;
    Ok(outcome)
}

/// The global policy as `migrate_legacy_config` needs to know it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyReadiness {
    /// Initialized, valid and supported, at this revision.
    Ready(u64),
    /// Definitively not usable: never initialized, damaged or unsupported.
    NotReady,
    /// Cannot tell now (locked, I/O): decide later.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Migration {
    /// No version 1 config (or vault management is disabled).
    NotNeeded,
    /// Rewritten as version 2 with this publication.
    Migrated(Option<Publication>),
    /// Something could not be decided now; the version 1 config is kept and tried again later.
    Deferred,
}

/// Upgrades a version 1 config to version 2 (Issue #51, PO decision P3). The active vault keeps
/// its MCP access (`confirmed`) only if it opens as a valid vault **and** the global policy is
/// ready; otherwise it is recorded as `pending`, which the user confirms in the side panel. A
/// vault that cannot be checked now (busy, I/O) or a policy that cannot be read now defers the
/// migration; nothing is decided from a transient failure. Never waits for the config lock and
/// never changes a vault.
pub fn migrate_legacy_config(
    env: &Environment,
    policy: impl FnOnce() -> PolicyReadiness,
) -> Migration {
    if !env.management_enabled() {
        return Migration::NotNeeded;
    }
    match resolve(env) {
        Ok(Active::Configured(config)) if config.is_legacy() => {}
        _ => return Migration::NotNeeded,
    }
    let Ok(lock) = ConfigLock::acquire_within(env, Duration::ZERO) else {
        return Migration::Deferred;
    };
    // Again under the lock: another process may have migrated or switched meanwhile.
    let config = match resolve(env) {
        Ok(Active::Configured(config)) if config.is_legacy() => config,
        _ => return Migration::NotNeeded,
    };
    let path = PathBuf::from(&config.active_vault);
    let token = match open_vault_detailed(&path) {
        Ok(db) => {
            let token = vault_token(&path).ok();
            drop(db);
            match token {
                Some(token) => Some(token),
                None => return Migration::Deferred,
            }
        }
        Err(failure) if failure.proves_unwritable() => None,
        Err(_) => return Migration::Deferred,
    };
    let state = match (token.is_some(), policy()) {
        (_, PolicyReadiness::Unknown) => return Migration::Deferred,
        (true, PolicyReadiness::Ready(revision)) => {
            Some((PublicationState::Confirmed, Some(revision)))
        }
        (true, PolicyReadiness::NotReady) => Some((PublicationState::Pending, None)),
        (false, _) => None,
    };
    let publication = token
        .zip(state)
        .map(|(vault_token, (state, policy_revision))| Publication {
            vault_token,
            state,
            policy_revision,
        });
    let migrated = Config {
        publication: publication.clone(),
        ..Config::new(
            config.active_vault,
            config.location_approved,
            VaultMode::Plain,
        )
    };
    let result = write_config(env, &migrated);
    drop(lock);
    match result {
        Ok(()) => Migration::Migrated(publication),
        Err(_) => Migration::Deferred,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub visits: i64,
    pub snapshots: i64,
    pub pages: i64,
    #[serde(rename = "pageBlobs")]
    pub page_blobs: i64,
}

pub fn counts(db: &Connection) -> rusqlite::Result<Counts> {
    db.query_row(
        "SELECT (SELECT count(*) FROM visits), (SELECT count(*) FROM tab_snapshots), (SELECT count(*) FROM saved_pages), (SELECT count(*) FROM page_blobs)",
        [],
        |row| {
            Ok(Counts {
                visits: row.get(0)?,
                snapshots: row.get(1)?,
                pages: row.get(2)?,
                page_blobs: row.get(3)?,
            })
        },
    )
}

/// Rows of `navigation_events`, `image_tags` and `image_tag_links`, checked by backups only
/// (`Counts` is what `vault_status` reports).
fn backup_only_counts(db: &Connection) -> rusqlite::Result<(i64, i64, i64)> {
    db.query_row(
        "SELECT (SELECT count(*) FROM navigation_events), (SELECT count(*) FROM image_tags), (SELECT count(*) FROM image_tag_links)",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
}

/// The vault an operation started against (see `Session::ticket` / `Session::write_for`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultTicket {
    pub path: String,
    /// Opaque identity of the vault file (see `vault_token`), safe to hand to the extension.
    pub token: String,
}

/// Identifies a vault file independently of how its path is spelled (see
/// `active_vault::vault_token`). Only the hash leaves the host, never the path. Replacing the file
/// (e.g. restoring a backup) yields a new token.
pub fn vault_token(path: &Path) -> Result<String, VaultError> {
    active_vault::vault_token(path).map_err(|error| OpenFailure::from_io(&error).kind)
}

/// The active vault as one resolution of the config names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveSpec {
    pub path: String,
    pub mode: VaultMode,
    pub vault_id: Option<VaultId>,
}

/// Holds the connection to the active vault for one host process.
///
/// Consistency with a concurrent vault switch (`switch_active`):
/// - Writes are exact. A write commits to a vault only while the config still names it, checked
///   under that vault's write lock, and the switch holds the same lock while replacing the config.
///   So every write lands either before the switch in the old vault or after it in the new one.
/// - Reads are only revalidated, not excluded. Before each read the cached connection is checked
///   against the config and reopened if the vault changed, so a long-lived host follows a switch
///   from its next request on. A read that runs while a switch happens may still return the old
///   vault's data (stale by at most that one request); reads never modify a vault.
/// - Today Chrome starts one host process per message, so each process resolves the config once
///   anyway; the revalidation matters for hosts that serve several requests.
pub struct Session {
    pub env: Environment,
    /// The open vault: its path, mode and vault id as the config named them.
    connection: Option<(ActiveSpec, Connection)>,
    keys: Box<dyn KeySource>,
    /// Tests only: runs each time the active vault was resolved and opened (`open`), before its
    /// connection is used: the window in which another process may switch the active vault.
    #[cfg(test)]
    pub(crate) after_open: Option<Box<dyn FnMut()>>,
}

impl Session {
    pub fn new(env: Environment) -> Self {
        let keys: Box<dyn KeySource> = Box::new(KeyStore::of(&env));
        Self {
            env,
            connection: None,
            keys,
            #[cfg(test)]
            after_open: None,
        }
    }

    /// Tests only: a session whose encrypted vaults get their keys from `keys`.
    #[cfg(test)]
    pub(crate) fn with_keys(env: Environment, keys: Box<dyn KeySource>) -> Self {
        Self {
            keys,
            ..Self::new(env)
        }
    }

    pub fn reset(&mut self) {
        self.connection = None;
    }

    /// Path, mode and id of the active vault. The override (`MEMOCAMPUS_DB`) is always plain.
    pub fn active_target(&self) -> Result<ActiveSpec, VaultError> {
        match resolve(&self.env)? {
            Active::Override(path) => Ok(ActiveSpec {
                path: path.to_string_lossy().into_owned(),
                mode: VaultMode::Plain,
                vault_id: None,
            }),
            Active::Configured(config) => Ok(ActiveSpec {
                path: config.active_vault,
                mode: config.mode,
                vault_id: config.vault_id,
            }),
            Active::NotConfigured => Err(VaultError::NotConfigured),
        }
    }

    /// The key source this session opens encrypted vaults with.
    pub fn keys(&self) -> &dyn KeySource {
        self.keys.as_ref()
    }

    /// Opens the configured vault, first dropping a cached connection that no longer matches
    /// the config (see the type-level notes for what this does and does not guarantee).
    fn open(&mut self) -> Result<(&str, &mut Connection), VaultError> {
        if let Some((cached, _)) = &self.connection {
            if self.active_target().ok().as_ref() != Some(cached) {
                self.reset();
            }
        }
        self.connect()?;
        #[cfg(test)]
        if let Some(mut hook) = self.after_open.take() {
            hook();
            self.after_open = Some(hook);
        }
        Ok(self.connected())
    }

    fn open_cached(&mut self) -> Result<(&str, &mut Connection), VaultError> {
        self.connect()?;
        Ok(self.connected())
    }

    fn connect(&mut self) -> Result<(), VaultError> {
        if self.connection.is_none() {
            let spec = self.active_target()?;
            let db = open_vault_in(
                Path::new(&spec.path),
                spec.mode,
                spec.vault_id,
                self.keys.as_ref(),
            )?;
            self.connection = Some((spec, db));
        }
        Ok(())
    }

    fn connected(&mut self) -> (&str, &mut Connection) {
        let (spec, db) = self
            .connection
            .as_mut()
            .expect("connection was just opened");
        (spec.path.as_str(), db)
    }

    fn connected_spec(&self) -> Option<ActiveSpec> {
        self.connection.as_ref().map(|(spec, _)| spec.clone())
    }

    pub fn read<T>(&mut self, f: impl FnOnce(&Connection) -> T) -> Result<T, VaultError> {
        let (_, db) = self.open()?;
        Ok(f(db))
    }

    /// Runs `f` in a write transaction, after confirming (under the write lock) that the
    /// vault is still the configured one. Otherwise nothing is written and the caller must not
    /// acknowledge, so the extension retries against the new vault.
    pub fn write(
        &mut self,
        f: impl FnOnce(&Connection) -> rusqlite::Result<usize>,
    ) -> Result<(), VaultError> {
        self.write_with(true, f)
    }

    /// `revalidate: false` lets tests model a switch that lands between the revalidation in
    /// `open` and taking the write lock, the window the locked re-check exists for.
    fn write_with(
        &mut self,
        revalidate: bool,
        f: impl FnOnce(&Connection) -> rusqlite::Result<usize>,
    ) -> Result<(), VaultError> {
        self.write_checked(revalidate, None, |db| {
            f(db).map(|_| ()).map_err(|_| VaultError::Db)
        })
    }

    /// A read that names its vault: `f` runs on the connection of the vault that one resolution of
    /// the config opened (as `read` does), and the token is that same vault's, taken from the path
    /// of that connection. So a token and data always belong to one vault, also when another process
    /// switches the active vault meanwhile: a switch after the resolution only affects the next call.
    /// (`ticket()` followed by `read()` resolves twice, and a switch between them pairs one vault's
    /// token with the other's data.) The token is taken before `f` runs; if it cannot be, `f` does not
    /// run and the call fails.
    pub fn read_with_token<T>(
        &mut self,
        f: impl FnOnce(&Connection) -> T,
    ) -> Result<(String, T), VaultError> {
        let (path, db) = self.open()?;
        let token = vault_token(Path::new(path))?;
        Ok((token, f(db)))
    }

    /// A read bound to the vault of `ticket`, as `write_for` binds a write: one resolution of the
    /// active vault opens a fresh connection (a cached one is never reused here), and `f` runs on it
    /// only if the file that connection holds is the ticket's vault file (same `vault_token`, i.e.
    /// the same file identity, however its path is spelled). Otherwise `f` does not run and the call
    /// fails with `Switched`. So a vault that was switched away and back (A→B→A) is accepted, and
    /// another file put at the same path is not.
    ///
    /// The token is taken from the path after the connection opened it. On Windows SQLite opens the
    /// vault without `FILE_SHARE_DELETE`, so while the connection lives the file at that path can be
    /// neither replaced nor renamed: the token names the very file `f` reads (tested).
    pub fn read_for<T, E: From<VaultError>>(
        &mut self,
        ticket: &VaultTicket,
        f: impl FnOnce(&Connection) -> Result<T, E>,
    ) -> Result<T, E> {
        self.reset();
        let (path, db) = self.open()?;
        if vault_token(Path::new(path))? != ticket.token {
            self.reset();
            return Err(VaultError::Switched.into());
        }
        f(db)
    }

    /// Identifies the vault that is active now, for an operation that will write later (an image
    /// transfer). Pass it to `write_for` so the write can only land in this same vault.
    pub fn ticket(&mut self) -> Result<VaultTicket, VaultError> {
        let (path, _) = self.open()?;
        let path = path.to_owned();
        let token = vault_token(Path::new(&path))?;
        Ok(VaultTicket { path, token })
    }

    /// Like `write`, but fails with `Switched` unless the vault is still the one in `ticket`,
    /// checked under the write lock (both the open connection and `config.json`).
    pub fn write_for<T, E: From<VaultError>>(
        &mut self,
        ticket: &VaultTicket,
        f: impl FnOnce(&Connection) -> Result<T, E>,
    ) -> Result<T, E> {
        self.write_checked(true, Some(&ticket.path), f)
    }

    fn write_checked<T, E: From<VaultError>>(
        &mut self,
        revalidate: bool,
        expected: Option<&str>,
        f: impl FnOnce(&Connection) -> Result<T, E>,
    ) -> Result<T, E> {
        let overridden = self.env.db_override.is_some();
        let env = self.env.clone();
        let (path, _) = if revalidate {
            self.open()?
        } else {
            self.open_cached()?
        };
        let path = path.to_owned();
        if expected.is_some_and(|expected| expected != path) {
            self.reset();
            return Err(VaultError::Switched.into());
        }
        let spec = self.connected_spec();
        let (_, db) = self.connected();
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| VaultError::Busy)?;
        if !overridden {
            match resolve(&env)? {
                Active::Configured(config)
                    if spec.as_ref().is_some_and(|spec| {
                        config.active_vault == spec.path
                            && config.mode == spec.mode
                            && config.vault_id == spec.vault_id
                    }) && config.active_vault == path => {}
                _ => {
                    drop(tx);
                    self.reset();
                    return Err(VaultError::Switched.into());
                }
            }
        }
        // Any error from `f` drops `tx` uncommitted, rolling back everything `f` wrote.
        let value = f(&tx)?;
        tx.commit().map_err(|_| VaultError::Db)?;
        Ok(value)
    }
}

/// Moves `from` to `to`, failing with `AlreadyExists` instead of replacing `to`.
#[cfg(windows)]
pub fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_WRITE_THROUGH};
    let wide = |path: &Path| {
        path.as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<u16>>()
    };
    let (from, to) = (wide(from), wide(to));
    // No MOVEFILE_REPLACE_EXISTING: an existing destination makes the call fail.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_WRITE_THROUGH) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    fs::hard_link(from, to)?;
    fs::remove_file(from)
}

/// `memocampus-backup-20261030-031500Z.sqlite3` (UTC).
pub fn backup_file_name(now: SystemTime) -> String {
    let secs = now
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    // Civil-from-days (Howard Hinnant), valid for the proleptic Gregorian calendar.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "memocampus-backup-{year:04}{month:02}{day:02}-{:02}{:02}{:02}Z.sqlite3",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Writes a consistent copy of `source` into `dir` using the SQLite Backup API.
///
/// The copy is made into a hidden `.partial` file under one read transaction (so it is a
/// single snapshot even while other processes write), verified, flushed, and only then
/// renamed to a fresh name. Existing files are never replaced; on failure the partial file is
/// removed and no backup is reported.
pub fn backup_vault(
    source: &Connection,
    dir: &Path,
    now: SystemTime,
) -> Result<PathBuf, VaultError> {
    if !dir.is_dir() {
        return Err(VaultError::ParentMissing);
    }
    let temp = dir.join(format!(
        ".memocampus-backup-{}.partial",
        uuid::Uuid::new_v4()
    ));
    match OpenOptions::new().write(true).create_new(true).open(&temp) {
        Ok(file) => drop(file),
        Err(_) => return Err(VaultError::BackupFailed),
    }
    let result = copy_and_verify(source, &temp).and_then(|()| finalize_backup(&temp, dir, now));
    if result.is_err() {
        let _ = fs::remove_file(&temp);
        let _ = fs::remove_file(sidecar(&temp, "-journal"));
    }
    result
}

fn failed<E>(_: E) -> VaultError {
    VaultError::BackupFailed
}

/// Row counts of every table a backup checks: `Counts`, image tables, other tables.
pub type ContentCounts = (Counts, (i64, i64, i64), (i64, i64, i64));

pub fn content_counts(db: &Connection) -> rusqlite::Result<ContentCounts> {
    Ok((
        counts(db)?,
        images::table_counts(db)?,
        backup_only_counts(db)?,
    ))
}

/// Copies `source` into `target` with the SQLite Backup API under one read transaction on the
/// source, so the copy is one snapshot even while other processes write (committed WAL content
/// included). Returns the source's counts read in that same transaction. Both connections may be
/// keyed (SQLCipher re-encrypts every page under the target's key); the Backup API refuses to copy
/// between a plain and an encrypted database.
pub fn snapshot_into(
    source: &Connection,
    target: &mut Connection,
) -> Result<ContentCounts, VaultError> {
    source.execute_batch("BEGIN").map_err(failed)?;
    let copied = (|| {
        // Counting inside the transaction takes the shared lock that pins the snapshot.
        let expected = content_counts(source).map_err(failed)?;
        let backup = Backup::new(source, target).map_err(failed)?;
        let deadline = Instant::now() + BUSY_TIMEOUT;
        loop {
            match backup.step(-1).map_err(failed)? {
                StepResult::Done => break,
                StepResult::More => {}
                _ if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
                _ => return Err(VaultError::Busy),
            }
        }
        Ok(expected)
    })();
    let _ = source.execute_batch("COMMIT");
    copied
}

/// A copy just made: full `integrity_check`, the exact schema and the counts the source had.
pub fn verify_copy(target: &Connection, expected: &ContentCounts) -> Result<(), VaultError> {
    let integrity: String = target
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(failed)?;
    schema::check(target).map_err(failed)?;
    if integrity != "ok" || content_counts(target).map_err(failed)? != *expected {
        return Err(VaultError::BackupFailed);
    }
    crate::pages::verify(target).map_err(failed)?;
    Ok(())
}

fn copy_and_verify(source: &Connection, temp: &Path) -> Result<(), VaultError> {
    let mut target = Connection::open_with_flags(
        temp,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(failed)?;
    let expected = snapshot_into(source, &mut target)?;
    verify_copy(&target, &expected)?;
    drop(target);
    OpenOptions::new()
        .write(true)
        .open(temp)
        .and_then(|file| file.sync_all())
        .map_err(failed)
}

fn finalize_backup(temp: &Path, dir: &Path, now: SystemTime) -> Result<PathBuf, VaultError> {
    let name = backup_file_name(now);
    let stem = name.trim_end_matches(".sqlite3");
    for attempt in 0..100 {
        let candidate = if attempt == 0 {
            dir.join(&name)
        } else {
            dir.join(format!("{stem}-{attempt}.sqlite3"))
        };
        match rename_no_replace(temp, &candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(VaultError::BackupFailed),
        }
    }
    Err(VaultError::FileExists)
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use rusqlite::params;

    /// A unique scratch directory under the system temp dir, removed on drop.
    pub struct TempDir(pub PathBuf);

    impl TempDir {
        pub fn new() -> Self {
            let dir =
                std::env::temp_dir().join(format!("memocampus-unit-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        pub fn env(&self) -> Environment {
            Environment {
                app_dir: Some(self.0.join("app")),
                db_override: None,
                exe_dir: Some(self.0.join("exe")),
            }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn insert_visit(db: &Connection, id: &str) -> rusqlite::Result<usize> {
        db.execute(
            "INSERT OR IGNORE INTO visits VALUES (?1, 'https://a.test/', 'A', '2026-10-30T00:00:00.000Z')",
            params![id],
        )
    }

    fn configure(env: &Environment, path: &Path) {
        let lock = ConfigLock::acquire(env).unwrap();
        switch_active(env, &lock, path, false).unwrap();
    }

    #[test]
    fn resolves_not_configured_then_persisted_path_across_restarts() {
        let tmp = TempDir::new();
        let env = tmp.env();
        assert_eq!(resolve(&env), Ok(Active::NotConfigured));
        let vault = tmp.0.join("v.sqlite3");
        configure(&env, &vault);
        // A fresh Environment models a restarted host process.
        let restarted = tmp.env();
        match resolve(&restarted).unwrap() {
            Active::Configured(config) => {
                assert_eq!(config.active_vault, vault.to_string_lossy());
                assert!(!config.location_approved);
            }
            other => panic!("unexpected {other:?}"),
        }
        let leftovers: Vec<_> = fs::read_dir(env.app_dir.unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn default_paths_live_under_the_app_dir_not_the_executable() {
        let tmp = TempDir::new();
        let env = tmp.env();
        assert_eq!(
            env.default_vault_path().unwrap(),
            tmp.0.join("app").join("memocampus.sqlite3")
        );
        assert_eq!(
            env.config_path().unwrap(),
            tmp.0.join("app").join("config.json")
        );
        let no_app_dir = Environment {
            app_dir: None,
            ..env
        };
        assert_eq!(
            no_app_dir.default_vault_path(),
            Err(VaultError::AppDirUnavailable)
        );
        assert!(!no_app_dir.management_enabled());
    }

    #[test]
    fn override_wins_and_disables_management() {
        let tmp = TempDir::new();
        let env = Environment {
            db_override: Some(tmp.0.join("dev.sqlite3")),
            ..tmp.env()
        };
        assert_eq!(
            resolve(&env),
            Ok(Active::Override(tmp.0.join("dev.sqlite3")))
        );
        assert!(!env.management_enabled());
    }

    #[test]
    fn corrupt_or_relative_config_is_reported_not_guessed() {
        let tmp = TempDir::new();
        let env = tmp.env();
        fs::create_dir_all(env.app_dir.as_ref().unwrap()).unwrap();
        fs::write(env.config_path().unwrap(), b"{not json").unwrap();
        assert_eq!(resolve(&env), Err(VaultError::ConfigInvalid));
        fs::write(
            env.config_path().unwrap(),
            br#"{"version":1,"activeVault":"relative.sqlite3"}"#,
        )
        .unwrap();
        assert_eq!(resolve(&env), Err(VaultError::ConfigInvalid));
    }

    #[test]
    fn create_makes_a_tagged_vault_and_never_overwrites() {
        let tmp = TempDir::new();
        let path = tmp.0.join("new.sqlite3");
        create_vault(&path).unwrap();
        let db = open_vault_checked(&path).unwrap();
        let app_id: i64 = db
            .query_row("PRAGMA application_id", [], |row| row.get(0))
            .unwrap();
        assert_eq!(app_id, schema::APP_ID);
        insert_visit(&db, "1").unwrap();
        drop(db);
        let before = fs::read(&path).unwrap();
        assert_eq!(create_vault(&path), Err(VaultError::FileExists));
        assert_eq!(fs::read(&path).unwrap(), before);

        let other = tmp.0.join("plain.txt");
        fs::write(&other, b"keep me").unwrap();
        assert_eq!(create_vault(&other), Err(VaultError::FileExists));
        assert_eq!(fs::read(&other).unwrap(), b"keep me");
    }

    #[test]
    fn create_refuses_stale_sidecars_and_missing_parents() {
        let tmp = TempDir::new();
        let path = tmp.0.join("v.sqlite3");
        fs::write(sidecar(&path, "-journal"), b"stale").unwrap();
        assert_eq!(create_vault(&path), Err(VaultError::SidecarExists));
        assert!(!path.exists());
        assert_eq!(
            create_vault(&tmp.0.join("nope").join("v.sqlite3")),
            Err(VaultError::ParentMissing)
        );
    }

    #[test]
    fn opening_never_creates_or_alters_files() {
        let tmp = TempDir::new();
        let missing = tmp.0.join("gone.sqlite3");
        assert_eq!(open_vault(&missing).err(), Some(VaultError::Missing));
        assert!(!missing.exists());

        let empty = tmp.0.join("empty.sqlite3");
        fs::write(&empty, b"").unwrap();
        assert_eq!(open_vault(&empty).err(), Some(VaultError::SchemaMismatch));
        assert_eq!(fs::metadata(&empty).unwrap().len(), 0);

        let text = tmp.0.join("text.sqlite3");
        fs::write(&text, b"hello, not sqlite at all").unwrap();
        assert_eq!(open_vault_checked(&text).err(), Some(VaultError::Invalid));
        assert_eq!(fs::read(&text).unwrap(), b"hello, not sqlite at all");

        let foreign = tmp.0.join("foreign.sqlite3");
        Connection::open(&foreign)
            .unwrap()
            .execute_batch("CREATE TABLE other (x);")
            .unwrap();
        assert_eq!(open_vault(&foreign).err(), Some(VaultError::SchemaMismatch));

        let tagged = tmp.0.join("tagged.sqlite3");
        let db = Connection::open(&tagged).unwrap();
        db.execute_batch(&schema::create_sql()).unwrap();
        db.execute_batch("PRAGMA application_id = 42;").unwrap();
        drop(db);
        assert_eq!(open_vault(&tagged).err(), Some(VaultError::Invalid));

        assert_eq!(open_vault(&tmp.0).err(), Some(VaultError::Invalid));
    }

    #[test]
    fn session_writes_only_while_its_vault_is_still_configured() {
        let tmp = TempDir::new();
        let env = tmp.env();
        let (a, b) = (tmp.0.join("a.sqlite3"), tmp.0.join("b.sqlite3"));
        create_vault(&a).unwrap();
        create_vault(&b).unwrap();
        configure(&env, &a);

        let mut session = Session::new(env.clone());
        session.write(|db| insert_visit(db, "1")).unwrap();
        // Another host process switches to `b` while this session still caches `a`; the
        // revalidation in `open` follows the switch.
        configure(&env, &b);
        session.write(|db| insert_visit(db, "2")).unwrap();
        // A switch that lands after revalidation but before the write lock is caught by the
        // locked re-check: nothing is written and the caller must not acknowledge.
        configure(&env, &a);
        assert_eq!(
            session.write_with(false, |db| insert_visit(db, "3")),
            Err(VaultError::Switched)
        );
        session.write(|db| insert_visit(db, "4")).unwrap();

        assert_eq!(visit_ids(&a), ["1", "4"]);
        assert_eq!(visit_ids(&b), ["2"]);
    }

    fn visit_ids(path: &Path) -> Vec<String> {
        let db = open_vault(path).unwrap();
        let mut statement = db
            .prepare("SELECT event_id FROM visits ORDER BY 1")
            .unwrap();
        let rows = statement.query_map([], |row| row.get(0)).unwrap();
        rows.collect::<rusqlite::Result<_>>().unwrap()
    }

    fn configured_path(env: &Environment) -> String {
        match resolve(env).unwrap() {
            Active::Configured(config) => config.active_vault,
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn cached_reads_follow_a_vault_switch() {
        let tmp = TempDir::new();
        let env = tmp.env();
        let (a, b) = (tmp.0.join("a.sqlite3"), tmp.0.join("b.sqlite3"));
        create_vault(&a).unwrap();
        create_vault(&b).unwrap();
        insert_visit(&open_vault(&b).unwrap(), "only-in-b").unwrap();
        configure(&env, &a);

        let mut session = Session::new(env.clone());
        let read_count =
            |session: &mut Session| session.read(|db| counts(db).unwrap().visits).unwrap();
        assert_eq!(read_count(&mut session), 0);
        configure(&env, &b);
        assert_eq!(
            read_count(&mut session),
            1,
            "stale connection to `a` must be dropped"
        );
        // A vanished vault is reported instead of serving the cached connection.
        configure(&env, &a);
        std::fs::remove_file(&a).unwrap();
        assert_eq!(session.read(|_| ()), Err(VaultError::Missing));
        assert!(!a.exists());
    }

    #[test]
    fn switch_from_a_missing_or_invalid_old_vault_needs_no_fence() {
        let tmp = TempDir::new();
        let env = tmp.env();
        let (old, new) = (tmp.0.join("old.sqlite3"), tmp.0.join("new.sqlite3"));
        create_vault(&new).unwrap();
        configure(&env, &old); // never existed
        configure(&env, &new);
        assert_eq!(configured_path(&env), new.to_string_lossy());

        let junk = tmp.0.join("junk.sqlite3");
        std::fs::write(&junk, b"not a database").unwrap();
        configure(&env, &junk);
        configure(&env, &new);
        assert_eq!(configured_path(&env), new.to_string_lossy());
        assert_eq!(std::fs::read(&junk).unwrap(), b"not a database");
    }

    fn sqlite_error(extended: i32) -> rusqlite::Error {
        rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(extended), None)
    }

    #[test]
    fn sqlite_errors_are_classified_with_their_codes_kept() {
        // (extended result code, expected kind)
        let cases = [
            (5, VaultError::Busy),     // SQLITE_BUSY
            (517, VaultError::Busy),   // SQLITE_BUSY_SNAPSHOT
            (6, VaultError::Busy),     // SQLITE_LOCKED
            (262, VaultError::Busy),   // SQLITE_LOCKED_SHAREDCACHE
            (10, VaultError::Io),      // SQLITE_IOERR
            (522, VaultError::Io),     // SQLITE_IOERR_SHORT_READ
            (3338, VaultError::Io),    // SQLITE_IOERR_ACCESS
            (14, VaultError::Io),      // SQLITE_CANTOPEN
            (3, VaultError::Io),       // SQLITE_PERM
            (8, VaultError::Io),       // SQLITE_READONLY
            (13, VaultError::Db),      // SQLITE_FULL
            (7, VaultError::Db),       // SQLITE_NOMEM
            (1, VaultError::Db),       // SQLITE_ERROR
            (23, VaultError::Db),      // SQLITE_AUTH
            (26, VaultError::Invalid), // SQLITE_NOTADB
            (11, VaultError::Invalid), // SQLITE_CORRUPT
        ];
        for (extended, kind) in cases {
            let failure = OpenFailure::from_sqlite(&sqlite_error(extended));
            assert_eq!(failure.kind, kind, "code {extended}");
            match failure.cause {
                OpenCause::Sqlite {
                    extended: kept,
                    code,
                } => {
                    assert_eq!(kept, extended);
                    assert_eq!(code, rusqlite::ffi::Error::new(extended).code);
                }
                other => panic!("code {extended}: cause lost: {other:?}"),
            }
            // Only definitive format errors may ever skip the switch fence.
            assert_eq!(
                failure.proves_unwritable(),
                kind == VaultError::Invalid,
                "code {extended}"
            );
        }
        let driver = OpenFailure::from_sqlite(&rusqlite::Error::QueryReturnedNoRows);
        assert_eq!(driver, OpenFailure::new(VaultError::Db, OpenCause::Driver));
        assert!(!driver.proves_unwritable());
    }

    #[test]
    fn io_errors_keep_the_os_code_and_only_not_found_is_missing() {
        let not_found = OpenFailure::from_io(&io::Error::from(io::ErrorKind::NotFound));
        assert_eq!(
            not_found,
            OpenFailure::new(VaultError::Missing, OpenCause::NotFound)
        );
        assert!(not_found.proves_unwritable());
        for code in [5, 32, 33, 1314] {
            // ACCESS_DENIED, SHARING_VIOLATION, LOCK_VIOLATION, PRIVILEGE_NOT_HELD
            let failure = OpenFailure::from_io(&io::Error::from_raw_os_error(code));
            assert_eq!(
                failure,
                OpenFailure::new(VaultError::Io, OpenCause::Os(Some(code)))
            );
            assert!(!failure.proves_unwritable());
        }
    }

    #[test]
    fn only_consistent_kind_and_cause_pairs_skip_the_fence() {
        let busy_cause = OpenCause::Sqlite {
            code: rusqlite::ErrorCode::DatabaseBusy,
            extended: 5,
        };
        // A kind that claims "invalid" with a transient cause, or "missing" without a confirmed
        // absence, never qualifies.
        for failure in [
            OpenFailure::new(VaultError::Invalid, busy_cause.clone()),
            OpenFailure::new(VaultError::Invalid, OpenCause::Os(Some(32))),
            OpenFailure::new(VaultError::Invalid, OpenCause::ReadOnly),
            OpenFailure::new(VaultError::Invalid, OpenCause::Driver),
            OpenFailure::new(VaultError::Missing, OpenCause::Os(Some(5))),
            OpenFailure::new(VaultError::Busy, busy_cause),
            OpenFailure::new(VaultError::Db, OpenCause::SchemaMismatch("visits")),
        ] {
            assert!(!failure.proves_unwritable(), "{failure:?}");
        }
    }

    #[test]
    fn real_format_problems_report_specific_causes() {
        let tmp = TempDir::new();
        let text = tmp.0.join("text.sqlite3");
        fs::write(&text, b"hello, not sqlite at all").unwrap();
        let failure = open_vault_detailed(&text).unwrap_err();
        assert_eq!(failure.kind, VaultError::Invalid);
        assert!(
            matches!(
                failure.cause,
                OpenCause::Sqlite {
                    code: rusqlite::ErrorCode::NotADatabase,
                    ..
                }
            ),
            "{failure:?}"
        );

        let foreign = tmp.0.join("foreign.sqlite3");
        let db = Connection::open(&foreign).unwrap();
        db.execute_batch(&schema::create_sql()).unwrap();
        db.execute_batch("PRAGMA application_id = 42;").unwrap();
        drop(db);
        assert_eq!(
            open_vault_detailed(&foreign).unwrap_err().cause,
            OpenCause::ForeignApplicationId(42)
        );

        let partial = tmp.0.join("partial.sqlite3");
        Connection::open(&partial)
            .unwrap()
            .execute_batch("CREATE TABLE visits (event_id, url, title, visited_at);")
            .unwrap();
        assert_eq!(
            open_vault_detailed(&partial).unwrap_err().cause,
            OpenCause::UnsupportedVersion(0)
        );
        assert_eq!(
            open_vault_detailed(&tmp.0).unwrap_err().cause,
            OpenCause::NotAFile
        );
        assert_eq!(
            open_vault_detailed(&tmp.0.join("absent.sqlite3"))
                .unwrap_err()
                .cause,
            OpenCause::NotFound
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_vault_held_open_without_sharing_is_an_io_error_and_the_switch_is_refused() {
        use std::os::windows::fs::OpenOptionsExt;
        let tmp = TempDir::new();
        let env = tmp.env();
        let (a, b) = (tmp.0.join("a.sqlite3"), tmp.0.join("b.sqlite3"));
        create_vault(&a).unwrap();
        create_vault(&b).unwrap();
        configure(&env, &a);
        // Another process (antivirus, backup tool) holds the file with no sharing at all.
        let holder = OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&a)
            .unwrap();
        let failure = open_vault_detailed(&a).unwrap_err();
        assert!(
            matches!(failure.kind, VaultError::Io | VaultError::Busy),
            "{failure:?}"
        );
        assert!(
            !failure.proves_unwritable(),
            "must not be mistaken for missing/invalid: {failure:?}"
        );
        let lock = ConfigLock::acquire(&env).unwrap();
        assert!(switch_active(&env, &lock, &b, false).is_err());
        assert_eq!(
            configured_path(&env),
            a.to_string_lossy(),
            "fence was not skipped"
        );
        drop(holder);
        switch_active(&env, &lock, &b, false).unwrap();
        assert_eq!(configured_path(&env), b.to_string_lossy());
    }

    #[test]
    fn a_read_only_vault_is_an_io_error_and_the_switch_is_refused() {
        let tmp = TempDir::new();
        let env = tmp.env();
        let (a, b) = (tmp.0.join("a.sqlite3"), tmp.0.join("b.sqlite3"));
        create_vault(&a).unwrap();
        create_vault(&b).unwrap();
        configure(&env, &a);
        let set_read_only = |read_only: bool| {
            let mut permissions = fs::metadata(&a).unwrap().permissions();
            permissions.set_readonly(read_only);
            fs::set_permissions(&a, permissions).unwrap();
        };
        set_read_only(true);
        let failure = open_vault_detailed(&a).unwrap_err();
        assert_eq!(
            failure,
            OpenFailure::new(VaultError::Io, OpenCause::ReadOnly)
        );
        let lock = ConfigLock::acquire(&env).unwrap();
        let refused = switch_active(&env, &lock, &b, false);
        set_read_only(false); // restore before asserting so the temp dir can be removed
        assert_eq!(refused, Err(VaultError::Io));
        assert_eq!(configured_path(&env), a.to_string_lossy());
        switch_active(&env, &lock, &b, false).unwrap();
        assert_eq!(configured_path(&env), b.to_string_lossy());
    }

    #[test]
    fn switch_aborts_while_the_old_vault_is_locked_and_succeeds_on_retry() {
        let tmp = TempDir::new();
        let env = tmp.env();
        let (a, b) = (tmp.0.join("a.sqlite3"), tmp.0.join("b.sqlite3"));
        create_vault(&a).unwrap();
        create_vault(&b).unwrap();
        configure(&env, &a);
        // A pending EXCLUSIVE write blocks even the schema read in `open_vault`. The old vault
        // is busy, which must not be mistaken for "invalid" (that would skip the fence).
        let holder = open_vault(&a).unwrap();
        holder
            .execute_batch("BEGIN EXCLUSIVE; INSERT INTO visits VALUES ('x', 'https://a.test/', 'A', '2026-10-30T00:00:00.000Z');")
            .unwrap();
        assert_eq!(open_vault(&a).err(), Some(VaultError::Busy));
        let lock = ConfigLock::acquire(&env).unwrap();
        assert_eq!(switch_active(&env, &lock, &b, false), Err(VaultError::Busy));
        assert_eq!(configured_path(&env), a.to_string_lossy());
        holder.execute_batch("COMMIT").unwrap();
        switch_active(&env, &lock, &b, false).unwrap();
        assert_eq!(configured_path(&env), b.to_string_lossy());
    }

    #[test]
    fn a_failed_config_write_releases_the_fence_and_keeps_the_old_config() {
        let tmp = TempDir::new();
        let env = tmp.env();
        let (a, b) = (tmp.0.join("a.sqlite3"), tmp.0.join("b.sqlite3"));
        create_vault(&a).unwrap();
        create_vault(&b).unwrap();
        configure(&env, &a);
        let lock = ConfigLock::acquire(&env).unwrap();
        // Make only the final rename fail: config.json is held open without delete sharing,
        // so it cannot be replaced (as when antivirus or a backup tool holds the file).
        let config = env.config_path().unwrap();
        let blocker = hold_without_delete_sharing(&config);
        assert!(switch_active(&env, &lock, &b, false).is_err());
        drop(blocker);
        // The old vault is not left write-locked by the aborted switch.
        insert_visit(&open_vault(&a).unwrap(), "after-abort").unwrap();
        assert_eq!(configured_path(&env), a.to_string_lossy());
        let leftovers = std::fs::read_dir(env.app_dir.as_ref().unwrap())
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")
            })
            .count();
        assert_eq!(leftovers, 0, "the temp config is cleaned up");
        // Retrying after the transient failure succeeds.
        switch_active(&env, &lock, &b, false).unwrap();
        assert_eq!(configured_path(&env), b.to_string_lossy());
    }

    #[cfg(windows)]
    fn hold_without_delete_sharing(path: &Path) -> File {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 1;
        OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(path)
            .unwrap()
    }

    #[test]
    fn a_panicking_switch_releases_the_fence() {
        // Models an abnormal end while the fence is held: dropping the connection (as on unwind
        // or process exit) must release the write lock.
        let tmp = TempDir::new();
        let a = tmp.0.join("a.sqlite3");
        create_vault(&a).unwrap();
        let path = a.clone();
        let result = std::panic::catch_unwind(move || {
            let fence = open_vault(&path).unwrap();
            fence.execute_batch("BEGIN IMMEDIATE").unwrap();
            panic!("abnormal end during switch");
        });
        assert!(result.is_err());
        insert_visit(&open_vault(&a).unwrap(), "after-panic").unwrap();
        assert_eq!(visit_ids(&a), ["after-panic"]);
    }

    #[test]
    fn session_refuses_to_write_without_a_vault() {
        let tmp = TempDir::new();
        let env = tmp.env();
        let mut session = Session::new(env.clone());
        assert_eq!(
            session.write(|db| insert_visit(db, "1")),
            Err(VaultError::NotConfigured)
        );
        let vault = tmp.0.join("v.sqlite3");
        configure(&env, &vault); // configured but the file does not exist
        assert_eq!(
            session.write(|db| insert_visit(db, "1")),
            Err(VaultError::Missing)
        );
        assert!(!vault.exists());
    }

    #[test]
    fn switch_waits_for_writers_and_fails_rather_than_racing() {
        let tmp = TempDir::new();
        let env = tmp.env();
        let (a, b) = (tmp.0.join("a.sqlite3"), tmp.0.join("b.sqlite3"));
        create_vault(&a).unwrap();
        create_vault(&b).unwrap();
        configure(&env, &a);
        let writer = open_vault(&a).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        let lock = ConfigLock::acquire(&env).unwrap();
        assert_eq!(switch_active(&env, &lock, &b, false), Err(VaultError::Busy));
        writer.execute_batch("ROLLBACK").unwrap();
        match resolve(&env).unwrap() {
            Active::Configured(config) => assert_eq!(config.active_vault, a.to_string_lossy()),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn config_lock_is_exclusive_until_released() {
        let tmp = TempDir::new();
        let env = tmp.env();
        let held = ConfigLock::acquire(&env).unwrap();
        assert_eq!(
            ConfigLock::acquire_within(&env, Duration::from_millis(150)).err(),
            Some(VaultError::Busy)
        );
        drop(held);
        assert!(ConfigLock::acquire_within(&env, Duration::from_millis(150)).is_ok());
    }

    #[test]
    fn backup_file_names_are_utc_timestamps() {
        let at = UNIX_EPOCH + Duration::from_secs(1_793_330_100); // 2026-10-30T03:15:00Z
        assert_eq!(
            backup_file_name(at),
            "memocampus-backup-20261030-031500Z.sqlite3"
        );
        assert_eq!(
            backup_file_name(UNIX_EPOCH + Duration::from_secs(951_782_400)),
            "memocampus-backup-20000229-000000Z.sqlite3"
        );
    }

    #[test]
    fn backup_copies_verifies_and_never_overwrites() {
        let tmp = TempDir::new();
        let source = tmp.0.join("v.sqlite3");
        create_vault(&source).unwrap();
        let db = open_vault(&source).unwrap();
        for i in 0..20 {
            insert_visit(&db, &i.to_string()).unwrap();
        }
        let dest = tmp.0.join("backups");
        fs::create_dir(&dest).unwrap();
        let now = UNIX_EPOCH + Duration::from_secs(1_793_330_100);
        let first = backup_vault(&db, &dest, now).unwrap();
        let second = backup_vault(&db, &dest, now).unwrap();
        assert_ne!(first, second);
        assert!(second.to_string_lossy().ends_with("-031500Z-1.sqlite3"));
        for path in [&first, &second] {
            let copy = open_vault_checked(path).unwrap();
            assert_eq!(
                counts(&copy).unwrap(),
                Counts {
                    visits: 20,
                    snapshots: 0,
                    pages: 0,
                    page_blobs: 0,
                }
            );
        }
        let names: Vec<String> = fs::read_dir(&dest)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names.len(), 2, "no partial files remain: {names:?}");
        assert_eq!(counts(&db).unwrap().visits, 20, "source untouched");
    }

    #[test]
    fn failed_backup_leaves_nothing_behind() {
        let tmp = TempDir::new();
        let source = tmp.0.join("v.sqlite3");
        create_vault(&source).unwrap();
        let db = open_vault(&source).unwrap();
        assert_eq!(
            backup_vault(&db, &tmp.0.join("missing"), SystemTime::now()),
            Err(VaultError::ParentMissing)
        );
        // A vault whose schema is gone cannot produce a verified backup.
        db.execute_batch("DROP TABLE tab_snapshots;").unwrap();
        let dest = tmp.0.join("backups");
        fs::create_dir(&dest).unwrap();
        assert_eq!(
            backup_vault(&db, &dest, SystemTime::now()),
            Err(VaultError::BackupFailed)
        );
        assert_eq!(fs::read_dir(&dest).unwrap().count(), 0);
    }

    #[test]
    fn backup_is_a_consistent_snapshot_while_another_process_writes() {
        let tmp = TempDir::new();
        let source = tmp.0.join("v.sqlite3");
        create_vault(&source).unwrap();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let writer = {
            let (source, stop) = (source.clone(), stop.clone());
            std::thread::spawn(move || {
                let db = open_vault(&source).unwrap();
                let mut written = 0;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    // Each pair is one transaction: a consistent copy never splits it.
                    let id = written.to_string();
                    let ok = db
                        .execute_batch(&format!(
                            "BEGIN IMMEDIATE; INSERT INTO visits VALUES ('{id}', 'https://a.test/', 'A', '2026-10-30T00:00:00.000Z'); INSERT INTO tab_snapshots VALUES ('{id}', 1, 0, 'https://a.test/', 'A', 0, 0, '2026-10-30T00:00:00.000Z'); COMMIT;"
                        ))
                        .is_ok();
                    if ok {
                        written += 1;
                    } else {
                        let _ = db.execute_batch("ROLLBACK");
                    }
                }
                written
            })
        };
        let dest = tmp.0.join("backups");
        fs::create_dir(&dest).unwrap();
        let db = open_vault(&source).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let mut backups = Vec::new();
        for _ in 0..5 {
            backups.push(backup_vault(&db, &dest, SystemTime::now()).unwrap());
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let written = writer.join().unwrap();
        assert!(written > 0);
        let mut largest = 0;
        for path in backups {
            let copy = open_vault_checked(&path).unwrap();
            let Counts {
                visits, snapshots, ..
            } = counts(&copy).unwrap();
            assert_eq!(visits, snapshots, "backup split a transaction");
            largest = largest.max(visits);
        }
        assert!(largest > 0, "backups ran while data was being written");
    }
}

#[cfg(test)]
mod secure_vault_tests;
