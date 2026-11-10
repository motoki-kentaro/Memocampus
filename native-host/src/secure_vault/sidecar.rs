//! The recovery sidecar `<vault>.mckey` (PO decision P1): what an encrypted vault needs, besides
//! its file and the Recovery Spell, to be opened on another PC or by another Windows user.
//!
//! ```json
//! {"version":2,"format":"memocampus-vault-recovery","vaultId":"<32 hex>",
//!  "cipherSalt":"<32 hex>","recovery":{<recovery::Envelope>, "scheme":"recovery-spell-256/..."}}
//! ```
//!
//! Not secret: the key inside is wrapped under the spell (256-bit secret). It travels with the
//! vault file; one moved without the other is detected by the salt, and the envelope is bound to
//! the vault id and salt.
//!
//! Version 1 was the 12-word BIP-39 envelope. It is recognised (format name and version only) and
//! reported as `Unsupported`, never as damaged and never read as a spell envelope.

use super::fsutil;
use super::recovery::{self, Envelope};
use super::secret::{CipherSalt, VaultId};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

pub const SUFFIX: &str = ".mckey";
pub const PENDING_SUFFIX: &str = ".mckey.pending";
const FORMAT: &str = "memocampus-vault-recovery";
const VERSION: u32 = 2;
const MAX_BYTES: u64 = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Sidecar {
    pub version: u32,
    pub format: String,
    pub vault_id: VaultId,
    pub cipher_salt: CipherSalt,
    pub recovery: Envelope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidecarError {
    Missing,
    /// Present but not a sidecar at all (damaged, truncated, other format).
    Invalid,
    /// A Memocampus sidecar of another version: the 12-word recovery phrase (version 1), or a
    /// newer one. Never read as a spell envelope.
    Unsupported,
    Io,
}

impl Sidecar {
    pub fn new(vault_id: VaultId, cipher_salt: CipherSalt, recovery: Envelope) -> Self {
        Self {
            version: VERSION,
            format: FORMAT.into(),
            vault_id,
            cipher_salt,
            recovery,
        }
    }
}

pub fn path_for(vault: &Path) -> PathBuf {
    fsutil::with_suffix(vault, SUFFIX)
}

pub fn pending_path_for(vault: &Path) -> PathBuf {
    fsutil::with_suffix(vault, PENDING_SUFFIX)
}

pub fn read(path: &Path) -> Result<Sidecar, SidecarError> {
    let bytes = fsutil::read_bounded(path, MAX_BYTES).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => SidecarError::Missing,
        io::ErrorKind::InvalidData => SidecarError::Invalid,
        _ => SidecarError::Io,
    })?;
    // The format name and version first, so another version is told apart from damage.
    #[derive(Deserialize)]
    struct Tag {
        version: u32,
        format: String,
    }
    let tag: Tag = serde_json::from_slice(&bytes).map_err(|_| SidecarError::Invalid)?;
    if tag.format != FORMAT {
        return Err(SidecarError::Invalid);
    }
    if tag.version != VERSION {
        return Err(SidecarError::Unsupported);
    }
    let sidecar: Sidecar = serde_json::from_slice(&bytes).map_err(|_| SidecarError::Invalid)?;
    if sidecar.recovery.scheme != recovery::SCHEME {
        return Err(SidecarError::Invalid);
    }
    Ok(sidecar)
}

/// The vault id and salt a Memocampus sidecar of **any** version names (the 12-word version 1
/// included), without reading its envelope. Only for deciding whether an abandoned creation's file
/// belongs to it; never for recovery.
pub fn identity(path: &Path) -> Option<(VaultId, CipherSalt)> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Named {
        format: String,
        vault_id: VaultId,
        cipher_salt: CipherSalt,
    }
    let bytes = fsutil::read_bounded(path, MAX_BYTES).ok()?;
    let named: Named = serde_json::from_slice(&bytes).ok()?;
    (named.format == FORMAT).then_some((named.vault_id, named.cipher_salt))
}

/// Writes a new sidecar at `path`, never replacing a file.
pub fn write_new(path: &Path, sidecar: &Sidecar) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(sidecar).expect("a sidecar serializes");
    fsutil::publish_new(path, &bytes)
}

#[cfg(test)]
mod tests {
    use super::super::secret::{RecoverySpell, VaultKey};
    use super::*;

    #[test]
    fn a_sidecar_round_trips_and_anything_else_is_invalid() {
        let dir = std::env::temp_dir().join(format!("mc-sidecar-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let vault = dir.join("v.sqlite3");
        let path = path_for(&vault);
        assert!(path.to_string_lossy().ends_with("v.sqlite3.mckey"));
        assert_eq!(read(&path).unwrap_err(), SidecarError::Missing);
        let (id, salt) = (
            VaultId::generate().unwrap(),
            CipherSalt::from_array([1; 16]),
        );
        let key = VaultKey::generate().unwrap();
        let envelope =
            recovery::wrap(&key, &RecoverySpell::generate().unwrap(), &id, &salt).unwrap();
        let sidecar = Sidecar::new(id, salt, envelope);
        write_new(&path, &sidecar).unwrap();
        assert_eq!(read(&path).unwrap(), sidecar);
        assert!(write_new(&path, &sidecar).is_err(), "never replaced");
        assert!(!key.appears_in(&std::fs::read(&path).unwrap()));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"version\": 2"));
        for bad in [
            text.replace("memocampus-vault-recovery", "other"),
            text[..text.len() / 2].to_string(),
            text.replacen('{', "{\"extra\":1,", 1),
            // A version 2 sidecar must hold a spell envelope.
            text.replace(recovery::SCHEME, recovery::LEGACY_SCHEME),
        ] {
            std::fs::write(&path, bad).unwrap();
            assert_eq!(read(&path).unwrap_err(), SidecarError::Invalid);
        }
        for other in [
            text.replace("\"version\": 2", "\"version\": 3"),
            text.replace("\"version\": 2", "\"version\": 0"),
        ] {
            std::fs::write(&path, other).unwrap();
            assert_eq!(read(&path).unwrap_err(), SidecarError::Unsupported);
        }
        // A sidecar of the 12-word scheme, as Slice B/C wrote it: unsupported, not damaged.
        std::fs::write(
            &path,
            format!(
                concat!(
                    "{{\"version\": 1, \"format\": \"memocampus-vault-recovery\", ",
                    "\"vaultId\": \"{}\", \"cipherSalt\": \"{}\", \"recovery\": {{",
                    "\"scheme\": \"{}\", \"salt\": \"{}\", \"nonce\": \"{}\", ",
                    "\"wrapped\": \"{}\", \"check\": \"{}\"}}}}"
                ),
                id.to_hex(),
                salt.to_hex(),
                recovery::LEGACY_SCHEME,
                "11".repeat(32),
                "22".repeat(24),
                "33".repeat(48),
                "44".repeat(32)
            ),
        )
        .unwrap();
        assert_eq!(read(&path).unwrap_err(), SidecarError::Unsupported);
        // Its identity is still known, for cleaning up an abandoned creation.
        assert_eq!(identity(&path), Some((id, salt)));
        std::fs::write(&path, b"{\"format\":\"other\"}").unwrap();
        assert_eq!(identity(&path), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
