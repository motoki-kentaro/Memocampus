use super::*;
use serde_json::{json, Value};

const TARGET: &str = "4f1d2c3b-5a6e-4d7f-8091-a2b3c4d5e6f7";
const SOURCE: &str = "0a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d";

fn valid() -> Value {
    json!({
        "sourceVisitId": SOURCE, "kind": "document", "transitionType": "link",
        "clientRedirect": false, "serverRedirect": true, "forwardBack": false,
        "fromAddressBar": false, "newTab": true, "occurredAt": "2026-11-06T12:00:00.000Z"
    })
}

fn with(key: &str, value: Value) -> Value {
    let mut navigation = valid();
    navigation[key] = value;
    navigation
}

#[test]
fn a_complete_navigation_is_taken_as_sent() {
    assert_eq!(
        parse(&valid(), TARGET),
        Some(Navigation {
            source_visit_id: Some(SOURCE.to_owned()),
            kind: "document",
            transition_type: "link",
            client_redirect: false,
            server_redirect: true,
            forward_back: false,
            from_address_bar: false,
            new_tab: true,
            occurred_at: "2026-11-06T12:00:00.000Z".to_owned(),
        })
    );
    let none = parse(&with("sourceVisitId", Value::Null), TARGET).unwrap();
    assert_eq!(none.source_visit_id, None);
    assert_eq!(
        parse(&with("kind", json!("history_state")), TARGET)
            .unwrap()
            .kind,
        "history_state"
    );
    for transition in TRANSITION_TYPES {
        assert_eq!(
            parse(&with("transitionType", json!(transition)), TARGET)
                .unwrap()
                .transition_type,
            transition
        );
    }
    for key in [
        "clientRedirect",
        "serverRedirect",
        "forwardBack",
        "fromAddressBar",
        "newTab",
    ] {
        assert!(parse(&with(key, json!(true)), TARGET).is_some(), "{key}");
        assert!(parse(&with(key, json!(false)), TARGET).is_some(), "{key}");
    }
}

#[test]
fn anything_else_is_refused_whole_and_never_mapped() {
    let mut cases: Vec<(String, Value)> = vec![
        ("null".into(), Value::Null),
        ("array".into(), json!([valid()])),
        ("string".into(), json!("document")),
        ("empty object".into(), json!({})),
    ];
    for key in KEYS {
        let mut missing = valid();
        missing.as_object_mut().unwrap().remove(key);
        cases.push((format!("missing {key}"), missing));
    }
    let mut extra = valid();
    extra["sourceUrl"] = json!("https://a.test/");
    cases.push(("extra key".into(), extra));
    let bad: [(&str, Value); 26] = [
        ("sourceVisitId", json!("")),
        ("sourceVisitId", json!(" ")),
        ("sourceVisitId", json!("a b")),
        ("sourceVisitId", json!("ünicode")),
        ("sourceVisitId", json!("x\u{0}")),
        ("sourceVisitId", json!("x".repeat(MAX_VISIT_ID + 1))),
        ("sourceVisitId", json!(42)),
        ("sourceVisitId", json!(TARGET)),
        ("kind", json!("fragment")),
        ("kind", json!("Document")),
        ("kind", Value::Null),
        ("transitionType", json!("auto_toplevel")),
        ("transitionType", json!("LINK")),
        ("transitionType", json!("")),
        ("transitionType", json!(1)),
        ("clientRedirect", json!(0)),
        ("serverRedirect", json!("false")),
        ("forwardBack", Value::Null),
        ("fromAddressBar", json!(1)),
        ("newTab", json!("true")),
        ("occurredAt", json!("2026-11-06T12:00:00Z")),
        ("occurredAt", json!("2026-11-06 12:00:00.000Z")),
        ("occurredAt", json!("2026-02-30T12:00:00.000Z")),
        ("occurredAt", json!("2026-13-01T12:00:00.000Z")),
        ("occurredAt", json!(1_762_430_400_000_i64)),
        ("occurredAt", json!("2026-11-06T24:00:00.000Z")),
    ];
    for (key, value) in bad {
        cases.push((format!("{key} = {value}"), with(key, value)));
    }
    for (name, navigation) in cases {
        assert_eq!(parse(&navigation, TARGET), None, "{name}");
    }
}

#[test]
fn a_visit_id_is_printable_ascii_of_bounded_length() {
    assert!(is_visit_id("v1"));
    assert!(is_visit_id(SOURCE));
    assert!(is_visit_id(&"x".repeat(MAX_VISIT_ID)));
    for bad in ["", " ", "a b", "tab\t", "é", &"x".repeat(MAX_VISIT_ID + 1)] {
        assert!(!is_visit_id(bad), "{bad:?}");
    }
}

#[test]
fn only_days_that_exist_are_timestamps() {
    for good in [
        "2024-02-29T00:00:00.000Z",
        "2000-02-29T23:59:59.999Z",
        "2026-04-30T12:00:00.000Z",
        "2026-12-31T12:00:00.000Z",
    ] {
        assert!(is_calendar_timestamp(good), "{good}");
    }
    for bad in [
        "2026-02-29T00:00:00.000Z",
        "1900-02-29T00:00:00.000Z",
        "2026-04-31T00:00:00.000Z",
        "2026-00-10T00:00:00.000Z",
        "2026-01-00T00:00:00.000Z",
        "2026-01-01T00:60:00.000Z",
        "2026-01-01T00:00:00.000",
    ] {
        assert!(!is_calendar_timestamp(bad), "{bad}");
    }
}
