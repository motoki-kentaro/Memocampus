//! Synthetic data and in-memory or temporary vaults only.
use super::*;
use crate::vault::{tests::TempDir, ConfigLock};
use memocampus_native_host::schema;

fn db() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    db.execute_batch(&schema::create_sql()).unwrap();
    db
}
fn visit(db: &Connection, id: &str, url: &str, title: &str, at: &str) {
    db.execute(
        "INSERT INTO visits VALUES (?1, ?2, ?3, ?4)",
        params![id, url, title, at],
    )
    .unwrap();
}
const NAV: &str = "INSERT INTO navigation_events (target_visit_id, source_visit_id, kind, transition_type, client_redirect, server_redirect, forward_back, from_address_bar, new_tab, occurred_at, source_censored)";
fn navigation(db: &Connection, target: &str, source: Option<&str>, censored: i64) {
    db.execute(
        &format!("{NAV} VALUES (?1, ?2, 'document', 'link', 0, 0, 0, 0, 0, '2026-11-17T01:00:00.000Z', ?3)"),
        params![target, source, censored],
    )
    .unwrap();
}
const DAY_FROM: &str = "2026-11-16T15:00:00.000Z";
const DAY_TO: &str = "2026-11-17T15:00:00.000Z";
fn day(n: u32) -> String {
    format!("2026-11-17T{:02}:{:02}:00.000Z", n / 60 % 24, n % 60)
}
fn list_with(db: &Connection, extra: Value) -> Result<Value, &'static str> {
    let mut message = json!({"type": "history_list", "requestId": "r", "visitedFrom": DAY_FROM, "visitedTo": DAY_TO});
    message
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    let request = parse_list(&message)?;
    run_list(db, &request)
}
fn ids(list: &Value) -> Vec<String> {
    list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_owned())
        .collect()
}
fn days_with(db: &Connection, bounds: &[String], extra: Value) -> Result<Value, &'static str> {
    let mut message = json!({"type": "history_days", "requestId": "r", "bounds": bounds});
    message
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    let request = parse_days(&message)?;
    run_days(db, &request)
}

#[test]
fn empty_vault_is_an_empty_list_and_no_days() {
    let db = db();
    let list = list_with(&db, json!({})).unwrap();
    assert_eq!(
        (
            list["total"].clone(),
            list["pages"].clone(),
            list["items"].clone()
        ),
        (json!(0), json!(0), json!([]))
    );
    assert_eq!(
        (list["page"].clone(), list["size"].clone()),
        (json!(1), json!(50)),
        "defaults"
    );
}

#[test]
fn one_visit_is_one_item_newest_first_ties_by_event_id_and_snapshots_are_no_visits() {
    let db = db();
    visit(
        &db,
        "a",
        "https://x.test/p",
        "first",
        "2026-11-17T01:00:00.000Z",
    );
    visit(
        &db,
        "b",
        "https://x.test/p",
        "same url again",
        "2026-11-17T02:00:00.000Z",
    );
    visit(
        &db,
        "c",
        "https://x.test/q",
        "tie one",
        "2026-11-17T03:00:00.000Z",
    );
    visit(
        &db,
        "d",
        "https://x.test/q",
        "tie two",
        "2026-11-17T03:00:00.000Z",
    );
    db.execute("INSERT INTO tab_snapshots VALUES ('s', 1, 0, 'https://x.test/p', 'snap', 0, 0, '2026-11-17T04:00:00.000Z')", []).unwrap();
    for filter in [
        json!({}),
        json!({"domain": "x.test"}),
        json!({"domain": "^x\\.", "mode": "regex"}),
    ] {
        let list = list_with(&db, filter).unwrap();
        assert_eq!(ids(&list), ["d", "c", "b", "a"]);
        assert_eq!(list["total"], 4);
    }
    // The same URL twice stays two items, each with its own time.
    let list = list_with(&db, json!({})).unwrap();
    assert_eq!(list["items"][2]["visitedAt"], "2026-11-17T02:00:00.000Z");
    assert_eq!(list["items"][3]["visitedAt"], "2026-11-17T01:00:00.000Z");
    assert_eq!(list["items"][0]["host"], "x.test");
}

#[test]
fn the_lower_bound_is_in_the_upper_bound_is_out_and_invalid_times_are_nowhere() {
    let db = db();
    visit(&db, "in-from", "https://x.test/", "", DAY_FROM);
    visit(
        &db,
        "last",
        "https://x.test/",
        "",
        "2026-11-17T14:59:59.999Z",
    );
    visit(&db, "at-to", "https://x.test/", "", DAY_TO);
    visit(
        &db,
        "before",
        "https://x.test/",
        "",
        "2026-11-16T14:59:59.999Z",
    );
    for (id, at) in [
        ("feb30", "2026-02-30T00:00:00.000Z"),
        ("garbage", "yesterday"),
        ("h24", "2026-11-17T24:00:00.000Z"),
        ("short", "2026-11-17T01:00:00Z"),
    ] {
        visit(&db, id, "https://x.test/", "", at);
    }
    // A text that sorts inside the range but is no instant.
    visit(
        &db,
        "sorted-in",
        "https://x.test/",
        "",
        "2026-11-17T05:00:00.000Zjunk",
    );
    let list = list_with(&db, json!({})).unwrap();
    assert_eq!(ids(&list), ["last", "in-from"]);
    assert_eq!(list["total"], 2);
}

#[test]
fn paging_has_exact_totals_and_a_page_past_the_end_is_empty_not_an_error() {
    let db = db();
    for n in 0..120 {
        visit(&db, &format!("v{n:03}"), "https://x.test/", "", &day(n));
    }
    for (size, pages, last) in [(25u64, 5u64, 20usize), (50, 3, 20), (100, 2, 20)] {
        for filter in [json!({}), json!({"domain": "x.test"})] {
            let mut request = filter.clone();
            request["size"] = json!(size);
            request["page"] = json!(1);
            let first = list_with(&db, request.clone()).unwrap();
            assert_eq!(
                (first["total"].clone(), first["pages"].clone()),
                (json!(120), json!(pages)),
                "size {size}"
            );
            assert_eq!(first["items"].as_array().unwrap().len() as u64, size);
            request["page"] = json!(pages);
            assert_eq!(
                list_with(&db, request.clone()).unwrap()["items"]
                    .as_array()
                    .unwrap()
                    .len(),
                last
            );
            request["page"] = json!(pages + 1);
            let beyond = list_with(&db, request.clone()).unwrap();
            assert_eq!(
                (
                    beyond["items"].clone(),
                    beyond["total"].clone(),
                    beyond["pages"].clone()
                ),
                (json!([]), json!(120), json!(pages))
            );
            request["page"] = json!(u64::MAX);
            assert_eq!(
                list_with(&db, request).unwrap()["items"],
                json!([]),
                "an extreme page is empty too"
            );
        }
    }
    // Pages are one run of the order, never overlapping.
    let mut seen = Vec::new();
    for page in 1..=5 {
        seen.extend(ids(
            &list_with(&db, json!({"size": 25, "page": page})).unwrap()
        ));
    }
    let expected: Vec<String> = (0..120).rev().map(|n| format!("v{n:03}")).collect();
    assert_eq!(seen, expected);
}

#[test]
fn bad_page_size_and_range_are_refused() {
    let db = db();
    for (extra, code) in [
        (json!({"page": 0}), "invalid_page"),
        (json!({"page": -1}), "invalid_page"),
        (json!({"page": "2"}), "invalid_page"),
        (json!({"page": 1.5}), "invalid_page"),
        (json!({"size": 10}), "invalid_size"),
        (json!({"size": 0}), "invalid_size"),
        (json!({"size": "50"}), "invalid_size"),
        (json!({"size": 101}), "invalid_size"),
        (
            json!({"visitedFrom": DAY_TO, "visitedTo": DAY_FROM}),
            INVALID_BOUNDS,
        ),
        (
            json!({"visitedFrom": DAY_FROM, "visitedTo": DAY_FROM}),
            INVALID_BOUNDS,
        ),
        (json!({"visitedFrom": "2026-11-17"}), INVALID_BOUNDS),
        (json!({"visitedFrom": null}), INVALID_BOUNDS),
        (
            json!({"visitedFrom": "2026-11-16T00:00:00.000Z", "visitedTo": "2026-11-17T02:00:00.001Z"}),
            INVALID_BOUNDS,
        ),
        (json!({"mode": "wildcard"}), "invalid_mode"),
        (json!({"mode": 1}), "invalid_mode"),
        (json!({"domain": 5}), "invalid_domain"),
    ] {
        assert_eq!(list_with(&db, extra.clone()).unwrap_err(), code, "{extra}");
    }
}

#[test]
fn a_day_of_23_24_or_25_hours_is_one_list_and_26_hours_is_the_most() {
    let db = db();
    visit(&db, "a", "https://x.test/", "", "2026-03-08T12:00:00.000Z");
    for (from, to) in [
        ("2026-03-08T05:00:00.000Z", "2026-03-09T04:00:00.000Z"),
        ("2026-11-01T04:00:00.000Z", "2026-11-02T05:00:00.000Z"),
        ("2026-03-08T00:00:00.000Z", "2026-03-09T02:00:00.000Z"),
    ] {
        assert!(
            list_with(&db, json!({"visitedFrom": from, "visitedTo": to})).is_ok(),
            "{from}"
        );
    }
    assert_eq!(list_with(&db, json!({"visitedFrom": "2026-03-08T00:00:00.000Z", "visitedTo": "2026-03-09T02:00:00.001Z"})).unwrap_err(), INVALID_BOUNDS);
    assert_eq!(list_with(&db, json!({"visitedFrom": "2026-03-08T05:00:00.000Z", "visitedTo": "2026-03-09T04:00:00.000Z"})).unwrap()["total"], 1);
}

#[test]
fn exact_domain_is_the_whole_host_without_case_and_trailing_dots() {
    let db = db();
    for (id, url) in [
        ("1", "https://github.com/a"),
        ("2", "https://GitHub.com./b"),
        ("3", "https://gist.github.com/c"),
        ("4", "https://notgithub.com/d"),
        ("5", "https://github.com.evil.test/e"),
        ("6", "https://github.com:8443/f"),
        ("7", "https://user@github.com/g"),
        ("8", "https://x.test/?next=github.com"),
        ("9", "https://日本語.jp/"),
    ] {
        visit(&db, id, url, "github.com", &day(id.parse::<u32>().unwrap()));
    }
    let hit = |domain: &str| ids(&list_with(&db, json!({"domain": domain})).unwrap());
    assert_eq!(
        hit("github.com"),
        ["7", "6", "2", "1"],
        "no subdomain, no look-alike, no path or title, with port and user"
    );
    assert_eq!(hit("GITHUB.COM"), hit("github.com"));
    assert_eq!(
        hit("  github.com.  "),
        hit("github.com"),
        "blanks and trailing dots"
    );
    assert_eq!(hit("gist.github.com"), ["3"]);
    assert_eq!(hit("日本語.jp"), ["9"], "IDN as punycode, both ways");
    assert_eq!(hit("xn--wgv71a119e.jp"), ["9"]);
    assert_eq!(
        list_with(&db, json!({"domain": "  "})).unwrap()["total"],
        9,
        "blank is every host"
    );
    assert_eq!(list_with(&db, json!({"domain": ""})).unwrap()["total"], 9);
    assert_eq!(
        list_with(&db, json!({"domain": "nothing.test"})).unwrap()["total"],
        0
    );
    for bad in [
        "github.com/path",
        "https://github.com",
        "github.com:8443",
        "a b",
        "user@github.com",
        "github.com?x",
        "github.com#x",
        &"a".repeat(513),
        "exa mple.com",
    ] {
        assert_eq!(
            list_with(&db, json!({"domain": bad})).unwrap_err(),
            "invalid_domain",
            "{bad}"
        );
    }
}

#[test]
fn regex_domain_is_a_partial_case_insensitive_match_of_the_host_only() {
    let db = db();
    for (id, url) in [
        ("1", "https://github.com/"),
        ("2", "https://gist.GitHub.com/"),
        ("3", "https://example.org/github.com"),
        ("4", "https://a.example.com/"),
        ("5", "https://b.test/?q=example.com"),
    ] {
        visit(&db, id, url, "", &day(id.parse::<u32>().unwrap()));
    }
    let hit =
        |pattern: &str| ids(&list_with(&db, json!({"domain": pattern, "mode": "regex"})).unwrap());
    assert_eq!(hit("hub"), ["2", "1"], "partial, any case, host only");
    assert_eq!(hit("^github\\.com$"), ["1"], "anchors make it whole");
    assert_eq!(
        hit("example\\.com$"),
        ["4"],
        "the query and the path are not the host"
    );
    assert_eq!(hit(".*"), ["5", "4", "3", "2", "1"]);
    assert_eq!(hit("^$"), Vec::<String>::new(), "a host is never empty");
    let refused =
        |pattern: &str| list_with(&db, json!({"domain": pattern, "mode": "regex"})).unwrap_err();
    assert_eq!(refused("("), "invalid_regex");
    assert_eq!(refused("[a-"), "invalid_regex");
    assert_eq!(refused("(?=x)a"), "unsupported_regex");
    assert_eq!(refused("(a)\\1"), "unsupported_regex");
    assert_eq!(refused(&"a".repeat(513)), "regex_too_long");
    assert_eq!(refused("(((a{100}){100}){100}){100}"), "regex_limit");
    // Never "no filter": the failed ones left the list unchanged and refused, and 512 bytes still pass.
    assert!(list_with(&db, json!({"domain": "a".repeat(512), "mode": "regex"})).is_ok());
}

#[test]
fn a_visit_without_a_host_matches_no_filter_but_is_listed_without_one() {
    let db = db();
    visit(&db, "1", "not a url", "", &day(1));
    visit(&db, "2", "https://x.test/", "", &day(2));
    assert_eq!(ids(&list_with(&db, json!({})).unwrap()), ["2", "1"]);
    assert_eq!(list_with(&db, json!({})).unwrap()["items"][1]["host"], "");
    assert_eq!(
        ids(&list_with(&db, json!({"domain": ".*", "mode": "regex"})).unwrap()),
        ["2"]
    );
    assert_eq!(
        ids(&list_with(&db, json!({"domain": "x.test"})).unwrap()),
        ["2"]
    );
}

/// The local days of a month as a person at UTC+9 (no DST) or New York (DST) has them.
fn month_bounds(year: i32, month: u32, offset_hours: i64, days: u32) -> Vec<String> {
    (0..=days)
        .map(|d| {
            let day_start = days_from_civil(year as i64, month as i64, 1 + d as i64) * 86_400_000
                - offset_hours * 3_600_000;
            iso(day_start)
        })
        .collect()
}
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}
fn iso(ms: i64) -> String {
    let (days, rest) = (ms.div_euclid(86_400_000), ms.rem_euclid(86_400_000));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3_600_000,
        rest / 60_000 % 60,
        rest / 1000 % 60,
        rest % 1000
    )
}

#[test]
fn the_days_of_a_month_agree_with_each_days_list_for_every_filter() {
    // (year, month, days, offset in hours: Tokyo, New York with its DST days (UTC-4 until 1 Nov, UTC-5 after), UTC)
    for (year, month, days, zone) in [
        (2026, 11, 30, 9i64),
        (2026, 11, 30, -5),
        (2028, 2, 29, 0),
        (2027, 2, 28, 9),
        (2026, 12, 31, 0),
        (2026, 3, 31, -4),
        (2026, 10, 31, 0),
    ] {
        let db = db();
        let bounds = month_bounds(year, month, zone, days);
        let mut n = 0;
        // Visits at, just before and just after every boundary, on several hosts, plus the first and last instants of the span.
        for (i, bound) in bounds.iter().enumerate() {
            let ms = visit_days::timestamp_ms(bound).unwrap();
            for (delta, host) in [
                (0i64, "a.test"),
                (-1, "B.test"),
                (1, "sub.a.test"),
                (3_600_000, "a.test"),
            ] {
                n += 1;
                visit(
                    &db,
                    &format!("v{n}"),
                    &format!("https://{host}/{i}"),
                    "",
                    &iso(ms + delta),
                );
            }
        }
        visit(
            &db,
            "bad",
            "https://a.test/",
            "",
            &format!("{year:04}-{month:02}-15T10:00:00.000Zx"),
        );
        for filter in [
            json!({}),
            json!({"domain": "a.test"}),
            json!({"domain": "A.TEST"}),
            json!({"domain": "b.test"}),
            json!({"domain": "^(sub\\.)?a\\.", "mode": "regex"}),
            json!({"domain": "test$", "mode": "regex"}),
            json!({"domain": "nothing", "mode": "regex"}),
        ] {
            let counts = days_with(&db, &bounds, filter.clone()).unwrap();
            let counts: Vec<u64> = counts["counts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c.as_u64().unwrap())
                .collect();
            assert_eq!(counts.len(), days as usize);
            let mut listed = 0;
            for (i, pair) in bounds.windows(2).enumerate() {
                let mut request = filter.clone();
                request["visitedFrom"] = json!(pair[0]);
                request["visitedTo"] = json!(pair[1]);
                let list = list_with(&db, request).unwrap();
                assert_eq!(
                    list["total"].as_u64().unwrap(),
                    counts[i],
                    "{year}-{month} day {} {filter}",
                    i + 1
                );
                listed += counts[i];
            }
            assert_eq!(
                days_with(&db, &bounds, filter.clone()).unwrap()["total"],
                listed
            );
        }
    }
}

#[test]
fn days_validates_bounds_and_the_filter_before_anything_and_deleted_visits_are_not_counted() {
    let db = db();
    let bounds = month_bounds(2026, 11, 9, 30);
    visit(&db, "a", "https://a.test/", "", "2026-11-10T00:00:00.000Z");
    visit(&db, "b", "https://a.test/", "", "2026-11-10T01:00:00.000Z");
    assert_eq!(days_with(&db, &bounds, json!({})).unwrap()["total"], 2);
    db.execute("DELETE FROM visits WHERE event_id = 'a'", [])
        .unwrap();
    assert_eq!(days_with(&db, &bounds, json!({})).unwrap()["total"], 1);
    assert_eq!(
        days_with(&db, &bounds, json!({"domain": "a.test"})).unwrap()["total"],
        1
    );
    assert_eq!(
        days_with(&db, &bounds[..5], json!({})).unwrap_err(),
        INVALID_BOUNDS
    );
    let mut reversed = bounds.clone();
    reversed.reverse();
    assert_eq!(
        days_with(&db, &reversed, json!({})).unwrap_err(),
        INVALID_BOUNDS
    );
    assert_eq!(
        days_with(&db, &bounds, json!({"domain": "(", "mode": "regex"})).unwrap_err(),
        "invalid_regex"
    );
    assert_eq!(
        days_with(&db, &bounds, json!({"mode": "x"})).unwrap_err(),
        "invalid_mode"
    );
}

#[test]
fn a_list_item_is_cut_on_character_boundaries_and_says_so() {
    let db = db();
    let url = format!("https://x.test/{}", "あ".repeat(5000));
    let title = "題".repeat(2000);
    visit(&db, "big", &url, &title, &day(1));
    visit(&db, "small", "https://x.test/", "short", &day(2));
    let list = list_with(&db, json!({})).unwrap();
    let (small, big) = (&list["items"][0], &list["items"][1]);
    assert_eq!(
        (
            small["urlTruncated"].clone(),
            small["titleTruncated"].clone()
        ),
        (json!(false), json!(false))
    );
    assert_eq!(
        (big["urlTruncated"].clone(), big["titleTruncated"].clone()),
        (json!(true), json!(true))
    );
    assert_eq!(big["url"].as_str().unwrap().chars().count(), LIST_URL_CHARS);
    assert_eq!(
        big["title"].as_str().unwrap().chars().count(),
        LIST_TITLE_CHARS
    );
    assert_eq!(big["host"], "x.test");
    // Exactly at the limit is not cut.
    assert_eq!(
        clip(&"a".repeat(LIST_TITLE_CHARS), LIST_TITLE_CHARS),
        ("a".repeat(LIST_TITLE_CHARS), false)
    );
    assert!(clip("あい", 1).1);
}

#[test]
fn a_page_whose_serialized_items_would_pass_the_budget_is_refused_whole() {
    let db = db();
    // Control characters become six bytes each (\u0001): 100 items of 2560 of them are about 1.5 MB.
    let url = format!("https://x.test/{}", "\u{1}".repeat(2040));
    let title = "\u{1}".repeat(LIST_TITLE_CHARS);
    for n in 0..100 {
        visit(&db, &format!("v{n:03}"), &url, &title, &day(n));
    }
    assert_eq!(list_with(&db, json!({"size": 100})).unwrap_err(), TOO_LARGE);
    assert_eq!(
        list_with(&db, json!({"size": 100, "domain": "x.test"})).unwrap_err(),
        TOO_LARGE,
        "also filtered"
    );
    // The same rows in smaller pages fit, and the totals stay exact.
    let page = list_with(&db, json!({"size": 25})).unwrap();
    assert_eq!(
        (page["total"].clone(), page["pages"].clone()),
        (json!(100), json!(4))
    );
    assert!(serde_json::to_vec(&page).unwrap().len() < 1_048_576);
}

#[test]
fn a_ten_megabyte_url_never_breaks_a_list_and_a_detail_over_the_budget_is_an_error_not_a_cut() {
    let db = db();
    let huge = format!("https://x.test/{}", "a".repeat(10 * 1024 * 1024));
    visit(&db, "huge", &huge, "t", &day(1));
    let list = list_with(&db, json!({"domain": "x.test"})).unwrap();
    assert_eq!(list["items"][0]["urlTruncated"], true);
    assert!(serde_json::to_vec(&list).unwrap().len() < BUDGET);
    assert_eq!(run_visit(&db, "huge").unwrap_err(), TOO_LARGE);
    visit(
        &db,
        "fits",
        &format!("https://x.test/{}", "a".repeat(300_000)),
        "t",
        &day(2),
    );
    let found = run_visit(&db, "fits").unwrap();
    assert_eq!(
        found["visit"]["url"].as_str().unwrap().len(),
        300_000 + 15,
        "the full URL, never cut"
    );
}

#[test]
fn the_three_states_of_a_source_are_never_mixed_up() {
    let db = db();
    for (id, url, n) in [
        ("a", "https://a.test/", 1),
        ("b", "https://b.test/", 2),
        ("c", "https://c.test/", 3),
        ("d", "https://d.test/", 4),
        ("e", "https://e.test/", 5),
        ("f", "https://f.test/", 6),
    ] {
        visit(&db, id, url, &format!("title {id}"), &day(n));
    }
    navigation(&db, "b", Some("a"), 0); // present
    navigation(&db, "c", None, 1); // deleted on purpose
    navigation(&db, "d", None, 0); // never known
    navigation(&db, "e", Some("a"), 0);
    navigation(&db, "f", Some("a"), 0);
    let state = |id: &str| run_visit(&db, id).unwrap()["source"].clone();
    let present = state("b");
    assert_eq!(present["state"], "present");
    assert_eq!(
        (
            present["visit"]["id"].clone(),
            present["visit"]["url"].clone(),
            present["visit"]["title"].clone()
        ),
        (json!("a"), json!("https://a.test/"), json!("title a"))
    );
    assert_eq!(state("c"), json!({"state": "censored"}));
    assert_eq!(
        state("d"),
        json!({"state": "unknown"}),
        "a bare NULL is unknown, never censored"
    );
    assert_eq!(
        state("a"),
        json!({"state": "unknown"}),
        "no navigation row at all: unknown, not a direct visit"
    );
    assert_eq!(run_visit(&db, "a").unwrap()["navigation"], Value::Null);
    let b = run_visit(&db, "b").unwrap();
    assert_eq!(
        (
            b["navigation"]["kind"].clone(),
            b["navigation"]["transitionType"].clone(),
            b["navigation"]["newTab"].clone()
        ),
        (json!("document"), json!("link"), json!(false))
    );
    // A foreign-key SET NULL without the marker (not what the host's deletion does) is still unknown.
    db.execute("DELETE FROM visits WHERE event_id = 'a'", [])
        .unwrap();
    assert_eq!(state("b"), json!({"state": "unknown"}));
}

#[test]
fn targets_are_every_navigation_that_names_this_visit_as_its_source() {
    let db = db();
    visit(&db, "src", "https://src.test/", "source", &day(1));
    for n in 0..5 {
        visit(
            &db,
            &format!("t{n}"),
            &format!("https://t{n}.test/"),
            &format!("target {n}"),
            &day(10 - n),
        );
        navigation(&db, &format!("t{n}"), Some("src"), 0);
    }
    visit(&db, "other", "https://other.test/", "", &day(20));
    navigation(&db, "other", None, 0);
    let found = run_visit(&db, "src").unwrap();
    let targets: Vec<&str> = found["targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap())
        .collect();
    assert_eq!(targets, ["t4", "t3", "t2", "t1", "t0"], "oldest first");
    assert_eq!(
        (
            found["targetCount"].clone(),
            found["targetsTruncated"].clone()
        ),
        (json!(5), json!(false))
    );
    assert_eq!(found["targets"][0]["transitionType"], "link");
    // A crowd of targets is cut, counted and said so.
    for n in 0..250 {
        let id = format!("m{n:03}");
        visit(&db, &id, "https://m.test/", "", &day(100 + n));
        navigation(&db, &id, Some("src"), 0);
    }
    let found = run_visit(&db, "src").unwrap();
    assert_eq!(
        (
            found["targets"].as_array().unwrap().len(),
            found["targetCount"].clone(),
            found["targetsTruncated"].clone()
        ),
        (TARGET_LIMIT, json!(255), json!(true))
    );
}

#[test]
fn a_missing_deleted_or_malformed_visit_is_not_found_or_invalid() {
    let db = db();
    visit(&db, "a", "https://a.test/", "", &day(1));
    visit(&db, "bad-time", "https://a.test/", "", "not a time");
    assert_eq!(run_visit(&db, "nope").unwrap_err(), "visit_not_found");
    assert_eq!(
        run_visit(&db, "bad-time").unwrap_err(),
        "visit_not_found",
        "no visit the list would not show"
    );
    db.execute("DELETE FROM visits WHERE event_id = 'a'", [])
        .unwrap();
    assert_eq!(run_visit(&db, "a").unwrap_err(), "visit_not_found");
    for bad in [
        json!({}),
        json!({"id": ""}),
        json!({"id": 5}),
        json!({"id": "has space"}),
        json!({"id": "x".repeat(129)}),
    ] {
        assert_eq!(parse_visit(&bad).unwrap_err(), "invalid_id", "{bad}");
    }
}

#[test]
fn a_deleted_source_leaves_no_url_title_or_id_in_the_detail() {
    let db = db();
    visit(
        &db,
        "a",
        "https://secret-source.test/private",
        "SECRET-TITLE",
        &day(1),
    );
    visit(&db, "b", "https://b.test/", "b", &day(2));
    navigation(&db, "b", Some("a"), 0);
    // What the host's deletion does: the marker and the NULL in one statement, then the visit goes.
    db.execute("UPDATE navigation_events SET source_visit_id = NULL, source_censored = 1 WHERE source_visit_id = 'a'", []).unwrap();
    db.execute("DELETE FROM visits WHERE event_id = 'a'", [])
        .unwrap();
    let shown = run_visit(&db, "b").unwrap();
    assert_eq!(shown["source"], json!({"state": "censored"}));
    let text = shown.to_string();
    assert!(
        !text.contains("secret-source")
            && !text.contains("SECRET-TITLE")
            && !text.contains("\"a\""),
        "{text}"
    );
}

#[test]
fn page_metadata_is_only_that_of_this_very_visit_and_snapshots_are_never_mixed_in() {
    let db = db();
    visit(&db, "old", "https://p.test/", "old visit", &day(1));
    visit(&db, "new", "https://p.test/", "new visit", &day(2));
    db.execute("INSERT INTO page_metadata (url, event_id, title, og_site_name, captured_at) VALUES ('https://p.test/', 'new', 'Meta', 'Site', '2026-11-17T00:02:00.000Z')", []).unwrap();
    db.execute("INSERT INTO tab_snapshots VALUES ('s', 1, 0, 'https://p.test/', 'snapshot title', 0, 0, '2026-11-17T00:03:00.000Z')", []).unwrap();
    assert_eq!(
        run_visit(&db, "old").unwrap()["pageMetadata"],
        Value::Null,
        "the URL's metadata is not this older visit's"
    );
    let found = run_visit(&db, "new").unwrap();
    assert_eq!(
        (
            found["pageMetadata"]["title"].clone(),
            found["pageMetadata"]["ogSiteName"].clone(),
            found["pageMetadata"]["description"].clone()
        ),
        (json!("Meta"), json!("Site"), Value::Null)
    );
    assert!(!found.to_string().contains("snapshot title"));
    assert!(!run_visit(&db, "old")
        .unwrap()
        .to_string()
        .contains("snapshot title"));
}

#[test]
fn the_reads_never_write() {
    let db = db();
    visit(&db, "a", "https://a.test/", "", &day(1));
    let before = db.total_changes();
    list_with(&db, json!({"domain": "a.test"})).unwrap();
    list_with(&db, json!({})).unwrap();
    days_with(&db, &month_bounds(2026, 11, 9, 30), json!({"domain": "a"})).unwrap();
    run_visit(&db, "a").unwrap();
    assert_eq!(db.total_changes(), before);
}

// ---- over a Session: requestId, the vault token, vault errors, refusals before the vault -----------------

fn session_with_vault() -> (TempDir, Session) {
    let tmp = TempDir::new();
    let env = tmp.env();
    let path = tmp.0.join("history.sqlite3");
    crate::vault::create_vault(&path).unwrap();
    let lock = ConfigLock::acquire(&env).unwrap();
    crate::vault::switch_active(&env, &lock, &path, false).unwrap();
    drop(lock);
    (tmp, Session::new(env))
}

#[test]
fn answers_carry_the_request_id_and_the_vault_token_of_the_vault_they_were_read_from() {
    let (_tmp, mut session) = session_with_vault();
    session.write(|db| db.execute("INSERT INTO visits VALUES ('a', 'https://a.test/', 'A', '2026-11-17T01:00:00.000Z')", [])).unwrap();
    let list = handle(
        &mut session,
        &json!({"type": "history_list", "requestId": "r-1", "visitedFrom": DAY_FROM, "visitedTo": DAY_TO}),
    );
    assert_eq!(
        (
            list["ok"].clone(),
            list["type"].clone(),
            list["requestId"].clone(),
            list["total"].clone()
        ),
        (
            json!(true),
            json!("history_list_result"),
            json!("r-1"),
            json!(1)
        )
    );
    let token = list["vaultToken"].as_str().unwrap().to_owned();
    assert_eq!(token.len(), 64);
    let bounds = month_bounds(2026, 11, 9, 30);
    let days = handle(
        &mut session,
        &json!({"type": "history_days", "requestId": "r-2", "bounds": bounds}),
    );
    assert_eq!(
        (
            days["type"].clone(),
            days["requestId"].clone(),
            days["vaultToken"].clone(),
            days["total"].clone()
        ),
        (
            json!("history_days_result"),
            json!("r-2"),
            json!(token),
            json!(1)
        )
    );
    let visit = handle(
        &mut session,
        &json!({"type": "history_visit", "requestId": "r-3", "id": "a"}),
    );
    assert_eq!(
        (
            visit["type"].clone(),
            visit["requestId"].clone(),
            visit["vaultToken"].clone(),
            visit["visit"]["title"].clone()
        ),
        (
            json!("history_visit_result"),
            json!("r-3"),
            json!(token),
            json!("A")
        )
    );
    let gone = handle(
        &mut session,
        &json!({"type": "history_visit", "requestId": "r-4", "id": "zzz"}),
    );
    assert_eq!(
        gone,
        json!({"ok": false, "type": "history_visit_result", "requestId": "r-4", "error": "visit_not_found"})
    );
}

#[test]
fn requests_are_refused_before_the_vault_is_opened_and_vault_errors_keep_their_codes() {
    let tmp = TempDir::new();
    let mut session = Session::new(tmp.env());
    let error = |message: Value, session: &mut Session| {
        handle(session, &message)["error"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    // No vault is configured, so a refusal of the request itself proves nothing was opened.
    for (message, code) in [
        (
            json!({"type": "history_list", "requestId": "r", "visitedFrom": DAY_FROM, "visitedTo": DAY_TO, "mode": "regex", "domain": "("}),
            "invalid_regex",
        ),
        (
            json!({"type": "history_list", "requestId": "r", "visitedFrom": DAY_FROM}),
            INVALID_BOUNDS,
        ),
        (
            json!({"type": "history_list", "requestId": "r", "visitedFrom": DAY_FROM, "visitedTo": DAY_TO, "size": 7}),
            "invalid_size",
        ),
        (
            json!({"type": "history_days", "requestId": "r", "bounds": []}),
            INVALID_BOUNDS,
        ),
        (
            json!({"type": "history_visit", "requestId": "r"}),
            "invalid_id",
        ),
        (json!({"type": "history_visit"}), "invalid_request_id"),
        (
            json!({"type": "history_other", "requestId": "r"}),
            "unknown_type",
        ),
        (
            json!({"type": "history_list", "requestId": "r", "visitedFrom": DAY_FROM, "visitedTo": DAY_TO}),
            "vault_not_configured",
        ),
        (
            json!({"type": "history_visit", "requestId": "r", "id": "a"}),
            "vault_not_configured",
        ),
    ] {
        assert_eq!(error(message.clone(), &mut session), code, "{message}");
    }
    assert!(
        !tmp.0.join("app").join("config.json").exists(),
        "nothing was created"
    );
}

// ---- measurement (ignored by default): MEMOCAMPUS_HISTORY_ROWS=100000 cargo test --release measure -- --ignored --nocapture

#[test]
#[ignore]
fn measure_history_reads() {
    use std::time::Instant;
    let rows: u64 = std::env::var("MEMOCAMPUS_HISTORY_ROWS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100_000);
    let runs: usize = 5;
    let tmp = TempDir::new();
    let path = tmp.0.join("measure.sqlite3");
    crate::vault::create_vault(&path).unwrap();
    let db = Connection::open(&path).unwrap();
    let month = month_bounds(2026, 11, 9, 30);
    let (start, end) = (
        visit_days::timestamp_ms(&month[0]).unwrap(),
        visit_days::timestamp_ms(&month[30]).unwrap(),
    );
    let hosts = [
        "github.com",
        "docs.rs",
        "news.example.org",
        "a.example.com",
        "wikipedia.org",
        "gist.github.com",
        "x.test",
    ];
    db.execute_batch("BEGIN").unwrap();
    {
        let mut insert = db
            .prepare("INSERT INTO visits VALUES (?1, ?2, ?3, ?4)")
            .unwrap();
        // Spread over three years ending with the measured month; one third of the rows fall in that month.
        for n in 0..rows {
            let at = if n % 3 == 0 {
                start + (n as i64 * 1_000_003) % (end - start)
            } else {
                start - ((n as i64 * 104_729) % (3 * 365 * 86_400_000)) - 1
            };
            insert
                .execute(params![
                    format!("00000000-0000-4000-8000-{n:012}"),
                    format!(
                        "https://{}/path/{}?q={}",
                        hosts[(n % 7) as usize],
                        n,
                        n * 31
                    ),
                    format!("Title {n}"),
                    iso(at)
                ])
                .unwrap();
        }
    }
    db.execute_batch("COMMIT").unwrap();
    let busy_day = (start + 14 * 86_400_000, start + 15 * 86_400_000);
    let in_day: i64 = db
        .query_row(
            "SELECT count(*) FROM visits WHERE visited_at >= ?1 AND visited_at < ?2",
            [iso(busy_day.0), iso(busy_day.1)],
            |r| r.get(0),
        )
        .unwrap();
    let in_month: i64 = db
        .query_row(
            "SELECT count(*) FROM visits WHERE visited_at >= ?1 AND visited_at < ?2",
            [&month[0], &month[30]],
            |r| r.get(0),
        )
        .unwrap();
    println!("rows {rows}; visits on the measured day {in_day}; in the measured month {in_month}; {runs} runs, release build, median of runs");
    let median = |mut v: Vec<u128>| {
        v.sort();
        v[v.len() / 2]
    };
    let time = |label: &str, f: &dyn Fn()| {
        let samples: Vec<u128> = (0..runs)
            .map(|_| {
                let t = Instant::now();
                f();
                t.elapsed().as_millis()
            })
            .collect();
        println!("{label}: {} ms (runs {samples:?})", median(samples.clone()));
    };
    let day = |extra: Value| {
        let mut message = json!({"type": "history_list", "requestId": "m", "visitedFrom": iso(busy_day.0), "visitedTo": iso(busy_day.1)});
        message
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let request = parse_list(&message).unwrap();
        run_list(&db, &request).unwrap();
    };
    time("1 list+total, no domain", &|| day(json!({})));
    time("1b list page 3, no domain", &|| day(json!({"page": 3})));
    time("2 list+total, exact domain", &|| {
        day(json!({"domain": "github.com"}))
    });
    time("2b list+total, regex domain", &|| {
        day(json!({"domain": "^(gist\\.)?github\\.com$", "mode": "regex"}))
    });
    let month_request = |extra: Value| {
        let mut message = json!({"type": "history_days", "requestId": "m", "bounds": month});
        message
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let request = parse_days(&message).unwrap();
        run_days(&db, &request).unwrap();
    };
    time("0 month, no domain", &|| month_request(json!({})));
    time("3 month, exact domain", &|| {
        month_request(json!({"domain": "github.com"}))
    });
    time("3b month, regex domain", &|| {
        month_request(json!({"domain": "(github|docs)\\.(com|rs)$", "mode": "regex"}))
    });
    time("4 detail by id", &|| {
        run_visit(&db, &format!("00000000-0000-4000-8000-{:012}", 3)).unwrap();
    });
}
