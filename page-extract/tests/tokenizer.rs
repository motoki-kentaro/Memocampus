//! Tests added with the tokenizer extractor (report XIII). Synthetic inputs only.

use page_extract::{extract, limits::ErrorCode, limits::Limits, Extraction};
use std::{fs, path::PathBuf};

const B: &str = "----MultipartBoundary--TOKENIZER----";

fn mhtml_with(headers_top: &str, part_headers: &str, html: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "From: <Saved by Blink>\r\n{headers_top}Subject: synthetic\r\nMIME-Version: 1.0\r\nContent-Type: multipart/related;\r\n\ttype=\"text/html\";\r\n\tboundary=\"{B}\"\r\n\r\n\r\n--{B}\r\n{part_headers}\r\n\r\n"
    )
    .into_bytes();
    out.extend_from_slice(html);
    out.extend_from_slice(format!("\r\n--{B}--\r\n").as_bytes());
    out
}

fn mhtml(html: &[u8]) -> Vec<u8> {
    mhtml_with(
        "Snapshot-Content-Location: https://synthetic.test/p\r\n",
        "Content-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\nContent-Location: https://synthetic.test/p",
        html,
    )
}

fn run(html: &str) -> Extraction {
    extract(&mhtml(html.as_bytes()), &Limits::default()).unwrap_or_else(|e| panic!("{e}"))
}

fn body(inner: &str) -> String {
    run(&format!(
        "<!DOCTYPE html><html><head><title>T</title></head><body>{inner}</body></html>"
    ))
    .content
    .body_text
}

fn fixture(name: &str) -> Vec<u8> {
    fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/chrome")
            .join(name),
    )
    .unwrap()
}

// ------------------------------------------------------------------ tokenizer: text

#[test]
fn t_japanese_english_and_mixed() {
    assert_eq!(body("<p>日本語の本文です。</p>"), "日本語の本文です。");
    assert_eq!(body("<p>English body text.</p>"), "English body text.");
    assert_eq!(
        body("<p>Rust で <code>SQLite</code> を使う 🦀</p>"),
        "Rust で SQLite を使う 🦀"
    );
}

#[test]
fn t_entities_in_text_and_attributes() {
    let x = run("<html><head><meta name=description content=\"A&amp;B &lt;説明&gt; &#x41;&#66;\"></head><body><p>&lt;p&gt; &amp;amp; &notin; &copy &#12354; &#x1F980; &nbsp;x</p></body></html>");
    assert_eq!(x.content.body_text, "<p> &amp; ∉ © あ 🦀 x");
    assert_eq!(x.metadata.description.as_deref(), Some("A&B <説明> AB"));
}

#[test]
fn t_script_and_style_content_never_becomes_text_or_tags() {
    let t = body(
        r#"<p>前</p><script>if (a<b && c>d) { document.write("<p>FAKE-P</p></div>"); var s = "</scr" + "ipt>"; }</script>
        <style>p::after { content: "</p><b>STYLE-FAKE</b>"; } a<b{}</style>
        <script><!-- <script> NESTED-ESCAPED </script> --></script>
        <noscript><p>NOSCRIPT-TEXT</p></noscript><iframe><p>IFRAME-FALLBACK</p></iframe>
        <script/>SELF-CLOSED-SCRIPT-STILL-SCRIPT</script><p>後</p>"#,
    );
    assert_eq!(t, "前\n後");
}

#[test]
fn t_rcdata_and_rawtext_that_are_visible() {
    // textarea and xmp show their content literally: markup inside is text.
    assert_eq!(
        body("<textarea><p>入力 &amp; 値</p></textarea><xmp><b>xmp &amp;</b></xmp>"),
        "<p>入力 & 値</p>\n<b>xmp &amp;</b>"
    );
    let x =
        run("<html><head><title>題名 <b>太字</b> &amp; 記号</title></head><body>x</body></html>");
    assert_eq!(x.metadata.title.as_deref(), Some("題名 <b>太字</b> & 記号"));
    assert_eq!(x.content.body_text, "x");
}

#[test]
fn t_comments_head_and_doctype_are_not_text() {
    let x = run("<!DOCTYPE html><!-- C1 --><html><head><!-- C2 --><link rel=x><style>s</style></head><body><!-- C3 -->本文<!-- C4 --></body></html><!-- C5 -->");
    assert_eq!(x.content.body_text, "本文");
    assert_eq!(x.diagnostics.tokens.comments, 5);
}

#[test]
fn t_block_boundaries_and_inline_joining() {
    assert_eq!(body("<p>東京</p><p>都内</p>"), "東京\n都内");
    assert_eq!(body("<span>東</span><span>京都</span>"), "東京都");
    assert_eq!(body("<div>東京</div>都内"), "東京\n都内");
    assert_eq!(body("<li>一<li>二"), "一\n二");
    assert_eq!(body("<h1>見出し</h1>本文"), "見出し\n本文");
    assert_eq!(body("<td>セルA</td><td>セルB</td>"), "セルA セルB");
    assert_eq!(body("東京<br>都内"), "東京\n都内");
    assert_eq!(body("<x-card>A</x-card><x-card>B</x-card>"), "A\nB");
    assert_eq!(body("<p>a <b>b</b><i>c</i> d</p>"), "a bc d");
    assert_eq!(body("<pre>行1\n  行2\n\n行3</pre>"), "行1\n行2\n行3");
}

#[test]
fn t_misnested_and_implicitly_closed_markup() {
    assert_eq!(body("<p>a<div>b</p>c</div>d"), "a\nb\nc\nd");
    assert_eq!(body("<b><i>x</b>y</i>z"), "xyz");
    assert_eq!(
        body("<table><tr><td>a<td>b<tr><td>c</table>後"),
        "a b\nc\n後"
    );
    assert_eq!(body("</span></div></p>stray"), "stray");
    // A hidden <p> is closed by the next <p> (implicit end tag): the second one is visible.
    assert_eq!(body("<p hidden>隠れ<p>見える"), "見える");
    // A hidden <li> is closed by the next <li>.
    assert_eq!(body("<ul><li hidden>隠れ<li>見える</ul>"), "見える");
    // A cell end closes a hidden <div> left open inside it (scope), as the tree builder does.
    assert_eq!(
        body("<table><tr><td><div hidden>隠れ</td><td>見える</td></tr></table>"),
        "見える"
    );
    // An unclosed hidden element hides the rest, as in the browser.
    assert_eq!(body("前<span hidden>x<div>y</div>"), "前");
}

#[test]
fn t_excluded_elements() {
    let t = body(
        "<p>可視</p><template><p>TEMPLATE<template>NESTED</template>STILL</template></template>\
         <svg><text>SVG-TEXT</text><style>a<b</style><foreignObject><p>FO</p></foreignObject></svg>\
         <canvas>CANVAS<p>IN-CANVAS</p></canvas><noembed>NOEMBED</noembed><noframes>NOFRAMES</noframes>\
         <p>終</p>",
    );
    assert_eq!(t, "可視\n終");
}

#[test]
fn t_foreign_content_rules() {
    // An HTML breakout tag inside SVG closes the SVG (browsers do the same): the text is shown.
    assert_eq!(body("<svg><text>S</text><p>BREAKOUT</p>"), "BREAKOUT");
    // In SVG, <script/> is self-closing and switches nothing: the following text is not lost.
    assert_eq!(body("<svg><script/></svg>後続"), "後続");
    // CDATA is recognised only in foreign content.
    assert_eq!(body("<svg><![CDATA[<p>CDATA</p>]]></svg>後"), "後");
    assert_eq!(body("<![CDATA[BOGUS]]>本文"), "本文");
    // MathML text is visible and kept.
    assert_eq!(body("<math><mi>x</mi><mo>=</mo><mn>1</mn></math>"), "x=1");
    // A <title> inside SVG is not the document title.
    let x = run(
        "<html><body><svg><title>SVG-TITLE</title></svg><title>本当の題名</title>x</body></html>",
    );
    assert_eq!(x.metadata.title.as_deref(), Some("本当の題名"));
    assert_eq!(x.content.body_text, "x");
}

#[test]
fn t_aria_hidden_and_until_found_text_is_kept() {
    assert_eq!(
        body("<div aria-hidden=\"true\">50 個の言語版</div><div hidden=\"until-found\">折りたたみ</div><div hidden>隠し</div><div style=\"display:none\">非表示</div>"),
        "50 個の言語版\n折りたたみ"
    );
}

#[test]
fn t_head_ends_implicitly() {
    // Non-space text or a body element in the head ends it; the text is body text.
    assert_eq!(
        run("<html><head><title>T</title>頭の後の文<p>段落</p></html>")
            .content
            .body_text,
        "頭の後の文\n段落"
    );
    assert_eq!(
        run("<html><head><meta charset=utf-8><div>DIV</div></head></html>")
            .content
            .body_text,
        "DIV"
    );
    // Text after </body> and </html> is appended to the body in browsers (one text run).
    assert_eq!(
        run("<html><body>a</body>b</html>c").content.body_text,
        "abc"
    );
}

#[test]
fn t_deep_nesting_and_open_element_limit() {
    let deep = format!(
        "{}深い{}",
        "<div>".repeat(200_000),
        "</div>".repeat(200_000)
    );
    let x = run(&format!("<html><body>{deep}</body></html>"));
    assert_eq!(x.content.body_text, "深い");
    assert!(x.diagnostics.tokens.untracked_elements > 0);
    assert!(x
        .diagnostics
        .warnings
        .contains(&"open_element_limit_reached"));
    assert!(x.diagnostics.tokens.max_open_depth <= Limits::default().max_open_elements);
    // Beyond the limit an exclusion cannot be tracked: the text is kept (more, never less).
    let over = format!("{}<div hidden>深すぎて追跡外</div>", "<div>".repeat(2_000));
    assert_eq!(body(&over), "深すぎて追跡外");
}

#[test]
fn t_many_tags() {
    let n = 300_000;
    let html = format!(
        "<html><body><p>{}</p></body></html>",
        "<span>字</span>".repeat(n)
    );
    let x = run(&html);
    assert_eq!(x.content.character_count, n);
    assert_eq!(x.diagnostics.tokens.start_tags, n + 3);
}

#[test]
fn t_chunked_input_gives_identical_output() {
    let synthetic = mhtml(
        "<html><head><title>分割 &amp; 題名</title><meta name=description content=\"説明&amp;\"></head>\
         <body><p>日本語&nbsp;テキスト&#x1F980;</p><script>if(a<b){x=\"</p>\"}</script>\
         <!-- comment --><textarea>&lt;入力&gt;</textarea>\r\n<pre>a\r\nb</pre><svg><![CDATA[x]]></svg>終わり</body></html>"
            .as_bytes(),
    );
    let inputs = [
        fixture("ja-article.mhtml"),
        fixture("sjis-news.mhtml"),
        fixture("complex.mhtml"),
        fixture("js-heavy.mhtml"),
        synthetic,
    ];
    for input in &inputs {
        let reference = extract(input, &Limits::default()).unwrap();
        for chunk in [1, 2, 3, 5, 7, 64, 4096] {
            let limits = Limits {
                tokenizer_chunk_bytes: chunk,
                ..Limits::default()
            };
            let x = extract(input, &limits).unwrap();
            assert_eq!(
                x.content.body_text, reference.content.body_text,
                "chunk {chunk}"
            );
            assert_eq!(x.metadata, reference.metadata, "chunk {chunk}");
            assert_eq!(
                x.diagnostics.encoding.malformed_sequences_replaced,
                reference.diagnostics.encoding.malformed_sequences_replaced
            );
        }
    }
}

// ------------------------------------------------------------------ metadata

#[test]
fn m_huge_non_target_meta_is_ignored() {
    // MEDIUM-1: a <meta> whose key is not extracted cannot refuse the page.
    let huge = "v".repeat(200 * 1024);
    let x = run(&format!(
        "<html><head><meta name=viewport content=\"{huge}\"><meta property=og:image content=\"{huge}\"><meta content=\"{huge}\"><meta name=description content=\"説明\"></head><body>x</body></html>"
    ));
    assert_eq!(x.metadata.description.as_deref(), Some("説明"));
}

#[test]
fn m_name_and_property_on_one_tag() {
    // MEDIUM-5: both keys of one tag are filled.
    let x = run("<html><head><meta name=\"description\" property=\"og:description\" content=\"記事の説明\"><meta name=\"keywords\" property=\"article:tag\" content=\"Rust, 検索\"></head><body>x</body></html>");
    assert_eq!(x.metadata.description.as_deref(), Some("記事の説明"));
    assert_eq!(x.metadata.og_description.as_deref(), Some("記事の説明"));
    assert_eq!(
        x.metadata.keywords.as_deref(),
        Some(&["Rust".to_owned(), "検索".to_owned()][..])
    );
    assert_eq!(x.metadata.article_tags, ["Rust, 検索"]);
    // The same key twice on one tag counts once (no duplicate warning).
    let y = run("<html><head><meta name=description property=DESCRIPTION content=a></head><body>x</body></html>");
    assert_eq!(y.metadata.description.as_deref(), Some("a"));
    assert!(!y
        .diagnostics
        .warnings
        .contains(&"duplicate_meta_description"));
}

#[test]
fn m_metadata_limits() {
    let limit = Limits::default().max_metadata_bytes;
    let too_long_title = format!(
        "<html><head><title>{}</title></head><body>x</body></html>",
        "t".repeat(limit + 1)
    );
    assert_eq!(
        extract(&mhtml(too_long_title.as_bytes()), &Limits::default()).unwrap_err(),
        ErrorCode::MetadataTooLarge
    );
    // Whitespace collapses while reading: a title padded with 1 MiB of spaces is fine.
    let padded = format!(
        "<html><head><title>a{}b</title></head><body>x</body></html>",
        " ".repeat(1024 * 1024)
    );
    assert_eq!(run(&padded).metadata.title.as_deref(), Some("a b"));
    // All article tags together are limited too.
    let tags: String = (0..20)
        .map(|i| {
            format!(
                "<meta property=article:tag content=\"{i}{}\">",
                "x".repeat(4000)
            )
        })
        .collect();
    assert_eq!(
        extract(
            &mhtml(format!("<html><head>{tags}</head><body>x</body></html>").as_bytes()),
            &Limits::default()
        )
        .unwrap_err(),
        ErrorCode::MetadataTooLarge
    );
}

#[test]
fn m_header_values_are_bounded_and_checked() {
    // MEDIUM-4: header-derived strings are dropped (with a warning) when abnormal.
    let html = b"<html><head><meta charset=shift_jis></head><body>x</body></html>";
    let long_loc = format!("https://synthetic.test/{}", "a".repeat(9000));
    let x = extract(
        &mhtml_with(
            &format!("Snapshot-Content-Location: {long_loc}\r\n"),
            &format!("Content-Type: text/html; charset=\"{}\"\r\nContent-Transfer-Encoding: 8bit\r\nContent-Location: {long_loc}", "u".repeat(100)),
            html,
        ),
        &Limits::default(),
    )
    .unwrap();
    assert!(
        x.source.snapshot_location.is_none() && x.source.selected_part.content_location.is_none()
    );
    assert!(x.diagnostics.encoding.mime_charset.is_none());
    // The oversized MIME charset is ignored, so the <meta> decides.
    assert_eq!(x.diagnostics.encoding.name, "Shift_JIS");
    for w in [
        "snapshot_location_too_large_dropped",
        "content_location_too_large_dropped",
        "mime_charset_too_large_ignored",
    ] {
        assert!(x.diagnostics.warnings.contains(&w), "{w}");
    }
    // Control characters (raw bytes in the header line).
    let y = extract(
        &mhtml_with(
            "Snapshot-Content-Location: https://synthetic.test/\x01bad\r\n",
            "Content-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\nContent-Location: https://synthetic.test/\x7fbad",
            b"<body>x</body>",
        ),
        &Limits::default(),
    )
    .unwrap();
    assert!(
        y.source.snapshot_location.is_none() && y.source.selected_part.content_location.is_none()
    );
    for w in [
        "snapshot_location_control_characters_dropped",
        "content_location_control_characters_dropped",
    ] {
        assert!(y.diagnostics.warnings.contains(&w), "{w}");
    }
    // Normal values are kept as written.
    let z = run("<body>x</body>");
    assert_eq!(
        z.source.snapshot_location.as_deref(),
        Some("https://synthetic.test/p")
    );
}

// ------------------------------------------------------------------ charset

#[test]
fn c_shift_jis_utf8_and_wrong_declaration() {
    let sjis = encoding_rs::SHIFT_JIS
        .encode("<meta charset=shift_jis><body><p>正常なシフトJIS ｶﾅ ①</p></body>")
        .0
        .into_owned();
    let x = extract(
        &mhtml_with(
            "",
            "Content-Type: text/html\r\nContent-Transfer-Encoding: 8bit",
            &sjis,
        ),
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(x.content.body_text, "正常なシフトJIS ｶﾅ ①");
    assert!(!x.diagnostics.encoding.malformed_sequences_replaced);
    assert_eq!(body("<p>UTF-8 の本文</p>"), "UTF-8 の本文");
    // Declared Shift_JIS, actually UTF-8: no repair (PO decision), success with the warning.
    let wrong = extract(
        &mhtml_with(
            "",
            "Content-Type: text/html\r\nContent-Transfer-Encoding: 8bit",
            "<meta charset=shift_jis><body>日本語の本文です</body>".as_bytes(),
        ),
        &Limits::default(),
    )
    .unwrap();
    assert!(wrong.diagnostics.encoding.malformed_sequences_replaced);
    assert!(wrong
        .diagnostics
        .warnings
        .contains(&"malformed_bytes_replaced_with_u_fffd"));
    assert_ne!(wrong.content.body_text, "日本語の本文です");
}

#[test]
fn c_decoded_size_limit() {
    // windows-1252 0x80 is U+20AC (3 UTF-8 bytes): 12 MiB of it fits the 16 MiB HTML limit but
    // becomes 36 MiB of UTF-8, above the 32 MiB decoded limit. It sits in a <script> so that the
    // 8 MiB body-text limit (checked while streaming) is not what stops it.
    let mut html = b"<meta charset=windows-1252><body><script>".to_vec();
    html.extend(std::iter::repeat_n(0x80u8, 12 * 1024 * 1024));
    assert_eq!(
        extract(
            &mhtml_with(
                "",
                "Content-Type: text/html\r\nContent-Transfer-Encoding: binary",
                &html
            ),
            &Limits::default()
        )
        .unwrap_err(),
        ErrorCode::DecodedHtmlTooLarge
    );
    let small = Limits {
        max_decoded_html_bytes: 100,
        ..Limits::default()
    };
    assert_eq!(
        extract(
            &mhtml(format!("<body>{}</body>", "あ".repeat(40)).as_bytes()),
            &small
        )
        .unwrap_err(),
        ErrorCode::DecodedHtmlTooLarge
    );
    assert!(extract(
        &mhtml(format!("<body>{}</body>", "a".repeat(40)).as_bytes()),
        &small
    )
    .is_ok());
}

#[test]
fn c_invalid_charset_label_warns() {
    let x = extract(
        &mhtml_with(
            "",
            "Content-Type: text/html; charset=no-such-charset\r\nContent-Transfer-Encoding: 8bit",
            b"<body>x</body>",
        ),
        &Limits::default(),
    )
    .unwrap();
    assert!(x
        .diagnostics
        .warnings
        .contains(&"unknown_mime_charset_label_ignored"));
    assert_eq!(x.diagnostics.encoding.name, "UTF-8");
}

// ------------------------------------------------------------------ no DOM

#[test]
fn n_extractor_is_the_tokenizer() {
    let x = run("<body>x</body>");
    assert_eq!(x.diagnostics.extractor, "html5ever-tokenizer");
    assert_eq!(
        page_extract::OUTPUT_SCHEMA,
        "memocampus.page-fulltext.poc/2"
    );
}
