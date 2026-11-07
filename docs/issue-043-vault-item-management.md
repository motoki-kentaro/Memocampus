# Issue #43: managing one item of a search result — Slice 1 (native host), Slice 2 (exact rules)

From a URL or an image of the side panel's search results, four operations: **保存しないグループへ追加** (Never Save),
**MCPサーバに見せないグループへ追加** (Save but no MCP), **Vaultから削除**, and **公開**. Slice 1 is the native host only:
the messages, their checks and their tests. Slice 2 adds the policy rule kind `exact` (one whole URL) and uses it for
scope `url` instead of Slice 1's escaped regex (see *Slice 2: exact rules*). There is no side panel UI yet (Slice 3).

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

## Slice 2 (side panel)

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
