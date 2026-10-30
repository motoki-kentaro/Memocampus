//! Active vault resolution and lifecycle.
//!
//! The vault (SQLite file) is independent of the host executable. Its path is persisted in
//! `%LOCALAPPDATA%\Memocampus\config.json`. Vaults are only ever created by an explicit
//! request, opened without SQLite's create flag, and existing files are never overwritten.

use rusqlite::backup::{Backup, StepResult};
use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::images;
use crate::SCHEMA_SQL;

/// `PRAGMA application_id` of vaults created by Memocampus ("MCMP").
pub const APP_ID: i64 = 0x4D43_4D50;
/// Schema version written into newly created vaults (2 = with image tables, see `images`).
pub const SCHEMA_VERSION: i64 = images::IMAGE_SCHEMA_VERSION;
pub const CONFIG_VERSION: u32 = 1;
const DEFAULT_VAULT_NAME: &str = "memocampus.sqlite3";
const BUSY_TIMEOUT: Duration = Duration::from_secs(3);
const LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const SIDECAR_SUFFIXES: [&str; 3] = ["-journal", "-wal", "-shm"];

const EXPECTED_TABLES: [(&str, &[&str]); 2] = [
    ("visits", &["event_id", "url", "title", "visited_at"]),
    (
        "tab_snapshots",
        &[
            "event_id",
            "window_id",
            "tab_index",
            "url",
            "title",
            "pinned",
            "incognito",
            "recorded_at",
        ],
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaultError {
    AppDirUnavailable,
    NotConfigured,
    ConfigInvalid,
    Missing,
    Invalid,
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
}

impl VaultError {
    pub fn code(self) -> &'static str {
        match self {
            Self::AppDirUnavailable => "app_dir_unavailable",
            Self::NotConfigured => "vault_not_configured",
            Self::ConfigInvalid => "config_invalid",
            Self::Missing => "vault_missing",
            Self::Invalid => "vault_invalid",
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub version: u32,
    pub active_vault: String,
    /// The user explicitly accepted location warnings (sync folder, removable drive, ...).
    #[serde(default)]
    pub location_approved: bool,
}

#[derive(Debug, PartialEq)]
pub enum Active {
    Override(PathBuf),
    Configured(Config),
    NotConfigured,
}

/// Resolves the active vault path without touching the vault itself.
pub fn resolve(env: &Environment) -> Result<Active, VaultError> {
    if let Some(path) = &env.db_override {
        return Ok(Active::Override(path.clone()));
    }
    let bytes = match fs::read(env.config_path()?) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Active::NotConfigured),
        Err(_) => return Err(VaultError::Io),
    };
    let config: Config = serde_json::from_slice(&bytes).map_err(|_| VaultError::ConfigInvalid)?;
    if config.version != CONFIG_VERSION || !Path::new(&config.active_vault).is_absolute() {
        return Err(VaultError::ConfigInvalid);
    }
    Ok(Active::Configured(config))
}

/// Atomically replaces `config.json` (write to a unique temp file, flush, rename).
pub fn write_config(env: &Environment, config: &Config) -> Result<(), VaultError> {
    let target = env.config_path()?;
    let dir = env.app_dir()?;
    fs::create_dir_all(dir).map_err(|_| VaultError::Io)?;
    let temp = dir.join(format!("config.json.{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(&serde_json::to_vec_pretty(config).map_err(io::Error::other)?)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, &target)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(|_| VaultError::Io)
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
    /// The named table is missing or lacks expected columns.
    SchemaMismatch(&'static str),
    IntegrityCheckFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenFailure {
    pub kind: VaultError,
    pub cause: OpenCause,
}

impl OpenFailure {
    fn new(kind: VaultError, cause: OpenCause) -> Self {
        Self { kind, cause }
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
        use rusqlite::ErrorCode;
        let rusqlite::Error::SqliteFailure(inner, _) = error else {
            return Self::new(VaultError::Db, OpenCause::Driver);
        };
        let kind = match inner.code {
            ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked => VaultError::Busy,
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
    }

    /// True only when no writer can possibly be using the vault: it does not exist, or its own
    /// contents prove it is not a Memocampus vault (writers run the same checks and are refused).
    /// Both the classification and the cause must agree, so a misclassified transient error can
    /// never qualify.
    pub fn proves_unwritable(&self) -> bool {
        use rusqlite::ErrorCode;
        match (&self.kind, &self.cause) {
            (VaultError::Missing, OpenCause::NotFound) => true,
            (VaultError::Invalid, cause) => matches!(
                cause,
                OpenCause::NotAFile
                    | OpenCause::ForeignApplicationId(_)
                    | OpenCause::SchemaMismatch(_)
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

fn validate_schema(db: &Connection) -> Result<(), OpenFailure> {
    let app_id: i64 = db
        .query_row("PRAGMA application_id", [], |row| row.get(0))
        .map_err(|error| OpenFailure::from_sqlite(&error))?;
    // 0 = vault created before application_id was introduced; accepted if the tables match.
    if app_id != 0 && app_id != APP_ID {
        return Err(OpenFailure::new(
            VaultError::Invalid,
            OpenCause::ForeignApplicationId(app_id),
        ));
    }
    for (table, columns) in EXPECTED_TABLES {
        let mut statement = db
            .prepare("SELECT name FROM pragma_table_info(?1)")
            .map_err(|error| OpenFailure::from_sqlite(&error))?;
        let actual = statement
            .query_map([table], |row| row.get::<_, String>(0))
            .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
            .map_err(|error| OpenFailure::from_sqlite(&error))?;
        if !columns
            .iter()
            .all(|column| actual.iter().any(|name| name == column))
        {
            return Err(OpenFailure::new(
                VaultError::Invalid,
                OpenCause::SchemaMismatch(table),
            ));
        }
    }
    Ok(())
}

/// Opens an existing vault. Never creates a file: SQLite's create flag is not passed.
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

/// `open_vault` plus a full structural check, for when the user picks a file to open.
pub fn open_vault_checked(path: &Path) -> Result<Connection, VaultError> {
    let db = open_vault(path)?;
    let result: String = db
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(|error| VaultError::from(OpenFailure::from_sqlite(&error)))?;
    if result != "ok" {
        return Err(OpenFailure::new(VaultError::Invalid, OpenCause::IntegrityCheckFailed).into());
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
        db.execute_batch(&format!(
            "BEGIN; PRAGMA application_id = {APP_ID}; PRAGMA user_version = {SCHEMA_VERSION}; {SCHEMA_SQL} {} COMMIT;",
            images::IMAGE_SCHEMA_SQL
        ))
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
    _lock: &ConfigLock,
    new_path: &Path,
    location_approved: bool,
) -> Result<(), VaultError> {
    let fence = match resolve(env) {
        Ok(Active::Configured(config)) => {
            match open_vault_detailed(Path::new(&config.active_vault)) {
                Ok(db) => Some(db),
                Err(failure) if failure.proves_unwritable() => None,
                Err(failure) => return Err(failure.kind),
            }
        }
        // Nothing resolvable is active, so no writer can be using an old vault.
        Ok(Active::NotConfigured) | Err(VaultError::ConfigInvalid) => None,
        Ok(Active::Override(_)) => return Err(VaultError::ManagementDisabled),
        Err(error) => return Err(error),
    };
    if let Some(db) = &fence {
        db.execute_batch("BEGIN IMMEDIATE")
            .map_err(|_| VaultError::Busy)?;
    }
    let result = write_config(
        env,
        &Config {
            version: CONFIG_VERSION,
            active_vault: new_path.to_string_lossy().into_owned(),
            location_approved,
        },
    );
    if let Some(db) = &fence {
        let _ = db.execute_batch("ROLLBACK");
    }
    result
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub visits: i64,
    pub snapshots: i64,
}

pub fn counts(db: &Connection) -> rusqlite::Result<Counts> {
    db.query_row(
        "SELECT (SELECT count(*) FROM visits), (SELECT count(*) FROM tab_snapshots)",
        [],
        |row| {
            Ok(Counts {
                visits: row.get(0)?,
                snapshots: row.get(1)?,
            })
        },
    )
}

/// The vault an operation started against (see `Session::ticket` / `Session::write_for`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultTicket {
    pub path: String,
    /// Opaque identity of the vault file (see `vault_token`), safe to hand to the extension.
    pub token: String,
}

/// Identifies a vault file independently of how its path is spelled (case, `\\?\` prefix,
/// junctions): SHA-256 over the volume serial number and file index on Windows. Only the hash
/// leaves the host, never the path. Replacing the file (e.g. restoring a backup) yields a new token.
pub fn vault_token(path: &Path) -> Result<String, VaultError> {
    use sha2::{Digest, Sha256};
    let identity = file_identity(path)?;
    Ok(Sha256::digest(identity)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

#[cfg(windows)]
fn file_identity(path: &Path) -> Result<Vec<u8>, VaultError> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };
    let file = File::open(path).map_err(|error| OpenFailure::from_io(&error).kind)?;
    // SAFETY: the handle is valid while `file` lives and the struct is plain data.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(VaultError::Io);
    }
    let mut identity = b"memocampus-vault-identity-v1\0".to_vec();
    for part in [
        info.dwVolumeSerialNumber,
        info.nFileIndexHigh,
        info.nFileIndexLow,
    ] {
        identity.extend_from_slice(&part.to_le_bytes());
    }
    Ok(identity)
}

#[cfg(not(windows))]
fn file_identity(path: &Path) -> Result<Vec<u8>, VaultError> {
    use std::os::unix::fs::MetadataExt;
    let metadata = fs::metadata(path).map_err(|error| OpenFailure::from_io(&error).kind)?;
    let mut identity = b"memocampus-vault-identity-v1\0".to_vec();
    identity.extend_from_slice(&metadata.dev().to_le_bytes());
    identity.extend_from_slice(&metadata.ino().to_le_bytes());
    Ok(identity)
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
    connection: Option<(String, Connection)>,
}

impl Session {
    pub fn new(env: Environment) -> Self {
        Self {
            env,
            connection: None,
        }
    }

    pub fn reset(&mut self) {
        self.connection = None;
    }

    /// Path of the active vault as recorded in the config (or the override).
    pub fn active_path(&self) -> Result<String, VaultError> {
        match resolve(&self.env)? {
            Active::Override(path) => Ok(path.to_string_lossy().into_owned()),
            Active::Configured(config) => Ok(config.active_vault),
            Active::NotConfigured => Err(VaultError::NotConfigured),
        }
    }

    /// Opens the configured vault, first dropping a cached connection that no longer matches
    /// the config (see the type-level notes for what this does and does not guarantee).
    fn open(&mut self) -> Result<(&str, &mut Connection), VaultError> {
        if let Some((cached, _)) = &self.connection {
            if self.active_path().ok().as_deref() != Some(cached.as_str()) {
                self.reset();
            }
        }
        self.open_cached()
    }

    fn open_cached(&mut self) -> Result<(&str, &mut Connection), VaultError> {
        if self.connection.is_none() {
            let path = self.active_path()?;
            let db = open_vault(Path::new(&path))?;
            self.connection = Some((path, db));
        }
        let (path, db) = self
            .connection
            .as_mut()
            .expect("connection was just opened");
        Ok((path.as_str(), db))
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
        let (path, db) = if revalidate {
            self.open()?
        } else {
            self.open_cached()?
        };
        let path = path.to_owned();
        if expected.is_some_and(|expected| expected != path) {
            self.reset();
            return Err(VaultError::Switched.into());
        }
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| VaultError::Busy)?;
        if !overridden {
            match resolve(&env)? {
                Active::Configured(config) if config.active_vault == path => {}
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

fn copy_and_verify(source: &Connection, temp: &Path) -> Result<(), VaultError> {
    let mut target = Connection::open_with_flags(
        temp,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(failed)?;
    source.execute_batch("BEGIN").map_err(failed)?;
    let copied = (|| {
        // Counting inside the transaction takes the shared lock that pins the snapshot.
        let expected = (
            counts(source).map_err(failed)?,
            images::table_counts(source).map_err(failed)?,
        );
        let backup = Backup::new(source, &mut target).map_err(failed)?;
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
    let expected = copied?;
    let integrity: String = target
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(failed)?;
    validate_schema(&target).map_err(failed)?;
    let actual = (
        counts(&target).map_err(failed)?,
        images::table_counts(&target).map_err(failed)?,
    );
    if integrity != "ok" || actual != expected {
        return Err(VaultError::BackupFailed);
    }
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
        assert_eq!(app_id, APP_ID);
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
        assert_eq!(open_vault(&empty).err(), Some(VaultError::Invalid));
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
        assert_eq!(open_vault(&foreign).err(), Some(VaultError::Invalid));

        let tagged = tmp.0.join("tagged.sqlite3");
        let db = Connection::open(&tagged).unwrap();
        db.execute_batch(SCHEMA_SQL).unwrap();
        db.execute_batch("PRAGMA application_id = 42;").unwrap();
        drop(db);
        assert_eq!(open_vault(&tagged).err(), Some(VaultError::Invalid));

        assert_eq!(open_vault(&tmp.0).err(), Some(VaultError::Invalid));
    }

    #[test]
    fn legacy_untagged_vaults_with_the_right_tables_are_accepted() {
        let tmp = TempDir::new();
        let legacy = tmp.0.join("legacy.sqlite3");
        Connection::open(&legacy)
            .unwrap()
            .execute_batch(SCHEMA_SQL)
            .unwrap();
        assert!(open_vault_checked(&legacy).is_ok());
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
        db.execute_batch(SCHEMA_SQL).unwrap();
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
            OpenCause::SchemaMismatch("tab_snapshots")
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
                    snapshots: 0
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
            let Counts { visits, snapshots } = counts(&copy).unwrap();
            assert_eq!(visits, snapshots, "backup split a transaction");
            largest = largest.max(visits);
        }
        assert!(largest > 0, "backups ran while data was being written");
    }
}
