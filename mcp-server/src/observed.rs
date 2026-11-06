//! The stdio transport, observed for diagnostics (`diag`) without changing a byte of it: every
//! message passes through as it is, and only its kind is logged — the method, the tool name when it
//! is one of this server's tools, the protocol version and client name of `initialize`, and
//! the code of a JSON-RPC error. Never a message's arguments or results.

use crate::diag;
use rmcp::model::{ClientNotification, ClientRequest, JsonRpcMessage, ServerResult};
use rmcp::service::{RxJsonRpcMessage, TxJsonRpcMessage};
use rmcp::transport::async_rw::AsyncRwTransport;
use rmcp::transport::Transport;
use rmcp::RoleServer;
use serde_json::json;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, ReadBuf};

/// The tools a `tools/call` may name in a log line. Any other name is logged as `other`.
pub const TOOL_NAMES: [&str; 4] = [
    "memocampus_search_visits",
    "memocampus_get_visits_by_day",
    "memocampus_get_domain_stats",
    "memocampus_search_images",
];

/// stdin, with its end and its read errors logged once. Reads are passed through unchanged.
pub struct ObservedStdin<R> {
    inner: R,
    bytes: u64,
    ended: bool,
}

impl<R: AsyncRead + Unpin> AsyncRead for ObservedStdin<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let wanted = buf.remaining() > 0;
        let this = &mut *self;
        let polled = Pin::new(&mut this.inner).poll_read(cx, buf);
        match &polled {
            Poll::Ready(Ok(())) => {
                let read = buf.filled().len() - before;
                this.bytes += read as u64;
                if wanted && read == 0 && !this.ended {
                    this.ended = true;
                    diag::event("stdin_eof", true, json!({"stdin_bytes": this.bytes}));
                }
            }
            Poll::Ready(Err(error)) if !this.ended => {
                this.ended = true;
                diag::event(
                    "stdin_read_error",
                    false,
                    diag::with(json!({"stdin_bytes": this.bytes}), diag::io_error(error)),
                );
            }
            _ => {}
        }
        polled
    }
}

pub struct Observed<T> {
    inner: T,
}

pub fn stdio() -> Observed<impl Transport<RoleServer, Error = io::Error>> {
    let (stdin, stdout) = rmcp::transport::stdio();
    let stdin = ObservedStdin {
        inner: stdin,
        bytes: 0,
        ended: false,
    };
    Observed {
        inner: AsyncRwTransport::<RoleServer, _, _>::new_server(stdin, stdout),
    }
}

enum Outgoing {
    InitializeResult,
    Error(i32),
    Other,
}

fn outgoing(item: &TxJsonRpcMessage<RoleServer>) -> Outgoing {
    match item {
        JsonRpcMessage::Response(response)
            if matches!(response.result, ServerResult::InitializeResult(_)) =>
        {
            Outgoing::InitializeResult
        }
        JsonRpcMessage::Error(error) => Outgoing::Error(error.error.code.0),
        _ => Outgoing::Other,
    }
}

fn incoming(message: &RxJsonRpcMessage<RoleServer>) {
    match message {
        JsonRpcMessage::Request(request) => match &request.request {
            ClientRequest::InitializeRequest(init) => {
                let params = &init.params;
                diag::event(
                    "mcp_initialize_received",
                    true,
                    json!({
                        "protocol_version": diag::client_text(&params.protocol_version.to_string()),
                        "client_name": diag::client_text(&params.client_info.name),
                        "client_version": diag::client_text(&params.client_info.version),
                    }),
                );
            }
            ClientRequest::CallToolRequest(call) => {
                let name = TOOL_NAMES
                    .into_iter()
                    .find(|tool| *tool == call.params.name)
                    .unwrap_or("other");
                diag::event("mcp_tool_call", true, json!({"tool": name}));
            }
            ClientRequest::CustomRequest(_) => {
                diag::event("mcp_request_received", true, json!({"method": "custom"}));
            }
            // A method name fixed by the protocol.
            other => {
                diag::event(
                    "mcp_request_received",
                    true,
                    json!({"method": other.method()}),
                );
            }
        },
        JsonRpcMessage::Notification(notification) => {
            let method = match &notification.notification {
                ClientNotification::InitializedNotification(_) => {
                    diag::event("mcp_initialized", true, json!({}));
                    return;
                }
                ClientNotification::CancelledNotification(_) => "notifications/cancelled",
                ClientNotification::ProgressNotification(_) => "notifications/progress",
                ClientNotification::RootsListChangedNotification(_) => {
                    "notifications/roots/list_changed"
                }
                _ => "custom",
            };
            diag::event("mcp_notification_received", true, json!({"method": method}));
        }
        JsonRpcMessage::Response(_) | JsonRpcMessage::Error(_) => {
            diag::event("mcp_client_reply_received", true, json!({}));
        }
    }
}

impl<T: Transport<RoleServer, Error = io::Error>> Transport<RoleServer> for Observed<T> {
    type Error = io::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleServer>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        let kind = outgoing(&item);
        let sent = self.inner.send(item);
        async move {
            let result = sent.await;
            match (&kind, &result) {
                // The transport resolves once the line is written and stdout flushed: the bytes are
                // in the pipe. Whether the client read them is not observable here.
                (Outgoing::InitializeResult, Ok(())) => {
                    diag::event("mcp_initialize_response_flushed", true, json!({}))
                }
                (Outgoing::InitializeResult, Err(error)) => diag::event(
                    "mcp_initialize_response_failed",
                    false,
                    diag::io_error(error),
                ),
                (Outgoing::Error(code), sent) => diag::event(
                    "mcp_error_response_sent",
                    sent.is_ok(),
                    json!({"code": code}),
                ),
                (Outgoing::Other, Err(error)) => {
                    diag::event("stdout_write_failure", false, diag::io_error(error))
                }
                (Outgoing::Other, Ok(())) => {}
            }
            result
        }
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleServer>> {
        let message = self.inner.receive().await;
        match &message {
            Some(message) => incoming(message),
            None => diag::event("mcp_receive_end", true, json!({})),
        }
        message
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        diag::event("mcp_transport_close", true, json!({}));
        self.inner.close().await
    }
}
