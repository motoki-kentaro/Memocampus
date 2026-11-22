//! PoC CLI for the Slice 2 index (PoC-only SQLite database; never the Memocampus Vault).
//!
//! ```text
//! page-index add     --db <file> --key <page-key> --input <file.mhtml>
//! page-index search  --db <file> --query <text> [--mode auto|full|verify] [--show-titles]
//! page-index delete  --db <file> --key <page-key>
//! page-index rebuild --db <file>
//! page-index check   --db <file>
//! page-index stats   --db <file>
//! page-index quality --input <file.mhtml>
//! page-index bench   --dir <empty-dir> [--pages 100,1000,10000] [--avg-chars 3000] [--runs 7] [--detail full|none]
//! ```
//!
//! Output is codes and numbers only. Page titles and URLs of hits are printed only with
//! `--show-titles`; body text is never printed. The database holds extracted page text: it is
//! refused inside a Git working tree (real path, symbolic links and junctions resolved; fail
//! closed) unless `--allow-db-in-git` is given; `bench --dir` has no override.
//!
//! Only `add` creates a database (when nothing exists at `--db`). `delete`, `rebuild` and `check`
//! open an existing page-index database for writing; `search` and `stats` open it read-only.
//! Any other file (empty, not SQLite, another application's database) is refused unmodified.

use page_fulltext::index::{
    Detail, Index, IndexError, Options, PageText, SearchMode, SearchResult,
};
use page_fulltext::limits::Limits;
use page_fulltext::quality::{self, GateConfig};
use page_fulltext::{extract, read_input};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

fn usage() -> ExitCode {
    eprintln!("usage: page-index <add|search|delete|rebuild|check|stats|quality|bench> [options] (see the source header)");
    ExitCode::from(2)
}

/// `Err` when `path` (a file or directory, which need not exist yet) is or would be inside a Git
/// working tree or a `.git` directory, or when that cannot be determined (fail closed).
///
/// Both the path as given (made absolute) and its real path are checked: the deepest existing
/// ancestor, the path itself included, is resolved with `canonicalize` (symbolic links and
/// Windows junctions), and the components that do not exist yet are appended to it. Every
/// directory on both paths, the path itself included, must have no `.git` entry (directory or
/// file, as in worktrees and submodules).
fn git_guard(path: &Path) -> Result<(), String> {
    let refuse = |why: String| Err(format!("refused: {why}"));
    let abs = match std::path::absolute(path) {
        Ok(p) => p,
        Err(e) => return refuse(format!("cannot make the path absolute: {e}")),
    };
    let mut existing = abs.as_path();
    let mut missing = Vec::new();
    loop {
        match std::fs::symlink_metadata(existing) {
            Ok(_) => break,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let (Some(name), Some(parent)) = (existing.file_name(), existing.parent()) else {
                    return refuse("no existing ancestor directory".into());
                };
                missing.push(name);
                existing = parent;
            }
            Err(e) => return refuse(format!("cannot check the path: {e}")),
        }
    }
    let mut real = match std::fs::canonicalize(existing) {
        Ok(p) => p,
        Err(e) => return refuse(format!("cannot resolve the real path: {e}")),
    };
    for name in missing.iter().rev() {
        real.push(name);
    }
    for candidate in [abs.as_path(), real.as_path()] {
        for dir in candidate.ancestors() {
            if dir
                .file_name()
                .is_some_and(|n| n.eq_ignore_ascii_case(".git"))
            {
                return refuse("the path is inside a .git directory".into());
            }
            match std::fs::symlink_metadata(dir.join(".git")) {
                Ok(_) => {
                    return refuse(
                        "the path is inside a Git working tree (the database holds page text)"
                            .into(),
                    )
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return refuse(format!("cannot check for a Git working tree: {e}")),
            }
        }
    }
    Ok(())
}

fn db_path(args: &HashMap<String, String>, flags: &[String]) -> Result<PathBuf, String> {
    let db = PathBuf::from(args.get("db").ok_or("missing --db")?);
    if !flags.iter().any(|f| f == "allow-db-in-git") {
        git_guard(&db).map_err(|e| format!("{e}; use a path outside, or --allow-db-in-git"))?;
    }
    Ok(db)
}

/// Registration: creates the database when nothing exists at the path.
fn open_or_create(args: &HashMap<String, String>, flags: &[String]) -> Result<Index, String> {
    Index::open_or_create(&db_path(args, flags)?, Options::default()).map_err(|e| e.to_string())
}

/// Maintenance (delete, rebuild, check): an existing page-index database, writable.
fn open_existing(args: &HashMap<String, String>, flags: &[String]) -> Result<Index, String> {
    Index::open_existing(&db_path(args, flags)?, Options::default()).map_err(|e| e.to_string())
}

/// Search and statistics: an existing page-index database, read-only.
fn open_readonly(args: &HashMap<String, String>, flags: &[String]) -> Result<Index, String> {
    Index::open_readonly(&db_path(args, flags)?).map_err(|e| e.to_string())
}

fn print_result(r: &SearchResult, show_titles: bool) {
    println!(
        "method={:?} fallback={} query_chars={} checked_pages={} hits={}",
        r.method,
        r.fallback.unwrap_or("-"),
        r.query_chars,
        r.checked_pages,
        r.hits.len()
    );
    for h in &r.hits {
        if show_titles {
            println!(
                "hit page_id={} key={} in_title={} in_body={} title={:?} url={:?}",
                h.page_id, h.page_key, h.in_title, h.in_body, h.title, h.url
            );
        } else {
            println!(
                "hit page_id={} key={} in_title={} in_body={}",
                h.page_id, h.page_key, h.in_title, h.in_body
            );
        }
    }
}

fn run(cmd: &str, args: &HashMap<String, String>, flags: &[String]) -> Result<(), String> {
    let e = |e: IndexError| e.to_string();
    match cmd {
        "add" => {
            let mut idx = open_or_create(args, flags)?;
            let key = args.get("key").ok_or("missing --key")?;
            let input = PathBuf::from(args.get("input").ok_or("missing --input")?);
            let limits = Limits::default();
            let x = read_input(&input, &limits).and_then(|b| extract(&b, &limits));
            let r = idx
                .register(key, x.as_ref().map_err(|c| *c), &GateConfig::default())
                .map_err(e)?;
            println!("{}", serde_json::to_string(&r).expect("json"));
            if let Some((status, q)) = idx.page_status(key).map_err(e)? {
                println!("page_status={status} quality={q}");
            }
        }
        "search" => {
            let idx = open_readonly(args, flags)?;
            let q = args.get("query").ok_or("missing --query")?;
            let mode = match args.get("mode").map(String::as_str) {
                None | Some("auto") => SearchMode::Auto,
                Some("full") => SearchMode::FullScan,
                Some("verify") => SearchMode::Verify,
                Some(_) => return Err("--mode auto|full|verify".into()),
            };
            let t = Instant::now();
            let r = idx.search(q, mode).map_err(e)?;
            print_result(&r, flags.iter().any(|f| f == "show-titles"));
            println!("search_us={}", t.elapsed().as_micros());
        }
        "delete" => {
            let mut idx = open_existing(args, flags)?;
            let key = args.get("key").ok_or("missing --key")?;
            println!("deleted={}", idx.delete(key).map_err(e)?);
        }
        "rebuild" => {
            let mut idx = open_existing(args, flags)?;
            println!(
                "{}",
                serde_json::to_string(&idx.rebuild().map_err(e)?).expect("json")
            );
        }
        "check" => {
            let mut idx = open_existing(args, flags)?;
            println!(
                "{}",
                serde_json::to_string(&idx.check().map_err(e)?).expect("json")
            );
        }
        "stats" => {
            let idx = open_readonly(args, flags)?;
            println!(
                "{}",
                serde_json::to_string(&idx.stats().map_err(e)?).expect("json")
            );
        }
        "quality" | "quality-matrix" => {
            let input = PathBuf::from(args.get("input").ok_or("missing --input")?);
            let limits = Limits::default();
            match read_input(&input, &limits).and_then(|b| extract(&b, &limits)) {
                Ok(x) if cmd == "quality-matrix" => {
                    // False-positive check: the same (genuine) text as if the page had been
                    // decoded with each legacy encoding. Numbers only.
                    for label in [
                        "windows-1252",
                        "windows-1251",
                        "iso-8859-5",
                        "koi8-r",
                        "shift_jis",
                        "euc-jp",
                        "iso-2022-jp",
                        "gbk",
                        "gb18030",
                        "big5",
                        "euc-kr",
                    ] {
                        let enc = quality::encoding_named(label);
                        let b = quality::measure(&x.content.body_text, enc);
                        let t = quality::measure(x.metadata.title.as_deref().unwrap_or(""), enc);
                        println!(
                            "as={} body_nonascii={} body_misdecoded={} ({}‰) title_misdecoded={} ({}‰)",
                            enc.name(), b.non_ascii_chars, b.misdecoded_chars, b.misdecoded_per_mille,
                            t.misdecoded_chars, t.misdecoded_per_mille
                        );
                    }
                }
                Ok(x) => {
                    let gate = GateConfig::default();
                    let enc = quality::encoding_named(x.diagnostics.encoding.name);
                    let b = quality::measure_with_removed(
                        &x.content.body_text,
                        enc,
                        x.diagnostics.dropped_control_characters,
                    );
                    let t = quality::measure(x.metadata.title.as_deref().unwrap_or(""), enc);
                    let bv = gate.judge(&b);
                    println!(
                        "{}",
                        serde_json::json!({
                            "encoding": x.diagnostics.encoding.name,
                            "encoding_source": x.diagnostics.encoding.source,
                            "warnings": x.diagnostics.warnings,
                            "body": b, "body_verdict": bv,
                            "title": t, "title_verdict": gate.judge_title(&t, bv),
                        })
                    );
                }
                Err(c) => println!(
                    "extraction_error={} class={}",
                    c,
                    page_fulltext::index::failure_class(c)
                ),
            }
        }
        "bench" => bench(args)?,
        _ => return Err("unknown command".into()),
    }
    Ok(())
}

fn main() -> ExitCode {
    let mut it = std::env::args().skip(1);
    let Some(cmd) = it.next() else {
        return usage();
    };
    let mut args = HashMap::new();
    let mut flags = Vec::new();
    let rest: Vec<String> = it.collect();
    let mut i = 0;
    while i < rest.len() {
        let Some(name) = rest[i].strip_prefix("--") else {
            return usage();
        };
        if matches!(name, "show-titles" | "allow-db-in-git") {
            flags.push(name.to_owned());
            i += 1;
        } else if i + 1 < rest.len() {
            args.insert(name.to_owned(), rest[i + 1].clone());
            i += 2;
        } else {
            return usage();
        }
    }
    match run(&cmd, &args, &flags) {
        Ok(()) => ExitCode::SUCCESS,
        Err(m) => {
            eprintln!("error: {m}");
            ExitCode::FAILURE
        }
    }
}

// ------------------------------------------------------------------ benchmark (synthetic pages)

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const WORDS: &[&str] = &[
    "東京",
    "都内",
    "の",
    "は",
    "を",
    "に",
    "が",
    "と",
    "で",
    "検索",
    "記事",
    "日本語",
    "保存",
    "ページ",
    "について",
    "します",
    "された",
    "研究",
    "情報",
    "歴史",
    "地域",
    "漁業",
    "魚",
    "焼き",
    "北海道",
    "カタカナ",
    "ひらがな",
    "漢字",
    "Rust",
    "SQLite",
    "search",
    "index",
    "the",
    "and",
    "Memocampus",
    "2026",
    "🦀",
    "👍🏽",
    "（",
    "）",
    "、",
    "。",
    " ",
    " ",
    "\n",
];

/// A synthetic page of about `avg` characters. Every 50th page contains `𩸽`; every page contains
/// its own marker `頁{i:06}番`.
fn page_text(rng: &mut Rng, i: usize, avg: usize) -> String {
    let target = avg / 2 + rng.below(avg);
    let mut s = String::new();
    let mut chars = 0;
    let marker_at = rng.below(target.max(1));
    let mut marked = false;
    while chars < target {
        if !marked && chars >= marker_at {
            s.push_str(&format!("頁{i:06}番"));
            if i.is_multiple_of(50) {
                s.push_str("𩸽を焼く");
            }
            marked = true;
        }
        let w = WORDS[rng.below(WORDS.len())];
        s.push_str(w);
        chars += w.chars().count();
    }
    if !marked {
        s.push_str(&format!("頁{i:06}番"));
    }
    s
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).expect("no NaN"));
    v[v.len() / 2]
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn bench(args: &HashMap<String, String>) -> Result<(), String> {
    let dir = PathBuf::from(args.get("dir").ok_or("missing --dir")?);
    // Synthetic text only, but still plaintext databases: never inside a Git working tree (no
    // override). Checked before the directory is created and again once it exists.
    git_guard(&dir).map_err(|e| format!("--dir {e}"))?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    git_guard(&dir).map_err(|e| format!("--dir {e}"))?;
    let sizes: Vec<usize> = args
        .get("pages")
        .map_or("100,1000,10000", String::as_str)
        .split(',')
        .map(|s| s.trim().parse().map_err(|_| "--pages"))
        .collect::<Result<_, _>>()?;
    let avg: usize = args
        .get("avg-chars")
        .map_or(Ok(3000), |s| s.parse())
        .map_err(|_| "--avg-chars")?;
    let runs: usize = args
        .get("runs")
        .map_or(Ok(7), |s| s.parse())
        .map_err(|_| "--runs")?;
    let detail = match args.get("detail").map(String::as_str) {
        None | Some("none") => Detail::None,
        Some("full") => Detail::Full,
        _ => return Err("--detail full|none".into()),
    };
    for &n in &sizes {
        let path = dir.join(format!("bench-{n}-{detail:?}.sqlite3").to_lowercase());
        // Existing files are never deleted or reused: use an empty directory.
        for suffix in ["", "-journal", "-wal", "-shm"] {
            if std::fs::symlink_metadata(format!("{}{suffix}", path.display())).is_ok() {
                return Err(format!(
                    "refused: {}{suffix} exists (use an empty --dir)",
                    path.display()
                ));
            }
        }
        let mut idx = Index::create(&path, Options { detail }).map_err(|e| e.to_string())?;
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ n as u64);
        let mut total_chars = 0usize;
        let mut per_page = Vec::with_capacity(n);
        let t_all = Instant::now();
        for i in 0..n {
            let body = page_text(&mut rng, i, avg);
            total_chars += body.chars().count();
            let key = format!("page-{i:06}");
            let title = format!("合成ページ {i} 検索テスト");
            let t = Instant::now();
            idx.put_page(
                &PageText {
                    key: &key,
                    url: None,
                    title: Some(&title),
                    body: &body,
                },
                "ok",
                "{}",
            )
            .map_err(|e| e.to_string())?;
            per_page.push(ms(t));
        }
        let register_total = ms(t_all);
        // Update and delete: `runs` pages each, median.
        let mut upd = Vec::new();
        let mut del = Vec::new();
        for r in 0..runs {
            let i = (r * 7919) % n;
            let key = format!("page-{i:06}");
            let body = page_text(&mut rng, i, avg);
            let title = format!("合成ページ {i} 検索テスト");
            let page = PageText {
                key: &key,
                url: None,
                title: Some(&title),
                body: &body,
            };
            let t = Instant::now();
            idx.put_page(&page, "ok", "{}").map_err(|e| e.to_string())?;
            upd.push(ms(t));
            let t = Instant::now();
            idx.delete(&key).map_err(|e| e.to_string())?;
            del.push(ms(t));
            idx.put_page(&page, "ok", "{}").map_err(|e| e.to_string())?;
        }
        let db_bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let t = Instant::now();
        let check = idx.check().map_err(|e| e.to_string())?;
        let check_ms = ms(t);
        let t = Instant::now();
        idx.rebuild().map_err(|e| e.to_string())?;
        let rebuild_ms = ms(t);
        println!(
            "{}",
            serde_json::json!({
                "pages": n, "detail": detail, "avg_chars": total_chars / n.max(1), "total_chars": total_chars,
                "register_total_ms": register_total, "register_median_ms": median(per_page),
                "update_median_ms": median(upd), "delete_median_ms": median(del),
                "db_bytes": db_bytes, "check_ms": check_ms, "check_ok": check.fts_integrity_ok,
                "rebuild_and_check_ms": rebuild_ms,
            })
        );
        let rare = format!("頁{:06}番", n / 2);
        let queries: [(&str, &str); 9] = [
            ("1char_common", "の"),
            ("1char_rare", "𩸽"),
            ("2char_common", "東京"),
            ("2char_absent", "鮭鱒"),
            ("3char_common", "について"),
            ("3char_with_hokke", "𩸽を焼"),
            ("unique_marker", &rare),
            ("absent_long", "存在しない検索語句です"),
            ("absent_3char", "鮭𩸽鮭"),
        ];
        for (label, q) in queries {
            let mut auto = Vec::new();
            let mut full = Vec::new();
            let mut last = None;
            let mut full_hits = 0;
            for _ in 0..runs {
                let t = Instant::now();
                let r = idx.search(q, SearchMode::Auto).map_err(|e| e.to_string())?;
                auto.push(ms(t));
                let t = Instant::now();
                let f = idx
                    .search(q, SearchMode::FullScan)
                    .map_err(|e| e.to_string())?;
                full.push(ms(t));
                full_hits = f.hits.len();
                last = Some(r);
            }
            let r = last.expect("runs > 0");
            if r.hits.len() != full_hits {
                return Err(format!(
                    "{label}: auto {} != full scan {full_hits}",
                    r.hits.len()
                ));
            }
            println!(
                "{}",
                serde_json::json!({
                    "pages": n, "detail": detail, "query": label, "query_chars": r.query_chars,
                    "method": r.method, "checked_pages": r.checked_pages, "hits": r.hits.len(),
                    "auto_median_ms": median(auto), "full_scan_median_ms": median(full), "runs": runs,
                })
            );
        }
    }
    Ok(())
}
