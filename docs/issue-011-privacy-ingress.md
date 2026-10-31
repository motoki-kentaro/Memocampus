# Issue #11 — Layer A at the recording entrances

Connects the Privacy Policy of `issue-010-privacy-policy.md` to automatic recording. Evaluation
rules, presets and the JSON schema are unchanged.

## Three outcomes that must not be confused

| Outcome | Meaning | Extension | Host answer |
|---|---|---|---|
| Allow | A valid policy did not put the URL in layer A (includes `saveButNoMcp`) | Recorded | `{ok: true, eventId}` |
| Refuse | A valid policy put the URL in layer A, or the URL is malformed | Not kept; removed if queued | `{ok: true, eventId, dropped: "policy"}` |
| No decision | No valid policy could be read | Nothing new kept; queue kept and paused | `{ok: false, eventId, error: "policy_*"}` |

`evaluatePolicy()` and `PolicyStore::evaluate()` answer `neverSave` for a failure too, so recording
does not rely on them alone. In the host, `policy_gate.rs` calls `load()` first and only a loaded
policy gives a decision. In the extension, `policy-gate.js` validates the host's document and
hands out `decide()` only for a valid one.

## Protocol

`policy_status` and `policy_initialize` carry only a `requestId`, never a URL. Both answer
`type: "policy_result"`.

- Ready: `{ok: true, state: "ready", policy, etag}`
- `policy_status` otherwise: `{ok: true, state: "uninitialized" | "invalid" | "busy" | "unavailable"}`
- `policy_initialize` otherwise: `{ok: false, error}`, including `policy_already_initialized`

`uninitialized` requires that neither `policy.json` nor `policy.initialized` exists. Everything
else that is missing is `invalid` and cannot be initialized over, and that includes a lone missing
`policy.lock` (the #10 decision: fail closed, no implicit recovery).

The host retries a busy read for about 100 ms before answering `policy_busy`. Each `visit` and
`tab_snapshot` reads the policy from disk. A URL that is not http(s) keeps the existing permanent
`invalid_url` answer. The policy check comes before the event is parsed, so that a refused URL
cannot come back as `invalid_event`, which an older extension would quarantine with its URL.

## Cache

Memory only, so a restarted worker starts from nothing. Only validated documents are cached, and a
failed fetch forgets the previous one.

| Caller | Allowed age of the answer |
|---|---|
| Visit, 全タブを記録, side panel status (`maxAgeMs: 0`) | None: asks the host every time |
| Start of a queue drain (`fresh`) | 1 second, to reuse the entrance's check |
| Later in the same drain | 30 seconds |

A `policy_*` error from delivery stops the use of the cached policy at once (`reportFailure`), and a
check that was in flight at that moment cannot bring it back. There is no change notification. Mid-drain,
the extension can therefore use an answer up to 30 seconds old; the host judges each of those events
against the file, so a refused one is still not stored.

## Queue

- A flush with neither pending events nor dead letters does not contact the host.
- Without a policy the flush returns the code. Nothing is sent or removed.
- When the policy version (`etag`) differs from the last one swept, refused events are removed from
  the pending and dead-letter lists in one write, with no copy or count. A restarted worker sweeps once.
- The head is then checked before it is sent, and an ACK with `dropped: "policy"` removes it.
- `locate` does not count refused ids as stored, so 全タブを記録 reports only what was kept.

Retention: new events are not collected while no decision is possible, so the queue cannot grow
then, and it is limited to `MAX_PENDING`. Stored events are not removed during a failure. They remain
until the policy is readable again, and are then swept, without a time limit in between.

## Tab snapshots

`全タブを記録` sends one independent `tab_snapshot` event per tab, each with its own id. There is no
parent row, count or relation in the schema, so judging members means judging events. If every tab is
excluded, nothing is written and the result is `queued: 0`.

## Verification

`tests/extension/privacy-ingress.test.js` runs the real worker, queue, gate and evaluator against a
fake chrome and host. `native-host/tests/stdio_protocol.rs` runs the real binary with temporary app
directories. Chrome itself is covered only by the manual dogfood.
