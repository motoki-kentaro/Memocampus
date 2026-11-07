# Issue #44: image tags — Slice 1 (schema 7, vault, native host) and Slice 2 (Image tab)

Tags the user gives saved images **afterwards**, to find them again (for example reaction images or comic panels kept
from a forum). Collecting stays effortless; sorting and finding come later. Slice 1 is the base: the vault format, the
rules for a tag, and native host messages. Slice 2 is the Image tab: editing tags and filtering by one tag
(see *Slice 2: the Image tab*).

**Not in Slices 1 and 2:**

- No tag input when saving: right-click saving is unchanged.
- No search by several tags, fuzzy or full-text search, no "untagged" filter, no bulk editing.
- **No MCP exposure**: the MCP server reads no tag (see *Privacy*).
- No tags on visited pages, no hierarchy, colours, synonyms or automatic tags.
- No conversion of schema 6 vaults.

## Decisions

| | Question | Decision |
|---|---|---|
| D1 | What is tagged | The **image**: one distinct file (`image_blobs`, the `blobId` of `image_list`), not one save action. Every save of the same bytes shows the same tags. Where an image came from is still read from its save history (`image_sources`), unchanged. |
| D2 | Migration | **None. Schema 7 exists only by creation.** A schema 6 vault is refused (`vault_schema_mismatch`) and left as it is, like every older one. Create a new vault after updating. |
| D3 | Case | **Case-insensitive, stored lowercase.** "Cat", "CAT" and "cat" are one tag, shown as "cat". |
| D4 | Unused tags | **Removed automatically**, by the vault itself (a trigger), in the same transaction as the last link. |
| D5 | Removing an image | Its tags go with it (`ON DELETE CASCADE`), and through D4 every tag no other image has. There is no host command that removes an image yet; this is the vault's own guarantee for when there is. |
| D6 | Limits | 64 characters per tag, 64 tags per image. |
| D7 | Incognito images | Can be tagged like any other: a tag is local, holds no URL and is never published. |

## Tag normalization

`image_tags::normalize_tag` turns what the user typed into the stored form, or refuses it with `invalid_tag`:

1. **Whitespace** (Unicode `White_Space`: spaces, tabs, line breaks, the ideographic space U+3000) is removed at both
   ends; every run of it inside becomes one ordinary space (U+0020). `"  猫　　耳 "` → `"猫 耳"`.
2. **Control characters** that remain (C0, DEL, C1) and **invisible format characters** (soft hyphen, zero-width
   space / joiners, direction marks and embeddings, bidi isolates, word joiners, BOM, interlinear annotations) are
   **refused, not removed**: what the user typed is never silently changed, and two tags never look the same while
   being different.
3. **Letters are lowercased** with Unicode rules (`str::to_lowercase`): `"ÄÖÜ"` → `"äöü"`, `"ＡＢＣ"` → `"ａｂｃ"`.
4. The result has **1 to 64 characters** (Unicode scalar values, as SQLite's `length` counts them). An empty or
   whitespace-only tag is refused. Input longer than 1024 bytes is refused before any work.

Nothing else is changed: **no Unicode normalization** (NFC / NFKC), so full-width and half-width letters, or
hiragana, katakana and kanji, stay different tags. Every printable character is allowed, including `/ : # ,` and
emoji. Exact duplicates cannot exist: `name` is UNIQUE and every write goes through the same normalization.

## Storage: schema 7

Schema 7 is schema 6 plus three objects; nothing existing changes. `schema::check` compares the stored text of every
object byte for byte, so the new tables, their index and the trigger are part of the exact shape a vault must have.

```sql
CREATE TABLE image_tags (
  id    INTEGER PRIMARY KEY,
  name  TEXT    NOT NULL UNIQUE CHECK (length(name) BETWEEN 1 AND 64 AND name = trim(name) AND name = lower(name))
)
CREATE TABLE image_tag_links (
  blob_id  INTEGER NOT NULL REFERENCES image_blobs (id) ON DELETE CASCADE,
  tag_id   INTEGER NOT NULL REFERENCES image_tags (id) ON DELETE RESTRICT,
  PRIMARY KEY (blob_id, tag_id)
)
CREATE INDEX image_tag_links_by_tag ON image_tag_links (tag_id, blob_id)
CREATE TRIGGER image_tags_drop_unused AFTER DELETE ON image_tag_links
BEGIN
  DELETE FROM image_tags WHERE id = old.tag_id AND NOT EXISTS (SELECT 1 FROM image_tag_links WHERE tag_id = old.tag_id);
END
```

- The CHECK repeats what SQLite can verify of the normalization (length, no outer spaces, no ASCII capitals); the
  full rules are the host's.
- `PRIMARY KEY (blob_id, tag_id)`: one link per image and tag. `image_tag_links_by_tag` serves the search by tag.
- `tag_id … RESTRICT`: a tag still on an image cannot be deleted. `blob_id … CASCADE`: an image's links go with it.
- The trigger runs for every removed link, also those removed by the CASCADE (verified in the tests), so a tag exists
  exactly as long as some image has it, and the tag list never shows a tag with no image.
- The host opens every vault with `foreign_keys = ON` (unchanged).
- Backups (`vault_backup`) copy the tags and now also compare the row counts of `image_tags` and `image_tag_links`
  between the vault and the copy, besides `integrity_check` and the schema check they already made.

**Why automatic removal (D4).** A tag with no image has no use and would only fill the tag list; keeping it would
need a separate "delete tag" command and a rule for when to show it. As a trigger, the rule holds for every way a
link disappears (the host's remove, a future image deletion, a CASCADE) without each caller having to remember it,
and a reader never has to filter. Giving the tag again simply creates it anew.

## Native host messages

One-shot messages, like `image_list`. Every message has `requestId`; every reply has `ok`, `type` (the message type
plus `_result`) and `requestId`. A failure is `{ ok: false, type, requestId, error }` and has **changed nothing**.

```text
→ image_tags_get    { blobId }
← image_tags_get_result    { vaultToken, blobId, tags }
→ image_tag_add     { blobId, sha256, vaultToken, tag }
← image_tag_add_result     { vaultToken, blobId, tag, added, tags }      added=false: it already had it
→ image_tag_remove  { blobId, sha256, vaultToken, tag }
← image_tag_remove_result  { vaultToken, blobId, tag, removed, tags }    removed=false: it did not have it
→ image_tag_list    { cursor?: string | null, limit?: 1..=200 (default 200) }
← image_tag_list_result    { vaultToken, items: [{ name, imageCount }], next: string | null }
→ image_tag_search  { tag, cursor?: { lastSavedAt, id } | null, limit?: 1..=60 (default 60) }
← image_tag_search_result  { vaultToken, tag, items: [image_list item], next: { lastSavedAt, id } | null }
```

- `tag` in a reply is the **normalized** name; `tags` is the image's tags **after** the request, ordered by name.
- Adding a tag the image has, and removing one it does not have, succeed without a change (`added` / `removed` false).
- The tag list is ordered by name (SQLite's binary order) and paged by name: `next` is the last name of the page.
- The search returns exactly the items `image_list` returns for those images (same fields, same order — most recently
  saved first — same cursor and the same reply size limit), so the panel can show them with the code it has.
  An unknown tag gives no items.
- Writes name the vault (`vaultToken`, from any earlier reply) and the bytes (`sha256`), like `image_read`. Each write
  is one transaction (`Session::write_for`), which lands only in the vault the request named.

| `error` | When |
|---|---|
| `invalid_request_id` | `requestId` missing, empty or too long |
| `invalid_blob_id` | `blobId` missing or not a positive integer |
| `invalid_hash` | (writes) `sha256` missing or not 64 lowercase hex digits |
| `invalid_vault_token` | (writes) `vaultToken` missing or malformed |
| `vault_switched` | (writes) another vault is active than the one named |
| `invalid_tag` | `tag` missing, not a string, or refused by the normalization |
| `blob_missing` | no image with this `blobId`, or (writes) it now has other bytes than `sha256` |
| `too_many_tags` | (add) the image already has 64 tags; a tag it already has is still accepted |
| `invalid_cursor` / `invalid_limit` | list: cursor not a normalized tag name; search: cursor not `{ lastSavedAt, id }`; limit out of range |
| `vault_*`, `config_invalid`, `db_error`, … | the vault errors every image message has (`vault_schema_mismatch` for a schema 6 vault) |

## Privacy

- Tags are text the user wrote about an image; they hold no URL, so the privacy policy (NeverSave / SaveButNoMcp) has
  nothing to judge in them, and no tag message reads it. Ingress (`visit`, `tab_snapshot`, image saves) is unchanged,
  including the fail-closed handling of a policy that cannot be read.
- **MCP reads no tag.** `VisibleStore` queries name their tables, and none is a tag table; tags on public and private
  images change no MCP answer and are never searchable there (test `image_tags_never_reach_a_reader`). A tag can
  itself be private ("for work", a person's name), so publishing tags needs its own decision first.
- The MCP server checks the vault with the same `schema::check`: it reads schema 7 vaults and refuses schema 6 ones.

## Tests

- `native-host/src/schema.rs`: schema 7 created and checked; constraints, keys, CASCADE / RESTRICT, the trigger
  (also through a CASCADE), `foreign_key_check` and `integrity_check`; missing trigger or changed definitions refused.
- `native-host/src/image_tags/tests.rs`: normalization (spacing, U+3000, Unicode lowercase, controls, invisible
  characters, length after lowercasing, idempotence), add / remove / read / list / search with paging, limits, missing
  or replaced images, the vault-switch check, rollback of a refused add, an image removed from the file, backups,
  images and their list unchanged by tags, every message and every error code.
- `native-host/tests/stdio_protocol.rs`: schema 6 (and relabelled or altered schema 7) refused for every operation
  including the tag messages, file left byte for byte; the real host process end to end (images saved by the existing
  flow, tagged, searched, untagged, backed up, one image removed, `integrity_check`, `foreign_key_check`).
- `visible-store`, `mcp-server`: schema 6 refused; tags never reach a reader.

## Slice 2: the Image tab

No host or schema change: the five messages of Slice 1 are used as they are, and `image_list` is unchanged.

**Editing (in the details of an image).** Clicking a tile opens its details as before (now titled 画像の詳細); above
the save history (保存履歴, unchanged: image URL, page URL, frame URL, dates) a **タグ** section shows the image's tags
as small chips, each with a × to remove it, and a one-line form: a text field with **追加**, and **既存のタグから選ぶ**.

- The tags are read when the image is opened (`image_tags_get`), not for every tile: no request per tile.
- What the user typed goes to the host unchanged; the extension never normalizes. The chips, messages and the filter
  show the host's answer (`tag`, `tags`): "  Reaction  " becomes the chip "reaction".
- Nothing is shown as done before the host confirmed it. While a request runs, every tag control of the image is
  disabled (no double sends; Enter while composing Japanese text does not submit); on failure the shown tags and the
  typed text stay, with the reason (`invalid_tag`, `too_many_tags`, `blob_missing` — "一覧を更新してください" —,
  vault errors). Whitespace alone is not sent.
- **既存のタグから選ぶ** opens the tag picker: the tags in use, by name with their image count, 200 per page
  (`image_tag_list`), **さらに表示** for the next page; a tag the image already has is shown but not selectable.
- Copying and exporting stay in the tile's right-click menu, unchanged; the tag editor is only in the details.

**Filtering (toolbar).** **タグで絞り込み** opens the same picker: pick a tag, or type one and press **絞り込む**.
The grid then shows the images with that tag, newest first, through `image_tag_search` — the same grid, thumbnails,
paging, details, copy and export as the unfiltered list (one code path, only the request differs). The toolbar shows a
chip "タグ: <name>" (the host's normalized name) with × to go back to all images. No match: "タグ「<name>」の付いた画像は
ありません。". A typed name the host refuses: back to all images, with the reason in the status line.

Under a filter, removing that tag from the open image takes its tile out of the grid at once; giving it back puts the
tile back at its place in the list order (the details stay open, so a mistake can be undone).

**Vault and async rules.** These extend the existing generation rule of the Image tab: every answer belongs to the list
it was asked for, and is dropped if the list was reset since.

- A vault change (in this panel or detected through a `vaultToken` that differs, e.g. switched from another window)
  drops the filter, the open picker, the details and every late answer of the old vault; the list restarts unfiltered.
- Tag writes name the image (`blobId`, `sha256`) and the vault of the list (`vaultToken`). If the host answers
  `vault_switched`, nothing was written anywhere; the panel says so and restarts in the active vault.
- Without a usable vault (no list loaded) the filter button and all tag controls are disabled.

**Tests** (`tests/extension/image-tags.test.js`, fake host that keeps tags per vault): editing, reuse through the picker
with paging, refusals, double sends, filtering with paging, typed and refused filters, answers overtaken by a newer
filter, the grid following tag edits, copy and export from a filtered grid, vault switches with late answers, a write
refused for another vault, no usable vault. An opt-in end-to-end test drives the same side panel code against the real
host binary in a temporary vault (`MEMOCAMPUS_TEST_HOST`): tag, find by tag, copy the original, untag, a write for
another vault refused, then `integrity_check` and `foreign_key_check`.

### Checking it in Chrome (for the PO)

With the extension and host of this branch, in a **test vault** (Settings → 場所を選んで新規作成…):

1. Save two or three images by right-click → **Memocampusへ保存**, as before (no tag prompt appears).
2. Image tab → click an image: the details show **タグ** ("タグはまだありません。") above **保存履歴** (image and page URLs).
3. Type `  Reaction  ` and press Enter: the chip **reaction** appears. Type `REACTION`: "既に付いています".
4. Add `ツッコミ` with IME: Enter that confirms the conversion must not add the tag; the next Enter does.
5. Another image → **既存のタグから選ぶ**: the tags with counts; pick one; a tag the image has is greyed out.
6. Remove a tag with ×.
7. Toolbar **タグで絞り込み** → pick a tag: only those images; the chip "タグ: …" is shown. Right-click a tile →
   **画像をコピー**, paste into a text field elsewhere (e.g. a reply box): the image is pasted.
8. Under the filter, remove that tag from an image: its tile disappears; add it again: it comes back in place.
9. × on the filter chip: all images again. Filter by a typed tag nobody has: the empty message names it.
10. With a filter active, switch to another vault in Settings, back to the Image tab: no filter, only that vault's images.
11. Check the layout at the side panel's narrow and wide widths, in light and dark mode, and with the keyboard
    (Tab to the controls, Escape closes the picker and returns focus).

## Later

- Tag input when saving, if still wanted.
- Tags shown on tiles (would need tags in `image_list` items, to avoid one request per tile).
- Search by several tags, an "untagged" filter, bulk editing.
- MCP: only after a decision on whether and which tags may be published.
- An image deletion command will rely on D5 as it is.
