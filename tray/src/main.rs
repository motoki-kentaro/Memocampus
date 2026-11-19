//! `memocampus-tray.exe` — Slice 1: the supervisor host, without a tray icon yet.
//!
//! It is a Windows GUI-subsystem program (no console window), so it reports through a log file only.
//! Slice 2 puts the message loop, the icon and the menu on top of the same supervisor; until then
//! this binary starts the server (if `server_autostart`), waits, and stops it.
//!
//!   memocampus-tray.exe [--run-seconds <n>] [--log <file>] [--no-autostart]
//!                       [--mcp-exe <path>] [--mcp-arg <arg>]...
//!
//! `--run-seconds` ends the run after `n` seconds with a graceful stop (without it, it runs until it
//! is killed, which is how the orphan guard is tested). `--log` appends one line per state change.
//! `--mcp-exe` and `--mcp-arg` replace the server executable and add arguments; they exist for the
//! tests and are not a way to change what a normal start runs.
//!
//! Exit codes: 0 done, 2 bad command line, 3 another tray runs for this application directory,
//! 4 no usable application directory.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(not(windows))]
fn main() {
    std::process::exit(1);
}

#[cfg(windows)]
fn main() {
    std::process::exit(host::run());
}

#[cfg(windows)]
mod host {
    use memocampus_tray::instance::{self, InstanceError};
    use memocampus_tray::settings;
    use memocampus_tray::state::{RestartPolicy, ServerState, Snapshot};
    use memocampus_tray::supervisor::{Command, Supervisor, Timings};
    use memocampus_tray::winproc::WinLauncher;
    use std::ffi::OsString;
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    struct Args {
        run_seconds: Option<u64>,
        log: Option<PathBuf>,
        autostart: bool,
        mcp_exe: Option<PathBuf>,
        mcp_args: Vec<OsString>,
    }

    fn parse() -> Option<Args> {
        let mut args = Args {
            run_seconds: None,
            log: None,
            autostart: true,
            mcp_exe: None,
            mcp_args: Vec::new(),
        };
        let mut it = std::env::args_os().skip(1);
        while let Some(name) = it.next() {
            match name.to_str()? {
                "--run-seconds" => args.run_seconds = Some(it.next()?.to_str()?.parse().ok()?),
                "--log" => args.log = Some(PathBuf::from(it.next()?)),
                "--no-autostart" => args.autostart = false,
                "--mcp-exe" => args.mcp_exe = Some(PathBuf::from(it.next()?)),
                "--mcp-arg" => args.mcp_args.push(it.next()?),
                _ => return None,
            }
        }
        Some(args)
    }

    pub fn run() -> i32 {
        let Some(args) = parse() else { return 2 };
        let log = args.log.clone();
        let write_line = move |text: String| {
            if let Some(path) = &log {
                if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
                    let _ = writeln!(file, "{text}");
                }
            }
        };
        let Some(app_dir) = settings::app_dir() else {
            write_line("exit=no_app_dir".into());
            return 4;
        };
        let _instance = match instance::acquire(&app_dir) {
            Ok(instance) => instance,
            Err(InstanceError::AlreadyRunning) => {
                write_line("exit=already_running".into());
                return 3;
            }
            Err(InstanceError::Failed) => {
                write_line("exit=instance_failed".into());
                return 3;
            }
        };

        // The server executable: beside this one, as installed, unless a test says otherwise.
        let exe = args.mcp_exe.clone().unwrap_or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|me| me.parent().map(|dir| dir.join("memocampus-mcp.exe")))
                .unwrap_or_else(|| PathBuf::from("memocampus-mcp.exe"))
        });
        let mut launcher = WinLauncher::new(exe);
        launcher.extra_args = args.mcp_args.clone();

        let settings_dir = app_dir.clone();
        let current = Arc::new(Mutex::new(None::<Snapshot>));
        let seen = Arc::clone(&current);
        let log_lines = write_line.clone();
        let supervisor = Supervisor::spawn(
            Box::new(launcher),
            Box::new(move || settings::load(&settings_dir)),
            Timings::standard(),
            RestartPolicy::standard(),
            Box::new(move |snapshot| {
                log_lines(format!(
                    "state={:?} pid={:?} port={:?} vault={:?} restarts={} last_exit={:?}",
                    snapshot.state,
                    snapshot.pid,
                    snapshot.port,
                    snapshot.vault,
                    snapshot.restarts,
                    snapshot.last_exit
                ));
                *seen.lock().unwrap() = Some(snapshot.clone());
            }),
        );

        let wanted = match settings::load(&app_dir) {
            Ok(settings) => settings.server_autostart,
            // The supervisor reports the same error as a state; nothing is started on a guess.
            Err(_) => true,
        };
        if args.autostart && wanted {
            supervisor.send(Command::Start);
        }
        let until = args
            .run_seconds
            .map(|seconds| Instant::now() + Duration::from_secs(seconds));
        loop {
            if until.is_some_and(|until| Instant::now() >= until) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        supervisor.shutdown();
        let end = current.lock().unwrap().clone();
        write_line(format!(
            "exit=done final={:?}",
            end.map(|snapshot| snapshot.state)
                .unwrap_or(ServerState::Stopped)
        ));
        0
    }
}
