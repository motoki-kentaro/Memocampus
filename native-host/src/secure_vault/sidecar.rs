//! The recovery sidecar `<vault>.mckey` of earlier slices, **no longer used** (Issue #51, Slice E).
//!
//! Slices B–D wrote the vault key wrapped under the Recovery Spell (version 2) or the 12-word phrase
//! (version 1) to `<vault>.mckey`, so a vault file without its sidecar could not be recovered. Since
//! Slice E the key is derived from the spell and the salt in the vault file itself
//! (`recovery::vault_key`): nothing writes, needs or reads a sidecar for a key.
//!
//! What remains is recognition only: a sidecar of any version naming a vault file's salt marks that
//! vault as one of the earlier, **unsupported** schemes (its key is random, not derived), so the
//! user is told so instead of "wrong spell". A sidecar is never deleted, replaced or migrated, and
//! one lying next to a Slice E vault (a copy, a name reused) changes nothing: the salt differs.

use super::fsutil;
use super::secret::{CipherSalt, VaultId};
use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const SUFFIX: &str = ".mckey";
pub const PENDING_SUFFIX: &str = ".mckey.pending";
const FORMAT: &str = "memocampus-vault-recovery";
const MAX_BYTES: u64 = 16 * 1024;

pub fn path_for(vault: &Path) -> PathBuf {
    fsutil::with_suffix(vault, SUFFIX)
}

pub fn pending_path_for(vault: &Path) -> PathBuf {
    fsutil::with_suffix(vault, PENDING_SUFFIX)
}

/// The vault id and salt a Memocampus sidecar of any version names, without reading its envelope.
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

/// True when a sidecar of an earlier slice lies next to `vault` and names its salt: the vault was
/// made by an earlier, unsupported scheme.
pub fn legacy_for(vault: &Path, salt: &CipherSalt) -> bool {
    [path_for(vault), pending_path_for(vault)]
        .iter()
        .any(|path| identity(path).is_some_and(|(_, named)| named == *salt))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_earlier_sidecar_is_recognised_only_for_its_own_salt() {
        let dir = std::env::temp_dir().join(format!("mc-sidecar-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let vault = dir.join("v.sqlite3");
        let path = path_for(&vault);
        assert!(path.to_string_lossy().ends_with("v.sqlite3.mckey"));
        let (id, salt) = (
            VaultId::generate().unwrap(),
            CipherSalt::from_array([1; 16]),
        );
        assert!(!legacy_for(&vault, &salt), "no sidecar");
        // Versions 1 and 2, as Slices B–D wrote them.
        for (version, scheme) in [
            (1, "bip39-128/hkdf-sha256/xchacha20poly1305/v1"),
            (2, "recovery-spell-256/hkdf-sha256/xchacha20poly1305/v2"),
        ] {
            std::fs::write(
                &path,
                format!(
                    concat!(
                        "{{\"version\": {}, \"format\": \"memocampus-vault-recovery\", ",
                        "\"vaultId\": \"{}\", \"cipherSalt\": \"{}\", \"recovery\": {{",
                        "\"scheme\": \"{}\", \"salt\": \"{}\", \"nonce\": \"{}\", ",
                        "\"wrapped\": \"{}\", \"check\": \"{}\"}}}}"
                    ),
                    version,
                    id.to_hex(),
                    salt.to_hex(),
                    scheme,
                    "11".repeat(32),
                    "22".repeat(24),
                    "33".repeat(48),
                    "44".repeat(32)
                ),
            )
            .unwrap();
            assert_eq!(identity(&path), Some((id, salt)));
            assert!(legacy_for(&vault, &salt), "{version}");
            assert!(!legacy_for(&vault, &CipherSalt::from_array([2; 16])));
        }
        std::fs::write(&path, b"{\"format\":\"other\"}").unwrap();
        assert_eq!(identity(&path), None);
        assert!(!legacy_for(&vault, &salt));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
