//! Synthetic data and temporary vaults only.
use super::*;
use crate::{
    pages,
    search::{search_pages, SearchRequest},
    vault::{tests::TempDir, Session},
};
use rusqlite::params;

const A: &str = "https://a.test/page?x=1";
const B: &str = "https://a.test/page?x=2";
const C: &str = "https://c.test/";

type Fixture = (TempDir, Session, Manager, Value);

fn fixture() -> Fixture {
    let tmp = TempDir::new();
    let env = tmp.env();
    let path = tmp.0.join("item.sqlite3");
    vault::create_vault(&path).unwrap();
    let lock = ConfigLock::acquire(&env).unwrap();
    vault::switch_active(&env, &lock, &path, false).unwrap();
    drop(lock);
    let mut session = Session::new(env);
    let mut manager = Manager::default();
    let binding = manager.handle(&mut session, &json!({"type":"item_state","requestId":"s"}));
    assert_eq!(binding["ok"], true, "{binding}");
    (tmp, session, manager, binding)
}
fn call(m: &mut Manager, s: &mut Session, b: &Value, kind: &str, extra: Value) -> Value {
    let mut message = json!({"type":kind,"requestId":"t","vaultToken":b["vaultToken"],"generation":b["generation"]});
    message
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    m.handle(s, &message)
}
fn run(s: &mut Session, sql: &str) {
    s.write(|db| {
        db.execute_batch(sql)?;
        Ok(0)
    })
    .unwrap();
}
fn visit(s: &mut Session, id: &str, url: &str, title: &str, at: &str) {
    s.write(|db| {
        db.execute(
            "INSERT INTO visits VALUES (?1, ?2, ?3, ?4)",
            params![id, url, title, at],
        )
    })
    .unwrap();
}
fn at(day: u32) -> String {
    format!("2026-11-{day:02}T10:00:00.000Z")
}
fn count(s: &mut Session, table: &str) -> i64 {
    s.read(|db| {
        db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    })
    .unwrap()
}
fn dump(s: &mut Session) -> Vec<String> {
    s.read(|db| {
        let mut st = db
            .prepare(
                "SELECT 'v ' || event_id || ' ' || url FROM visits
                 UNION ALL SELECT 's ' || event_id || ' ' || url FROM tab_snapshots
                 UNION ALL SELECT 'p ' || url || ' ' || event_id FROM page_metadata
                 UNION ALL SELECT 'n ' || target_visit_id || ' <- ' || coalesce(source_visit_id, 'NULL') || CASE WHEN source_censored = 1 THEN ' CENSORED' ELSE '' END FROM navigation_events
                 UNION ALL SELECT 'b ' || id FROM image_blobs
                 UNION ALL SELECT 'sv ' || save_id FROM saved_images
                 UNION ALL SELECT 'th ' || blob_id FROM image_thumbnails
                 UNION ALL SELECT 'l ' || blob_id || ' ' || t.name FROM image_tag_links JOIN image_tags t ON t.id = tag_id
                 UNION ALL SELECT 't ' || name FROM image_tags
                 UNION ALL SELECT 'pg ' || id FROM saved_pages
                 ORDER BY 1",
            )
            .unwrap();
        st.query_map([], |r| r.get(0)).unwrap().collect::<rusqlite::Result<Vec<String>>>().unwrap()
    })
    .unwrap()
}
fn integrity(s: &mut Session) {
    s.read(|db| {
        assert_eq!(
            db.query_row::<String, _, _>("PRAGMA integrity_check", [], |r| r.get(0))
                .unwrap(),
            "ok"
        );
        assert_eq!(
            db.query_row::<i64, _, _>("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r
                .get(0))
                .unwrap(),
            0
        );
    })
    .unwrap();
}
fn preview(m: &mut Manager, s: &mut Session, b: &Value, target: Value) -> Value {
    call(m, s, b, "item_delete_preview", json!({"target": target}))
}
fn confirm(m: &mut Manager, s: &mut Session, b: &Value, p: &Value) -> Value {
    call(m, s, b, "item_delete", json!({"reviewId": p["reviewId"]}))
}
fn without(rows: &[String], gone: &[&str]) -> Vec<String> {
    rows.iter()
        .filter(|r| !gone.contains(&r.as_str()))
        .cloned()
        .collect()
}

/// A: a1 -> a2 (inside A), a3 (carries A's metadata); B: b1 (from a3); C: c1, c2 (c1 -> a3).
fn urls(s: &mut Session) {
    for (id, url, day) in [
        ("a1", A, 1),
        ("a2", A, 2),
        ("a3", A, 3),
        ("b1", B, 4),
        ("c1", C, 5),
        ("c2", C, 6),
    ] {
        visit(s, id, url, &format!("title {id}"), &at(day));
    }
    run(
        s,
        &format!(
            "INSERT INTO navigation_events (target_visit_id, source_visit_id, kind, transition_type, client_redirect, server_redirect, forward_back, from_address_bar, new_tab, occurred_at) VALUES ('a2','a1','document','link',0,0,0,0,0,'{t}');
             INSERT INTO navigation_events (target_visit_id, source_visit_id, kind, transition_type, client_redirect, server_redirect, forward_back, from_address_bar, new_tab, occurred_at) VALUES ('b1','a3','document','link',0,0,0,0,0,'{t}');
             INSERT INTO navigation_events (target_visit_id, source_visit_id, kind, transition_type, client_redirect, server_redirect, forward_back, from_address_bar, new_tab, occurred_at) VALUES ('a3','c1','document','link',0,0,0,0,0,'{t}');
             INSERT INTO navigation_events (target_visit_id, source_visit_id, kind, transition_type, client_redirect, server_redirect, forward_back, from_address_bar, new_tab, occurred_at) VALUES ('c2','c1','document','link',0,0,0,0,0,'{t}');
             INSERT INTO tab_snapshots VALUES ('s1',1,0,'{A}','A',0,0,'{t}');
             INSERT INTO tab_snapshots VALUES ('s2',1,1,'{C}','C',0,0,'{t}');
             INSERT INTO page_metadata (url,event_id,title,captured_at) VALUES ('{A}','a3','A','{t}');
             INSERT INTO page_metadata (url,event_id,title,captured_at) VALUES ('{B}','b1','B','{t}');",
            t = at(1)
        ),
    );
}
fn image(s: &mut Session, id: i64, sha: char, saves: i64) {
    run(s, &format!("INSERT INTO image_blobs VALUES ({id}, '{}', 'image/png', 3, 1, 1, 0, 0, {saves}, x'010203')", sha.to_string().repeat(64)));
    for n in 0..saves {
        run(s, &format!("INSERT INTO saved_images (save_id, blob_id, image_url, page_url, saved_at, incognito, mcp_public) VALUES ('{id}-{n}', {id}, 'https://img.test/x.png', 'https://page.test/', {n}, 0, 1)"));
    }
    run(
        s,
        &format!("INSERT INTO image_thumbnails VALUES ({id}, 'image/webp', 1, 1, 1, x'00')"),
    );
}
fn tag(s: &mut Session, blob: i64, name: &str) {
    s.write(|db| {
        let sha: String = db.query_row(
            "SELECT sha256 FROM image_blobs WHERE id = ?1",
            [blob],
            |r| r.get(0),
        )?;
        crate::image_tags::add_tag(db, blob, &sha, name).unwrap();
        Ok(0)
    })
    .unwrap();
}
/// 1 (a, three saves): only-one, shared. 2 (b): shared. 3 (c): untagged.
fn images(s: &mut Session) {
    image(s, 1, 'a', 3);
    image(s, 2, 'b', 1);
    image(s, 3, 'c', 2);
    tag(s, 1, "only-one");
    tag(s, 1, "shared");
    tag(s, 2, "shared");
}

#[test]
fn one_visit_is_deleted_alone_and_the_url_and_its_snapshot_stay() {
    let (_t, mut s, mut m, b) = fixture();
    urls(&mut s);
    let before = dump(&mut s);
    let p = preview(&mut m, &mut s, &b, json!({"scope":"visit","id":"a3"}));
    assert_eq!(p["ok"], true, "{p}");
    assert_eq!(p["detail"]["url"], A);
    assert_eq!(
        p["counts"],
        json!({"kind":"visits","visits":1,"urls":1,"pageMetadata":1,"navigations":1,"navigationSourcesCleared":1,"snapshotsKept":1})
    );
    assert_eq!(dump(&mut s), before, "a preview changes nothing");
    let done = confirm(&mut m, &mut s, &b, &p);
    assert_eq!(done["ok"], true, "{done}");
    // a3 (and A's metadata row, which it carried, and its own navigation); b1 keeps its row, no source.
    let mut expected = without(
        &before,
        &[
            "v a3 https://a.test/page?x=1",
            "p https://a.test/page?x=1 a3",
            "n a3 <- c1",
            "n b1 <- a3",
        ],
    );
    expected.push("n b1 <- NULL CENSORED".into());
    expected.sort();
    assert_eq!(dump(&mut s), expected);
    let rest = dump(&mut s);
    assert!(
        rest.contains(&"v a1 https://a.test/page?x=1".to_string())
            && rest.contains(&"s s1 https://a.test/page?x=1".to_string())
    );
    integrity(&mut s);
}

#[test]
fn a_filter_deletes_the_matching_visits_only_never_snapshots_or_the_rest_of_a_url() {
    let (_t, mut s, mut m, b) = fixture();
    urls(&mut s);
    run(
        &mut s,
        "UPDATE visits SET title = 'needle' WHERE event_id IN ('a1','c2')",
    );
    let before = dump(&mut s);
    let p = preview(
        &mut m,
        &mut s,
        &b,
        json!({"scope":"visits","query":"  needle  "}),
    );
    assert_eq!(p["counts"]["visits"], 2);
    assert_eq!(p["counts"]["urls"], 2);
    assert_eq!(p["counts"]["snapshotsKept"], 2);
    assert_eq!(
        p["target"]["query"], "needle",
        "the trimmed query, as the list searches"
    );
    assert_eq!(confirm(&mut m, &mut s, &b, &p)["ok"], true);
    let after = dump(&mut s);
    assert!(!after
        .iter()
        .any(|r| r == "v a1 https://a.test/page?x=1" || r == "v c2 https://c.test/"));
    for kept in [
        "v a2 https://a.test/page?x=1",
        "v a3 https://a.test/page?x=1",
        "v c1 https://c.test/",
        "s s1 https://a.test/page?x=1",
        "s s2 https://c.test/",
    ] {
        assert!(after.contains(&kept.to_string()), "{kept}");
    }
    assert_eq!(count(&mut s, "tab_snapshots"), 2);
    assert!(before.len() > after.len());
    integrity(&mut s);
    // Nothing is left to match.
    assert_eq!(
        preview(
            &mut m,
            &mut s,
            &b,
            json!({"scope":"visits","query":"needle"})
        )["error"],
        "item_not_found"
    );
}

#[test]
fn no_filter_means_every_visit_and_still_no_snapshot() {
    let (_t, mut s, mut m, b) = fixture();
    urls(&mut s);
    let p = preview(&mut m, &mut s, &b, json!({"scope":"visits","query":""}));
    assert_eq!(p["counts"]["visits"], 6);
    assert_eq!(confirm(&mut m, &mut s, &b, &p)["ok"], true);
    assert_eq!(
        (
            count(&mut s, "visits"),
            count(&mut s, "tab_snapshots"),
            count(&mut s, "page_metadata"),
            count(&mut s, "navigation_events")
        ),
        (0, 2, 0, 0)
    );
    integrity(&mut s);
}

#[test]
fn a_date_range_is_the_lists_own_window_and_stays_inside_it() {
    let (_t, mut s, mut m, b) = fixture();
    urls(&mut s);
    run(
        &mut s,
        "UPDATE visits SET visited_at = 'not a time' WHERE event_id = 'c2'",
    );
    let target = json!({"scope":"visits","query":"","visitedFrom":"2026-11-02T00:00:00.000Z","visitedTo":"2026-11-05T00:00:00.000Z"});
    let p = preview(&mut m, &mut s, &b, target);
    // a2 (2nd), a3 (3rd), b1 (4th); c1 is on the 5th (exclusive end); c2's text is no instant.
    assert_eq!(p["counts"]["visits"], 3, "{p}");
    assert_eq!(confirm(&mut m, &mut s, &b, &p)["ok"], true);
    let left = dump(&mut s);
    for kept in [
        "v a1 https://a.test/page?x=1",
        "v c1 https://c.test/",
        "v c2 https://c.test/",
    ] {
        assert!(left.contains(&kept.to_string()), "{kept}");
    }
    // Only one of the two bounds, or a malformed one, is not a window.
    for bad in [
        json!({"visitedFrom":"2026-11-02T00:00:00.000Z"}),
        json!({"visitedFrom":"x","visitedTo":"y"}),
    ] {
        let mut t = json!({"scope":"visits","query":""});
        t.as_object_mut()
            .unwrap()
            .extend(bad.as_object().unwrap().clone());
        assert_eq!(preview(&mut m, &mut s, &b, t)["error"], "invalid_target");
    }
}

/// The set a filter deletes is the set the list finds: the same rows, by the same test.
#[test]
fn the_deletion_filter_and_the_url_search_mean_the_same_thing() {
    let (_t, mut s, mut m, b) = fixture();
    for (i, (url, title)) in [
        (A, "Alpha 100%"),
        (A, "beta"),
        (B, "100% sure"),
        (C, "under_score"),
        (C, "UNDERxSCORE"),
        ("https://d.test/Path", "x"),
        ("https://d.test/path", "y"),
        ("https://e.test/日本語", "日本語"),
    ]
    .into_iter()
    .enumerate()
    {
        visit(&mut s, &format!("v{i}"), url, title, &at(i as u32 + 1));
    }
    let windows = [
        None,
        Some(("2026-11-02T00:00:00.000Z", "2026-11-06T00:00:00.000Z")),
        Some(("2027-01-01T00:00:00.000Z", "2027-01-02T00:00:00.000Z")),
    ];
    for query in [
        "",
        "100%",
        "_",
        "under_score",
        "PATH",
        "日本",
        "a.test",
        " beta ",
        "no such text",
        "\\",
        "x",
    ] {
        for window in windows {
            let mut target = json!({"scope":"visits","query":query});
            if let Some((from, to)) = window {
                target["visitedFrom"] = json!(from);
                target["visitedTo"] = json!(to);
            }
            let hits: std::collections::BTreeSet<String> = s
                .read(|db| {
                    let request = SearchRequest {
                        query: query.trim().to_owned(),
                        limit: 100,
                        offset: 0,
                        visited: window.map(|(a, b)| (a.to_owned(), b.to_owned())),
                    };
                    search_pages(db, &request)
                        .unwrap()
                        .items
                        .into_iter()
                        .map(|i| i.url)
                        .collect()
                })
                .unwrap();
            let p = preview(&mut m, &mut s, &b, target.clone());
            let targeted: std::collections::BTreeSet<String> = s
                .read(|db| {
                    let wanted = Target::checked(&target).unwrap();
                    let (pattern, from, to, id) = visit_params(&wanted);
                    let sql = format!("SELECT DISTINCT url FROM visits WHERE {}", visit_match());
                    db.prepare(&sql)
                        .unwrap()
                        .query_map(params![pattern, from, to, id], |r| r.get(0))
                        .unwrap()
                        .collect::<rusqlite::Result<_>>()
                        .unwrap()
                })
                .unwrap();
            // Visits only (no snapshots here): the URLs of the deleted visits are the URLs found.
            assert_eq!(targeted, hits, "query {query:?} window {window:?}");
            assert_eq!(
                p["ok"],
                !hits.is_empty(),
                "query {query:?} window {window:?}: {p}"
            );
        }
    }
}

#[test]
fn a_url_is_deleted_whole_and_exactly_as_before() {
    let (_t, mut s, mut m, b) = fixture();
    urls(&mut s);
    let before = dump(&mut s);
    let p = preview(&mut m, &mut s, &b, json!({"scope":"url","url":A}));
    assert_eq!(
        p["counts"],
        json!({"kind":"url","urls":1,"visits":3,"tabSnapshots":1,"pageMetadata":1,"navigations":2,"navigationSourcesCleared":1,"imageSavesKept":0}),
        "{p}"
    );
    assert_eq!(confirm(&mut m, &mut s, &b, &p)["ok"], true);
    let mut expected = without(
        &before,
        &[
            "v a1 https://a.test/page?x=1",
            "v a2 https://a.test/page?x=1",
            "v a3 https://a.test/page?x=1",
            "s s1 https://a.test/page?x=1",
            "p https://a.test/page?x=1 a3",
            "n a2 <- a1",
            "n a3 <- c1",
            "n b1 <- a3",
        ],
    );
    expected.push("n b1 <- NULL CENSORED".into());
    expected.sort();
    assert_eq!(dump(&mut s), expected);
    integrity(&mut s);
    // Exactly that text: a prefix, a case change, another query are other URLs.
    for other in [
        "https://a.test/page",
        "HTTPS://A.TEST/page?x=2",
        "https://a.test/page?x=2%",
    ] {
        assert_eq!(
            preview(&mut m, &mut s, &b, json!({"scope":"url","url":other}))["error"],
            "item_not_found",
            "{other}"
        );
    }
}

#[test]
fn a_confirmation_is_for_exactly_the_previewed_set_even_with_the_same_count() {
    let (_t, mut s, mut m, b) = fixture();
    urls(&mut s);
    run(
        &mut s,
        "UPDATE visits SET title = 'needle' WHERE event_id IN ('a1','a2')",
    );
    let target = json!({"scope":"visits","query":"needle"});
    let before = dump(&mut s);

    // 1. Two other visits take the place of the two previewed ones (same count, other ids).
    let p = preview(&mut m, &mut s, &b, target.clone());
    assert_eq!(p["counts"]["visits"], 2);
    run(&mut s, "UPDATE visits SET title = 'plain' WHERE event_id IN ('a1','a2'); UPDATE visits SET title = 'needle' WHERE event_id IN ('c1','c2')");
    let swapped = dump(&mut s);
    let r = confirm(&mut m, &mut s, &b, &p);
    assert_eq!(
        (r["ok"].clone(), r["error"].clone()),
        (json!(false), json!("item_targets_changed"))
    );
    assert_eq!(dump(&mut s), swapped, "nothing deleted");

    // 2. A third matching visit appears after the preview.
    run(&mut s, "UPDATE visits SET title = 'needle' WHERE event_id IN ('a1','a2'); UPDATE visits SET title = 'plain' WHERE event_id IN ('c1','c2')");
    let p = preview(&mut m, &mut s, &b, target.clone());
    run(
        &mut s,
        "UPDATE visits SET title = 'needle' WHERE event_id = 'b1'",
    );
    assert_eq!(
        confirm(&mut m, &mut s, &b, &p)["error"],
        "item_targets_changed"
    );

    // 3. A previewed visit is edited (same ids, same count): refused too.
    run(
        &mut s,
        "UPDATE visits SET title = 'plain' WHERE event_id = 'b1'",
    );
    let p = preview(&mut m, &mut s, &b, target.clone());
    run(
        &mut s,
        "UPDATE visits SET title = 'needle edited' WHERE event_id = 'a1'",
    );
    assert_eq!(
        confirm(&mut m, &mut s, &b, &p)["error"],
        "item_targets_changed"
    );

    // 4. A URL deletion: a new visit or snapshot of the URL, or a changed one.
    run(
        &mut s,
        "UPDATE visits SET title = 'title a1' WHERE event_id = 'a1'",
    );
    for change in ["INSERT INTO visits VALUES ('a9', 'https://a.test/page?x=1', 't', '2026-11-09T10:00:00.000Z')", "DELETE FROM visits WHERE event_id = 'a9'; INSERT INTO visits VALUES ('a8', 'https://a.test/page?x=1', 't', '2026-11-09T10:00:00.000Z'); DELETE FROM visits WHERE event_id = 'a2'; INSERT INTO visits VALUES ('a2', 'https://a.test/page?x=1', 'other', '2026-11-02T10:00:00.000Z')"] {
        let p = preview(&mut m, &mut s, &b, json!({"scope":"url","url":A}));
        assert_eq!(p["ok"], true, "{p}");
        run(&mut s, change);
        assert_eq!(confirm(&mut m, &mut s, &b, &p)["error"], "item_targets_changed", "{change}");
    }
    let p = preview(&mut m, &mut s, &b, json!({"scope":"url","url":A}));
    run(
        &mut s,
        "UPDATE tab_snapshots SET title = 'changed' WHERE event_id = 's1'",
    );
    assert_eq!(
        confirm(&mut m, &mut s, &b, &p)["error"],
        "item_targets_changed"
    );
    assert!(count(&mut s, "visits") >= 6 && before.len() > 3);
    integrity(&mut s);
}

#[test]
fn a_confirmation_is_single_use_expires_and_dies_with_its_connection() {
    let (_t, mut s, mut m, b) = fixture();
    urls(&mut s);
    let target = json!({"scope":"visit","id":"c1"});
    let p = preview(&mut m, &mut s, &b, target.clone());
    assert!(uuid::Uuid::parse_str(p["reviewId"].as_str().unwrap()).is_ok());
    assert_eq!(
        call(
            &mut m,
            &mut s,
            &b,
            "item_delete",
            json!({"reviewId":"00000000-0000-4000-8000-000000000000"})
        )["error"],
        "invalid_review"
    );
    // A wrong id is refused and does not use up the right one.
    assert_eq!(confirm(&mut m, &mut s, &b, &p)["ok"], true);
    assert_eq!(
        confirm(&mut m, &mut s, &b, &p)["error"],
        "invalid_review",
        "single use"
    );

    // Another preview replaces the first one.
    let first = preview(&mut m, &mut s, &b, json!({"scope":"visit","id":"c2"}));
    let second = preview(&mut m, &mut s, &b, json!({"scope":"visit","id":"b1"}));
    assert_eq!(
        confirm(&mut m, &mut s, &b, &first)["error"],
        "invalid_review"
    );
    assert_eq!(confirm(&mut m, &mut s, &b, &second)["ok"], true);
    assert_eq!(count(&mut s, "visits"), 4);

    // Expiry.
    let p = preview(&mut m, &mut s, &b, json!({"scope":"visit","id":"c2"}));
    m.review.as_mut().unwrap().issued = Instant::now() - REVIEW_TTL - Duration::from_secs(1);
    assert_eq!(confirm(&mut m, &mut s, &b, &p)["error"], "invalid_review");
    assert_eq!(count(&mut s, "visits"), 4);

    // Another connection (a restarted Host, another port) never confirms it.
    let p = preview(&mut m, &mut s, &b, json!({"scope":"visit","id":"c2"}));
    let mut other = Manager::default();
    let ob = other.handle(&mut s, &json!({"type":"item_state","requestId":"s"}));
    assert_eq!(
        call(
            &mut other,
            &mut s,
            &ob,
            "item_delete",
            json!({"reviewId":p["reviewId"]})
        )["error"],
        "invalid_review"
    );
    assert_eq!(count(&mut s, "visits"), 4);
    // The ids are not guessable: they differ every time.
    let q = preview(&mut m, &mut s, &b, json!({"scope":"visit","id":"c2"}));
    assert_ne!(p["reviewId"], q["reviewId"]);
}

#[test]
fn the_state_of_a_deletion_is_asked_on_the_same_connection_only() {
    let (_t, mut s, mut m, b) = fixture();
    urls(&mut s);
    let p = preview(&mut m, &mut s, &b, json!({"scope":"visit","id":"c1"}));
    let done = confirm(&mut m, &mut s, &b, &p);
    assert_eq!(done["operationId"], p["reviewId"]);
    let status = call(
        &mut m,
        &mut s,
        &b,
        "item_delete_status",
        json!({"operationId": p["reviewId"]}),
    );
    assert_eq!(
        (
            status["confirmed"].clone(),
            status["deleted"]["visits"].clone()
        ),
        (json!(true), json!(1))
    );
    let unknown = call(
        &mut m,
        &mut s,
        &b,
        "item_delete_status",
        json!({"operationId":"x"}),
    );
    assert_eq!(unknown["confirmed"], false);
    let mut restarted = Manager::default();
    let rb = restarted.handle(&mut s, &json!({"type":"item_state","requestId":"s"}));
    assert_eq!(
        call(
            &mut restarted,
            &mut s,
            &rb,
            "item_delete_status",
            json!({"operationId": p["reviewId"]})
        )["confirmed"],
        false
    );
}

#[test]
fn a_vault_switch_or_a_selection_change_refuses_every_old_review() {
    let (t, mut s, mut m, b) = fixture();
    urls(&mut s);
    let env = t.env();
    let first = t.0.join("item.sqlite3");
    let other = t.0.join("other.sqlite3");
    vault::create_vault(&other).unwrap();
    let before = dump(&mut s);
    let p = preview(&mut m, &mut s, &b, json!({"scope":"visit","id":"c1"}));
    // A -> B -> A: the same file, a new selection.
    for path in [&other, &first] {
        let lock = ConfigLock::acquire(&env).unwrap();
        vault::switch_active(&env, &lock, path, false).unwrap();
    }
    assert_eq!(confirm(&mut m, &mut s, &b, &p)["error"], "vault_switched");
    assert_eq!(
        call(&mut m, &mut s, &b, "visit_history", json!({"url":C}))["error"],
        "vault_switched"
    );
    assert_eq!(
        preview(&mut m, &mut s, &b, json!({"scope":"url","url":C}))["error"],
        "vault_switched"
    );
    assert_eq!(dump(&mut s), before);
    // The new binding, then: B, with an old token.
    let b2 = m.handle(&mut s, &json!({"type":"item_state","requestId":"s"}));
    assert_ne!(b2["generation"], b["generation"]);
    let p = preview(&mut m, &mut s, &b2, json!({"scope":"visit","id":"c1"}));
    let lock = ConfigLock::acquire(&env).unwrap();
    vault::switch_active(&env, &lock, &other, false).unwrap();
    drop(lock);
    assert_eq!(confirm(&mut m, &mut s, &b2, &p)["error"], "vault_switched");
    let b3 = m.handle(&mut s, &json!({"type":"item_state","requestId":"s"}));
    assert_ne!(b3["vaultToken"], b2["vaultToken"]);
    assert_eq!(
        preview(&mut m, &mut s, &b3, json!({"scope":"visit","id":"c1"}))["error"],
        "item_not_found",
        "B has no such visit"
    );
    // A is untouched.
    let lock = ConfigLock::acquire(&env).unwrap();
    vault::switch_active(&env, &lock, &first, false).unwrap();
    drop(lock);
    assert_eq!(dump(&mut s), before);
}

#[test]
fn a_locked_secure_vault_refuses_reads_and_confirmations() {
    use memocampus_native_host::{
        active_vault::VaultMode,
        secure_vault::{
            cipher,
            secret::{CipherSalt, VaultId, VaultKey},
        },
    };
    use std::{cell::Cell, rc::Rc};
    struct Keys(Rc<Cell<bool>>);
    impl vault::KeySource for Keys {
        fn key_for(&self, _: &vault::KeyRequest) -> std::result::Result<VaultKey, vault::KeyError> {
            if self.0.get() {
                Ok(VaultKey::from_bytes(&mut [7; 32]))
            } else {
                Err(vault::KeyError::Locked)
            }
        }
    }
    let tmp = TempDir::new();
    let env = tmp.env();
    let path = tmp.0.join("secure.sqlite3");
    cipher::create(
        &path,
        &VaultKey::from_bytes(&mut [7; 32]),
        &CipherSalt::from_array([2; 16]),
    )
    .unwrap();
    let config = vault::Config::encrypted(
        path.to_string_lossy().into_owned(),
        false,
        VaultId::from_array([1; 16]),
    );
    assert_eq!(config.mode, VaultMode::Encrypted);
    vault::write_config(&env, &config).unwrap();
    let unlocked = Rc::new(Cell::new(true));
    let mut s = Session::with_keys(env, Box::new(Keys(unlocked.clone())));
    let mut m = Manager::default();
    let b = m.handle(&mut s, &json!({"type":"item_state","requestId":"s"}));
    urls(&mut s);
    let p = preview(&mut m, &mut s, &b, json!({"scope":"visit","id":"c1"}));
    assert_eq!(p["ok"], true, "{p}");
    unlocked.set(false);
    assert_eq!(
        call(&mut m, &mut s, &b, "visit_history", json!({"url":C}))["error"],
        "vault_locked"
    );
    assert_eq!(confirm(&mut m, &mut s, &b, &p)["error"], "vault_locked");
    unlocked.set(true);
    assert_eq!(
        confirm(&mut m, &mut s, &b, &p)["error"],
        "invalid_review",
        "a lock drops the review for good"
    );
    assert_eq!(count(&mut s, "visits"), 6);
}

#[test]
fn images_are_deleted_by_the_unit_the_tile_shows_with_their_own_tags_only() {
    let (_t, mut s, mut m, b) = fixture();
    urls(&mut s);
    images(&mut s);
    let before = dump(&mut s);
    let p = preview(
        &mut m,
        &mut s,
        &b,
        json!({"scope":"image","blobId":1,"sha256":"a".repeat(64)}),
    );
    assert_eq!(
        p["counts"],
        json!({"kind":"images","images":1,"bytes":3,"saves":3,"thumbnails":1,"tagLinks":2,"tagsRemoved":1}),
        "{p}"
    );
    assert_eq!(confirm(&mut m, &mut s, &b, &p)["ok"], true);
    let mut expected = without(
        &before,
        &[
            "b 1",
            "sv 1-0",
            "sv 1-1",
            "sv 1-2",
            "th 1",
            "l 1 only-one",
            "l 1 shared",
            "t only-one",
        ],
    );
    expected.sort();
    assert_eq!(dump(&mut s), expected);
    integrity(&mut s);
    // The bytes of another hash, or an unknown id, are no such image.
    assert_eq!(
        preview(
            &mut m,
            &mut s,
            &b,
            json!({"scope":"image","blobId":2,"sha256":"a".repeat(64)})
        )["error"],
        "item_not_found"
    );
    assert_eq!(
        preview(
            &mut m,
            &mut s,
            &b,
            json!({"scope":"image","blobId":99,"sha256":"b".repeat(64)})
        )["error"],
        "item_not_found"
    );
}

#[test]
fn a_tag_filter_and_all_images_delete_the_set_the_gallery_shows() {
    let (_t, mut s, mut m, b) = fixture();
    images(&mut s);
    let listed: Vec<i64> = s
        .read(|db| {
            crate::image_tags::search_by_tag(db, "shared", None, 60)
                .unwrap()
                .items
                .into_iter()
                .map(|i| i.blob_id)
                .collect()
        })
        .unwrap();
    assert_eq!(listed.len(), 2);
    // The typed tag is normalized as the gallery does.
    let p = preview(
        &mut m,
        &mut s,
        &b,
        json!({"scope":"images","tag":"  SHARED "}),
    );
    assert_eq!(
        p["counts"],
        json!({"kind":"images","images":2,"bytes":6,"saves":4,"thumbnails":2,"tagLinks":3,"tagsRemoved":2}),
        "{p}"
    );
    assert_eq!(p["target"]["tag"], "shared");
    assert_eq!(confirm(&mut m, &mut s, &b, &p)["ok"], true);
    let left = dump(&mut s);
    assert!(
        left.iter().any(|r| r == "b 3")
            && !left
                .iter()
                .any(|r| r == "b 1" || r == "b 2" || r.starts_with("t ") || r.starts_with("l ")),
        "{left:?}"
    );
    integrity(&mut s);
    assert_eq!(
        preview(&mut m, &mut s, &b, json!({"scope":"images","tag":"shared"}))["error"],
        "item_not_found"
    );
    assert_eq!(
        preview(&mut m, &mut s, &b, json!({"scope":"images","tag":" "}))["error"],
        "invalid_tag"
    );
    // No filter: every image.
    let p = preview(&mut m, &mut s, &b, json!({"scope":"images"}));
    assert_eq!(p["counts"]["images"], 1);
    assert_eq!(confirm(&mut m, &mut s, &b, &p)["ok"], true);
    assert_eq!(
        count(&mut s, "image_blobs")
            + count(&mut s, "saved_images")
            + count(&mut s, "image_thumbnails"),
        0
    );
    integrity(&mut s);
}

#[test]
fn a_set_of_images_changed_after_the_preview_is_refused() {
    let (_t, mut s, mut m, b) = fixture();
    images(&mut s);
    let target = json!({"scope":"images","tag":"shared"});
    // Same count, another image takes the tag.
    let p = preview(&mut m, &mut s, &b, target.clone());
    run(&mut s, "DELETE FROM image_tag_links WHERE blob_id = 2");
    tag(&mut s, 3, "shared");
    assert_eq!(
        confirm(&mut m, &mut s, &b, &p)["error"],
        "item_targets_changed"
    );
    // A new save of a previewed image, a tag removed, a thumbnail gone.
    for change in [
        "INSERT INTO saved_images (save_id, blob_id, image_url, page_url, saved_at, incognito, mcp_public) VALUES ('new', 1, 'https://img.test/y.png', NULL, 9, 0, 0)",
        "DELETE FROM image_tag_links WHERE blob_id = 1 AND tag_id = (SELECT id FROM image_tags WHERE name = 'only-one')",
        "DELETE FROM image_thumbnails WHERE blob_id = 1",
        "UPDATE saved_images SET mcp_public = 0 WHERE save_id = '1-0'",
    ] {
        let p = preview(&mut m, &mut s, &b, json!({"scope":"image","blobId":1,"sha256":"a".repeat(64)}));
        assert_eq!(p["ok"], true, "{p}");
        run(&mut s, change);
        assert_eq!(confirm(&mut m, &mut s, &b, &p)["error"], "item_targets_changed", "{change}");
    }
    assert_eq!(count(&mut s, "image_blobs"), 3);
    integrity(&mut s);
}

#[test]
fn urls_images_and_pages_do_not_touch_each_other() {
    let (_t, mut s, mut m, b) = fixture();
    urls(&mut s);
    images(&mut s);
    let bytes = b"MIME-Version: 1.0\r\n";
    let sha = format!("{:x}", Sha256::digest(bytes));
    s.write(|db| {
        pages::insert(
            db,
            &uuid::Uuid::new_v4().to_string(),
            &pages::Metadata {
                url: A.into(),
                title: "page".into(),
                captured_at: 1,
                incognito: false,
            },
            bytes,
            &sha,
            1,
        )
        .unwrap();
        Ok(0)
    })
    .unwrap();
    let pages_before: Vec<String> = dump(&mut s)
        .into_iter()
        .filter(|r| r.starts_with("pg "))
        .collect();
    assert_eq!(pages_before.len(), 1);
    for target in [
        json!({"scope":"visits","query":""}),
        json!({"scope":"url","url":A}),
        json!({"scope":"images"}),
    ] {
        let p = preview(&mut m, &mut s, &b, target);
        assert_eq!(confirm(&mut m, &mut s, &b, &p)["ok"], true, "{p}");
    }
    assert_eq!(
        (count(&mut s, "saved_pages"), count(&mut s, "page_blobs")),
        (1, 1)
    );
    assert_eq!(count(&mut s, "image_blobs"), 0);
    assert_eq!(count(&mut s, "visits"), 0);
    integrity(&mut s);
}

#[test]
fn a_deletion_that_fails_part_way_leaves_everything() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(&memocampus_native_host::schema::create_sql())
        .unwrap();
    db.execute_batch("PRAGMA foreign_keys = ON").unwrap();
    for (id, url) in [("a1", A), ("a2", A), ("c1", C)] {
        db.execute(
            "INSERT INTO visits VALUES (?1, ?2, 'needle', ?3)",
            params![id, url, at(1)],
        )
        .unwrap();
    }
    let target = Target::checked(&json!({"scope":"visits","query":"needle"})).unwrap();
    let (counts, fingerprint, _) = measure(&db, &target).unwrap();
    let review = Review {
        id: "r".into(),
        target,
        fingerprint,
        counts,
        issued: Instant::now(),
    };
    db.execute_batch("CREATE TEMP TRIGGER fail BEFORE DELETE ON visits WHEN old.event_id = 'c1' BEGIN SELECT RAISE(ABORT, 'x'); END;").unwrap();
    let tx = db.unchecked_transaction().unwrap();
    assert!(delete(&tx, &review).is_err());
    drop(tx);
    assert_eq!(
        db.query_row::<i64, _, _>("SELECT count(*) FROM visits", [], |r| r.get(0))
            .unwrap(),
        3
    );
    assert_eq!(
        db.query_row::<i64, _, _>(
            "SELECT count(*) FROM sqlite_temp_master WHERE name = 'item_delete_visits'",
            [],
            |r| r.get(0)
        )
        .unwrap(),
        0
    );
}

#[test]
fn history_pages_one_urls_visits_with_the_lists_filter() {
    let (_t, mut s, mut m, b) = fixture();
    urls(&mut s);
    for n in 0..25 {
        visit(
            &mut s,
            &format!("m{n:02}"),
            "https://many.test/",
            if n % 2 == 0 { "even" } else { "odd" },
            &format!("2026-12-01T10:00:{n:02}.000Z"),
        );
    }
    visit(
        &mut s,
        "same-time",
        "https://many.test/",
        "tie",
        "2026-12-01T10:00:24.000Z",
    );
    run(&mut s, "INSERT INTO tab_snapshots VALUES ('ms',1,0,'https://many.test/','x',0,0,'2026-12-01T10:00:00.000Z')");
    let mut seen = Vec::new();
    let mut after = Value::Null;
    loop {
        let r = call(
            &mut m,
            &mut s,
            &b,
            "visit_history",
            json!({"url":"https://many.test/","limit":10,"after":after}),
        );
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(r["counts"], json!({"matched":26,"total":26,"snapshots":1}));
        for item in r["items"].as_array().unwrap() {
            seen.push(item["id"].as_str().unwrap().to_owned());
        }
        if r["next"].is_null() {
            break;
        }
        after = r["next"].clone();
    }
    assert_eq!(seen.len(), 26);
    assert_eq!(
        seen.iter().collect::<std::collections::BTreeSet<_>>().len(),
        26,
        "no repeat, no gap"
    );
    assert_eq!(&seen[..2], ["same-time", "m24"], "newest first, ties by id");
    // A deletion between two pages skips nothing.
    let r = call(
        &mut m,
        &mut s,
        &b,
        "visit_history",
        json!({"url":"https://many.test/","limit":5}),
    );
    run(&mut s, "DELETE FROM visits WHERE event_id IN ('m23','m22')");
    let rest = call(
        &mut m,
        &mut s,
        &b,
        "visit_history",
        json!({"url":"https://many.test/","limit":50,"after":r["next"]}),
    );
    assert_eq!(rest["items"].as_array().unwrap().len(), 26 - 5, "{rest}");
    // The list's filter applies; another URL's visits never appear; a URL is exact, not a pattern.
    let even = call(
        &mut m,
        &mut s,
        &b,
        "visit_history",
        json!({"url":"https://many.test/","query":"even","limit":50}),
    );
    assert!(even["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|i| i["title"] == "even"));
    assert_eq!(even["counts"]["total"], 24);
    let a = call(&mut m, &mut s, &b, "visit_history", json!({"url":A}));
    assert_eq!(
        a["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["a3", "a2", "a1"]
    );
    assert_eq!(
        call(
            &mut m,
            &mut s,
            &b,
            "visit_history",
            json!({"url":"https://a.test/page%"})
        )["items"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    for bad in [
        json!({"url":""}),
        json!({"url":A,"limit":0}),
        json!({"url":A,"limit":51}),
        json!({"url":A,"after":{"id":"x"}}),
        json!({"url":A,"query":"x".repeat(201)}),
    ] {
        assert_eq!(
            call(&mut m, &mut s, &b, "visit_history", bad.clone())["ok"],
            false,
            "{bad}"
        );
    }
}

#[test]
fn malformed_requests_are_refused_before_any_vault_is_opened() {
    let tmp = TempDir::new();
    let mut s = Session::new(vault::Environment {
        app_dir: Some(tmp.0.join("app")),
        db_override: None,
        exe_dir: None,
    });
    let mut m = Manager::default();
    let sha = "a".repeat(64);
    for (message, code) in [
        (json!({"type":"item_delete_preview"}), "invalid_request_id"),
        (
            json!({"type":"item_delete_preview","requestId":"r"}),
            "invalid_target",
        ),
        (
            json!({"type":"item_delete_preview","requestId":"r","target":{"scope":"all"}}),
            "invalid_target",
        ),
        (
            json!({"type":"item_delete_preview","requestId":"r","target":{"scope":"visit","id":""}}),
            "invalid_target",
        ),
        (
            json!({"type":"item_delete_preview","requestId":"r","target":{"scope":"visit","id":1}}),
            "invalid_target",
        ),
        (
            json!({"type":"item_delete_preview","requestId":"r","target":{"scope":"url","url":""}}),
            "invalid_target",
        ),
        (
            json!({"type":"item_delete_preview","requestId":"r","target":{"scope":"url","url":"x".repeat(vault_items::MAX_URL_CHARS + 1)}}),
            "invalid_target",
        ),
        (
            json!({"type":"item_delete_preview","requestId":"r","target":{"scope":"image","blobId":0,"sha256":sha}}),
            "invalid_target",
        ),
        (
            json!({"type":"item_delete_preview","requestId":"r","target":{"scope":"image","blobId":1,"sha256":"A".repeat(64)}}),
            "invalid_target",
        ),
        (
            json!({"type":"item_delete_preview","requestId":"r","target":{"scope":"images","tag":3}}),
            "invalid_target",
        ),
        (
            json!({"type":"item_delete_preview","requestId":"r","target":{"scope":"visits","query":"x".repeat(201)}}),
            "invalid_target",
        ),
        (
            json!({"type":"item_delete_preview","requestId":"r","target":{"scope":"visit","id":"a"}}),
            "vault_not_configured",
        ),
        (
            json!({"type":"item_delete","requestId":"r","reviewId":"x"}),
            "vault_not_configured",
        ),
        (
            json!({"type":"url_delete","requestId":"r","url":A}),
            "unknown_type",
        ),
        (
            json!({"type":"image_delete","requestId":"r"}),
            "unknown_type",
        ),
    ] {
        let reply = m.handle(&mut s, &message);
        assert_eq!(
            (reply["ok"].clone(), reply["error"].clone()),
            (json!(false), json!(code)),
            "{message}"
        );
    }
}

#[test]
fn a_backup_keeps_what_was_deleted_and_the_deleted_vault_restores_clean() {
    let (t, mut s, mut m, b) = fixture();
    urls(&mut s);
    images(&mut s);
    let backup_dir = t.0.join("backups");
    std::fs::create_dir(&backup_dir).unwrap();
    let before = s
        .read(|db| vault::backup_vault(db, &backup_dir, std::time::SystemTime::now()).unwrap())
        .unwrap();
    let p = preview(&mut m, &mut s, &b, json!({"scope":"visits","query":""}));
    assert_eq!(confirm(&mut m, &mut s, &b, &p)["ok"], true);
    let p = preview(&mut m, &mut s, &b, json!({"scope":"images"}));
    assert_eq!(confirm(&mut m, &mut s, &b, &p)["ok"], true);
    let after = s
        .read(|db| {
            vault::backup_vault(
                db,
                &backup_dir.join("..").join("backups"),
                std::time::SystemTime::now() + Duration::from_secs(5),
            )
            .unwrap()
        })
        .unwrap();
    for (path, visits, blobs) in [(before, 6, 3), (after, 0, 0)] {
        let db = Connection::open(&path).unwrap();
        db.execute_batch("PRAGMA foreign_keys = ON").unwrap();
        let n = |sql: &str| db.query_row::<i64, _, _>(sql, [], |r| r.get(0)).unwrap();
        assert_eq!(
            (
                n("SELECT count(*) FROM visits"),
                n("SELECT count(*) FROM image_blobs")
            ),
            (visits, blobs),
            "{path:?}"
        );
        assert_eq!(
            db.query_row::<String, _, _>("PRAGMA integrity_check", [], |r| r.get(0))
                .unwrap(),
            "ok"
        );
        assert_eq!(n("SELECT count(*) FROM pragma_foreign_key_check"), 0);
    }
}

fn navigations(s: &mut Session) -> Vec<String> {
    dump(s)
        .into_iter()
        .filter(|row| row.starts_with("n "))
        .collect()
}
const NAV: &str = "INSERT INTO navigation_events (target_visit_id, source_visit_id, kind, transition_type, client_redirect, server_redirect, forward_back, from_address_bar, new_tab, occurred_at)";
fn chain(s: &mut Session) {
    // a -> b -> c, and an unrelated navigation whose source was never known.
    for (id, url) in [
        ("a", "https://a.test/"),
        ("b", "https://b.test/"),
        ("c", "https://c.test/"),
        ("u", "https://u.test/"),
    ] {
        visit(s, id, url, id, &at(1));
    }
    let t = at(1);
    run(
        s,
        &format!(
            "{NAV} VALUES ('b','a','document','link',0,0,0,0,0,'{t}');
             {NAV} VALUES ('c','b','document','link',0,0,0,0,0,'{t}');
             {NAV} VALUES ('u',NULL,'document','typed',0,0,0,1,0,'{t}');"
        ),
    );
}

#[test]
fn a_deleted_source_is_censored_never_rewired_and_never_confused_with_an_unknown_one() {
    let (_t, mut s, mut m, b) = fixture();
    chain(&mut s);
    assert_eq!(navigations(&mut s), ["n b <- a", "n c <- b", "n u <- NULL"]);
    let p = preview(&mut m, &mut s, &b, json!({"scope":"visit","id":"b"}));
    // n = 1 visit; k = 1 navigation goes with it (b's own); m = 1 stays with a censored source (c's).
    assert_eq!(
        (
            p["counts"]["visits"].clone(),
            p["counts"]["navigations"].clone(),
            p["counts"]["navigationSourcesCleared"].clone()
        ),
        (json!(1), json!(1), json!(1)),
        "{p}"
    );
    assert_eq!(confirm(&mut m, &mut s, &b, &p)["ok"], true);
    // a -> b is gone with b; c keeps a row that says its source was deleted; nothing says a -> c.
    assert_eq!(navigations(&mut s), ["n c <- NULL CENSORED", "n u <- NULL"]);
    let left = dump(&mut s);
    assert!(!left.iter().any(|row| row.contains("b.test")));
    s.read(|db| {
        let n: i64 = db
            .query_row("SELECT count(*) FROM navigation_events WHERE source_visit_id = 'a' OR (target_visit_id = 'c' AND source_visit_id IS NOT NULL)", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "no invented a -> c");
        let state = |target: &str| {
            let (source, censored): (Option<String>, bool) = db
                .query_row("SELECT source_visit_id, source_censored FROM navigation_events WHERE target_visit_id = ?1", [target], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap();
            assert!(source.is_none());
            crate::navigation::source_state(None, censored)
        };
        assert_eq!(state("c"), Some(crate::navigation::SourceState::Censored));
        assert_eq!(state("u"), Some(crate::navigation::SourceState::Unknown), "a NULL that was never a visit stays unknown");
    })
    .unwrap();
    integrity(&mut s);
    // A censored row stays censored through later deletions, and goes with its own target.
    let p = preview(&mut m, &mut s, &b, json!({"scope":"visit","id":"a"}));
    assert_eq!(
        p["counts"]["navigationSourcesCleared"], 0,
        "a has no navigation of its own and none from it now"
    );
    assert_eq!(confirm(&mut m, &mut s, &b, &p)["ok"], true);
    assert_eq!(navigations(&mut s), ["n c <- NULL CENSORED", "n u <- NULL"]);
    let p = preview(&mut m, &mut s, &b, json!({"scope":"visit","id":"c"}));
    assert_eq!(p["counts"]["navigations"], 1);
    assert_eq!(confirm(&mut m, &mut s, &b, &p)["ok"], true);
    assert_eq!(navigations(&mut s), ["n u <- NULL"]);
}

#[test]
fn source_and_target_deleted_together_leave_no_marker_and_other_sources_do() {
    let (_t, mut s, mut m, b) = fixture();
    chain(&mut s);
    visit(&mut s, "d", "https://d.test/", "d", &at(2));
    run(
        &mut s,
        &format!(
            "{NAV} VALUES ('d','b','document','link',0,0,0,0,0,'{}')",
            at(2)
        ),
    );
    // A filter that takes a, b and c: a -> b and b -> c go with their targets; d (not matched) keeps a censored source.
    run(
        &mut s,
        "UPDATE visits SET title = 'needle' WHERE event_id IN ('a','b','c')",
    );
    let p = preview(
        &mut m,
        &mut s,
        &b,
        json!({"scope":"visits","query":"needle"}),
    );
    assert_eq!(
        (
            p["counts"]["visits"].clone(),
            p["counts"]["navigations"].clone(),
            p["counts"]["navigationSourcesCleared"].clone()
        ),
        (json!(3), json!(2), json!(1)),
        "{p}"
    );
    assert_eq!(confirm(&mut m, &mut s, &b, &p)["ok"], true);
    assert_eq!(navigations(&mut s), ["n d <- NULL CENSORED", "n u <- NULL"]);
    integrity(&mut s);
}

#[test]
fn a_whole_url_deletion_censors_the_sources_it_takes_away_too() {
    let (_t, mut s, mut m, b) = fixture();
    chain(&mut s);
    let p = preview(
        &mut m,
        &mut s,
        &b,
        json!({"scope":"url","url":"https://b.test/"}),
    );
    assert_eq!(
        (
            p["counts"]["navigations"].clone(),
            p["counts"]["navigationSourcesCleared"].clone()
        ),
        (json!(1), json!(1))
    );
    assert_eq!(confirm(&mut m, &mut s, &b, &p)["ok"], true);
    assert_eq!(navigations(&mut s), ["n c <- NULL CENSORED", "n u <- NULL"]);
}

#[test]
fn a_change_of_the_affected_navigations_alone_is_refused_even_with_the_same_visits_and_counts() {
    // Only the navigation side changes after the preview; the visits set stays.
    let z = at(9);
    for change in [
        // another navigation now has the previewed visit as its source (the count of censored ones grows)
        format!("INSERT INTO visits VALUES ('z','https://z.test/','z','{z}'); {NAV} VALUES ('z','b','document','link',0,0,0,0,0,'{z}')"),
        // the same number, other navigations: c's source moves from b to a, u's from none to b
        "UPDATE navigation_events SET source_visit_id = 'a' WHERE target_visit_id = 'c'; UPDATE navigation_events SET source_visit_id = 'b' WHERE target_visit_id = 'u'".to_string(),
        // a flag of an affected navigation changes
        "UPDATE navigation_events SET transition_type = 'typed' WHERE target_visit_id = 'c'".to_string(),
    ] {
        let (_t, mut s, mut m, b) = fixture();
        chain(&mut s);
        let p = preview(&mut m, &mut s, &b, json!({"scope":"visit","id":"b"}));
        run(&mut s, &change);
        let before = dump(&mut s);
        assert_eq!(confirm(&mut m, &mut s, &b, &p)["error"], "item_targets_changed", "{change}");
        assert_eq!(dump(&mut s), before, "nothing deleted, nothing marked");
    }
}

#[test]
fn the_censored_marker_survives_a_backup_and_holds_no_trace_of_the_deleted_visit() {
    let (t, mut s, mut m, b) = fixture();
    chain(&mut s);
    run(
        &mut s,
        "UPDATE visits SET title = 'SECRET-TITLE' WHERE event_id = 'b'",
    );
    let p = preview(&mut m, &mut s, &b, json!({"scope":"visit","id":"b"}));
    assert_eq!(confirm(&mut m, &mut s, &b, &p)["ok"], true);
    let dir = t.0.join("backups");
    std::fs::create_dir(&dir).unwrap();
    let copy = s
        .read(|db| vault::backup_vault(db, &dir, std::time::SystemTime::now()).unwrap())
        .unwrap();
    let db = Connection::open(&copy).unwrap();
    let rows: Vec<(String, i64)> = db
        .prepare("SELECT target_visit_id, source_censored FROM navigation_events ORDER BY 1")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(rows, [("c".to_string(), 1), ("u".to_string(), 0)]);
    assert_eq!(
        db.query_row::<String, _, _>("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap(),
        "ok"
    );
    // The deleted visit's text appears in no value of any table of the live vault (the file itself is not forensics-proof).
    s.read(|db| {
        for table in [
            "visits",
            "tab_snapshots",
            "page_metadata",
            "navigation_events",
        ] {
            let mut st = db.prepare(&format!("SELECT * FROM {table}")).unwrap();
            let columns = st.column_count();
            let mut rows = st.query([]).unwrap();
            while let Some(row) = rows.next().unwrap() {
                for i in 0..columns {
                    let value: rusqlite::types::Value = row.get(i).unwrap();
                    if let rusqlite::types::Value::Text(text) = value {
                        assert!(
                            !text.contains("SECRET-TITLE") && !text.contains("b.test"),
                            "{table}: {text}"
                        );
                    }
                }
            }
        }
    })
    .unwrap();
}
