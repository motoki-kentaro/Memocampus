//! Startup diagnostics: one JSON line per event in `<app dir>\logs\mcp-startup.log`, where the app
//! directory is `%LOCALAPPDATA%\Memocampus` (or `MEMOCAMPUS_APP_DIR`, as for the policy).
//!
//! What a line may hold is fixed here: event names, stages, error codes and kinds, OS error
//! numbers, the protocol version and client name a client announces, tool names this server
//! defines, and facts about the process (paths of the executable and working directory, the
//! parent's file name, the kind of each standard handle). Never a vault path, URL, title, query,
//! argument value, message body or policy rule.
//!
//! Writing never fails the server: every error is ignored, nothing here panics, nothing is held
//! across a write (so a panic hook can log safely), and stdout and stderr are never touched.
//! Three generations of about 5 MiB each are kept: `mcp-startup.log`, `.1` and `.2`.

use serde_json::{json, Map, Value};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub const FILE_NAME: &str = "mcp-startup.log";
pub const MAX_BYTES: u64 = 5 * 1024 * 1024;
/// The current file and two older ones.
const OLDER_GENERATIONS: u32 = 2;
/// Text taken from a client (its name, version and protocol version) is cut to this and reduced to
/// `[A-Za-z0-9._@/+ -]`.
const MAX_CLIENT_TEXT: usize = 64;

struct Log {
    file: PathBuf,
    started: Instant,
    pid: u32,
}

static LOG: OnceLock<Option<Log>> = OnceLock::new();

fn non_empty(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// The same application directory the policy store uses. Only an absolute one: a log is never
/// written relative to the working directory.
pub fn app_dir() -> Option<(PathBuf, &'static str)> {
    let (dir, source) = match non_empty("MEMOCAMPUS_APP_DIR") {
        Some(dir) => (dir, "MEMOCAMPUS_APP_DIR"),
        None => (
            non_empty("LOCALAPPDATA")?.join("Memocampus"),
            "LOCALAPPDATA",
        ),
    };
    dir.is_absolute().then_some((dir, source))
}

/// Called first in `main`. Without a usable directory nothing is ever written.
pub fn init() {
    LOG.get_or_init(|| {
        let (dir, _) = app_dir()?;
        let dir = dir.join("logs");
        let _ = fs::create_dir_all(&dir);
        Some(Log {
            file: dir.join(FILE_NAME),
            started: Instant::now(),
            pid: std::process::id(),
        })
    });
}

fn generation(file: &Path, n: u32) -> PathBuf {
    let mut name = file.as_os_str().to_owned();
    name.push(format!(".{n}"));
    PathBuf::from(name)
}

/// Oldest first. Another process may rotate at the same time; the worst case is one lost generation.
fn rotate(file: &Path) {
    let _ = fs::remove_file(generation(file, OLDER_GENERATIONS));
    for n in (1..OLDER_GENERATIONS).rev() {
        let _ = fs::rename(generation(file, n), generation(file, n + 1));
    }
    let _ = fs::rename(file, generation(file, 1));
}

fn utc_now() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
        .and_then(crate::dates::format_ms)
        .unwrap_or_default()
}

fn line(log: &Log, event: &str, ok: bool, fields: Value) -> String {
    let mut text = format!(
        "{{\"ts\":{},\"pid\":{},\"elapsed_ms\":{},\"event\":{},\"ok\":{ok}",
        Value::from(utc_now()),
        log.pid,
        log.started.elapsed().as_millis(),
        Value::from(event),
    );
    if let Value::Object(fields) = fields {
        for (key, value) in fields {
            text.push(',');
            text.push_str(&Value::from(key).to_string());
            text.push(':');
            text.push_str(&value.to_string());
        }
    }
    text.push_str("}\n");
    text
}

/// One event. `fields` must be an object built from the values this module allows.
pub fn event(event: &str, ok: bool, fields: Value) {
    let Some(Some(log)) = LOG.get() else {
        return;
    };
    append(&log.file, &line(log, event, ok, fields));
}

/// A line that would take the file past `MAX_BYTES` starts a new one.
fn append(path: &Path, text: &str) {
    if fs::metadata(path).is_ok_and(|meta| meta.len() + text.len() as u64 > MAX_BYTES) {
        rotate(path);
    }
    // One write per line, appended, so lines of concurrent processes do not interleave.
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(text.as_bytes());
    }
}

/// An I/O error as its kind and OS error number, never its message (which may name a path).
pub fn io_error(error: &std::io::Error) -> Value {
    json!({
        "error_kind": format!("{:?}", error.kind()),
        "os_error": error.raw_os_error(),
    })
}

/// Merges two objects of fields.
pub fn with(mut base: Value, extra: Value) -> Value {
    if let (Value::Object(base), Value::Object(extra)) = (&mut base, extra) {
        base.extend(extra);
    }
    base
}

/// Client-supplied text reduced to a short, harmless label.
pub fn client_text(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_ascii_alphanumeric() || "._@/+ -".contains(*c))
        .take(MAX_CLIENT_TEXT)
        .collect()
}

/// Facts about how this process was started.
pub fn startup_context() -> Value {
    let path_text = |path: std::io::Result<PathBuf>| match path {
        Ok(path) => Value::from(path.to_string_lossy().into_owned()),
        Err(error) => io_error(&error),
    };
    let mut env = Map::new();
    for name in ["LOCALAPPDATA", "MEMOCAMPUS_APP_DIR"] {
        env.insert(name.to_owned(), Value::from(non_empty(name).is_some()));
    }
    with(
        json!({
            "mode": "mcp",
            "version": env!("CARGO_PKG_VERSION"),
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "exe": path_text(std::env::current_exe()),
            "cwd": path_text(std::env::current_dir()),
            "argc": std::env::args_os().count().saturating_sub(1),
            "env_set": env,
            "log_dir_source": app_dir().map(|(_, source)| source),
        }),
        platform::context(),
    )
}

/// The vault file as the OS sees it, for a vault that was refused. No path.
pub fn vault_file(path: &Path) -> Value {
    match fs::metadata(path) {
        Ok(meta) => {
            json!({"vault_file": {"exists": true, "is_file": meta.is_file(), "len": meta.len()}})
        }
        Err(error) => json!({"vault_file": with(json!({"exists": false}), io_error(&error))}),
    }
}

/// Logs a panic's thread and location, never its message, then runs the hook that was there.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|at| format!("{}:{}", at.file(), at.line()));
        event(
            "process_error",
            false,
            json!({
                "stage": "panic",
                "thread": std::thread::current().name().map(client_text),
                "location": location,
            }),
        );
        previous(info);
    }));
}

#[cfg(windows)]
mod platform {
    use serde_json::{json, Value};
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileType, FILE_TYPE_CHAR, FILE_TYPE_DISK, FILE_TYPE_PIPE,
    };
    use windows_sys::Win32::System::Console::{
        GetConsoleProcessList, GetConsoleWindow, GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE,
        STD_OUTPUT_HANDLE,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    fn handle_kind(which: u32) -> &'static str {
        // SAFETY: GetStdHandle and GetFileType only read the handle table.
        unsafe {
            let handle = GetStdHandle(which);
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                return "none";
            }
            match GetFileType(handle) {
                FILE_TYPE_PIPE => "pipe",
                FILE_TYPE_CHAR => "char",
                FILE_TYPE_DISK => "disk",
                _ => "unknown",
            }
        }
    }

    /// `(parent PID, parent file name)` from a process snapshot.
    fn parent() -> Option<(u32, Option<String>)> {
        let me = std::process::id();
        let mut parent_pid = None;
        let mut names = Vec::new();
        // SAFETY: the snapshot handle is closed below; the entry is a plain struct sized for the call.
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snapshot == INVALID_HANDLE_VALUE {
                return None;
            }
            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut more = Process32FirstW(snapshot, &mut entry) != 0;
            while more {
                let len = entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len());
                let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
                if entry.th32ProcessID == me {
                    parent_pid = Some(entry.th32ParentProcessID);
                }
                names.push((entry.th32ProcessID, name));
                more = Process32NextW(snapshot, &mut entry) != 0;
            }
            CloseHandle(snapshot);
        }
        let parent_pid = parent_pid?;
        let name = names
            .into_iter()
            .find(|(pid, _)| *pid == parent_pid)
            .map(|(_, name)| super::client_text(&name));
        Some((parent_pid, name))
    }

    pub fn context() -> Value {
        // SAFETY: both only query this process's console.
        let (window, processes) = unsafe {
            let mut list = [0u32; 16];
            (
                !GetConsoleWindow().is_null(),
                GetConsoleProcessList(list.as_mut_ptr(), list.len() as u32),
            )
        };
        let parent = parent();
        json!({
            "stdin": handle_kind(STD_INPUT_HANDLE),
            "stdout": handle_kind(STD_OUTPUT_HANDLE),
            "stderr": handle_kind(STD_ERROR_HANDLE),
            "console_window": window,
            "console_processes": processes,
            "parent_pid": parent.as_ref().map(|(pid, _)| *pid),
            "parent_name": parent.and_then(|(_, name)| name),
        })
    }
}

#[cfg(not(windows))]
mod platform {
    pub fn context() -> serde_json::Value {
        serde_json::json!({})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_text_keeps_a_short_plain_label_only() {
        assert_eq!(client_text("codex-mcp-client 0.1"), "codex-mcp-client 0.1");
        assert_eq!(client_text("a\"b\n{c}<d>\\e"), "abcde");
        assert_eq!(client_text(&"x".repeat(500)).len(), MAX_CLIENT_TEXT);
    }

    #[test]
    fn rotation_keeps_three_generations() {
        let dir = std::env::temp_dir().join(format!("memocampus-diag-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join(FILE_NAME);
        for round in 0..5 {
            fs::write(&file, format!("{round}")).unwrap();
            rotate(&file);
        }
        let mut names: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, ["mcp-startup.log.1", "mcp-startup.log.2"]);
        assert_eq!(fs::read_to_string(generation(&file, 1)).unwrap(), "4");
        assert_eq!(fs::read_to_string(generation(&file, 2)).unwrap(), "3");

        // By size: a full file moves to `.1` before the line that would overfill it.
        fs::write(&file, vec![b'x'; MAX_BYTES as usize - 4]).unwrap();
        append(&file, "abc\n");
        assert_eq!(fs::metadata(&file).unwrap().len(), MAX_BYTES);
        append(&file, "next\n");
        assert_eq!(fs::read_to_string(&file).unwrap(), "next\n");
        assert_eq!(fs::metadata(generation(&file, 1)).unwrap().len(), MAX_BYTES);
        assert_eq!(fs::read_to_string(generation(&file, 2)).unwrap(), "4");
        assert!(!generation(&file, 3).exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_line_is_one_json_object_with_the_fixed_fields_first() {
        let log = Log {
            file: PathBuf::new(),
            started: Instant::now(),
            pid: 7,
        };
        let text = line(&log, "x", true, json!({"stage": "s\n\"q"}));
        assert!(text.ends_with('\n') && text.matches('\n').count() == 1);
        assert!(text.starts_with("{\"ts\":\""));
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            (
                &value["pid"],
                &value["event"],
                &value["ok"],
                &value["stage"]
            ),
            (&json!(7), &json!("x"), &json!(true), &json!("s\n\"q"))
        );
    }
}
