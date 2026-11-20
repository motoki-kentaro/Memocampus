//! The process supervisor: owns the `memocampus-mcp` child and its state, on a thread of its own so
//! that a UI (Slice 2) never waits for a process. The UI sends `Command`s and reads `Snapshot`s.
//!
//! The Windows specifics (creating the process, the job object, the management pipe) are behind
//! `Launcher` and `ManagedChild`, so the state machine is tested here with scripted children and
//! against a real process in `tests/`.
//!
//! Rules this file enforces:
//! - `Running` only after the server said it is listening. Never before, never on a guess.
//! - A refusal to start (vault not published, port taken, bad arguments) is not retried and never
//!   falls back to another port or another vault.
//! - Only a crash is retried, within `RestartPolicy`'s bound; then `Error(CrashLoop)`.
//! - A stop asks over the management channel first, and only the child this supervisor started is
//!   ever terminated, through the handle it holds. Nothing here looks a process up by name or id.

use crate::classify::{classify_line, Diag, EXIT_REFUSED};
use crate::settings::{Settings, SettingsError};
use crate::state::{
    error_for_refusal, ErrorKind, LastExit, RestartPolicy, ServerState, Snapshot, VaultStatus,
};
use std::ffi::OsString;
use std::io;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// What to start: the arguments from the settings. The executable, its environment and the
/// management channel belong to the launcher.
pub struct LaunchSpec {
    pub args: Vec<OsString>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchError {
    ExeMissing,
    Failed,
}

/// A started server process, as the supervisor sees it.
pub trait ManagedChild: Send {
    fn pid(&self) -> u32;
    /// The exit code once it has ended, `None` while it runs.
    fn try_exit(&mut self) -> io::Result<Option<i32>>;
    /// Asks it to stop over the management channel. Errors when the channel is gone.
    fn request_stop(&mut self) -> io::Result<()>;
    /// Ends this child now. Only ever this child: it is the handle the supervisor created.
    fn terminate(&mut self) -> io::Result<()>;
}

pub struct Launched {
    pub child: Box<dyn ManagedChild>,
    /// The child's stderr, line by line, until it ends.
    pub lines: Receiver<String>,
}

pub trait Launcher: Send {
    fn launch(&mut self, spec: &LaunchSpec) -> Result<Launched, LaunchError>;
}

pub type SettingsSource = Box<dyn FnMut() -> Result<Settings, SettingsError> + Send>;
pub type Observer = Box<dyn FnMut(&Snapshot) + Send>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Start,
    Stop,
    /// Stop if running, re-read the settings, start again.
    Restart,
    /// Stop the server (if any) and end the supervisor.
    Quit,
}

#[derive(Debug, Clone, Copy)]
pub struct Timings {
    /// From the launch to the "listening" line.
    pub start_timeout: Duration,
    /// From the stop request to the exit, before the child is terminated.
    pub stop_timeout: Duration,
    /// How long a terminated child is waited for.
    pub kill_wait: Duration,
    pub poll: Duration,
}

impl Timings {
    pub fn standard() -> Self {
        Self {
            start_timeout: Duration::from_secs(10),
            // The server gives open connections 5 s at shutdown; this leaves it a margin.
            stop_timeout: Duration::from_secs(8),
            kill_wait: Duration::from_secs(3),
            poll: Duration::from_millis(100),
        }
    }
}

/// What the server says when it refuses to start: kept, so the exit can be explained.
fn is_refusal(diag: &Diag) -> bool {
    matches!(
        diag,
        Diag::PortInUse | Diag::ListenFailed(_) | Diag::VaultRefused(_) | Diag::CannotStart(_)
    )
}

struct Live {
    child: Box<dyn ManagedChild>,
    lines: Receiver<String>,
    started: Instant,
    /// The last refusal-type thing the server said.
    said: Option<Diag>,
}

struct Core {
    launcher: Box<dyn Launcher>,
    settings: SettingsSource,
    timings: Timings,
    policy: RestartPolicy,
    observer: Observer,
    shared: Arc<Mutex<Snapshot>>,
    snap: Snapshot,
    live: Option<Live>,
    restart_at: Option<Instant>,
    commands: Receiver<Command>,
}

/// The handle a UI holds. Dropping it stops the server and the thread.
pub struct Supervisor {
    commands: Sender<Command>,
    shared: Arc<Mutex<Snapshot>>,
    thread: Option<JoinHandle<()>>,
}

impl Supervisor {
    pub fn spawn(
        launcher: Box<dyn Launcher>,
        settings: SettingsSource,
        timings: Timings,
        policy: RestartPolicy,
        observer: Observer,
    ) -> Self {
        let (commands, receive) = mpsc::channel();
        let shared = Arc::new(Mutex::new(Snapshot::initial()));
        let core = Core {
            launcher,
            settings,
            timings,
            policy,
            observer,
            shared: Arc::clone(&shared),
            snap: Snapshot::initial(),
            live: None,
            restart_at: None,
            commands: receive,
        };
        let thread = thread::Builder::new()
            .name("supervisor".into())
            .spawn(move || core.run())
            .expect("the supervisor thread starts");
        Self {
            commands,
            shared,
            thread: Some(thread),
        }
    }

    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    pub fn snapshot(&self) -> Snapshot {
        self.shared.lock().expect("snapshot lock").clone()
    }

    /// Stops the server and waits for the supervisor thread to end.
    pub fn shutdown(mut self) {
        self.finish();
    }

    fn finish(&mut self) {
        let _ = self.commands.send(Command::Quit);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.finish();
    }
}

impl Core {
    fn run(mut self) {
        loop {
            match self.commands.recv_timeout(self.timings.poll) {
                Ok(Command::Quit) | Err(RecvTimeoutError::Disconnected) => {
                    self.stop_live();
                    return;
                }
                Ok(command) => self.handle(command),
                Err(RecvTimeoutError::Timeout) => {}
            }
            self.tick();
        }
    }

    fn publish(&mut self) {
        self.snap.seq += 1;
        *self.shared.lock().expect("snapshot lock") = self.snap.clone();
        (self.observer)(&self.snap);
    }

    fn set_state(&mut self, state: ServerState) {
        self.snap.state = state;
        self.publish();
    }

    fn handle(&mut self, command: Command) {
        match command {
            Command::Start => {
                if self.live.is_none() {
                    self.policy.reset();
                    self.restart_at = None;
                    self.snap.restarts = 0;
                    self.start();
                }
            }
            Command::Stop => {
                self.restart_at = None;
                if self.live.is_some() {
                    self.stop_live();
                } else if matches!(self.snap.state, ServerState::Error(_)) {
                    // The user acknowledged the error.
                    self.set_state(ServerState::Stopped);
                }
            }
            Command::Restart => {
                self.restart_at = None;
                self.stop_live();
                self.policy.reset();
                self.snap.restarts = 0;
                self.start();
            }
            Command::Quit => {}
        }
    }

    fn fail(&mut self, kind: ErrorKind) {
        self.snap.port = None;
        self.snap.pid = None;
        self.set_state(ServerState::Error(kind));
    }

    fn start(&mut self) {
        self.snap.vault = VaultStatus::Unknown;
        self.snap.port = None;
        self.snap.pid = None;
        self.set_state(ServerState::Starting);
        let settings = match (self.settings)() {
            Ok(settings) => settings,
            Err(error) => return self.fail(ErrorKind::SettingsInvalid(error.code())),
        };
        let spec = LaunchSpec {
            args: settings.server_args(),
        };
        match self.launcher.launch(&spec) {
            Ok(Launched { child, lines }) => {
                self.snap.pid = Some(child.pid());
                self.live = Some(Live {
                    child,
                    lines,
                    started: Instant::now(),
                    said: None,
                });
                self.publish();
            }
            Err(LaunchError::ExeMissing) => self.fail(ErrorKind::McpMissing),
            Err(LaunchError::Failed) => self.fail(ErrorKind::LaunchFailed),
        }
    }

    fn tick(&mut self) {
        let Some(live) = self.live.as_mut() else {
            if self.restart_at.is_some_and(|at| Instant::now() >= at) {
                self.restart_at = None;
                self.start();
            }
            return;
        };
        let exit = live.child.try_exit();
        let started = live.started;
        self.drain_lines();
        match exit {
            Ok(Some(code)) => self.on_exit(Some(code)),
            // Not knowing whether it runs is not "running": treat it as ended.
            Err(_) => {
                self.terminate_live();
                self.on_exit(None);
            }
            Ok(None) => {
                if self.snap.state == ServerState::Starting
                    && started.elapsed() >= self.timings.start_timeout
                {
                    self.stop_live();
                    self.fail(ErrorKind::StartTimeout);
                }
            }
        }
    }

    fn drain_lines(&mut self) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let mut reached_listening = None;
        while let Ok(line) = live.lines.try_recv() {
            match classify_line(&line) {
                Diag::Listening { port } if self.snap.state == ServerState::Starting => {
                    reached_listening = Some(port);
                }
                diag if is_refusal(&diag) => live.said = Some(diag),
                _ => {}
            }
        }
        if let Some(port) = reached_listening {
            self.snap.port = Some(port);
            // Up. What the vault allows is not known from this, and is not claimed.
            self.set_state(ServerState::Running);
        }
    }

    /// The child ended without being asked (a stop goes through `stop_live`).
    fn on_exit(&mut self, code: Option<i32>) {
        // What it printed last is still in the pipe: read it before deciding why it ended.
        if let Some(live) = self.live.as_mut() {
            let deadline = Instant::now() + Duration::from_millis(300);
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                match live.lines.recv_timeout(left) {
                    Ok(line) => {
                        let diag = classify_line(&line);
                        if is_refusal(&diag) {
                            live.said = Some(diag);
                        }
                    }
                    Err(_) => break,
                }
            }
        }
        let said = self.live.take().and_then(|live| live.said);
        self.snap.pid = None;
        self.snap.port = None;
        self.snap.last_exit = code.map(LastExit::Code);
        let was_starting = self.snap.state == ServerState::Starting;
        if was_starting && code == Some(EXIT_REFUSED) {
            let error = error_for_refusal(said.as_ref());
            if let ErrorKind::VaultRefused(why) = &error {
                self.snap.vault = VaultStatus::Refused(why.clone());
            }
            return self.fail(error);
        }
        // It ended by itself after it said it was up, or without a recognised refusal: a crash.
        match self.policy.on_crash(Instant::now()) {
            Some(wait) => {
                self.restart_at = Some(Instant::now() + wait);
                self.snap.restarts += 1;
                self.fail(ErrorKind::Crashed {
                    exit: code,
                    will_restart: true,
                });
            }
            None => self.fail(ErrorKind::CrashLoop),
        }
    }

    fn terminate_live(&mut self) {
        if let Some(live) = self.live.as_mut() {
            let _ = live.child.terminate();
        }
    }

    /// Stops the child: over the management channel first, and only if it has not ended within
    /// `stop_timeout` by terminating this one process. Ends in `Stopped`.
    fn stop_live(&mut self) {
        let Some(mut live) = self.live.take() else {
            return;
        };
        self.set_state(ServerState::Stopping);
        // The channel may already be gone (the child ended): then the exit shows below.
        let _ = live.child.request_stop();
        let deadline = Instant::now() + self.timings.stop_timeout;
        let mut forced = false;
        let code = loop {
            while live.lines.try_recv().is_ok() {}
            if let Ok(Some(code)) = live.child.try_exit() {
                break Some(code);
            }
            if Instant::now() >= deadline {
                forced = true;
                let _ = live.child.terminate();
                let until = Instant::now() + self.timings.kill_wait;
                break loop {
                    match live.child.try_exit() {
                        Ok(Some(code)) => break Some(code),
                        _ if Instant::now() >= until => break None,
                        _ => thread::sleep(Duration::from_millis(10)),
                    }
                };
            }
            thread::sleep(Duration::from_millis(10));
        };
        self.snap.pid = None;
        self.snap.port = None;
        self.snap.last_exit = Some(match (forced, code) {
            (true, _) => LastExit::Forced,
            (false, Some(0)) => LastExit::Graceful,
            (false, Some(code)) => LastExit::Code(code),
            (false, None) => LastExit::Forced,
        });
        self.set_state(ServerState::Stopped);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[derive(Clone)]
    struct Script {
        /// stderr lines, each after its delay from the launch.
        lines: Vec<(u64, &'static str)>,
        /// Ends by itself with this code after this many ms.
        exit: Option<(u64, i32)>,
        honors_stop: bool,
        /// When set, no line is sent until the test sets it to true: the child exists and says
        /// nothing for exactly as long as the test wants, with no timing involved.
        hold: Option<Arc<AtomicBool>>,
    }

    impl Script {
        fn held_until(self, release: &Arc<AtomicBool>) -> Self {
            Self {
                hold: Some(Arc::clone(release)),
                ..self
            }
        }
        fn listening(port: u16) -> Self {
            Self {
                lines: vec![(
                    20,
                    Box::leak(
                        format!("memocampus-mcp: listening on http://127.0.0.1:{port}/mcp")
                            .into_boxed_str(),
                    ),
                )],
                exit: None,
                honors_stop: true,
                hold: None,
            }
        }
        fn refused(line: &'static str) -> Self {
            Self {
                lines: vec![(5, line)],
                exit: Some((15, 2)),
                honors_stop: true,
                hold: None,
            }
        }
        fn crashing() -> Self {
            Self {
                exit: Some((80, 101)),
                ..Self::listening(8765)
            }
        }
    }

    #[derive(Default)]
    struct Flags {
        launches: AtomicUsize,
        stop_requests: AtomicUsize,
        terminated: AtomicBool,
    }

    struct FakeChild {
        born: Instant,
        exit: Option<(u64, i32)>,
        honors_stop: bool,
        stopped: bool,
        flags: Arc<Flags>,
    }

    impl ManagedChild for FakeChild {
        fn pid(&self) -> u32 {
            4242
        }
        fn try_exit(&mut self) -> io::Result<Option<i32>> {
            if self.flags.terminated.load(Ordering::SeqCst) {
                return Ok(Some(1));
            }
            if self.stopped && self.honors_stop {
                return Ok(Some(0));
            }
            if let Some((after, code)) = self.exit {
                if self.born.elapsed() >= Duration::from_millis(after) {
                    return Ok(Some(code));
                }
            }
            Ok(None)
        }
        fn request_stop(&mut self) -> io::Result<()> {
            self.flags.stop_requests.fetch_add(1, Ordering::SeqCst);
            self.stopped = true;
            Ok(())
        }
        fn terminate(&mut self) -> io::Result<()> {
            self.flags.terminated.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    struct FakeLauncher {
        scripts: VecDeque<Result<Script, LaunchError>>,
        flags: Arc<Flags>,
        args: Arc<Mutex<Vec<Vec<String>>>>,
    }

    impl Launcher for FakeLauncher {
        fn launch(&mut self, spec: &LaunchSpec) -> Result<Launched, LaunchError> {
            self.flags.launches.fetch_add(1, Ordering::SeqCst);
            self.flags.terminated.store(false, Ordering::SeqCst);
            self.args.lock().unwrap().push(
                spec.args
                    .iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect(),
            );
            let script = self
                .scripts
                .pop_front()
                .expect("a script for this launch")?;
            let (send, lines) = mpsc::channel();
            let to_send = script.lines.clone();
            let hold = script.hold.clone();
            thread::spawn(move || {
                if let Some(hold) = hold {
                    while !hold.load(Ordering::SeqCst) {
                        thread::sleep(Duration::from_millis(1));
                    }
                }
                let mut last = 0;
                for (at, line) in to_send {
                    thread::sleep(Duration::from_millis(at.saturating_sub(last)));
                    last = at;
                    let _ = send.send(line.to_owned());
                }
            });
            Ok(Launched {
                child: Box::new(FakeChild {
                    born: Instant::now(),
                    exit: script.exit,
                    honors_stop: script.honors_stop,
                    stopped: false,
                    flags: Arc::clone(&self.flags),
                }),
                lines,
            })
        }
    }

    struct Rig {
        supervisor: Supervisor,
        flags: Arc<Flags>,
        history: Arc<Mutex<Vec<ServerState>>>,
        args: Arc<Mutex<Vec<Vec<String>>>>,
    }

    fn fast() -> Timings {
        Timings {
            start_timeout: Duration::from_millis(300),
            stop_timeout: Duration::from_millis(200),
            kill_wait: Duration::from_millis(100),
            poll: Duration::from_millis(5),
        }
    }

    fn rig_with(
        scripts: Vec<Result<Script, LaunchError>>,
        settings: SettingsSource,
        policy: RestartPolicy,
    ) -> Rig {
        rig_timed(scripts, settings, policy, fast())
    }

    fn rig_timed(
        scripts: Vec<Result<Script, LaunchError>>,
        settings: SettingsSource,
        policy: RestartPolicy,
        timings: Timings,
    ) -> Rig {
        let flags = Arc::new(Flags::default());
        let args = Arc::new(Mutex::new(Vec::new()));
        let history = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&history);
        let supervisor = Supervisor::spawn(
            Box::new(FakeLauncher {
                scripts: scripts.into(),
                flags: Arc::clone(&flags),
                args: Arc::clone(&args),
            }),
            settings,
            timings,
            policy,
            Box::new(move |snapshot| {
                let mut history = recorded.lock().unwrap();
                if history.last() != Some(&snapshot.state) {
                    history.push(snapshot.state.clone());
                }
            }),
        );
        Rig {
            supervisor,
            flags,
            history,
            args,
        }
    }

    fn rig(scripts: Vec<Result<Script, LaunchError>>) -> Rig {
        rig_with(
            scripts,
            Box::new(|| Ok(Settings::default())),
            RestartPolicy::new(3, Duration::from_secs(60), vec![Duration::from_millis(10)]),
        )
    }

    fn wait_for(rig: &Rig, what: &str, condition: impl Fn(&Snapshot) -> bool) -> Snapshot {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            let snapshot = rig.supervisor.snapshot();
            if condition(&snapshot) {
                return snapshot;
            }
            assert!(
                Instant::now() < until,
                "timed out waiting for {what}: {snapshot:?}"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn is_error(snapshot: &Snapshot) -> bool {
        matches!(snapshot.state, ServerState::Error(_))
    }

    #[test]
    fn running_is_reached_only_after_the_server_says_it_is_listening() {
        // The child is held back until the test lets it speak, so "it has said nothing yet" does
        // not depend on how fast this machine is (it used to be a fixed 8 ms sleep).
        let release = Arc::new(AtomicBool::new(false));
        // A start timeout far beyond the test's own pauses: a starved machine must not turn the
        // held silence into a timeout.
        let rig = rig_timed(
            vec![Ok(Script::listening(8765).held_until(&release))],
            Box::new(|| Ok(Settings::default())),
            RestartPolicy::new(3, Duration::from_secs(60), vec![Duration::from_millis(10)]),
            Timings {
                start_timeout: Duration::from_secs(20),
                ..fast()
            },
        );
        rig.supervisor.send(Command::Start);
        // `Starting` is reported before the child's pid is known: wait for both.
        let starting = wait_for(&rig, "a started child", |s| {
            s.state == ServerState::Starting && s.pid.is_some()
        });
        assert_eq!(starting.pid, Some(4242), "the child exists");
        // Silence for a while (longer than the 20 ms the line would take) changes nothing.
        thread::sleep(Duration::from_millis(100));
        assert_eq!(rig.supervisor.snapshot().state, ServerState::Starting);
        release.store(true, Ordering::SeqCst);
        let up = wait_for(&rig, "running", |s| s.state == ServerState::Running);
        assert_eq!(up.port, Some(8765));
        assert_eq!(up.pid, Some(4242));
        // The vault is not claimed ready just because the socket is up.
        assert_eq!(up.vault, VaultStatus::Unknown);
        assert_eq!(
            *rig.history.lock().unwrap(),
            [ServerState::Starting, ServerState::Running]
        );
    }

    #[test]
    fn an_unpublished_vault_is_an_error_with_its_code_and_is_never_retried() {
        let rig = rig(vec![
            Ok(Script::refused(
                "memocampus-mcp: vault refused: vault_publication_pending",
            )),
            Ok(Script::listening(1)),
        ]);
        rig.supervisor.send(Command::Start);
        let error = wait_for(&rig, "error", is_error);
        assert_eq!(
            error.state,
            ServerState::Error(ErrorKind::VaultRefused("vault_publication_pending".into()))
        );
        assert_eq!(
            error.vault,
            VaultStatus::Refused("vault_publication_pending".into())
        );
        thread::sleep(Duration::from_millis(150));
        assert_eq!(
            rig.flags.launches.load(Ordering::SeqCst),
            1,
            "no automatic retry"
        );
        assert!(is_error(&rig.supervisor.snapshot()));
        // The user starts again once the vault is published: that is a new attempt, vault unknown.
        rig.supervisor.send(Command::Start);
        wait_for(&rig, "running", |s| s.state == ServerState::Running);
        assert_eq!(rig.supervisor.snapshot().vault, VaultStatus::Unknown);
    }

    #[test]
    fn a_port_in_use_is_an_error_and_no_other_port_is_tried() {
        let rig = rig(vec![
            Ok(Script::refused(
                "memocampus-mcp: cannot listen: address_in_use",
            )),
            Ok(Script::listening(1)),
        ]);
        rig.supervisor.send(Command::Start);
        let error = wait_for(&rig, "error", is_error);
        assert_eq!(error.state, ServerState::Error(ErrorKind::PortInUse));
        thread::sleep(Duration::from_millis(150));
        assert_eq!(rig.flags.launches.load(Ordering::SeqCst), 1);
        // The arguments never named another port than the setting.
        assert!(rig
            .args
            .lock()
            .unwrap()
            .iter()
            .flatten()
            .any(|a| a == "127.0.0.1:8765"));
        assert!(!rig
            .args
            .lock()
            .unwrap()
            .iter()
            .flatten()
            .any(|a| a == "127.0.0.1:0"));
    }

    #[test]
    fn a_refusal_without_a_reason_is_still_an_error_and_not_a_crash() {
        let mut script = Script::refused("memocampus-mcp: something unclassified");
        script.exit = Some((15, 2));
        let rig = rig(vec![Ok(script)]);
        rig.supervisor.send(Command::Start);
        let error = wait_for(&rig, "error", is_error);
        assert_eq!(
            error.state,
            ServerState::Error(ErrorKind::StartRefused(None))
        );
        assert_eq!(error.restarts, 0);
    }

    #[test]
    fn a_crash_is_retried_a_bounded_number_of_times_and_then_it_gives_up() {
        let rig = rig(vec![
            Ok(Script::crashing()),
            Ok(Script::crashing()),
            Ok(Script::crashing()),
            Ok(Script::crashing()),
            Ok(Script::crashing()),
        ]);
        rig.supervisor.send(Command::Start);
        let end = wait_for(&rig, "crash loop", |s| {
            s.state == ServerState::Error(ErrorKind::CrashLoop)
        });
        assert_eq!(end.restarts, 3);
        thread::sleep(Duration::from_millis(200));
        // First start plus three restarts: not one more.
        assert_eq!(rig.flags.launches.load(Ordering::SeqCst), 4);
        assert_eq!(
            rig.supervisor.snapshot().state,
            ServerState::Error(ErrorKind::CrashLoop)
        );
        // The history shows the crashes that were going to be retried.
        assert!(rig.history.lock().unwrap().iter().any(|s| matches!(
            s,
            ServerState::Error(ErrorKind::Crashed {
                exit: Some(101),
                will_restart: true
            })
        )));
    }

    #[test]
    fn a_user_start_after_a_crash_loop_begins_afresh() {
        let rig = rig_with(
            vec![
                Ok(Script::crashing()),
                Ok(Script::crashing()),
                Ok(Script::listening(8765)),
            ],
            Box::new(|| Ok(Settings::default())),
            RestartPolicy::new(1, Duration::from_secs(60), vec![Duration::from_millis(10)]),
        );
        rig.supervisor.send(Command::Start);
        wait_for(&rig, "crash loop", |s| {
            s.state == ServerState::Error(ErrorKind::CrashLoop)
        });
        rig.supervisor.send(Command::Start);
        let up = wait_for(&rig, "running", |s| s.state == ServerState::Running);
        assert_eq!(up.restarts, 0);
    }

    #[test]
    fn a_stop_asks_over_the_channel_and_does_not_terminate_a_child_that_obeys() {
        let rig = rig(vec![Ok(Script::listening(8765))]);
        rig.supervisor.send(Command::Start);
        wait_for(&rig, "running", |s| s.state == ServerState::Running);
        rig.supervisor.send(Command::Stop);
        let stopped = wait_for(&rig, "stopped", |s| s.state == ServerState::Stopped);
        assert_eq!(stopped.last_exit, Some(LastExit::Graceful));
        assert_eq!(rig.flags.stop_requests.load(Ordering::SeqCst), 1);
        assert!(!rig.flags.terminated.load(Ordering::SeqCst));
        assert_eq!(stopped.pid, None);
        assert_eq!(stopped.port, None);
        assert!(rig.history.lock().unwrap().contains(&ServerState::Stopping));
    }

    #[test]
    fn a_child_that_ignores_the_stop_is_terminated_after_the_timeout() {
        let mut script = Script::listening(8765);
        script.honors_stop = false;
        let rig = rig(vec![Ok(script)]);
        rig.supervisor.send(Command::Start);
        wait_for(&rig, "running", |s| s.state == ServerState::Running);
        let began = Instant::now();
        rig.supervisor.send(Command::Stop);
        let stopped = wait_for(&rig, "stopped", |s| s.state == ServerState::Stopped);
        assert!(
            began.elapsed() >= Duration::from_millis(200),
            "waited for the timeout"
        );
        assert_eq!(stopped.last_exit, Some(LastExit::Forced));
        assert!(rig.flags.terminated.load(Ordering::SeqCst));
    }

    #[test]
    fn a_server_that_never_says_it_is_listening_is_an_error_and_is_ended() {
        let rig = rig(vec![Ok(Script {
            lines: vec![],
            exit: None,
            honors_stop: true,
            hold: None,
        })]);
        rig.supervisor.send(Command::Start);
        let error = wait_for(&rig, "start timeout", is_error);
        assert_eq!(error.state, ServerState::Error(ErrorKind::StartTimeout));
        assert!(!rig.history.lock().unwrap().contains(&ServerState::Running));
        assert_eq!(rig.flags.stop_requests.load(Ordering::SeqCst), 1);
        assert_eq!(error.pid, None);
    }

    #[test]
    fn bad_settings_never_start_a_server_on_guessed_values() {
        let good = Arc::new(AtomicBool::new(false));
        let switch = Arc::clone(&good);
        let rig = rig_with(
            vec![Ok(Script::listening(8765))],
            Box::new(move || {
                if switch.load(Ordering::SeqCst) {
                    Ok(Settings::default())
                } else {
                    Err(SettingsError::Invalid("json"))
                }
            }),
            RestartPolicy::standard(),
        );
        rig.supervisor.send(Command::Start);
        let error = wait_for(&rig, "error", is_error);
        assert_eq!(
            error.state,
            ServerState::Error(ErrorKind::SettingsInvalid("settings_invalid_json".into()))
        );
        assert_eq!(rig.flags.launches.load(Ordering::SeqCst), 0);
        // Fixed, and restarted: the settings are read again.
        good.store(true, Ordering::SeqCst);
        rig.supervisor.send(Command::Restart);
        wait_for(&rig, "running", |s| s.state == ServerState::Running);
        assert_eq!(rig.flags.launches.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_missing_executable_is_an_error_and_is_not_retried() {
        let rig = rig(vec![
            Err(LaunchError::ExeMissing),
            Err(LaunchError::ExeMissing),
        ]);
        rig.supervisor.send(Command::Start);
        let error = wait_for(&rig, "error", is_error);
        assert_eq!(error.state, ServerState::Error(ErrorKind::McpMissing));
        thread::sleep(Duration::from_millis(100));
        assert_eq!(rig.flags.launches.load(Ordering::SeqCst), 1);
        let failed = rig_with(
            vec![Err(LaunchError::Failed)],
            Box::new(|| Ok(Settings::default())),
            RestartPolicy::standard(),
        );
        failed.supervisor.send(Command::Start);
        assert_eq!(
            wait_for(&failed, "error", is_error).state,
            ServerState::Error(ErrorKind::LaunchFailed)
        );
    }

    #[test]
    fn restart_stops_the_running_server_and_starts_a_new_one() {
        let rig = rig(vec![
            Ok(Script::listening(8765)),
            Ok(Script::listening(8765)),
        ]);
        rig.supervisor.send(Command::Start);
        wait_for(&rig, "running", |s| s.state == ServerState::Running);
        rig.supervisor.send(Command::Restart);
        let until = Instant::now() + Duration::from_secs(5);
        while rig.flags.launches.load(Ordering::SeqCst) < 2 {
            assert!(Instant::now() < until);
            thread::sleep(Duration::from_millis(5));
        }
        wait_for(&rig, "running again", |s| s.state == ServerState::Running);
        assert_eq!(rig.flags.stop_requests.load(Ordering::SeqCst), 1);
        let history = rig.history.lock().unwrap().clone();
        assert_eq!(
            history,
            [
                ServerState::Starting,
                ServerState::Running,
                ServerState::Stopping,
                ServerState::Stopped,
                ServerState::Starting,
                ServerState::Running
            ]
        );
    }

    #[test]
    fn stop_acknowledges_an_error_and_dropping_the_supervisor_stops_the_child() {
        let rig = rig(vec![Ok(Script::refused(
            "memocampus-mcp: cannot listen: address_in_use",
        ))]);
        rig.supervisor.send(Command::Start);
        wait_for(&rig, "error", is_error);
        rig.supervisor.send(Command::Stop);
        wait_for(&rig, "stopped", |s| s.state == ServerState::Stopped);

        let rig = rig_with(
            vec![Ok(Script::listening(8765))],
            Box::new(|| Ok(Settings::default())),
            RestartPolicy::standard(),
        );
        let flags = Arc::clone(&rig.flags);
        rig.supervisor.send(Command::Start);
        wait_for(&rig, "running", |s| s.state == ServerState::Running);
        drop(rig);
        assert_eq!(flags.stop_requests.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn the_unexpected_end_of_a_running_server_is_reported_not_hidden() {
        let rig = rig_with(
            vec![Ok(Script::crashing())],
            Box::new(|| Ok(Settings::default())),
            RestartPolicy::new(0, Duration::from_secs(60), vec![Duration::from_millis(10)]),
        );
        rig.supervisor.send(Command::Start);
        let end = wait_for(&rig, "error", is_error);
        assert_eq!(end.state, ServerState::Error(ErrorKind::CrashLoop));
        assert_eq!(end.last_exit, Some(LastExit::Code(101)));
        assert_eq!(end.pid, None);
    }
}
