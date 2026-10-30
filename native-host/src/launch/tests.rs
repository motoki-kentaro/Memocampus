use super::*;
use std::ffi::OsString;

fn args(list: &[&str]) -> impl Iterator<Item = OsString> {
    list.iter()
        .map(OsString::from)
        .collect::<Vec<_>>()
        .into_iter()
}

#[test]
fn only_a_console_of_its_own_without_an_extension_origin_is_a_direct_launch() {
    let own = Stdin::Console { sole_process: true };
    let shared = Stdin::Console {
        sole_process: false,
    };
    assert!(is_direct(own, args(&[])));
    assert!(
        !is_direct(shared, args(&[])),
        "started from cmd or PowerShell"
    );
    assert!(
        !is_direct(Stdin::Redirected, args(&[])),
        "pipes, files, NUL"
    );
    assert!(
        !is_direct(Stdin::Unusable, args(&[])),
        "unknown: never guess"
    );
    let chrome = [
        "chrome-extension://abcdefghijklmnopabcdefghijklmnop/",
        "--parent-window=0",
    ];
    for stdin in [own, shared, Stdin::Redirected, Stdin::Unusable] {
        assert!(!is_direct(stdin, args(&chrome)), "{stdin:?}");
    }
}

#[test]
fn the_guide_text_and_style_are_as_specified() {
    assert_eq!(TITLE, "Memocampus Native Host");
    assert_eq!(
        TEXT,
        "このプログラムは Memocampus Chrome 拡張機能と連携して動作します。\n\n単体で起動する必要はありません。\nChrome から自動的に起動されます。"
    );
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONINFORMATION, MB_OK};
        assert_eq!(STYLE, MB_OK | MB_ICONINFORMATION);
        assert_eq!(
            STYLE, 0x40,
            "information icon, OK button only, nothing else"
        );
    }
}

#[cfg(windows)]
mod handles {
    use super::super::{classify, Stdin};
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;

    #[test]
    fn missing_and_broken_handles_are_unusable() {
        assert_eq!(classify(std::ptr::null_mut()), Stdin::Unusable);
        assert_eq!(classify(INVALID_HANDLE_VALUE), Stdin::Unusable);
        // A value that is not an open handle in this process.
        assert_eq!(classify(0x0FFF_FFFC as _), Stdin::Unusable);
    }

    #[test]
    fn pipes_files_and_nul_are_redirections() {
        let (reader, _writer) = std::io::pipe().unwrap();
        assert_eq!(classify(reader.as_raw_handle()), Stdin::Redirected);
        let file = std::fs::File::open(file!()).unwrap();
        assert_eq!(classify(file.as_raw_handle()), Stdin::Redirected);
        let nul = std::fs::File::open("NUL").unwrap();
        assert_eq!(
            classify(nul.as_raw_handle()),
            Stdin::Redirected,
            "a character device, not a console"
        );
    }
}
