//! Slice 2: full-text index of extracted pages in a PoC-only SQLite database (FTS5 trigram).
//!
//! **Search contract.** A page HITs a query when its indexed title or its indexed body contains
//! the query as a literal substring: the same Unicode scalar values in the same order (no case
//! folding, no normalization, no width folding, no wildcard). The final decision is always made
//! here with `str::contains` on the stored text; FTS5 is only used to find candidate pages and
//! never decides a HIT by itself.
//!
//! - 1–2 characters (Unicode scalar values, so `𩸽` is one): every page is scanned.
//! - 3 or more: FTS5 trigram `GLOB` (with the query's `*`, `?`, `[` escaped as character classes)
//!   on the title and body columns gives the candidates, which are then checked. If the index is
//!   not known to be consistent, or the query contains control characters, every page is scanned
//!   instead (completeness before speed).
//!
//! The user's query is always a bound parameter; it is never put into SQL text, and never into
//! FTS5 MATCH syntax.
//!
//! **Data.** The database holds extracted page text: treat it like the MHTML archives themselves.
//!
//! **Opening a file.** Only a database created by this PoC is ever used, and a file is never
//! written to before it is known to be one ([`Index::open_existing`]):
//! 1. the file's first 100 bytes are read without SQLite: SQLite magic, this PoC's
//!    `application_id` ([`APPLICATION_ID`]) and rollback-journal format in the header, otherwise
//!    refused here;
//! 2. it is opened read-only with `immutable=1` (no locks, no journal read or created) and the
//!    application id, schema version, the complete schema (tables, columns, triggers, FTS5
//!    options, compared with a reference created in memory) and the `meta` rows are checked;
//! 3. only then is the real connection opened (read-write without create, or read-only), and the
//!    same checks run again on it before anything is written.
//!
//! Searching and statistics use [`Index::open_readonly`]: never creates, never writes.
//!
//! **Journal.** Always SQLite's default rollback journal (`journal_mode = DELETE`, `synchronous =
//! FULL`); the journal mode is never set or changed, and a database in WAL format is refused
//! (PO decision, report XIV-14). After a crash, the `-journal` file left behind is rolled back by
//! SQLite itself the next time the database is opened for writing.

use crate::limits::ErrorCode;
use crate::quality::{self, GateConfig, TextQuality, Verdict};
use crate::Extraction;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::Serialize;
use std::io::Read;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// SQLite `application_id` of a page-index database ("MCPF"). Never the only check: the schema and
/// the `meta` rows are verified too.
pub const APPLICATION_ID: i32 = 0x4D43_5046;
/// `meta.app` of a page-index database.
pub const APP_NAME: &str = "memocampus.page-fulltext.poc";
/// 2: `application_id` and `meta.app` added (databases of version 1 are refused; recreate them).
pub const SCHEMA_VERSION: &str = "2";
/// Queries shorter than this (in Unicode scalar values) cannot use the trigram index.
pub const MIN_FTS_CHARS: usize = 3;
pub const MAX_QUERY_BYTES: usize = 4096;
pub const MAX_KEY_BYTES: usize = 1024;
pub const TOKENIZER: &str = "trigram case_sensitive 1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexError {
    /// The SQLite library lacks FTS5 or the trigram tokenizer (with the SQLite message).
    Fts5Unavailable(String),
    SchemaMismatch(String),
    EmptyQuery,
    QueryTooLarge,
    InvalidKey,
    /// Text with a NUL character (never produced by the extractor).
    InvalidText,
    /// `SearchMode::Verify`: the FTS5 candidates missed pages that the full scan found.
    FtsMissedPages(usize),
    /// Fault injection in tests: the transaction was abandoned at this point.
    InjectedFault(&'static str),
    /// The file is not a page-index database (or could not be shown to be one); it was not
    /// modified.
    NotAPageIndex(String),
    /// No file at the path (nothing was created).
    NotFound,
    /// `create`: a file already exists at the path (it was not touched).
    AlreadyExists,
    /// A write was asked of an index opened read-only (or the file is write-protected).
    ReadOnly,
    /// A crashed write left a hot `-journal`: a read-only connection cannot roll it back. Opening
    /// the database for writing (`open_existing`, e.g. `page-index check`) lets SQLite do it.
    NeedsRecovery,
    Io(String),
    Sqlite(String),
}

impl IndexError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Fts5Unavailable(_) => "fts5_unavailable",
            Self::SchemaMismatch(_) => "schema_mismatch",
            Self::NotAPageIndex(_) => "not_a_page_index",
            Self::NotFound => "not_found",
            Self::AlreadyExists => "already_exists",
            Self::ReadOnly => "read_only",
            Self::NeedsRecovery => "needs_recovery",
            Self::Io(_) => "io_error",
            Self::EmptyQuery => "empty_query",
            Self::QueryTooLarge => "query_too_large",
            Self::InvalidKey => "invalid_key",
            Self::InvalidText => "invalid_text",
            Self::FtsMissedPages(_) => "fts_missed_pages",
            Self::InjectedFault(_) => "injected_fault",
            Self::Sqlite(_) => "sqlite_error",
        }
    }
}

impl std::fmt::Display for IndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fts5Unavailable(m)
            | Self::SchemaMismatch(m)
            | Self::NotAPageIndex(m)
            | Self::Io(m)
            | Self::Sqlite(m) => {
                write!(f, "{}: {m}", self.code())
            }
            Self::FtsMissedPages(n) => write!(f, "{}: {n}", self.code()),
            Self::InjectedFault(p) => write!(f, "{}: {p}", self.code()),
            _ => f.write_str(self.code()),
        }
    }
}

impl std::error::Error for IndexError {}

impl From<rusqlite::Error> for IndexError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sqlite(e.to_string())
    }
}

impl From<rusqlite::types::FromSqlError> for IndexError {
    fn from(e: rusqlite::types::FromSqlError) -> Self {
        Self::Sqlite(e.to_string())
    }
}

type Result<T> = std::result::Result<T, IndexError>;

/// FTS5 `detail` option: `full` keeps token positions, `none` only which rows contain a token
/// (smaller; trigram phrase matching then rechecks row content inside FTS5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Detail {
    Full,
    None,
}

impl Detail {
    fn as_str(self) -> &'static str {
        match self {
            Detail::Full => "full",
            Detail::None => "none",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// Used when the database is created; an existing database keeps its own.
    pub detail: Detail,
}

impl Default for Options {
    /// `detail=none`: the PO's chosen configuration (report XIV-4); `full` stays for comparison.
    fn default() -> Self {
        Self {
            detail: Detail::None,
        }
    }
}

/// How 3+ character queries ask FTS5 for candidates. `Glob` is the one used; the others exist for
/// the comparison in the report (XIV) and its test.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateQuery {
    /// `title GLOB ?` / `body GLOB ?` with `*query*`, wildcards escaped as `[*]` `[?]` `[[]`.
    Glob,
    /// `LIKE ? ESCAPE '\'` with `%query%`. Case-insensitive for ASCII in SQLite.
    Like,
    /// `pages_fts MATCH ?` with the query as one FTS5 string (`"` doubled).
    MatchPhrase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    /// FTS5 candidates for 3+ characters when safe, otherwise a full scan.
    Auto,
    /// Always scan every page.
    FullScan,
    /// Both, and an error if the FTS5 path missed a page the full scan found (tests, benchmark).
    Verify,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    FtsCandidates,
    FullScan,
}

/// One page that HITs. Never carries the body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Hit {
    pub page_id: i64,
    pub page_key: String,
    pub url: Option<String>,
    pub title: Option<String>,
    pub in_title: bool,
    pub in_body: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub method: Method,
    /// Why a 3+ character query was not answered with FTS5 candidates.
    pub fallback: Option<&'static str>,
    pub query_chars: usize,
    /// Pages whose text was checked (candidates, or every page).
    pub checked_pages: usize,
    /// In page_id order.
    pub hits: Vec<Hit>,
}

impl SearchResult {
    pub fn page_ids(&self) -> Vec<i64> {
        self.hits.iter().map(|h| h.page_id).collect()
    }
}

/// What a registration did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Inserted,
    Updated,
    /// The new extraction failed or was unusable; the indexed page was left as it was.
    KeptPrevious,
    /// The extraction failed and there was no page: nothing stored but the attempt.
    NotRegistered,
}

/// Quality record kept with a page and with each attempt (codes and numbers only, no text).
#[derive(Debug, Clone, Serialize)]
pub struct QualityRecord {
    pub status: &'static str,
    pub body: TextQuality,
    pub body_verdict: Verdict,
    pub title: TextQuality,
    pub title_verdict: Verdict,
    pub encoding: &'static str,
    pub encoding_source: crate::charset::EncodingSource,
    pub malformed_bytes_replaced: bool,
    pub warnings: Vec<&'static str>,
    pub gate: GateConfig,
}

#[derive(Debug, Clone, Serialize)]
pub struct Registration {
    pub outcome: Outcome,
    pub page_id: Option<i64>,
    /// Page status code, or the failure class when the extraction failed.
    pub status: &'static str,
    pub error_code: Option<&'static str>,
    pub title_indexed: bool,
    pub body_indexed: bool,
}

/// Index state stored in `meta.fts_state`: FTS5 is used for candidates only when `ok`.
pub const FTS_OK: &str = "ok";
pub const FTS_INCONSISTENT: &str = "inconsistent";

#[derive(Debug, Clone, Serialize)]
pub struct CheckReport {
    pub pages: i64,
    pub fts_integrity_ok: bool,
    /// The FTS5 message when the integrity check failed.
    pub fts_message: Option<String>,
    pub sqlite_integrity_ok: bool,
    pub fts_state: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Stats {
    pub pages: i64,
    pub pages_with_body: i64,
    pub pages_with_title: i64,
    pub attempts: i64,
    pub detail: String,
    pub fts_state: String,
    pub sqlite_version: &'static str,
}

pub struct Index {
    conn: Connection,
    read_only: bool,
    #[doc(hidden)]
    pub fault: Option<&'static str>,
    #[doc(hidden)]
    pub candidate_query: CandidateQuery,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Fails with [`IndexError::Fts5Unavailable`] when this SQLite cannot create an FTS5 table with
/// `tokenize` (no FTS5, or no such tokenizer). Nothing is left behind.
pub fn probe_fts5(conn: &Connection, tokenize: &str) -> Result<()> {
    let sql = format!(
        "CREATE VIRTUAL TABLE temp.fts5_probe USING fts5(x, tokenize={});",
        sql_string(tokenize)
    );
    conn.execute_batch(&sql)
        .map_err(|e| IndexError::Fts5Unavailable(e.to_string()))?;
    conn.execute_batch("DROP TABLE temp.fts5_probe;")?;
    Ok(())
}

/// A constant (never user input) as an SQL string literal.
fn sql_string(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// `*query*` with GLOB's special characters as one-character classes.
pub fn glob_pattern(q: &str) -> String {
    let mut p = String::with_capacity(q.len() + 8);
    p.push('*');
    for c in q.chars() {
        match c {
            '*' => p.push_str("[*]"),
            '?' => p.push_str("[?]"),
            '[' => p.push_str("[[]"),
            _ => p.push(c),
        }
    }
    p.push('*');
    p
}

fn like_pattern(q: &str) -> String {
    let mut p = String::with_capacity(q.len() + 8);
    p.push('%');
    for c in q.chars() {
        if matches!(c, '%' | '_' | '\\') {
            p.push('\\');
        }
        p.push(c);
    }
    p.push('%');
    p
}

fn match_phrase(q: &str) -> String {
    format!("\"{}\"", q.replace('"', "\"\""))
}

fn check_key(key: &str) -> Result<()> {
    if key.is_empty() || key.len() > MAX_KEY_BYTES || key.chars().any(char::is_control) {
        return Err(IndexError::InvalidKey);
    }
    Ok(())
}

fn check_query(q: &str) -> Result<usize> {
    if q.is_empty() {
        return Err(IndexError::EmptyQuery);
    }
    if q.len() > MAX_QUERY_BYTES {
        return Err(IndexError::QueryTooLarge);
    }
    Ok(q.chars().count())
}

/// Failure class of an extraction error (what the gate records).
pub fn failure_class(e: ErrorCode) -> &'static str {
    use ErrorCode::*;
    match e {
        NotAFile | InputUnreadable => "input_unreadable",
        InputTooLarge => "input_too_large",
        HtmlTooLarge | DecodedHtmlTooLarge | TextTooLarge | MetadataTooLarge => "content_too_large",
        NotMime
        | NotMultipartRelated
        | InvalidBoundary
        | MalformedMultipart
        | UnterminatedMultipart
        | NoParts
        | TooManyParts
        | NestingTooDeep
        | UnexpectedNestedMessage
        | InvalidTransferEncoding
        | InvalidBase64
        | InvalidQuotedPrintable
        | StartNotFound
        | StartAmbiguous
        | RootNotHtml
        | NoHtmlPart => "mhtml_parse_failed",
        UnsupportedCharset => "charset_unusable",
        InternalPanic => "internal_error",
    }
}

/// A page row with title and body text (no text other than what is indexed).
pub struct PageText<'a> {
    pub key: &'a str,
    pub url: Option<&'a str>,
    pub title: Option<&'a str>,
    pub body: &'a str,
}

fn not_index(why: impl Into<String>) -> IndexError {
    IndexError::NotAPageIndex(why.into())
}

/// Any SQLite error while examining a file that is not yet known to be ours means "not ours".
fn examined<T>(r: rusqlite::Result<T>, what: &str) -> Result<T> {
    r.map_err(|e| not_index(format!("{what}: {e}")))
}

/// Step 1 (no SQLite involved): the SQLite magic and this PoC's application id in the header.
fn check_header(path: &Path) -> Result<()> {
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(IndexError::NotFound),
        Err(e) => return Err(IndexError::Io(e.to_string())),
    };
    let meta = f.metadata().map_err(|e| IndexError::Io(e.to_string()))?;
    if !meta.is_file() {
        return Err(not_index("not a regular file"));
    }
    let mut h = [0u8; 100];
    if f.read_exact(&mut h).is_err() {
        return Err(not_index(
            "not an SQLite database (shorter than its header)",
        ));
    }
    if &h[..16] != b"SQLite format 3\0" {
        return Err(not_index("not an SQLite database"));
    }
    let app = i32::from_be_bytes([h[68], h[69], h[70], h[71]]);
    if app != APPLICATION_ID {
        return Err(not_index(format!("application_id {app:#x}")));
    }
    // File format write/read versions: 1 = rollback journal, 2 = WAL.
    if h[18] != 1 || h[19] != 1 {
        return Err(not_index(
            "not in rollback-journal format (WAL is not used)",
        ));
    }
    Ok(())
}

/// `file:` URI of `path` with `immutable=1`: read-only, no locking, no `-wal`/`-shm`/journal
/// files are created or read. Used only to examine a file before it is trusted.
fn immutable_uri(path: &Path) -> Result<String> {
    let abs = std::path::absolute(path).map_err(|e| IndexError::Io(e.to_string()))?;
    let s = abs
        .to_str()
        .ok_or_else(|| not_index("path is not valid Unicode"))?;
    let s = if cfg!(windows) {
        let s = s.strip_prefix(r"\\?\").unwrap_or(s);
        let b = s.as_bytes();
        if !(b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\') {
            // UNC and device paths: not supported here (fail closed).
            return Err(not_index("only local drive paths are supported"));
        }
        format!("/{}", s.replace('\\', "/"))
    } else {
        s.to_owned()
    };
    let mut uri = String::from("file:");
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/:".contains(&b) {
            uri.push(b as char);
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    uri.push_str("?immutable=1");
    Ok(uri)
}

/// Settings of this connection only (nothing is written to the file).
fn configure(conn: &Connection) -> Result<()> {
    // Untrusted text never reaches SQL as code, but keep the schema's functions restricted.
    conn.execute_batch(
        "PRAGMA trusted_schema = OFF;
         PRAGMA secure_delete = ON;
         PRAGMA foreign_keys = ON;",
    )?;
    Ok(())
}

fn rows2(conn: &Connection, sql: &str) -> rusqlite::Result<Vec<(String, String)>> {
    let mut st = conn.prepare(sql)?;
    let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    rows.collect()
}

type SchemaRow = (String, String, String, Option<String>);

fn schema_rows(conn: &Connection) -> rusqlite::Result<Vec<SchemaRow>> {
    let mut st =
        conn.prepare("SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY type, name")?;
    let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
    rows.collect()
}

/// Steps 2–5 (and 7): this PoC's application id, schema version, exact schema (compared with a
/// reference created in memory with the same `detail`), FTS5 configuration and `meta` rows.
/// Reads only. Returns the database's `detail`.
fn verify(conn: &Connection) -> Result<Detail> {
    let app: i32 = examined(
        conn.query_row("PRAGMA application_id", [], |r| r.get(0)),
        "application_id",
    )?;
    if app != APPLICATION_ID {
        return Err(not_index(format!("application_id {app:#x}")));
    }
    // Reading the journal mode (no argument) does not change it.
    let journal: String = examined(
        conn.query_row("PRAGMA journal_mode", [], |r| r.get(0)),
        "journal_mode",
    )?;
    if !matches!(journal.as_str(), "delete" | "memory") {
        return Err(not_index(format!("journal_mode {journal}")));
    }
    let has_meta: bool = examined(
        conn.query_row(
            "SELECT count(*) = 1 FROM sqlite_schema WHERE type = 'table' AND name = 'meta'",
            [],
            |r| r.get(0),
        ),
        "schema",
    )?;
    if !has_meta {
        return Err(not_index("no meta table"));
    }
    let meta = examined(
        rows2(conn, "SELECT key, value FROM meta ORDER BY key"),
        "meta",
    )?;
    let get = |k: &str| {
        meta.iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.as_str())
    };
    match get("schema_version") {
        None => return Err(not_index("no schema version")),
        Some(SCHEMA_VERSION) => {}
        Some(v) => return Err(IndexError::SchemaMismatch(format!("found {v}"))),
    }
    let detail = match get("detail") {
        Some("full") => Detail::Full,
        Some("none") => Detail::None,
        _ => return Err(not_index("meta.detail")),
    };
    let reference = Connection::open_in_memory()?;
    reference.execute_batch(&schema_sql(detail))?;
    if examined(schema_rows(conn), "schema")? != schema_rows(&reference)? {
        return Err(not_index("schema differs from a page-index database"));
    }
    let fts_config = "SELECT k, CAST(v AS TEXT) FROM pages_fts_config ORDER BY k";
    if examined(rows2(conn, fts_config), "FTS5 configuration")? != rows2(&reference, fts_config)? {
        return Err(not_index("FTS5 configuration differs"));
    }
    let keys: Vec<&str> = meta.iter().map(|(k, _)| k.as_str()).collect();
    if keys != ["app", "detail", "fts_state", "schema_version", "tokenizer"]
        || get("app") != Some(APP_NAME)
        || get("tokenizer") != Some(TOKENIZER)
        || !matches!(get("fts_state"), Some(FTS_OK | FTS_INCONSISTENT))
    {
        return Err(not_index("meta rows differ from a page-index database"));
    }
    Ok(detail)
}

/// The schema (tables, index, FTS5 table, triggers). Also the reference that [`verify`] compares
/// an existing database with, so it must stay byte-for-byte the text used at creation.
fn schema_sql(detail: Detail) -> String {
    format!(
        "CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT;
             CREATE TABLE pages(
               page_id INTEGER PRIMARY KEY,
               page_key TEXT NOT NULL UNIQUE,
               url TEXT,
               title TEXT,
               body TEXT NOT NULL,
               status TEXT NOT NULL,
               quality TEXT NOT NULL,
               registered_at INTEGER NOT NULL,
               updated_at INTEGER NOT NULL
             ) STRICT;
             CREATE TABLE attempts(
               attempt_id INTEGER PRIMARY KEY,
               page_key TEXT NOT NULL,
               at INTEGER NOT NULL,
               outcome TEXT NOT NULL,
               status TEXT NOT NULL,
               error_code TEXT,
               quality TEXT
             ) STRICT;
             CREATE INDEX attempts_by_key ON attempts(page_key, attempt_id);
             CREATE VIRTUAL TABLE pages_fts USING fts5(
               title, body,
               content = 'pages', content_rowid = 'page_id',
               tokenize = {tok}, detail = {detail}
             );
             CREATE TRIGGER pages_ai AFTER INSERT ON pages BEGIN
               INSERT INTO pages_fts(rowid, title, body) VALUES (new.page_id, new.title, new.body);
             END;
             CREATE TRIGGER pages_ad AFTER DELETE ON pages BEGIN
               INSERT INTO pages_fts(pages_fts, rowid, title, body)
                 VALUES ('delete', old.page_id, old.title, old.body);
             END;
             CREATE TRIGGER pages_au AFTER UPDATE OF title, body ON pages BEGIN
               INSERT INTO pages_fts(pages_fts, rowid, title, body)
                 VALUES ('delete', old.page_id, old.title, old.body);
               INSERT INTO pages_fts(rowid, title, body) VALUES (new.page_id, new.title, new.body);
             END;",
        tok = sql_string(TOKENIZER),
        detail = sql_string(detail.as_str()),
    )
}

impl Index {
    fn new(conn: Connection, read_only: bool) -> Self {
        Self {
            conn,
            read_only,
            fault: None,
            candidate_query: CandidateQuery::Glob,
        }
    }

    /// A new index in memory (tests).
    pub fn open_in_memory(options: Options) -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        configure(&conn)?;
        Self::init_schema(&conn, options.detail)?;
        verify(&conn)?;
        Ok(Self::new(conn, false))
    }

    /// Opens an existing page-index database for reading only: nothing is created (a missing
    /// path is [`IndexError::NotFound`]), nothing is written, the journal mode is not changed.
    /// Any other file is refused unmodified.
    pub fn open_readonly(path: &Path) -> Result<Self> {
        probe_fts5(&Connection::open_in_memory()?, TOKENIZER)?;
        Self::examine(path)?;
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        configure(&conn)?;
        // The first read: SQLite reports a hot journal it may not roll back on this connection.
        if let Err(rusqlite::Error::SqliteFailure(f, _)) =
            conn.query_row("SELECT count(*) FROM sqlite_schema", [], |r| {
                r.get::<_, i64>(0)
            })
        {
            if f.extended_code == rusqlite::ffi::SQLITE_READONLY_ROLLBACK {
                return Err(IndexError::NeedsRecovery);
            }
        }
        verify(&conn)?;
        Ok(Self::new(conn, true))
    }

    /// Opens an existing page-index database for writing (never creates one). Any other file is
    /// refused before anything is written to it. `options.detail` is ignored (the database keeps
    /// its own).
    ///
    /// If a crashed session left a hot `-journal`, SQLite rolls it back on this connection's
    /// first read (its standard recovery), which happens only after steps 1–5 accepted the file.
    pub fn open_existing(path: &Path, _options: Options) -> Result<Self> {
        probe_fts5(&Connection::open_in_memory()?, TOKENIZER)?;
        Self::examine(path)?;
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        configure(&conn)?;
        // SQLite opens a write-protected file read-only without an error: report it here.
        if conn.is_readonly(rusqlite::DatabaseName::Main)? {
            return Err(IndexError::ReadOnly);
        }
        // Step 7: the file may have been replaced since it was examined; check again on this
        // connection before anything is written through it.
        verify(&conn)?;
        Ok(Self::new(conn, false))
    }

    /// Creates a new page-index database at `path`, which must not exist
    /// ([`IndexError::AlreadyExists`] otherwise: an existing file is never touched).
    pub fn create(path: &Path, options: Options) -> Result<Self> {
        probe_fts5(&Connection::open_in_memory()?, TOKENIZER)?;
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(IndexError::AlreadyExists)
            }
            Err(e) => return Err(IndexError::Io(e.to_string())),
        }
        let made = (|| {
            let conn = Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            configure(&conn)?;
            Self::init_schema(&conn, options.detail)?;
            verify(&conn)?;
            Ok(conn)
        })();
        match made {
            Ok(conn) => Ok(Self::new(conn, false)),
            Err(e) => {
                // Only the file created just above (and its journal) is removed.
                for suffix in ["", "-journal"] {
                    let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
                }
                Err(e)
            }
        }
    }

    /// The explicit create-or-open used by registration: [`Index::create`] when nothing exists
    /// at `path`, otherwise [`Index::open_existing`] (so an empty or foreign file is refused, not
    /// initialized).
    pub fn open_or_create(path: &Path, options: Options) -> Result<Self> {
        match std::fs::symlink_metadata(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                match Self::create(path, options) {
                    Err(IndexError::AlreadyExists) => Self::open_existing(path, options),
                    r => r,
                }
            }
            Err(e) => Err(IndexError::Io(e.to_string())),
            Ok(_) => Self::open_existing(path, options),
        }
    }

    /// Steps 1–5 without a normal connection: the header, then an `immutable=1` read-only
    /// connection (reads and creates no journal, takes no locks) for [`verify`].
    fn examine(path: &Path) -> Result<()> {
        check_header(path)?;
        let conn = examined(
            Connection::open_with_flags(
                immutable_uri(path)?,
                OpenFlags::SQLITE_OPEN_READ_ONLY
                    | OpenFlags::SQLITE_OPEN_URI
                    | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            ),
            "open",
        )?;
        configure(&conn)?;
        verify(&conn)?;
        Ok(())
    }

    /// The schema, application id and `meta` rows of a new (empty) database, in one transaction
    /// that first checks the database is still empty.
    fn init_schema(conn: &Connection, detail: Detail) -> Result<()> {
        let sql = format!(
            "PRAGMA application_id = {APPLICATION_ID};
             {schema}
             INSERT INTO meta(key, value) VALUES
               ('app', {app}), ('schema_version', {ver}), ('tokenizer', {tok}),
               ('detail', {detail}), ('fts_state', {ok});",
            schema = schema_sql(detail),
            app = sql_string(APP_NAME),
            ver = sql_string(SCHEMA_VERSION),
            tok = sql_string(TOKENIZER),
            detail = sql_string(detail.as_str()),
            ok = sql_string(FTS_OK),
        );
        conn.execute_batch("BEGIN IMMEDIATE;")?;
        let r = (|| {
            let (objects, app): (i64, i32) = conn.query_row(
                "SELECT (SELECT count(*) FROM sqlite_schema), (SELECT application_id FROM pragma_application_id)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            if objects != 0 || app != 0 {
                return Err(not_index("the new database file is not empty"));
            }
            conn.execute_batch(&sql)?;
            conn.execute_batch("COMMIT;")?;
            Ok(())
        })();
        if r.is_err() {
            let _ = conn.execute_batch("ROLLBACK;");
        }
        r
    }

    fn writable(&self) -> Result<()> {
        if self.read_only {
            Err(IndexError::ReadOnly)
        } else {
            Ok(())
        }
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    fn meta(&self, key: &str) -> Result<String> {
        Ok(self
            .conn
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))?)
    }

    pub fn fts_state(&self) -> Result<String> {
        self.meta("fts_state")
    }

    /// Inserts or replaces the page `key` (same key: same page_id, no duplicate), in one
    /// transaction with its FTS5 rows (triggers) and an attempt record.
    pub fn put_page(
        &mut self,
        page: &PageText<'_>,
        status: &str,
        quality: &str,
    ) -> Result<(Outcome, i64)> {
        self.writable()?;
        check_key(page.key)?;
        if page.body.contains('\0')
            || page.title.is_some_and(|t| t.contains('\0'))
            || page.url.is_some_and(|u| u.contains('\0'))
        {
            return Err(IndexError::InvalidText);
        }
        let t = now();
        let fault = self.fault;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existed: Option<i64> = tx
            .query_row(
                "SELECT page_id FROM pages WHERE page_key = ?1",
                [page.key],
                |r| r.get(0),
            )
            .optional()?;
        let page_id: i64 = tx.query_row(
            "INSERT INTO pages(page_key, url, title, body, status, quality, registered_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
             ON CONFLICT(page_key) DO UPDATE SET
               url = excluded.url, title = excluded.title, body = excluded.body,
               status = excluded.status, quality = excluded.quality, updated_at = excluded.updated_at
             RETURNING page_id",
            params![page.key, page.url, page.title, page.body, status, quality, t],
            |r| r.get(0),
        )?;
        if fault == Some("after_page_write") {
            return Err(IndexError::InjectedFault("after_page_write"));
        }
        let outcome = if existed.is_some() {
            Outcome::Updated
        } else {
            Outcome::Inserted
        };
        tx.execute(
            "INSERT INTO attempts(page_key, at, outcome, status, error_code, quality)
             VALUES (?1, ?2, ?3, ?4, NULL, ?5)",
            params![page.key, t, outcome_str(outcome), status, quality],
        )?;
        if fault == Some("before_commit") {
            return Err(IndexError::InjectedFault("before_commit"));
        }
        tx.commit()?;
        Ok((outcome, page_id))
    }

    fn record_attempt(
        &mut self,
        key: &str,
        outcome: Outcome,
        status: &str,
        error_code: Option<&str>,
        quality: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO attempts(page_key, at, outcome, status, error_code, quality)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                key,
                now(),
                outcome_str(outcome),
                status,
                error_code,
                quality
            ],
        )?;
        Ok(())
    }

    /// The registered page `key`: its page_id, and whether it has an indexed body (a body is
    /// stored only when it passed the gate, and the FTS5 rows mirror the stored text).
    fn registered(&self, key: &str) -> Result<Option<(i64, bool)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT page_id, body <> '' FROM pages WHERE page_key = ?1",
                [key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    /// Registers an extraction result through the quality gate:
    /// - failed extraction: the existing page (if any) is not touched; the attempt is recorded;
    /// - a registered page with an indexed body keeps its title, body and FTS5 rows unless the
    ///   new extraction has a body that passes the gate (an empty, garbled or failed
    ///   re-extraction never replaces an indexed body: PO decision, report XIV-13);
    /// - a registered page without a body keeps its previous state when the new body is garbled
    ///   or nothing is indexable;
    /// - otherwise the page is inserted or replaced with whatever passed the gate (a new page
    ///   with a clean title and an empty or garbled body is registered with its title only).
    ///
    /// Every call records an attempt (outcome, status, error or quality), including the kept ones.
    pub fn register(
        &mut self,
        key: &str,
        extraction: std::result::Result<&Extraction, ErrorCode>,
        gate: &GateConfig,
    ) -> Result<Registration> {
        self.writable()?;
        check_key(key)?;
        let existing = self.registered(key)?;
        let x = match extraction {
            Err(e) => {
                let outcome = if existing.is_some() {
                    Outcome::KeptPrevious
                } else {
                    Outcome::NotRegistered
                };
                let class = failure_class(e);
                self.record_attempt(key, outcome, class, Some(e.as_str()), None)?;
                return Ok(Registration {
                    outcome,
                    page_id: existing.map(|(id, _)| id),
                    status: class,
                    error_code: Some(e.as_str()),
                    title_indexed: false,
                    body_indexed: false,
                });
            }
            Ok(x) => x,
        };
        let body = &x.content.body_text;
        let title = x.metadata.title.as_deref().unwrap_or("");
        let decoded_with = quality::encoding_named(x.diagnostics.encoding.name);
        let body_q = quality::measure_with_removed(
            body,
            decoded_with,
            x.diagnostics.dropped_control_characters,
        );
        let title_q = quality::measure(title, decoded_with);
        let body_verdict = gate.judge(&body_q);
        let title_verdict = gate.judge_title(&title_q, body_verdict);
        let index_body = matches!(body_verdict, Verdict::Clean | Verdict::Suspect);
        let index_title = matches!(title_verdict, Verdict::Clean | Verdict::Suspect);
        let status = match (index_body, index_title, body_verdict) {
            (true, _, _)
                if body_verdict == Verdict::Clean
                    && matches!(title_verdict, Verdict::Clean | Verdict::Empty)
                    && x.diagnostics.warnings.is_empty() =>
            {
                "ok"
            }
            (true, _, _) => "ok_with_warnings",
            (false, true, Verdict::Empty) => "title_only_empty_body",
            (false, true, _) => "title_only_garbled_body",
            (false, false, Verdict::Empty) => "nothing_indexed_empty",
            (false, false, _) => "nothing_indexed_garbled",
        };
        let record = QualityRecord {
            status,
            body: body_q,
            body_verdict,
            title: title_q,
            title_verdict,
            encoding: x.diagnostics.encoding.name,
            encoding_source: x.diagnostics.encoding.source,
            malformed_bytes_replaced: x.diagnostics.encoding.malformed_sequences_replaced,
            warnings: x.diagnostics.warnings.clone(),
            gate: *gate,
        };
        let quality_json = serde_json::to_string(&record).expect("serializable");
        let keep = match existing {
            // An indexed body is replaced only by a body that passed the gate.
            Some((_, true)) => !index_body,
            // A registered page without a body: not replaced by a garbled or empty result.
            Some((_, false)) => body_verdict == Verdict::Garbled || (!index_body && !index_title),
            None => false,
        };
        if keep {
            self.record_attempt(
                key,
                Outcome::KeptPrevious,
                status,
                None,
                Some(&quality_json),
            )?;
            return Ok(Registration {
                outcome: Outcome::KeptPrevious,
                page_id: existing.map(|(id, _)| id),
                status,
                error_code: None,
                title_indexed: false,
                body_indexed: false,
            });
        }
        let url = x.source.snapshot_location.as_deref().or(x
            .source
            .selected_part
            .content_location
            .as_deref());
        let page = PageText {
            key,
            url,
            title: index_title.then_some(title),
            body: if index_body { body } else { "" },
        };
        let (outcome, page_id) = self.put_page(&page, status, &quality_json)?;
        Ok(Registration {
            outcome,
            page_id: Some(page_id),
            status,
            error_code: None,
            title_indexed: index_title,
            body_indexed: index_body,
        })
    }

    /// Removes the page and its FTS5 rows (trigger) in one transaction. False if there was none.
    pub fn delete(&mut self, key: &str) -> Result<bool> {
        self.writable()?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let n = tx.execute("DELETE FROM pages WHERE page_key = ?1", [key])?;
        tx.commit()?;
        Ok(n > 0)
    }

    /// Rebuilds the FTS5 index from the pages table, checks it, and marks it usable.
    pub fn rebuild(&mut self) -> Result<CheckReport> {
        self.writable()?;
        let fault = self.fault;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("INSERT INTO pages_fts(pages_fts) VALUES ('rebuild')", [])?;
        if fault == Some("during_rebuild") {
            return Err(IndexError::InjectedFault("during_rebuild"));
        }
        tx.commit()?;
        self.check()
    }

    /// FTS5 integrity check against the content table (`rank = 1`) and SQLite's own check. Sets
    /// `fts_state`: a failed check makes every search a full scan until [`Index::rebuild`].
    pub fn check(&mut self) -> Result<CheckReport> {
        self.writable()?;
        let fts = self.conn.execute(
            "INSERT INTO pages_fts(pages_fts, rank) VALUES ('integrity-check', 1)",
            [],
        );
        let (fts_ok, fts_message) = match fts {
            Ok(_) => (true, None),
            Err(rusqlite::Error::SqliteFailure(f, m))
                if f.code == rusqlite::ErrorCode::DatabaseCorrupt =>
            {
                (false, Some(m.unwrap_or_default()))
            }
            Err(e) => return Err(e.into()),
        };
        let sqlite_ok: String = self
            .conn
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        let state = if fts_ok && sqlite_ok == "ok" {
            FTS_OK
        } else {
            FTS_INCONSISTENT
        };
        self.conn.execute(
            "UPDATE meta SET value = ?1 WHERE key = 'fts_state'",
            [state],
        )?;
        Ok(CheckReport {
            pages: self.page_count()?,
            fts_integrity_ok: fts_ok,
            fts_message,
            sqlite_integrity_ok: sqlite_ok == "ok",
            fts_state: state.to_owned(),
        })
    }

    pub fn page_count(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT count(*) FROM pages", [], |r| r.get(0))?)
    }

    pub fn stats(&self) -> Result<Stats> {
        let q = |sql: &str| -> Result<i64> { Ok(self.conn.query_row(sql, [], |r| r.get(0))?) };
        Ok(Stats {
            pages: q("SELECT count(*) FROM pages")?,
            pages_with_body: q("SELECT count(*) FROM pages WHERE body <> ''")?,
            pages_with_title: q("SELECT count(*) FROM pages WHERE title IS NOT NULL")?,
            attempts: q("SELECT count(*) FROM attempts")?,
            detail: self.meta("detail")?,
            fts_state: self.fts_state()?,
            sqlite_version: rusqlite::version(),
        })
    }

    /// Status and quality JSON of a page (codes and numbers only).
    pub fn page_status(&self, key: &str) -> Result<Option<(String, String)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT status, quality FROM pages WHERE page_key = ?1",
                [key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    /// Attempts for a page, oldest first: (outcome, status, error_code).
    pub fn attempts(&self, key: &str) -> Result<Vec<(String, String, Option<String>)>> {
        let mut st = self.conn.prepare(
            "SELECT outcome, status, error_code FROM attempts WHERE page_key = ?1 ORDER BY attempt_id",
        )?;
        let rows = st.query_map([key], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// The database connection, for tests that damage the index on purpose.
    #[doc(hidden)]
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// HIT/MISS of one page.
    pub fn page_hits(&self, key: &str, q: &str) -> Result<bool> {
        check_query(q)?;
        let row: Option<(Option<String>, String)> = self
            .conn
            .query_row(
                "SELECT title, body FROM pages WHERE page_key = ?1",
                [key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(row.is_some_and(|(t, b)| t.is_some_and(|t| t.contains(q)) || b.contains(q)))
    }

    pub fn search(&self, q: &str, mode: SearchMode) -> Result<SearchResult> {
        let n = check_query(q)?;
        let fallback = if n < MIN_FTS_CHARS {
            Some("short_query")
        } else if q.chars().any(char::is_control) {
            Some("control_character")
        } else if self.fts_state()? != FTS_OK {
            Some("index_not_verified")
        } else {
            None
        };
        match (mode, fallback) {
            (SearchMode::FullScan, _) => self.full_scan(q, n, fallback),
            (_, Some(reason)) => self.full_scan(q, n, Some(reason)),
            (SearchMode::Auto, None) => self.fts_candidates(q, n),
            (SearchMode::Verify, None) => {
                let fts = self.fts_candidates(q, n)?;
                let all = self.full_scan(q, n, None)?;
                let found = fts.page_ids();
                let missed = all
                    .hits
                    .iter()
                    .filter(|h| found.binary_search(&h.page_id).is_err())
                    .count();
                if missed > 0 || found.len() != all.hits.len() {
                    return Err(IndexError::FtsMissedPages(missed));
                }
                Ok(fts)
            }
        }
    }

    fn collect(
        &self,
        sql: &str,
        param: Option<&str>,
        q: &str,
        method: Method,
        n: usize,
        fallback: Option<&'static str>,
    ) -> Result<SearchResult> {
        let mut st = self.conn.prepare_cached(sql)?;
        let mut rows = match param {
            Some(p) => st.query([p])?,
            None => st.query([])?,
        };
        let mut hits = Vec::new();
        let mut checked = 0;
        while let Some(r) = rows.next()? {
            checked += 1;
            let title = r.get_ref(3)?.as_str_or_null()?;
            let body = r.get_ref(4)?.as_str()?;
            let in_title = title.is_some_and(|t| t.contains(q));
            let in_body = body.contains(q);
            if in_title || in_body {
                hits.push(Hit {
                    page_id: r.get(0)?,
                    page_key: r.get(1)?,
                    url: r.get(2)?,
                    title: title.map(str::to_owned),
                    in_title,
                    in_body,
                });
            }
        }
        Ok(SearchResult {
            method,
            fallback,
            query_chars: n,
            checked_pages: checked,
            hits,
        })
    }

    fn full_scan(&self, q: &str, n: usize, fallback: Option<&'static str>) -> Result<SearchResult> {
        self.collect(
            "SELECT page_id, page_key, url, title, body FROM pages ORDER BY page_id",
            None,
            q,
            Method::FullScan,
            n,
            fallback,
        )
    }

    fn fts_candidates(&self, q: &str, n: usize) -> Result<SearchResult> {
        let (sql, pattern) = match self.candidate_query {
            CandidateQuery::Glob => (
                "SELECT page_id, page_key, url, title, body FROM pages WHERE page_id IN (
                   SELECT rowid FROM pages_fts WHERE title GLOB ?1
                   UNION SELECT rowid FROM pages_fts WHERE body GLOB ?1)
                 ORDER BY page_id",
                glob_pattern(q),
            ),
            CandidateQuery::Like => (
                "SELECT page_id, page_key, url, title, body FROM pages WHERE page_id IN (
                   SELECT rowid FROM pages_fts WHERE title LIKE ?1 ESCAPE '\\'
                   UNION SELECT rowid FROM pages_fts WHERE body LIKE ?1 ESCAPE '\\')
                 ORDER BY page_id",
                like_pattern(q),
            ),
            CandidateQuery::MatchPhrase => (
                "SELECT page_id, page_key, url, title, body FROM pages WHERE page_id IN (
                   SELECT rowid FROM pages_fts WHERE pages_fts MATCH ?1)
                 ORDER BY page_id",
                match_phrase(q),
            ),
        };
        self.collect(sql, Some(&pattern), q, Method::FtsCandidates, n, None)
    }

    /// The FTS5 path alone (no fallback, no comparison), for the candidate method comparison.
    #[doc(hidden)]
    pub fn search_fts_only(&self, q: &str) -> Result<SearchResult> {
        let n = check_query(q)?;
        self.fts_candidates(q, n)
    }
}

fn outcome_str(o: Outcome) -> &'static str {
    match o {
        Outcome::Inserted => "inserted",
        Outcome::Updated => "updated",
        Outcome::KeptPrevious => "kept_previous",
        Outcome::NotRegistered => "not_registered",
    }
}
