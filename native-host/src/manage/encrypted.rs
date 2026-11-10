//! Encrypted vaults in the side panel (Issue #51, Slice B): creation with a Recovery Spell
//! (ふっかつのじゅもん), opening an existing one, and recovery with the spell.
//!
//! # Creation (`vault_create_encrypted` → `vault_create_encrypted_commit` | `_cancel`)
//!
//! Chrome starts a host process per message, so a creation spans processes. Its state is the
//! manifest `<app dir>\vault-create.json` and three pending files, none of which can be taken for
//! a usable vault:
//!
//! | Phase (manifest) | Files | Active vault |
//! |---|---|---|
//! | none | — | unchanged |
//! | `creating` | `<target>.pending` (keyed DB), `keys\<id>.json.pending` (DPAPI) | unchanged |
//! | `awaiting_confirmation` | both, verified; the spell was shown once | unchanged |
//! | `promoted` | `<target>`, `keys\<id>.json` | unchanged |
//! | none (committed) | the same | `<target>`, publication pending |
//!
//! - Begin generates the Recovery Spell (256 CSPRNG bits) and the salt (128 CSPRNG bits); VK and
//!   the vault id follow from them (Slice E, `recovery`). It writes the files (each never replacing
//!   anything; no sidecar), checks them, and returns the spell (43 base64url characters) once. The
//!   spell and VK are never written anywhere in the clear: VK only DPAPI-protected, the spell
//!   nowhere. No message shows it again.
//! - Commit needs the whole spell as the user re-entered (typed or pasted) it in the panel, all 43
//!   characters: the host checks that it derives, with the vault's salt, the key of the pending
//!   vault, i.e. that the spell the user kept is the one that recovers it. A mistyped spell is refused (`recovery_spell_rejected` or
//!   `recovery_spell_invalid`) and the creation stays pending, so the user can try again. It then verifies the DB, promotes the files (no replacement) and switches the active
//!   vault, which is the commit point. A retried commit continues from where a crash stopped.
//! - Cancel removes only files that name this creation's vault id and salt, never an active vault.
//! - While a manifest exists no other creation starts (`vault_creation_pending`), so an interrupted
//!   creation can never be mixed up with a new key. The spell cannot be shown again: an
//!   unconfirmed creation is cancelled and started over.
//!
//! # Opening and recovery (Slice E)
//!
//! `vault_open` of a file that is not plain SQLite reads its salt (the first 16 bytes), from which
//! its vault id follows, and unlocks it with this user's DPAPI key store. If there is no usable key
//! (`key_unavailable`), nothing changes and the panel offers recovery: `vault_recover` takes the
//! spell, derives VK from it and the file's salt, proves the key on the vault itself, and only then
//! stores a DPAPI entry for this user and makes the vault active, publication pending. The vault
//! file and the spell are all it needs: no sidecar, no other file, no key of another PC. A wrong
//! spell or a damaged file changes nothing.
//!
//! A vault of an earlier scheme (Slices B–D: its `.mckey` sidecar or a version 1 key store entry
//! names its salt) is `vault_legacy_unsupported`: never opened, changed or removed by this build.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::{
    approved, check_location, display, normalize_existing, normalize_new, requested_path,
    require_management, status, Failure, DEFAULT_VAULT_NAME,
};
use crate::dialog::Dialogs;
use crate::location;
use crate::vault::{
    self, Active, CipherSalt, ConfigLock, KeyError, KeyRequest, KeySource, KeyStore, NewActive,
    Session, VaultError, VaultId, VaultKey,
};
use memocampus_native_host::active_vault::VaultMode;
use memocampus_native_host::secure_vault::{
    cipher, fsutil, key_store,
    recovery::{self, NewVault},
    secret::RecoverySpell,
    sidecar, spell,
};
use zeroize::Zeroizing;

pub const MANIFEST: &str = "vault-create.json";
/// Version 2 (Slice E): no sidecar. A version 1 manifest (an unfinished creation of an earlier
/// scheme) is unreadable here and can only be cancelled.
const MANIFEST_VERSION: u32 = 2;
const PENDING_DB: &str = ".pending";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Creating,
    AwaitingConfirmation,
    Promoted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Manifest {
    version: u32,
    vault_id: VaultId,
    target: String,
    phase: Phase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cipher_salt: Option<CipherSalt>,
    location_approved: bool,
}

impl Manifest {
    fn target(&self) -> PathBuf {
        PathBuf::from(&self.target)
    }
}

fn manifest_path(session: &Session) -> Result<PathBuf, VaultError> {
    Ok(session
        .env
        .app_dir
        .as_deref()
        .ok_or(VaultError::AppDirUnavailable)?
        .join(MANIFEST))
}

fn app_dir(session: &Session) -> Result<PathBuf, VaultError> {
    session
        .env
        .app_dir
        .clone()
        .ok_or(VaultError::AppDirUnavailable)
}

fn read_manifest(session: &Session) -> Result<Option<Manifest>, Failure> {
    match fsutil::read_bounded(&manifest_path(session)?, 16 * 1024) {
        Ok(bytes) => match serde_json::from_slice::<Manifest>(&bytes) {
            Ok(manifest)
                if manifest.version == MANIFEST_VERSION
                    && Path::new(&manifest.target).is_absolute() =>
            {
                Ok(Some(manifest))
            }
            _ => Err(Failure::with("vault_creation_damaged", json!({}))),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(VaultError::Io.into()),
    }
}

fn write_manifest(session: &Session, manifest: &Manifest, new: bool) -> Result<(), Failure> {
    let bytes = serde_json::to_vec_pretty(manifest).expect("a manifest serializes");
    let path = manifest_path(session)?;
    let written = if new {
        fsutil::publish_new(&path, &bytes)
    } else {
        fsutil::publish_replacing(&path, &bytes)
    };
    written.map_err(|error| match error.kind() {
        io::ErrorKind::AlreadyExists => Failure::with("vault_creation_pending", json!({})),
        _ => VaultError::Io.into(),
    })
}

fn pending_db(target: &Path) -> PathBuf {
    fsutil::with_suffix(target, PENDING_DB)
}

/// The creation the manifest records, for the status shown in the panel: its target path, or
/// None. A creation that already became the active vault (a crash before the manifest was
/// removed) is not pending.
pub fn pending_creation(session: &Session) -> Option<String> {
    match read_manifest(session) {
        Ok(Some(manifest)) if !completed(session, &manifest) => Some(manifest.target),
        Ok(_) => None,
        Err(_) => Some(String::new()),
    }
}

fn completed(session: &Session, manifest: &Manifest) -> bool {
    matches!(vault::resolve(&session.env), Ok(Active::Configured(config))
        if config.vault_id == Some(manifest.vault_id) && config.active_vault == manifest.target)
}

fn random_failure() -> Failure {
    Failure::with("random_unavailable", json!({}))
}

pub(super) fn key_store_failure(error: key_store::StoreError) -> Failure {
    let code = match error {
        key_store::StoreError::Unsupported => "key_protector_unavailable",
        key_store::StoreError::ProtectorFailed => "key_protector_failed",
        key_store::StoreError::Exists => "file_exists",
        key_store::StoreError::Io => "io_error",
    };
    Failure::with(code, json!({}))
}

/// `vault_create_encrypted`: everything but the activation; answers with the spell, once.
pub fn begin(
    session: &mut Session,
    dialogs: &dyn Dialogs,
    message: &Value,
) -> Result<Value, Failure> {
    require_management(session)?;
    if let Some(manifest) = read_manifest(session)? {
        if !completed(session, &manifest) {
            return Err(Failure::with(
                "vault_creation_pending",
                json!({ "path": manifest.target }),
            ));
        }
        let _ = fs::remove_file(manifest_path(session)?);
    }
    // Slice D: never beside an unfinished conversion (two spells at once could be confused).
    super::convert::refuse_while_pending(session)?;
    let default_path = session.env.default_vault_path()?;
    let target = match requested_path(message, "path")? {
        Some(path) => path,
        None if message.get("useDefault").and_then(Value::as_bool) == Some(true) => {
            if let Some(dir) = default_path.parent() {
                fs::create_dir_all(dir).map_err(|_| VaultError::Io)?;
            }
            default_path
        }
        None => {
            let dir = default_path.parent().unwrap_or(Path::new(""));
            dialogs
                .choose_new_vault(dir, DEFAULT_VAULT_NAME)
                .ok_or(VaultError::Cancelled)?
        }
    };
    let target = normalize_new(&target)?;
    let reserved = [target.clone(), pending_db(&target)];
    if let Some(taken) = reserved.iter().find(|path| path.exists()) {
        return Err(Failure::with(
            "file_exists",
            json!({ "path": display(taken) }),
        ));
    }
    for suffix in ["-journal", "-wal", "-shm"] {
        for base in [&target, &pending_db(&target)] {
            if fsutil::with_suffix(base, suffix).exists() {
                return Err(VaultError::SidecarExists.into());
            }
        }
    }
    let assessment = location::probe(&target, session.env.exe_dir.as_deref());
    let location_approved = check_location(&assessment, &target, approved(message))?;
    let app = app_dir(session)?;

    let _lock = ConfigLock::acquire(&session.env)?;
    let made = NewVault::generate().map_err(|_| random_failure())?;
    let mut manifest = Manifest {
        version: MANIFEST_VERSION,
        vault_id: made.id,
        target: display(&target),
        phase: Phase::Creating,
        cipher_salt: Some(made.salt),
        location_approved,
    };
    write_manifest(session, &manifest, true)?;
    let result = (|| {
        cipher::create(&pending_db(&target), &made.key, &made.salt).map_err(create_failure)?;
        key_store::store(
            &app,
            &made.id,
            &made.salt,
            &made.key,
            key_store::Write::Pending,
        )
        .map_err(key_store_failure)?;
        verify_pending(session, &manifest, &made.spell)?;
        manifest.phase = Phase::AwaitingConfirmation;
        write_manifest(session, &manifest, false)
    })();
    if let Err(failure) = result {
        remove_created(session, &manifest);
        return Err(failure);
    }
    // The only time the spell leaves the host. The response goes to the panel over native
    // messaging and nowhere else: no log, no file, no other message carries it.
    let shown: Zeroizing<String> = spell::encode(&made.spell);
    drop(made);
    Ok(json!({
        "path": display(&target),
        "recoverySpell": shown.as_str(),
    }))
}

pub(super) fn create_failure(error: cipher::CreateError) -> Failure {
    match error {
        cipher::CreateError::Exists => VaultError::FileExists.into(),
        cipher::CreateError::ParentMissing => VaultError::ParentMissing.into(),
        cipher::CreateError::Unavailable => VaultError::EncryptionUnsupported.into(),
        cipher::CreateError::Io => VaultError::Io.into(),
        cipher::CreateError::Db => VaultError::Db.into(),
    }
}

/// Every pending file opens to the key `secret` derives with the vault's salt: the DPAPI entry and
/// the vault itself.
fn verify_pending(
    session: &Session,
    manifest: &Manifest,
    secret: &RecoverySpell,
) -> Result<(), Failure> {
    let damaged = || Failure::with("vault_creation_damaged", json!({}));
    let salt = manifest.cipher_salt.ok_or_else(damaged)?;
    if recovery::vault_id(&salt) != manifest.vault_id {
        return Err(damaged());
    }
    let target = manifest.target();
    let app = app_dir(session)?;
    // Each file under its pending name, or already under its final name if a commit was
    // interrupted between promotions (`promote_file` accepts both the same way).
    let pick = |pending: PathBuf, promoted: PathBuf| {
        if pending.exists() {
            pending
        } else {
            promoted
        }
    };
    let stored = key_store::unlock_pending(&app, &manifest.vault_id, &salt)
        .or_else(|_| key_store::unlock(&app, &manifest.vault_id, &salt))
        .map_err(|_| damaged())?;
    if !recovery::vault_key(secret, &salt).same_as(&stored) {
        return Err(Failure::with("recovery_spell_rejected", json!({})));
    }
    let db_path = pick(pending_db(&target), target);
    if cipher::read_head(&db_path).ok() != Some(cipher::Head::Salt(salt)) {
        return Err(damaged());
    }
    vault::open_checked_in(
        &db_path,
        VaultMode::Encrypted,
        Some(manifest.vault_id),
        &GivenKey(stored),
    )
    .map_err(|_| damaged())?;
    Ok(())
}

/// `vault_create_encrypted_commit`: the spell re-entered in full in the panel, then promotion and
/// the switch. `recoverySpell` must be the spell begin returned, all 43 characters.
pub fn commit(session: &mut Session, message: &Value) -> Result<Value, Failure> {
    require_management(session)?;
    let typed = Zeroizing::new(
        message
            .get("recoverySpell")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    );
    let secret = spell::parse(&typed).map_err(spell_failure)?;
    drop(typed);
    let lock = ConfigLock::acquire(&session.env)?;
    let mut manifest = match read_manifest(session)? {
        Some(manifest) if manifest.phase != Phase::Creating => manifest,
        _ => return Err(Failure::with("vault_creation_not_pending", json!({}))),
    };
    if completed(session, &manifest) {
        let _ = fs::remove_file(manifest_path(session)?);
        session.reset();
        return Ok(json!({ "path": manifest.target, "status": status(session) }));
    }
    verify_pending(session, &manifest, &secret)?;
    drop(secret);
    let salt = manifest.cipher_salt.expect("verified");
    let target = manifest.target();
    let app = app_dir(session)?;
    if manifest.phase == Phase::AwaitingConfirmation {
        let promote = || -> io::Result<()> {
            key_store::promote_pending(&app, &manifest.vault_id, &salt)?;
            promote_file(&pending_db(&target), &target, |path| {
                cipher::read_head(path).ok() == Some(cipher::Head::Salt(salt))
            })
        };
        promote().map_err(|error| match error.kind() {
            io::ErrorKind::AlreadyExists => {
                Failure::with("file_exists", json!({ "path": display(&target) }))
            }
            _ => VaultError::Io.into(),
        })?;
        manifest.phase = Phase::Promoted;
        write_manifest(session, &manifest, false)?;
    }
    let keys = KeyStore::of(&session.env);
    vault::switch_active_to(
        &session.env,
        &lock,
        &NewActive::encrypted(&target, manifest.vault_id),
        manifest.location_approved,
        &keys,
        true,
    )
    .map_err(|error| {
        // The vault is complete but not active; a retried commit switches again.
        Failure::with(error.code(), json!({ "path": display(&target) }))
    })?;
    let _ = fs::remove_file(manifest_path(session)?);
    drop(lock);
    session.reset();
    Ok(json!({ "path": display(&target), "status": status(session) }))
}

/// Renames `from` to `to` without replacing; if `from` is gone and `to` is already this
/// creation's file (`ours`), that step was done before a crash.
pub(super) fn promote_file(from: &Path, to: &Path, ours: impl Fn(&Path) -> bool) -> io::Result<()> {
    match fsutil::rename_no_replace(from, to) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound && ours(to) => Ok(()),
        Err(error) => Err(error),
    }
}

/// `vault_create_encrypted_cancel`: removes what the recorded creation made.
pub fn cancel(session: &mut Session) -> Result<Value, Failure> {
    require_management(session)?;
    let _lock = ConfigLock::acquire(&session.env)?;
    match read_manifest(session) {
        Ok(Some(manifest)) => {
            if !completed(session, &manifest) {
                remove_created(session, &manifest);
            }
        }
        Ok(None) => {}
        // A manifest this version cannot read names nothing that could safely be removed: only the
        // manifest goes, so the panel is no longer stuck. Its `.pending` files stay; they can never
        // become active (their names and the missing manifest refuse every commit).
        Err(_) => {}
    }
    let _ = fs::remove_file(manifest_path(session)?);
    session.reset();
    Ok(json!({ "status": status(session) }))
}

/// Removes the files of an abandoned creation that provably belong to it, and the manifest. Never
/// the active vault.
fn remove_created(session: &Session, manifest: &Manifest) {
    let target = manifest.target();
    let active = |path: &Path| {
        matches!(vault::resolve(&session.env), Ok(Active::Configured(config))
            if Path::new(&config.active_vault) == path)
    };
    let salt = manifest.cipher_salt;
    let belongs = |path: &Path| match (cipher::read_head(path), salt) {
        (Ok(cipher::Head::Salt(found)), Some(salt)) => found == salt,
        // Created with `create_new` under a name this creation reserved, before its salt was known.
        (Ok(_), None) => path == pending_db(&target),
        _ => false,
    };
    for db in [pending_db(&target), target.clone()] {
        if !active(&db) && db.exists() && belongs(&db) {
            let _ = fs::remove_file(&db);
            let _ = fs::remove_file(fsutil::with_suffix(&db, "-journal"));
        }
    }
    if let Some(app) = session.env.app_dir.as_deref() {
        key_store::remove_created(app, &manifest.vault_id, salt.as_ref());
    }
    if let Ok(path) = manifest_path(session) {
        let _ = fs::remove_file(path);
    }
}

/// A key already in hand (just unwrapped or just read), for one open.
pub(super) struct GivenKey(pub(super) VaultKey);

impl KeySource for GivenKey {
    fn key_for(&self, _: &KeyRequest) -> Result<VaultKey, KeyError> {
        Ok(self.0.duplicate())
    }
}

/// `recovery_spell_invalid`, with what is wrong (never the input itself).
pub(super) fn spell_failure(error: spell::SpellError) -> Failure {
    let details = match error {
        spell::SpellError::Length(count) => json!({ "reason": "length", "count": count }),
        spell::SpellError::Character(position) => {
            json!({ "reason": "character", "position": position })
        }
        spell::SpellError::NonCanonical => json!({ "reason": "non_canonical" }),
    };
    Failure::with("recovery_spell_invalid", details)
}

/// The spell of a request (`recoverySpell`), parsed strictly. Missing counts as length 0.
pub(super) fn requested_spell(message: &Value) -> Result<RecoverySpell, Failure> {
    let typed = Zeroizing::new(
        message
            .get("recoverySpell")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    );
    spell::parse(&typed).map_err(spell_failure)
}

/// True when the encrypted file at `path`, starting with `salt`, is a vault of an earlier scheme
/// (Slices B–D): a `.mckey` sidecar or a version 1 key store entry names its salt. Such a vault's
/// key is random, never derived from a spell, so this build cannot open or recover it.
pub(super) fn is_legacy(path: &Path, salt: &CipherSalt, app: &Path) -> bool {
    sidecar::legacy_for(path, salt) || key_store::legacy_entry_for(app, salt)
}

pub(super) fn legacy_failure(path: &Path) -> Failure {
    Failure::with(
        VaultError::LegacyUnsupported.code(),
        json!({ "path": display(path), "recoveryAvailable": false }),
    )
}

/// `vault_open` of a file that is not plain SQLite: an encrypted vault, unlocked with this user's
/// key store under the vault id its salt names. If no usable key exists, the answer offers
/// recovery with the spell (`recoveryAvailable`), which needs nothing but the file.
pub fn open_existing(
    session: &mut Session,
    target: &Path,
    location_approved: bool,
) -> Result<Option<Value>, Failure> {
    let salt = match cipher::read_head(target) {
        Ok(cipher::Head::Salt(salt)) if cipher::plausible_size(target) => salt,
        Ok(_) => return Ok(None),
        Err(_) => return Err(VaultError::Io.into()),
    };
    let app = app_dir(session)?;
    if is_legacy(target, &salt, &app) {
        return Err(legacy_failure(target));
    }
    let vault_id = recovery::vault_id(&salt);
    let keys = KeyStore::of(&session.env);
    vault::open_checked_in(target, VaultMode::Encrypted, Some(vault_id), &keys).map_err(
        |failure| {
            Failure::with(
                failure.kind.code(),
                json!({
                    "path": display(target),
                    "recoveryAvailable": failure.needs_handle_fence(),
                }),
            )
        },
    )?;
    let lock = ConfigLock::acquire(&session.env)?;
    vault::switch_active_to(
        &session.env,
        &lock,
        &NewActive::encrypted(target, vault_id),
        location_approved,
        &keys,
        false,
    )?;
    drop(lock);
    session.reset();
    Ok(Some(
        json!({ "path": display(target), "status": status(session) }),
    ))
}

/// `vault_recover`: unlocks an encrypted vault with its Recovery Spell, stores its key for this
/// Windows user (DPAPI) and makes it the active vault, publication pending.
pub fn recover(
    session: &mut Session,
    dialogs: &dyn Dialogs,
    message: &Value,
) -> Result<Value, Failure> {
    require_management(session)?;
    let typed = Zeroizing::new(
        message
            .get("recoverySpell")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    );
    let target = match requested_path(message, "path")? {
        Some(path) => path,
        None => {
            let start = session.env.app_dir.clone();
            dialogs
                .choose_existing_vault(start.as_deref())
                .ok_or(VaultError::Cancelled)?
        }
    };
    let target = normalize_existing(&target)?;
    super::refuse_work_file(session, &target)?;
    let assessment = location::probe(&target, session.env.exe_dir.as_deref());
    let location_approved = check_location(&assessment, &target, approved(message))?;
    let with_path = |code: &'static str| Failure::with(code, json!({ "path": display(&target) }));
    let salt = match cipher::read_head(&target) {
        Ok(cipher::Head::Salt(salt)) if cipher::plausible_size(&target) => salt,
        Ok(cipher::Head::PlainSqlite) => return Err(with_path("vault_not_encrypted")),
        Ok(_) => return Err(with_path("vault_invalid")),
        Err(_) => return Err(VaultError::Io.into()),
    };
    let app = app_dir(session)?;
    if is_legacy(&target, &salt, &app) {
        return Err(legacy_failure(&target));
    }
    let secret = spell::parse(&typed).map_err(spell_failure)?;
    drop(typed);
    // The file's salt and the spell: nothing else.
    let vault_id = recovery::vault_id(&salt);
    let key = recovery::vault_key(&secret, &salt);
    drop(secret);
    // Proven on the vault itself before anything is stored. SQLCipher refuses another key at the
    // first read (`vault_decrypt_failed`): a wrong spell, or a damaged or altered file (its salt
    // included). Either way nothing is written.
    vault::open_checked_in(
        &target,
        VaultMode::Encrypted,
        Some(vault_id),
        &GivenKey(key.duplicate()),
    )
    .map_err(|failure| match failure.kind {
        VaultError::DecryptFailed => with_path("recovery_spell_rejected"),
        kind => with_path(kind.code()),
    })?;
    let write = match key_store::unlock(&app, &vault_id, &salt) {
        Ok(existing) if existing.same_as(&key) => None,
        Err(KeyError::Missing) => Some(key_store::Write::New),
        // An entry this user cannot use (another profile's, damaged, another key): replaced in one
        // rename, now that the spell proved the key.
        _ => Some(key_store::Write::Replace),
    };
    if let Some(write) = write {
        key_store::store(&app, &vault_id, &salt, &key, write).map_err(key_store_failure)?;
    }
    drop(key);
    let lock = ConfigLock::acquire(&session.env)?;
    vault::switch_active_to(
        &session.env,
        &lock,
        &NewActive::encrypted(&target, vault_id),
        location_approved,
        &KeyStore::of(&session.env),
        true,
    )?;
    drop(lock);
    session.reset();
    Ok(json!({ "path": display(&target), "status": status(session) }))
}

/// For the status: whether the panel can offer recovery with the spell for the encrypted vault at
/// `path`: any encrypted-looking file of this scheme (the file and the spell are all it needs), not
/// one of an earlier scheme.
pub fn recovery_available(path: &Path, app: Option<&Path>) -> bool {
    match cipher::read_head(path) {
        Ok(cipher::Head::Salt(salt)) if cipher::plausible_size(path) => {
            !sidecar::legacy_for(path, &salt)
                && !app.is_some_and(|app| key_store::legacy_entry_for(app, &salt))
        }
        _ => false,
    }
}
