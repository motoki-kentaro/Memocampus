# Issue #25 — Privacy Policy engine

Status: **Slices 1 to 5 merged; Slice 6 implemented, for review.** Version 2 has patterns with exact paths and a wildcard in the last path segment, path rules also meet the path with its slashes collapsed, and regex rules meet the whole URL. The host offers messages to validate, test and apply a draft and to start a new install; no settings UI exists.
Labels: **[verified]** was run, **[code]** was read in the source, **[inferred]** is reasoning not run.

## Slice 1: what changed

**Policy format: unchanged.** `schemaVersion` and `engineVersion` stay 1, the file format and the rule grammar are as in
`issue-010-privacy-policy.md`, and the meaning of every existing rule is the same.

Three deliberate changes, all of which refuse more and none of which allow more:

1. **Credential names behind a question mark (Rust and JS).** What follows the last `?` of a parameter name counts, so
   `?token`, `??token`, `???token` and `a?token` are all `token`, in the query and in the fragment, and also when the `?` is
   written `%3F`. Before, Rust allowed `https://example.com/??token=x` and `#??token=x`, while JS refused them. Since the
   MCP exit judges with Rust alone, such an image URL with its flag set would have been published. JS also allowed `???token`
   and `?a?token`, as did Rust. A `?` in a value (`?q=a?token`) still means nothing.
2. **Invalid Punycode (JS only).** The URL parser in Node passes an ASCII `xn--` label unchecked; Rust's parser decodes and
   validates it, so `https://xn--a.example.com/` was refused only by Rust. JS now decodes each such label and requires its
   Unicode form to parse back to the same label. Rust was not relaxed.
3. **A host kept with an escape (JS only).** Chrome accepts `https:///a%20b` with the host `a%20b`, which Rust refuses. JS now
   refuses any host containing `%`.

## CompiledPolicy

```rust
pub struct CompiledPolicy {
    blind: Zone,        // user rules of neverSave and, if enabled, authentication hosts
    auth_paths: bool,
    credentials: bool,
    private: Zone,      // user rules of saveButNoMcp and, if enabled, government, webmail, AI chats
}
struct Zone {
    exact: HashMap<String, Vec<Option<String>>>,     // host -> path prefixes; None is the whole host
    wildcard: HashMap<String, Vec<Option<String>>>,
}
```

- **Only by `CompiledPolicy::compile(&Policy)`.** It checks versions and limits and parses every rule, including those of a
  disabled layer. One invalid rule refuses the whole policy; no rule is skipped.
- **Immutable.** A changed policy means compiling again.
- **Judging** normalizes the URL once, then looks up the exact host and each suffix after a dot. These are exactly the suffixes
  on a label boundary, so the result equals the former `ends_with(".host")`. There is no substring match. Only the rules of the
  URL's host and parent domains are visited. Rules are never parsed while judging.
- **Order** is unchanged: blind rules, authentication paths, credentials, then private rules. An invalid URL is `NeverSave`
  whatever the layers say.
- `Policy::evaluate` remains for one URL, and compiles once per call.

### Unit and lifetime

| Where | Compiled | Lives |
|---|---|---|
| VisibleStore | Once per read attempt, right after the policy is loaded and before the vault is opened | That attempt |
| Native Host | Once per event (`PolicyGate::decide`), as it reads the file per event | That decision |
| Extension | None | Since Slice 2 it judges nothing (below) |

If the ETag differs after the set is built, VisibleStore loads and compiles the new policy and builds again, up to 3 attempts as
before. A policy that does not compile is `policy_unavailable`. Nothing is cached across calls, so there is no older set or
policy to fall back on.

## Equivalence with the former evaluator

- **In the repository:** `native-host/src/policy/tests.rs` keeps the former evaluator and compares it with the compiled one over
  generated URLs, for each rule, each category switch and each layer switch. The only allowed difference is the deliberate one,
  and only towards `NeverSave`. `tests/privacy/vectors.json` gained cases and no existing case changed.
- **Once, outside the repository, 83,534 synthetic URLs with the default and a custom policy [verified]:** the former and the
  new Rust evaluator differ on 2,778 decisions, every one towards `NeverSave` and every one on a URL with such a name. **Loosened:
  0.**

| Compared with new Rust | Refused only by Rust | Refused only by JS |
|---|---|---|
| Chrome 156, headless, empty temporary profile | **0** | 195 URLs, for example a `*` in the host |
| Node 24 | 4 URLs, such as `xn--lrf.example.com` | 10 URLs, such as `xn--en7f.example.com` |

The extension runs in Chrome, where nothing is allowed that Rust refuses in this corpus. The four in Node are invalid labels
that Node's tables accept, and Chrome refuses them too. Where only JS refuses, a visit is not recorded, which is the safe side.
This comparison is not part of CI, as CI has no Chrome step; the shared vectors are.

## Measurements

Image read through VisibleStore, former and new code in the same session, same `perf_images` fixture, release build, median of 5,
synthetic vault, sequential runs on one Windows 11 machine [verified]. Lock is the time up to closing the vault. Judge and build is judging
plus the memory database, and is not split further. Memory database size was the same for both.

| Saves | Public | Extra rules | Total, former | Total, new | Judge and build, former | Judge and build, new | Lock, former | Lock, new | Set |
|---|---|---|---|---|---|---|---|---|---|
| 10,000 | 50% | 0 | 188 ms | 66 ms | 174 ms | 52 ms | 7.3 ms | 7.6 ms | 1.7 MiB |
| 10,000 | 50% | 100 | 2.80 s | 66 ms | 2.77 s | 51 ms | 13 ms | 7.1 ms | 1.7 MiB |
| 10,000 | 50% | 999 | **23.9 s** | **66 ms** | 23.9 s | 50 ms | 8.2 ms | 7.1 ms | 1.7 MiB |
| 100,000 | 0% | 0 | 89 ms | 77 ms | 11 ms | 11 ms | 78 ms | 66 ms | 0 MiB |
| 100,000 | 50% | 0 | 3.71 s | 696 ms | 3.39 s | 547 ms | 132 ms | 69 ms | 17.4 MiB |
| 100,000 | 100% | 0 | 6.95 s | 2.68 s | 6.48 s | 2.22 s | 140 ms | 135 ms | 34.1 MiB |

Time no longer grows with the number of rules in these runs. These rules share one form and match nothing, so many path rules on one
host would cost more; that case was not measured. Host writes did not fail in any run, and the slowest commit was 155 ms or less with the
new code, 295 ms with the former. The remaining time at 100,000 public rows is mostly the memory database [inferred from 1.8 µs per URL].


Compiled size and time, by allocator count and clock on one machine [verified]:

| Rules | Compile | Compiled size | `CompiledPolicy::evaluate` per URL |
|---|---|---|---|
| 0 | 9.7 µs | 4.6 KiB | 1.8 µs |
| 100 | 90 µs | 19 KiB | 1.8 µs |
| 999 | 0.88 ms | 209 KiB | 1.8 µs |

Those URLs match no rule and have one host. Memory of the raw rows during a read was not measured.

## Remaining risks

- **The extension was not faster in this slice.** `evaluatePolicy` cost about 4 ms per URL at 999 rules [verified in Slice 0]. Slice 2 took it
  out of recording.
- **Credential detection stays a finite list of names.** A name such as `my_token` is not detected.
- **New false refusals are possible but narrow:** a parameter really named `a?token` is refused.
- **Node and Chrome differ,** so passing vectors in Node does not prove Chrome's behaviour. The Chrome comparison was run once by
  hand, and Chrome versions change.
- **Gaps found in Slice 0 are unchanged by design:** `example.com//private/a` does not match `example.com/private/*`, and rules such
  as `.example.com` are accepted though they match no ordinary URL.
- Host and VisibleStore compile per use, so 999 rules cost about 1 ms per decision or read. Not a problem at this size.

## Handoff to Slice 2

1. Where a URL is judged (host or extension) is a PO decision from Slice 0. `CompiledPolicy` is ready for a batch decision message.
2. If JS stays a judge, compile there too, and add the Chrome comparison to a repeatable check.
3. `Zone` is the place for the later syntax. Keep the path list per host, and do not fall back to a scan over all rules.
4. Any later change to these semantics needs an engine version, as `issue-010-privacy-policy.md` requires.

## Slice 2: one judge

The policy's meaning, file format and rule grammar did not change. Only who judges did.

### Trust boundary

```text
Extension (no rule, no evaluator)              Native Host
  candidate URL, in memory only   ───────────▶  CompiledPolicy, once per batch
  never_save or no complete answer: forgotten
  allow, save_but_no_mcp          ◀───────────  { id, decision } only
        │
  chrome.storage queue
        │  event
        ▼                                       judged again against the file, then Vault
```

The extension trusts the host's answer, and the host does not trust the extension: an allow is no permit. `extension/policy.js` stays
for the shared vectors and as the tests' fake host; a test refuses any import of it from another extension file.

**A new exposure, decided by the PO:** a URL that is refused now reaches the local host process, in memory, to be judged. It is not
stored, returned or logged. `policy_status` still carries no URL. Nothing leaves the machine.

### Entrances

| Entrance | URL taken from | Judged | Kept in | Written after |
|---|---|---|---|---|
| Visit | `webNavigation.onCompleted`, main frame | Batch of 1, after the web scheme and incognito checks | Queue if allowed | Host judges again |
| 全タブを記録 | `tabs.query`, normal web tabs | One batch, split if long. No complete answer: nothing kept | Queue if allowed | Host judges again |
| Stored events and dead letters | `chrome.storage` | Whole lists, when the policy version changes or the worker starts | Removed if refused | Host judges again |
| Manual image save | Context menu | Not judged, as before: an explicit act | Vault, private | The MCP exit judges it |
| Search, vault, image messages | User actions | These are not recording | — | — |

There is no entrance from tab updates; the code has only `webNavigation.onCompleted`. Incognito URLs do not reach the host: the worker
leaves them out before asking.

### Host API

```text
→ { type: "policy_evaluate_batch", requestId, items: [{ id, url }] }
← { ok: true, type: "policy_evaluate_batch_result", requestId, etag, revision, items: [{ id, decision }] }
← { ok: false, type: "policy_evaluate_batch_result", requestId, error }
```

- `decision` is `allow`, `save_but_no_mcp` or `never_save`. A malformed URL or unsupported scheme is `never_save`.
- `id`: 1 to 32 characters of ASCII letters, digits or `-`, and unique. The extension uses list positions, which say nothing about a URL.
- **Refused whole:** `invalid_policy_batch` for a bad shape or duplicate, `policy_batch_too_large` above 1000 items, and `policy_not_initialized`,
  `policy_invalid`, `policy_busy` or `policy_unavailable` when no policy can be used, also for an empty list. There is no partial answer.
- The answer holds no URL or rule.
- One policy load and one compile per batch.

### Size limits

Chrome's documentation states 1 MB for a message from a host and 64 MiB to a host, framed as JSON in UTF-8 after a 32-bit length in native byte
order. The host already reads up to 64 MiB. [verified on developer.chrome.com, 2026-11-02]

| Limit | Value | Derived from |
|---|---|---|
| Items per message | 1000 | The answer is about 32 bytes per item measured, 32 kB in all, so far below 1 MB. The largest queue sweep is 6000 URLs (5000 pending, 1000 dead letters), so 6 messages |
| Request bytes | 32 MiB, counted conservatively | Half of 64 MiB. 1000 URLs of the longest kind can exceed 64 MiB, so a count alone is not enough |
| URL length | 32,768 UTF-16 units | The evaluator refuses longer ones, so the extension refuses them without sending |

Duplicates are asked once. All parts of a split list must succeed and report one `etag`; otherwise none counts, and nothing is kept.

### Queue and sweep

- The per-event check before sending is gone. Each worker start and each new policy version has the host judge every stored URL, and refused
  events leave both lists in one write, with no copy.
- Without a complete answer nothing is sent and nothing changes; the flush reports the code and tries again later.
- An event without a string URL has nothing to judge and is removed.
- An event enqueued while the host is being asked is not in its answer, and stays: it was judged itself.
- **No head-of-line blocking from an event:** an unusable URL is simply `never_save`, so it goes and those behind it continue. A host giving broken answers
  pauses the whole queue, since nothing can be judged, and it resumes once answers are complete.

### Races

An allow can be overtaken by a policy change. `record_event` loads the file and judges once more immediately before the INSERT, using the same `CompiledPolicy`,
and answers `dropped: "policy"`. Decisions are not cached. `current` only reuses a policy's version for up to 1 s at the start of a drain and 30 s during it, which
decides when to sweep and no longer decides any URL.

### Fail-closed

Host down, timeout, Busy, uninitialized or damaged policy, a reply of another type or request, missing, extra or repeated ids, an unknown decision, missing `etag` or
`revision`, parts from different versions, an old host that does not know the message, and a failure reported while a request was in flight: in all of these no URL
is kept, and the toolbar shows the problem.

### Measurements

The real host binary, one process per message as `sendNativeMessage` starts it, driven from Node on one Windows 11 machine, temporary directory, median of 15
[verified]. **Chrome's own IPC is not included**, and the machine's process start is most of the time.

| Rules | 1 URL | 10 URLs | 100 URLs | 1000 URLs | 6000 URLs in 6 messages |
|---|---|---|---|---|---|
| 0 | 11.1 ms | 11.0 ms | 11.7 ms | 15.4 ms | 95 ms |
| 100 | 11.4 ms | 11.4 ms | 11.8 ms | 15.8 ms | 95 ms |
| 999 | 15.0 ms | 14.5 ms | 15.3 ms | 18.7 ms | 119 ms |

Inside the host, median of 21:

| Rules | Policy load | Compile | Judge 10 / 100 / 1000 URLs |
|---|---|---|---|
| 0 | 0.16 ms | 0.01 ms | 0.02 / 0.20 / 2.1 ms |
| 100 | 0.25 ms | 0.09 ms | 0.02 / 0.20 / 2.1 ms |
| 999 | 0.99 ms | 0.90 ms | 0.02 / 0.21 / 2.1 ms |

The former JS evaluator took about 4 ms per URL at 999 rules, so 1000 URLs took about 4 s in the worker [inferred from Slice 0]. A visit still starts two processes, one to judge and one to deliver,
as it did before; a worker's first flush adds one for the sweep.

### Remaining risks

- **Refused URLs pass through the host's memory,** as stated above. A crash dump of that process could hold one.
- **Host and extension must be updated together.** An old host stops recording, which is safe, until it is updated.
- **Worker termination while waiting** loses that visit, by design: nothing is kept before the answer.
- **`policy_status` still returns the whole policy,** which the extension no longer needs beyond the revision.
- The shared vectors still test `policy.js`, which no longer decides anything in the product. Slice 1's Node and Chrome comparison now matters only if JS returns as a judge.
- Chrome itself has not run this: the tests use a fake chrome and fake host for the extension, and the real host over stdio.

### Handoff to Slice 3

1. Policy v2 and the rule model can change the host alone; the extension sees only decisions.
2. Decide whether to retire `policy.js` and drop the policy document from `policy_status`.
3. #23's rule test can use this message for the active policy; a draft needs its own message.

## Slice 3: Policy v2, explicit migration, status without rules

### The model

```json
{ "schemaVersion": 2, "engineVersion": 2, "revision": 3, "categories": { … },
  "layers": {
    "neverSave":    { "enabled": true, "rules": [ { "kind": "pattern", "pattern": "*.example.com" } ] },
    "saveButNoMcp": { "enabled": true, "rules": [ { "kind": "pattern", "pattern": "example.com/private/*" } ] } } }
```

All other fields are as in version 1. In Rust a rule is `RuleSpec::Text(String)` (version 1) or `RuleSpec::Typed(TypedRule)` (version 2), with
`TypedRule::Pattern { pattern }` and `TypedRule::Regex { pattern }`, `kind` as the tag and no other field allowed.

| Policy | Result |
|---|---|
| `schemaVersion` and `engineVersion` both 1, every rule a string | Evaluated as before |
| Both 2, every rule typed, every rule `pattern` | Evaluated; a pattern means what the same string means in version 1 |
| Version 2 with a `regex` rule, also in a disabled layer | Until Slice 5 **refused whole**: `policy_unsupported`, state `unsupported`. Not ignored, not read as a pattern. Since Slice 5 evaluated; see there |
| Unknown `kind`, a missing or extra field, a string rule in version 2, a typed rule in version 1 | Invalid: `policy_invalid` |
| Any other pair of versions, such as 2 and 1 | Invalid |

A pattern goes through the same `Rule::parse` and `Zone` as before. Only the access to its text changed, so compiling and judging cost the same.

**New installs start at version 2:** `policy-defaults.json` is version 2 with the same settings and no user rules, so a first initialization writes version 2.
It is exactly what migrating the former empty default gives (tested). An existing version 1 policy is never touched by initialization, which refuses
to run when a policy exists. Recording still starts only as before (#24).

### Migration

`PolicyStore::migrate(&expected)` and the message `policy_migrate`. Nothing else calls it: no load, status, judgement, write, `policy_get`, vault operation or
extension start rewrites a version 1 file, which a test checks byte for byte after each of them.

1. The caller sends the ETag it has seen. A missing one is `invalid_etag`, a different one `policy_conflict`.
2. `Policy::migrated` requires a valid version 1 policy, and turns each string into `{ kind: "pattern", pattern }` in the same layer and order. Categories,
   switches and everything else are copied. The candidate is validated and compiled.
3. It is written under the writer lock only if the stored revision and ETag are still those of `expected`, with the next revision, through the same
   temporary file and atomic replacement as every update. **Only this path may change the version, and only from 1 to 2:** `update` refuses any
   candidate whose `schemaVersion` or `engineVersion` differs from the stored policy's.
4. Any failure leaves the file, revision and ETag untouched: invalid source, `policy_not_migratable` for anything but version 1 (so a second migration is
   refused), `policy_conflict`, `policy_busy` (not retried), and a failed write.

The new ETag and revision make the extension sweep its queue again, as for any change. There is no long-lived compiled policy anywhere: the host and
VisibleStore load and compile per request, so no process keeps judging with the old version after the file changed.

```text
→ { type: "policy_migrate", requestId, etag }
← { ok: true, type: "policy_migrate_result", requestId, state: "ready", etag, revision, schemaVersion: 2, engineVersion: 2 }
← { ok: false, type: "policy_migrate_result", requestId, error }
```

### Status and document

```text
→ { type: "policy_status", requestId }
← { ok: true, type: "policy_result", requestId, state: "ready", etag, revision, schemaVersion, engineVersion }
→ { type: "policy_get", requestId }
← { ok: true, type: "policy_get_result", requestId, state: "ready", etag, revision, schemaVersion, engineVersion, policy }
```

`policy_status` and `policy_initialize` no longer carry the policy. The extension takes the revision from the status and never asks for `policy_get` or
`policy_migrate`, which a test checks in its source and its messages. Since #23 Slice 1 one exception: the Privacy settings page (`options-state.js`),
opened explicitly, reads the document with `policy_get`, and since #23 Slice 2 also sends `policy_validate`, `policy_test_rule`, `policy_test_url` and
`policy_apply` (`issue-023-privacy-policy-options.md`); the service worker never loads that module, which the same test checks. `policy_get` refuses with `policy_too_large` an answer over 1,000,000 bytes, since Chrome
takes at most 1 MB from a host and the file may be up to 1 MiB.

### Remaining risks

- **Existing users stay at version 1** until they migrate explicitly; nothing asks them yet (#23).
- The JS evaluator, kept for tests, reads version 2 as well, and refuses regex rules; it decides nothing in the product. Shared vectors stay version 1, and
  the Rust test also judges each of them in the other version.
- An extension older than this slice reads the revision from the policy document, which the status no longer carries, so it stops recording until it is updated.
  Host and extension must be updated together, as before.

### Handoff to Slice 4

1. The regex evaluator replaces the `Err(UnsupportedRule)` of `RuleSpec::pattern` with its own compiled form, in a zone of its own.
2. #23 reads with `policy_get`, offers `policy_migrate` when `schemaVersion` is 1, and needs a write message, which does not exist yet.
3. `policy_too_large` is tested on both sides of the limit through the real host.

## Slice 4: exact paths, a wildcard in the last segment, collapsed slashes

### The path a rule meets [code] [verified]

Rust `url` 2.5.8 (WHATWG). The judged path was, and stays, `path_of(url.path())`:

| Step | Done by | Example |
|---|---|---|
| Dot segments, also `%2e` and `%2E`, are resolved | `url` | `/x/%2e%2e/private/a` → `/private/a` |
| Everything is percent-decoded once, **also `%2F`** | `path_of`, before Slice 4 | `/private%2Fsecret` → `/private/secret` |
| A `%`, control character or backslash left after decoding is an invalid URL, so `NeverSave` | `path_of` | `/%252F` → invalid |
| Case and a trailing `/` are kept; runs of `/` are kept | — | `/Private/`, `//private/a` |

Decoding `%2F` already happened before this slice, so `example.com/private/*` already matched `/private%2Fsecret`. Not decoding it would take that match
away, so it stays. Slice 4 adds no decoding.

### Grammar

Version 2 `pattern` rules gain two forms. A version 1 string keeps the grammar it had, and so do the builtins: `example.com/private` in version 1 is
`policy_invalid`, as before.

| Pattern | Matches | Does not match |
|---|---|---|
| `example.com/private` (exact path, v2) | `/private`, also with a query | `/private/`, `/private/a`, `/private2`, `/Private` |
| `example.com/*.png` (v2) | `/a.png`, `/.png` | `/images/a.png`, `/a.jpg`, `/a.PNG`, `/a.png/b` |
| `example.com/images/*.png` (v2) | `/images/a.png` | `/images/sub/a.png`, `/a.png` |
| `example.com/file-*.jpg` (v2) | `/file-.jpg`, `/file-123.jpg` | `/file-a/b.jpg`, `/other.jpg` |
| `example.com/images/photo-*` (v2) | `/images/photo-`, `/images/photo-1.png` | `/images/photo-1/a` |
| `example.com/private/*` (v1 and v2, unchanged) | `/private/` and everything below | `/private`, `/private-x` |

The host part is unchanged: `*.example.com` covers the apex and every subdomain at any depth. A `*` in a path matches any text, also none, without a
`/`. It may appear once, in the last segment. A path ending in `/*` is the old prefix form and keeps its meaning.

Refused (`policy_invalid`, also in a disabled layer): `example.com/user/*/profile`, `example.com/*/*.png`, `example.com/images/**/photo.png`,
`example.com/a*b*c.png`, `example.com/*/`; a `.` or `..` segment; `%2A` or `%2F` that would add or move a wildcard; an invalid escape; and everything refused
before (scheme, port, userinfo, query, fragment, `*` inside a host). A `regex` rule was still refused whole as `policy_unsupported` until Slice 5.

### Collapsed slashes

Each path rule, user or builtin (authentication paths, callback paths), meets two views of the URL's path. A match in either counts:

1. the judged path above, unchanged;
2. the same with each run of `/` made one: `//private/secret` → `/private/secret`. Nothing else changes: no decoding, case, dot segment or query.

The judged path still matches, so `example.com//private/*` keeps matching `//private/secret`. A second view can only add a match, which only refuses more:
an old `NeverSave` stays `NeverSave`, an old `SaveButNoMcp` becomes at most `NeverSave`. This applies to version 1 and version 2.

### Structure

`Zone` keeps its host index. Each host holds a list of `Option<PathRule>`, `PathRule` being `Prefix`, `Exact` or `Name { dir, before, after }`, all parsed
once at compile time. Judging computes the collapsed path only when the path contains `//`. No regular expression or new crate is used.

### Verification

- **Old against new** (scratch harness, main `37e5143` against this tree) [verified]: 3,000 random version 1 and version 2 policies of patterns both can
  read, random categories and switches, 600,000 URLs with doubled slashes, encodings and dot segments. 0 looser decisions; 13,971 stricter, all on a path
  with `//`.
- The unit test comparing with the former evaluator now also has URLs with `//`, `///`, `//login` and `//callback`. Any difference must be stricter and on
  such a path.
- A pattern valid in version 1 parses to the same rule in version 2. A migration keeps every rule text as written.
- Through the real host: batch decisions and the judgement before the vault for the new forms and doubled slashes; no refused URL in the vault; a version 1
  file stays version 1. Through VisibleStore: the new forms keep rows out of the public read.

### Measurements [verified]

Release build, synthetic rules and URLs, median per URL. The machine changes speed during a run, so old and new were measured in alternation.

| Case | Old | New |
|---|---|---|
| 999 host rules, URL of another host | 2 µs | 2 µs |
| 999 host rules, URL of another host with `//` | 2 µs | 3 µs |
| 999 `dir/*` rules on the URL's host | 3.3 µs | 3.6 µs |
| 999 `dir/*` rules on the URL's host, URL with `//` | 8.2–9.1 µs | 12.6–13.8 µs (two views) |
| 999 `dir/*.png` rules on the URL's host | — | 10.3–11.8 µs |
| 999 exact path rules on the URL's host | — | 5.6–6.3 µs |
| Compile, 999 rules, v1 or v2 | 0.8–1.7 ms | 0.8–2.0 ms |

VisibleStore, 10,000 image rows, 50 % public, 999 rules: 68.8 ms in total, compile 0.87 ms (Slice 3: 72.5 ms). With no rule: 67.3 ms.

### Remaining risks

- A server that reads `%2F` as part of a name is judged by the decoded path: `example.com/images/*.png` does not match `/images/sub%2Fa.png`, since the
  judged path is `/images/sub/a.png`. The old prefix form is not affected.
- Rules of one host are still scanned in a list. 999 rules on one host cost about 4 to 8 µs per URL, 13 µs with `//`.
- The JS evaluator, kept for tests, knows neither the new forms nor collapsed slashes. A version 2 policy with a new form is invalid there. It decides
  nothing in the product.
- Not run: Chrome, CI.

### Handoff to Slice 5

1. Regular expressions replace the `Err(UnsupportedRule)` of `RuleSpec::pattern`.
2. #23 needs to explain the version 2 forms, and that version 1 rules gain them only after the explicit migration.

## Slice 5: regex rules

### The rule

```json
{ "kind": "regex", "pattern": "^https://www\\.google\\.com/search(?:\\?|$)" }
```

Version 2 only, in either layer, mixed freely with `pattern` rules. A version 1 policy holding one is `policy_invalid`, and nothing migrates or upgrades a
policy because of a regex. `schemaVersion` and `engineVersion` stay 2.

### What a regex meets [code] [verified]

The URL as the Rust `url` crate (2.5.8) writes it (`Url::as_str`, the same as `to_string`), after the checks every URL passes: an invalid URL is `NeverSave`
before any rule. Measured on that crate:

| Input | What the regex meets |
|---|---|
| `HTTPS://WWW.GOOGLE.COM:443/search?q=test` | `https://www.google.com/search?q=test`: scheme and host in lower case, default port gone |
| `https://例え.テスト/秘密?q=日本` | `https://xn--r8jz45g.xn--zckzah/%E7%A7%98%E5%AF%86?q=%E6%97%A5%E6%9C%AC`: punycode, UTF-8 percent-encoded |
| `https://example.com/a%2Fb?x=%41` | unchanged: an encoding stays as it came, `%2F` is not decoded (a pattern decodes it) |
| `https://127.1/`, `https://[0:0::1]/` | `https://127.0.0.1/`, `https://[::1]/` |
| `https://example.com/a/../b/./c` | `https://example.com/b/c` |
| `https://example.com:8443/`, `https://user:pw@example.com/`, `…#frag` | port, userinfo and fragment kept |
| `blob:https://example.com/x` | `https://example.com/x`: the inner URL, which is also what the patterns judge |
| `https://example.com./x` | both `https://example.com./x` and `https://example.com/x`; a match in either counts |

The second view of a host with trailing dots exists because a pattern reads `example.com.` as `example.com`; without it a regex written for
`example.com` would let that URL through. It is made only when the host has such a dot and the policy has a regex. Nothing else is rewritten: no decoding, no
case folding.

- **Found anywhere**: `Regex::is_match`. `^` and `$` anchor to the whole URL; `example\.com/private` also matches `https://a.example.com/private/x` and a
  query that contains that text.
- **Case-sensitive**, as the `regex` crate is; `(?i)` makes one regex insensitive. Scheme and host are always lower case, so `EXAMPLE` never matches a host.
- **Unicode**: the syntax is Unicode-aware, but the URL is ASCII, so `秘密` or `\p{Han}` never match; write `%E7%A7%98%E5%AF%86` or `xn--…`.
- No match text, capture or position is kept.

### Pattern and regex

| | Pattern | Regex |
|---|---|---|
| Meets | host index, then the decoded path, raw and collapsed | the whole URL as written by the parser |
| Query, fragment, scheme, port | never | yes |
| `%2F` | decoded to `/` | stays `%2F` |
| Case | host folded, path sensitive | sensitive unless `(?i)` |
| Cost per URL | only rules of the URL's host | every regex of an enabled layer |

### Limits

| Limit | Value | Error |
|---|---:|---|
| Regex rules per policy, both layers, enabled or not | 128 | `RegexLimit` |
| Length of one regex | 4,096 bytes of UTF-8 | `RegexLimit` |
| Compiled size of all regexes, shared equally | 8 MiB, at most 1 MiB for one | `RegexLimit` |
| Lazy DFA cache of all regexes, shared equally | 8 MiB, at most 256 KiB for one | grows no further; matching falls back to a slower, still linear engine |
| Nesting | 250 (the crate's default) | `RegexLimit` |

Existing limits stay: 1,000 rules per layer, a 1 MiB file, URLs up to 32,768 UTF-16 units.

Why these values, measured with the crate (scratch harness, synthetic patterns):

- Ordinary rules compile to 1–10 KB: `^https://www\.google\.com/search(?:\?|$)` 1.2 KB, `(?i)^https://mail\.example\.com/[0-9]+/inbox` 2.3 KB. A Unicode
  `\w` is 50 KB on its own; `(?-u)\w` is 0.3 KB.
- A fixed 1 MiB for each of 128 regexes allowed 128 × `\w{20}`: 150–180 MiB of heap, 1.2 s to compile, and 845 ms to judge one 31 KB URL. Shared
  equally, 128 regexes get 64 KiB each, which `\w+` (50 KB) still fits; one or a few get 1 MiB, so `\w{20}` is possible alone.
  The worst a policy can do is about 11–13 MiB of heap and 75 ms to compile. Judging is linear: 36 µs for an ordinary URL, 57 ms for a 31 KB URL.
- Whether one regex fits depends on how many others the policy has. Adding a regex can make an existing one too large, and the store then refuses the
  update: the policy is never stored half valid.

### Errors

| Cause | Rust | Host reply, state |
|---|---|---|
| Syntax error: `(a`, `[a`, `a{2,1}`, `\p{NoSuchClass}` | `InvalidPolicy` | `policy_invalid`, `invalid` |
| Look-around, backreference: `(?<=a)b`, `a(?=b)`, `(a)\1` | `UnsupportedRule` | `policy_unsupported`, `unsupported` |
| Over a limit: count, length, compiled size, nesting, `a{100000}` | `RegexLimit` | `policy_invalid`, `invalid` |
| Policy file missing, busy, unreadable | as before | `policy_unavailable`, `policy_busy`, … |

No reply, error or log carries the regex or a URL. The kinds are told apart with `regex-syntax` (the crate `regex` builds on, at the same version) only
after a regex failed to compile. Every case refuses the whole policy, also when the regex sits in a disabled layer: no URL is judged, nothing is recorded,
VisibleStore publishes nothing. The store does not write such a policy.

### Where regexes live

`Zone` holds `regexes: Vec<Regex>` next to its host index, compiled once in `CompiledPolicy::compile`. `Policy::validate` compiles each regex once to know it
is valid and drops it. Judging calls only `is_match`; a test checks that the judging code builds no regex. The Blind Zone, patterns and regexes, is decided
before the Private Zone, so the order of rules within a layer or of patterns and regexes changes no decision. A policy without regexes judges exactly as in
Slice 4.

**No `RegexSet`.** `RegexSet` answers which of many regexes match in one pass, but it is one automaton for all of them: one rule's size then counts against all,
an error cannot be told per rule, and its compile and cache limits apply to the set. At 128 regexes the plain list costs 4 µs per URL more than none. It
becomes worth measuring if the limit of 128 rises or judging regexes dominates a batch.

### Trust boundary and privacy

Unchanged: the extension asks `policy_evaluate_batch`, drops `never_save` in memory, and the host judges every event again in `record_event` with the policy
current then. Through the real host [verified]: a Blind regex matching the query is dropped in the batch and in `record_event` and never reaches the vault; a
regex added after the precheck refuses the event before the vault, and the batch then answers with the new revision and ETag. A Private regex stores the
row, and VisibleStore keeps it out of the public read like any `saveButNoMcp` row.

VisibleStore applies regexes to visits, images and their pages and frames. Rows recorded before a regex existed disappear from the public read as soon as
it does, and a hidden image answers `not_found` exactly like an unknown one.

### Compatibility

- **A Slice 4 host** reads a regex rule as unsupported: `policy_unsupported`, recording stops, VisibleStore answers `PolicyUnavailable`. Measured on 2,000
  random policies with regexes: every one refused, none judged. No older host ignores a regex and records.
- **The extension** decides nothing. Its message for `policy_unsupported` now names look-around and backreferences.
- **The JS evaluator** (`extension/policy.js`, tests only) does not evaluate regexes and refuses a policy holding one. No JavaScript `RegExp` judges in the
  product.

### Verification

- **Slice 4 against Slice 5** (scratch harness, main `db62533` against this tree) [verified]: 3,000 random version 1 and version 2 policies without regex, with
  every pattern form, category and switch, 600,000 URLs: every decision identical. The 2,000 version 2 ones again with 2–6 regexes: 400,000 URLs, none less
  strict than without the regexes, 94,768 stricter.
- In the test suite: adding a regex to policies of every kind never refuses less, and one that matches nothing changes nothing.
- `^(a+)+$`, `(a+)+$`, `(a|aa)+$`, `(a|a)*b` and `(?:a*)*b` on a 30,000-character URL answer at once; no time is asserted in the tests.

### Measurements [verified]

Release build, synthetic rules, median per URL of 5,000 ordinary URLs (about 80 characters). "Load" is `Policy::parse` with its validation, "compile"
`CompiledPolicy::compile`; the host does both for each message.

| Policy | Load | Compile | Heap | Per URL |
|---|---:|---:|---:|---:|
| Slice 4, 999 patterns | — | 0.8 ms | — | 2.3 µs |
| 999 patterns, no regex | 1.0 ms | 0.8 ms | 398 KiB | 2.4 µs |
| 1 regex | 0.1 ms | 0.1 ms | 22 KiB | 2.4 µs |
| 32 regexes | 2.8 ms | 2.3 ms | 536 KiB | 3.7 µs |
| 128 regexes | 7.1 ms | 5.5 ms | 2.1 MiB | 6.5 µs |
| 128 unanchored regexes that read the whole URL and fail | 20 ms | 20.5 ms | 4.8 MiB | 5.3 µs |
| 999 patterns + 128 regexes | 5.0 ms | 5.1 ms | 2.5 MiB | 6.5 µs |
| 128 regexes, URL of 31 KB | | | | 198 µs |
| 128 unanchored regexes, URL of 31 KB | | | | 507 µs |
| 128 × `\w+N\d` (half the budget each) | 35 ms | 37 ms | 13 MiB | 1.7 µs; 6.4 ms for 31 KB |
| 8 × `\w{20}N` (1 MiB each) | 73 ms | 75 ms | 9.4–11 MiB | 36 µs; 57 ms for 31 KB |

"128 regexes" are anchored on scheme and host: `^https://(?:[^/]+\.)?siteN\.example/private/`, `^https://www\.searchN\.example/search(?:\?|$)`,
`(?i)^https://mailN\.example/[0-9]+/inbox`, `^https://appN\.example/.*[?&]session=[a-f0-9]{8}`. "Unanchored" are `(?i)secretN[a-z]{2,8}token|/pwN/\d+`.

VisibleStore, 10,000 image rows, 50 % public, 999 patterns: 73.2 ms without regex, 154.9 ms with 128 regexes (compile 4.9 ms; the rest is judging 20,000
URLs).

### Remaining risks

- A regex meets the encoded URL: `/%41dmin` is not `/Admin` to a regex, and `%2F` is not `/`. A site that writes the same address in two encodings can pass a
  regex written for one of them. Patterns do not have this gap.
- The URL is ASCII, so a regex with non-ASCII text never matches; #23 should say so.
- Each message compiles every regex twice (validation, then the compiled policy). With 128 ordinary regexes that is about 13 ms per `record_event`; at the
  worst a policy can do, about 150 ms.
- A regex of the worst size reading a 31 KB URL takes about 57 ms, so a batch of 1,000 such URLs would take about a minute. Ordinary URLs and regexes take
  microseconds.
- Not run: Chrome, CI.

### Handoff to Slice 6

1. The GUI API should return `RegexLimit`, `UnsupportedRule` and `InvalidPolicy` per rule, which the Rust error now distinguishes, without the regex text in a
   log.
2. The editor should explain the URL a regex meets (lower case host, punycode, percent-encoding), and that the limit of one regex depends on how many there
   are.
3. A test endpoint for regexes is still out of scope.

## Slice 6: policy management API

### Messages

| Message | Writes | Purpose |
|---|---|---|
| `policy_status`, `policy_get`, `policy_evaluate_batch` | no | as before |
| `policy_validate` | no | whether a draft could be applied as it is |
| `policy_test_rule` | no | whether one rule alone matches a URL |
| `policy_test_url` | no | the decision a draft would give a URL |
| `policy_preset` | no | a preset, as a draft |
| `policy_apply` | yes | a draft in place of the policy of a given ETag |
| `policy_migrate` | yes | version 1 to 2, as before |
| `policy_initialize` | yes | the bundled policy for a new install, as before |
| `policy_start` | yes | a draft as the first policy of a new install, with a usable vault |

Every reply: `{ ok: true, type: "<message>_result", requestId, … }` or `{ ok: false, type, requestId, error, errors? }`. No reply holds a URL or a rule
of the request, except the documents of `policy_get` and `policy_preset`. A request without a string `requestId` is answered with `requestId: null`
and `invalid_request_id`; an unknown `type` with `unknown_type`. The host keeps serving after either. A reply over 1,000,000 bytes becomes
`policy_too_large`. A draft over 1 MiB of JSON is `policy_draft_too_large` and is not read further.

### Draft and active policy

A draft is a whole policy document in the request, `policy`, as `policy_get` returns it. The host keeps no draft: the page (#23) holds it and sends it with
each request. Nothing but `policy_apply`, `policy_start`, `policy_migrate` and `policy_initialize` changes the file, revision or ETag; a test checks the
bytes after each of the others.

### `policy_validate`

```text
→ { type: "policy_validate", requestId, policy }
← { ok: true, …, schemaVersion, engineVersion }
← { ok: false, …, error: "policy_invalid" | "policy_unsupported", errors: [{ layer?, ruleIndex?, code }] }
```

Valid means exactly what applying needs: the draft compiles (`CompiledPolicy::compile`, every rule of both layers, enabled or not, with the regex budget of
the whole draft) and fits the file. `error` is `policy_unsupported` only if every problem is an unsupported regex. Codes:

| Code | Place |
|---|---|
| `invalid_pattern`, `invalid_regex`, `unsupported_regex`, `regex_limit`, `invalid_rule_form` | layer and index of the rule |
| `too_many_rules` | layer |
| `invalid_version`, `invalid_revision`, `too_many_regex_rules`, `policy_too_large`, `invalid_policy_document`, `invalid_policy` | none |

`invalid_rule_form` is a string rule in version 2 or a typed one in version 1, or a rule that is no rule at all (`{ "kind": "glob" }`). A document that is
no policy for another reason (an unknown field, a missing category) is `invalid_policy_document`, with no place: the host does not guess one. Whether a
regex fits depends on how many regexes the draft has, so a regex may be `regex_limit` in one draft and valid in another. Nothing is corrected or dropped.

### `policy_test_rule`

```text
→ { type: "policy_test_rule", requestId, rule: { kind, pattern }, url }
← { ok: true, …, matched }
← { ok: false, …, error: "invalid_rule" | "invalid_pattern" | "invalid_regex" | "unsupported_regex" | "regex_limit" | "invalid_url" }
```

The rule is judged by the compiled zone a policy uses: alone in an enabled Blind Zone, with no builtin. So the host wildcard, paths raw and collapsed, and
the URL a regex meets are the same as in a decision. **A match is no decision**: `matched: false` does not mean a URL would be recorded, since another rule
or a builtin (authentication, credentials) may refuse it. A version 2 rule only. A URL the production judge cannot read is `invalid_url`, which a policy
refuses. A regex alone has the budget of a policy holding one regex; `policy_validate` checks it within the draft.

Since #23 Slice 3 the Privacy settings page adds or changes a rule of its draft only after this answer is `matched: true` for exactly that rule and
URL (the URL checker, `issue-023-privacy-policy-options.md`). That gate is the page's; the host's matching and its answers are unchanged.

### `policy_test_url`

```text
→ { type: "policy_test_url", requestId, policy, url }
← { ok: true, …, decision: "allow" | "save_but_no_mcp" | "never_save" }
```

The draft is validated as above and judged like the stored policy: Blind before Private, builtins included, an unreadable URL `never_save`. An invalid
draft gives no decision. **The decision permits nothing**: recording judges each event again with the stored policy only, which a test shows with a URL the
draft allows and the stored policy refuses.

### `policy_apply`

```text
→ { type: "policy_apply", requestId, expectedEtag, policy }
← { ok: true, …, state: "ready", etag, revision, schemaVersion, engineVersion }
← { ok: false, …, error, errors? }
```

1. `expectedEtag` must be a non-empty string: `invalid_etag`.
2. The stored policy is loaded; another ETag is `policy_conflict`.
3. The draft is validated whole (errors as `policy_validate`).
4. Its version must be the stored one: `policy_version_mismatch`. Changing the version is `policy_migrate` only, and no draft migrates a policy.
5. `PolicyStore::update`: under the writer lock it reads the file again, requires the same ETag and revision, and the draft's `revision` equal to the
   stored one (otherwise `policy_conflict`), checks the version once more, validates again, and replaces the file atomically with revision + 1.
6. Any failure leaves the file, revision and ETag as they were: invalid draft, conflict, `policy_busy`, a failed write (`policy_unavailable`; a test holds
   the file open so that the replacement fails).

The revision is the host's: the draft's revision only proves which policy it was made from. Of several writers with one ETag exactly one succeeds; the others
get `policy_conflict` or `policy_busy` (a test runs four at once). Rules keep their order and layer.

The extension sees the new ETag and revision at its next status or batch and judges its queue again; the new policy decides every later event, in the batch
and again in `record_event`. A user may remove a Blind rule this way, which allows what it refused. That is an explicit change, not a loosening by the
engine: nothing refused before is kept anywhere to be stored later, and nothing is fetched again.

### `policy_preset`

```text
→ { type: "policy_preset", requestId, presetId: "default" }
← { ok: true, …, presetId, schemaVersion, engineVersion, policy }
```

Only `default`, the bundled policy that a new install starts with. Neither #24 nor the docs settle the contents of another preset ("privacy focused"), so
none exists; any other id is `unknown_preset`. A preset is a draft: getting one creates, changes and starts nothing, and nothing puts it back into a stored
policy. It is applied with `policy_start` (new install) or `policy_apply`.

### `policy_start` and when recording starts

Recording already depends on the stored policy: before the first initialization the host answers `policy_not_initialized` to every judgement and every
event, and the extension keeps no URL. A vault is chosen explicitly elsewhere. So "start recording" is the first initialization, and #24 asks for exactly
that: the validated first policy, stored atomically with the existing marker and lock.

```text
→ { type: "policy_start", requestId, policy }
← { ok: true, …, state: "ready", etag, revision: 1, schemaVersion, engineVersion }
```

Refused, writing nothing: an initialized store (`policy_already_initialized`, also for every existing install), no usable vault (the vault's code, such as
`vault_not_configured`), an invalid draft, or a version other than 2 (`policy_version_mismatch`). The draft is checked whole, and its size, before the marker
is written, since the marker is never rolled back. The revision becomes 1 whatever the draft says. After it the policy is in the file, so a restart of the
worker or host finds recording on; nothing turns it on otherwise. Events from before are not recorded, since none was kept.

What it does not do: the host cannot tell that a person clicked; the message is explicit, but any caller of the host could send it. The extension sends it
nowhere yet; the first-run page (#24) will. `policy_initialize` stays as it was.

### M-1: one compilation per request

Before, every message compiled each regex twice: `Policy::parse` validated by compiling and dropping them, then the caller compiled the policy again.
`record_event` did the same through `Policy::evaluate`.

Now validating is compiling: `Policy::validate` is `CompiledPolicy::compile`, and `PolicyStore::load` returns the policy with its compiled form
(`Snapshot::compiled`), made from the same bytes as its ETag. The gate judges batches and events with it, and VisibleStore builds its set with it and
afterwards compares only the ETag of the file (`PolicyStore::current_etag`), which reads the bytes without compiling them. The compiled policy lives for
the one request that loaded it; the next one reads the file again, so a changed policy is always the one used.

| Request | Compilations before | after |
|---|---:|---:|
| `policy_evaluate_batch`, `record_event` | 2 | 1 |
| VisibleStore read (policy before and after) | 3 | 1 |
| `policy_validate`, `policy_test_url` | — | 1 (a failing draft once more for its problems) |
| `policy_apply` | — | 5: the stored policy, the draft, the store's check of the draft, the stored policy under the lock, the result |

### L-1: a slow batch

Each `policy_evaluate_batch` is one `sendNativeMessage`, a host process of its own, which the extension waits for 10 s (`SEARCH_TIMEOUT_MS`). Past that it
has no answer: no URL of the batch is kept, delivery pauses, and the next check asks again. The host process finishes and exits; it writes nothing for a
batch. A test checks the timeout gives no decision.

A batch now holds at most an estimated megabyte (`MAX_BATCH_BYTES`, 32 MiB before), counting each character twice, as well as at most 1,000 URLs. At the
worst a policy can do (Slice 5: about 57 ms for a 31 KB URL, some 2 µs a byte) that is about a second per message. Ordinary URLs are bounded by the count:
1,000 of them are about 100 KB. A worse case remains: a single URL can take at most about 60 ms, so no single message can time out, but a long list of
large URLs takes as many seconds as it has megabytes, one message after the other.

Not done: cancelling a running host, or splitting by the cost of the policy.

### Measurements [verified]

The real host process, release build, 999 Blind patterns and N Private regexes, time per message within one process (median of 5).

| Message | 0 regex | 1 | 32 | 128 |
|---|---:|---:|---:|---:|
| `policy_evaluate_batch`, 100 URLs, Slice 5 | 2.8 ms | 3.1 ms | 7.3 ms | 14.7 ms |
| the same, Slice 6 | 2.2 ms | 2.3 ms | 5.1 ms | 9.6 ms |
| `record_event`, Slice 5 | 6.7 ms | 6.8 ms | 10.3 ms | 16.7 ms |
| the same, Slice 6 | 2.0 ms | 2.1 ms | 4.5 ms | 8.3 ms |
| `policy_validate` | 3.3 ms | 2.6 ms | 5.1 ms | 9.8 ms |
| `policy_test_url` | 3.4 ms | 3.5 ms | 8.3 ms | 14.8 ms |
| `policy_test_rule` (one regex) | 0.17 ms | 0.13 ms | 0.17 ms | 0.14 ms |
| `policy_apply` (process included) | 20 ms | 21 ms | 39 ms | 57 ms |

VisibleStore, 10,000 image rows, 50 % public, 999 patterns: 72.8 → 68.9 ms without regex, 160.0 → 145.9 ms with 128 regexes.

### Remaining risks

- `policy_start` and `policy_apply` can be sent by anything that can talk to the host; the host cannot prove a person's choice. Only the extension can.
- A store whose policy is invalid or unsupported cannot be repaired with `policy_apply`, which needs a loaded ETag. Fail-closed, but #23 needs a way out
  (for example `policy_get` failing with a reason, and an explicit reset), which this slice does not add.
- `policy_apply` compiles five times; fine for a click, not for a loop.
- Only the `default` preset exists.
- The extension has no client for the new messages yet.
- Not run: Chrome, CI.

### Handoff to #23 (options page)

1. Read with `policy_get`, keep the draft in the page, check it with `policy_validate` and show `errors` by layer and index. Do not show the draft's own
   text back from the host; it never sends it.
2. Explain that `policy_test_rule` is a match, not a decision, and use `policy_test_url` for the decision.
3. Apply with `policy_apply` and the ETag read; on `policy_conflict` read again and let the person merge, never retry blindly.
4. Version 1 policies: offer `policy_migrate`, then edit.

### Handoff to #24 (first run)

1. Show `policy_preset` `default`, let the person edit it, check with `policy_validate`.
2. Ask for a vault with the existing vault messages; `policy_start` refuses without one.
3. "記憶を開始する" sends `policy_start` with the draft. Before it, nothing is recorded; the existing uninitialized state already holds that.
4. Existing installs are initialized: `policy_start` refuses them, so they keep their policy and recording.
