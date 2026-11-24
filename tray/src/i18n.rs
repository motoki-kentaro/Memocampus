//! The tray's human-facing text in Japanese and English (Issue #78).
//!
//! Japanese is the source text; English is its translation. The language follows the Windows user UI
//! language (`GetUserDefaultUILanguage`, the same API the installer uses for its default): Japanese
//! shows Japanese, every other language and a language that cannot be determined show English. There
//! is no setting, no command-line option and no environment variable for it.
//!
//! Every text is a `Key`, and the catalog below gives each key both languages: an entry without one
//! of them does not compile, and an English lookup can never fall back to Japanese (there is no
//! fallback path at all). Values are inserted only through `{name}` placeholders and `fill`; the tests
//! check that both languages of a key use the same placeholders and that English holds no Japanese.
//!
//! Only what a person reads is here. Menu ids, log lines, error codes (`vault_publication_pending`,
//! `settings_invalid_json`, ...), registry names, host names and the window class stay as they are
//! in every language, and nothing decides anything by comparing these texts.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Ja,
    En,
}

/// `LANG_JAPANESE`, the primary language of a LANGID (its low 10 bits).
const LANG_JAPANESE: u16 = 0x11;

impl Lang {
    /// The language for a Windows LANGID. `None` (or 0) is a language Windows could not tell: English.
    pub fn from_langid(langid: Option<u16>) -> Lang {
        match langid {
            Some(id) if id & 0x3ff == LANG_JAPANESE => Lang::Ja,
            _ => Lang::En,
        }
    }

    /// The language of the current user's Windows UI, decided once at start.
    #[cfg(windows)]
    pub fn detect() -> Lang {
        // SAFETY: no arguments; it returns a LANGID, 0 when it has none to give.
        let id = unsafe { windows_sys::Win32::Globalization::GetUserDefaultUILanguage() };
        Lang::from_langid((id != 0).then_some(id))
    }

    /// For the diagnostic log only.
    pub fn code(self) -> &'static str {
        match self {
            Lang::Ja => "ja",
            Lang::En => "en",
        }
    }
}

macro_rules! catalog {
    ($($key:ident { ja: $ja:literal, en: $en:literal $(,)? })*) => {
        /// One text the tray shows.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Key { $($key),* }

        impl Key {
            pub const ALL: &'static [Key] = &[$(Key::$key),*];
        }

        /// The text of `key` in `lang`, placeholders not yet filled.
        pub fn text(lang: Lang, key: Key) -> &'static str {
            match lang {
                Lang::Ja => match key { $(Key::$key => $ja),* },
                Lang::En => match key { $(Key::$key => $en),* },
            }
        }
    };
}

catalog! {
    // The menu: its title, the operations and the items that only inform.
    Title { ja: "Memocampus MCP Server", en: "Memocampus MCP Server" }
    StartServer { ja: "サーバ起動", en: "Start server" }
    StartServerRetry { ja: "サーバ起動（再試行）", en: "Start server (retry)" }
    StopServer { ja: "サーバ停止", en: "Stop server" }
    RestartServer { ja: "サーバ再起動", en: "Restart server" }
    StartWithWindows { ja: "Windowsログオン時に起動", en: "Start with Windows" }
    RepairAutostart { ja: "自動起動の登録を現在の場所へ更新", en: "Update startup entry to this location" }
    StartServerAutomatically { ja: "MCPサーバを自動起動", en: "Start MCP server automatically" }
    OpenSettings { ja: "設定ファイルを開く", en: "Open configuration file" }
    ReloadSettings { ja: "設定を再読み込み", en: "Reload configuration" }
    OpenLogs { ja: "ログフォルダを開く", en: "Open log folder" }
    Quit { ja: "終了", en: "Quit" }

    // Before the first state is known (replaced at once by the first view).
    StatusChecking { ja: "状態: 確認中…", en: "Status: Checking…" }
    AddressChecking { ja: "待受予定: 127.0.0.1", en: "Will listen on 127.0.0.1" }
    VaultChecking { ja: "Vault公開: 未確認", en: "Vault access: Not confirmed" }
    AutostartChecking { ja: "自動起動: 確認中…", en: "Startup: Checking…" }
    SettingsChecking { ja: "設定: 確認中…", en: "Configuration: Checking…" }

    // The server's state: `StatusLine` carries one of the words below.
    StatusLine { ja: "状態: {state}", en: "Status: {state}" }
    StateStopped { ja: "停止中", en: "Stopped" }
    StateStarting { ja: "起動中…", en: "Starting…" }
    StateRunning { ja: "稼働中", en: "Running" }
    StateStopping { ja: "停止処理中…", en: "Stopping…" }
    StateError { ja: "エラー（{error}）", en: "Error ({error})" }
    StateQuitting { ja: "終了処理中…", en: "Quitting…" }

    // Why the server is in error.
    ErrorSettingsInvalid { ja: "設定ファイルが不正です", en: "configuration file is invalid" }
    ErrorMcpMissing { ja: "MCP実行ファイルが見つかりません", en: "MCP server program not found" }
    ErrorLaunchFailed { ja: "MCPを起動できません", en: "cannot start the MCP server" }
    ErrorPortInUse { ja: "ポートが使用中です", en: "port is already in use" }
    ErrorListenFailed { ja: "待受を開始できません", en: "cannot listen on the port" }
    ErrorVaultRefused { ja: "Vaultの公開が承認されていません", en: "vault access is not approved" }
    ErrorStartRefused { ja: "MCPが起動を拒否しました", en: "the MCP server refused to start" }
    ErrorStartTimeout { ja: "起動が時間内に完了しません", en: "start did not finish in time" }
    ErrorCrashedRestarting { ja: "異常終了しました（再起動を試みます）", en: "stopped unexpectedly (restarting)" }
    ErrorCrashed { ja: "異常終了しました", en: "stopped unexpectedly" }
    ErrorCrashLoop { ja: "異常終了を繰り返したため停止しました", en: "stopped after repeated crashes" }

    // The address line: the reported address while running, the configured port otherwise.
    AddressRunning { ja: "127.0.0.1:{port}", en: "127.0.0.1:{port}" }
    AddressPlanned { ja: "待受予定: 127.0.0.1:{port}", en: "Will listen on 127.0.0.1:{port}" }

    // The vault line. `{code}` is the server's refusal code, shown as it is.
    VaultUnknown {
        ja: "Vault公開: 未確認（稼働中でも公開可否は示しません）",
        en: "Vault access: Not confirmed (Running does not mean the vault is available)",
    }
    VaultRefused { ja: "Vault公開: 拒否（{code}）", en: "Vault access: Refused ({code})" }

    // The logon auto-start registration, beside its check mark.
    AutostartOff { ja: "自動起動: オフ", en: "Startup: Off" }
    AutostartNotRegistered { ja: "自動起動: 未登録（設定はオンです）", en: "Startup: Not registered (the setting is on)" }
    AutostartRegistered { ja: "自動起動: 登録済み", en: "Startup: Registered" }
    AutostartLeftRegistered {
        ja: "自動起動: 登録が残っています（設定はオフです）",
        en: "Startup: Still registered (the setting is off)",
    }
    AutostartDisabledByWindows { ja: "自動起動: Windows側で無効にされています", en: "Startup: Turned off in Windows" }
    AutostartApprovalUnknown {
        ja: "自動起動: 登録済み（Windows側の状態は不明）",
        en: "Startup: Registered (its state in Windows is unknown)",
    }
    AutostartPathMismatch {
        ja: "自動起動: 登録パスが現在の実行ファイルと異なります",
        en: "Startup: Registered for a different program location",
    }
    AutostartUnreadable { ja: "自動起動: レジストリを読めません", en: "Startup: Cannot read the registry" }

    // The settings line, or the last operation that failed.
    SettingsInForce { ja: "設定: 反映済み", en: "Configuration: Applied" }
    SettingsRestartNeeded {
        ja: "設定: 変更あり（サーバ再起動で反映）",
        en: "Configuration: Changed (restart the server to apply)",
    }
    SettingsInvalid { ja: "設定: 不正（以前の設定を使用中）", en: "Configuration: Invalid (previous settings in use)" }
    LastOperationFailed { ja: "直前の操作に失敗: {error}", en: "Last action failed: {error}" }

    // What the last operation failed with (`controller::OpError`).
    OpAutostartUnreadable {
        ja: "ログオン時の自動起動の登録を読み取れないため変更しません",
        en: "the startup entry cannot be read, so it was not changed",
    }
    OpRegistryInconsistent {
        ja: "自動起動の登録を元に戻せず、設定と登録が一致していません（レジストリのMemocampusTrayを確認してください）",
        en: "the startup entry could not be restored and does not match the setting (check MemocampusTray in the registry)",
    }
    OpSaveFailedUndone {
        ja: "設定ファイルを保存できませんでした（変更を取り消しました）",
        en: "could not save the configuration file (the change was undone)",
    }
    OpAutostartRegisterFailed { ja: "ログオン時の自動起動を登録できませんでした", en: "could not register startup" }
    OpSettingsCreateFailed { ja: "設定ファイルを作成できませんでした", en: "could not create the configuration file" }
    OpSettingsInvalidNoChange {
        ja: "設定ファイルが不正なため変更できません（ファイルは変更していません）",
        en: "the configuration file is invalid, so nothing was changed (the file was left as it is)",
    }
    OpAutostartChangeFailed {
        ja: "ログオン時の自動起動を変更できませんでした（レジストリ）",
        en: "could not change startup (registry)",
    }
    OpSettingsSaveFailed { ja: "設定ファイルを保存できませんでした", en: "could not save the configuration file" }
    OpAutostartRepairFailed { ja: "自動起動の登録を更新できませんでした", en: "could not update the startup entry" }
    OpSettingsOpenFailed { ja: "設定ファイルを開けませんでした", en: "could not open the configuration file" }
    OpLogsMissing { ja: "ログフォルダがまだありません", en: "the log folder does not exist yet" }
    OpLogsOpenFailed { ja: "ログフォルダを開けませんでした", en: "could not open the log folder" }

    // The tooltip. `{state}` is one of the state words above.
    Tooltip { ja: "Memocampus MCP: {state}", en: "Memocampus MCP: {state}" }
    TooltipRunning { ja: "Memocampus MCP: {state} {address}", en: "Memocampus MCP: {state} {address}" }
    TooltipQuitting { ja: "Memocampus MCP: 終了処理中", en: "Memocampus MCP: Quitting" }
}

/// The `{name}` placeholders of a text, in order.
pub fn placeholders(template: &str) -> Vec<&str> {
    let mut names = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let Some(close) = rest[open..].find('}') else {
            break;
        };
        names.push(&rest[open + 1..open + close]);
        rest = &rest[open + close + 1..];
    }
    names
}

/// The text of `key` with its placeholders filled. Each placeholder of the text must be given, and
/// nothing else (checked by the tests for every call site's key; in a debug build also here).
pub fn fill(lang: Lang, key: Key, args: &[(&str, &str)]) -> String {
    let template = text(lang, key);
    debug_assert!(
        {
            let mut wanted = placeholders(template);
            let mut given: Vec<&str> = args.iter().map(|(name, _)| *name).collect();
            wanted.sort_unstable();
            given.sort_unstable();
            wanted == given
        },
        "{key:?}: placeholders and arguments differ"
    );
    // One pass over the template: a value is never searched for placeholders itself.
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let Some(close) = rest[open..].find('}') else {
            break;
        };
        let name = &rest[open + 1..open + close];
        out.push_str(&rest[..open]);
        match args.iter().find(|(given, _)| *given == name) {
            Some((_, value)) => out.push_str(value),
            None => out.push_str(&rest[open..open + close + 1]),
        }
        rest = &rest[open + close + 1..];
    }
    out.push_str(rest);
    out
}

/// Shorthand for a text without placeholders.
pub fn t(lang: Lang, key: Key) -> String {
    fill(lang, key, &[])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hiragana, katakana, CJK ideographs and the full-width forms (（）：、。 and so on).
    fn is_japanese(c: char) -> bool {
        matches!(c,
            '\u{3000}'..='\u{303F}' | '\u{3040}'..='\u{30FF}' | '\u{4E00}'..='\u{9FFF}'
            | '\u{FF00}'..='\u{FFEF}')
    }

    /// The texts that are the same in both languages on purpose: a product name, an address, and
    /// the tooltip frames whose words are filled in from translated keys.
    const SAME_IN_BOTH: [Key; 4] = [
        Key::Title,
        Key::AddressRunning,
        Key::Tooltip,
        Key::TooltipRunning,
    ];

    #[test]
    fn every_key_has_both_languages_with_the_same_placeholders() {
        assert!(!Key::ALL.is_empty());
        for &key in Key::ALL {
            let (ja, en) = (text(Lang::Ja, key), text(Lang::En, key));
            assert!(!ja.trim().is_empty() && !en.trim().is_empty(), "{key:?}");
            let (mut a, mut b) = (placeholders(ja), placeholders(en));
            a.sort_unstable();
            b.sort_unstable();
            assert_eq!(a, b, "{key:?}");
            for name in a {
                assert!(
                    !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase()),
                    "{key:?}: {{{name}}}"
                );
            }
            // A brace is only ever a placeholder.
            assert_eq!(ja.matches('{').count(), ja.matches('}').count(), "{key:?}");
            assert_eq!(en.matches('{').count(), en.matches('}').count(), "{key:?}");
        }
    }

    #[test]
    fn english_holds_no_japanese_and_japanese_is_translated() {
        for &key in Key::ALL {
            let (ja, en) = (text(Lang::Ja, key), text(Lang::En, key));
            assert!(!en.chars().any(is_japanese), "{key:?}: {en}");
            if SAME_IN_BOTH.contains(&key) {
                assert_eq!(ja, en, "{key:?} is the same in both on purpose");
            } else {
                assert_ne!(ja, en, "{key:?} is not translated");
                assert!(ja.chars().any(is_japanese), "{key:?}: {ja}");
            }
        }
    }

    #[test]
    fn every_key_is_listed_once() {
        for (i, a) in Key::ALL.iter().enumerate() {
            assert!(!Key::ALL[i + 1..].contains(a), "{a:?}");
        }
    }

    #[test]
    fn japanese_is_chosen_only_for_japanese_and_everything_else_is_english() {
        assert_eq!(Lang::from_langid(Some(0x0411)), Lang::Ja); // ja-JP
        assert_eq!(Lang::from_langid(Some(0x0011)), Lang::Ja); // neutral Japanese
        for other in [
            0x0409u16, 0x0809, 0x0407, 0x040C, 0x0804, 0x0404, 0x0412, 0x0410,
        ] {
            assert_eq!(Lang::from_langid(Some(other)), Lang::En, "{other:#06x}");
        }
        assert_eq!(Lang::from_langid(None), Lang::En, "not determined");
        // 0x0011 masked off by a sublanguage still Japanese; another primary with the same low bits
        // is not (0x0111 is primary 0x111).
        assert_eq!(Lang::from_langid(Some(0x0111)), Lang::En);
    }

    #[test]
    #[cfg(windows)]
    fn the_detected_language_follows_this_machines_ui_language() {
        // SAFETY: as in `detect`.
        let id = unsafe { windows_sys::Win32::Globalization::GetUserDefaultUILanguage() };
        let expected = if id & 0x3ff == 0x11 {
            Lang::Ja
        } else {
            Lang::En
        };
        assert_eq!(Lang::detect(), expected);
    }

    #[test]
    fn fill_puts_each_value_in_its_place_and_leaves_no_placeholder() {
        assert_eq!(
            fill(Lang::En, Key::AddressPlanned, &[("port", "8765")]),
            "Will listen on 127.0.0.1:8765"
        );
        assert_eq!(
            fill(
                Lang::Ja,
                Key::VaultRefused,
                &[("code", "vault_publication_pending")]
            ),
            "Vault公開: 拒否（vault_publication_pending）"
        );
        // A value that looks like a placeholder is not filled again.
        assert_eq!(
            fill(
                Lang::En,
                Key::TooltipRunning,
                &[("state", "{address}"), ("address", "127.0.0.1:1")]
            ),
            "Memocampus MCP: {address} 127.0.0.1:1"
        );
    }

    #[test]
    #[should_panic(expected = "placeholders and arguments differ")]
    #[cfg(debug_assertions)]
    fn a_missing_argument_is_caught() {
        fill(Lang::En, Key::AddressPlanned, &[]);
    }

    #[test]
    fn placeholders_are_found_in_order() {
        assert_eq!(placeholders("a {state} b {address}"), ["state", "address"]);
        assert!(placeholders("no braces").is_empty());
    }
}
