//! Slice 2: search index tests (report XIV). Synthetic text and the Chrome fixtures only; every
//! test checks HIT/MISS explicitly, never only that a statement ran.

use page_fulltext::index::{
    probe_fts5, CandidateQuery, Detail, Index, IndexError, Method, Options, Outcome, PageText,
    SearchMode,
};
use page_fulltext::limits::{ErrorCode, Limits};
use page_fulltext::quality::GateConfig;
use page_fulltext::{extract, Extraction};
use std::path::PathBuf;

fn index() -> Index {
    Index::open_in_memory(Options::default()).unwrap()
}

fn put(ix: &mut Index, key: &str, title: Option<&str>, body: &str) -> i64 {
    ix.put_page(
        &PageText {
            key,
            url: None,
            title,
            body,
        },
        "ok",
        "{}",
    )
    .unwrap()
    .1
}

/// Keys of the pages that HIT, checked to be the same with every search mode.
fn hits(ix: &Index, q: &str) -> Vec<String> {
    let auto = ix.search(q, SearchMode::Auto).unwrap();
    let full = ix.search(q, SearchMode::FullScan).unwrap();
    let verify = ix.search(q, SearchMode::Verify).unwrap();
    let keys = |r: &page_fulltext::index::SearchResult| {
        r.hits
            .iter()
            .map(|h| h.page_key.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(keys(&auto), keys(&full), "auto vs full scan for {q:?}");
    assert_eq!(keys(&verify), keys(&full), "verify vs full scan for {q:?}");
    keys(&auto)
}

fn assert_hit(ix: &Index, q: &str, expected: &[&str]) {
    assert_eq!(hits(ix, q), expected, "query {q:?}");
}

// ------------------------------------------------------------------ the specification table

#[test]
fn s_hokke_table_from_the_specification() {
    let mut ix = index();
    put(&mut ix, "p1", None, "今日は𩸽を焼いて食べた。");
    for (q, hit) in [
        ("𩸽", true),
        ("𩸽を", true),
        ("𩸽を焼", true),
        ("焼いて", true),
        ("鮭", false),
        ("𩸽𩸽", false),
    ] {
        let expected: &[&str] = if hit { &["p1"] } else { &[] };
        assert_hit(&ix, q, expected);
    }
    // 𩸽 is one Unicode scalar value (4 UTF-8 bytes, 2 UTF-16 units): a 1-character query.
    assert_eq!("𩸽".len(), 4);
    assert_eq!("𩸽".encode_utf16().count(), 2);
    let r = ix.search("𩸽", SearchMode::Auto).unwrap();
    assert_eq!(
        (r.query_chars, r.method, r.fallback),
        (1, Method::FullScan, Some("short_query"))
    );
    let r = ix.search("𩸽を焼", SearchMode::Auto).unwrap();
    assert_eq!((r.query_chars, r.method), (3, Method::FtsCandidates));
    // A 3-character query that is 2 + 4 + 3 bytes is not split on bytes or UTF-16 units.
    assert_hit(&ix, "は𩸽を", &["p1"]);
    assert_hit(&ix, "𩸽は", &[]);
}

#[test]
fn s_japanese_one_two_and_more_characters() {
    let mut ix = index();
    put(
        &mut ix,
        "a",
        None,
        "東京都内のひらがなとカタカナ、漢字の文章です。",
    );
    put(&mut ix, "b", None, "大阪のカタログ");
    assert_hit(&ix, "の", &["a", "b"]);
    assert_hit(&ix, "京", &["a"]);
    assert_hit(&ix, "カタ", &["a", "b"]);
    assert_hit(&ix, "東京", &["a"]);
    assert_hit(&ix, "京東", &[]);
    assert_hit(&ix, "東京都内", &["a"]);
    assert_hit(&ix, "カタカナ", &["a"]);
    assert_hit(&ix, "カタログ", &["b"]);
    assert_hit(&ix, "ひらがなとカタカナ、漢字", &["a"]);
    assert_hit(&ix, "ひらがなと カタカナ", &[]);
}

#[test]
fn s_hokke_repeated_and_in_longer_queries() {
    let mut ix = index();
    put(&mut ix, "double", None, "𩸽𩸽の干物");
    put(&mut ix, "spaced", None, "𩸽 𩸽");
    put(&mut ix, "single", Some("𩸽"), "ほっけ");
    assert_hit(&ix, "𩸽", &["double", "spaced", "single"]);
    assert_hit(&ix, "𩸽𩸽", &["double"]);
    assert_hit(&ix, "𩸽𩸽𩸽", &[]);
    assert_hit(&ix, "𩸽𩸽の", &["double"]);
    assert_hit(&ix, "𩸽 𩸽", &["spaced"]);
    assert_hit(&ix, "の干物", &["double"]);
    let r = ix.search("𩸽", SearchMode::Auto).unwrap();
    let single = r.hits.iter().find(|h| h.page_key == "single").unwrap();
    assert!(single.in_title && !single.in_body);
}

#[test]
fn s_ascii_and_japanese_mixed() {
    let mut ix = index();
    put(&mut ix, "a", None, "RustでSQLiteを使う 2026年版");
    assert_hit(&ix, "でSQ", &["a"]);
    assert_hit(&ix, "Rustで", &["a"]);
    assert_hit(&ix, "SQLiteを使う", &["a"]);
    assert_hit(&ix, "2026年", &["a"]);
    assert_hit(&ix, "Rust で", &[]);
}

// ------------------------------------------------------------------ exact match: no folding

#[test]
fn s_case_is_significant() {
    let mut ix = index();
    put(&mut ix, "a", None, "Rust and SQLite. Ärger Σίσυφος");
    assert_hit(&ix, "Rust", &["a"]);
    assert_hit(&ix, "rust", &[]);
    assert_hit(&ix, "RUST", &[]);
    assert_hit(&ix, "sqlite", &[]);
    assert_hit(&ix, "Q", &["a"]);
    assert_hit(&ix, "q", &[]);
    assert_hit(&ix, "Ärger", &["a"]);
    assert_hit(&ix, "ärger", &[]);
    assert_hit(&ix, "ΣΊΣΥΦΟΣ", &[]);
}

#[test]
fn s_nfc_and_nfd_are_different() {
    let mut ix = index();
    put(&mut ix, "nfc", None, "がっこう café");
    put(&mut ix, "nfd", None, "か\u{3099}っこう cafe\u{301}");
    assert_hit(&ix, "が", &["nfc"]);
    assert_hit(&ix, "か\u{3099}", &["nfd"]);
    // The base character alone is a substring of the decomposed form (code point semantics).
    assert_hit(&ix, "か", &["nfd"]);
    assert_hit(&ix, "がっこう", &["nfc"]);
    assert_hit(&ix, "か\u{3099}っこう", &["nfd"]);
    assert_hit(&ix, "café", &["nfc"]);
    assert_hit(&ix, "cafe\u{301}", &["nfd"]);
    assert_hit(&ix, "cafe", &["nfd"]);
}

#[test]
fn s_fullwidth_and_halfwidth_are_different() {
    let mut ix = index();
    put(&mut ix, "full", None, "ＡＢＣ１２３カタカナ");
    put(&mut ix, "half", None, "ABC123ｶﾀｶﾅ");
    assert_hit(&ix, "ABC", &["half"]);
    assert_hit(&ix, "ＡＢＣ", &["full"]);
    assert_hit(&ix, "カタカナ", &["full"]);
    assert_hit(&ix, "ｶﾀｶﾅ", &["half"]);
    assert_hit(&ix, "1", &["half"]);
    assert_hit(&ix, "１", &["full"]);
}

#[test]
fn s_emoji() {
    let mut ix = index();
    put(&mut ix, "a", None, "いいね👍🏽です 家族👨‍👩‍👧 旗🇯🇵 🦀🦀🦀");
    put(&mut ix, "b", None, "いいね👍です");
    assert_hit(&ix, "👍🏽", &["a"]);
    // Code point semantics: the base emoji is a substring of the modified one.
    assert_hit(&ix, "👍", &["a", "b"]);
    assert_hit(&ix, "👍で", &["b"]);
    assert_hit(&ix, "👍🏽で", &["a"]);
    assert_hit(&ix, "👨‍👩‍👧", &["a"]);
    assert_hit(&ix, "🇯🇵", &["a"]);
    assert_hit(&ix, "🦀🦀🦀", &["a"]);
    assert_hit(&ix, "🦀🦀🦀🦀", &[]);
    assert_hit(&ix, "🍣", &[]);
}

// ------------------------------------------------------------------ special characters

#[test]
fn s_sql_special_characters_are_plain_text() {
    let mut ix = index();
    put(
        &mut ix,
        "a",
        None,
        "割引100%_off it's \"quoted\" a\\b; -- comment /* x */",
    );
    put(&mut ix, "b", None, "割引100x off its axb");
    for (q, expected) in [
        ("%", &["a"][..]),
        ("_", &["a"]),
        ("100%", &["a"]),
        ("%_o", &["a"]),
        ("a_b", &[]),
        ("ax", &["b"]),
        ("it's", &["a"]),
        ("'", &["a"]),
        ("\"quoted\"", &["a"]),
        ("a\\b", &["a"]),
        ("; --", &["a"]),
        ("/* x */", &["a"]),
        ("'; DROP TABLE pages; --", &[]),
        ("' OR '1'='1", &[]),
    ] {
        assert_hit(&ix, q, expected);
    }
    assert_eq!(ix.page_count().unwrap(), 2);
}

#[test]
fn s_fts5_and_glob_special_characters_are_plain_text() {
    let mut ix = index();
    put(
        &mut ix,
        "a",
        None,
        "x*y a?c [ab] f(x) -minus \"q\" col:val ^start NEAR(a b) AND OR NOT 東京*",
    );
    put(
        &mut ix,
        "b",
        None,
        "xzy abc a ab f x minus col val start NEAR a b 東京都",
    );
    for (q, expected) in [
        ("x*y", &["a"][..]),
        ("a?c", &["a"]),
        ("[ab]", &["a"]),
        ("[ab", &["a"]),
        ("ab]", &["a"]),
        ("f(x)", &["a"]),
        ("-minus", &["a"]),
        ("\"q\"", &["a"]),
        ("col:val", &["a"]),
        ("^start", &["a"]),
        ("NEAR(a b)", &["a"]),
        ("AND OR NOT", &["a"]),
        ("東京*", &["a"]),
        ("東京都", &["b"]),
        ("abc", &["b"]),
        ("***", &[]),
        ("???", &[]),
        ("[[[", &[]),
        ("\"\"\"", &[]),
    ] {
        assert_hit(&ix, q, expected);
    }
    // These go through FTS5 (3+ characters, no fallback).
    let r = ix.search("x*y", SearchMode::Auto).unwrap();
    assert_eq!((r.method, r.fallback), (Method::FtsCandidates, None));
}

#[test]
fn s_empty_long_and_oversized_queries() {
    let mut ix = index();
    // 600 characters, 1,900 bytes (the limit is 4,096 bytes; the doubled query is 3,800).
    let long: String = "長い文字列🦀".repeat(100);
    put(&mut ix, "a", None, &format!("前{long}後"));
    assert_eq!(
        ix.search("", SearchMode::Auto).unwrap_err(),
        IndexError::EmptyQuery
    );
    assert_hit(&ix, &long, &["a"]);
    assert_hit(&ix, &format!("前{long}後"), &["a"]);
    assert_hit(&ix, &format!("{long}{long}"), &[]);
    let huge = "あ".repeat(2000);
    assert_eq!(
        ix.search(&huge, SearchMode::Auto).unwrap_err(),
        IndexError::QueryTooLarge
    );
}

#[test]
fn s_newlines_and_spaces() {
    let mut ix = index();
    // The extractor separates blocks with "\n" and collapses other whitespace to one space.
    put(&mut ix, "a", None, "東京\n都内 の 天気");
    assert_hit(&ix, "東京\n都内", &["a"]);
    assert_hit(&ix, "京\n都", &["a"]);
    assert_hit(&ix, "\n", &["a"]);
    assert_hit(&ix, "東京都内", &[]);
    assert_hit(&ix, "東京 都内", &[]);
    assert_hit(&ix, "都内 の", &["a"]);
    assert_hit(&ix, "都内  の", &[]);
    assert_hit(&ix, " の ", &["a"]);
    assert_hit(&ix, "   ", &[]);
    assert_hit(&ix, " ", &["a"]);
}

#[test]
fn s_title_and_body_are_separate_fields() {
    let mut ix = index();
    put(&mut ix, "a", Some("題名ABC"), "本文DEF");
    put(&mut ix, "b", None, "題名ABC本文");
    let r = ix.search("題名ABC", SearchMode::Auto).unwrap();
    assert_eq!(r.page_ids().len(), 2);
    assert!(r.hits[0].in_title && !r.hits[0].in_body);
    assert!(!r.hits[1].in_title && r.hits[1].in_body);
    // No match across the title/body boundary.
    assert_hit(&ix, "ABC本文D", &[]);
    assert_hit(&ix, "ABC本文", &["b"]);
    // Results carry no body text.
    let json = serde_json::to_string(&ix.search("本文", SearchMode::Auto).unwrap()).unwrap();
    assert!(!json.contains("DEF"));
    assert!(json.contains("題名ABC"));
}

#[test]
fn s_single_page_hit_or_miss() {
    let mut ix = index();
    put(&mut ix, "a", Some("𩸽の話"), "本文");
    assert!(ix.page_hits("a", "𩸽").unwrap());
    assert!(ix.page_hits("a", "本文").unwrap());
    assert!(!ix.page_hits("a", "鮭").unwrap());
    assert!(!ix.page_hits("missing", "本文").unwrap());
}

// ------------------------------------------------------------------ register / update / delete

#[test]
fn r_register_update_delete() {
    let mut ix = index();
    let id = put(&mut ix, "a", Some("旧題"), "古い本文です");
    assert_hit(&ix, "古い本文", &["a"]);
    let (outcome, id2) = ix
        .put_page(
            &PageText {
                key: "a",
                url: Some("https://synthetic.test/a"),
                title: Some("新題"),
                body: "新しい本文です",
            },
            "ok",
            "{}",
        )
        .unwrap();
    assert_eq!((outcome, id2), (Outcome::Updated, id));
    assert_hit(&ix, "古い本文", &[]);
    assert_hit(&ix, "旧題", &[]);
    assert_hit(&ix, "新しい本文", &["a"]);
    assert_hit(&ix, "新題", &["a"]);
    let r = ix.search("新しい", SearchMode::Auto).unwrap();
    assert_eq!(r.hits[0].url.as_deref(), Some("https://synthetic.test/a"));
    assert!(ix.delete("a").unwrap());
    assert!(!ix.delete("a").unwrap());
    assert_hit(&ix, "新しい本文", &[]);
    assert_hit(&ix, "新", &[]);
    assert_eq!(ix.page_count().unwrap(), 0);
    assert!(ix.check().unwrap().fts_integrity_ok);
}

#[test]
fn r_reregistration_does_not_duplicate() {
    let mut ix = index();
    let id = put(&mut ix, "a", None, "同じページ");
    for _ in 0..3 {
        assert_eq!(put(&mut ix, "a", None, "同じページ"), id);
    }
    put(&mut ix, "b", None, "別のページ");
    assert_eq!(ix.page_count().unwrap(), 2);
    assert_hit(&ix, "同じページ", &["a"]);
    assert_hit(&ix, "ページ", &["a", "b"]);
    assert!(ix.check().unwrap().fts_integrity_ok);
}

#[test]
fn r_invalid_keys_and_text_are_refused() {
    let mut ix = index();
    for key in ["", "a\nb", &"k".repeat(2000)] {
        let e = ix
            .put_page(
                &PageText {
                    key,
                    url: None,
                    title: None,
                    body: "x",
                },
                "ok",
                "{}",
            )
            .unwrap_err();
        assert_eq!(e, IndexError::InvalidKey);
    }
    let e = ix
        .put_page(
            &PageText {
                key: "a",
                url: None,
                title: None,
                body: "a\0b",
            },
            "ok",
            "{}",
        )
        .unwrap_err();
    assert_eq!(e, IndexError::InvalidText);
    assert_eq!(ix.page_count().unwrap(), 0);
}

fn temp_db(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("search-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(format!("{name}-{}.sqlite3", std::process::id()));
    for s in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{s}", p.display()));
    }
    p
}

#[test]
fn r_close_and_reopen() {
    let path = temp_db("reopen");
    {
        let mut ix = Index::open_or_create(&path, Options::default()).unwrap();
        put(&mut ix, "a", Some("𩸽"), "今日は𩸽を焼いて食べた。");
        put(&mut ix, "b", None, "鮭を焼いた");
        assert_hit(&ix, "焼い", &["a", "b"]);
    }
    let mut ix = Index::open_or_create(&path, Options::default()).unwrap();
    assert_hit(&ix, "𩸽", &["a"]);
    assert_hit(&ix, "焼い", &["a", "b"]);
    assert_hit(&ix, "𩸽を焼", &["a"]);
    assert_hit(&ix, "鮭を焼", &["b"]);
    assert!(ix.check().unwrap().fts_integrity_ok);
    // A database with another schema version is refused, not used.
    ix.connection()
        .execute(
            "UPDATE meta SET value = '0' WHERE key = 'schema_version'",
            [],
        )
        .unwrap();
    drop(ix);
    assert!(matches!(
        Index::open_existing(&path, Options::default()),
        Err(IndexError::SchemaMismatch(_))
    ));
}

#[test]
fn r_other_databases_are_refused_and_not_modified() {
    // A SQLite file that this PoC did not create (for example a Vault) is never used.
    let path = temp_db("foreign");
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch("CREATE TABLE pages(id INTEGER PRIMARY KEY, data BLOB); INSERT INTO pages(data) VALUES (x'00');")
            .unwrap();
    }
    let before = std::fs::read(&path).unwrap();
    let e = Index::open_existing(&path, Options::default())
        .err()
        .expect("refused");
    assert!(matches!(e, IndexError::NotAPageIndex(_)), "{e}");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "the file was modified"
    );
    assert!(!PathBuf::from(format!("{}-wal", path.display())).exists());
    // A file that is not SQLite at all is refused too.
    let junk = temp_db("junk");
    std::fs::write(
        &junk,
        b"not a database file, just bytes........................................................",
    )
    .unwrap();
    assert!(Index::open_existing(&junk, Options::default()).is_err());
}

#[test]
fn r_rebuild() {
    let mut ix = index();
    put(&mut ix, "a", Some("題"), "再構築の確認");
    put(&mut ix, "b", None, "別の本文");
    ix.delete("b").unwrap();
    let report = ix.rebuild().unwrap();
    assert!(report.fts_integrity_ok && report.sqlite_integrity_ok);
    assert_eq!(report.pages, 1);
    assert_hit(&ix, "再構築の", &["a"]);
    assert_hit(&ix, "別の本文", &[]);
}

#[test]
fn r_index_damage_is_detected_and_searches_fall_back() {
    let mut ix = index();
    put(&mut ix, "a", None, "壊れた索引の確認用");
    put(&mut ix, "b", None, "正常なページ");
    // Remove page a's entries from the FTS5 index only (the content row stays): FTS5 can no
    // longer find it.
    ix.connection()
        .execute(
            "INSERT INTO pages_fts(pages_fts, rowid, title, body)
             SELECT 'delete', page_id, title, body FROM pages WHERE page_key = 'a'",
            [],
        )
        .unwrap();
    // Before a check, the FTS5 path misses the page; Verify mode reports it.
    assert!(ix.search_fts_only("索引の確認").unwrap().hits.is_empty());
    assert_eq!(
        ix.search("索引の確認", SearchMode::Verify).unwrap_err(),
        IndexError::FtsMissedPages(1)
    );
    // The check detects it; every search is then a full scan and finds the page.
    let report = ix.check().unwrap();
    assert!(!report.fts_integrity_ok);
    assert_eq!(report.fts_state, "inconsistent");
    let r = ix.search("索引の確認", SearchMode::Auto).unwrap();
    assert_eq!(
        (r.method, r.fallback),
        (Method::FullScan, Some("index_not_verified"))
    );
    assert_eq!(r.hits.len(), 1);
    // A rebuild repairs it and FTS5 is used again.
    assert!(ix.rebuild().unwrap().fts_integrity_ok);
    let r = ix.search("索引の確認", SearchMode::Auto).unwrap();
    assert_eq!((r.method, r.hits.len()), (Method::FtsCandidates, 1));
}

#[test]
fn r_failure_inside_a_transaction_rolls_back() {
    let mut ix = index();
    put(&mut ix, "a", Some("元の題"), "元の本文です");
    for point in ["after_page_write", "before_commit"] {
        ix.fault = Some(point);
        let e = ix
            .put_page(
                &PageText {
                    key: "a",
                    url: None,
                    title: Some("壊れた題"),
                    body: "途中の本文",
                },
                "ok",
                "{}",
            )
            .unwrap_err();
        assert_eq!(e, IndexError::InjectedFault(point));
        let e = ix
            .put_page(
                &PageText {
                    key: "new",
                    url: None,
                    title: None,
                    body: "新規の本文",
                },
                "ok",
                "{}",
            )
            .unwrap_err();
        assert_eq!(e, IndexError::InjectedFault(point));
        ix.fault = None;
        assert_hit(&ix, "元の本文", &["a"]);
        assert_hit(&ix, "元の題", &["a"]);
        assert_hit(&ix, "途中の本文", &[]);
        assert_hit(&ix, "新規の本文", &[]);
        assert_eq!(ix.page_count().unwrap(), 1);
        assert_eq!(ix.attempts("a").unwrap().len(), 1);
        assert!(ix.check().unwrap().fts_integrity_ok);
    }
    ix.fault = Some("during_rebuild");
    assert!(ix.rebuild().is_err());
    ix.fault = None;
    assert_hit(&ix, "元の本文", &["a"]);
    assert!(ix.check().unwrap().fts_integrity_ok);
}

#[test]
fn r_missing_fts5_feature_is_an_explicit_error() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    assert!(probe_fts5(&conn, "trigram case_sensitive 1").is_ok());
    let e = probe_fts5(&conn, "no_such_tokenizer").unwrap_err();
    assert!(matches!(e, IndexError::Fts5Unavailable(_)));
    assert_eq!(e.code(), "fts5_unavailable");
}

// ------------------------------------------------------------------ FTS5 candidates vs full scan

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 as usize
    }
}

const PIECES: &[&str] = &[
    "a",
    "b",
    "A",
    "B",
    "ab",
    " ",
    " ",
    "\n",
    "𩸽",
    "の",
    "ノ",
    "ﾉ",
    "が",
    "か\u{3099}",
    "é",
    "e\u{301}",
    "👍",
    "🏽",
    "\u{200D}",
    "🦀",
    "*",
    "?",
    "[",
    "]",
    "%",
    "_",
    "\"",
    "'",
    "-",
    "(",
    ")",
    ":",
    "^",
    "\\",
    "東京",
    "ＡＢ",
    "İ",
    "ß",
    "ﬁ",
    "Σ",
    "σ",
    "ς",
];

fn random_text(rng: &mut Rng, max: usize) -> String {
    let n = rng.next() % max;
    (0..n).map(|_| PIECES[rng.next() % PIECES.len()]).collect()
}

/// Every candidate method against the full scan over a random corpus built from characters that
/// stress case folding, normalization, wildcards and FTS5 syntax. The method used (`Glob`) must
/// never differ; the others are measured for the report.
#[test]
fn f_fts5_candidates_never_miss_compared_with_full_scan() {
    let mut rng = Rng(0x5EED_1234_ABCD_0001);
    for detail in [Detail::Full, Detail::None] {
        let mut ix = Index::open_in_memory(Options { detail }).unwrap();
        let mut bodies = Vec::new();
        for i in 0..150 {
            let body = random_text(&mut rng, 80);
            let title = random_text(&mut rng, 10);
            put(&mut ix, &format!("p{i:03}"), Some(&title), &body);
            bodies.push(body);
        }
        let mut queries: Vec<String> = Vec::new();
        for _ in 0..1500 {
            // Substrings of stored text (mostly HITs) and random strings (mostly MISSes).
            let b: Vec<char> = bodies[rng.next() % bodies.len()].chars().collect();
            if b.len() >= 3 {
                let len = 3 + rng.next() % 6;
                let start = rng.next() % b.len();
                queries.push(b[start..(start + len).min(b.len())].iter().collect());
            }
            queries.push(random_text(&mut rng, 7));
        }
        let mut mismatches = [0usize; 3];
        let mut checked = 0;
        for q in queries
            .iter()
            .filter(|q| q.chars().count() >= 3 && !q.contains('\0'))
        {
            checked += 1;
            let full = ix.search(q, SearchMode::FullScan).unwrap().page_ids();
            for (k, method) in [
                CandidateQuery::Glob,
                CandidateQuery::Like,
                CandidateQuery::MatchPhrase,
            ]
            .into_iter()
            .enumerate()
            {
                ix.candidate_query = method;
                match ix.search_fts_only(q) {
                    Ok(r) if r.page_ids() == full => {}
                    _ => mismatches[k] += 1,
                }
            }
            ix.candidate_query = CandidateQuery::Glob;
        }
        println!(
            "detail={detail:?} queries={checked} mismatches: glob={} like={} match_phrase={}",
            mismatches[0], mismatches[1], mismatches[2]
        );
        assert!(checked > 1000);
        assert_eq!(mismatches[0], 0, "GLOB candidates missed pages");
    }
}

// ------------------------------------------------------------------ quality gate (extraction)

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
    // The fixtures are ASCII outside the replaced label (quoted-printable / base64 bodies).
    let i = bytes
        .windows(from.len())
        .position(|w| w == from.as_bytes())
        .unwrap();
    [&bytes[..i], to.as_bytes(), &bytes[i + from.len()..]].concat()
}

#[test]
fn g_end_to_end_hokke_through_quoted_printable() {
    // 𩸽 split by a quoted-printable soft line break inside its 4 bytes, as Chrome may write it.
    let html = b"<html><head><meta charset=3D\"utf-8\"><title>=F0=A9=B8=BD=E3=81=AE=E8=A9=B1</title></head><body><p>=E4=BB=8A=E6=97=A5=E3=81=AF=F0=A9=\r\n=B8=BD=E3=82=92=E7=84=BC=E3=81=84=E3=81=9F=E3=80=82</p></body></html>";
    let bytes = mhtml(
        "Content-Type: text/html\r\nContent-Transfer-Encoding: quoted-printable",
        html,
    );
    let x = extracted(&bytes).unwrap();
    assert_eq!(x.content.body_text, "今日は𩸽を焼いた。");
    let mut ix = index();
    let reg = ix.register("qp", Ok(&x), &GateConfig::default()).unwrap();
    assert_eq!((reg.outcome, reg.status), (Outcome::Inserted, "ok"));
    assert_hit(&ix, "𩸽", &["qp"]);
    assert_hit(&ix, "𩸽を焼", &["qp"]);
    assert_hit(&ix, "𩸽の話", &["qp"]);
    assert_hit(&ix, "𩸽𩸽", &[]);
    let r = ix.search("𩸽", SearchMode::Auto).unwrap();
    assert!(r.hits[0].in_title && r.hits[0].in_body);
    assert_eq!(r.hits[0].url.as_deref(), Some("https://synthetic.test/p"));
}

#[test]
fn g_clean_pages_and_pages_with_warnings() {
    let mut ix = index();
    let x = extracted(&fixture("ja-article.mhtml")).unwrap();
    let reg = ix.register("ja", Ok(&x), &GateConfig::default()).unwrap();
    assert_eq!(
        (reg.status, reg.body_indexed, reg.title_indexed),
        ("ok", true, true)
    );
    let word: String = x.content.body_text.chars().take(4).collect();
    assert_hit(&ix, &word, &["ja"]);
    // A page with extraction warnings is indexed and keeps them.
    let warn = mhtml(
        "Content-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: 7bit",
        "<html><head><title>警告</title></head><body><p>本文\u{1}です</p></body></html>".as_bytes(),
    );
    let x = extracted(&warn).unwrap();
    assert!(!x.diagnostics.warnings.is_empty());
    let reg = ix.register("warn", Ok(&x), &GateConfig::default()).unwrap();
    assert_eq!(reg.status, "ok_with_warnings");
    assert_hit(&ix, "本文です", &["warn"]);
    let (status, quality) = ix.page_status("warn").unwrap().unwrap();
    assert_eq!(status, "ok_with_warnings");
    assert!(quality.contains("control_characters_removed"));
}

#[test]
fn g_empty_body_keeps_the_title() {
    let mut ix = index();
    let x = extracted(&mhtml(
        "Content-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: 8bit",
        "<html><head><title>題名だけのページ</title></head><body><script>x()</script></body></html>".as_bytes(),
    ))
    .unwrap();
    let reg = ix
        .register("empty", Ok(&x), &GateConfig::default())
        .unwrap();
    assert_eq!(
        (reg.status, reg.title_indexed, reg.body_indexed),
        ("title_only_empty_body", true, false)
    );
    assert_hit(&ix, "題名だけ", &["empty"]);
    assert_hit(&ix, "x()", &[]);
}

#[test]
fn g_garbled_text_is_not_indexed_and_quality_is_kept() {
    let gate = GateConfig::default();
    // Shift_JIS page declared as UTF-8: replacement characters.
    let sjis = replace(
        &fixture("sjis-news.mhtml"),
        "charset=3DShift_JIS",
        "charset=3Dutf-8",
    );
    let good = extracted(&fixture("sjis-news.mhtml")).unwrap();
    let word: String = good
        .content
        .body_text
        .chars()
        .filter(|c| !c.is_ascii())
        .take(3)
        .collect();
    let mut ix = index();
    let x = extracted(&sjis).unwrap();
    assert!(x.diagnostics.encoding.malformed_sequences_replaced);
    let reg = ix.register("sjis", Ok(&x), &gate).unwrap();
    assert_eq!(reg.status, "nothing_indexed_garbled");
    assert!(!reg.body_indexed && !reg.title_indexed);
    let (_, quality) = ix.page_status("sjis").unwrap().unwrap();
    let q: serde_json::Value = serde_json::from_str(&quality).unwrap();
    assert_eq!(q["body_verdict"], "garbled");
    assert!(q["body"]["replacement_chars"].as_u64().unwrap() > 0);
    assert!(q["body"]["replacement_per_mille"].as_u64().unwrap() >= 10);
    assert_hit(&ix, &word, &[]);
    assert_hit(&ix, "\u{FFFD}", &[]);

    // UTF-8 page declared as windows-1252 (mojibake without U+FFFD), ASCII title kept.
    let html = "<html><head><meta charset=\"windows-1252\"><title>ASCII title</title></head><body><p>日本語の本文がここにあります。漢字とひらがなとカタカナ。</p></body></html>";
    let x = extracted(&mhtml(
        "Content-Type: text/html\r\nContent-Transfer-Encoding: 8bit",
        html.as_bytes(),
    ))
    .unwrap();
    assert_eq!(x.diagnostics.encoding.name, "windows-1252");
    let reg = ix.register("mojibake", Ok(&x), &gate).unwrap();
    assert_eq!(
        (reg.status, reg.title_indexed, reg.body_indexed),
        ("title_only_garbled_body", true, false)
    );
    assert_hit(&ix, "ASCII title", &["mojibake"]);
    let (_, quality) = ix.page_status("mojibake").unwrap().unwrap();
    let q: serde_json::Value = serde_json::from_str(&quality).unwrap();
    assert_eq!(q["body_verdict"], "garbled");
    assert_eq!(q["body"]["misdecoded_as"], "windows-1252");
    // This short sample is one unbroken run that lost C1 bytes: caught by the removed-control
    // rule (the misdecoded-run rule catches the long real-page variants, report XIV-6).
    assert!(q["body"]["removed_control_per_mille"].as_u64().unwrap() >= 50);

    // The same mojibake with a Japanese title: the title is not trusted either.
    let x = extracted(&mhtml(
        "Content-Type: text/html\r\nContent-Transfer-Encoding: 8bit",
        html.replace("ASCII title", "日本語の題名").as_bytes(),
    ))
    .unwrap();
    let reg = ix.register("mojibake-ja", Ok(&x), &gate).unwrap();
    assert_eq!(reg.status, "nothing_indexed_garbled");
}

#[test]
fn g_genuine_legacy_encoded_page_is_clean() {
    let mut ix = index();
    let x = extracted(&fixture("sjis-news.mhtml")).unwrap();
    assert_eq!(x.diagnostics.encoding.name, "Shift_JIS");
    let reg = ix.register("sjis", Ok(&x), &GateConfig::default()).unwrap();
    assert_eq!((reg.status, reg.body_indexed), ("ok", true));
}

#[test]
fn g_failed_or_garbled_re_extraction_keeps_the_indexed_page() {
    let gate = GateConfig::default();
    let mut ix = index();
    let good = extracted(&fixture("ja-article.mhtml")).unwrap();
    let word: String = good.content.body_text.chars().take(4).collect();
    ix.register("page", Ok(&good), &gate).unwrap();
    for e in [
        ErrorCode::NotMime,
        ErrorCode::InputTooLarge,
        ErrorCode::InvalidBase64,
    ] {
        let reg = ix.register("page", Err(e), &gate).unwrap();
        assert_eq!(reg.outcome, Outcome::KeptPrevious);
        assert_eq!(reg.error_code, Some(e.as_str()));
        assert_hit(&ix, &word, &["page"]);
    }
    // A garbled re-extraction of the same page (wrong charset declaration) is not used either.
    let garbled = extracted(&replace(
        &fixture("ja-article.mhtml"),
        "charset=3DUTF-8",
        "charset=3Dshift_jis",
    ))
    .unwrap();
    let reg = ix.register("page", Ok(&garbled), &gate).unwrap();
    assert_eq!(reg.outcome, Outcome::KeptPrevious);
    assert_hit(&ix, &word, &["page"]);
    let (status, _) = ix.page_status("page").unwrap().unwrap();
    assert_eq!(status, "ok");
    let attempts = ix.attempts("page").unwrap();
    let outcomes: Vec<(&str, &str)> = attempts
        .iter()
        .map(|(o, s, _)| (o.as_str(), s.as_str()))
        .collect();
    assert_eq!(
        outcomes,
        [
            ("inserted", "ok"),
            ("kept_previous", "mhtml_parse_failed"),
            ("kept_previous", "input_too_large"),
            ("kept_previous", "mhtml_parse_failed"),
            ("kept_previous", "nothing_indexed_garbled"),
        ]
    );
    // A failure for a new page stores nothing but the attempt.
    let reg = ix
        .register("new", Err(ErrorCode::UnterminatedMultipart), &gate)
        .unwrap();
    assert_eq!(reg.outcome, Outcome::NotRegistered);
    assert!(ix.page_status("new").unwrap().is_none());
    assert_eq!(ix.page_count().unwrap(), 1);
    assert!(ix.check().unwrap().fts_integrity_ok);
}

/// Review fix HIGH-2 (PO decision): a re-extraction with an empty body (title only) or a garbled
/// body never replaces a page's indexed body; the attempt is recorded; a new page may still be
/// registered with its title only; an explicit delete still removes the page.
#[test]
fn g_empty_body_re_extraction_keeps_the_indexed_body() {
    let gate = GateConfig::default();
    let mut ix = index();
    // 1. A page with a body.
    let good = extracted(&fixture("ja-article.mhtml")).unwrap();
    let title = good.metadata.title.clone().unwrap();
    let words: Vec<String> = [0, 1, 2]
        .iter()
        .map(|k| {
            let chars: Vec<char> = good.content.body_text.chars().collect();
            let at = chars.len() * k / 3;
            chars[at..at + 5].iter().collect()
        })
        .collect();
    let first = ix.register("page", Ok(&good), &gate).unwrap();
    assert_eq!(
        (first.outcome, first.body_indexed),
        (Outcome::Inserted, true)
    );
    let id = first.page_id.unwrap();
    // 2. The same page again: a title, an empty body.
    let empty = extracted(&mhtml(
        "Content-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: 8bit",
        "<html><head><title>空になった再抽出の題名</title></head><body><script>x()</script></body></html>".as_bytes(),
    ))
    .unwrap();
    assert!(empty.content.body_text.is_empty());
    let reg = ix.register("page", Ok(&empty), &gate).unwrap();
    assert_eq!(
        (reg.outcome, reg.page_id, reg.status),
        (Outcome::KeptPrevious, Some(id), "title_only_empty_body")
    );
    assert!(!reg.title_indexed && !reg.body_indexed);
    // 3. The body is still found, through FTS5 and the full scan (assert_hit compares all modes).
    for w in &words {
        assert_hit(&ix, w, &["page"]);
    }
    // 4. Title, body and FTS5 rows are the previous ones.
    let t5: String = title.chars().take(5).collect();
    assert_hit(&ix, &t5, &["page"]);
    assert_hit(&ix, "空になった再抽出", &[]);
    let (status, _) = ix.page_status("page").unwrap().unwrap();
    assert_eq!(status, "ok");
    let (stored_title, stored_body): (Option<String>, String) = ix
        .connection()
        .query_row(
            "SELECT title, body FROM pages WHERE page_key = 'page'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(stored_title.as_deref(), Some(title.as_str()));
    assert_eq!(stored_body, good.content.body_text);
    let report = ix.check().unwrap();
    assert!(report.fts_integrity_ok && report.sqlite_integrity_ok);
    // A garbled body with a clean ASCII title is not used either.
    let garbled = extracted(&mhtml(
        "Content-Type: text/html\r\nContent-Transfer-Encoding: 8bit",
        "<html><head><meta charset=\"windows-1252\"><title>ASCII title</title></head><body><p>日本語の本文がここにあります。漢字とひらがなとカタカナ。</p></body></html>".as_bytes(),
    ))
    .unwrap();
    let reg = ix.register("page", Ok(&garbled), &gate).unwrap();
    assert_eq!(
        (reg.outcome, reg.status),
        (Outcome::KeptPrevious, "title_only_garbled_body")
    );
    assert_hit(&ix, &words[1], &["page"]);
    assert_hit(&ix, "ASCII title", &[]);
    // 5. The attempts are recorded.
    let attempts = ix.attempts("page").unwrap();
    let outcomes: Vec<(&str, &str)> = attempts
        .iter()
        .map(|(o, s, _)| (o.as_str(), s.as_str()))
        .collect();
    assert_eq!(
        outcomes,
        [
            ("inserted", "ok"),
            ("kept_previous", "title_only_empty_body"),
            ("kept_previous", "title_only_garbled_body"),
        ]
    );
    // A re-extraction with a good body still updates the page.
    let reg = ix.register("page", Ok(&good), &gate).unwrap();
    assert_eq!((reg.outcome, reg.page_id), (Outcome::Updated, Some(id)));
    // 6. A new page with a title and an empty body is registered with its title only, and a
    //    page without a body may be updated by another title-only extraction.
    let reg = ix.register("title-only", Ok(&empty), &gate).unwrap();
    assert_eq!(
        (reg.outcome, reg.status, reg.title_indexed, reg.body_indexed),
        (Outcome::Inserted, "title_only_empty_body", true, false)
    );
    assert_hit(&ix, "空になった再抽出", &["title-only"]);
    let reg = ix.register("title-only", Ok(&empty), &gate).unwrap();
    assert_eq!(reg.outcome, Outcome::Updated);
    let reg = ix.register("title-only", Ok(&good), &gate).unwrap();
    assert_eq!((reg.outcome, reg.body_indexed), (Outcome::Updated, true));
    // 7. An explicit delete removes the page from every search.
    assert!(ix.delete("page").unwrap());
    for w in &words {
        assert_hit(&ix, w, &["title-only"]);
    }
    assert!(ix.delete("title-only").unwrap());
    for w in &words {
        assert_hit(&ix, w, &[]);
    }
    assert_eq!(ix.page_count().unwrap(), 0);
    assert!(ix.check().unwrap().fts_integrity_ok);
}

#[test]
fn g_real_size_limit_failure_is_classified() {
    let limits = Limits {
        max_input_bytes: 10,
        ..Limits::default()
    };
    let e = extract(&fixture("ja-article.mhtml"), &limits).unwrap_err();
    assert_eq!(page_fulltext::index::failure_class(e), "input_too_large");
    let e = extract(b"not mime at all", &Limits::default()).unwrap_err();
    assert_eq!(page_fulltext::index::failure_class(e), "mhtml_parse_failed");
}

// ------------------------------------------------------------------ real MHTML (PO-provided)

/// Acceptance test with the PO's real MHTML (Wikipedia 𩸽 page). Not run by default; run with
/// `PAGE_FULLTEXT_REAL_HOKKE=<path> cargo test --release --test search -- --ignored --nocapture`.
/// Prints counts and HIT/MISS only (no page text). The query strings other than 𩸽 are taken
/// from the extraction itself, never assumed.
#[test]
#[ignore]
fn real_hokke_acceptance() {
    let path = std::env::var("PAGE_FULLTEXT_REAL_HOKKE").expect("set PAGE_FULLTEXT_REAL_HOKKE");
    let limits = Limits::default();
    let bytes = page_fulltext::read_input(std::path::Path::new(&path), &limits).unwrap();
    let x = extract(&bytes, &limits).expect("1. extraction succeeds");
    let body = x.content.body_text.clone();
    let title = x.metadata.title.clone().unwrap_or_default();
    let in_body = body.matches('𩸽').count();
    let in_title = title.matches('𩸽').count();
    println!(
        "1. extraction: status={} body_chars={} warnings={:?}",
        x.status, x.content.character_count, x.diagnostics.warnings
    );
    println!("2. U+29E3D in extraction: body={in_body} title={in_title}");
    assert!(
        in_body + in_title > 0,
        "2. 𩸽 is in the extracted body or title"
    );

    // Real page text: the database goes to the system temporary directory, never inside this
    // Git working tree (not even the ignored target/), and is removed at the end.
    let db = std::env::temp_dir().join(format!(
        "page-index-real-hokke-{}.sqlite3",
        std::process::id()
    ));
    for s in ["", "-wal", "-shm", "-journal"] {
        let _ = std::fs::remove_file(format!("{}{s}", db.display()));
    }
    let mut ix = Index::open_or_create(&db, Options::default()).unwrap();
    let reg = ix
        .register("real-hokke", Ok(&x), &GateConfig::default())
        .unwrap();
    println!(
        "   register: {:?} status={} body_indexed={} title_indexed={}",
        reg.outcome, reg.status, reg.body_indexed, reg.title_indexed
    );
    assert!(reg.body_indexed);
    // Some unrelated synthetic pages around it.
    for i in 0..20 {
        put(
            &mut ix,
            &format!("synthetic-{i}"),
            Some("合成"),
            &format!("合成ページ{i}の本文。鮭を焼く。"),
        );
    }
    let only = |ix: &Index, q: &str| ix.search(q, SearchMode::Verify).unwrap().page_ids();
    let real_id = reg.page_id.unwrap();

    let r = ix.search("𩸽", SearchMode::Auto).unwrap();
    println!("3. '𩸽': method={:?} hits={:?}", r.method, r.page_ids());
    assert_eq!(r.page_ids(), [real_id]);

    // 4. 3+ character strings that exist: taken from the extraction (around each 𩸽, and the
    //    title), checked against the stored text by the literal rule.
    let chars: Vec<char> = body.chars().collect();
    let mut tried = 0;
    for (i, _) in chars.iter().enumerate().filter(|(_, c)| **c == '𩸽') {
        for (from, to) in [
            (i, i + 3),
            (i.saturating_sub(2), i + 1),
            (i.saturating_sub(4), i + 4),
        ] {
            let q: String = chars[from..to.min(chars.len())].iter().collect();
            if q.chars().count() >= 3 {
                let r = ix.search(&q, SearchMode::Auto).unwrap();
                println!(
                    "4. body substring around 𩸽 ({} chars): method={:?} hits={:?}",
                    q.chars().count(),
                    r.method,
                    r.page_ids()
                );
                assert_eq!(r.page_ids(), [real_id]);
                assert_eq!(only(&ix, &q), [real_id]);
                tried += 1;
            }
        }
    }
    if title.chars().count() >= 3 {
        let q: String = title.chars().take(5).collect();
        let r = ix.search(&q, SearchMode::Auto).unwrap();
        println!(
            "4. title prefix ({} chars): hits={:?} in_title={}",
            q.chars().count(),
            r.page_ids(),
            r.hits.iter().any(|h| h.in_title)
        );
        assert!(r.page_ids().contains(&real_id));
        tried += 1;
    }
    assert!(tried >= 3);
    // Completeness sweep: substrings of the extraction at every 3rd position, several lengths;
    // each must HIT the real page (Verify: FTS5 path and full scan agree). Counts only.
    let mut swept = 0;
    let mut via_fts = 0;
    for start in (0..chars.len()).step_by(3) {
        for len in [1, 2, 3, 4, 6, 10] {
            if start + len > chars.len() {
                continue;
            }
            let q: String = chars[start..start + len].iter().collect();
            let r = ix.search(&q, SearchMode::Verify).unwrap();
            assert!(
                r.page_ids().contains(&real_id),
                "substring at {start} len {len} missed"
            );
            swept += 1;
            if r.method == Method::FtsCandidates {
                via_fts += 1;
            }
        }
    }
    println!(
        "4. sweep: {swept} substrings of the body all HIT ({via_fts} through FTS5 candidates)"
    );

    // 5. Strings verified to be absent from the extraction.
    for q in ["𩸽𩸽", "鮭𩸽鮭", "存在しない検索語句ZZQ", "ほっけ𩸽ホッケ"]
    {
        assert!(!body.contains(q) && !title.contains(q));
        let r = ix.search(q, SearchMode::Auto).unwrap();
        println!(
            "5. absent ({} chars): hits={:?}",
            q.chars().count(),
            r.page_ids()
        );
        assert!(!r.page_ids().contains(&real_id));
    }

    // 6. Close and reopen: read-only first (no file changes), then writable.
    drop(ix);
    let files = || {
        ["", "-wal", "-shm", "-journal"].map(|s| std::fs::read(format!("{}{s}", db.display())).ok())
    };
    let before = files();
    {
        let ro = Index::open_readonly(&db).unwrap();
        assert_eq!(only(&ro, "𩸽"), [real_id]);
        let q: String = chars.iter().skip(chars.len() / 4).take(6).collect();
        assert!(only(&ro, &q).contains(&real_id));
    }
    assert_eq!(files(), before, "read-only reopen changed the files");
    println!("6. read-only reopen: '𩸽' HIT, database/-wal/-shm/-journal unchanged");
    let mut ix = Index::open_existing(&db, Options::default()).unwrap();
    assert_eq!(only(&ix, "𩸽"), [real_id]);
    assert!(ix.check().unwrap().fts_integrity_ok);
    println!("6. reopened: '𩸽' hits={:?}, integrity ok", only(&ix, "𩸽"));

    // 8. Re-register: no duplicate.
    let reg2 = ix
        .register("real-hokke", Ok(&x), &GateConfig::default())
        .unwrap();
    assert_eq!(
        (reg2.outcome, reg2.page_id),
        (Outcome::Updated, Some(real_id))
    );
    assert_eq!(ix.page_count().unwrap(), 21);
    assert_eq!(only(&ix, "𩸽"), [real_id]);
    println!(
        "8. re-registered: outcome={:?} pages={} '𩸽' hits={:?}",
        reg2.outcome,
        ix.page_count().unwrap(),
        only(&ix, "𩸽")
    );

    // 9. Update: the body changes (synthetic replacement text), the old text no longer HITs.
    let q_old: String = chars.iter().skip(chars.len() / 3).take(8).collect();
    assert!(!title.contains(&q_old) && body.contains(&q_old));
    assert_eq!(only(&ix, &q_old), [real_id]);
    ix.put_page(
        &PageText {
            key: "real-hokke",
            url: None,
            title: Some(&title),
            body: "差し替え後の本文。𩸽は無い。",
        },
        "ok",
        "{}",
    )
    .unwrap();
    let old_hits = ix.search(&q_old, SearchMode::Verify).unwrap().page_ids();
    assert!(!old_hits.contains(&real_id));
    assert_eq!(only(&ix, "差し替え後"), [real_id]);
    ix.register("real-hokke", Ok(&x), &GateConfig::default())
        .unwrap();
    assert_eq!(only(&ix, "𩸽"), [real_id]);
    println!("9. update reflected: old text MISS, new text HIT, restored");

    // 10. Results carry no body text.
    let json = serde_json::to_string(&ix.search("𩸽", SearchMode::Auto).unwrap()).unwrap();
    let probe: String = chars.iter().skip(chars.len() / 2).take(20).collect();
    assert!(!json.contains(&probe) && json.len() < 2000);
    println!("10. result JSON bytes={} (no body text)", json.len());

    // 7. Delete.
    assert!(ix.delete("real-hokke").unwrap());
    assert!(only(&ix, "𩸽").is_empty());
    assert!(ix.check().unwrap().fts_integrity_ok);
    println!("7. deleted: '𩸽' hits={:?}", only(&ix, "𩸽"));
    drop(ix);
    for s in ["", "-wal", "-shm", "-journal"] {
        let _ = std::fs::remove_file(format!("{}{s}", db.display()));
    }
    assert!(!db.exists());
}
