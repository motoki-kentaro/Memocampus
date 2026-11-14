use super::*;
use crate::{pages, vault::tests::TempDir};

#[test]
fn ten_thousand_groups_are_bounded_and_maximum_metadata_obeys_response_budget() {
    let (_t, mut s, mut m, b) = fixture();
    seed(&mut s, "https://seed.test/", "row", 1, 1);
    s.write(|db|{let mut insert=db.prepare("INSERT INTO saved_pages (save_id,blob_id,url,title,captured_at,saved_at,incognito) VALUES (?1,1,?2,'synthetic',1,?3,0)")?;
        for i in 1..10_000 {insert.execute(params![uuid::Uuid::new_v4().to_string(),format!("https://synthetic.test/{i}"),i])?;}Ok(0)
    }).unwrap();
    let started = Instant::now();
    let first = call(&mut m, &mut s, &b, "page_list", json!({"limit":50}));
    assert_eq!(first["ok"], true, "{first}");
    assert_eq!(first["items"].as_array().unwrap().len(), 50);
    let next = call(
        &mut m,
        &mut s,
        &b,
        "page_list",
        json!({"limit":50,"cursor":first["next"]}),
    );
    assert_eq!(next["ok"], true, "{next}");
    assert_ne!(next["items"][0]["id"], first["items"][0]["id"]);
    eprintln!(
        "synthetic Page 10k groups / two pages {:?}",
        started.elapsed()
    );
    s.write(|db| {
        db.execute_batch("DELETE FROM saved_pages; DELETE FROM page_blobs")?;
        Ok(0)
    })
    .unwrap();
    for i in 0..8 {
        seed(
            &mut s,
            &format!("https://large.test/{i}"),
            &"\"".repeat(crate::pages::MAX_TITLE_BYTES),
            i,
            i as u8,
        );
    }
    let first = call(&mut m, &mut s, &b, "page_list", json!({"limit":50}));
    assert_eq!(first["ok"], true, "{first}");
    assert!(serde_json::to_vec(&first).unwrap().len() < BUDGET);
    assert!(first["next"].is_string());
    assert!(first["items"].as_array().unwrap().len() < 8);
}

#[test]
fn large_event_ids_are_lossless_strings_and_numeric_rounding_is_refused() {
    let (_t, mut s, mut m, b) = fixture();
    let id = seed(&mut s, "https://synthetic.test/", "large", 1, 1);
    s.write(|db| {
        db.execute(
            "UPDATE saved_pages SET id=9007199254740993 WHERE id=?1",
            [id],
        )
    })
    .unwrap();
    let listed = call(&mut m, &mut s, &b, "page_list", json!({}));
    assert_eq!(listed["items"][0]["id"], "9007199254740993");
    let p = preview(
        &mut m,
        &mut s,
        &b,
        json!({"scope":"event","id":"9007199254740993"}),
    );
    assert_eq!(confirm(&mut m, &mut s, &b, &p)["ok"], true);
    assert_eq!(
        call(
            &mut m,
            &mut s,
            &b,
            "page_delete_preview",
            json!({"target":{"scope":"event","id":9007199254740992_i64}})
        )["error"],
        "invalid_target"
    );
}

#[test]
fn locked_secure_vault_rejects_reads_and_confirm_and_invalidates_review_after_unlock() {
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
    let id = VaultId::from_array([1; 16]);
    cipher::create(
        &path,
        &VaultKey::from_bytes(&mut [7; 32]),
        &CipherSalt::from_array([2; 16]),
    )
    .unwrap();
    let config = vault::Config::encrypted(path.to_string_lossy().into_owned(), false, id);
    assert_eq!(config.mode, VaultMode::Encrypted);
    vault::write_config(&env, &config).unwrap();
    let unlocked = Rc::new(Cell::new(true));
    let mut s = Session::with_keys(env, Box::new(Keys(unlocked.clone())));
    let mut m = Manager::default();
    let b = m.handle(&mut s, &json!({"type":"page_state","requestId":"s"}));
    let id = seed(&mut s, "https://synthetic.test/", "secure", 1, 1);
    let p = preview(&mut m, &mut s, &b, json!({"scope":"event","id":id}));
    unlocked.set(false);
    assert_eq!(
        call(&mut m, &mut s, &b, "page_list", json!({}))["error"],
        "vault_locked"
    );
    assert_eq!(confirm(&mut m, &mut s, &b, &p)["error"], "vault_locked");
    unlocked.set(true);
    assert_eq!(confirm(&mut m, &mut s, &b, &p)["error"], "invalid_review");
    assert_eq!(n(&mut s, "saved_pages"), 1);
}

fn fixture() -> (TempDir, Session, Manager, Value) {
    let tmp = TempDir::new();
    let env = tmp.env();
    let path = tmp.0.join("page.sqlite3");
    vault::create_vault(&path).unwrap();
    let lock = ConfigLock::acquire(&env).unwrap();
    vault::switch_active(&env, &lock, &path, false).unwrap();
    drop(lock);
    let mut session = Session::new(env);
    let mut manager = Manager::default();
    let binding = manager.handle(
        &mut session,
        &json!({"type":"page_state","requestId":"state"}),
    );
    assert_eq!(binding["ok"], true);
    (tmp, session, manager, binding)
}
fn call(m: &mut Manager, s: &mut Session, b: &Value, kind: &str, extra: Value) -> Value {
    let mut message = json!({"type":kind,"requestId":"test","vaultToken":b["vaultToken"],"generation":b["generation"]});
    message
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    m.handle(s, &message)
}
fn seed(s: &mut Session, url: &str, title: &str, time: i64, byte: u8) -> i64 {
    let id = uuid::Uuid::new_v4().to_string();
    let bytes = [byte];
    let hash = format!("{:x}", Sha256::digest(bytes));
    let mut event_id = 0;
    s.write(|db| {
        event_id = pages::insert(
            db,
            &id,
            &pages::Metadata {
                url: url.into(),
                title: title.into(),
                captured_at: time,
                incognito: false,
            },
            &bytes,
            &hash,
            time,
        )
        .unwrap()
        .saved_page_id;
        Ok(0)
    })
    .unwrap();
    event_id
}
fn preview(m: &mut Manager, s: &mut Session, b: &Value, target: Value) -> Value {
    let r = call(m, s, b, "page_delete_preview", json!({"target":target}));
    assert_eq!(r["ok"], true, "{r}");
    r
}
fn confirm(m: &mut Manager, s: &mut Session, b: &Value, p: &Value) -> Value {
    call(m, s, b, "page_delete", json!({"reviewId":p["reviewId"]}))
}
fn n(s: &mut Session, table: &str) -> i64 {
    s.read(|db| {
        db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    })
    .unwrap()
}

#[test]
fn event_delete_shared_blob_last_reference_and_unchanged_url_image_tables() {
    let (_t, mut s, mut m, b) = fixture();
    let a = seed(&mut s, "https://private.test/path?q=1", "a", 1, 1);
    let c = seed(&mut s, "https://other.test/", "b", 2, 1);
    s.write(|db|{db.execute_batch("INSERT INTO visits VALUES ('keep','https://synthetic.test/','keep','2026-11-01T00:00:00.000Z'); INSERT INTO image_blobs (sha256,mime_type,byte_length,width,height,created_at,last_saved_at,data) VALUES (printf('%064d',1),'image/png',1,1,1,0,0,x'01');")?;Ok(0)}).unwrap();
    let p = preview(&mut m, &mut s, &b, json!({"scope":"event","id":a}));
    assert_eq!(p["counts"]["blobsKept"], 1);
    assert_eq!(confirm(&mut m, &mut s, &b, &p)["ok"], true);
    assert_eq!(n(&mut s, "page_blobs"), 1);
    assert_eq!(
        confirm(&mut m, &mut s, &b, &p)["ok"],
        false,
        "confirmation is one-shot"
    );
    let p = preview(&mut m, &mut s, &b, json!({"scope":"event","id":c}));
    assert_eq!(p["counts"]["blobsRemoved"], 1);
    assert_eq!(confirm(&mut m, &mut s, &b, &p)["ok"], true);
    assert_eq!(n(&mut s, "saved_pages"), 0);
    assert_eq!(n(&mut s, "page_blobs"), 0);
    assert_eq!(n(&mut s, "visits"), 1);
    assert_eq!(n(&mut s, "image_blobs"), 1);
}
#[test]
fn group_exact_url_and_filter_all_database_matches_not_loaded_page() {
    let (_t, mut s, mut m, b) = fixture();
    for i in 0..65 {
        seed(
            &mut s,
            &format!("https://synthetic.test/{i}"),
            "needle",
            i,
            i as u8,
        );
    }
    seed(&mut s, "https://exact.test/path?q=1#A", "old", 100, 100);
    seed(&mut s, "https://exact.test/path?q=1#A", "new", 101, 100);
    seed(&mut s, "https://exact.test/path?q=1#a", "keep", 102, 100);
    let first = call(
        &mut m,
        &mut s,
        &b,
        "page_list",
        json!({"query":"needle","limit":2}),
    );
    assert_eq!(first["ok"], true, "{first}");
    assert_eq!(first["items"].as_array().unwrap().len(), 2);
    let p = preview(
        &mut m,
        &mut s,
        &b,
        json!({"scope":"filter","query":"needle"}),
    );
    assert_eq!(p["counts"]["events"], 65);
    assert_eq!(confirm(&mut m, &mut s, &b, &p)["ok"], true);
    let p = preview(
        &mut m,
        &mut s,
        &b,
        json!({"scope":"url","url":"https://exact.test/path?q=1#A"}),
    );
    assert_eq!(p["counts"]["events"], 2);
    assert_eq!(confirm(&mut m, &mut s, &b, &p)["ok"], true);
    assert_eq!(n(&mut s, "saved_pages"), 1);
    assert_eq!(n(&mut s, "page_blobs"), 1);
    let p = preview(&mut m, &mut s, &b, json!({"scope":"filter","query":""}));
    assert_eq!(p["counts"]["events"], 1);
    assert_eq!(confirm(&mut m, &mut s, &b, &p)["ok"], true);
    assert_eq!(n(&mut s, "page_blobs"), 0);
}
#[test]
fn same_count_replacement_metadata_and_shared_refs_invalidate_confirmation() {
    for change in ["insert", "replace", "metadata", "shared"] {
        let (_t, mut s, mut m, b) = fixture();
        let id = seed(&mut s, "https://synthetic.test/", "target", 1, 1);
        let p = preview(
            &mut m,
            &mut s,
            &b,
            json!({"scope":"url","url":"https://synthetic.test/"}),
        );
        if change == "replace" {
            s.write(|db| db.execute("DELETE FROM saved_pages WHERE id=?1", [id]))
                .unwrap();
        }
        if change == "metadata" {
            s.write(|db| db.execute("UPDATE saved_pages SET title='changed' WHERE id=?1", [id]))
                .unwrap();
        } else {
            seed(
                &mut s,
                if change == "shared" {
                    "https://other.test/"
                } else {
                    "https://synthetic.test/"
                },
                "target",
                1,
                1,
            );
        }
        let before = n(&mut s, "saved_pages");
        let r = confirm(&mut m, &mut s, &b, &p);
        assert_eq!(r["error"], "page_targets_changed", "{change}: {r}");
        assert_eq!(n(&mut s, "saved_pages"), before);
    }
}
#[test]
fn deletion_failure_rolls_back_events_and_blobs() {
    let (_t, mut s, _m, _b) = fixture();
    for i in 0..3 {
        seed(&mut s, "https://synthetic.test/", "row", i, i as u8);
    }
    // Fault after events are removed. TEMP trigger avoids changing the strict main schema.
    s.read(|db|{
        let target=Target::Filter{query:String::new()};let review=Review{id:"test".into(),fingerprint:fingerprint(db,&target).unwrap(),counts:counts(db,&target).unwrap(),target,issued:Instant::now()};
        db.execute_batch("CREATE TEMP TRIGGER stop_blob BEFORE DELETE ON page_blobs BEGIN SELECT RAISE(ABORT,'synthetic'); END;").unwrap();
        let tx=db.unchecked_transaction().unwrap();assert!(delete(&tx,&review).is_err());drop(tx);
        assert_eq!(db.query_row("SELECT count(*) FROM saved_pages",[],|r|r.get::<_,i64>(0)).unwrap(),3);
        assert_eq!(db.query_row("SELECT count(*) FROM page_blobs",[],|r|r.get::<_,i64>(0)).unwrap(),3);
    }).unwrap();
}
#[test]
fn keyset_groups_and_history_are_separate_sorted_and_literal_search_is_lossless() {
    let (_t, mut s, mut m, b) = fixture();
    for i in 0..151 {
        seed(
            &mut s,
            &format!("https://synthetic.test/{i}"),
            "same",
            10,
            i as u8,
        );
    }
    seed(
        &mut s,
        "https://synthetic.test/0",
        "latest 日本語 %_\\",
        11,
        0,
    );
    let mut all = Vec::new();
    let mut cursor = Value::Null;
    loop {
        let r = call(
            &mut m,
            &mut s,
            &b,
            "page_list",
            json!({"limit":13,"cursor":cursor}),
        );
        assert_eq!(r["ok"], true, "{r}");
        all.extend(
            r["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v["url"].as_str().unwrap().to_owned()),
        );
        cursor = r["next"].clone();
        if cursor.is_null() {
            break;
        }
    }
    assert_eq!(all.len(), 151);
    let mut unique = all.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), 151);
    assert_eq!(all[0], "https://synthetic.test/0");
    let r = call(&mut m, &mut s, &b, "page_list", json!({"query":"%_\\"}));
    assert_eq!(r["items"].as_array().unwrap().len(), 1);
    assert_eq!(r["items"][0]["eventCount"], 2);
    assert_eq!(r["items"][0]["matchedCount"], 1);
    let r = call(
        &mut m,
        &mut s,
        &b,
        "page_history",
        json!({"url":"https://synthetic.test/0","limit":1}),
    );
    assert_eq!(r["items"][0]["savedAt"], 11);
    assert!(r["next"].is_string());
    let r = call(
        &mut m,
        &mut s,
        &b,
        "page_history",
        json!({"url":"https://synthetic.test/0","limit":1,"cursor":r["next"]}),
    );
    assert_eq!(r["items"][0]["savedAt"], 10);
    assert!(r["next"].is_null());
    assert_eq!(
        call(&mut m, &mut s, &b, "page_list", json!({"query":"SAME"}))["items"]
            .as_array()
            .unwrap()
            .len(),
        20
    );
    assert!(
        call(&mut m, &mut s, &b, "page_list", json!({"query":"日本語"}))["items"]
            .as_array()
            .unwrap()
            .len()
            == 1
    );
}
#[test]
fn expired_tampered_cross_query_and_changed_dataset_cursors_rejected() {
    let (_t, mut s, mut m, b) = fixture();
    for i in 0..3 {
        seed(
            &mut s,
            &format!("https://synthetic.test/{i}"),
            "row",
            i,
            i as u8,
        );
    }
    let r = call(&mut m, &mut s, &b, "page_list", json!({"limit":1}));
    let cursor = r["next"].clone();
    assert_eq!(
        call(&mut m, &mut s, &b, "page_list", json!({"cursor":"forged"}))["error"],
        "invalid_cursor"
    );
    assert_eq!(
        call(
            &mut m,
            &mut s,
            &b,
            "page_list",
            json!({"query":"other","cursor":cursor})
        )["error"],
        "invalid_cursor"
    );
    m.cursors.get_mut(cursor.as_str().unwrap()).unwrap().issued =
        Instant::now() - CURSOR_TTL - Duration::from_secs(1);
    assert_eq!(
        call(&mut m, &mut s, &b, "page_list", json!({"cursor":cursor}))["error"],
        "invalid_cursor"
    );
    let r = call(&mut m, &mut s, &b, "page_list", json!({"limit":1}));
    seed(&mut s, "https://new.test/", "row", 5, 1);
    assert_eq!(
        call(&mut m, &mut s, &b, "page_list", json!({"cursor":r["next"]}))["error"],
        "page_list_changed"
    );
}
#[test]
fn vault_a_b_a_rejects_list_review_and_cursor_and_host_restart_loses_confirmation() {
    let (tmp, mut s, mut m, b) = fixture();
    seed(&mut s, "https://synthetic.test/", "row", 1, 1);
    let p = preview(&mut m, &mut s, &b, json!({"scope":"filter","query":""}));
    assert_eq!(
        confirm(&mut Manager::default(), &mut s, &b, &p)["error"],
        "invalid_review"
    );
    let other = tmp.0.join("other.sqlite3");
    vault::create_vault(&other).unwrap();
    let env = s.env.clone();
    for path in [&other, &tmp.0.join("page.sqlite3")] {
        let lock = ConfigLock::acquire(&env).unwrap();
        vault::switch_active(&env, &lock, path, false).unwrap();
    }
    assert_eq!(confirm(&mut m, &mut s, &b, &p)["error"], "vault_switched");
    assert_eq!(
        call(&mut m, &mut s, &b, "page_list", json!({}))["error"],
        "vault_switched"
    );
    assert_eq!(n(&mut s, "saved_pages"), 1);
}
#[test]
fn invalid_ids_no_matches_expired_reviews_and_receipt_status() {
    let (_t, mut s, mut m, b) = fixture();
    for id in [json!(0), json!(-1), json!("1 OR 1=1")] {
        assert_eq!(
            call(
                &mut m,
                &mut s,
                &b,
                "page_delete_preview",
                json!({"target":{"scope":"event","id":id}})
            )["error"],
            "invalid_target"
        );
    }
    assert_eq!(
        call(
            &mut m,
            &mut s,
            &b,
            "page_delete_preview",
            json!({"target":{"scope":"event","id":123}})
        )["error"],
        "page_not_found"
    );
    let id = seed(&mut s, "https://synthetic.test/", "row", 1, 1);
    let p = preview(&mut m, &mut s, &b, json!({"scope":"event","id":id}));
    m.review.as_mut().unwrap().issued = Instant::now() - REVIEW_TTL - Duration::from_secs(1);
    assert_eq!(confirm(&mut m, &mut s, &b, &p)["error"], "invalid_review");
    let p = preview(&mut m, &mut s, &b, json!({"scope":"event","id":id}));
    assert_eq!(confirm(&mut m, &mut s, &b, &p)["ok"], true);
    assert_eq!(
        call(
            &mut m,
            &mut s,
            &b,
            "page_delete_status",
            json!({"operationId":p["reviewId"]})
        )["confirmed"],
        true
    );
    assert_eq!(
        call(
            &mut Manager::default(),
            &mut s,
            &b,
            "page_delete_status",
            json!({"operationId":p["reviewId"]})
        )["confirmed"],
        false
    );
}
