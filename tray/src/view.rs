//! What the tray shows, as a pure function of what is known: the icon, the tooltip and every menu
//! item's text, enabled state and check mark. The UI thread only applies a `View`; nothing here
//! touches Windows, so every rule below is unit-tested.
//!
//! Rules (Issue #67, Slice 2): a button is never enabled for an operation the state does not allow;
//! a state is never shown that the supervisor has not reported (no "Running" on a click); the check
//! marks are the settings, and what Windows really does is said beside them; "Running" says nothing
//! about the vault, which has its own line.

use crate::autostart::{Approval, Autostart};
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
    /// The last operation that failed (a fixed, user-readable text), until the next succeeds.
    pub last_error: Option<String>,
    pub quitting: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    pub icon: IconKind,
    pub tooltip: String,
    pub title: &'static str,
    pub status: String,
    pub address: String,
    pub vault: String,
    pub start_label: &'static str,
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

fn error_text(kind: &ErrorKind) -> String {
    match kind {
        ErrorKind::SettingsInvalid(_) => "設定ファイルが不正です".into(),
        ErrorKind::McpMissing => "MCP実行ファイルが見つかりません".into(),
        ErrorKind::LaunchFailed => "MCPを起動できません".into(),
        ErrorKind::PortInUse => "ポートが使用中です".into(),
        ErrorKind::ListenFailed(_) => "待受を開始できません".into(),
        ErrorKind::VaultRefused(_) => "Vaultの公開が承認されていません".into(),
        ErrorKind::StartRefused(_) => "MCPが起動を拒否しました".into(),
        ErrorKind::StartTimeout => "起動が時間内に完了しません".into(),
        ErrorKind::Crashed {
            will_restart: true, ..
        } => "異常終了しました（再起動を試みます）".into(),
        ErrorKind::Crashed { .. } => "異常終了しました".into(),
        ErrorKind::CrashLoop => "異常終了を繰り返したため停止しました".into(),
    }
}

fn state_text(state: &ServerState) -> String {
    match state {
        ServerState::Stopped => "状態: 停止中".into(),
        ServerState::Starting => "状態: 起動中…".into(),
        ServerState::Running => "状態: 稼働中".into(),
        ServerState::Stopping => "状態: 停止処理中…".into(),
        ServerState::Error(kind) => format!("状態: エラー（{}）", error_text(kind)),
    }
}

fn autostart_note(setting: bool, actual: Autostart) -> (String, bool) {
    let text = match (actual, setting) {
        (Autostart::NotRegistered, false) => "自動起動: オフ",
        (Autostart::NotRegistered, true) => "自動起動: 未登録（設定はオンです）",
        (Autostart::Registered(Approval::Enabled), true) => "自動起動: 登録済み",
        (Autostart::Registered(Approval::Enabled), false) => {
            "自動起動: 登録が残っています（設定はオフです）"
        }
        (Autostart::Registered(Approval::DisabledByWindows), _) => {
            "自動起動: Windows側で無効にされています"
        }
        (Autostart::Registered(Approval::Unknown), _) => {
            "自動起動: 登録済み（Windows側の状態は不明）"
        }
        (Autostart::PathMismatch(_), _) => "自動起動: 登録パスが現在の実行ファイルと異なります",
        (Autostart::Unreadable, _) => "自動起動: レジストリを読めません",
    };
    (text.into(), matches!(actual, Autostart::PathMismatch(_)))
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

pub fn view(inputs: &Inputs) -> View {
    let snapshot = &inputs.snapshot;
    let state = &snapshot.state;
    let live = !inputs.quitting;
    let address = match (state, snapshot.port) {
        (ServerState::Running, Some(port)) => format!("127.0.0.1:{port}"),
        _ => format!("待受予定: 127.0.0.1:{}", inputs.listen_port),
    };
    let vault = match &snapshot.vault {
        VaultStatus::Unknown => "Vault公開: 未確認（稼働中でも公開可否は示しません）".to_owned(),
        VaultStatus::Refused(code) => format!("Vault公開: 拒否（{code}）"),
    };
    let (autostart_note, mismatch) = autostart_note(inputs.autostart_setting, inputs.autostart);
    let settings_note = match (&inputs.last_error, &inputs.settings_note) {
        (Some(error), _) => format!("直前の操作に失敗: {error}"),
        (None, SettingsNote::InForce) => "設定: 反映済み".into(),
        (None, SettingsNote::RestartNeeded) => "設定: 変更あり（サーバ再起動で反映）".into(),
        (None, SettingsNote::Invalid(_)) => "設定: 不正（以前の設定を使用中）".into(),
    };
    let status = if inputs.quitting {
        "状態: 終了処理中…".to_owned()
    } else {
        state_text(state)
    };
    let tooltip = limited(match (inputs.quitting, state) {
        (true, _) => "Memocampus MCP: 終了処理中".to_owned(),
        (false, ServerState::Running) => format!("Memocampus MCP: 稼働中 {address}"),
        (false, other) => format!(
            "Memocampus MCP: {}",
            state_text(other).trim_start_matches("状態: ")
        ),
    });
    let can_start = matches!(state, ServerState::Stopped | ServerState::Error(_));
    View {
        icon: icon_kind(state),
        tooltip,
        title: "Memocampus MCP Server",
        status,
        address,
        vault,
        start_label: if matches!(state, ServerState::Error(_)) {
            "サーバ起動（再試行）"
        } else {
            "サーバ起動"
        },
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
        i.last_error = Some("レジストリに書き込めません".into());
        assert!(view(&i).settings_note.starts_with("直前の操作に失敗"));
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
