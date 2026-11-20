//! `tray-settings.json`: the tray's own settings, in the application directory beside `config.json`
//! and `policy.json`, which the tray never reads for its own use and never writes.
//!
//! Fail closed (Issue #67): a file that is missing is the only case that yields the defaults. A file
//! that cannot be read, is not valid, has an unknown field, or names a version this build does not
//! know is an error, and the server is not started on guessed values. A save never overwrites such a
//! file either.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = "tray-settings.json";
pub const VERSION: u64 = 1;
/// The same default as `memocampus-mcp --transport streamable-http`.
pub const DEFAULT_PORT: u16 = 8765;
/// `memocampus-mcp` takes at most this many `--allow-host` values.
pub const MAX_ALLOW_HOSTS: usize = 8;
const MAX_FILE_BYTES: u64 = 64 * 1024;
const MAX_HOST_CHARS: usize = 260;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VaultMode {
    /// No `--vault`: the server follows the active vault the Native Host's `config.json` names.
    FollowActive,
    /// `--vault <path>`. The server still reads that file only while it is the active, confirmed
    /// vault: this setting does not select a vault and cannot widen what the server may read.
    Pinned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub version: u64,
    pub autostart: bool,
    pub server_autostart: bool,
    pub listen_port: u16,
    pub allow_hosts: Vec<String>,
    pub vault_mode: VaultMode,
    pub pinned_vault_path: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: VERSION,
            autostart: true,
            server_autostart: true,
            listen_port: DEFAULT_PORT,
            allow_hosts: Vec::new(),
            vault_mode: VaultMode::FollowActive,
            pinned_vault_path: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsError {
    /// The file or its directory cannot be read now.
    Io,
    TooLarge,
    /// Not JSON, a wrong type, an unknown field, or a value that is out of range.
    Invalid(&'static str),
    /// A `version` other than the one this build writes. Never rewritten.
    UnknownVersion(u64),
    /// A save refused because the file there is not a settings file this build understands.
    WouldOverwrite,
}

impl SettingsError {
    /// A fixed code for the log; never a path or a value from the file.
    pub fn code(&self) -> String {
        match self {
            Self::Io => "settings_io".into(),
            Self::TooLarge => "settings_too_large".into(),
            Self::Invalid(why) => format!("settings_invalid_{why}"),
            Self::UnknownVersion(_) => "settings_unknown_version".into(),
            Self::WouldOverwrite => "settings_would_overwrite".into(),
        }
    }
}

/// The application directory, as `memocampus-mcp` and the Native Host resolve it:
/// `MEMOCAMPUS_APP_DIR`, else `%LOCALAPPDATA%\Memocampus`; only an absolute path.
pub fn app_dir() -> Option<PathBuf> {
    let non_empty = |name: &str| {
        std::env::var_os(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    let dir = non_empty("MEMOCAMPUS_APP_DIR")
        .or_else(|| non_empty("LOCALAPPDATA").map(|base| base.join("Memocampus")))?;
    dir.is_absolute().then_some(dir)
}

pub fn path(app_dir: &Path) -> PathBuf {
    app_dir.join(FILE_NAME)
}

impl Settings {
    pub fn validate(&self) -> Result<(), SettingsError> {
        if self.version != VERSION {
            return Err(SettingsError::UnknownVersion(self.version));
        }
        if self.listen_port == 0 {
            return Err(SettingsError::Invalid("listen_port"));
        }
        if self.allow_hosts.len() > MAX_ALLOW_HOSTS {
            return Err(SettingsError::Invalid("allow_hosts"));
        }
        for (at, host) in self.allow_hosts.iter().enumerate() {
            // Only what could break the command line or confuse the log. Whether it is an acceptable
            // `<dns name>:<port>` is for `memocampus-mcp` to decide (one rule, one place).
            let sane = !host.is_empty()
                && host.chars().count() <= MAX_HOST_CHARS
                && host.chars().all(|c| c.is_ascii_graphic())
                && !host.starts_with('-')
                && !self.allow_hosts[..at].contains(host);
            if !sane {
                return Err(SettingsError::Invalid("allow_hosts"));
            }
        }
        match (self.vault_mode, &self.pinned_vault_path) {
            (VaultMode::FollowActive, None) => Ok(()),
            (VaultMode::FollowActive, Some(_)) => Err(SettingsError::Invalid("pinned_vault_path")),
            (VaultMode::Pinned, Some(file))
                if Path::new(file).is_absolute() && !file.contains('\0') =>
            {
                Ok(())
            }
            (VaultMode::Pinned, _) => Err(SettingsError::Invalid("pinned_vault_path")),
        }
    }

    /// Parses and validates the bytes of a settings file.
    pub fn parse(bytes: &[u8]) -> Result<Self, SettingsError> {
        let value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|_| SettingsError::Invalid("json"))?;
        // The version first, so a newer file is reported as such rather than as "unknown field".
        match value.get("version").and_then(serde_json::Value::as_u64) {
            Some(VERSION) => {}
            Some(other) => return Err(SettingsError::UnknownVersion(other)),
            None => return Err(SettingsError::Invalid("version")),
        }
        let settings: Self =
            serde_json::from_value(value).map_err(|_| SettingsError::Invalid("fields"))?;
        settings.validate()?;
        Ok(settings)
    }

    /// The command-line arguments that start `memocampus-mcp` for these settings. The management
    /// channel's flag is added by the launcher, which owns that handle.
    pub fn server_args(&self) -> Vec<std::ffi::OsString> {
        let mut args: Vec<std::ffi::OsString> = vec![
            "--transport".into(),
            "streamable-http".into(),
            "--listen".into(),
            format!("127.0.0.1:{}", self.listen_port).into(),
        ];
        if let (VaultMode::Pinned, Some(file)) = (self.vault_mode, &self.pinned_vault_path) {
            args.push("--vault".into());
            args.push(file.into());
        }
        for host in &self.allow_hosts {
            args.push("--allow-host".into());
            args.push(host.into());
        }
        args
    }
}

/// The settings in `app_dir`: the defaults if, and only if, the file does not exist.
pub fn load(app_dir: &Path) -> Result<Settings, SettingsError> {
    let file = match fs::File::open(path(app_dir)) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Settings::default()),
        Err(_) => return Err(SettingsError::Io),
    };
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| SettingsError::Io)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(SettingsError::TooLarge);
    }
    Settings::parse(&bytes)
}

/// Writes `settings` atomically (a temporary file, flushed, then renamed over the old one). It
/// refuses when a file is already there that `load` would refuse: an unknown version or an invalid
/// file is the user's to look at, not for the tray to replace.
pub fn save(app_dir: &Path, settings: &Settings) -> Result<(), SettingsError> {
    settings.validate()?;
    match load(app_dir) {
        Ok(_) => {}
        Err(SettingsError::Io) => return Err(SettingsError::Io),
        Err(_) => return Err(SettingsError::WouldOverwrite),
    }
    let bytes = serde_json::to_vec_pretty(settings).map_err(|_| SettingsError::Invalid("json"))?;
    fs::create_dir_all(app_dir).map_err(|_| SettingsError::Io)?;
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let temp = app_dir.join(format!("tray-settings.{}.{unique}.tmp", std::process::id()));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        replace(&temp, &path(app_dir))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(|_| SettingsError::Io)
}

#[cfg(windows)]
fn replace(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let wide = |path: &Path| -> Vec<u16> { path.as_os_str().encode_wide().chain([0]).collect() };
    let (from, to) = (wide(from), wide(to));
    // SAFETY: both are NUL-terminated and live for the call.
    let moved = unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(PathBuf);
    impl Dir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "memocampus-tray-{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write(dir: &Dir, text: &str) {
        fs::write(path(&dir.0), text).unwrap();
    }

    #[test]
    fn a_missing_file_is_the_only_way_to_get_the_defaults() {
        let dir = Dir::new("missing");
        let settings = load(&dir.0).unwrap();
        assert_eq!(settings, Settings::default());
        assert_eq!(settings.listen_port, 8765);
        assert!(settings.autostart && settings.server_autostart);
        assert_eq!(settings.vault_mode, VaultMode::FollowActive);
        // A directory that is not there at all is the same case.
        assert_eq!(load(&dir.0.join("nowhere")).unwrap(), Settings::default());
    }

    #[test]
    fn the_documented_file_round_trips() {
        let dir = Dir::new("roundtrip");
        write(
            &dir,
            r#"{"version":1,"autostart":true,"server_autostart":true,"listen_port":8765,
                "allow_hosts":[],"vault_mode":"follow_active","pinned_vault_path":null}"#,
        );
        assert_eq!(load(&dir.0).unwrap(), Settings::default());
    }

    #[test]
    fn every_bad_file_is_refused_and_none_becomes_the_defaults() {
        let dir = Dir::new("bad");
        let good = r#""autostart":true,"server_autostart":true,"listen_port":8765,"allow_hosts":[],"vault_mode":"follow_active","pinned_vault_path":null"#;
        let cases: Vec<(String, SettingsError)> = vec![
            ("".into(), SettingsError::Invalid("json")),
            ("not json".into(), SettingsError::Invalid("json")),
            ("[]".into(), SettingsError::Invalid("version")),
            (format!("{{{good}}}"), SettingsError::Invalid("version")),
            (
                format!("{{\"version\":2,{good}}}"),
                SettingsError::UnknownVersion(2),
            ),
            (
                format!("{{\"version\":\"1\",{good}}}"),
                SettingsError::Invalid("version"),
            ),
            // An unknown field is never dropped silently.
            (
                format!("{{\"version\":1,\"extra\":1,{good}}}"),
                SettingsError::Invalid("fields"),
            ),
            (
                format!(
                    "{{\"version\":1,{}}}",
                    good.replace("\"listen_port\":8765", "\"listen_port\":0")
                ),
                SettingsError::Invalid("listen_port"),
            ),
            (
                format!(
                    "{{\"version\":1,{}}}",
                    good.replace("\"listen_port\":8765", "\"listen_port\":70000")
                ),
                SettingsError::Invalid("fields"),
            ),
            (
                format!(
                    "{{\"version\":1,{}}}",
                    good.replace(
                        "\"vault_mode\":\"follow_active\"",
                        "\"vault_mode\":\"auto\""
                    )
                ),
                SettingsError::Invalid("fields"),
            ),
            // Pinned needs an absolute path; follow_active must not carry one.
            (
                format!(
                    "{{\"version\":1,{}}}",
                    good.replace("\"follow_active\"", "\"pinned\"")
                ),
                SettingsError::Invalid("pinned_vault_path"),
            ),
            (
                format!(
                    "{{\"version\":1,{}}}",
                    good.replace("\"follow_active\"", "\"pinned\"").replace(
                        "\"pinned_vault_path\":null",
                        "\"pinned_vault_path\":\"rel.sqlite3\""
                    )
                ),
                SettingsError::Invalid("pinned_vault_path"),
            ),
            (
                format!(
                    "{{\"version\":1,{}}}",
                    good.replace(
                        "\"pinned_vault_path\":null",
                        "\"pinned_vault_path\":\"C:\\\\v.sqlite3\""
                    )
                ),
                SettingsError::Invalid("pinned_vault_path"),
            ),
        ];
        for (text, expected) in cases {
            write(&dir, &text);
            assert_eq!(load(&dir.0).unwrap_err(), expected, "{text}");
        }
        // Too large.
        write(&dir, &" ".repeat(70_000));
        assert_eq!(load(&dir.0).unwrap_err(), SettingsError::TooLarge);
        // A directory in place of the file cannot be read.
        fs::remove_file(path(&dir.0)).unwrap();
        fs::create_dir(path(&dir.0)).unwrap();
        assert_eq!(load(&dir.0).unwrap_err(), SettingsError::Io);
    }

    #[test]
    fn allow_hosts_are_limited_and_cannot_inject_arguments() {
        let mut settings = Settings::default();
        for bad in ["", "-x", "a b", "a\tb", "a\"", "é.example:1"] {
            settings.allow_hosts = vec![bad.to_owned()];
            // `a"` is graphic ASCII and allowed through (quoting handles it); the others are not.
            let expected_ok = bad == "a\"";
            assert_eq!(settings.validate().is_ok(), expected_ok, "{bad:?}");
        }
        settings.allow_hosts = vec!["a.example:1".into(), "a.example:1".into()];
        assert!(settings.validate().is_err(), "a duplicate");
        settings.allow_hosts = (1..=9).map(|n| format!("h{n}.example:1")).collect();
        assert!(settings.validate().is_err(), "more than eight");
        settings.allow_hosts.truncate(8);
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn server_args_follow_the_active_vault_unless_pinned() {
        let args = |settings: &Settings| -> Vec<String> {
            settings
                .server_args()
                .into_iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect()
        };
        let mut settings = Settings::default();
        assert_eq!(
            args(&settings),
            [
                "--transport",
                "streamable-http",
                "--listen",
                "127.0.0.1:8765"
            ]
        );
        settings.listen_port = 8080;
        settings.allow_hosts = vec!["a.example.net:8443".into()];
        settings.vault_mode = VaultMode::Pinned;
        settings.pinned_vault_path = Some("C:\\v\\vault.sqlite3".into());
        assert_eq!(
            args(&settings),
            [
                "--transport",
                "streamable-http",
                "--listen",
                "127.0.0.1:8080",
                "--vault",
                "C:\\v\\vault.sqlite3",
                "--allow-host",
                "a.example.net:8443"
            ]
        );
        // Never stdio, never a non-loopback address.
        assert!(!args(&settings)
            .iter()
            .any(|a| a == "stdio" || a.contains("0.0.0.0")));
    }

    #[test]
    fn save_writes_atomically_and_never_replaces_a_file_it_does_not_understand() {
        let dir = Dir::new("save");
        let mut settings = Settings {
            listen_port: 8080,
            ..Settings::default()
        };
        save(&dir.0, &settings).unwrap();
        assert_eq!(load(&dir.0).unwrap(), settings);
        // No temporary file is left behind.
        let left: Vec<_> = fs::read_dir(&dir.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, [FILE_NAME]);
        // An update replaces it.
        settings.autostart = false;
        save(&dir.0, &settings).unwrap();
        assert!(!load(&dir.0).unwrap().autostart);
        // A newer version, and a file that is not valid, are kept byte for byte.
        for text in ["{\"version\":2,\"future\":true}", "garbage"] {
            write(&dir, text);
            assert_eq!(
                save(&dir.0, &settings).unwrap_err(),
                SettingsError::WouldOverwrite
            );
            assert_eq!(fs::read_to_string(path(&dir.0)).unwrap(), text);
        }
        // Invalid settings are not written at all.
        let bad = Settings {
            listen_port: 0,
            ..Settings::default()
        };
        fs::remove_file(path(&dir.0)).unwrap();
        assert!(save(&dir.0, &bad).is_err());
        assert!(!path(&dir.0).exists());
    }

    #[test]
    fn it_never_touches_config_json_or_policy_json() {
        let dir = Dir::new("others");
        fs::write(dir.0.join("config.json"), "CONFIG").unwrap();
        fs::write(dir.0.join("policy.json"), "POLICY").unwrap();
        save(&dir.0, &Settings::default()).unwrap();
        assert_eq!(
            fs::read_to_string(dir.0.join("config.json")).unwrap(),
            "CONFIG"
        );
        assert_eq!(
            fs::read_to_string(dir.0.join("policy.json")).unwrap(),
            "POLICY"
        );
    }
}
