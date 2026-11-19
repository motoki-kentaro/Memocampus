//! The supervisor against real processes on Windows: a stand-in server (`examples/stub_mcp.rs`)
//! that prints the real server's stderr lines and honours the same management channel. The real
//! `memocampus-mcp` is exercised against the same channel in `mcp-server/tests/http_mcp.rs`.
//!
//! Isolated: every test uses its own temporary application directory and process; nothing here
//! reads or changes the user's Memocampus data, settings or any server already running.
#![cfg(windows)]

use memocampus_tray::settings::{self, Settings};
use memocampus_tray::state::{
    ErrorKind, LastExit, RestartPolicy, ServerState, Snapshot, VaultStatus,
};
use memocampus_tray::supervisor::{Command, Supervisor, Timings};
use memocampus_tray::winproc::WinLauncher;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command as Process, Stdio};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
use windows_sys::Win32::System::Threading::{
    OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
};

const TRAY: &str = env!("CARGO_BIN_EXE_memocampus-tray");

fn stub() -> PathBuf {
    // `cargo test` builds the examples beside the binaries: target/<profile>/examples. The tray
    // binary's own location is the reliable anchor (a test binary may sit in a build directory).
    let exe = Path::new(TRAY)
        .parent()
        .unwrap()
        .join("examples")
        .join("stub_mcp.exe");
    assert!(exe.is_file(), "build the example first: {}", exe.display());
    exe
}

struct Temp(PathBuf);
impl Temp {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "memocampus-tray-it-{name}-{}-{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
                ^ std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}
/// Kills and reaps a process the test started, even when the test panics first.
struct Reaped(Child);
impl Drop for Reaped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn alive(pid: u32) -> bool {
    // SAFETY: a query about a process by id with the least access; the handle is closed.
    unsafe {
        let handle = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if handle.is_null() {
            return false;
        }
        let running = WaitForSingleObject(handle, 0) == WAIT_TIMEOUT;
        CloseHandle(handle);
        running
    }
}

fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(15);
    while !condition() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// `pid=<n> console_window=<b> console_processes=<n>` lines the stub appended, one per start.
fn starts(file: &Path) -> Vec<(u32, String)> {
    fs::read_to_string(file)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let pid = line
                .split_whitespace()
                .next()?
                .strip_prefix("pid=")?
                .parse()
                .ok()?;
            Some((pid, line.to_owned()))
        })
        .collect()
}

struct Rig {
    supervisor: Supervisor,
    info: PathBuf,
    _dir: Temp,
}

fn timings() -> Timings {
    Timings {
        start_timeout: Duration::from_millis(1500),
        stop_timeout: Duration::from_millis(1500),
        kill_wait: Duration::from_secs(3),
        poll: Duration::from_millis(20),
    }
}

fn rig(mode: &str, policy: RestartPolicy) -> Rig {
    let dir = Temp::new(mode);
    let info = dir.file("starts.txt");
    let mut launcher = WinLauncher::new(stub());
    launcher.extra_args = vec![
        "--stub".into(),
        mode.into(),
        "--stub-pid-file".into(),
        info.clone().into(),
    ];
    let supervisor = Supervisor::spawn(
        Box::new(launcher),
        Box::new(|| Ok(Settings::default())),
        timings(),
        policy,
        Box::new(|_| {}),
    );
    Rig {
        supervisor,
        info,
        _dir: dir,
    }
}

fn wait_state(rig: &Rig, what: &str, condition: impl Fn(&Snapshot) -> bool) -> Snapshot {
    let mut last = rig.supervisor.snapshot();
    wait_until(what, || {
        last = rig.supervisor.snapshot();
        condition(&last)
    });
    last
}

fn once() -> RestartPolicy {
    RestartPolicy::new(0, Duration::from_secs(60), vec![Duration::from_millis(20)])
}

#[test]
fn a_started_server_has_no_console_and_stops_over_the_management_pipe() {
    let rig = rig("serve", once());
    rig.supervisor.send(Command::Start);
    let up = wait_state(&rig, "running", |s| s.state == ServerState::Running);
    assert_eq!(up.vault, VaultStatus::Unknown);
    assert!(up.port.is_some());
    let launches = starts(&rig.info);
    assert_eq!(launches.len(), 1);
    let (pid, line) = &launches[0];
    assert_eq!(up.pid, Some(*pid));
    // No console window and no console at all: there is nothing to show.
    assert!(
        line.contains("console_window=false console_processes=0"),
        "{line}"
    );
    assert!(alive(*pid));

    rig.supervisor.send(Command::Stop);
    let stopped = wait_state(&rig, "stopped", |s| s.state == ServerState::Stopped);
    // It left by the stop request: exit 0, no termination.
    assert_eq!(stopped.last_exit, Some(LastExit::Graceful));
    wait_until("the process to be gone", || !alive(*pid));
}

#[test]
fn restart_and_the_restart_after_a_crash_start_silent_servers_too() {
    let rig = rig(
        "serve",
        RestartPolicy::new(3, Duration::from_secs(60), vec![Duration::from_millis(20)]),
    );
    rig.supervisor.send(Command::Start);
    wait_state(&rig, "running", |s| s.state == ServerState::Running);
    rig.supervisor.send(Command::Restart);
    wait_until("a second start", || starts(&rig.info).len() == 2);
    wait_state(&rig, "running again", |s| {
        s.state == ServerState::Running && s.pid == Some(starts(&rig.info)[1].0)
    });
    let launches = starts(&rig.info);
    assert_ne!(launches[0].0, launches[1].0);
    wait_until("the first to be gone", || !alive(launches[0].0));
    for (_, line) in &launches {
        assert!(
            line.contains("console_window=false console_processes=0"),
            "{line}"
        );
    }
}

#[test]
fn a_crash_is_seen_from_the_exit_code_and_a_refusal_from_its_code() {
    let crash = rig("crash", once());
    crash.supervisor.send(Command::Start);
    let end = wait_state(&crash, "the crash", |s| {
        matches!(s.state, ServerState::Error(_))
    });
    assert_eq!(end.state, ServerState::Error(ErrorKind::CrashLoop));
    assert_eq!(end.last_exit, Some(LastExit::Code(101)));
    assert_eq!(end.pid, None);

    let vault = rig("refuse-vault", once());
    vault.supervisor.send(Command::Start);
    let end = wait_state(&vault, "the refusal", |s| {
        matches!(s.state, ServerState::Error(_))
    });
    assert_eq!(
        end.state,
        ServerState::Error(ErrorKind::VaultRefused("vault_publication_pending".into()))
    );
    assert_eq!(
        end.vault,
        VaultStatus::Refused("vault_publication_pending".into())
    );
    assert_eq!(end.restarts, 0);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(starts(&vault.info).len(), 1, "a refusal is not retried");

    let port = rig("port-in-use", once());
    port.supervisor.send(Command::Start);
    let end = wait_state(&port, "the refusal", |s| {
        matches!(s.state, ServerState::Error(_))
    });
    assert_eq!(end.state, ServerState::Error(ErrorKind::PortInUse));
}

#[test]
fn a_server_that_ignores_the_stop_is_terminated_and_only_that_one() {
    let bystander_info = Temp::new("bystander");
    let info = bystander_info.file("starts.txt");
    let mut bystander = Reaped(
        Process::new(stub())
            .args(["--stub", "idle", "--stub-pid-file"])
            .arg(&info)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let bystander_pid = bystander.0.id();

    let rig = rig("hang", once());
    rig.supervisor.send(Command::Start);
    wait_state(&rig, "running", |s| s.state == ServerState::Running);
    let pid = starts(&rig.info)[0].0;
    let began = Instant::now();
    rig.supervisor.send(Command::Stop);
    let stopped = wait_state(&rig, "stopped", |s| s.state == ServerState::Stopped);
    assert!(
        began.elapsed() >= Duration::from_millis(1400),
        "it waited for the timeout first"
    );
    assert_eq!(stopped.last_exit, Some(LastExit::Forced));
    wait_until("the hung server to be gone", || !alive(pid));
    // The same kind of process, started by someone else, is untouched.
    assert!(alive(bystander_pid), "an unrelated server was stopped");
    drop(rig);
    assert!(alive(bystander_pid));
    bystander.0.kill().unwrap();
}

#[test]
fn a_server_that_never_listens_is_an_error_and_is_ended() {
    let rig = rig("silent", once());
    rig.supervisor.send(Command::Start);
    let end = wait_state(&rig, "the timeout", |s| {
        matches!(s.state, ServerState::Error(_))
    });
    assert_eq!(end.state, ServerState::Error(ErrorKind::StartTimeout));
    let pid = starts(&rig.info)[0].0;
    wait_until("it to be gone", || !alive(pid));
}

#[test]
fn a_missing_executable_is_reported_and_nothing_starts() {
    let supervisor = Supervisor::spawn(
        Box::new(WinLauncher::new(PathBuf::from(
            "C:\\definitely\\not\\here\\memocampus-mcp.exe",
        ))),
        Box::new(|| Ok(Settings::default())),
        timings(),
        once(),
        Box::new(|_| {}),
    );
    supervisor.send(Command::Start);
    wait_until("the error", || {
        supervisor.snapshot().state == ServerState::Error(ErrorKind::McpMissing)
    });
}

/// Starts the tray program with its own isolated application directory.
struct TrayProcess {
    child: Reaped,
    log: PathBuf,
    info: PathBuf,
    app: PathBuf,
    _dir: Temp,
}

fn tray(extra: &[&str], app: Option<&Path>) -> TrayProcess {
    tray_mode("serve", extra, app)
}

fn tray_mode(mode: &str, extra: &[&str], app: Option<&Path>) -> TrayProcess {
    let dir = Temp::new("tray");
    let app_dir = app
        .map(Path::to_path_buf)
        .unwrap_or_else(|| dir.file("app"));
    fs::create_dir_all(&app_dir).unwrap();
    let (log, info) = (dir.file("tray.log"), dir.file("starts.txt"));
    let child = Process::new(TRAY)
        .arg("--log")
        .arg(&log)
        .arg("--mcp-exe")
        .arg(stub())
        .args([
            "--mcp-arg",
            "--stub",
            "--mcp-arg",
            mode,
            "--mcp-arg",
            "--stub-pid-file",
            "--mcp-arg",
        ])
        .arg(&info)
        .args(extra)
        .env("MEMOCAMPUS_APP_DIR", &app_dir)
        .env("LOCALAPPDATA", dir.file("localappdata"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    TrayProcess {
        child: Reaped(child),
        log,
        info,
        app: app_dir,
        _dir: dir,
    }
}

#[test]
fn killing_the_tray_does_not_leave_the_server_behind() {
    let mut tray = tray(&[], None);
    wait_until("the server to run", || {
        fs::read_to_string(&tray.log)
            .unwrap_or_default()
            .contains("state=Running")
    });
    let server = starts(&tray.info)[0].0;
    assert!(alive(server));
    // As a crash or a Task Manager "End task" would: no chance to run any cleanup.
    tray.child.0.kill().unwrap();
    let _ = tray.child.0.wait();
    wait_until("the orphan guard to end the server", || !alive(server));
}

#[test]
fn the_job_object_alone_ends_a_server_that_ignores_both_the_stop_and_the_pipe_closing() {
    // `hang` prints that it listens, then ignores a stop request and ignores the management pipe
    // closing. Nothing but the job object can end it when the tray is killed: this separates the
    // orphan guard from the graceful path (where the server also leaves on its own when the pipe closes).
    let mut tray = tray_mode("hang", &[], None);
    wait_until("the server to run", || {
        fs::read_to_string(&tray.log)
            .unwrap_or_default()
            .contains("state=Running")
    });
    let server = starts(&tray.info)[0].0;
    assert!(alive(server));
    tray.child.0.kill().unwrap();
    let _ = tray.child.0.wait();
    wait_until("the job object to end the server", || !alive(server));
}

#[test]
fn a_quit_stops_the_server_gracefully_and_a_second_tray_is_refused() {
    let first = tray(&["--run-seconds", "4"], None);
    let mut first = first;
    wait_until("the server to run", || {
        fs::read_to_string(&first.log)
            .unwrap_or_default()
            .contains("state=Running")
    });
    let server = starts(&first.info)[0].0;

    // The same application directory: refused, and the first is not disturbed.
    let mut second = tray(&["--run-seconds", "1"], Some(&first.app));
    let second_status = second.child.0.wait().unwrap();
    assert_eq!(
        second_status.code(),
        Some(3),
        "{}",
        fs::read_to_string(&second.log).unwrap_or_default()
    );
    assert!(fs::read_to_string(&second.log)
        .unwrap()
        .contains("exit=already_running"));
    assert!(
        starts(&second.info).is_empty(),
        "the refused tray started no server"
    );
    assert!(alive(server));

    // Another application directory: allowed at the same time.
    let mut other = tray(&["--run-seconds", "1"], None);
    wait_until("the other server to run", || {
        fs::read_to_string(&other.log)
            .unwrap_or_default()
            .contains("state=Running")
    });
    assert_eq!(other.child.0.wait().unwrap().code(), Some(0));

    // The first ends by its own timer: a graceful stop, then exit 0.
    assert_eq!(first.child.0.wait().unwrap().code(), Some(0));
    let log = fs::read_to_string(&first.log).unwrap();
    assert!(log.contains("last_exit=Some(Graceful)"), "{log}");
    assert!(log.contains("exit=done final=Stopped"), "{log}");
    wait_until("the server to be gone", || !alive(server));
}

#[test]
fn an_invalid_settings_file_starts_nothing() {
    let dir = Temp::new("badsettings");
    let app = dir.file("app");
    fs::create_dir_all(&app).unwrap();
    fs::write(settings::path(&app), "{\"version\":2}").unwrap();
    let mut tray = tray(&["--run-seconds", "1"], Some(&app));
    assert_eq!(tray.child.0.wait().unwrap().code(), Some(0));
    let log = fs::read_to_string(&tray.log).unwrap();
    assert!(log.contains("settings_unknown_version"), "{log}");
    assert!(
        starts(&tray.info).is_empty(),
        "no server on a guessed value"
    );
    // The unknown file is untouched.
    assert_eq!(
        fs::read_to_string(settings::path(&app)).unwrap(),
        "{\"version\":2}"
    );
}
