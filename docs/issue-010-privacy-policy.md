# Issue #10 — Privacy Policy v1

## Scope and S0 findings

The Native Host crate now exposes a `policy` and `policy_store` library. The extension has
a pure `policy.js` evaluator. These APIs are **not connected** to event collection, queues,
image saving, native messaging dispatch, search results, or MCP. No GUI is added. Merely
starting the current application does not initialize or enforce this policy yet (#11/#12).
(Since #11, automatic recording enforces it; see `issue-011-privacy-ingress.md`.)

Inspection of the existing implementation:

- `main.rs`: `visits` and `tab_snapshots` use `event_id TEXT PRIMARY KEY`; saves use
  parameterized `INSERT OR IGNORE`. Re-delivery is idempotent by event ID, not URL.
- `images.rs`: separate `image_blobs`, `saved_images`, and `image_thumbnails` tables.
  This change adds no columns, migrations, backfills, or reclassification of stored rows.
- `event-queue.js`: pending and dead-letter events persist in `chrome.storage.local`.
  Policy enforcement before enqueueing and retry handling remain #11 work.
- `vault.rs`: lazy Vault selection from `config.json`, with `MEMOCAMPUS_DB` override;
  existing files open using READ_WRITE + NO_MUTEX (without CREATE). It checks schema,
  foreign keys, and rejects connections SQLite silently opened read-only. `search.rs`
  uses SELECTs but the shared Vault connection is not a dedicated read-only open.
- No application `journal_mode` assignment or dedicated READ_ONLY Vault open exists.
  This issue does not enable WAL. Local temporary-database probes and all existing
  tests are separate from network-Vault measurements. Network measurements and a WAL
  decision remain deferred; no real Vault or backup is opened for this work.

## Decisions fixed for v1

The current task authorizes resolving the Issue's open design choices safely. These choices
were presented before implementation:

- Global `%LOCALAPPDATA%\Memocampus\policy.json`, independent of Vault choice.
  `MEMOCAMPUS_APP_DIR` provides the same app-directory override as the existing Host.
  `MEMOCAMPUS_DB` has no effect on policy location. No executable-folder fallback.
- Explicit `PolicyStore::initialize()` only: no initialization on load, evaluate, or Host startup.
- `policy.initialized` is a separate, durable initialization-attempt marker. It is created and
  flushed **before** policy publication. A failed/interrupted initialization deliberately
  requires recovery; it is never silently retried with defaults.
- `policy.lock` provides OS advisory locking, released on process exit. Writers take an
  exclusive lock; readers take a shared lock by opening the existing file without creating
  anything. This prevents Windows delete-pending races during replacement. Writers do not
  delete the lock file. Contention returns `Busy`; callers can retry after reloading. The target is
  local Windows application storage, not a shared network policy directory.
  `policy.lock` の単独消失は異常状態として扱い、暗黙復旧せずfail-closedする。復旧は将来の明示的な管理操作で扱う。
- Same-directory exclusive temporary file, flush, then atomic replace (Windows
  `MoveFileExW(REPLACE_EXISTING | WRITE_THROUGH)`). The destination is never deleted first.
  Non-Windows uses rename plus directory sync. Failed writes clean up only their own temp.
  A crash may leave an ignored temp, but a successful read sees a complete policy; reads
  during a writer return `Busy` and evaluation fails closed.
- Update compares both revision and SHA-256 of the loaded bytes under the writer lock.
  The caller supplies the loaded revision; the Host increments it. Stale revisions, edits
  with the same revision, and exhausted revisions fail. Reload after an uncertain result.
- Missing/corrupt marker or policy, unreadable/oversized files, unknown versions or fields,
  invalid rules, and malformed URLs deny automatic recording and MCP eligibility. Files
  are preserved for recovery; a last-known-good cached policy is not used as fallback.

The marker detects ordinary policy-file loss. No local marker can distinguish first use
from deliberate removal of **all** application state. Even then reads remain fail-closed;
only an explicit initialization operation could create defaults. Recovery/reset tooling is
not included. Direct JSON edits and noncooperating writers are unsupported: hashing detects
edits preceding an update, but it is not a lock against an external writer during replacement.
This is crash-consistent local storage, not a guarantee against hardware/OS durability failures
or a malicious process running as the same user. Do not store this directory inside a Vault.

## Schema and initial settings

`extension/policy-defaults.json` is the bundled initial document; Rust includes those exact
bytes. `extension/policy-builtins.json` is the shared engine-v1 preset/detection data.
The browser uses the generated ordinary ES module `policy-builtins.js`, so the evaluator
does not require JSON module imports. `node scripts/generate-policy-builtins.mjs` regenerates
it, and an automatic test checks it against the canonical JSON.

- `schemaVersion: 1`, `engineVersion: 1`, `revision: 1..9007199254740991` (JS-safe integer).
- `layers.neverSave` and `layers.saveButNoMcp`: required `enabled` boolean and `rules` array.
  Each array allows at most 1,000 rules of at most 2,048 UTF-16 code units each.
- `categories`: required booleans `authentication`, `credentials`, `government`, `webmail`,
  `aiChats`. Initial values are all true; both layers start enabled, with empty user rules.
- A layer's switch disables its user rules and all builtins in that layer. Category switches
  disable only the corresponding builtins; user rules remain in force. Invalid input/config
  is always fail-closed even when both layers are disabled.
- A (`neverSave`) wins over B (`saveButNoMcp`). `none` means neither rule layer matches.
  A forbids **automatic** recording; this API does not cancel explicit manual image saving.
  It also must not be interpreted as permission to expose manual images to MCP.

Initial A categories: Google/Microsoft/OpenAI/Apple authentication hosts and recognized
authentication paths; credential-bearing URLs. Initial B categories: My Number / tax agency
domains (including subdomains), Google/Outlook webmail, ChatGPT/Claude/Gemini hosts. The exact
finite lists are in `policy-builtins.json`; they are presets, not exhaustive classifications.
Users can add rules to either layer and toggle categories/layers through the Host update API.
Deleting a user rule does not disable a builtin; disable that category explicitly. There are
no allow rules that override a matching deny rule. A later change to normalization or builtin
semantics requires an engine-version change and shared-vector review.

## URL and rule contract

Both implementations use URL parsers (Rust `url`, JavaScript WHATWG `URL`):

- Only absolute HTTP(S) URLs, or one `blob:` wrapper around HTTP(S), are accepted.
  Opaque/nested blob, other schemes, missing hosts, control characters, spaces, and backslashes
  are denied. Maximum URL length: 32,768 UTF-16 code units.
- Host case folds to lowercase; trailing ASCII dots are removed after parser IDN/punycode
  normalization. `www` is an ordinary subdomain, never removed. Parser IPv4/IPv6 canonicalization
  applies. Default ports disappear; nondefault ports remain in normalized output.
- Rules match all ports and both schemes. Port-qualified rules and IPv6 rule literals are
  deliberately unsupported in v1 (URLs with IPv6 remain valid). Domain and IPv4 rules work.
- `example.com`: exact host only. `*.example.com`: root plus dot-delimited descendants.
  `example.com/path/*`: exact host and `/path/` prefix, not bare `/path` or `/path-other`.
  A wildcard host may also have a path prefix. No regex, middle wildcards, userinfo, query,
  fragment, scheme, or port in a rule. Paths are case-sensitive.
- Percent-decode path once as strict UTF-8, including encoded slash; collapse dot segments.
  Invalid escapes/UTF-8, decoded controls/backslash/percent (including double encodings) are
  denied. This intentionally errs on the side of blocking ambiguous URLs. Query/fragment
  are separately validated and retained in normalized output, never appended to rule paths.
- Userinfo counts as credentials. Query/fragment parameter names are form-decoded and
  ASCII-case-insensitive. Hash-router query strings are inspected too. Recognized credential
  names (even with empty values) trigger A. `code` alone triggers only on known callback paths,
  or together with `state`. Values and free text are not scanned for words such as `token`.
- Known auth paths match exact path or `/`-delimited descendants, not arbitrary occurrences
  in article paths. The detector is a conservative finite preset, not a credential scanner
  capable of recognizing every site's proprietary URL format.

## API usage and integration boundary

Rust: `PolicyStore::from_process()` -> `load()` returns `Snapshot { policy, etag }`.
Edit a clone of `snapshot.policy` and pass `update(&snapshot, candidate)`; do not increment
revision yourself. `load()` returns structured errors; `store.evaluate(url)` converts a load
failure to `NeverSave`. `Policy::evaluate` validates before evaluating. Store reads are bounded
to 1 MiB and reject nonregular files/symlinks at the policy/marker leaf.

JavaScript: `validatePolicy(hostDocument)` and `evaluatePolicy(hostDocument, url)`; no file
writer or persistence API is provided. The Host must validate JSON before giving a document
to JS. `evaluatePolicy(null, url)` fails closed. Module loading in an actual extension
context and cache invalidation/Host protocol integration must be verified in #11. No manual
Chrome dogfood or enforcement claim is made by these unit tests.

## Verification

`tests/privacy/vectors.json` is consumed directly by both language test suites: decisions,
normalization, valid/invalid rule grammar, and malformed policy objects. Store tests use
dedicated randomly named temporary app directories, not the actual application settings.

Commands: `npm test`; in `native-host`, `cargo test --locked`, `cargo fmt --all -- --check`,
`cargo clippy --locked --all-targets -- -D warnings`, `cargo build --release --locked`.
CI and PO runtime confirmation are separate from local checks. No commit/push/PR is part of
this implementation task.

### Local S0 probe (2026-10-31)

A separate Python SQLite temporary-file probe reported journal mode `delete`, one row after
two `INSERT OR IGNORE` operations with the same ID, and a successful SELECT through an
explicit `mode=ro` connection. This is a local SQLite probe, not a measurement of the user's
Vault or the Rust bundled SQLite on a network share. No WAL switch was performed.
