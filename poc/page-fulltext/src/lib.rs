//! PoC: search text and metadata from Chrome-made MHTML (PageVault full-text search, Slice 1).
//!
//! Not product code. Reads one local file; no network, no script execution, no Vault access.
//!
//! ```text
//! file ──size check──▶ bytes ──mail-parser + strict checks──▶ root text/html part (transfer-decoded)
//!      ──BOM / MIME charset / <meta> prescan──▶ encoding decision
//!      ──▶ [convert ≤64 KiB to UTF-8 ──▶ html5ever tokenizer ──▶ tokens]  repeated, no DOM
//!      ──▶ body text (separated)  +  metadata (title, description, keywords, og:*, article:tag)
//! ```

pub mod charset;
pub mod html;
pub mod index;
pub mod limits;
pub mod metadata;
pub mod mime;
pub mod quality;

use limits::{ErrorCode, Limits};
use serde::Serialize;
use std::{
    fs::File,
    io::Read,
    path::Path,
    time::{Duration, Instant},
};

/// Version 2: tokenizer extractor (no DOM); timings and token counters changed.
pub const OUTPUT_SCHEMA: &str = "memocampus.page-fulltext.poc/2";
pub const EXTRACTOR: &str = "html5ever-tokenizer";

#[derive(Debug, Serialize)]
pub struct Source {
    pub format: &'static str,
    pub input_bytes: usize,
    /// Every MIME part, the top-level message and containers included.
    pub mime_part_count: usize,
    pub mime_leaf_count: usize,
    pub html_part_count: usize,
    pub selected_part: SelectedPart,
    /// The `Snapshot-Content-Location` header (Chrome: the page URL). Untrusted text.
    pub snapshot_location: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SelectedPart {
    /// Part id in document order (0 is the top-level message, 1 the first child ...).
    pub index: usize,
    pub rule: mime::RootRule,
    pub content_type: &'static str,
    pub transfer_encoding: mime::TransferEncoding,
    pub content_location: Option<String>,
    pub decoded_bytes: usize,
}

#[derive(Debug, Serialize)]
pub struct Content {
    pub body_text: String,
    pub character_count: usize,
    pub utf8_bytes: usize,
}

#[derive(Debug, Serialize)]
pub struct EncodingInfo {
    pub name: &'static str,
    pub source: charset::EncodingSource,
    pub mime_charset: Option<String>,
    pub malformed_sequences_replaced: bool,
}

#[derive(Debug, Serialize)]
pub struct Timings {
    pub mime_us: u128,
    pub charset_detect_us: u128,
    /// Charset conversion, tokenizing and extraction together (they run interleaved).
    pub decode_tokenize_extract_us: u128,
}

#[derive(Debug, Serialize)]
pub struct Diagnostics {
    pub extractor: &'static str,
    pub encoding: EncodingInfo,
    pub has_body_element: bool,
    pub excluded_subtrees: usize,
    pub hidden_subtrees: usize,
    pub dropped_control_characters: usize,
    pub tokens: html::TokenStats,
    /// Codes only; a warning never carries page text.
    pub warnings: Vec<&'static str>,
    pub timings: Timings,
}

#[derive(Debug, Serialize)]
pub struct Extraction {
    pub schema: &'static str,
    pub status: &'static str,
    pub source: Source,
    pub metadata: metadata::Metadata,
    pub content: Content,
    pub diagnostics: Diagnostics,
}

fn micros(d: Duration) -> u128 {
    d.as_micros()
}

/// Extraction from bytes already in memory. Never panics on any input (a panic inside a library
/// is caught and reported as `internal_panic`).
pub fn extract(input: &[u8], limits: &Limits) -> Result<Extraction, ErrorCode> {
    std::panic::catch_unwind(|| extract_inner(input, limits))
        .unwrap_or(Err(ErrorCode::InternalPanic))
}

fn extract_inner(input: &[u8], limits: &Limits) -> Result<Extraction, ErrorCode> {
    let t0 = Instant::now();
    let root = mime::select_root(input, limits)?;
    let t1 = Instant::now();
    let detected = charset::detect(&root.html, root.mime_charset.as_deref())?;
    let t2 = Instant::now();
    let body = html::extract(
        detected.encoding,
        &root.html[detected.body_offset..],
        limits,
    )?;
    let t3 = Instant::now();
    let mut warnings = root.warnings.clone();
    for &w in detected.warnings.iter().chain(&body.warnings) {
        metadata::warn(&mut warnings, w);
    }
    if !body.has_body {
        warnings.push("no_body_element");
    }
    if body.text.is_empty() {
        warnings.push("empty_body_text");
    }
    if body.dropped_controls > 0 {
        warnings.push("control_characters_removed");
    }
    let character_count = body.text.chars().count();
    let utf8_bytes = body.text.len();
    Ok(Extraction {
        schema: OUTPUT_SCHEMA,
        status: "success",
        source: Source {
            format: "mhtml",
            input_bytes: input.len(),
            mime_part_count: root.part_count,
            mime_leaf_count: root.leaf_count,
            html_part_count: root.html_part_count,
            selected_part: SelectedPart {
                index: root.part_id,
                rule: root.rule,
                content_type: "text/html",
                transfer_encoding: root.transfer_encoding,
                content_location: root.content_location,
                decoded_bytes: root.html.len(),
            },
            snapshot_location: root.snapshot_location,
        },
        metadata: body.metadata,
        content: Content {
            body_text: body.text,
            character_count,
            utf8_bytes,
        },
        diagnostics: Diagnostics {
            extractor: EXTRACTOR,
            encoding: EncodingInfo {
                name: detected.encoding.name(),
                source: detected.source,
                mime_charset: root.mime_charset,
                malformed_sequences_replaced: body.malformed,
            },
            has_body_element: body.has_body,
            excluded_subtrees: body.excluded_subtrees,
            hidden_subtrees: body.hidden_subtrees,
            dropped_control_characters: body.dropped_controls,
            tokens: body.stats,
            warnings,
            timings: Timings {
                mime_us: micros(t1 - t0),
                charset_detect_us: micros(t2 - t1),
                decode_tokenize_extract_us: micros(t3 - t2),
            },
        },
    })
}

/// Reads a local file: refuses anything that is not a regular file or is above the input limit
/// before reading, and never reads more than the limit plus one byte.
pub fn read_input(path: &Path, limits: &Limits) -> Result<Vec<u8>, ErrorCode> {
    let meta = std::fs::metadata(path).map_err(|_| ErrorCode::InputUnreadable)?;
    if !meta.is_file() {
        return Err(ErrorCode::NotAFile);
    }
    if meta.len() > limits.max_input_bytes as u64 {
        return Err(ErrorCode::InputTooLarge);
    }
    let file = File::open(path).map_err(|_| ErrorCode::InputUnreadable)?;
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.take(limits.max_input_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ErrorCode::InputUnreadable)?;
    if bytes.len() > limits.max_input_bytes {
        return Err(ErrorCode::InputTooLarge);
    }
    Ok(bytes)
}
