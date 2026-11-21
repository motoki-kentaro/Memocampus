//! Character encoding of the root HTML part.
//!
//! Order (the HTML Standard's, with MIME as the transport layer):
//!   1. Byte order mark (UTF-8, UTF-16LE, UTF-16BE).
//!   2. `charset` parameter of the part's MIME Content-Type (Chrome writes none for HTML parts).
//!   3. `<meta charset>` / `<meta http-equiv="Content-Type" content="...charset=...">` in the first
//!      1024 bytes (simplified HTML prescan; Chrome always inserts such a tag when it serializes).
//!   4. Nothing declared: UTF-8, reported as `default`.
//!
//! A label that encoding_rs does not know is skipped (warning) and the next source is used. A label
//! that maps to the `replacement` encoding is refused. Per the HTML Standard a `<meta>` that names
//! UTF-16 means UTF-8, and `x-user-defined` means windows-1252. Malformed byte sequences become
//! U+FFFD and are reported; the text is not refused for them.

use crate::limits::ErrorCode;
use encoding_rs::{Encoding, REPLACEMENT, UTF_16BE, UTF_16LE, UTF_8, WINDOWS_1252, X_USER_DEFINED};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EncodingSource {
    Bom,
    MimeCharset,
    MetaPrescan,
    Default,
}

fn label(raw: &str) -> Option<&'static Encoding> {
    Encoding::for_label(
        raw.trim()
            .trim_matches(|c| c == '"' || c == '\'')
            .as_bytes(),
    )
}

fn lower(b: u8) -> u8 {
    b.to_ascii_lowercase()
}

fn starts_with_ci(hay: &[u8], needle: &[u8]) -> bool {
    hay.len() >= needle.len() && hay.iter().zip(needle).all(|(a, b)| lower(*a) == *b)
}

/// One attribute of a tag, as the prescan reads it: (lower-cased name, value bytes).
fn next_attribute(s: &[u8], mut i: usize) -> Option<(Vec<u8>, Vec<u8>, usize)> {
    while i < s.len() && (s[i].is_ascii_whitespace() || s[i] == b'/') {
        i += 1;
    }
    if i >= s.len() || s[i] == b'>' {
        return None;
    }
    let mut name = Vec::new();
    while i < s.len() && !s[i].is_ascii_whitespace() && s[i] != b'=' && s[i] != b'>' && s[i] != b'/'
    {
        name.push(lower(s[i]));
        i += 1;
    }
    while i < s.len() && s[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= s.len() || s[i] != b'=' {
        return Some((name, Vec::new(), i));
    }
    i += 1;
    while i < s.len() && s[i].is_ascii_whitespace() {
        i += 1;
    }
    let mut value = Vec::new();
    if i < s.len() && (s[i] == b'"' || s[i] == b'\'') {
        let quote = s[i];
        i += 1;
        while i < s.len() && s[i] != quote {
            value.push(s[i]);
            i += 1;
        }
        i += 1;
    } else {
        while i < s.len() && !s[i].is_ascii_whitespace() && s[i] != b'>' {
            value.push(s[i]);
            i += 1;
        }
    }
    Some((name, value, i))
}

/// `charset=` inside a Content-Type value.
fn charset_in_content(value: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(value).to_ascii_lowercase();
    let at = text.find("charset")?;
    let rest = text[at + 7..].trim_start().strip_prefix('=')?.trim_start();
    let rest = rest.trim_start_matches(['"', '\'']);
    let end = rest
        .find(|c: char| c == ';' || c == '"' || c == '\'' || c.is_whitespace())
        .unwrap_or(rest.len());
    (end > 0).then(|| rest[..end].to_owned())
}

/// Simplified HTML encoding prescan of the first 1024 bytes: skips comments, reads `<meta>` tags.
/// Returns the first declared label (even one encoding_rs does not know, so it can be reported).
pub fn prescan(bytes: &[u8]) -> Option<String> {
    let s = &bytes[..bytes.len().min(1024)];
    let mut i = 0;
    while i < s.len() {
        if s[i..].starts_with(b"<!--") {
            let end = s[i + 4..].windows(3).position(|w| w == b"-->")?;
            i += 4 + end + 3;
            continue;
        }
        if starts_with_ci(&s[i..], b"<meta")
            && s.get(i + 5)
                .is_some_and(|c| c.is_ascii_whitespace() || *c == b'/')
        {
            let mut j = i + 5;
            let (mut charset, mut http_equiv_ct, mut content) = (None, false, None);
            while let Some((name, value, next)) = next_attribute(s, j) {
                j = next;
                match name.as_slice() {
                    b"charset" if charset.is_none() => {
                        charset = Some(String::from_utf8_lossy(&value).into_owned())
                    }
                    b"http-equiv" => http_equiv_ct = value.eq_ignore_ascii_case(b"content-type"),
                    b"content" if content.is_none() => content = Some(value),
                    _ => {}
                }
            }
            if let Some(c) = charset {
                return Some(c);
            }
            if http_equiv_ct {
                if let Some(c) = content.as_deref().and_then(charset_in_content) {
                    return Some(c);
                }
            }
            i = j.max(i + 1);
            continue;
        }
        i += 1;
    }
    None
}

/// The encoding decision; nothing is converted yet. `body_offset` skips a BOM.
#[derive(Debug)]
pub struct Detected {
    pub encoding: &'static Encoding,
    pub source: EncodingSource,
    pub body_offset: usize,
    pub warnings: Vec<&'static str>,
}

pub fn detect(bytes: &[u8], mime_charset: Option<&str>) -> Result<Detected, ErrorCode> {
    let mut warnings = Vec::new();
    let (encoding, source, body) = if let Some((enc, bom)) = Encoding::for_bom(bytes) {
        (enc, EncodingSource::Bom, &bytes[bom..])
    } else {
        let from_mime = match mime_charset {
            Some(raw) => match label(raw) {
                Some(enc) => Some(enc),
                None => {
                    warnings.push("unknown_mime_charset_label_ignored");
                    None
                }
            },
            None => None,
        };
        if let Some(enc) = from_mime {
            (enc, EncodingSource::MimeCharset, bytes)
        } else {
            let from_meta = match prescan(bytes) {
                Some(raw) => match label(&raw) {
                    Some(enc) if enc == UTF_16LE || enc == UTF_16BE => Some(UTF_8),
                    Some(enc) if enc == X_USER_DEFINED => Some(WINDOWS_1252),
                    Some(enc) => Some(enc),
                    None => {
                        warnings.push("unknown_meta_charset_label_ignored");
                        None
                    }
                },
                None => None,
            };
            match from_meta {
                Some(enc) => (enc, EncodingSource::MetaPrescan, bytes),
                None => {
                    warnings.push("encoding_not_declared_utf8_assumed");
                    (UTF_8, EncodingSource::Default, bytes)
                }
            }
        }
    };
    if encoding == REPLACEMENT {
        return Err(ErrorCode::UnsupportedCharset);
    }
    Ok(Detected {
        encoding,
        source,
        body_offset: bytes.len() - body.len(),
        warnings,
    })
}

/// Converts the HTML bytes to UTF-8 in steps of at most `chunk_bytes` source bytes and hands each
/// converted piece to `sink`. The total UTF-8 output is counted while converting and refused above
/// `max_utf8_bytes` (`decoded_html_too_large`); the whole decoded document is never held at once.
/// Malformed sequences become U+FFFD (never repaired, never refused); the return value says
/// whether that happened. A sink error stops the conversion.
pub fn decode_streaming(
    encoding: &'static Encoding,
    body: &[u8],
    chunk_bytes: usize,
    max_utf8_bytes: usize,
    mut sink: impl FnMut(&str) -> Result<(), ErrorCode>,
) -> Result<bool, ErrorCode> {
    let chunk_bytes = chunk_bytes.max(1);
    let mut decoder = encoding.new_decoder_without_bom_handling();
    // Room for one step's worst case: 3 UTF-8 bytes per source byte, plus a pending sequence.
    let mut out = String::with_capacity(chunk_bytes.saturating_mul(3).saturating_add(16));
    let (mut total, mut malformed) = (0usize, false);
    let mut rest = body;
    loop {
        let take = rest.len().min(chunk_bytes);
        let last = take == rest.len();
        let mut piece = &rest[..take];
        loop {
            out.clear();
            let (result, read, replaced) = decoder.decode_to_string(piece, &mut out, last);
            malformed |= replaced;
            piece = &piece[read..];
            total = total.saturating_add(out.len());
            if total > max_utf8_bytes {
                return Err(ErrorCode::DecodedHtmlTooLarge);
            }
            if !out.is_empty() {
                sink(&out)?;
            }
            match result {
                encoding_rs::CoderResult::InputEmpty => break,
                encoding_rs::CoderResult::OutputFull => {
                    if read == 0 && out.is_empty() {
                        // Cannot happen with the capacity above; never loop without progress.
                        out.reserve(64);
                    }
                }
            }
        }
        rest = &rest[take..];
        if last {
            return Ok(malformed);
        }
    }
}
