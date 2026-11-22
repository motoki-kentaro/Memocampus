//! Slice 2: quality gate between the extraction (Slice 1) and the search index.
//!
//! Two measures per text (body and title separately), numbers only:
//! - **replacement characters** (U+FFFD): bytes that were not valid in the chosen encoding;
//! - **misdecoded UTF-8**: text that is UTF-8 read with a legacy single/double-byte encoding (the
//!   usual mojibake: `æ—¥æœ¬` for `日本` through windows-1252, `譌･譛ｬ` through Shift_JIS). It
//!   contains no U+FFFD, so it is measured by encoding each character back with the legacy
//!   encoding and checking whether the bytes form valid multi-byte UTF-8 sequences. Genuine text
//!   rarely does; mojibake almost always does.
//!
//! The thresholds in [`GateConfig::default`] are **provisional** (PoC): chosen from the
//! measurements in the report (XIV), not a product specification.

use encoding_rs::{EncoderResult, Encoding, UTF_16BE, UTF_16LE, UTF_8};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
pub struct TextQuality {
    pub chars: usize,
    pub non_ascii_chars: usize,
    pub replacement_chars: usize,
    /// replacement_chars per 1000 characters.
    pub replacement_per_mille: u32,
    /// Characters that, encoded back with `misdecoded_as`, form valid multi-byte UTF-8.
    pub misdecoded_chars: usize,
    /// misdecoded_chars per 1000 non-ASCII characters.
    pub misdecoded_per_mille: u32,
    pub misdecoded_as: Option<&'static str>,
    /// Control characters the extractor removed from this text (body only: Slice 1 reports
    /// them for the body). C1 controls appear when UTF-8 bytes are read with ISO-8859-x and
    /// similar encodings, so many of them mean mojibake even when the rest looks normal.
    pub removed_controls: usize,
    /// removed_controls per 1000 of (characters + removed_controls).
    pub removed_control_per_mille: u32,
}

fn per_mille(part: usize, whole: usize) -> u32 {
    if whole == 0 {
        0
    } else {
        ((part as u128 * 1000) / whole as u128) as u32
    }
}

/// Characters of `text` in runs (maximal sequences of non-ASCII characters that `enc` can
/// encode) whose encoded bytes are, from start to end, valid UTF-8. Mojibake runs are (they are
/// UTF-8 bytes read with `enc`); runs of genuine text almost never are: the first byte of a
/// legacy-encoded character is rarely a valid UTF-8 lead followed by the right continuation bytes
/// over a whole run. (Checking each sequence separately instead would count genuine EUC-JP
/// kanji, whose two bytes are often a valid 2-byte UTF-8 sequence.)
/// A run is misdecoded UTF-8 when at least 3/4 of its bytes form valid multi-byte UTF-8 sequences
/// (read greedily from the start). Not "all": the extractor removes C1 control characters and
/// collapses whitespace, so a windows-1252 mojibake run has lost some of its bytes (`ã` U+0081
/// `‚` for `あ` keeps only `ã‚`).
fn run_is_misdecoded(bytes: &[u8]) -> bool {
    if bytes.len() < 2 {
        return false;
    }
    let mut i = 0;
    let mut good = 0;
    while i < bytes.len() {
        let len = match bytes[i] {
            0xC2..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF4 => 4,
            _ => 0,
        };
        if len > 0 && i + len <= bytes.len() && std::str::from_utf8(&bytes[i..i + len]).is_ok() {
            good += len;
            i += len;
        } else {
            i += 1;
        }
    }
    good * 4 >= bytes.len() * 3
}

pub fn misdecoded_chars(text: &str, enc: &'static Encoding) -> usize {
    let mut encoder = enc.new_encoder();
    let mut bytes: Vec<u8> = Vec::new();
    let mut run_chars = 0usize;
    let mut count = 0;
    let mut buf = [0u8; 16];
    let flush = |bytes: &mut Vec<u8>, run_chars: &mut usize, count: &mut usize| {
        if run_is_misdecoded(bytes) {
            *count += *run_chars;
        }
        bytes.clear();
        *run_chars = 0;
    };
    for c in text.chars() {
        if c.is_ascii() {
            flush(&mut bytes, &mut run_chars, &mut count);
            continue;
        }
        let mut s = [0u8; 4];
        let src = c.encode_utf8(&mut s);
        let (result, _, written) =
            encoder.encode_from_utf8_without_replacement(src, &mut buf, false);
        match result {
            EncoderResult::InputEmpty => {
                bytes.extend_from_slice(&buf[..written]);
                run_chars += 1;
            }
            _ => {
                flush(&mut bytes, &mut run_chars, &mut count);
                encoder = enc.new_encoder();
            }
        }
    }
    flush(&mut bytes, &mut run_chars, &mut count);
    count
}

/// Measures `text`, which the extractor decoded with `decoded_with`. Misdecoded UTF-8 can only
/// come from a page decoded with a non-UTF-8 encoding, so it is checked with that encoding only
/// (a UTF-8 page is never suspected of it; its bad bytes show up as U+FFFD instead).
pub fn measure(text: &str, decoded_with: &'static Encoding) -> TextQuality {
    let mut q = TextQuality::default();
    for c in text.chars() {
        q.chars += 1;
        if !c.is_ascii() {
            q.non_ascii_chars += 1;
        }
        if c == '\u{FFFD}' {
            q.replacement_chars += 1;
        }
    }
    q.replacement_per_mille = per_mille(q.replacement_chars, q.chars);
    let legacy = decoded_with != UTF_8 && decoded_with != UTF_16LE && decoded_with != UTF_16BE;
    if q.non_ascii_chars > 0 && legacy {
        q.misdecoded_chars = misdecoded_chars(text, decoded_with);
        q.misdecoded_as = Some(decoded_with.name());
    }
    q.misdecoded_per_mille = per_mille(q.misdecoded_chars, q.non_ascii_chars);
    q
}

/// [`measure`] plus the control characters the extractor removed from this text.
pub fn measure_with_removed(
    text: &str,
    decoded_with: &'static Encoding,
    removed_controls: usize,
) -> TextQuality {
    let mut q = measure(text, decoded_with);
    q.removed_controls = removed_controls;
    q.removed_control_per_mille = per_mille(removed_controls, q.chars + removed_controls);
    q
}

/// The encoding named by the extraction diagnostics (`Encoding::name()`), UTF-8 if unknown.
pub fn encoding_named(name: &str) -> &'static Encoding {
    Encoding::for_label(name.as_bytes()).unwrap_or(UTF_8)
}

/// Provisional thresholds (PoC). A text is **garbled** (not indexed) when either rule holds.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct GateConfig {
    /// At least this many U+FFFD and at least `max_replacement_per_mille` of the text.
    pub min_replacement_chars: usize,
    pub max_replacement_per_mille: u32,
    /// At least this many non-ASCII characters (fewer is too small a sample to judge) and at
    /// least `max_misdecoded_per_mille` of them misdecoded UTF-8.
    pub min_non_ascii_chars: usize,
    pub max_misdecoded_per_mille: u32,
    /// At least this many removed control characters and at least `max_removed_control_per_mille`.
    pub min_removed_controls: usize,
    pub max_removed_control_per_mille: u32,
}

impl Default for GateConfig {
    fn default() -> Self {
        Self {
            min_replacement_chars: 4,
            max_replacement_per_mille: 10,
            min_non_ascii_chars: 16,
            max_misdecoded_per_mille: 300,
            min_removed_controls: 8,
            max_removed_control_per_mille: 50,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Clean,
    /// Some replacement characters or misdecoded characters, below the thresholds: indexed, with
    /// the numbers kept.
    Suspect,
    Garbled,
    Empty,
}

impl GateConfig {
    pub fn judge(&self, q: &TextQuality) -> Verdict {
        if q.chars == 0 {
            return Verdict::Empty;
        }
        let replacement = q.replacement_chars >= self.min_replacement_chars
            && q.replacement_per_mille >= self.max_replacement_per_mille;
        let misdecoded = q.non_ascii_chars >= self.min_non_ascii_chars
            && q.misdecoded_per_mille >= self.max_misdecoded_per_mille;
        let controls = q.removed_controls >= self.min_removed_controls
            && q.removed_control_per_mille >= self.max_removed_control_per_mille;
        if replacement || misdecoded || controls {
            Verdict::Garbled
        } else if q.replacement_chars > 0 || q.misdecoded_chars > 0 || q.removed_controls > 0 {
            Verdict::Suspect
        } else {
            Verdict::Clean
        }
    }

    /// The title is decoded with the same encoding as the body but is usually too short to judge
    /// alone (and the extractor does not report control characters removed from it). When the
    /// body is garbled, the encoding is in doubt: a title with any non-ASCII character is garbled
    /// too. An ASCII-only title is kept (ASCII reads the same in every ASCII-compatible encoding).
    pub fn judge_title(&self, title: &TextQuality, body_verdict: Verdict) -> Verdict {
        let own = self.judge(title);
        if body_verdict == Verdict::Garbled && title.non_ascii_chars > 0 {
            Verdict::Garbled
        } else {
            own
        }
    }
}
