//! Rollback journal (PO decision, report XIV-14): file states after a normal end, after a crash
//! inside a transaction and after a crash just after a commit, recovery by SQLite's standard
//! hot-journal rollback, and atomicity of failed transactions. Synthetic text only.
//!
//! A crash is a child process (this test binary, running the ignored `crash_child` test) that
//! ends with `std::process::abort()`: no destructor, no rollback, no close.

use page_fulltext::index::{Index, IndexError, Method, Options, PageText, SearchMode};
use std::path::{Path, PathBuf};
use std::process::Command;

const CHILD_ENV: &str = "PAGE_INDEX_CRASH_CHILD";
const SIDE: [&str; 4] = ["", "-journal", "-wal", "-shm"];

fn fresh(name: &str) -> PathBuf {
    let d =
        PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("journal-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join(name);
    for s in SIDE {
        let _ = std::fs::remove_file(side(&p, s));
    }
    p
}

fn side(p: &Path, s: &str) -> PathBuf {
    PathBuf::from(format!("{}{s}", p.display()))
}

fn snapshot(p: &Path) -> Vec<Option<Vec<u8>>> {
    SIDE.iter()
        .map(|s| std::fs::read(side(p, s)).ok())
        .collect()
}

fn page(key: &str, body: &str) -> (String, String) {
    (key.to_owned(), body.to_owned())
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

/// A closed index with 20 committed pages (`committed-<i>`, each containing `確定済みの𩸽`).
fn committed_db(name: &str) -> PathBuf {
    let p = fresh(name);
    let mut ix = Index::create(&p, Options::default()).unwrap();
    for i in 0..20 {
        let (k, b) = page(
            &format!("committed-{i}"),
            &format!("確定済みの𩸽 その{i}。"),
        );
        put(&mut ix, &k, &b);
    }
    drop(ix);
    p
}

/// Runs `mode` on `db` in a child process that aborts at the end.
fn crash(mode: &str, db: &Path) {
    let out = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "crash_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, format!("{mode}|{}", db.display()))
        .output()
        .unwrap();
    assert!(!out.status.success(), "the child did not crash");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(&format!("child reached {mode}")),
        "child output: {stdout} {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The child side. Does nothing unless started by [`crash`].
#[test]
#[ignore]
fn crash_child() {
    let Ok(spec) = std::env::var(CHILD_ENV) else {
        return;
    };
    let (mode, db) = spec.split_once('|').unwrap();
    let db = Path::new(db);
    let mut ix = Index::open_existing(db, Options::default()).unwrap();
    match mode {
        // Many uncommitted pages with a tiny page cache, so that SQLite spills changed pages
        // into the database file before the commit (the journal then holds the originals).
        "mid_transaction" => {
            let c = ix.connection();
            c.execute_batch("PRAGMA cache_size = 2; BEGIN IMMEDIATE;")
                .unwrap();
            let body = "未確定の本文".repeat(2000);
            for i in 0..200 {
                c.execute(
                    "INSERT INTO pages(page_key, url, title, body, status, quality, registered_at, updated_at)
                     VALUES (?1, NULL, '未確定', ?2, 'ok', '{}', 0, 0)",
                    rusqlite::params![format!("uncommitted-{i}"), body],
                )
                .unwrap();
            }
            c.execute("DELETE FROM pages WHERE page_key = 'committed-0'", [])
                .unwrap();
        }
        // A committed registration, then the crash.
        "after_commit" => put(&mut ix, "after-commit", "確定後に異常終了した𩸽"),
        other => panic!("unknown mode {other}"),
    }
    println!("child reached {mode}");
    use std::io::Write;
    std::io::stdout().flush().unwrap();
    std::process::abort();
}

#[test]
fn j_crash_inside_a_transaction_is_rolled_back_by_sqlite() {
    let p = committed_db("mid-transaction.sqlite3");
    let before_crash = std::fs::read(&p).unwrap();
    crash("mid_transaction", &p);

    // What the crash left: a hot journal, and a database file that already holds uncommitted
    // pages (spilled); never a -wal or -shm file.
    let left = snapshot(&p);
    let journal = left[1].as_ref().expect("a -journal file was left");
    assert!(!journal.is_empty());
    assert_ne!(
        left[0].as_ref().unwrap(),
        &before_crash,
        "no page was spilled"
    );
    assert_eq!(left[2..], [None, None]);

    // Read-only: SQLite may not roll the journal back on a read-only connection; the open is
    // refused as needing recovery, and nothing is changed (the database, the journal).
    assert_eq!(
        Index::open_readonly(&p).err(),
        Some(IndexError::NeedsRecovery)
    );
    assert_eq!(snapshot(&p), left, "read-only open changed files");

    // Writable: SQLite's own hot-journal rollback restores the committed state.
    let mut ix = Index::open_existing(&p, Options::default()).unwrap();
    assert_eq!(
        snapshot(&p)[1..],
        [None, None, None],
        "journal rolled back and removed"
    );
    assert_eq!(
        std::fs::read(&p).unwrap(),
        before_crash,
        "byte-identical to before the crash"
    );
    assert_eq!(ix.page_count().unwrap(), 20);
    let r = ix.search("確定済みの𩸽", SearchMode::Verify).unwrap();
    assert_eq!((r.method, r.hits.len()), (Method::FtsCandidates, 20));
    assert!(ix
        .search("未確定の本文", SearchMode::Verify)
        .unwrap()
        .hits
        .is_empty());
    let report = ix.check().unwrap();
    assert!(report.fts_integrity_ok && report.sqlite_integrity_ok);
    // And it is usable afterwards.
    put(&mut ix, "after-recovery", "復旧後の𩸽");
    assert_eq!(ix.search("𩸽", SearchMode::Auto).unwrap().hits.len(), 21);
    drop(ix);
    assert_eq!(snapshot(&p)[1..], [None, None, None]);
    // The read-only open now works too.
    let ro = Index::open_readonly(&p).unwrap();
    assert_eq!(ro.page_count().unwrap(), 21);
}

#[test]
fn j_crash_after_a_commit_keeps_the_committed_page() {
    let p = committed_db("after-commit.sqlite3");
    crash("after_commit", &p);
    // A commit deletes the journal: nothing is left behind.
    assert_eq!(snapshot(&p)[1..], [None, None, None]);
    let before = snapshot(&p);
    let ro = Index::open_readonly(&p).unwrap();
    assert_eq!(
        ro.search("異常終了した𩸽", SearchMode::Verify)
            .unwrap()
            .hits
            .len(),
        1
    );
    assert_eq!(ro.page_count().unwrap(), 21);
    drop(ro);
    assert_eq!(snapshot(&p), before);
    let mut ix = Index::open_existing(&p, Options::default()).unwrap();
    assert!(ix.check().unwrap().fts_integrity_ok);
}

#[test]
fn j_hot_journal_of_another_database_is_not_touched() {
    // A generic SQLite database crashed inside a transaction: refused by the header check, so
    // SQLite never opens it and its hot journal is not rolled back by this PoC.
    let p = fresh("foreign-hot.sqlite3");
    {
        let c = rusqlite::Connection::open(&p).unwrap();
        c.execute_batch("CREATE TABLE t(x BLOB); INSERT INTO t VALUES (zeroblob(100000));")
            .unwrap();
    }
    // Simulate the crash by keeping a copy of the journal of an open transaction.
    {
        let c = rusqlite::Connection::open(&p).unwrap();
        c.execute_batch("PRAGMA cache_size = 2; BEGIN; UPDATE t SET x = zeroblob(200000);")
            .unwrap();
        // Uncommitted pages are spilled when the cache is this small.
        std::fs::copy(side(&p, "-journal"), side(&p, "-journal.copy")).unwrap();
        std::fs::copy(&p, side(&p, ".copy")).unwrap();
    }
    std::fs::rename(side(&p, "-journal.copy"), side(&p, "-journal")).unwrap();
    std::fs::rename(side(&p, ".copy"), &p).unwrap();
    let before = snapshot(&p);
    assert!(before[1].is_some());
    for e in [
        Index::open_existing(&p, Options::default()).err(),
        Index::open_readonly(&p).err(),
        Index::open_or_create(&p, Options::default()).err(),
    ] {
        assert!(matches!(e, Some(IndexError::NotAPageIndex(_))), "{e:?}");
    }
    assert_eq!(
        snapshot(&p),
        before,
        "the foreign database or its journal changed"
    );
}

#[test]
fn j_failed_transactions_on_a_file_keep_the_committed_data() {
    let p = committed_db("fault.sqlite3");
    let mut ix = Index::open_existing(&p, Options::default()).unwrap();
    for point in ["after_page_write", "before_commit"] {
        ix.fault = Some(point);
        for key in ["committed-3", "new-page"] {
            let e = ix
                .put_page(
                    &PageText {
                        key,
                        url: None,
                        title: Some("途中"),
                        body: "途中で失敗した本文",
                    },
                    "ok",
                    "{}",
                )
                .unwrap_err();
            assert_eq!(e, IndexError::InjectedFault(point));
            assert_eq!(snapshot(&p)[1..], [None, None, None], "{point}");
        }
    }
    ix.fault = Some("during_rebuild");
    assert!(ix.rebuild().is_err());
    ix.fault = None;
    assert_eq!(snapshot(&p)[1..], [None, None, None]);
    drop(ix);
    let mut ix = Index::open_existing(&p, Options::default()).unwrap();
    assert_eq!(ix.page_count().unwrap(), 20);
    assert!(ix
        .search("途中で失敗", SearchMode::Verify)
        .unwrap()
        .hits
        .is_empty());
    assert_eq!(
        ix.search("その3。", SearchMode::Verify).unwrap().hits.len(),
        1
    );
    assert_eq!(ix.attempts("committed-3").unwrap().len(), 1);
    assert!(ix.check().unwrap().fts_integrity_ok);
}
