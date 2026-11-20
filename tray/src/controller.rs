//! The tray's own operations — everything that is not "start/stop the server" — on a thread that is
//! not the UI thread: the two check-mark settings, the logon auto-start registration, reloading the
//! settings file and opening the settings file and the log folder. File and registry access happens
//! here, never in the message loop.
//!
//! Rules: a settings file that cannot be used is never overwritten (not by a toggle, not by a
//! reload); the registry is changed first and read back, and the settings are saved after, with the
//! registry put back to its exact previous value (absent, or the same text) if the save fails, and
//! the put-back is read back too: if it fails the state is reported as inconsistent, never as undone; nothing is restarted by
//! a settings change.

use crate::autostart::{self, Autostart, RunRegistry};
use crate::settings::{self, Settings, VaultMode, DEFAULT_PORT};
use crate::view::Action;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Opens a file or folder with its default application (Explorer, the user's editor).
pub trait Opener: Send {
    fn open(&mut self, path: &Path) -> io::Result<()>;
}

/// What the UI needs from the controller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerState {
    pub listen_port: u16,
    pub autostart_setting: bool,
    pub server_autostart_setting: bool,
    pub autostart: Autostart,
    /// A fixed code when the settings file cannot be used (the last valid settings stay in force).
    pub settings_invalid: Option<String>,
    /// The file is valid and differs, in a way that needs a restart, from what the server was
    /// started with.
    pub pending_restart: bool,
    /// A registry change could not be undone after its settings save failed: the logon entry and
    /// the setting disagree. Cleared by the next change that succeeds.
    pub registry_inconsistent: bool,
    pub last_error: Option<String>,
}

/// What the supervisor's settings source last did, shared with the controller and the UI (S2-04).
///
/// Every attempt to read the settings for a start bumps `generation`, whatever it found: the UI
/// compares generations, not values, so a restart that reads the very same settings as before (or
/// fails to read any) is still noticed. `settings` is what the server was started with; after an
/// attempt that failed nothing was started on guessed values, so it is `None` and no "restart
/// needed" is judged against an older start.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Applied {
    pub generation: u64,
    pub settings: Option<Settings>,
}

impl Applied {
    /// One attempt to read the settings for a start: `Some` if it succeeded.
    pub fn record(&mut self, loaded: Option<&Settings>) {
        self.generation += 1;
        self.settings = loaded.cloned();
    }
}

pub type SharedApplied = Arc<Mutex<Applied>>;

/// Remembers the last generation seen. `news` is true once per attempt that happened since.
#[derive(Debug, Default)]
pub struct AppliedWatch {
    seen: u64,
}

impl AppliedWatch {
    pub fn news(&mut self, applied: &SharedApplied) -> bool {
        let now = applied.lock().unwrap_or_else(|e| e.into_inner()).generation;
        let changed = now != self.seen;
        self.seen = now;
        changed
    }
}

pub struct Controller {
    app_dir: PathBuf,
    exe: PathBuf,
    registry: Box<dyn RunRegistry + Send>,
    opener: Box<dyn Opener>,
    /// The last valid settings: the ones in force. `None` when there has never been a valid file
    /// (a missing file counts as valid: the defaults).
    settings: Option<Settings>,
    invalid: Option<String>,
    /// What the server was last started with, written by the supervisor's settings source.
    applied: SharedApplied,
    autostart: Autostart,
    registry_inconsistent: bool,
    last_error: Option<String>,
}

/// The fields the server only reads when it starts.
fn server_fields(settings: &Settings) -> (u16, &[String], VaultMode, &Option<String>) {
    (
        settings.listen_port,
        &settings.allow_hosts,
        settings.vault_mode,
        &settings.pinned_vault_path,
    )
}

impl Controller {
    pub fn new(
        app_dir: PathBuf,
        exe: PathBuf,
        registry: Box<dyn RunRegistry + Send>,
        opener: Box<dyn Opener>,
        applied: SharedApplied,
    ) -> Self {
        let mut controller = Self {
            app_dir,
            exe,
            registry,
            opener,
            settings: None,
            invalid: None,
            applied,
            autostart: Autostart::Unreadable,
            registry_inconsistent: false,
            last_error: None,
        };
        controller.load();
        controller.autostart = autostart::status(controller.registry.as_ref(), &controller.exe);
        controller
    }

    fn load(&mut self) {
        match settings::load(&self.app_dir) {
            Ok(settings) => {
                self.settings = Some(settings);
                self.invalid = None;
            }
            // The previous valid settings (or none) stay; the file is not touched.
            Err(error) => self.invalid = Some(error.code()),
        }
    }

    pub fn state(&self) -> ControllerState {
        let in_force = self.settings.clone().unwrap_or_default();
        let pending_restart = match (
            &self.settings,
            &self
                .applied
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .settings,
        ) {
            (Some(file), Some(applied)) => server_fields(file) != server_fields(applied),
            _ => false,
        };
        ControllerState {
            listen_port: if self.settings.is_some() {
                in_force.listen_port
            } else {
                DEFAULT_PORT
            },
            autostart_setting: in_force.autostart,
            server_autostart_setting: in_force.server_autostart,
            autostart: self.autostart,
            settings_invalid: self.invalid.clone(),
            pending_restart,
            registry_inconsistent: self.registry_inconsistent,
            last_error: self.last_error.clone(),
        }
    }

    fn fail(&mut self, text: &str) {
        self.last_error = Some(text.into());
    }

    fn refresh_autostart(&mut self) {
        self.autostart = autostart::status(self.registry.as_ref(), &self.exe);
    }

    /// Changes the Run value to on or off. Returns the exact value it had before (for a restore),
    /// or `Err` after reporting why nothing was changed. If the change itself fails half way, the
    /// previous value is put back here; if even that fails the state is reported as inconsistent.
    fn change_registry(&mut self, enable: bool, failure: &str) -> Result<Option<String>, ()> {
        // What cannot be read cannot be restored, so nothing is changed.
        let Ok(before) = self.registry.run_value() else {
            self.refresh_autostart();
            self.fail("ログオン時の自動起動の登録を読み取れないため変更しません");
            return Err(());
        };
        let changed = if enable {
            autostart::enable(self.registry.as_ref(), &self.exe)
        } else {
            autostart::disable(self.registry.as_ref(), &self.exe)
        };
        if changed.is_ok() {
            return Ok(before);
        }
        // A failure may still have changed something (a write that did not read back): look.
        if self.registry.run_value().ok().as_ref() != Some(&before) && !self.restore(&before) {
            self.mark_inconsistent();
            return Err(());
        }
        self.refresh_autostart();
        self.fail(failure);
        Err(())
    }

    /// Puts the Run value back exactly as it was (absent, or the same text), and reads it back.
    fn restore(&self, before: &Option<String>) -> bool {
        let done = match before {
            None => self.registry.delete_run_value(),
            Some(value) => self.registry.set_run_value(value),
        };
        done.is_ok() && matches!(self.registry.run_value(), Ok(now) if now == *before)
    }

    fn mark_inconsistent(&mut self) {
        self.registry_inconsistent = true;
        self.refresh_autostart();
        self.fail(
            "自動起動の登録を元に戻せず、設定と登録が一致していません（レジストリのMemocampusTrayを確認してください）",
        );
    }

    /// The settings could not be saved after the registry was changed: undo the registry change.
    /// Only a verified restore may say "undone".
    fn undo_after_failed_save(&mut self, before: &Option<String>) {
        if self.restore(before) {
            self.refresh_autostart();
            self.fail("設定ファイルを保存できませんでした（変更を取り消しました）");
        } else {
            self.mark_inconsistent();
        }
    }

    /// The first start of a normal (not test) tray: the settings file does not exist yet, so the
    /// defaults apply — and the default is "start at logon". Registers it and writes the file, once;
    /// if the file cannot be written the registration is undone, so the two stay in step. Never
    /// called for an isolated, read-only or test run.
    pub fn first_run(&mut self) {
        if settings::path(&self.app_dir).exists() {
            return;
        }
        let defaults = Settings::default();
        let before = if defaults.autostart {
            match self.change_registry(true, "ログオン時の自動起動を登録できませんでした")
            {
                Ok(before) => Some(before),
                // Nothing was saved either, so the next start tries again.
                Err(()) => return,
            }
        } else {
            None
        };
        if settings::save(&self.app_dir, &defaults).is_err() {
            match before {
                Some(before) => self.undo_after_failed_save(&before),
                None => self.fail("設定ファイルを作成できませんでした"),
            }
            return;
        }
        self.registry_inconsistent = false;
        self.load();
        self.refresh_autostart();
    }

    fn settings_for_change(&mut self) -> Option<Settings> {
        if self.invalid.is_some() {
            self.fail("設定ファイルが不正なため変更できません（ファイルは変更していません）");
            return None;
        }
        Some(self.settings.clone().unwrap_or_default())
    }

    pub fn toggle_autostart(&mut self) {
        let Some(mut next) = self.settings_for_change() else {
            return;
        };
        next.autostart = !next.autostart;
        let Ok(before) = self.change_registry(
            next.autostart,
            "ログオン時の自動起動を変更できませんでした（レジストリ）",
        ) else {
            return;
        };
        if settings::save(&self.app_dir, &next).is_err() {
            self.undo_after_failed_save(&before);
            return;
        }
        self.settings = Some(next);
        self.last_error = None;
        self.registry_inconsistent = false;
        self.refresh_autostart();
    }

    pub fn toggle_server_autostart(&mut self) {
        let Some(mut next) = self.settings_for_change() else {
            return;
        };
        next.server_autostart = !next.server_autostart;
        match settings::save(&self.app_dir, &next) {
            Ok(()) => {
                self.settings = Some(next);
                self.last_error = None;
            }
            Err(_) => self.fail("設定ファイルを保存できませんでした"),
        }
    }

    /// The user asked to point the Run value at this executable (it was moved). Only when the
    /// value is registered for another path: nothing else is ever rewritten.
    pub fn repair_autostart(&mut self) {
        self.refresh_autostart();
        if !matches!(self.autostart, Autostart::PathMismatch(_)) {
            return;
        }
        if self
            .change_registry(true, "自動起動の登録を更新できませんでした")
            .is_ok()
        {
            self.last_error = None;
            self.registry_inconsistent = false;
        }
        self.refresh_autostart();
    }

    /// Reads the settings file again without touching the last error: a server start happened, so
    /// what "restart needed" is judged against changed. Nothing is written and nothing restarted.
    pub fn resync(&mut self) {
        self.load();
        self.refresh_autostart();
    }

    /// One operation from the menu. The write operations are finished (saved, or restored and
    /// verified) before this returns: nothing here is interrupted half way.
    pub fn perform(&mut self, action: Action) {
        match action {
            Action::ToggleAutostart => self.toggle_autostart(),
            Action::ToggleServerAutostart => self.toggle_server_autostart(),
            Action::RepairAutostart => self.repair_autostart(),
            Action::OpenSettings => self.open_settings(),
            Action::Reload => self.reload(),
            Action::OpenLogs => self.open_logs(),
            Action::Start | Action::Stop | Action::Restart | Action::Quit => {}
        }
    }

    pub fn reload(&mut self) {
        self.load();
        self.refresh_autostart();
        if self.invalid.is_none() {
            self.last_error = None;
        }
    }

    pub fn open_settings(&mut self) {
        let file = settings::path(&self.app_dir);
        if !file.exists() {
            // Only a file that does not exist is created (with the defaults) so there is one to edit.
            if settings::save(&self.app_dir, &Settings::default()).is_err() {
                return self.fail("設定ファイルを作成できませんでした");
            }
            self.load();
        }
        // An invalid file is opened as it is, for the user to fix.
        match self.opener.open(&file) {
            Ok(()) => self.last_error = None,
            Err(_) => self.fail("設定ファイルを開けませんでした"),
        }
    }

    pub fn open_logs(&mut self) {
        let logs = self.app_dir.join("logs");
        if !logs.is_dir() {
            return self.fail("ログフォルダがまだありません");
        }
        match self.opener.open(&logs) {
            Ok(()) => self.last_error = None,
            Err(_) => self.fail("ログフォルダを開けませんでした"),
        }
    }

    /// The error of the settings file as a code, for the log.
    pub fn settings_error_code(&self) -> Option<&str> {
        self.invalid.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autostart::{Approval, RunRegistry};
    use std::fs;
    use std::sync::Mutex as StdMutex;

    /// `Controller` needs `Send`; a registry shared with the test is wrapped in a mutex. Faults can
    /// be injected per operation: the Nth write (set or delete) fails, the Nth read lies.
    /// The read with this number (counted from 1) returns this value instead of the truth.
    type Lie = (usize, Option<String>);

    #[derive(Default, Clone)]
    struct Shared {
        value: Arc<StdMutex<Option<String>>>,
        approved: Arc<StdMutex<Option<Vec<u8>>>>,
        fail_writes: Arc<StdMutex<bool>>,
        writes: Arc<StdMutex<usize>>,
        failing_writes: Arc<StdMutex<Vec<usize>>>,
        reads: Arc<StdMutex<usize>>,
        lying_reads: Arc<StdMutex<Vec<Lie>>>,
        /// Another application's entry: nothing here may ever touch it.
        other: Arc<StdMutex<Option<String>>>,
    }
    impl Shared {
        /// The `n`th write from now on fails (1 = the next).
        fn fail_write(&self, n: usize) {
            let at = *self.writes.lock().unwrap() + n;
            self.failing_writes.lock().unwrap().push(at);
        }
        /// The `n`th read from now on returns `value` instead of the truth (1 = the next).
        fn lie_on_read(&self, n: usize, value: Option<&str>) {
            let at = *self.reads.lock().unwrap() + n;
            self.lying_reads
                .lock()
                .unwrap()
                .push((at, value.map(str::to_owned)));
        }
        fn write_fails(&self) -> bool {
            let mut writes = self.writes.lock().unwrap();
            *writes += 1;
            *self.fail_writes.lock().unwrap()
                || self.failing_writes.lock().unwrap().contains(&*writes)
        }
    }
    impl RunRegistry for Shared {
        fn run_value(&self) -> io::Result<Option<String>> {
            let mut reads = self.reads.lock().unwrap();
            *reads += 1;
            if let Some((_, lie)) = self
                .lying_reads
                .lock()
                .unwrap()
                .iter()
                .find(|(at, _)| at == &*reads)
            {
                return Ok(lie.clone());
            }
            Ok(self.value.lock().unwrap().clone())
        }
        fn set_run_value(&self, value: &str) -> io::Result<()> {
            if self.write_fails() {
                return Err(io::Error::other("write"));
            }
            *self.value.lock().unwrap() = Some(value.to_owned());
            Ok(())
        }
        fn delete_run_value(&self) -> io::Result<()> {
            if self.write_fails() {
                return Err(io::Error::other("delete"));
            }
            *self.value.lock().unwrap() = None;
            Ok(())
        }
        fn approved_bytes(&self) -> io::Result<Option<Vec<u8>>> {
            Ok(self.approved.lock().unwrap().clone())
        }
    }

    #[derive(Clone, Default)]
    struct Opened(Arc<StdMutex<Vec<PathBuf>>>);
    impl Opener for Opened {
        fn open(&mut self, path: &Path) -> io::Result<()> {
            self.0.lock().unwrap().push(path.to_path_buf());
            Ok(())
        }
    }

    struct Rig {
        dir: PathBuf,
        registry: Shared,
        opened: Opened,
        applied: SharedApplied,
        controller: Controller,
    }
    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn exe() -> PathBuf {
        PathBuf::from(r"C:\Program Files\Memocampus\memocampus-tray.exe")
    }

    fn rig(name: &str) -> Rig {
        let dir = std::env::temp_dir().join(format!(
            "memocampus-controller-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let registry = Shared::default();
        let opened = Opened::default();
        let applied: SharedApplied = Arc::default();
        let controller = Controller::new(
            dir.clone(),
            exe(),
            Box::new(registry.clone()),
            Box::new(opened.clone()),
            Arc::clone(&applied),
        );
        Rig {
            dir,
            registry,
            opened,
            applied,
            controller,
        }
    }

    fn write_settings(rig: &Rig, settings: &Settings) {
        fs::write(
            settings::path(&rig.dir),
            serde_json::to_vec_pretty(settings).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn without_a_file_the_defaults_are_shown_and_nothing_is_written_or_registered() {
        let rig = rig("fresh");
        let state = rig.controller.state();
        assert_eq!(state.listen_port, 8765);
        assert!(state.autostart_setting && state.server_autostart_setting);
        assert_eq!(state.autostart, Autostart::NotRegistered);
        assert!(
            !settings::path(&rig.dir).exists(),
            "looking creates nothing"
        );
        assert!(rig.registry.value.lock().unwrap().is_none());
    }

    #[test]
    fn the_first_normal_run_registers_the_default_and_writes_the_file_once() {
        let mut rig = rig("first");
        rig.controller.first_run();
        assert!(settings::path(&rig.dir).exists());
        assert_eq!(
            rig.controller.state().autostart,
            Autostart::Registered(Approval::Enabled)
        );
        // A second time changes nothing, even if the user has since turned it off.
        rig.controller.toggle_autostart();
        assert_eq!(rig.controller.state().autostart, Autostart::NotRegistered);
        rig.controller.first_run();
        assert_eq!(rig.controller.state().autostart, Autostart::NotRegistered);
    }

    #[test]
    fn toggling_auto_start_changes_the_registry_and_the_file_together() {
        let mut rig = rig("toggle");
        write_settings(
            &rig,
            &Settings {
                autostart: false,
                ..Settings::default()
            },
        );
        rig.controller.reload();
        assert!(!rig.controller.state().autostart_setting);
        rig.controller.toggle_autostart();
        let state = rig.controller.state();
        assert!(state.autostart_setting);
        assert_eq!(state.autostart, Autostart::Registered(Approval::Enabled));
        assert!(settings::load(&rig.dir).unwrap().autostart);
        assert_eq!(
            rig.registry.value.lock().unwrap().as_deref(),
            Some("\"C:\\Program Files\\Memocampus\\memocampus-tray.exe\"")
        );
        rig.controller.toggle_autostart();
        assert!(!settings::load(&rig.dir).unwrap().autostart);
        assert!(rig.registry.value.lock().unwrap().is_none());
        assert_eq!(rig.controller.state().last_error, None);
    }

    #[test]
    fn a_registry_failure_changes_nothing_and_is_reported() {
        let mut rig = rig("regfail");
        write_settings(
            &rig,
            &Settings {
                autostart: false,
                ..Settings::default()
            },
        );
        rig.controller.reload();
        *rig.registry.fail_writes.lock().unwrap() = true;
        rig.controller.toggle_autostart();
        let state = rig.controller.state();
        assert!(!state.autostart_setting, "the setting did not change");
        assert!(!settings::load(&rig.dir).unwrap().autostart);
        assert!(state.last_error.is_some());
    }

    #[test]
    fn a_failed_save_puts_the_registry_back() {
        let mut rig = rig("savefail");
        write_settings(
            &rig,
            &Settings {
                autostart: false,
                ..Settings::default()
            },
        );
        rig.controller.reload();
        // The file turns into something the tray will not overwrite, behind the controller's back.
        fs::write(settings::path(&rig.dir), "{\"version\":2}").unwrap();
        rig.controller.toggle_autostart();
        assert!(
            rig.registry.value.lock().unwrap().is_none(),
            "registered, then undone"
        );
        assert!(rig.controller.state().last_error.is_some());
        assert_eq!(
            fs::read_to_string(settings::path(&rig.dir)).unwrap(),
            "{\"version\":2}"
        );
    }

    #[test]
    fn an_invalid_file_is_never_overwritten_by_a_toggle_or_a_reload() {
        let mut rig = rig("invalid");
        fs::write(settings::path(&rig.dir), "{ not json").unwrap();
        rig.controller.reload();
        let state = rig.controller.state();
        assert_eq!(
            state.settings_invalid.as_deref(),
            Some("settings_invalid_json")
        );
        rig.controller.toggle_autostart();
        rig.controller.toggle_server_autostart();
        assert_eq!(
            fs::read_to_string(settings::path(&rig.dir)).unwrap(),
            "{ not json"
        );
        assert!(
            rig.registry.value.lock().unwrap().is_none(),
            "no registry change either"
        );
        assert!(rig.controller.state().last_error.is_some());
    }

    #[test]
    fn a_reload_keeps_the_previous_valid_settings_when_the_file_turns_invalid() {
        let mut rig = rig("reload");
        write_settings(
            &rig,
            &Settings {
                listen_port: 9100,
                ..Settings::default()
            },
        );
        rig.controller.reload();
        assert_eq!(rig.controller.state().listen_port, 9100);
        fs::write(settings::path(&rig.dir), "garbage").unwrap();
        rig.controller.reload();
        let state = rig.controller.state();
        assert_eq!(
            state.listen_port, 9100,
            "the last valid settings stay in force"
        );
        assert!(state.settings_invalid.is_some());
        write_settings(
            &rig,
            &Settings {
                listen_port: 9200,
                ..Settings::default()
            },
        );
        rig.controller.reload();
        let state = rig.controller.state();
        assert_eq!(state.listen_port, 9200);
        assert_eq!(state.settings_invalid, None);
    }

    #[test]
    fn a_change_that_needs_a_restart_is_flagged_and_nothing_is_restarted() {
        let mut rig = rig("pending");
        write_settings(&rig, &Settings::default());
        rig.controller.reload();
        rig.applied
            .lock()
            .unwrap()
            .record(Some(&Settings::default()));
        assert!(!rig.controller.state().pending_restart);
        write_settings(
            &rig,
            &Settings {
                listen_port: 8080,
                ..Settings::default()
            },
        );
        rig.controller.reload();
        assert!(
            rig.controller.state().pending_restart,
            "the running server still uses 8765"
        );
        // The two toggles are not read by the server, so they never ask for a restart.
        write_settings(&rig, &Settings::default());
        rig.controller.reload();
        rig.controller.toggle_server_autostart();
        assert!(!rig.controller.state().pending_restart);
        assert!(!rig.controller.state().server_autostart_setting);
    }

    #[test]
    fn a_moved_executable_is_repaired_only_when_asked() {
        let mut rig = rig("repair");
        *rig.registry.value.lock().unwrap() = Some("\"D:\\old\\memocampus-tray.exe\"".into());
        rig.controller.reload();
        assert_eq!(
            rig.controller.state().autostart,
            Autostart::PathMismatch(Approval::Enabled)
        );
        assert_eq!(
            rig.registry.value.lock().unwrap().as_deref(),
            Some("\"D:\\old\\memocampus-tray.exe\""),
            "starting and reloading do not rewrite it"
        );
        rig.controller.repair_autostart();
        assert_eq!(
            rig.controller.state().autostart,
            Autostart::Registered(Approval::Enabled)
        );
        // Not a mismatch: nothing to repair, nothing written.
        *rig.registry.value.lock().unwrap() = None;
        rig.controller.repair_autostart();
        assert!(rig.registry.value.lock().unwrap().is_none());
    }

    #[test]
    fn a_choice_made_in_windows_is_respected_when_toggling() {
        let mut rig = rig("windows");
        *rig.registry.approved.lock().unwrap() = Some(vec![3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        write_settings(
            &rig,
            &Settings {
                autostart: false,
                ..Settings::default()
            },
        );
        rig.controller.reload();
        rig.controller.toggle_autostart();
        let state = rig.controller.state();
        assert!(state.autostart_setting);
        assert_eq!(
            state.autostart,
            Autostart::Registered(Approval::DisabledByWindows)
        );
        assert_eq!(
            rig.registry.approved.lock().unwrap().as_deref(),
            Some(&[3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0][..])
        );
    }

    #[test]
    fn the_settings_and_log_openers_create_only_what_is_missing_and_open_what_is_there() {
        let mut rig = rig("open");
        // No settings file: one with the defaults is created, then opened.
        rig.controller.open_settings();
        assert_eq!(settings::load(&rig.dir).unwrap(), Settings::default());
        assert_eq!(*rig.opened.0.lock().unwrap(), [settings::path(&rig.dir)]);
        // An invalid file is opened as it is.
        fs::write(settings::path(&rig.dir), "broken").unwrap();
        rig.controller.open_settings();
        assert_eq!(
            fs::read_to_string(settings::path(&rig.dir)).unwrap(),
            "broken"
        );
        assert_eq!(rig.opened.0.lock().unwrap().len(), 2);
        // No log folder yet: said, and nothing is created.
        rig.controller.open_logs();
        assert!(rig.controller.state().last_error.is_some());
        assert!(!rig.dir.join("logs").exists());
        fs::create_dir(rig.dir.join("logs")).unwrap();
        rig.controller.open_logs();
        assert_eq!(
            rig.opened.0.lock().unwrap().last(),
            Some(&rig.dir.join("logs"))
        );
        assert_eq!(rig.controller.state().last_error, None);
    }

    const OLD: &str = "\"D:\\old\\memocampus-tray.exe\"";
    const OURS: &str = "\"C:\\Program Files\\Memocampus\\memocampus-tray.exe\"";

    /// A rig whose settings file says `autostart` and whose Run value is `registry`.
    fn rig_with(name: &str, autostart: bool, registry: Option<&str>) -> Rig {
        let mut rig = rig(name);
        write_settings(
            &rig,
            &Settings {
                autostart,
                ..Settings::default()
            },
        );
        *rig.registry.value.lock().unwrap() = registry.map(str::to_owned);
        *rig.registry.other.lock().unwrap() = Some("other app".into());
        rig.controller.reload();
        rig
    }

    /// Makes the next save fail: the file turns into one the tray will not overwrite.
    fn break_the_settings_file(rig: &Rig) {
        fs::write(settings::path(&rig.dir), "{\"version\":2}").unwrap();
    }

    fn untouched_other(rig: &Rig) {
        assert_eq!(
            rig.registry.other.lock().unwrap().as_deref(),
            Some("other app"),
            "another application's entry"
        );
    }

    #[test]
    fn a_failed_save_restores_the_exact_previous_value_not_a_fresh_registration() {
        // Registered for another path (an old location); the user switches it off; the save fails.
        let mut rig = rig_with("exact", true, Some(OLD));
        break_the_settings_file(&rig);
        rig.controller.toggle_autostart();
        assert_eq!(
            rig.registry.value.lock().unwrap().as_deref(),
            Some(OLD),
            "the old value is back, not this executable's"
        );
        let state = rig.controller.state();
        assert!(!state.registry_inconsistent);
        assert!(state.last_error.unwrap().contains("取り消しました"));
        assert_eq!(
            state.autostart,
            Autostart::PathMismatch(Approval::Enabled),
            "re-read from the registry"
        );
        untouched_other(&rig);
    }

    #[test]
    fn a_failed_save_after_registering_returns_to_not_registered() {
        let mut rig = rig_with("fresh-reg", false, None);
        break_the_settings_file(&rig);
        rig.controller.toggle_autostart();
        assert_eq!(*rig.registry.value.lock().unwrap(), None);
        let state = rig.controller.state();
        assert!(!state.registry_inconsistent);
        assert!(state.last_error.unwrap().contains("取り消しました"));
        assert_eq!(state.autostart, Autostart::NotRegistered);
        untouched_other(&rig);
    }

    #[test]
    fn a_failed_save_after_unregistering_puts_our_own_value_back() {
        let mut rig = rig_with("unreg", true, Some(OURS));
        break_the_settings_file(&rig);
        rig.controller.toggle_autostart();
        assert_eq!(rig.registry.value.lock().unwrap().as_deref(), Some(OURS));
        assert!(!rig.controller.state().registry_inconsistent);
    }

    #[test]
    fn a_restore_that_fails_is_reported_as_inconsistent_never_as_undone() {
        let mut rig = rig_with("restorefail", false, None);
        break_the_settings_file(&rig);
        // Write 1 registers, the save fails, write 2 (the restore) fails.
        rig.registry.fail_write(2);
        rig.controller.toggle_autostart();
        let state = rig.controller.state();
        assert!(state.registry_inconsistent);
        let error = state.last_error.unwrap();
        assert!(!error.contains("取り消しました"), "{error}");
        assert!(error.contains("一致していません"), "{error}");
        assert_eq!(
            rig.registry.value.lock().unwrap().as_deref(),
            Some(OURS),
            "the registration is still there: said so, not hidden"
        );
        assert_eq!(
            state.autostart,
            Autostart::Registered(Approval::Enabled),
            "the status shows the truth"
        );
        untouched_other(&rig);
    }

    #[test]
    fn a_restore_whose_read_back_disagrees_is_reported_as_inconsistent() {
        let mut rig = rig_with("restorelie", false, None);
        break_the_settings_file(&rig);
        // Reads in this toggle: 1 before, 2 the registration's read-back, 3 the restore's read-back.
        rig.registry.lie_on_read(3, Some("something else"));
        rig.controller.toggle_autostart();
        let state = rig.controller.state();
        assert!(state.registry_inconsistent);
        assert!(!state.last_error.unwrap().contains("取り消しました"));
    }

    #[test]
    fn a_change_that_fails_leaves_the_previous_value_and_says_so() {
        let mut rig = rig_with("changefail", false, Some(OLD));
        rig.registry.fail_write(1);
        rig.controller.toggle_autostart();
        let state = rig.controller.state();
        assert!(!state.autostart_setting, "the setting did not change");
        assert_eq!(rig.registry.value.lock().unwrap().as_deref(), Some(OLD));
        assert!(state.last_error.is_some() && !state.registry_inconsistent);
        assert!(!settings::load(&rig.dir).unwrap().autostart);
        untouched_other(&rig);
    }

    #[test]
    fn a_registration_that_does_not_read_back_is_undone_and_if_that_fails_flagged() {
        // The write worked but the read-back says it is not there: the change is an error, and the
        // value that did get written is put back to what it was.
        let mut rig = rig_with("noreadback", false, None);
        rig.registry.lie_on_read(2, None);
        rig.controller.toggle_autostart();
        let state = rig.controller.state();
        assert_eq!(*rig.registry.value.lock().unwrap(), None);
        assert!(state.last_error.is_some() && !state.registry_inconsistent);
        assert!(!state.autostart_setting);

        // And when the put-back fails as well.
        let mut rig = rig_with("noreadback2", false, None);
        rig.registry.lie_on_read(2, None);
        rig.registry.fail_write(2);
        rig.controller.toggle_autostart();
        let state = rig.controller.state();
        assert!(state.registry_inconsistent);
        assert!(!state.last_error.unwrap().contains("取り消しました"));
        assert!(!state.autostart_setting, "the setting still says off");
    }

    #[test]
    fn a_failed_first_run_save_undoes_the_registration() {
        // The application directory is a file: the settings file cannot be created.
        let mut rig = rig("firstfail");
        fs::remove_dir_all(&rig.dir).unwrap();
        fs::write(&rig.dir, "not a directory").unwrap();
        rig.controller.first_run();
        let state = rig.controller.state();
        let registered = rig.registry.value.lock().unwrap().clone();
        let _ = fs::remove_file(&rig.dir);
        fs::create_dir_all(&rig.dir).unwrap();
        assert_eq!(registered, None, "undone");
        assert!(!state.registry_inconsistent);
        assert!(state.last_error.unwrap().contains("取り消しました"));
    }

    #[test]
    fn a_first_run_registration_that_fails_writes_no_settings_and_changes_nothing() {
        let mut rig = rig("firstregfail");
        rig.registry.fail_write(1);
        rig.controller.first_run();
        assert!(
            !settings::path(&rig.dir).exists(),
            "retried at the next start"
        );
        assert_eq!(*rig.registry.value.lock().unwrap(), None);
        assert!(rig.controller.state().last_error.is_some());
    }

    #[test]
    fn a_success_clears_an_earlier_inconsistency() {
        let mut rig = rig_with("heal", false, None);
        break_the_settings_file(&rig);
        rig.registry.fail_write(2);
        rig.controller.toggle_autostart();
        assert!(rig.controller.state().registry_inconsistent);
        // The user fixes the file and the registry value is still ours: toggling on now agrees.
        write_settings(
            &rig,
            &Settings {
                autostart: false,
                ..Settings::default()
            },
        );
        rig.controller.reload();
        rig.controller.toggle_autostart();
        let state = rig.controller.state();
        assert!(!state.registry_inconsistent && state.autostart_setting);
        assert_eq!(state.last_error, None);
    }

    #[test]
    fn the_restart_flag_follows_what_the_server_was_started_with() {
        // S2-04: the file differs while the server runs with the old values: flagged. After a
        // restart the server was started with the file's values, so the flag is recomputed away.
        let mut rig = rig("afterrestart");
        write_settings(&rig, &Settings::default());
        rig.controller.reload();
        rig.applied
            .lock()
            .unwrap()
            .record(Some(&Settings::default()));
        let edited = Settings {
            listen_port: 8123,
            allow_hosts: vec!["muse.example.ts.net".into()],
            ..Settings::default()
        };
        write_settings(&rig, &edited);
        rig.controller.reload();
        assert!(
            rig.controller.state().pending_restart,
            "right after the reload"
        );
        // The restart: the supervisor's settings source recorded what it started with.
        rig.applied.lock().unwrap().record(Some(&edited));
        rig.controller.reload();
        assert!(
            !rig.controller.state().pending_restart,
            "after the restart completed"
        );
    }

    fn with_port(port: u16) -> Settings {
        Settings {
            listen_port: port,
            ..Settings::default()
        }
    }

    /// What the UI does on a snapshot: look at the generation and, if a start happened, resync.
    fn ui_step(rig: &mut Rig, watch: &mut AppliedWatch) {
        if watch.news(&rig.applied) {
            rig.controller.resync();
        }
    }

    #[test]
    fn a_to_b_to_a_without_a_reload_clears_the_stale_notice_after_a_restart() {
        // S2-04 (second review): started with A, the file becomes B and is reloaded (notice), the
        // file goes back to A and is NOT reloaded, then a restart. The applied value is A both
        // before and after, so comparing values sees nothing; the generation does.
        let mut rig = rig("aba");
        let mut watch = AppliedWatch::default();
        write_settings(&rig, &with_port(8000)); // A
        rig.controller.reload();
        rig.applied.lock().unwrap().record(Some(&with_port(8000)));
        ui_step(&mut rig, &mut watch);
        write_settings(&rig, &with_port(9000)); // B
        rig.controller.reload();
        assert!(rig.controller.state().pending_restart);
        assert_eq!(rig.controller.state().listen_port, 9000);
        write_settings(&rig, &with_port(8000)); // back to A, not reloaded
        assert!(rig.controller.state().pending_restart, "stale, still B");
        // The restart reads A again.
        rig.applied.lock().unwrap().record(Some(&with_port(8000)));
        ui_step(&mut rig, &mut watch);
        let state = rig.controller.state();
        assert!(!state.pending_restart, "no notice: the server runs with A");
        assert_eq!(state.listen_port, 8000, "and the tray shows A");
    }

    #[test]
    fn a_restart_that_read_the_same_settings_is_still_noticed() {
        let mut watch = AppliedWatch::default();
        let applied: SharedApplied = Arc::default();
        assert!(!watch.news(&applied), "nothing has happened yet");
        applied.lock().unwrap().record(Some(&Settings::default()));
        assert!(watch.news(&applied));
        assert!(!watch.news(&applied), "once per attempt");
        applied.lock().unwrap().record(Some(&Settings::default()));
        assert!(watch.news(&applied), "same value, new attempt");
    }

    #[test]
    fn a_failed_read_at_start_is_not_shown_as_applied() {
        // The server was started with 8000; the file is then broken and a restart refuses to start
        // (fail closed): nothing runs with the old values on the tray's say-so, no notice is
        // judged against them, and the invalid file is reported.
        let mut rig = rig("readfail");
        let mut watch = AppliedWatch::default();
        write_settings(&rig, &with_port(8000));
        rig.controller.reload();
        rig.applied.lock().unwrap().record(Some(&with_port(8000)));
        ui_step(&mut rig, &mut watch);
        fs::write(settings::path(&rig.dir), "garbage").unwrap();
        rig.applied.lock().unwrap().record(None); // the attempt that failed
        ui_step(&mut rig, &mut watch);
        let state = rig.controller.state();
        assert!(state.settings_invalid.is_some(), "the file is reported");
        assert!(!state.pending_restart, "no start is claimed to be applied");
        assert_eq!(
            fs::read_to_string(settings::path(&rig.dir)).unwrap(),
            "garbage",
            "the file is not touched"
        );
        // Fixed and started again: applied, nothing pending.
        write_settings(&rig, &with_port(8100));
        rig.applied.lock().unwrap().record(Some(&with_port(8100)));
        ui_step(&mut rig, &mut watch);
        let state = rig.controller.state();
        assert_eq!(state.settings_invalid, None);
        assert!(!state.pending_restart);
        assert_eq!(state.listen_port, 8100);
    }

    #[test]
    fn a_resync_does_not_clear_an_error_and_writes_nothing() {
        let mut rig = rig("resync");
        write_settings(&rig, &Settings::default());
        rig.controller.reload();
        let before = fs::read_to_string(settings::path(&rig.dir)).unwrap();
        rig.controller.open_logs(); // no log folder: an error is shown
        assert!(rig.controller.state().last_error.is_some());
        rig.controller.resync();
        assert!(rig.controller.state().last_error.is_some());
        assert_eq!(
            fs::read_to_string(settings::path(&rig.dir)).unwrap(),
            before
        );
    }
}
