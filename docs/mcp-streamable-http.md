# MCP over Streamable HTTP (local, loopback only)

The same read-only MCP server as stdio ([issue-008-mcp-server.md](issue-008-mcp-server.md)), served over Streamable HTTP
on loopback. This is the base for a later HTTPS connection from ChatGPT; TLS, exposure to the internet, OAuth and client
registration are not part of it.

Markers: **[verified]** by an automated test or a measured run, **[code]** read in the code, **[inferred]** not tested.

## 1. Architecture

```text
                       ┌── stdio            observed::stdio()  (rmcp AsyncRwTransport)
server::Memocampus ────┤
 (4 tools, one         └── Streamable HTTP  http::serve(): gate → rmcp StreamableHttpService (stateless, JSON)
  VisibleStore)
```

- `server::Memocampus` (tools, argument checks, limits, answer shaping) is unchanged and is the only MCP core. stdio serves it
  directly. HTTP serves clones of it, which share one `Arc<VisibleStore>`, so IDs and cursors from one request work in the next
  [verified]. No tool code is duplicated, and there is no HTTP-specific read path: every call goes through VisibleStore
  and its policy gate, exactly as over stdio [verified: the same requests give equal answers over both, private rows absent].
- The protocol itself is rmcp 3.7.0's `StreamableHttpService` (feature `transport-streamable-http-server`), served by hyper 1
  (HTTP/1.1). Memocampus adds only the gate in front of it (`src/http.rs`) and the token (`src/token.rs`).
- New dependencies: hyper, hyper-util, http, http-body-util, bytes, getrandom 0.4 (already in the tree through uuid),
  tokio-util; rmcp's HTTP feature pulls in http-body, sse-stream, tower-service, rand, base64, async-trait.

## 2. Protocol versions

Both eras are served by one configuration:

| Client | Mechanism | Over HTTP |
|---|---|---|
| 2026-07-28 | no `initialize`; per-request `_meta` and `MCP-Protocol-Version` / `Mcp-Method` / `Mcp-Name` headers, checked against the body | served; mismatch or missing header → 400 `-32020` HeaderMismatch; unknown method → 404 `-32601` [verified] |
| 2025-11-25, 2025-06-18, 2025-03-26 (2024-11-05) | `initialize`, then `MCP-Protocol-Version` on later requests | served statelessly [verified] |

Why both: the latest specification (2026-07-28) removed sessions and `initialize`, but clients in use today (ChatGPT's
connectors, Codex, Claude, the MCP Inspector) still start with `initialize` [inferred from their documentation and the
Inspector run below]. rmcp negotiates per request, so supporting both costs nothing extra.

Stateless serving (`legacy_session_mode = false`) is valid for both eras: 2025-11-25 makes `MCP-Session-Id` a MAY and allows
405 for GET and DELETE; 2026-07-28 has no sessions and says to answer GET and DELETE with 405 and ignore `Mcp-Session-Id`.
So: no session ID is minted, a client's session ID is ignored, GET and DELETE answer 405 with `Allow: POST`, and every
request is answered with one `application/json` object (`json_response = true`; the tools send no progress notifications).
Notifications are answered 202 with no body [verified].

## 3. Starting it

```powershell
memocampus-mcp.exe --vault "C:\Users\<you>\AppData\Local\Memocampus\memocampus.sqlite3" `
  --transport streamable-http --listen 127.0.0.1:8765
```

- Without `--transport`, or with `--transport stdio`, the server is the stdio server exactly as before [verified: the stdio
  suite is unchanged and passes].
- `--listen` defaults to `127.0.0.1:8765`. Only `127.0.0.1:<port>` and `[::1]:<port>` are accepted; `0.0.0.0`, `[::]`, other
  addresses and host names refuse the start (exit 2). `--listen` without `--transport streamable-http` is an error, not a
  silent stdio start. Unknown or repeated options are errors. There is no option to disable authentication [verified].
- Port in use: exit 2, `memocampus-mcp: cannot listen: address_in_use`. On Windows the socket is bound with
  `SO_EXCLUSIVEADDRUSE`, so no other socket can share the port while the server runs [verified for a plain second bind].
- stderr names the token file and the address: `memocampus-mcp: bearer token created in …\mcp-http-token` /
  `… read from …`, then `memocampus-mcp: listening on http://127.0.0.1:8765/mcp`. stdout is unused in HTTP mode.
- Stop: Ctrl+C, Ctrl+Break or closing its console. It stops accepting, cancels rmcp's work, gives open connections 5 s, exits 0
  and prints `memocampus-mcp: stopped (<reason>)`. The port can be reused at once [verified with Ctrl+Break].

## 4. Authentication

- `Authorization: Bearer <token>` on every request; the scheme is case-insensitive. Missing, wrong, a different scheme, or more
  than one `Authorization` header → 401 with `WWW-Authenticate: Bearer realm="memocampus"`, before the request reaches rmcp or a
  tool [verified]. The header is removed from the request before it is handed to rmcp.
- The token: 32 bytes from the OS CSPRNG (`getrandom`), 64 lowercase hex characters, in `%LOCALAPPDATA%\Memocampus\mcp-http-token`
  (the policy's app directory; `MEMOCAMPUS_APP_DIR` when set). Created on the first HTTP start with `create_new`, never
  rewritten. A file that is not exactly one token (one trailing line break tolerated) refuses the start with
  `token_file_invalid` and is left as it is [verified]. To rotate: stop the server, delete the file, start it again.
- File permissions: the file inherits the ACL of `%LOCALAPPDATA%\Memocampus`, which on a standard profile is the user,
  SYSTEM and Administrators [inferred; no explicit ACL is set].
- Compared in constant time for the token length (no early exit) [code]. Never logged, never printed, never accepted in a URL:
  the endpoint is exactly `/mcp` with no query string, so `/mcp?access_token=…` is 404 [verified].
- Client setup: give the client the URL `http://127.0.0.1:8765/mcp` and the header `Authorization: Bearer <contents of the
  file>`. For Codex CLI, for example, `bearer_token_env_var` names an environment variable set from the file [inferred from its
  documentation; not run here].

## 5. Network and browser protection

- Listening on loopback only (§3); a connection from a non-loopback peer is dropped (cannot happen when bound to loopback).
- `Host` must be `127.0.0.1:<port>`, `localhost:<port>` or `[::1]:<port>` (case-insensitive, exactly one header, the port
  required and written without leading zeros) → otherwise 403 (DNS rebinding) [verified]. rmcp checks the same allowlist again.
- A TLS proxy in front (Tailscale Serve, for example) forwards its public `Host`, which is refused by default. It is accepted
  only when given explicitly, as one exact name and port per option (repeatable for different values, at most 8, HTTP only):

  ```powershell
  memocampus-mcp.exe --vault "<vault>" --transport streamable-http --listen 127.0.0.1:8765 `
    --allow-host i137.taild78809.ts.net:8443
  ```

  The value must be a DNS name of at least two labels of `[a-z0-9-]` (case-insensitive, stored lowercase), not an IP address,
  without a trailing dot, `*`, scheme, path or user, and a port from 1 to 65535 without sign or leading zero. Anything else,
  or the same value twice, refuses the start (exit 2) [verified]. There is no wildcard and no way to accept any Host: `*.ts.net`
  is refused, and `other.<tailnet>.ts.net:8443`, `<name>:443` or `<name>` without the port stay 403 [verified]. An accepted
  extra Host changes nothing else: `Origin` is still refused, and without the token the answer is 401 [verified]. stderr lists
  each accepted name (`also accepting Host …`); the log records only how many (`http_listening.extra_hosts`).
- `Origin`: any request carrying one is refused with 403 and a JSON-RPC error without `id`, whatever its value, before the
  token is checked [verified]. No browser origin is allowed and no CORS header is ever sent; a CORS preflight (`OPTIONS`) gets
  403 too. Requests without `Origin` (native clients, a reverse proxy) are allowed — the bearer token is what authorizes them.
  rmcp is also configured to refuse every `Origin` (a second layer).
- Responses carry `Cache-Control: no-store` and `X-Content-Type-Options: nosniff`.

## 6. Concurrency and limits

- One `VisibleStore` for the process, fixed vault path, as for stdio. Each call opens its own read-only SQLite connection
  and rebuilds the visible set; no connection is shared between requests or threads, so SQLite's thread rules are not in play
  [code]. A vault that is replaced or removed is read as it is at that call (the stdio tests cover this path).
- At most 4 requests are processed at once (others wait), at most 32 open connections (more are closed at once), 30 s per
  request including that wait (then 503), 10 s to read request headers, 64 KiB per request body (413) [verified: body limit,
  12 concurrent calls while the host writes 50 visits, integrity ok].
- A client that disconnects: rmcp cancels the handler; a VisibleStore read already running finishes in its blocking thread and
  holds no write lock [code].

## 7. Diagnostic log

The startup log (`issue-008-mcp-server.md` §2.1) records, in HTTP mode, `vault_init_start` with `transport`,
`http_token_ready` (`created`), `http_listening` (`port`), one `http_request` per request (`method`, `status`, `outcome`:
`served`, `unauthorized`, `origin_refused`, `host_refused`, `not_found`, `timeout`; `duration_ms`), `http_shutdown` and
`process_exit` with the stop reason. Never a header value, body, tool argument, token or path of the vault [verified].
Tool names are not logged over HTTP (the body is not inspected).

## 8. Tests

`tests/http_mcp.rs` (11 tests, the real process, synthetic vaults only): equal answers over HTTP and stdio for every tool
including errors, a cursor across requests, the legacy lifecycle without sessions, 2026-07-28 requests and header checks,
malformed requests (415, 406, 400, 413, -32601, bad arguments), the token (missing, wrong, short, other scheme, duplicated
header, in the query), Origin and Host refusals, loopback-only listening and a used port, an unusable token file, concurrent
requests during host writes, Ctrl+Break and restart on the same port. Unit tests for the token and the listen address.
Mutation run: 15 of 16 security mutants killed; the survivor removes rmcp's own Origin check, which the gate makes redundant.

## 9. Remaining risks

- Plain HTTP on loopback: any process of any user on this computer can connect, and only the token keeps it out. A process
  running as the same user can read the token file, as it can read the vault itself.
- The token is long-lived until the file is deleted; there is no expiry, no per-client token and no revocation list.
- No explicit ACL on the token file (inherited).
- No rate limit on failed authentication (a 256-bit token is not guessable; a local flood is bounded by the connection limit).
- rmcp answers unparsable JSON with 415 rather than 400 `-32700`; that is the SDK's behaviour and is kept.
- The Ctrl+Break test needs the test process to share a console with the server; whether GitHub's Windows runner provides
  one has not been checked.

## 10. Before HTTPS and ChatGPT

- TLS terminated by a reverse proxy or tunnel in front of this server, which keeps listening on loopback. The proxy's public
  `Host` is given with `--allow-host` (§5); the proxy must not add an `Origin` and must forward `Authorization`.
- ChatGPT's connectors authenticate with OAuth 2.1 (or no auth); a static bearer token is not what they send [inferred]. An
  authorization server, protected-resource metadata (RFC 9728) and audience-bound access tokens are needed; the bearer check in
  the gate is the place where a validated access token would be accepted.
- Exposure decisions: which tools are reachable remotely, rate limits, audit of remote calls, token rotation, and what happens
  to answers once they reach the AI provider.
