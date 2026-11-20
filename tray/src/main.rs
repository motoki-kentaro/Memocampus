//! `memocampus-tray.exe`: the Windows tray application (Issue #67). A GUI-subsystem program: no
//! console window, ever. It shows the tray icon and menu, starts the Streamable HTTP server of
//! `memocampus-mcp.exe` (the one beside it) when `server_autostart` is on, and registers itself for
//! logon through the per-user Run key when the user asks (or, once, on the very first normal start).
//!
//!   memocampus-tray.exe                      the tray (what the Run key starts)
//!
//! Options for verification only; none is needed or used in normal operation:
//!
//!   --log <file>         append state changes and actions to a file
//!   --run-seconds <n>    quit by itself after n seconds
//!   --mcp-exe <path>     another server executable (a stand-in); --mcp-arg <arg> adds an argument
//!   --registry-root <id> use a private copy of the Run key under HKCU\Software\MemocampusTrayTest
//!   --selftest           run a scripted sequence through the same actions as the menu, then quit
//!   --headless           Slice 1's host: no icon, no menu, no registration (`--no-autostart`
//!                        does not start the server)
//!
//! Which registry a run may write is decided once, before anything touches it (`mode`): only a start
//! with none of the options above and no `MEMOCAMPUS_APP_DIR` is the product, with the user's real
//! Run key and the first-run registration (settings file absent, so the default "start at logon"
//! applies). With `--registry-root` the run gets a private copy of the keys. Any other verification
//! option leaves the real Run key readable and **unwritable**: no registration, toggle, update or
//! removal can reach it. `--selftest` drives toggles, so it is refused (exit 2) without
//! `--registry-root`.
//!
//! Exit codes: 0 done, 2 bad command line, 3 another tray runs for this application directory,
//! 4 no usable application directory, 5 no tray icon could be created, 6 a self-test step failed.
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
    use memocampus_tray::autostart::WinRegistry;
    use memocampus_tray::instance::{self, InstanceError};
    use memocampus_tray::mode::{self, Flags, RunMode};
    use memocampus_tray::settings;
    use memocampus_tray::state::{RestartPolicy, ServerState, Snapshot};
    use memocampus_tray::supervisor::{Command, Supervisor, Timings};
    use memocampus_tray::ui::{self, UiConfig};
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
        headless: bool,
        registry_root: Option<String>,
        selftest: bool,
    }

    fn parse() -> Option<Args> {
        let mut args = Args {
            run_seconds: None,
            log: None,
            autostart: true,
            mcp_exe: None,
            mcp_args: Vec::new(),
            headless: false,
            registry_root: None,
            selftest: false,
        };
        let mut it = std::env::args_os().skip(1);
        while let Some(name) = it.next() {
            match name.to_str()? {
                "--run-seconds" => args.run_seconds = Some(it.next()?.to_str()?.parse().ok()?),
                "--log" => args.log = Some(PathBuf::from(it.next()?)),
                "--no-autostart" => args.autostart = false,
                "--mcp-exe" => args.mcp_exe = Some(PathBuf::from(it.next()?)),
                "--mcp-arg" => args.mcp_args.push(it.next()?),
                "--headless" => args.headless = true,
                "--selftest" => args.selftest = true,
                "--registry-root" => args.registry_root = Some(it.next()?.to_str()?.to_owned()),
                _ => return None,
            }
        }
        Some(args)
    }

    /// The tray: icon, menu, message loop. The single-instance guard lives as long as it runs.
    fn run_ui(
        args: &Args,
        mode: &RunMode,
        app_dir: PathBuf,
        _instance: instance::SingleInstance,
    ) -> i32 {
        // The registry this run may use follows from its mode alone.
        let registry = match WinRegistry::for_mode(mode) {
            Ok(registry) => registry,
            Err(_) => return 2,
        };
        let mcp_exe = args.mcp_exe.clone().unwrap_or_else(default_mcp_exe);
        let product = mode.first_run_registration();
        ui::run(UiConfig {
            app_dir,
            mcp_exe,
            mcp_args: args.mcp_args.clone(),
            registry,
            product,
            log: args.log.clone(),
            run_seconds: args.run_seconds,
            selftest: args.selftest,
        })
    }

    /// The server executable: beside this one, as installed.
    fn default_mcp_exe() -> PathBuf {
        std::env::current_exe()
            .ok()
            .and_then(|me| me.parent().map(|dir| dir.join("memocampus-mcp.exe")))
            .unwrap_or_else(|| PathBuf::from("memocampus-mcp.exe"))
    }

    pub fn run() -> i32 {
        let Some(args) = parse() else { return 2 };
        let log = args.log.clone();
        // Before anything else, and before any registry access: which registry this run may write.
        let mode = match mode::decide(&Flags {
            registry_root: args.registry_root.clone(),
            selftest: args.selftest,
            run_seconds: args.run_seconds.is_some(),
            log: args.log.is_some(),
            mcp_exe: args.mcp_exe.is_some(),
            mcp_args: !args.mcp_args.is_empty(),
            no_autostart: !args.autostart,
            headless: args.headless,
            app_dir_override: std::env::var_os("MEMOCAMPUS_APP_DIR").is_some(),
        }) {
            Ok(mode) => mode,
            Err(_) => {
                if let Some(path) = &log {
                    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
                        let _ = writeln!(file, "exit=selftest_needs_isolated_registry");
                    }
                }
                return 2;
            }
        };
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

        if !args.headless {
            return run_ui(&args, &mode, app_dir, _instance);
        }

        // The server executable: beside this one, as installed, unless a test says otherwise.
        let exe = args.mcp_exe.clone().unwrap_or_else(default_mcp_exe);
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
