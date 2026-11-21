//! CLI of the Slice 1 PoC.
//!
//!   page-fulltext --input <file.mhtml> [--output <new-file.json>]
//!
//! Default: prints only a summary (sizes, counts, encoding, warning codes, timings, memory). Page
//! text and metadata are written only to the file named with `--output`, which must not exist yet;
//! nothing is written when extraction fails. Exit codes: 0 success, 1 input refused (error code on
//! stderr), 2 usage, 3 output file problem.

use page_fulltext::{extract, limits::Limits, read_input};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    fs::OpenOptions,
    io::Write,
    path::PathBuf,
    process::ExitCode,
    sync::atomic::{AtomicUsize, Ordering},
    time::Instant,
};

/// Counts live heap bytes and their peak (measurement only; it does not limit anything).
struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            let now = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(now, Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        unsafe { System.dealloc(p, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }
    unsafe fn realloc(&self, p: *mut u8, layout: Layout, new: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, layout, new) };
        if !q.is_null() {
            if new >= layout.size() {
                let now =
                    LIVE.fetch_add(new - layout.size(), Ordering::Relaxed) + new - layout.size();
                PEAK.fetch_max(now, Ordering::Relaxed);
            } else {
                LIVE.fetch_sub(layout.size() - new, Ordering::Relaxed);
            }
        }
        q
    }
}
#[global_allocator]
static ALLOC: Counting = Counting;

#[cfg(windows)]
fn peak_working_set() -> Option<usize> {
    #[repr(C)]
    #[allow(non_snake_case)]
    struct Counters {
        cb: u32,
        PageFaultCount: u32,
        PeakWorkingSetSize: usize,
        WorkingSetSize: usize,
        QuotaPeakPagedPoolUsage: usize,
        QuotaPagedPoolUsage: usize,
        QuotaPeakNonPagedPoolUsage: usize,
        QuotaNonPagedPoolUsage: usize,
        PagefileUsage: usize,
        PeakPagefileUsage: usize,
    }
    unsafe extern "system" {
        fn GetCurrentProcess() -> isize;
        fn K32GetProcessMemoryInfo(process: isize, counters: *mut Counters, cb: u32) -> i32;
    }
    let mut c = Counters {
        cb: std::mem::size_of::<Counters>() as u32,
        PageFaultCount: 0,
        PeakWorkingSetSize: 0,
        WorkingSetSize: 0,
        QuotaPeakPagedPoolUsage: 0,
        QuotaPagedPoolUsage: 0,
        QuotaPeakNonPagedPoolUsage: 0,
        QuotaNonPagedPoolUsage: 0,
        PagefileUsage: 0,
        PeakPagefileUsage: 0,
    };
    let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) };
    (ok != 0).then_some(c.PeakWorkingSetSize)
}
#[cfg(not(windows))]
fn peak_working_set() -> Option<usize> {
    None
}

fn usage() -> ExitCode {
    eprintln!("usage: page-fulltext --input <file.mhtml> [--output <new-file.json>]");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let mut input = None;
    let mut output = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--input") if input.is_none() => input = args.next().map(PathBuf::from),
            Some("--output") if output.is_none() => output = args.next().map(PathBuf::from),
            _ => return usage(),
        }
    }
    let Some(input) = input else { return usage() };
    let limits = Limits::default();
    // Refuse an existing output before doing any work (and never overwrite it later either).
    if let Some(out) = &output {
        if out.exists() {
            eprintln!("status=error code=output_exists");
            return ExitCode::from(3);
        }
    }
    let start = Instant::now();
    let result = read_input(&input, &limits).and_then(|bytes| extract(&bytes, &limits));
    let total = start.elapsed();
    let extraction = match result {
        Ok(x) => x,
        Err(code) => {
            eprintln!("status=error code={code}");
            return ExitCode::from(1);
        }
    };
    let s = &extraction.source;
    let d = &extraction.diagnostics;
    let m = &extraction.metadata;
    let present = |o: bool| if o { "present" } else { "absent" };
    println!("status=success");
    println!("input_bytes={}", s.input_bytes);
    println!(
        "mime_parts={} leaves={} html_parts={}",
        s.mime_part_count, s.mime_leaf_count, s.html_part_count
    );
    println!(
        "selected_part={} rule={:?} transfer_encoding={:?} html_decoded_bytes={}",
        s.selected_part.index,
        s.selected_part.rule,
        s.selected_part.transfer_encoding,
        s.selected_part.decoded_bytes
    );
    println!(
        "encoding={} source={:?} malformed_replaced={}",
        d.encoding.name, d.encoding.source, d.encoding.malformed_sequences_replaced
    );
    println!(
        "text_utf8_bytes={} characters={}",
        extraction.content.utf8_bytes, extraction.content.character_count
    );
    println!(
        "metadata title={} description={} keywords={} og_title={} og_description={} article_tags={}",
        present(m.title.is_some()),
        present(m.description.is_some()),
        m.keywords.as_ref().map_or("absent".to_owned(), |k| k.len().to_string()),
        present(m.og_title.is_some()),
        present(m.og_description.is_some()),
        m.article_tags.len()
    );
    println!(
        "extractor={} excluded_subtrees={} hidden_subtrees={}",
        d.extractor, d.excluded_subtrees, d.hidden_subtrees
    );
    let t = &d.tokens;
    println!(
        "tokens start_tags={} end_tags={} text={} comments={} parse_errors={} ignored_end_tags={} implicitly_closed={} max_open_depth={} untracked_elements={} excluded_chars={}",
        t.start_tags,
        t.end_tags,
        t.text_tokens,
        t.comments,
        t.parse_errors,
        t.ignored_end_tags,
        t.implicitly_closed,
        t.max_open_depth,
        t.untracked_elements,
        t.excluded_chars
    );
    println!(
        "warnings={}",
        if d.warnings.is_empty() {
            "-".to_owned()
        } else {
            d.warnings.join(",")
        }
    );
    println!(
        "time_us mime={} charset_detect={} decode_tokenize_extract={} total_with_read={}",
        d.timings.mime_us,
        d.timings.charset_detect_us,
        d.timings.decode_tokenize_extract_us,
        total.as_micros()
    );
    println!(
        "memory peak_heap_bytes={} peak_working_set_bytes={}",
        PEAK.load(Ordering::Relaxed),
        peak_working_set().map_or("n/a".to_owned(), |v| v.to_string())
    );

    if let Some(out) = output {
        let json = match serde_json::to_vec_pretty(&extraction) {
            Ok(j) => j,
            Err(_) => {
                eprintln!("status=error code=output_write_failed");
                return ExitCode::from(3);
            }
        };
        // create_new: fails if the file appeared meanwhile; a failed write removes what was created.
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&out) {
            Ok(f) => f,
            Err(_) => {
                eprintln!("status=error code=output_exists_or_unwritable");
                return ExitCode::from(3);
            }
        };
        if file
            .write_all(&json)
            .and_then(|()| file.sync_all())
            .is_err()
        {
            drop(file);
            let _ = std::fs::remove_file(&out);
            eprintln!("status=error code=output_write_failed");
            return ExitCode::from(3);
        }
        println!("output=written bytes={}", json.len());
    }
    ExitCode::SUCCESS
}
