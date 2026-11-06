//! The Streamable HTTP transport: `server::Memocampus`, the same handler as stdio, served by rmcp's
//! `StreamableHttpService` behind a gate on `http://127.0.0.1:<port>/mcp`.
//!
//! - Loopback only: the listen address must be `127.0.0.1` or `[::1]`, bound with
//!   `SO_EXCLUSIVEADDRUSE` on Windows so no other socket can share the port.
//! - The gate, before anything reaches the MCP layer: the path is `/mcp` without a query; `Host`
//!   names this server (`127.0.0.1:<port>` or `localhost:<port>`, against DNS rebinding, or exactly
//!   one of the `--allow-host` names with its port, none by default); a request
//!   carrying `Origin` is refused with 403 (no browser origin is allowed, and no CORS header is ever
//!   sent); `Authorization: Bearer <token>` matches (`token`), otherwise 401 before any tool runs.
//! - rmcp then applies the protocol itself: Accept / Content-Type, `MCP-Protocol-Version`, the
//!   2026-07-28 header and `_meta` checks, JSON-RPC errors. It is configured statelessly: no
//!   `MCP-Session-Id`, GET and DELETE answer 405, and each request is answered with one JSON object.
//! - Plain HTTP: TLS is for a reverse proxy in front of it (e.g. Tailscale Serve), whose public
//!   `<name>:<port>` must then be given with `--allow-host`.

use crate::diag;
use crate::server::Memocampus;
use crate::token::{self, Token};
use bytes::Bytes;
use http::{header, HeaderValue, Method, Request, Response, StatusCode};
use http_body_util::{combinators::BoxBody, BodyExt, Full};
use hyper::body::Incoming;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::server::graceful::GracefulShutdown;
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use serde_json::json;
use std::convert::Infallible;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::{TcpListener, TcpSocket};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

pub const DEFAULT_LISTEN: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8765);
pub const PATH: &str = "/mcp";
/// Tool arguments are a few hundred bytes; anything near this is not a request of this server.
const MAX_BODY_BYTES: usize = 64 * 1024;
/// Requests handled at once. More wait for a permit; a vault read is CPU- and I/O-bound, and the
/// handler is the same as for one stdio client.
const MAX_CONCURRENT_REQUESTS: usize = 4;
/// Open connections. A connection beyond this is closed at once.
const MAX_CONNECTIONS: usize = 32;
/// From receipt, including the wait for a permit. A read of 100k visits takes about 2 s.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);
/// Connections still open at shutdown are given this long to finish.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

type Body = BoxBody<Bytes, Infallible>;

/// `127.0.0.1:<port>` or `[::1]:<port>`, nothing else (no `0.0.0.0`, no other interface, no name).
pub fn parse_listen(text: &str) -> Option<SocketAddr> {
    let addr: SocketAddr = text.parse().ok()?;
    let loopback = match addr.ip() {
        IpAddr::V4(ip) => ip == Ipv4Addr::LOCALHOST,
        IpAddr::V6(ip) => ip == Ipv6Addr::LOCALHOST,
    };
    loopback.then_some(addr)
}

/// At most this many `--allow-host` values.
pub const MAX_ALLOWED_HOSTS: usize = 8;

/// A `Host` accepted in addition to the loopback ones, given explicitly with `--allow-host`: the
/// public name a TLS-terminating proxy in front of this server forwards (e.g. Tailscale Serve's
/// `<machine>.<tailnet>.ts.net:8443`). Exactly one DNS name and one port; there is no wildcard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowedHost {
    /// Lowercase.
    name: String,
    port: u16,
}

impl AllowedHost {
    pub fn authority(&self) -> String {
        format!("{}:{}", self.name, self.port)
    }
}

fn is_dns_label(label: &str) -> bool {
    (1..=63).contains(&label.len())
        && label
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !label.starts_with('-')
        && !label.ends_with('-')
}

/// `<dns name>:<port>`: at least two labels of `[a-z0-9-]` (case-insensitive), a top-level label
/// that is not all digits (so no IP address), no trailing dot, no `*`, and a port from 1 to 65535
/// written without a sign or leading zero. Anything else is refused, never repaired.
pub fn parse_allowed_host(text: &str) -> Option<AllowedHost> {
    let (name, port) = text.rsplit_once(':')?;
    let port_is_canonical = !port.is_empty()
        && port.len() <= 5
        && port.bytes().all(|byte| byte.is_ascii_digit())
        && !port.starts_with('0');
    if !port_is_canonical {
        return None;
    }
    let port: u16 = port.parse().ok()?;
    let name = name.to_ascii_lowercase();
    let labels: Vec<&str> = name.split('.').collect();
    let top = labels.last()?;
    let valid = name.len() <= 253
        && labels.len() >= 2
        && labels.iter().all(|label| is_dns_label(label))
        && !top.bytes().all(|byte| byte.is_ascii_digit());
    valid.then_some(AllowedHost { name, port })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListenError {
    AddressInUse,
    PermissionDenied,
    Other,
}

impl ListenError {
    pub fn code(self) -> &'static str {
        match self {
            Self::AddressInUse => "address_in_use",
            Self::PermissionDenied => "permission_denied",
            Self::Other => "listen_failed",
        }
    }
}

#[cfg(windows)]
fn exclusive(socket: &TcpSocket) -> std::io::Result<()> {
    use std::os::windows::io::AsRawSocket;
    use windows_sys::Win32::Networking::WinSock::{
        setsockopt, SOCKET, SOL_SOCKET, SO_EXCLUSIVEADDRUSE,
    };
    let on: i32 = 1;
    // SAFETY: a valid socket handle owned by `socket`, and a pointer to a live i32 of that size.
    let result = unsafe {
        setsockopt(
            socket.as_raw_socket() as SOCKET,
            SOL_SOCKET,
            SO_EXCLUSIVEADDRUSE,
            (&on as *const i32).cast(),
            std::mem::size_of::<i32>() as i32,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(windows))]
fn exclusive(_: &TcpSocket) -> std::io::Result<()> {
    Ok(())
}

pub fn bind(addr: SocketAddr) -> Result<TcpListener, (ListenError, std::io::Error)> {
    let classify = |error: std::io::Error| {
        let kind = match error.kind() {
            std::io::ErrorKind::AddrInUse => ListenError::AddressInUse,
            std::io::ErrorKind::PermissionDenied => ListenError::PermissionDenied,
            _ => ListenError::Other,
        };
        (kind, error)
    };
    let socket = if addr.is_ipv4() {
        TcpSocket::new_v4()
    } else {
        TcpSocket::new_v6()
    }
    .map_err(classify)?;
    exclusive(&socket).map_err(classify)?;
    socket.bind(addr).map_err(classify)?;
    socket.listen(64).map_err(classify)
}

fn plain(status: StatusCode, text: &'static str) -> Response<Body> {
    let mut response = Response::new(Full::new(Bytes::from_static(text.as_bytes())).boxed());
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

/// A JSON-RPC error without an `id`, as the specification allows for a refused request.
fn refused(status: StatusCode, message: &str) -> Response<Body> {
    let body = json!({"jsonrpc": "2.0", "error": {"code": -32600, "message": message}});
    let mut response = Response::new(Full::new(Bytes::from(body.to_string())).boxed());
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

#[derive(Clone)]
struct Gate {
    mcp: StreamableHttpService<Memocampus, NeverSessionManager>,
    token: Arc<Token>,
    port: u16,
    /// `--allow-host` values; empty unless given.
    extra_hosts: Arc<Vec<AllowedHost>>,
    permits: Arc<Semaphore>,
}

fn method_label(method: &Method) -> &'static str {
    match *method {
        Method::POST => "POST",
        Method::GET => "GET",
        Method::DELETE => "DELETE",
        Method::OPTIONS => "OPTIONS",
        Method::PUT => "PUT",
        Method::HEAD => "HEAD",
        _ => "other",
    }
}

impl Gate {
    fn host_is_allowed(&self, request: &Request<Incoming>) -> bool {
        let mut hosts = request.headers().get_all(header::HOST).iter();
        let (Some(host), None) = (hosts.next(), hosts.next()) else {
            return false;
        };
        let Some(authority) = host
            .to_str()
            .ok()
            .and_then(|text| text.parse::<http::uri::Authority>().ok())
        else {
            return false;
        };
        let name = authority.host().to_ascii_lowercase();
        // The port as written, in its one decimal spelling (no `08443` for 8443).
        let port = authority
            .port()
            .filter(|port| port.as_u16().to_string() == port.as_str())
            .map(|port| port.as_u16());
        let loopback =
            port == Some(self.port) && matches!(name.as_str(), "127.0.0.1" | "localhost" | "[::1]");
        loopback
            || self
                .extra_hosts
                .iter()
                .any(|allowed| allowed.name == name && Some(allowed.port) == port)
    }

    fn authorized(&self, request: &Request<Incoming>) -> bool {
        let mut values = request.headers().get_all(header::AUTHORIZATION).iter();
        let (Some(value), None) = (values.next(), values.next()) else {
            return false;
        };
        token::bearer(value.as_bytes()).is_some_and(|presented| self.token.matches(presented))
    }

    /// The response, and what the gate decided (for the log).
    async fn handle(&self, mut request: Request<Incoming>) -> (Response<Body>, &'static str) {
        if request.uri().path() != PATH || request.uri().query().is_some() {
            return (plain(StatusCode::NOT_FOUND, "Not Found"), "not_found");
        }
        if !self.host_is_allowed(&request) {
            return (
                refused(StatusCode::FORBIDDEN, "Forbidden: Host is not allowed"),
                "host_refused",
            );
        }
        if request.headers().contains_key(header::ORIGIN) {
            return (
                refused(StatusCode::FORBIDDEN, "Forbidden: Origin is not allowed"),
                "origin_refused",
            );
        }
        if !self.authorized(&request) {
            let mut response = refused(StatusCode::UNAUTHORIZED, "Unauthorized");
            response.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static("Bearer realm=\"memocampus\""),
            );
            return (response, "unauthorized");
        }
        // The token goes no further than this gate.
        request.headers_mut().remove(header::AUTHORIZATION);
        let served = tokio::time::timeout(REQUEST_TIMEOUT, async {
            let _permit = self.permits.acquire().await;
            self.mcp.handle(request).await
        })
        .await;
        match served {
            Ok(response) => (response, "served"),
            Err(_) => (
                plain(StatusCode::SERVICE_UNAVAILABLE, "Request timed out"),
                "timeout",
            ),
        }
    }
}

/// Resolves when the process is asked to stop: Ctrl+C, Ctrl+Break, or its console closing.
async fn stop_requested() -> &'static str {
    #[cfg(windows)]
    {
        use tokio::signal::windows;
        let (Ok(mut brk), Ok(mut close)) = (windows::ctrl_break(), windows::ctrl_close()) else {
            let _ = tokio::signal::ctrl_c().await;
            return "ctrl_c";
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => "ctrl_c",
            _ = brk.recv() => "ctrl_break",
            _ = close.recv() => "console_close",
        }
    }
    #[cfg(not(windows))]
    {
        let _ = tokio::signal::ctrl_c().await;
        "ctrl_c"
    }
}

/// Serves until a stop is requested; returns its reason.
pub async fn serve(
    handler: Memocampus,
    listener: TcpListener,
    token: Token,
    extra_hosts: Vec<AllowedHost>,
) -> &'static str {
    let port = listener.local_addr().map(|addr| addr.port()).unwrap_or(0);
    let cancel = CancellationToken::new();
    // rmcp checks `Host` again against the same list as the gate.
    let allowed_hosts = [
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        format!("[::1]:{port}"),
    ]
    .into_iter()
    .chain(extra_hosts.iter().map(AllowedHost::authority));
    let config = StreamableHttpServerConfig::default()
        .with_allowed_hosts(allowed_hosts)
        .enforce_origin_validation()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_sse_keep_alive(None)
        .with_sse_retry(None)
        .with_max_request_body_bytes(MAX_BODY_BYTES)
        .with_cancellation_token(cancel.clone());
    let mcp = StreamableHttpService::new(
        move || Ok(handler.clone()),
        Arc::new(NeverSessionManager::default()),
        config,
    );
    let gate = Gate {
        mcp,
        token: Arc::new(token),
        port,
        extra_hosts: Arc::new(extra_hosts),
        permits: Arc::new(Semaphore::new(MAX_CONCURRENT_REQUESTS)),
    };
    let connections = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let graceful = GracefulShutdown::new();
    let stop = stop_requested();
    tokio::pin!(stop);
    let reason = loop {
        let accepted = tokio::select! {
            reason = &mut stop => break reason,
            accepted = listener.accept() => accepted,
        };
        let (stream, peer) = match accepted {
            Ok(accepted) => accepted,
            Err(error) => {
                diag::event("http_accept_error", false, diag::io_error(&error));
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        // Bound to loopback, so this cannot fail; checked all the same.
        if !peer.ip().is_loopback() {
            continue;
        }
        let Ok(permit) = Arc::clone(&connections).try_acquire_owned() else {
            diag::event(
                "http_connection_refused",
                false,
                json!({"reason": "too_many"}),
            );
            continue;
        };
        let gate = gate.clone();
        let service = hyper::service::service_fn(move |request: Request<Incoming>| {
            let gate = gate.clone();
            async move {
                let started = Instant::now();
                let method = method_label(request.method());
                let (mut response, outcome) = gate.handle(request).await;
                let headers = response.headers_mut();
                headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
                headers.insert(
                    header::X_CONTENT_TYPE_OPTIONS,
                    HeaderValue::from_static("nosniff"),
                );
                diag::event(
                    "http_request",
                    outcome == "served" && response.status().is_success(),
                    json!({
                        "method": method,
                        "status": response.status().as_u16(),
                        "outcome": outcome,
                        "duration_ms": started.elapsed().as_millis(),
                    }),
                );
                Ok::<_, Infallible>(response)
            }
        });
        let connection = hyper::server::conn::http1::Builder::new()
            .timer(TokioTimer::new())
            .header_read_timeout(HEADER_READ_TIMEOUT)
            .serve_connection(TokioIo::new(stream), service);
        let connection = graceful.watch(connection);
        tokio::spawn(async move {
            let _ = connection.await;
            drop(permit);
        });
    };
    drop(listener);
    cancel.cancel();
    let drained = tokio::time::timeout(SHUTDOWN_GRACE, graceful.shutdown())
        .await
        .is_ok();
    diag::event("http_shutdown", drained, json!({"reason": reason}));
    reason
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_loopback_address_with_a_port_is_accepted() {
        assert_eq!(parse_listen("127.0.0.1:8765"), Some(DEFAULT_LISTEN));
        assert!(parse_listen("127.0.0.1:0").is_some());
        assert!(parse_listen("[::1]:8765").is_some());
        for bad in [
            "0.0.0.0:8765",
            "[::]:8765",
            "127.0.0.2:8765",
            "192.168.1.2:8765",
            "localhost:8765",
            "127.0.0.1",
            "8765",
            "",
            "127.0.0.1:99999",
        ] {
            assert_eq!(parse_listen(bad), None, "{bad}");
        }
    }

    #[test]
    fn an_allowed_host_is_one_exact_dns_name_and_port() {
        let host = |name: &str, port| AllowedHost {
            name: name.into(),
            port,
        };
        for (text, expected) in [
            (
                "i137.taild78809.ts.net:8443",
                host("i137.taild78809.ts.net", 8443),
            ),
            (
                "I137.TailD78809.TS.net:8443",
                host("i137.taild78809.ts.net", 8443),
            ),
            ("a-b.example:1", host("a-b.example", 1)),
            ("x.y:65535", host("x.y", 65535)),
        ] {
            assert_eq!(parse_allowed_host(text), Some(expected), "{text}");
        }
        let long_label = format!("{}.ts.net:8443", "a".repeat(64));
        let long_name = format!("{}ts.net:8443", "abcdefghi.".repeat(25));
        for bad in [
            "",
            "i137.taild78809.ts.net",
            "i137.taild78809.ts.net:",
            ":8443",
            "i137.taild78809.ts.net:0",
            "i137.taild78809.ts.net:08443",
            "i137.taild78809.ts.net:+8443",
            "i137.taild78809.ts.net:65536",
            "i137.taild78809.ts.net:99999",
            "i137.taild78809.ts.net:8443x",
            "i137.taild78809.ts.net:84 43",
            "i137.taild78809.ts.net:8443:8443",
            "*.ts.net:8443",
            "*:8443",
            "*.taild78809.ts.net:8443",
            "i137.*.ts.net:8443",
            "ts.net:*",
            "localhost:8443",
            "net:8443",
            "1.2.3.4:8443",
            "127.0.0.1:8443",
            "[::1]:8443",
            "i137.taild78809.ts.net.:8443",
            ".i137.taild78809.ts.net:8443",
            "i137..ts.net:8443",
            "host_name.ts.net:8443",
            "-a.ts.net:8443",
            "a-.ts.net:8443",
            "http://i137.taild78809.ts.net:8443",
            "i137.taild78809.ts.net:8443/mcp",
            "user@i137.taild78809.ts.net:8443",
            " i137.taild78809.ts.net:8443",
            "i137.taild78809.ts.net :8443",
            "ｉ137.taild78809.ts.net:8443",
            &long_label,
            &long_name,
        ] {
            assert_eq!(parse_allowed_host(bad), None, "{bad:?}");
        }
    }
}
