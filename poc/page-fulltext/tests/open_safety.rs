//! Review fixes HIGH-1 and MEDIUM-2 (report XIV-13): which files the index opens, and what
//! opening them does to the file system. Synthetic databases only (never a Vault).
//!
//! Every refused file is compared byte for byte before and after, and must have gained no
//! `-wal`, `-shm` or `-journal` file.

use page_fulltext::index::{
    Detail, Index, IndexError, Method, Options, PageText, SearchMode, APPLICATION_ID,
};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

fn dir() -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("open-safety-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn fresh(name: &str) -> PathBuf {
    let p = dir().join(name);
    for s in ["", "-wal", "-shm", "-journal"] {
        let _ = std::fs::remove_file(side(&p, s));
    }
    p
}

fn side(p: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}{suffix}", p.display()))
}

/// Contents of the file and of its side files (None = absent).
fn snapshot(p: &Path) -> Vec<Option<Vec<u8>>> {
    ["", "-wal", "-shm", "-journal"]
        .iter()
        .map(|s| std::fs::read(side(p, s)).ok())
        .collect()
}

fn put(ix: &mut Index, key: &str, body: &str) {
    ix.put_page(
        &PageText {
            key,
            url: None,
            title: Some("題"),
            body,
        },
        "ok",
        "{}",
    )
    .unwrap();
}

/// A page-index database with two pages, closed.
fn poc_db(name: &str, detail: Detail) -> PathBuf {
    let p = fresh(name);
    let mut ix = Index::create(&p, Options { detail }).unwrap();
    put(&mut ix, "a", "今日は𩸽を焼いて食べた。");
    put(&mut ix, "b", "鮭を焼いた");
    drop(ix);
    p
}

/// Every way of opening refuses `p` and leaves it (and its side files) exactly as it was.
fn assert_refused(p: &Path, what: &str) -> IndexError {
    let before = snapshot(p);
    let e1 = Index::open_existing(p, Options::default())
        .err()
        .unwrap_or_else(|| panic!("{what}: open_existing accepted it"));
    let e2 = Index::open_readonly(p)
        .err()
        .unwrap_or_else(|| panic!("{what}: open_readonly accepted it"));
    let e3 = Index::open_or_create(p, Options::default())
        .err()
        .unwrap_or_else(|| panic!("{what}: open_or_create accepted it"));
    assert!(
        Index::create(p, Options::default()).is_err(),
        "{what}: create"
    );
    assert_eq!(
        snapshot(p),
        before,
        "{what}: the file or its side files changed"
    );
    assert_eq!(e1.code(), e2.code(), "{what}");
    assert_eq!(e1.code(), e3.code(), "{what}");
    e1
}

fn not_a_page_index(e: &IndexError) -> bool {
    matches!(e, IndexError::NotAPageIndex(_))
}

// ------------------------------------------------------------------ HIGH-1: foreign files

#[test]
fn o_generic_sqlite_databases_are_refused_unmodified() {
    // Rollback-journal and WAL databases of "another application" (the header check refuses
    // them before SQLite opens the file).
    for wal in [false, true] {
        let p = fresh(&format!("generic-{wal}.sqlite3"));
        {
            let c = Connection::open(&p).unwrap();
            if wal {
                c.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))
                    .unwrap();
            }
            c.execute_batch(
                "CREATE TABLE notes(id INTEGER PRIMARY KEY, body TEXT);
                 INSERT INTO notes(body) VALUES ('another application');",
            )
            .unwrap();
        }
        let e = assert_refused(&p, "generic database");
        assert!(not_a_page_index(&e), "{e}");
    }
}

#[test]
fn o_forged_and_incomplete_databases_are_refused_unmodified() {
    // Only `meta` with the current schema version (the check that the review defeated).
    let p = fresh("forged-meta.sqlite3");
    Connection::open(&p)
        .unwrap()
        .execute_batch(
            "CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT;
             INSERT INTO meta VALUES ('schema_version', '2'), ('schema_version_1', '1');
             CREATE TABLE notes(x);",
        )
        .unwrap();
    assert!(not_a_page_index(&assert_refused(&p, "schema_version only")));

    // This PoC's application id and nothing else (rollback journal, then WAL).
    for wal in [false, true] {
        let p = fresh(&format!("app-id-only-{wal}.sqlite3"));
        {
            let c = Connection::open(&p).unwrap();
            c.execute_batch(&format!(
                "PRAGMA application_id = {APPLICATION_ID}; CREATE TABLE notes(x);"
            ))
            .unwrap();
            if wal {
                c.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))
                    .unwrap();
            }
        }
        assert!(not_a_page_index(&assert_refused(&p, "application id only")));
    }

    // The application id and the meta rows of a real index, but none of its tables.
    let p = fresh("app-id-and-meta.sqlite3");
    Connection::open(&p)
        .unwrap()
        .execute_batch(&format!(
            "PRAGMA application_id = {APPLICATION_ID};
             CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT;
             INSERT INTO meta VALUES ('app', 'memocampus.page-fulltext.poc'), ('detail', 'none'),
               ('fts_state', 'ok'), ('schema_version', '2'), ('tokenizer', 'trigram case_sensitive 1');"
        ))
        .unwrap();
    assert!(not_a_page_index(&assert_refused(&p, "meta only")));

    // Real indexes with one part missing or changed.
    let damaged: &[(&str, &str)] = &[
        ("no-attempts", "DROP TABLE attempts;"),
        ("no-pages", "DROP TRIGGER pages_ai; DROP TRIGGER pages_ad; DROP TRIGGER pages_au; DROP TABLE pages;"),
        ("no-fts", "DROP TABLE pages_fts;"),
        ("no-trigger", "DROP TRIGGER pages_au;"),
        ("extra-column", "ALTER TABLE pages ADD COLUMN extra TEXT;"),
        ("extra-table", "CREATE TABLE extra(x);"),
        // Another FTS5 configuration: tokenizer without case_sensitive.
        (
            "other-tokenizer",
            "DROP TABLE pages_fts;
             CREATE VIRTUAL TABLE pages_fts USING fts5(title, body, content = 'pages',
               content_rowid = 'page_id', tokenize = 'trigram', detail = 'none');",
        ),
        // detail in meta does not match the FTS5 table.
        ("detail-mismatch", "UPDATE meta SET value = 'full' WHERE key = 'detail';"),
        ("meta-app", "UPDATE meta SET value = 'other.app' WHERE key = 'app';"),
        ("meta-extra", "INSERT INTO meta VALUES ('extra', 'x');"),
        ("meta-missing", "DELETE FROM meta WHERE key = 'fts_state';"),
        ("meta-fts-state", "UPDATE meta SET value = 'unknown' WHERE key = 'fts_state';"),
        ("fts-config", "INSERT INTO pages_fts_config(k, v) VALUES ('pgsz', 4096);"),
        ("other-app-id", "PRAGMA application_id = 1;"),
    ];
    // A complete index converted to WAL (XIV-14: only the rollback journal is used).
    let p = poc_db("damaged-wal-format.sqlite3", Detail::None);
    Connection::open(&p)
        .unwrap()
        .query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))
        .unwrap();
    assert_eq!(std::fs::read(&p).unwrap()[18..20], [2, 2]);
    assert!(not_a_page_index(&assert_refused(&p, "WAL format")));
    for (name, sql) in damaged {
        let p = poc_db(&format!("damaged-{name}.sqlite3"), Detail::None);
        Connection::open(&p).unwrap().execute_batch(sql).unwrap();
        let e = assert_refused(&p, name);
        assert!(not_a_page_index(&e), "{name}: {e}");
    }
}

#[test]
fn o_other_schema_versions_are_refused_unmodified() {
    for v in ["1", "3", ""] {
        let p = poc_db(&format!("version-{v}.sqlite3"), Detail::None);
        Connection::open(&p)
            .unwrap()
            .execute(
                "UPDATE meta SET value = ?1 WHERE key = 'schema_version'",
                [v],
            )
            .unwrap();
        let e = assert_refused(&p, "schema version");
        assert!(matches!(e, IndexError::SchemaMismatch(_)), "{v}: {e}");
    }
}

#[test]
fn o_files_that_are_not_sqlite_are_refused_unmodified() {
    let empty = fresh("empty.sqlite3");
    std::fs::write(&empty, b"").unwrap();
    assert!(not_a_page_index(&assert_refused(&empty, "empty file")));
    assert_eq!(std::fs::metadata(&empty).unwrap().len(), 0);

    let junk = fresh("junk.sqlite3");
    std::fs::write(&junk, vec![b'x'; 4096]).unwrap();
    assert!(not_a_page_index(&assert_refused(&junk, "not SQLite")));

    // SQLite magic, this application id, garbage after it.
    let mut fake = b"SQLite format 3\0".to_vec();
    fake.resize(4096, 0xA5);
    fake[68..72].copy_from_slice(&APPLICATION_ID.to_be_bytes());
    let corrupt = fresh("corrupt.sqlite3");
    std::fs::write(&corrupt, &fake).unwrap();
    assert!(not_a_page_index(&assert_refused(
        &corrupt,
        "corrupt SQLite"
    )));

    // A directory.
    let d = dir().join("a-directory.sqlite3");
    std::fs::create_dir_all(&d).unwrap();
    assert!(Index::open_existing(&d, Options::default()).is_err());
    assert!(Index::open_readonly(&d).is_err());
    assert!(Index::open_or_create(&d, Options::default()).is_err());
}

#[test]
fn o_missing_files_are_not_created() {
    let p = fresh("missing.sqlite3");
    assert_eq!(
        Index::open_existing(&p, Options::default()).err(),
        Some(IndexError::NotFound)
    );
    assert_eq!(Index::open_readonly(&p).err(), Some(IndexError::NotFound));
    assert_eq!(snapshot(&p), vec![None, None, None, None]);
    // A missing parent directory: no directory is created either.
    let q = dir().join("no-such-dir").join("x.sqlite3");
    assert!(Index::open_or_create(&q, Options::default()).is_err());
    assert!(!dir().join("no-such-dir").exists());
}

#[test]
fn o_valid_databases_open_and_create_refuses_existing_files() {
    for detail in [Detail::None, Detail::Full] {
        let p = poc_db(&format!("valid-{detail:?}.sqlite3"), detail);
        let mut ix = Index::open_existing(&p, Options::default()).unwrap();
        assert_eq!(
            ix.stats().unwrap().detail,
            format!("{detail:?}").to_lowercase()
        );
        assert_eq!(ix.search("𩸽", SearchMode::Auto).unwrap().hits.len(), 1);
        let r = ix.search("𩸽を焼", SearchMode::Verify).unwrap();
        assert_eq!((r.method, r.hits.len()), (Method::FtsCandidates, 1));
        assert!(ix.check().unwrap().fts_integrity_ok);
        put(&mut ix, "c", "三番目の𩸽");
        drop(ix);
        let before = snapshot(&p);
        assert_eq!(
            Index::create(&p, Options::default()).err(),
            Some(IndexError::AlreadyExists)
        );
        assert_eq!(snapshot(&p), before);
        let ix = Index::open_or_create(&p, Options::default()).unwrap();
        assert_eq!(ix.search("𩸽", SearchMode::Auto).unwrap().hits.len(), 2);
    }
    // The default configuration is detail=none (PO decision).
    let p = fresh("default.sqlite3");
    let ix = Index::open_or_create(&p, Options::default()).unwrap();
    assert_eq!(ix.stats().unwrap().detail, "none");
}

// ------------------------------------------------------------------ MEDIUM-2: read-only use

/// Header bytes 18–19: 1 = rollback journal, 2 = WAL.
fn journal_bytes(p: &Path) -> [u8; 2] {
    let b = std::fs::read(p).unwrap();
    [b[18], b[19]]
}

/// Rollback journal at all times (PO decision, XIV-14): never a `-wal` or `-shm` file, and no
/// `-journal` left between transactions, while open or after closing.
#[test]
fn m_writable_session_leaves_no_side_files() {
    let p = poc_db("closed.sqlite3", Detail::None);
    assert_eq!(snapshot(&p)[1..], [None, None, None]);
    assert_eq!(journal_bytes(&p), [1, 1], "rollback-journal format");
    let mut ix = Index::open_existing(&p, Options::default()).unwrap();
    let mode: String = ix
        .connection()
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    let sync: i64 = ix
        .connection()
        .query_row("PRAGMA synchronous", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        (mode.as_str(), sync),
        ("delete", 2),
        "SQLite defaults: DELETE, FULL"
    );
    let no_side_files = |when: &str| {
        assert_eq!(snapshot(&p)[1..], [None, None, None], "{when}");
        assert_eq!(journal_bytes(&p), [1, 1], "{when}");
    };
    put(&mut ix, "c", "書き込み中");
    no_side_files("after insert");
    put(&mut ix, "c", "更新後の本文");
    no_side_files("after update");
    assert!(ix.delete("c").unwrap());
    no_side_files("after delete");
    assert!(ix.check().unwrap().fts_integrity_ok);
    no_side_files("after check");
    assert!(ix.rebuild().unwrap().fts_integrity_ok);
    no_side_files("after rebuild");
    assert_eq!(
        ix.search("𩸽を焼", SearchMode::Verify).unwrap().hits.len(),
        1
    );
    drop(ix);
    no_side_files("after close");
}

#[test]
fn m_read_only_search_and_stats_change_nothing() {
    let p = poc_db("readonly.sqlite3", Detail::None);
    let before = snapshot(&p);
    {
        let mut ix = Index::open_readonly(&p).unwrap();
        assert!(ix.is_read_only());
        assert_eq!(ix.search("𩸽", SearchMode::Auto).unwrap().hits.len(), 1);
        let r = ix.search("𩸽を焼", SearchMode::Verify).unwrap();
        assert_eq!((r.method, r.hits.len()), (Method::FtsCandidates, 1));
        assert_eq!(ix.stats().unwrap().pages, 2);
        // Writes are refused before SQLite is asked.
        assert_eq!(ix.delete("a").err(), Some(IndexError::ReadOnly));
        assert_eq!(ix.check().err(), Some(IndexError::ReadOnly));
        assert_eq!(ix.rebuild().err(), Some(IndexError::ReadOnly));
        let e = ix
            .put_page(
                &PageText {
                    key: "x",
                    url: None,
                    title: None,
                    body: "x",
                },
                "ok",
                "{}",
            )
            .err();
        assert_eq!(e, Some(IndexError::ReadOnly));
        // And SQLite itself refuses them on this connection.
        assert!(ix.connection().execute("DELETE FROM pages", []).is_err());
        assert_eq!(snapshot(&p), before, "while open");
    }
    assert_eq!(
        snapshot(&p),
        before,
        "after close: file, -wal, -shm, -journal"
    );
}

#[test]
fn m_read_only_file_attribute() {
    let p = poc_db("readonly-attr.sqlite3", Detail::None);
    let before = snapshot(&p);
    let original = std::fs::metadata(&p).unwrap().permissions();
    let mut perm = original.clone();
    perm.set_readonly(true);
    std::fs::set_permissions(&p, perm).unwrap();
    let result = Index::open_readonly(&p).map(|ix| {
        (
            ix.search("𩸽", SearchMode::Auto).unwrap().hits.len(),
            ix.search("鮭を焼", SearchMode::Auto).unwrap().hits.len(),
        )
    });
    // Writable opening fails (the file cannot be written) without changing it.
    let writable = Index::open_existing(&p, Options::default()).err();
    let created = Index::open_or_create(&p, Options::default()).err();
    let after = snapshot(&p);
    std::fs::set_permissions(&p, original).unwrap();
    assert_eq!(result.unwrap(), (1, 1));
    assert_eq!(writable, Some(IndexError::ReadOnly));
    assert_eq!(created, Some(IndexError::ReadOnly));
    assert_eq!(after, before);
}

#[test]
fn m_read_only_while_a_writer_is_open() {
    let p = poc_db("concurrent.sqlite3", Detail::None);
    let mut w = Index::open_existing(&p, Options::default()).unwrap();
    put(&mut w, "c", "書き込み側の𩸽");
    // The reader sees the committed pages; no side file exists between transactions.
    assert_eq!(snapshot(&p)[1..], [None, None, None]);
    {
        let r = Index::open_readonly(&p).unwrap();
        assert_eq!(r.search("𩸽", SearchMode::Auto).unwrap().hits.len(), 2);
        assert_eq!(
            r.search("書き込み側", SearchMode::Verify)
                .unwrap()
                .hits
                .len(),
            1
        );
    }
    drop(w);
    assert_eq!(snapshot(&p)[1..], [None, None, None]);
}

#[test]
fn m_read_only_falls_back_when_the_index_is_not_verified() {
    let p = poc_db("unverified.sqlite3", Detail::None);
    Connection::open(&p)
        .unwrap()
        .execute(
            "UPDATE meta SET value = 'inconsistent' WHERE key = 'fts_state'",
            [],
        )
        .unwrap();
    let before = snapshot(&p);
    {
        let ix = Index::open_readonly(&p).unwrap();
        let r = ix.search("𩸽を焼", SearchMode::Auto).unwrap();
        assert_eq!(
            (r.method, r.fallback, r.hits.len()),
            (Method::FullScan, Some("index_not_verified"), 1)
        );
    }
    assert_eq!(snapshot(&p), before);
}
