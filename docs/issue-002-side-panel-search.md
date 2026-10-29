# Issue 002 — side panel search over the local vault

## Acceptance criteria

- Clicking the toolbar action opens the Memocampus side panel; the panel also hosts the existing “record all tabs” button.
- On open, the panel focuses the search box and lists recent pages, newest activity first.
- Typing filters by case-insensitive (ASCII) substring of URL or title, debounced and IME-aware.
- Results show title, shortened URL (full URL on hover/focus), and the visit and snapshot times as separate labels.
- Clicking a result opens it in a new tab (Ctrl/middle click: background tab). No tab is closed.
- Empty results, host connection failures, timeouts, and DB errors are shown in the panel.
- Results load 50 at a time; a stale response never overwrites a newer query.
- `visits` and `tab_snapshots` rows are never modified by search; aggregation happens in the query.
- No query or URL is logged by the extension or the host.

## Native Messaging: `search_pages`

Request (side panel → host, via `chrome.runtime.sendNativeMessage`):

```json
{ "type": "search_pages", "requestId": "uuid", "query": "rust", "limit": 50, "offset": 0 }
```

- `requestId`: required, non-empty string ≤ 128 chars.
- `query`: optional string, trimmed, ≤ 200 chars. Empty lists everything.
- `limit`: optional integer 1–100 (default 50). `offset`: optional integer 0–100000 (default 0).

Success response:

```json
{
  "ok": true, "type": "search_pages_result", "requestId": "uuid", "hasMore": false,
  "items": [{
    "url": "https://…", "title": "…",
    "lastVisitedAt": "2026-10-29T10:00:00.000Z",
    "lastCapturedAt": null,
    "lastActivityAt": "2026-10-29T10:00:00.000Z",
    "source": "visit"
  }]
}
```

- One item per URL across `visits` and `tab_snapshots`.
- `lastVisitedAt`: latest `visits.visited_at` (null if never visited). `lastCapturedAt`: latest `tab_snapshots.recorded_at` (null if never captured).
- `lastActivityAt`: the later of the two; sort key (descending, ties by URL ascending).
- `source`: `visit`, `snapshot`, or `both`. `title`: latest non-empty title from either table.

Error response: `{ "ok": false, "type": "search_pages_result", "requestId": "uuid" | null, "error": "<code>" }`
with code `invalid_request_id`, `invalid_query`, `query_too_long`, `invalid_limit`, `invalid_offset`, or `db_error`.

Existing `visit` / `tab_snapshot` messages and their `{ ok, eventId }` acknowledgements are unchanged.

## Explicit non-goals

Image storage, full-text page search, semantic search, tag editing, history deletion, cloud sync, automatic tab closing.
