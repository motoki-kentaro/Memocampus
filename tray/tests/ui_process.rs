//! The tray application itself (Issue #67, Slice 2): the real `memocampus-tray.exe` in its default
//! mode — tray icon, menu model, message loop — with a stand-in server (`examples/stub_mcp.rs`), an
//! isolated application directory and an **isolated copy of the Run key** under
//! `HKCU\Software\MemocampusTrayTest\<id>`. The user's real Run key is never written: it is only
//! read, before and after, to show that it is unchanged.
//!
//! What this cannot do and does not claim: click a real popup menu or look at the screen. The menu
//! actions are driven through the same message path a click uses (`--selftest`); that the icon
//! looks right, the popup opens and the console never flashes is for the person at the desktop.
//!
//! The tests that start the tray need a Windows desktop session with a notification area. Where the
//! icon cannot be created, or does not appear within 30 seconds, such a test **fails**: it never
//! returns early and passes. A machine without a desktop (a CI runner, a service session) runs
//! `cargo test --features no-desktop` instead: those tests are then `ignored`, which cargo counts
//! and prints as "ignored" in the summary, so "not run" can never be read as "passed".
//! (`cargo test -- --ignored` runs only them.) The registry tests need no desktop and always run.
#![cfg(windows)]

use memocampus_tray::autostart::{self, Approval, Autostart, RunRegistry, WinRegistry, VALUE_NAME};
use memocampus_tray::settings;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM, WAIT_TIMEOUT};
use windows_sys::Win32::System::Threading::{
    OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowThreadProcessId, PostMessageW, WM_CLOSE,
};

const TRAY: &str = env!("CARGO_BIN_EXE_memocampus-tray");

fn stub() -> PathBuf {
    let exe = Path::new(TRAY)
        .parent()
        .unwrap()
        .join("examples")
        .join("stub_mcp.exe");
    assert!(exe.is_file(), "build the example first: {}", exe.display());
    exe
}

fn unique(name: &str) -> String {
    format!(
        "{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}

struct Temp(PathBuf);
impl Temp {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("memocampus-tray-ui-{}", unique(name)));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// An isolated Run key that is removed again, whatever the test does.
struct Isolated {
    id: String,
    registry: WinRegistry,
}
impl Isolated {
    fn new(name: &str) -> Self {
        let id = unique(name);
        let registry = WinRegistry::isolated(&id).unwrap();
        Self { id, registry }
    }
}
impl Drop for Isolated {
    fn drop(&mut self) {
        let _ = self.registry.remove_isolated();
    }
}

struct Reaped(Child);
impl Drop for Reaped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
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
    let until = Instant::now() + Duration::from_secs(30);
    while !condition() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

struct Run {
    child: Reaped,
    app: PathBuf,
    log: PathBuf,
    starts: PathBuf,
    _dir: Temp,
}

impl Run {
    fn log(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }
}

/// Starts the tray in its default (UI) mode with a stand-in server in `mode`.
fn tray(mode: &str, registry: &Isolated, extra: &[&str], app: Option<&Path>) -> Run {
    let dir = Temp::new("run");
    let app_dir = app
        .map(Path::to_path_buf)
        .unwrap_or_else(|| dir.0.join("app"));
    fs::create_dir_all(&app_dir).unwrap();
    let (log, starts) = (dir.0.join("tray.log"), dir.0.join("starts.txt"));
    let child = Command::new(TRAY)
        .arg("--log")
        .arg(&log)
        .arg("--registry-root")
        .arg(&registry.id)
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
        .arg(&starts)
        .args(extra)
        .env("MEMOCAMPUS_APP_DIR", &app_dir)
        .env("LOCALAPPDATA", dir.0.join("localappdata"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    Run {
        child: Reaped(child),
        app: app_dir,
        log,
        starts,
        _dir: dir,
    }
}

fn server_pids(run: &Run) -> Vec<u32> {
    fs::read_to_string(&run.starts)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            line.split_whitespace()
                .next()?
                .strip_prefix("pid=")?
                .parse()
                .ok()
        })
        .collect()
}

/// Waits for the tray's icon. A missing notification area or a timeout is a failure of the test,
/// not a reason to skip it.
fn require_icon(run: &Run) {
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        let log = run.log();
        if log.contains("tray_icon=created") {
            return;
        }
        assert!(
            !log.contains("exit=no_tray_icon"),
            "the tray could not create its icon (is there a desktop with a notification area? \
             on a machine without one use `--features no-desktop`): {log}"
        );
        assert!(
            Instant::now() < until,
            "no tray icon within 30 seconds: {log}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn real_run_value() -> Option<String> {
    WinRegistry::user().run_value().unwrap()
}

#[test]
#[cfg_attr(
    feature = "no-desktop",
    ignore = "needs a desktop with a notification area"
)]
fn the_scripted_menu_actions_drive_the_server_and_the_settings_and_the_tray_quits_cleanly() {
    let real_before = real_run_value();
    let registry = Isolated::new("selftest");
    // Another application's entry in the same (isolated) Run key must come through untouched.
    registry
        .registry
        .put_other("SomeOtherApp", "\"C:\\Other\\other.exe\" --flag")
        .unwrap();
    let mut run = tray("serve", &registry, &["--selftest"], None);
    require_icon(&run);
    let status = run.child.0.wait().unwrap();
    let log = run.log();
    assert_eq!(status.code(), Some(0), "{log}");
    for step in [
        "running_after_start",
        "stop",
        "stays_stopped_after_manual_stop",
        "start",
        "restart_new_process",
        "toggle_autostart",
        "toggle_autostart_back",
        "toggle_server_autostart",
        "restart_notice_after_edit",
        "restart_applies_edit_and_clears_notice",
        "icon_loss_injected",
        "refused_update_is_not_counted_as_drawn",
        "retries_are_bounded_then_wait",
        "latest_state_is_drawn_after_taskbar_created",
    ] {
        assert!(
            log.contains(&format!("selftest step={step} ok")),
            "{step}: {log}"
        );
    }
    assert!(!log.contains("FAIL"), "{log}");
    // The tray took its language from this user's Windows UI language (Issue #78): there is no way
    // to choose it, so the run shows what this machine is set to.
    let language = memocampus_tray::i18n::Lang::detect().code();
    assert!(
        log.contains(&format!("ui_language={language}\n"))
            || log.contains(&format!("ui_language={language}\r\n")),
        "{log}"
    );

    // The states, in the order the supervisor reported them, and what each would show.
    let states: Vec<&str> = log.lines().filter(|l| l.starts_with("state=")).collect();
    let kinds: Vec<&str> = states
        .iter()
        .map(|l| &l["state=".len()..l.find(' ').unwrap()])
        .collect();
    // Starting is reported once, or twice when the pid is learned in a separate report: both are
    // fine. What matters is that Running comes only after Starting.
    let running = kinds.iter().position(|k| *k == "Running").expect("Running");
    assert!(
        running >= 1 && kinds[..running].iter().all(|k| *k == "Starting"),
        "{kinds:?}"
    );
    for expected in ["Stopping", "Stopped"] {
        assert!(kinds.contains(&expected), "{kinds:?}");
    }
    let views: Vec<&str> = log.lines().filter(|l| l.starts_with("view ")).collect();
    let find = |text: &str| views.iter().any(|v| v.contains(text));
    assert!(
        find("icon=Running") && find("start=false stop=true restart=true"),
        "{views:?}"
    );
    assert!(
        find("icon=Stopped") && find("start=true stop=false restart=false"),
        "{views:?}"
    );
    assert!(find("icon=Starting") && find("icon=Stopping"), "{views:?}");

    // The icon is in the notification area, and comes back when the shell announces a new taskbar.
    assert!(
        log.contains("icon_rect_before=true icon_rect_after_taskbar_created=true"),
        "{log}"
    );
    // The icon was really removed from the notification area (NIM_DELETE), the updates that followed
    // were refused and logged, and after the new-taskbar message the latest state was drawn and the
    // icon is back. (That the pixels are right is for the person at the desktop.)
    let lost = log
        .find("inject_icon_loss removed=true")
        .unwrap_or_else(|| panic!("the icon was not removed: {log}"));
    assert!(log[lost..].contains("icon_rect_after=false"), "{log}");
    let after_loss = &log[lost..];
    let refused = after_loss
        .find("icon_update_failed")
        .unwrap_or_else(|| panic!("no refused update after the loss: {log}"));
    let recovered = after_loss
        .find("taskbar_created")
        .unwrap_or_else(|| panic!("the tray did not see the new taskbar: {log}"));
    assert!(refused < recovered, "{log}");
    // S2-09: while the shell refuses, the tray retried exactly six times after the last state it
    // was asked to draw, then gave up and waited; no seventh retry follows.
    let until_taskbar = &after_loss[..recovered];
    let gave_up = until_taskbar
        .rfind("render_gave_up")
        .unwrap_or_else(|| panic!("the retries never ran out: {log}"));
    let since_last_state = until_taskbar[..gave_up]
        .rsplit_once("\nview ")
        .map(|(_, rest)| rest)
        .unwrap_or_else(|| panic!("no state change before giving up: {log}"));
    assert_eq!(
        since_last_state.matches("render_retry").count(),
        6,
        "six retries, not seven: {log}"
    );
    assert_eq!(
        until_taskbar[gave_up..].matches("render_retry").count(),
        0,
        "nothing retries after giving up: {log}"
    );
    assert!(
        after_loss.contains("probe icon_rect_now=true"),
        "the icon is back in the notification area: {log}"
    );
    // A clean quit: the same process ended by Quit, no server left, and the operations thread had
    // finished too: a write accepted right before Quit ran to its end (S2-08).
    assert!(
        log.contains("quit_begin")
            && log.contains("quit_done supervisor=stopped controller=completed")
            && log.contains("exit=done")
            && !log.contains("quit_incomplete"),
        "{log}"
    );
    assert!(
        log.contains("selftest quit_with_write_in_flight expect_server_autostart=Some(true)"),
        "{log}"
    );
    for pid in server_pids(&run) {
        wait_until("the stand-in server to be gone", || !alive(pid));
    }

    // The toggles went to the file, and the registry is the isolated copy: the first toggle turned
    // auto-start off, the second on again, so the entry exists, for this executable only.
    let saved = settings::load(&run.app).unwrap();
    assert!(saved.autostart, "toggled twice");
    assert!(
        saved.server_autostart,
        "toggled off, then on by the write that was in flight when Quit came"
    );
    let exe = Path::new(TRAY).canonicalize().unwrap();
    assert_eq!(
        autostart::status(&registry.registry, &exe),
        Autostart::Registered(Approval::Enabled)
    );
    assert_eq!(
        registry.registry.other("SomeOtherApp").unwrap().as_deref(),
        Some("\"C:\\Other\\other.exe\" --flag"),
        "another application's Run entry is not touched"
    );
    // And the user's real Run key was not written at all.
    assert_eq!(real_run_value(), real_before, "the real Run key changed");
}

#[test]
#[cfg_attr(
    feature = "no-desktop",
    ignore = "needs a desktop with a notification area"
)]
fn a_test_run_never_registers_the_logon_entry_by_itself() {
    // Default autostart is "on", and there is no settings file: still nothing is registered, because
    // this is not a normal product start (isolated registry, stand-in server, isolated app dir).
    let real_before = real_run_value();
    let registry = Isolated::new("noregister");
    let mut run = tray("serve", &registry, &["--run-seconds", "3"], None);
    require_icon(&run);
    assert_eq!(run.child.0.wait().unwrap().code(), Some(0), "{}", run.log());
    assert_eq!(
        registry.registry.run_value().unwrap(),
        None,
        "nothing registered in the isolated key"
    );
    assert!(
        !settings::path(&run.app).exists(),
        "and no settings file was written"
    );
    assert_eq!(real_run_value(), real_before);
}

#[test]
#[cfg_attr(
    feature = "no-desktop",
    ignore = "needs a desktop with a notification area"
)]
fn the_server_does_not_start_when_server_autostart_is_off() {
    let registry = Isolated::new("serverauto");
    // Off in the file: the tray runs, the server does not start.
    let off = Temp::new("off");
    let app = off.0.join("app");
    fs::create_dir_all(&app).unwrap();
    settings::save(
        &app,
        &settings::Settings {
            server_autostart: false,
            ..settings::Settings::default()
        },
    )
    .unwrap();
    let mut run = tray("serve", &registry, &["--run-seconds", "3"], Some(&app));
    require_icon(&run);
    assert_eq!(run.child.0.wait().unwrap().code(), Some(0), "{}", run.log());
    assert!(
        server_pids(&run).is_empty(),
        "no server was started: {}",
        run.log()
    );
    assert!(!run.log().contains("state=Starting"), "{}", run.log());
}

#[test]
#[cfg_attr(
    feature = "no-desktop",
    ignore = "needs a desktop with a notification area"
)]
fn an_invalid_settings_file_shows_an_error_and_starts_nothing_and_is_not_overwritten() {
    let registry = Isolated::new("invalid");
    let dir = Temp::new("invalidapp");
    let app = dir.0.join("app");
    fs::create_dir_all(&app).unwrap();
    fs::write(settings::path(&app), "{\"version\":2}").unwrap();
    let mut run = tray("serve", &registry, &["--run-seconds", "3"], Some(&app));
    require_icon(&run);
    assert_eq!(run.child.0.wait().unwrap().code(), Some(0), "{}", run.log());
    let log = run.log();
    assert!(log.contains("settings_unknown_version"), "{log}");
    assert!(
        log.contains("icon=Error"),
        "the icon shows the error: {log}"
    );
    assert!(server_pids(&run).is_empty());
    assert_eq!(
        fs::read_to_string(settings::path(&app)).unwrap(),
        "{\"version\":2}"
    );
}

#[test]
#[cfg_attr(
    feature = "no-desktop",
    ignore = "needs a desktop with a notification area"
)]
fn a_taken_port_and_a_refusing_vault_are_shown_as_errors_and_not_retried() {
    for (mode, expected) in [
        ("port-in-use", "PortInUse"),
        ("refuse-vault", "VaultRefused"),
    ] {
        let registry = Isolated::new(mode);
        let mut run = tray(mode, &registry, &["--run-seconds", "4"], None);
        require_icon(&run);
        assert_eq!(run.child.0.wait().unwrap().code(), Some(0), "{}", run.log());
        let log = run.log();
        assert!(log.contains(&format!("state=Error({expected}")), "{log}");
        assert!(log.contains("icon=Error"), "{log}");
        assert_eq!(server_pids(&run).len(), 1, "not retried: {log}");
        // After the error the retry is offered (Start is enabled), a stop is not.
        assert!(
            log.lines().any(|l| l.starts_with("view icon=Error")
                && l.contains("start=true stop=false restart=true")),
            "{log}"
        );
    }
}

#[test]
#[cfg_attr(
    feature = "no-desktop",
    ignore = "needs a desktop with a notification area"
)]
fn killing_the_tray_leaves_no_server_behind_in_ui_mode_too() {
    let registry = Isolated::new("kill");
    let mut run = tray("hang", &registry, &[], None);
    require_icon(&run);
    wait_until("the server to run", || run.log().contains("state=Running"));
    let server = server_pids(&run)[0];
    assert!(alive(server));
    run.child.0.kill().unwrap();
    let _ = run.child.0.wait();
    wait_until("the job object to end the server", || !alive(server));
}

#[test]
#[cfg_attr(
    feature = "no-desktop",
    ignore = "needs a desktop with a notification area"
)]
fn a_second_tray_for_the_same_application_directory_is_refused() {
    let registry = Isolated::new("twice");
    let first = tray("serve", &registry, &["--run-seconds", "6"], None);
    require_icon(&first);
    let mut second = tray(
        "serve",
        &registry,
        &["--run-seconds", "1"],
        Some(&first.app),
    );
    assert_eq!(
        second.child.0.wait().unwrap().code(),
        Some(3),
        "{}",
        second.log()
    );
    assert!(second.log().contains("exit=already_running"));
    assert!(server_pids(&second).is_empty());
    let mut first = first;
    assert_eq!(first.child.0.wait().unwrap().code(), Some(0));
}

// ---- The auto-start registration on a real (isolated) registry ----

fn exe() -> PathBuf {
    PathBuf::from(r"C:\Users\ユーザー\My Apps\Memocampus\memocampus-tray.exe")
}

#[test]
fn registering_and_unregistering_touches_only_the_tray_value() {
    let isolated = Isolated::new("registry");
    let registry = &isolated.registry;
    registry.put_other("Alpha", "\"C:\\a\\a.exe\"").unwrap();
    registry.put_other("Beta", "\"C:\\b b\\b.exe\" /x").unwrap();
    assert_eq!(
        autostart::status(registry, &exe()),
        Autostart::NotRegistered
    );

    // Off while it is off: nothing happens, no error.
    assert_eq!(
        autostart::disable(registry, &exe()).unwrap(),
        Autostart::NotRegistered
    );
    // On, twice: one value, the quoted path (spaces and non-ASCII letters intact).
    assert_eq!(
        autostart::enable(registry, &exe()).unwrap(),
        Autostart::Registered(Approval::Enabled)
    );
    assert_eq!(
        autostart::enable(registry, &exe()).unwrap(),
        Autostart::Registered(Approval::Enabled)
    );
    assert_eq!(
        registry.run_value().unwrap().as_deref(),
        Some("\"C:\\Users\\ユーザー\\My Apps\\Memocampus\\memocampus-tray.exe\"")
    );
    // The other applications' entries are exactly as they were.
    assert_eq!(
        registry.other("Alpha").unwrap().as_deref(),
        Some("\"C:\\a\\a.exe\"")
    );
    assert_eq!(
        registry.other("Beta").unwrap().as_deref(),
        Some("\"C:\\b b\\b.exe\" /x")
    );
    assert_eq!(
        autostart::disable(registry, &exe()).unwrap(),
        Autostart::NotRegistered
    );
    assert_eq!(registry.run_value().unwrap(), None);
    assert_eq!(
        registry.other("Alpha").unwrap().as_deref(),
        Some("\"C:\\a\\a.exe\"")
    );
    assert_eq!(
        registry.other("Beta").unwrap().as_deref(),
        Some("\"C:\\b b\\b.exe\" /x")
    );
}

#[test]
fn a_moved_executable_and_a_windows_side_choice_are_detected_and_respected() {
    let isolated = Isolated::new("moved");
    let registry = &isolated.registry;
    autostart::enable(
        registry,
        &PathBuf::from(r"D:\old place\memocampus-tray.exe"),
    )
    .unwrap();
    assert_eq!(
        autostart::status(registry, &exe()),
        Autostart::PathMismatch(Approval::Enabled)
    );
    // Looking did not rewrite it.
    assert_eq!(
        registry.run_value().unwrap().as_deref(),
        Some("\"D:\\old place\\memocampus-tray.exe\"")
    );

    // The user turned it off in Windows' startup management: a record in StartupApproved.
    let disabled = [3u8, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8];
    registry.put_approved(&disabled).unwrap();
    assert_eq!(
        autostart::status(registry, &exe()),
        Autostart::PathMismatch(Approval::DisabledByWindows)
    );
    // Enabling again (the user asked) registers the path, and still does not override Windows' record.
    assert_eq!(
        autostart::enable(registry, &exe()).unwrap(),
        Autostart::Registered(Approval::DisabledByWindows)
    );
    assert_eq!(
        registry.approved_bytes().unwrap().as_deref(),
        Some(&disabled[..])
    );
    // An enabled record reads as enabled; one in a shape not known reads as unknown, and is left alone.
    registry
        .put_approved(&[2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
        .unwrap();
    assert_eq!(
        autostart::status(registry, &exe()),
        Autostart::Registered(Approval::Enabled)
    );
    registry.put_approved(&[9, 9]).unwrap();
    assert_eq!(
        autostart::status(registry, &exe()),
        Autostart::Registered(Approval::Unknown)
    );
    assert_eq!(
        registry.approved_bytes().unwrap().as_deref(),
        Some(&[9u8, 9][..])
    );
}

#[test]
fn the_isolated_registry_cannot_be_pointed_at_the_real_keys_and_cleans_up_after_itself() {
    for bad in ["", "a b", "..\\x", "a\\b", &"x".repeat(100)] {
        assert!(WinRegistry::isolated(bad).is_err(), "{bad:?}");
    }
    assert!(
        WinRegistry::user().remove_isolated().is_err(),
        "never removes the real key"
    );
    let id = unique("cleanup");
    let registry = WinRegistry::isolated(&id).unwrap();
    autostart::enable(&registry, &exe()).unwrap();
    assert!(registry.run_value().unwrap().is_some());
    registry.remove_isolated().unwrap();
    assert_eq!(
        registry.run_value().unwrap(),
        None,
        "the private keys are gone"
    );
    assert_eq!(registry.approved_bytes().unwrap(), None);
    assert_eq!(VALUE_NAME, "MemocampusTray");
}

// ---- S2-01: no verification run can write the user's real Run key ----

#[test]
fn a_selftest_without_an_isolated_registry_is_refused_before_any_registry_access() {
    let real_before = real_run_value();
    let dir = Temp::new("refused");
    let log = dir.0.join("tray.log");
    let status = Command::new(TRAY)
        .arg("--selftest")
        .arg("--log")
        .arg(&log)
        .env("MEMOCAMPUS_APP_DIR", dir.0.join("app"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(2));
    let text = fs::read_to_string(&log).unwrap_or_default();
    assert!(
        text.contains("exit=selftest_needs_isolated_registry"),
        "{text}"
    );
    assert!(!text.contains("tray_icon=created"), "it never got going");
    assert!(
        !dir.0.join("app").exists(),
        "no application directory, no settings: it stopped first"
    );
    assert_eq!(real_run_value(), real_before);
}

#[test]
fn the_real_run_key_cannot_be_written_through_any_handle_a_verification_run_gets() {
    use memocampus_tray::mode::{decide, Flags, RunMode};
    let real_before = real_run_value();
    // A verification option without an isolated registry: the real key, read-only.
    let flags = Flags {
        run_seconds: true,
        ..Flags::default()
    };
    let mode = decide(&flags).unwrap();
    assert_eq!(mode, RunMode::ReadOnly);
    let registry = WinRegistry::for_mode(&mode).unwrap();
    assert!(!registry.is_writable());
    // Every write path refuses, in the registry layer, before it touches anything.
    let denied = |result: std::io::Result<()>| {
        assert_eq!(
            result.unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        )
    };
    denied(registry.set_run_value("\"C:\\x\\x.exe\""));
    denied(registry.delete_run_value());
    denied(autostart::enable(&registry, &exe()).map(|_| ()));
    denied(autostart::disable(&registry, &exe()).map(|_| ()));
    // Reading is allowed.
    assert_eq!(registry.run_value().unwrap(), real_before);
    // The plain accessor used by these tests to look at the real key is read-only too.
    assert!(!WinRegistry::user().is_writable());
    // Only the product mode gets a writable real handle (not written to here), and an isolated id a
    // private writable one.
    assert!(WinRegistry::for_mode(&RunMode::Product)
        .unwrap()
        .is_writable());
    let isolated = Isolated::new("mode");
    let private = WinRegistry::for_mode(&RunMode::Isolated(isolated.id.clone())).unwrap();
    assert!(private.is_writable() && private.is_isolated());
    assert_eq!(real_run_value(), real_before, "the real key did not change");
}

#[test]
#[cfg_attr(
    feature = "no-desktop",
    ignore = "needs a desktop with a notification area"
)]
fn a_verification_run_without_an_isolated_registry_registers_nothing() {
    // `--run-seconds` and a stand-in server, no `--registry-root`, an isolated application
    // directory with no settings file: the first-run registration must not happen.
    let real_before = real_run_value();
    let dir = Temp::new("readonly");
    let app = dir.0.join("app");
    fs::create_dir_all(&app).unwrap();
    let log = dir.0.join("tray.log");
    let mut child = Reaped(
        Command::new(TRAY)
            .arg("--log")
            .arg(&log)
            .args(["--run-seconds", "3", "--mcp-exe"])
            .arg(stub())
            .env("MEMOCAMPUS_APP_DIR", &app)
            .env("LOCALAPPDATA", dir.0.join("localappdata"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let code = child.0.wait().unwrap().code();
    let text = fs::read_to_string(&log).unwrap_or_default();
    assert_eq!(code, Some(0), "{text}");
    assert_eq!(real_run_value(), real_before, "the real Run key changed");
    assert!(!settings::path(&app).exists(), "no first-run settings file");
}

// ---- S2-07: only a known StartupApproved record is interpreted ----

#[test]
fn a_startup_record_of_an_unknown_type_or_shape_is_unknown_and_left_alone() {
    let isolated = Isolated::new("approved");
    let registry = &isolated.registry;
    autostart::enable(registry, &exe()).unwrap();
    // Right type and length, an enabled first byte: enabled. A disabled one: disabled.
    registry
        .put_approved(&[6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
        .unwrap();
    assert_eq!(
        autostart::status(registry, &exe()),
        Autostart::Registered(Approval::Enabled)
    );
    registry
        .put_approved(&[3, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8])
        .unwrap();
    assert_eq!(
        autostart::status(registry, &exe()),
        Autostart::Registered(Approval::DisabledByWindows)
    );
    // A first byte nobody documented, a longer record, a flag in a reserved byte: unknown.
    for record in [
        &[1u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0][..],
        &[3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0][..],
        &[3, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0][..],
    ] {
        registry.put_approved(record).unwrap();
        assert_eq!(
            autostart::status(registry, &exe()),
            Autostart::Registered(Approval::Unknown),
            "{record:?}"
        );
        assert_eq!(
            registry.approved_bytes().unwrap().as_deref(),
            Some(record),
            "left as Windows wrote it"
        );
    }
    // The right bytes in the wrong registry type (a string): not a record this build knows.
    registry.put_approved_text("\u{3}").unwrap();
    assert_eq!(
        autostart::status(registry, &exe()),
        Autostart::Registered(Approval::Unknown)
    );
}

// ---- S2-06 / S2-08: closing the window is the orderly quit ----

struct Search {
    pid: u32,
    found: Option<HWND>,
}

unsafe extern "system" fn visit(window: HWND, lparam: LPARAM) -> i32 {
    // SAFETY: `lparam` is the `Search` the caller passed and keeps alive for the whole call.
    let search = unsafe { &mut *(lparam as *mut Search) };
    let mut owner = 0u32;
    // SAFETY: plain queries about a window handle.
    unsafe { GetWindowThreadProcessId(window, &mut owner) };
    if owner != search.pid {
        return 1;
    }
    let mut name = [0u16; 64];
    // SAFETY: the buffer is 64 wide characters, as passed.
    let length = unsafe { GetClassNameW(window, name.as_mut_ptr(), 64) } as usize;
    if String::from_utf16_lossy(&name[..length]) == "MemocampusTrayHost" {
        search.found = Some(window);
        return 0;
    }
    1
}

fn tray_window(pid: u32) -> HWND {
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        let mut search = Search { pid, found: None };
        // SAFETY: the callback only reads and writes the `Search` it is given.
        unsafe { EnumWindows(Some(visit), &mut search as *mut Search as LPARAM) };
        if let Some(window) = search.found {
            return window;
        }
        assert!(Instant::now() < until, "the tray's window was not found");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
#[cfg_attr(
    feature = "no-desktop",
    ignore = "needs a desktop with a notification area"
)]
fn wm_close_is_the_orderly_quit_and_asking_twice_is_harmless() {
    let registry = Isolated::new("wmclose");
    let mut run = tray("serve", &registry, &[], None);
    require_icon(&run);
    wait_until("the server to run", || run.log().contains("state=Running"));
    let pids = server_pids(&run);
    assert!(!pids.is_empty());
    let window = tray_window(run.child.0.id());
    // Two close requests back to back: the second arrives while the first quit is under way.
    // SAFETY: messages posted to the tray's own window, in a process this test started.
    unsafe {
        PostMessageW(window, WM_CLOSE, 0, 0);
        PostMessageW(window, WM_CLOSE, 0, 0);
    }
    let until = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = run.child.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < until,
            "the tray did not exit after WM_CLOSE: {}",
            run.log()
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    let log = run.log();
    assert_eq!(status.code(), Some(0), "{log}");
    assert_eq!(log.matches("quit_begin").count(), 1, "one quit: {log}");
    assert_eq!(
        log.matches("quit_done supervisor=stopped controller=completed")
            .count(),
        1,
        "{log}"
    );
    assert!(
        log.contains("exit=done") && !log.contains("quit_incomplete"),
        "{log}"
    );
    // The window was not destroyed before the quit (the server was stopped through the quit).
    assert!(
        log.find("quit_begin").unwrap() < log.find("quit_done").unwrap(),
        "{log}"
    );
    for pid in pids {
        wait_until("the stand-in server to be gone", || !alive(pid));
    }
}
