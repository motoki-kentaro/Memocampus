# Issue #25 — Privacy Policy engine

Status: **Slice 1 merged; Slice 2 implemented, for review.** No regular expression, Policy v2, new rule syntax or settings UI exists.
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
