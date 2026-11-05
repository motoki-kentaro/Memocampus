//! Temporary vaults and policies only. Every private value contains `CANARY`.

use super::*;
use crate::snapshot::{RawImageSave, RawVisit};
use crate::visible_set::like_pattern;
use memocampus_native_host::policy::{CompiledPolicy, Policy};
use memocampus_native_host::schema::{self, APP_ID, SCHEMA_VERSION as SUPPORTED_SCHEMA};
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

const ALL: SearchVisits = SearchVisits {
    query: "",
    from: None,
    to: None,
    limit: Some(MAX_LIMIT),
};

struct Fixture {
    dir: PathBuf,
    vault: PathBuf,
    app: PathBuf,
    store: VisibleStore,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn create_vault(path: &Path, app_id: i64, version: i64) {
    Connection::open(path)
        .unwrap()
        .execute_batch(&format!(
            "{} PRAGMA application_id = {app_id}; PRAGMA user_version = {version};",
            schema::create_sql()
        ))
        .unwrap();
}

/// Like the Native Host: rollback journal, 3 s busy timeout.
fn writer(path: &Path) -> Connection {
    let db = Connection::open(path).unwrap();
    db.busy_timeout(Duration::from_secs(3)).unwrap();
    db
}

fn visit(db: &Connection, id: &str, url: &str, title: &str, at: &str) {
    db.execute(
        "INSERT INTO visits VALUES (?1, ?2, ?3, ?4)",
        params![id, url, title, at],
    )
    .unwrap();
}

fn day(n: u32) -> String {
    format!("2026-10-{n:02}T00:00:00.000Z")
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("memocampus-visible-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let vault = dir.join("v.sqlite3");
        let app = dir.join("app");
        create_vault(&vault, APP_ID, SUPPORTED_SCHEMA);
        let store = VisibleStore::new(vault.clone(), app.clone()).unwrap();
        let fixture = Self {
            dir,
            vault,
            app,
            store,
        };
        fixture.policy_store().initialize().unwrap();
        fixture.add_rule(true, "a-layer.test");
        fixture.add_rule(false, "b-layer.test");
        fixture
    }

    fn policy_store(&self) -> PolicyStore {
        PolicyStore::new(self.app.clone()).unwrap()
    }

    /// `never_save` selects layer A, otherwise layer B.
    fn add_rule(&self, never_save: bool, rule: &str) {
        let store = self.policy_store();
        let current = store.load().unwrap();
        let mut next = current.policy.clone();
        let layer = if never_save {
            &mut next.layers.never_save
        } else {
            &mut next.layers.save_but_no_mcp
        };
        layer.rules.push(rule.into());
        store.update(&current, next).unwrap();
    }

    /// A public row, then one private row of each kind, all newer than the public row.
    fn seed(&self) {
        let db = writer(&self.vault);
        visit(&db, "pub-1", "https://one.public.test/", "One", &day(1));
        visit(&db, "pub-2", "https://two.public.test/", "Two", &day(2));
        for (id, url, title) in private_rows() {
            visit(&db, id, url, title, &day(20));
        }
    }

    fn urls(&self) -> Vec<String> {
        let page = self.store.search_visits(&ALL).unwrap();
        assert_clean(&page);
        page.items.into_iter().map(|item| item.url).collect()
    }

    fn fingerprint(&self) -> (String, std::time::SystemTime, Vec<String>) {
        let hash = Sha256::digest(fs::read(&self.vault).unwrap());
        let mut names: Vec<String> = fs::read_dir(&self.dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        (
            format!("{hash:x}"),
            fs::metadata(&self.vault).unwrap().modified().unwrap(),
            names,
        )
    }

    fn sidecars(&self) -> bool {
        ["-journal", "-wal", "-shm"].iter().any(|suffix| {
            let mut name = self.vault.as_os_str().to_owned();
            name.push(suffix);
            Path::new(&name).exists()
        })
    }

    fn lock_policy(&self) -> File {
        let lock = File::open(self.app.join("policy.lock")).unwrap();
        lock.try_lock().unwrap();
        lock
    }
}

fn private_rows() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        (
            "priv-a",
            "https://a-layer.test/CANARY-a",
            "CANARY title of layer A",
        ),
        (
            "priv-b",
            "https://b-layer.test/CANARY-b",
            "CANARY title of layer B",
        ),
        (
            "priv-cred",
            "https://public.test/?access_token=CANARY-token",
            "CANARY credential",
        ),
        (
            "priv-user",
            "https://user:CANARY-pw@public.test/",
            "CANARY userinfo",
        ),
        ("priv-bad", "not a url CANARY", "CANARY malformed"),
        ("priv-data", "data:text/plain,CANARY", "CANARY data"),
        ("priv-file", "file:///C:/CANARY.txt", "CANARY file"),
        (
            "priv-auth",
            "https://public.test/login/CANARY",
            "CANARY auth path",
        ),
    ]
}

fn assert_clean(value: &impl std::fmt::Debug) {
    let text = format!("{value:?}");
    assert!(!text.contains("CANARY"), "{text}");
    for id in [
        "priv-a",
        "priv-b",
        "priv-cred",
        "priv-user",
        "priv-bad",
        "priv-data",
    ] {
        assert!(!text.contains(id), "{text}");
    }
}

fn assert_failed<T: std::fmt::Debug>(result: Result<T, VisibleError>, expected: VisibleError) {
    let error = result.unwrap_err();
    assert_eq!(error, expected);
    assert_clean(&error);
    assert_clean(&error.code());
}

// --- Public and private rows --------------------------------------------------------------------

#[test]
fn only_public_visits_are_returned_with_their_titles() {
    let f = Fixture::new();
    f.seed();
    let page = f.store.search_visits(&ALL).unwrap();
    assert_clean(&page);
    assert_eq!(page.total, 2);
    assert!(!page.has_more);
    assert_eq!(page.items[0].url, "https://two.public.test/");
    assert_eq!(page.items[0].title, "Two");
    assert_eq!(page.items[0].visited_at, day(2));
    assert_eq!(page.items[1].url, "https://one.public.test/");
}

#[test]
fn searching_for_private_text_finds_nothing() {
    let f = Fixture::new();
    f.seed();
    for query in ["CANARY", "a-layer", "b-layer", "access_token", "layer"] {
        let page = f
            .store
            .search_visits(&SearchVisits {
                query,
                ..SearchVisits::default()
            })
            .unwrap();
        assert_eq!((page.total, page.items.len()), (0, 0), "{query}");
    }
}

#[test]
fn a_public_url_keeps_a_title_even_if_it_looks_secret_as_policy_is_url_based() {
    // Pins the documented limit (D7): titles are not judged, so this is not a safety guarantee.
    let f = Fixture::new();
    visit(
        &writer(&f.vault),
        "t",
        "https://public.test/",
        "secret in a title",
        &day(1),
    );
    assert_eq!(
        f.store.search_visits(&ALL).unwrap().items[0].title,
        "secret in a title"
    );
}

#[test]
fn rows_with_unusable_values_are_never_published() {
    let f = Fixture::new();
    let db = writer(&f.vault);
    visit(&db, "ok", "https://public.test/ok", "ok", &day(1));
    visit(&db, "x1", "https://public.test/CANARY1", "x", "yesterday");
    visit(
        &db,
        "x2",
        "https://public.test/CANARY2",
        "x",
        "2026-13-01T00:00:00.000Z",
    );
    visit(
        &db,
        "x3",
        "https://public.test/CANARY3",
        "x",
        "2026-02-29T00:00:00.000Z",
    );
    visit(
        &db,
        "x4",
        "https://public.test/CANARY4",
        "x",
        "2026-10-01T00:00:00Z",
    );
    db.execute_batch(
        "INSERT INTO visits VALUES ('b1', x'0043414e415259', 't', '2026-10-01T00:00:00.000Z');
         INSERT INTO visits VALUES ('b2', 'https://public.test/CANARY5', x'00', '2026-10-01T00:00:00.000Z');
         INSERT INTO visits VALUES ('b3', 'https://public.test/CANARY6', 't', x'00');
         INSERT INTO visits VALUES (NULL, 'https://public.test/CANARY7', 't', '2026-10-01T00:00:00.000Z');
         INSERT INTO visits VALUES ('b4', 'https://public.test/CANARY8', cast(x'ff' as text), '2026-10-01T00:00:00.000Z');",
    )
    .unwrap();
    assert_eq!(f.urls(), ["https://public.test/ok"]);
}

#[test]
fn the_gate_agrees_with_the_shared_policy_vectors() {
    let vectors: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/privacy/vectors.json")).unwrap();
    let key = [7u8; 32];
    let decisions = vectors["decisions"].as_array().unwrap();
    assert!(decisions.len() > 10);
    for vector in decisions {
        let policy = vector.get("policy").map_or_else(Policy::default, |p| {
            Policy::parse(&serde_json::to_vec(p).unwrap()).unwrap()
        });
        let row = RawVisit {
            event_id: "e".into(),
            url: vector["url"].as_str().unwrap().into(),
            title: "t".into(),
            visited_at: day(1),
        };
        let set = VisibleSet::build(vec![row], &compiled(&policy), &key).unwrap();
        let total = set.query("%", (i64::MIN, i64::MAX), 10).unwrap().total;
        assert_eq!(
            total == 1,
            vector["expected"] == "none",
            "{}",
            vector["name"]
        );
    }
}

// --- Queries work inside the public set ---------------------------------------------------------

#[test]
fn private_rows_do_not_change_totals_order_or_limits() {
    let f = Fixture::new();
    let db = writer(&f.vault);
    for n in 1..=5 {
        visit(
            &db,
            &format!("p{n}"),
            &format!("https://public.test/{n}"),
            "Page",
            &day(n),
        );
    }
    let requests = [
        ALL,
        SearchVisits {
            limit: Some(2),
            ..ALL
        },
        SearchVisits {
            query: "Page",
            limit: Some(3),
            ..ALL
        },
    ];
    let before: Vec<_> = requests
        .iter()
        .map(|request| f.store.search_visits(request).unwrap())
        .collect();
    let by_day = VisitsByDay {
        date: "2026-10-03",
        utc_offset: "+00:00",
        limit: None,
    };
    let day_before = f.store.get_visits_by_day(&by_day).unwrap();
    // Newer, older and same-time private rows, some with the searched word, on every position.
    for (n, (id, url, _)) in private_rows().into_iter().enumerate() {
        for at in [day(9), day(3), day(1)] {
            visit(&db, &format!("{id}-{at}"), url, "Page CANARY", &at);
        }
        visit(
            &db,
            &format!("{id}-{n}"),
            url,
            "Page CANARY",
            "2026-09-01T00:00:00.000Z",
        );
    }
    let after: Vec<_> = requests
        .iter()
        .map(|request| f.store.search_visits(request).unwrap())
        .collect();
    assert_eq!(before, after);
    assert_eq!(after[1].total, 5);
    assert_eq!(after[1].items.len(), 2);
    assert!(after[1].has_more);
    assert_eq!(f.store.get_visits_by_day(&by_day).unwrap(), day_before);
    assert_eq!(day_before.total, 1);
}

#[test]
fn limits_default_cap_and_refuse_bad_values() {
    let f = Fixture::new();
    let db = writer(&f.vault);
    db.execute_batch("BEGIN").unwrap();
    for n in 0..120 {
        visit(&db, &format!("p{n}"), "https://public.test/", "t", &day(1));
    }
    db.execute_batch("COMMIT").unwrap();
    let default = f.store.search_visits(&SearchVisits::default()).unwrap();
    assert_eq!((default.items.len(), default.total), (50, 120));
    assert!(default.has_more);
    assert_eq!(f.store.search_visits(&ALL).unwrap().items.len(), 100);
    let by_day = f
        .store
        .get_visits_by_day(&VisitsByDay {
            date: "2026-10-01",
            utc_offset: "+00:00",
            limit: None,
        })
        .unwrap();
    assert_eq!((by_day.items.len(), by_day.total), (50, 120));
    for limit in [0, MAX_LIMIT + 1, u32::MAX] {
        assert_failed(
            f.store.search_visits(&SearchVisits {
                limit: Some(limit),
                ..ALL
            }),
            VisibleError::InvalidArgument,
        );
    }
}

#[test]
fn query_text_is_literal_and_bound_as_data() {
    let f = Fixture::new();
    let db = writer(&f.vault);
    visit(
        &db,
        "1",
        "https://public.test/percent",
        "100% done",
        &day(1),
    );
    visit(&db, "2", "https://public.test/plain", "1000 done", &day(2));
    visit(&db, "3", "https://public.test/snake_case", "x", &day(3));
    visit(&db, "4", "https://public.test/snakeXcase", "x", &day(4));
    let search = |query| {
        f.store
            .search_visits(&SearchVisits {
                query,
                ..SearchVisits::default()
            })
            .unwrap()
    };
    assert_eq!(search("100%").total, 1);
    assert_eq!(search("snake_case").total, 1);
    assert_eq!(search("SNAKE_case").total, 1);
    assert_eq!(search("  plain ").total, 1);
    assert_eq!(search("') OR 1=1; DROP TABLE visible_visits; --").total, 0);
    assert_eq!(search("").total, 4);
    assert_eq!(like_pattern(r"a%b_c\d"), r"%a\%b\_c\\d%");
    let too_long = "あ".repeat(MAX_QUERY_CHARS + 1);
    assert_failed(
        f.store.search_visits(&SearchVisits {
            query: &too_long,
            ..SearchVisits::default()
        }),
        VisibleError::InvalidArgument,
    );
    assert_eq!(search(&"あ".repeat(MAX_QUERY_CHARS)).total, 0);
}

#[test]
fn date_range_is_half_open_and_validated() {
    let f = Fixture::new();
    let db = writer(&f.vault);
    for n in 1..=3 {
        visit(&db, &n.to_string(), "https://public.test/", "t", &day(n));
    }
    let (from, to) = (day(1), day(3));
    let page = f
        .store
        .search_visits(&SearchVisits {
            from: Some(&from),
            to: Some(&to),
            ..ALL
        })
        .unwrap();
    assert_eq!(page.total, 2);
    assert_eq!(page.items[0].visited_at, day(2));
    for bad in ["2026-10-01", "2026-10-01T00:00:00Z", "tomorrow", ""] {
        assert_failed(
            f.store.search_visits(&SearchVisits {
                from: Some(bad),
                ..ALL
            }),
            VisibleError::InvalidArgument,
        );
    }
    assert_failed(
        f.store.search_visits(&SearchVisits {
            from: Some(&to),
            to: Some(&from),
            ..ALL
        }),
        VisibleError::InvalidArgument,
    );
}

#[test]
fn a_local_day_uses_the_explicit_offset() {
    let f = Fixture::new();
    let db = writer(&f.vault);
    for (id, at) in [
        ("before", "2026-10-30T14:59:59.999Z"),
        ("first", "2026-10-30T15:00:00.000Z"),
        ("last", "2026-10-31T14:59:59.999Z"),
        ("after", "2026-10-31T15:00:00.000Z"),
    ] {
        visit(&db, id, &format!("https://public.test/{id}"), "t", at);
    }
    let urls = |date, utc_offset| -> Vec<String> {
        f.store
            .get_visits_by_day(&VisitsByDay {
                date,
                utc_offset,
                limit: None,
            })
            .unwrap()
            .items
            .into_iter()
            .map(|item| item.url)
            .collect()
    };
    assert_eq!(
        urls("2026-10-31", "+09:00"),
        ["https://public.test/last", "https://public.test/first"]
    );
    assert_eq!(
        urls("2026-10-30", "-05:00"),
        ["https://public.test/first", "https://public.test/before"]
    );
    for (date, offset) in [
        ("2026-10-31", "Asia/Tokyo"),
        ("2026-10-31", "+15:00"),
        ("2026-10-31", "-13:00"),
        ("2026-10-31", "+09:60"),
        ("2026-10-31", "09:00"),
        ("2026-10-31", ""),
        ("2026-02-30", "+09:00"),
        ("2026/10/31", "+09:00"),
        ("26-10-31", "+09:00"),
    ] {
        assert_failed(
            f.store.get_visits_by_day(&VisitsByDay {
                date,
                utc_offset: offset,
                limit: None,
            }),
            VisibleError::InvalidArgument,
        );
    }
}

#[test]
fn timestamps_parse_exactly_and_match_known_instants() {
    assert_eq!(time::parse_timestamp("1970-01-01T00:00:00.000Z"), Some(0));
    assert_eq!(
        time::parse_timestamp("2000-03-01T00:00:00.001Z"),
        Some(951_868_800_001)
    );
    assert_eq!(
        time::parse_timestamp("2024-02-29T23:59:59.999Z"),
        Some(1_709_251_199_999)
    );
    assert!(time::parse_timestamp("1969-12-31T23:59:59.999Z").is_some_and(|ms| ms == -1));
    for bad in [
        "2023-02-29T00:00:00.000Z",
        "2026-10-01T24:00:00.000Z",
        "2026-10-01T00:60:00.000Z",
        "2026-10-01T00:00:60.000Z",
        "2026-10-01 00:00:00.000Z",
        "2026-10-01T00:00:00.000+",
        "٢٠٢٦-10-01T00:00:00.000Z",
    ] {
        assert_eq!(time::parse_timestamp(bad), None, "{bad}");
    }
}

// --- External IDs -------------------------------------------------------------------------------

#[test]
fn external_ids_are_opaque_stable_per_store_and_expose_no_internal_id() {
    let f = Fixture::new();
    let db = writer(&f.vault);
    for n in 1..=9 {
        visit(
            &db,
            &format!("event-{n}"),
            "https://public.test/",
            "t",
            &day(n),
        );
    }
    let ids = |store: &VisibleStore| -> Vec<String> {
        store
            .search_visits(&ALL)
            .unwrap()
            .items
            .into_iter()
            .map(|item| item.id)
            .collect()
    };
    let first = ids(&f.store);
    assert_eq!(first, ids(&f.store), "stable within one store");
    for id in &first {
        assert!(id.starts_with("visit_") && id.len() == 38, "{id}");
        assert!(id[6..].bytes().all(|b| b.is_ascii_hexdigit()), "{id}");
        assert!(!id.contains("event"));
    }
    let unique: std::collections::HashSet<_> = first.iter().collect();
    assert_eq!(unique.len(), 9);
    // Adding or removing other rows, private ones included, does not move an ID.
    visit(&db, "priv", "https://a-layer.test/CANARY", "t", &day(10));
    assert_eq!(first, ids(&f.store));
    db.execute("DELETE FROM visits WHERE event_id = 'event-1'", [])
        .unwrap();
    assert_eq!(first[..8], ids(&f.store)[..]);
    // Another store (a restarted server) cannot correlate them.
    let other = VisibleStore::new(f.vault.clone(), f.app.clone()).unwrap();
    assert!(ids(&other).iter().all(|id| !first.contains(id)));
}

// --- Vault safety -------------------------------------------------------------------------------

#[test]
fn a_missing_vault_is_an_error_and_nothing_is_created() {
    let f = Fixture::new();
    fs::remove_file(&f.vault).unwrap();
    assert_failed(f.store.search_visits(&ALL), VisibleError::VaultUnavailable);
    assert!(!f.vault.exists());
    let nested = f.dir.join("no-such-dir").join("v.sqlite3");
    let store = VisibleStore::new(nested.clone(), f.app.clone()).unwrap();
    assert_failed(store.search_visits(&ALL), VisibleError::VaultUnavailable);
    assert!(!nested.parent().unwrap().exists());
    let directory = VisibleStore::new(f.dir.clone(), f.app.clone()).unwrap();
    assert_failed(
        directory.search_visits(&ALL),
        VisibleError::VaultUnavailable,
    );
}

#[test]
fn a_relative_or_network_path_is_refused_before_any_access() {
    assert_eq!(
        location::resolve_local(Path::new("v.sqlite3")),
        Err(VisibleError::VaultUnavailable)
    );
    #[cfg(windows)]
    for unc in [
        r"\\server\share\CANARY.sqlite3",
        r"\\?\UNC\server\share\CANARY.sqlite3",
        r"\\.\pipe\CANARY",
    ] {
        assert_eq!(
            location::resolve_local(Path::new(unc)),
            Err(VisibleError::VaultUnsupported),
            "{unc}"
        );
    }
}

#[cfg(windows)]
#[test]
fn only_a_fixed_drive_is_accepted() {
    use location::{volume_of_drive_type, Volume};
    // GetDriveTypeW: unknown, no root, removable, fixed, remote, CD-ROM, RAM disk.
    for (drive_type, expected) in [
        (0, Volume::Other),
        (1, Volume::Other),
        (2, Volume::Other),
        (3, Volume::Fixed),
        (4, Volume::Other),
        (5, Volume::Other),
        (6, Volume::Other),
    ] {
        assert_eq!(volume_of_drive_type(drive_type), expected, "{drive_type}");
    }
}

#[test]
fn every_other_schema_version_or_application_id_is_refused() {
    let f = Fixture::new();
    for (name, app_id, version) in [
        // Schema 3 is reached in place from older vaults, so it proves nothing about old visits.
        ("tagged-3", APP_ID, 3),
        ("tagged-2", APP_ID, 2),
        ("tagged-1", APP_ID, 1),
        ("legacy-0", 0, 0),
        ("untagged-3", 0, 3),
        ("untagged-4", 0, SUPPORTED_SCHEMA),
        ("foreign-4", 42, SUPPORTED_SCHEMA),
        ("newer", APP_ID, SUPPORTED_SCHEMA + 1),
    ] {
        let path = f.dir.join(format!("{name}.sqlite3"));
        create_vault(&path, app_id, version);
        visit(
            &writer(&path),
            "old",
            "https://public.test/CANARY-unidentifiable",
            "CANARY maybe incognito",
            &day(1),
        );
        let store = VisibleStore::new(path, f.app.clone()).unwrap();
        assert_failed(store.search_visits(&ALL), VisibleError::VaultSchemaMismatch);
    }
}

#[test]
fn another_schema_shape_is_refused_even_at_version_4() {
    let f = Fixture::new();
    let flag = "mcp_public INTEGER NOT NULL CHECK (mcp_public IN (0, 1))";
    let visits = "CREATE TABLE visits (event_id TEXT PRIMARY KEY, url TEXT NOT NULL, title TEXT NOT NULL, visited_at TEXT NOT NULL);";
    for (name, from, to) in [
        // The flag: exists, INTEGER, NOT NULL, no DEFAULT, CHECK.
        ("no-flag", ",\n  mcp_public INTEGER NOT NULL CHECK (mcp_public IN (0, 1))", ""),
        ("flag-nullable", flag, "mcp_public INTEGER CHECK (mcp_public IN (0, 1))"),
        ("flag-text", flag, "mcp_public TEXT NOT NULL CHECK (mcp_public IN (0, 1))"),
        ("flag-default", flag, "mcp_public INTEGER NOT NULL DEFAULT 0 CHECK (mcp_public IN (0, 1))"),
        ("flag-default-1", flag, "mcp_public INTEGER NOT NULL DEFAULT 1 CHECK (mcp_public IN (0, 1))"),
        ("flag-no-check", flag, "mcp_public INTEGER NOT NULL"),
        ("flag-wide-check", flag, "mcp_public INTEGER NOT NULL CHECK (mcp_public IN (0, 1, 2))"),
        // Visits.
        ("missing", visits, "CREATE TABLE visits (event_id TEXT PRIMARY KEY, url TEXT NOT NULL, title TEXT NOT NULL);"),
        ("extra", visits, "CREATE TABLE visits (event_id TEXT PRIMARY KEY, url TEXT NOT NULL, title TEXT NOT NULL, visited_at TEXT NOT NULL, incognito INTEGER);"),
        ("nullable", visits, "CREATE TABLE visits (event_id TEXT PRIMARY KEY, url TEXT, title TEXT NOT NULL, visited_at TEXT NOT NULL);"),
        ("typed", visits, "CREATE TABLE visits (event_id TEXT PRIMARY KEY, url BLOB NOT NULL, title TEXT NOT NULL, visited_at TEXT NOT NULL);"),
        ("view", visits, "CREATE TABLE t (event_id TEXT PRIMARY KEY, url TEXT NOT NULL, title TEXT NOT NULL, visited_at TEXT NOT NULL); CREATE VIEW visits AS SELECT * FROM t;"),
    ] {
        let sql = schema::create_sql();
        assert!(sql.contains(from), "{name}");
        let path = f.dir.join(format!("{name}.sqlite3"));
        Connection::open(&path)
            .unwrap()
            .execute_batch(&sql.replace(from, to))
            .unwrap();
        let store = VisibleStore::new(path, f.app.clone()).unwrap();
        assert_failed(store.search_visits(&ALL), VisibleError::VaultSchemaMismatch);
    }
}

#[test]
fn a_schema_3_vault_relabelled_as_4_is_refused_and_its_rows_stay_hidden() {
    let f = Fixture::new();
    let path = f.dir.join("relabelled.sqlite3");
    let db = Connection::open(&path).unwrap();
    db.execute_batch(&format!(
        "{}; PRAGMA application_id = {APP_ID}; PRAGMA user_version = {SUPPORTED_SCHEMA};",
        schema::create_sql().replace(
            ",\n  mcp_public INTEGER NOT NULL CHECK (mcp_public IN (0, 1))",
            ""
        )
    ))
    .unwrap();
    visit(
        &db,
        "old",
        "https://public.test/CANARY-old",
        "CANARY",
        &day(1),
    );
    drop(db);
    let before = fs::read(&path).unwrap();
    let store = VisibleStore::new(path.clone(), f.app.clone()).unwrap();
    assert_failed(store.search_visits(&ALL), VisibleError::VaultSchemaMismatch);
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn page_metadata_never_reaches_a_reader_and_a_vault_without_it_is_refused() {
    // Issue #22: VisibleStore reads no metadata, for public URLs or private ones.
    let f = Fixture::new();
    f.seed();
    let before = f.store.search_visits(&ALL).unwrap();
    let db = writer(&f.vault);
    let rows = db
        .prepare("SELECT event_id, url FROM visits")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    for (event_id, url) in rows {
        db.execute(
            "INSERT INTO page_metadata (url, event_id, title, description, og_title, og_site_name, og_image_url, favicon_url, captured_at) VALUES (?1, ?2, 'CANARY meta', 'CANARY desc', 'CANARY og', 'CANARY site', 'https://a-layer.test/CANARY.png', 'https://b-layer.test/CANARY.ico', '2026-11-05T00:00:00.000Z')",
            params![url, event_id],
        )
        .unwrap();
    }
    let after = f.store.search_visits(&ALL).unwrap();
    assert_clean(&after);
    assert_eq!(format!("{after:?}"), format!("{before:?}"));
    let found = f
        .store
        .search_visits(&SearchVisits {
            query: "CANARY",
            ..ALL
        })
        .unwrap();
    assert!(found.items.is_empty());

    // A vault shaped like schema 4 (no page_metadata) is not the current schema.
    let sql = schema::create_sql();
    let start = sql.find(" CREATE TABLE page_metadata").unwrap();
    let end = start + sql[start..].find(");").unwrap() + 2;
    let path = f.dir.join("schema-4-shape.sqlite3");
    Connection::open(&path)
        .unwrap()
        .execute_batch(&format!("{}{}", &sql[..start], &sql[end..]))
        .unwrap();
    let store = VisibleStore::new(path, f.app.clone()).unwrap();
    assert_failed(store.search_visits(&ALL), VisibleError::VaultSchemaMismatch);
}

#[test]
fn a_vault_with_a_public_flag_set_still_answers_visits_only_from_the_visits_gate() {
    let f = Fixture::new();
    f.seed();
    let db = writer(&f.vault);
    db.execute_batch(
        "INSERT INTO image_blobs (sha256, mime_type, byte_length, width, height, created_at, last_saved_at, data) VALUES (printf('%064d', 1), 'image/png', 1, 1, 1, 1, 1, x'00');
         INSERT INTO saved_images (save_id, blob_id, image_url, page_url, saved_at, incognito, mcp_public) VALUES ('s', 1, 'https://a-layer.test/CANARY.png', 'https://b-layer.test/CANARY', 1, 0, 1);",
    )
    .unwrap();
    assert_eq!(f.urls().len(), 2);
}

#[test]
fn non_databases_and_damaged_files_fail_closed() {
    let f = Fixture::new();
    f.seed();
    let store_for = |path: PathBuf| VisibleStore::new(path, f.app.clone()).unwrap();

    let empty = f.dir.join("empty.sqlite3");
    fs::write(&empty, b"").unwrap();
    assert_failed(
        store_for(empty.clone()).search_visits(&ALL),
        VisibleError::VaultSchemaMismatch,
    );
    assert_eq!(fs::metadata(&empty).unwrap().len(), 0);

    let text = f.dir.join("text.sqlite3");
    fs::write(&text, "not a database CANARY ".repeat(50)).unwrap();
    assert_failed(
        store_for(text).search_visits(&ALL),
        VisibleError::VaultSchemaMismatch,
    );

    // A file that claims to be SQLite in its header only.
    let fake = f.dir.join("fake.sqlite3");
    let mut bytes = fs::read(&f.vault).unwrap();
    let page = usize::from(u16::from_be_bytes([bytes[16], bytes[17]]));
    for byte in &mut bytes[100..] {
        *byte = 0xAB;
    }
    fs::write(&fake, &bytes).unwrap();
    assert_failed(
        store_for(fake).search_visits(&ALL),
        VisibleError::VaultCorrupt,
    );

    // The visits table's pages overwritten, the schema page intact.
    let damaged = f.dir.join("damaged.sqlite3");
    let mut bytes = fs::read(&f.vault).unwrap();
    for byte in &mut bytes[page..] {
        *byte = 0xAB;
    }
    fs::write(&damaged, &bytes).unwrap();
    assert_failed(
        store_for(damaged).search_visits(&ALL),
        VisibleError::VaultCorrupt,
    );

    // Cut short: the header promises more pages than the file has.
    let truncated = f.dir.join("truncated.sqlite3");
    let bytes = fs::read(&f.vault).unwrap();
    fs::write(&truncated, &bytes[..page]).unwrap();
    assert_failed(
        store_for(truncated).search_visits(&ALL),
        VisibleError::VaultCorrupt,
    );
}

#[test]
fn wal_vaults_are_refused_without_leaving_sidecars() {
    let f = Fixture::new();
    f.seed();
    {
        let db = Connection::open(&f.vault).unwrap();
        let mode: String = db
            .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
    }
    assert!(
        !f.sidecars(),
        "SQLite removes them when the last connection closes"
    );
    let before = f.fingerprint();
    assert_failed(f.store.search_visits(&ALL), VisibleError::VaultUnsupported);
    assert_eq!(f.fingerprint(), before);
    assert!(!f.sidecars());
}

#[test]
fn a_leftover_wal_file_is_refused() {
    let f = Fixture::new();
    f.seed();
    let mut wal = f.vault.as_os_str().to_owned();
    wal.push("-wal");
    fs::write(&wal, b"leftover").unwrap();
    assert_failed(f.store.search_visits(&ALL), VisibleError::VaultUnsupported);
    assert_eq!(fs::read(&wal).unwrap(), b"leftover");
}

/// Each check also stands alone: the header can turn to WAL between the raw check and the open, and
/// SQLite itself would enter WAL mode on seeing a `-wal` file beside a rollback-journal header.
#[test]
fn the_wal_checks_hold_independently() {
    let f = Fixture::new();
    f.seed();
    let db = Connection::open(&f.vault).unwrap();
    db.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))
        .unwrap();
    assert_eq!(
        snapshot::check_schema(&db),
        Err(VisibleError::VaultUnsupported)
    );
    drop(db);

    let other = Fixture::new();
    other.seed();
    let mut wal = other.vault.as_os_str().to_owned();
    wal.push("-wal");
    fs::write(&wal, b"leftover").unwrap();
    assert_eq!(
        snapshot::check_file(&other.vault),
        Err(VisibleError::VaultUnsupported)
    );
}

#[test]
fn reads_never_modify_the_vault_or_policy() {
    let f = Fixture::new();
    f.seed();
    let policy = fs::read(f.app.join("policy.json")).unwrap();
    let before = f.fingerprint();
    for _ in 0..3 {
        f.store.search_visits(&ALL).unwrap();
        f.store
            .get_visits_by_day(&VisitsByDay {
                date: "2026-10-01",
                utc_offset: "+09:00",
                limit: None,
            })
            .unwrap();
    }
    assert_eq!(f.fingerprint(), before);
    assert_eq!(fs::read(f.app.join("policy.json")).unwrap(), policy);
    let names = fs::read_dir(&f.app).unwrap().count();
    assert_eq!(names, 3, "policy.json, policy.initialized, policy.lock");
}

#[test]
fn all_selects_see_one_committed_state() {
    let f = Fixture::new();
    f.seed();
    let late = writer(&f.vault);
    late.busy_timeout(Duration::ZERO).unwrap();
    let mut blocked = None;
    let page = f
        .store
        .search_visits_staged(&ALL, &mut |stage| {
            if stage == Stage::SchemaChecked {
                late.execute_batch("BEGIN IMMEDIATE").unwrap();
                visit(&late, "late", "https://public.test/late", "t", &day(9));
                blocked = Some(late.execute_batch("COMMIT").is_err());
                late.execute_batch("ROLLBACK").unwrap();
            }
        })
        .unwrap();
    assert_eq!(blocked, Some(true), "the snapshot holds the shared lock");
    assert_eq!(page.total, 2);
    // The vault is free again once this read's connection is closed.
    let free = writer(&f.vault);
    free.busy_timeout(Duration::ZERO).unwrap();
    let mut wrote = false;
    let page = f
        .store
        .search_visits_staged(&ALL, &mut |stage| {
            if stage == Stage::VaultClosed {
                visit(&free, "after", "https://public.test/after", "t", &day(9));
                wrote = true;
            }
        })
        .unwrap();
    assert!(wrote, "no lock is held while rows are judged");
    assert_eq!(page.total, 2, "this read's snapshot was already taken");
    assert_eq!(f.store.search_visits(&ALL).unwrap().total, 3);
}

#[test]
fn concurrent_host_writes_succeed_and_no_read_sees_part_of_a_transaction() {
    let f = Fixture::new();
    let stop = Arc::new(AtomicBool::new(false));
    let worker = {
        let (vault, stop) = (f.vault.clone(), stop.clone());
        std::thread::spawn(move || {
            let db = writer(&vault);
            let mut batches = 0u32;
            while !stop.load(Ordering::Relaxed) {
                db.execute_batch("BEGIN IMMEDIATE").unwrap();
                for part in 0..2 {
                    visit(
                        &db,
                        &format!("{batches}-{part}"),
                        "https://public.test/",
                        "batch",
                        &day(1),
                    );
                }
                db.execute_batch("COMMIT").unwrap();
                batches += 1;
            }
            batches
        })
    };
    let mut reads = 0;
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        match f.store.search_visits(&ALL) {
            Ok(page) => {
                assert_eq!(page.total % 2, 0, "half a transaction");
                reads += 1;
            }
            Err(error) => assert_eq!(error, VisibleError::VaultUnavailable),
        }
    }
    stop.store(true, Ordering::Relaxed);
    let batches = worker.join().expect("a host write failed");
    assert!(reads > 0 && batches > 0, "{reads} reads, {batches} batches");
}

#[test]
fn an_open_write_transaction_shows_the_committed_state() {
    let f = Fixture::new();
    f.seed();
    let db = writer(&f.vault);
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    visit(
        &db,
        "pending",
        "https://public.test/CANARY-pending",
        "t",
        &day(9),
    );
    assert_eq!(f.store.search_visits(&ALL).unwrap().total, 2);
    db.execute_batch("ROLLBACK").unwrap();
}

#[test]
fn an_exclusive_lock_is_an_error_without_partial_data() {
    let f = Fixture::new();
    f.seed();
    let db = writer(&f.vault);
    db.execute_batch("BEGIN EXCLUSIVE").unwrap();
    assert_failed(f.store.search_visits(&ALL), VisibleError::VaultUnavailable);
    db.execute_batch("ROLLBACK").unwrap();
    assert_eq!(f.store.search_visits(&ALL).unwrap().total, 2);
}

#[test]
fn a_hot_journal_is_an_error_and_the_vault_stays_untouched() {
    let f = Fixture::new();
    f.seed();
    let source = f.dir.join("source.sqlite3");
    fs::copy(&f.vault, &source).unwrap();
    let crashed = writer(&source);
    crashed
        .execute_batch("PRAGMA cache_size = 1; BEGIN; UPDATE visits SET title = title || 'X';")
        .unwrap();
    for n in 0..3000 {
        visit(
            &crashed,
            &format!("h{n}"),
            "https://public.test/CANARY-hot",
            "t",
            &day(9),
        );
    }
    let hot = f.dir.join("hot.sqlite3");
    let journal = f.dir.join("hot.sqlite3-journal");
    fs::copy(&source, &hot).unwrap();
    fs::copy(f.dir.join("source.sqlite3-journal"), &journal).unwrap();
    crashed.execute_batch("ROLLBACK").unwrap();
    let (vault_bytes, journal_bytes) = (fs::read(&hot).unwrap(), fs::read(&journal).unwrap());
    let store = VisibleStore::new(hot.clone(), f.app.clone()).unwrap();
    assert_failed(store.search_visits(&ALL), VisibleError::VaultUnavailable);
    assert_eq!(fs::read(&hot).unwrap(), vault_bytes);
    assert_eq!(fs::read(&journal).unwrap(), journal_bytes);
}

#[test]
fn a_replaced_vault_is_read_fresh_and_old_data_never_survives() {
    let f = Fixture::new();
    f.seed();
    assert_eq!(f.urls().len(), 2);
    let new = f.dir.join("new.sqlite3");
    create_vault(&new, APP_ID, SUPPORTED_SCHEMA);
    visit(
        &writer(&new),
        "n",
        "https://public.test/new",
        "New",
        &day(1),
    );
    fs::rename(&new, &f.vault).unwrap();
    assert_eq!(f.urls(), ["https://public.test/new"]);

    // Replaced by something unusable: no fallback to what was just served.
    fs::write(&f.vault, "garbage ".repeat(100)).unwrap();
    assert_failed(
        f.store.search_visits(&ALL),
        VisibleError::VaultSchemaMismatch,
    );
    fs::remove_file(&f.vault).unwrap();
    assert_failed(f.store.search_visits(&ALL), VisibleError::VaultUnavailable);
    assert!(!f.vault.exists());
}

#[test]
fn a_vault_replaced_or_removed_during_a_read_never_mixes_states() {
    let f = Fixture::new();
    f.seed();
    let new = f.dir.join("new.sqlite3");
    create_vault(&new, APP_ID, SUPPORTED_SCHEMA);
    visit(
        &writer(&new),
        "n",
        "https://public.test/new",
        "New",
        &day(1),
    );
    let page = f
        .store
        .search_visits_staged(&ALL, &mut |stage| {
            if stage == Stage::PolicyLoaded {
                fs::rename(&new, &f.vault).unwrap();
            }
        })
        .unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].url, "https://public.test/new");
    let result = f.store.search_visits_staged(&ALL, &mut |stage| {
        if stage == Stage::PolicyLoaded {
            fs::remove_file(&f.vault).unwrap();
        }
    });
    assert_failed(result, VisibleError::VaultUnavailable);
}

// --- Policy safety ------------------------------------------------------------------------------

#[test]
fn an_uninitialized_policy_means_no_data_and_creates_nothing() {
    let f = Fixture::new();
    f.seed();
    let empty = f.dir.join("no-app");
    let store = VisibleStore::new(f.vault.clone(), empty.clone()).unwrap();
    assert_failed(store.search_visits(&ALL), VisibleError::PolicyUnavailable);
    assert!(!empty.exists());
    assert_failed(
        VisibleStore::new(f.vault.clone(), "relative".into()).map(|_| ()),
        VisibleError::PolicyUnavailable,
    );
}

#[test]
fn damaged_policies_mean_no_data_and_recovery_needs_only_the_next_read() {
    let f = Fixture::new();
    f.seed();
    assert_eq!(f.urls().len(), 2);
    let path = f.app.join("policy.json");
    let good = fs::read(&path).unwrap();
    let mut unsupported: serde_json::Value = serde_json::from_slice(&good).unwrap();
    unsupported["engineVersion"] = 3.into();
    for bad in [
        b"{broken".to_vec(),
        b"{}".to_vec(),
        serde_json::to_vec(&unsupported).unwrap(),
    ] {
        fs::write(&path, &bad).unwrap();
        assert_failed(f.store.search_visits(&ALL), VisibleError::PolicyUnavailable);
        assert_failed(
            f.store.get_visits_by_day(&VisitsByDay {
                date: "2026-10-01",
                utc_offset: "+00:00",
                limit: None,
            }),
            VisibleError::PolicyUnavailable,
        );
    }
    fs::remove_file(&path).unwrap();
    assert_failed(f.store.search_visits(&ALL), VisibleError::PolicyUnavailable);
    fs::write(&path, &good).unwrap();
    assert_eq!(f.urls().len(), 2);
    let marker = f.app.join("policy.initialized");
    let kept = fs::read(&marker).unwrap();
    fs::remove_file(&marker).unwrap();
    assert_failed(f.store.search_visits(&ALL), VisibleError::PolicyUnavailable);
    fs::write(&marker, kept).unwrap();
    assert_eq!(f.urls().len(), 2);
}

#[test]
fn a_held_policy_lock_is_retried_then_refused() {
    let f = Fixture::new();
    f.seed();
    let lock = f.lock_policy();
    assert_failed(f.store.search_visits(&ALL), VisibleError::PolicyUnavailable);
    drop(lock);
    assert_eq!(f.urls().len(), 2);
    // Released within the retries: the read succeeds.
    let lock = f.lock_policy();
    let release = std::thread::spawn(move || {
        std::thread::sleep(POLICY_BUSY_WAIT);
        drop(lock);
    });
    assert_eq!(f.urls().len(), 2);
    release.join().unwrap();
}

#[test]
fn a_finished_policy_change_applies_to_the_next_read() {
    let f = Fixture::new();
    f.seed();
    assert_eq!(f.urls().len(), 2);
    f.add_rule(false, "one.public.test");
    assert_eq!(f.urls(), ["https://two.public.test/"]);
    f.add_rule(true, "*.public.test");
    assert!(f.urls().is_empty());
    let store = f.policy_store();
    let current = store.load().unwrap();
    let mut next = current.policy.clone();
    next.layers
        .never_save
        .rules
        .retain(|rule| *rule != "*.public.test".into());
    next.layers
        .save_but_no_mcp
        .rules
        .retain(|rule| *rule != "one.public.test".into());
    store.update(&current, next).unwrap();
    assert_eq!(f.urls().len(), 2);
}

#[test]
fn a_policy_change_at_any_point_of_a_read_is_applied_to_that_read() {
    for at in [
        Stage::PolicyLoaded,
        Stage::SchemaChecked,
        Stage::VaultClosed,
        Stage::SetBuilt,
    ] {
        let f = Fixture::new();
        f.seed();
        let mut changed = false;
        let page = f
            .store
            .search_visits_staged(&ALL, &mut |stage| {
                if stage == at && !changed {
                    f.add_rule(false, "one.public.test");
                    changed = true;
                }
            })
            .unwrap();
        assert!(changed);
        assert_eq!(page.total, 1, "{at:?}");
        assert_eq!(page.items[0].url, "https://two.public.test/", "{at:?}");
    }
}

#[test]
fn a_policy_that_keeps_changing_or_breaks_after_building_means_no_data() {
    let f = Fixture::new();
    f.seed();
    let mut count = 0;
    let result = f.store.search_visits_staged(&ALL, &mut |stage| {
        if stage == Stage::SetBuilt {
            count += 1;
            f.add_rule(false, &format!("x{count}.test"));
        }
    });
    assert_failed(result, VisibleError::PolicyUnavailable);
    assert_eq!(count, POLICY_CHANGE_ATTEMPTS);

    let path = f.app.join("policy.json");
    let good = fs::read(&path).unwrap();
    let result = f.store.search_visits_staged(&ALL, &mut |stage| {
        if stage == Stage::SetBuilt {
            fs::write(&path, b"{broken").unwrap();
        }
    });
    assert_failed(result, VisibleError::PolicyUnavailable);
    fs::write(&path, good).unwrap();

    let lock = std::cell::RefCell::new(None);
    let result = f.store.search_visits_staged(&ALL, &mut |stage| {
        if stage == Stage::SetBuilt {
            *lock.borrow_mut() = Some(f.lock_policy());
        }
    });
    assert_failed(result, VisibleError::PolicyUnavailable);
}

#[test]
fn no_earlier_answer_is_reused_after_a_failure() {
    let f = Fixture::new();
    f.seed();
    assert_eq!(f.urls().len(), 2);
    let lock = f.lock_policy();
    assert_failed(f.store.search_visits(&ALL), VisibleError::PolicyUnavailable);
    drop(lock);
    // The vault changes while the policy was failing; the answer follows the vault.
    visit(
        &writer(&f.vault),
        "n",
        "https://public.test/n",
        "t",
        &day(9),
    );
    assert_eq!(f.urls().len(), 3);
}

// --- Query boundary -----------------------------------------------------------------------------

fn canary_set() -> VisibleSet {
    let rows = private_rows()
        .into_iter()
        .map(|(id, url, title)| RawVisit {
            event_id: id.into(),
            url: url.into(),
            title: title.into(),
            visited_at: day(1),
        })
        .chain([RawVisit {
            event_id: "pub".into(),
            url: "https://public.test/".into(),
            title: "Public".into(),
            visited_at: day(1),
        }]);
    let mut policy = Policy::default();
    policy.layers.never_save.rules.push("a-layer.test".into());
    policy
        .layers
        .save_but_no_mcp
        .rules
        .push("b-layer.test".into());
    VisibleSet::build(rows.collect(), &compiled(&policy), &[1u8; 32]).unwrap()
}

#[test]
fn the_memory_database_holds_public_rows_only_and_cannot_reach_a_vault() {
    let f = Fixture::new();
    f.seed();
    let set = canary_set();
    let tables: Vec<String> = set
        .db
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(tables, ["visible_visits"]);
    let dump: Vec<String> = set
        .db
        .prepare("SELECT public_id || url || title || visited_at || visited_ms FROM visible_visits")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(dump.len(), 1);
    assert_clean(&dump);
    let databases: i64 = set
        .db
        .query_row("SELECT count(*) FROM pragma_database_list", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(databases, 1);
    let attach = format!("ATTACH DATABASE '{}' AS v", f.vault.display());
    assert!(
        set.db.execute_batch(&attach).is_err(),
        "ATTACH is limited to 0"
    );
    assert!(set
        .db
        .execute_batch("ATTACH DATABASE ':memory:' AS m")
        .is_err());
    assert!(
        set.db.execute_batch("DELETE FROM visible_visits").is_err(),
        "query_only"
    );
    assert!(set.db.execute_batch("CREATE TABLE x (y)").is_err());
}

#[test]
fn the_store_is_shareable_between_threads_and_reads_do_not_interfere() {
    fn assert_shareable<T: Send + Sync>() {}
    assert_shareable::<VisibleStore>();
    let f = Fixture::new();
    f.seed();
    let store = &f.store;
    std::thread::scope(|scope| {
        let readers: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(move || {
                    for _ in 0..5 {
                        assert_eq!(store.search_visits(&ALL).unwrap().total, 2);
                    }
                })
            })
            .collect();
        for reader in readers {
            reader.join().unwrap();
        }
    });
}

fn source_files() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.file_name().unwrap() != "tests.rs")
        .map(|path| {
            (
                path.file_name().unwrap().to_string_lossy().into_owned(),
                fs::read_to_string(&path).unwrap(),
            )
        })
        .collect()
}

#[test]
fn the_public_api_exposes_no_connection_or_sql_and_nothing_logs() {
    let files = source_files();
    assert_eq!(files.len(), 6);
    for (name, source) in &files {
        for line in source.lines().map(str::trim_start) {
            assert!(
                !(line.starts_with("pub ") && line.contains("Connection")),
                "{name}: {line}"
            );
            if name != "lib.rs" {
                assert!(
                    !line.starts_with("pub fn")
                        && !line.starts_with("pub struct")
                        && !line.starts_with("pub mod")
                        && !line.starts_with("pub use"),
                    "{name}: only lib.rs may export: {line}"
                );
            }
        }
        for banned in ["println!", "eprint", "dbg!", "log::", "tracing::", "panic!"] {
            assert!(!source.contains(banned), "{name}: {banned}");
        }
    }
    let lib = &files.iter().find(|(name, _)| name == "lib.rs").unwrap().1;
    for line in lib.lines() {
        assert!(!line.starts_with("pub mod"), "{line}");
        assert!(!line.starts_with("pub use"), "{line}");
    }
}

#[test]
fn only_read_only_access_appears_in_the_snapshot() {
    let source = include_str!("snapshot.rs");
    assert!(source.contains("SQLITE_OPEN_READ_ONLY"));
    for banned in [
        "SQLITE_OPEN_READ_WRITE",
        "SQLITE_OPEN_CREATE",
        "SQLITE_OPEN_URI",
        "immutable=",
        "ensure_schema",
    ] {
        let code: String = source
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect();
        assert!(!code.contains(banned), "{banned}");
    }
}

fn compiled(policy: &Policy) -> CompiledPolicy {
    CompiledPolicy::compile(policy).unwrap()
}

// --- Images: fixtures ---------------------------------------------------------------------------

const ALL_IMAGES: SearchImages = SearchImages {
    query: "",
    from: None,
    to: None,
    limit: Some(MAX_LIMIT),
};
const T1: i64 = 1_790_000_000_000;

fn at(offset_ms: i64) -> String {
    time::format_timestamp(T1 + offset_ms).unwrap()
}

/// The blob's own counters and dates are private values: they must never be what is reported.
fn blob(db: &Connection, n: u32) -> i64 {
    db.execute(
        "INSERT INTO image_blobs (sha256, mime_type, byte_length, width, height, created_at, last_saved_at, save_count, data)
         VALUES (printf('%064d', ?1), 'image/png', 11, 40, 30, 111, 999999999999999, 777, CAST('CANARY-data' AS BLOB))",
        [n],
    )
    .unwrap();
    db.last_insert_rowid()
}

#[derive(Clone)]
struct Save<'a> {
    id: &'a str,
    image: &'a str,
    page: Option<&'a str>,
    frame: Option<&'a str>,
    at: i64,
    incognito: i64,
    public: i64,
}

const PUBLIC_SAVE: Save = Save {
    id: "public-save",
    image: "https://cdn.public.test/one.png",
    page: Some("https://page.public.test/"),
    frame: None,
    at: T1,
    incognito: 0,
    public: 1,
};

fn save(db: &Connection, blob_id: i64, row: &Save) {
    db.execute(
        "INSERT INTO saved_images (save_id, blob_id, image_url, page_url, frame_url, saved_at, incognito, mcp_public)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![row.id, blob_id, row.image, row.page, row.frame, row.at, row.incognito, row.public],
    )
    .unwrap();
}

fn image_page(f: &Fixture) -> ImagePage {
    f.store.search_images(&ALL_IMAGES).unwrap()
}

fn sources_of(f: &Fixture, image_id: &str) -> Result<ImageSourcePage, VisibleError> {
    f.store.get_image_sources(&GetImageSources {
        image_id,
        limit: Some(MAX_LIMIT),
    })
}

fn is_listed(row: Save) -> bool {
    let f = Fixture::new();
    let db = writer(&f.vault);
    save(&db, blob(&db, 1), &row);
    let page = image_page(&f);
    assert_clean(&page);
    page.total == 1
}

/// One private save of each kind. Every value only such a save holds contains `CANARY`.
fn add_private_saves(db: &Connection, blob_id: i64, prefix: &str, newest: i64) {
    let rows = [
        Save {
            public: 0,
            image: "https://cdn.public.test/CANARY-flag.png",
            ..PUBLIC_SAVE
        },
        Save {
            incognito: 1,
            image: "https://cdn.public.test/CANARY-incognito.png",
            ..PUBLIC_SAVE
        },
        Save {
            image: "https://a-layer.test/CANARY-image.png",
            ..PUBLIC_SAVE
        },
        Save {
            page: Some("https://b-layer.test/CANARY-page"),
            ..PUBLIC_SAVE
        },
        Save {
            frame: Some("https://public.test/?access_token=CANARY-frame"),
            ..PUBLIC_SAVE
        },
        Save {
            page: Some("not a url CANARY"),
            ..PUBLIC_SAVE
        },
        Save {
            image: "data:image/png",
            page: None,
            ..PUBLIC_SAVE
        },
    ];
    for (n, row) in rows.iter().enumerate() {
        save(
            db,
            blob_id,
            &Save {
                id: &format!("{prefix}-CANARY-{n}"),
                at: newest - n as i64,
                ..row.clone()
            },
        );
    }
}

// --- Images: the decision for one row -----------------------------------------------------------

#[test]
fn the_flag_is_required_for_publication() {
    assert!(is_listed(PUBLIC_SAVE));
    assert!(!is_listed(Save {
        public: 0,
        ..PUBLIC_SAVE
    }));
}

#[test]
fn an_incognito_row_is_private_whatever_its_flag_says() {
    assert!(!is_listed(Save {
        incognito: 1,
        ..PUBLIC_SAVE
    }));
    assert!(!is_listed(Save {
        incognito: 1,
        public: 0,
        ..PUBLIC_SAVE
    }));
}

#[test]
fn every_source_url_is_judged_and_any_match_makes_the_row_private() {
    for bad in [
        "https://a-layer.test/x",
        "https://b-layer.test/x",
        "https://public.test/?access_token=x",
        "https://user:pw@public.test/",
        "https://public.test/login/x",
    ] {
        assert!(
            !is_listed(Save {
                image: bad,
                ..PUBLIC_SAVE
            }),
            "image {bad}"
        );
        assert!(
            !is_listed(Save {
                page: Some(bad),
                ..PUBLIC_SAVE
            }),
            "page {bad}"
        );
        assert!(
            !is_listed(Save {
                frame: Some(bad),
                ..PUBLIC_SAVE
            }),
            "frame {bad}"
        );
    }
}

#[test]
fn credentials_behind_extra_question_marks_and_invalid_punycode_stay_private() {
    for bad in [
        "https://public.test/??token=CANARY",
        "https://public.test/???token=CANARY",
        "https://public.test/?a?token=CANARY",
        "https://public.test/#??token=CANARY",
        "https://xn--a.public.test/CANARY",
    ] {
        for row in [
            Save {
                image: bad,
                ..PUBLIC_SAVE
            },
            Save {
                page: Some(bad),
                ..PUBLIC_SAVE
            },
            Save {
                frame: Some(bad),
                ..PUBLIC_SAVE
            },
        ] {
            assert!(!is_listed(row), "{bad}");
        }
    }
    assert!(is_listed(Save {
        image: "https://public.test/?q=a?token",
        ..PUBLIC_SAVE
    }));
}

#[test]
fn a_policy_with_a_broken_rule_gives_no_decision_rather_than_skipping_the_rule() {
    let f = Fixture::new();
    let db = writer(&f.vault);
    save(&db, blob(&db, 1), &PUBLIC_SAVE);
    visit(&db, "v", "https://public.test/", "t", &day(1));
    let path = f.app.join("policy.json");
    let mut policy: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    policy["layers"]["saveButNoMcp"]["enabled"] = false.into();
    policy["layers"]["saveButNoMcp"]["rules"] = serde_json::json!(["https://CANARY.test/*"]);
    fs::write(&path, serde_json::to_vec(&policy).unwrap()).unwrap();
    assert_failed(
        f.store.search_images(&ALL_IMAGES),
        VisibleError::PolicyUnavailable,
    );
    assert_failed(f.store.search_visits(&ALL), VisibleError::PolicyUnavailable);
}

#[test]
fn a_public_frame_or_blob_url_is_judged_like_any_other() {
    assert!(is_listed(Save {
        frame: Some("https://frame.public.test/"),
        ..PUBLIC_SAVE
    }));
    assert!(is_listed(Save {
        image: "blob:https://public.test/8c1f",
        ..PUBLIC_SAVE
    }));
    assert!(!is_listed(Save {
        image: "blob:https://b-layer.test/8c1f",
        ..PUBLIC_SAVE
    }));
}

#[test]
fn absent_sources_and_the_recorded_data_marker_are_skipped_but_something_must_be_judged() {
    assert!(is_listed(Save {
        page: None,
        ..PUBLIC_SAVE
    }));
    assert!(is_listed(Save {
        image: "data:image/png",
        ..PUBLIC_SAVE
    }));
    assert!(!is_listed(Save {
        image: "data:image/png",
        page: None,
        ..PUBLIC_SAVE
    }));
    assert!(!is_listed(Save {
        image: "data:image/png",
        page: Some("https://b-layer.test/"),
        ..PUBLIC_SAVE
    }));
}

#[test]
fn anything_else_that_cannot_be_judged_makes_the_row_private() {
    let long = format!("https://public.test/{}", "x".repeat(8192));
    for bad in [
        "",
        " ",
        "not a url",
        "/relative",
        "ftp://public.test/x",
        "file:///C:/x.png",
        "javascript:alert(1)",
        "about:blank",
        "chrome://settings",
        "unknown://public.test/",
        "data:image/png;base64,AAAA",
        "data:text/plain,hello",
        "data:image/",
        "data:",
        "DATA:image/png",
        "blob:ftp://public.test/x",
        long.as_str(),
    ] {
        assert!(
            !is_listed(Save {
                image: bad,
                ..PUBLIC_SAVE
            }),
            "image {bad}"
        );
        assert!(
            !is_listed(Save {
                page: Some(bad),
                ..PUBLIC_SAVE
            }),
            "page {bad}"
        );
        assert!(
            !is_listed(Save {
                frame: Some(bad),
                ..PUBLIC_SAVE
            }),
            "frame {bad}"
        );
    }
    // The marker stands only for the image's own source: a save records it nowhere else.
    assert!(!is_listed(Save {
        page: Some("data:image/png"),
        ..PUBLIC_SAVE
    }));
    assert!(!is_listed(Save {
        frame: Some("data:image/png"),
        ..PUBLIC_SAVE
    }));
}

#[test]
fn stored_values_of_another_type_are_not_taken_for_absent_or_repaired() {
    for column in ["page_url", "frame_url", "image_url"] {
        for value in ["x'00'", "42", "CAST(x'ff' AS TEXT)"] {
            let f = Fixture::new();
            let db = writer(&f.vault);
            save(&db, blob(&db, 1), &PUBLIC_SAVE);
            assert_eq!(image_page(&f).total, 1);
            db.execute_batch(&format!("UPDATE saved_images SET {column} = {value}"))
                .unwrap();
            assert_eq!(image_page(&f).total, 0, "{column} {value}");
        }
    }
    for update in [
        "UPDATE saved_images SET saved_at = 'yesterday'",
        "UPDATE saved_images SET saved_at = -1",
        "UPDATE saved_images SET saved_at = 9000000000000000",
        "UPDATE saved_images SET saved_at = 1.5",
        "UPDATE image_blobs SET mime_type = 'image/svg+xml'",
        "UPDATE image_blobs SET mime_type = 'text/html'",
        "UPDATE image_blobs SET width = 65536",
        "UPDATE image_blobs SET height = 65536",
        "UPDATE image_blobs SET width = 'x'",
        "UPDATE image_blobs SET sha256 = printf('%064s', 'z')",
        "PRAGMA foreign_keys = OFF; UPDATE saved_images SET blob_id = 999",
    ] {
        let f = Fixture::new();
        let db = writer(&f.vault);
        save(&db, blob(&db, 1), &PUBLIC_SAVE);
        db.execute_batch(update).unwrap();
        assert_eq!(image_page(&f).total, 0, "{update}");
    }
}

// --- Images: recomputation ----------------------------------------------------------------------

#[test]
fn one_public_save_among_many_private_ones_describes_the_image_alone() {
    let f = Fixture::new();
    let db = writer(&f.vault);
    let blob_id = blob(&db, 1);
    db.execute_batch("BEGIN").unwrap();
    for n in 0..52 {
        save(
            &db,
            blob_id,
            &Save {
                id: &format!("CANARY-{n}"),
                image: "https://cdn.public.test/CANARY.png",
                page: Some("https://page.public.test/CANARY"),
                at: T1 + 1_000 + n,
                public: 0,
                ..PUBLIC_SAVE
            },
        );
    }
    db.execute_batch("COMMIT").unwrap();
    save(&db, blob_id, &PUBLIC_SAVE);
    let page = image_page(&f);
    assert_clean(&page);
    assert_eq!((page.total, page.has_more), (1, false));
    assert_eq!(
        page.items[0],
        VisibleImage {
            id: page.items[0].id.clone(),
            mime_type: "image/png".into(),
            width: 40,
            height: 30,
            public_save_count: 1,
            first_saved_at: at(0),
            last_saved_at: at(0),
            latest_image_url: PUBLIC_SAVE.image.into(),
            latest_page_url: PUBLIC_SAVE.page.map(str::to_owned),
        }
    );
    let sources = sources_of(&f, &page.items[0].id).unwrap();
    assert_clean(&sources);
    assert_eq!((sources.total, sources.items.len()), (1, 1));
    assert_eq!(sources.items[0].saved_at, at(0));
    assert_eq!(sources.items[0].image_url, PUBLIC_SAVE.image);
}

#[test]
fn several_public_saves_of_one_image_are_counted_together() {
    let f = Fixture::new();
    let db = writer(&f.vault);
    let blob_id = blob(&db, 1);
    add_private_saves(&db, blob_id, "priv", T1 + 500_000);
    add_private_saves(&db, blob_id, "old", T1 - 500_000);
    for (id, image, offset) in [
        ("a", "https://a.public.test/1.png", 0),
        ("c", "https://c.public.test/3.png", 2_000),
        ("b", "https://b.public.test/2.png", 1_000),
    ] {
        save(
            &db,
            blob_id,
            &Save {
                id,
                image,
                page: None,
                at: T1 + offset,
                ..PUBLIC_SAVE
            },
        );
    }
    let page = image_page(&f);
    assert_clean(&page);
    let item = &page.items[0];
    assert_eq!(page.total, 1, "one image, however many saves");
    assert_eq!(item.public_save_count, 3);
    assert_eq!(
        (item.first_saved_at.as_str(), item.last_saved_at.as_str()),
        (at(0).as_str(), at(2_000).as_str())
    );
    assert_eq!(item.latest_image_url, "https://c.public.test/3.png");
    assert_eq!(item.latest_page_url, None);
    let sources = sources_of(&f, &item.id).unwrap();
    assert_clean(&sources);
    assert_eq!(
        sources
            .items
            .iter()
            .map(|source| source.image_url.as_str())
            .collect::<Vec<_>>(),
        [
            "https://c.public.test/3.png",
            "https://b.public.test/2.png",
            "https://a.public.test/1.png"
        ]
    );
    let limited = f
        .store
        .get_image_sources(&GetImageSources {
            image_id: &item.id,
            limit: Some(2),
        })
        .unwrap();
    assert_eq!(
        (limited.items.len(), limited.total, limited.has_more),
        (2, 3, true)
    );
}

#[test]
fn an_image_with_no_public_save_does_not_exist_for_a_reader() {
    let f = Fixture::new();
    let db = writer(&f.vault);
    add_private_saves(&db, blob(&db, 1), "priv", T1);
    let page = image_page(&f);
    assert_eq!((page.total, page.items.len(), page.has_more), (0, 0, false));
}

#[test]
fn private_saves_change_nothing_a_reader_can_observe() {
    let f = Fixture::new();
    let db = writer(&f.vault);
    let (one, two) = (blob(&db, 1), blob(&db, 2));
    save(&db, one, &PUBLIC_SAVE);
    save(
        &db,
        two,
        &Save {
            id: "second",
            image: "https://cdn.public.test/two.png",
            at: T1 + 10,
            ..PUBLIC_SAVE
        },
    );
    let observe = || {
        let mut seen = Vec::new();
        for limit in [1, 2, MAX_LIMIT] {
            for query in ["", "public", "CANARY", "layer", "two.png"] {
                seen.push(format!(
                    "{:?}",
                    f.store.search_images(&SearchImages {
                        query,
                        limit: Some(limit),
                        ..SearchImages::default()
                    })
                ));
            }
        }
        for item in image_page(&f).items {
            seen.push(format!("{:?}", sources_of(&f, &item.id)));
        }
        seen
    };
    let before = observe();

    // Newer than everything public, on the same blobs and on a blob of their own.
    add_private_saves(&db, one, "p1", T1 + 500_000);
    add_private_saves(&db, two, "p2", T1 - 500_000);
    add_private_saves(&db, blob(&db, 3), "p3", T1 + 900_000);
    db.execute("UPDATE image_blobs SET save_count = save_count + 50", [])
        .unwrap();
    let during = observe();
    assert_eq!(during, before);
    assert_clean(&during);

    db.execute_batch(
        "DELETE FROM saved_images WHERE save_id LIKE '%CANARY%'; DELETE FROM image_blobs WHERE id = 3;",
    )
    .unwrap();
    assert_eq!(observe(), before);
}

#[test]
fn ordering_limit_and_totals_cover_public_images_only() {
    let f = Fixture::new();
    let db = writer(&f.vault);
    let mut expected = Vec::new();
    for n in 0..5 {
        let blob_id = blob(&db, n);
        let image = format!("https://cdn.public.test/{n}.png");
        save(
            &db,
            blob_id,
            &Save {
                id: &format!("pub-{n}"),
                image: &image,
                at: T1 + i64::from(n) * 1_000,
                ..PUBLIC_SAVE
            },
        );
        // Each private save would reverse the order if it counted.
        add_private_saves(
            &db,
            blob_id,
            &format!("p{n}"),
            T1 + 900_000 - i64::from(n) * 1_000,
        );
        expected.insert(0, image);
    }
    for n in 5..15 {
        add_private_saves(&db, blob(&db, n), &format!("p{n}"), T1 + 2_000_000);
    }
    let page = f
        .store
        .search_images(&SearchImages {
            limit: Some(2),
            ..SearchImages::default()
        })
        .unwrap();
    assert_clean(&page);
    assert_eq!((page.total, page.has_more), (5, true));
    assert_eq!(
        page.items
            .iter()
            .map(|item| item.latest_image_url.clone())
            .collect::<Vec<_>>(),
        expected[..2]
    );
    let all = image_page(&f);
    assert_eq!(
        all.items
            .into_iter()
            .map(|item| item.latest_image_url)
            .collect::<Vec<_>>(),
        expected
    );
}

#[test]
fn search_matches_public_sources_only_and_the_range_uses_public_dates() {
    let f = Fixture::new();
    let db = writer(&f.vault);
    let (one, other) = (blob(&db, 1), blob(&db, 2));
    save(
        &db,
        one,
        &Save {
            frame: Some("https://frame.public.test/needle"),
            ..PUBLIC_SAVE
        },
    );
    save(
        &db,
        one,
        &Save {
            id: "later",
            image: "https://later.public.test/x.png",
            at: T1 + 60_000,
            ..PUBLIC_SAVE
        },
    );
    save(
        &db,
        other,
        &Save {
            id: "hidden",
            image: "https://cdn.public.test/hidden-needle.png",
            at: T1 + 30_000,
            public: 0,
            ..PUBLIC_SAVE
        },
    );
    save(
        &db,
        other,
        &Save {
            id: "shown",
            image: "https://cdn.public.test/plain.png",
            at: T1 + 120_000,
            ..PUBLIC_SAVE
        },
    );
    let total = |query: &str, from: Option<&str>, to: Option<&str>| {
        f.store
            .search_images(&SearchImages {
                query,
                from,
                to,
                limit: None,
            })
            .unwrap()
            .total
    };
    assert_eq!(total("", None, None), 2);
    assert_eq!(
        total("needle", None, None),
        1,
        "the frame URL, not the private image URL"
    );
    assert_eq!(total("hidden", None, None), 0);
    assert_eq!(total("page.public", None, None), 2);
    assert_eq!(total("%", None, None), 0, "LIKE characters are literal");
    // The private save of `other` at +30 s gives it nothing to match in this range.
    let (start, end) = (at(30_000), at(60_000));
    assert_eq!(total("", Some(&start), Some(&end)), 0, "`to` is exclusive");
    let end = at(60_001);
    assert_eq!(total("", Some(&start), Some(&end)), 1);

    // What a match returns still describes all public saves of the image.
    let page = f
        .store
        .search_images(&SearchImages {
            query: "needle",
            ..SearchImages::default()
        })
        .unwrap();
    assert_eq!(page.items[0].public_save_count, 2);
    assert_eq!(page.items[0].last_saved_at, at(60_000));
}

#[test]
fn image_arguments_are_checked_like_visit_arguments() {
    let f = Fixture::new();
    let long = "x".repeat(MAX_QUERY_CHARS + 1);
    for request in [
        SearchImages {
            query: &long,
            ..SearchImages::default()
        },
        SearchImages {
            from: Some("yesterday"),
            ..SearchImages::default()
        },
        SearchImages {
            to: Some("2026-10-01"),
            ..SearchImages::default()
        },
        SearchImages {
            from: Some("2026-10-02T00:00:00.000Z"),
            to: Some("2026-10-01T00:00:00.000Z"),
            ..SearchImages::default()
        },
        SearchImages {
            limit: Some(0),
            ..SearchImages::default()
        },
        SearchImages {
            limit: Some(MAX_LIMIT + 1),
            ..SearchImages::default()
        },
    ] {
        assert_failed(
            f.store.search_images(&request),
            VisibleError::InvalidArgument,
        );
    }
    for limit in [0, MAX_LIMIT + 1] {
        assert_failed(
            f.store.get_image_sources(&GetImageSources {
                image_id: "image_x",
                limit: Some(limit),
            }),
            VisibleError::InvalidArgument,
        );
    }
    let db = writer(&f.vault);
    db.execute_batch("BEGIN").unwrap();
    let shared = blob(&db, 1);
    for n in 0..=DEFAULT_LIMIT {
        save(
            &db,
            shared,
            &Save {
                id: &format!("s{n}"),
                at: T1 + 1,
                ..PUBLIC_SAVE
            },
        );
        save(
            &db,
            blob(&db, 100 + n),
            &Save {
                id: &format!("t{n}"),
                ..PUBLIC_SAVE
            },
        );
    }
    db.execute_batch("COMMIT").unwrap();
    let page = f.store.search_images(&SearchImages::default()).unwrap();
    assert_eq!(page.items.len(), DEFAULT_LIMIT as usize, "default limit");
    let sources = f
        .store
        .get_image_sources(&GetImageSources {
            image_id: &page.items[0].id,
            limit: None,
        })
        .unwrap();
    assert_eq!(
        (sources.items.len(), sources.total, sources.has_more),
        (DEFAULT_LIMIT as usize, u64::from(DEFAULT_LIMIT) + 1, true)
    );
}

// --- Images: external IDs -----------------------------------------------------------------------

fn opaque(id: &str, prefix: &str) {
    let hex = id.strip_prefix(prefix).unwrap_or_else(|| panic!("{id}"));
    assert_eq!(hex.len(), 32, "{id}");
    assert!(hex.bytes().all(|b| b.is_ascii_hexdigit()), "{id}");
}

#[test]
fn image_and_source_ids_are_opaque_of_two_kinds_and_stable_per_store() {
    let f = Fixture::new();
    let db = writer(&f.vault);
    save(&db, blob(&db, 1), &PUBLIC_SAVE);
    let image = image_page(&f).items.remove(0);
    let source = sources_of(&f, &image.id).unwrap().items.remove(0);
    opaque(&image.id, "image_");
    opaque(&source.id, "source_");
    let text = format!("{image:?} {source:?}");
    let sha = format!("{:064}", 1);
    for internal in [sha.as_str(), PUBLIC_SAVE.id, "777"] {
        assert!(!text.contains(internal), "{internal}");
    }
    assert_eq!(image_page(&f).items[0].id, image.id);
    assert_eq!(sources_of(&f, &image.id).unwrap().items[0].id, source.id);
    // A source ID is no image ID, and another store's IDs mean nothing here.
    assert_failed(sources_of(&f, &source.id), VisibleError::NotFound);
    let other = VisibleStore::new(f.vault.clone(), f.app.clone()).unwrap();
    assert_ne!(
        other.search_images(&ALL_IMAGES).unwrap().items[0].id,
        image.id
    );
    assert_failed(
        other.get_image_sources(&GetImageSources {
            image_id: &image.id,
            limit: None,
        }),
        VisibleError::NotFound,
    );
}

#[test]
fn ids_of_different_kinds_differ_beyond_their_prefix_even_for_the_same_input() {
    let key = [7u8; 32];
    let digest = |kind: &str| {
        crate::visible_set::public_id(&key, kind, "same-input")
            .split_once('_')
            .unwrap()
            .1
            .to_owned()
    };
    assert_ne!(digest("image"), digest("source"));
    assert_ne!(digest("image"), digest("visit"));
}

#[test]
fn the_ids_of_public_rows_do_not_depend_on_rowids_or_on_private_rows() {
    // The same public save after two different private histories.
    let f = Fixture::new();
    let db = writer(&f.vault);
    let plain = blob(&db, 1);
    save(&db, plain, &PUBLIC_SAVE);
    let before = image_page(&f).items.remove(0);
    let source = sources_of(&f, &before.id).unwrap().items.remove(0);
    drop(db);
    fs::remove_file(&f.vault).unwrap();
    create_vault(&f.vault, APP_ID, SUPPORTED_SCHEMA);
    let db = writer(&f.vault);
    add_private_saves(&db, blob(&db, 50), "p", T1);
    add_private_saves(&db, blob(&db, 51), "q", T1);
    let moved = blob(&db, 1);
    assert_ne!(moved, plain);
    save(&db, moved, &PUBLIC_SAVE);
    assert_eq!(image_page(&f).items[0].id, before.id);
    assert_eq!(sources_of(&f, &before.id).unwrap().items[0].id, source.id);
}

#[test]
fn a_private_image_and_an_unknown_id_give_the_same_answer() {
    let f = Fixture::new();
    let db = writer(&f.vault);
    save(&db, blob(&db, 1), &PUBLIC_SAVE);
    let id = image_page(&f).items.remove(0).id;
    db.execute_batch("UPDATE saved_images SET mcp_public = 0")
        .unwrap();
    let sha = format!("{:064}", 1);
    for unknown in [
        id.as_str(),
        "image_00000000000000000000000000000000",
        "",
        "1",
        sha.as_str(),
        "' OR 1=1 --",
        PUBLIC_SAVE.id,
    ] {
        let error = sources_of(&f, unknown).unwrap_err();
        assert_eq!(error, VisibleError::NotFound, "{unknown}");
        assert_eq!(error.code(), "not_found");
        assert_eq!(format!("{error:?}"), "NotFound");
    }
}

// --- Images: updates and failures ---------------------------------------------------------------

#[test]
fn flag_and_policy_changes_apply_to_the_next_call_and_an_earlier_id_dies_with_them() {
    let f = Fixture::new();
    let db = writer(&f.vault);
    save(&db, blob(&db, 1), &PUBLIC_SAVE);
    let id = image_page(&f).items.remove(0).id;
    db.execute_batch("UPDATE saved_images SET mcp_public = 0")
        .unwrap();
    assert_eq!(image_page(&f).total, 0);
    assert_failed(sources_of(&f, &id), VisibleError::NotFound);
    db.execute_batch("UPDATE saved_images SET mcp_public = 1")
        .unwrap();
    assert_eq!(
        image_page(&f).items[0].id,
        id,
        "the same image, the same ID"
    );
    f.add_rule(false, "cdn.public.test");
    assert_eq!(image_page(&f).total, 0);
    assert_failed(sources_of(&f, &id), VisibleError::NotFound);
}

#[test]
fn a_policy_change_at_any_point_of_an_image_read_is_applied_to_that_read() {
    for moment in [
        Stage::PolicyLoaded,
        Stage::SchemaChecked,
        Stage::VaultClosed,
        Stage::SetBuilt,
    ] {
        let f = Fixture::new();
        let db = writer(&f.vault);
        save(&db, blob(&db, 1), &PUBLIC_SAVE);
        let mut applied = false;
        let page = f
            .store
            .search_images_staged(&ALL_IMAGES, &mut |stage| {
                if stage == moment && !applied {
                    applied = true;
                    f.add_rule(false, "cdn.public.test");
                }
            })
            .unwrap();
        assert!(applied);
        assert_eq!(page.total, 0, "{moment:?}");
    }
}

#[test]
fn image_reads_fail_closed_and_no_earlier_answer_is_reused() {
    let f = Fixture::new();
    let db = writer(&f.vault);
    save(&db, blob(&db, 1), &PUBLIC_SAVE);
    let id = image_page(&f).items.remove(0).id;
    let assert_both = |expected: VisibleError| {
        assert_failed(f.store.search_images(&ALL_IMAGES), expected);
        assert_failed(sources_of(&f, &id), expected);
    };

    let lock = f.lock_policy();
    assert_both(VisibleError::PolicyUnavailable);
    drop(lock);
    let policy = f.app.join("policy.json");
    let good = fs::read(&policy).unwrap();
    fs::write(&policy, b"{ CANARY damaged").unwrap();
    assert_both(VisibleError::PolicyUnavailable);
    fs::remove_file(&policy).unwrap();
    assert_both(VisibleError::PolicyUnavailable);
    fs::write(&policy, &good).unwrap();
    assert_eq!(image_page(&f).total, 1);

    let uninitialized = VisibleStore::new(f.vault.clone(), f.dir.join("empty-app")).unwrap();
    assert_failed(
        uninitialized.search_images(&ALL_IMAGES),
        VisibleError::PolicyUnavailable,
    );

    drop(db);
    let vault_bytes = fs::read(&f.vault).unwrap();
    fs::remove_file(&f.vault).unwrap();
    assert_both(VisibleError::VaultUnavailable);
    assert!(!f.vault.exists(), "nothing was created");

    fs::write(&f.vault, vec![b'x'; 4096]).unwrap();
    assert_both(VisibleError::VaultSchemaMismatch);

    fs::remove_file(&f.vault).unwrap();
    create_vault(&f.vault, APP_ID, 3);
    assert_both(VisibleError::VaultSchemaMismatch);

    // A replaced vault is read fresh: what the old one held does not survive.
    fs::remove_file(&f.vault).unwrap();
    create_vault(&f.vault, APP_ID, SUPPORTED_SCHEMA);
    assert_eq!(image_page(&f).total, 0);
    assert_failed(sources_of(&f, &id), VisibleError::NotFound);

    fs::write(&f.vault, &vault_bytes).unwrap();
    assert_eq!(image_page(&f).items[0].id, id);
}

#[test]
fn an_image_read_sees_one_committed_state_and_holds_no_lock_while_judging() {
    let f = Fixture::new();
    let db = writer(&f.vault);
    let blob_id = blob(&db, 1);
    save(&db, blob_id, &PUBLIC_SAVE);

    // A row that is written but not committed is never public.
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    save(
        &db,
        blob_id,
        &Save {
            id: "uncommitted",
            image: "https://cdn.public.test/uncommitted.png",
            at: T1 + 1,
            ..PUBLIC_SAVE
        },
    );
    let page = image_page(&f);
    assert_eq!(page.items[0].public_save_count, 1);
    assert_eq!(page.items[0].latest_image_url, PUBLIC_SAVE.image);
    db.execute_batch("ROLLBACK").unwrap();

    let late = writer(&f.vault);
    late.busy_timeout(Duration::ZERO).unwrap();
    let mut blocked = None;
    let mut wrote = false;
    let page = f
        .store
        .search_images_staged(&ALL_IMAGES, &mut |stage| {
            if stage == Stage::SchemaChecked {
                late.execute_batch("BEGIN IMMEDIATE").unwrap();
                late.execute_batch("UPDATE saved_images SET mcp_public = 0")
                    .unwrap();
                blocked = Some(late.execute_batch("COMMIT").is_err());
                late.execute_batch("ROLLBACK").unwrap();
            }
            if stage == Stage::VaultClosed {
                save(
                    &late,
                    blob_id,
                    &Save {
                        id: "after",
                        ..PUBLIC_SAVE
                    },
                );
                wrote = true;
            }
        })
        .unwrap();
    assert_eq!(blocked, Some(true), "the snapshot holds the shared lock");
    assert!(wrote, "no lock is held while rows are judged");
    assert_eq!(page.items[0].public_save_count, 1);
    assert_eq!(image_page(&f).items[0].public_save_count, 2);
}

#[test]
fn reading_images_leaves_the_vault_untouched() {
    let f = Fixture::new();
    let db = writer(&f.vault);
    save(&db, blob(&db, 1), &PUBLIC_SAVE);
    drop(db);
    let before = f.fingerprint();
    let id = image_page(&f).items.remove(0).id;
    sources_of(&f, &id).unwrap();
    assert_eq!(f.fingerprint(), before);
    assert!(!f.sidecars());
}

#[test]
fn visits_and_images_answer_independently() {
    let f = Fixture::new();
    f.seed();
    let db = writer(&f.vault);
    save(
        &db,
        blob(&db, 1),
        &Save {
            image: "https://one.public.test/image.png",
            ..PUBLIC_SAVE
        },
    );
    assert_eq!(f.store.search_visits(&ALL).unwrap().total, 2);
    assert_eq!(image_page(&f).total, 1);
    assert_eq!(
        f.store
            .search_visits(&SearchVisits {
                query: "image.png",
                ..SearchVisits::default()
            })
            .unwrap()
            .total,
        0
    );
}

// --- Images: what the set holds -----------------------------------------------------------------

fn raw_saves() -> Vec<RawImageSave> {
    let raw = |id: &str, image: &str, incognito: i64, mcp_public: i64| RawImageSave {
        save_id: id.into(),
        image_url: image.into(),
        page_url: Some("https://page.public.test/".into()),
        frame_url: None,
        saved_at: T1,
        incognito,
        mcp_public,
        sha256: "a".repeat(64),
        mime_type: "image/png".into(),
        width: 1,
        height: 1,
    };
    vec![
        raw("pub", "https://public.test/1.png", 0, 1),
        raw("CANARY-1", "https://public.test/CANARY.png", 0, 0),
        raw("CANARY-2", "https://public.test/CANARY.png", 1, 1),
        raw("CANARY-3", "https://a-layer.test/CANARY.png", 0, 1),
        raw("CANARY-4", "https://b-layer.test/CANARY.png", 0, 1),
        // Values the schema refuses are still private if they ever arrive.
        raw("CANARY-5", "https://public.test/CANARY.png", 2, 1),
        raw("CANARY-6", "https://public.test/CANARY.png", 0, 2),
        raw("CANARY-7", "https://public.test/CANARY.png", -1, 1),
    ]
}

fn test_policy() -> Policy {
    let mut policy = Policy::default();
    policy.layers.never_save.rules.push("a-layer.test".into());
    policy
        .layers
        .save_but_no_mcp
        .rules
        .push("b-layer.test".into());
    policy
}

#[test]
fn the_decision_is_made_for_each_row_and_refuses_unexpected_flag_values() {
    let policy = compiled(&test_policy());
    let public: Vec<String> = raw_saves()
        .into_iter()
        .filter(|save| visible_images::save_is_public(save, &policy))
        .map(|save| save.save_id)
        .collect();
    assert_eq!(public, ["pub"]);
}

#[test]
fn the_image_memory_database_holds_public_values_only_and_cannot_be_written_or_attached() {
    let set = VisibleImages::build(raw_saves(), &compiled(&test_policy()), &[1u8; 32]).unwrap();
    let mut dump = Vec::new();
    for (table, columns) in [
        (
            "visible_images",
            "public_id || mime_type || width || height || public_save_count || first_saved_ms || last_saved_ms",
        ),
        (
            "visible_image_sources",
            "public_id || image_id || image_url || ifnull(page_url, '') || ifnull(frame_url, '') || saved_ms",
        ),
    ] {
        let rows: Vec<String> = set
            .db
            .prepare(&format!("SELECT {columns} FROM {table}"))
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(rows.len(), 1, "{table}");
        dump.extend(rows);
    }
    assert_clean(&dump);
    assert!(
        !dump.join("\n").contains(&"a".repeat(64)),
        "the hash is not kept"
    );
    let tables: i64 = set
        .db
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(tables, 2);
    assert!(set
        .db
        .execute_batch("ATTACH DATABASE ':memory:' AS m")
        .is_err());
    assert!(set.db.execute_batch("DELETE FROM visible_images").is_err());
    assert!(set.db.execute_batch("CREATE TABLE x (y)").is_err());
}

#[test]
fn an_original_is_never_selected_and_no_blob_counter_or_hash_is_published() {
    let select = include_str!("snapshot.rs")
        .lines()
        .find(|line| line.starts_with("const SELECT_IMAGE_SAVES"))
        .unwrap();
    for banned in [
        "data",
        "save_count",
        "created_at",
        "last_saved_at",
        "byte_length",
        "thumbnail",
        "*",
        "SELECT s.id",
        "b.id,",
    ] {
        assert!(!select.contains(banned), "{banned}");
    }
    assert!(!include_str!("lib.rs").contains("sha256"));
    assert!(!include_str!("visible_images.rs").contains("image_blobs "));
}

#[test]
fn timestamps_format_back_to_what_they_parse_from() {
    for text in [
        "1970-01-01T00:00:00.000Z",
        "2000-02-29T23:59:59.999Z",
        "2026-10-31T12:34:56.789Z",
        "2024-12-31T00:00:00.001Z",
        "9999-12-31T23:59:59.999Z",
    ] {
        let ms = time::parse_timestamp(text).unwrap();
        assert_eq!(time::format_timestamp(ms).as_deref(), Some(text));
    }
    assert_eq!(time::format_timestamp(-1), None);
    assert_eq!(time::format_timestamp(253_402_300_800_000), None);
    for ms in (0..253_402_300_799_999i64).step_by(7_919_000_123) {
        let text = time::format_timestamp(ms).unwrap();
        assert_eq!(time::parse_timestamp(&text), Some(ms), "{text}");
    }
}

// --- Measurements (not a gate): `cargo test --release perf_report -- --ignored --nocapture` -------

#[test]
#[ignore = "measurement"]
fn perf_report() {
    let rows: u32 = std::env::var("MC_PERF_ROWS").map_or(10_000, |v| v.parse().unwrap());
    let rules: u32 = std::env::var("MC_PERF_RULES").map_or(0, |v| v.parse().unwrap());
    let private_every: u32 = 10;
    let f = Fixture::new();
    let db = writer(&f.vault);
    db.execute_batch("BEGIN").unwrap();
    for n in 0..rows {
        let host = if n % private_every == 0 {
            "b-layer"
        } else {
            "public"
        };
        visit(
            &db,
            &uuid::Uuid::new_v4().to_string(),
            &format!("https://{host}.test/some/ordinary/path/{n}?q=value"),
            &format!("An ordinary page title number {n}"),
            &format!(
                "2026-10-{:02}T{:02}:{:02}:{:02}.000Z",
                n % 28 + 1,
                n % 24,
                n % 60,
                n / 60 % 60
            ),
        );
    }
    db.execute_batch("COMMIT").unwrap();
    drop(db);
    let store = f.policy_store();
    let current = store.load().unwrap();
    let mut policy = current.policy.clone();
    for n in 0..rules {
        policy
            .layers
            .save_but_no_mcp
            .rules
            .push(format!("*.host{n}.example").into());
    }
    store.update(&current, policy.clone()).unwrap();
    let vault_bytes = fs::metadata(&f.vault).unwrap().len();

    let mut samples = Vec::new();
    for _ in 0..5 {
        let start = Instant::now();
        let mut marks = Vec::new();
        let page = f
            .store
            .search_visits_staged(
                &SearchVisits {
                    query: "number 99",
                    ..SearchVisits::default()
                },
                &mut |stage| marks.push((stage, start.elapsed())),
            )
            .unwrap();
        let total = start.elapsed();
        let mark = |wanted| marks.iter().find(|(stage, _)| *stage == wanted).unwrap().1;
        let lock_held = mark(Stage::VaultClosed) - mark(Stage::PolicyLoaded);
        let judge_build = mark(Stage::SetBuilt) - mark(Stage::VaultClosed);
        samples.push((
            total,
            lock_held,
            judge_build,
            total - mark(Stage::SetBuilt),
            page.total,
        ));
    }
    samples.sort();
    let (total, lock_held, judge_build, query, hits) = samples[samples.len() / 2];

    // A host-like writer during continuous reads: slowest commit, and whether any failed.
    let stop = Arc::new(AtomicBool::new(false));
    let worker = {
        let (vault, stop) = (f.vault.clone(), stop.clone());
        std::thread::spawn(move || {
            let db = writer(&vault);
            let (mut worst, mut commits, mut failures, mut n) = (Duration::ZERO, 0u32, 0u32, 0u32);
            while !stop.load(Ordering::Relaxed) {
                let start = Instant::now();
                let result = db.execute(
                    "INSERT INTO visits VALUES (?1, 'https://public.test/w', 't', '2026-10-01T00:00:00.000Z')",
                    params![format!("w{n}")],
                );
                n += 1;
                match result {
                    Ok(_) => {
                        commits += 1;
                        worst = worst.max(start.elapsed());
                    }
                    Err(_) => failures += 1,
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            (worst, commits, failures)
        })
    };
    let (mut reads, mut busy) = (0, 0);
    for _ in 0..5 {
        match f.store.search_visits(&SearchVisits::default()) {
            Ok(_) => reads += 1,
            Err(_) => busy += 1,
        }
    }
    stop.store(true, Ordering::Relaxed);
    let (worst, commits, failures) = worker.join().unwrap();

    let raw = snapshot::read_visits(&f.vault, &mut |_| {}).unwrap();
    let raw_bytes: usize = raw
        .iter()
        .map(|r| r.event_id.len() + r.url.len() + r.title.len() + r.visited_at.len())
        .sum();
    let set = VisibleSet::build(raw, &compiled(&policy), &[0u8; 32]).unwrap();
    let set_bytes: i64 = set
        .db
        .query_row(
            "SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()",
            [],
            |row| row.get(0),
        )
        .unwrap();
    println!(
        "PERF rows={rows} rules={rules} vault={:.1}MiB total={total:?} lock_held={lock_held:?} judge_build={judge_build:?} query={query:?} hits={hits} raw_text={:.1}MiB set={:.1}MiB host_commits={commits} host_failures={failures} worst_commit={worst:?} reads={reads} read_busy={busy}",
        vault_bytes as f64 / 1_048_576.0,
        raw_bytes as f64 / 1_048_576.0,
        set_bytes as f64 / 1_048_576.0,
    );
}

/// `MC_PERF_ROWS`, `MC_PERF_PUBLIC` (percent), `MC_PERF_RULES`. Five saves share each blob.
#[test]
#[ignore = "measurement"]
fn perf_images() {
    let env =
        |name: &str, default: u32| std::env::var(name).map_or(default, |v| v.parse().unwrap());
    let (rows, percent, rules, regexes) = (
        env("MC_PERF_ROWS", 10_000),
        env("MC_PERF_PUBLIC", 50),
        env("MC_PERF_RULES", 0),
        env("MC_PERF_REGEX", 0),
    );
    let f = Fixture::new();
    let db = writer(&f.vault);
    db.execute_batch("BEGIN").unwrap();
    let mut blob_id = 0;
    for n in 0..rows {
        if n % 5 == 0 {
            blob_id = blob(&db, n);
        }
        save(
            &db,
            blob_id,
            &Save {
                id: &format!("save-{n}"),
                image: &format!("https://cdn.public.test/assets/images/{n}.png?size=large"),
                page: Some(&format!("https://page.public.test/some/ordinary/path/{n}")),
                at: T1 + i64::from(n),
                public: i64::from(n * 7 % 100 < percent),
                ..PUBLIC_SAVE
            },
        );
    }
    db.execute_batch("COMMIT").unwrap();
    drop(db);
    let store = f.policy_store();
    let current = store.load().unwrap();
    let mut policy = current.policy.clone();
    for n in 0..rules {
        policy
            .layers
            .save_but_no_mcp
            .rules
            .push(format!("*.host{n}.example").into());
    }
    for n in 0..regexes {
        use memocampus_native_host::policy::{RuleSpec, TypedRule};
        policy
            .layers
            .never_save
            .rules
            .push(RuleSpec::Typed(TypedRule::Regex {
                pattern: format!(r"^https://(?:[^/]+\.)?site{n}\.example/private/"),
            }));
    }
    store.update(&current, policy.clone()).unwrap();

    let mut samples = Vec::new();
    for _ in 0..5 {
        let start = Instant::now();
        let mut marks = Vec::new();
        let page = f
            .store
            .search_images_staged(
                &SearchImages {
                    query: "99",
                    ..SearchImages::default()
                },
                &mut |stage| marks.push((stage, start.elapsed())),
            )
            .unwrap();
        let total = start.elapsed();
        let mark = |wanted| marks.iter().find(|(stage, _)| *stage == wanted).unwrap().1;
        samples.push((
            total,
            mark(Stage::VaultClosed) - mark(Stage::PolicyLoaded),
            mark(Stage::SetBuilt) - mark(Stage::VaultClosed),
            total - mark(Stage::SetBuilt),
            page.total,
        ));
    }
    samples.sort();
    let (total, lock_held, judge_build, query, hits) = samples[samples.len() / 2];

    let stop = Arc::new(AtomicBool::new(false));
    let worker = {
        let (vault, stop) = (f.vault.clone(), stop.clone());
        std::thread::spawn(move || {
            let db = writer(&vault);
            let (mut worst, mut commits, mut failures, mut n) = (Duration::ZERO, 0u32, 0u32, 0u32);
            while !stop.load(Ordering::Relaxed) {
                let start = Instant::now();
                match db.execute(
                    "INSERT INTO saved_images (save_id, blob_id, image_url, saved_at, incognito, mcp_public) VALUES (?1, 1, 'https://public.test/w', 1, 0, 0)",
                    params![format!("w{n}")],
                ) {
                    Ok(_) => {
                        commits += 1;
                        worst = worst.max(start.elapsed());
                    }
                    Err(_) => failures += 1,
                }
                n += 1;
                std::thread::sleep(Duration::from_millis(50));
            }
            (worst, commits, failures)
        })
    };
    let (mut reads, mut busy) = (0, 0);
    for _ in 0..5 {
        match f.store.search_images(&SearchImages::default()) {
            Ok(_) => reads += 1,
            Err(_) => busy += 1,
        }
    }
    stop.store(true, Ordering::Relaxed);
    let (worst, commits, failures) = worker.join().unwrap();

    let raw = snapshot::read_image_saves(&f.vault, &mut |_| {}).unwrap();
    let start = Instant::now();
    let policy = compiled(&policy);
    let compile = start.elapsed();
    let set = VisibleImages::build(raw, &policy, &[0u8; 32]).unwrap();
    let set_bytes: i64 = set
        .db
        .query_row(
            "SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()",
            [],
            |row| row.get(0),
        )
        .unwrap();
    println!(
        "PERF-IMG rows={rows} public={percent}% rules={rules} regexes={regexes} compile={compile:?} total={total:?} lock_held={lock_held:?} judge_build={judge_build:?} query={query:?} hits={hits} set={:.1}MiB host_commits={commits} host_failures={failures} worst_commit={worst:?} reads={reads} read_busy={busy}",
        set_bytes as f64 / 1_048_576.0,
    );
}

// --- Policy version 2 (Issue #25 Slice 3) --------------------------------------------------------

/// The fixture's policy written back as version 1, with its rules as strings.
fn as_version_1(f: &Fixture) {
    use memocampus_native_host::policy::{RuleSpec, TypedRule};
    let store = f.policy_store();
    let mut policy = store.load().unwrap().policy;
    (policy.schema_version, policy.engine_version) = (1, 1);
    for layer in [
        &mut policy.layers.never_save,
        &mut policy.layers.save_but_no_mcp,
    ] {
        for rule in &mut layer.rules {
            if let RuleSpec::Typed(TypedRule::Pattern { pattern }) = rule {
                *rule = RuleSpec::Text(pattern.clone());
            }
        }
    }
    fs::write(
        store.policy_path(),
        serde_json::to_vec_pretty(&policy).unwrap(),
    )
    .unwrap();
}

fn everything_visible(f: &Fixture) -> Vec<String> {
    let mut seen = vec![
        format!("{:?}", f.store.search_visits(&ALL)),
        format!("{:?}", f.store.search_images(&ALL_IMAGES)),
    ];
    for item in image_page(f).items {
        seen.push(format!("{:?}", sources_of(f, &item.id)));
    }
    seen
}

fn seed_images(f: &Fixture) {
    let db = writer(&f.vault);
    let blob_id = blob(&db, 1);
    save(&db, blob_id, &PUBLIC_SAVE);
    save(
        &db,
        blob_id,
        &Save {
            id: "CANARY-b",
            page: Some("https://b-layer.test/CANARY"),
            ..PUBLIC_SAVE
        },
    );
    save(
        &db,
        blob(&db, 2),
        &Save {
            id: "CANARY-a",
            image: "https://a-layer.test/CANARY.png",
            ..PUBLIC_SAVE
        },
    );
}

#[test]
fn version_1_and_its_explicit_migration_publish_exactly_the_same() {
    let f = Fixture::new();
    f.seed();
    seed_images(&f);
    as_version_1(&f);
    let store = f.policy_store();
    let v1 = store.load().unwrap();
    assert_eq!(v1.policy.schema_version, 1);
    let before = everything_visible(&f);
    assert_clean(&before);
    assert_eq!(f.store.search_visits(&ALL).unwrap().total, 2);
    assert_eq!(
        image_page(&f).items[0].public_save_count,
        1,
        "the layer B save is private"
    );
    let migrated = store.migrate(&v1).unwrap();
    assert_eq!(migrated.policy.schema_version, 2);
    assert_eq!(everything_visible(&f), before, "the same set, the same ids");
    assert_eq!(store.load().unwrap().etag, migrated.etag);
}

#[test]
fn version_2_keeps_layer_b_private() {
    let f = Fixture::new();
    f.seed();
    seed_images(&f);
    assert_eq!(f.policy_store().load().unwrap().policy.schema_version, 2);
    let visible = everything_visible(&f);
    assert_clean(&visible);
    assert_eq!(f.store.search_visits(&ALL).unwrap().total, 2);
    assert_eq!(image_page(&f).items.len(), 1);
    assert_eq!(image_page(&f).items[0].public_save_count, 1);
}

#[test]
fn a_policy_with_an_unusable_regex_publishes_nothing() {
    let f = Fixture::new();
    f.seed();
    seed_images(&f);
    let id = image_page(&f).items.remove(0).id;
    let path = f.app.join("policy.json");
    let original = fs::read(&path).unwrap();
    // Unsupported, invalid, and over the compiled size limit.
    for pattern in ["(?<=CANARY)x", "(CANARY", "\\w{100}CANARY"] {
        let mut value: serde_json::Value = serde_json::from_slice(&original).unwrap();
        value["layers"]["saveButNoMcp"]["rules"] =
            serde_json::json!([{ "kind": "regex", "pattern": pattern }]);
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert_failed(f.store.search_visits(&ALL), VisibleError::PolicyUnavailable);
        assert_failed(
            f.store.search_images(&ALL_IMAGES),
            VisibleError::PolicyUnavailable,
        );
        assert_failed(sources_of(&f, &id), VisibleError::PolicyUnavailable);
        // Disabling the layer does not make it usable: the rule cannot be judged.
        value["layers"]["saveButNoMcp"]["enabled"] = false.into();
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert_failed(f.store.search_visits(&ALL), VisibleError::PolicyUnavailable);
    }
}

#[test]
fn the_new_patterns_and_collapsed_slashes_keep_rows_out_of_the_public_read() {
    let f = Fixture::new();
    f.add_rule(true, "new.test/secret");
    f.add_rule(false, "new.test/img/*.png");
    f.add_rule(false, "new.test/notes");
    let db = writer(&f.vault);
    let rows = [
        ("blind-exact", "https://new.test/secret", false),
        ("blind-doubled", "https://new.test//secret", false),
        ("private-name", "https://new.test/img/CANARY.png", false),
        ("private-doubled", "https://new.test/img//CANARY.png", false),
        ("private-exact", "https://new.test///notes", false),
        ("public-below", "https://new.test/secret/x", true),
        ("public-sub", "https://new.test/img/sub/x.png", true),
        ("public-other", "https://new.test/img/x.jpg", true),
    ];
    for (id, url, _) in rows {
        visit(&db, id, url, "Title", &day(3));
    }
    drop(db);
    let visible = f.urls();
    for (id, url, public) in rows {
        assert_eq!(visible.iter().any(|u| u == url), public, "{id}");
    }
    assert_eq!(visible.len(), 3, "{visible:?}");
}

fn add_regex(f: &Fixture, never_save: bool, pattern: &str) {
    use memocampus_native_host::policy::{RuleSpec, TypedRule};
    let store = f.policy_store();
    let current = store.load().unwrap();
    let mut next = current.policy.clone();
    let layer = if never_save {
        &mut next.layers.never_save
    } else {
        &mut next.layers.save_but_no_mcp
    };
    layer.rules.push(RuleSpec::Typed(TypedRule::Regex {
        pattern: pattern.into(),
    }));
    store.update(&current, next).unwrap();
}

#[test]
fn regex_rules_keep_rows_recorded_before_them_out_of_the_public_read() {
    let f = Fixture::new();
    let db = writer(&f.vault);
    let rows = [
        ("blind", "https://rx.test/blind/CANARY", false),
        ("private", "https://rx.test/notes?CANARY=1", false),
        ("both", "https://both.test/CANARY", false),
        ("case", "https://rx.test/BLIND/CANARY", false),
        ("public", "https://rx.test/public", true),
        ("near", "https://rx.test/notes", true),
    ];
    for (id, url, _) in rows {
        visit(&db, id, url, "Title", &day(3));
    }
    let blob_id = blob(&db, 1);
    save(&db, blob_id, &PUBLIC_SAVE);
    let hidden_blob = blob(&db, 2);
    save(
        &db,
        hidden_blob,
        &Save {
            id: "rx-page",
            page: Some("https://rx.test/notes?CANARY=img"),
            ..PUBLIC_SAVE
        },
    );
    let image_blob = blob(&db, 3);
    save(
        &db,
        image_blob,
        &Save {
            id: "rx-image",
            image: "https://cdn.public.test/blind/CANARY.png",
            ..PUBLIC_SAVE
        },
    );
    drop(db);
    // Before the regexes everything is public.
    assert_eq!(
        f.store.search_visits(&ALL).unwrap().total,
        rows.len() as u64
    );
    let before = image_page(&f);
    assert_eq!(before.items.len(), 3);
    let before_ids: Vec<String> = before.items.iter().map(|item| item.id.clone()).collect();

    add_regex(&f, true, "(?i)/blind/");
    add_regex(&f, false, r"^https://rx\.test/notes\?");
    add_regex(&f, false, r"^https://both\.test/");
    add_regex(&f, true, r"^https://both\.test/CANARY$");
    let visible = f.urls();
    for (id, url, public) in rows {
        assert_eq!(visible.iter().any(|u| u == url), public, "{id}");
    }
    assert_eq!(visible.len(), 2, "{visible:?}");
    let after = image_page(&f);
    assert_clean(&after);
    assert_eq!(after.items.len(), 1, "only the public save's image");
    // A hidden image answers exactly as one that never existed.
    let unknown = sources_of(&f, "image_00000000000000000000000000000000").unwrap_err();
    let hidden: Vec<&String> = before_ids
        .iter()
        .filter(|id| !after.items.iter().any(|item| &item.id == *id))
        .collect();
    assert_eq!(hidden.len(), 2);
    for id in hidden {
        assert_eq!(sources_of(&f, id).unwrap_err(), unknown);
    }
    let public = &after.items[0];
    let sources = sources_of(&f, &public.id).unwrap();
    assert_clean(&sources);
}

fn add_domain(f: &Fixture, never_save: bool, pattern: &str) {
    use memocampus_native_host::policy::{RuleSpec, TypedRule};
    let store = f.policy_store();
    let current = store.load().unwrap();
    let mut next = current.policy.clone();
    let layer = if never_save {
        &mut next.layers.never_save
    } else {
        &mut next.layers.save_but_no_mcp
    };
    layer.rules.push(RuleSpec::Typed(TypedRule::DomainWildcard {
        pattern: pattern.into(),
    }));
    store.update(&current, next).unwrap();
}

#[test]
fn domain_wildcards_keep_rows_recorded_before_them_out_of_the_public_read() {
    let f = Fixture::new();
    let db = writer(&f.vault);
    let rows = [
        ("apex", "https://blind-zone.com/CANARY", false),
        ("sub", "https://a.b.blind-zone.com/CANARY", false),
        ("private", "https://www.private-zone.com/notes", false),
        ("upper", "https://WWW.PRIVATE-ZONE.COM./x", false),
        ("near", "https://fake-blind-zone.com/", true),
        ("suffix", "https://blind-zone.com.evil.com/", true),
        ("public", "https://public-zone.com/", true),
    ];
    for (id, url, _) in rows {
        visit(&db, id, url, "Title", &day(3));
    }
    drop(db);
    assert_eq!(
        f.store.search_visits(&ALL).unwrap().total,
        rows.len() as u64
    );

    add_domain(&f, true, "*.blind-zone.com");
    add_domain(&f, false, "*.private-zone.com");
    let visible = f.urls();
    for (id, url, public) in rows {
        assert_eq!(visible.iter().any(|u| u == url), public, "{id}");
    }
    assert_eq!(visible.len(), 3, "{visible:?}");
}

#[test]
fn an_approved_domain_hides_rows_and_lost_approvals_close_the_read() {
    let f = Fixture::new();
    let db = writer(&f.vault);
    for (id, url) in [
        ("shop", "https://shop.co.jp/a"),
        ("apex", "https://co.jp/"),
        ("other", "https://shop.ne.jp/"),
    ] {
        visit(&db, id, url, "Title", &day(3));
    }
    drop(db);
    let store = f.policy_store();
    store.approve_domain("co.jp").unwrap();
    add_domain(&f, false, "*.co.jp");
    assert_eq!(f.urls(), ["https://shop.ne.jp/"]);

    // The approvals cannot be read: the policy that needs one is unusable, and nothing is public.
    let settings = f.app.join("settings.sqlite3");
    std::fs::write(&settings, b"damaged").unwrap();
    assert_failed(f.store.search_visits(&ALL), VisibleError::PolicyUnavailable);
}
