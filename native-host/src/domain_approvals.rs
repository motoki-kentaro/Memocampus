//! Domains a person approved for `domainWildcard` rules the Public Suffix List restricts
//! (docs/issue-025-policy-engine.md, PSL override). Kept by the host alone in a small SQLite file of
//! its application directory, beside the policy and apart from every vault: one row per domain,
//! keyed by the canonical domain. Nothing about URLs or browsing is stored.
//!
//! Reading never creates the file. A row counts only if its value is exactly `true` and its key
//! names a canonical domain; anything else, and any failure to read, is no approval.
use crate::public_suffix::syntax_domain;
use rusqlite::{params, Connection, OpenFlags, TransactionBehavior};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const FILE_NAME: &str = "settings.sqlite3";
const KEY_PREFIX: &str = "domainWildcard.pslOverride.";
const APPROVED: &str = "true";
const BUSY_TIMEOUT: Duration = Duration::from_millis(500);
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS metadata (
    key        TEXT PRIMARY KEY NOT NULL,
    value      TEXT NOT NULL,
    updated_at TEXT NOT NULL
)";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Approval {
    /// Canonical: lower case, IDNA to ASCII, as `public_suffix::syntax_domain` writes it.
    pub domain: String,
    /// When it was approved, UTC, `2026-11-04T01:02:03.004Z`.
    pub approved_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalError {
    /// The file exists but could not be read as approvals.
    Unreadable,
    /// The file could not be written; nothing was approved or revoked.
    WriteFailed,
}

pub fn key_of(domain: &str) -> String {
    format!("{KEY_PREFIX}{domain}")
}

/// Whether `domain` is a canonical domain: `*.` and it is valid syntax and written the same.
pub fn is_canonical(domain: &str) -> bool {
    syntax_domain(&format!("*.{domain}")).is_some_and(|canonical| canonical == domain)
}

/// Every approval, by domain. A missing file is no approval; an unreadable one is an error, which
/// a caller judging a policy takes as no approval.
pub fn read(path: &Path) -> Result<Vec<Approval>, ApprovalError> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(ApprovalError::Unreadable),
        Ok(metadata) if !metadata.is_file() => return Err(ApprovalError::Unreadable),
        Ok(_) => {}
    }
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| ApprovalError::Unreadable)?;
    db.busy_timeout(BUSY_TIMEOUT)
        .map_err(|_| ApprovalError::Unreadable)?;
    rows(&db).map_err(|_| ApprovalError::Unreadable)
}

fn rows(db: &Connection) -> rusqlite::Result<Vec<Approval>> {
    let mut statement =
        db.prepare("SELECT key, value, updated_at FROM metadata WHERE substr(key, 1, ?1) = ?2")?;
    let found = statement.query_map(params![KEY_PREFIX.len(), KEY_PREFIX], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut approvals = BTreeMap::new();
    for row in found {
        let (key, value, approved_at) = row?;
        let domain = &key[KEY_PREFIX.len()..];
        if value == APPROVED && is_canonical(domain) {
            approvals.insert(domain.to_owned(), approved_at);
        }
    }
    Ok(approvals
        .into_iter()
        .map(|(domain, approved_at)| Approval {
            domain,
            approved_at,
        })
        .collect())
}

fn open_for_write(path: &Path) -> Result<Connection, ApprovalError> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| !metadata.is_file()) {
        return Err(ApprovalError::WriteFailed);
    }
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| ApprovalError::WriteFailed)?;
    db.busy_timeout(BUSY_TIMEOUT)
        .map_err(|_| ApprovalError::WriteFailed)?;
    db.execute_batch(SCHEMA)
        .map_err(|_| ApprovalError::WriteFailed)?;
    Ok(db)
}

/// Stores the approval of a canonical domain, committed before it returns. Approving it again
/// keeps the first time.
pub fn write(path: &Path, domain: &str, now: SystemTime) -> Result<Approval, ApprovalError> {
    if !is_canonical(domain) {
        return Err(ApprovalError::WriteFailed);
    }
    let mut db = open_for_write(path)?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| ApprovalError::WriteFailed)?;
    let key = key_of(domain);
    let kept: Option<(String, String)> = tx
        .query_row(
            "SELECT value, updated_at FROM metadata WHERE key = ?1",
            [&key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok();
    let approved_at = match kept {
        Some((value, at)) if value == APPROVED => at,
        _ => {
            let at = timestamp(now);
            tx.execute(
                "INSERT INTO metadata (key, value, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                params![key, APPROVED, at],
            )
            .map_err(|_| ApprovalError::WriteFailed)?;
            at
        }
    };
    tx.commit().map_err(|_| ApprovalError::WriteFailed)?;
    Ok(Approval {
        domain: domain.to_owned(),
        approved_at,
    })
}

/// Removes the approval of a domain, committed before it returns. False if there was none.
pub fn remove(path: &Path, domain: &str) -> Result<bool, ApprovalError> {
    if fs::symlink_metadata(path).is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound) {
        return Ok(false);
    }
    let mut db = open_for_write(path)?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| ApprovalError::WriteFailed)?;
    let removed = tx
        .execute("DELETE FROM metadata WHERE key = ?1", [key_of(domain)])
        .map_err(|_| ApprovalError::WriteFailed)?;
    tx.commit().map_err(|_| ApprovalError::WriteFailed)?;
    Ok(removed > 0)
}

/// UTC with milliseconds, as the extension writes its times.
pub fn timestamp(now: SystemTime) -> String {
    let millis = now
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);
    let (days, rem) = (millis.div_euclid(86_400_000), millis.rem_euclid(86_400_000));
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
    let secs = rem / 1000;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60,
        rem % 1000
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("memocampus-kv-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn db(&self) -> PathBuf {
            self.0.join(FILE_NAME)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    const AT: fn() -> SystemTime = || UNIX_EPOCH + Duration::from_millis(1_793_754_123_004);

    fn domains(path: &Path) -> Vec<String> {
        read(path).unwrap().into_iter().map(|a| a.domain).collect()
    }

    #[test]
    fn approvals_are_written_listed_and_removed_one_domain_at_a_time() {
        let tmp = Scratch::new();
        assert_eq!(read(&tmp.db()), Ok(Vec::new()), "no file: no approval");
        assert!(!tmp.db().exists(), "reading creates nothing");
        let first = write(&tmp.db(), "co.jp", AT()).unwrap();
        assert_eq!(first.approved_at, "2026-11-04T01:02:03.004Z");
        write(&tmp.db(), "example.newtld", AT() + Duration::from_secs(60)).unwrap();
        let again = write(&tmp.db(), "co.jp", AT() + Duration::from_secs(120)).unwrap();
        assert_eq!(again, first, "approving again keeps the first time");
        assert_eq!(domains(&tmp.db()), ["co.jp", "example.newtld"]);
        assert_eq!(remove(&tmp.db(), "co.jp"), Ok(true));
        assert_eq!(remove(&tmp.db(), "co.jp"), Ok(false));
        assert_eq!(domains(&tmp.db()), ["example.newtld"]);
        assert_eq!(remove(&tmp.0.join("none.sqlite3"), "co.jp"), Ok(false));
    }

    #[test]
    fn only_canonical_domains_are_written() {
        let tmp = Scratch::new();
        for bad in ["CO.JP", "*.co.jp", "co.jp.", " co.jp", "例え.jp", "a/b", ""] {
            assert_eq!(
                write(&tmp.db(), bad, AT()),
                Err(ApprovalError::WriteFailed),
                "{bad:?}"
            );
        }
        assert!(write(&tmp.db(), "xn--r8jz45g.jp", AT()).is_ok());
        assert!(!tmp.db().exists() || domains(&tmp.db()) == ["xn--r8jz45g.jp"]);
    }

    #[test]
    fn a_value_other_than_true_or_a_key_not_canonical_is_no_approval() {
        let tmp = Scratch::new();
        write(&tmp.db(), "co.jp", AT()).unwrap();
        let db = Connection::open(tmp.db()).unwrap();
        for (key, value) in [
            ("ne.jp", "TRUE"),
            ("or.jp", "1"),
            ("ac.jp", "yes"),
            ("go.jp", "true "),
            ("ed.jp", ""),
            ("gr.jp", "false"),
            ("CO.UK", "true"),
            ("*.com", "true"),
        ] {
            db.execute(
                "INSERT INTO metadata VALUES (?1, ?2, 'x')",
                params![key_of(key), value],
            )
            .unwrap();
        }
        db.execute("INSERT INTO metadata VALUES ('other.key', 'true', 'x')", [])
            .unwrap();
        drop(db);
        assert_eq!(domains(&tmp.db()), ["co.jp"]);
    }

    #[test]
    fn a_file_that_is_not_the_store_is_unreadable_and_never_written_over() {
        let tmp = Scratch::new();
        fs::write(tmp.db(), b"not a database at all, just bytes").unwrap();
        assert_eq!(read(&tmp.db()), Err(ApprovalError::Unreadable));
        assert_eq!(
            write(&tmp.db(), "co.jp", AT()),
            Err(ApprovalError::WriteFailed)
        );
        assert_eq!(
            fs::read(tmp.db()).unwrap(),
            b"not a database at all, just bytes"
        );
        let dir = Scratch::new();
        fs::create_dir(dir.db()).unwrap();
        assert_eq!(read(&dir.db()), Err(ApprovalError::Unreadable));
        assert_eq!(
            write(&dir.db(), "co.jp", AT()),
            Err(ApprovalError::WriteFailed)
        );
        let empty = Scratch::new();
        Connection::open(empty.db())
            .unwrap()
            .execute_batch("CREATE TABLE t (x)")
            .unwrap();
        assert_eq!(
            read(&empty.db()),
            Err(ApprovalError::Unreadable),
            "no table"
        );
    }

    #[test]
    fn a_write_that_cannot_take_the_database_approves_nothing() {
        let tmp = Scratch::new();
        write(&tmp.db(), "co.jp", AT()).unwrap();
        let holder = Connection::open(tmp.db()).unwrap();
        holder.execute_batch("BEGIN EXCLUSIVE").unwrap();
        assert_eq!(
            write(&tmp.db(), "ne.jp", AT()),
            Err(ApprovalError::WriteFailed)
        );
        assert_eq!(remove(&tmp.db(), "co.jp"), Err(ApprovalError::WriteFailed));
        holder.execute_batch("ROLLBACK").unwrap();
        drop(holder);
        assert_eq!(
            domains(&tmp.db()),
            ["co.jp"],
            "neither the write nor the removal happened"
        );
    }

    #[test]
    fn timestamps_are_utc_with_milliseconds() {
        assert_eq!(timestamp(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            timestamp(UNIX_EPOCH + Duration::from_millis(951_782_400_999)),
            "2000-02-29T00:00:00.999Z"
        );
    }
}
