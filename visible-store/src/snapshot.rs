//! The only code that opens a vault. It reads under one read transaction with a read-only
//! connection, and closes the connection before any row is judged.

use crate::{location, Stage, VisibleError};
use memocampus_native_host::schema::{self, SchemaError};
use rusqlite::{Connection, ErrorCode, OpenFlags};
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

const BUSY_TIMEOUT: Duration = Duration::from_secs(3);
const HEADER_LEN: usize = 100;
const MAGIC: &[u8] = b"SQLite format 3\0";
/// File-format read and write versions in the header: 1 is a rollback journal, 2 is WAL.
const ROLLBACK_JOURNAL: u8 = 1;
const WAL: u8 = 2;
// `page_metadata` has one row per URL (its latest capture), so each visit gets at most one.
const SELECT_VISITS: &str = "SELECT v.event_id, v.url, v.title, v.visited_at, m.title, m.description, m.og_title, m.og_description, m.og_site_name, m.og_image_url, m.favicon_url, m.captured_at FROM visits v LEFT JOIN page_metadata m ON m.url = v.url";
// Never `data` or the blob's counters. `sha256` and `save_id` identify; no rowid leaves the vault.
const SELECT_IMAGE_SAVES: &str = "SELECT s.save_id, s.image_url, s.page_url, s.frame_url, s.saved_at, s.incognito, s.mcp_public, b.sha256, b.mime_type, b.width, b.height FROM saved_images s JOIN image_blobs b ON b.id = s.blob_id";

/// One stored visit, not yet judged. Never leaves this crate.
pub(crate) struct RawVisit {
    pub event_id: String,
    pub url: String,
    pub title: String,
    pub visited_at: String,
    /// The metadata stored for the visit's URL, if any could be read.
    pub page: Option<RawPage>,
}

/// One `page_metadata` row, not yet checked. Never leaves this crate.
#[derive(Default)]
pub(crate) struct RawPage {
    pub title: Option<String>,
    pub description: Option<String>,
    pub og_title: Option<String>,
    pub og_description: Option<String>,
    pub og_site_name: Option<String>,
    pub og_image_url: Option<String>,
    pub favicon_url: Option<String>,
    pub captured_at: String,
}

/// One stored save action with its image's metadata, not yet judged. Never leaves this crate.
pub(crate) struct RawImageSave {
    pub save_id: String,
    pub image_url: String,
    pub page_url: Option<String>,
    pub frame_url: Option<String>,
    pub saved_at: i64,
    pub incognito: i64,
    pub mcp_public: i64,
    pub sha256: String,
    pub mime_type: String,
    pub width: i64,
    pub height: i64,
}

/// SQLite's message may echo data, so only a fixed classification is kept.
fn classify(error: &rusqlite::Error) -> VisibleError {
    match error.sqlite_error_code() {
        Some(ErrorCode::NotADatabase) => VisibleError::VaultSchemaMismatch,
        Some(ErrorCode::DatabaseCorrupt) => VisibleError::VaultCorrupt,
        _ => VisibleError::VaultUnavailable,
    }
}

fn sidecar_exists(path: &Path, suffix: &str) -> bool {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    Path::new(&name).exists()
}

/// Checked on the raw file first: merely reading a WAL database leaves `-wal` and `-shm` files.
pub(crate) fn check_file(path: &Path) -> Result<(), VisibleError> {
    let metadata = std::fs::metadata(path).map_err(|_| VisibleError::VaultUnavailable)?;
    if !metadata.is_file() {
        return Err(VisibleError::VaultUnavailable);
    }
    let mut header = [0u8; HEADER_LEN];
    File::open(path)
        .and_then(|mut file| file.read_exact(&mut header))
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::UnexpectedEof => VisibleError::VaultSchemaMismatch,
            _ => VisibleError::VaultUnavailable,
        })?;
    if !header.starts_with(MAGIC) {
        return Err(VisibleError::VaultSchemaMismatch);
    }
    let versions = [header[18], header[19]];
    if versions.contains(&WAL) || sidecar_exists(path, "-wal") || sidecar_exists(path, "-shm") {
        return Err(VisibleError::VaultUnsupported);
    }
    if versions != [ROLLBACK_JOURNAL, ROLLBACK_JOURNAL] {
        return Err(VisibleError::VaultSchemaMismatch);
    }
    Ok(())
}

fn scalar_i64(db: &Connection, sql: &str) -> Result<i64, VisibleError> {
    db.query_row(sql, [], |row| row.get(0))
        .map_err(|error| classify(&error))
}

/// Schema 4 can only be created, never reached from an older vault: up to schema 3, in-place
/// upgrades kept visits from before incognito capture was blocked (#9), with no column to identify
/// them. The check needs no write access and uses this package's own read-only connection.
pub(crate) fn check_schema(db: &Connection) -> Result<(), VisibleError> {
    schema::check(db).map_err(|error| match error {
        SchemaError::Sqlite(error) => classify(&error),
        _ => VisibleError::VaultSchemaMismatch,
    })?;
    let journal: String = db
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .map_err(|error| classify(&error))?;
    if journal != "delete" {
        return Err(VisibleError::VaultUnsupported);
    }
    Ok(())
}

/// A stored value of another type or invalid UTF-8 has no text to judge, so that row is left out.
/// The same in a metadata column leaves out the metadata only: the visit does not depend on it.
fn read_rows(db: &Connection) -> rusqlite::Result<Vec<RawVisit>> {
    let mut statement = db.prepare(SELECT_VISITS)?;
    let mut rows = statement.query([])?;
    let mut visits = Vec::new();
    while let Some(row) = rows.next()? {
        let text = |index: usize| {
            row.get_ref(index)
                .map(|value| value.as_str().ok().map(str::to_owned))
        };
        let optional = |index: usize| {
            row.get_ref(index).map(|value| {
                value
                    .as_str_or_null()
                    .ok()
                    .map(|text| text.map(str::to_owned))
            })
        };
        let page = match (
            optional(4)?,
            optional(5)?,
            optional(6)?,
            optional(7)?,
            optional(8)?,
            optional(9)?,
            optional(10)?,
            optional(11)?,
        ) {
            (
                Some(title),
                Some(description),
                Some(og_title),
                Some(og_description),
                Some(og_site_name),
                Some(og_image_url),
                Some(favicon_url),
                Some(Some(captured_at)),
            ) => Some(RawPage {
                title,
                description,
                og_title,
                og_description,
                og_site_name,
                og_image_url,
                favicon_url,
                captured_at,
            }),
            _ => None,
        };
        if let (Some(event_id), Some(url), Some(title), Some(visited_at)) =
            (text(0)?, text(1)?, text(2)?, text(3)?)
        {
            visits.push(RawVisit {
                event_id,
                url,
                title,
                visited_at,
                page,
            });
        }
    }
    Ok(visits)
}

/// Nothing is read: the vault passes the same checks as for a read, and is closed again.
fn read_nothing(_: &Connection) -> rusqlite::Result<()> {
    Ok(())
}

pub(crate) fn check_vault(path: &Path, stage: &mut dyn FnMut(Stage)) -> Result<(), VisibleError> {
    read_committed(path, stage, read_nothing)
}

/// As with visits, a row with a value of another type is left out. For an optional URL that means
/// anything but text or NULL: an unreadable source is not an absent one.
fn read_image_rows(db: &Connection) -> rusqlite::Result<Vec<RawImageSave>> {
    let mut statement = db.prepare(SELECT_IMAGE_SAVES)?;
    let mut rows = statement.query([])?;
    let mut saves = Vec::new();
    while let Some(row) = rows.next()? {
        let text = |index: usize| {
            row.get_ref(index)
                .map(|value| value.as_str().ok().map(str::to_owned))
        };
        let optional = |index: usize| {
            row.get_ref(index).map(|value| {
                value
                    .as_str_or_null()
                    .ok()
                    .map(|text| text.map(str::to_owned))
            })
        };
        let integer = |index: usize| row.get_ref(index).map(|value| value.as_i64().ok());
        if let (
            Some(save_id),
            Some(image_url),
            Some(page_url),
            Some(frame_url),
            Some(saved_at),
            Some(incognito),
            Some(mcp_public),
            Some(sha256),
            Some(mime_type),
            Some(width),
            Some(height),
        ) = (
            text(0)?,
            text(1)?,
            optional(2)?,
            optional(3)?,
            integer(4)?,
            integer(5)?,
            integer(6)?,
            text(7)?,
            text(8)?,
            integer(9)?,
            integer(10)?,
        ) {
            saves.push(RawImageSave {
                save_id,
                image_url,
                page_url,
                frame_url,
                saved_at,
                incognito,
                mcp_public,
                sha256,
                mime_type,
                width,
                height,
            });
        }
    }
    Ok(saves)
}

pub(crate) fn read_visits(
    path: &Path,
    stage: &mut dyn FnMut(Stage),
) -> Result<Vec<RawVisit>, VisibleError> {
    read_committed(path, stage, read_rows)
}

pub(crate) fn read_image_saves(
    path: &Path,
    stage: &mut dyn FnMut(Stage),
) -> Result<Vec<RawImageSave>, VisibleError> {
    read_committed(path, stage, read_image_rows)
}

/// Returns what `read` finds in one committed state of the vault.
fn read_committed<T>(
    path: &Path,
    stage: &mut dyn FnMut(Stage),
    read: fn(&Connection) -> rusqlite::Result<T>,
) -> Result<T, VisibleError> {
    let canonical = location::resolve_local(path)?;
    check_file(&canonical)?;
    // Without SQLITE_OPEN_URI a path can never carry `immutable=1`, which would skip locking.
    let db = Connection::open_with_flags(
        &canonical,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| classify(&error))?;
    db.busy_timeout(BUSY_TIMEOUT)
        .map_err(|error| classify(&error))?;
    db.execute_batch("PRAGMA trusted_schema = OFF; PRAGMA query_only = ON; BEGIN;")
        .map_err(|error| classify(&error))?;
    // This first read takes the shared lock, which pins every later read to one committed state.
    scalar_i64(&db, "SELECT count(*) FROM sqlite_master")?;
    check_schema(&db)?;
    stage(Stage::SchemaChecked);
    let rows = read(&db).map_err(|error| classify(&error))?;
    db.execute_batch("COMMIT")
        .map_err(|error| classify(&error))?;
    db.close().map_err(|(_, error)| classify(&error))?;
    Ok(rows)
}
