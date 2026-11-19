//! What the tray learns from `memocampus-mcp`: its stderr lines (fixed texts, see `mcp-server/src/main.rs`
//! and `http.rs`) and how it exited. Only a classification is kept; the line itself is dropped, so
//! nothing a line might carry (a path, for one) reaches the tray's state or log.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Diag {
    /// `memocampus-mcp: listening on http://127.0.0.1:<port>/mcp`: the socket is bound.
    Listening {
        port: u16,
    },
    /// `cannot listen: address_in_use`
    PortInUse,
    /// `cannot listen: <other code>`
    ListenFailed(String),
    /// `vault refused: <code>`: the publication gate said no at start.
    VaultRefused(String),
    /// `cannot start: <code>`
    CannotStart(String),
    /// `stopped (<reason>)`
    Stopped(String),
    Other,
}

const PREFIX: &str = "memocampus-mcp: ";

/// Lower-case ASCII letters, digits and `_`, 1..=48: the shape of every code the server prints.
fn code(text: &str) -> Option<String> {
    let ok = (1..=48).contains(&text.len())
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    ok.then(|| text.to_owned())
}

pub fn classify_line(line: &str) -> Diag {
    let Some(rest) = line.trim_end().strip_prefix(PREFIX) else {
        return Diag::Other;
    };
    if let Some(address) = rest.strip_prefix("listening on http://") {
        // `127.0.0.1:<port>/mcp` or `[::1]:<port>/mcp`
        let port = address
            .strip_suffix("/mcp")
            .and_then(|authority| authority.rsplit_once(':'))
            .and_then(|(host, port)| {
                matches!(host, "127.0.0.1" | "[::1]")
                    .then(|| port.parse::<u16>().ok())
                    .flatten()
            });
        return port.map_or(Diag::Other, |port| Diag::Listening { port });
    }
    if let Some(reason) = rest.strip_prefix("cannot listen: ") {
        return match code(reason) {
            Some(reason) if reason == "address_in_use" => Diag::PortInUse,
            Some(reason) => Diag::ListenFailed(reason),
            None => Diag::Other,
        };
    }
    if let Some(reason) = rest.strip_prefix("vault refused: ") {
        return code(reason).map_or(Diag::Other, Diag::VaultRefused);
    }
    if let Some(reason) = rest.strip_prefix("cannot start: ") {
        return code(reason).map_or(Diag::Other, Diag::CannotStart);
    }
    if let Some(reason) = rest
        .strip_prefix("stopped (")
        .and_then(|reason| reason.strip_suffix(')'))
    {
        return code(reason).map_or(Diag::Other, Diag::Stopped);
    }
    Diag::Other
}

/// The server's exit code for a refusal to start (`fail` in `main.rs`): arguments, vault, listen.
pub const EXIT_REFUSED: i32 = 2;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_listening_line_is_the_only_start_signal() {
        assert_eq!(
            classify_line("memocampus-mcp: listening on http://127.0.0.1:8765/mcp"),
            Diag::Listening { port: 8765 }
        );
        assert_eq!(
            classify_line("memocampus-mcp: listening on http://[::1]:9/mcp\r"),
            Diag::Listening { port: 9 }
        );
        for other in [
            "memocampus-mcp: bearer token created in C:\\x\\mcp-http-token",
            "memocampus-mcp: listening on http://0.0.0.0:8765/mcp",
            "memocampus-mcp: listening on http://127.0.0.1:99999/mcp",
            "memocampus-mcp: listening on http://127.0.0.1:8765/other",
            "listening on http://127.0.0.1:8765/mcp",
            "other: memocampus-mcp: listening on http://127.0.0.1:8765/mcp",
            "",
        ] {
            assert_eq!(classify_line(other), Diag::Other, "{other:?}");
        }
    }

    #[test]
    fn refusals_carry_only_their_code() {
        assert_eq!(
            classify_line("memocampus-mcp: cannot listen: address_in_use"),
            Diag::PortInUse
        );
        assert_eq!(
            classify_line("memocampus-mcp: cannot listen: permission_denied"),
            Diag::ListenFailed("permission_denied".into())
        );
        assert_eq!(
            classify_line("memocampus-mcp: vault refused: vault_publication_pending"),
            Diag::VaultRefused("vault_publication_pending".into())
        );
        assert_eq!(
            classify_line("memocampus-mcp: vault refused: vault_not_configured"),
            Diag::VaultRefused("vault_not_configured".into())
        );
        assert_eq!(
            classify_line("memocampus-mcp: cannot start: token_no_app_dir"),
            Diag::CannotStart("token_no_app_dir".into())
        );
        assert_eq!(
            classify_line("memocampus-mcp: stopped (management_stop)"),
            Diag::Stopped("management_stop".into())
        );
        // A "code" that is not one is not trusted: no path or free text gets through.
        for bad in [
            "memocampus-mcp: vault refused: C:\\Users\\x\\vault.sqlite3",
            "memocampus-mcp: vault refused: ",
            "memocampus-mcp: vault refused: Vault_Refused",
            "memocampus-mcp: cannot start: a b",
        ] {
            assert_eq!(classify_line(bad), Diag::Other, "{bad:?}");
        }
    }
}
