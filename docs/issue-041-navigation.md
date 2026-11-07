# Issue #41: observed navigation facts of documents — schema 6, host ingestion, extension, Chrome dogfood

Status: Slice 1 (schema, Native Host ingestion) and Slice 2 (extension: observing, correlating, delivering) implemented
with automated tests. Chrome dogfood is Slice 3. Nothing reads `navigation_events`: no MCP tool, no VisibleStore API, no UI.

## PO decisions applied

| | Decision | Here |
|---|---|---|
| H1 | No migration | **Schema 6, creation-only.** A schema 5 vault is refused (`vault_schema_mismatch`) and left as it is. Create a new vault after updating. |
| H2 | Meaning of the source | `source_visit_id` is the document shown before (same tab) or in the opener tab. **Not proof of a click or a hyperlink.** The extension chooses it (Slice 2); the host only checks it. |
| H3 | SPA | **Superseded (PO, after the audit): SPA URL changes are not recorded.** The host still accepts `kind = "history_state"` (schema 6 is unchanged), but the extension never sends it. |
| H4 | Fragments | Not collected; there is no `fragment` kind. |
| H5–H8 | iframe source NULL, `forward_back` without direction, no redirect URLs, no unfinished loads | Nothing to store beyond the flags below. |

## Message

A `visit` event may carry `navigation`; without it, everything is as before.

```json
"navigation": {
  "sourceVisitId": null, "kind": "document", "transitionType": "link",
  "clientRedirect": false, "serverRedirect": false, "forwardBack": false,
  "fromAddressBar": false, "newTab": false, "occurredAt": "2026-11-06T12:00:00.000Z"
}
```

Checked in `native-host/src/navigation.rs`. A navigation is refused as a whole, and only the navigation, if:

- it is not an object, a key is missing, or any other key is present;
- `sourceVisitId` is not `null` or 1–128 printable ASCII characters, or it is the visit's own id;
- `kind` is not `document` / `history_state`, or `transitionType` is not one of Chrome's eleven values or `unknown`
  (nothing is mapped: `auto_toplevel` or `LINK` is refused);
- a flag is not a JSON boolean;
- `occurredAt` is not `YYYY-MM-DDTHH:MM:SS.sssZ` with a day that exists.

`occurredAt` is when Chrome reported the navigation; it is not `visitedAt`.

## Order on the host (`native-host/src/main.rs`, `record_event`)

1. `incognito` must be `false`, else `incognito_not_allowed` (unchanged).
2. The visit's URL is judged; NeverSave → `dropped: "policy"`, nothing stored, navigation included (unchanged). No decision →
   temporary error.
3. The visit is parsed and checked (unchanged); an invalid visit is `invalid_event` whatever its navigation.
4. The navigation is parsed. Invalid → visit stored, `navigation: "invalid"`.
5. Source (`allowed_source`): the stored visit with that id is read with its URL, and that URL is judged by the current policy.
   Not stored → NULL. NeverSave → NULL. SaveButNoMcp or allowed → kept. **No decision → the whole event fails (temporary),
   nothing is stored.**
6. One write transaction (`BEGIN IMMEDIATE`): the visit (`INSERT OR IGNORE`, unchanged), its page metadata, its navigation:

   ```sql
   INSERT INTO navigation_events (...)
   SELECT v.event_id, (SELECT s.event_id FROM visits s WHERE s.event_id = :source AND s.url = :source_url), ...
   FROM visits v WHERE v.event_id = :id AND v.url = :url
   ON CONFLICT (target_visit_id) DO NOTHING
   ```

   - The target must be a stored visit with exactly this id **and** URL: an id stored earlier with another URL gets no
     navigation (`"invalid"`).
   - The source is bound to the id and URL that were judged; if it is not in this vault it becomes NULL, never a foreign-key
     error.
   - `DO NOTHING` covers only the target's key (a resend). Unlike `INSERT OR IGNORE`, a CHECK or NOT NULL failure is an
     error that rolls back the whole event (unit test).
   - A row is never updated: a source that arrives after its target leaves NULL; a resend with other values keeps the
     first row.

## Acknowledgement

| Case | Answer |
|---|---|
| No `navigation` | `{ ok: true, eventId }` (unchanged) |
| Stored, or already stored (resend) | `{ ok: true, eventId, navigation: "stored" }` |
| Refused navigation, or target id stored with another URL | `{ ok: true, eventId, navigation: "invalid" }` — the visit is stored |
| NeverSave destination | `{ ok: true, eventId, dropped: "policy" }` (unchanged, no `navigation`) |
| Incognito, invalid visit | `{ ok: false, eventId, error }` (unchanged) |
| No policy decision (destination or source), vault busy or missing | `{ ok: false, eventId, error }`, temporary, nothing stored |

For a visit with `navigation`, the extension accepts only `"stored"` as a delivered navigation (Slice 2, below).

## Host capability

```text
→ { type: "host_capabilities", requestId }
← { ok: true, type: "host_capabilities_result", requestId, capabilities: ["navigation_v1"] }
```

It reads neither the vault nor the policy. A host from before #41 answers it as an unknown event:
`{ ok: false, eventId: null, error: "unknown_type" }`; it would also store a visit and silently ignore its `navigation`.
So the extension (Slice 2) must ask first and send `navigation` only to a host that names `navigation_v1`, and
treat a stored visit whose acknowledgement has no `navigation` field as "navigation not stored". A host with this capability
writes only to schema 6 vaults, since it refuses every other one.

## Schema 6

`native-host/src/schema.rs` (byte-for-byte check, as before):

```sql
CREATE TABLE navigation_events (
  target_visit_id  TEXT    NOT NULL PRIMARY KEY REFERENCES visits (event_id) ON DELETE CASCADE,
  source_visit_id  TEXT    REFERENCES visits (event_id) ON DELETE SET NULL,
  kind             TEXT    NOT NULL CHECK (kind IN ('document', 'history_state')),
  transition_type  TEXT    NOT NULL CHECK (transition_type IN ('link', 'typed', 'auto_bookmark', 'auto_subframe', 'manual_subframe', 'generated', 'start_page', 'form_submit', 'reload', 'keyword', 'keyword_generated', 'unknown')),
  client_redirect  INTEGER NOT NULL CHECK (client_redirect IN (0, 1)),
  server_redirect  INTEGER NOT NULL CHECK (server_redirect IN (0, 1)),
  forward_back     INTEGER NOT NULL CHECK (forward_back IN (0, 1)),
  from_address_bar INTEGER NOT NULL CHECK (from_address_bar IN (0, 1)),
  new_tab          INTEGER NOT NULL CHECK (new_tab IN (0, 1)),
  occurred_at      TEXT    NOT NULL CHECK (length(occurred_at) = 24),
  CHECK (source_visit_id IS NULL OR source_visit_id <> target_visit_id)
);
CREATE INDEX navigation_events_by_source ON navigation_events (source_visit_id);
```

- No URL, title, referrer or redirect URL is stored here. Both ends are visits.
- Nothing deletes visits today; the `ON DELETE` actions only define what would happen.
- The one-row-per-target key is the event's identity: the visit id from the extension.
- Backups copy the table and compare its row count (`vault.rs`, `navigation_count`); `vault_status` counts are unchanged.

## Privacy

- NeverSave or incognito destination, or no decision: no visit, no navigation (tests).
- NeverSave source: NULL. The row keeps no sign that a source was removed. A NULL source from this cause looks the same as
  one that was never known.
- SaveButNoMcp source: kept. **A future reader must judge both ends against the current policy and leave out the whole row
  when either end is not visible**, including from counts and from whether a row exists. Being stored is not permission to
  publish.
- A policy change after storage changes no row, as for visits. Visibility is decided at read time.
- VisibleStore and the MCP server read no navigation (a test keeps their answers unchanged with rows present).

## Tests

- `native-host/src/navigation/tests.rs`: shape, every refusal, ids, calendar dates.
- `native-host/src/schema.rs`: keys, CHECKs, one row per target, `SET NULL` / `CASCADE`.
- `native-host/src/main.rs`: CHECK failures are not hidden and roll back the visit; a source without a policy decision is an
  error; a missing source is NULL.
- `native-host/tests/stdio_protocol.rs` (real host process): storing with all kinds and flags, resends, sources that are
  missing, late, NeverSave or SaveButNoMcp, 13 invalid navigations, an id reused with another URL, NeverSave / incognito /
  broken policy, a locked vault, four concurrent hosts (foreign keys and integrity), backups, `host_capabilities`, and schema 5
  (and look-alikes) refused.
- `visible-store/src/tests.rs`, `mcp-server/tests/stdio_mcp.rs`: answers are unchanged with navigation rows; schema 5 is refused.

# Slice 2: the extension

No new permission, content script, queue or endpoint. Files: `extension/navigation-tracker.js` (correlation),
`extension/host-capabilities.js` (capability check), `extension/event-queue.js` (ACK rules), `extension/service-worker.js`
(wiring). Tests: `tests/extension/navigation.test.js`.

## Events

| Chrome event | Tracker | Visit |
|---|---|---|
| `webNavigation.onCommitted` (main frame, not prerender) | In a normal tab: the commit's facts and its source are fixed now; becomes the tab's head. Incognito / unreadable tab: its state is deleted. | none |
| `webNavigation.onCompleted` (main frame, http/https) | Takes the pending commit of this `documentId`. | As before (incognito, policy, page metadata), plus `navigation` if a commit was found. |
| `webNavigation.onHistoryStateUpdated` (main frame) | **Only forgets:** the tab no longer shows a recorded visit (no source until the next recorded document), the document's pending commit is dropped, the document is marked not pristine. | **none** (pushState / replaceState are never recorded) |
| `webNavigation.onCreatedNavigationTarget` (url http(s) or `about:blank`) | Branch for the new tab's first http(s) commit. | none |
| `tabs.onRemoved`, `tabs.onReplaced` | State of the tab(s) deleted, branch included. | none |

Not handled: URL changes inside a document (pushState, replaceState, fragments), subframes, prerendered documents, loads
that never complete (H8), redirect URLs (H7). The state of a web application is the application's own; Memocampus records
that its documents were visited.

## Source rules

`sourceVisitId` is the visit the tab showed just before — or, for a new tab, the visit its opener's main frame showed —
and only if that visit was **queued**. It is null when:

- the transition is `typed`, `auto_bookmark`, `generated`, `start_page`, `keyword`, `keyword_generated` or `unknown`, or
  carries `from_address_bar` — **unless it carries `forward_back`** (below);
- the document shown before was not recorded (NeverSave, policy failure, incognito check, never completed) or its visit
  is not queued yet;
- the tab's state is unknown (for example after a worker restart: nothing is restored);
- the tab showed SPA states since its last recorded document (a history update);
- a new tab was opened from a subframe (H5), from an unknown or unrecorded opener;
- a branch cannot be matched: the first http(s) commit of the new tab must be within 30 s of it and not typed (or any
  other transition above). A branch to an http URL must also arrive at that URL (or a server redirect from it). `about:blank`
  in between is skipped; anything else drops the branch. A branch arriving after its commit is applied only while that
  commit waits for `onCompleted`.

**Branch to `about:blank`** (`window.open("about:blank")`, then the opener sets `location`). Chrome reports the branch with
url `about:blank`, then the about:blank document (committed and completed, never a visit), then the http(s) commit
(`link`). The final URL is never in the branch, so no URL is compared: the branch is kept without a target URL and is
taken only by **the first http(s) commit of that same tab**, within 30 s, not typed, with only `about:blank` in between. It
then gives `newTab: true` and the opener's visit as source (null from a subframe or an unknown opener; null source and
`newTab` are separate facts). Anything else — expiry, another page in between, a typed URL, the tab closed or replaced —
drops it, and that navigation has `newTab: false`.

**Back and forward** (`forward_back`, Slice 3 I-2). Chrome then reports the history entry's **original** transition and
qualifiers: back to a page first reached by the address bar is `typed` (+ `forward_back`), forward to a reloaded page is
`reload` (+ `forward_back`). Those describe how the entry was first reached, not what happened now. So `forward_back` is
checked first: such a navigation names the visit shown just before, whatever its transition type. All the other conditions
still apply (recorded and queued, same tab, consistent state); `transitionType` and the qualifiers are stored as reported.
Back is not told from forward.

A URL that matches is never enough on its own: correlation goes by tab, `documentId` and the head token. `onCompleted`
takes a pending commit only for the same document **and** the commit's URL, and only if no history update dropped it
(URL ABA cannot bring it back). A visit is recorded as source only after it is queued, so a source is always ahead of its
target in the FIFO queue.

`transitionType` is Chrome's value (`link` is Chrome's classification, not a proven click); a value outside Chrome's
eleven is sent as `unknown`. Qualifiers map one to one; `forward_back` has no direction. `occurredAt` is Chrome's
`timeStamp` cut to milliseconds; an unusable one gives a visit without `navigation` (nothing is made up).

## State and service worker restarts

Per tab, **in the worker's memory only**: last committed `documentId`, the head `{ token, visitId }`, up to four pending
commits (facts, fixed source, commit URL), one branch, and the last 16 documents that had a history update. Nothing is
written to `chrome.storage.session` or anywhere else; nothing for incognito tabs. Deleted when the tab closes or is
replaced.

A stopped worker starts with no state (PO decision): the first navigation of each tab after it has no source, and a commit
seen before the stop is not paired with its completion (that visit is recorded without `navigation`). Correlation resumes
with the next recorded document. Storage failures therefore cannot leave a stale copy that names a wrong source.

**Title and page of a document visit.** They are read after the load (the tab's title; the page metadata in that very
document). They are kept only if the tab still shows the visit's URL (title) and **no history update was seen in that
document**, checked after reading (`pristine`); otherwise the visit is recorded with `title: ""` and no page. A document
whose `documentId` is unknown is not pristine. Metadata completeness gives way to provenance.

Every state step runs on a per-tab chain (only `isNormalTab` is awaited in it). Policy checks, page reads and the host are
outside it; a branch reads its opener and updates the new tab in two separate steps. No step waits for another tab while
holding one.

## Delivery

- **Capability:** the queue asks `host_capabilities` once per delivery run (there is no lasting connection: every native
  message is a host process) and again after an ACK without `navigation`. No answer → `timeout` or `host_unavailable`,
  the event stays queued. `unknown_type` with `eventId: null` → an old host.
- **Old host:** the visit is sent **without** `navigation`, stored, and counted as `navigationNotStored`.
- **ACK:** with a navigation, delivered means `ok`, same `eventId`, `navigation: "stored"`. `"invalid"` → visit stored,
  counted as `navigationInvalid`, not resent (a resend could not change it). No `navigation` → `navigationNotStored`.
  Any other value → `invalid_ack`, temporary. These counts keep no URL; they appear in the queue status, the side panel
  and the toolbar badge.
- Events are immutable once queued: resends carry the same id, source and facts.
- Privacy is unchanged: NeverSave / incognito / no policy → no visit and no navigation; policy sweeps remove queued visits
  with their navigation; the navigation itself holds no URL.
- **Sweep and sources (audit F-4):** when a policy change removes queued (or dead-letter) visits, every visit that stays and
  names one of them as `sourceVisitId` gets `null` instead, in the same write. Its id and queue position do not change;
  nothing already sent is touched.
- **Known limit R-4 (accepted by the PO):** a source that is no longer queued (already delivered) keeps its id in a queued
  target's `navigation`. No URL, title or page of it stays in the queue; the host judges the source against the current
  policy on arrival and stores NULL for a NeverSave one, and MCP reads no navigation.

# Slice 3: Chrome dogfood (Chromium 159, isolated profile, synthetic vault and policy)

Observed (raw `webNavigation` events, then the stored rows):

| Action | Chrome's events | Stored |
|---|---|---|
| Link, middle / Ctrl+click, `target=_blank`, `window.open(url)` | new tabs: `onCreatedNavigationTarget` (`sourceFrameId` 0) **before** the new tab's commit | source = opener's visit, `new_tab` 1 |
| `window.open("about:blank")` then `location` | branch url `about:blank` → about:blank commit + completed → http commit `link` | `new_tab` 1, source = opener (fixed in I-1) |
| `target=_blank` in an iframe | `sourceFrameId` ≠ 0 | source NULL, `new_tab` 1 |
| Back / forward | same `documentId` (back/forward cache), `onCompleted` fires, transition = the entry's original one + `forward_back` | source = page left (fixed in I-2) |
| 302 / JavaScript redirect | `server_redirect` on the final commit / `client_redirect`; the redirecting document never completes | intermediate URL never stored |
| pushState / replaceState | `onHistoryStateUpdated`, same `documentId`, `link`; also for a replace to the same URL | (then: one visit per URL change; **now: nothing**, see below) |
| Hash change | `onReferenceFragmentUpdated` only | nothing |
| New tabs in Chromium | a prerendered search warm-up (`frameId` ≠ 0, `prerender`) | ignored |

Also checked in Chrome: NeverSave in between (no visit, next source NULL), SaveButNoMcp (kept, not in MCP), a broken
policy (nothing queued), a policy change sweeping queued visits, a stopped host and vault, a synthetic old host and bad
ACKs, a service worker stopped between commit and completion.

Known limits (not changed): `tabs.onReplaced` and prerender activation were not seen; incognito with the extension allowed
was not tried; sources are often NULL for very fast navigations (the visit before is not queued yet); a host that keeps
answering with a wrong `eventId` or an unknown `navigation` value pauses the queue (temporary, as for any bad ACK; a
separate issue).

# Independent audit findings (F-1 to F-4) and their fixes

| | Cause | Fix |
|---|---|---|
| F-1 (High) | A visit took `tabs.get().title` when it was built, i.e. the tab's title *then*, possibly of a later NeverSave URL (SPA: C allowed, then B NeverSave before the handler ran). Policy judged only C's URL. | **History visits are recorded with an empty title.** A document visit keeps the tab's title only while the tab still shows that very URL; otherwise `""`. Its page metadata is unchanged (it is read in that document and checked against its URL). |
| F-2 (High) | After a failed `storage.session` write and a worker restart, the restored head (A) was trusted although a NeverSave page (B) had been shown since: a false A → C. | Restored tabs are unconfirmed until Chrome confirms document and URL at the next `onBeforeNavigate`; a failed write resets the tab in this worker. See *State and service worker restarts*. |
| F-3 (High) | `onCompleted` was paired with a pending commit by `documentId` only. A document committed at B (from A) and moved by `pushState` to C before completing gave C's document visit the source A. | `onCompleted` takes a pending commit only if its URL is the commit's URL (salted digest). Otherwise the visit is recorded without `navigation`. |
| F-4 (Low) | A sweep removed a newly NeverSave source visit from the queue but left its id in a target's `navigation`. | The sweep nulls those references (queue and dead letters) in the same write. |

These fixes were made first; the PO decision below then removed the SPA visits and the restored state, which also removes
what F-1 (history visits), F-2 (restored head) and the F-2 confirmation step were about.

# PO decision after the re-audit: SPA URL changes are not recorded

**"The state inside a web application is the application's own; Memocampus records that the application was visited."**
pushState, replaceState and fragment changes make no visit and no navigation. `onHistoryStateUpdated` is used only to
forget (see *Events*). Worker restarts are not bridged.

Removed: the SPA visit (`history_state`) and its source chain, title rule, policy check and queueing in the service worker;
the URL digest and its salt; everything stored in `chrome.storage.session` by the tracker (restore, field checks,
`onBeforeNavigate` confirmation, `getFrame` / `tabs.get` checks, write-failure handling). Tests of those features were
removed with them; tests that inspected stored state were rewritten to check behaviour.

Re-audit findings:

- **R-1** (title or page of another time on an allowed visit): no SPA visit reads a title any more; a document visit keeps
  its title and page only from a pristine document (above). Left: a history update Chrome reports only after the page read
  has returned is not seen in time; a document completed after a worker restart cannot know of updates before the restart.
- **R-2** (stale pending or head reused after a restore or with back/forward cache): nothing is restored; heads are tokens
  of this worker, so a reused `documentId` never brings an old visit back.
- **R-3** (URL ABA): a history update drops the document's pending commit; a later `onCompleted` at the commit URL finds
  nothing.
- **R-4**: accepted as a known limit (see *Delivery*).

Checked in Chromium 159 after this decision (isolated profile, synthetic vault): a link keeps its source; two pushState, a
replaceState and a fragment change stored nothing (no `history_state`, no SPA URL); the next document after them has no
source, and the one after that names it; a document moved by pushState while loading (`/slowspa` → `/moved`) is one visit
without navigation, title or page. `integrity_check` ok, `foreign_key_check` empty. Stable Chrome not verified. I-3 (bad
ACKs pause the queue) is unchanged.
