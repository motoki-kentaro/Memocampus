# Issue #23: Privacy settings page (options page)

Status: Slice 1 (page, basic protection, draft) implemented. Slice 2 (rule tables, URL test, apply) and Slice 3 (migration, recovery) are open. CSV
import and export is postponed.

## Slice 1: the page and the draft

### Structure

`options.html` is the extension's `options_page` (Chrome: Extensions → Memocampus → Details → Extension options). The side panel's settings tab has a
"Privacy設定" button that calls `chrome.runtime.openOptionsPage()`, which opens the page in a tab or focuses the open one. No permission was added.

| File | Role |
|---|---|
| `options.html`, `options.css` | the page; no inline script, handler or style, no remote resource |
| `options.js` | entry: the page with `chromeNativeTransport(chrome.runtime)` |
| `options-page.js` | rendering and events; DOM and host are passed in, so tests run it against a fake DOM |
| `options-state.js` | reading the active policy, checking replies, the draft |

One page, in this order: the active policy (state, Schema / Engine Version, Revision, reload), 基本保護 (five categories), 保存しないグループ
(`neverSave`), MCPサーバに見せないグループ (`saveButNoMcp`), URLテスト (planned), 管理 (migration and recovery, planned), and a bar with the dirty
state, 変更を破棄 and 適用. Planned sections hold text only, no control. Internal names (`neverSave`, `saveButNoMcp`, the category keys) are unchanged.

### Host

The page calls the host itself with `callHost` (`search-client.js`), as the side panel does for vaults and search: a fresh `requestId`, the reply type
and the `requestId` checked, `ok: true` required, 10 s timeout. It sends `policy_status`, and `policy_get` only when the status is `ready`. Nothing else:
no `policy_initialize`, `policy_start`, `policy_apply`, `policy_migrate`, and not even `policy_validate`, since Slice 1 has nothing to check. A test
checks the sources and every message sent. The service worker does not load the page's modules.

A `policy_get` reply is taken as active only if it is whole and consistent: a non-empty `etag`; integer `revision`, `schemaVersion`,
`engineVersion` equal to the policy's own; version 1/1 or 2/2; the five categories as booleans; both layers with a boolean `enabled` and a list of
rules, strings in version 1 and `{ kind: "pattern" | "regex", pattern }` in version 2. Anything else is "不正な応答" and nothing is shown as active.

| Host answer | State shown | Page |
|---|---|---|
| `ready`, version 2 | Ready | draft, categories editable, rules read-only |
| `ready`, version 1 | Ready | values shown, nothing editable, explicit migration needed (Slice 3); nothing migrates |
| `uninitialized` | Not Initialized | explained: no recording until initialized, which the side panel offers; the page initializes nothing |
| `invalid`, `unsupported` | Invalid | recording stopped, file untouched, recovery planned (Slice 3) |
| `busy`, `unavailable`, `policy_get` failing, no host, timeout, old host, malformed reply | Unavailable | the reason; no value is shown |

When no policy was read, the category list and rule tables are hidden and the page says that it has not read the policy, so no unchecked box can be
taken for an active "off".

### Draft and active

`readActive` makes a deep copy of the reply's policy and freezes it. The draft is another deep copy (`structuredClone`), so an edit cannot reach the
active policy; fields the page does not show are kept. The draft is in the page's memory only: never in `chrome.storage`, `localStorage`, IndexedDB or
the host. Closing or reloading the page loses it.

Dirty is derived: the draft differs from the active policy (`sameValue`, keys in any order, arrays in order). Turning a category back clears it. Slice
2's rule edits are part of the same document, so the same comparison covers them. 変更を破棄 makes the draft a new copy of the active policy.

A category is editable only in a ready version 2 policy. Turning off a category that the active policy has on shows a warning (保存されるようになる for
the Blind Zone categories, MCPサーバ向けの公開対象になる for the Private Zone ones); the choice stands. When a layer is disabled, its categories say they
are not used.

適用 is always disabled in Slice 1, with the reason "ルール編集と適用機能は次の開発段階で利用可能になります。" No code path applies.

### Unapplied changes

- `beforeunload` asks while the draft is dirty. Chrome shows this prompt only after the person has interacted with the page, and the text is
  Chrome's own; it cannot be relied on.
- 再読み込み on the page asks in the page itself (変更を破棄して再読み込み / やめる) while dirty; without changes it reads at once.
- The page starts no navigation of its own (the side panel opens the page, not the reverse), so there is nothing else to ask about.
- Nothing is stored for the prompt.

The page reads the policy once when it opens and on 再読み込み. A change made elsewhere meanwhile (the side panel initializing, another page) shows only
after reloading. Slice 2 applies with the ETag that was read, so a stale draft is refused (`policy_conflict`), not applied.

### Rule tables

Both groups are tables of their own: 種類 (Pattern, Regex, or Pattern（v1）for a version 1 string), ルール, 操作 ("編集不可"). An empty group says
"ルールはありません。" Rule text is set with `textContent` only, never `innerHTML`, never in an attribute, never logged; a test shows markup in rules as
text and spies on the console. There is no control in a table yet.

### Tests

`tests/extension/options.test.js`: manifest and page (no new permission, no inline or remote code), every state, malformed replies, the draft
(copy, dirty, undo, discard, warnings, no storage), the tables (kinds, empty, hostile text), leaving and reloading, and the messages sent.
`sidepanel.test.js`: the side panel opens the page. `privacy-ingress.test.js`: only `options-state.js` contains `policy_get`, and the service worker's
imports do not reach it.

## Handoff

Slice 2:

1. Rule tables: add, edit, delete, Pattern / Regex, per group. Keep each group's own table.
2. Check the draft with `policy_validate` and show `errors` by `layer` and `ruleIndex`; the host never sends rule text back.
3. URL test: `policy_test_rule` is a match, not a decision; `policy_test_url` gives the draft's decision. Neither permits anything.
4. Apply with `policy_apply`, the ETag from `policy_get`, and the draft. On `policy_conflict` read again and let the person decide; never retry blindly.
   Enable 適用 only for a dirty, valid draft.

Slice 3:

1. Version 1: explicit `policy_migrate` with the ETag, then edit.
2. Invalid or unsupported policy: a recovery message and its page. `policy_apply` cannot replace a policy that cannot be loaded (#25 Slice 6).
3. Error cases end to end, Chrome dogfood.

Later: CSV import and export per group (`regex,pattern`, RFC 4180, UTF-8), not in Slices 1–3.
