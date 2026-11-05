# Issue #22: light page metadata (OGP, description, favicon)

Status: implemented on `feature/22-page-metadata`; Codex review, Chrome dogfood and CI pending.

"OGP があればラッキー。なければそれでいい": when an allowed page finishes loading, the extension reads a few values from its DOM
once and sends them with the page's visit. Nothing is fetched, followed or guessed. Missing values are NULL, and any failure
only means "no metadata": the visit is recorded as before.

## Differences from the original issue (PO decision for the MVP)

| Original issue | Now |
|---|---|
| First `h1` | **Deferred.** No body text is read. |
| `og:image` not collected | **Its URL string is stored** (`og_image_url`). The image is never downloaded. |
| — | **Favicon URL** (`link[rel~="icon"]`), string only, never `/favicon.ico` by guess. |
| SPA navigation to be studied | **Deferred.** One read per load; no `pushState` tracking, no MutationObserver, no retry. |
| Search over description and h1 | **Deferred.** Nothing reads the metadata yet: no search, UI, MCP or VisibleStore API. |
| Schema change on new vaults only | **Schema 5**, creation-only like schema 4. No migration, no backfill. |

Not collected: headings, article text, the DOM, MHTML, JSON-LD, Twitter Card, CSS, image bytes, screenshots. No page is crawled
or revisited, and no request is sent for metadata.

## Flow

1. `webNavigation.onCompleted`, top frame, http(s) only (as before).
2. `chrome.tabs.get`: only a tab known to be normal (`incognito === false`) continues; a closed tab stops here (as before).
3. **The host judges the URL** (`policy.evaluate`, the existing gate). NeverSave, a policy that cannot be read and any failure
   stop here: **the DOM is never read** for them, and nothing is queued.
4. Only then `capturePageMetadata` (`extension/page-metadata.js`): one `chrome.scripting.executeScript` with
   `target: { tabId, documentIds: [details.documentId] }`, isolated world, `readPageDom` as the function. 2 s timeout.
5. The visit is queued, with `page` when step 4 gave an answer, and delivered by the existing queue.
6. The host judges the URL again on arrival (as before). A refused visit is dropped with its metadata; an allowed one is stored
   with its metadata in one transaction.

No new permission: `scripting` and host access already exist (image saving). No content script is declared, so nothing runs in
pages that are not allowed.

## Identity

- **Document.** `documentIds` makes Chrome run the read in exactly the document that completed, or fail. A later navigation in the
  tab, a closed tab or a discarded document gives an error, so no metadata. Without `documentId` in the event, nothing is read.
- **URL.** The answer counts only if the document's `location.href` is still exactly the visit's URL. A page that changed its URL
  (pushState, replaceState, a fragment) before the read gives no metadata. Redirects: `onCompleted` reports the final URL, which is
  the one recorded and read.
- **Event.** The metadata is a field of the visit event, so it shares its id, its queue entry, its retries, its sweep by the
  policy and its dead-letter copy. There is no second queue and no second id.
- **Host.** The row is written by `INSERT … SELECT … FROM visits WHERE event_id = ? AND url = ?` in the visit's transaction: only
  a stored visit with exactly this URL gives a row, whose `url` and `event_id` come from that visit. An id reused with another URL
  gives nothing.

## Values

Text (`title` = `document.title`, `description`, `og_title`, `og_description`, `og_site_name`):

- Every White_Space character becomes a space, every other control character (Cc) is removed, runs of spaces become one, the ends
  are trimmed. Broken UTF-16 (lone surrogates) becomes U+FFFD in the extension, as the host could not read it otherwise. HTML
  entities are already decoded by the DOM.
- Cut, without ellipsis, to: `title`, `og_title` **500**; `og_site_name` **200**; `description`, `og_description` **1000** Unicode
  scalar values. Never inside a character, never ending in a space.
- Empty or blank: NULL.

URLs (`og_image_url` = `meta[property="og:image"]`, `favicon_url` = `link[rel~="icon"]`):

- Resolved against `document.baseURI`, so `<base href>` is respected. Kept as the browser serialises them; the query is not
  changed or removed.
- Only `http:`/`https:` with a host and **without user or password**, at most **2048** characters. `data:`, `javascript:`, `blob:`,
  `file:` and everything else is NULL.

Several tags of one kind: the first with a usable value (at most 8 are read per kind, 8192 characters each, before shaping).
Attribute and property names match without regard to case (`meta[name="description" i]`).

The host (`native-host/src/page_metadata.rs`) applies the same rules again, since what arrives is page text. Both sides are
checked against `tests/page-metadata/vectors.json`. On the host, a `page` that is not an object, a field of the wrong type, or a
`capturedAt` other than `YYYY-MM-DDTHH:MM:SS.sssZ` makes it ignore the whole `page`; the visit is stored regardless and the
acknowledgement is unchanged. A URL that may not be kept is only that field's NULL.

Size of one event: at most 3,200 characters of text and 4 KiB of URLs on top of the visit (measured: 10,339 bytes of JSON for the
largest possible event with CJK text, 382 for a typical one, 147 for a visit without metadata). If extension storage refuses the
event with its metadata (`storage_write_failed`, e.g. quota), the visit is queued again without it.

## Storage: schema 5

```sql
CREATE TABLE page_metadata (
  url            TEXT NOT NULL PRIMARY KEY,
  event_id       TEXT NOT NULL REFERENCES visits (event_id) ON DELETE CASCADE,
  title          TEXT CHECK (title IS NULL OR length(title) BETWEEN 1 AND 500),
  description    TEXT CHECK (description IS NULL OR length(description) BETWEEN 1 AND 1000),
  og_title       TEXT CHECK (og_title IS NULL OR length(og_title) BETWEEN 1 AND 500),
  og_description TEXT CHECK (og_description IS NULL OR length(og_description) BETWEEN 1 AND 1000),
  og_site_name   TEXT CHECK (og_site_name IS NULL OR length(og_site_name) BETWEEN 1 AND 200),
  og_image_url   TEXT CHECK (og_image_url IS NULL OR length(og_image_url) BETWEEN 1 AND 2048),
  favicon_url    TEXT CHECK (favicon_url IS NULL OR length(favicon_url) BETWEEN 1 AND 2048),
  captured_at    TEXT NOT NULL CHECK (length(captured_at) = 24)
)
```

- **One row per URL**: the metadata of the latest visit of it whose page could be read. There is no `urls` table; visits are
  rows keyed by event id with their URL, so the row names the visit it came from.
- `captured_at` is the extension's time of the read (`toISOString`, UTC, milliseconds), as `visited_at`.
- **UPSERT** `ON CONFLICT (url) DO UPDATE … WHERE excluded.captured_at >= page_metadata.captured_at`: the row is updated in place
  (never `INSERT OR REPLACE`); an older capture arriving late changes nothing; resending the same event writes the same values.
- A newer capture replaces every field, so a field the page no longer has becomes NULL. **A visit without `page`** (the DOM could
  not be read) **leaves the row as it is.**
- `schema::check` compares the stored definition byte for byte, as for schema 4.

### Creation only: what the PO has to do

The new host refuses a schema 4 vault (`vault_schema_mismatch`, unchanged) and the old file is not touched. To dogfood:

1. Optionally copy the current vault file (the app cannot back up a refused vault).
2. Update the host and the extension together.
3. Side panel → Vault → create a new vault (default path is taken by the old file, so choose another name, or move the old file
   away first by hand).
4. Visits queued meanwhile stay in the extension queue and go to the new vault. Old history stays in the old file.

## Privacy

- NeverSave, an unusable policy and incognito tabs: no DOM read, nothing queued, sent or logged (tests).
- SaveButNoMcp: the metadata is stored like its URL. **No reader exists yet.** VisibleStore does not read `page_metadata`; any
  future MCP use must judge by `url` through VisibleStore, as for visits (a test keeps VisibleStore's output unchanged with
  `page_metadata` rows present).
- The values never appear in errors, acknowledgements or logs; `page-metadata.js` has no `console` call and sends nothing.
- `og_image_url` and `favicon_url` are strings from an allowed page and are not judged as URLs of their own (they may name
  other hosts).

## Tests

- `tests/extension/page-metadata.test.js`: extraction (OGP / none / description or title only / favicon / relative and `<base>`
  URLs / invalid URLs / empty / long / several tags / case), identity (document id, URL, wrong shapes, timeouts, late failures),
  the service worker (order of policy and read, NeverSave / unknown policy / incognito not read, SaveButNoMcp stored, closed tab,
  pushState, redirect source, quick navigations, restart and resend, storage refusal, sweep of queued and dead-letter events,
  no logging, no new permission).
- `native-host/src/page_metadata/tests.rs`: shaping and limits, shared vectors.
- `native-host/tests/stdio_protocol.rs`: stored with its visit, UPSERT in place, NULLs, row kept without `page`, late and resent
  captures, URL/event binding and foreign key, bad metadata ignored with the visit stored, host cleaning, policy (NeverSave dropped,
  SaveButNoMcp stored, no policy nothing), schema 4 refused.
- `visible-store/src/tests.rs`: metadata never reaches a reader; a vault without the table is refused.

Measured against the release host over stdio, one process per message, median of 15 (process start included): a visit takes
26.2 ms on `main` (schema 4), 26.7 ms on this branch without metadata, 26.9 ms with typical metadata, 27.4 ms with the largest.
Shaping the worst raw input in the worker takes about 2 ms. The read in the page was not measured (no Chrome in the tests).

## Not done

h1, SPA tracking, search, UI, favicon or image display, MCP, FTS5, vector search, summaries, migration, backfill.
