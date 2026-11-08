use super::*;
use memocampus_native_host::schema;
use rusqlite::params;

/// The inverse of `days_from_civil`.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

/// `Date#toISOString()` of a time in milliseconds.
fn iso(ms: i64) -> String {
    let (year, month, day) = civil_from_days(ms.div_euclid(86_400_000));
    let rest = ms.rem_euclid(86_400_000);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3_600_000,
        rest / 60_000 % 60,
        rest / 1000 % 60,
        rest % 1000
    )
}

/// What the extension sends for a month at a fixed UTC offset: the start of each local day and of
/// the day after the month.
fn month_bounds(year: i64, month: i64, offset_minutes: i64) -> Vec<String> {
    let days = days_in_month(year, month);
    (1..=days + 1)
        .map(|day| {
            let start = days_from_civil(year, month, 1) + day - 1;
            iso(start * 86_400_000 - offset_minutes * 60_000)
        })
        .collect()
}

fn db() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(&schema::create_sql()).unwrap();
    db
}

fn visit(db: &Connection, id: &str, url: &str, at: &str) {
    db.execute(
        "INSERT INTO visits VALUES (?1, ?2, 't', ?3)",
        params![id, url, at],
    )
    .unwrap();
}

fn days_with(counts: &[u64]) -> Vec<(usize, u64)> {
    counts
        .iter()
        .enumerate()
        .filter(|(_, count)| **count > 0)
        .map(|(index, count)| (index + 1, *count))
        .collect()
}

#[test]
fn a_timestamp_is_exactly_an_existing_utc_instant() {
    assert_eq!(timestamp_ms("1970-01-01T00:00:00.000Z"), Some(0));
    assert_eq!(
        timestamp_ms("2026-11-07T15:00:00.000Z"),
        Some(1_794_063_600_000)
    );
    for ms in [0, 951_782_400_000, 1_709_164_799_999, 1_794_063_600_123] {
        assert_eq!(timestamp_ms(&iso(ms)), Some(ms), "{}", iso(ms));
    }
    assert!(
        timestamp_ms("2024-02-29T23:59:59.999Z").is_some(),
        "a leap day"
    );
    assert!(timestamp_ms("0000-01-01T00:00:00.000Z").is_some());
    assert!(timestamp_ms("9999-12-31T23:59:59.999Z").is_some());
    for bad in [
        "",
        "2026-11-07",
        "2026-11-07T15:00:00Z",
        "2026-11-07T15:00:00.000",
        "2026-11-07T15:00:00.000z",
        "2026-11-07 15:00:00.000Z",
        "2026-11-07T15:00:00.000+09:00",
        "+02026-11-07T15:00:00.000Z",
        "2026-02-29T00:00:00.000Z",
        "2026-02-30T00:00:00.000Z",
        "2026-04-31T00:00:00.000Z",
        "2026-13-01T00:00:00.000Z",
        "2026-00-01T00:00:00.000Z",
        "2026-11-00T00:00:00.000Z",
        "2026-11-07T24:00:00.000Z",
        "2026-11-07T23:60:00.000Z",
        "2026-11-07T23:59:60.000Z",
        "２０２６-11-07T15:00:00.000Z",
        "2026-11-07T15:00:00.000Z ",
        "2026-11-07T1５:00:00.000Z",
    ] {
        assert_eq!(timestamp_ms(bad), None, "{bad:?}");
    }
}

#[test]
fn the_sql_test_of_a_stored_time_agrees_with_the_parser() {
    let db = db();
    for (index, text) in [
        "2026-11-07T15:00:00.000Z",
        "2024-02-29T23:59:59.999Z",
        "2026-02-29T00:00:00.000Z",
        "2026-02-30T00:00:00.000Z",
        "2026-04-31T12:00:00.000Z",
        "2026-11-07T24:00:00.000Z",
        "2026-11-07T23:60:00.000Z",
        "2026-11-07T23:59:60.000Z",
        "2026-13-01T00:00:00.000Z",
        "2026-11-07T15:00:00Z",
        "2026-11-07 15:00:00.000Z",
        "2026-11-07T15:00:00.000z",
        "garbage",
        "",
    ]
    .iter()
    .enumerate()
    {
        visit(&db, &index.to_string(), "https://a.test/", text);
        let valid: bool = db
            .query_row(
                &format!("SELECT EXISTS (SELECT 1 FROM visits WHERE event_id = ?1 AND {VALID_VISITED_AT})"),
                [index.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(valid, timestamp_ms(text).is_some(), "{text:?}");
    }
}

#[test]
fn bounds_are_29_to_32_increasing_instants_within_32_days() {
    let request = |bounds: Value| parse_request(&json!({ "requestId": "r", "bounds": bounds }));
    for (year, month) in [(2026, 2), (2024, 2), (2026, 4), (2026, 3)] {
        let bounds = month_bounds(year, month, 540);
        assert_eq!(request(json!(bounds)).unwrap(), bounds, "{year}-{month}");
    }
    let march = month_bounds(2026, 3, 540);
    let mut reversed = march.clone();
    reversed.swap(3, 4);
    let mut repeated = march.clone();
    repeated[4] = repeated[3].clone();
    let mut wrong_day = march.clone();
    wrong_day[5] = "2026-02-30T15:00:00.000Z".into();
    let mut too_long = march.clone();
    too_long[31] = iso(timestamp_ms(&march[0]).unwrap() + MAX_SPAN_MS + 1);
    let mut number = json!(march.clone());
    number[2] = json!(1_772_000_000_000_i64);
    for bad in [
        json!(null),
        json!("2026-03-01T00:00:00.000Z"),
        json!({}),
        json!(march[..28]),
        json!([
            march.clone(),
            vec![iso(timestamp_ms(&march[31]).unwrap() + 1)]
        ]
        .concat()),
        json!(reversed),
        json!(repeated),
        json!(wrong_day),
        json!(too_long),
        number,
        json!(march.iter().map(|b| format!("{b} ")).collect::<Vec<_>>()),
        json!(vec!["x".repeat(100_000); 30]),
    ] {
        assert_eq!(request(bad.clone()), Err(INVALID_BOUNDS), "{bad}");
    }
    assert_eq!(
        parse_request(&json!({ "requestId": "r" })),
        Err(INVALID_BOUNDS)
    );
    // The span may be 32 days, not more (a month of local days, and a little room).
    let first = timestamp_ms(&march[0]).unwrap();
    let edge: Vec<&str> = vec![&march[0], &march[1]];
    assert!(checked_bounds(&edge).is_ok());
    let span = [iso(first), iso(first + MAX_SPAN_MS)];
    assert!(checked_bounds(&[&span[0], &span[1]]).is_ok());
}

#[test]
fn visits_are_counted_by_the_local_day_their_instant_falls_in() {
    let db = db();
    // November 2026 in Tokyo (UTC+9): Nov 1 starts at Oct 31 15:00 UTC.
    let bounds = month_bounds(2026, 11, 540);
    assert_eq!(bounds.len(), 31);
    assert_eq!(bounds[0], "2026-10-31T15:00:00.000Z");
    assert_eq!(bounds[30], "2026-11-30T15:00:00.000Z");
    for (id, url, at) in [
        ("before", "https://a.test/", "2026-10-31T14:59:59.999Z"), // Oct 31 in Tokyo
        ("first", "https://a.test/", "2026-10-31T15:00:00.000Z"),  // Nov 1 00:00 exactly
        ("same-url", "https://a.test/", "2026-10-31T23:00:00.000Z"), // Nov 1 again, same URL
        ("other", "https://b.test/", "2026-11-01T14:59:59.999Z"),  // Nov 1 23:59:59.999
        ("utc-day-7", "https://c.test/", "2026-11-07T16:00:00.000Z"), // Nov 8 in Tokyo
        ("last", "https://a.test/", "2026-11-30T14:59:59.999Z"),   // Nov 30 23:59:59.999
        ("after", "https://a.test/", "2026-11-30T15:00:00.000Z"),  // Dec 1 00:00
    ] {
        visit(&db, id, url, at);
    }
    db.execute(
        "INSERT INTO tab_snapshots VALUES ('s', 1, 0, 'https://a.test/', 't', 0, 0, '2026-11-02T03:00:00.000Z')",
        [],
    )
    .unwrap();
    let counts = count_visits(&db, &bounds).unwrap();
    assert_eq!(counts.len(), 30);
    assert_eq!(
        days_with(&counts),
        [(1, 3), (8, 1), (30, 1)],
        "a snapshot is no visit"
    );
}

#[test]
fn a_month_with_a_daylight_saving_change_is_counted_by_the_bounds_given() {
    let db = db();
    // March 2026 in New York, as the extension computes it: days start at 05:00 UTC until March 8
    // (EST), then at 04:00 UTC (EDT); March 8 is 23 hours long.
    let bounds: Vec<String> = (1..=32)
        .map(|day| {
            let start = days_from_civil(2026, 3, 1) + day - 1;
            iso(start * 86_400_000 + if day <= 8 { 5 } else { 4 } * 3_600_000)
        })
        .collect();
    assert_eq!(bounds[7], "2026-03-08T05:00:00.000Z");
    assert_eq!(bounds[8], "2026-03-09T04:00:00.000Z");
    assert!(parse_request(&json!({ "bounds": bounds })).is_ok());
    for (id, at) in [
        ("a", "2026-03-09T03:59:59.999Z"), // March 8, 23:59 EDT
        ("b", "2026-03-09T04:00:00.000Z"), // March 9, 00:00 EDT
        ("c", "2026-03-08T04:59:59.999Z"), // March 7, 23:59 EST
    ] {
        visit(&db, id, "https://a.test/", at);
    }
    let counts = count_visits(&db, &bounds).unwrap();
    assert_eq!(days_with(&counts), [(7, 1), (8, 1), (9, 1)]);
}

#[test]
fn a_stored_time_that_is_no_valid_instant_is_counted_nowhere_and_kept() {
    let db = db();
    let bounds = month_bounds(2026, 2, 0);
    for (id, at) in [
        ("ok", "2026-02-10T10:00:00.000Z"),
        ("feb30", "2026-02-30T10:00:00.000Z"),
        ("hour24", "2026-02-10T24:00:00.000Z"),
        ("minute60", "2026-02-10T10:60:00.000Z"),
        ("no-millis", "2026-02-10T10:00:00Z"),
        ("space", "2026-02-10 10:00:00.000Z"),
        ("offset", "2026-02-10T10:00:00.000+09:00"),
        ("garbage", "2026-02-1x"),
    ] {
        visit(&db, id, "https://a.test/", at);
    }
    let counts = count_visits(&db, &bounds).unwrap();
    assert_eq!(days_with(&counts), [(10, 1)]);
    let rows: i64 = db
        .query_row("SELECT count(*) FROM visits", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 8, "nothing is changed");
    // Many of them: still an answer, no panic.
    let tx = db.unchecked_transaction().unwrap();
    for n in 0..5000 {
        visit(
            &tx,
            &format!("bad{n}"),
            "https://a.test/",
            "2026-02-1\u{0}x",
        );
    }
    tx.commit().unwrap();
    assert_eq!(days_with(&count_visits(&db, &bounds).unwrap()), [(10, 1)]);
}

#[test]
fn answers_have_the_result_type_the_request_id_and_never_a_visit() {
    let message = json!({ "type": "visit_days", "requestId": "r1" });
    assert_eq!(
        success(&message, "t".repeat(64), vec![0, 2]),
        json!({ "ok": true, "type": "visit_days_result", "requestId": "r1", "vaultToken": "t".repeat(64), "counts": [0, 2] })
    );
    assert_eq!(
        failure(&message, INVALID_BOUNDS),
        json!({ "ok": false, "type": "visit_days_result", "requestId": "r1", "error": "invalid_bounds" })
    );
}
