# Issue #23: Privacy settings page (options page)

Status: Slice 1 (page, basic protection, draft) and Slice 2 (rule editing, validation, tests, apply) merged. Slice 3 (URL checker: adding and
changing rules only after a host match) merged. The Domain Wildcard kind in the checker implemented, for review. Migration and recovery are open (see
Handoff). CSV import and export is postponed.

## Slice 1: the page and the draft

### Structure

`options.html` is the extension's `options_page` (Chrome: Extensions → Memocampus → Details → Extension options). The side panel's settings tab has a
"Privacy設定" button that calls `chrome.runtime.openOptionsPage()`, which opens the page in a tab or focuses the open one. No permission was added.

| File | Role |
|---|---|
| `options.html`, `options.css` | the page; no inline script, handler or style, no remote resource |
| `options.js` | entry: the page with `chromeNativeTransport(chrome.runtime)` |
| `options-page.js` | rendering and events; DOM and host are passed in, so tests run it against a fake DOM |
| `options-state.js` | reading the active policy, checking replies, the draft; validating, testing and applying a draft (Slice 2) |

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
| `invalid` | Invalid（設定ファイルが無効です） | recording stopped, file untouched, recovery planned (Slice 3) |
| `unsupported` | Unsupported（現在のバージョンでは対応していません） | as `invalid`, and update the host if the policy comes from a newer one (Slice 2; was shown as Invalid) |
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

In Slice 1 適用 was always disabled; Slice 2 applies (below).

### Unapplied changes

- `beforeunload` asks while the draft is dirty. Chrome shows this prompt only after the person has interacted with the page, and the text is
  Chrome's own; it cannot be relied on.
- 再読み込み on the page asks in the page itself (変更を破棄して再読み込み / やめる) while dirty; without changes it reads at once.
- The page starts no navigation of its own (the side panel opens the page, not the reverse), so there is nothing else to ask about.
- Nothing is stored for the prompt.

The page reads the policy once when it opens, on 再読み込み and after applying. A change made elsewhere meanwhile (the side panel initializing, another
page) shows only after reloading; applying with the ETag that was read refuses a stale draft (`policy_conflict`).

### Rule tables

Both groups are tables of their own. In Slice 1 every rule was read-only; since Slice 2 a version 2 rule is edited in place (below). A version 1 rule
stays read-only: 種類 Pattern（v1）, ルール, 操作 "編集不可", テスト "—". An empty group says "ルールはありません。" Rule text is never put through
`innerHTML`, into an attribute or into a log; a test shows markup in rules as text and spies on the console.

### Tests

`tests/extension/options.test.js`: manifest and page (no new permission, no inline or remote code), every state, malformed replies, the draft
(copy, dirty, undo, discard, warnings, no storage), the tables (kinds, empty, hostile text), leaving and reloading, and the messages sent.
`sidepanel.test.js`: the side panel opens the page. `privacy-ingress.test.js`: only `options-state.js` contains `policy_get`, and the service worker's
imports do not reach it.

## Slice 2: editing, checking, testing and applying

### Trust boundary

```text
page draft (memory) ─ policy_validate / policy_test_rule / policy_test_url ─▶ host judges, writes nothing
                    └ 適用 (explicit) ─ policy_validate ─ policy_apply(expectedEtag, draft) ─▶ PolicyStore, atomic, revision + 1
```

The page judges no URL and no rule: it shows the host's answers. Its own checks are of the reply's form only. The service worker loads none of the
page's modules, sends none of these messages, and recording still judges every event against the stored policy (`record_event`); a test result or an
apply result is never a permit. `privacy-ingress.test.js` checks that only `options-state.js` names `policy_get`, `policy_validate`, `policy_test_rule`,
`policy_test_url` and `policy_apply`, that no file names `policy_start`, `policy_preset` or `policy_migrate`, and that the worker's imports reach no
page module.

### Two tables

Slice 3 replaced the typed rows and ルール追加 below: rows are shown, not typed into, and a rule is added or changed through the URL checker only (see
"Slice 3"). Deleting, the layer switch and the dirty comparison are unchanged.

| Table | Draft | Columns |
|---|---|---|
| 保存しないグループ | `layers.neverSave` | 種類 (select: Pattern / Regex), ルール (text field), 操作 (削除), テスト |
| MCPサーバに見せないグループ | `layers.saveButNoMcp` | the same |

The tables share their code, never their data: every edit names its layer. Above each table, このグループを有効にする sets the layer's `enabled`; a
disabled layer keeps its rules, and the host checks them as well.

- **Add** (ルール追加): appends `{ kind: "pattern", pattern: "" }` to that layer and moves focus to its field. An empty text is allowed while editing;
  the host refuses it (`invalid_pattern`), so it cannot be applied.
- **Edit**: the select changes `kind` and the field `pattern`, in the draft only. Changing the kind never rewrites the text. Typing changes no row:
  the table is built again only when a rule is added or removed, or the draft is replaced, so the field keeps focus and cursor.
- **Delete**: removes the rule from its layer; focus moves to the 削除 of the rule now in that place, else the one before, else ルール追加.
- **Order**: kept; no reordering in this slice.
- **Dirty**: as in Slice 1, the difference from the active policy. Writing a text back, or adding a removed rule again, is no change.
- **Editable**: only a ready version 2 policy, and not while applying. Version 1, Invalid, Unsupported and Unavailable are as in Slice 1.

Each change counts up the draft's `version`, so an answer can be matched to the draft it was about.

### Validation

検証 sends the whole draft (`policy_validate`); applying does the same first. Nothing is sent while typing. The answer is shown only if the draft is
the same object at the same `version` as when it was sent; otherwise the page says that the draft changed and drops it. One check runs at a time, and a
reload or an apply drops any answer still to come.

| Host problem | Shown |
|---|---|
| `layer` and `ruleIndex` of a row on the page | on that row: "エラー: …" under the field, `aria-invalid` on the field |
| `layer` only (`too_many_rules`) | under that group's table |
| no place (`too_many_regex_rules`, `policy_too_large`, …), or a place the page does not have | in the summary only, never on another row |

Codes become Japanese text (`PROBLEM_TEXT`, `ERROR_TEXT` in `options-page.js`): `invalid_pattern`, `invalid_regex`, `unsupported_regex`, `regex_limit`,
`invalid_rule_form`, `too_many_rules`, `too_many_regex_rules`, `policy_conflict`, `policy_busy`, `policy_unavailable`, `policy_version_mismatch`,
`invalid_etag`, `policy_draft_too_large`, `timeout`, `host_unavailable` and others. No message contains a rule or a URL; the host sends neither back.
Errors are words and a mark, not colour alone. A check is all or nothing: no rule is dropped to make a draft pass.

### Rule test and URL test

Slice 3 moved the rule test into the URL checker and renamed the URL test Policy全体テスト; the URL test below is otherwise unchanged.

One field (URLテスト) holds the test URL. It is sent to the host for the test only: not stored, not queued, not logged, gone with the page.

- **テスト** on a row sends that row's `{ kind, pattern }` and the URL (`policy_test_rule`): 一致しました / 一致しませんでした / テストできませんでした
  and the reason. Without a URL nothing is sent. The page says that a rule alone is no decision: another rule or a basic protection may still refuse
  the URL. A version 1 row has no test (the host tests version 2 rules only).
- **このURLを判定** sends the whole draft and the URL (`policy_test_url`): 保存しません (`never_save`), Vaultに保存しますが、MCPには見せません
  (`save_but_no_mcp`), 保存対象です (`allow`), with "これは編集中の設定によるテスト結果です。現在適用されている設定とは異なる場合があります。" An
  invalid draft gets no decision, and the page makes none up.
- Any change of the draft or of the URL clears every test answer shown, and an answer that arrives after such a change, or after a later run, is
  dropped. So a result shown always belongs to the draft and URL on screen.

### Apply

```text
適用 ─▶ protection may be relaxed? ─ yes ─▶ confirm (キャンセル / 適用する)
   └─▶ lock the page ─ policy_validate(snapshot) ─ refused ─▶ problems shown, nothing sent
                                                 └ ok ─▶ policy_apply(expectedEtag, snapshot), sent once
          applied ─▶ policy_status + policy_get ─ ready ─▶ new active policy, new draft, clean
                                                └ fails ─▶ "適用は成功しましたが、最新状態を取得できませんでした", draft kept, 適用 blocked
          refused (policy_conflict) ─▶ draft kept, 適用 blocked until 再読み込み
          refused (another code)    ─▶ draft kept, reason shown, 適用 again possible
          unknown ─▶ policy_status + policy_get, compared (below)
```

- `expectedEtag` is the ETag of the active policy the draft was made from, and the draft keeps that policy's `revision`, as the host requires. The host
  checks both under its lock and writes revision + 1, all or nothing.
- 適用 is enabled only for a dirty version 2 draft, while nothing else runs and no earlier apply waits to be checked. While applying, every control that
  could change the draft is disabled, as are 変更を破棄, 再読み込み, 検証 and the tests, and a click that still arrives changes nothing; one click sends
  one `policy_apply`. Leaving still asks while the draft is dirty.
- **Conflict** (`policy_conflict`): nothing is overwritten or merged. The draft and the fields stay on screen, the message says so, and 適用 stays
  disabled: the ETag the draft was made from is out of date. 再読み込み reads the current policy, after asking, since it replaces the draft.
- **Refused before writing**: `policy_conflict`, `invalid_etag`, `policy_version_mismatch`, `invalid_policy_draft`, `policy_draft_too_large`,
  `policy_not_initialized`, `unknown_type`, or a refused draft with its problems. Any other failure may have followed the write (the host's store
  writes the file before it reads it back), so it is unknown.
- **Unknown** (no answer, a disconnect, a malformed answer, `policy_busy`, `policy_unavailable`, …): never sent again by itself, and neither success
  nor failure is claimed. The page reads the policy and compares (`compareAfterApply`):

  | Read | Verdict | Page |
  |---|---|---|
  | the ETag and revision the draft was made from | not applied (when read) | draft kept; 適用 again only on a click, still guarded by the same ETag |
  | revision + 1 and exactly the draft | applied | new active policy, clean |
  | anything else | changed by someone else | draft kept, 適用 blocked until 再読み込み |
  | no policy (host gone, …) | still unknown | draft kept, 適用 blocked; 適用結果を再確認 reads again |

  A host process that timed out may still be running; if it writes after the page read "not applied", the next explicit apply gets
  `policy_conflict`, never a second write.

### Confirming a change that may protect less

Before applying, the draft is compared with the active policy (`relaxations`): a category turned off, a layer disabled, or a rule of the active policy
that the draft no longer has with the same kind and text (removed, edited, or its kind changed; compared as a multiset per layer). Adding a rule,
turning protection on or reordering asks nothing. If anything is found, a confirmation opens with the text of the order and a list of what was found,
without rule text, and says that this is a comparison, not a judgement: the URLs a rule meets are not compared. Focus starts on キャンセル, so an Enter
or Space at once cancels; Escape cancels too. キャンセル keeps the draft and sends nothing. 適用する applies only the draft that was confirmed: an
edit meanwhile closes the confirmation.

### Accessibility

Every field, select and button in a row has a label naming the group and the row ("保存しないグループ 2行目のルール"). Tab order follows the page;
buttons work with Enter and Space; focus is visible. After adding, focus is on the new field; after deleting, on the rule now there; after applying, on
the status. Validation, tests, applying, conflicts and unknown outcomes are announced in polite live regions and in words. The confirmation is an
`alertdialog`. At 520 px and below the columns narrow; a long rule scrolls inside its field.

### Measurements

Synthetic policies only; a temporary `MEMOCAMPUS_APP_DIR` created by the harness with `policy_initialize`; Windows 11, Node 24, release host
[verified, in this session].

| Page work (fake DOM, 100 rules per group, median of 20) | ms |
|---|---|
| open and draw (status and get answered at once) | 1.9 |
| one keystroke in a rule | 0.28 |
| delete a rule (one table built again) | 0.77 |
| dirty comparison, clean draft | 0.034 |
| relaxation comparison | 0.28 |

| Host round trip, including the host process start (median of 10) | ms |
|---|---|
| empty request (`unknown_type`): start alone | 17.5 |
| `policy_status` | 20.2 |
| `policy_get` | 21.4 |
| `policy_validate` (200 rules, 25 of them regex) | 20.8 |
| `policy_test_rule` | 20.0 |
| `policy_test_url` (200 rules) | 23.1 |
| `policy_apply` | 39.3 |

Applying is therefore about four round trips (validate, apply, status, get), some 100 ms. Chrome's own cost of starting a native host is not in these
figures and was not measured.

### Tests

`tests/extension/options-edit.test.js`: editing per group (add, edit, kind, delete, focus, independence, discard, dirty, no rebuild while typing with
100 rules), layers, version 1 and the model's refusals; validation (valid, rows, layers, summary, disabled layer, stale, reload, no answer); rule and URL
tests (host answers shown as given, no URL no request, every decision, invalid draft, stale answers); applying (success, invalid draft, refusals,
conflict, one click, success with a failed read, lost answer applied, unknown and not applied, unknown and unreadable, changed meanwhile, the
classification and the comparison); each relaxing change with its confirmation; hostile text and URLs in no attribute, message or log; the messages
sent. The Slice 1 tests stay, adapted where Slice 2 changed the page (editable rows, 適用, Unsupported).

## Slice 3: URL checker

The person writes a rule and a URL, the host says whether the rule matches, and only a match lets the rule enter a group of the draft. This is a
change of the page only: the host's matching (`policy_test_rule`, `policy::rule_matches`) and the policy engine are unchanged, and applying is
Slice 2's 適用.

### Page

Sections, top to bottom: 適用中のプライバシー設定, 基本保護, **URLチェッカー**, 保存しないグループ, MCPサーバに見せないグループ, Policy全体テスト, 管理,
and the apply bar. The checker, top to bottom:

```text
ルールの種類   ○ Pattern  ○ Regex            radio group (name="checker-kind"), Pattern first
ルール [?]     [ text field ]                  input type="text"; ? = help
チェック対象URL [ text field ]                  input type="text" (not type="url")
[ マッチングを確認 ]                             also Enter in either field
判定結果       未チェック / 確認中… / 一致しました / …   role="status", aria-live="polite"
               この結果はルール単体の一致確認です。実際の保存判定は、適用中のPolicy全体で決まります。
[ 保存しないグループに追加 ]  [ MCPサーバに見せないグループに追加 ]   or, while editing: [ 変更を反映 ] [ 編集をやめる ]
```

- **Fields as typed.** Neither field is trimmed, case-folded, normalized, percent-decoded or completed; what is on screen is what is sent and what is
  added. A long URL scrolls inside its field. The fields live in the page's memory only (no storage, console or telemetry) and stay after an add, a
  reload or a discard; closing the page loses them.
- **Enter** in the rule or URL field checks, except while an input method composes (`isComposing`, or `keyCode` 229). With a field empty nothing is
  sent and focus moves to the empty field. While a question is on its way, the same question is not sent again (Enter or click, any number of times).
- **Help** (`?`): one sentence, "PatternはMemocampusのパターン照合です。RegexはRust Regex構文で判定します。先読み・後読みには対応していません。" It opens on
  hover, on keyboard focus and on click (a click keeps it open; Escape closes it), with `aria-expanded`. No cheat sheet. The page links nowhere: its
  no-external-URL test stays.

### Host matching

マッチングを確認 sends `policy_test_rule` with `{ kind, pattern }` and `url`, as typed, through `callHost` (request id and reply type checked). The page
has no matching of its own: no `RegExp`, no URL parsing, no pattern logic (a test checks the page's modules for them). The answer becomes a state
(`matchState`):

| Host answer | 判定結果 | Groups |
|---|---|---|
| none yet | 未チェック | disabled |
| on its way | 確認中… | disabled |
| `matched: true` (a boolean) | 一致しました | enabled (unless already there) |
| `matched: false` | 一致しませんでした | disabled |
| `invalid_rule`, `invalid_pattern`, `invalid_regex`, `unsupported_regex`, `regex_limit` | ルールを確認してください。 + the reason | disabled |
| `invalid_url` | URLを確認してください。 + the reason | disabled |
| timeout, no host, a disconnect, a malformed or foreign reply, any other code | 判定できませんでした。 + the reason | disabled |

The text names the result; colour and mark (✓ ✕ ▲) only repeat it. No message contains the rule or the URL.

### Match gate

保存しないグループに追加 / MCPサーバに見せないグループに追加 add the rule only if all of these hold, checked both when the buttons are drawn and again
inside the click (so a script's `click()`, a double click or a stale button gets nothing):

1. a ready version 2 policy, and no apply or read-back running;
2. a known kind and a rule text that is not empty, and a URL that is not empty;
3. the latest answer is a match (`matched: true`, reply checked by `callHost` and `testRule`);
4. that answer is about exactly the kind, text and URL the checker holds now, in this reading of the policy;
5. the target group does not have the rule (see Duplicates).

Staleness: each check records the kind, text and URL it asked about and a token (`createLatestGate`). Any change of the kind, the text or the URL
drops the answer at once, and an answer arriving after such a change, or after a later question, is dropped. A reload, a discard and an apply
(refused or not) forget the check and any edit, and drop an answer still on its way, so a match found before one is not used after it. A change that reaches the fields without an event (a script) is still caught:
the gate compares the answer with the fields every time.

### Duplicates

A rule is the pair `kind` and `pattern`, compared exactly (`sameRule`): case, spaces and Unicode form count; Pattern and Regex are different rules; two
regexes that mean the same but are written differently are two rules. The check is per group: a rule of 保存しないグループ may be added to
MCPサーバに見せないグループ too (the page then notes that 保存しないグループ wins, so a matching URL is not saved). A group that has the rule shows
"登録済み" and its button stays disabled; the click checks again, and the draft model (`appendRule`) refuses a duplicate as well.

### Adding: draft only

An add appends the rule to `draft.policy.layers.neverSave.rules` or `…saveButNoMcp.rules`, nothing else: the active policy stays as it is, no
`policy_validate` or `policy_apply` is sent, and the draft becomes dirty, so the rule is applied by Slice 2's 適用 (checked and guarded by the ETag).
After an add the page scrolls to the new row (smoothly, or at once under `prefers-reduced-motion`), marks it for two seconds with an outline (no
animation under reduced motion), moves focus to its チェッカーで編集, and says "下書きに追加しました"; the fields stay, so the same rule can be added to the
other group. A disabled group takes the rule and says "このグループは無効です。ルールは追加されますが、適用後もグループを有効にするまで判定に使われません。";
the page never turns a group on by itself.

### Editing a rule

The tables have no typed fields and no ルール追加: a row shows its kind and text and has チェッカーで編集 and 削除.

1. チェッカーで編集 loads the row's kind and text into the checker and remembers the group, the row, the rule and the group's layout counter. The draft
   does not change. The checker shows "編集中: 「…」のn行目のルール", hides the add buttons and shows 変更を反映 and 編集をやめる; the row is marked.
2. The person changes the rule and the URL and checks. 変更を反映 is enabled only under the match gate above, and only if the rule differs from the
   one being edited and no other rule of that group is the same.
3. 変更を反映 replaces that rule only (`replaceRule`), and only if the row still holds the rule that was loaded and the group was not rearranged since
   (a rule of it deleted, added or replaced, or the draft discarded or read again). Otherwise nothing changes and the page says that the rule changed
   or was deleted; another row is never written. Other groups are untouched.
4. 編集をやめる ends the edit; the draft stays as it was, the fields stay.

An edited rule is "a rule of the active policy the draft no longer has", so applying asks first (Slice 2's confirmation). 削除 needs no match and still
asks before applying. Categories and the group switch stay as they were: they change no rule.

So there is no way to put a rule into the draft that the host has not just matched: not by typing in a table, not by an unchecked add, not by an
edit. The active policy's own rules are shown, kept, deleted and applied again without any check.

### Policy-wide test

Policy全体テスト is Slice 2's URL test, with its own URL field: it sends the whole draft and the URL (`policy_test_url`) and shows 保存しません /
Vaultに保存しますが、MCPには見せません / 保存対象です with "これは編集中の設定によるテスト結果です。現在適用されている設定とは異なる場合があります。" The
checker answers whether one rule matches; this answers what the draft as a whole decides. The two fields and results are separate, and the text of each
section says which is which. Neither stores, queues or logs a URL, and neither result is a permit: recording judges with the stored policy only.

### Accessibility

A fieldset with a legend groups the two radios (arrow keys move between them); both fields have labels; the result is a polite live region; the reason
a button is disabled is in words under the result (`checker-gate`) and per group (`checker-…-note`), linked with `aria-describedby`. Focus: an empty
field on a check without input, the rule field when an edit starts or is cancelled, the new or changed row after an add or a change, the group heading
after the last rule is deleted. Typing in the checker draws the checker only, so no row is built again and focus stays. Dark theme and narrow windows
use the page's colours and its 520 px rules; the group buttons wrap.

### Measurements

Synthetic policies only; a temporary `MEMOCAMPUS_APP_DIR` created by the harness with `policy_initialize`; Windows 11, Node 24, release host
[verified, in this session].

| Page work (fake DOM, 100 rules per group, median of 20) | ms |
|---|---|
| open and draw | 2.7 |
| one keystroke in the checker's rule field | 0.010 |
| one keystroke in the URL field, 30,000-character URL | 0.011 |
| add a matched rule (table built again, scroll, focus) | 1.1 |
| reflect an edit | 1.2 |
| duplicate check (`hasRule`, 100 rules) | 0.003 |

| Host round trip, including the host process start (median of 10) | ms |
|---|---|
| empty request (`unknown_type`): start alone | 22.1 |
| `policy_test_rule`, regex and pattern | 20.8 |
| `policy_test_rule`, 30,000-character URL | 25.4 |

The real host, through the page: a regex and a pattern match; a non-match; look-around (`unsupported_regex`), `(` (`invalid_regex`), `a b` and
` bank.example` (`invalid_pattern`), `not a url` and a 40,000-character URL (`invalid_url`) all refuse both groups; a duplicate shows 登録済み; an edit
is reflected; Policy全体テスト says 保存しません; the policy files are byte for byte the same until 適用, which writes revision 2 with the rule.
Chrome itself was not used.

### Tests

`tests/extension/options-checker.test.js` (new): the page's order and labels; fields sent as typed; Enter, composing, empty fields and repeated
questions; a long URL; every host answer and its state; the host's answer shown whatever the rule looks like; stale answers after a rule, URL or kind
change, out of order, after a reload, a discard or an apply; clicks without a current match (also after a change without an event); a second click and
a duplicate that arrives meanwhile; nothing while applying; duplicate identity (case, spaces, kind, Unicode form, equivalent regexes) and per group;
adding to each group (draft only, no apply, dirty, scroll, mark, focus), reduced motion, a disabled group; editing (loaded, nothing before a match, no
match, unchanged, reflected in place, relaxation), a target deleted, moved or discarded or read again, a copy of another rule, cancelling, deleting
without a match; the help; nothing logged, stored or put in an attribute; no matching code in the page's modules.

`tests/extension/options-fixtures.js` (new) holds the fake host and the helpers both page test files use. The Slice 2 tests stay and now edit
through the checker (`addRule`, `editRule`); their checks are unchanged, except that the per-row テスト (now the checker) is gone, an invalid draft is a
matched rule the whole-draft check refuses (the empty rule cannot be added any more), and the lists of messages sent leave out the checker's
`policy_test_rule`. The Slice 1 tests stay, adapted to three columns, read-only rows and the new labels.

## Domain Wildcard in the URL checker

The host's third kind of rule (`domainWildcard`, `issue-025-policy-engine.md`, Domain Wildcard) is the checker's third kind. Nothing else about
adding or changing a rule is new: it goes through the same match gate.

- **Choosing it.** 「ルールの種類」 has three radios of one group: Pattern, Regex, Domain Wildcard (`value="domainWildcard"`). The rule stays one line
  of text. With Domain Wildcard chosen, the rule field shows the example `*.example.com` and a note under the help: no scheme or path, the form
  `*.example.com`, and that the domain itself also matches; the rule field names that note as its description only while it is shown, since a
  screen reader reads a hidden description too. The other kinds show neither. The `?` help adds the kind: the domain and all subdomains,
  by host name alone, and no public suffix such as `*.com` or `*.co.jp`.
- **Judged by the host only.** The page sends `{ kind: "domainWildcard", pattern }` exactly as typed and shows what `policy_test_rule` answers. It has
  no domain, suffix or host logic of its own. `invalid_domain_wildcard` is a rule error (「ルールを確認してください」) with a reason that names the form
  and that public suffixes, subdomains, schemes, paths, ports and spaces cannot be used; it never repeats the rule.
- **Gate, staleness, adding, editing.** As for the other kinds: only a boolean `matched: true` for the current kind, text and URL enables the buttons
  and is checked again on the click; a change of the kind (also to or from Domain Wildcard), the text or the URL drops the answer; adding writes the
  draft only; 「チェッカーで編集」 loads the kind and text, and 「変更を反映」 needs a new match. A changed or removed domain wildcard, and a rule changed
  into one, ask before applying (the rule's kind and text identify it).
- **Duplicates.** The same kind and the very same text, in the same group. `*.Example.com` and `*.example.com` are two rules, and so are the pattern
  `*.example.com` and the domain wildcard `*.example.com`, though the host matches them alike. Nothing is normalized or merged.
- **Rows.** A domain wildcard is shown as 「Domain Wildcard」 with its text as stored (`*.例え.jp` is not shown as Punycode).
- **No migration.** Existing policies are not converted; a version 1 policy stays read-only as before.

Tests: `tests/extension/options-domain.test.js` (the radios and their order, the example and note, the text sent as typed, match, no match, invalid
rule, invalid URL and host failures, the host's answer shown whatever the rule looks like, staleness by rule, URL and kind, duplicates, editing with
the confirmation, atomic apply and conflict, display, a draft error named by the host, and the extension's evaluator refusing the kind), with the
checker, edit and page tests above. Real host [verified]: the page against the release host in a temporary app directory matched and refused the
order's examples, added and applied rules (revision 2, stored as written), and the batch judged them.

## Handoff

Slice 3 became the URL checker (above). Still open, for a later slice:

1. Version 1: explicit `policy_migrate` with the ETag, then edit. Not implemented: a version 1 policy is still shown read-only, and the checker is
   disabled for it.
2. Invalid or unsupported policy: a recovery message and its page. `policy_apply` cannot replace a policy that cannot be loaded (#25 Slice 6). A
   recovery that writes a new policy must go through the same gate: rules it brings come from a policy the host checked, not from typed text.
3. Error cases end to end, Chrome dogfood.
4. From Slice 2: a person with a conflict loses the draft on 再読み込み; a view of both, or carrying the draft over, is not built. The page does not
   notice a change made elsewhere until it reads again.
5. Reordering rules, searching and filtering them.
6. From Slice 3: the match gate checks one rule against one URL; it says nothing of the URLs the rule meets besides that one. A checked rule that
   matches far more than intended is still added. The confirmation before applying compares rules, not what they match.

Later: CSV import and export per group (`regex,pattern`, RFC 4180, UTF-8), not in Slices 1–3.
