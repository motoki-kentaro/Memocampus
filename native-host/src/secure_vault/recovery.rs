//! The recovery envelope: the vault key wrapped under a key derived from the recovery seed.
//!
//! ```text
//! (KEK, K_check) = HKDF-SHA256(ikm = seed (128 bits), salt = 32 random bytes,
//!                              info = "memocampus/recovery/v1/kek" | ".../check")
//! check   = HMAC-SHA256(K_check, "memocampus/recovery/v1/phrase-check\0" || vaultId || cipherSalt)
//! wrapped = XChaCha20-Poly1305(KEK, nonce = 24 random bytes,
//!                              AAD = "memocampus/recovery/v1/wrap\0" || vaultId || cipherSalt)(VK)
//! ```
//!
//! HKDF, not a slow KDF: the seed is 128 uniformly random bits, so stretching adds nothing (the
//! recovery level is 128 bits, PO decision P5). The check value separates a wrong phrase
//! (`PhraseRejected`) from damaged or altered metadata (`Damaged`); it gives no shortcut, since each
//! guess still costs one HKDF, exactly as testing the AEAD tag would. Both the check and the AAD
//! bind the vault id and the vault file's salt, so an envelope moved to another vault is refused.

use super::fsutil::{hex, unhex};
use super::secret::{random_bytes, CipherSalt, RandomError, RecoverySeed, VaultId, VaultKey};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use zeroize::Zeroizing;

pub const SCHEME: &str = "bip39-128/hkdf-sha256/xchacha20poly1305/v1";
const KEK_INFO: &[u8] = b"memocampus/recovery/v1/kek";
const CHECK_INFO: &[u8] = b"memocampus/recovery/v1/check";
const CHECK_LABEL: &[u8] = b"memocampus/recovery/v1/phrase-check\0";
const WRAP_LABEL: &[u8] = b"memocampus/recovery/v1/wrap\0";
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
    /// Another scheme or version.
    Unsupported,
    /// A field of the wrong length or encoding.
    Malformed,
    /// The phrase is not the one of this envelope.
    PhraseRejected,
    /// The phrase matches, but the wrapped key does not authenticate: the envelope (or the vault id
    /// or salt it is bound to) was altered or damaged.
    Damaged,
}

fn bound(label: &[u8], vault_id: &VaultId, salt: &CipherSalt) -> Vec<u8> {
    [label, vault_id.as_bytes(), salt.as_bytes()].concat()
}

/// (KEK, K_check) for `seed` and the envelope salt.
fn derive(seed: &RecoverySeed, salt: &[u8; 32]) -> (Zeroizing<[u8; 32]>, Zeroizing<[u8; 32]>) {
    let hkdf = Hkdf::<Sha256>::new(Some(salt), seed.expose());
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
    seed: &RecoverySeed,
    vault_id: &VaultId,
    cipher_salt: &CipherSalt,
) -> Result<Envelope, RandomError> {
    let (mut salt, mut nonce) = ([0u8; 32], [0u8; 24]);
    random_bytes(&mut salt)?;
    random_bytes(&mut nonce)?;
    let (kek, k_check) = derive(seed, &salt);
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
    seed: &RecoverySeed,
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
    let (kek, k_check) = derive(seed, &salt);
    check_mac(&k_check, vault_id, cipher_salt)
        .verify_slice(&check)
        .map_err(|_| UnwrapError::PhraseRejected)?;
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
    use super::super::phrase;
    use super::*;

    fn fixture() -> (VaultKey, RecoverySeed, VaultId, CipherSalt) {
        (
            VaultKey::generate().unwrap(),
            RecoverySeed::generate().unwrap(),
            VaultId::generate().unwrap(),
            CipherSalt::from_array([9; 16]),
        )
    }

    #[test]
    fn the_right_phrase_and_only_it_unwraps_the_key() {
        let (key, seed, id, salt) = fixture();
        let envelope = wrap(&key, &seed, &id, &salt).unwrap();
        assert!(unwrap(&envelope, &seed, &id, &salt).unwrap().same_as(&key));
        // Through the words, as a user types them.
        let typed = phrase::parse(&phrase::encode(&seed).to_ascii_uppercase()).unwrap();
        assert!(unwrap(&envelope, &typed, &id, &salt).unwrap().same_as(&key));
        let other = RecoverySeed::generate().unwrap();
        assert_eq!(
            unwrap(&envelope, &other, &id, &salt).unwrap_err(),
            UnwrapError::PhraseRejected
        );
        let text = serde_json::to_string(&envelope).unwrap();
        assert!(!key.appears_in(text.as_bytes()));
        assert!(!text.contains(phrase::encode(&seed).as_str()));
    }

    #[test]
    fn every_single_word_change_that_passes_the_checksum_is_still_refused() {
        let (key, seed, id, salt) = fixture();
        let envelope = wrap(&key, &seed, &id, &salt).unwrap();
        let phrase = phrase::encode(&seed);
        let words: Vec<&str> = phrase.split(' ').collect();
        let list = bip39::Language::English.word_list();
        let mut passed_checksum = 0;
        for position in 0..12 {
            for replacement in list.iter().step_by(5) {
                if *replacement == words[position] {
                    continue;
                }
                let mut changed = words.clone();
                changed[position] = replacement;
                if let Ok(candidate) = phrase::parse(&changed.join(" ")) {
                    passed_checksum += 1;
                    assert_eq!(
                        unwrap(&envelope, &candidate, &id, &salt).unwrap_err(),
                        UnwrapError::PhraseRejected
                    );
                }
            }
        }
        assert!(passed_checksum > 50, "{passed_checksum}");
    }

    #[test]
    fn damage_is_told_apart_from_a_wrong_phrase_and_another_vault_is_refused() {
        let (key, seed, id, salt) = fixture();
        let envelope = wrap(&key, &seed, &id, &salt).unwrap();
        // Tag or ciphertext altered: the phrase is right, the metadata is not.
        for position in [0, 40, 95] {
            let mut altered = envelope.clone();
            let mut bytes = unhex(&altered.wrapped).unwrap();
            bytes[position / 2] ^= 1;
            altered.wrapped = hex(&bytes);
            assert_eq!(
                unwrap(&altered, &seed, &id, &salt).unwrap_err(),
                UnwrapError::Damaged,
                "{position}"
            );
        }
        let mut altered = envelope.clone();
        altered.nonce = hex(&[0u8; 24]);
        assert_eq!(
            unwrap(&altered, &seed, &id, &salt).unwrap_err(),
            UnwrapError::Damaged
        );
        // The check value or the envelope salt altered: indistinguishable from a wrong phrase.
        let mut altered = envelope.clone();
        altered.check = hex(&[0u8; 32]);
        assert_eq!(
            unwrap(&altered, &seed, &id, &salt).unwrap_err(),
            UnwrapError::PhraseRejected
        );
        // Bound to the vault id and salt.
        assert_eq!(
            unwrap(&envelope, &seed, &VaultId::generate().unwrap(), &salt).unwrap_err(),
            UnwrapError::PhraseRejected
        );
        assert_eq!(
            unwrap(&envelope, &seed, &id, &CipherSalt::from_array([8; 16])).unwrap_err(),
            UnwrapError::PhraseRejected
        );
        // Format.
        let mut altered = envelope.clone();
        altered.scheme = "bip39-128/hkdf-sha256/xchacha20poly1305/v2".into();
        assert_eq!(
            unwrap(&altered, &seed, &id, &salt).unwrap_err(),
            UnwrapError::Unsupported
        );
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
                unwrap(&altered, &seed, &id, &salt).unwrap_err(),
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
