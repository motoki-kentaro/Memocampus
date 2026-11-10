//! The vault key of an encrypted vault, derived from its Recovery Spell (Issue #51, Slice E;
//! `docs/secure-vault/slice-e.md`).
//!
//! ```text
//! spell   = 32 bytes from the OS CSPRNG (shown once as 43 base64url characters, `spell`)
//! salt    = 16 bytes from the OS CSPRNG: the vault file's first 16 bytes (SQLCipher's salt)
//! VK      = HKDF-SHA256(ikm = spell, salt = salt, info = "memocampus/vault-key/v3")   (32 bytes)
//! vaultId = SHA-256("memocampus/vault-id/v3\0" || salt)[..16]
//! ```
//!
//! VK is SQLCipher's **raw key** (`x'<64 hex>'`, `cipher::apply_key`): SQLCipher derives nothing
//! from it with PBKDF2. A new vault is keyed with the raw key **and** its salt
//! (`x'<64 hex><32 hex>'`, `cipher::create`), so SQLCipher writes exactly that salt as the file's
//! first 16 bytes; any later open reads the salt back from the file. The vault file and the spell
//! are therefore all a recovery needs: no sidecar, no key store, no DPAPI, no other file.
//!
//! Everything but the spell is public (the source, this derivation, the salt, the vault id). HKDF
//! rather than a slow KDF: the spell is 256 uniformly random bits, so stretching adds nothing, and
//! each guess costs at least one HKDF plus SQLCipher's page HMAC check. The salt makes VK unique
//! to the vault (two vaults of one spell never share a key) and the label separates it from every
//! other key in Memocampus (`kcv`, backup MAC and policy keys are `HKDF(VK, …)` with their own
//! labels; the password backups' keys come from Argon2id). The vault id is a public name only,
//! never key material.
//!
//! A new spell or a new salt means a new key: re-issuing the spell, converting a plain vault or
//! restoring a backup writes a **new** vault (a new salt, so a new vault id) through
//! `convert::export`; nothing ever re-keys a vault in place.
//!
//! Earlier schemes (the spell envelope in `<vault>.mckey`, version 2, and the 12-word phrase,
//! version 1) wrapped a random VK. Their vaults cannot be opened by this derivation (the key is
//! not a function of the spell and salt) and are refused as unsupported (`sidecar::legacy_*`).

use super::secret::{random_bytes, CipherSalt, RandomError, RecoverySpell, VaultId, VaultKey};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

/// The name of this scheme, in the key store entry and the backup header.
pub const SCHEME: &str = "spell-hkdf-sha256/sqlcipher4-raw/v3";
const VK_INFO: &[u8] = b"memocampus/vault-key/v3";
const ID_LABEL: &[u8] = b"memocampus/vault-id/v3\0";
const KCV_INFO: &[u8] = b"memocampus/kcv/v1";

/// The vault key of the vault whose file starts with `salt`, for `spell`.
pub fn vault_key(spell: &RecoverySpell, salt: &CipherSalt) -> VaultKey {
    let mut out = Zeroizing::new([0u8; 32]);
    Hkdf::<Sha256>::new(Some(salt.as_bytes()), spell.expose())
        .expand(VK_INFO, out.as_mut())
        .expect("32 bytes is a valid HKDF-SHA256 length");
    VaultKey::from_bytes(&mut out)
}

/// The vault id of the vault whose file starts with `salt`. Public.
pub fn vault_id(salt: &CipherSalt) -> VaultId {
    let digest = Sha256::new()
        .chain_update(ID_LABEL)
        .chain_update(salt.as_bytes())
        .finalize();
    VaultId::from_array(digest[..16].try_into().expect("16 of 32 bytes"))
}

/// What a new encrypted vault is made of: a fresh spell and salt, and what follows from them.
pub struct NewVault {
    pub spell: RecoverySpell,
    pub salt: CipherSalt,
    pub key: VaultKey,
    pub id: VaultId,
}

impl NewVault {
    pub fn generate() -> Result<Self, RandomError> {
        let spell = RecoverySpell::generate()?;
        let salt = CipherSalt::generate()?;
        Ok(Self::of(spell, salt))
    }

    /// The vault of `spell` and `salt` (a restore: the backup's spell, a new salt).
    pub fn of(spell: RecoverySpell, salt: CipherSalt) -> Self {
        let key = vault_key(&spell, &salt);
        Self {
            id: vault_id(&salt),
            spell,
            salt,
            key,
        }
    }
}

/// Fresh random bytes for a salt that must differ from `avoid` (a re-issue or restore never keeps
/// the salt it replaces; a collision of 128 random bits is not expected, but costs nothing to rule
/// out).
pub fn fresh_salt(avoid: Option<&CipherSalt>) -> Result<CipherSalt, RandomError> {
    loop {
        let mut bytes = [0u8; 16];
        random_bytes(&mut bytes)?;
        let salt = CipherSalt::from_array(bytes);
        if avoid != Some(&salt) {
            return Ok(salt);
        }
    }
}

/// Key check value: proves that a key (from DPAPI, or derived from a typed spell) is the key of
/// this vault id and salt, without revealing it.
/// `HMAC-SHA256(HKDF(VK, "memocampus/kcv/v1"), vaultId || cipherSalt)`.
pub fn key_check_value(key: &VaultKey, vault_id: &VaultId, cipher_salt: &CipherSalt) -> [u8; 32] {
    let mut derived = Zeroizing::new([0u8; 32]);
    Hkdf::<Sha256>::new(None, key.expose())
        .expand(KCV_INFO, derived.as_mut())
        .expect("32 bytes is a valid HKDF-SHA256 length");
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(derived.as_ref()).expect("any key length");
    mac.update(vault_id.as_bytes());
    mac.update(cipher_salt.as_bytes());
    mac.finalize().into_bytes().into()
}

/// Constant-time comparison of a stored key check value.
pub fn key_check_matches(
    key: &VaultKey,
    vault_id: &VaultId,
    cipher_salt: &CipherSalt,
    stored: &[u8],
) -> bool {
    use subtle::ConstantTimeEq;
    stored.len() == 32
        && bool::from(
            key_check_value(key, vault_id, cipher_salt)
                .as_slice()
                .ct_eq(stored),
        )
}

#[cfg(test)]
mod tests {
    use super::super::fsutil::hex;
    use super::super::spell;
    use super::*;

    fn spell_of(byte: u8) -> RecoverySpell {
        let mut bytes = [byte; 32];
        RecoverySpell::from_bytes(&mut bytes)
    }

    #[test]
    fn the_key_is_a_function_of_the_spell_and_the_salt_only() {
        let (secret, salt) = (spell_of(0x11), CipherSalt::from_array([0x22; 16]));
        let key = vault_key(&secret, &salt);
        assert!(key.same_as(&vault_key(&secret, &salt)), "deterministic");
        // Through the 43 characters, as a user pastes them.
        let typed = spell::parse(&spell::encode(&secret)).unwrap();
        assert!(key.same_as(&vault_key(&typed, &salt)));
        assert!(!key.same_as(&vault_key(&spell_of(0x12), &salt)));
        assert!(!key.same_as(&vault_key(&secret, &CipherSalt::from_array([0x23; 16]))));
        assert_ne!(key.expose(), secret.expose(), "the spell is never the key");
        // A fixed vector (HKDF-SHA256 as in tests/secure_vault_e_poc.rs): the derivation can never
        // change silently, or every vault would stop opening.
        assert_eq!(
            hex(key.expose()),
            hex(&{
                let prk = <Hmac<Sha256> as Mac>::new_from_slice(&[0x22; 16])
                    .unwrap()
                    .chain_update([0x11; 32])
                    .finalize()
                    .into_bytes();
                let t1: [u8; 32] = <Hmac<Sha256> as Mac>::new_from_slice(&prk)
                    .unwrap()
                    .chain_update(VK_INFO)
                    .chain_update([1])
                    .finalize()
                    .into_bytes()
                    .into();
                t1
            })
        );
    }

    #[test]
    fn every_single_character_change_of_the_spell_changes_the_key() {
        let (secret, salt) = (
            RecoverySpell::generate().unwrap(),
            CipherSalt::generate().unwrap(),
        );
        let key = vault_key(&secret, &salt);
        let text = spell::encode(&secret).to_string();
        let alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut parsed = 0;
        for position in 0..spell::LEN {
            for replacement in alphabet.chars().step_by(3) {
                let mut changed: Vec<char> = text.chars().collect();
                if changed[position] == replacement {
                    continue;
                }
                changed[position] = replacement;
                let changed: String = changed.into_iter().collect();
                if let Ok(candidate) = spell::parse(&changed) {
                    parsed += 1;
                    assert!(!vault_key(&candidate, &salt).same_as(&key));
                }
            }
        }
        assert!(parsed > 800, "{parsed}");
    }

    #[test]
    fn the_vault_id_is_public_and_follows_the_salt() {
        let salt = CipherSalt::from_array([0x5a; 16]);
        assert_eq!(vault_id(&salt), vault_id(&salt));
        assert_ne!(
            vault_id(&salt),
            vault_id(&CipherSalt::from_array([0x5b; 16]))
        );
        assert_ne!(vault_id(&salt).as_bytes(), salt.as_bytes());
        let made = NewVault::generate().unwrap();
        assert_eq!(made.id, vault_id(&made.salt));
        assert!(made.key.same_as(&vault_key(&made.spell, &made.salt)));
        let other = NewVault::generate().unwrap();
        assert_ne!(other.salt, made.salt);
        assert!(!other.key.same_as(&made.key));
        assert_ne!(fresh_salt(Some(&made.salt)).unwrap(), made.salt);
    }

    #[test]
    fn the_key_check_value_belongs_to_one_key_vault_and_salt() {
        let made = NewVault::generate().unwrap();
        let (key, id, salt) = (&made.key, &made.id, &made.salt);
        let kcv = key_check_value(key, id, salt);
        assert!(key_check_matches(key, id, salt, &kcv));
        assert!(!key_check_matches(
            &VaultKey::generate().unwrap(),
            id,
            salt,
            &kcv
        ));
        assert!(!key_check_matches(
            key,
            &VaultId::generate().unwrap(),
            salt,
            &kcv
        ));
        assert!(!key_check_matches(
            key,
            id,
            &CipherSalt::from_array([1; 16]),
            &kcv
        ));
        assert!(!key_check_matches(key, id, salt, &kcv[..31]));
        assert!(!key.appears_in(&kcv));
        assert!(!kcv.windows(32).any(|w| w == made.spell.expose()));
    }
}
