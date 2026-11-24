//! What the tray shows, as a pure function of what is known: the icon, the tooltip and every menu
//! item's text, enabled state and check mark. The UI thread only applies a `View`; nothing here
//! touches Windows, so every rule below is unit-tested.
//!
//! Rules (Issue #67, Slice 2): a button is never enabled for an operation the state does not allow;
//! a state is never shown that the supervisor has not reported (no "Running" on a click); the check
//! marks are the settings, and what Windows really does is said beside them; "Running" says nothing
//! about the vault, which has its own line.
//!
//! Every text comes from the catalog (`i18n`) in the language the view is asked for (Issue #78). The
//! language changes words only: which items are enabled, the icon and the check marks are decided
//! from the state alone, the same in every language.

use crate::autostart::{Approval, Autostart};
use crate::controller::OpError;
use crate::i18n::{fill, t, Key, Lang};
use crate::state::{ErrorKind, ServerState, Snapshot, VaultStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconKind {
    Stopped,
    Starting,
    Running,
    Stopping,
    Error,
}

pub fn icon_kind(state: &ServerState) -> IconKind {
    match state {
        ServerState::Stopped => IconKind::Stopped,
        ServerState::Starting => IconKind::Starting,
        ServerState::Running => IconKind::Running,
        ServerState::Stopping => IconKind::Stopping,
        ServerState::Error(_) => IconKind::Error,
    }
}

/// The colour of each state: grey, yellow (starting and stopping), green, red.
pub fn icon_color(kind: IconKind) -> [u8; 3] {
    match kind {
        IconKind::Stopped => [128, 128, 128],
        IconKind::Starting | IconKind::Stopping => [230, 180, 0],
        IconKind::Running => [40, 160, 70],
        IconKind::Error => [200, 50, 50],
    }
}

pub const ICON_SIZE: u32 = 32;

/// The same "M" on a coloured disc for every state, drawn here (no image file, no decoder).
pub fn icon_rgba(kind: IconKind) -> Vec<u8> {
    // A 5 x 5 letter M, each cell 3 x 3 pixels, centred.
    const M: [&str; 5] = ["X...X", "XX.XX", "X.X.X", "X...X", "X...X"];
    let [r, g, b] = icon_color(kind);
    let size = ICON_SIZE as i32;
    let centre = (size - 1) as f32 / 2.0;
    let mut pixels = vec![0u8; (ICON_SIZE * ICON_SIZE * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let (dx, dy) = (x as f32 - centre, y as f32 - centre);
            let distance = (dx * dx + dy * dy).sqrt();
            let at = ((y * size + x) * 4) as usize;
            if distance > 15.0 {
                continue; // transparent outside the disc
            }
            let (cell_x, cell_y) = ((x - 8) / 3, (y - 8) / 3);
            let in_letter = (8..23).contains(&x)
                && (8..23).contains(&y)
                && M[cell_y as usize].as_bytes()[cell_x as usize] == b'X';
            let rim = distance > 13.0;
            let colour = if in_letter {
                [255, 255, 255]
            } else if rim {
                [r / 2, g / 2, b / 2]
            } else {
                [r, g, b]
            };
            pixels[at..at + 4].copy_from_slice(&[colour[0], colour[1], colour[2], 255]);
        }
    }
    pixels
}

/// What the settings file and the supervisor say about the settings in force.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsNote {
    /// The file is valid and the server (if running) was started with it.
    InForce,
    /// The file is valid and differs from what the running server was started with. Nothing is
    /// restarted on the user's behalf.
    RestartNeeded,
    /// The file cannot be used (a fixed code); the last valid settings stay in force.
    Invalid(String),
}

#[derive(Debug, Clone)]
pub struct Inputs {
    pub snapshot: Snapshot,
    /// `listen_port` of the settings now in force (shown while the server is not running).
    pub listen_port: u16,
    pub autostart_setting: bool,
    pub server_autostart_setting: bool,
    pub autostart: Autostart,
    pub settings_note: SettingsNote,
    /// The last operation that failed (a fixed code), until the next succeeds.
    pub last_error: Option<OpError>,
    pub quitting: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    pub icon: IconKind,
    pub tooltip: String,
    pub title: String,
    pub status: String,
    pub address: String,
    pub vault: String,
    pub start_label: String,
    pub start: bool,
    pub stop: bool,
    pub restart: bool,
    pub autostart_checked: bool,
    pub autostart_enabled: bool,
    pub autostart_note: String,
    pub repair_enabled: bool,
    pub server_autostart_checked: bool,
    pub server_autostart_enabled: bool,
    pub settings_note: String,
    pub open_settings: bool,
    pub reload: bool,
    pub open_logs: bool,
    pub quit: bool,
}

fn error_key(kind: &ErrorKind) -> Key {
    match kind {
        ErrorKind::SettingsInvalid(_) => Key::ErrorSettingsInvalid,
        ErrorKind::McpMissing => Key::ErrorMcpMissing,
        ErrorKind::LaunchFailed => Key::ErrorLaunchFailed,
        ErrorKind::PortInUse => Key::ErrorPortInUse,
        ErrorKind::ListenFailed(_) => Key::ErrorListenFailed,
        ErrorKind::VaultRefused(_) => Key::ErrorVaultRefused,
        ErrorKind::StartRefused(_) => Key::ErrorStartRefused,
        ErrorKind::StartTimeout => Key::ErrorStartTimeout,
        ErrorKind::Crashed {
            will_restart: true, ..
        } => Key::ErrorCrashedRestarting,
        ErrorKind::Crashed { .. } => Key::ErrorCrashed,
        ErrorKind::CrashLoop => Key::ErrorCrashLoop,
    }
}

/// The state in a word or two ("稼働中", "Error (port is already in use)"), for the status line and
/// the tooltip.
fn state_word(lang: Lang, state: &ServerState) -> String {
    match state {
        ServerState::Stopped => t(lang, Key::StateStopped),
        ServerState::Starting => t(lang, Key::StateStarting),
        ServerState::Running => t(lang, Key::StateRunning),
        ServerState::Stopping => t(lang, Key::StateStopping),
        ServerState::Error(kind) => fill(
            lang,
            Key::StateError,
            &[("error", &t(lang, error_key(kind)))],
        ),
    }
}

fn op_error_key(error: OpError) -> Key {
    match error {
        OpError::AutostartUnreadable => Key::OpAutostartUnreadable,
        OpError::RegistryInconsistent => Key::OpRegistryInconsistent,
        OpError::SaveFailedUndone => Key::OpSaveFailedUndone,
        OpError::AutostartRegisterFailed => Key::OpAutostartRegisterFailed,
        OpError::SettingsCreateFailed => Key::OpSettingsCreateFailed,
        OpError::SettingsInvalidNoChange => Key::OpSettingsInvalidNoChange,
        OpError::AutostartChangeFailed => Key::OpAutostartChangeFailed,
        OpError::SettingsSaveFailed => Key::OpSettingsSaveFailed,
        OpError::AutostartRepairFailed => Key::OpAutostartRepairFailed,
        OpError::SettingsOpenFailed => Key::OpSettingsOpenFailed,
        OpError::LogsMissing => Key::OpLogsMissing,
        OpError::LogsOpenFailed => Key::OpLogsOpenFailed,
    }
}

fn autostart_note(lang: Lang, setting: bool, actual: Autostart) -> (String, bool) {
    let key = match (actual, setting) {
        (Autostart::NotRegistered, false) => Key::AutostartOff,
        (Autostart::NotRegistered, true) => Key::AutostartNotRegistered,
        (Autostart::Registered(Approval::Enabled), true) => Key::AutostartRegistered,
        (Autostart::Registered(Approval::Enabled), false) => Key::AutostartLeftRegistered,
        (Autostart::Registered(Approval::DisabledByWindows), _) => Key::AutostartDisabledByWindows,
        (Autostart::Registered(Approval::Unknown), _) => Key::AutostartApprovalUnknown,
        (Autostart::PathMismatch(_), _) => Key::AutostartPathMismatch,
        (Autostart::Unreadable, _) => Key::AutostartUnreadable,
    };
    (t(lang, key), matches!(actual, Autostart::PathMismatch(_)))
}

/// The tooltip is limited to 127 UTF-16 units by Windows.
fn limited(text: String) -> String {
    let mut out = String::new();
    let mut units = 0;
    for c in text.chars() {
        units += c.len_utf16();
        if units > 126 {
            out.push('…');
            break;
        }
        out.push(c);
    }
    out
}

pub fn view(inputs: &Inputs, lang: Lang) -> View {
    let snapshot = &inputs.snapshot;
    let state = &snapshot.state;
    let live = !inputs.quitting;
    let address = match (state, snapshot.port) {
        (ServerState::Running, Some(port)) => {
            fill(lang, Key::AddressRunning, &[("port", &port.to_string())])
        }
        _ => fill(
            lang,
            Key::AddressPlanned,
            &[("port", &inputs.listen_port.to_string())],
        ),
    };
    let vault = match &snapshot.vault {
        VaultStatus::Unknown => t(lang, Key::VaultUnknown),
        VaultStatus::Refused(code) => fill(lang, Key::VaultRefused, &[("code", code)]),
    };
    let (autostart_note, mismatch) =
        autostart_note(lang, inputs.autostart_setting, inputs.autostart);
    let settings_note = match (&inputs.last_error, &inputs.settings_note) {
        (Some(error), _) => fill(
            lang,
            Key::LastOperationFailed,
            &[("error", &t(lang, op_error_key(*error)))],
        ),
        (None, SettingsNote::InForce) => t(lang, Key::SettingsInForce),
        (None, SettingsNote::RestartNeeded) => t(lang, Key::SettingsRestartNeeded),
        (None, SettingsNote::Invalid(_)) => t(lang, Key::SettingsInvalid),
    };
    let word = if inputs.quitting {
        t(lang, Key::StateQuitting)
    } else {
        state_word(lang, state)
    };
    let status = fill(lang, Key::StatusLine, &[("state", &word)]);
    let tooltip = limited(match (inputs.quitting, state) {
        (true, _) => t(lang, Key::TooltipQuitting),
        (false, ServerState::Running) => fill(
            lang,
            Key::TooltipRunning,
            &[("state", &word), ("address", &address)],
        ),
        (false, _) => fill(lang, Key::Tooltip, &[("state", &word)]),
    });
    let can_start = matches!(state, ServerState::Stopped | ServerState::Error(_));
    View {
        icon: icon_kind(state),
        tooltip,
        title: t(lang, Key::Title),
        status,
        address,
        vault,
        start_label: t(
            lang,
            if matches!(state, ServerState::Error(_)) {
                Key::StartServerRetry
            } else {
                Key::StartServer
            },
        ),
        start: live && can_start,
        stop: live && matches!(state, ServerState::Running | ServerState::Starting),
        restart: live && matches!(state, ServerState::Running | ServerState::Error(_)),
        autostart_checked: inputs.autostart_setting,
        autostart_enabled: live,
        autostart_note,
        repair_enabled: live && mismatch,
        server_autostart_checked: inputs.server_autostart_setting,
        server_autostart_enabled: live,
        settings_note,
        open_settings: live,
        reload: live,
        open_logs: live,
        quit: live,
    }
}

/// What a click on a menu item asks for. The menu items carry these ids; nothing else is accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Start,
    Stop,
    Restart,
    ToggleAutostart,
    ToggleServerAutostart,
    RepairAutostart,
    OpenSettings,
    Reload,
    OpenLogs,
    Quit,
}

pub const ACTION_IDS: [(&str, Action); 10] = [
    ("start", Action::Start),
    ("stop", Action::Stop),
    ("restart", Action::Restart),
    ("autostart", Action::ToggleAutostart),
    ("server_autostart", Action::ToggleServerAutostart),
    ("repair_autostart", Action::RepairAutostart),
    ("open_settings", Action::OpenSettings),
    ("reload", Action::Reload),
    ("open_logs", Action::OpenLogs),
    ("quit", Action::Quit),
];

pub fn action_for_id(id: &str) -> Option<Action> {
    ACTION_IDS
        .iter()
        .find(|(name, _)| *name == id)
        .map(|(_, action)| *action)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rules below were written against the Japanese source text; English is checked after them.
    fn view(inputs: &Inputs) -> View {
        super::view(inputs, Lang::Ja)
    }

    fn inputs(state: ServerState) -> Inputs {
        let mut snapshot = Snapshot::initial();
        snapshot.state = state;
        Inputs {
            snapshot,
            listen_port: 8765,
            autostart_setting: true,
            server_autostart_setting: true,
            autostart: Autostart::Registered(Approval::Enabled),
            settings_note: SettingsNote::InForce,
            last_error: None,
            quitting: false,
        }
    }

    fn ops(state: ServerState) -> (bool, bool, bool) {
        let v = view(&inputs(state));
        (v.start, v.stop, v.restart)
    }

    #[test]
    fn each_state_allows_exactly_the_operations_that_make_sense() {
        // (start, stop, restart)
        assert_eq!(ops(ServerState::Stopped), (true, false, false));
        assert_eq!(ops(ServerState::Starting), (false, true, false));
        assert_eq!(ops(ServerState::Running), (false, true, true));
        assert_eq!(ops(ServerState::Stopping), (false, false, false));
        let error = ServerState::Error(ErrorKind::PortInUse);
        assert_eq!(ops(error), (true, false, true), "an error can be retried");
    }

    #[test]
    fn the_retry_is_labelled_as_one_only_after_an_error() {
        assert_eq!(
            view(&inputs(ServerState::Stopped)).start_label,
            "サーバ起動"
        );
        assert_eq!(
            view(&inputs(ServerState::Error(ErrorKind::CrashLoop))).start_label,
            "サーバ起動（再試行）"
        );
    }

    #[test]
    fn quitting_disables_every_operation_and_says_so() {
        for state in [
            ServerState::Stopped,
            ServerState::Running,
            ServerState::Error(ErrorKind::McpMissing),
        ] {
            let mut i = inputs(state);
            i.quitting = true;
            let v = view(&i);
            assert!(!v.start && !v.stop && !v.restart);
            assert!(!v.autostart_enabled && !v.server_autostart_enabled && !v.repair_enabled);
            assert!(!v.open_settings && !v.reload && !v.open_logs && !v.quit);
            assert!(v.tooltip.contains("終了処理中") && v.status.contains("終了処理中"));
        }
    }

    #[test]
    fn the_icon_follows_the_reported_state_only() {
        let kinds: Vec<IconKind> = [
            ServerState::Stopped,
            ServerState::Starting,
            ServerState::Running,
            ServerState::Stopping,
            ServerState::Error(ErrorKind::StartTimeout),
        ]
        .into_iter()
        .map(|s| view(&inputs(s)).icon)
        .collect();
        assert_eq!(
            kinds,
            [
                IconKind::Stopped,
                IconKind::Starting,
                IconKind::Running,
                IconKind::Stopping,
                IconKind::Error
            ]
        );
        // The tooltip says the state in words, so colour is not the only carrier.
        assert!(view(&inputs(ServerState::Running))
            .tooltip
            .contains("稼働中"));
        assert!(view(&inputs(ServerState::Stopped))
            .tooltip
            .contains("停止中"));
        assert!(view(&inputs(ServerState::Error(ErrorKind::PortInUse)))
            .tooltip
            .contains("ポート"));
    }

    #[test]
    fn the_address_is_the_reported_one_only_while_running() {
        let mut running = inputs(ServerState::Running);
        running.snapshot.port = Some(9001);
        assert_eq!(view(&running).address, "127.0.0.1:9001");
        let stopped = inputs(ServerState::Stopped);
        assert_eq!(view(&stopped).address, "待受予定: 127.0.0.1:8765");
        // Running without a reported port is not shown as an address that was never reported.
        assert_eq!(
            view(&inputs(ServerState::Running)).address,
            "待受予定: 127.0.0.1:8765"
        );
    }

    #[test]
    fn running_does_not_say_the_vault_is_readable() {
        let running = view(&inputs(ServerState::Running));
        assert!(running.vault.contains("未確認"), "{}", running.vault);
        let mut refused = inputs(ServerState::Error(ErrorKind::VaultRefused(
            "vault_publication_pending".into(),
        )));
        refused.snapshot.vault = VaultStatus::Refused("vault_publication_pending".into());
        let v = view(&refused);
        assert!(v.vault.contains("vault_publication_pending"));
        assert!(v.status.contains("Vault"));
    }

    #[test]
    fn the_check_marks_are_the_settings_and_the_notes_say_what_windows_does() {
        let mut i = inputs(ServerState::Stopped);
        i.autostart = Autostart::Registered(Approval::DisabledByWindows);
        let v = view(&i);
        assert!(v.autostart_checked, "the setting stays on");
        assert!(v.autostart_note.contains("Windows側で無効"));
        i.autostart_setting = false;
        i.autostart = Autostart::NotRegistered;
        let v = view(&i);
        assert!(!v.autostart_checked);
        assert_eq!(v.autostart_note, "自動起動: オフ");
        i.autostart_setting = true;
        let v = view(&i);
        assert!(
            v.autostart_note.contains("未登録"),
            "on in the settings, but not registered"
        );
        i.server_autostart_setting = false;
        assert!(!view(&i).server_autostart_checked);
    }

    #[test]
    fn a_moved_executable_is_reported_and_can_be_repaired_only_on_request() {
        let mut i = inputs(ServerState::Stopped);
        i.autostart = Autostart::PathMismatch(Approval::Enabled);
        let v = view(&i);
        assert!(v.autostart_note.contains("異なります"));
        assert!(v.repair_enabled);
        i.autostart = Autostart::Registered(Approval::Enabled);
        assert!(!view(&i).repair_enabled);
        i.autostart = Autostart::Unreadable;
        assert!(view(&i).autostart_note.contains("読めません"));
    }

    #[test]
    fn settings_notes_distinguish_in_force_pending_restart_and_invalid() {
        let mut i = inputs(ServerState::Running);
        assert_eq!(view(&i).settings_note, "設定: 反映済み");
        i.settings_note = SettingsNote::RestartNeeded;
        assert!(view(&i).settings_note.contains("再起動で反映"));
        i.settings_note = SettingsNote::Invalid("settings_invalid_json".into());
        assert!(view(&i).settings_note.contains("不正"));
        i.last_error = Some(OpError::AutostartChangeFailed);
        assert!(view(&i).settings_note.starts_with("直前の操作に失敗"));
    }

    const ALL_STATES: [fn() -> ServerState; 16] = [
        || ServerState::Stopped,
        || ServerState::Starting,
        || ServerState::Running,
        || ServerState::Stopping,
        || ServerState::Error(ErrorKind::SettingsInvalid("settings_invalid_json".into())),
        || ServerState::Error(ErrorKind::McpMissing),
        || ServerState::Error(ErrorKind::LaunchFailed),
        || ServerState::Error(ErrorKind::PortInUse),
        || ServerState::Error(ErrorKind::ListenFailed("os_10013".into())),
        || ServerState::Error(ErrorKind::VaultRefused("vault_publication_pending".into())),
        || ServerState::Error(ErrorKind::StartRefused(Some("token".into()))),
        || ServerState::Error(ErrorKind::StartRefused(None)),
        || ServerState::Error(ErrorKind::StartTimeout),
        || {
            ServerState::Error(ErrorKind::Crashed {
                exit: Some(1),
                will_restart: true,
            })
        },
        || {
            ServerState::Error(ErrorKind::Crashed {
                exit: None,
                will_restart: false,
            })
        },
        || ServerState::Error(ErrorKind::CrashLoop),
    ];

    const ALL_OP_ERRORS: [OpError; 12] = [
        OpError::AutostartUnreadable,
        OpError::RegistryInconsistent,
        OpError::SaveFailedUndone,
        OpError::AutostartRegisterFailed,
        OpError::SettingsCreateFailed,
        OpError::SettingsInvalidNoChange,
        OpError::AutostartChangeFailed,
        OpError::SettingsSaveFailed,
        OpError::AutostartRepairFailed,
        OpError::SettingsOpenFailed,
        OpError::LogsMissing,
        OpError::LogsOpenFailed,
    ];

    const ALL_AUTOSTART: [Autostart; 6] = [
        Autostart::NotRegistered,
        Autostart::Registered(Approval::Enabled),
        Autostart::Registered(Approval::DisabledByWindows),
        Autostart::Registered(Approval::Unknown),
        Autostart::PathMismatch(Approval::Enabled),
        Autostart::Unreadable,
    ];

    /// Every combination of what the view depends on that changes a text.
    fn every_input() -> Vec<Inputs> {
        let mut all = Vec::new();
        for state in ALL_STATES {
            for quitting in [false, true] {
                for port in [None, Some(9001)] {
                    let mut i = inputs(state());
                    i.quitting = quitting;
                    i.snapshot.port = port;
                    if let ServerState::Error(ErrorKind::VaultRefused(code)) = &i.snapshot.state {
                        i.snapshot.vault = VaultStatus::Refused(code.clone());
                    }
                    all.push(i);
                }
            }
        }
        for autostart in ALL_AUTOSTART {
            for setting in [false, true] {
                let mut i = inputs(ServerState::Stopped);
                i.autostart = autostart;
                i.autostart_setting = setting;
                all.push(i);
            }
        }
        for note in [
            SettingsNote::InForce,
            SettingsNote::RestartNeeded,
            SettingsNote::Invalid("settings_invalid_json".into()),
        ] {
            let mut i = inputs(ServerState::Running);
            i.settings_note = note;
            all.push(i);
        }
        for error in ALL_OP_ERRORS {
            let mut i = inputs(ServerState::Running);
            i.last_error = Some(error);
            all.push(i);
        }
        all
    }

    fn texts(v: &View) -> [&str; 8] {
        [
            &v.tooltip,
            &v.title,
            &v.status,
            &v.address,
            &v.vault,
            &v.start_label,
            &v.autostart_note,
            &v.settings_note,
        ]
    }

    fn has_japanese(text: &str) -> bool {
        text.chars().any(|c| {
            matches!(c,
                '\u{3000}'..='\u{303F}' | '\u{3040}'..='\u{30FF}' | '\u{4E00}'..='\u{9FFF}'
                | '\u{FF00}'..='\u{FFEF}')
        })
    }

    #[test]
    fn the_language_changes_the_words_only_never_what_can_be_done() {
        for i in every_input() {
            let (ja, en) = (super::view(&i, Lang::Ja), super::view(&i, Lang::En));
            let decisions = |v: &View| {
                (
                    v.icon,
                    v.start,
                    v.stop,
                    v.restart,
                    v.autostart_checked,
                    v.autostart_enabled,
                    v.repair_enabled,
                    v.server_autostart_checked,
                    v.server_autostart_enabled,
                    (v.open_settings, v.reload, v.open_logs, v.quit),
                )
            };
            assert_eq!(decisions(&ja), decisions(&en), "{i:?}");
        }
    }

    #[test]
    fn every_english_text_is_english_and_no_placeholder_is_left_in_either_language() {
        for i in every_input() {
            for lang in [Lang::Ja, Lang::En] {
                let v = super::view(&i, lang);
                for text in texts(&v) {
                    assert!(!text.is_empty(), "{i:?}");
                    assert!(!text.contains('{') && !text.contains('}'), "{text}");
                    if lang == Lang::En {
                        assert!(!has_japanese(text), "{text}");
                    }
                }
                assert!(v.tooltip.encode_utf16().count() <= 127, "{}", v.tooltip);
            }
        }
    }

    #[test]
    fn every_state_and_every_failure_has_its_own_words() {
        for lang in [Lang::Ja, Lang::En] {
            let mut statuses: Vec<String> = ALL_STATES
                .iter()
                .filter(|s| !matches!(s(), ServerState::Error(ErrorKind::StartRefused(None))))
                .map(|s| super::view(&inputs(s()), lang).status)
                .collect();
            let count = statuses.len();
            statuses.sort();
            statuses.dedup();
            // A refusal with or without its reason reads the same; the other kinds all differ.
            assert_eq!(statuses.len(), count, "{lang:?}: {statuses:?}");
            let mut notes: Vec<String> = ALL_OP_ERRORS
                .iter()
                .map(|e| {
                    let mut i = inputs(ServerState::Running);
                    i.last_error = Some(*e);
                    super::view(&i, lang).settings_note
                })
                .collect();
            notes.sort();
            notes.dedup();
            assert_eq!(notes.len(), ALL_OP_ERRORS.len(), "{lang:?}");
        }
    }

    #[test]
    fn the_japanese_texts_are_the_ones_shown_before_the_catalog() {
        let mut running = inputs(ServerState::Running);
        running.snapshot.port = Some(9001);
        let v = view(&running);
        assert_eq!(v.title, "Memocampus MCP Server");
        assert_eq!(v.status, "状態: 稼働中");
        assert_eq!(v.tooltip, "Memocampus MCP: 稼働中 127.0.0.1:9001");
        assert_eq!(
            v.vault,
            "Vault公開: 未確認（稼働中でも公開可否は示しません）"
        );
        assert_eq!(v.autostart_note, "自動起動: 登録済み");
        assert_eq!(v.settings_note, "設定: 反映済み");
        let v = view(&inputs(ServerState::Error(ErrorKind::PortInUse)));
        assert_eq!(v.status, "状態: エラー（ポートが使用中です）");
        assert_eq!(v.tooltip, "Memocampus MCP: エラー（ポートが使用中です）");
        assert_eq!(
            view(&inputs(ServerState::Stopped)).tooltip,
            "Memocampus MCP: 停止中"
        );
        assert_eq!(view(&inputs(ServerState::Starting)).status, "状態: 起動中…");
        let mut quitting = inputs(ServerState::Running);
        quitting.quitting = true;
        let v = view(&quitting);
        assert_eq!(v.status, "状態: 終了処理中…");
        assert_eq!(v.tooltip, "Memocampus MCP: 終了処理中");
        let mut failed = inputs(ServerState::Running);
        failed.last_error = Some(OpError::SaveFailedUndone);
        assert_eq!(
            view(&failed).settings_note,
            "直前の操作に失敗: 設定ファイルを保存できませんでした（変更を取り消しました）"
        );
    }

    #[test]
    fn the_english_texts_say_the_same_things() {
        let en = |i: &Inputs| super::view(i, Lang::En);
        let mut running = inputs(ServerState::Running);
        running.snapshot.port = Some(9001);
        let v = en(&running);
        assert_eq!(v.title, "Memocampus MCP Server");
        assert_eq!(v.status, "Status: Running");
        assert_eq!(v.address, "127.0.0.1:9001");
        assert_eq!(v.tooltip, "Memocampus MCP: Running 127.0.0.1:9001");
        assert!(
            v.vault.starts_with("Vault access: Not confirmed"),
            "{}",
            v.vault
        );
        assert_eq!(v.start_label, "Start server");
        let stopped = en(&inputs(ServerState::Stopped));
        assert_eq!(stopped.address, "Will listen on 127.0.0.1:8765");
        assert_eq!(stopped.tooltip, "Memocampus MCP: Stopped");
        let error = en(&inputs(ServerState::Error(ErrorKind::PortInUse)));
        assert_eq!(error.status, "Status: Error (port is already in use)");
        assert_eq!(error.start_label, "Start server (retry)");
        let mut refused = inputs(ServerState::Error(ErrorKind::VaultRefused(
            "vault_publication_pending".into(),
        )));
        refused.snapshot.vault = VaultStatus::Refused("vault_publication_pending".into());
        assert_eq!(
            en(&refused).vault,
            "Vault access: Refused (vault_publication_pending)",
            "the code is shown as it is"
        );
        let mut quitting = inputs(ServerState::Running);
        quitting.quitting = true;
        assert_eq!(en(&quitting).status, "Status: Quitting…");
        let mut failed = inputs(ServerState::Running);
        failed.last_error = Some(OpError::LogsMissing);
        assert_eq!(
            en(&failed).settings_note,
            "Last action failed: the log folder does not exist yet"
        );
    }

    #[test]
    fn the_tooltip_fits_what_windows_allows() {
        let mut i = inputs(ServerState::Error(ErrorKind::SettingsInvalid(
            "x".repeat(500),
        )));
        i.snapshot.port = None;
        let v = view(&i);
        assert!(
            v.tooltip.encode_utf16().count() <= 127,
            "{}",
            v.tooltip.len()
        );
        let long = limited("あ".repeat(300));
        assert!(long.encode_utf16().count() <= 127 && long.ends_with('…'));
    }

    #[test]
    fn every_menu_id_maps_to_one_action_and_nothing_else_does() {
        let mut seen = Vec::new();
        for (id, action) in ACTION_IDS {
            assert_eq!(action_for_id(id), Some(action));
            assert!(!seen.contains(&action), "{id}");
            seen.push(action);
        }
        for other in ["", "Start", "title", "status", "shutdown", "start "] {
            assert_eq!(action_for_id(other), None, "{other:?}");
        }
    }

    #[test]
    fn the_icons_are_one_design_in_five_state_colours() {
        let kinds = [
            IconKind::Stopped,
            IconKind::Starting,
            IconKind::Running,
            IconKind::Stopping,
            IconKind::Error,
        ];
        let icons: Vec<Vec<u8>> = kinds.iter().map(|k| icon_rgba(*k)).collect();
        for icon in &icons {
            assert_eq!(icon.len(), (ICON_SIZE * ICON_SIZE * 4) as usize);
            // The corner is outside the disc: transparent. The letter is white. The disc is solid.
            assert_eq!(icon[3], 0);
            let centre = ((ICON_SIZE / 2 * ICON_SIZE + ICON_SIZE / 2) * 4) as usize;
            assert_eq!(icon[centre + 3], 255);
            let white = icon
                .chunks(4)
                .filter(|p| p[..3] == [255, 255, 255] && p[3] == 255)
                .count();
            assert!(white > 30, "the M is drawn: {white}");
        }
        // Distinct colours for distinct meanings; starting and stopping share one on purpose.
        let colour_at = |icon: &Vec<u8>| {
            let at = ((10 * ICON_SIZE + 6) * 4) as usize;
            icon[at..at + 3].to_vec()
        };
        assert_eq!(colour_at(&icons[1]), colour_at(&icons[3]));
        let mut distinct = vec![
            colour_at(&icons[0]),
            colour_at(&icons[1]),
            colour_at(&icons[2]),
            colour_at(&icons[4]),
        ];
        distinct.sort();
        distinct.dedup();
        assert_eq!(distinct.len(), 4);
        // Same shape in every state: only colour differs.
        let alpha = |icon: &Vec<u8>| icon.chunks(4).map(|p| p[3]).collect::<Vec<_>>();
        assert!(icons.iter().all(|icon| alpha(icon) == alpha(&icons[0])));
    }
}
