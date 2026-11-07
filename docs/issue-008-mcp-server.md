# Issue #8 — Slice 1: local read-only MCP server

Status: **implemented on `feature/8-local-mcp-server`; Codex review, PO dogfood and CI pending.**
Labels as in `issue-012-visible-store-design.md`: **[verified]** was run, **[code]** was read, **[inferred]** is reasoning.

```text
Claude Desktop / Claude Code
        │ MCP over stdio (JSON-RPC, one message per line)
        ▼
memocampus-mcp.exe --vault <path>      mcp-server/  (rmcp: protocol, schemas, stdio transport)
        │ typed calls only
        ▼
VisibleStore                            visible-store/  (privacy gate, read-only snapshot)
        │ SQLite, read-only, one short read transaction per call
        ▼
Vault (schema 7)  ◄── writes ── Native Host ◄── Native Messaging ── Chrome extension
```

## 1. Preflight

| Area | Finding | |
|---|---|---|
| Packages | Three independent Cargo packages (`native-host`, `visible-store`, now `mcp-server`), each its own workspace and lock file | [code] |
| VisibleStore | Already had `search_visits`, `get_visits_by_day` (fixed offset), `search_images`, `get_image_sources`; rebuilt per call from a fresh policy and a fresh snapshot, no cache; fixed error codes; no paging; no metadata | [code] |
| Schema 7 | `visits` (no incognito column; incognito is refused at ingress), `page_metadata` (one row per URL, the latest capture), `navigation_events` (Issue #41; not read by VisibleStore), `image_blobs` / `saved_images` (`mcp_public` per save, written 0), `image_thumbnails`, `image_tags` / `image_tag_links` (Issue #44; not read by VisibleStore) | [code] |
| Journal | The host never sets `journal_mode`: rollback journal (`delete`). VisibleStore **refuses WAL** (header, `-wal`/`-shm`, or mode), opens `SQLITE_OPEN_READ_ONLY`, `busy_timeout` 3 s, `query_only`, and never runs a PRAGMA that writes | [code][verified] |
| Policy | Global `%LOCALAPPDATA%\Memocampus\policy.json` (`MEMOCAMPUS_APP_DIR` for tests), read and compiled on every call, ETag re-checked after the build; any failure is `policy_unavailable` | [code] |
| Logging | VisibleStore has no logging call (source scan test) | [code] |
| Manual image publishing | Exists: the 保存履歴 checkbox (`image_mcp_public_set`, Issue #12 Slice 2/3) | [code] |

### SDK: `rmcp` 3.7.0 (official Rust SDK), pinned `=3.7.0`

- 3.8.0 is the latest (2026-10-26). 3.7.0 (2026-10-22) is the newest release at least two weeks old; 3.8.0 only adds an HTTP
  client error type and an SSE client fix, neither of which this server uses. Features: `server`, `macros`, `transport-io` only
  (no HTTP, no auth, no client).
- Protocol: the SDK knows `2024-11-05`, `2025-03-26`, `2025-06-18`, `2025-11-25` and `2026-07-28`. `initialize` answers the
  version the client asks for when known; otherwise the newest version that still has an `initialize` handshake
  (`2025-11-25`). So older and current Claude clients both connect [verified for each version over the real process; the actual
  Claude clients are for the PO's dogfood].
- It supplies the JSON-RPC parsing, the `tools/list` / `tools/call` routing, JSON Schema generation (`schemars` 1) for inputs and
  outputs, `structuredContent` with a text copy for clients that ignore it, and error mapping. No JSON-RPC code is written here.
- Runtime: Tokio, current-thread. VisibleStore reads block, so each runs in `spawn_blocking`.
- The SDK logs through `tracing`; no subscriber is installed, so it writes nothing anywhere.

## 2. Binary and startup

```powershell
memocampus-mcp.exe --vault "C:\Users\<you>\AppData\Local\Memocampus\memocampus.sqlite3"
```

- Exactly `--vault <absolute path>` (or `--help` / `--version`). A relative path is refused: an MCP client's working directory is
  not something to rely on.
- The vault is checked once at start with the same checks as a read (`VisibleStore::check_vault`): location (fixed local drive,
  no UNC), file header, not WAL, `journal_mode = delete`, schema 7 byte for byte. Failure: exit code 2 and one line on stderr,
  e.g. `memocampus-mcp: vault refused: vault_schema_mismatch`. Nothing is created: a missing vault stays missing [verified].
- The path is fixed for the life of the process; the Chrome side panel's active vault is not followed. Every call reads the file
  at that path again, so a file replaced there is read as the new file, and a removed one is an error (never an older answer).
- stdout carries MCP only. stderr gets nothing while running; the only output is that start-up line, which has a fixed code and
  never a URL, title or path. Tests assert an empty stderr after sessions with errors, CANARY rows and bad input [verified].
- Ends when the client closes stdin (exit 0) [verified]. Reads still running are abandoned; they hold no write lock.
- Start-up and connection events go to a diagnostic log file, never to stdout or stderr (§2.1).

### 2.1 Diagnostic log (`mcp-startup.log`)

Added to tell apart clients that start the server but fail the handshake (Codex Desktop: `connection closed: initialize
response`; ChatGPT Desktop: a console window that flashes) from clients that work (Codex CLI). It observes; it changes nothing
in the protocol.

- File: `%LOCALAPPDATA%\Memocampus\logs\mcp-startup.log` (`MEMOCAMPUS_APP_DIR\logs` when that is set, as for the policy).
  Never beside the executable, in the vault's folder or in the working directory; with no usable directory nothing is written.
- JSON Lines, one event per line: `ts` (UTC), `pid`, `elapsed_ms` (since process start), `event`, `ok`, then the event's fields.
  Several processes may append to one file; group by `pid`. Rotation: at about 5 MiB the file becomes `.1` (`.1` becomes `.2`,
  the old `.2` is deleted), so at most three files.
- Writing never affects the server: every I/O error is ignored, no lock is held across a write, and a failure to create the
  directory or the file leaves the session as it was [verified by test].

| Event | When |
|---|---|
| `process_start` | first line of `main` |
| `startup_context` | `mode` (`mcp`), `version`, `os`, `arch`, `exe`, `cwd`, `argc`, `env_set` (whether `LOCALAPPDATA` / `MEMOCAMPUS_APP_DIR` are set, never values), `log_dir_source`; on Windows `stdin`/`stdout`/`stderr` (`pipe`/`char`/`disk`/`none`), `console_window`, `console_processes`, `parent_pid`, `parent_name` |
| `vault_init_start` / `_success` / `_failure` | policy load and vault check; failure has `step` (`policy`/`vault_check`), `error` (the fixed code) and, for the vault, `vault_file` (`exists`, `is_file`, `len`, or the I/O `error_kind` and `os_error`), never the path |
| `mcp_transport_start` | just before the stdio handshake |
| `mcp_initialize_received` | `protocol_version`, `client_name`, `client_version` (reduced to `[A-Za-z0-9._@/+ -]`, 64 characters) |
| `mcp_initialize_response_flushed` | the initialize response was written to stdout and flushed: it is in the pipe. Whether the client read it is not observable, so this is not called "completed" |
| `mcp_initialize_response_failed` | that write failed: `error_kind`, `os_error` |
| `mcp_service_started` | rmcp's handshake returned and the service loop runs |
| `mcp_initialized` | `notifications/initialized` received |
| `mcp_request_received` / `mcp_notification_received` | the method name fixed by the protocol (`custom` for any other) |
| `mcp_tool_call` | `tool`: one of the four names, otherwise `other`. Never arguments |
| `mcp_error_response_sent` | a JSON-RPC error was sent: `code` only. Logged when the write completes, so it may follow later events |
| `stdout_write_failure` | any other write to stdout failed: `error_kind`, `os_error` |
| `stdin_eof` / `stdin_read_error` | stdin ended (`stdin_bytes` read in total) or failed (`error_kind`, `os_error`) |
| `mcp_receive_end` / `mcp_transport_close` | the transport delivered its end / was closed by rmcp |
| `process_exit` | normal exit: `reason` (`closed`, `cancelled`, `help`, `version`), `exit_code` 0 |
| `process_error` | exit code 2: `stage` (`arguments`, `vault_init`, `runtime`, `handshake`, `service`) and its `error`; for `handshake` the rmcp variant and step (e.g. `connection_closed` / `initialize request`); for a panic, `stage: panic`, the thread name and source location (never the message), before the previous panic hook runs |

Never logged: vault paths, URLs, titles, queries, tool arguments or results, message bodies, policy rules, environment values.
A process killed from outside (the client ending it) logs no exit event: its last line is the last thing it saw.

## 3. Privacy

VisibleStore is the only data path. `mcp-server` has no SQLite dependency (only its tests do, to build synthetic vaults) and calls
typed APIs; it cannot run SQL or see a connection.

- **Visits**: public only if the policy answers `None` for the URL (neither layer A nor B; an unparsable URL is NeverSave).
- **Page metadata**: attached to a visit only through its URL, so it follows the visit's decision. Text fields are published as
  stored, like titles (D7). `og_image_url` and `favicon_url` are given only if they are themselves public by the same policy and in
  the exact form the host writes (`page_metadata::clean_url`): an icon on a private host is not shown even on a public page.
  Text not in the host's form (`clean_text`) is absent, not repaired; a metadata row with a value of another type or a bad
  `captured_at` is left out whole, the visit stays.
- **Search, counts, paging**: all computed in the per-call `:memory:` set that holds public rows only. A private page whose only
  match is in its `og:description` changes no total, page or cursor [verified, differential test and CANARY E2E].
- **Domain counts**: `GROUP BY host` over the same set, so private visits are neither counted nor in the totals.
- **Images**: Issue #12 Slice 3, unchanged: a save is public only if `mcp_public = 1`, not incognito, metadata usable, and every
  judged source URL public; counts and dates are recomputed from public saves; SHA-256 is **not** published (D4). MIME type is now
  also searchable, still only over public saves in the range.
- **IDs**: `visit_`/`image_` + HMAC with a per-process random key. A private row has no ID, so an unknown and a private ID cannot
  differ. IDs and cursors are valid only for the running process.
- **Policy changes**: each call loads and compiles the policy again and re-checks its ETag after building the set. A change
  finished before a call applies to it. Invalid, missing, locked or uninitialized policy: `policy_unavailable` for every tool, no
  data; restoring the file restores answers on the next call [verified: Allow→SaveButNoMcp, Allow→NeverSave, invalid, deleted,
  restored, with the server running].
- **Errors**: `{"error":"<code>"}` with a fixed code, plus `"argument":"<name>"` for an argument the server refused. Argument type
  errors come from deserialization and quote only the client's own input. Nothing from the vault appears in an error.

What is **not** claimed: a public URL or title can still contain anything a site put there (D7, issue #14); metadata is the site's
own text. Data returned to a client leaves Memocampus: the client may send it to an AI provider.

## 4. Tools

Every tool is annotated `readOnlyHint: true`, `destructiveHint: false`, `idempotentHint: true`, `openWorldHint: false`, has
`additionalProperties: false` inputs, and an output schema. The server instructions and the descriptions say that page text is
untrusted data, that og/description text is the site's claim, and that no time spent is recorded.

Dates: `date_from` / `date_to` take either `YYYY-MM-DD` (a day in the local time zone of the computer running the server; as
`date_to` the whole day is included) or an exact UTC instant `YYYY-MM-DDTHH:MM:SS.sssZ` (`date_to` exclusive). The bounds used are
returned as `from` / `to`.

### `memocampus_search_visits`

In: `query` (required string, ≤ 200 characters, literal, ASCII case-insensitive; empty matches all), `date_from`, `date_to`,
`limit` (1–50, default 20), `cursor`.
Searched: URL, tab title, page title, description, og:title, og:description, og:site_name (not the image or icon URLs).
Out: `visits[]` of `{id, url, visited_at, title, page_title, description, og_title, og_description, og_site_name, og_image_url,
favicon_url, metadata_captured_at, truncated}`, `total`, `has_more`, `next_cursor`, `from`, `to`. Newest first, then ID.
Each visit is one page load; several visits of a URL are several results, each with that URL's latest metadata
(`metadata_captured_at` says when it was read). A visit without metadata has nulls.

### `memocampus_get_visits_by_day`

In: `date` (`YYYY-MM-DD`), `utc_offset` (optional, `+09:00`, `-12:00`…`+14:00`), `limit` (1–50, default 20), `cursor`.
Out: `date`, `utc_offset` used, `from`, `to` (UTC, half-open), `total` (public visits that day), `visits[]` as above,
`has_more`, `next_cursor`.

Time zone: the vault stores UTC instants (`toISOString`), so the day is a UTC range. Without `utc_offset` the offset is the one in
effect on this computer at the start of that date, from Windows' time zone settings (`TzSpecificLocalTimeToSystemTime`), never a
fixed JST. On a day when daylight saving time changes, the end of the day is taken with the offset of its start (one hour off at
one end); Japan has no DST. A test converts each computed midnight back with the opposite Windows function [verified].

### `memocampus_get_domain_stats`

In: `date_from`, `date_to`, `limit` (1–100, default 20).
Out: `domains[]` of `{domain, visit_count}` most visited first (then by name), `total_visits` (all public visits in the period),
`total_domains`, `has_more`, `from`, `to`.
`domain` is the host as the policy normalizes it (`policy::normalize_url`): lower case, trailing dot removed, port excluded.
Subdomains are separate; no eTLD+1 grouping. Every visit counts, including repeated loads of a URL. These are page loads, not
time; the description tells the model not to state durations.

### `memocampus_search_images`

In: `query` (optional, ≤ 200, literal, in image/page/frame URL or MIME type of a public save), `date_from`, `date_to` (save time),
`limit` (1–50, default 20).
Out: `images[]` of `{id, mime_type, width, height, public_save_count, first_saved_at, last_saved_at, latest_image_url,
latest_page_url}`, `total`, `has_more`. No image data. With no save marked public, the answer is empty, which is correct.

### Limits

| | Value | Why |
|---|---|---|
| `query` | 200 characters | VisibleStore's limit |
| `limit` | visits 1–50 (default 20), domains 1–100 (20), images 1–50 (20) | A 50-visit page with full metadata measured 57 KB |
| URL, tab title in an answer | 2000 characters, then cut, `truncated: true` | Stored without a limit; metadata already has the #22 limits |
| One answer's items | ≤ 200 KiB serialized; the page ends early, `has_more` and `next_cursor` continue it; one item always fits (worst case ≈ 35 KB) | Keeps tool results usable by a client |
| `cursor` | ≤ 200 characters, else `invalid_cursor` | A cursor is about 90 |
| Incoming line | 16 MiB (SDK default) | |

### Paging

A cursor is `<visited_ms>.<visit id>.<MAC>` with an HMAC under the process key: the position after the last returned visit in the
order (time descending, ID). It names a position, not a snapshot: the next page is read with the policy and data of its own time,
so a row that became private is simply not there, and nothing private can be reached through it. `total` is always the whole
match. A cursor that was altered, has another spelling, or comes from another server process is `invalid_cursor`, checked before
the vault or policy is read [verified]. Not bound to the query: using it with another query just starts that query at that
position.

## 5. VisibleStore changes

- `VisibleVisit.page: Option<VisiblePage>`; the snapshot reads `visits LEFT JOIN page_metadata ON url`.
- `SearchVisits` and `VisitsByDay` take `cursor`; `VisitPage.has_more` now means "after this page"; `cursor_after(&visit)`.
- `domain_stats(&DomainStats{from, to, limit}) -> DomainStatsPage{items, total_visits, total_domains, has_more}`.
- `check_vault()`: the read's checks without reading rows.
- `VisibleError::InvalidCursor` (`invalid_cursor`).
- The search filter also covers the five metadata text fields; image search also covers the MIME type.

Changed existing test: `page_metadata_never_reaches_a_reader_and_a_vault_without_it_is_refused` asserted the #22 state (no reader of
metadata). It became `page_metadata_reaches_a_reader_only_with_a_public_visit_and_a_vault_without_it_is_refused`: private rows'
CANARY metadata still never appears or matches, public rows now carry theirs, and the schema 4 refusal part is kept. Struct literals
in tests gained `cursor: None` / `page: None`.

## 6. SQLite and concurrency

- No WAL: the host uses a rollback journal and VisibleStore refuses WAL, so there is no checkpoint to manage and the server
  never runs `PRAGMA journal_mode`. A WAL vault is refused at start (`vault_unsupported`) and not converted [verified].
- One read transaction per call: open read-only, `BEGIN`, read, `COMMIT`, close, then judge with no lock held. The shared lock
  pins one committed state, so no partial transaction is seen [verified: totals of a writer committing pairs are always even and
  never decrease].
- `busy_timeout` 3 s, like the host. A lock shorter than that is waited for; a longer one is `vault_unavailable` for that call,
  and the next call works [verified with an EXCLUSIVE lock of 0.8 s and 5 s].
- A session leaves the vault byte-identical with the same mtime and no file beside it [verified]; killing the server mid-call
  leaves it intact (`integrity_check` ok) [verified].
- Measured with the release Native Host process per visit (as Chrome runs it) while the server reads, see §8.

## 7. Tests

| Suite | Count | |
|---|---|---|
| `mcp-server` unit | 9 | dates, offsets, local zone round trip, cutting, error shape, tool annotations and descriptions, arguments |
| `mcp-server` E2E (`tests/stdio_mcp.rs`, real process, synthetic vault/policy in temp dirs) | 16 | below |
| `visible-store` | 92 unit (+12 new, 1 changed), 1 process, 7 doc | |

E2E: version negotiation for five versions and `tools/list`; malformed lines (parse error with null id, server keeps going),
unknown method and tool, every argument type and value error; empty results; start-up refusal (missing, schema 4, WAL, junk,
bad arguments; nothing created); search by URL, title and each metadata field, Japanese, wildcards literal, 200/201 characters;
repeated URL; day bounds at the exact millisecond for `+09:00`, `-05:00` and the local zone, dates as range bounds; cursor
paging (45 rows in 3 pages, no gaps or repeats), foreign cursor, size budget with 30 oversized rows; domain counts with
subdomains, repeats and range; CANARY privacy across all four tools including an og:description-only match and a mixed image
blob, checked in every answer, raw stdout line and stderr; policy hot reload and fail-closed; vault unchanged; vault removed,
replaced with junk, restored; host-like writer during reads; short and long locks; four concurrent calls; stdin close and kill.

Mutation run outside the repository, each mutant alone, then restored: **21/21 killed** (13 in VisibleStore: metadata URL and
text checks, metadata search, cursor order, total, MAC and spelling, domain, join key, vault check, MIME search, day cursor; 8 in
the server: size budget, cutting, `has_more` after a budget cut, upper date bound, local offset sign, start-up check, limit and
query checks).

## 8. Measurements

Release builds, one Windows 11 machine, local disk, vault created and written by the release Native Host, rows seeded in the
host's form: 1/3 with metadata, 1/10 private, default policy. Median of 5 calls through MCP (stdio round trip included).
**Single-machine figures, not guarantees.**

| Rows | search (scan) | search (metadata term) | day | domain stats | images (none public) |
|---|---|---|---|---|---|
| 10,000 | 155 ms | 164 ms | 145 ms | 167 ms | 3 ms |
| 100,000 | 1.81 s | 1.92 s | 1.68 s | 1.70 s | 3 ms |

- While the server read continuously, 150 visits through the real host (one process each) all succeeded: median 28–30 ms, slowest
  63 ms at 10k rows and 468 ms at 100k rows (waiting for the reader's shared lock), well inside the host's 3 s busy timeout. The
  server's totals never decreased, and all 150 were visible afterwards.
- Bottleneck: every call rebuilds the public set (judging every row with the compiled policy and copying it into memory); a 100k
  vault costs about 1.7 s per call regardless of the query. The lock is held only for the read part. A cache keyed on the vault
  and policy is the next optimization and must satisfy `issue-012` §7's conditions. Not adopted here.

## 9. PO dogfood procedure (Windows)

Use your own vault only by hand; the automated tests never read it.

1. Build (no need to close Chrome; this does not touch the host executable):
   ```powershell
   cd C:\works\Memocampus\mcp-server
   cargo build --release --locked
   ```
   The server is `C:\works\Memocampus\mcp-server\target\release\memocampus-mcp.exe`.
2. Find the vault path: the side panel's **Vault** section shows it; by default
   `C:\Users\<you>\AppData\Local\Memocampus\memocampus.sqlite3`. It must be a schema 7 vault.
3. Check it starts (it waits for a client; press Ctrl+C to stop):
   ```powershell
   & "C:\works\Memocampus\mcp-server\target\release\memocampus-mcp.exe" --vault "$env:LOCALAPPDATA\Memocampus\memocampus.sqlite3"
   ```
   An immediate exit with `memocampus-mcp: vault refused: <code>` means: `vault_unavailable` (path wrong or missing),
   `vault_schema_mismatch` (not a schema 7 Memocampus vault), `vault_unsupported` (WAL, or not on a fixed local drive).
4. Register with **Claude Code** (user scope, so it works from any folder):
   ```powershell
   claude mcp add --scope user memocampus -- "C:\works\Memocampus\mcp-server\target\release\memocampus-mcp.exe" --vault "C:\Users\<you>\AppData\Local\Memocampus\memocampus.sqlite3"
   claude mcp list
   ```
   In a session, `/mcp` should show `memocampus` connected with 4 tools.
   Or **Claude Desktop**: Settings → Developer → Edit Config (`%APPDATA%\Claude\claude_desktop_config.json`), then restart it:
   ```json
   {
     "mcpServers": {
       "memocampus": {
         "command": "C:\\works\\Memocampus\\mcp-server\\target\\release\\memocampus-mcp.exe",
         "args": ["--vault", "C:\\Users\\<you>\\AppData\\Local\\Memocampus\\memocampus.sqlite3"]
       }
     }
   }
   ```
   Do not set `MEMOCAMPUS_APP_DIR`: without it the server uses the same policy as the extension (`%LOCALAPPDATA%\Memocampus`).
5. Ask, for example: 「最近どんなサイトを見てる？」 (expect `memocampus_get_domain_stats` and/or `memocampus_get_visits_by_day`),
   「今日の閲覧履歴を見せて」, 「"<a word from an og:description>" を含むページを探して」.
6. Diagnose: tool errors are `{"error":"<code>"}`. `policy_unavailable`: initialize or repair the policy in the side panel (it
   recovers on the next call). `vault_unavailable` during use: the vault was moved, or the host held a lock over 3 s; retry.
   Claude Desktop keeps each server's stderr under `%APPDATA%\Claude\logs\` (`mcp-server-memocampus.log`) [inferred from the
   client's documented behaviour; steps 4–6 were not run here, since they need your real vault and client].
   The server's own start-up and connection events are in `%LOCALAPPDATA%\Memocampus\logs\mcp-startup.log` (§2.1).

### Checklist

1. The client lists the server as connected.
2. Four tools: `memocampus_search_visits`, `memocampus_get_visits_by_day`, `memocampus_get_domain_stats`, `memocampus_search_images`.
3. A URL search returns visits.
4. A search for a word only in a page's og:description finds it.
5. Today's visits are listed, with the right local day boundaries.
6. Domain counts appear, and the answer does not talk about time spent.
7. Pages in a never-save or save-but-no-MCP rule (e.g. add a rule for a site you visited, then ask again) are absent from every
   answer and count, from the next question on.
8. Images: with nothing marked in 保存履歴 the image search is empty; marking one save of a public page makes it appear with
   that save only.
9. Browse in Chrome while asking: visits keep being recorded (no recording errors in the extension badge) and appear in answers.
10. After closing the client, the server process is gone and the extension still records into the vault.

## 10. Not done (later slices)

Streamable HTTP (since added on loopback: [mcp-streamable-http.md](mcp-streamable-http.md)), OAuth, any non-loopback network
listener; `get_image_sources` as a tool (VisibleStore has it); page text / MHTML; FTS5,
embeddings, summaries; a cache of the visible set; IANA zone names and DST-exact day ends; stable IDs across restarts; eTLD+1
grouping; SHA-256 in image results (refused by D4). For Slice 2, `server::Memocampus` is transport-independent: the same handler
can be served by rmcp's Streamable HTTP service without touching the tools.
