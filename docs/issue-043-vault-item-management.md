# Issue #43: managing one item of a search result — Slice 1 (native host), Slice 2 (exact rules), Slice 3 (side panel)

From a URL or an image of the side panel's search results, four operations: **保存しないグループへ追加** (Never Save),
**MCPサーバに見せないグループへ追加** (Save but no MCP), **Vaultから削除**, and **公開**. Slice 1 is the native host only:
the messages, their checks and their tests. Slice 2 adds the policy rule kind `exact` (one whole URL) and uses it for
scope `url` instead of Slice 1's escaped regex (see *Slice 2: exact rules*). Slice 3 is the side panel (see *Slice 3:
the side panel*).

**Not in Slice 1:** the drop-down and the confirmation dialogs, bulk selection or deletion (Issue #7 plans deletion of
single visits and in bulk; this is deletion of one URL), a trash or undo, a new per-item publication flag, a new policy
engine, a schema change, an automatic release of broad rules.

## What exists, and what is reused

| Area | Found | Used as |
|---|---|---|
| Policy | `policy.json` (`PolicyStore`): version 2, two layers `neverSave` and `saveButNoMcp` (each `enabled` and up to 1000 rules), builtin categories. Rules: `pattern` (host, optional path; no scheme, port, query or fragment), `regex` (the whole URL, at most 128 per policy and 4096 bytes each), `domainWildcard` (`*.` and a registrable domain), and since Slice 2 `exact` (one whole URL). | Unchanged. Changes are drafts stored with `PolicyStore::update`, as `policy_apply` stores them. |
| Evaluation | `CompiledPolicy::evaluate`: Never Save first (rules, authentication category, auth paths, credentials), then Save but no MCP; a URL that cannot be judged is Never Save. Several matching rules: the stricter layer wins; within a layer, any match. | Every `current` and `expected` decision is this function's. No other judgement is written. |
| Revisions | Each stored policy has a `revision` and an `etag` (hash of the file); `PolicyStore::update` refuses unless the stored policy is still the expected one (`policy_conflict`), under its write lock. | Preview returns the `etag`; apply takes it back as `expectedEtag`. |
| MCP | `VisibleStore` reads and compiles the policy again for every request, and keeps nothing. A visit is public only if its URL is `allow`; an image save only if `mcp_public = 1` and each of its image, page and frame URLs is `allow`. | Nothing to invalidate: the next MCP answer follows a stored change (tested end to end). |
| URLs | A search result is one `url` text, aggregated from `visits` and `tab_snapshots`. `page_metadata` has one row per URL (FK to a visit, `ON DELETE CASCADE`). `navigation_events`: target `ON DELETE CASCADE`, source `ON DELETE SET NULL`. | The deletion unit is that `url` text, compared exactly (`=`). |
| Images | `image_blobs` (one per SHA-256, the `blobId` of `image_list`), `saved_images` (save history, `ON DELETE RESTRICT`), `image_thumbnails` and `image_tag_links` (`ON DELETE CASCADE`), trigger `image_tags_drop_unused` (schema 7). | The deletion unit is the image (`blobId` + `sha256`). |
| Vault | `vaultToken` (identity of the active vault file), `Session::write_for` (checks under the write lock that the vault is still the one of the token). | Every deletion names the vault, like tag writes and `image_read`. |

### A point the issue could not keep as written

**"URL完全一致" is not a pattern.** The pattern grammar has no query, fragment, scheme or port: `forum.test/thread/1` also
matches `?page=3`, `http://`, and any port. Slice 1 wrote a rule for one URL as an escaped regex
`^…$`, which cost one of the 128 regex rules each and showed as a regex. Since Slice 2 it is an **exact rule** (see
*Slice 2: exact rules*). A version 1 policy has neither: it must be migrated first (`policy_migration_required`).

**"ドメイン単位" is the host.** Only an explicit `scope: "host"` gives a pattern rule of the URL's host: every path, query,
scheme and port of that host, but not its subdomains. Wider rules (`*.example.com`, a domain wildcard, regexes) stay
with the settings page.

## Messages

Every message carries `requestId`; every reply `ok`, `type` (`<message>_result`) and `requestId`. A failure is
`{ ok: false, error }` and has changed nothing. The host also names the capability `vault_items_v1` in
`host_capabilities`, for a side panel to ask before it shows these operations.

### Policy: `policy_item_preview`, `policy_item_apply`

```text
→ policy_item_preview { url, action, scope? }
← { etag, revision, url, action, scope, current, expected, changes, affectsOtherUrls, executable, reason?, blockers? }
→ policy_item_apply   { url, action, scope?, expectedEtag }
← { etag, revision, url, action, scope, previous, decision, changed, changes }
```

- `action`: `never_save`, `save_but_no_mcp`, `publish`. `scope`: `url` (default) or `host`; `publish` takes `url` only.
- `url`: an `http(s)` URL the engine can judge (at most 32,768 UTF-16 units); otherwise `invalid_url`.
- `current`, `expected`, `previous`, `decision`: `allow`, `save_but_no_mcp`, `never_save`, from the engine.
- `changes`: `[{ op: "add" | "remove", layer: "neverSave" | "saveButNoMcp", rule: { kind, pattern }, ruleIndex? }]`.
- `affectsOtherUrls`: true for a host rule.
- `executable: false` comes with `reason`:

| `reason` | Meaning |
|---|---|
| `publish_blocked` | Another rule or a builtin keeps the URL private. `blockers`: `{ layer, ruleIndex, kind }` per matching rule of an enabled layer, `{ layer, builtin: true }` for a builtin category. Never the text of a rule. |
| `layer_disabled` | The layer is off: the rule would change nothing now. |
| `policy_migration_required` | A version 1 policy (`policy_migrate` first). |
| `policy_limit` | Too many rules (1000 per layer) or regexes, or the file would be too large. |
| `unsupported_url` | No pattern can name this host (an IPv6 address). |

- Apply: `policy_conflict` unless the stored policy is still the one of `expectedEtag` (the preview's), checked here and
  again by the store under its lock. The plan is computed again from that same policy, so it is the plan the preview
  showed. An unexecutable plan is refused with its `reason`. A plan with no change writes nothing (`changed: false`).
  `decision` is judged on the stored result.
- Other errors: `invalid_action`, `invalid_scope`, `invalid_etag`, and the policy states of `policy_get`
  (`policy_not_initialized`, `policy_invalid`, `policy_busy`, …).

**Adding** a rule never makes anything more public (the engine is monotonic: one more rule, never a looser decision).
An identical rule already in the layer is a no-op. Save but no MCP on a Never Save URL is added (it outlives the Never
Save rule) and the URL stays Never Save. **Nothing stored is deleted by a rule**: the vault keeps the rows, and MCP hides
them from its next request on. Recording obeys the new rule from the next event on.

**Publishing** is not a permission and adds no rule. It removes this URL's URL-exact rules (the ones scope `url` adds),
from both layers, and only if the engine then says `allow`. If anything else still keeps the URL private — a host rule,
a wildcard, a domain wildcard, another regex, a builtin category, a credential in the URL — the whole operation is
refused (`publish_blocked`) and nothing is removed, so one rule is never removed while another still applies, and no
rule that also covers other URLs is touched. Those are changed on the settings page. A URL the policy allows is a
no-op.

### URL deletion: `url_delete_preview`, `url_delete`

```text
→ url_delete_preview { url }              ← { vaultToken, url, counts, irreversible: true }
→ url_delete         { url, vaultToken }  ← { vaultToken, url, deleted, irreversible: true }
```

`counts` / `deleted`: `{ visits, tabSnapshots, pageMetadata, navigations, navigationSourcesCleared, imageSavesKept }`.

- Deleted: every visit and tab snapshot whose `url` is exactly this text, the URL's page metadata, and the navigation
  row of each of its visits.
- Kept: every other URL, also the same page with another query. A navigation of another visit that came from one of
  these visits keeps its row, with source `NULL` (`navigationSourcesCleared`); no visit is joined to another. Images
  saved from the page are not deleted (`imageSavesKept` says how many name this URL).
- One transaction, committed before the reply; the counts are taken in it and the result is checked (nothing of the
  URL left), otherwise everything rolls back (`db_error`). A URL with nothing stored is a no-op of zero counts.
- The policy is not read: if it allows the URL, a later visit is recorded again. To stop that, add a rule.
- Errors: `invalid_url`, `invalid_vault_token`, `vault_switched` (the vault of the token is not the active one),
  vault states (`vault_not_configured`, `vault_busy`, …), `db_error`.

### Image deletion: `image_delete_preview`, `image_delete`

```text
→ image_delete_preview { blobId, sha256 }              ← { vaultToken, blobId, sha256, byteLength, counts, irreversible: true }
→ image_delete         { blobId, sha256, vaultToken }  ← { vaultToken, blobId, sha256, deleted, irreversible: true }
```

`counts` / `deleted`: `{ saves, thumbnails, tagLinks, tagsRemoved }`.

- Deleted: every save of the image (its whole save history), its bytes, its thumbnail, its tag links, and each of its
  tags no other image has (`image_tags_drop_unused`).
- Kept: every other image and its tags, every visit and URL.
- `blob_missing` if the id is unknown or names other bytes now; `vault_switched` for another vault; the same
  transaction rules as for URLs.
- Afterwards the image is in no list, tag search, save history or read (`image_read` answers `blob_missing`).

### Irreversible

Both deletions are final: no trash, no undo. A backup made before (`vault_backup`) is a separate file and still holds
the data. The vault file does not shrink (no `VACUUM`); deleted pages are reused by SQLite. A side panel must say so in
its confirmation, with the counts of the preview.

## Safety, in short

- Policy: only drafts of the stored policy, judged by the engine and stored by `PolicyStore` (validation, approvals,
  size, revision, lock). Publishing removes only URL-exact rules of the URL, and only when the result is `allow`.
- Preview then apply: the ETag of the preview is required; a policy changed in between is `policy_conflict`.
- Vault: every deletion names its vault; `vault_switched` is checked again under the write lock, so nothing of one vault
  is deleted in another (tested with a real switch).
- Exactness: SQL with parameters only, URL compared with `=` (no `LIKE`), images by id and hash.
- Answers hold no rule text other than the rule being added or removed (from the URL the caller sent).

## Tests

- `native-host/src/policy_gate/items/tests.rs`: URL rules match exactly one URL (queries, fragments, schemes, ports,
  subdomains, regex metacharacters); host rules only when asked; idempotence; monotonicity; disabled layers; publishing
  removes only this URL's rules, refused with blockers for host rules, wildcards, domain wildcards, regexes, builtins and
  credentials, never relaxed to the other layer; version 1; limits; invalid URLs; preview then apply through a
  `PolicyStore`, stale ETag refused, refused plans write nothing.
- `native-host/src/vault_items/tests.rs`: URL deletion (all visits, snapshots, metadata, navigations, `SET NULL`, other
  URLs and images untouched, search), exact match (`%`, `_`, case, trailing `/`), rollback on a failure part way;
  image deletion (saves, thumbnail, tags, other image untouched, list), hash mismatch, rollback; requests checked
  before any vault is opened.
- `native-host/tests/vault_items.rs` (the real host binary, temporary vault and policy): the URL and image flows with
  integrity and foreign key checks and a backup that keeps the data; a deletion for the previous vault refused in the
  new one after a real switch, both vaults unchanged; policy changes obeyed by recording (`dropped: policy`) and by
  `policy_evaluate_batch`, a stale preview refused, publishing blocked by a host rule writes nothing.
- `tests/extension/vault-items.test.js` (opt-in: `MEMOCAMPUS_TEST_HOST` and `MEMOCAMPUS_TEST_MCP`): one MCP process
  follows each change at once — a URL hidden, published, hidden with its host; an image hidden through its page and
  published; a URL and an image deleted.

## Notes for the side panel (written for Slice 1)

- The drop-down on each URL and image result; `host_capabilities` names `vault_items_v1`.
- Current state: one `policy_item_preview` per opened item (not per row). For an image, the URLs that judge it are its
  image, page and frame URLs, and its `mcpPublic` flag per save (`image_mcp_public_set`, which exists already): the UI
  must show which of them keeps it private, and which URL a rule would be for.
- Confirmations: the preview's `changes`, `expected`, `affectsOtherUrls` (host scope), and for deletions the counts and
  that they are irreversible. Apply with the preview's `etag` / `vaultToken`; on `policy_conflict` or `vault_switched`,
  preview again.
- `publish_blocked`: name the blocking layer and rule kind, link to the settings page.
- (Settled by Slice 2: URL-exact rules are `exact` rules, outside the regex limit.)

## Slice 2: exact rules

A version 2 rule may be `{ "kind": "exact", "pattern": "https://example.com/thread/123?page=3" }`: that one URL, and no
other. It is a kind of its own (`TypedRule::Exact`), in either layer, judged in the same pass as the others: Never Save
first, the builtins as before.

**What "the same URL" means.** The engine already reads every URL with one parser (the WHATWG URL parser of the `url`
crate) and gives regex rules the URL as that parser writes it, and the same without the trailing dots of its host. An
exact rule uses exactly that form for both sides (`policy::exact_url_key`), and compares the two strings:

| Part | Same URL | Another URL |
|---|---|---|
| Scheme | case (`HTTPS:` = `https:`) | `http` vs `https` |
| Host | case, IDN = its punycode, trailing dots (`example.com.`) | `www.` or any other host |
| Port | the default port written or not (`:443` on https) | any other port |
| Path | dot segments (`/a/./b`, `/x/../b`), a raw character the parser encodes (`/漫` = `/%E6%BC%AB`), an empty path = `/` | a trailing `/`, case, `%61` vs `a`, `%e6` vs `%E6` |
| Query | — | any difference, also `?` vs none, order, case |
| Fragment | — | any difference, also `#` vs none |

Nothing else is normalized, and no new normalization was written: this is the parser's output, as a regex rule saw
it. A rule is accepted only if it is an `http(s)` URL the policy can judge (`normalize_url`: not empty, no space or
`\`, no bad percent-encoding, at most 32,768 characters); otherwise the problem is `invalid_exact_url`, and as for every
kind, one bad rule makes the whole policy unusable (fail closed), also in a disabled layer. A `blob:` URL is judged by
the URL inside it, as before.

**Limits.** An exact rule compiles no regex: the rules of a layer go into a set of these strings, looked up once per
URL. It does not count against the 128 regex rules or their size and DFA budget (the budget of each regex is the same
with or without exact rules). The limits that hold are those of every rule: 1000 rules per layer, and the size of the
policy file.

**Item API.** Scope `url` adds `{ kind: "exact", pattern: <the URL as the parser writes it> }`. An exact rule of the same
URL written in another form counts as present (no duplicate). `publish` removes every exact rule of the URL, however
written, from both layers, under the same conditions as before; a regex, also a Slice 1 regex for this URL, is a rule
like any other and blocks publishing (there is no migration of Slice 1 rules; Slice 1 was not released).

**Settings page.** The checker has a fourth kind, 「Exact URL」 (`value="exact"`); rows show 「Exact URL」 and the URL as
stored, unescaped. Added, changed and removed through the checker like the other kinds (the rule must match the
checked URL, so the URL is typed twice); the host's `invalid_exact_url` has its own message.

**Not changed:** the pattern, regex and domain wildcard rules, the builtins, the order of the layers, `policy_test_rule`
and the other messages, VisibleStore and MCP (they use the same evaluator), the JavaScript reference evaluator of the
tests (`extension/policy.js`, which judges patterns only and refuses every other kind, as for regexes).

## Slice 3: the side panel

The four actions in the URL tab and the Image tab, through the host messages above. One Rust change: a successful
`search_pages` answer now names its vault (`vaultToken`, as `image_list` does), so a URL deletion can be bound to the
vault the list came from.

### Where

- **URL tab.** Under each result, a small line: the host's decision for the URL (「公開」「MCPサーバに見せない」
  「保存しない」, or 「状態不明」) and a drop-down 「操作を選択…」 with the four actions. The link above keeps its whole
  width. The decisions come from one `policy_evaluate_batch` per page of results (the host's judgement, URLs only),
  and are asked again after every policy change made here.
- **Image tab.** In the image details, a section 「管理」 above the save history, with the same drop-down and a line
  that says how the host judges the URLs of the loaded save history (and that more may follow).
- Only a host that names `vault_items_v1` (`host_capabilities`) gets these. Before an answer nothing is shown; an older
  host shows no drop-down in the URL tab, and the image details say the host must be updated.

### Disabled choices

The host's decision, never a judgement of the page's own:

| Action | Disabled when the URL is | Why |
|---|---|---|
| 保存しないグループへ追加 | 保存しない | it cannot be stricter |
| MCPサーバに見せないグループへ追加 | MCPサーバに見せない, 保存しない | it would not change the state |
| 公開 | 公開 | nothing to remove |
| Vaultから削除 | never | |

An unknown state disables nothing: the preview decides. A Save but no MCP URL can still be added to Never Save (stricter).
For an image an action is disabled only when the whole save history is loaded and every URL of it is in such a
state, or it has no URL a rule can be for. Whether an action can be done at all (a broad rule, a disabled layer, a
limit) is the preview's answer, shown in the dialog.

### Preview, confirmation, apply

Choosing an action does nothing but open the dialog (`item-dialog.js`, one for both tabs), which asks the host first:

- **Policy:** `policy_item_preview` for the URL, scope 「このURLのみ」 (exact rule) by default. The dialog shows the URL,
  the scope, the current and expected state as the host says, the rules added or removed (kind and text), and that a
  rule deletes no stored data. 「同じホスト全体」 is a second radio; choosing it previews again, says in red that other
  URLs are affected, and the confirm button stays disabled until a checkbox acknowledges it. `executable: false` is
  explained in words (and `publish_blocked` lists the blocking layer and rule kind, or the builtins, never a rule's
  text) with a button 「Privacy設定を開く」; nothing can be confirmed. A preview with no change (the same rule exists,
  the URL is public already) cannot be confirmed either.
- **Confirm** sends `policy_item_apply` with the preview's ETag. Only the host's success closes the dialog, resets the
  drop-down and shows the new state. `policy_conflict` (the policy changed elsewhere) and every other failure keep the
  dialog with the reason; the confirm button stays disabled until 「もう一度確認」 previews again. A failure is never
  shown as done, and nothing is sent again by itself.
- While a request runs, every control of the dialog is disabled (one request per confirmation), and Escape and
  「キャンセル」 wait for the answer.

### Images: which URL

A rule is for a URL, never for the image. Choosing a policy action for an image loads its whole save history (at most
1,000 saves; beyond that the action is refused and the settings page is the way), collects the `http(s)` URLs of it —
image URL, page URL, frame URL; a `data:` image has none — once each, with what they are and how many saves name them,
and has the host judge them. The dialog lists them as radios, **none chosen**: the person picks the URL, and only then
is it previewed. A URL whose state the action would not change is shown but cannot be chosen.

`mcp_public` is not changed. Every policy dialog for an image says that an image reaches MCP only when a save allows it
(「MCPへの公開を許可」) and its image, page and frame URLs are all public, and how many saves allow it; with none,
that publishing a URL will not make the image appear in MCP.

### Deleting

- **URL:** `url_delete_preview`, then a dialog with the host's counts (visits, tab snapshots, page metadata, the
  navigations to it, the navigations from it that keep their row without a source), that image saves are kept, that a
  backup keeps the data, that it cannot be undone, and that a later visit may be recorded again. 「削除する（取り消せません）」
  sends `url_delete` with the vault token of the preview. If the preview's vault is not the one the list came from
  (switched in another window), nothing is deleted: the dialog closes and the list reloads. After the host's success
  the row goes, the count goes down by one, and the next page starts one row earlier (no URL skipped or repeated).
- **Image:** `image_delete_preview` (with the gallery's vault token checked the same way), a dialog with the host's
  counts (the original's size, every save, the thumbnail, tag links and tags that disappear with it), the backup and
  undo notes; `image_delete` with id, hash and the preview's token. After the host's success the tile and the details
  go (and an open tag picker closes); other images, their tags and thumbnails are not touched. An image the vault no
  longer has is said to be gone, with nothing to retry.
- `vault_switched` at deletion: nothing deleted, the dialog closes, the list starts again in the active vault.

### Async rules

The dialog belongs to one item. A new search, a new page of results replacing the rows, a vault change, another
selected image or closed details close it; every answer carries the opening and step it was asked for and is dropped
when they are no longer current. Decisions of rows carry a stamp, so an older batch never overwrites a newer one.

### Tests

`tests/extension/vault-items-ui.test.js` (fake DOM, a fake host with a small policy of exact URLs and hosts): states
and disabled choices, preview before anything, exact rules, host scope only with acknowledgement, publishing and
`publish_blocked` with the settings link, ETag conflict and preview again, failures not shown as done, Escape, double
clicks, URL deletion with counts, paging after a deletion, a vault switched before the preview or before the
confirmation, a new search or vault switch while a preview is pending, image actions with a chosen URL, `mcp_public`
notes, image deletion with tags, a vault switched elsewhere, another selection dropping a late preview, an image gone,
an older host. Opt-in (`MEMOCAMPUS_TEST_HOST`): the side panel against the real host — a URL hidden, published and
deleted, an image deleted, then `integrity_check` and `foreign_key_check`.

### Checking it in Chrome (for the PO)

In a **test vault**, with this branch's extension and host:

1. URL tab: each result shows a state and 「操作を選択…」. Choose 「保存しないグループへ追加」: the dialog shows the URL,
   「このURLのみ（Exact URL）」 and the rule; nothing changes until 「保存しないグループへ追加する」. Afterwards the state reads
   「保存しない」 and the Privacy settings page lists the rule as 「Exact URL」.
2. The same with 「同じホスト全体」: the warning and the checkbox; the confirm button waits for it.
3. 「公開」 on that URL: the rule goes. Add a host rule in the settings page, then 「公開」 on a URL of that host: refused
   with the reason and 「Privacy設定を開く」.
4. Keep a preview open, change the policy in the settings page, confirm: 「ほかの画面で変更されました」, then 「もう一度確認」.
5. 「Vaultから削除」 on a URL: the counts, the warnings; after deleting, the row and the count go; scroll for more results.
6. Image tab: open an image with several saves; 「MCPサーバに見せないグループへ追加」 lists its URLs, none chosen; pick one.
   「公開」 shows the note about 「MCPへの公開を許可」.
7. 「Vaultから削除」 on an image with tags: the counts; afterwards the tile and the details are gone, other images keep
   their tags; copy and export of other images still work.
8. With a dialog open, switch the vault in Settings: the dialog closes, nothing is deleted.
9. Keyboard only: Tab to a drop-down, choose with the arrow keys, Escape closes the dialog and focus returns. Narrow
   and wide side panel, light and dark mode.
