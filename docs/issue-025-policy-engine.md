# Issue #25 — Privacy Policy engine

Status: **Slice 1 implemented, for review.** No regular expression, Policy v2, new rule syntax or settings UI exists.
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
| Extension | No compilation in this slice | `evaluatePolicy` still validates and parses per URL |

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

- **The extension is not faster.** `evaluatePolicy` costs about 4 ms per URL at 999 rules [verified in Slice 0]. A queue sweep or
  all-tabs recording feels this.
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
