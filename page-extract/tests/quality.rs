//! Quality gate verdicts (Issue #71 S1).
//!
//! In the PoC the gate was tested only through the SQLite index (`poc/page-fulltext/tests/
//! search.rs`, the `g_*` tests), which is not part of this crate. These tests take the same inputs
//! and check the same verdicts and numbers, measured exactly as the PoC's `Index::register` does
//! (`poc/page-fulltext/src/index.rs`, the lines that call `quality::measure_with_removed`,
//! `quality::measure`, `judge` and `judge_title`). What the index does with a verdict (status
//! names, keeping the previous body) is not tested here: it belongs to the index. Synthetic and
//! Chrome-made fixture inputs only.

use page_extract::{
    extract,
    limits::{ErrorCode, Limits},
    quality::{self, GateConfig, TextQuality, Verdict},
    Extraction,
};
use std::path::PathBuf;

const BOUNDARY: &str = "----MultipartBoundary--SLICE2----";

fn mhtml(part_headers: &str, html: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "From: <Saved by Blink>\r\nSnapshot-Content-Location: https://synthetic.test/p\r\nSubject: synthetic\r\nMIME-Version: 1.0\r\nContent-Type: multipart/related;\r\n\ttype=\"text/html\";\r\n\tboundary=\"{BOUNDARY}\"\r\n\r\n\r\n--{BOUNDARY}\r\n{part_headers}\r\nContent-Location: https://synthetic.test/p\r\n\r\n"
    )
    .into_bytes();
    out.extend_from_slice(html);
    out.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    out
}

fn extracted(bytes: &[u8]) -> Result<Extraction, ErrorCode> {
    extract(bytes, &Limits::default())
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/chrome")
            .join(name),
    )
    .unwrap()
}

fn replace(bytes: &[u8], from: &str, to: &str) -> Vec<u8> {
    let s = String::from_utf8_lossy(bytes);
    assert!(s.contains(from));
    let i = bytes
        .windows(from.len())
        .position(|w| w == from.as_bytes())
        .unwrap();
    [&bytes[..i], to.as_bytes(), &bytes[i + from.len()..]].concat()
}

struct Gated {
    body: TextQuality,
    body_verdict: Verdict,
    title: TextQuality,
    title_verdict: Verdict,
}

/// The measurement of the PoC's `Index::register`, unchanged.
fn gate(x: &Extraction) -> Gated {
    let gate = GateConfig::default();
    let body = &x.content.body_text;
    let title = x.metadata.title.as_deref().unwrap_or("");
    let decoded_with = quality::encoding_named(x.diagnostics.encoding.name);
    let body_q =
        quality::measure_with_removed(body, decoded_with, x.diagnostics.dropped_control_characters);
    let title_q = quality::measure(title, decoded_with);
    let body_verdict = gate.judge(&body_q);
    let title_verdict = gate.judge_title(&title_q, body_verdict);
    Gated {
        body: body_q,
        body_verdict,
        title: title_q,
        title_verdict,
    }
}

fn indexable(v: Verdict) -> bool {
    matches!(v, Verdict::Clean | Verdict::Suspect)
}

#[test]
fn q_provisional_thresholds_are_the_pocs() {
    let g = GateConfig::default();
    assert_eq!(
        (
            g.min_replacement_chars,
            g.max_replacement_per_mille,
            g.min_non_ascii_chars,
            g.max_misdecoded_per_mille,
            g.min_removed_controls,
            g.max_removed_control_per_mille,
        ),
        (4, 10, 16, 300, 8, 50)
    );
}

#[test]
fn q_thresholds_are_inclusive_and_need_both_count_and_rate() {
    let g = GateConfig::default();
    let q = |replacement_chars, replacement_per_mille| TextQuality {
        chars: 1000,
        replacement_chars,
        replacement_per_mille,
        ..TextQuality::default()
    };
    assert_eq!(g.judge(&q(4, 10)), Verdict::Garbled);
    assert_eq!(g.judge(&q(3, 10)), Verdict::Suspect);
    assert_eq!(g.judge(&q(4, 9)), Verdict::Suspect);
    assert_eq!(g.judge(&q(0, 0)), Verdict::Clean);
    assert_eq!(g.judge(&TextQuality::default()), Verdict::Empty);
}

// From g_end_to_end_hokke_through_quoted_printable.
#[test]
fn q_hokke_through_quoted_printable_is_clean() {
    let html = b"<html><head><meta charset=3D\"utf-8\"><title>=F0=A9=B8=BD=E3=81=AE=E8=A9=B1</title></head><body><p>=E4=BB=8A=E6=97=A5=E3=81=AF=F0=A9=\r\n=B8=BD=E3=82=92=E7=84=BC=E3=81=84=E3=81=9F=E3=80=82</p></body></html>";
    let x = extracted(&mhtml(
        "Content-Type: text/html\r\nContent-Transfer-Encoding: quoted-printable",
        html,
    ))
    .unwrap();
    assert_eq!(x.content.body_text, "今日は𩸽を焼いた。");
    assert_eq!(x.metadata.title.as_deref(), Some("𩸽の話"));
    let g = gate(&x);
    assert_eq!(
        (g.body_verdict, g.title_verdict),
        (Verdict::Clean, Verdict::Clean)
    );
    // Unicode scalar values: 𩸽 (U+29E3D, 4 UTF-8 bytes) is one character.
    assert_eq!((g.body.chars, g.title.chars), (9, 3));
    assert!(x.diagnostics.warnings.is_empty());
}

// From g_clean_pages_and_pages_with_warnings.
#[test]
fn q_clean_page_and_page_with_warnings() {
    let x = extracted(&fixture("ja-article.mhtml")).unwrap();
    let g = gate(&x);
    assert_eq!(
        (g.body_verdict, g.title_verdict),
        (Verdict::Clean, Verdict::Clean)
    );
    assert!(x.diagnostics.warnings.is_empty());

    let x = extracted(&mhtml(
        "Content-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: 7bit",
        "<html><head><title>警告</title></head><body><p>本文\u{1}です</p></body></html>".as_bytes(),
    ))
    .unwrap();
    assert!(x
        .diagnostics
        .warnings
        .contains(&"control_characters_removed"));
    let g = gate(&x);
    // One removed control character: below the threshold, so indexed (the PoC's status was
    // "ok_with_warnings").
    assert_eq!(g.body.removed_controls, 1);
    assert_eq!(g.body_verdict, Verdict::Suspect);
    assert!(indexable(g.body_verdict) && indexable(g.title_verdict));
    assert_eq!(x.content.body_text, "本文です");
}

// From g_empty_body_keeps_the_title.
#[test]
fn q_empty_body_with_a_title() {
    let x = extracted(&mhtml(
        "Content-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: 8bit",
        "<html><head><title>題名だけのページ</title></head><body><script>x()</script></body></html>".as_bytes(),
    ))
    .unwrap();
    assert!(x.content.body_text.is_empty());
    let g = gate(&x);
    assert_eq!(
        (g.body_verdict, g.title_verdict),
        (Verdict::Empty, Verdict::Clean)
    );
}

// From g_garbled_text_is_not_indexed_and_quality_is_kept.
#[test]
fn q_garbled_texts() {
    // Shift_JIS page declared as UTF-8: replacement characters; nothing indexable.
    let sjis = replace(
        &fixture("sjis-news.mhtml"),
        "charset=3DShift_JIS",
        "charset=3Dutf-8",
    );
    let x = extracted(&sjis).unwrap();
    assert!(x.diagnostics.encoding.malformed_sequences_replaced);
    let g = gate(&x);
    assert_eq!(g.body_verdict, Verdict::Garbled);
    assert!(!indexable(g.title_verdict));
    assert!(g.body.replacement_chars > 0);
    assert!(g.body.replacement_per_mille >= 10);

    // UTF-8 page declared as windows-1252 (mojibake without U+FFFD), ASCII title kept.
    let html = "<html><head><meta charset=\"windows-1252\"><title>ASCII title</title></head><body><p>日本語の本文がここにあります。漢字とひらがなとカタカナ。</p></body></html>";
    let x = extracted(&mhtml(
        "Content-Type: text/html\r\nContent-Transfer-Encoding: 8bit",
        html.as_bytes(),
    ))
    .unwrap();
    assert_eq!(x.diagnostics.encoding.name, "windows-1252");
    let g = gate(&x);
    assert_eq!(
        (g.body_verdict, g.title_verdict),
        (Verdict::Garbled, Verdict::Clean)
    );
    assert_eq!(g.body.misdecoded_as, Some("windows-1252"));
    assert!(g.body.removed_control_per_mille >= 50);

    // The same mojibake with a Japanese title: the title is not trusted either.
    let x = extracted(&mhtml(
        "Content-Type: text/html\r\nContent-Transfer-Encoding: 8bit",
        html.replace("ASCII title", "日本語の題名").as_bytes(),
    ))
    .unwrap();
    let g = gate(&x);
    assert_eq!(
        (g.body_verdict, g.title_verdict),
        (Verdict::Garbled, Verdict::Garbled)
    );
}

// From g_genuine_legacy_encoded_page_is_clean.
#[test]
fn q_genuine_legacy_encoded_page_is_clean() {
    let x = extracted(&fixture("sjis-news.mhtml")).unwrap();
    assert_eq!(x.diagnostics.encoding.name, "Shift_JIS");
    let g = gate(&x);
    assert_eq!(g.body_verdict, Verdict::Clean);
    assert!(matches!(g.title_verdict, Verdict::Clean | Verdict::Empty));
    assert!(x.diagnostics.warnings.is_empty());
}

// From g_failed_or_garbled_re_extraction_keeps_the_indexed_page (the extraction half: which
// re-extractions the gate refuses; keeping the previous body is the index's part).
#[test]
fn q_wrong_charset_re_extraction_is_garbled() {
    let garbled = extracted(&replace(
        &fixture("ja-article.mhtml"),
        "charset=3DUTF-8",
        "charset=3Dshift_jis",
    ))
    .unwrap();
    let g = gate(&garbled);
    assert_eq!(g.body_verdict, Verdict::Garbled);
    assert!(!indexable(g.title_verdict));
}
