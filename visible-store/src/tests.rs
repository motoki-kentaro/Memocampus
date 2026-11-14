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
    cursor: None,
};

#[test]
fn manually_saved_pages_never_join_any_existing_mcp_read_or_statistic() {
    let f = Fixture::new();
    f.seed();
    let before = f.store.search_visits(&ALL).unwrap();
    let domains = DomainStats::default();
    let stats_before = f.store.domain_stats(&domains).unwrap();
    let images_before = f.store.search_images(&SearchImages::default()).unwrap();
    let db = writer(&f.vault);
    for (i, url) in [
        "https://a-layer.test/CANARY-PAGE",
        "https://b-layer.test/CANARY-PAGE",
        "https://public.test/CANARY-PAGE",
    ]
    .iter()
    .enumerate()
    {
        db.execute("INSERT INTO page_blobs (id,sha256,byte_length,format,mime_type,data) VALUES (?1,?2,11,'mhtml','multipart/related',CAST('CANARY-PAGE' AS BLOB))", params![i as i64 + 1, format!("{i:064x}")]).unwrap();
        db.execute("INSERT INTO saved_pages (save_id,blob_id,url,title,captured_at,saved_at,incognito) VALUES (?1,?2,?3,'CANARY-PAGE-TITLE',0,0,0)", params![format!("page-{i}"),i as i64 + 1,url]).unwrap();
    }
    assert_eq!(f.store.search_visits(&ALL).unwrap(), before);
    assert_eq!(f.store.domain_stats(&domains).unwrap(), stats_before);
    assert_eq!(
        f.store.search_images(&SearchImages::default()).unwrap(),
        images_before
    );
    assert!(f
        .store
        .search_visits(&SearchVisits {
            query: "CANARY-PAGE",
            ..ALL
        })
        .unwrap()
        .items
        .is_empty());
    assert!(f
        .store
        .search_images(&SearchImages {
            query: "CANARY-PAGE",
            ..SearchImages::default()
        })
        .unwrap()
        .items
        .is_empty());
}

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

/// A plain vault read directly, without the publication gate.
fn plain(path: &Path) -> Target {
    Target {
        path: path.to_path_buf(),
        mode: VaultMode::Plain,
        token: None,
        vault_id: None,
        app_dir: PathBuf::new(),
    }
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
        let store = VisibleStore::ungated(vault.clone(), app.clone()).unwrap();
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
            page: None,
        };
        let set = VisibleSet::build(vec![row], &compiled(&policy), &key).unwrap();
        let total = set
            .query("%", (i64::MIN, i64::MAX), None, 10)
            .unwrap()
            .total;
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
        cursor: None,
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
            cursor: None,
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
                cursor: None,
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
                cursor: None,
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
    let other = VisibleStore::ungated(f.vault.clone(), f.app.clone()).unwrap();
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
    let store = VisibleStore::ungated(nested.clone(), f.app.clone()).unwrap();
    assert_failed(store.search_visits(&ALL), VisibleError::VaultUnavailable);
    assert!(!nested.parent().unwrap().exists());
    let directory = VisibleStore::ungated(f.dir.clone(), f.app.clone()).unwrap();
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
        let store = VisibleStore::ungated(path, f.app.clone()).unwrap();
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
        let store = VisibleStore::ungated(path, f.app.clone()).unwrap();
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
    let store = VisibleStore::ungated(path.clone(), f.app.clone()).unwrap();
    assert_failed(store.search_visits(&ALL), VisibleError::VaultSchemaMismatch);
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn page_metadata_reaches_a_reader_only_with_a_public_visit_and_a_vault_without_it_is_refused() {
    // Issue #8 publishes the metadata of a public URL (Issue #22). Before #8, no metadata was read
    // at all; private URLs keep every field to themselves, as their titles.
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
        if event_id.starts_with("pub-") {
            meta(
                &db,
                &url,
                &event_id,
                &format!("Meta {event_id}"),
                "2026-11-05T00:00:00.000Z",
            );
        } else {
            db.execute(
                "INSERT INTO page_metadata (url, event_id, title, description, og_title, og_description, og_site_name, og_image_url, favicon_url, captured_at) VALUES (?1, ?2, 'CANARY meta', 'CANARY desc', 'CANARY og', 'CANARY ogd', 'CANARY site', 'https://cdn.public.test/CANARY.png', 'https://cdn.public.test/CANARY.ico', '2026-11-05T00:00:00.000Z')",
                params![url, event_id],
            )
            .unwrap();
        }
    }
    let after = f.store.search_visits(&ALL).unwrap();
    assert_clean(&after);
    assert_eq!(after.total, before.total);
    let ids = |page: &VisitPage| page.items.iter().map(|v| v.id.clone()).collect::<Vec<_>>();
    assert_eq!(ids(&after), ids(&before));
    assert!(before.items.iter().all(|v| v.page.is_none()));
    let one = after
        .items
        .iter()
        .find(|v| v.url == "https://one.public.test/")
        .unwrap();
    let page = one.page.as_ref().unwrap();
    assert_eq!(page.description.as_deref(), Some("Meta pub-1 description"));
    assert_eq!(page.og_site_name.as_deref(), Some("Meta pub-1 site"));
    assert_eq!(
        page.og_image_url.as_deref(),
        Some("https://cdn.public.test/og.png?w=1")
    );
    assert_eq!(page.captured_at, "2026-11-05T00:00:00.000Z");
    for query in ["CANARY", "CANARY desc", "CANARY ogd", "CANARY site"] {
        let found = f
            .store
            .search_visits(&SearchVisits { query, ..ALL })
            .unwrap();
        assert_eq!(
            (found.total, found.items.len(), found.has_more),
            (0, 0, false),
            "{query}"
        );
    }

    // A vault shaped like schema 4 (no page_metadata) is not the current schema.
    let sql = schema::create_sql();
    let start = sql.find(" CREATE TABLE page_metadata").unwrap();
    let end = start + sql[start..].find(");").unwrap() + 2;
    let path = f.dir.join("schema-4-shape.sqlite3");
    Connection::open(&path)
        .unwrap()
        .execute_batch(&format!("{}{}", &sql[..start], &sql[end..]))
        .unwrap();
    let store = VisibleStore::ungated(path, f.app.clone()).unwrap();
    assert_failed(store.search_visits(&ALL), VisibleError::VaultSchemaMismatch);
    assert_failed(store.check_vault(), VisibleError::VaultSchemaMismatch);
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
    let store_for = |path: PathBuf| VisibleStore::ungated(path, f.app.clone()).unwrap();

    let empty = f.dir.join("empty.sqlite3");
    fs::write(&empty, b"").unwrap();
    assert_failed(
        store_for(empty.clone()).search_visits(&ALL),
        VisibleError::VaultSchemaMismatch,
    );
    assert_eq!(fs::metadata(&empty).unwrap().len(), 0);

    // No SQLite header in plain mode: a mode mismatch (perhaps an encrypted vault the config
    // misdescribes), never taken for a schema or damage problem (Issue #51 B0-4).
    let text = f.dir.join("text.sqlite3");
    fs::write(&text, "not a database CANARY ".repeat(50)).unwrap();
    assert_failed(
        store_for(text).search_visits(&ALL),
        VisibleError::VaultModeMismatch,
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
        snapshot::check_file(&other.vault, VaultMode::Plain),
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
                cursor: None,
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
    let store = VisibleStore::ungated(hot.clone(), f.app.clone()).unwrap();
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
    assert_failed(f.store.search_visits(&ALL), VisibleError::VaultModeMismatch);
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
    let store = VisibleStore::ungated(f.vault.clone(), empty.clone()).unwrap();
    assert_failed(store.search_visits(&ALL), VisibleError::PolicyUnavailable);
    assert!(!empty.exists());
    assert_failed(
        VisibleStore::ungated(f.vault.clone(), "relative".into()).map(|_| ()),
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
                cursor: None,
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
            page: None,
        })
        .chain([RawVisit {
            event_id: "pub".into(),
            url: "https://public.test/".into(),
            title: "Public".into(),
            visited_at: day(1),
            page: None,
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
    let other = VisibleStore::ungated(f.vault.clone(), f.app.clone()).unwrap();
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

    let uninitialized = VisibleStore::ungated(f.vault.clone(), f.dir.join("empty-app")).unwrap();
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
    assert_both(VisibleError::VaultModeMismatch);

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

    let raw = snapshot::read_visits(&plain(&f.vault), &mut |_| {}).unwrap();
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

    let raw = snapshot::read_image_saves(&plain(&f.vault), &mut |_| {}).unwrap();
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

// --- Issue #8: metadata, paging and domain counts ----------------------------------------------

/// A metadata row in the form the host writes, every text field derived from `name`.
fn meta(db: &Connection, url: &str, event_id: &str, name: &str, captured_at: &str) {
    db.execute(
        "INSERT INTO page_metadata (url, event_id, title, description, og_title, og_description, og_site_name, og_image_url, favicon_url, captured_at)
         VALUES (?1, ?2, ?3 || ' title', ?3 || ' description', ?3 || ' og', ?3 || ' og description', ?3 || ' site', 'https://cdn.public.test/og.png?w=1', 'https://cdn.public.test/favicon.ico', ?4)",
        params![url, event_id, name, captured_at],
    )
    .unwrap();
}

fn search(f: &Fixture, query: &str) -> VisitPage {
    let page = f
        .store
        .search_visits(&SearchVisits { query, ..ALL })
        .unwrap();
    assert_clean(&page);
    page
}

#[test]
fn every_metadata_text_field_is_searched_and_a_visit_without_metadata_still_is() {
    let f = Fixture::new();
    f.seed();
    let db = writer(&f.vault);
    visit(&db, "m", "https://news.public.test/a?id=7", "Doc", &day(5));
    meta(
        &db,
        "https://news.public.test/a?id=7",
        "m",
        "Kyoto 紅葉",
        &day(5),
    );
    visit(
        &db,
        "plain",
        "https://plain.public.test/",
        "No meta here",
        &day(6),
    );
    let urls = |page: VisitPage| page.items.into_iter().map(|v| v.url).collect::<Vec<_>>();
    for query in [
        "news.public",      // URL
        "Doc",              // visit title
        "紅葉 title",       // page title
        "紅葉 description", // description
        "紅葉 og",          // og:title
        "og description",   // og:description
        "紅葉 site",        // og:site_name
        "kyoto",            // ASCII case-insensitive, as for URLs and titles
    ] {
        assert_eq!(
            urls(search(&f, query)),
            ["https://news.public.test/a?id=7"],
            "{query}"
        );
    }
    // The metadata URLs name other resources and are not searched.
    assert!(search(&f, "favicon.ico").items.is_empty());
    assert!(search(&f, "og.png").items.is_empty());
    let plain = search(&f, "No meta");
    assert_eq!(urls(plain.clone()), ["https://plain.public.test/"]);
    assert_eq!(plain.items[0].page, None);
    // Metadata of no visit at all (another URL) is never searched or returned.
    meta(&db, "https://orphan.public.test/", "m", "Orphan", &day(5));
    assert!(search(&f, "Orphan").items.is_empty());
}

#[test]
fn every_visit_of_a_url_carries_the_url_s_latest_metadata() {
    let f = Fixture::new();
    let db = writer(&f.vault);
    let url = "https://repeat.public.test/";
    for (id, n) in [("r1", 1), ("r2", 2), ("r3", 3)] {
        visit(&db, id, url, "Repeat", &day(n));
    }
    meta(&db, url, "r2", "Latest", &day(2));
    let page = search(&f, "Latest og");
    assert_eq!(page.total, 3, "one row per visit, not per URL");
    assert!(page
        .items
        .iter()
        .all(|v| v.page.as_ref().unwrap().og_title.as_deref() == Some("Latest og")));
    let ids: std::collections::HashSet<_> = page.items.iter().map(|v| v.id.clone()).collect();
    assert_eq!(ids.len(), 3);
}

#[test]
fn metadata_urls_of_private_hosts_and_values_the_host_never_writes_are_left_out() {
    let f = Fixture::new();
    let db = writer(&f.vault);
    type Case = (
        &'static str,
        &'static str,
        &'static str,
        Option<&'static str>,
        Option<&'static str>,
    );
    let cases: [Case; 6] = [
        // (id, og_image_url, favicon_url, expected og_image_url, expected favicon_url)
        (
            "ok",
            "https://cdn.public.test/a.png",
            "https://cdn.public.test/i.ico",
            Some("https://cdn.public.test/a.png"),
            Some("https://cdn.public.test/i.ico"),
        ),
        (
            "a",
            "https://a-layer.test/CANARY.png",
            "https://b-layer.test/CANARY.ico",
            None,
            None,
        ),
        (
            "cred",
            "https://cdn.public.test/x.png?access_token=CANARY",
            "https://cdn.public.test/i.ico",
            None,
            Some("https://cdn.public.test/i.ico"),
        ),
        (
            "user",
            "https://u:CANARY@cdn.public.test/x.png",
            "https://cdn.public.test/login/CANARY",
            None,
            None,
        ),
        (
            "scheme",
            "javascript:CANARY",
            "data:image/png;base64,CANARY",
            None,
            None,
        ),
        (
            "blank",
            " https://cdn.public.test/CANARY.png",
            "https://cdn.public.test/i.ico",
            None,
            Some("https://cdn.public.test/i.ico"),
        ),
    ];
    for (n, (id, image, icon, _, _)) in cases.iter().enumerate() {
        let url = format!("https://page.public.test/{id}");
        visit(&db, id, &url, "Page", &day(n as u32 + 1));
        db.execute(
            "INSERT INTO page_metadata (url, event_id, og_title, og_image_url, favicon_url, captured_at) VALUES (?1, ?2, 'Shown', ?3, ?4, ?5)",
            params![url, id, image, icon, day(n as u32 + 1)],
        )
        .unwrap();
    }
    // Text not in the host's form (control characters, outer or double spaces) is absent, not repaired.
    visit(
        &db,
        "text",
        "https://page.public.test/text",
        "Page",
        &day(9),
    );
    db.execute(
        "INSERT INTO page_metadata (url, event_id, title, description, og_title, og_site_name, captured_at) VALUES ('https://page.public.test/text', 'text', ' CANARY', 'CANARY  two', ?1, 'kept', ?2)",
        params![format!("CANARY{}", '\u{7}'), day(9)],
    )
    .unwrap();
    // A timestamp of another form leaves out the whole metadata, never the visit.
    visit(
        &db,
        "time",
        "https://page.public.test/time",
        "Page",
        &day(10),
    );
    db.execute(
        "INSERT INTO page_metadata (url, event_id, og_title, captured_at) VALUES ('https://page.public.test/time', 'time', 'CANARY time', '2026-11-05 00:00:00.000Z')",
        [],
    )
    .unwrap();
    let page = f.store.search_visits(&ALL).unwrap();
    assert_clean(&page);
    assert_eq!(page.total, 8);
    for (id, _, _, image, icon) in cases {
        let visit = page
            .items
            .iter()
            .find(|v| v.url.ends_with(&format!("/{id}")))
            .unwrap();
        let meta = visit.page.as_ref().unwrap();
        assert_eq!(meta.og_title.as_deref(), Some("Shown"), "{id}");
        assert_eq!(
            (meta.og_image_url.as_deref(), meta.favicon_url.as_deref()),
            (image, icon),
            "{id}"
        );
    }
    let text = page
        .items
        .iter()
        .find(|v| v.url.ends_with("/text"))
        .unwrap();
    let meta = text.page.as_ref().unwrap();
    assert_eq!(
        (
            meta.title.clone(),
            meta.description.clone(),
            meta.og_title.clone()
        ),
        (None, None, None)
    );
    assert_eq!(meta.og_site_name.as_deref(), Some("kept"));
    let time = page
        .items
        .iter()
        .find(|v| v.url.ends_with("/time"))
        .unwrap();
    assert_eq!(time.page, None);
}

#[test]
fn metadata_of_a_wrong_type_is_left_out_and_the_visit_is_kept() {
    let f = Fixture::new();
    let db = writer(&f.vault);
    visit(&db, "v", "https://typed.public.test/", "Typed", &day(1));
    // The vault's CHECK constraints do not stop a BLOB: length() of a BLOB is its size.
    db.execute(
        "INSERT INTO page_metadata (url, event_id, og_title, captured_at) VALUES ('https://typed.public.test/', 'v', CAST('CANARY' AS BLOB), ?1)",
        [day(1)],
    )
    .unwrap();
    let page = f.store.search_visits(&ALL).unwrap();
    assert_clean(&page);
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].page, None);
}

#[test]
fn private_metadata_never_changes_a_total_a_page_or_a_cursor() {
    // The same public rows with and without private metadata matching the query.
    let f = Fixture::new();
    f.seed();
    let db = writer(&f.vault);
    for n in 1..=5 {
        let url = format!("https://list.public.test/{n}");
        visit(&db, &format!("l{n}"), &url, "List", &day(n));
        meta(&db, &url, &format!("l{n}"), "Shared word", &day(n));
    }
    let first = f
        .store
        .search_visits(&SearchVisits {
            query: "Shared word",
            limit: Some(2),
            ..ALL
        })
        .unwrap();
    for (id, url, _) in private_rows() {
        // Private visits whose metadata alone holds the query, newer than every public one.
        db.execute(
            "INSERT OR IGNORE INTO page_metadata (url, event_id, og_description, captured_at) VALUES (?1, ?2, 'Shared word CANARY', ?3)",
            params![url, id, day(20)],
        )
        .unwrap();
    }
    let again = f
        .store
        .search_visits(&SearchVisits {
            query: "Shared word",
            limit: Some(2),
            ..ALL
        })
        .unwrap();
    assert_clean(&again);
    assert_eq!(again, first);
    assert_eq!(again.total, 5);
    let only_private = search(&f, "CANARY");
    assert_eq!((only_private.total, only_private.has_more), (0, false));
}

#[test]
fn a_cursor_pages_through_public_rows_only_without_gaps_or_repeats() {
    let f = Fixture::new();
    f.seed();
    let db = writer(&f.vault);
    // Equal times too: the order is time, then ID.
    for n in 0..7 {
        visit(
            &db,
            &format!("p{n}"),
            &format!("https://page.public.test/{n}"),
            "Paged",
            &day(5 + n / 3),
        );
    }
    let request = |cursor: Option<&str>| {
        f.store
            .search_visits(&SearchVisits {
                query: "page.public",
                limit: Some(3),
                cursor,
                ..ALL
            })
            .unwrap()
    };
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let page = request(cursor.as_deref());
        assert_clean(&page);
        assert_eq!(page.total, 7, "the whole match, whatever the cursor");
        seen.extend(page.items.iter().map(|v| v.id.clone()));
        if !page.has_more {
            break;
        }
        cursor = Some(f.store.cursor_after(page.items.last().unwrap()).unwrap());
    }
    let all = f
        .store
        .search_visits(&SearchVisits {
            query: "page.public",
            ..ALL
        })
        .unwrap();
    assert_eq!(
        seen,
        all.items.iter().map(|v| v.id.clone()).collect::<Vec<_>>()
    );
    assert_eq!(seen.len(), 7);

    // The same pages over a day.
    let by_day = |cursor: Option<&str>| {
        f.store
            .get_visits_by_day(&VisitsByDay {
                date: "2026-10-05",
                utc_offset: "+00:00",
                limit: Some(2),
                cursor,
            })
            .unwrap()
    };
    let first = by_day(None);
    assert_eq!(
        (first.total, first.items.len(), first.has_more),
        (3, 2, true)
    );
    let next = by_day(Some(&f.store.cursor_after(&first.items[1]).unwrap()));
    assert_eq!((next.total, next.items.len(), next.has_more), (3, 1, false));
    assert!(!first.items.contains(&next.items[0]));
}

#[test]
fn a_row_that_became_private_is_skipped_by_the_next_page() {
    let f = Fixture::new();
    let db = writer(&f.vault);
    for n in 1..=4 {
        visit(
            &db,
            &format!("h{n}"),
            &format!("https://h{n}.public.test/"),
            "Hot",
            &day(n),
        );
    }
    let first = f
        .store
        .search_visits(&SearchVisits {
            query: "Hot",
            limit: Some(2),
            ..ALL
        })
        .unwrap();
    let cursor = f.store.cursor_after(first.items.last().unwrap()).unwrap();
    // The policy changes between two pages: the next page applies the new one.
    f.add_rule(false, "h2.public.test");
    let next = f
        .store
        .search_visits(&SearchVisits {
            query: "Hot",
            limit: Some(2),
            cursor: Some(&cursor),
            ..ALL
        })
        .unwrap();
    assert_eq!(
        next.items
            .iter()
            .map(|v| v.url.as_str())
            .collect::<Vec<_>>(),
        ["https://h1.public.test/"]
    );
    assert_eq!((next.total, next.has_more), (3, false));
}

#[test]
fn cursors_are_checked_before_any_read_and_belong_to_one_store() {
    let f = Fixture::new();
    f.seed();
    let page = f.store.search_visits(&ALL).unwrap();
    let cursor = f.store.cursor_after(&page.items[0]).unwrap();
    let other = VisibleStore::ungated(f.vault.clone(), f.app.clone()).unwrap();
    let (ms, rest) = cursor.split_once('.').unwrap();
    let (id, mac) = rest.split_once('.').unwrap();
    let mut flipped = mac.to_owned();
    flipped.replace_range(0..1, if mac.starts_with('0') { "1" } else { "0" });
    let forged = [
        String::new(),
        "x".into(),
        format!("{ms}.{id}"),
        format!("{ms}.{id}.{mac}.x"),
        format!("0{ms}.{id}.{mac}"),
        format!("-{ms}.{id}.{mac}"),
        format!("{}.{id}.{mac}", ms.parse::<i64>().unwrap() + 1),
        format!("{ms}.{}.{mac}", id.to_uppercase()),
        format!("{ms}.{id}.{flipped}"),
        format!("{ms}.{id}.{}", mac.to_uppercase()),
        format!("{ms}.visit_{}.{mac}", "0".repeat(32)),
        "9".repeat(400),
    ];
    for bad in &forged {
        assert_failed(
            f.store.search_visits(&SearchVisits {
                cursor: Some(bad),
                ..ALL
            }),
            VisibleError::InvalidCursor,
        );
    }
    assert_failed(
        other.search_visits(&SearchVisits {
            cursor: Some(&cursor),
            ..ALL
        }),
        VisibleError::InvalidCursor,
    );
    assert_eq!(VisibleError::InvalidCursor.code(), "invalid_cursor");
    // Checked before the policy or the vault is touched.
    fs::remove_file(&f.vault).unwrap();
    assert_failed(
        f.store.search_visits(&SearchVisits {
            cursor: Some("x"),
            ..ALL
        }),
        VisibleError::InvalidCursor,
    );
    assert!(f
        .store
        .search_visits(&SearchVisits {
            cursor: Some(&cursor),
            ..ALL
        })
        .is_err());
}

#[test]
fn domains_count_public_visits_per_host_and_private_rows_change_nothing() {
    let f = Fixture::new();
    let db = writer(&f.vault);
    let rows = [
        ("1", "https://www.example.test/a", 1),
        ("2", "https://www.example.test/a", 2), // the same URL again: counted again
        ("3", "https://WWW.Example.test./b", 3), // case and trailing dot as the policy sees them
        ("4", "https://www.example.test:8443/c", 3), // the port is not part of the domain
        ("5", "https://docs.example.test/", 4), // subdomains apart
        ("6", "https://other.test/", 5),
        ("7", "https://other.test/", 30), // outside the range below
    ];
    for (id, url, n) in rows {
        visit(&db, id, url, "T", &day(n));
    }
    let stats = |from: Option<&str>, to: Option<&str>, limit| {
        let page = f
            .store
            .domain_stats(&DomainStats { from, to, limit })
            .unwrap();
        assert_clean(&page);
        page
    };
    let to = day(10);
    let all = stats(None, Some(&to), None);
    let pairs = |page: &DomainStatsPage| {
        page.items
            .iter()
            .map(|item| (item.domain.clone(), item.visit_count))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        pairs(&all),
        [
            ("www.example.test".into(), 4),
            ("docs.example.test".into(), 1),
            ("other.test".into(), 1)
        ]
    );
    assert_eq!(
        (all.total_visits, all.total_domains, all.has_more),
        (6, 3, false)
    );
    assert_eq!(
        all.items.iter().map(|i| i.visit_count).sum::<u64>(),
        all.total_visits
    );
    let limited = stats(None, Some(&to), Some(1));
    assert_eq!(
        (
            limited.items.len(),
            limited.total_visits,
            limited.total_domains,
            limited.has_more
        ),
        (1, 6, 3, true)
    );
    let from = day(3);
    let ranged = stats(Some(&from), Some(&to), None);
    assert_eq!(ranged.total_visits, 4);

    // Private rows, before and inside the range, of public-looking and private hosts alike.
    for (id, url, title) in private_rows() {
        visit(&db, id, url, title, &day(4));
    }
    visit(
        &db,
        "priv-sub",
        "https://www.example.test/?password=CANARY",
        "CANARY",
        &day(4),
    );
    assert_eq!(stats(None, Some(&to), None), all);
    assert_eq!(stats(Some(&from), Some(&to), None), ranged);
    let empty = stats(Some(&day(28)), Some(&day(29)), None);
    assert_eq!(
        (
            empty.items.len(),
            empty.total_visits,
            empty.total_domains,
            empty.has_more
        ),
        (0, 0, 0, false)
    );
}

#[test]
fn domain_arguments_are_checked_like_search_arguments() {
    let f = Fixture::new();
    for request in [
        DomainStats {
            limit: Some(0),
            ..Default::default()
        },
        DomainStats {
            limit: Some(MAX_LIMIT + 1),
            ..Default::default()
        },
        DomainStats {
            from: Some("2026-10-01"),
            ..Default::default()
        },
        DomainStats {
            from: Some(&day(2)),
            to: Some(&day(1)),
            ..Default::default()
        },
    ] {
        assert_failed(
            f.store.domain_stats(&request),
            VisibleError::InvalidArgument,
        );
    }
    fs::remove_file(f.app.join("policy.json")).unwrap();
    assert_failed(
        f.store.domain_stats(&DomainStats::default()),
        VisibleError::PolicyUnavailable,
    );
}

#[test]
fn checking_the_vault_reads_nothing_and_reports_what_a_read_would() {
    let f = Fixture::new();
    f.seed();
    let before = f.fingerprint();
    f.store.check_vault().unwrap();
    assert_eq!(f.fingerprint(), before);
    assert!(!f.sidecars());
    // The policy is not needed to check the vault.
    fs::remove_file(f.app.join("policy.json")).unwrap();
    f.store.check_vault().unwrap();
    let missing = VisibleStore::ungated(f.dir.join("missing.sqlite3"), f.app.clone()).unwrap();
    assert_failed(missing.check_vault(), VisibleError::VaultUnavailable);
    assert!(!f.dir.join("missing.sqlite3").exists());
    let junk = f.dir.join("junk.sqlite3");
    fs::write(&junk, b"not a database at all, padded beyond one hundred bytes of header so that the read is complete .....").unwrap();
    let junk = VisibleStore::ungated(junk, f.app.clone()).unwrap();
    assert_failed(junk.check_vault(), VisibleError::VaultSchemaMismatch);
}

#[test]
fn an_image_is_found_by_its_type_within_the_range_of_its_public_saves() {
    let f = Fixture::new();
    let db = writer(&f.vault);
    let blob_id = blob(&db, 1);
    save(&db, blob_id, &PUBLIC_SAVE);
    let found = f
        .store
        .search_images(&SearchImages {
            query: "image/png",
            ..ALL_IMAGES
        })
        .unwrap();
    assert_eq!(found.total, 1);
    assert!(f
        .store
        .search_images(&SearchImages {
            query: "image/jpeg",
            ..ALL_IMAGES
        })
        .unwrap()
        .items
        .is_empty());
    let late = at(PUBLIC_SAVE.at - T1 + 1);
    assert!(f
        .store
        .search_images(&SearchImages {
            query: "png",
            from: Some(&late),
            ..ALL_IMAGES
        })
        .unwrap()
        .items
        .is_empty());
    // A private save of a JPEG is not found by its type.
    let jpeg = db
        .execute(
            "INSERT INTO image_blobs (sha256, mime_type, byte_length, width, height, created_at, last_saved_at, data) VALUES (printf('%064d', 2), 'image/jpeg', 1, 1, 1, 1, 1, x'00')",
            [],
        )
        .map(|_| db.last_insert_rowid())
        .unwrap();
    save(
        &db,
        jpeg,
        &Save {
            id: "private-jpeg",
            public: 0,
            ..PUBLIC_SAVE
        },
    );
    assert!(f
        .store
        .search_images(&SearchImages {
            query: "jpeg",
            ..ALL_IMAGES
        })
        .unwrap()
        .items
        .is_empty());
}

#[test]
fn navigation_events_never_reach_a_reader_and_a_vault_without_them_is_refused() {
    // Issue #41 stores navigations; nothing reads them yet. Rows between public and private
    // visits change no answer, no total and no domain count.
    let f = Fixture::new();
    f.seed();
    let stats = DomainStats::default();
    let before = (
        f.store.search_visits(&ALL).unwrap(),
        f.store.domain_stats(&stats).unwrap(),
    );
    let db = writer(&f.vault);
    db.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    for (target, source) in [
        ("pub-2", Some("pub-1")),
        ("pub-1", Some("priv-a")),
        ("priv-a", Some("pub-2")),
        ("priv-b", None),
    ] {
        db.execute(
            "INSERT INTO navigation_events VALUES (?1, ?2, 'document', 'link', 0, 0, 0, 0, 1, '2026-11-06T12:00:00.000Z')",
            params![target, source],
        )
        .unwrap();
    }
    drop(db);
    let after = (
        f.store.search_visits(&ALL).unwrap(),
        f.store.domain_stats(&stats).unwrap(),
    );
    assert_eq!(after, before);
    assert_clean(&after.0);

    // A vault shaped like schema 5 (no navigation_events) is not the current schema.
    let sql = schema::create_sql();
    let start = sql.find(" CREATE TABLE navigation_events").unwrap();
    let end = start + sql[start..].find(");").unwrap() + 2;
    let sql = format!("{}{}", &sql[..start], &sql[end..]).replace(
        " CREATE INDEX navigation_events_by_source ON navigation_events (source_visit_id);",
        "",
    );
    assert!(!sql.contains("navigation_events"));
    let path = f.dir.join("schema-5-shape.sqlite3");
    Connection::open(&path)
        .unwrap()
        .execute_batch(&sql.replace("PRAGMA user_version = 8;", "PRAGMA user_version = 5;"))
        .unwrap();
    let store = VisibleStore::ungated(path, f.app.clone()).unwrap();
    assert_failed(store.search_visits(&ALL), VisibleError::VaultSchemaMismatch);
    assert_failed(store.check_vault(), VisibleError::VaultSchemaMismatch);

    // Schema 6 (Issue #44: no image tags; the tag objects are the last ones created) is refused too.
    let sql = schema::create_sql();
    let start = sql.find(" CREATE TABLE image_tags ").unwrap();
    let schema_6 = sql[..start].replace("PRAGMA user_version = 8;", "PRAGMA user_version = 6;");
    assert!(!schema_6.contains("image_tag") && schema_6.contains("user_version = 6;"));
    let path = f.dir.join("schema-6-shape.sqlite3");
    Connection::open(&path)
        .unwrap()
        .execute_batch(&schema_6)
        .unwrap();
    let store = VisibleStore::ungated(path, f.app.clone()).unwrap();
    assert_failed(store.search_visits(&ALL), VisibleError::VaultSchemaMismatch);
    assert_failed(store.check_vault(), VisibleError::VaultSchemaMismatch);
}

#[test]
fn image_tags_never_reach_a_reader() {
    // Issue #44 stores tags for local use only. Tags on a public and on a private image change no
    // image answer, no total and no source, are never searchable, and never appear in any answer.
    let f = Fixture::new();
    let db = writer(&f.vault);
    db.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    let public = blob(&db, 1);
    save(&db, public, &PUBLIC_SAVE);
    let private = blob(&db, 2);
    save(
        &db,
        private,
        &Save {
            id: "private-save",
            public: 0,
            ..PUBLIC_SAVE
        },
    );
    let search = |query: &str| {
        f.store
            .search_images(&SearchImages {
                query,
                ..ALL_IMAGES
            })
            .unwrap()
    };
    let before = image_page(&f);
    let sources_before = sources_of(&f, &before.items[0].id).unwrap();
    db.execute_batch(&format!(
        "INSERT INTO image_tags (id, name) VALUES (1, 'secret-tag'), (2, 'shared-tag');
         INSERT INTO image_tag_links VALUES ({public}, 1), ({public}, 2), ({private}, 2);"
    ))
    .unwrap();
    let after = image_page(&f);
    assert_eq!(after, before);
    assert_eq!((after.total, after.items.len()), (1, 1));
    assert_eq!(sources_of(&f, &after.items[0].id).unwrap(), sources_before);
    for query in ["secret-tag", "shared-tag", "tag"] {
        assert!(search(query).items.is_empty(), "{query}");
    }
    let text = format!("{after:?}{sources_before:?}");
    assert!(!text.contains("-tag"), "{text}");
    assert_eq!(f.store.check_vault(), Ok(()));
}

// --- Issue #51, Slice B0: publication gate (B0-1, B0-2) and vault mode (B0-4) ------------------

/// Names `vault` as the active vault of `app` in `mode`, with a publication `state` for its file.
/// The vault id `activate` records for an encrypted vault.
const ENCRYPTED_ID: &str = "33333333333333333333333333333333";

fn activate(app: &Path, vault: &Path, mode: &str, state: Option<&str>) {
    let mut config = serde_json::json!({"version": 2, "activeVault": vault, "mode": mode});
    if mode == "encrypted" {
        config["vaultId"] = ENCRYPTED_ID.into();
    }
    if let Some(state) = state {
        config["publication"] = serde_json::json!({
            "vaultToken": active_vault::vault_token(vault).unwrap(),
            "state": state,
        });
    }
    fs::create_dir_all(app).unwrap();
    fs::write(app.join("config.json"), config.to_string()).unwrap();
}

/// The fixture's vault (public rows 1 and 2, private canaries) and a second vault `b` with one
/// public row of its own.
fn two_vaults(f: &Fixture) -> PathBuf {
    f.seed();
    let b = f.dir.join("b.sqlite3");
    create_vault(&b, APP_ID, SUPPORTED_SCHEMA);
    visit(&writer(&b), "b-1", "https://b.public.test/", "B", &day(3));
    b
}

#[test]
fn a_following_store_reads_whichever_vault_is_active_and_published_now() {
    let f = Fixture::new();
    let b = two_vaults(&f);
    let store = VisibleStore::follow_active(f.app.clone()).unwrap();
    let urls = |store: &VisibleStore| -> Result<Vec<String>, VisibleError> {
        let page = store.search_visits(&ALL)?;
        assert_clean(&page);
        Ok(page.items.into_iter().map(|item| item.url).collect())
    };
    assert_eq!(urls(&store), Err(VisibleError::VaultNotConfigured));
    activate(&f.app, &f.vault, "plain", Some("confirmed"));
    assert_eq!(urls(&store).unwrap().len(), 2);
    activate(&f.app, &b, "plain", Some("confirmed"));
    assert_eq!(
        urls(&store).unwrap(),
        ["https://b.public.test/"],
        "the old vault still exists and is never read"
    );
    assert!(f.vault.exists());
    // Back to A, but A is not confirmed in this config: nothing.
    activate(&f.app, &f.vault, "plain", Some("pending"));
    assert_eq!(urls(&store), Err(VisibleError::VaultPublicationPending));
    activate(&f.app, &f.vault, "plain", None);
    assert_eq!(urls(&store), Err(VisibleError::VaultPublicationPending));
    let day_request = VisitsByDay {
        date: "2026-10-01",
        utc_offset: "+00:00",
        ..VisitsByDay::default()
    };
    for (name, read) in [
        (
            "domains",
            store.domain_stats(&DomainStats::default()).map(|_| ()),
        ),
        ("images", store.search_images(&ALL_IMAGES).map(|_| ())),
        ("day", store.get_visits_by_day(&day_request).map(|_| ())),
        ("check", store.check_vault()),
    ] {
        assert_eq!(read, Err(VisibleError::VaultPublicationPending), "{name}");
    }
}

#[test]
fn a_pinned_store_reads_only_while_its_file_is_the_published_active_vault() {
    let f = Fixture::new();
    let b = two_vaults(&f);
    let pinned = VisibleStore::pinned(f.vault.clone(), f.app.clone()).unwrap();
    activate(&f.app, &f.vault, "plain", Some("confirmed"));
    assert_eq!(pinned.search_visits(&ALL).unwrap().total, 2);
    pinned.check_vault().unwrap();
    activate(&f.app, &b, "plain", Some("confirmed"));
    assert_failed(pinned.search_visits(&ALL), VisibleError::VaultNotActive);
    assert_failed(pinned.check_vault(), VisibleError::VaultNotActive);
    activate(&f.app, &f.vault, "plain", Some("confirmed"));
    if cfg!(windows) {
        // The pinned path need not be spelled like the config's.
        let spelled = PathBuf::from(f.vault.to_string_lossy().to_uppercase());
        let other_spelling = VisibleStore::pinned(spelled, f.app.clone()).unwrap();
        assert_eq!(other_spelling.search_visits(&ALL).unwrap().total, 2);
    }
    let missing = VisibleStore::pinned(f.dir.join("none.sqlite3"), f.app.clone()).unwrap();
    assert_failed(missing.search_visits(&ALL), VisibleError::VaultNotActive);
    assert!(!f.dir.join("none.sqlite3").exists());
}

#[test]
fn the_gate_refuses_before_the_policy_and_fails_closed_on_every_problem() {
    let f = Fixture::new();
    f.seed();
    let store = VisibleStore::follow_active(f.app.clone()).unwrap();
    let vault_before = fs::read(&f.vault).unwrap();
    // Gate first: a pending vault is refused as pending even without a policy.
    activate(&f.app, &f.vault, "plain", Some("pending"));
    let away = f.dir.join("policy.json.away");
    fs::rename(f.app.join("policy.json"), &away).unwrap();
    assert_failed(
        store.search_visits(&ALL),
        VisibleError::VaultPublicationPending,
    );
    // Confirmed, but no policy: still nothing.
    activate(&f.app, &f.vault, "plain", Some("confirmed"));
    assert_failed(store.search_visits(&ALL), VisibleError::PolicyUnavailable);
    fs::rename(&away, f.app.join("policy.json")).unwrap();
    assert_eq!(store.search_visits(&ALL).unwrap().total, 2);
    // A version 1 config is never published, whatever it names.
    let legacy = serde_json::json!({"version": 1, "activeVault": f.vault}).to_string();
    fs::write(f.app.join("config.json"), legacy).unwrap();
    assert_failed(
        store.search_visits(&ALL),
        VisibleError::VaultPublicationPending,
    );
    let relative = r#"{"version":2,"activeVault":"relative","mode":"plain"}"#;
    for text in ["{\"version\":2,", "", relative] {
        fs::write(f.app.join("config.json"), text).unwrap();
        assert_failed(store.search_visits(&ALL), VisibleError::ConfigInvalid);
    }
    fs::remove_file(f.app.join("config.json")).unwrap();
    assert_failed(store.search_visits(&ALL), VisibleError::VaultNotConfigured);
    // The published vault disappears: unavailable, and nothing is created in its place.
    activate(&f.app, &f.vault, "plain", Some("confirmed"));
    let moved = f.dir.join("moved.sqlite3");
    fs::rename(&f.vault, &moved).unwrap();
    assert_failed(store.search_visits(&ALL), VisibleError::VaultUnavailable);
    assert!(!f.vault.exists());
    // A different file at the same path is another vault: its approval is not inherited.
    fs::copy(&moved, &f.vault).unwrap();
    assert_failed(
        store.search_visits(&ALL),
        VisibleError::VaultPublicationPending,
    );
    fs::remove_file(&f.vault).unwrap();
    fs::rename(&moved, &f.vault).unwrap();
    assert_eq!(store.search_visits(&ALL).unwrap().total, 2);
    assert_eq!(
        fs::read(&f.vault).unwrap(),
        vault_before,
        "the gate never writes the vault"
    );
}

#[test]
fn a_switch_or_revocation_during_a_read_discards_the_read() {
    for at in [
        Stage::Admitted,
        Stage::PolicyLoaded,
        Stage::SchemaChecked,
        Stage::VaultClosed,
    ] {
        for revoke in [false, true] {
            let f = Fixture::new();
            let b = two_vaults(&f);
            activate(&f.app, &f.vault, "plain", Some("confirmed"));
            let store = VisibleStore::follow_active(f.app.clone()).unwrap();
            let mut done = false;
            let result = store.search_visits_staged(&ALL, &mut |stage| {
                if stage == at && !done {
                    done = true;
                    if revoke {
                        activate(&f.app, &f.vault, "plain", Some("pending"));
                    } else {
                        activate(&f.app, &b, "plain", Some("confirmed"));
                    }
                }
            });
            assert!(done);
            assert_eq!(
                result.err(),
                Some(VisibleError::VaultSwitched),
                "{at:?} revoke={revoke}"
            );
            // The next read follows the new state.
            let next = store.search_visits(&ALL);
            if revoke {
                assert_failed(next, VisibleError::VaultPublicationPending);
            } else {
                assert_eq!(next.unwrap().items[0].url, "https://b.public.test/");
            }
        }
    }
}

/// Issue #51, F-02. The host re-initializes a lost policy in this order: the publication is set
/// back to pending, then the new (default, more permissive) policy is written. A reader sees
/// nothing at every point of it, including a read that passed the gate before the withdrawal.
#[test]
fn a_policy_reinitialized_after_a_loss_is_never_read_under_the_old_approval() {
    let lose_policy = |app: &Path| {
        for name in ["policy.json", "policy.initialized"] {
            fs::remove_file(app.join(name)).unwrap();
        }
    };
    let f = Fixture::new();
    f.seed();
    activate(&f.app, &f.vault, "plain", Some("confirmed"));
    let store = VisibleStore::follow_active(f.app.clone()).unwrap();
    assert_eq!(store.search_visits(&ALL).unwrap().total, 2);

    // Lost policy: nothing, whatever the publication says.
    lose_policy(&f.app);
    assert_failed(store.search_visits(&ALL), VisibleError::PolicyUnavailable);
    // Withdrawn, not yet initialized.
    activate(&f.app, &f.vault, "plain", Some("pending"));
    assert_failed(
        store.search_visits(&ALL),
        VisibleError::VaultPublicationPending,
    );
    // Initialized: still pending until the user confirms again.
    f.policy_store().initialize().unwrap();
    assert_failed(
        store.search_visits(&ALL),
        VisibleError::VaultPublicationPending,
    );

    // A read admitted under the old approval (with no policy) while both steps happen: the new
    // policy loads, but the gate checked again after the read sees the withdrawal.
    lose_policy(&f.app);
    activate(&f.app, &f.vault, "plain", Some("confirmed"));
    let mut done = false;
    let result = store.search_visits_staged(&ALL, &mut |stage| {
        if stage == Stage::Admitted && !done {
            done = true;
            activate(&f.app, &f.vault, "plain", Some("pending"));
            f.policy_store().initialize().unwrap();
        }
    });
    assert!(done);
    assert_eq!(result.err(), Some(VisibleError::VaultSwitched));

    // What the withdrawal prevents: the same new policy under the old approval publishes a row
    // that only the lost policy hid (layer B).
    activate(&f.app, &f.vault, "plain", Some("confirmed"));
    let exposed = store.search_visits(&ALL).unwrap();
    assert!(exposed
        .items
        .iter()
        .any(|item| item.url.starts_with("https://b-layer.test/")));
}

#[test]
fn a_switch_away_and_back_during_a_read_still_reads_the_one_published_vault() {
    let f = Fixture::new();
    let b = two_vaults(&f);
    activate(&f.app, &f.vault, "plain", Some("confirmed"));
    let store = VisibleStore::follow_active(f.app.clone()).unwrap();
    let page = store
        .search_visits_staged(&ALL, &mut |stage| {
            if stage == Stage::SchemaChecked {
                activate(&f.app, &b, "plain", Some("confirmed"));
                activate(&f.app, &f.vault, "plain", Some("confirmed"));
            }
        })
        .unwrap();
    assert_clean(&page);
    assert_eq!(page.total, 2);
}

#[test]
fn a_file_replaced_between_the_gate_and_the_open_is_never_read() {
    let f = Fixture::new();
    let b = two_vaults(&f);
    activate(&f.app, &f.vault, "plain", Some("confirmed"));
    let store = VisibleStore::follow_active(f.app.clone()).unwrap();
    let aside = f.dir.join("aside.sqlite3");
    let result = store.search_visits_staged(&ALL, &mut |stage| {
        if stage == Stage::Admitted {
            // Same path, another file (B's content) after the gate took A's identity.
            fs::rename(&f.vault, &aside).unwrap();
            fs::copy(&b, &f.vault).unwrap();
        }
    });
    assert_eq!(result.err(), Some(VisibleError::VaultSwitched));
}

#[test]
fn the_mode_comes_from_the_config_and_a_contradicting_file_is_never_misread() {
    let f = Fixture::new();
    f.seed();
    let store = VisibleStore::follow_active(f.app.clone()).unwrap();
    let encrypted = f.dir.join("encrypted.sqlite3");
    let salt_first: Vec<u8> = (0..8192u32).map(|i| (i * 31 + 7) as u8).collect();
    fs::write(&encrypted, &salt_first).unwrap();

    // Slice B: an encrypted vault whose key this user's key store does not hold is locked; it is
    // never opened without a key.
    activate(&f.app, &encrypted, "encrypted", Some("confirmed"));
    assert_failed(store.search_visits(&ALL), VisibleError::VaultLocked);
    assert_failed(store.check_vault(), VisibleError::VaultLocked);
    // A plain SQLite file recorded as encrypted, and a salt-first file recorded as plain.
    activate(&f.app, &f.vault, "encrypted", Some("confirmed"));
    assert_failed(store.search_visits(&ALL), VisibleError::VaultModeMismatch);
    activate(&f.app, &encrypted, "plain", Some("confirmed"));
    assert_failed(store.search_visits(&ALL), VisibleError::VaultModeMismatch);
    // Unknown modes are not configs.
    for mode in ["sqlcipher", "Plain", "", "auto"] {
        activate(&f.app, &f.vault, mode, Some("confirmed"));
        assert_failed(store.search_visits(&ALL), VisibleError::ConfigInvalid);
    }
    // An empty file cannot be an encrypted vault either.
    let empty = f.dir.join("empty.sqlite3");
    fs::write(&empty, b"").unwrap();
    let ungated =
        VisibleStore::ungated_in(empty.clone(), f.app.clone(), VaultMode::Encrypted).unwrap();
    assert_failed(ungated.search_visits(&ALL), VisibleError::VaultModeMismatch);
    // Plain mode keeps every existing behaviour.
    activate(&f.app, &f.vault, "plain", Some("confirmed"));
    assert_eq!(store.search_visits(&ALL).unwrap().total, 2);
    assert_eq!(
        fs::read(&encrypted).unwrap(),
        salt_first,
        "never opened without its key"
    );
    assert_eq!(fs::metadata(&empty).unwrap().len(), 0);
    for suffix in ["-journal", "-wal", "-shm"] {
        let mut name = encrypted.as_os_str().to_owned();
        name.push(suffix);
        assert!(!Path::new(&name).exists());
    }
}

#[test]
fn no_reader_can_write_the_config_or_a_publication_decision() {
    // Issue #51: publication decisions are made only by the Native Host for the side panel. The
    // read path for MCP reads `config.json` through `active_vault` and has no way to write it.
    for (name, source) in source_files() {
        for banned in [
            "to_bytes",
            "fs::write",
            "OpenOptions",
            "File::create",
            "fs::rename",
            "remove_file",
            "CONFIG_FILE",
        ] {
            assert!(!source.contains(banned), "{name}: {banned}");
        }
    }
}

// --- Issue #51 Slice B: encrypted vaults ---------------------------------------------------------

mod encrypted {
    use super::*;
    use memocampus_native_host::secure_vault::secret::{CipherSalt, VaultId, VaultKey};
    use memocampus_native_host::secure_vault::{cipher, key_store};

    /// An encrypted vault beside the fixture's plain one, with the same public and private rows,
    /// its key in the fixture's key store. Returns (path, id, salt, key).
    fn encrypted_vault(f: &Fixture, name: &str) -> (PathBuf, VaultId, CipherSalt, VaultKey) {
        let path = f.dir.join(name);
        let key = VaultKey::generate().unwrap();
        let salt = CipherSalt::generate().unwrap();
        cipher::create(&path, &key, &salt).unwrap();
        let id = memocampus_native_host::secure_vault::recovery::vault_id(&salt);
        key_store::store(&f.app, &id, &salt, &key, key_store::Write::New).unwrap();
        let db = cipher::open(&path, &key, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE).unwrap();
        visit(&db, "pub-1", "https://one.public.test/", "One", &day(1));
        visit(&db, "pub-2", "https://two.public.test/", "Two", &day(2));
        for (row, url, title) in private_rows() {
            visit(&db, row, url, title, &day(20));
        }
        drop(db);
        (path, id, salt, key)
    }

    fn publish(app: &Path, vault: &Path, id: &VaultId, state: &str) {
        let config = serde_json::json!({
            "version": 2, "activeVault": vault, "mode": "encrypted", "vaultId": id.to_hex(),
            "publication": {"vaultToken": active_vault::vault_token(vault).unwrap(), "state": state},
        });
        fs::write(app.join("config.json"), config.to_string()).unwrap();
    }

    #[test]
    fn a_published_encrypted_vault_is_read_through_the_same_privacy_gate() {
        let f = Fixture::new();
        f.seed();
        let (path, id, _, key) = encrypted_vault(&f, "enc.sqlite3");
        let store = VisibleStore::follow_active(f.app.clone()).unwrap();
        publish(&f.app, &path, &id, "pending");
        assert_failed(
            store.search_visits(&ALL),
            VisibleError::VaultPublicationPending,
        );
        publish(&f.app, &path, &id, "confirmed");
        let page = store.search_visits(&ALL).unwrap();
        assert_clean(&page);
        assert_eq!(page.total, 2, "the policy hides the private rows");
        assert_eq!(page.items[0].url, "https://two.public.test/");
        store.check_vault().unwrap();
        assert!(!key.appears_in(format!("{page:?}").as_bytes()));
        // Privacy rules apply at once, as for a plain vault.
        f.add_rule(false, "two.public.test");
        let page = store.search_visits(&ALL).unwrap();
        assert_eq!(page.total, 1);
        // The plain vault is never read in its place.
        assert_eq!(page.items[0].url, "https://one.public.test/");
        let leftovers: Vec<_> = ["-journal", "-wal", "-shm"]
            .iter()
            .filter(|suffix| {
                let mut name = path.as_os_str().to_owned();
                name.push(suffix);
                Path::new(&name).exists()
            })
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn no_key_a_wrong_key_entry_or_a_swapped_file_never_reads_and_never_falls_back() {
        let f = Fixture::new();
        f.seed();
        let (path, id, salt, _) = encrypted_vault(&f, "enc.sqlite3");
        let store = VisibleStore::follow_active(f.app.clone()).unwrap();
        publish(&f.app, &path, &id, "confirmed");
        assert_eq!(store.search_visits(&ALL).unwrap().total, 2);
        let entry = key_store::entry_path(&f.app, &id);
        let saved = fs::read(&entry).unwrap();

        // No key for this user: locked; nothing else (the plain vault) is read instead.
        fs::remove_file(&entry).unwrap();
        assert_failed(store.search_visits(&ALL), VisibleError::VaultLocked);
        assert_failed(store.check_vault(), VisibleError::VaultLocked);
        // Another vault's entry under this id: locked.
        let (other, other_id, _, _) = encrypted_vault(&f, "other.sqlite3");
        fs::copy(key_store::entry_path(&f.app, &other_id), &entry).unwrap();
        assert_failed(store.search_visits(&ALL), VisibleError::VaultLocked);
        fs::write(&entry, &saved).unwrap();
        assert_eq!(store.search_visits(&ALL).unwrap().total, 2);

        // A file carrying this vault's salt but another key's pages: the key is refused.
        let bytes = fs::read(&path).unwrap();
        let mut forged = fs::read(&other).unwrap();
        forged[..16].copy_from_slice(salt.as_bytes());
        let aside = f.dir.join("aside.sqlite3");
        fs::rename(&path, &aside).unwrap();
        fs::write(&path, &forged).unwrap();
        publish(&f.app, &path, &id, "confirmed");
        assert_failed(store.search_visits(&ALL), VisibleError::VaultDecryptFailed);
        // Damaged pages after the salt: refused, never reported as a schema problem.
        let mut damaged = bytes.clone();
        for byte in &mut damaged[16..2048] {
            *byte ^= 0x55;
        }
        fs::remove_file(&path).unwrap();
        fs::write(&path, &damaged).unwrap();
        publish(&f.app, &path, &id, "confirmed");
        let error = store.search_visits(&ALL).unwrap_err();
        assert!(
            matches!(
                error,
                VisibleError::VaultDecryptFailed | VisibleError::VaultCorrupt
            ),
            "{error:?}"
        );
        // The original file back under a new identity: pending until confirmed again.
        fs::remove_file(&path).unwrap();
        fs::copy(&aside, &path).unwrap();
        publish(&f.app, &path, &id, "confirmed");
        assert_eq!(store.search_visits(&ALL).unwrap().total, 2);
        assert_eq!(
            fs::read(&aside).unwrap(),
            bytes,
            "reads never change the vault"
        );
    }

    #[test]
    fn a_switch_between_encrypted_and_plain_vaults_during_a_read_discards_it() {
        for at in [Stage::Admitted, Stage::PolicyLoaded, Stage::SchemaChecked] {
            let f = Fixture::new();
            f.seed();
            let (path, id, _, _) = encrypted_vault(&f, "enc.sqlite3");
            let store = VisibleStore::follow_active(f.app.clone()).unwrap();
            publish(&f.app, &path, &id, "confirmed");
            let mut done = false;
            let result = store.search_visits_staged(&ALL, &mut |stage| {
                if stage == at && !done {
                    done = true;
                    activate(&f.app, &f.vault, "plain", Some("confirmed"));
                }
            });
            assert!(done);
            assert_eq!(result.err(), Some(VisibleError::VaultSwitched), "{at:?}");
            // And back: the encrypted vault keeps its approval only for its own file.
            assert_eq!(store.search_visits(&ALL).unwrap().total, 2);
            publish(&f.app, &path, &id, "confirmed");
            assert_eq!(store.search_visits(&ALL).unwrap().total, 2);
        }
    }

    #[test]
    fn an_encrypted_file_recorded_without_its_id_or_as_plain_is_never_misread() {
        let f = Fixture::new();
        let (path, _, _, _) = encrypted_vault(&f, "enc.sqlite3");
        let ungated = VisibleStore::ungated_encrypted(path.clone(), f.app.clone(), None).unwrap();
        assert_failed(ungated.search_visits(&ALL), VisibleError::VaultLocked);
        activate(&f.app, &path, "plain", Some("confirmed"));
        let store = VisibleStore::follow_active(f.app.clone()).unwrap();
        assert_failed(store.search_visits(&ALL), VisibleError::VaultModeMismatch);
    }
}
