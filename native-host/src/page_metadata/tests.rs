use super::*;
use serde_json::json;

const AT: &str = "2026-11-05T01:02:03.456Z";

fn page(extra: Value) -> Value {
    let mut page = json!({ "capturedAt": AT });
    page.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    page
}

#[test]
fn every_field_is_kept_apart_and_missing_ones_are_null() {
    let full = parse(&page(json!({
        "title": "Doc title", "description": "Plain description", "ogTitle": "OG title",
        "ogDescription": "OG description", "ogSiteName": "Site", "ogImageUrl": "https://cdn.test/a.png?x=1&y=2",
        "faviconUrl": "https://site.test/icon.ico",
    })))
    .unwrap();
    assert_eq!(full.title.as_deref(), Some("Doc title"));
    assert_eq!(full.description.as_deref(), Some("Plain description"));
    assert_eq!(full.og_title.as_deref(), Some("OG title"));
    assert_eq!(full.og_description.as_deref(), Some("OG description"));
    assert_eq!(full.og_site_name.as_deref(), Some("Site"));
    assert_eq!(
        full.og_image_url.as_deref(),
        Some("https://cdn.test/a.png?x=1&y=2")
    );
    assert_eq!(
        full.favicon_url.as_deref(),
        Some("https://site.test/icon.ico")
    );
    assert_eq!(full.captured_at, AT);

    let bare = parse(&page(json!({ "title": "Only title", "ogTitle": null }))).unwrap();
    assert_eq!(
        bare,
        PageMetadata {
            title: Some("Only title".into()),
            description: None,
            og_title: None,
            og_description: None,
            og_site_name: None,
            og_image_url: None,
            favicon_url: None,
            captured_at: AT.into(),
        }
    );
}

#[test]
fn text_is_cleaned_the_same_way_as_the_extension_does() {
    assert_eq!(clean_text("  a \t\n b  ", 100).as_deref(), Some("a b"));
    assert_eq!(
        clean_text("a\u{0}b\u{7f}c\u{9f}", 100).as_deref(),
        Some("abc")
    );
    assert_eq!(clean_text("a \u{1b} b", 100).as_deref(), Some("a b"));
    assert_eq!(
        clean_text("a\u{a0}\u{3000}\u{2028}b", 100).as_deref(),
        Some("a b")
    );
    assert_eq!(clean_text("", 100), None);
    assert_eq!(clean_text(" \t\r\n\u{3000} ", 100), None);
    assert_eq!(clean_text("\u{0}\u{1}", 100), None);
    // Cut in characters, never inside one, and never ending in a space.
    assert_eq!(clean_text("日本語のタイトル", 3).as_deref(), Some("日本語"));
    assert_eq!(clean_text("😀😀😀", 2).as_deref(), Some("😀😀"));
    assert_eq!(clean_text("ab cd", 3).as_deref(), Some("ab"));
    assert_eq!(clean_text("ab cd", 4).as_deref(), Some("ab c"));
    assert_eq!(
        clean_text(&"x".repeat(5000), MAX_TITLE)
            .unwrap()
            .chars()
            .count(),
        MAX_TITLE
    );
}

#[test]
fn long_values_are_cut_to_their_limits() {
    let long = "長".repeat(10_000);
    let got = parse(&page(json!({
        "title": long, "ogTitle": long, "description": long, "ogDescription": long, "ogSiteName": long,
    })))
    .unwrap();
    let count = |value: &Option<String>| value.as_ref().unwrap().chars().count();
    assert_eq!(count(&got.title), MAX_TITLE);
    assert_eq!(count(&got.og_title), MAX_TITLE);
    assert_eq!(count(&got.description), MAX_DESCRIPTION);
    assert_eq!(count(&got.og_description), MAX_DESCRIPTION);
    assert_eq!(count(&got.og_site_name), MAX_SITE_NAME);
}

#[test]
fn only_plain_web_urls_are_kept_and_never_changed() {
    for kept in [
        "https://cdn.test/a.png",
        "http://site.test/favicon.ico",
        "https://site.test/a%20b.png?token=keep&x=%2F#frag",
        "https://xn--wgv71a.test/i.png",
    ] {
        assert_eq!(clean_url(kept).as_deref(), Some(kept), "{kept}");
    }
    let long = format!("https://site.test/{}", "a".repeat(MAX_URL));
    for refused in [
        "",
        "/relative/icon.png",
        "icon.png",
        "data:image/png;base64,AAAA",
        "javascript:alert(1)",
        "blob:https://site.test/uuid",
        "file:///C:/x.png",
        "chrome://favicon/x",
        "ftp://site.test/x.png",
        "https://user:pass@site.test/x.png",
        "https://user@site.test/x.png",
        "https://site.test/a b.png",
        "https://site.test/a\nb.png",
        " https://site.test/x.png",
        "https://",
        "not a url",
        long.as_str(),
    ] {
        assert_eq!(clean_url(refused), None, "{refused}");
    }
    let edge = format!(
        "https://s.test/{}",
        "a".repeat(MAX_URL - "https://s.test/".len())
    );
    assert_eq!(clean_url(&edge).as_deref(), Some(edge.as_str()));
}

#[test]
fn a_bad_page_changes_nothing() {
    for bad in [
        json!(null),
        json!("page"),
        json!([]),
        json!({}),
        json!({ "capturedAt": 5 }),
        json!({ "capturedAt": "2026-11-05T01:02:03Z" }),
        json!({ "capturedAt": "2026-11-05 01:02:03.456Z" }),
        json!({ "capturedAt": "2026-13-05T01:02:03.456Z" }),
        json!({ "capturedAt": "2026-11-05T24:02:03.456Z" }),
        json!({ "capturedAt": "+02026-11-05T01:02:03.456Z" }),
        json!({ "capturedAt": "２026-11-05T01:02:03.456Z" }),
        page(json!({ "title": 5 })),
        page(json!({ "description": ["x"] })),
        page(json!({ "ogImageUrl": {} })),
        page(json!({ "faviconUrl": true })),
    ] {
        assert_eq!(parse(&bad), None, "{bad}");
    }
    // A URL that may not be kept is only that field's NULL, not a refusal.
    let got = parse(&page(
        json!({ "title": "T", "ogImageUrl": "javascript:x", "faviconUrl": "data:," }),
    ))
    .unwrap();
    assert_eq!(
        (got.title.as_deref(), got.og_image_url, got.favicon_url),
        (Some("T"), None, None)
    );
}

#[test]
fn timestamps_order_as_text() {
    assert!(is_timestamp(AT));
    assert!(is_timestamp("0001-01-01T00:00:00.000Z"));
    assert!(!is_timestamp("2026-11-05T01:02:60.000Z"));
    assert!(!is_timestamp("2026-11-00T01:02:03.000Z"));
    assert!("2026-11-05T01:02:03.456Z" < "2026-11-05T01:02:03.457Z");
}

#[test]
fn the_shared_vectors_agree_with_the_extension() {
    let vectors: Value =
        serde_json::from_str(include_str!("../../../tests/page-metadata/vectors.json")).unwrap();
    for case in vectors["text"].as_array().unwrap() {
        let (raw, limit) = (case[0].as_str().unwrap(), case[1].as_u64().unwrap());
        assert_eq!(
            clean_text(raw, limit as usize).as_deref(),
            case[2].as_str(),
            "{case}"
        );
    }
    for case in vectors["url"].as_array().unwrap() {
        assert_eq!(
            clean_url(case[0].as_str().unwrap()).as_deref(),
            case[1].as_str(),
            "{case}"
        );
    }
}
