//! Key material and identifiers.
//!
//! `VaultKey` and `RecoverySpell` are secrets: they cannot be printed, cloned, compared with `==` or
//! serialized, and their bytes are zeroed when they are dropped. Their bytes are visible only inside
//! this module tree (`expose`), where they are handed to a cipher or the key protector.
//!
//! `VaultId` and `CipherSalt` are not secret: they name a vault and are stored in the clear.

use subtle::ConstantTimeEq;
use zeroize::Zeroize;

/// The OS CSPRNG could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RandomError;

/// Fills `buf` from the operating system's CSPRNG (on Windows `ProcessPrng`/`BCryptGenRandom`).
pub fn random_bytes(buf: &mut [u8]) -> Result<(), RandomError> {
    getrandom::fill(buf).map_err(|_| RandomError)
}

/// The 256-bit key of one encrypted vault (VK), the raw SQLCipher key. Since Slice E it is derived
/// from the Recovery Spell and the vault's public salt (`recovery::vault_key`); `generate` remains
/// for tests and for keys that are never a vault's.
pub struct VaultKey([u8; 32]);

impl VaultKey {
    /// A fresh key from the OS CSPRNG, independent of any other value.
    pub fn generate() -> Result<Self, RandomError> {
        let mut key = Self([0u8; 32]);
        random_bytes(&mut key.0)?;
        Ok(key)
    }

    /// A key from bytes the caller already holds (a test fixture, or an unwrapped envelope). The
    /// caller's copy is zeroed.
    pub fn from_bytes(bytes: &mut [u8; 32]) -> Self {
        let key = Self(*bytes);
        bytes.zeroize();
        key
    }

    pub(crate) fn expose(&self) -> &[u8; 32] {
        &self.0
    }

    /// A second copy, also zeroed on drop (to hand a key just unwrapped to a key source).
    pub fn duplicate(&self) -> Self {
        Self(self.0)
    }

    /// Constant-time equality.
    pub fn same_as(&self, other: &Self) -> bool {
        self.0.ct_eq(&other.0).into()
    }

    /// Canary for tests and audits: true if `haystack` contains this key in any form a leak
    /// would take (raw bytes, hex in either case, standard or URL-safe base64). Reveals nothing
    /// beyond that one bit.
    pub fn appears_in(&self, haystack: &[u8]) -> bool {
        use base64::Engine;
        let hex_lower = super::fsutil::hex(&self.0);
        let mut forms: Vec<Vec<u8>> = vec![
            self.0.to_vec(),
            hex_lower.clone().into_bytes(),
            hex_lower.to_ascii_uppercase().into_bytes(),
            base64::engine::general_purpose::STANDARD
                .encode(self.0)
                .into_bytes(),
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(self.0)
                .into_bytes(),
        ];
        let found = forms.iter().any(|form| {
            haystack
                .windows(form.len())
                .any(|window| window == form.as_slice())
        });
        for form in &mut forms {
            form.zeroize();
        }
        found
    }
}

impl Drop for VaultKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl std::fmt::Debug for VaultKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VaultKey(..)")
    }
}

/// The 256 bits behind the Recovery Spell (ふっかつのじゅもん), from the OS CSPRNG: the one secret a
/// recovery needs (Slice E). Its bytes are only ever the input keying material of HKDF
/// (`recovery::vault_key`), never a key themselves.
pub struct RecoverySpell([u8; 32]);

impl RecoverySpell {
    pub fn generate() -> Result<Self, RandomError> {
        let mut spell = Self([0u8; 32]);
        random_bytes(&mut spell.0)?;
        Ok(spell)
    }

    pub(crate) fn from_bytes(bytes: &mut [u8; 32]) -> Self {
        let spell = Self(*bytes);
        bytes.zeroize();
        spell
    }

    pub(crate) fn expose(&self) -> &[u8; 32] {
        &self.0
    }

    /// Constant-time equality.
    pub fn same_as(&self, other: &Self) -> bool {
        self.0.ct_eq(&other.0).into()
    }
}

impl Drop for RecoverySpell {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl std::fmt::Debug for RecoverySpell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RecoverySpell(..)")
    }
}

macro_rules! public_id {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, PartialEq, Eq, Hash)]
        pub struct $name([u8; 16]);

        impl $name {
            pub const fn from_array(bytes: [u8; 16]) -> Self {
                Self(bytes)
            }

            pub fn as_bytes(&self) -> &[u8; 16] {
                &self.0
            }

            /// 32 lower-case hexadecimal digits.
            pub fn to_hex(&self) -> String {
                super::fsutil::hex(&self.0)
            }

            /// Exactly 32 lower-case hexadecimal digits, as `to_hex` writes them.
            pub fn parse_hex(text: &str) -> Option<Self> {
                super::fsutil::unhex(text)?.try_into().ok().map(Self)
            }
        }

        impl std::fmt::Debug for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, concat!(stringify!($name), "({})"), self.to_hex())
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.to_hex())
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let text = String::deserialize(deserializer)?;
                Self::parse_hex(&text)
                    .ok_or_else(|| serde::de::Error::custom(concat!("invalid ", stringify!($name))))
            }
        }
    };
}

public_id!(
    VaultId,
    "128 bits naming one encrypted vault, in the config and the key store. Since Slice E a function of the vault's public salt (`recovery::vault_id`), so the vault file alone names it."
);
public_id!(
    CipherSalt,
    "The first 16 bytes of a SQLCipher file, stored in the clear: SQLCipher's salt and, since Slice E, the public salt the vault key is derived with. Random (OS CSPRNG) for every new vault."
);

impl VaultId {
    /// 128 random bits: tests, and identifiers that are not a vault's.
    pub fn generate() -> Result<Self, RandomError> {
        let mut bytes = [0u8; 16];
        random_bytes(&mut bytes)?;
        Ok(Self(bytes))
    }
}

impl CipherSalt {
    /// A new vault's salt, from the OS CSPRNG.
    pub fn generate() -> Result<Self, RandomError> {
        let mut bytes = [0u8; 16];
        random_bytes(&mut bytes)?;
        Ok(Self(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_independent_random_values_and_never_printed() {
        let keys: Vec<VaultKey> = (0..256).map(|_| VaultKey::generate().unwrap()).collect();
        for (i, a) in keys.iter().enumerate() {
            assert_ne!(a.expose(), &[0u8; 32]);
            for b in &keys[i + 1..] {
                assert!(!a.same_as(b));
            }
        }
        let shown = format!("{:?} {:?}", keys[0], RecoverySpell::generate().unwrap());
        assert_eq!(shown, "VaultKey(..) RecoverySpell(..)");
        assert!(!keys[0].appears_in(shown.as_bytes()));
    }

    #[test]
    fn a_key_is_found_in_any_leaked_form() {
        let mut bytes = [0x5au8; 32];
        bytes[0] = 0xc3;
        let key = VaultKey::from_bytes(&mut bytes);
        assert_eq!(bytes, [0u8; 32], "the caller's copy is zeroed");
        let hex = super::super::fsutil::hex(key.expose());
        for text in [format!("x'{hex}'"), hex.to_ascii_uppercase(), {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD.encode(key.expose())
        }] {
            assert!(key.appears_in(text.as_bytes()), "{}", text.len());
        }
        assert!(key.appears_in(key.expose()));
        assert!(!key.appears_in(&hex.as_bytes()[1..]));
    }

    #[test]
    fn ids_round_trip_as_lower_case_hex_only() {
        let id = VaultId::generate().unwrap();
        assert_eq!(VaultId::parse_hex(&id.to_hex()), Some(id));
        assert_eq!(id.to_hex().len(), 32);
        for bad in ["", "00", &"A".repeat(32), &"0".repeat(33), &"g".repeat(32)] {
            assert_eq!(VaultId::parse_hex(bad), None, "{bad}");
        }
        assert_ne!(VaultId::generate().unwrap(), id);
    }
}
