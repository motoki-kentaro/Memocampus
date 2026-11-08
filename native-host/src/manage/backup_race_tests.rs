//! Issue #48: `vault_backup` backs up the vault the user saw, or nothing.
//!
//! Two synthetic vaults with different contents (A: 3 visits of a.test, B: 5 of b.test). The active
//! vault is switched at a chosen point by a *second* host session (as another host process would:
//! through `vault_open`, which rewrites `config.json`): before the request (the panel still showing
//! the old vault), while the backup folder dialog is open (a fake `Dialogs`), or right after a
//! resolution (`Session::after_open`). Each backup file is then opened and identified by its rows.
//!
//! Slice 0 reproduced the wrong vault being backed up in each window. The backup is now bound to the
//! vault the panel showed (`expectedVaultToken`, from `vault_status`): these tests assert that it is
//! that vault, or nothing at all (`vault_switched`, no file, no `.partial`).

use super::*;
use crate::vault::tests::TempDir;
use crate::vault::{create_vault, open_vault, open_vault_checked, Environment};
use std::cell::RefCell;

/// A, B and a backup folder; A is active.
struct Fixture {
    tmp: TempDir,
    env: Environment,
    a: PathBuf,
    b: PathBuf,
    dest: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = TempDir::new();
    let env = tmp.env();
    let (a, b) = (tmp.0.join("a.sqlite3"), tmp.0.join("b.sqlite3"));
    for (path, host, rows) in [(&a, "a.test", 3), (&b, "b.test", 5)] {
        create_vault(path).unwrap();
        let db = open_vault(path).unwrap();
        for i in 0..rows {
            db.execute(
                "INSERT INTO visits VALUES (?1, ?2, 't', '2026-11-09T00:00:00.000Z')",
                [format!("{host}-{i}"), format!("https://{host}/")],
            )
            .unwrap();
        }
    }
    let dest = tmp.0.join("backups");
    std::fs::create_dir(&dest).unwrap();
    let f = Fixture {
        tmp,
        env,
        a,
        b,
        dest,
    };
    switch_by_another_host(&f, &f.a);
    f
}

/// What another host process does when the user opens a vault in another window: `vault_open`.
fn switch_by_another_host(f: &Fixture, to: &Path) {
    let mut other = Session::new(f.env.clone());
    let opened = handle(
        &mut other,
        &NoDialogs,
        &json!({ "type": "vault_open", "requestId": "o", "path": to }),
    );
    assert_eq!(opened["ok"], true, "{opened}");
}

/// The vault a backup file holds: "A", "B", or what else it is.
fn identify(backup: &Path) -> String {
    let db = open_vault_checked(backup).unwrap();
    let hosts: Vec<(String, i64)> = db
        .prepare("SELECT url, count(*) FROM visits GROUP BY url ORDER BY url")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    match hosts.as_slice() {
        [(url, 3)] if url == "https://a.test/" => "A".into(),
        [(url, 5)] if url == "https://b.test/" => "B".into(),
        other => format!("{other:?}"),
    }
}

fn files_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

fn visits_in(path: &Path) -> i64 {
    open_vault(path)
        .unwrap()
        .query_row("SELECT count(*) FROM visits", [], |row| row.get(0))
        .unwrap()
}

struct NoDialogs;

impl Dialogs for NoDialogs {
    fn choose_new_vault(&self, _: &Path, _: &str) -> Option<PathBuf> {
        unreachable!()
    }
    fn choose_existing_vault(&self, _: Option<&Path>) -> Option<PathBuf> {
        unreachable!()
    }
    fn choose_backup_dir(&self) -> Option<PathBuf> {
        unreachable!("a directory was given, or no dialog may open")
    }
    fn choose_export_file(&self, _: &str, _: &str) -> Option<PathBuf> {
        unreachable!()
    }
}

/// The backup folder dialog: runs `during` while it is "open" (the user choosing a folder), then
/// answers `answer` (None: cancelled).
struct FolderDialog<'a> {
    answer: Option<PathBuf>,
    during: RefCell<Option<Box<dyn FnOnce() + 'a>>>,
}

impl<'a> FolderDialog<'a> {
    fn new(answer: Option<PathBuf>, during: impl FnOnce() + 'a) -> Self {
        Self {
            answer,
            during: RefCell::new(Some(Box::new(during))),
        }
    }
}

impl Dialogs for FolderDialog<'_> {
    fn choose_new_vault(&self, _: &Path, _: &str) -> Option<PathBuf> {
        unreachable!()
    }
    fn choose_existing_vault(&self, _: Option<&Path>) -> Option<PathBuf> {
        unreachable!()
    }
    fn choose_backup_dir(&self) -> Option<PathBuf> {
        if let Some(during) = self.during.borrow_mut().take() {
            during();
        }
        self.answer.clone()
    }
    fn choose_export_file(&self, _: &str, _: &str) -> Option<PathBuf> {
        unreachable!()
    }
}

/// The token of the vault the panel shows: what `vault_status` answered (another host process).
fn shown(f: &Fixture) -> String {
    let status = handle(
        &mut Session::new(f.env.clone()),
        &NoDialogs,
        &json!({ "type": "vault_status", "requestId": "s" }),
    );
    status["status"]["vaultToken"].as_str().unwrap().to_owned()
}

fn token_of(path: &Path) -> String {
    vault::vault_token(path).unwrap()
}

/// `expected`: the `expectedVaultToken` to send (null: none).
fn backup(
    session: &mut Session,
    dialogs: &dyn Dialogs,
    directory: Option<&Path>,
    expected: Value,
) -> Value {
    let mut message = json!({ "type": "vault_backup", "requestId": "b" });
    if !expected.is_null() {
        message["expectedVaultToken"] = expected;
    }
    if let Some(directory) = directory {
        message["directory"] = json!(directory);
    }
    handle(session, dialogs, &message)
}

fn backed_up(answer: &Value) -> String {
    assert_eq!(answer["ok"], true, "{answer}");
    identify(Path::new(answer["backupPath"].as_str().unwrap()))
}

/// Refused as switched, and nothing written: no backup and no `.partial` in the folder.
fn refused(f: &Fixture, answer: &Value) {
    assert_eq!(answer["ok"], false, "{answer}");
    assert_eq!(answer["error"], "vault_switched", "{answer}");
    assert!(answer.get("backupPath").is_none() && answer.get("vaultPath").is_none());
    assert_eq!(files_in(&f.dest), Vec::<String>::new());
}

/// A session that switches the active vault to `to` right after its `nth` resolution.
fn switching_after(f: &Fixture, nth: usize, to: &Path) -> Session {
    let mut session = Session::new(f.env.clone());
    let (env, to) = (f.env.clone(), to.to_owned());
    let mut opens = 0;
    session.after_open = Some(Box::new(move || {
        opens += 1;
        if opens == nth {
            let lock = vault::ConfigLock::acquire(&env).unwrap();
            vault::switch_active(&env, &lock, &to, false).unwrap();
        }
    }));
    session
}

#[test]
fn case_a_the_vault_shown_is_backed_up_and_named_in_the_answer() {
    let f = fixture();
    let dialogs = FolderDialog::new(Some(f.dest.clone()), || {});
    let answer = backup(
        &mut Session::new(f.env.clone()),
        &dialogs,
        None,
        json!(shown(&f)),
    );
    assert_eq!(backed_up(&answer), "A");
    assert_eq!(answer["vaultPath"], display(&f.a));
    assert_eq!(answer["vaultToken"], token_of(&f.a));
    switch_by_another_host(&f, &f.b);
    let answer = backup(
        &mut Session::new(f.env.clone()),
        &NoDialogs,
        Some(&f.dest),
        json!(shown(&f)),
    );
    assert_eq!(backed_up(&answer), "B");
    assert_eq!(answer["vaultPath"], display(&f.b));
    assert_eq!(answer["vaultToken"], token_of(&f.b));
}

#[test]
fn case_b_a_switch_to_b_while_the_dialog_is_open_is_refused() {
    let f = fixture();
    let token = shown(&f);
    let dialogs = FolderDialog::new(Some(f.dest.clone()), || switch_by_another_host(&f, &f.b));
    let answer = backup(
        &mut Session::new(f.env.clone()),
        &dialogs,
        None,
        json!(token),
    );
    refused(&f, &answer);
}

#[test]
fn case_c_and_the_other_way_round() {
    let f = fixture();
    switch_by_another_host(&f, &f.b);
    let token = shown(&f);
    let dialogs = FolderDialog::new(Some(f.dest.clone()), || switch_by_another_host(&f, &f.a));
    let answer = backup(
        &mut Session::new(f.env.clone()),
        &dialogs,
        None,
        json!(token),
    );
    refused(&f, &answer);
}

#[test]
fn case_d_without_a_dialog_a_switch_after_the_check_is_refused() {
    let f = fixture();
    let token = shown(&f);
    let mut session = switching_after(&f, 1, &f.b);
    let answer = backup(&mut session, &NoDialogs, Some(&f.dest), json!(token));
    refused(&f, &answer);
}

#[test]
fn a_switch_after_the_backups_own_resolution_still_backs_up_the_vault_shown() {
    // The backup resolved A and holds A's file; the switch only affects later requests.
    let f = fixture();
    let token = shown(&f);
    let mut session = switching_after(&f, 2, &f.b);
    let answer = backup(&mut session, &NoDialogs, Some(&f.dest), json!(token));
    assert_eq!(backed_up(&answer), "A");
    assert_eq!(answer["vaultToken"], token_of(&f.a));
}

#[test]
fn w0_the_panel_still_shows_a_while_b_became_active_is_refused_before_any_dialog() {
    let f = fixture();
    let token = shown(&f); // the panel shows A
    switch_by_another_host(&f, &f.b); // another window opens B; the panel is not told
    let answer = backup(
        &mut Session::new(f.env.clone()),
        &NoDialogs,
        None,
        json!(token),
    );
    refused(&f, &answer);
    let answer = backup(
        &mut Session::new(f.env.clone()),
        &NoDialogs,
        Some(&f.dest),
        json!(token),
    );
    refused(&f, &answer);
}

#[test]
fn a_to_b_and_back_to_a_while_the_dialog_is_open_backs_up_a() {
    // A→B→A: the same vault file again, so the backup is A's at the time of the backup.
    let f = fixture();
    let token = shown(&f);
    let dialogs = FolderDialog::new(Some(f.dest.clone()), || {
        switch_by_another_host(&f, &f.b);
        open_vault(&f.a)
            .unwrap()
            .execute(
                "INSERT INTO visits VALUES ('late', 'https://a.test/', 't', '2026-11-09T01:00:00.000Z')",
                [],
            )
            .unwrap();
        switch_by_another_host(&f, &f.a);
    });
    let answer = backup(
        &mut Session::new(f.env.clone()),
        &dialogs,
        None,
        json!(token),
    );
    assert_eq!(answer["ok"], true, "{answer}");
    assert_eq!(answer["vaultToken"], token);
    assert_eq!(
        visits_in(Path::new(answer["backupPath"].as_str().unwrap())),
        4,
        "A, including the row written while the dialog was open"
    );
}

#[test]
fn another_vault_file_put_at_the_same_path_is_refused() {
    // While the dialog is open, A's path gets another file (a copy of B moved over it): the path
    // and the config are unchanged, the vault is not the one shown.
    let f = fixture();
    let token = shown(&f);
    let dialogs = FolderDialog::new(Some(f.dest.clone()), || {
        let copy = f.tmp.0.join("b-copy.sqlite3");
        std::fs::copy(&f.b, &copy).unwrap();
        // Fails if anything still held A open (no connection is kept while the dialog is open).
        std::fs::rename(&copy, &f.a).unwrap();
    });
    let answer = backup(
        &mut Session::new(f.env.clone()),
        &dialogs,
        None,
        json!(token),
    );
    refused(&f, &answer);
    assert_ne!(
        token_of(&f.a),
        token,
        "another file identity at the same path"
    );
}

#[cfg(windows)]
#[test]
fn an_open_vault_file_can_be_neither_replaced_nor_moved() {
    // Why the token taken from the path names the file a connection reads: while SQLite holds it
    // (opened without FILE_SHARE_DELETE), Windows refuses to replace, move or delete it.
    let f = fixture();
    let held = open_vault(&f.a).unwrap();
    let copy = f.tmp.0.join("b-copy.sqlite3");
    std::fs::copy(&f.b, &copy).unwrap();
    assert!(std::fs::rename(&copy, &f.a).is_err(), "replaced while open");
    assert!(
        std::fs::rename(&f.a, f.tmp.0.join("moved.sqlite3")).is_err(),
        "moved while open"
    );
    assert!(std::fs::remove_file(&f.a).is_err(), "deleted while open");
    drop(held);
    std::fs::rename(&copy, &f.a).unwrap();
}

#[test]
fn the_expected_token_is_required_and_checked_before_anything_else() {
    let f = fixture();
    let token = shown(&f);
    for (expected, error) in [
        (Value::Null, "invalid_vault_token"),
        (json!(""), "invalid_vault_token"),
        (json!(42), "invalid_vault_token"),
        (json!("xyz"), "invalid_vault_token"),
        (json!(token.to_uppercase()), "invalid_vault_token"),
        (json!(format!("{token}0")), "invalid_vault_token"),
        (json!(token_of(&f.b)), "vault_switched"),
        (json!("0".repeat(64)), "vault_switched"),
    ] {
        // No dialog may open for any of these.
        let answer = backup(
            &mut Session::new(f.env.clone()),
            &NoDialogs,
            None,
            expected.clone(),
        );
        assert_eq!(answer["ok"], false, "{expected}");
        assert_eq!(answer["error"], error, "{expected}");
        assert!(answer.get("backupPath").is_none());
    }
    assert_eq!(files_in(&f.dest), Vec::<String>::new());
}

#[test]
fn case_e_cancel_and_failures_write_nothing_and_answer_no_success() {
    let f = fixture();
    let token = shown(&f);
    // Cancelled: nothing written.
    let dialogs = FolderDialog::new(None, || {});
    let answer = backup(
        &mut Session::new(f.env.clone()),
        &dialogs,
        None,
        json!(token),
    );
    assert_eq!(
        (answer["ok"].clone(), answer["error"].clone()),
        (json!(false), json!("cancelled"))
    );
    // The vault made active meanwhile is missing.
    let gone = f.tmp.0.join("gone.sqlite3");
    let dialogs = FolderDialog::new(Some(f.dest.clone()), || {
        let lock = vault::ConfigLock::acquire(&f.env).unwrap();
        vault::switch_active(&f.env, &lock, &gone, false).unwrap();
    });
    let answer = backup(
        &mut Session::new(f.env.clone()),
        &dialogs,
        None,
        json!(token),
    );
    assert_eq!(answer["error"], "vault_missing", "{answer}");
    // No vault configured at all.
    let empty = TempDir::new();
    let answer = backup(
        &mut Session::new(empty.env()),
        &NoDialogs,
        Some(&f.dest),
        json!(token),
    );
    assert_eq!(answer["error"], "vault_not_configured");
    // A destination that is not a folder.
    switch_by_another_host(&f, &f.a);
    let answer = backup(
        &mut Session::new(f.env.clone()),
        &NoDialogs,
        Some(&f.tmp.0.join("no-such-folder")),
        json!(token),
    );
    assert_eq!(answer["error"], "parent_missing");
    // A vault that cannot be backed up any more (its schema broken while the dialog was open).
    let dialogs = FolderDialog::new(Some(f.dest.clone()), || {
        open_vault(&f.a)
            .unwrap()
            .execute_batch("DROP TABLE tab_snapshots;")
            .unwrap();
    });
    let answer = backup(
        &mut Session::new(f.env.clone()),
        &dialogs,
        None,
        json!(token),
    );
    assert_eq!(answer["ok"], false, "{answer}");
    assert!(answer.get("backupPath").is_none());
    // Nothing (and no `.partial`) in the backup folder after any of these; B untouched.
    assert_eq!(files_in(&f.dest), Vec::<String>::new());
    assert_eq!(visits_in(&f.b), 5);
}

#[test]
fn a_backup_never_replaces_an_existing_file_also_across_vaults() {
    // Two backups in the same second (one of A, one of B) both survive under distinct names.
    let f = fixture();
    let first = backup(
        &mut Session::new(f.env.clone()),
        &NoDialogs,
        Some(&f.dest),
        json!(shown(&f)),
    );
    switch_by_another_host(&f, &f.b);
    let second = backup(
        &mut Session::new(f.env.clone()),
        &NoDialogs,
        Some(&f.dest),
        json!(shown(&f)),
    );
    assert_ne!(first["backupPath"], second["backupPath"]);
    assert_eq!(
        (backed_up(&first), backed_up(&second)),
        ("A".into(), "B".into())
    );
    assert_eq!(files_in(&f.dest).len(), 2);
}

#[test]
fn read_for_runs_only_on_the_tickets_vault() {
    let f = fixture();
    let mut session = Session::new(f.env.clone());
    let ticket = session.ticket().unwrap();
    let mut ran = false;
    let count = session.read_for(&ticket, |db| {
        ran = true;
        db.query_row("SELECT count(*) FROM visits", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(|_| VaultError::Db)
    });
    assert_eq!(count, Ok(3));
    assert!(ran);
    switch_by_another_host(&f, &f.b);
    let mut ran = false;
    let refused: Result<(), VaultError> = session.read_for(&ticket, |_| {
        ran = true;
        Ok(())
    });
    assert_eq!(refused, Err(VaultError::Switched));
    assert!(!ran, "f never runs on another vault");
    // `write_for` with the same ticket is refused as before.
    assert_eq!(
        session.write_for(&ticket, |_| Ok::<_, VaultError>(())),
        Err(VaultError::Switched)
    );
    // A ticket of B is good for B, once B is active.
    let ticket = session.ticket().unwrap();
    assert_eq!(ticket.token, token_of(&f.b));
    assert_eq!(session.read_for(&ticket, |_| Ok::<_, VaultError>(1)), Ok(1));
}
