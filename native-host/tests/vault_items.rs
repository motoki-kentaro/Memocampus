//! Issue #43, Slice 1 through the real host binary: URL and image deletion, and policy changes for
//! one URL. Each test has its own temp directory as `MEMOCAMPUS_APP_DIR` and a synthetic vault; no
//! user data, policy or Chrome profile is touched.

use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("memocampus-items-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn app_dir(&self) -> PathBuf {
        self.0.join("app")
    }

    fn vault(&self) -> PathBuf {
        self.app_dir().join("memocampus.sqlite3")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One host process, as Chrome starts one per message: every reply.
fn exchange(tmp: &TempDir, messages: &[Value]) -> Vec<Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_memocampus-native-host"))
        .env("MEMOCAMPUS_APP_DIR", tmp.app_dir())
        .env_remove("MEMOCAMPUS_DB")
        .current_dir(&tmp.0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for message in messages {
        let body = serde_json::to_vec(message).unwrap();
        stdin.write_all(&(body.len() as u32).to_ne_bytes()).unwrap();
        stdin.write_all(&body).unwrap();
    }
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty(), "host must not log");
    let mut stdout = output.stdout.as_slice();
    let mut replies = Vec::new();
    let mut length = [0u8; 4];
    while stdout.read_exact(&mut length).is_ok() {
        let mut body = vec![0u8; u32::from_ne_bytes(length) as usize];
        stdout.read_exact(&mut body).unwrap();
        replies.push(serde_json::from_slice(&body).unwrap());
    }
    replies
}

fn call(tmp: &TempDir, message: Value) -> Value {
    exchange(tmp, &[message]).remove(0)
}

const A: &str = "https://forum.test/thread/1?page=2";
const A_OTHER_QUERY: &str = "https://forum.test/thread/1?page=3";
const B: &str = "https://blog.test/post";
const T: &str = "2026-11-07T10:00:00.000Z";

fn sha(c: char) -> String {
    c.to_string().repeat(64)
}

/// A fresh vault and policy, filled directly: A has three visits and a snapshot, B links from A,
/// A_OTHER_QUERY is another URL of the same page. Images 1 (two saves, tags) and 2 (shares a tag).
fn setup() -> TempDir {
    let tmp = TempDir::new();
    assert_eq!(
        call(
            &tmp,
            json!({ "type": "policy_initialize", "requestId": "p" })
        )["ok"],
        true
    );
    assert_eq!(
        call(
            &tmp,
            json!({ "type": "vault_create", "requestId": "c", "useDefault": true })
        )["ok"],
        true
    );
    let db = Connection::open(tmp.vault()).unwrap();
    db.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    for (id, url) in [
        ("a1", A),
        ("a2", A),
        ("a3", A),
        ("q1", A_OTHER_QUERY),
        ("b1", B),
    ] {
        db.execute(
            "INSERT INTO visits VALUES (?1, ?2, 'title', ?3)",
            params![id, url, T],
        )
        .unwrap();
    }
    db.execute_batch(&format!(
        "INSERT INTO navigation_events VALUES ('a2', 'a1', 'document', 'link', 0, 0, 0, 0, 0, '{T}');
         INSERT INTO navigation_events VALUES ('b1', 'a3', 'document', 'link', 0, 0, 0, 0, 1, '{T}');
         INSERT INTO tab_snapshots VALUES ('s1', 1, 0, '{A}', 'A', 0, 0, '{T}');
         INSERT INTO page_metadata (url, event_id, title, captured_at) VALUES ('{A}', 'a3', 'A', '{T}');"
    ))
    .unwrap();
    for (id, c, saves) in [(1, 'a', 2), (2, 'b', 1)] {
        db.execute(
            "INSERT INTO image_blobs VALUES (?1, ?2, 'image/png', 3, 1, 1, 0, ?1, ?3, x'010203')",
            params![id, sha(c), saves],
        )
        .unwrap();
        db.execute(
            "INSERT INTO image_thumbnails VALUES (?1, 'image/webp', 1, 1, 1, x'00')",
            [id],
        )
        .unwrap();
        for n in 0..saves {
            db.execute(
                "INSERT INTO saved_images (save_id, blob_id, image_url, page_url, saved_at, incognito, mcp_public)
                 VALUES (?1, ?2, ?3, ?4, ?5, 0, 1)",
                params![format!("save-{id}-{n}"), id, format!("https://img.test/{id}.png"), A, n],
            )
            .unwrap();
        }
    }
    drop(db);
    let token = vault_token(&tmp);
    for (id, c, tag) in [(1, 'a', "only-one"), (1, 'a', "shared"), (2, 'b', "shared")] {
        let reply = call(
            &tmp,
            json!({ "type": "image_tag_add", "requestId": "t", "blobId": id, "sha256": sha(c), "vaultToken": token, "tag": tag }),
        );
        assert_eq!(reply["ok"], true, "{reply}");
    }
    tmp
}

fn vault_token(tmp: &TempDir) -> String {
    let list = call(tmp, json!({ "type": "image_list", "requestId": "l" }));
    list["vaultToken"].as_str().unwrap().to_owned()
}

fn dump(path: &Path) -> Vec<String> {
    let db = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let mut statement = db
        .prepare(
            "SELECT 'v ' || event_id || ' ' || url FROM visits
             UNION ALL SELECT 's ' || event_id || ' ' || url FROM tab_snapshots
             UNION ALL SELECT 'p ' || url FROM page_metadata
             UNION ALL SELECT 'n ' || target_visit_id || ' <- ' || coalesce(source_visit_id, 'NULL') FROM navigation_events
             UNION ALL SELECT 'b ' || id FROM image_blobs
             UNION ALL SELECT 'i ' || save_id FROM saved_images
             UNION ALL SELECT 'th ' || blob_id FROM image_thumbnails
             UNION ALL SELECT 'l ' || blob_id || ' ' || t.name FROM image_tag_links JOIN image_tags t ON t.id = tag_id
             UNION ALL SELECT 't ' || name FROM image_tags
             ORDER BY 1",
        )
        .unwrap();
    let rows = statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<String>>>()
        .unwrap();
    let integrity: String = db
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    let broken: i64 = db
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(broken, 0);
    rows
}

fn without(rows: &[String], gone: &[&str]) -> Vec<String> {
    rows.iter()
        .filter(|row| !gone.contains(&row.as_str()))
        .cloned()
        .collect()
}

#[test]
fn a_url_is_previewed_then_deleted_whole_and_alone() {
    let tmp = setup();
    let before = dump(&tmp.vault());
    // A backup taken before: a separate file, untouched by the deletion.
    let backups = tmp.0.join("backups");
    std::fs::create_dir(&backups).unwrap();
    let backup = call(
        &tmp,
        json!({ "type": "vault_backup", "requestId": "b", "directory": backups, "expectedVaultToken": vault_token(&tmp) }),
    );
    let backup = PathBuf::from(backup["backupPath"].as_str().unwrap());

    let preview = call(
        &tmp,
        json!({ "type": "url_delete_preview", "requestId": "p", "url": A }),
    );
    let token = vault_token(&tmp);
    let counts = json!({ "visits": 3, "tabSnapshots": 1, "pageMetadata": 1, "navigations": 1, "navigationSourcesCleared": 1, "imageSavesKept": 3 });
    assert_eq!(
        preview,
        json!({ "ok": true, "type": "url_delete_preview_result", "requestId": "p", "vaultToken": token, "url": A, "counts": counts, "irreversible": true })
    );
    assert_eq!(dump(&tmp.vault()), before, "a preview changes nothing");

    // A token of no vault: refused, nothing changed.
    let refused = call(
        &tmp,
        json!({ "type": "url_delete", "requestId": "d", "url": A, "vaultToken": "0".repeat(64) }),
    );
    assert_eq!(
        (refused["ok"].clone(), refused["error"].clone()),
        (json!(false), json!("vault_switched"))
    );
    assert_eq!(dump(&tmp.vault()), before);

    let deleted = call(
        &tmp,
        json!({ "type": "url_delete", "requestId": "d", "url": A, "vaultToken": token }),
    );
    assert_eq!(
        deleted,
        json!({ "ok": true, "type": "url_delete_result", "requestId": "d", "vaultToken": token, "url": A, "deleted": counts, "irreversible": true })
    );
    let mut expected = without(
        &before,
        &[
            &format!("v a1 {A}"),
            &format!("v a2 {A}"),
            &format!("v a3 {A}"),
            &format!("s s1 {A}"),
            &format!("p {A}"),
            "n a2 <- a1",
            "n b1 <- a3",
        ],
    );
    expected.push("n b1 <- NULL".into());
    expected.sort();
    assert_eq!(
        dump(&tmp.vault()),
        expected,
        "B keeps its visit and navigation, without a source"
    );
    // Search: gone; the same page with another query and B are still found.
    let found = call(
        &tmp,
        json!({ "type": "search_pages", "requestId": "s", "query": "" }),
    );
    let urls: Vec<&str> = found["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["url"].as_str().unwrap())
        .collect();
    assert!(
        !urls.contains(&A) && urls.contains(&A_OTHER_QUERY) && urls.contains(&B),
        "{urls:?}"
    );
    assert_eq!(dump(&backup), before, "the backup still holds it");
    // Again: a no-op with zero counts.
    let again = call(
        &tmp,
        json!({ "type": "url_delete", "requestId": "d", "url": A, "vaultToken": token }),
    );
    assert_eq!(again["deleted"]["visits"], 0);
    // A new visit of the URL is recorded again: the policy allows it.
    let ack = call(
        &tmp,
        json!({ "type": "visit", "id": "a9", "url": A, "title": "again", "incognito": false, "visitedAt": T }),
    );
    assert_eq!(ack["ok"], true, "{ack}");
}

#[test]
fn a_deletion_meant_for_the_previous_vault_is_refused_in_the_new_one() {
    let tmp = setup();
    let token_a = vault_token(&tmp);
    let before_a = dump(&tmp.vault());
    // Another vault becomes active (another window, the settings page), with the same URL and image.
    let vault_b = tmp.0.join("second.sqlite3");
    assert_eq!(
        call(
            &tmp,
            json!({ "type": "vault_create", "requestId": "c", "path": vault_b })
        )["ok"],
        true
    );
    let db = Connection::open(&vault_b).unwrap();
    db.execute(
        "INSERT INTO visits VALUES ('x', ?1, 't', ?2)",
        params![A, T],
    )
    .unwrap();
    db.execute(
        "INSERT INTO image_blobs VALUES (1, ?1, 'image/png', 3, 1, 1, 0, 0, 1, x'01')",
        [sha('a')],
    )
    .unwrap();
    db.execute("INSERT INTO saved_images (save_id, blob_id, image_url, saved_at, incognito, mcp_public) VALUES ('s', 1, 'https://img.test/1.png', 0, 0, 0)", []).unwrap();
    drop(db);
    let before_b = dump(&vault_b);
    for message in [
        json!({ "type": "url_delete", "requestId": "d", "url": A, "vaultToken": token_a }),
        json!({ "type": "image_delete", "requestId": "d", "blobId": 1, "sha256": sha('a'), "vaultToken": token_a }),
    ] {
        let reply = call(&tmp, message);
        assert_eq!(
            (reply["ok"].clone(), reply["error"].clone()),
            (json!(false), json!("vault_switched")),
            "{reply}"
        );
    }
    assert_eq!(
        dump(&vault_b),
        before_b,
        "nothing of the new vault is deleted"
    );
    assert_eq!(dump(&tmp.vault()), before_a, "nor of the old one");
}

#[test]
fn an_image_is_deleted_with_its_history_thumbnail_and_unused_tags_only() {
    let tmp = setup();
    let before = dump(&tmp.vault());
    let token = vault_token(&tmp);
    let preview = call(
        &tmp,
        json!({ "type": "image_delete_preview", "requestId": "p", "blobId": 1, "sha256": sha('a') }),
    );
    let counts = json!({ "saves": 2, "thumbnails": 1, "tagLinks": 2, "tagsRemoved": 1 });
    assert_eq!(
        preview,
        json!({ "ok": true, "type": "image_delete_preview_result", "requestId": "p", "vaultToken": token, "blobId": 1, "sha256": sha('a'), "byteLength": 3, "counts": counts, "irreversible": true })
    );
    // The bytes of another image, or an image that does not exist: refused, nothing changed.
    for (id, c) in [(1, 'b'), (2, 'a'), (9, 'a')] {
        let reply = call(
            &tmp,
            json!({ "type": "image_delete", "requestId": "d", "blobId": id, "sha256": sha(c), "vaultToken": token }),
        );
        assert_eq!(
            (reply["ok"].clone(), reply["error"].clone()),
            (json!(false), json!("blob_missing")),
            "{reply}"
        );
    }
    assert_eq!(dump(&tmp.vault()), before);

    let deleted = call(
        &tmp,
        json!({ "type": "image_delete", "requestId": "d", "blobId": 1, "sha256": sha('a'), "vaultToken": token }),
    );
    assert_eq!(
        deleted,
        json!({ "ok": true, "type": "image_delete_result", "requestId": "d", "vaultToken": token, "blobId": 1, "sha256": sha('a'), "deleted": counts, "irreversible": true })
    );
    assert_eq!(
        dump(&tmp.vault()),
        without(
            &before,
            &[
                "b 1",
                "i save-1-0",
                "i save-1-1",
                "th 1",
                "l 1 only-one",
                "l 1 shared",
                "t only-one"
            ]
        ),
        "visits, URLs and image 2 with its tag stay"
    );
    // Gone from every read: the list, the tag search, the original, the save history.
    let list = call(&tmp, json!({ "type": "image_list", "requestId": "l" }));
    assert_eq!(
        list["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["blobId"].clone())
            .collect::<Vec<_>>(),
        [json!(2)]
    );
    let tagged = call(
        &tmp,
        json!({ "type": "image_tag_search", "requestId": "s", "tag": "shared" }),
    );
    assert_eq!(tagged["items"].as_array().unwrap().len(), 1);
    let read = call(
        &tmp,
        json!({ "type": "image_read", "requestId": "r", "blobId": 1, "sha256": sha('a'), "vaultToken": token, "offset": 0 }),
    );
    assert_eq!(read["error"], "blob_missing");
    let sources = call(
        &tmp,
        json!({ "type": "image_sources", "requestId": "s", "blobId": 1 }),
    );
    assert_eq!(sources["items"], json!([]), "{sources}");
    let again = call(
        &tmp,
        json!({ "type": "image_delete", "requestId": "d", "blobId": 1, "sha256": sha('a'), "vaultToken": token }),
    );
    assert_eq!(again["error"], "blob_missing");
}

fn preview(tmp: &TempDir, url: &str, action: &str, scope: Option<&str>) -> Value {
    call(
        tmp,
        json!({ "type": "policy_item_preview", "requestId": "p", "url": url, "action": action, "scope": scope }),
    )
}

fn apply(tmp: &TempDir, url: &str, action: &str, scope: Option<&str>, etag: &Value) -> Value {
    call(
        tmp,
        json!({ "type": "policy_item_apply", "requestId": "a", "url": url, "action": action, "scope": scope, "expectedEtag": etag }),
    )
}

fn visit(tmp: &TempDir, id: &str, url: &str) -> Value {
    call(
        tmp,
        json!({ "type": "visit", "id": id, "url": url, "title": "t", "incognito": false, "visitedAt": T }),
    )
}

#[test]
fn a_policy_change_for_one_url_is_what_recording_obeys_next() {
    let tmp = setup();
    let before = dump(&tmp.vault());
    // Never Save, URL exact: the next visit of A is dropped, the same page with another query is not.
    let shown = preview(&tmp, A, "never_save", None);
    assert_eq!(
        (
            shown["current"].clone(),
            shown["expected"].clone(),
            shown["executable"].clone()
        ),
        (json!("allow"), json!("never_save"), json!(true)),
        "{shown}"
    );
    let done = apply(&tmp, A, "never_save", None, &shown["etag"]);
    assert_eq!(
        (
            done["ok"].clone(),
            done["decision"].clone(),
            done["revision"].clone()
        ),
        (json!(true), json!("never_save"), json!(2)),
        "{done}"
    );
    assert_eq!(visit(&tmp, "n1", A)["dropped"], "policy");
    assert_eq!(visit(&tmp, "n2", A_OTHER_QUERY).get("dropped"), None);
    let mut expected = before.clone();
    expected.push(format!("v n2 {A_OTHER_QUERY}"));
    expected.sort();
    assert_eq!(
        dump(&tmp.vault()),
        expected,
        "a rule deletes nothing; only n2 was recorded"
    );
    let judged = call(
        &tmp,
        json!({ "type": "policy_evaluate_batch", "requestId": "e", "items": [{ "id": "a", "url": A }, { "id": "q", "url": A_OTHER_QUERY }] }),
    );
    assert_eq!(
        judged["items"],
        json!([{ "id": "a", "decision": "never_save" }, { "id": "q", "decision": "allow" }])
    );

    // The preview of a policy that changed since is not applied.
    let stale = preview(&tmp, B, "save_but_no_mcp", Some("host"));
    assert_eq!(stale["affectsOtherUrls"], true);
    let other = preview(&tmp, B, "never_save", None);
    assert_eq!(
        apply(&tmp, B, "never_save", None, &other["etag"])["ok"],
        true
    );
    let refused = apply(&tmp, B, "save_but_no_mcp", Some("host"), &stale["etag"]);
    assert_eq!(refused["error"], "policy_conflict");

    // A host rule keeps A private: publishing A is refused and changes nothing.
    let host = preview(&tmp, A, "save_but_no_mcp", Some("host"));
    assert_eq!(
        apply(&tmp, A, "save_but_no_mcp", Some("host"), &host["etag"])["decision"],
        "never_save"
    );
    let blocked = preview(&tmp, A, "publish", None);
    assert_eq!(
        (
            blocked["executable"].clone(),
            blocked["reason"].clone(),
            blocked["blockers"].clone()
        ),
        (
            json!(false),
            json!("publish_blocked"),
            json!([{ "layer": "saveButNoMcp", "ruleIndex": 0, "kind": "pattern" }])
        )
    );
    let policy = call(&tmp, json!({ "type": "policy_get", "requestId": "g" }));
    assert_eq!(
        apply(&tmp, A, "publish", None, &blocked["etag"])["error"],
        "publish_blocked"
    );
    assert_eq!(
        call(&tmp, json!({ "type": "policy_get", "requestId": "g" })),
        policy,
        "nothing written"
    );
    // B's URL rule alone: published, and recorded again.
    let shown = preview(&tmp, B, "publish", None);
    assert_eq!(
        shown["changes"],
        json!([{ "op": "remove", "layer": "neverSave", "ruleIndex": 1, "rule": { "kind": "exact", "pattern": B } }])
    );
    assert_eq!(
        apply(&tmp, B, "publish", None, &shown["etag"])["decision"],
        "allow"
    );
    assert_eq!(visit(&tmp, "b9", B).get("dropped"), None);
    let rules =
        call(&tmp, json!({ "type": "policy_get", "requestId": "g" }))["policy"]["layers"].clone();
    assert_eq!(
        rules,
        json!({
            "neverSave": { "enabled": true, "rules": [{ "kind": "exact", "pattern": A }] },
            "saveButNoMcp": { "enabled": true, "rules": [{ "kind": "pattern", "pattern": "forum.test" }] },
        })
    );
}
