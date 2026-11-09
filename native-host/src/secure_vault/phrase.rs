//! The 12-word recovery phrase: the BIP-39 English wordlist and its entropy ↔ words encoding with
//! the SHA-256 checksum (128 bits of entropy + 4 checksum bits = 12 × 11 bits).
//!
//! Only that encoding is used. Not used: the PBKDF2 mnemonic-to-seed step, the BIP-39 passphrase,
//! BIP-32 or any wallet derivation, other languages. The 16 entropy bytes are the recovery seed,
//! the input of `recovery::wrap`.
//!
//! Input is accepted case-insensitively with any whitespace between words. A word not in the list
//! is reported by position; the checksum catches most single-word mistakes, and whatever still
//! passes it is refused by the envelope's authentication.

use super::secret::RecoverySeed;
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

pub const WORDS: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhraseError {
    /// Not exactly 12 words (the count found).
    WordCount(usize),
    /// 1-based position of a word that is not in the list.
    UnknownWord(usize),
    /// Every word is in the list, but the checksum does not match: a word is wrong or misplaced.
    Checksum,
}

fn word_list() -> &'static [&'static str; 2048] {
    bip39::Language::English.word_list()
}

/// The phrase of `seed`: 12 lower-case words separated by single spaces.
pub fn encode(seed: &RecoverySeed) -> Zeroizing<String> {
    let entropy = seed.expose();
    let checksum = Sha256::digest(entropy)[0] >> 4;
    // 132 bits: the entropy, then the 4 checksum bits, read as 12 groups of 11.
    let mut bits = Zeroizing::new([0u8; 17]);
    bits[..16].copy_from_slice(entropy);
    bits[16] = checksum << 4;
    let mut out = Zeroizing::new(String::with_capacity(12 * 9));
    for i in 0..WORDS {
        let mut index = 0usize;
        for bit in i * 11..i * 11 + 11 {
            index = (index << 1) | usize::from((bits[bit / 8] >> (7 - bit % 8)) & 1);
        }
        if i > 0 {
            out.push(' ');
        }
        out.push_str(word_list()[index]);
    }
    out
}

/// The words of `phrase` (as `encode` writes it), for display.
pub fn words(phrase: &str) -> Vec<&str> {
    phrase.split(' ').collect()
}

/// The recovery seed of a phrase typed by the user.
pub fn parse(input: &str) -> Result<RecoverySeed, PhraseError> {
    let typed: Vec<Zeroizing<String>> = input
        .split_whitespace()
        .map(|word| Zeroizing::new(word.to_ascii_lowercase()))
        .collect();
    if typed.len() != WORDS {
        return Err(PhraseError::WordCount(typed.len()));
    }
    let mut indices = Zeroizing::new([0u16; WORDS]);
    for (i, word) in typed.iter().enumerate() {
        let index = word_list()
            .binary_search(&word.as_str())
            .map_err(|_| PhraseError::UnknownWord(i + 1))?;
        indices[i] = index as u16;
    }
    let mut bits = Zeroizing::new([0u8; 17]);
    for (i, index) in indices.iter().enumerate() {
        for b in 0..11 {
            if (index >> (10 - b)) & 1 == 1 {
                let bit = i * 11 + b;
                bits[bit / 8] |= 1 << (7 - bit % 8);
            }
        }
    }
    let mut entropy = [0u8; 16];
    entropy.copy_from_slice(&bits[..16]);
    if Sha256::digest(entropy)[0] >> 4 != bits[16] >> 4 {
        entropy.zeroize();
        return Err(PhraseError::Checksum);
    }
    Ok(RecoverySeed::from_bytes(&mut entropy))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(byte: u8) -> RecoverySeed {
        RecoverySeed::from_bytes(&mut [byte; 16])
    }

    /// The official BIP-39 test vectors for 128-bit entropy (trezor/python-mnemonic vectors.json).
    #[test]
    fn official_vectors_encode_and_parse() {
        let vectors = [
            (0x00, "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about"),
            (0x7f, "legal winner thank year wave sausage worth useful legal winner thank yellow"),
            (0x80, "letter advice cage absurd amount doctor acoustic avoid letter advice cage above"),
            (0xff, "zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo wrong"),
        ];
        for (byte, phrase) in vectors {
            assert_eq!(encode(&seed(byte)).as_str(), phrase);
            assert_eq!(parse(phrase).unwrap().expose(), &[byte; 16]);
        }
        assert_eq!(
            encode(&RecoverySeed::from_bytes(&mut [
                0x9e, 0x88, 0x5d, 0x95, 0x2a, 0xd3, 0x62, 0xca, 0xeb, 0x4e, 0xfe, 0x34, 0xa8, 0xe9,
                0x1b, 0xd2
            ]))
            .as_str(),
            "ozone drill grab fiber curtain grace pudding thank cruise elder eight picnic"
        );
    }

    #[test]
    fn the_wordlist_is_the_sorted_english_list_with_unique_prefixes() {
        let list = word_list();
        assert_eq!(list.len(), 2048);
        assert!(
            list.windows(2).all(|pair| pair[0] < pair[1]),
            "binary search needs order"
        );
        let prefixes: std::collections::HashSet<String> = list
            .iter()
            .map(|word| word.chars().take(4).collect())
            .collect();
        assert_eq!(prefixes.len(), 2048);
    }

    #[test]
    fn input_is_normalised_and_errors_name_the_problem() {
        let phrase = encode(&seed(0x7f));
        let shouted = format!("  {}\n", phrase.to_ascii_uppercase().replace(' ', " \t "));
        assert_eq!(parse(&shouted).unwrap().expose(), &[0x7f; 16]);
        let words: Vec<&str> = phrase.split(' ').collect();
        assert_eq!(
            parse(&words[..11].join(" ")).unwrap_err(),
            PhraseError::WordCount(11)
        );
        assert_eq!(
            parse(&format!("{} zoo", phrase.as_str())).unwrap_err(),
            PhraseError::WordCount(13)
        );
        let mut unknown = words.clone();
        unknown[4] = "memocampus";
        assert_eq!(
            parse(&unknown.join(" ")).unwrap_err(),
            PhraseError::UnknownWord(5)
        );
        unknown[4] = "lègal";
        assert_eq!(
            parse(&unknown.join(" ")).unwrap_err(),
            PhraseError::UnknownWord(5)
        );
    }

    #[test]
    fn about_one_in_sixteen_single_word_changes_pass_the_checksum() {
        let phrase = encode(&RecoverySeed::generate().unwrap());
        let words: Vec<&str> = phrase.split(' ').collect();
        let (mut tried, mut passed) = (0u32, 0u32);
        for position in 0..WORDS {
            for replacement in word_list().iter().step_by(7) {
                if *replacement == words[position] {
                    continue;
                }
                let mut changed = words.clone();
                changed[position] = replacement;
                tried += 1;
                match parse(&changed.join(" ")) {
                    Ok(_) => passed += 1,
                    Err(error) => assert_eq!(error, PhraseError::Checksum),
                }
            }
        }
        let rate = f64::from(passed) / f64::from(tried);
        assert!((0.04..0.09).contains(&rate), "{passed}/{tried}");
    }
}
