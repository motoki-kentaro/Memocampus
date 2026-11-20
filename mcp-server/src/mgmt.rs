//! The management channel (Issue #67, Slice 1): how a supervisor that started this process (the
//! Windows tray) asks it to stop, and how it learns that the supervisor is gone.
//!
//! It is an anonymous pipe whose read end this process inherited. The supervisor holds the only
//! write end, so no other process can send a request, and nothing here is an MCP tool or an HTTP
//! endpoint. It exists only when `--management-pipe-handle <n>` is given (HTTP transport only);
//! without it nothing here runs and the server stops exactly as before (Ctrl+C / Ctrl+Break / console).
//!
//! The protocol is one byte, `S` (stop). Meaning of what the server reads:
//! - `S`: a stop request. The server stops through the same path as Ctrl+C (`http::serve`'s graceful
//!   shutdown), reason `management_stop`.
//! - end of file / broken pipe: the supervisor went away without asking (it crashed or was killed).
//!   The server stops the same way, reason `management_closed`: nothing is left to manage it.
//! - any other byte, or a read error: the channel is not what the supervisor writes. The server
//!   stops (fail closed), reason `management_invalid`.
//!
//! The handle value is only a number in this process's handle table. It is not a secret and is
//! never logged; `open` refuses a value that is not a readable pipe or that is a standard handle.

/// The command-line flag that carries the inherited read end.
pub const FLAG: &str = "--management-pipe-handle";

/// A handle value as written on the command line: decimal digits, no sign, no leading zero, not 0.
pub fn parse_handle(text: &str) -> Option<usize> {
    let canonical = !text.is_empty()
        && text.len() <= 20
        && text.bytes().all(|byte| byte.is_ascii_digit())
        && !text.starts_with('0');
    if canonical {
        text.parse().ok()
    } else {
        None
    }
}

pub const STOP_BYTE: u8 = b'S';

/// The reason the server stops for one byte read from the channel.
pub fn reason_for_byte(byte: u8) -> &'static str {
    if byte == STOP_BYTE {
        "management_stop"
    } else {
        "management_invalid"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenError {
    /// Not a readable pipe, or one of this process's standard handles.
    NotAChannel,
    /// Management channels exist on Windows only.
    #[cfg_attr(windows, allow(dead_code))]
    Unsupported,
}

impl OpenError {
    pub fn code(self) -> &'static str {
        match self {
            Self::NotAChannel => "management_not_a_pipe",
            Self::Unsupported => "management_unsupported",
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::{reason_for_byte, OpenError};
    use tokio::sync::oneshot;
    use windows_sys::Win32::Foundation::{ERROR_BROKEN_PIPE, HANDLE};
    use windows_sys::Win32::Storage::FileSystem::{GetFileType, ReadFile, FILE_TYPE_PIPE};
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    use windows_sys::Win32::System::Pipes::PeekNamedPipe;

    pub struct Channel(HANDLE);

    // SAFETY: a kernel handle value; it is used by one reader thread only.
    unsafe impl Send for Channel {}

    pub fn open(value: usize) -> Result<Channel, OpenError> {
        let handle = value as HANDLE;
        // SAFETY: these calls only query a handle value; an invalid value makes them fail.
        unsafe {
            for standard in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
                if GetStdHandle(standard) == handle {
                    return Err(OpenError::NotAChannel);
                }
            }
            if GetFileType(handle) != FILE_TYPE_PIPE {
                return Err(OpenError::NotAChannel);
            }
            // Needs read access: it fails for the write end and for any handle that is not ours to read.
            let mut available = 0u32;
            if PeekNamedPipe(
                handle,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available,
                std::ptr::null_mut(),
            ) == 0
            {
                return Err(OpenError::NotAChannel);
            }
        }
        Ok(Channel(handle))
    }

    /// Starts the reader. The receiver resolves once, with the reason to stop. The handle stays open
    /// until the process ends.
    pub fn watch(channel: Channel) -> oneshot::Receiver<&'static str> {
        let (send, receive) = oneshot::channel();
        let spawned = std::thread::Builder::new()
            .name("management-pipe".into())
            .spawn(move || {
                // Moves the whole `Channel` (which is `Send`), not its raw-pointer field.
                let channel = channel;
                let mut byte = 0u8;
                let mut read = 0u32;
                // SAFETY: `byte` and `read` are live for the call; the handle is a readable pipe.
                let ok = unsafe {
                    ReadFile(
                        channel.0,
                        (&mut byte as *mut u8).cast(),
                        1,
                        &mut read,
                        std::ptr::null_mut(),
                    )
                };
                let reason = if ok != 0 {
                    if read == 1 {
                        reason_for_byte(byte)
                    } else {
                        "management_closed"
                    }
                // SAFETY: reads the calling thread's last error, straight after the call.
                } else if unsafe { windows_sys::Win32::Foundation::GetLastError() }
                    == ERROR_BROKEN_PIPE
                {
                    "management_closed"
                } else {
                    "management_invalid"
                };
                let _ = send.send(reason);
            });
        if spawned.is_err() {
            // No reader: stop rather than run with a channel nobody watches. The sender was moved
            // into the closure that did not start, so the receiver reports the channel closed.
        }
        receive
    }
}

#[cfg(not(windows))]
mod imp {
    use super::OpenError;
    use tokio::sync::oneshot;

    pub struct Channel;

    pub fn open(_: usize) -> Result<Channel, OpenError> {
        Err(OpenError::Unsupported)
    }

    pub fn watch(_: Channel) -> oneshot::Receiver<&'static str> {
        oneshot::channel().1
    }
}

pub use imp::{open, watch};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_canonical_decimal_handle_value_is_accepted() {
        assert_eq!(parse_handle("123"), Some(123));
        assert_eq!(parse_handle("4"), Some(4));
        for bad in [
            "",
            "0",
            "012",
            "-4",
            "+4",
            "4 ",
            " 4",
            "0x10",
            "1e3",
            "99999999999999999999999",
        ] {
            assert_eq!(parse_handle(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn only_the_stop_byte_is_a_stop_request_and_any_other_byte_is_invalid() {
        assert_eq!(reason_for_byte(b'S'), "management_stop");
        for byte in [0u8, b's', b'\n', b'X', 0xff] {
            assert_eq!(reason_for_byte(byte), "management_invalid");
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_standard_handle_or_garbage_is_not_a_channel() {
        use windows_sys::Win32::System::Console::{GetStdHandle, STD_INPUT_HANDLE};
        let stdin = unsafe { GetStdHandle(STD_INPUT_HANDLE) } as usize;
        if stdin != 0 {
            assert!(open(stdin).is_err());
        }
        assert!(open(0x7fff_fff0).is_err());
    }
}
