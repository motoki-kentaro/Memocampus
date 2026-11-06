//! The bearer token of the Streamable HTTP transport.
//!
//! One token per Windows user, in `<app dir>\mcp-http-token` (the app directory of the policy:
//! `%LOCALAPPDATA%\Memocampus`, or `MEMOCAMPUS_APP_DIR`). The file inherits the ACL of that
//! directory, which on a standard profile grants access to the user, SYSTEM and Administrators
//! only. It is created on the first HTTP start from 32 bytes of the operating system's CSPRNG
//! (`getrandom`), written as 64 lowercase hex digits, and never rewritten: deleting the file and
//! starting again issues a new token. A file that is not exactly one such token refuses the start.
//!
//! The token never appears in a log, on stdout or stderr, or in a URL; it is accepted only in an
//! `Authorization: Bearer` header and compared in constant time.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = "mcp-http-token";
const TOKEN_BYTES: usize = 32;
pub const TOKEN_CHARS: usize = TOKEN_BYTES * 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenError {
    /// No absolute application directory (`LOCALAPPDATA` / `MEMOCAMPUS_APP_DIR`).
    NoAppDir,
    /// The random generator failed.
    Random,
    /// The file could not be created or read.
    Io,
    /// The file exists but does not hold one token.
    Invalid,
}

impl TokenError {
    pub fn code(self) -> &'static str {
        match self {
            Self::NoAppDir => "token_no_app_dir",
            Self::Random => "token_random_failed",
            Self::Io => "token_io_failed",
            Self::Invalid => "token_file_invalid",
        }
    }
}

/// A loaded token. `Debug` does not show it.
pub struct Token([u8; TOKEN_CHARS]);

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token(..)")
    }
}

fn is_token(text: &[u8]) -> bool {
    text.len() == TOKEN_CHARS
        && text
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

impl Token {
    fn parse(text: &[u8]) -> Result<Self, TokenError> {
        // A trailing line break (an editor, `Set-Content`) is tolerated; nothing else is.
        let text = text
            .strip_suffix(b"\r\n")
            .or_else(|| text.strip_suffix(b"\n"))
            .unwrap_or(text);
        if !is_token(text) {
            return Err(TokenError::Invalid);
        }
        let mut token = [0u8; TOKEN_CHARS];
        token.copy_from_slice(text);
        Ok(Self(token))
    }

    fn generate() -> Result<Self, TokenError> {
        let mut bytes = [0u8; TOKEN_BYTES];
        getrandom::fill(&mut bytes).map_err(|_| TokenError::Random)?;
        let mut token = [0u8; TOKEN_CHARS];
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for (i, byte) in bytes.iter().enumerate() {
            token[2 * i] = HEX[usize::from(byte >> 4)];
            token[2 * i + 1] = HEX[usize::from(byte & 0x0f)];
        }
        Ok(Self(token))
    }

    /// Whether `presented` is this token. The time taken depends only on the length of
    /// `presented`, never on where it first differs.
    pub fn matches(&self, presented: &[u8]) -> bool {
        let mut difference = u8::from(presented.len() != TOKEN_CHARS);
        for (i, expected) in self.0.iter().enumerate() {
            let byte = presented.get(i).copied().unwrap_or(0);
            difference |= expected ^ byte;
        }
        std::hint::black_box(difference) == 0
    }
}

pub fn path(app_dir: &Path) -> PathBuf {
    app_dir.join(FILE_NAME)
}

/// The token in `app_dir`, created there if there is none. `created` tells whether it was.
pub fn load_or_create(app_dir: &Path) -> Result<(Token, bool), TokenError> {
    let file = path(app_dir);
    match fs::read(&file) {
        Ok(text) => return Token::parse(&text).map(|token| (token, false)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(TokenError::Io),
    }
    let token = Token::generate()?;
    fs::create_dir_all(app_dir).map_err(|_| TokenError::Io)?;
    match OpenOptions::new().write(true).create_new(true).open(&file) {
        Ok(mut out) => {
            if out
                .write_all(&token.0)
                .and_then(|()| out.sync_all())
                .is_err()
            {
                drop(out);
                let _ = fs::remove_file(&file);
                return Err(TokenError::Io);
            }
            Ok((token, true))
        }
        // Another process created it first: use that one.
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let text = fs::read(&file).map_err(|_| TokenError::Io)?;
            Token::parse(&text).map(|token| (token, false))
        }
        Err(_) => Err(TokenError::Io),
    }
}

/// The token of an `Authorization` header value: `Bearer <token>`, the scheme in any case.
pub fn bearer(header: &[u8]) -> Option<&[u8]> {
    let (scheme, rest) = header.split_at_checked(7)?;
    (scheme.eq_ignore_ascii_case(b"bearer ")).then_some(rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("memocampus-token-{}", getrandom::u64().unwrap()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_token_is_created_once_and_read_back_unchanged() {
        let dir = temp();
        let (first, created) = load_or_create(&dir).unwrap();
        assert!(created);
        let text = fs::read(path(&dir)).unwrap();
        assert!(is_token(&text), "{:?}", String::from_utf8_lossy(&text));
        let (second, created) = load_or_create(&dir).unwrap();
        assert!(!created);
        assert!(second.matches(&first.0));
        let (other, _) = load_or_create(&temp()).unwrap();
        assert!(!other.matches(&first.0), "two files, two tokens");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_file_that_is_not_exactly_one_token_refuses_the_start() {
        let dir = temp();
        fs::create_dir_all(&dir).unwrap();
        let good = "0123456789abcdef".repeat(4);
        for bad in [
            String::new(),
            good[..63].to_owned(),
            format!("{good}0"),
            good.to_uppercase(),
            format!(" {good}"),
            format!("{good}\n\n"),
            "g".repeat(64),
        ] {
            fs::write(path(&dir), &bad).unwrap();
            assert_eq!(
                load_or_create(&dir).unwrap_err(),
                TokenError::Invalid,
                "{bad:?}"
            );
            assert_eq!(
                fs::read_to_string(path(&dir)).unwrap(),
                bad,
                "never rewritten"
            );
        }
        for ok in [good.clone(), format!("{good}\n"), format!("{good}\r\n")] {
            fs::write(path(&dir), &ok).unwrap();
            let (token, created) = load_or_create(&dir).unwrap();
            assert!(!created && token.matches(good.as_bytes()));
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_the_exact_token_matches() {
        let token = Token::parse("0123456789abcdef".repeat(4).as_bytes()).unwrap();
        let good = "0123456789abcdef".repeat(4);
        assert!(token.matches(good.as_bytes()));
        for bad in [
            "",
            &good[..63],
            &format!("{good}0"),
            &good.to_uppercase(),
            &format!("{}1", &good[..63]),
        ] {
            assert!(!token.matches(bad.as_bytes()), "{bad:?}");
        }
        assert_eq!(format!("{token:?}"), "Token(..)");
    }

    #[test]
    fn the_bearer_scheme_is_case_insensitive_and_required() {
        assert_eq!(bearer(b"Bearer abc"), Some(&b"abc"[..]));
        assert_eq!(bearer(b"bearer abc"), Some(&b"abc"[..]));
        assert_eq!(bearer(b"BEARER abc"), Some(&b"abc"[..]));
        for bad in [
            &b"Basic abc"[..],
            b"Bearer",
            b"Bearerabc",
            b"",
            b"Token abc",
        ] {
            assert_eq!(bearer(bad), None, "{:?}", String::from_utf8_lossy(bad));
        }
    }
}
