use super::*;
use crate::vault::{tests::TempDir, Environment};
use std::path::Path;

fn configure(env: &Environment, path: &Path) {
    let lock = ConfigLock::acquire(env).unwrap();
    vault::switch_active(env, &lock, path, false).unwrap();
}
fn fixture() -> (TempDir, Session) {
    let tmp = TempDir::new();
    let env = tmp.env();
    let path = tmp.0.join("synthetic.sqlite3");
    vault::create_vault(&path).unwrap();
    configure(&env, &path);
    (tmp, Session::new(env))
}
fn meta() -> Value {
    json!({"url":"https://synthetic.test/never-save?access_token=DUMMY", "title":"synthetic private title", "capturedAt":1,"incognito":false})
}
fn open(uploads: &mut Uploads, session: &mut Session, id: &str) -> Value {
    uploads.handle(
        session,
        &json!({"type":"page_save_open","saveId":id,"protocol":1,"metadata":meta()}),
    )
}
fn send(
    uploads: &mut Uploads,
    session: &mut Session,
    binding: &Value,
    kind: &str,
    extra: Value,
) -> Value {
    let mut message = json!({"type":kind,"saveId":binding["saveId"],"vaultToken":binding["vaultToken"],"generation":binding["generation"]});
    message
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    uploads.handle(session, &message)
}
fn prepared(
    uploads: &mut Uploads,
    session: &mut Session,
    id: &str,
    bytes: &[u8],
) -> (Value, Value) {
    let binding = open(uploads, session, id);
    assert_eq!(binding["ok"], true, "{binding}");
    let begun = send(
        uploads,
        session,
        &binding,
        "page_save_begin",
        json!({"byteLength":bytes.len()}),
    );
    assert_eq!(begun["ok"], true, "{begun}");
    for (i, chunk) in bytes.chunks(pages::CHUNK_BYTES).enumerate() {
        let ack = send(
            uploads,
            session,
            &binding,
            "page_save_chunk",
            json!({"index":i,"data":STANDARD.encode(chunk)}),
        );
        assert_eq!(ack["ok"], true, "{ack}");
    }
    let ready = send(uploads, session, &binding, "page_save_prepare", json!({}));
    assert_eq!(ready["ok"], true, "{ready}");
    (binding, ready)
}
fn count(session: &mut Session) -> i64 {
    session
        .read(|db| {
            db.query_row("SELECT count(*) FROM saved_pages", [], |r| r.get(0))
                .unwrap()
        })
        .unwrap()
}

#[test]
fn commit_ack_loss_status_idempotent_and_new_action_same_blob() {
    let (_tmp, mut session) = fixture();
    let mut uploads = Uploads::default();
    let id = uuid::Uuid::new_v4().to_string();
    let bytes = b"MIME-Version: 1.0\r\n\r\nsynthetic\0\xff";
    let (binding, ready) = prepared(&mut uploads, &mut session, &id, bytes);
    assert_eq!(count(&mut session), 0, "prepare is not a successful save");
    let committed = send(
        &mut uploads,
        &mut session,
        &binding,
        "page_save_commit",
        json!({"sha256":ready["sha256"]}),
    );
    assert_eq!(committed["type"], "page_saved");
    // Discard the COMMIT response, simulate another host process/port and perform only a read.
    let mut other = Session::new(session.env.clone());
    let mut status = Uploads::default();
    let answer = send(
        &mut status,
        &mut other,
        &binding,
        "page_save_status",
        json!({"metadata":meta(),"byteLength":bytes.len(),"sha256":ready["sha256"]}),
    );
    assert_eq!(answer["type"], "page_saved");
    assert_eq!(answer["replayed"], true);
    assert_eq!(count(&mut session), 1);
    let (binding, ready) = prepared(&mut uploads, &mut session, &id, bytes);
    let replay = send(
        &mut uploads,
        &mut session,
        &binding,
        "page_save_commit",
        json!({"sha256":ready["sha256"]}),
    );
    assert_eq!(replay["replayed"], true);
    let (binding, ready) = prepared(
        &mut uploads,
        &mut session,
        &uuid::Uuid::new_v4().to_string(),
        bytes,
    );
    let new = send(
        &mut uploads,
        &mut session,
        &binding,
        "page_save_commit",
        json!({"sha256":ready["sha256"]}),
    );
    assert_eq!(new["deduplicated"], true);
    assert_eq!(count(&mut session), 2);
    session.read(|db| pages::verify(db).unwrap()).unwrap();
}

#[test]
fn malformed_chunks_sizes_ids_and_missing_chunks_discard_all_data() {
    for (kind, extra) in [
        ("page_save_begin", json!({"byteLength":0})),
        (
            "page_save_begin",
            json!({"byteLength":pages::MAX_PAGE_BYTES+1}),
        ),
        ("page_save_chunk", json!({"index":1,"data":"eA=="})),
        ("page_save_chunk", json!({"index":0,"data":"!!!!"})),
        ("page_save_chunk", json!({"index":0,"data":"eHh4"})),
        ("page_save_prepare", json!({})),
        ("page_save_commit", json!({"sha256":"a".repeat(64)})),
        (
            "page_save_chunk",
            json!({"index":0,"data":"eA==","saveId":uuid::Uuid::new_v4().to_string()}),
        ),
    ] {
        let (_tmp, mut session) = fixture();
        let mut uploads = Uploads::default();
        let binding = open(
            &mut uploads,
            &mut session,
            &uuid::Uuid::new_v4().to_string(),
        );
        if kind != "page_save_begin" {
            assert_eq!(
                send(
                    &mut uploads,
                    &mut session,
                    &binding,
                    "page_save_begin",
                    json!({"byteLength":1})
                )["ok"],
                true
            );
        }
        let response = send(&mut uploads, &mut session, &binding, kind, extra);
        assert_eq!(response["ok"], false, "{response}");
        assert!(!uploads.active());
        assert_eq!(count(&mut session), 0);
    }
}

#[test]
fn duplicate_excess_and_out_of_order_chunks_are_not_acked() {
    for index in [0, 1, 2] {
        let (_tmp, mut session) = fixture();
        let mut uploads = Uploads::default();
        let binding = open(
            &mut uploads,
            &mut session,
            &uuid::Uuid::new_v4().to_string(),
        );
        send(
            &mut uploads,
            &mut session,
            &binding,
            "page_save_begin",
            json!({"byteLength":1}),
        );
        assert_eq!(
            send(
                &mut uploads,
                &mut session,
                &binding,
                "page_save_chunk",
                json!({"index":0,"data":"eA=="})
            )["ok"],
            true
        );
        assert_eq!(
            send(
                &mut uploads,
                &mut session,
                &binding,
                "page_save_chunk",
                json!({"index":index,"data":"eA=="})
            )["ok"],
            false
        );
        assert_eq!(count(&mut session), 0);
    }
}

#[test]
fn concurrent_ports_excluded_and_disconnect_releases_app_lock() {
    let (_tmp, mut session) = fixture();
    let mut first = Uploads::default();
    let mut second = Uploads::default();
    assert_eq!(
        open(&mut first, &mut session, &uuid::Uuid::new_v4().to_string())["ok"],
        true
    );
    assert_eq!(
        open(&mut second, &mut session, &uuid::Uuid::new_v4().to_string())["error"],
        "page_upload_busy"
    );
    drop(first);
    assert_eq!(
        open(&mut second, &mut session, &uuid::Uuid::new_v4().to_string())["ok"],
        true
    );
}

#[test]
fn switching_and_a_b_a_are_rejected_at_every_phase() {
    for phase in [
        "page_save_begin",
        "page_save_chunk",
        "page_save_prepare",
        "page_save_commit",
    ] {
        for back in [false, true] {
            let (tmp, mut session) = fixture();
            let env = session.env.clone();
            let a = tmp.0.join("synthetic.sqlite3");
            let b = tmp.0.join("b.sqlite3");
            vault::create_vault(&b).unwrap();
            let mut uploads = Uploads::default();
            let binding = open(
                &mut uploads,
                &mut session,
                &uuid::Uuid::new_v4().to_string(),
            );
            if phase != "page_save_begin" {
                send(
                    &mut uploads,
                    &mut session,
                    &binding,
                    "page_save_begin",
                    json!({"byteLength":1}),
                );
            }
            if matches!(phase, "page_save_prepare" | "page_save_commit") {
                send(
                    &mut uploads,
                    &mut session,
                    &binding,
                    "page_save_chunk",
                    json!({"index":0,"data":"eA=="}),
                );
            }
            let ready = if phase == "page_save_commit" {
                send(
                    &mut uploads,
                    &mut session,
                    &binding,
                    "page_save_prepare",
                    json!({}),
                )
            } else {
                json!({})
            };
            configure(&env, &b);
            if back {
                configure(&env, &a);
            }
            let response = send(
                &mut uploads,
                &mut session,
                &binding,
                phase,
                json!({"byteLength":1,"index":0,"data":"eA==","sha256":ready["sha256"]}),
            );
            assert_eq!(
                response["error"], "vault_switched",
                "{phase} back={back}: {response}"
            );
            assert_eq!(count(&mut session), 0);
            assert_eq!(
                vault::open_vault(&a)
                    .unwrap()
                    .query_row("SELECT count(*) FROM saved_pages", [], |r| r
                        .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
    }
}

#[test]
fn unconfigured_unsupported_incognito_and_generation_expiry_fail_closed() {
    let tmp = TempDir::new();
    let mut invalid_session = Session::new(tmp.env());
    let rejected = Uploads::default().handle(&mut invalid_session, &json!({"type":"page_save_open","saveId":"https://synthetic.test/SECRET-not-an-ID","protocol":1,"metadata":meta()}));
    assert_eq!(rejected["saveId"], Value::Null);
    assert!(!rejected.to_string().contains("SECRET"));
    let (old_tmp, old_session) = fixture();
    let env = old_session.env.clone();
    drop(old_session);
    let path = old_tmp.0.join("synthetic.sqlite3");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("PRAGMA user_version = 7").unwrap();
    drop(db);
    let mut config = vault::Config::new(
        path.to_string_lossy().into_owned(),
        false,
        memocampus_native_host::active_vault::VaultMode::Plain,
    );
    config.selection_generation = None;
    vault::write_config(&env, &config).unwrap();
    let before = std::fs::read(&path).unwrap();
    let mut old_session = Session::new(env);
    assert_eq!(
        open(
            &mut Uploads::default(),
            &mut old_session,
            &uuid::Uuid::new_v4().to_string()
        )["error"],
        "vault_schema_mismatch"
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let tmp = TempDir::new();
    let mut session = Session::new(tmp.env());
    let mut uploads = Uploads::default();
    assert_eq!(
        open(
            &mut uploads,
            &mut session,
            &uuid::Uuid::new_v4().to_string()
        )["error"],
        "vault_not_configured"
    );
    let (_tmp, mut session) = fixture();
    for (field, value) in [
        ("incognito", json!(true)),
        ("url", json!("file:///synthetic")),
    ] {
        let mut metadata = meta();
        metadata[field] = value;
        let response=uploads.handle(&mut session,&json!({"type":"page_save_open","saveId":uuid::Uuid::new_v4().to_string(),"protocol":1,"metadata":metadata}));
        assert_eq!(response["ok"], false);
    }
    let binding = open(
        &mut uploads,
        &mut session,
        &uuid::Uuid::new_v4().to_string(),
    );
    uploads.active.as_mut().unwrap().opened =
        Instant::now() - TOTAL_TIMEOUT - Duration::from_secs(1);
    assert_eq!(
        send(
            &mut uploads,
            &mut session,
            &binding,
            "page_save_begin",
            json!({"byteLength":1})
        )["error"],
        "timeout"
    );
}

#[test]
fn exact_128_mib_transfer_prepare_commit_and_streaming_verify() {
    let (_tmp, mut session) = fixture();
    let mut uploads = Uploads::default();
    let binding = open(
        &mut uploads,
        &mut session,
        &uuid::Uuid::new_v4().to_string(),
    );
    assert_eq!(
        send(
            &mut uploads,
            &mut session,
            &binding,
            "page_save_begin",
            json!({"byteLength":pages::MAX_PAGE_BYTES})
        )["ok"],
        true
    );
    let chunk = STANDARD.encode(vec![0x53; pages::CHUNK_BYTES]);
    let started = Instant::now();
    for index in 0..128 {
        assert_eq!(
            send(
                &mut uploads,
                &mut session,
                &binding,
                "page_save_chunk",
                json!({"index":index,"data":chunk})
            )["ok"],
            true
        );
    }
    let ready = send(
        &mut uploads,
        &mut session,
        &binding,
        "page_save_prepare",
        json!({}),
    );
    let saved = send(
        &mut uploads,
        &mut session,
        &binding,
        "page_save_commit",
        json!({"sha256":ready["sha256"]}),
    );
    assert_eq!(saved["type"], "page_saved", "{saved}");
    assert_eq!(saved["byteLength"], pages::MAX_PAGE_BYTES);
    session.read(|db| pages::verify(db).unwrap()).unwrap();
    eprintln!("Page128MiB transfer+prepare+COMMIT+verify elapsed={:?}; receiver cap={}; no whole-input fixture", started.elapsed(), pages::MAX_PAGE_BYTES);
}

#[test]
fn secure_vault_rechecks_key_on_each_step_and_saves_raw_bytes() {
    use memocampus_native_host::secure_vault::{
        cipher,
        secret::{CipherSalt, VaultId, VaultKey},
    };
    use std::{cell::Cell, rc::Rc};
    struct Keys(Rc<Cell<bool>>);
    impl vault::KeySource for Keys {
        fn key_for(&self, _: &vault::KeyRequest) -> Result<VaultKey, vault::KeyError> {
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
    let salt = CipherSalt::from_array([2; 16]);
    cipher::create(&path, &VaultKey::from_bytes(&mut [7; 32]), &salt).unwrap();
    vault::write_config(
        &env,
        &vault::Config::encrypted(path.to_string_lossy().into_owned(), false, id),
    )
    .unwrap();
    let unlocked = Rc::new(Cell::new(true));
    let mut session = Session::with_keys(env, Box::new(Keys(unlocked.clone())));
    let mut uploads = Uploads::default();
    let binding = open(
        &mut uploads,
        &mut session,
        &uuid::Uuid::new_v4().to_string(),
    );
    assert_eq!(binding["ok"], true, "{binding}");
    unlocked.set(false);
    assert_eq!(
        send(
            &mut uploads,
            &mut session,
            &binding,
            "page_save_begin",
            json!({"byteLength":1})
        )["error"],
        "vault_locked"
    );
    unlocked.set(true);
    let (binding, ready) = prepared(
        &mut uploads,
        &mut session,
        &uuid::Uuid::new_v4().to_string(),
        b"synthetic secure MHTML\0\xff",
    );
    let saved = send(
        &mut uploads,
        &mut session,
        &binding,
        "page_save_commit",
        json!({"sha256":ready["sha256"]}),
    );
    assert_eq!(saved["type"], "page_saved", "{saved}");
    session.read(|db| pages::verify(db).unwrap()).unwrap();
    let disk = std::fs::read(&path).unwrap();
    assert!(!disk
        .windows(b"synthetic secure MHTML".len())
        .any(|w| w == b"synthetic secure MHTML"));
}
