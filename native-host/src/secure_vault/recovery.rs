//! The recovery envelope: the vault key wrapped under keys derived from the Recovery Spell.
//!
//! ```text
//! spell   = 32 bytes from the OS CSPRNG (shown once as 43 base64url characters, `spell`)
//! (KEK, K_check) = HKDF-SHA256(ikm = spell, salt = 32 random bytes (per envelope),
//!                              info = "memocampus/recovery-spell/v2/kek" | ".../check")
//! check   = HMAC-SHA256(K_check, "memocampus/recovery-spell/v2/spell-check\0" || vaultId || cipherSalt)
//! wrapped = XChaCha20-Poly1305(KEK, nonce = 24 random bytes,
//!                              AAD = "memocampus/recovery-spell/v2/wrap\0" || vaultId || cipherSalt)(VK)
//! ```
//!
//! The spell's bytes are only the HKDF input: each use gets its own key through its own `info`
//! label, and none of the labels is shared with the 12-word scheme this replaced
//! (`bip39-128/.../v1`, refused as `Unsupported`) or with the keys derived from VK (key check
//! value, backup MAC and policy copy, all `HKDF(VK, ...)` with their own labels).
//!
//! HKDF, not a slow KDF: the spell is 256 uniformly random bits, so stretching adds nothing. The
//! check value separates a wrong spell (`SpellRejected`) from damaged or altered metadata
//! (`Damaged`); it gives no shortcut, since each guess still costs one HKDF, exactly as testing the
//! AEAD tag would. Both the check and the AAD bind the vault id and the vault file's salt, so an
//! envelope moved to another vault is refused.

use super::fsutil::{hex, unhex};
use super::secret::{random_bytes, CipherSalt, RandomError, RecoverySpell, VaultId, VaultKey};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use zeroize::Zeroizing;

pub const SCHEME: &str = "recovery-spell-256/hkdf-sha256/xchacha20poly1305/v2";
/// The 12-word BIP-39 envelope of Slice B/C. Recognised only to be refused as unsupported.
pub const LEGACY_SCHEME: &str = "bip39-128/hkdf-sha256/xchacha20poly1305/v1";
const KEK_INFO: &[u8] = b"memocampus/recovery-spell/v2/kek";
const CHECK_INFO: &[u8] = b"memocampus/recovery-spell/v2/check";
const CHECK_LABEL: &[u8] = b"memocampus/recovery-spell/v2/spell-check\0";
const WRAP_LABEL: &[u8] = b"memocampus/recovery-spell/v2/wrap\0";
const KCV_INFO: &[u8] = b"memocampus/kcv/v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Envelope {
    pub scheme: String,
    pub salt: String,
    pub nonce: String,
    pub wrapped: String,
    pub check: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnwrapError {
    /// Another scheme or version (including the 12-word scheme this replaced).
    Unsupported,
    /// A field of the wrong length or encoding.
    Malformed,
    /// The spell is not the one of this envelope.
    SpellRejected,
    /// The spell matches, but the wrapped key does not authenticate: the envelope (or the vault id
    /// or salt it is bound to) was altered or damaged.
    Damaged,
}

fn bound(label: &[u8], vault_id: &VaultId, salt: &CipherSalt) -> Vec<u8> {
    [label, vault_id.as_bytes(), salt.as_bytes()].concat()
}

/// (KEK, K_check) for `spell` and the envelope salt.
fn derive(spell: &RecoverySpell, salt: &[u8; 32]) -> (Zeroizing<[u8; 32]>, Zeroizing<[u8; 32]>) {
    let hkdf = Hkdf::<Sha256>::new(Some(salt), spell.expose());
    let (mut kek, mut check) = (Zeroizing::new([0u8; 32]), Zeroizing::new([0u8; 32]));
    hkdf.expand(KEK_INFO, kek.as_mut())
        .expect("32 bytes is a valid HKDF-SHA256 length");
    hkdf.expand(CHECK_INFO, check.as_mut())
        .expect("32 bytes is a valid HKDF-SHA256 length");
    (kek, check)
}

fn check_mac(k_check: &[u8; 32], vault_id: &VaultId, salt: &CipherSalt) -> Hmac<Sha256> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(k_check).expect("any key length");
    mac.update(&bound(CHECK_LABEL, vault_id, salt));
    mac
}

pub fn wrap(
    key: &VaultKey,
    spell: &RecoverySpell,
    vault_id: &VaultId,
    cipher_salt: &CipherSalt,
) -> Result<Envelope, RandomError> {
    let (mut salt, mut nonce) = ([0u8; 32], [0u8; 24]);
    random_bytes(&mut salt)?;
    random_bytes(&mut nonce)?;
    let (kek, k_check) = derive(spell, &salt);
    let wrapped = XChaCha20Poly1305::new(kek.as_ref().into())
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: key.expose(),
                aad: &bound(WRAP_LABEL, vault_id, cipher_salt),
            },
        )
        .expect("XChaCha20-Poly1305 encryption of 32 bytes cannot fail");
    let check = check_mac(&k_check, vault_id, cipher_salt)
        .finalize()
        .into_bytes();
    Ok(Envelope {
        scheme: SCHEME.into(),
        salt: hex(&salt),
        nonce: hex(&nonce),
        wrapped: hex(&wrapped),
        check: hex(&check),
    })
}

fn fixed<const N: usize>(text: &str) -> Result<[u8; N], UnwrapError> {
    unhex(text)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(UnwrapError::Malformed)
}

pub fn unwrap(
    envelope: &Envelope,
    spell: &RecoverySpell,
    vault_id: &VaultId,
    cipher_salt: &CipherSalt,
) -> Result<VaultKey, UnwrapError> {
    if envelope.scheme != SCHEME {
        return Err(UnwrapError::Unsupported);
    }
    let salt: [u8; 32] = fixed(&envelope.salt)?;
    let nonce: [u8; 24] = fixed(&envelope.nonce)?;
    let wrapped: [u8; 48] = fixed(&envelope.wrapped)?;
    let check: [u8; 32] = fixed(&envelope.check)?;
    let (kek, k_check) = derive(spell, &salt);
    check_mac(&k_check, vault_id, cipher_salt)
        .verify_slice(&check)
        .map_err(|_| UnwrapError::SpellRejected)?;
    let plain = Zeroizing::new(
        XChaCha20Poly1305::new(kek.as_ref().into())
            .decrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &wrapped,
                    aad: &bound(WRAP_LABEL, vault_id, cipher_salt),
                },
            )
            .map_err(|_| UnwrapError::Damaged)?,
    );
    let mut bytes: [u8; 32] = plain
        .as_slice()
        .try_into()
        .map_err(|_| UnwrapError::Damaged)?;
    Ok(VaultKey::from_bytes(&mut bytes))
}

/// Key check value: proves that a key (from DPAPI) is the key of this vault id and salt, without
/// revealing it. `HMAC-SHA256(HKDF(VK, "memocampus/kcv/v1"), vaultId || cipherSalt)`.
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
    use super::super::spell;
    use super::*;

    fn fixture() -> (VaultKey, RecoverySpell, VaultId, CipherSalt) {
        (
            VaultKey::generate().unwrap(),
            RecoverySpell::generate().unwrap(),
            VaultId::generate().unwrap(),
            CipherSalt::from_array([9; 16]),
        )
    }

    #[test]
    fn the_right_spell_and_only_it_unwraps_the_key() {
        let (key, secret, id, salt) = fixture();
        let envelope = wrap(&key, &secret, &id, &salt).unwrap();
        assert_eq!(envelope.scheme, SCHEME);
        assert!(unwrap(&envelope, &secret, &id, &salt)
            .unwrap()
            .same_as(&key));
        // Through the 43 characters, as a user pastes them.
        let typed = spell::parse(&spell::encode(&secret)).unwrap();
        assert!(unwrap(&envelope, &typed, &id, &salt).unwrap().same_as(&key));
        let other = RecoverySpell::generate().unwrap();
        assert_eq!(
            unwrap(&envelope, &other, &id, &salt).unwrap_err(),
            UnwrapError::SpellRejected
        );
        let text = serde_json::to_string(&envelope).unwrap();
        assert!(!key.appears_in(text.as_bytes()));
        assert!(!text.contains(spell::encode(&secret).as_str()));
        assert!(!text.contains(&hex(secret.expose())));
    }

    #[test]
    fn every_single_character_change_of_the_spell_is_refused() {
        let (key, secret, id, salt) = fixture();
        let envelope = wrap(&key, &secret, &id, &salt).unwrap();
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
                    assert_eq!(
                        unwrap(&envelope, &candidate, &id, &salt).unwrap_err(),
                        UnwrapError::SpellRejected
                    );
                }
            }
        }
        assert!(parsed > 800, "{parsed}");
    }

    #[test]
    fn the_spell_bytes_are_never_a_key_themselves() {
        // KEK and the check key come from HKDF with their own labels: even a spell equal to VK
        // (not something generation produces, but the strongest case) yields an envelope that
        // neither contains nor reveals it.
        let (key, _, id, salt) = fixture();
        let mut same = *key.expose();
        let spell_equal_to_key = RecoverySpell::from_bytes(&mut same);
        let envelope = wrap(&key, &spell_equal_to_key, &id, &salt).unwrap();
        let text = serde_json::to_string(&envelope).unwrap();
        assert!(!key.appears_in(text.as_bytes()));
        let salt_bytes: [u8; 32] = unhex(&envelope.salt).unwrap().try_into().unwrap();
        let (kek, k_check) = derive(&spell_equal_to_key, &salt_bytes);
        assert_ne!(kek.as_ref(), key.expose());
        assert_ne!(k_check.as_ref(), key.expose());
        assert_ne!(kek.as_ref(), k_check.as_ref());
        assert_ne!(&key_check_value(&key, &id, &salt), kek.as_ref());
        // Two envelopes of one spell use fresh salts and nonces: nothing repeats.
        let again = wrap(&key, &spell_equal_to_key, &id, &salt).unwrap();
        assert_ne!(again.salt, envelope.salt);
        assert_ne!(again.nonce, envelope.nonce);
        assert_ne!(again.wrapped, envelope.wrapped);
    }

    #[test]
    fn damage_is_told_apart_from_a_wrong_spell_and_another_vault_is_refused() {
        let (key, secret, id, salt) = fixture();
        let envelope = wrap(&key, &secret, &id, &salt).unwrap();
        // Tag or ciphertext altered: the spell is right, the metadata is not.
        for position in [0, 40, 95] {
            let mut altered = envelope.clone();
            let mut bytes = unhex(&altered.wrapped).unwrap();
            bytes[position / 2] ^= 1;
            altered.wrapped = hex(&bytes);
            assert_eq!(
                unwrap(&altered, &secret, &id, &salt).unwrap_err(),
                UnwrapError::Damaged,
                "{position}"
            );
        }
        let mut altered = envelope.clone();
        altered.nonce = hex(&[0u8; 24]);
        assert_eq!(
            unwrap(&altered, &secret, &id, &salt).unwrap_err(),
            UnwrapError::Damaged
        );
        // The check value or the envelope salt altered: indistinguishable from a wrong spell.
        let mut altered = envelope.clone();
        altered.check = hex(&[0u8; 32]);
        assert_eq!(
            unwrap(&altered, &secret, &id, &salt).unwrap_err(),
            UnwrapError::SpellRejected
        );
        let mut altered = envelope.clone();
        altered.salt = hex(&[7u8; 32]);
        assert_eq!(
            unwrap(&altered, &secret, &id, &salt).unwrap_err(),
            UnwrapError::SpellRejected
        );
        // Bound to the vault id and salt.
        assert_eq!(
            unwrap(&envelope, &secret, &VaultId::generate().unwrap(), &salt).unwrap_err(),
            UnwrapError::SpellRejected
        );
        assert_eq!(
            unwrap(&envelope, &secret, &id, &CipherSalt::from_array([8; 16])).unwrap_err(),
            UnwrapError::SpellRejected
        );
        // Format: the 12-word scheme and any other scheme are unsupported, not "wrong spell".
        for scheme in [
            LEGACY_SCHEME,
            "recovery-spell-256/hkdf-sha256/xchacha20poly1305/v3",
            "",
        ] {
            let mut altered = envelope.clone();
            altered.scheme = scheme.into();
            assert_eq!(
                unwrap(&altered, &secret, &id, &salt).unwrap_err(),
                UnwrapError::Unsupported,
                "{scheme}"
            );
        }
        for field in 0..4 {
            let mut altered = envelope.clone();
            let target = match field {
                0 => &mut altered.salt,
                1 => &mut altered.nonce,
                2 => &mut altered.wrapped,
                _ => &mut altered.check,
            };
            target.pop();
            assert_eq!(
                unwrap(&altered, &secret, &id, &salt).unwrap_err(),
                UnwrapError::Malformed
            );
        }
        assert!(serde_json::from_str::<Envelope>(
            &serde_json::to_string(&envelope)
                .unwrap()
                .replace('}', ",\"extra\":1}")
        )
        .is_err());
    }

    #[test]
    fn the_key_check_value_belongs_to_one_key_vault_and_salt() {
        let (key, _, id, salt) = fixture();
        let kcv = key_check_value(&key, &id, &salt);
        assert!(key_check_matches(&key, &id, &salt, &kcv));
        assert!(!key_check_matches(
            &VaultKey::generate().unwrap(),
            &id,
            &salt,
            &kcv
        ));
        assert!(!key_check_matches(
            &key,
            &VaultId::generate().unwrap(),
            &salt,
            &kcv
        ));
        assert!(!key_check_matches(
            &key,
            &id,
            &CipherSalt::from_array([1; 16]),
            &kcv
        ));
        assert!(!key_check_matches(&key, &id, &salt, &kcv[..31]));
        assert!(!key.appears_in(&kcv));
    }
}
