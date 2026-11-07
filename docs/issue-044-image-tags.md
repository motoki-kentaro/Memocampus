# Issue #44: image tags — Slice 1 (schema 7, vault, native host)

Tags the user gives saved images **afterwards**, to find them again (for example reaction images or comic panels kept
from a forum). Slice 1 is the base only: the vault format, the rules for a tag, and native host messages.

**Not in Slice 1:**

- No tag input when saving: right-click saving is unchanged.
- No tag editing, tag display or tag search in the side panel (Slice 2 and later).
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

## Slice 2 and later

- Side panel: tag editing on a tile (`image_tags_get` / `image_tag_add` / `image_tag_remove`), the tag list, search.
- Possibly tags in `image_list` items, to avoid one request per tile.
- Tag input when saving, if still wanted.
- MCP: only after a decision on whether and which tags may be published.
- An image deletion command will rely on D5 as it is.
