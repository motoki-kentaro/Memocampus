//! Tells a double-click in Explorer apart from a Chrome Native Messaging launch.
//!
//! Chrome starts the host with stdin and stdout redirected to pipes and passes the caller's
//! origin (`chrome-extension://<id>/`) as the first argument. A console program started from
//! Explorer instead gets a new console of its own: stdin is a console handle and this process is
//! the only one attached to that console. Only that combination counts as a direct launch; a pipe,
//! a file, `NUL`, a missing or broken handle, or a console shared with a shell (cmd, PowerShell)
//! keeps the previous behaviour, i.e. the Native Messaging loop. Nothing is written to stdout.

/// What stdin is, as far as the launch decision cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stdin {
    /// A console input handle; `sole_process` if no other process shares the console.
    Console { sole_process: bool },
    /// A pipe, file, character device such as `NUL`, or an unknown type.
    Redirected,
    /// No handle, or one that cannot be queried.
    Unusable,
}

pub const TITLE: &str = "Memocampus Native Host";
pub const TEXT: &str = "このプログラムは Memocampus Chrome 拡張機能と連携して動作します。\n\n単体で起動する必要はありません。\nChrome から自動的に起動されます。";
#[cfg(windows)]
pub const STYLE: u32 = windows_sys::Win32::UI::WindowsAndMessaging::MB_OK
    | windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONINFORMATION;

/// True only for a launch that cannot be Chrome's (see the module comment).
pub fn is_direct(stdin: Stdin, mut args: impl Iterator<Item = std::ffi::OsString>) -> bool {
    let from_extension = args.any(|arg| arg.to_string_lossy().starts_with("chrome-extension://"));
    stdin == (Stdin::Console { sole_process: true }) && !from_extension
}

/// Shows the guide once and returns true for a direct launch; otherwise does nothing.
pub fn guide_if_direct() -> bool {
    if !is_direct(current_stdin(), std::env::args_os().skip(1)) {
        return false;
    }
    show_guide();
    true
}

#[cfg(windows)]
pub fn current_stdin() -> Stdin {
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_INPUT_HANDLE};
    // SAFETY: GetStdHandle only reads the process's standard handle table.
    classify(unsafe { GetStdHandle(STD_INPUT_HANDLE) })
}

#[cfg(not(windows))]
pub fn current_stdin() -> Stdin {
    Stdin::Redirected
}

/// GetFileType tells pipes and files from character devices; GetConsoleMode succeeds only for
/// console handles (not for `NUL`, which is also a character device); GetConsoleProcessList
/// counts the processes attached to this process's console.
#[cfg(windows)]
pub fn classify(handle: windows_sys::Win32::Foundation::HANDLE) -> Stdin {
    use windows_sys::Win32::Foundation::{GetLastError, INVALID_HANDLE_VALUE, NO_ERROR};
    use windows_sys::Win32::Storage::FileSystem::{GetFileType, FILE_TYPE_CHAR, FILE_TYPE_UNKNOWN};
    use windows_sys::Win32::System::Console::{GetConsoleMode, GetConsoleProcessList};
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Stdin::Unusable;
    }
    // SAFETY: the calls only query `handle`; an invalid handle makes them fail, not misbehave.
    let file_type = unsafe { GetFileType(handle) };
    if file_type == FILE_TYPE_UNKNOWN && unsafe { GetLastError() } != NO_ERROR {
        return Stdin::Unusable;
    }
    let mut mode = 0;
    if file_type != FILE_TYPE_CHAR || unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
        return Stdin::Redirected;
    }
    let mut ids = [0u32; 2];
    let attached = unsafe { GetConsoleProcessList(ids.as_mut_ptr(), ids.len() as u32) };
    Stdin::Console {
        sole_process: attached == 1,
    }
}

#[cfg(windows)]
fn show_guide() {
    use windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW;
    let wide = |text: &str| text.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (title, text) = (wide(TITLE), wide(TEXT));
    // SAFETY: both strings are NUL-terminated and outlive the modal call; no owner window.
    unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), STYLE) };
}

#[cfg(not(windows))]
fn show_guide() {}

#[cfg(test)]
mod tests;
