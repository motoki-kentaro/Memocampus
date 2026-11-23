//! Resource limits and error codes of the extractor.
//!
//! The limits are PoC values, not a product specification. Every one of them is enforced before the
//! work it bounds is allocated where the libraries allow it (see README, "Resource limits").

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Whole MHTML file. Equal to the product's `MAX_PAGE_BYTES` (128 MiB).
    pub max_input_bytes: usize,
    /// The selected HTML part after transfer decoding (before charset conversion).
    pub max_html_bytes: usize,
    /// The HTML after charset conversion, UTF-8 bytes, counted while decoding (a legacy encoding
    /// or replacement characters can make UTF-8 up to 3 times larger than the source bytes).
    pub max_decoded_html_bytes: usize,
    /// Extracted body text, UTF-8 bytes.
    pub max_text_bytes: usize,
    /// MIME parts of every level, the top-level message included.
    pub max_parts: usize,
    /// Nested MIME containers, the top-level message included.
    pub max_depth: usize,
    /// One metadata value (title, description, one keyword list, one tag ...), UTF-8 bytes.
    pub max_metadata_bytes: usize,
    /// `Content-Location` and `Snapshot-Content-Location` values kept in the output, bytes.
    pub max_location_bytes: usize,
    /// The MIME `charset` parameter of the root part, bytes.
    pub max_charset_label_bytes: usize,
    /// Elements tracked on the extractor's open-element stack (not a DOM: names and flags only).
    pub max_open_elements: usize,
    /// Bytes of transfer-decoded HTML converted and fed to the tokenizer per step.
    pub tokenizer_chunk_bytes: usize,
}

pub const MIB: usize = 1024 * 1024;

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_input_bytes: 128 * MIB,
            max_html_bytes: 16 * MIB,
            max_decoded_html_bytes: 32 * MIB,
            max_text_bytes: 8 * MIB,
            max_parts: 1024,
            max_depth: 16,
            max_metadata_bytes: 64 * 1024,
            max_location_bytes: 8 * 1024,
            max_charset_label_bytes: 64,
            max_open_elements: 1024,
            tokenizer_chunk_bytes: 64 * 1024,
        }
    }
}

/// Why an input was refused. Codes only: an error never carries text from the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    NotAFile,
    InputUnreadable,
    InputTooLarge,
    NotMime,
    NotMultipartRelated,
    InvalidBoundary,
    MalformedMultipart,
    UnterminatedMultipart,
    NoParts,
    TooManyParts,
    NestingTooDeep,
    UnexpectedNestedMessage,
    InvalidTransferEncoding,
    InvalidBase64,
    InvalidQuotedPrintable,
    StartNotFound,
    StartAmbiguous,
    RootNotHtml,
    NoHtmlPart,
    HtmlTooLarge,
    DecodedHtmlTooLarge,
    UnsupportedCharset,
    TextTooLarge,
    MetadataTooLarge,
    InternalPanic,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotAFile => "not_a_file",
            Self::InputUnreadable => "input_unreadable",
            Self::InputTooLarge => "input_too_large",
            Self::NotMime => "not_mime",
            Self::NotMultipartRelated => "not_multipart_related",
            Self::InvalidBoundary => "invalid_boundary",
            Self::MalformedMultipart => "malformed_multipart",
            Self::UnterminatedMultipart => "unterminated_multipart",
            Self::NoParts => "no_parts",
            Self::TooManyParts => "too_many_parts",
            Self::NestingTooDeep => "nesting_too_deep",
            Self::UnexpectedNestedMessage => "unexpected_nested_message",
            Self::InvalidTransferEncoding => "invalid_transfer_encoding",
            Self::InvalidBase64 => "invalid_base64",
            Self::InvalidQuotedPrintable => "invalid_quoted_printable",
            Self::StartNotFound => "start_not_found",
            Self::StartAmbiguous => "start_ambiguous",
            Self::RootNotHtml => "root_not_html",
            Self::NoHtmlPart => "no_html_part",
            Self::HtmlTooLarge => "html_too_large",
            Self::DecodedHtmlTooLarge => "decoded_html_too_large",
            Self::UnsupportedCharset => "unsupported_charset",
            Self::TextTooLarge => "text_too_large",
            Self::MetadataTooLarge => "metadata_too_large",
            Self::InternalPanic => "internal_panic",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::error::Error for ErrorCode {}
