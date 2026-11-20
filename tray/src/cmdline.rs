//! A Windows command line from an argument list, quoted the way `CommandLineToArgvW` and the MSVC
//! runtime read it back (the rules `std::process::Command` follows on Windows).

use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::OsStrExt;

const QUOTE: u16 = b'"' as u16;
const BACKSLASH: u16 = b'\\' as u16;

/// One argument, quoted so that it is read back as exactly itself.
pub fn quote(arg: &OsStr, out: &mut Vec<u16>) {
    let units: Vec<u16> = arg.encode_wide().collect();
    let needs_quotes = units.is_empty()
        || units
            .iter()
            .any(|&u| u == b' ' as u16 || u == b'\t' as u16 || u == QUOTE);
    if !needs_quotes {
        out.extend(&units);
        return;
    }
    out.push(QUOTE);
    let mut backslashes = 0usize;
    for &unit in &units {
        if unit == BACKSLASH {
            backslashes += 1;
            continue;
        }
        if unit == QUOTE {
            // Backslashes before a quote are doubled, and the quote itself escaped.
            out.extend(std::iter::repeat_n(BACKSLASH, backslashes * 2 + 1));
        } else {
            out.extend(std::iter::repeat_n(BACKSLASH, backslashes));
        }
        backslashes = 0;
        out.push(unit);
    }
    // Backslashes before the closing quote are doubled so they do not escape it.
    out.extend(std::iter::repeat_n(BACKSLASH, backslashes * 2));
    out.push(QUOTE);
}

/// `"program" arg arg ...`, NUL-terminated, as `CreateProcessW` wants it.
pub fn command_line(program: &OsStr, args: &[OsString]) -> Vec<u16> {
    let mut line = Vec::new();
    // The program is always quoted: a path with a space must not be split.
    line.push(QUOTE);
    line.extend(program.encode_wide());
    line.push(QUOTE);
    for arg in args {
        line.push(b' ' as u16);
        quote(arg, &mut line);
    }
    line.push(0);
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quoted(text: &str) -> String {
        let mut out = Vec::new();
        quote(OsStr::new(text), &mut out);
        String::from_utf16(&out).unwrap()
    }

    #[test]
    fn plain_arguments_are_left_alone_and_others_are_quoted() {
        assert_eq!(quoted("--listen"), "--listen");
        assert_eq!(quoted("127.0.0.1:8765"), "127.0.0.1:8765");
        assert_eq!(quoted(""), "\"\"");
        assert_eq!(
            quoted("C:\\Program Files\\v.sqlite3"),
            "\"C:\\Program Files\\v.sqlite3\""
        );
        assert_eq!(quoted("a\tb"), "\"a\tb\"");
    }

    #[test]
    fn quotes_and_trailing_backslashes_are_escaped_as_the_runtime_expects() {
        assert_eq!(quoted("say \"hi\""), "\"say \\\"hi\\\"\"");
        // A trailing backslash would escape the closing quote, so it is doubled.
        assert_eq!(quoted("C:\\my dir\\"), "\"C:\\my dir\\\\\"");
        // Backslashes before a quote are doubled, and the quote escaped.
        assert_eq!(quoted("a\\\"b"), "\"a\\\\\\\"b\"");
        // A backslash elsewhere is literal.
        assert_eq!(quoted("a\\b c"), "\"a\\b c\"");
    }

    #[test]
    fn the_program_is_always_quoted_and_the_line_is_nul_terminated() {
        let line = command_line(
            OsStr::new("C:\\Program Files\\Memocampus\\memocampus-mcp.exe"),
            &["--vault".into(), "C:\\a b\\v.sqlite3".into()],
        );
        assert_eq!(line.last(), Some(&0));
        assert_eq!(
            String::from_utf16(&line[..line.len() - 1]).unwrap(),
            "\"C:\\Program Files\\Memocampus\\memocampus-mcp.exe\" --vault \"C:\\a b\\v.sqlite3\""
        );
    }
}
