//! `memocampus-mcp --vault <absolute path>`: the user's Memocampus vault as a read-only MCP server
//! over stdio (Issue #8, Slice 1), or, only when asked for with
//! `--transport streamable-http [--listen 127.0.0.1:<port>]`, over Streamable HTTP on loopback
//! (`http`). Both serve the same handler (`server::Memocampus`) over the same `VisibleStore`.
//!
//! In stdio mode stdout carries the MCP protocol only, and the only other output is one line on
//! stderr when the server cannot start: a fixed code, never a URL, title or other content of the
//! vault. In HTTP mode stderr also names the address and the token file (never the token) and the
//! reason it stopped. Startup and connection events are appended to a diagnostic log file (`diag`).

mod dates;
mod diag;
mod http;
mod observed;
mod server;
mod token;

use memocampus_visible_store::VisibleStore;
use rmcp::service::{QuitReason, ServerInitializeError};
use rmcp::ServiceExt;
use serde_json::json;
use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage: memocampus-mcp --vault <absolute path to the vault .sqlite3 file> \
[--transport stdio | --transport streamable-http [--listen 127.0.0.1:<port>] \
[--allow-host <dns name>:<port>]...]";

fn fail(message: &str) -> ExitCode {
    eprintln!("memocampus-mcp: {message}");
    ExitCode::from(2)
}

/// `process_error` for a stage, then the exit that `fail` makes.
fn fail_logged(stage: &str, fields: serde_json::Value, message: &str) -> ExitCode {
    diag::event(
        "process_error",
        false,
        diag::with(json!({"stage": stage, "exit_code": 2}), fields),
    );
    fail(message)
}

/// The variant, and for a closed connection the step rmcp names (a fixed text of the SDK); for a
/// transport error the I/O error's kind and number. Never a message the client sent.
fn handshake_error(error: &ServerInitializeError) -> serde_json::Value {
    match error {
        ServerInitializeError::ConnectionClosed(step) => {
            json!({"error": "connection_closed", "step": diag::client_text(step)})
        }
        ServerInitializeError::ExpectedInitializeRequest(_) => {
            json!({"error": "expected_initialize_request"})
        }
        ServerInitializeError::UnexpectedInitializeResponse(_) => {
            json!({"error": "unexpected_initialize_response"})
        }
        ServerInitializeError::InitializeFailed(data) => {
            json!({"error": "initialize_failed", "code": data.code.0})
        }
        ServerInitializeError::TransportError { error, context } => diag::with(
            json!({"error": "transport", "step": diag::client_text(context)}),
            error
                .error
                .downcast_ref::<std::io::Error>()
                .map(diag::io_error)
                .unwrap_or_else(|| json!({})),
        ),
        ServerInitializeError::Cancelled => json!({"error": "cancelled"}),
        _ => json!({"error": "other"}),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transport {
    Stdio,
    /// Only ever a loopback address (`http::parse_listen`).
    Http(SocketAddr),
}

#[derive(Debug, PartialEq)]
struct Options {
    vault: PathBuf,
    transport: Transport,
    /// `--allow-host`, HTTP only; empty unless given.
    allow_hosts: Vec<http::AllowedHost>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArgError {
    Usage,
    NotAbsolute,
    Transport,
    Listen,
    ListenWithoutHttp,
    AllowHost,
    AllowHostWithoutHttp,
}

impl ArgError {
    fn code(self) -> &'static str {
        match self {
            Self::Usage => "usage",
            Self::NotAbsolute => "not_absolute",
            Self::Transport => "transport",
            Self::Listen => "listen",
            Self::ListenWithoutHttp => "listen_without_http",
            Self::AllowHost => "allow_host",
            Self::AllowHostWithoutHttp => "allow_host_without_http",
        }
    }

    fn message(self) -> &'static str {
        match self {
            Self::Usage => USAGE,
            Self::NotAbsolute => "the vault path must be absolute",
            Self::Transport => "--transport must be stdio or streamable-http",
            Self::Listen => "--listen must be 127.0.0.1:<port> or [::1]:<port>",
            Self::ListenWithoutHttp => "--listen needs --transport streamable-http",
            Self::AllowHost => {
                "--allow-host must be one exact <dns name>:<port> (no wildcard, no IP address), \
                 each at most once, at most 8"
            }
            Self::AllowHostWithoutHttp => "--allow-host needs --transport streamable-http",
        }
    }
}

/// `--vault <path>` (required, absolute), `--transport <stdio|streamable-http>` and `--listen
/// <address>`, each at most once, and `--allow-host <name>:<port>` (HTTP only) repeatable for
/// different values, in any order. Anything else refuses the start: a setting that is not
/// understood is never guessed at.
fn options(mut args: impl Iterator<Item = OsString>) -> Result<Options, ArgError> {
    let (mut vault, mut transport, mut listen) = (None, None, None);
    let mut allow_hosts: Vec<http::AllowedHost> = Vec::new();
    while let Some(name) = args.next() {
        if name.to_str() == Some("--allow-host") {
            let host = args
                .next()
                .ok_or(ArgError::Usage)?
                .to_str()
                .and_then(http::parse_allowed_host)
                .ok_or(ArgError::AllowHost)?;
            if allow_hosts.contains(&host) || allow_hosts.len() == http::MAX_ALLOWED_HOSTS {
                return Err(ArgError::AllowHost);
            }
            allow_hosts.push(host);
            continue;
        }
        let slot = match name.to_str() {
            Some("--vault") => &mut vault,
            Some("--transport") => &mut transport,
            Some("--listen") => &mut listen,
            _ => return Err(ArgError::Usage),
        };
        let value = args.next().ok_or(ArgError::Usage)?;
        if slot.replace(value).is_some() {
            return Err(ArgError::Usage);
        }
    }
    let vault = PathBuf::from(vault.ok_or(ArgError::Usage)?);
    if !vault.is_absolute() {
        return Err(ArgError::NotAbsolute);
    }
    let http = match transport.as_ref().map(|value| value.to_str()) {
        None | Some(Some("stdio")) => false,
        Some(Some("streamable-http")) => true,
        Some(_) => return Err(ArgError::Transport),
    };
    let transport = match (http, listen) {
        (false, None) => Transport::Stdio,
        (false, Some(_)) => return Err(ArgError::ListenWithoutHttp),
        (true, None) => Transport::Http(http::DEFAULT_LISTEN),
        (true, Some(listen)) => Transport::Http(
            listen
                .to_str()
                .and_then(http::parse_listen)
                .ok_or(ArgError::Listen)?,
        ),
    };
    if !http && !allow_hosts.is_empty() {
        return Err(ArgError::AllowHostWithoutHttp);
    }
    Ok(Options {
        vault,
        transport,
        allow_hosts,
    })
}

fn main() -> ExitCode {
    diag::init();
    diag::event("process_start", true, json!({}));
    diag::install_panic_hook();
    diag::event("startup_context", true, diag::startup_context());
    let (code, reason) = run();
    if let Some(reason) = reason {
        diag::event(
            "process_exit",
            true,
            json!({"reason": reason, "exit_code": 0}),
        );
    }
    code
}

/// The exit, and the reason for a normal one. A failure has logged `process_error` instead.
fn run() -> (ExitCode, Option<&'static str>) {
    let mut args = std::env::args_os().skip(1).peekable();
    match args.peek().and_then(|arg| arg.to_str()) {
        Some("--help" | "-h") => {
            eprintln!("{USAGE}");
            return (ExitCode::SUCCESS, Some("help"));
        }
        Some("--version") => {
            eprintln!("memocampus-mcp {}", env!("CARGO_PKG_VERSION"));
            return (ExitCode::SUCCESS, Some("version"));
        }
        _ => {}
    }
    let Options {
        vault,
        transport,
        allow_hosts,
    } = match options(args) {
        Ok(options) => options,
        Err(error) => {
            return (
                fail_logged("arguments", json!({"error": error.code()}), error.message()),
                None,
            );
        }
    };
    let transport_name = match transport {
        Transport::Stdio => "stdio",
        Transport::Http(_) => "streamable-http",
    };
    diag::event(
        "vault_init_start",
        true,
        json!({"transport": transport_name}),
    );
    let store = match VisibleStore::from_process(vault.clone()) {
        Ok(store) => store,
        Err(error) => {
            let fields = json!({"step": "policy", "error": error.code()});
            diag::event("vault_init_failure", false, fields.clone());
            let message = format!("cannot start: {}", error.code());
            return (fail_logged("vault_init", fields, &message), None);
        }
    };
    // Fixed for the life of the process and checked once now, so a wrong path or an old vault is
    // reported before any client connects. Every tool call checks it again.
    if let Err(error) = store.check_vault() {
        let fields = diag::with(
            json!({"step": "vault_check", "error": error.code()}),
            diag::vault_file(&vault),
        );
        diag::event("vault_init_failure", false, fields.clone());
        let message = format!("vault refused: {}", error.code());
        return (fail_logged("vault_init", fields, &message), None);
    }
    diag::event("vault_init_success", true, json!({}));
    let handler = server::Memocampus::new(store);
    match transport {
        Transport::Stdio => run_stdio(handler),
        Transport::Http(listen) => run_http(handler, listen, allow_hosts),
    }
}

fn runtime() -> Result<tokio::runtime::Runtime, ExitCode> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| fail_logged("runtime", diag::io_error(&error), "cannot start: runtime"))
}

fn run_stdio(handler: server::Memocampus) -> (ExitCode, Option<&'static str>) {
    let runtime = match runtime() {
        Ok(runtime) => runtime,
        Err(code) => return (code, None),
    };
    let served = runtime.block_on(async {
        diag::event("mcp_transport_start", true, json!({}));
        let service = handler.serve(observed::stdio()).await.map_err(|error| {
            (
                "handshake",
                handshake_error(&error),
                "the client did not complete the MCP handshake",
            )
        })?;
        diag::event("mcp_service_started", true, json!({}));
        match service.waiting().await {
            Ok(QuitReason::Closed) => Ok("closed"),
            Ok(QuitReason::Cancelled) => Ok("cancelled"),
            Ok(_) => Ok("other"),
            Err(_) => Err((
                "service",
                json!({"error": "join"}),
                "the connection ended with an error",
            )),
        }
    });
    // Blocking reads still running are not waited for: they hold no write lock and change nothing.
    runtime.shutdown_background();
    match served {
        Ok(reason) => (ExitCode::SUCCESS, Some(reason)),
        Err((stage, fields, message)) => (fail_logged(stage, fields, message), None),
    }
}

fn run_http(
    handler: server::Memocampus,
    listen: SocketAddr,
    allow_hosts: Vec<http::AllowedHost>,
) -> (ExitCode, Option<&'static str>) {
    let token_failed = |error: token::TokenError| {
        let message = format!("cannot start: {}", error.code());
        (
            fail_logged("token", json!({"error": error.code()}), &message),
            None,
        )
    };
    let Some((app_dir, _)) = diag::app_dir() else {
        return token_failed(token::TokenError::NoAppDir);
    };
    let (token, created) = match token::load_or_create(&app_dir) {
        Ok(loaded) => loaded,
        Err(error) => return token_failed(error),
    };
    diag::event("http_token_ready", true, json!({"created": created}));
    let runtime = match runtime() {
        Ok(runtime) => runtime,
        Err(code) => return (code, None),
    };
    let served = runtime.block_on(async {
        let listener = http::bind(listen).map_err(|(kind, error)| {
            (
                diag::with(json!({"error": kind.code()}), diag::io_error(&error)),
                format!("cannot listen: {}", kind.code()),
            )
        })?;
        let port = listener.local_addr().map(|addr| addr.port()).unwrap_or(0);
        let family = if listen.is_ipv4() { "ipv4" } else { "ipv6" };
        diag::event(
            "http_listening",
            true,
            json!({"loopback": family, "port": port, "extra_hosts": allow_hosts.len()}),
        );
        let shown = if listen.is_ipv4() {
            format!("127.0.0.1:{port}")
        } else {
            format!("[::1]:{port}")
        };
        eprintln!(
            "memocampus-mcp: bearer token {} {}",
            if created { "created in" } else { "read from" },
            token::path(&app_dir).display()
        );
        for host in &allow_hosts {
            eprintln!(
                "memocampus-mcp: also accepting Host {} (from --allow-host)",
                host.authority()
            );
        }
        eprintln!("memocampus-mcp: listening on http://{shown}{}", http::PATH);
        Ok(http::serve(handler, listener, token, allow_hosts).await)
    });
    runtime.shutdown_background();
    match served {
        Ok(reason) => {
            eprintln!("memocampus-mcp: stopped ({reason})");
            (ExitCode::SUCCESS, Some(reason))
        }
        Err((fields, message)) => (fail_logged("listen", fields, &message), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Options, ArgError> {
        options(args.iter().map(OsString::from))
    }

    fn absolute() -> &'static str {
        if cfg!(windows) {
            r"C:\v\vault.sqlite3"
        } else {
            "/v/vault.sqlite3"
        }
    }

    #[test]
    fn only_an_absolute_vault_path_is_accepted() {
        let absolute = absolute();
        assert_eq!(
            parse(&["--vault", absolute]).unwrap(),
            Options {
                vault: PathBuf::from(absolute),
                transport: Transport::Stdio,
                allow_hosts: Vec::new(),
            }
        );
        for bad in [
            &[][..],
            &["--vault"],
            &["vault.sqlite3"],
            &["--vault", "vault.sqlite3"],
            &["--vault", absolute, "--vault", absolute],
            &["--db", absolute],
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn http_is_chosen_only_explicitly_and_only_on_loopback() {
        let v = absolute();
        let http = |listen: &str| Transport::Http(listen.parse().unwrap());
        let sh = "streamable-http";
        for (args, transport) in [
            (vec!["--vault", v, "--transport", "stdio"], Transport::Stdio),
            (
                vec!["--vault", v, "--transport", sh],
                http("127.0.0.1:8765"),
            ),
            (
                vec![
                    "--transport",
                    sh,
                    "--listen",
                    "127.0.0.1:9000",
                    "--vault",
                    v,
                ],
                http("127.0.0.1:9000"),
            ),
            (
                vec!["--vault", v, "--transport", sh, "--listen", "[::1]:9000"],
                http("[::1]:9000"),
            ),
        ] {
            assert_eq!(parse(&args).unwrap().transport, transport, "{args:?}");
        }
        for (args, error) in [
            (
                vec!["--vault", v, "--transport", "http"],
                ArgError::Transport,
            ),
            (
                vec!["--vault", v, "--transport", "Streamable-HTTP"],
                ArgError::Transport,
            ),
            (
                vec!["--vault", v, "--listen", "127.0.0.1:9000"],
                ArgError::ListenWithoutHttp,
            ),
            (
                vec![
                    "--vault",
                    v,
                    "--transport",
                    "stdio",
                    "--listen",
                    "127.0.0.1:9000",
                ],
                ArgError::ListenWithoutHttp,
            ),
            (
                vec!["--vault", v, "--transport", sh, "--listen", "0.0.0.0:9000"],
                ArgError::Listen,
            ),
            (
                vec![
                    "--vault",
                    v,
                    "--transport",
                    sh,
                    "--listen",
                    "localhost:9000",
                ],
                ArgError::Listen,
            ),
            (
                vec!["--vault", v, "--transport", sh, "--transport", "stdio"],
                ArgError::Usage,
            ),
            (vec!["--vault", v, "--transport"], ArgError::Usage),
            (vec!["--transport", sh], ArgError::Usage),
            (vec!["--vault", v, "--auth", "none"], ArgError::Usage),
        ] {
            assert_eq!(parse(&args).unwrap_err(), error, "{args:?}");
        }
    }

    #[test]
    fn extra_hosts_are_only_ever_given_one_by_one_for_http() {
        let v = absolute();
        let sh = "streamable-http";
        let ts = "i137.taild78809.ts.net:8443";
        assert!(parse(&["--vault", v, "--transport", sh])
            .unwrap()
            .allow_hosts
            .is_empty());
        let one = parse(&["--vault", v, "--transport", sh, "--allow-host", ts]).unwrap();
        assert_eq!(one.allow_hosts, [http::parse_allowed_host(ts).unwrap()]);
        let two = parse(&[
            "--allow-host",
            ts,
            "--vault",
            v,
            "--allow-host",
            "other.example:443",
            "--transport",
            sh,
        ])
        .unwrap();
        assert_eq!(two.allow_hosts.len(), 2);
        let many: Vec<String> = (1..=9).map(|n| format!("h{n}.example:443")).collect();
        let mut nine = vec!["--vault", v, "--transport", sh];
        for host in &many {
            nine.extend(["--allow-host", host.as_str()]);
        }
        assert_eq!(parse(&nine).unwrap_err(), ArgError::AllowHost, "at most 8");
        assert_eq!(parse(&nine[..nine.len() - 2]).unwrap().allow_hosts.len(), 8);
        for (args, error) in [
            (
                vec!["--vault", v, "--allow-host", ts],
                ArgError::AllowHostWithoutHttp,
            ),
            (
                vec!["--vault", v, "--transport", "stdio", "--allow-host", ts],
                ArgError::AllowHostWithoutHttp,
            ),
            (
                vec![
                    "--vault",
                    v,
                    "--transport",
                    sh,
                    "--allow-host",
                    ts,
                    "--allow-host",
                    ts,
                ],
                ArgError::AllowHost,
            ),
            (
                vec![
                    "--vault",
                    v,
                    "--transport",
                    sh,
                    "--allow-host",
                    ts,
                    "--allow-host",
                    "I137.taild78809.ts.net:8443",
                ],
                ArgError::AllowHost,
            ),
            (
                vec![
                    "--vault",
                    v,
                    "--transport",
                    sh,
                    "--allow-host",
                    "*.ts.net:8443",
                ],
                ArgError::AllowHost,
            ),
            (
                vec!["--vault", v, "--transport", sh, "--allow-host", "*"],
                ArgError::AllowHost,
            ),
            (
                vec![
                    "--vault",
                    v,
                    "--transport",
                    sh,
                    "--allow-host",
                    "i137.taild78809.ts.net",
                ],
                ArgError::AllowHost,
            ),
            (
                vec![
                    "--vault",
                    v,
                    "--transport",
                    sh,
                    "--allow-host",
                    "0.0.0.0:8443",
                ],
                ArgError::AllowHost,
            ),
            (
                vec!["--vault", v, "--transport", sh, "--allow-host"],
                ArgError::Usage,
            ),
        ] {
            assert_eq!(parse(&args).unwrap_err(), error, "{args:?}");
        }
    }
}
