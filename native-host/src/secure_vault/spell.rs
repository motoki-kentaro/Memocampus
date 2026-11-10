//! The Recovery Spell (ふっかつのじゅもん): 256 bits from the OS CSPRNG, written as RFC 4648
//! base64url without padding, i.e. exactly 43 characters of `A-Z a-z 0-9 - _`.
//!
//! The 32 bytes are the input keying material of `recovery` (HKDF-SHA256 with per-purpose `info`
//! labels); they are never used as a key directly.
//!
//! Input is checked strictly and never "repaired": exactly 43 characters, each from the base64url
//! alphabet (case-sensitive; no `+`, `/`, `=`, no whitespace inside, no full-width letters), and the
//! canonical encoding of 32 bytes (the last character carries 4 data bits and 2 zero bits: a
//! character with either of those 2 bits set is refused, not rounded). Trimming spaces around a
//! pasted spell is the panel's job; the host accepts only the spell itself.
//!
//! A spell is only ever generated here; a string the user makes up is never adopted as a spell.

use super::secret::RecoverySpell;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

/// Characters of an encoded spell.
pub const LEN: usize = 43;
/// Bytes behind it.
pub const BYTES: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpellError {
    /// Not exactly 43 characters (the count found, in Unicode scalar values).
    Length(usize),
    /// 1-based position of the first character outside the base64url alphabet.
    Character(usize),
    /// 43 alphabet characters that are not the canonical encoding of 32 bytes.
    NonCanonical,
}

fn in_alphabet(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

/// The spell of `spell`: 43 base64url characters, no padding.
pub fn encode(spell: &RecoverySpell) -> Zeroizing<String> {
    let mut out = Zeroizing::new(String::with_capacity(LEN + 1));
    URL_SAFE_NO_PAD.encode_string(spell.expose(), &mut out);
    debug_assert_eq!(out.len(), LEN);
    out
}

/// The secret behind a spell typed or pasted by the user. Never trims, folds case or substitutes.
pub fn parse(input: &str) -> Result<RecoverySpell, SpellError> {
    let count = input.chars().count();
    if count != LEN {
        return Err(SpellError::Length(count));
    }
    if let Some(position) = input.chars().position(|c| !in_alphabet(c)) {
        return Err(SpellError::Character(position + 1));
    }
    // 43 ASCII characters from here on. The engine refuses padding and non-zero trailing bits;
    // the round trip below states the canonical-form rule independently of that configuration.
    let decoded = Zeroizing::new(
        URL_SAFE_NO_PAD
            .decode(input.as_bytes())
            .map_err(|_| SpellError::NonCanonical)?,
    );
    let mut bytes: [u8; BYTES] = decoded
        .as_slice()
        .try_into()
        .map_err(|_| SpellError::NonCanonical)?;
    let spell = RecoverySpell::from_bytes(&mut bytes);
    if !bool::from(encode(&spell).as_bytes().ct_eq(input.as_bytes())) {
        return Err(SpellError::NonCanonical);
    }
    Ok(spell)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spell_of(bytes: [u8; 32]) -> RecoverySpell {
        let mut bytes = bytes;
        RecoverySpell::from_bytes(&mut bytes)
    }

    #[test]
    fn a_generated_spell_is_32_random_bytes_written_as_43_url_safe_characters() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..256 {
            let spell = RecoverySpell::generate().unwrap();
            assert_eq!(spell.expose().len(), BYTES);
            let text = encode(&spell);
            assert_eq!(text.len(), LEN);
            assert_eq!(text.chars().count(), LEN);
            assert!(text.chars().all(in_alphabet), "{}", text.len());
            assert!(!text.contains(['+', '/', '=']));
            let back = parse(&text).unwrap();
            assert!(back.same_as(&spell));
            assert_eq!(back.expose(), spell.expose());
            assert!(seen.insert(text.to_string()), "two spells were equal");
        }
    }

    #[test]
    fn known_vectors_round_trip() {
        let zeros = encode(&spell_of([0; 32]));
        assert_eq!(zeros.as_str(), &"A".repeat(43));
        let ones = encode(&spell_of([0xff; 32]));
        assert_eq!(ones.as_str(), format!("{}8", "_".repeat(42)));
        let mut counting = [0u8; 32];
        for (i, byte) in counting.iter_mut().enumerate() {
            *byte = i as u8 * 8 + 3;
        }
        let text = encode(&spell_of(counting));
        assert_eq!(text.as_str(), "AwsTGyMrMztDS1NbY2tze4OLk5ujq7O7w8vT2-Pr8_s");
        assert_eq!(parse(&text).unwrap().expose(), &counting);
        // `-` and `_` (values 62 and 63) decode, where standard base64 would use `+` and `/`.
        assert_eq!(parse(&ones).unwrap().expose(), &[0xff; 32]);
    }

    #[test]
    fn length_is_counted_in_characters_and_must_be_exactly_43() {
        let text = encode(&RecoverySpell::generate().unwrap());
        assert_eq!(parse(&text[..42]).unwrap_err(), SpellError::Length(42));
        assert_eq!(
            parse(&format!("{}A", text.as_str())).unwrap_err(),
            SpellError::Length(44)
        );
        assert_eq!(parse("").unwrap_err(), SpellError::Length(0));
        // Surrounding whitespace is the panel's to trim, not the host's.
        assert_eq!(
            parse(&format!(" {}", text.as_str())).unwrap_err(),
            SpellError::Length(44)
        );
        assert_eq!(
            parse(&format!("{}\n", text.as_str())).unwrap_err(),
            SpellError::Length(44)
        );
        // Padding to 44 is refused too.
        assert_eq!(
            parse(&format!("{}=", text.as_str())).unwrap_err(),
            SpellError::Length(44)
        );
    }

    #[test]
    fn characters_outside_the_url_safe_alphabet_are_refused_by_position() {
        let text = encode(&RecoverySpell::generate().unwrap()).to_string();
        let replace = |position: usize, with: &str| {
            let mut chars: Vec<String> = text.chars().map(String::from).collect();
            chars[position] = with.into();
            chars.concat()
        };
        for (position, bad) in [
            (0, "+"),
            (5, "/"),
            (42, "="),
            (20, " "),
            (20, "\t"),
            (10, "."),
            (10, "~"),
            (30, "Ａ"), // full-width A, one character
            (30, "０"),
            (30, "é"),
            (1, "\u{0}"),
        ] {
            let candidate = replace(position, bad);
            assert_eq!(candidate.chars().count(), LEN);
            assert_eq!(
                parse(&candidate).unwrap_err(),
                SpellError::Character(position + 1),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn a_last_character_with_trailing_bits_set_is_refused_not_rounded() {
        let text = encode(&spell_of([0; 32])).to_string();
        let stem = &text[..42];
        // 'A' = 0b000000 is canonical; 'B', 'C', 'D' carry a set bit in the 2 unused bits.
        for last in "BCDFGHJ".chars() {
            assert_eq!(
                parse(&format!("{stem}{last}")).unwrap_err(),
                SpellError::NonCanonical,
                "{last}"
            );
        }
        // Exactly the characters whose low two bits are zero are accepted at the end.
        let canonical: String = (0u8..64)
            .filter(|value| value & 0b11 == 0)
            .map(|value| URL_SAFE_NO_PAD.encode([value << 2]).chars().next().unwrap())
            .collect();
        assert_eq!(canonical, "AEIMQUYcgkosw048");
        for last in canonical.chars() {
            assert!(parse(&format!("{stem}{last}")).is_ok(), "{last}");
        }
    }

    #[test]
    fn case_matters_and_a_different_spell_is_a_different_secret() {
        let spell = spell_of([0x5a; 32]);
        let text = encode(&spell).to_string();
        let position = text
            .chars()
            .position(|c| c.is_ascii_alphabetic())
            .expect("a letter");
        let flipped: String = text
            .chars()
            .enumerate()
            .map(|(i, c)| {
                if i == position {
                    if c.is_ascii_uppercase() {
                        c.to_ascii_lowercase()
                    } else {
                        c.to_ascii_uppercase()
                    }
                } else {
                    c
                }
            })
            .collect();
        assert_ne!(flipped, text);
        match parse(&flipped) {
            Ok(other) => assert!(!other.same_as(&spell)),
            Err(error) => assert_eq!(error, SpellError::NonCanonical),
        }
        assert!(text.chars().any(|c| c.is_ascii_lowercase()));
        assert!(!matches!(
            parse(&text.to_ascii_uppercase()),
            Ok(other) if other.same_as(&spell)
        ));
        let a = RecoverySpell::generate().unwrap();
        let b = RecoverySpell::generate().unwrap();
        assert!(!a.same_as(&b));
        assert_ne!(encode(&a).as_str(), encode(&b).as_str());
    }

    #[test]
    fn a_spell_is_never_printed() {
        let spell = RecoverySpell::generate().unwrap();
        let shown = format!("{spell:?} {:?}", parse(&encode(&spell)));
        assert!(!shown.contains(encode(&spell).as_str()));
        assert!(shown.contains("RecoverySpell(..)"));
    }
}
