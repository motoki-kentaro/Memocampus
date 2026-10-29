use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

mod dialog;
mod location;
mod manage;
mod search;
mod vault;

const SCHEMA_SQL: &str = "CREATE TABLE IF NOT EXISTS visits (event_id TEXT PRIMARY KEY, url TEXT NOT NULL, title TEXT NOT NULL, visited_at TEXT NOT NULL); CREATE TABLE IF NOT EXISTS tab_snapshots (event_id TEXT PRIMARY KEY, window_id INTEGER NOT NULL, tab_index INTEGER NOT NULL, url TEXT NOT NULL, title TEXT NOT NULL, pinned INTEGER NOT NULL, incognito INTEGER NOT NULL, recorded_at TEXT NOT NULL);";

#[derive(Debug, Deserialize)]
struct VisitEvent {
    id: String,
    url: String,
    title: String,
    #[serde(rename = "visitedAt")]
    visited_at: String,
}

#[derive(Debug, Deserialize)]
struct TabSnapshot {
    id: String,
    #[serde(rename = "windowId")]
    window_id: i64,
    #[serde(rename = "tabIndex")]
    tab_index: i64,
    url: String,
    title: String,
    pinned: bool,
    incognito: bool,
    #[serde(rename = "recordedAt")]
    recorded_at: String,
}

#[derive(Debug, Serialize)]
struct Ack<'a> {
    ok: bool,
    /// `null` only when the message carried no usable string id.
    #[serde(rename = "eventId")]
    event_id: Option<&'a str>,
    /// Set only on failure, so successful acknowledgements are unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'static str>,
}

impl<'a> Ack<'a> {
    fn from_result(event_id: Option<&'a str>, result: Result<(), &'static str>) -> Self {
        Self {
            ok: result.is_ok(),
            event_id,
            error: result.err(),
        }
    }
}

// Permanent rejections: resending the same event can never succeed, so the extension
// quarantines it instead of retrying. Vault errors (`VaultError::code`) are all temporary.
const INVALID_URL: &str = "invalid_url";
const INVALID_EVENT: &str = "invalid_event";
const INVALID_MESSAGE: &str = "invalid_message";
const UNKNOWN_TYPE: &str = "unknown_type";

/// Stores one queued event. Every outcome is reported, so a bad event cannot stall the queue.
fn record_event(
    session: &mut vault::Session,
    value: serde_json::Value,
) -> Result<(), &'static str> {
    match value.get("type").and_then(|value| value.as_str()) {
        Some("visit") => {
            let event: VisitEvent = serde_json::from_value(value).map_err(|_| INVALID_EVENT)?;
            if event.id.is_empty() {
                return Err(INVALID_EVENT);
            }
            if !is_recordable_url(&event.url) {
                return Err(INVALID_URL);
            }
            session
                .write(|db| db.execute("INSERT OR IGNORE INTO visits (event_id, url, title, visited_at) VALUES (?1, ?2, ?3, ?4)", params![event.id, event.url, event.title, event.visited_at]))
                .map_err(vault::VaultError::code)
        }
        Some("tab_snapshot") => {
            let event: TabSnapshot = serde_json::from_value(value).map_err(|_| INVALID_EVENT)?;
            if event.id.is_empty() {
                return Err(INVALID_EVENT);
            }
            if !is_recordable_url(&event.url) {
                return Err(INVALID_URL);
            }
            session
                .write(|db| db.execute("INSERT OR IGNORE INTO tab_snapshots (event_id, window_id, tab_index, url, title, pinned, incognito, recorded_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)", params![event.id, event.window_id, event.tab_index, event.url, event.title, event.pinned, event.incognito, event.recorded_at]))
                .map_err(vault::VaultError::code)
        }
        _ => Err(UNKNOWN_TYPE),
    }
}

fn is_recordable_url(raw: &str) -> bool {
    match url::Url::parse(raw) {
        Ok(parsed) => matches!(parsed.scheme(), "http" | "https"),
        Err(_) => false,
    }
}

fn read_message() -> io::Result<Option<Vec<u8>>> {
    let mut length = [0u8; 4];
    let mut stdin = io::stdin().lock();
    if stdin.read_exact(&mut length).is_err() {
        return Ok(None);
    }
    let size = u32::from_ne_bytes(length) as usize;
    if size > 64 * 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message too large",
        ));
    }
    let mut body = vec![0u8; size];
    stdin.read_exact(&mut body)?;
    Ok(Some(body))
}

fn write_ack(ack: &Ack<'_>) -> io::Result<()> {
    write_message(ack)
}

fn write_message<T: Serialize>(message: &T) -> io::Result<()> {
    let body =
        serde_json::to_vec(message).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let mut stdout = io::stdout().lock();
    stdout.write_all(&(body.len() as u32).to_ne_bytes())?;
    stdout.write_all(&body)?;
    stdout.flush()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // The vault is resolved from config.json and opened lazily; nothing is created implicitly.
    let mut session = vault::Session::new(vault::Environment::from_process());
    while let Some(body) = read_message()? {
        let value: serde_json::Value = match serde_json::from_slice(&body) {
            Ok(value) => value,
            Err(_) => {
                write_ack(&Ack::from_result(None, Err(INVALID_MESSAGE)))?;
                continue;
            }
        };
        match value.get("type").and_then(|value| value.as_str()) {
            Some("search_pages") => {
                let response = session
                    .read(|db| search::handle(db, &value))
                    .unwrap_or_else(|error| search::failure(&value, error.code()));
                write_message(&response)?;
            }
            Some("vault_status" | "vault_create" | "vault_open" | "vault_backup") => {
                write_message(&manage::handle(
                    &mut session,
                    &dialog::NativeDialogs,
                    &value,
                ))?;
            }
            // `visit`, `tab_snapshot`, and anything unrecognised.
            _ => {
                let event_id = value
                    .get("id")
                    .and_then(|id| id.as_str())
                    .map(str::to_owned);
                let result = record_event(&mut session, value);
                write_ack(&Ack::from_result(event_id.as_deref(), result))?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::is_recordable_url;
    use rusqlite::{params, Connection};

    #[test]
    fn only_web_urls_are_recordable() {
        assert!(is_recordable_url("https://example.test/path"));
        assert!(is_recordable_url("http://localhost:8080"));
        assert!(!is_recordable_url("chrome://settings"));
        assert!(!is_recordable_url("file:///C:/secret.txt"));
        assert!(!is_recordable_url("not a url"));
    }

    #[test]
    fn duplicate_event_id_is_idempotent() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE visits (event_id TEXT PRIMARY KEY, url TEXT, title TEXT, visited_at TEXT);").unwrap();
        let sql = "INSERT OR IGNORE INTO visits VALUES (?1, ?2, ?3, ?4)";
        let event = [
            "event-1",
            "https://example.test",
            "Example",
            "2026-10-30T00:00:00Z",
        ];
        db.execute(sql, params![event[0], event[1], event[2], event[3]])
            .unwrap();
        db.execute(sql, params![event[0], event[1], event[2], event[3]])
            .unwrap();
        let count: i64 = db
            .query_row("SELECT count(*) FROM visits", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }
}
