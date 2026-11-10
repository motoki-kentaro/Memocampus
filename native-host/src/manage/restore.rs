//! Restore (Issue #51, Slice C): a backup of any format becomes a **new** vault. Nothing is ever
//! restored in place, and the active vault is unchanged until the user activates the restored one.
//!
//! # Messages
//!
//! - `vault_restore_inspect {path?}`: picks the backup file (dialog unless `path`), detects its format
//!   and checks everything that needs no secret (bounds, exact length). Answers which secrets the
//!   restore needs. Writes nothing.
//! - `vault_restore {backupPath, path?, password?, recoverySpell?, approveWarnings?}`: picks the
//!   target (a file that does not exist; dialog unless `path`), restores, verifies, and leaves a
//!   complete vault there that is **not active**. Answers its counts and the backup's policy copy,
//!   for the user to review.
//! - `vault_restore_activate {expectedVaultToken}`: makes the restored vault active. Publication
//!   starts `pending`, whatever the policy is.
//! - `vault_restore_discard {}`: removes what the recorded restore created (never the active vault).
//!
//! # Transaction
//!
//! The manifest `<app dir>\vault-restore.json` records one restore at a time:
//!
//! | Phase | On disk | Retry / clean-up |
//! |---|---|---|
//! | `restoring` | `<target>.partial` (being written; C: plaintext, as the restored plain vault itself) | discard, then restore again |
//! | `verified` | `<target>.partial` verified; encrypted: `keys\<newId>.json.pending`, `<target>.mckey.pending` | activate (finishes the promotion) or discard |
//! | `restored` | `<target>` (+ `<target>.mckey`, `keys\<newId>.json`) | activate or discard |
//! | none | — (activated: the config names it, publication pending) | — |
//!
//! - Every file is created new (`create_new`) and promoted with `rename_no_replace`: an existing file
//!   is never replaced.
//! - Discarding removes only files proven to be this restore's: the `.partial` it reserved, a target
//!   whose file identity (`vault_token`, recorded before the rename) matches, and sidecar and key
//!   store entries naming its new vault id. Never the active vault.
//! - While a manifest exists no other restore starts (`vault_restore_pending`).
//!
//! # Encrypted vaults
//!
//! The vault key comes from the backup's recovery envelope and the Recovery Spell only; no key
//! store or DPAPI data of the original PC is used or copied. The restored vault gets a **new vault
//! id** (so a restore next to the original, or a second restore of the same backup, never collides
//! with an existing key store entry), its key is protected with this Windows user's DPAPI, and a new
//! sidecar (version 2) wraps the same key under the same spell for the new id and the file's salt:
//! the spell that restored the vault is the one that recovers it. A Slice C backup B or D (12-word
//! phrase, container version 1) is refused as `backup_legacy_recovery_phrase` before any secret is
//! asked for.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read};
use std::path::{Path, PathBuf};

use super::backup::{self as bk, checkpoint, crashed, output_failure, Guarded, Step};
use super::encrypted::{key_store_failure, promote_file, requested_spell};
use super::{
    approved, check_location, display, normalize_existing, normalize_new, requested_path,
    require_management, status, Failure,
};
use crate::dialog::Dialogs;
use crate::location;
use crate::vault::{self, Active, ConfigLock, KeyStore, NewActive, Session, VaultError};
use memocampus_native_host::active_vault::VaultMode;
use memocampus_native_host::schema::{self, SchemaError};
use memocampus_native_host::secure_vault::container::{self, FormatError, Kind, PasswordReader};
use memocampus_native_host::secure_vault::secret::{CipherSalt, RecoverySpell, VaultId, VaultKey};
use memocampus_native_host::secure_vault::{cipher, fsutil, key_store, recovery, sidecar};
use rusqlite::{Connection, OpenFlags};
use zeroize::Zeroizing;

pub const MANIFEST: &str = "vault-restore.json";
const MANIFEST_VERSION: u32 = 1;
const PARTIAL: &str = ".partial";
const DEFAULT_TARGET_NAME: &str = "memocampus-restored.sqlite3";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Restoring,
    Verified,
    Restored,
}

impl Phase {
    fn code(self) -> &'static str {
        match self {
            Self::Restoring => "restoring",
            Self::Verified => "verified",
            Self::Restored => "restored",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Manifest {
    version: u32,
    target: String,
    format: String,
    mode: VaultMode,
    phase: Phase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    vault_id: Option<VaultId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cipher_salt: Option<CipherSalt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token: Option<String>,
    location_approved: bool,
}

impl Manifest {
    fn target(&self) -> PathBuf {
        PathBuf::from(&self.target)
    }
}

fn app_dir(session: &Session) -> Result<PathBuf, VaultError> {
    session
        .env
        .app_dir
        .clone()
        .ok_or(VaultError::AppDirUnavailable)
}

fn manifest_path(session: &Session) -> Result<PathBuf, VaultError> {
    Ok(app_dir(session)?.join(MANIFEST))
}

fn read_manifest(session: &Session) -> Result<Option<Manifest>, Failure> {
    match fsutil::read_bounded(&manifest_path(session)?, 16 * 1024) {
        Ok(bytes) => match serde_json::from_slice::<Manifest>(&bytes) {
            Ok(manifest)
                if manifest.version == MANIFEST_VERSION
                    && Path::new(&manifest.target).is_absolute()
                    && ids_consistent(&manifest) =>
            {
                Ok(Some(manifest))
            }
            _ => Err(Failure::with("vault_restore_damaged", json!({}))),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(VaultError::Io.into()),
    }
}

/// A plain restore never has a vault id; an encrypted one has it from verification on.
fn ids_consistent(manifest: &Manifest) -> bool {
    match manifest.mode {
        VaultMode::Plain => manifest.vault_id.is_none() && manifest.cipher_salt.is_none(),
        VaultMode::Encrypted => {
            manifest.phase == Phase::Restoring
                || (manifest.vault_id.is_some() && manifest.cipher_salt.is_some())
        }
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
        io::ErrorKind::AlreadyExists => Failure::with("vault_restore_pending", json!({})),
        _ => VaultError::Io.into(),
    })
}

fn partial_of(target: &Path) -> PathBuf {
    fsutil::with_suffix(target, PARTIAL)
}

/// For the status: the restore the manifest records (not yet activated), or null. An unreadable
/// manifest is reported so it can be discarded.
pub fn pending_restore(session: &Session) -> Value {
    match read_manifest(session) {
        Ok(Some(manifest)) if !completed(session, &manifest) => json!({
            "path": manifest.target,
            "phase": manifest.phase.code(),
            "format": manifest.format,
            "mode": manifest.mode.code(),
            "vaultToken": manifest.token,
        }),
        Ok(_) => Value::Null,
        Err(_) => json!({ "path": null, "phase": "damaged" }),
    }
}

fn completed(session: &Session, manifest: &Manifest) -> bool {
    matches!(vault::resolve(&session.env), Ok(Active::Configured(config))
        if config.active_vault == manifest.target && config.vault_id == manifest.vault_id)
}

/// The failure a container error is reported as. Fixed codes: nothing about the secrets.
fn format_failure(error: FormatError) -> Failure {
    let code = match error {
        FormatError::UnsupportedFormat => "backup_unsupported_format",
        FormatError::UnsupportedVersion => "backup_unsupported_version",
        FormatError::LegacyRecoveryPhrase => "backup_legacy_recovery_phrase",
        FormatError::Malformed => "backup_malformed",
        FormatError::Incomplete => "backup_incomplete",
        FormatError::SchemaMismatch => "backup_schema_mismatch",
        FormatError::SpellRejected => "recovery_spell_rejected",
        FormatError::RecoveryDamaged => "backup_damaged",
        FormatError::PasswordRejected => "backup_password_rejected",
        FormatError::Tampered => "backup_tampered",
        FormatError::Io => "io_error",
        FormatError::Output(kind) => {
            return output_failure(&io::Error::from(kind), "restore_failed");
        }
    };
    Failure::with(code, json!({}))
}

/// What a backup file is, checked without any secret.
struct Examined {
    /// None: format A (plain SQLite).
    kind: Option<Kind>,
    /// Whether it carries a policy copy (None: not visible without the password).
    policy: Option<bool>,
    len: u64,
}

impl Examined {
    fn format(&self) -> &'static str {
        self.kind.map_or("A", Kind::code)
    }

    fn mode(&self) -> VaultMode {
        match self.kind {
            Some(Kind::B | Kind::D) => VaultMode::Encrypted,
            _ => VaultMode::Plain,
        }
    }
}

fn examine(path: &Path) -> Result<Examined, Failure> {
    let metadata = fs::metadata(path).map_err(|_| VaultError::Missing)?;
    if !metadata.is_file() {
        return Err(format_failure(FormatError::UnsupportedFormat));
    }
    let len = metadata.len();
    let mut file = BufReader::new(File::open(path).map_err(|_| VaultError::Io)?);
    let mut head = [0u8; 16];
    let mut read = 0;
    while read < head.len() {
        match file.read(&mut head[read..]) {
            Ok(0) => return Err(format_failure(FormatError::UnsupportedFormat)),
            Ok(n) => read += n,
            Err(_) => return Err(VaultError::Io.into()),
        }
    }
    let mut file = BufReader::new(File::open(path).map_err(|_| VaultError::Io)?);
    match container::detect(&head) {
        container::Detected::PlainSqlite => {
            check_plain_backup(path)?;
            Ok(Examined {
                kind: None,
                policy: Some(false),
                len,
            })
        }
        container::Detected::Container(Kind::B) => {
            let (_, header) = container::read_b_header(&mut file, len).map_err(format_failure)?;
            Ok(Examined {
                kind: Some(Kind::B),
                policy: Some(header.policy.is_some()),
                len,
            })
        }
        container::Detected::Container(kind) => {
            let header =
                container::inspect_password(&mut file, len, kind).map_err(format_failure)?;
            let policy = match kind {
                Kind::C => Some(
                    header
                        .sections
                        .iter()
                        .any(|s| s.kind == container::SECTION_POLICY),
                ),
                _ => None,
            };
            Ok(Examined {
                kind: Some(kind),
                policy,
                len,
            })
        }
        container::Detected::UnsupportedVersion => {
            Err(format_failure(FormatError::UnsupportedVersion))
        }
        container::Detected::LegacyRecoveryPhrase => {
            Err(format_failure(FormatError::LegacyRecoveryPhrase))
        }
        container::Detected::Unknown => Err(format_failure(FormatError::UnsupportedFormat)),
    }
}

/// A format A file must be a Memocampus vault of this schema (opened read-only).
fn check_plain_backup(path: &Path) -> Result<(), Failure> {
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| Failure::with("backup_vault_invalid", json!({})))?;
    schema::check(&db).map_err(schema_failure)
}

fn schema_failure(error: SchemaError) -> Failure {
    match error {
        SchemaError::UnsupportedVersion(_) => Failure::with("backup_schema_mismatch", json!({})),
        _ => Failure::with("backup_vault_invalid", json!({})),
    }
}

fn backup_path(dialogs: &dyn Dialogs, message: &Value, key: &str) -> Result<PathBuf, Failure> {
    let path = match requested_path(message, key)? {
        Some(path) => path,
        None => dialogs.choose_backup_file().ok_or(VaultError::Cancelled)?,
    };
    normalize_existing(&path)
}

/// `vault_restore_inspect`.
pub fn inspect(
    session: &mut Session,
    dialogs: &dyn Dialogs,
    message: &Value,
) -> Result<Value, Failure> {
    require_management(session)?;
    let path = backup_path(dialogs, message, "path")?;
    let examined = examine(&path).map_err(|mut failure| {
        failure
            .extra
            .insert("backupPath".into(), display(&path).into());
        failure
    })?;
    let kind = examined.kind;
    Ok(json!({
        "backupPath": display(&path),
        "format": examined.format(),
        "mode": examined.mode().code(),
        "needsPassword": kind.is_some_and(Kind::needs_password),
        "needsSpell": kind.is_some_and(Kind::needs_spell),
        "policyCopy": examined.policy,
        "size": examined.len,
    }))
}

/// The secrets a restore of `kind` needs, from the request.
struct Secrets {
    password: Option<Zeroizing<String>>,
    spell: Option<RecoverySpell>,
}

fn secrets(kind: Option<Kind>, message: &Value) -> Result<Secrets, Failure> {
    let password = match kind {
        Some(kind) if kind.needs_password() => {
            let text = message
                .get("password")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .ok_or_else(|| Failure::with("backup_password_required", json!({})))?;
            if text.len() > container::PASSWORD_MAX_BYTES {
                return Err(format_failure(FormatError::PasswordRejected));
            }
            Some(Zeroizing::new(text.to_owned()))
        }
        _ => None,
    };
    let spell = match kind {
        Some(kind) if kind.needs_spell() => Some(requested_spell(message)?),
        _ => None,
    };
    Ok(Secrets { password, spell })
}

/// `vault_restore`.
pub fn restore(
    session: &mut Session,
    dialogs: &dyn Dialogs,
    message: &Value,
) -> Result<Value, Failure> {
    require_management(session)?;
    if let Some(manifest) = read_manifest(session)? {
        if !completed(session, &manifest) {
            return Err(Failure::with(
                "vault_restore_pending",
                json!({ "path": manifest.target }),
            ));
        }
        let _ = fs::remove_file(manifest_path(session)?);
    }
    let backup = match requested_path(message, "backupPath")? {
        Some(path) => normalize_existing(&path)?,
        None => return Err(Failure::with("backup_path_required", json!({}))),
    };
    let examined = examine(&backup)?;
    let secrets = secrets(examined.kind, message)?;
    let target = match requested_path(message, "path")? {
        Some(path) => path,
        None => dialogs
            .choose_restore_target(DEFAULT_TARGET_NAME)
            .ok_or(VaultError::Cancelled)?,
    };
    let target = normalize_new(&target)?;
    let mut reserved = vec![
        target.clone(),
        partial_of(&target),
        sidecar::path_for(&target),
        sidecar::pending_path_for(&target),
    ];
    for suffix in ["-journal", "-wal", "-shm"] {
        reserved.push(fsutil::with_suffix(&target, suffix));
        reserved.push(fsutil::with_suffix(&partial_of(&target), suffix));
    }
    if let Some(taken) = reserved.iter().find(|path| path.exists()) {
        return Err(Failure::with(
            "file_exists",
            json!({ "path": display(taken) }),
        ));
    }
    let assessment = location::probe(&target, session.env.exe_dir.as_deref());
    let location_approved = check_location(&assessment, &target, approved(message))?;
    let mut manifest = Manifest {
        version: MANIFEST_VERSION,
        target: display(&target),
        format: examined.format().into(),
        mode: examined.mode(),
        phase: Phase::Restoring,
        vault_id: None,
        cipher_salt: None,
        token: None,
        location_approved,
    };
    // A new PC may have no app directory yet (no config, no policy).
    fs::create_dir_all(app_dir(session)?).map_err(|_| VaultError::Io)?;
    write_manifest(session, &manifest, true)?;
    let outcome = run(session, &mut manifest, &backup, &examined, &secrets);
    drop(secrets);
    match outcome {
        Ok(summary) => {
            session.reset();
            Ok(json!({
                "path": display(&target),
                "backupPath": display(&backup),
                "format": manifest.format,
                "mode": manifest.mode.code(),
                "vaultToken": manifest.token,
                "counts": summary.counts,
                "policyCopy": summary.policy,
                "status": status(session),
            }))
        }
        Err(mut failure) => {
            if !crashed() {
                remove_restored(session, &manifest);
            }
            failure.extra.insert("path".into(), display(&target).into());
            Err(failure)
        }
    }
}

struct Summary {
    counts: Value,
    policy: Value,
}

/// What the stream produced: the key (encrypted backups) and the policy copy.
struct Stream {
    key: Option<VaultKey>,
    salt: Option<CipherSalt>,
    policy: Option<Zeroizing<Vec<u8>>>,
}

fn run(
    session: &Session,
    manifest: &mut Manifest,
    backup: &Path,
    examined: &Examined,
    secrets: &Secrets,
) -> Result<Summary, Failure> {
    let target = manifest.target();
    let partial = partial_of(&target);
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&partial)
        .map_err(|error| output_failure(&error, "restore_failed"))?;
    let streamed = match examined.kind {
        None => {
            drop(file);
            restore_plain_file(backup, &partial)?;
            Stream {
                key: None,
                salt: None,
                policy: None,
            }
        }
        Some(kind) => stream_container(backup, examined.len, kind, secrets, file)?,
    };
    checkpoint(Step::RestoreWrite).map_err(|e| output_failure(&e, "restore_failed"))?;
    let counts = verify_restored(
        &partial,
        manifest.mode,
        streamed.key.as_ref(),
        streamed.salt,
    )?;
    checkpoint(Step::RestoreVerify).map_err(|_| Failure::with("restore_failed", json!({})))?;
    if manifest.mode == VaultMode::Encrypted {
        let (Some(key), Some(salt), Some(secret)) = (&streamed.key, streamed.salt, &secrets.spell)
        else {
            return Err(Failure::with("restore_failed", json!({})));
        };
        let vault_id =
            VaultId::generate().map_err(|_| Failure::with("random_unavailable", json!({})))?;
        manifest.vault_id = Some(vault_id);
        manifest.cipher_salt = Some(salt);
        write_manifest(session, manifest, false)?;
        checkpoint(Step::RestoreKeys).map_err(|_| Failure::with("restore_failed", json!({})))?;
        let app = app_dir(session)?;
        key_store::store(&app, &vault_id, &salt, key, key_store::Write::Pending)
            .map_err(key_store_failure)?;
        let envelope = recovery::wrap(key, secret, &vault_id, &salt)
            .map_err(|_| Failure::with("random_unavailable", json!({})))?;
        sidecar::write_new(
            &sidecar::pending_path_for(&target),
            &sidecar::Sidecar::new(vault_id, salt, envelope),
        )
        .map_err(|error| output_failure(&error, "restore_failed"))?;
    }
    manifest.token = Some(vault::vault_token(&partial)?);
    manifest.phase = Phase::Verified;
    write_manifest(session, manifest, false)?;
    checkpoint(Step::RestorePromote).map_err(|_| Failure::with("restore_failed", json!({})))?;
    promote(session, manifest)?;
    let policy = match examined.kind {
        None => json!({ "state": "not_supported" }),
        Some(_) => bk::describe_policy_copy(streamed.policy.as_deref().map(Vec::as_slice)),
    };
    Ok(Summary { counts, policy })
}

/// Format A: the Backup API from the backup file (read-only) into the new `.partial`.
fn restore_plain_file(backup: &Path, partial: &Path) -> Result<(), Failure> {
    let source = Connection::open_with_flags(
        backup,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| Failure::with("backup_vault_invalid", json!({})))?;
    schema::check(&source).map_err(schema_failure)?;
    let mut target = Connection::open_with_flags(
        partial,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| Failure::with("restore_failed", json!({})))?;
    let expected = vault::snapshot_into(&source, &mut target)
        .map_err(|_| Failure::with("restore_failed", json!({})))?;
    vault::verify_copy(&target, &expected)
        .map_err(|_| Failure::with("backup_damaged", json!({})))?;
    target
        .close()
        .map_err(|_| Failure::with("restore_failed", json!({})))?;
    Ok(())
}

/// Formats B, C, D: decrypt / authenticate while streaming into the `.partial`.
fn stream_container(
    backup: &Path,
    len: u64,
    kind: Kind,
    secrets: &Secrets,
    file: File,
) -> Result<Stream, Failure> {
    let input = BufReader::new(File::open(backup).map_err(|_| VaultError::Io)?);
    let mut out = BufWriter::with_capacity(
        1 << 20,
        Guarded {
            inner: file,
            step: Step::RestoreWrite,
        },
    );
    let password = secrets
        .password
        .as_ref()
        .map(|text| text.as_bytes())
        .unwrap_or_default();
    let with_spell = |header: &container::HeaderB| match &secrets.spell {
        Some(secret) => container::unlock_with_spell(header, secret),
        None => Err(FormatError::SpellRejected),
    };
    let streamed = match kind {
        Kind::B => {
            let mut input = input;
            let restored =
                container::read_b(&mut input, len, with_spell, &mut out).map_err(format_failure)?;
            Stream {
                salt: Some(restored.header.cipher_salt),
                key: Some(restored.key),
                policy: restored.policy,
            }
        }
        Kind::C => {
            let (header, mut reader) =
                PasswordReader::open(input, len, password, Kind::C).map_err(format_failure)?;
            let mut policy = None;
            for section in &header.sections {
                match section.kind.as_str() {
                    container::SECTION_META => {
                        let meta = container::read_small(&mut reader, section.len)
                            .map_err(format_failure)?;
                        container::Meta::parse(&meta).map_err(format_failure)?;
                    }
                    container::SECTION_POLICY => {
                        policy = Some(
                            container::read_small(&mut reader, section.len)
                                .map_err(format_failure)?,
                        );
                    }
                    _ => container::copy_exact(&mut reader, section.len, &mut out)
                        .map_err(format_failure)?,
                }
            }
            if !reader.is_complete() {
                return Err(format_failure(FormatError::Tampered));
            }
            Stream {
                key: None,
                salt: None,
                policy,
            }
        }
        Kind::D => {
            let (header, mut reader) =
                PasswordReader::open(input, len, password, Kind::D).map_err(format_failure)?;
            let restored =
                container::read_b(&mut reader, header.sections[0].len, with_spell, &mut out)
                    .map_err(format_failure)?;
            if !reader.is_complete() {
                return Err(format_failure(FormatError::Tampered));
            }
            Stream {
                salt: Some(restored.header.cipher_salt),
                key: Some(restored.key),
                policy: restored.policy,
            }
        }
    };
    let guarded = out
        .into_inner()
        .map_err(|error| output_failure(error.error(), "restore_failed"))?;
    guarded
        .inner
        .sync_all()
        .map_err(|error| output_failure(&error, "restore_failed"))?;
    Ok(streamed)
}

/// The restored `.partial` opens in its mode (with the key for an encrypted one), passes a full
/// `integrity_check` and the exact schema check. Returns its counts.
fn verify_restored(
    partial: &Path,
    mode: VaultMode,
    key: Option<&VaultKey>,
    salt: Option<CipherSalt>,
) -> Result<Value, Failure> {
    let invalid = || Failure::with("backup_damaged", json!({}));
    let head = cipher::read_head(partial).map_err(|_| VaultError::Io)?;
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let db = match (mode, key, salt) {
        (VaultMode::Plain, None, None) if head == cipher::Head::PlainSqlite => {
            Connection::open_with_flags(partial, flags).map_err(|_| invalid())?
        }
        (VaultMode::Encrypted, Some(key), Some(salt)) if head == cipher::Head::Salt(salt) => {
            cipher::open(partial, key, flags).map_err(|_| invalid())?
        }
        _ => return Err(invalid()),
    };
    let integrity: String = db
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(|_| invalid())?;
    if integrity != "ok" {
        return Err(invalid());
    }
    schema::check(&db).map_err(schema_failure)?;
    let (counts, images, _) = vault::content_counts(&db).map_err(|_| invalid())?;
    db.close().map_err(|_| invalid())?;
    Ok(json!({
        "visits": counts.visits,
        "snapshots": counts.snapshots,
        "images": images.1,
    }))
}

/// Moves the verified files to their final names (no replacement), idempotently.
fn promote(session: &Session, manifest: &mut Manifest) -> Result<(), Failure> {
    let target = manifest.target();
    let token = manifest.token.clone().ok_or(VaultError::Io)?;
    let promoted = (|| -> io::Result<()> {
        if let (Some(vault_id), Some(salt)) = (manifest.vault_id, manifest.cipher_salt) {
            let app = app_dir(session).map_err(|_| io::Error::other("app dir"))?;
            key_store::promote_pending(&app, &vault_id, &salt)?;
            promote_file(
                &sidecar::pending_path_for(&target),
                &sidecar::path_for(&target),
                |path| {
                    sidecar::read(path)
                        .is_ok_and(|found| found.vault_id == vault_id && found.cipher_salt == salt)
                },
            )?;
        }
        promote_file(&partial_of(&target), &target, |path| {
            vault::vault_token(path).ok().as_deref() == Some(token.as_str())
        })
    })();
    promoted.map_err(|error| match error.kind() {
        io::ErrorKind::AlreadyExists => {
            Failure::with("file_exists", json!({ "path": display(&target) }))
        }
        _ => VaultError::Io.into(),
    })?;
    manifest.phase = Phase::Restored;
    write_manifest(session, manifest, false)
}

/// Removes what the recorded restore provably created, and the manifest. Never the active vault.
fn remove_restored(session: &Session, manifest: &Manifest) {
    let target = manifest.target();
    let active = |path: &Path| {
        matches!(vault::resolve(&session.env), Ok(Active::Configured(config))
            if Path::new(&config.active_vault) == path)
    };
    let ours = |path: &Path| match &manifest.token {
        Some(token) => vault::vault_token(path).ok().as_deref() == Some(token.as_str()),
        // Before verification only the `.partial` this restore created (`create_new`, under the
        // name it reserved while no such file existed) can be its own.
        None => path == partial_of(&target),
    };
    for db in [partial_of(&target), target.clone()] {
        if db.exists() && !active(&db) && ours(&db) {
            let _ = fs::remove_file(&db);
        }
        if !active(&db) {
            let _ = fs::remove_file(fsutil::with_suffix(&db, "-journal"));
        }
    }
    if let Some(vault_id) = manifest.vault_id {
        for path in [
            sidecar::pending_path_for(&target),
            sidecar::path_for(&target),
        ] {
            if sidecar::read(&path).is_ok_and(|found| found.vault_id == vault_id) {
                let _ = fs::remove_file(&path);
            }
        }
        if let Some(app) = session.env.app_dir.as_deref() {
            key_store::remove_created(app, &vault_id, manifest.cipher_salt.as_ref());
        }
    }
    if let Ok(path) = manifest_path(session) {
        let _ = fs::remove_file(path);
    }
}

/// `vault_restore_discard`.
pub fn discard(session: &mut Session) -> Result<Value, Failure> {
    require_management(session)?;
    let _lock = ConfigLock::acquire(&session.env)?;
    match read_manifest(session) {
        Ok(Some(manifest)) if !completed(session, &manifest) => remove_restored(session, &manifest),
        // Activated already, nothing recorded, or a manifest this version cannot read (it names
        // nothing that could safely be removed): only the manifest goes.
        _ => {
            let _ = fs::remove_file(manifest_path(session)?);
        }
    }
    session.reset();
    Ok(json!({ "status": status(session) }))
}

/// `vault_restore_activate`: the user confirmed the restored vault; it becomes the active vault,
/// publication pending.
pub fn activate(session: &mut Session, message: &Value) -> Result<Value, Failure> {
    require_management(session)?;
    let expected = message
        .get("expectedVaultToken")
        .and_then(Value::as_str)
        .filter(|token| crate::image_original::is_hex64(token))
        .ok_or_else(|| Failure::with("invalid_vault_token", json!({})))?
        .to_owned();
    let lock = ConfigLock::acquire(&session.env)?;
    let mut manifest = match read_manifest(session)? {
        Some(manifest) => manifest,
        None => return Err(Failure::with("vault_restore_not_pending", json!({}))),
    };
    let target = manifest.target();
    if completed(session, &manifest) {
        let _ = fs::remove_file(manifest_path(session)?);
        session.reset();
        return Ok(json!({ "path": display(&target), "status": status(session) }));
    }
    if manifest.phase == Phase::Restoring {
        return Err(Failure::with(
            "vault_restore_incomplete",
            json!({ "path": display(&target) }),
        ));
    }
    if manifest.token.as_deref() != Some(expected.as_str()) {
        return Err(VaultError::Switched.into());
    }
    if manifest.phase == Phase::Verified {
        promote(session, &mut manifest)?;
    }
    if vault::vault_token(&target).ok() != manifest.token {
        return Err(Failure::with(
            "vault_restore_damaged",
            json!({ "path": display(&target) }),
        ));
    }
    let new = match (manifest.mode, manifest.vault_id) {
        (VaultMode::Plain, None) => NewActive::plain(&target),
        (VaultMode::Encrypted, Some(vault_id)) => NewActive::encrypted(&target, vault_id),
        _ => return Err(Failure::with("vault_restore_damaged", json!({}))),
    };
    let keys = KeyStore::of(&session.env);
    vault::open_checked_in(&target, new.mode, new.vault_id, &keys).map_err(|failure| {
        Failure::with(failure.kind.code(), json!({ "path": display(&target) }))
    })?;
    vault::switch_active_to(
        &session.env,
        &lock,
        &new,
        manifest.location_approved,
        &keys,
        true,
    )
    .map_err(|error| Failure::with(error.code(), json!({ "path": display(&target) })))?;
    let _ = fs::remove_file(manifest_path(session)?);
    drop(lock);
    session.reset();
    Ok(json!({ "path": display(&target), "status": status(session) }))
}
