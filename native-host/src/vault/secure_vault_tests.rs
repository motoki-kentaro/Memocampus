//! Issue #51, Slice B0: mode-aware opening, key errors and the key-independent switch fence
//! (B0-3), publication decisions bound to a vault's identity (B0-2), the version 1 → 2 config
//! migration (P3), and config durability (B0-5). Synthetic vaults in temporary directories only.

use super::tests::TempDir;
use super::*;
use std::cell::RefCell;
use std::rc::Rc;

/// Bytes that are neither plain SQLite nor empty: what an encrypted vault looks like to a process
/// without its key (SQLCipher starts with a random salt).
fn encrypted_like(path: &Path) -> Vec<u8> {
    let bytes: Vec<u8> = (0..4096u32).map(|i| (i * 7 + 13) as u8).collect();
    fs::write(path, &bytes).unwrap();
    bytes
}

fn config_with(env: &Environment, path: &Path, mode: VaultMode, publication: Option<Publication>) {
    let config = Config {
        publication,
        ..Config::new(path.to_string_lossy().into_owned(), false, mode)
    };
    write_config(env, &config).unwrap();
}

fn current(env: &Environment) -> Config {
    match resolve(env).unwrap() {
        Active::Configured(config) => config,
        other => panic!("unexpected {other:?}"),
    }
}

fn configure(env: &Environment, path: &Path) {
    let lock = ConfigLock::acquire(env).unwrap();
    switch_active(env, &lock, path, false).unwrap();
}

struct FixedKeys(Result<(), KeyError>);

impl KeySource for FixedKeys {
    fn key_for(&self, _: &Path) -> Result<VaultKey, KeyError> {
        self.0.map(|()| VaultKey([7; 32]))
    }
}

// --- B0-3: key errors and the switch fence --------------------------------------------------

#[test]
fn key_failures_have_their_own_codes_and_never_prove_the_vault_unwritable() {
    let cases = [
        (
            KeyError::NotSupported,
            VaultError::EncryptionUnsupported,
            "vault_encryption_unsupported",
        ),
        (
            KeyError::Missing,
            VaultError::KeyUnavailable,
            "key_unavailable",
        ),
        (
            KeyError::ProtectorFailed,
            VaultError::KeyUnavailable,
            "key_unavailable",
        ),
        (
            KeyError::Mismatch,
            VaultError::KeyUnavailable,
            "key_unavailable",
        ),
        (KeyError::Locked, VaultError::Locked, "vault_locked"),
    ];
    let tmp = TempDir::new();
    let path = tmp.0.join("enc.sqlite3");
    let bytes = encrypted_like(&path);
    for (error, kind, code) in cases {
        let failure =
            open_vault_in(&path, VaultMode::Encrypted, &FixedKeys(Err(error))).unwrap_err();
        assert_eq!(failure.kind, kind, "{error:?}");
        assert_eq!(failure.cause, OpenCause::Key(error));
        assert_eq!(kind.code(), code);
        assert!(!failure.proves_unwritable(), "{error:?}");
        assert!(failure.needs_handle_fence(), "{error:?}");
    }
    // A key that was obtained still opens nothing in this version.
    let failure = open_vault_in(&path, VaultMode::Encrypted, &FixedKeys(Ok(()))).unwrap_err();
    assert_eq!(failure.kind, VaultError::EncryptionUnsupported);
    assert!(!failure.proves_unwritable());
    assert_eq!(fs::read(&path).unwrap(), bytes, "never handed to SQLite");
    for suffix in SIDECAR_SUFFIXES {
        assert!(!sidecar(&path, suffix).exists());
    }
}

#[test]
fn a_refused_key_is_a_decrypt_failure_never_proof_while_plain_not_a_database_stays_proof() {
    let not_a_database = rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(26), None);
    let keyed = OpenFailure::from_sqlite_in(&not_a_database, VaultMode::Encrypted);
    assert_eq!(keyed.kind, VaultError::DecryptFailed);
    assert!(!keyed.proves_unwritable());
    assert!(keyed.needs_handle_fence());
    let corrupt = rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(11), None);
    let keyed_corrupt = OpenFailure::from_sqlite_in(&corrupt, VaultMode::Encrypted);
    assert_eq!(keyed_corrupt.kind, VaultError::Invalid);
    assert!(
        !keyed_corrupt.proves_unwritable(),
        "nothing proves in encrypted mode"
    );
    // Regression: plain mode keeps its rules.
    let plain = OpenFailure::from_sqlite(&not_a_database);
    assert_eq!(plain.kind, VaultError::Invalid);
    assert!(plain.proves_unwritable());
    assert!(!plain.needs_handle_fence());
    // Absence proves in either mode.
    let tmp = TempDir::new();
    let missing = open_vault_in(&tmp.0.join("gone"), VaultMode::Encrypted, &NoKeys).unwrap_err();
    assert_eq!(missing.kind, VaultError::Missing);
    assert!(missing.proves_unwritable());
    // Every other failure in encrypted mode, whatever its kind and cause, proves nothing.
    for (kind, cause) in [
        (VaultError::Invalid, OpenCause::NotAFile),
        (VaultError::Invalid, OpenCause::ForeignApplicationId(1)),
        (VaultError::SchemaMismatch, OpenCause::UnsupportedVersion(0)),
        (VaultError::ModeMismatch, OpenCause::ModeMismatch),
    ] {
        let failure = OpenFailure::new(kind, cause.clone()).in_mode(VaultMode::Encrypted);
        assert!(!failure.proves_unwritable(), "{failure:?}");
    }
}

#[test]
fn a_file_contradicting_the_recorded_mode_is_a_mode_mismatch_and_left_alone() {
    let tmp = TempDir::new();
    let plain = tmp.0.join("plain.sqlite3");
    create_vault(&plain).unwrap();
    let before = fs::read(&plain).unwrap();
    let failure = open_vault_in(&plain, VaultMode::Encrypted, &FixedKeys(Ok(()))).unwrap_err();
    assert_eq!(failure.kind, VaultError::ModeMismatch);
    assert!(!failure.proves_unwritable());
    let empty = tmp.0.join("empty.sqlite3");
    fs::write(&empty, b"").unwrap();
    assert_eq!(
        open_vault_in(&empty, VaultMode::Encrypted, &NoKeys)
            .unwrap_err()
            .kind,
        VaultError::ModeMismatch
    );
    assert_eq!(fs::read(&plain).unwrap(), before);
    assert_eq!(fs::metadata(&empty).unwrap().len(), 0);
}

#[test]
fn a_session_never_writes_to_reads_or_initializes_a_vault_it_cannot_key() {
    let tmp = TempDir::new();
    let env = tmp.env();
    let path = tmp.0.join("enc.sqlite3");
    let bytes = encrypted_like(&path);
    config_with(&env, &path, VaultMode::Encrypted, None);
    for (keys, code) in [
        (FixedKeys(Err(KeyError::Missing)), "key_unavailable"),
        (FixedKeys(Err(KeyError::Locked)), "vault_locked"),
        (FixedKeys(Ok(())), "vault_encryption_unsupported"),
    ] {
        let mut session = Session::with_keys(env.clone(), Box::new(keys));
        let written = session.write(|db| {
            db.execute("INSERT INTO visits VALUES ('x', 'https://a.test/', 'A', '2026-11-09T00:00:00.000Z')", [])
        });
        assert_eq!(written.map_err(VaultError::code), Err(code));
        assert_eq!(session.read(|_| ()).map_err(VaultError::code), Err(code));
    }
    let mut session = Session::new(env.clone());
    assert_eq!(session.ticket(), Err(VaultError::EncryptionUnsupported));
    // Creating a vault at that path is refused: nothing re-initializes it.
    assert_eq!(create_vault(&path), Err(VaultError::FileExists));
    assert_eq!(fs::read(&path).unwrap(), bytes);
    for suffix in SIDECAR_SUFFIXES {
        assert!(!sidecar(&path, suffix).exists());
    }
}

#[test]
fn a_cached_plain_connection_is_dropped_when_the_config_changes_the_mode() {
    let tmp = TempDir::new();
    let env = tmp.env();
    let a = tmp.0.join("a.sqlite3");
    create_vault(&a).unwrap();
    configure(&env, &a);
    let mut session = Session::new(env.clone());
    session
        .write(|db| db.execute("INSERT INTO visits VALUES ('1', 'https://a.test/', 'A', '2026-11-09T00:00:00.000Z')", []))
        .unwrap();
    // The same path, now recorded as encrypted: the plain connection must not be reused.
    config_with(&env, &a, VaultMode::Encrypted, None);
    assert_eq!(
        session.write(|db| db.execute("INSERT INTO visits VALUES ('2', 'https://a.test/', 'A', '2026-11-09T00:00:00.000Z')", [])),
        Err(VaultError::ModeMismatch)
    );
    // A switch that lands between the revalidation and the write lock is caught under the lock.
    configure(&env, &a);
    session.read(|_| ()).unwrap();
    config_with(&env, &a, VaultMode::Encrypted, None);
    assert_eq!(
        session.write_with(false, |db| db.execute("INSERT INTO visits VALUES ('3', 'https://a.test/', 'A', '2026-11-09T00:00:00.000Z')", [])),
        Err(VaultError::Switched)
    );
    let ids: Vec<String> = open_vault(&a)
        .unwrap()
        .prepare("SELECT event_id FROM visits")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(ids, ["1"]);
}

#[test]
fn switching_away_from_an_unkeyable_vault_holds_an_exclusive_handle_instead() {
    let tmp = TempDir::new();
    let env = tmp.env();
    let (enc, b) = (tmp.0.join("enc.sqlite3"), tmp.0.join("b.sqlite3"));
    let bytes = encrypted_like(&enc);
    create_vault(&b).unwrap();
    config_with(&env, &enc, VaultMode::Encrypted, None);

    // While the switch holds its fence, nobody can open the old vault: not a reader, not a writer
    // that would have the key (modelled by SQLite opening the file).
    let seen = Rc::new(RefCell::new(Vec::new()));
    let (path, record) = (enc.clone(), seen.clone());
    WHILE_FENCED.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            let file = File::open(&path).map(|_| ()).map_err(|e| e.raw_os_error());
            let sqlite = Connection::open(&path)
                .and_then(|db| {
                    db.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
                        r.get::<_, i64>(0)
                    })
                })
                .map(|_| ())
                .map_err(|e| e.sqlite_error_code());
            record.borrow_mut().push((file, sqlite));
        }))
    });
    let lock = ConfigLock::acquire(&env).unwrap();
    let switched = switch_active(&env, &lock, &b, false);
    WHILE_FENCED.with(|hook| hook.borrow_mut().take());
    switched.unwrap();
    assert_eq!(
        *seen.borrow(),
        [(Err(Some(32)), Err(Some(rusqlite::ErrorCode::CannotOpen)))],
        "sharing violation for a plain open, CannotOpen (temporary) for SQLite, never NotADatabase"
    );
    assert_eq!(current(&env).active_vault, b.to_string_lossy());
    assert_eq!(
        fs::read(&enc).unwrap(),
        bytes,
        "the old vault is not changed"
    );
}

#[test]
fn the_exclusive_handle_fence_waits_for_any_process_holding_the_vault() {
    let tmp = TempDir::new();
    let env = tmp.env();
    let (enc, b) = (tmp.0.join("enc.sqlite3"), tmp.0.join("b.sqlite3"));
    encrypted_like(&enc);
    create_vault(&b).unwrap();
    config_with(&env, &enc, VaultMode::Encrypted, None);
    let lock = ConfigLock::acquire(&env).unwrap();
    // A process that has the key would hold the file open (SQLite shares read and write).
    let holder = File::open(&enc).unwrap();
    assert_eq!(switch_active(&env, &lock, &b, false), Err(VaultError::Busy));
    assert_eq!(
        current(&env).active_vault,
        enc.to_string_lossy(),
        "config unchanged"
    );
    assert_eq!(current(&env).mode, VaultMode::Encrypted);
    drop(holder);
    // Every key failure takes the same fence: none of them skips it.
    for error in [
        KeyError::Missing,
        KeyError::ProtectorFailed,
        KeyError::Mismatch,
        KeyError::Locked,
    ] {
        config_with(&env, &enc, VaultMode::Encrypted, None);
        let holder = File::open(&enc).unwrap();
        assert_eq!(
            switch_active_with(&env, &lock, &b, false, &FixedKeys(Err(error))),
            Err(VaultError::Busy),
            "{error:?}"
        );
        drop(holder);
        switch_active_with(&env, &lock, &b, false, &FixedKeys(Err(error))).unwrap();
        assert_eq!(current(&env).active_vault, b.to_string_lossy());
    }
}

#[test]
fn a_mode_mismatch_never_skips_the_fence() {
    let tmp = TempDir::new();
    let env = tmp.env();
    let (a, b) = (tmp.0.join("a.sqlite3"), tmp.0.join("b.sqlite3"));
    create_vault(&a).unwrap();
    create_vault(&b).unwrap();
    // A plain vault recorded as encrypted: opening it as plain to take the write lock would
    // mean guessing the mode, so the exclusive handle is used, and a connection blocks it.
    config_with(&env, &a, VaultMode::Encrypted, None);
    let writer = open_vault(&a).unwrap();
    let lock = ConfigLock::acquire(&env).unwrap();
    assert_eq!(switch_active(&env, &lock, &b, false), Err(VaultError::Busy));
    drop(writer);
    switch_active(&env, &lock, &b, false).unwrap();
    assert_eq!(current(&env).active_vault, b.to_string_lossy());
}

#[test]
fn a_plain_old_vault_keeps_the_write_lock_fence_and_its_existing_rules() {
    let tmp = TempDir::new();
    let env = tmp.env();
    let (a, b, junk) = (
        tmp.0.join("a.sqlite3"),
        tmp.0.join("b.sqlite3"),
        tmp.0.join("junk.sqlite3"),
    );
    create_vault(&a).unwrap();
    create_vault(&b).unwrap();
    fs::write(
        &junk,
        b"definitely not a database, far longer than a header",
    )
    .unwrap();
    configure(&env, &a);
    // An ordinary reader (shared handle) does not block the write-lock fence of a plain vault.
    let reader = File::open(&a).unwrap();
    configure(&env, &b);
    drop(reader);
    // A plain junk file still proves unwritable: no fence, as before.
    configure(&env, &junk);
    configure(&env, &b);
    assert_eq!(current(&env).active_vault, b.to_string_lossy());
}

// --- B0-2: publication ------------------------------------------------------------------------

fn publication_of(env: &Environment) -> Option<(String, PublicationState)> {
    current(env)
        .publication
        .map(|publication| (publication.vault_token, publication.state))
}

#[test]
fn every_switch_starts_pending_for_the_new_file_and_only_the_same_file_keeps_its_approval() {
    let tmp = TempDir::new();
    let env = tmp.env();
    let (a, b) = (tmp.0.join("a.sqlite3"), tmp.0.join("b.sqlite3"));
    create_vault(&a).unwrap();
    create_vault(&b).unwrap();
    let (ta, tb) = (vault_token(&a).unwrap(), vault_token(&b).unwrap());
    configure(&env, &a);
    assert_eq!(
        publication_of(&env),
        Some((ta.clone(), PublicationState::Pending))
    );
    let lock = ConfigLock::acquire(&env).unwrap();
    set_publication(
        &env,
        &lock,
        &ta,
        PublicationChange::Confirm { policy_revision: 3 },
    )
    .unwrap();
    assert_eq!(
        publication_of(&env),
        Some((ta.clone(), PublicationState::Confirmed))
    );
    assert_eq!(current(&env).publication.unwrap().policy_revision, Some(3));
    drop(lock);

    // Re-opening the same file keeps its decision; any other file starts pending.
    configure(&env, &a);
    assert_eq!(
        publication_of(&env),
        Some((ta.clone(), PublicationState::Confirmed))
    );
    configure(&env, &b);
    assert_eq!(
        publication_of(&env),
        Some((tb.clone(), PublicationState::Pending))
    );
    // Back to A: A's earlier approval is not reused (one decision is kept, for the active vault).
    configure(&env, &a);
    assert_eq!(
        publication_of(&env),
        Some((ta.clone(), PublicationState::Pending))
    );

    // A copy of A is another file: pending.
    let copy = tmp.0.join("copy.sqlite3");
    fs::copy(&a, &copy).unwrap();
    let lock = ConfigLock::acquire(&env).unwrap();
    set_publication(
        &env,
        &lock,
        &ta,
        PublicationChange::Confirm { policy_revision: 3 },
    )
    .unwrap();
    drop(lock);
    configure(&env, &copy);
    let (token, state) = publication_of(&env).unwrap();
    assert_ne!(token, ta);
    assert_eq!(state, PublicationState::Pending);
}

#[test]
fn a_decision_applies_only_to_the_vault_the_user_saw() {
    let tmp = TempDir::new();
    let env = tmp.env();
    let (a, b) = (tmp.0.join("a.sqlite3"), tmp.0.join("b.sqlite3"));
    create_vault(&a).unwrap();
    create_vault(&b).unwrap();
    let ta = vault_token(&a).unwrap();
    configure(&env, &a);
    // The panel showed A; another process switched to B before the confirmation arrived.
    configure(&env, &b);
    let before = fs::read(env.config_path().unwrap()).unwrap();
    let lock = ConfigLock::acquire(&env).unwrap();
    assert_eq!(
        set_publication(
            &env,
            &lock,
            &ta,
            PublicationChange::Confirm { policy_revision: 1 }
        ),
        Err(VaultError::Switched)
    );
    assert_eq!(
        set_publication(&env, &lock, &ta, PublicationChange::Revoke),
        Err(VaultError::Switched)
    );
    assert_eq!(fs::read(env.config_path().unwrap()).unwrap(), before);

    // A file replaced at the same path is not the vault shown either.
    let tb = vault_token(&b).unwrap();
    fs::remove_file(&b).unwrap();
    create_vault(&b).unwrap();
    assert_eq!(
        set_publication(
            &env,
            &lock,
            &tb,
            PublicationChange::Confirm { policy_revision: 1 }
        ),
        Err(VaultError::Switched)
    );
}

#[test]
fn only_a_usable_vault_can_be_confirmed_but_any_can_be_revoked() {
    let tmp = TempDir::new();
    let env = tmp.env();
    let lock = ConfigLock::acquire(&env).unwrap();
    let token = "0".repeat(64);
    let confirm = PublicationChange::Confirm { policy_revision: 1 };
    assert_eq!(
        set_publication(&env, &lock, &token, confirm),
        Err(VaultError::NotConfigured)
    );

    let junk = tmp.0.join("junk.sqlite3");
    fs::write(&junk, b"not a database at all, but long enough").unwrap();
    let tj = vault_token(&junk).unwrap();
    switch_active(&env, &lock, &junk, false).unwrap();
    assert_eq!(
        set_publication(&env, &lock, &tj, confirm),
        Err(VaultError::Invalid)
    );
    let enc = tmp.0.join("enc.sqlite3");
    encrypted_like(&enc);
    let te = vault_token(&enc).unwrap();
    config_with(
        &env,
        &enc,
        VaultMode::Encrypted,
        Some(Publication {
            vault_token: te.clone(),
            state: PublicationState::Confirmed,
            policy_revision: None,
        }),
    );
    assert_eq!(
        set_publication(&env, &lock, &te, confirm),
        Err(VaultError::EncryptionUnsupported)
    );
    set_publication(&env, &lock, &te, PublicationChange::Revoke).unwrap();
    assert_eq!(publication_of(&env), Some((te, PublicationState::Pending)));

    fs::write(env.config_path().unwrap(), b"{\"version\":2,").unwrap();
    assert_eq!(
        set_publication(&env, &lock, &tj, confirm),
        Err(VaultError::ConfigInvalid)
    );
    let overridden = Environment {
        db_override: Some(junk),
        ..env
    };
    assert_eq!(
        set_publication(&overridden, &lock, &tj, confirm),
        Err(VaultError::ManagementDisabled)
    );
}

// --- P3: version 1 → 2 ------------------------------------------------------------------------

fn legacy(env: &Environment, path: &Path) {
    fs::create_dir_all(env.app_dir.as_ref().unwrap()).unwrap();
    fs::write(
        env.config_path().unwrap(),
        serde_json::json!({"version": 1, "activeVault": path, "locationApproved": true})
            .to_string(),
    )
    .unwrap();
}

#[test]
fn a_legacy_active_vault_keeps_mcp_access_only_with_a_ready_policy() {
    let tmp = TempDir::new();
    let env = tmp.env();
    let a = tmp.0.join("a.sqlite3");
    create_vault(&a).unwrap();
    let ta = vault_token(&a).unwrap();

    legacy(&env, &a);
    let migration = migrate_legacy_config(&env, || PolicyReadiness::Ready(7));
    let expected = Publication {
        vault_token: ta.clone(),
        state: PublicationState::Confirmed,
        policy_revision: Some(7),
    };
    assert_eq!(migration, Migration::Migrated(Some(expected.clone())));
    let config = current(&env);
    assert_eq!(config.version, CONFIG_VERSION);
    assert_eq!(config.mode, VaultMode::Plain);
    assert!(config.location_approved, "kept");
    assert_eq!(config.publication, Some(expected));
    assert_eq!(
        migrate_legacy_config(&env, || unreachable!()),
        Migration::NotNeeded
    );

    legacy(&env, &a);
    assert_eq!(
        migrate_legacy_config(&env, || PolicyReadiness::NotReady),
        Migration::Migrated(Some(Publication {
            vault_token: ta,
            state: PublicationState::Pending,
            policy_revision: None,
        }))
    );

    // No usable vault: nothing to approve.
    let missing = tmp.0.join("missing.sqlite3");
    legacy(&env, &missing);
    assert_eq!(
        migrate_legacy_config(&env, || PolicyReadiness::Ready(1)),
        Migration::Migrated(None)
    );
    assert!(!missing.exists());
}

#[test]
fn nothing_is_decided_from_a_transient_failure() {
    let tmp = TempDir::new();
    let env = tmp.env();
    let a = tmp.0.join("a.sqlite3");
    create_vault(&a).unwrap();
    legacy(&env, &a);
    let before = fs::read(env.config_path().unwrap()).unwrap();

    assert_eq!(
        migrate_legacy_config(&env, || PolicyReadiness::Unknown),
        Migration::Deferred
    );
    let held = ConfigLock::acquire(&env).unwrap();
    assert_eq!(
        migrate_legacy_config(&env, || unreachable!()),
        Migration::Deferred
    );
    drop(held);
    let writer = open_vault(&a).unwrap();
    writer
        .execute_batch("BEGIN EXCLUSIVE; INSERT INTO visits VALUES ('x', 'https://a.test/', 'A', '2026-11-09T00:00:00.000Z');")
        .unwrap();
    assert_eq!(
        migrate_legacy_config(&env, || unreachable!()),
        Migration::Deferred
    );
    writer.execute_batch("COMMIT").unwrap();
    assert_eq!(
        fs::read(env.config_path().unwrap()).unwrap(),
        before,
        "still version 1"
    );

    let overridden = Environment {
        db_override: Some(a.clone()),
        ..env.clone()
    };
    assert_eq!(
        migrate_legacy_config(&overridden, || unreachable!()),
        Migration::NotNeeded
    );
    assert!(matches!(
        migrate_legacy_config(&env, || PolicyReadiness::Ready(1)),
        Migration::Migrated(Some(_))
    ));
}

// --- B0-5: config durability ------------------------------------------------------------------

fn temp_files(env: &Environment) -> Vec<String> {
    fs::read_dir(env.app_dir.as_ref().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".tmp"))
        .collect()
}

#[test]
fn a_failed_config_write_at_any_step_keeps_the_old_config_and_no_temp_file() {
    let tmp = TempDir::new();
    let env = tmp.env();
    let (a, b) = (tmp.0.join("a.sqlite3"), tmp.0.join("b.sqlite3"));
    create_vault(&a).unwrap();
    create_vault(&b).unwrap();
    configure(&env, &a);
    let before = fs::read(env.config_path().unwrap()).unwrap();
    let lock = ConfigLock::acquire(&env).unwrap();
    for step in [
        ConfigStep::Create,
        ConfigStep::Write,
        ConfigStep::Sync,
        ConfigStep::Replace,
    ] {
        CONFIG_FAULT.with(|fault| fault.set(Some(step)));
        let result = switch_active(&env, &lock, &b, false);
        CONFIG_FAULT.with(|fault| fault.set(None));
        assert_eq!(result, Err(VaultError::Io), "{step:?}");
        assert_eq!(
            fs::read(env.config_path().unwrap()).unwrap(),
            before,
            "{step:?}"
        );
        assert!(temp_files(&env).is_empty(), "{step:?}");
        // The fence was released: the old vault takes writes.
        open_vault(&a)
            .unwrap()
            .execute_batch("BEGIN IMMEDIATE; ROLLBACK;")
            .unwrap();
    }
}

#[test]
fn a_crash_before_the_replace_leaves_the_old_config_in_force() {
    let tmp = TempDir::new();
    let env = tmp.env();
    let (a, b) = (tmp.0.join("a.sqlite3"), tmp.0.join("b.sqlite3"));
    create_vault(&a).unwrap();
    create_vault(&b).unwrap();
    configure(&env, &a);
    let before = fs::read(env.config_path().unwrap()).unwrap();
    let lock = ConfigLock::acquire(&env).unwrap();
    CONFIG_FAULT.with(|fault| fault.set(Some(ConfigStep::CrashBeforeReplace)));
    let result = switch_active(&env, &lock, &b, false);
    CONFIG_FAULT.with(|fault| fault.set(None));
    assert_eq!(result, Err(VaultError::Io));
    // What a restarted host finds: the old config, and a complete but unused temp file.
    assert_eq!(fs::read(env.config_path().unwrap()).unwrap(), before);
    assert_eq!(temp_files(&env).len(), 1);
    assert_eq!(current(&env).active_vault, a.to_string_lossy());
    // The next switch is unaffected by the leftover.
    switch_active(&env, &lock, &b, false).unwrap();
    assert_eq!(current(&env).active_vault, b.to_string_lossy());
}

#[test]
fn a_torn_or_foreign_config_is_never_taken_as_a_valid_one() {
    let tmp = TempDir::new();
    let env = tmp.env();
    let a = tmp.0.join("a.sqlite3");
    create_vault(&a).unwrap();
    configure(&env, &a);
    let whole = fs::read(env.config_path().unwrap()).unwrap();
    // Every prefix of a valid config, as a non-atomic writer could leave it.
    for len in 0..whole.len() {
        fs::write(env.config_path().unwrap(), &whole[..len]).unwrap();
        assert_eq!(
            resolve(&env),
            Err(VaultError::ConfigInvalid),
            "prefix {len}"
        );
        let mut session = Session::new(env.clone());
        assert_eq!(session.read(|_| ()), Err(VaultError::ConfigInvalid));
    }
    fs::write(env.config_path().unwrap(), &whole).unwrap();
    assert!(resolve(&env).is_ok());
}

#[test]
fn concurrent_config_writers_never_leave_a_mixed_or_partial_file() {
    let tmp = TempDir::new();
    let env = tmp.env();
    let paths: Vec<PathBuf> = (0..4)
        .map(|i| tmp.0.join(format!("v{i}.sqlite3")))
        .collect();
    write_config(
        &env,
        &Config::new(
            paths[0].to_string_lossy().into_owned(),
            false,
            VaultMode::Plain,
        ),
    )
    .unwrap();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reader = {
        let (env, stop) = (env.clone(), stop.clone());
        std::thread::spawn(move || {
            let (mut reads, mut unreadable) = (0, 0);
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                match resolve(&env) {
                    Ok(Active::Configured(_)) => reads += 1,
                    // A reader may meet the replace in progress: unreadable, never invalid.
                    Err(VaultError::Io) => unreadable += 1,
                    other => panic!("{other:?}"),
                }
            }
            (reads, unreadable)
        })
    };
    let writers: Vec<_> = paths
        .iter()
        .map(|path| {
            let (env, path) = (env.clone(), path.clone());
            std::thread::spawn(move || {
                let config =
                    Config::new(path.to_string_lossy().into_owned(), true, VaultMode::Plain);
                (0..40)
                    .filter(|_| write_config(&env, &config).is_ok())
                    .count()
            })
        })
        .collect();
    let written: usize = writers
        .into_iter()
        .map(|writer| writer.join().unwrap())
        .sum();
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let (reads, _) = reader.join().unwrap();
    assert!(written > 0 && reads > 0);
    let last = current(&env);
    assert!(paths
        .iter()
        .any(|path| last.active_vault == path.to_string_lossy()));
    assert!(
        temp_files(&env).is_empty(),
        "failed writers remove their temp files"
    );
}

// --- F-02: a policy initialization withdraws the publication -----------------------------------

fn confirmed(env: &Environment, path: &Path) -> String {
    configure(env, path);
    let token = vault_token(path).unwrap();
    let lock = ConfigLock::acquire(env).unwrap();
    set_publication(
        env,
        &lock,
        &token,
        PublicationChange::Confirm { policy_revision: 3 },
    )
    .unwrap();
    token
}

fn gate(env: &Environment) -> crate::policy_gate::PolicyGate {
    let store =
        memocampus_native_host::policy_store::PolicyStore::new(env.app_dir.clone().unwrap())
            .unwrap();
    crate::policy_gate::PolicyGate::with_store_in(store, env.clone())
}

fn initialize(env: &Environment) -> serde_json::Value {
    gate(env).handle(&serde_json::json!({ "type": "policy_initialize", "requestId": "i" }))
}

fn policy_files(env: &Environment) -> [bool; 2] {
    let app = env.app_dir.as_ref().unwrap();
    ["policy.json", "policy.initialized"].map(|name| app.join(name).exists())
}

fn lose_policy(env: &Environment) {
    let app = env.app_dir.as_ref().unwrap();
    for name in ["policy.json", "policy.initialized"] {
        fs::remove_file(app.join(name)).unwrap();
    }
}

#[test]
fn withdrawing_sets_only_a_confirmed_publication_back_to_pending_with_its_own_token() {
    let tmp = TempDir::new();
    let env = tmp.env();
    // Nothing configured: nothing written.
    assert_eq!(withdraw_publication(&env), Ok(Withdrawal::NothingPublished));
    assert!(!env.config_path().unwrap().exists());

    let a = tmp.0.join("a.sqlite3");
    create_vault(&a).unwrap();
    let ta = confirmed(&env, &a);
    assert_eq!(withdraw_publication(&env), Ok(Withdrawal::Withdrawn));
    let config = current(&env);
    assert_eq!(
        config.publication,
        Some(Publication {
            vault_token: ta,
            state: PublicationState::Pending,
            policy_revision: None,
        })
    );
    assert_eq!(config.active_vault, a.to_string_lossy(), "same vault");
    // Already pending: the file is not rewritten.
    let before = fs::read(env.config_path().unwrap()).unwrap();
    assert_eq!(withdraw_publication(&env), Ok(Withdrawal::NothingPublished));
    assert_eq!(fs::read(env.config_path().unwrap()).unwrap(), before);
    // A config MCP refuses anyway is left as it is.
    fs::write(env.config_path().unwrap(), b"{\"version\":2,").unwrap();
    assert_eq!(withdraw_publication(&env), Ok(Withdrawal::NothingPublished));
    assert_eq!(
        fs::read(env.config_path().unwrap()).unwrap(),
        b"{\"version\":2,"
    );
    // Vault management disabled: nothing to withdraw.
    let overridden = Environment {
        db_override: Some(a),
        ..env
    };
    assert_eq!(
        withdraw_publication(&overridden),
        Ok(Withdrawal::NothingPublished)
    );
}

#[test]
fn a_legacy_config_is_closed_so_a_later_start_cannot_grandfather_it_under_the_new_policy() {
    let tmp = TempDir::new();
    let env = tmp.env();
    let a = tmp.0.join("a.sqlite3");
    create_vault(&a).unwrap();
    legacy(&env, &a);
    assert_eq!(withdraw_publication(&env), Ok(Withdrawal::LegacyClosed));
    let config = current(&env);
    assert_eq!(config.version, CONFIG_VERSION);
    assert_eq!(config.publication, None);
    assert!(config.location_approved, "kept");
    // The next start finds nothing to migrate, even with a ready policy.
    assert_eq!(
        migrate_legacy_config(&env, || PolicyReadiness::Ready(1)),
        Migration::NotNeeded
    );
    assert_eq!(current(&env).publication, None);
}

#[test]
fn initializing_a_lost_policy_withdraws_the_publication_before_writing_it() {
    let tmp = TempDir::new();
    let env = tmp.env();
    let a = tmp.0.join("a.sqlite3");
    create_vault(&a).unwrap();
    assert_eq!(initialize(&env)["ok"], true);
    let ta = confirmed(&env, &a);

    // An initialized policy is never replaced, and the approval stays.
    assert_eq!(initialize(&env)["error"], "policy_already_initialized");
    assert_eq!(
        publication_of(&env),
        Some((ta.clone(), PublicationState::Confirmed))
    );

    // The policy is lost and initialized again: pending, for the same vault.
    lose_policy(&env);
    let reply = initialize(&env);
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["revision"], 1);
    assert_eq!(publication_of(&env), Some((ta, PublicationState::Pending)));
}

#[test]
fn a_failed_withdrawal_initializes_nothing_and_a_retry_never_confirms_again() {
    let tmp = TempDir::new();
    let env = tmp.env();
    let a = tmp.0.join("a.sqlite3");
    create_vault(&a).unwrap();
    assert_eq!(initialize(&env)["ok"], true);
    let ta = confirmed(&env, &a);
    lose_policy(&env);

    // The config cannot be written: no policy is written either. MCP reads nothing meanwhile
    // (no policy), whatever the publication still says.
    for step in [ConfigStep::Create, ConfigStep::Write, ConfigStep::Replace] {
        CONFIG_FAULT.with(|fault| fault.set(Some(step)));
        let reply = initialize(&env);
        CONFIG_FAULT.with(|fault| fault.set(None));
        assert_eq!(reply["ok"], false, "{step:?}");
        assert_eq!(reply["error"], "policy_unavailable", "{step:?}");
        assert_eq!(policy_files(&env), [false, false], "{step:?}");
        assert_eq!(
            publication_of(&env),
            Some((ta.clone(), PublicationState::Confirmed))
        );
    }
    // Another process holds the config lock: refused as busy after the wait, nothing written.
    let held = ConfigLock::acquire(&env).unwrap();
    let reply = initialize(&env);
    drop(held);
    assert_eq!(reply["error"], "policy_busy", "{reply}");
    assert_eq!(policy_files(&env), [false, false]);

    // The retry succeeds and leaves the vault pending; nothing turns it back to confirmed.
    assert_eq!(initialize(&env)["ok"], true);
    assert_eq!(
        publication_of(&env),
        Some((ta.clone(), PublicationState::Pending))
    );
    assert_eq!(initialize(&env)["error"], "policy_already_initialized");
    assert_eq!(
        migrate_legacy_config(&env, || PolicyReadiness::Ready(1)),
        Migration::NotNeeded
    );
    assert_eq!(publication_of(&env), Some((ta, PublicationState::Pending)));
}

#[test]
fn a_policy_written_after_the_withdrawal_fails_leaves_the_vault_pending() {
    let tmp = TempDir::new();
    let env = tmp.env();
    let a = tmp.0.join("a.sqlite3");
    create_vault(&a).unwrap();
    let store =
        memocampus_native_host::policy_store::PolicyStore::new(env.app_dir.clone().unwrap())
            .unwrap();
    store.initialize().unwrap();
    let ta = confirmed(&env, &a);
    lose_policy(&env);
    // The withdrawal succeeds, then the policy cannot be written (its marker path is taken).
    let marker = env.app_dir.as_ref().unwrap().join("policy.initialized");
    let result = store.initialize_with_then(&Default::default(), || {
        withdraw_publication(&env).unwrap();
        fs::create_dir(&marker).unwrap();
        Ok(())
    });
    assert!(result.is_err());
    assert!(!env.app_dir.as_ref().unwrap().join("policy.json").exists());
    // Safe side: re-approval needed rather than a publication that carries on.
    assert_eq!(publication_of(&env), Some((ta, PublicationState::Pending)));
}

#[test]
fn a_withdrawal_after_a_switch_touches_only_the_vault_now_active() {
    let tmp = TempDir::new();
    let env = tmp.env();
    let (a, b) = (tmp.0.join("a.sqlite3"), tmp.0.join("b.sqlite3"));
    create_vault(&a).unwrap();
    create_vault(&b).unwrap();
    assert_eq!(initialize(&env)["ok"], true);
    let ta = confirmed(&env, &a);
    let tb = confirmed(&env, &b);
    lose_policy(&env);
    assert_eq!(initialize(&env)["ok"], true);
    assert_eq!(
        publication_of(&env),
        Some((tb.clone(), PublicationState::Pending))
    );
    // Back to A: pending as after any switch (B0), never A's old approval.
    configure(&env, &a);
    assert_eq!(publication_of(&env), Some((ta, PublicationState::Pending)));
    assert_ne!(publication_of(&env).unwrap().0, tb);
}
