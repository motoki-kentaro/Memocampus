//! Issue #71, Slice S3: the per-save Page permission `saved_pages.mcp_public` of schema 10, on
//! temporary plain and encrypted vaults only. Nothing here grants or reads any approval or policy:
//! the permission is necessary for MCP, never sufficient. Saves are named as the S7 list will name
//! them: id and instance, read together when the save is shown (`shown`).

use super::*;
use crate::page_manage::Manager;
use crate::vault::{self, tests::TempDir, ConfigLock, Session, VaultTicket};
use memocampus_native_host::secure_vault::{
    cipher,
    secret::{CipherSalt, VaultId, VaultKey},
};
use serde_json::{json, Value};
use std::{cell::Cell, path::Path, rc::Rc};

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

struct Fixture {
    tmp: TempDir,
    session: Session,
    ticket: VaultTicket,
    unlocked: Rc<Cell<bool>>,
}

/// An active vault in a temporary app directory: plain, or encrypted (SQLCipher) with a test key.
fn fixture(encrypted: bool) -> Fixture {
    let tmp = TempDir::new();
    let env = tmp.env();
    let unlocked = Rc::new(Cell::new(true));
    let mut session = if encrypted {
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
        vault::write_config(&env, &config).unwrap();
        Session::with_keys(env, Box::new(Keys(unlocked.clone())))
    } else {
        let path = tmp.0.join("plain.sqlite3");
        vault::create_vault(&path).unwrap();
        let lock = ConfigLock::acquire(&env).unwrap();
        vault::switch_active(&env, &lock, &path, false).unwrap();
        drop(lock);
        Session::new(env)
    };
    let ticket = session.ticket().unwrap();
    Fixture {
        tmp,
        session,
        ticket,
        unlocked,
    }
}

fn meta(url: &str) -> Metadata {
    Metadata {
        url: url.into(),
        title: "synthetic".into(),
        captured_at: 1,
        incognito: false,
    }
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn save(f: &mut Fixture, save_id: &str, url: &str, bytes: &[u8]) -> Receipt {
    let mut receipt = None;
    f.session
        .write(|db| {
            receipt = Some(insert(db, save_id, &meta(url), bytes, &hash(bytes), 5).unwrap());
            Ok(0)
        })
        .unwrap();
    receipt.unwrap()
}

fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Every save as `SaveRef`, in id order, as a list shows them.
fn shown_all(db: &Connection) -> Vec<SaveRef> {
    db.prepare("SELECT id, instance FROM saved_pages ORDER BY id")
        .unwrap()
        .query_map([], |row| {
            Ok(SaveRef {
                id: row.get(0)?,
                instance: row.get(1)?,
            })
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

/// The save a receipt names, as it is shown right after saving.
fn shown(f: &mut Fixture, receipt: &Receipt) -> SaveRef {
    let id = receipt.saved_page_id;
    f.session
        .read(|db| shown_all(db).into_iter().find(|save| save.id == id))
        .unwrap()
        .unwrap()
}

/// Every save as (id, mcp_public), read through the strict reader.
fn permissions(f: &mut Fixture) -> Vec<(i64, bool)> {
    f.session
        .read(|db| {
            shown_all(db)
                .into_iter()
                .map(|save| (save.id, mcp_public(db, save).unwrap().unwrap()))
                .collect()
        })
        .unwrap()
}

fn blob_count(f: &mut Fixture) -> i64 {
    f.session
        .read(|db| {
            db.query_row("SELECT count(*) FROM page_blobs", [], |r| r.get(0))
                .unwrap()
        })
        .unwrap()
}

fn set(f: &mut Fixture, save: SaveRef, value: bool) -> Result<(), &'static str> {
    let ticket = f.ticket.clone();
    set_mcp_public(&mut f.session, &ticket, save, value)
}

/// Deletes one save the way the side panel does (preview, then confirm).
fn delete_event(f: &mut Fixture, saved_page_id: i64) {
    let mut manager = Manager::default();
    let binding = manager.handle(
        &mut f.session,
        &json!({"type":"page_state","requestId":"s"}),
    );
    assert_eq!(binding["ok"], true, "{binding}");
    let mut call = |kind: &str, extra: Value| {
        let mut message = json!({"type":kind,"requestId":"p","vaultToken":binding["vaultToken"],"generation":binding["generation"]});
        message
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        manager.handle(&mut f.session, &message)
    };
    let preview = call(
        "page_delete_preview",
        json!({"target":{"scope":"event","id":saved_page_id}}),
    );
    assert_eq!(preview["ok"], true, "{preview}");
    let done = call("page_delete", json!({"reviewId":preview["reviewId"]}));
    assert_eq!(done["ok"], true, "{done}");
}

#[test]
fn new_saves_are_permitted_and_each_save_keeps_its_own_choice_also_on_a_shared_original() {
    for encrypted in [false, true] {
        let mut f = fixture(encrypted);
        let shared = b"MIME-Version: 1.0\r\n\r\nshared synthetic";
        let (a_id, b_id) = (id(), id());
        let a = save(&mut f, &a_id, "https://a.test/", shared);
        let b = save(&mut f, &b_id, "https://b.test/", shared);
        let c = save(&mut f, &id(), "https://c.test/", b"other synthetic");
        assert!(b.deduplicated && a.blob_id == b.blob_id && c.blob_id != a.blob_id);
        let (a, b, c) = (shown(&mut f, &a), shown(&mut f, &b), shown(&mut f, &c));
        let (ai, bi, ci) = (a.id, b.id, c.id);
        assert_eq!(
            permissions(&mut f),
            [(ai, true), (bi, true), (ci, true)],
            "default 1 ({encrypted})"
        );

        // One save, never its blob or the other save of the same original.
        set(&mut f, a, false).unwrap();
        assert_eq!(permissions(&mut f), [(ai, false), (bi, true), (ci, true)]);
        set(&mut f, b, false).unwrap();
        set(&mut f, a, true).unwrap();
        assert_eq!(permissions(&mut f), [(ai, true), (bi, false), (ci, true)]);
        // Setting the value it already has is fine and changes nothing else.
        set(&mut f, b, false).unwrap();
        assert_eq!(permissions(&mut f), [(ai, true), (bi, false), (ci, true)]);

        // A replay of a save (same save_id) never resets the choice, nor makes a new instance.
        let replay = save(&mut f, &b_id, "https://b.test/", shared);
        assert!(replay.replayed && replay.saved_page_id == bi);
        assert_eq!(shown(&mut f, &replay), b);
        assert_eq!(permissions(&mut f), [(ai, true), (bi, false), (ci, true)]);

        // Unknown ids, and a known id with another instance, change nothing.
        for missing in [0, -1, ci + 100, i64::MAX] {
            let unknown = SaveRef { id: missing, ..c };
            assert_eq!(set(&mut f, unknown, false), Err("not_found"));
        }
        for other in [a.instance, [0; 16], [0xff; 16]] {
            let wrong = SaveRef {
                id: ci,
                instance: other,
            };
            assert_eq!(set(&mut f, wrong, false), Err("not_found"));
        }
        assert_eq!(permissions(&mut f), [(ai, true), (bi, false), (ci, true)]);
        assert!(a.instance != b.instance && b.instance != c.instance);
    }
}

#[test]
fn deleted_saves_take_their_choice_with_them_and_new_saves_start_permitted() {
    for encrypted in [false, true] {
        let mut f = fixture(encrypted);
        let shared = b"MIME-Version: 1.0\r\n\r\nshared synthetic";
        let a_id = id();
        let a = save(&mut f, &a_id, "https://a.test/", shared);
        let a = shown(&mut f, &a);
        let b = save(&mut f, &id(), "https://b.test/", shared);
        let b = shown(&mut f, &b);
        set(&mut f, a, false).unwrap();
        set(&mut f, b, false).unwrap();

        // Delete one save of a shared original: the original stays for the other save.
        delete_event(&mut f, a.id);
        assert_eq!(permissions(&mut f), [(b.id, false)]);
        assert_eq!(blob_count(&mut f), 1);
        // The same original saved again is a new save and starts at 1, whatever deleted or
        // remaining saves of it chose; even under the deleted save's own save_id.
        let again = save(&mut f, &id(), "https://a.test/", shared);
        assert!(again.deduplicated && !again.replayed);
        let reused = save(&mut f, &a_id, "https://a.test/", shared);
        assert!(!reused.replayed && reused.saved_page_id != a.id);
        assert_eq!(
            permissions(&mut f),
            [
                (b.id, false),
                (again.saved_page_id, true),
                (reused.saved_page_id, true)
            ]
        );
        assert_eq!(
            set(&mut f, a, true),
            Err("not_found"),
            "a deleted save stays gone"
        );

        // Delete every save of the original: the original goes, and a new save of the same bytes
        // starts at 1 too.
        let (again, reused) = (shown(&mut f, &again), shown(&mut f, &reused));
        for gone in [b, again, reused] {
            set(&mut f, gone, false).unwrap();
            delete_event(&mut f, gone.id);
        }
        assert_eq!(blob_count(&mut f), 0);
        let fresh = save(&mut f, &id(), "https://a.test/", shared);
        assert!(!fresh.deduplicated);
        assert_eq!(permissions(&mut f), [(fresh.saved_page_id, true)]);
    }
}

#[test]
fn a_choice_lands_only_in_the_vault_of_its_ticket() {
    let mut f = fixture(false);
    let a = save(&mut f, &id(), "https://a.test/", b"synthetic");
    let a = shown(&mut f, &a);
    // Same path, another file identity: refused under the write lock, nothing written.
    let forged = VaultTicket {
        path: f.ticket.path.clone(),
        token: "0".repeat(64),
    };
    assert_eq!(
        set_mcp_public(&mut f.session, &forged, a, false),
        Err("vault_switched")
    );
    // Another configured vault: refused too.
    let other = f.tmp.0.join("other.sqlite3");
    vault::create_vault(&other).unwrap();
    let env = f.tmp.env();
    let lock = ConfigLock::acquire(&env).unwrap();
    vault::switch_active(&env, &lock, &other, false).unwrap();
    drop(lock);
    let mut moved = Session::new(env.clone());
    let other = save_in(&mut moved, b"synthetic");
    assert_eq!(
        set_mcp_public(&mut moved, &f.ticket, other, false),
        Err("vault_switched")
    );
    let read = |session: &mut Session, save: SaveRef| {
        session.read(|db| mcp_public(db, save).unwrap()).unwrap()
    };
    assert_eq!(read(&mut moved, other), Some(true));
    let lock = ConfigLock::acquire(&env).unwrap();
    vault::switch_active(&env, &lock, Path::new(&f.ticket.path), false).unwrap();
    drop(lock);
    let mut back = Session::new(env);
    assert_eq!(
        read(&mut back, a),
        Some(true),
        "the forged ticket wrote nothing"
    );
}

fn save_in(session: &mut Session, bytes: &[u8]) -> SaveRef {
    let mut saved = 0;
    session
        .write(|db| {
            saved = insert(db, &id(), &meta("https://o.test/"), bytes, &hash(bytes), 5)
                .unwrap()
                .saved_page_id;
            Ok(0)
        })
        .unwrap();
    session
        .read(|db| shown_all(db).into_iter().find(|save| save.id == saved))
        .unwrap()
        .unwrap()
}

#[test]
fn a_locked_encrypted_vault_changes_nothing() {
    let mut f = fixture(true);
    let a = save(&mut f, &id(), "https://a.test/", b"synthetic");
    let a = shown(&mut f, &a);
    f.unlocked.set(false);
    f.session = Session::with_keys(f.tmp.env(), Box::new(Keys(f.unlocked.clone())));
    assert_eq!(set(&mut f, a, false), Err("vault_locked"));
    f.unlocked.set(true);
    assert_eq!(permissions(&mut f), [(a.id, true)]);
}

#[test]
fn stored_values_other_than_zero_and_one_are_refused_not_read_as_either() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(&memocampus_native_host::schema::create_sql())
        .unwrap();
    let bytes = b"synthetic";
    insert(&db, &id(), &meta("https://a.test/"), bytes, &hash(bytes), 5).unwrap();
    let [shown] = shown_all(&db)[..] else {
        panic!("one save")
    };
    let saved = shown.id;
    assert_eq!(mcp_public(&db, shown), Ok(Some(true)));
    let next = SaveRef {
        id: saved + 1,
        ..shown
    };
    assert_eq!(mcp_public(&db, next), Ok(None));
    // Only a file edited around its CHECK can hold these; the reader refuses each of them.
    db.execute_batch("PRAGMA ignore_check_constraints = ON")
        .unwrap();
    assert!(
        db.execute(
            "UPDATE saved_pages SET mcp_public = NULL WHERE id = ?1",
            [saved]
        )
        .is_err(),
        "NOT NULL holds even without CHECKs"
    );
    for forged in ["2", "-1", "'yes'", "0.5", "x'01'", "'1'", "1.0"] {
        db.execute(
            &format!("UPDATE saved_pages SET mcp_public = {forged} WHERE id = ?1"),
            [saved],
        )
        .unwrap();
        let stored: String = db
            .query_row(
                "SELECT typeof(mcp_public) || ':' || quote(mcp_public) FROM saved_pages WHERE id = ?1",
                [saved],
                |r| r.get(0),
            )
            .unwrap();
        if stored == "integer:1" {
            // INTEGER affinity turned it into the integer 1, which is a valid value.
            assert_eq!(mcp_public(&db, shown), Ok(Some(true)), "{forged}");
        } else {
            assert_eq!(
                mcp_public(&db, shown),
                Err("invalid_mcp_public"),
                "{forged} stored as {stored}"
            );
        }
    }
}

/// S3-02: SQLite gives a new save the id of a deleted highest one. An operation prepared for the
/// deleted save, permit or withhold, arriving late, must not reach the save that holds its id now;
/// neither when the new save reuses the deleted one's `save_id` (same bytes, same metadata).
#[test]
fn an_operation_for_a_deleted_save_never_reaches_the_save_that_took_its_id() {
    for encrypted in [false, true] {
        let mut f = fixture(encrypted);
        let kept = save(&mut f, &id(), "https://kept.test/", b"kept synthetic");
        let kept = shown(&mut f, &kept);
        let a = save(&mut f, &id(), "https://a.test/", b"a synthetic");
        let a = shown(&mut f, &a);
        set(&mut f, kept, false).unwrap();

        // A is deleted; B, another save, takes A's id.
        delete_event(&mut f, a.id);
        let b = save(&mut f, &id(), "https://b.test/", b"b synthetic");
        assert_eq!(b.saved_page_id, a.id, "SQLite reused the id ({encrypted})");
        let b = shown(&mut f, &b);
        assert_ne!(b.instance, a.instance);
        // A withhold meant for A: B keeps its 1.
        assert_eq!(set(&mut f, a, false), Err("not_found"));
        assert_eq!(permissions(&mut f), [(kept.id, false), (b.id, true)]);
        // A permit meant for A: B, withheld meanwhile, stays withheld.
        set(&mut f, b, false).unwrap();
        assert_eq!(set(&mut f, a, true), Err("not_found"));
        assert_eq!(permissions(&mut f), [(kept.id, false), (b.id, false)]);
        assert_eq!(
            f.session.read(|db| mcp_public(db, a)).unwrap(),
            Ok(None),
            "A is not readable through B either"
        );

        // B is deleted and saved again under its own save_id, with the same bytes and metadata:
        // the same id, the same save_id, yet another save.
        let b_save_id: String = f
            .session
            .read(|db| {
                db.query_row(
                    "SELECT save_id FROM saved_pages WHERE id = ?1",
                    [b.id],
                    |r| r.get(0),
                )
                .unwrap()
            })
            .unwrap();
        delete_event(&mut f, b.id);
        let c = save(&mut f, &b_save_id, "https://b.test/", b"b synthetic");
        assert!(!c.replayed && c.saved_page_id == b.id);
        let c = shown(&mut f, &c);
        assert_ne!(c.instance, b.instance);
        for value in [true, false] {
            assert_eq!(set(&mut f, b, value), Err("not_found"));
        }
        assert_eq!(set(&mut f, a, false), Err("not_found"));
        assert_eq!(permissions(&mut f), [(kept.id, false), (c.id, true)]);
        // The current save itself is reachable as usual.
        set(&mut f, c, false).unwrap();
        assert_eq!(permissions(&mut f), [(kept.id, false), (c.id, false)]);
    }
}
