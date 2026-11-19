//! A stand-in for `memocampus-mcp` for the tray's tests: it prints the same stderr lines the real
//! server prints, honours the same management channel, and misbehaves on request. It reads and
//! serves nothing.
//!
//! The mode is the first `--stub <mode>` argument; the rest of the command line (the settings'
//! arguments, `--management-pipe-handle <n>`) is read as the real server reads it.
//!
//! Modes: `serve` (default), `refuse-vault`, `port-in-use`, `crash`, `hang` (serves, ignores a stop
//! request), `silent` (never says it listens), `idle` (no management channel needed: sleeps).
//!
//! A line is appended to the file named by `--stub-pid-file <path>` at each start: this process's
//! id (so a test finds the process it started without looking one up by name) and whether it has a
//! console window or is attached to a console at all.

#[cfg(windows)]
fn main() {
    use std::io::Write;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::ReadFile;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let after = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|at| args.get(at + 1))
            .cloned()
    };
    if let Some(path) = after("--stub-pid-file") {
        use windows_sys::Win32::System::Console::{GetConsoleProcessList, GetConsoleWindow};
        // SAFETY: plain queries about this process's console; the list buffer is live.
        let (window, attached) = unsafe {
            let mut list = [0u32; 4];
            (
                !GetConsoleWindow().is_null(),
                GetConsoleProcessList(list.as_mut_ptr(), 4),
            )
        };
        // One line per start, so a test sees every launch of a restarted server.
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(
                file,
                "pid={} console_window={window} console_processes={attached}",
                std::process::id()
            );
        }
    }
    let mode = after("--stub").unwrap_or_else(|| "serve".into());
    let say = |text: &str| {
        let mut err = std::io::stderr();
        let _ = writeln!(err, "memocampus-mcp: {text}");
        let _ = err.flush();
    };
    match mode.as_str() {
        "refuse-vault" => {
            say("vault refused: vault_publication_pending");
            std::process::exit(2);
        }
        "port-in-use" => {
            say("cannot listen: address_in_use");
            std::process::exit(2);
        }
        "idle" => loop {
            std::thread::sleep(std::time::Duration::from_secs(60));
        },
        _ => {}
    }
    let port = after("--listen")
        .and_then(|a| a.rsplit(':').next().map(str::to_owned))
        .unwrap_or_else(|| "0".into());
    if mode != "silent" {
        say(&format!("listening on http://127.0.0.1:{port}/mcp"));
    }
    if mode == "crash" {
        std::thread::sleep(std::time::Duration::from_millis(300));
        std::process::exit(101);
    }
    let handle: Option<usize> = after("--management-pipe-handle").and_then(|h| h.parse().ok());
    let Some(handle) = handle else {
        // No management channel: nothing can stop this but the tray's job object.
        loop {
            std::thread::sleep(std::time::Duration::from_secs(60));
        }
    };
    let mut byte = 0u8;
    let mut read = 0u32;
    // SAFETY: the inherited read end of the management pipe; `byte` and `read` are live.
    let ok = unsafe {
        ReadFile(
            handle as HANDLE,
            (&mut byte as *mut u8).cast(),
            1,
            &mut read,
            std::ptr::null_mut(),
        )
    };
    let reason = if ok != 0 && read == 1 && byte == b'S' {
        "management_stop"
    } else {
        "management_closed"
    };
    if mode == "hang" {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(60));
        }
    }
    say(&format!("stopped ({reason})"));
}

#[cfg(not(windows))]
fn main() {}
