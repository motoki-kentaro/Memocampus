//! MHTML structure: parse with `mail-parser`, then validate strictly and pick the root HTML part.
//!
//! `mail-parser` is lenient by design: it parses broken input on a best-effort basis and *reports*
//! what was wrong (part flags, decode problems). Parsing successfully is therefore not taken as the
//! archive being valid. Every flag and problem that means a broken structure or encoding is turned
//! into an error here, and every leaf part's transfer encoding is checked without decoding it.
//!
//! Root selection (RFC 2387 and what Chrome writes, see README):
//!   1. `start` parameter of the top-level `multipart/related`: the child whose Content-ID it names.
//!      No match, or more than one, is an error (never a guess).
//!   2. Without `start`: the first child (RFC 2387 default). Chrome writes no `start` and puts the
//!      main frame first, with `Content-Location` equal to the `Snapshot-Content-Location` header.
//!
//! The root must be `text/html`; otherwise the archive is refused (no other HTML part is taken
//! silently instead). Other HTML parts (iframes, which Chrome stores as separate
//! `cid:frame-...@mhtml.blink` parts) are counted, not extracted.

use crate::limits::{ErrorCode, Limits};
use mail_parser::{MessageParser, MessagePart, PartFlags};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RootRule {
    StartParameter,
    FirstPart,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TransferEncoding {
    SevenBit,
    EightBit,
    Binary,
    QuotedPrintable,
    Base64,
}

#[derive(Debug)]
pub struct Root<'a> {
    /// Transfer-decoded bytes of the root HTML part (an owned copy: at most `max_html_bytes`).
    pub html: Vec<u8>,
    _input: std::marker::PhantomData<&'a [u8]>,
    /// The part's id in document order (0 is the top-level message).
    pub part_id: usize,
    pub rule: RootRule,
    pub transfer_encoding: TransferEncoding,
    /// `charset` parameter of the root part's Content-Type, as written.
    pub mime_charset: Option<String>,
    pub content_location: Option<String>,
    pub snapshot_location: Option<String>,
    /// Every part, the top-level message and containers included.
    pub part_count: usize,
    /// Leaf parts (resources and documents).
    pub leaf_count: usize,
    pub html_part_count: usize,
    pub warnings: Vec<&'static str>,
}

fn is_type(part: &MessagePart<'_>, ty: &str, sub: &str) -> bool {
    part.content_type().is_some_and(|ct| {
        ct.ctype().eq_ignore_ascii_case(ty)
            && ct.subtype().is_some_and(|s| s.eq_ignore_ascii_case(sub))
    })
}

fn is_multipart_type(part: &MessagePart<'_>) -> bool {
    part.content_type()
        .is_some_and(|ct| ct.ctype().eq_ignore_ascii_case("multipart"))
}

/// RFC 2046 boundary: 1..=70 characters of `bchars`, not ending with a space.
fn valid_boundary(b: &str) -> bool {
    let ok = |c: u8| c.is_ascii_alphanumeric() || b"'()+_,-./:=? ".contains(&c);
    (1..=70).contains(&b.len()) && b.bytes().all(ok) && !b.ends_with(' ')
}

fn transfer_encoding(part: &MessagePart<'_>) -> Result<TransferEncoding, ErrorCode> {
    let Some(raw) = part.content_transfer_encoding() else {
        return Ok(TransferEncoding::SevenBit);
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "7bit" => Ok(TransferEncoding::SevenBit),
        "8bit" => Ok(TransferEncoding::EightBit),
        "binary" => Ok(TransferEncoding::Binary),
        "quoted-printable" => Ok(TransferEncoding::QuotedPrintable),
        "base64" => Ok(TransferEncoding::Base64),
        _ => Err(ErrorCode::InvalidTransferEncoding),
    }
}

/// Base64 body check without decoding or allocating: only the alphabet, whitespace (ignored) and
/// `=` padding at the end; the number of significant characters is a multiple of 4.
pub fn check_base64(body: &[u8]) -> Result<(), ErrorCode> {
    let mut count = 0usize;
    let mut padding = 0usize;
    for &b in body {
        match b {
            b' ' | b'\t' | b'\r' | b'\n' => {}
            b'=' => {
                padding += 1;
                count += 1;
                if padding > 2 {
                    return Err(ErrorCode::InvalidBase64);
                }
            }
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'+' | b'/' => {
                if padding > 0 {
                    return Err(ErrorCode::InvalidBase64);
                }
                count += 1;
            }
            _ => return Err(ErrorCode::InvalidBase64),
        }
    }
    if !count.is_multiple_of(4) {
        return Err(ErrorCode::InvalidBase64);
    }
    Ok(())
}

/// Quoted-printable check without decoding: every `=` is followed by two hex digits, or by
/// optional spaces/tabs and a line end (soft line break), or ends the body.
pub fn check_quoted_printable(body: &[u8]) -> Result<(), ErrorCode> {
    let mut i = 0;
    while i < body.len() {
        if body[i] == b'=' {
            let rest = &body[i + 1..];
            if rest.len() >= 2 && rest[0].is_ascii_hexdigit() && rest[1].is_ascii_hexdigit() {
                i += 3;
                continue;
            }
            let mut j = 0;
            while j < rest.len() && (rest[j] == b' ' || rest[j] == b'\t') {
                j += 1;
            }
            let soft = j == rest.len()
                || rest[j] == b'\n'
                || (rest[j] == b'\r' && rest.get(j + 1) == Some(&b'\n'));
            if !soft {
                return Err(ErrorCode::InvalidQuotedPrintable);
            }
            i += 1 + j;
            continue;
        }
        i += 1;
    }
    Ok(())
}

fn check_leaf(
    part: &MessagePart<'_>,
    warnings: &mut Vec<&'static str>,
) -> Result<TransferEncoding, ErrorCode> {
    let te = transfer_encoding(part)?;
    let body = part.raw_body();
    match te {
        TransferEncoding::Base64 => check_base64(body)?,
        TransferEncoding::QuotedPrintable => check_quoted_printable(body)?,
        TransferEncoding::SevenBit => {
            if body.iter().any(|&b| b >= 0x80) {
                push(warnings, "seven_bit_part_has_8bit_bytes");
            }
        }
        TransferEncoding::EightBit | TransferEncoding::Binary => {}
    }
    Ok(te)
}

fn push(warnings: &mut Vec<&'static str>, code: &'static str) {
    if !warnings.contains(&code) {
        warnings.push(code);
    }
}

/// The raw bytes of a header read as UTF-8. Bytes that are not UTF-8 drop the value with a warning
/// (never a lossy or partial value).
fn header_text<'a>(
    raw: &'a [u8],
    invalid_utf8: &'static str,
    warnings: &mut Vec<&'static str>,
) -> Option<&'a str> {
    let Ok(text) = std::str::from_utf8(raw) else {
        push(warnings, invalid_utf8);
        return None;
    };
    let text = text.trim();
    (!text.is_empty()).then_some(text)
}

/// Which header value a check is about (selects the warning codes).
#[derive(Clone, Copy)]
enum HeaderValue {
    ContentLocation,
    SnapshotLocation,
    MimeCharset,
}

/// A header-derived string kept in the output: trimmed, at most `max` bytes, no control
/// characters. An abnormal value is dropped (`None`) with a warning, never kept partly.
fn checked_header(
    raw: Option<&str>,
    max: usize,
    which: HeaderValue,
    warnings: &mut Vec<&'static str>,
) -> Option<String> {
    let value = raw?.trim();
    if value.is_empty() {
        return None;
    }
    let (too_large, control, invalid) = match which {
        HeaderValue::ContentLocation => (
            "content_location_too_large_dropped",
            "content_location_control_characters_dropped",
            "content_location_invalid_utf8_dropped",
        ),
        HeaderValue::SnapshotLocation => (
            "snapshot_location_too_large_dropped",
            "snapshot_location_control_characters_dropped",
            "snapshot_location_invalid_utf8_dropped",
        ),
        HeaderValue::MimeCharset => (
            "mime_charset_too_large_ignored",
            "mime_charset_control_characters_ignored",
            "mime_charset_invalid_utf8_ignored",
        ),
    };
    if value.len() > max {
        push(warnings, too_large);
        return None;
    }
    // A replacement character means the parser met bytes that were not UTF-8 (the MIME charset
    // parameter is only available parsed); a real header value has no reason to contain one.
    if value.contains('\u{FFFD}') {
        push(warnings, invalid);
        return None;
    }
    if value.chars().any(char::is_control) {
        push(warnings, control);
        return None;
    }
    Some(value.to_owned())
}

pub fn select_root<'a>(input: &'a [u8], limits: &Limits) -> Result<Root<'a>, ErrorCode> {
    if input.len() > limits.max_input_bytes {
        return Err(ErrorCode::InputTooLarge);
    }
    // Encoded nested messages are never parsed (`max_encoded_nesting(0)`); a `message/*` part is
    // refused below anyway. The depth and part limits are the library's own; reaching either sets
    // LIMIT_REACHED, which is an error here, never a truncated success.
    let parser = MessageParser::new()
        .max_parts(limits.max_parts)
        .max_depth(limits.max_depth)
        .max_encoded_nesting(0);
    let message = parser.parse(input).ok_or(ErrorCode::NotMime)?;
    let top = message.root_part();
    if !is_type(&top, "multipart", "related") {
        return Err(ErrorCode::NotMultipartRelated);
    }
    let ct = top.content_type().ok_or(ErrorCode::NotMultipartRelated)?;
    match ct.attribute("boundary") {
        Some(b) if valid_boundary(b) => {}
        _ => return Err(ErrorCode::InvalidBoundary),
    }

    let mut warnings = Vec::new();
    let part_count = message.parts().len();
    let mut leaf_count = 0;
    let mut html_part_count = 0;
    for part in message.parts() {
        let flags = part.flags();
        if flags.contains(PartFlags::LIMIT_REACHED) {
            return Err(if part_count >= limits.max_parts {
                ErrorCode::TooManyParts
            } else {
                ErrorCode::NestingTooDeep
            });
        }
        if flags.contains(PartFlags::UNTERMINATED) {
            return Err(ErrorCode::UnterminatedMultipart);
        }
        if flags.intersects(
            PartFlags::MISSING_DELIMITER | PartFlags::FALLBACK_DELIMITER | PartFlags::NO_BLANK_LINE,
        ) {
            return Err(ErrorCode::MalformedMultipart);
        }
        if part.is_message()
            || part
                .content_type()
                .is_some_and(|ct| ct.ctype().eq_ignore_ascii_case("message"))
        {
            return Err(ErrorCode::UnexpectedNestedMessage);
        }
        if part.is_multipart() || is_multipart_type(&part) {
            if !part.is_multipart() {
                return Err(ErrorCode::MalformedMultipart);
            }
            match part.content_type().and_then(|ct| ct.attribute("boundary")) {
                Some(b) if valid_boundary(b) => {}
                _ => return Err(ErrorCode::InvalidBoundary),
            }
            if part.children().len() == 0 {
                return Err(ErrorCode::NoParts);
            }
            continue;
        }
        leaf_count += 1;
        check_leaf(&part, &mut warnings)?;
        if is_type(&part, "text", "html") {
            html_part_count += 1;
        }
    }

    if html_part_count == 0 {
        return Err(ErrorCode::NoHtmlPart);
    }
    let children: Vec<MessagePart<'_>> = top.children().collect();
    if children.is_empty() {
        return Err(ErrorCode::NoParts);
    }
    let (root, rule) = match ct.attribute("start") {
        Some(start) => {
            let wanted = start.trim().trim_start_matches('<').trim_end_matches('>');
            let mut found = children
                .iter()
                .filter(|c| c.content_id().is_some_and(|id| id.trim() == wanted));
            let first = found.next().ok_or(ErrorCode::StartNotFound)?;
            if found.next().is_some() {
                return Err(ErrorCode::StartAmbiguous);
            }
            (*first, RootRule::StartParameter)
        }
        None => (children[0], RootRule::FirstPart),
    };
    if !is_type(&root, "text", "html") {
        return Err(ErrorCode::RootNotHtml);
    }
    if let Some(ty) = ct.attribute("type") {
        if !ty.trim().eq_ignore_ascii_case("text/html") {
            push(&mut warnings, "related_type_is_not_text_html");
        }
    }

    // mail-parser turns bytes that are not UTF-8 into U+FFFD in its parsed value, so the raw bytes
    // are checked first: such a value is dropped, never kept with replacement characters.
    let content_location_utf8 = root.headers().get("Content-Location").is_none_or(|h| {
        header_text(
            h.raw_value(),
            "content_location_invalid_utf8_dropped",
            &mut warnings,
        )
        .is_some()
    });
    let content_location = checked_header(
        root.content_location().filter(|_| content_location_utf8),
        limits.max_location_bytes,
        HeaderValue::ContentLocation,
        &mut warnings,
    );
    let snapshot_raw = message
        .headers()
        .get("Snapshot-Content-Location")
        .map(|h| h.raw_value());
    let snapshot_location = checked_header(
        snapshot_raw.and_then(|raw| {
            header_text(raw, "snapshot_location_invalid_utf8_dropped", &mut warnings)
        }),
        limits.max_location_bytes,
        HeaderValue::SnapshotLocation,
        &mut warnings,
    );
    match (&content_location, &snapshot_location) {
        (Some(a), Some(b)) if a != b => push(
            &mut warnings,
            "root_location_differs_from_snapshot_location",
        ),
        (None, Some(_)) => push(&mut warnings, "root_has_no_content_location"),
        _ => {}
    }
    if let Some(loc) = &content_location {
        let same = children
            .iter()
            .filter(|c| {
                is_type(c, "text", "html") && c.content_location().is_some_and(|l| l.trim() == loc)
            })
            .count();
        if same > 1 {
            push(&mut warnings, "several_html_parts_share_the_root_location");
        }
    }

    // Size before decoding: `decoded_len` measures base64 and quoted-printable without decoding.
    if root.decoded_len() > limits.max_html_bytes {
        return Err(ErrorCode::HtmlTooLarge);
    }
    let transfer_encoding = transfer_encoding(&root)?;
    let (html, problems) = root.decoded_checked();
    if !problems.is_empty() {
        return Err(match transfer_encoding {
            TransferEncoding::Base64 => ErrorCode::InvalidBase64,
            TransferEncoding::QuotedPrintable => ErrorCode::InvalidQuotedPrintable,
            _ => ErrorCode::InvalidTransferEncoding,
        });
    }
    if html.len() > limits.max_html_bytes {
        return Err(ErrorCode::HtmlTooLarge);
    }
    let mime_charset = checked_header(
        root.content_type().and_then(|ct| ct.attribute("charset")),
        limits.max_charset_label_bytes,
        HeaderValue::MimeCharset,
        &mut warnings,
    );
    if html_part_count > 1 {
        push(&mut warnings, "secondary_html_parts_not_extracted");
    }
    Ok(Root {
        html: html.into_owned(),
        _input: std::marker::PhantomData,
        part_id: root.id() as usize,
        rule,
        transfer_encoding,
        mime_charset,
        content_location,
        snapshot_location,
        part_count,
        leaf_count,
        html_part_count,
        warnings,
    })
}
