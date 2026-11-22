//! Regression tests for the independent review findings HIGH-1, HIGH-2 (end-tag handling at SVG/
//! MathML integration points and for `</template>`) and MEDIUM-1 (non-UTF-8 header bytes), plus
//! boundary cases of the same state handling (report XIII-12). Synthetic inputs only.

use page_fulltext::{extract, limits::Limits, Extraction};

const B: &str = "----MultipartBoundary--ENDTAGS----";

fn mhtml_bytes(headers_top: &[u8], part_headers: &[u8], html: &[u8]) -> Vec<u8> {
    let mut out = b"From: <Saved by Blink>\r\n".to_vec();
    out.extend_from_slice(headers_top);
    out.extend_from_slice(
        format!("Subject: synthetic\r\nMIME-Version: 1.0\r\nContent-Type: multipart/related;\r\n\ttype=\"text/html\";\r\n\tboundary=\"{B}\"\r\n\r\n\r\n--{B}\r\n").as_bytes(),
    );
    out.extend_from_slice(part_headers);
    out.extend_from_slice(b"\r\n\r\n");
    out.extend_from_slice(html);
    out.extend_from_slice(format!("\r\n--{B}--\r\n").as_bytes());
    out
}

const PART: &[u8] = b"Content-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\nContent-Location: https://synthetic.test/p";

fn run(html: &str) -> Extraction {
    extract(
        &mhtml_bytes(
            b"Snapshot-Content-Location: https://synthetic.test/p\r\n",
            PART,
            html.as_bytes(),
        ),
        &Limits::default(),
    )
    .unwrap_or_else(|e| panic!("{e}"))
}

fn body(inner: &str) -> String {
    run(&format!(
        "<!DOCTYPE html><html><head><title>T</title></head><body>{inner}</body></html>"
    ))
    .content
    .body_text
}

/// `inner` must give exactly `expected`, and no excluded canary may appear.
fn check(inner: &str, expected: &str) {
    let t = body(inner);
    assert_eq!(t, expected, "input: {inner}");
    for canary in ["cap", "cell", "SECRET"] {
        assert!(!t.contains(canary), "{canary} leaked from: {inner}");
    }
}

// ------------------------------------------------------------------ HIGH-1

#[test]
fn h1_review_counterexamples() {
    check(
        "<svg><foreignObject><p>cap</foreignObject></svg>\n<p>IMPORTANT_BODY</p>",
        "IMPORTANT_BODY",
    );
    check(
        "<svg><desc><p>cap</desc></svg>\n<p>IMPORTANT_BODY</p>",
        "IMPORTANT_BODY",
    );
    check(
        "<div hidden><math><mtext><p>x</mtext></math></div>\n<p>IMPORTANT_BODY</p>",
        "IMPORTANT_BODY",
    );
}

#[test]
fn h1_every_integration_point_and_deeper_html() {
    for ip in ["foreignObject", "desc", "title"] {
        check(
            &format!("<svg><g><{ip}><div><ul><li><b>cap</{ip}></svg><p>BODY</p>"),
            "BODY",
        );
    }
    for ip in ["mi", "mo", "mn", "ms", "mtext"] {
        // MathML is visible: its text stays, and the hidden ancestor ends at </div>.
        check(
            &format!("<div hidden><math><mrow><{ip}><p>SECRET</{ip}></math></div><p>BODY</p>"),
            "BODY",
        );
        assert_eq!(
            body(&format!("<math><{ip}><p>数式</{ip}></math><p>BODY</p>")),
            "数式\nBODY"
        );
    }
    // A table left open inside the integration point does not stop the end tag either.
    check(
        "<svg><foreignObject><table><tr><td>cap</foreignObject></svg><p>BODY</p>",
        "BODY",
    );
    // End tags are matched ASCII case-insensitively.
    check(
        "<svg><foreignObject><p>cap</FOREIGNOBJECT></SVG><p>BODY</p>",
        "BODY",
    );
    // Nested SVG: the inner one closes first, the outer one still excludes.
    check(
        "<svg><foreignObject><svg><desc><p>cap</desc></svg>cap</foreignObject>cap</svg>BODY",
        "BODY",
    );
}

#[test]
fn h1_exclusions_are_not_ended_early() {
    // Closing the SVG does not end a hidden ancestor.
    check(
        "<div hidden><svg><foreignObject><p>SECRET</foreignObject></svg>SECRET</div><p>BODY</p>",
        "BODY",
    );
    // An HTML end tag inside an integration point still cannot close an element outside it
    // (the integration point is a scope boundary): the hidden div ends at its own end tag.
    check(
        "<div hidden><svg><foreignObject><p>SECRET</div>SECRET</foreignObject></svg>SECRET</div>BODY",
        "BODY",
    );
    // Inside a template the SVG end tag cannot close an SVG outside it.
    check(
        "<svg><foreignObject><template><p>SECRET</foreignObject>SECRET</template>cap</svg>BODY",
        "BODY",
    );
    // An unclosed SVG still hides the rest (as in browsers).
    check("前<svg><foreignObject><p>cap", "前");
}

#[test]
fn h1_same_kind_html_end_tags_close_through_svg() {
    // A plain SVG/MathML element (not an integration point) is not a scope boundary: the HTML end
    // tag closes the SVG with the hidden element, as in browsers.
    check("<div hidden><svg><g><text>SECRET</div><p>BODY</p>", "BODY");
    check("<div hidden><math><mrow>SECRET</div><p>BODY</p>", "BODY");
    // </p> and </br> leave foreign content (HTML Standard), so the rest is not in the SVG.
    check("<svg><g>cap</p>BODY", "BODY");
    assert_eq!(body("<p>前<svg><g>cap</br>後"), "前\n後");
    // Table end tags use the table scope, where integration points are not boundaries.
    check(
        "<table><tr><td><svg><foreignObject><p>cap</table><p>BODY</p>",
        "BODY",
    );
    // Any heading end tag closes the open heading; </dd> is not stopped by a nested <dl>.
    check("<h1 hidden>SECRET</h2><p>BODY</p>", "BODY");
    check("<dl><dd hidden><dl><dt>SECRET</dd><p>BODY</p>", "BODY");
}

#[test]
fn h1_normal_svg_and_math_unchanged() {
    assert_eq!(
        body("<p>前<svg><desc><b>説明</b></desc><text>t</text></svg>後</p>"),
        "前後"
    );
    assert_eq!(
        body("<math><mi>x</mi><mo>=</mo><mfrac><mn>1</mn><mn>2</mn></mfrac></math>"),
        "x=12"
    );
    assert_eq!(body("<svg><text>S</text><p>BREAKOUT</p>"), "BREAKOUT");
    // A breakout start tag stops at the integration point: the text stays inside the SVG.
    check(
        "<svg><foreignObject><svg><g><p>cap</p>cap</foreignObject>cap</svg>BODY",
        "BODY",
    );
}

// ------------------------------------------------------------------ HIGH-2

#[test]
fn h2_review_counterexample() {
    check(
        "<template><table><tr><td>cell</template>\n<p>IMPORTANT_BODY</p>",
        "IMPORTANT_BODY",
    );
}

#[test]
fn h2_template_end_tag_cases() {
    for open in [
        "<table><caption>cell",
        "<table><tbody><tr><th>cell",
        "<ul><li><div><p>cell",
        "<svg><foreignObject><p>cell",
        "<math><mtext><p>cell",
        "<object><applet><marquee>cell",
        "<select><option>cell",
        "<div hidden><span>cell",
    ] {
        check(&format!("<template>{open}</template><p>BODY</p>"), "BODY");
    }
    // Nested templates: </template> closes the inner one, the outer one still excludes.
    check(
        "<template><div><template><table><tr><td>cell</template>SECRET</template><p>BODY</p>",
        "BODY",
    );
    // End tags of elements outside the template are ignored inside it.
    check(
        "<div hidden><template></div>SECRET</template>SECRET</div><p>BODY</p>",
        "BODY",
    );
    check(
        "<template><p>SECRET</div></td></table></svg></body></html>SECRET</template><p>BODY</p>",
        "BODY",
    );
    // A stray </template> changes nothing.
    assert_eq!(body("a</template>b<div>c</template>d</div>"), "ab\ncd");
}

// ------------------------------------------------------------------ limits and cost

#[test]
fn e_stack_limit_with_integration_points() {
    let limit = Limits::default().max_open_elements;
    // Integration points beyond the limit are not tracked; the text is kept (more, never less).
    let x = run(&format!(
        "<html><body>{}<svg><foreignObject><p>深い</foreignObject></svg>後</body></html>",
        "<div>".repeat(limit + 10)
    ));
    assert_eq!(x.content.body_text, "深い後");
    assert!(x.diagnostics.tokens.max_open_depth <= limit);
    // At the limit, an SVG that is tracked is still closed by its end tag.
    let y = run(&format!(
        "<html><body>{}<svg><foreignObject><p>cap</foreignObject></svg>後</body></html>",
        "<div>".repeat(limit - 3)
    ));
    assert_eq!(y.content.body_text, "後");
}

#[test]
fn e_many_stray_end_tags_against_a_full_stack() {
    // Every end tag kind handled here costs a few index lookups, not a stack walk.
    let n = 200_000;
    let html = format!(
        "<html><body>{}<svg><foreignObject><template>{}</template></foreignObject></svg>{}終</body></html>",
        "<div>".repeat(1_000),
        "</h1></template-x></foreignobject></desc></dd></p></br>".repeat(n),
        "</h3></mtext></svg></td>".repeat(n),
    );
    let started = std::time::Instant::now();
    let x = run(&html);
    assert_eq!(x.content.body_text, "終");
    assert!(x.diagnostics.tokens.end_tags >= 11 * n);
    // Generous bound (debug build): a quadratic walk would take minutes.
    assert!(started.elapsed().as_secs() < 60);
}

// ------------------------------------------------------------------ MEDIUM-1

#[test]
fn m1_non_utf8_snapshot_location_is_dropped_with_a_warning() {
    let x = extract(
        &mhtml_bytes(
            b"Snapshot-Content-Location: https://synthetic.test/\xff\xfebad\r\n",
            PART,
            b"<body>x</body>",
        ),
        &Limits::default(),
    )
    .unwrap();
    assert!(x.source.snapshot_location.is_none());
    assert!(x
        .diagnostics
        .warnings
        .contains(&"snapshot_location_invalid_utf8_dropped"));
    // The root part is still selected as before and its own location is kept.
    assert_eq!(x.content.body_text, "x");
    assert_eq!(
        x.source.selected_part.content_location.as_deref(),
        Some("https://synthetic.test/p")
    );
    // Without a snapshot location there is nothing to compare with: no location warnings.
    assert!(!x
        .diagnostics
        .warnings
        .iter()
        .any(|w| w.starts_with("root_")));
}

#[test]
fn m1_same_check_for_content_location_and_mime_charset() {
    // The parser would turn these bytes into U+FFFD; the values are dropped instead.
    let x = extract(
        &mhtml_bytes(
            b"Snapshot-Content-Location: https://synthetic.test/p\r\n",
            b"Content-Type: text/html; charset=\"utf\xff8\"\r\nContent-Transfer-Encoding: 8bit\r\nContent-Location: https://synthetic.test/\xff\xfebad",
            b"<html><head><meta charset=shift_jis></head><body>x</body></html>",
        ),
        &Limits::default(),
    )
    .unwrap();
    assert!(x.source.selected_part.content_location.is_none());
    assert!(x.diagnostics.encoding.mime_charset.is_none());
    for w in [
        "content_location_invalid_utf8_dropped",
        "mime_charset_invalid_utf8_ignored",
    ] {
        assert!(x.diagnostics.warnings.contains(&w), "{w}");
    }
    // The ignored MIME charset leaves the decision to the <meta>; the root is the same part.
    assert_eq!(x.diagnostics.encoding.name, "Shift_JIS");
    assert_eq!(x.content.body_text, "x");
    assert_eq!(x.source.selected_part.index, 1);
}

#[test]
fn m1_valid_utf8_and_other_checks_unchanged() {
    let x = extract(
        &mhtml_bytes(
            "Snapshot-Content-Location: https://synthetic.test/日本\r\n".as_bytes(),
            "Content-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\nContent-Location: https://synthetic.test/日本".as_bytes(),
            b"<body>x</body>",
        ),
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(
        x.source.snapshot_location.as_deref(),
        Some("https://synthetic.test/日本")
    );
    assert!(!x
        .diagnostics
        .warnings
        .iter()
        .any(|w| w.contains("location")));
    // Invalid UTF-8 and a control character: the UTF-8 check comes first, one warning.
    let y = extract(
        &mhtml_bytes(
            b"Snapshot-Content-Location: https://synthetic.test/\x01\xff\r\n",
            PART,
            b"<body>x</body>",
        ),
        &Limits::default(),
    )
    .unwrap();
    assert!(y.source.snapshot_location.is_none());
    assert!(y
        .diagnostics
        .warnings
        .contains(&"snapshot_location_invalid_utf8_dropped"));
    assert!(!y
        .diagnostics
        .warnings
        .contains(&"snapshot_location_control_characters_dropped"));
}
