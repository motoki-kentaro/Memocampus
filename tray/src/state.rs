//! What the tray reports about the server it manages, and the policy for restarting it.
//!
//! Two things are kept apart on purpose (Issue #67): whether the HTTP server is up (`ServerState`),
//! and whether the vault may be read through it (`VaultStatus`). The tray learns the second only
//! when the server refuses to start; while it runs the vault status is `Unknown`, never "ready".

use crate::classify::Diag;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorKind {
    /// `tray-settings.json` is not usable; the server was not started on guessed values.
    SettingsInvalid(String),
    /// The `memocampus-mcp` executable is not there.
    McpMissing,
    /// The process could not be created.
    LaunchFailed,
    /// Another socket holds the port. The tray never picks another and never touches the holder.
    PortInUse,
    ListenFailed(String),
    /// The server refused the vault at start (`vault_publication_pending`, `vault_not_configured`, ...).
    VaultRefused(String),
    /// The server refused to start for another reason (arguments, token, ...), or exited with the
    /// refusal code without saying why.
    StartRefused(Option<String>),
    /// It did not report that it was listening in time.
    StartTimeout,
    /// It ended without being asked. `will_restart` says whether the policy allows another try.
    Crashed {
        exit: Option<i32>,
        will_restart: bool,
    },
    /// Too many crashes in a short time; the tray stopped trying.
    CrashLoop,
}

impl ErrorKind {
    /// A short stable code for the log and the status text; never a path or a message.
    pub fn code(&self) -> String {
        match self {
            Self::SettingsInvalid(code) => code.clone(),
            Self::McpMissing => "mcp_missing".into(),
            Self::LaunchFailed => "launch_failed".into(),
            Self::PortInUse => "port_in_use".into(),
            Self::ListenFailed(code) => format!("listen_failed_{code}"),
            Self::VaultRefused(code) => code.clone(),
            Self::StartRefused(Some(code)) => format!("start_refused_{code}"),
            Self::StartRefused(None) => "start_refused".into(),
            Self::StartTimeout => "start_timeout".into(),
            Self::Crashed { .. } => "crashed".into(),
            Self::CrashLoop => "crash_loop".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerState {
    Stopped,
    Starting,
    /// The socket is bound (the server said so). Nothing more is claimed.
    Running,
    Stopping,
    Error(ErrorKind),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultStatus {
    /// Not known: the server has not refused, and the tray does not probe the vault.
    Unknown,
    /// The server refused the vault when it started, with this code.
    Refused(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub state: ServerState,
    pub vault: VaultStatus,
    /// The port the server reported, while it runs.
    pub port: Option<u16>,
    /// The server process this snapshot is about, while there is one.
    pub pid: Option<u32>,
    /// How many times in a row the tray restarted it after a crash.
    pub restarts: u32,
    /// How the last child ended, when it has.
    pub last_exit: Option<LastExit>,
    /// Increases on every change, so an observer can tell a new snapshot from a repeat.
    pub seq: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LastExit {
    /// Exit code 0 after a stop request.
    Graceful,
    /// It did not end in time and the tray terminated its own process.
    Forced,
    /// Ended with this code.
    Code(i32),
}

impl Snapshot {
    pub fn initial() -> Self {
        Self {
            state: ServerState::Stopped,
            vault: VaultStatus::Unknown,
            port: None,
            pid: None,
            restarts: 0,
            last_exit: None,
            seq: 0,
        }
    }
}

/// The error a refused start maps to, from the last thing the server said and its exit code.
pub fn error_for_refusal(said: Option<&Diag>) -> ErrorKind {
    match said {
        Some(Diag::PortInUse) => ErrorKind::PortInUse,
        Some(Diag::ListenFailed(code)) => ErrorKind::ListenFailed(code.clone()),
        Some(Diag::VaultRefused(code)) => ErrorKind::VaultRefused(code.clone()),
        Some(Diag::CannotStart(code)) => ErrorKind::StartRefused(Some(code.clone())),
        _ => ErrorKind::StartRefused(None),
    }
}

/// How often and how fast the tray restarts a server that ended without being asked. Never
/// unbounded: more than `max_restarts` crashes within `window` and it gives up (`CrashLoop`).
/// A refusal to start is not a crash and is never retried by this.
#[derive(Debug, Clone)]
pub struct RestartPolicy {
    pub max_restarts: u32,
    pub window: Duration,
    pub backoff: Vec<Duration>,
    crashes: Vec<Instant>,
}

impl RestartPolicy {
    pub fn new(max_restarts: u32, window: Duration, backoff: Vec<Duration>) -> Self {
        assert!(!backoff.is_empty());
        Self {
            max_restarts,
            window,
            backoff,
            crashes: Vec::new(),
        }
    }

    /// 3 restarts within 5 minutes, waiting 1 s, 2 s, then 4 s.
    pub fn standard() -> Self {
        Self::new(
            3,
            Duration::from_secs(300),
            vec![
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
            ],
        )
    }

    /// Records a crash at `now`; the wait before the next start, or `None` to give up.
    pub fn on_crash(&mut self, now: Instant) -> Option<Duration> {
        self.crashes
            .retain(|at| now.saturating_duration_since(*at) < self.window);
        self.crashes.push(now);
        let n = self.crashes.len();
        if n > self.max_restarts as usize {
            return None;
        }
        Some(self.backoff[(n - 1).min(self.backoff.len() - 1)])
    }

    /// A start by the user begins afresh.
    pub fn reset(&mut self) {
        self.crashes.clear();
    }

    pub fn crashes_in_window(&self) -> u32 {
        self.crashes.len() as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_restart_policy_backs_off_and_then_gives_up() {
        let t0 = Instant::now();
        let mut policy = RestartPolicy::standard();
        assert_eq!(policy.on_crash(t0), Some(Duration::from_secs(1)));
        assert_eq!(
            policy.on_crash(t0 + Duration::from_secs(2)),
            Some(Duration::from_secs(2))
        );
        assert_eq!(
            policy.on_crash(t0 + Duration::from_secs(5)),
            Some(Duration::from_secs(4))
        );
        // The fourth crash within the window: no more tries.
        assert_eq!(policy.on_crash(t0 + Duration::from_secs(10)), None);
        assert_eq!(policy.on_crash(t0 + Duration::from_secs(11)), None);
    }

    #[test]
    fn old_crashes_age_out_of_the_window() {
        let t0 = Instant::now();
        let mut policy = RestartPolicy::standard();
        for n in 0..3 {
            assert!(policy.on_crash(t0 + Duration::from_secs(n)).is_some());
        }
        // Long after, a crash is the first again.
        assert_eq!(
            policy.on_crash(t0 + Duration::from_secs(1000)),
            Some(Duration::from_secs(1))
        );
        assert_eq!(policy.crashes_in_window(), 1);
    }

    #[test]
    fn a_user_start_resets_the_count() {
        let t0 = Instant::now();
        let mut policy = RestartPolicy::standard();
        for n in 0..4 {
            policy.on_crash(t0 + Duration::from_secs(n));
        }
        assert_eq!(policy.on_crash(t0 + Duration::from_secs(5)), None);
        policy.reset();
        assert!(policy.on_crash(t0 + Duration::from_secs(6)).is_some());
    }

    #[test]
    fn a_short_backoff_list_repeats_its_last_value() {
        let t0 = Instant::now();
        let mut policy =
            RestartPolicy::new(5, Duration::from_secs(60), vec![Duration::from_millis(10)]);
        for n in 0..5 {
            assert_eq!(
                policy.on_crash(t0 + Duration::from_millis(n)),
                Some(Duration::from_millis(10))
            );
        }
        assert_eq!(policy.on_crash(t0 + Duration::from_millis(6)), None);
    }

    #[test]
    fn a_refusal_maps_to_its_own_error_and_never_to_a_crash() {
        assert_eq!(
            error_for_refusal(Some(&Diag::PortInUse)),
            ErrorKind::PortInUse
        );
        assert_eq!(
            error_for_refusal(Some(&Diag::VaultRefused(
                "vault_publication_pending".into()
            ))),
            ErrorKind::VaultRefused("vault_publication_pending".into())
        );
        assert_eq!(error_for_refusal(None), ErrorKind::StartRefused(None));
        assert_eq!(
            error_for_refusal(Some(&Diag::Listening { port: 1 })),
            ErrorKind::StartRefused(None)
        );
    }

    #[test]
    fn error_codes_never_carry_free_text() {
        for kind in [
            ErrorKind::McpMissing,
            ErrorKind::PortInUse,
            ErrorKind::StartTimeout,
            ErrorKind::CrashLoop,
            ErrorKind::Crashed {
                exit: Some(101),
                will_restart: true,
            },
        ] {
            assert!(
                kind.code()
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b == b'_'),
                "{kind:?}"
            );
        }
    }
}
