//! Slice 1 tests. Synthetic MHTML is built here; `tests/fixtures/chrome/*.mhtml` were produced by
//! Chrome from synthetic local pages (tools/chrome-capture.mjs). No real page or Vault is used.

use page_fulltext::{extract, limits::ErrorCode, limits::Limits, read_input, Extraction};
use std::{fs, path::PathBuf, process::Command};

// ---------------------------------------------------------------- builders

struct Part {
    headers: String,
    body: Vec<u8>,
}

fn part(headers: &str, body: impl Into<Vec<u8>>) -> Part {
    Part {
        headers: headers.to_owned(),
        body: body.into(),
    }
}

const B: &str = "----MultipartBoundary--TEST----";

fn related_with(top_extra: &str, boundary: &str, parts: &[Part], close: bool) -> Vec<u8> {
    let mut out = format!(
        "From: <Saved by Blink>\r\nSnapshot-Content-Location: https://synthetic.test/page\r\nSubject: synthetic\r\nMIME-Version: 1.0\r\nContent-Type: multipart/related;\r\n\ttype=\"text/html\";\r\n\tboundary=\"{boundary}\"{top_extra}\r\n\r\n\r\n"
    )
    .into_bytes();
    for p in parts {
        out.extend_from_slice(
            format!("--{boundary}\r\n{}\r\n\r\n", p.headers.trim_end()).as_bytes(),
        );
        out.extend_from_slice(&p.body);
        out.extend_from_slice(b"\r\n");
    }
    if close {
        out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    }
    out
}

fn related(parts: &[Part]) -> Vec<u8> {
    related_with("", B, parts, true)
}

fn qp(bytes: &[u8]) -> Vec<u8> {
    // Minimal quoted-printable encoder with soft line breaks every 72 characters.
    let mut out = Vec::new();
    let mut line = 0;
    for &b in bytes {
        let enc: Vec<u8> = if b == b'\n' {
            line = 0;
            out.extend_from_slice(b"\r\n");
            continue;
        } else if b == b'=' || b >= 0x7f || (b < 0x20 && b != b'\t') {
            format!("={b:02X}").into_bytes()
        } else {
            vec![b]
        };
        if line + enc.len() > 72 {
            out.extend_from_slice(b"=\r\n");
            line = 0;
        }
        line += enc.len();
        out.extend_from_slice(&enc);
    }
    out
}

fn b64(bytes: &[u8]) -> Vec<u8> {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    for (i, chunk) in bytes.chunks(3).enumerate() {
        if i > 0 && i % 19 == 0 {
            out.extend_from_slice(b"\r\n");
        }
        let n = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(A[(n >> 18) as usize & 63]);
        out.push(A[(n >> 12) as usize & 63]);
        out.push(if chunk.len() > 1 {
            A[(n >> 6) as usize & 63]
        } else {
            b'='
        });
        out.push(if chunk.len() > 2 {
            A[n as usize & 63]
        } else {
            b'='
        });
    }
    out
}

fn html_qp(html: &str) -> Part {
    part(
        "Content-Type: text/html\r\nContent-ID: <frame-ROOT@mhtml.blink>\r\nContent-Transfer-Encoding: quoted-printable\r\nContent-Location: https://synthetic.test/page",
        qp(html.as_bytes()),
    )
}

fn page(body: &str) -> String {
    format!("<!DOCTYPE html><html><head><meta http-equiv=\"Content-Type\" content=\"text/html; charset=UTF-8\"><title>T</title></head><body>{body}</body></html>")
}

fn ok(bytes: &[u8]) -> Extraction {
    extract(bytes, &Limits::default()).unwrap_or_else(|e| panic!("expected success, got {e}"))
}

fn err(bytes: &[u8]) -> ErrorCode {
    match extract(bytes, &Limits::default()) {
        Ok(_) => panic!("expected an error"),
        Err(e) => e,
    }
}

fn text_of(body: &str) -> String {
    ok(&related(&[html_qp(&page(body))])).content.body_text
}

fn fixture(name: &str) -> Vec<u8> {
    fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/chrome")
            .join(name),
    )
    .unwrap()
}

// ---------------------------------------------------------------- A. MIME

#[test]
fn a_valid_related_with_resources() {
    let png = b64(b"\x89PNG\r\n\x1a\nBINARY-CANARY-IMAGE");
    let x = ok(&related(&[
        html_qp(&page("<p>本文テキスト</p>")),
        part("Content-Type: image/png\r\nContent-Transfer-Encoding: base64\r\nContent-Location: https://synthetic.test/a.png", png),
        part("Content-Type: text/css\r\nContent-Transfer-Encoding: quoted-printable\r\nContent-Location: https://synthetic.test/a.css", qp(b"p::after{content:'CSS-CANARY'}")),
        part("Content-Type: application/javascript\r\nContent-Transfer-Encoding: 8bit", "var x='JS-PART-CANARY';"),
    ]));
    assert_eq!(x.content.body_text, "本文テキスト");
    assert_eq!(x.source.selected_part.index, 1);
    assert_eq!(x.source.mime_leaf_count, 4);
    assert_eq!(x.source.html_part_count, 1);
    for canary in ["BINARY-CANARY", "CSS-CANARY", "JS-PART-CANARY", "PNG"] {
        assert!(!x.content.body_text.contains(canary));
    }
}

#[test]
fn a_base64_quoted_printable_and_8bit_roots() {
    let html = page("<p>エンコード確認 = equals</p>");
    for p in [
        part(
            "Content-Type: text/html\r\nContent-Transfer-Encoding: base64",
            b64(html.as_bytes()),
        ),
        part(
            "Content-Type: text/html\r\nContent-Transfer-Encoding: quoted-printable",
            qp(html.as_bytes()),
        ),
        part(
            "Content-Type: text/html\r\nContent-Transfer-Encoding: 8bit",
            html.as_bytes(),
        ),
        part(
            "Content-Type: text/html\r\nContent-Transfer-Encoding: binary",
            html.as_bytes(),
        ),
    ] {
        assert_eq!(
            ok(&related(&[p])).content.body_text,
            "エンコード確認 = equals"
        );
    }
}

#[test]
fn a_soft_line_breaks_join_words() {
    let raw = b"<html><body><p>abc=\r\ndef =E6=97=A5=E6=9C=AC</p></body></html>".to_vec();
    let x = ok(&related(&[part(
        "Content-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: quoted-printable",
        raw,
    )]));
    assert_eq!(x.content.body_text, "abcdef 日本");
}

#[test]
fn a_start_parameter_selects_the_root_and_unknown_start_fails() {
    let parts = [
        part("Content-Type: text/html\r\nContent-ID: <frame-OTHER@x>\r\nContent-Transfer-Encoding: 8bit", page("<p>WRONG-FRAME</p>")),
        part("Content-Type: text/html\r\nContent-ID: <root@x>\r\nContent-Transfer-Encoding: 8bit", page("<p>RIGHT-ROOT</p>")),
    ];
    let x = ok(&related_with(";\r\n\tstart=\"<root@x>\"", B, &parts, true));
    assert_eq!(x.content.body_text, "RIGHT-ROOT");
    assert_eq!(x.source.selected_part.index, 2);
    assert!(x
        .diagnostics
        .warnings
        .contains(&"secondary_html_parts_not_extracted"));
    assert_eq!(
        page_fulltext::extract(
            &related_with(";\r\n\tstart=\"<missing@x>\"", B, &parts, true),
            &Limits::default()
        )
        .unwrap_err(),
        ErrorCode::StartNotFound
    );
    let dup = [
        part("Content-Type: text/html\r\nContent-ID: <root@x>", page("a")),
        part("Content-Type: text/html\r\nContent-ID: <root@x>", page("b")),
    ];
    assert_eq!(
        page_fulltext::extract(
            &related_with(";\r\n\tstart=\"<root@x>\"", B, &dup, true),
            &Limits::default()
        )
        .unwrap_err(),
        ErrorCode::StartAmbiguous
    );
}

#[test]
fn a_first_part_not_html_is_refused_never_guessed() {
    let parts = [
        part("Content-Type: text/css", "p{}"),
        part("Content-Type: text/html", page("<p>HTML</p>")),
    ];
    assert_eq!(err(&related(&parts)), ErrorCode::RootNotHtml);
}

#[test]
fn a_no_html_part() {
    assert_eq!(
        err(&related(&[
            part("Content-Type: text/css", "p{}"),
            part(
                "Content-Type: image/png\r\nContent-Transfer-Encoding: base64",
                b64(b"x")
            )
        ])),
        ErrorCode::NoHtmlPart
    );
}

#[test]
fn a_boundary_problems() {
    let html = || html_qp(&page("<p>x</p>"));
    // No boundary parameter.
    let mut no_param = related(&[html()]);
    let s = String::from_utf8(no_param.clone())
        .unwrap()
        .replace(&format!(";\r\n\tboundary=\"{B}\""), "");
    no_param = s.into_bytes();
    assert_eq!(err(&no_param), ErrorCode::InvalidBoundary);
    // Boundary longer than 70 characters.
    let long = "b".repeat(71);
    assert_eq!(
        err(&related_with("", &long, &[html()], true)),
        ErrorCode::InvalidBoundary
    );
    // The body uses another boundary than the declared one.
    let mismatched = String::from_utf8(related(&[html()])).unwrap().replacen(
        &format!("boundary=\"{B}\""),
        "boundary=\"declared-but-unused\"",
        1,
    );
    // mail-parser parses it (flagging the parts); the flags make it an error here.
    assert_eq!(err(mismatched.as_bytes()), ErrorCode::MalformedMultipart);
    // No close delimiter.
    assert_eq!(
        err(&related_with("", B, &[html()], false)),
        ErrorCode::UnterminatedMultipart
    );
}

#[test]
fn a_invalid_transfer_encodings() {
    let bad64 = part(
        "Content-Type: text/html\r\nContent-Transfer-Encoding: base64",
        b"PGh0bWw+!!!not*base64".to_vec(),
    );
    assert_eq!(err(&related(&[bad64])), ErrorCode::InvalidBase64);
    let truncated = part(
        "Content-Type: text/html\r\nContent-Transfer-Encoding: base64",
        b"PGh0bWw".to_vec(),
    );
    assert_eq!(err(&related(&[truncated])), ErrorCode::InvalidBase64);
    let bad_image = part(
        "Content-Type: image/png\r\nContent-Transfer-Encoding: base64",
        b"iVBORw0K$$$$".to_vec(),
    );
    assert_eq!(
        err(&related(&[html_qp(&page("x")), bad_image])),
        ErrorCode::InvalidBase64
    );
    let badqp = part(
        "Content-Type: text/html\r\nContent-Transfer-Encoding: quoted-printable",
        b"<p>a=ZZb</p>".to_vec(),
    );
    assert_eq!(err(&related(&[badqp])), ErrorCode::InvalidQuotedPrintable);
    let unknown = part(
        "Content-Type: text/html\r\nContent-Transfer-Encoding: x-uuencode",
        page("x"),
    );
    assert_eq!(
        err(&related(&[unknown])),
        ErrorCode::InvalidTransferEncoding
    );
}

#[test]
fn a_part_count_limit() {
    let mut parts = vec![html_qp(&page("x"))];
    for i in 0..1100 {
        parts.push(part(
            &format!("Content-Type: text/css\r\nContent-Location: https://synthetic.test/{i}.css"),
            "p{}",
        ));
    }
    assert_eq!(err(&related(&parts)), ErrorCode::TooManyParts);
    // Exactly at a small configured limit passes, one above fails.
    let small = Limits {
        max_parts: 4,
        ..Limits::default()
    };
    let three = related(&[
        html_qp(&page("x")),
        part("Content-Type: text/css", "a"),
        part("Content-Type: text/css", "b"),
    ]);
    assert!(extract(&three, &small).is_ok());
    let four = related(&[
        html_qp(&page("x")),
        part("Content-Type: text/css", "a"),
        part("Content-Type: text/css", "b"),
        part("Content-Type: text/css", "c"),
    ]);
    assert_eq!(extract(&four, &small).unwrap_err(), ErrorCode::TooManyParts);
}

fn nested(depth: usize) -> Vec<u8> {
    // depth multipart/mixed levels inside the related root, an HTML part at the bottom.
    let mut inner = format!("Content-Type: text/html\r\n\r\n{}", page("<p>deep</p>"));
    for level in 0..depth {
        let b = format!("nested-{level}");
        inner = format!(
            "Content-Type: multipart/mixed; boundary=\"{b}\"\r\n\r\n--{b}\r\n{inner}\r\n--{b}--"
        );
    }
    let mut out = related(&[html_qp(&page("<p>root</p>"))]);
    let close = format!("--{B}--\r\n");
    let at = out.len() - close.len();
    out.truncate(at);
    out.extend_from_slice(format!("--{B}\r\n{inner}\r\n{close}").as_bytes());
    out
}

#[test]
fn a_nesting_depth_limit() {
    assert_eq!(ok(&nested(3)).content.body_text, "root");
    assert_eq!(err(&nested(20)), ErrorCode::NestingTooDeep);
}

#[test]
fn a_nested_message_and_non_mime_inputs() {
    let msg = part("Content-Type: message/rfc822", "Subject: x\r\n\r\nbody");
    assert_eq!(
        err(&related(&[html_qp(&page("x")), msg])),
        ErrorCode::UnexpectedNestedMessage
    );
    // A plain HTML file has no MIME header block at all: mail-parser refuses it (not_mime).
    assert!(matches!(
        err(page("<p>plain html file</p>").as_bytes()),
        ErrorCode::NotMime | ErrorCode::NotMultipartRelated
    ));
    assert_eq!(
        err(b"Content-Type: text/html\r\n\r\n<p>single part</p>"),
        ErrorCode::NotMultipartRelated
    );
    let mixed = String::from_utf8(related(&[html_qp(&page("x"))]))
        .unwrap()
        .replace("multipart/related", "multipart/mixed");
    assert_eq!(err(mixed.as_bytes()), ErrorCode::NotMultipartRelated);
    assert!(extract(b"", &Limits::default()).is_err());
}

// ---------------------------------------------------------------- B. HTML body

#[test]
fn b_languages_entities_emoji_and_separation() {
    assert_eq!(text_of("<p>東京都</p><p>江東区</p>"), "東京都\n江東区");
    assert_eq!(
        text_of("<p>Hello <b>world</b></p><p>English and 日本語 混在</p>"),
        "Hello world\nEnglish and 日本語 混在"
    );
    assert_eq!(
        text_of("<p>&lt;tag&gt; &amp; &copy; &#x1F980; &quot;q&quot;</p>"),
        "<tag> & © 🦀 \"q\""
    );
    assert_eq!(
        text_of("<p>  多い   空白\n\n改行\t</p>\n\n<p>&nbsp;次&#12288;段落</p>"),
        "多い 空白 改行\n次 段落"
    );
    assert_eq!(text_of("<p>行1<br>行2</p>"), "行1\n行2");
    assert_eq!(
        text_of("<ul><li>一</li><li>二</li></ul><h2>見出し</h2>"),
        "一\n二\n見出し"
    );
    assert_eq!(
        text_of(
            "<table><tr><th>A</th><th>B</th></tr><tr><td>セル1</td><td>セル2</td></tr></table>"
        ),
        "A B\nセル1 セル2"
    );
    assert_eq!(text_of("<p><b>bold</b><i>italic</i></p>"), "bolditalic");
}

#[test]
fn b_excluded_and_hidden_content() {
    let body = r#"<p>VISIBLE</p><script>SCRIPT</script><style>STYLE</style><noscript>NOSCRIPT</noscript>
        <template><p>TEMPLATE</p></template><svg><text>SVG</text></svg><canvas>CANVAS</canvas><!-- COMMENT -->
        <p hidden>HIDDEN</p><div aria-hidden="TRUE">ARIA</div><div style="color:red; DISPLAY : none !important">DISPLAY</div>
        <span style="visibility:hidden">VISIBILITY</span><iframe>IFRAME-FALLBACK</iframe><div aria-hidden="false">ARIA-FALSE-SHOWN</div>"#;
    let x = ok(&related(&[html_qp(&page(body))]));
    // Changed with the tokenizer extractor (report XIII): `aria-hidden="true"` hides from
    // assistive technology, not from the screen, so its text is searchable (ARIA). Inline
    // `visibility:hidden` is not excluded either, because a descendant can be visible again.
    // The DOM extractor of Slice 1 returned "VISIBLE\nARIA-FALSE-SHOWN".
    assert_eq!(
        x.content.body_text,
        "VISIBLE\nARIA\nVISIBILITY\nARIA-FALSE-SHOWN"
    );
    assert_eq!(x.diagnostics.hidden_subtrees, 2);
}

#[test]
fn b_metadata_is_not_body_text() {
    let html = "<html><head><title>TITLE-ONLY</title><meta name=description content=\"DESC-ONLY\"></head><body><p>本文</p></body></html>";
    let x = ok(&related(&[part(
        "Content-Type: text/html; charset=utf-8",
        html,
    )]));
    assert_eq!(x.content.body_text, "本文");
    assert_eq!(x.metadata.title.as_deref(), Some("TITLE-ONLY"));
    assert_eq!(x.content.character_count, 2);
}

// ---------------------------------------------------------------- C. metadata

#[test]
fn c_all_fields_case_and_multiplicity() {
    let head = r#"<TITLE>  題名 &amp;  タイトル </TITLE>
        <META NAME="Description" CONTENT="説明&lt;文&gt;"><meta name="description" content="second">
        <meta name="KEYWORDS" content="Rust, SQLite、全文検索 ， MHTML;; Rust">
        <meta PROPERTY="OG:Title" content="OG題名"><meta property="og:description" content="OG説明">
        <meta property="article:tag" content="タグ1"><meta property="article:tag" content="タグ2"><meta property="article:tag" content="タグ1">
        <meta property="article:tag" content="   ">"#;
    let html = format!(
        "<html><head>{head}</head><body><svg><title>SVG-TITLE</title></svg><p>x</p></body></html>"
    );
    let x = ok(&related(&[part(
        "Content-Type: text/html; charset=utf-8",
        html,
    )]));
    let m = &x.metadata;
    assert_eq!(m.title.as_deref(), Some("題名 & タイトル"));
    assert_eq!(m.description.as_deref(), Some("説明<文>"));
    assert_eq!(
        m.keywords.as_deref(),
        Some(
            &[
                "Rust".to_owned(),
                "SQLite".into(),
                "全文検索".into(),
                "MHTML".into()
            ][..]
        )
    );
    assert_eq!(m.og_title.as_deref(), Some("OG題名"));
    assert_eq!(m.og_description.as_deref(), Some("OG説明"));
    assert_eq!(m.article_tags, ["タグ1", "タグ2"]);
    for w in ["duplicate_meta_description", "empty_meta_article_tag"] {
        assert!(x.diagnostics.warnings.contains(&w), "{w}");
    }
}

#[test]
fn c_missing_values_are_absent_not_empty() {
    let x = ok(&related(&[part(
        "Content-Type: text/html; charset=utf-8",
        "<html><body><p>x</p></body></html>",
    )]));
    let m = &x.metadata;
    assert!(
        m.title.is_none()
            && m.description.is_none()
            && m.keywords.is_none()
            && m.og_title.is_none()
            && m.og_description.is_none()
    );
    assert!(m.article_tags.is_empty());
    let json = serde_json::to_value(&x).unwrap();
    assert!(json["metadata"]["title"].is_null() && json["metadata"]["keywords"].is_null());
    let empty = ok(&related(&[part("Content-Type: text/html; charset=utf-8", "<html><head><title> </title><meta name=description content=\"\"></head><body>x</body></html>")]));
    assert!(empty.metadata.title.is_none() && empty.metadata.description.is_none());
    assert!(empty.diagnostics.warnings.contains(&"empty_title"));
}

#[test]
fn c_charsets_declared_in_several_ways() {
    let sjis = |s: &str| encoding_rs::SHIFT_JIS.encode(s).0.into_owned();
    let body =
        "<html><head><title>日本語タイトル</title></head><body><p>シフトJIS本文</p></body></html>";
    // MIME charset only.
    let x = ok(&related(&[part(
        "Content-Type: text/html; charset=Shift_JIS\r\nContent-Transfer-Encoding: 8bit",
        sjis(body),
    )]));
    assert_eq!(
        (x.diagnostics.encoding.name, x.content.body_text.as_str()),
        ("Shift_JIS", "シフトJIS本文")
    );
    // <meta charset> only.
    let meta = format!("<meta charset=\"shift_jis\">{body}");
    let x = ok(&related(&[part(
        "Content-Type: text/html\r\nContent-Transfer-Encoding: 8bit",
        sjis(&meta),
    )]));
    assert_eq!(x.metadata.title.as_deref(), Some("日本語タイトル"));
    // EUC-JP via http-equiv, quoted-printable.
    let euc = encoding_rs::EUC_JP
        .encode(&format!(
            "<meta http-equiv=content-type content='text/html; charset=EUC-JP'>{body}"
        ))
        .0
        .into_owned();
    let x = ok(&related(&[part(
        "Content-Type: text/html\r\nContent-Transfer-Encoding: quoted-printable",
        qp(&euc),
    )]));
    assert_eq!(
        (x.diagnostics.encoding.name, x.content.body_text.as_str()),
        ("EUC-JP", "シフトJIS本文")
    );
    // MIME charset wins over <meta>.
    let x = ok(&related(&[part(
        "Content-Type: text/html; charset=utf-8",
        "<meta charset=shift_jis><body>ユーティーエフ</body>",
    )]));
    assert_eq!(x.content.body_text, "ユーティーエフ");
    // BOM wins over everything.
    let mut bom = b"\xEF\xBB\xBF".to_vec();
    bom.extend_from_slice("<meta charset=shift_jis><body>BOM優先</body>".as_bytes());
    assert_eq!(
        ok(&related(&[part(
            "Content-Type: text/html; charset=Shift_JIS\r\nContent-Transfer-Encoding: 8bit",
            bom
        )]))
        .content
        .body_text,
        "BOM優先"
    );
}

#[test]
fn c_unknown_or_unusable_charset_labels() {
    let x = ok(&related(&[part(
        "Content-Type: text/html; charset=x-no-such\r\nContent-Transfer-Encoding: 8bit",
        "<meta charset=also-unknown><body>既定UTF-8</body>",
    )]));
    assert_eq!(x.diagnostics.encoding.name, "UTF-8");
    for w in [
        "unknown_mime_charset_label_ignored",
        "unknown_meta_charset_label_ignored",
        "encoding_not_declared_utf8_assumed",
    ] {
        assert!(x.diagnostics.warnings.contains(&w), "{w}");
    }
    // A label that maps to the replacement encoding is refused.
    assert_eq!(
        err(&related(&[part(
            "Content-Type: text/html; charset=iso-2022-kr",
            "<body>x</body>"
        )])),
        ErrorCode::UnsupportedCharset
    );
    // <meta> naming UTF-16 means UTF-8 (HTML Standard).
    assert_eq!(
        ok(&related(&[part(
            "Content-Type: text/html",
            "<meta charset=utf-16><body>U16は8</body>"
        )]))
        .diagnostics
        .encoding
        .name,
        "UTF-8"
    );
}

// ---------------------------------------------------------------- D. security and limits

#[test]
fn d_input_size_limit_is_checked_before_reading() {
    let dir = std::env::temp_dir().join(format!("page-fulltext-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("too-large.mhtml");
    let file = fs::File::create(&path).unwrap();
    file.set_len(128 * 1024 * 1024 + 1).unwrap();
    drop(file);
    assert_eq!(
        read_input(&path, &Limits::default()).unwrap_err(),
        ErrorCode::InputTooLarge
    );
    fs::remove_file(&path).unwrap();
    assert_eq!(
        read_input(&dir, &Limits::default()).unwrap_err(),
        ErrorCode::NotAFile
    );
    fs::remove_dir(&dir).unwrap();
    let small = Limits {
        max_input_bytes: 100,
        ..Limits::default()
    };
    assert_eq!(
        extract(&related(&[html_qp(&page("x"))]), &small).unwrap_err(),
        ErrorCode::InputTooLarge
    );
}

#[test]
fn d_decoded_html_limit_real_value() {
    // 16 MiB + 1 decoded bytes in base64: refused before decoding (decoded_len).
    let html = vec![b'a'; 16 * 1024 * 1024 + 1];
    assert_eq!(
        err(&related(&[part(
            "Content-Type: text/html\r\nContent-Transfer-Encoding: base64",
            b64(&html)
        )])),
        ErrorCode::HtmlTooLarge
    );
    let small = Limits {
        max_html_bytes: 100,
        ..Limits::default()
    };
    assert_eq!(
        extract(&related(&[html_qp(&page(&"x".repeat(200)))]), &small).unwrap_err(),
        ErrorCode::HtmlTooLarge
    );
}

#[test]
fn d_text_limit_real_value() {
    // ~9 MiB of visible text inside a 16 MiB HTML part.
    let text = "あいうえお".repeat(9 * 1024 * 1024 / 15 + 1);
    let html = format!("<html><body><p>{text}</p></body></html>");
    assert!(html.len() < 16 * 1024 * 1024);
    assert_eq!(
        err(&related(&[part(
            "Content-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: 8bit",
            html
        )])),
        ErrorCode::TextTooLarge
    );
    let small = Limits {
        max_text_bytes: 10,
        ..Limits::default()
    };
    assert_eq!(
        extract(&related(&[html_qp(&page("<p>12345678901</p>"))]), &small).unwrap_err(),
        ErrorCode::TextTooLarge
    );
    assert!(extract(&related(&[html_qp(&page("<p>1234567890</p>"))]), &small).is_ok());
}

#[test]
fn d_metadata_limit() {
    let desc = "d".repeat(64 * 1024 + 1);
    let html = format!(
        "<html><head><meta name=description content=\"{desc}\"></head><body>x</body></html>"
    );
    assert_eq!(
        err(&related(&[part(
            "Content-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: 8bit",
            html
        )])),
        ErrorCode::MetadataTooLarge
    );
    let ok_desc = "d".repeat(64 * 1024);
    let html = format!(
        "<html><head><meta name=description content=\"{ok_desc}\"></head><body>x</body></html>"
    );
    assert!(extract(
        &related(&[part(
            "Content-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: 8bit",
            html
        )]),
        &Limits::default()
    )
    .is_ok());
}

#[test]
fn d_control_characters_and_invalid_utf8() {
    let raw = b"<html><head><title>T\x01itle</title></head><body><p>a\x00b\x07c\x1bd</p><p>bad \xff\xfe utf8</p></body></html>".to_vec();
    let x = ok(&related(&[part(
        "Content-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: 8bit",
        raw,
    )]));
    assert!(!x
        .content
        .body_text
        .chars()
        .any(|c| c.is_control() && c != '\n'));
    assert!(x.content.body_text.contains("abcd") || x.content.body_text.contains("ab"));
    assert!(x.content.body_text.contains('\u{FFFD}'));
    assert!(x.diagnostics.encoding.malformed_sequences_replaced);
    assert!(x
        .diagnostics
        .warnings
        .contains(&"malformed_bytes_replaced_with_u_fffd"));
    assert_eq!(x.metadata.title.as_deref(), Some("Title"));
}

#[test]
fn d_errors_carry_no_page_text() {
    for code in [
        ErrorCode::InvalidBase64,
        ErrorCode::RootNotHtml,
        ErrorCode::TextTooLarge,
    ] {
        assert!(code
            .as_str()
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'));
    }
}

fn lcg(seed: &mut u64) -> u64 {
    *seed = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    *seed >> 33
}

#[test]
fn d_mutated_inputs_never_panic() {
    let mut inputs = vec![
        fixture("ja-article.mhtml"),
        fixture("complex.mhtml"),
        fixture("sjis-news.mhtml"),
        related(&[
            html_qp(&page("<p>x</p>")),
            part(
                "Content-Type: image/png\r\nContent-Transfer-Encoding: base64",
                b64(b"img"),
            ),
        ]),
        nested(5),
    ];
    let mut seed = 0x5eed;
    let mut runs = 0;
    for base in inputs.drain(..) {
        for _ in 0..400 {
            let mut m = base.clone();
            match lcg(&mut seed) % 4 {
                0 => {
                    for _ in 0..1 + lcg(&mut seed) % 8 {
                        let i = (lcg(&mut seed) as usize) % m.len();
                        m[i] = lcg(&mut seed) as u8;
                    }
                }
                1 => m.truncate((lcg(&mut seed) as usize) % m.len()),
                2 => {
                    let i = (lcg(&mut seed) as usize) % m.len();
                    let junk: Vec<u8> = (0..1 + lcg(&mut seed) % 64)
                        .map(|_| lcg(&mut seed) as u8)
                        .collect();
                    m.splice(i..i, junk);
                }
                _ => {
                    let i = (lcg(&mut seed) as usize) % m.len();
                    let n = ((lcg(&mut seed) as usize) % 200).min(m.len() - i);
                    m.drain(i..i + n);
                }
            }
            // `extract` catches panics as InternalPanic: none may occur.
            assert_ne!(
                extract(&m, &Limits::default()).err(),
                Some(ErrorCode::InternalPanic)
            );
            runs += 1;
        }
    }
    assert_eq!(runs, 2000);
}

#[test]
fn d_deeply_nested_html_does_not_overflow() {
    let html = format!(
        "<html><body>{}深い{}</body></html>",
        "<div>".repeat(100_000),
        "</div>".repeat(100_000)
    );
    let x = ok(&related(&[part(
        "Content-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: 8bit",
        html,
    )]));
    assert_eq!(x.content.body_text, "深い");
}

// ---------------------------------------------------------------- mail-parser alone (why the strict layer exists)

#[test]
fn mail_parser_alone_accepts_what_the_poc_refuses() {
    use mail_parser::{DecodeProblems, MessageParser, PartFlags};
    // No close delimiter: parsed, only flagged.
    let unterminated = related_with("", B, &[html_qp(&page("<p>x</p>"))], false);
    let m = MessageParser::new()
        .parse(&unterminated)
        .expect("mail-parser returns a message");
    assert!(m
        .parts()
        .any(|p| p.flags().contains(PartFlags::UNTERMINATED)));
    // Invalid base64: decoded best-effort (bad bytes skipped), only reported.
    let bad = related(&[part(
        "Content-Type: text/html\r\nContent-Transfer-Encoding: base64",
        b"PGh0bWw+!!!not*base64".to_vec(),
    )]);
    let m = MessageParser::new().parse(&bad).unwrap();
    let html = m
        .parts()
        .find(|p| p.is_content_type("text", "html"))
        .unwrap();
    let (bytes, problems) = html.decoded_checked();
    assert!(!bytes.is_empty());
    assert!(problems.contains(DecodeProblems::MALFORMED_TRANSFER_ENCODING));
    // Boundary declared but not used: still a message, and NO flag at all. The top-level part says
    // multipart/related but was kept as one leaf; only the PoC's own check (declared multipart that
    // the library did not open) turns this into `malformed_multipart`.
    let mismatched = String::from_utf8(related(&[html_qp(&page("x"))]))
        .unwrap()
        .replacen(
            &format!("boundary=\"{B}\""),
            "boundary=\"declared-but-unused\"",
            1,
        );
    let m = MessageParser::new().parse(mismatched.as_bytes()).unwrap();
    let top = m.root_part();
    assert!(top.is_content_type("multipart", "related") && !top.is_multipart());
    assert!(m.parts().all(|p| p.flags().is_empty()));
    // Chrome's Shift_JIS page: no MIME charset, so mail-parser's own text() decodes it as UTF-8
    // with replacement characters. The PoC decodes the bytes itself (BOM / MIME / <meta>).
    let sjis = fixture("sjis-news.mhtml");
    let m = MessageParser::new().parse(&sjis).unwrap();
    let html = m
        .parts()
        .find(|p| p.is_content_type("text", "html"))
        .unwrap();
    let (text, problems) = html.text_checked().unwrap();
    assert!(text.contains('\u{FFFD}'));
    assert!(problems.contains(DecodeProblems::MALFORMED_CHARSET));
    assert_eq!(ok(&sjis).diagnostics.encoding.name, "Shift_JIS");
}

// ---------------------------------------------------------------- CLI

fn cli(args: &[&std::ffi::OsStr]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_page-fulltext"))
        .args(args)
        .output()
        .unwrap()
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("page-fulltext-cli-{}-{name}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn cli_default_output_has_no_page_text() {
    let dir = scratch("default");
    let input = dir.join("in.mhtml");
    fs::write(&input, related(&[html_qp("<html><head><title>TITLE-CANARY</title><meta name=description content=DESC-CANARY></head><body><p>BODY-CANARY</p></body></html>")])).unwrap();
    let out = cli(&[std::ffi::OsStr::new("--input"), input.as_os_str()]);
    assert!(out.status.success());
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    for canary in [
        "TITLE-CANARY",
        "DESC-CANARY",
        "BODY-CANARY",
        "synthetic.test",
    ] {
        assert!(!all.contains(canary), "{canary} leaked to the console");
    }
    assert!(all.contains("status=success") && all.contains("characters=11"));
    assert_eq!(
        fs::read_dir(&dir).unwrap().count(),
        1,
        "nothing else was written"
    );
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn cli_output_is_written_only_when_asked_never_overwritten_never_partial() {
    let dir = scratch("output");
    let input = dir.join("in.mhtml");
    fs::write(&input, related(&[html_qp(&page("<p>本文</p>"))])).unwrap();
    let json = dir.join("out.json");
    let out = cli(&[
        std::ffi::OsStr::new("--input"),
        input.as_os_str(),
        std::ffi::OsStr::new("--output"),
        json.as_os_str(),
    ]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&fs::read(&json).unwrap()).unwrap();
    assert_eq!(v["status"], "success");
    assert_eq!(v["content"]["body_text"], "本文");
    // Existing file: refused, unchanged.
    fs::write(&json, b"KEEP").unwrap();
    let out = cli(&[
        std::ffi::OsStr::new("--input"),
        input.as_os_str(),
        std::ffi::OsStr::new("--output"),
        json.as_os_str(),
    ]);
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(fs::read(&json).unwrap(), b"KEEP");
    // Failed extraction: no output file at all.
    let bad = dir.join("bad.mhtml");
    fs::write(&bad, b"not mime at all").unwrap();
    let none = dir.join("none.json");
    let out = cli(&[
        std::ffi::OsStr::new("--input"),
        bad.as_os_str(),
        std::ffi::OsStr::new("--output"),
        none.as_os_str(),
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("status=error code="));
    assert!(!none.exists());
    fs::remove_dir_all(&dir).unwrap();
}

// ---------------------------------------------------------------- Chrome-made fixtures

#[test]
fn chrome_ja_article() {
    let x = ok(&fixture("ja-article.mhtml"));
    let t = &x.content.body_text;
    assert_eq!(x.source.selected_part.index, 1);
    assert_eq!(x.diagnostics.encoding.name, "UTF-8");
    assert!(t.contains("東京都\n江東区"));
    assert!(t.contains("🦀") && t.contains("項目 値") && t.contains("English and 日本語 mixed"));
    // Visible on screen, hence searchable since the tokenizer extractor (see b_excluded_...).
    assert!(t.contains("ARIA-HIDDEN-CANARY") && t.contains("VISIBILITY-HIDDEN-CANARY"));
    for canary in [
        "COMMENT-CANARY",
        "STYLE-CANARY",
        "SCRIPT-CANARY",
        "HIDDEN-ATTR",
        "DISPLAY-NONE",
        "NOSCRIPT",
        "TEMPLATE-CANARY",
        "SVG-CANARY",
        "CANVAS-CANARY",
        "CSS-CONTENT-CANARY",
    ] {
        assert!(!t.contains(canary), "{canary}");
    }
    let m = &x.metadata;
    assert_eq!(
        m.title.as_deref(),
        Some("合成記事：RustでSQLiteを扱う & 検索の基礎")
    );
    assert_eq!(m.keywords.as_ref().unwrap().len(), 4);
    assert_eq!(m.article_tags, ["Rust", "データベース"]);
}

#[test]
fn chrome_sjis_frames_and_js() {
    let s = ok(&fixture("sjis-news.mhtml"));
    assert_eq!(s.diagnostics.encoding.name, "Shift_JIS");
    assert!(s.content.body_text.contains("①②③") && s.content.body_text.contains("ｶﾀｶﾅ"));
    let c = ok(&fixture("complex.mhtml"));
    assert_eq!(c.source.html_part_count, 3);
    assert!(
        !c.content.body_text.contains("FRAME-BODY-TEXT")
            && !c.content.body_text.contains("SRCDOC-FRAME-TEXT")
    );
    assert!(c
        .diagnostics
        .warnings
        .contains(&"secondary_html_parts_not_extracted"));
    let j = ok(&fixture("js-heavy.mhtml"));
    assert!(
        j.content.body_text.contains("生成された項目A")
            && !j.content.body_text.contains("読み込み中")
    );
    assert!(!j.content.body_text.contains("JS-HIDDEN-CANARY"));
}
