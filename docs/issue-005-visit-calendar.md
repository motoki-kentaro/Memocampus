# Issue #5: a calendar of visit days in the URL tab — Slice 1 (native host)

Find a page by **when it was visited**: a month calendar above the URL tab's list; days with visits are selectable;
choosing one narrows the existing list to the visits of that local day. Slice 0 settled the design (PO approved);
Slice 1 is the native host only: one new message, a date range for `search_pages`, and a capability. The calendar
itself is Slice 2.

**Unchanged:** the vault schema (no table, index or version), the privacy policy and its gate, VisibleStore and the
MCP server, how visits are recorded, and `search_pages` without a date range.

## Decisions (PO)

- A visit day is the day of a **visit** (`visits.visited_at`), one per visit: a URL visited on three days is found on
  each of them. Tab snapshots (「全タブを記録」) are no visits: neither counted nor searched by date.
- Days are the person's **local** days, half-open: `[local 00:00, next local 00:00)`. The extension computes the UTC
  instants of those starts (`new Date(y, m, d).toISOString()`), so daylight saving time is its own (a day may be 23,
  23.5, 24.5 or 25 hours long, or start at 01:00 where midnight does not exist). The host knows no time zone, adds no
  24 hours and corrects nothing.
- A query and a date are both required to match (AND).
- No cache of month counts.

## `visit_days`

```text
→ { type: "visit_days", requestId, bounds: [b0, b1, …, bn] }
← { ok: true,  type: "visit_days_result", requestId, vaultToken, counts: [c0, …, c(n-1)] }
← { ok: false, type: "visit_days_result", requestId, error }
```

- `bounds`: the starts of the local days of a month and of the next month's first day: 29 to 32 texts.
- `counts[i]`: the visits with `bounds[i] <= visited_at < bounds[i + 1]`, every visit counted (also several of one URL).
- `vaultToken`: the vault the counts were read from, as `search_pages` and `image_list` name theirs.
- Errors: `invalid_request_id`; `invalid_bounds` (checked before any vault is opened); the vault states
  (`vault_not_configured`, `vault_missing`, `vault_busy`, …); `db_error`.
- One query over the span (`visited_at >= b0 AND visited_at < bn`, bound parameters), each visit put in its day inside
  the host by text order; only the counts leave it.

**Valid bounds** (`visit_days::checked_bounds`): an array of 29 to 32 strings, each exactly `YYYY-MM-DDTHH:MM:SS.sssZ`
naming an existing instant (no February 30, no hour 24, no minute or second 60), strictly increasing, the last at most
32 days after the first. Anything else (another type, a string of another length or form, an offset other than `Z`,
a repeated or reversed bound) is `invalid_bounds`.

## `search_pages` with a date range

Two optional fields, both or neither: `visitedFrom`, `visitedTo`. Both exact instants as above, `visitedFrom <
visitedTo`, at most 32 days apart; one alone, or anything else, is `invalid_bounds`, and no query runs.

| | Without a range (as before) | With a range |
|---|---|---|
| Rows | `visits` and `tab_snapshots` | the visits with `visitedFrom <= visited_at < visitedTo` (and a valid `visited_at`) |
| One row per | URL | URL |
| `title` | latest non-empty title | latest non-empty title **among those visits**; none: empty (the panel shows the URL) |
| `lastVisitedAt` | last visit | last visit **in the range** |
| `lastCapturedAt` | last snapshot | `null` |
| `lastActivityAt` | later of the two | = `lastVisitedAt` |
| `source` | `visit`, `snapshot` or `both` | `visit` |
| Query | URL or any of its titles | URL or the title of one of those visits (AND the range) |
| Order, paging, `hasMore` | `lastActivityAt DESC, url ASC`, `limit`/`offset` | the same |

Nothing of a visit outside the range reaches a row: its time, its title and its match are not used. Without the two
fields the SQL is the same query with the range parameters `NULL` (`?4 IS NULL OR …`), and the results are exactly
those of before (tested; measured the same speed).

## Stored times that are not valid

The host has always stored `visited_at` as the extension sent it, without a check (and this slice adds none, and
changes no row). Only an exact, existing UTC instant in `Date#toISOString()` form takes part in a count or a date
range (`visit_days::VALID_VISITED_AT`, tested to agree with the bounds parser):

- the shape, with `GLOB`;
- an existing date and time: SQLite reads it back unchanged (`strftime('%Y-%m-%dT%H:%M:%fZ', visited_at) =
  visited_at`), which refuses February 30 or April 31 (SQLite would move them to the next month) and minute or second
  60;
- an hour below 24 (SQLite keeps `24:00` as written, so this is tested on its own).

Between such texts, text order is time order, so the range test is a text comparison. The validity test comes after
the range test and costs only rows in range. Other rows are in no day and no range, and still in the list without a
range. Thousands of them change nothing but their own absence (tested).

## Vault boundaries

`visit_days` and `search_pages` read with `Session::read_with_token`: one resolution of the active vault opens (or
revalidates) the connection, and both the token and the data come from that connection and its path. Taking a ticket
and then reading resolved the vault twice, so a switch by another host process between the two could pair vault A's
token with vault B's rows (found in review, fixed before any commit). Now a switch during a request changes nothing of
that request; the next one is wholly the new vault's. A failed read is a failure without a token. Tested
deterministically with a test-only hook that runs right after each resolution (`Session::after_open`, compiled in
tests only) and switches the vault there: A's token with A's data, then B's with B's, for both messages; the old
ticket-then-read pattern is shown to mix them in the same window. The extension compares the token of the counts with
that of its list (Slice 2).

## Capability

`host_capabilities` names `visit_calendar_v1` (with `navigation_v1`, `vault_items_v1`): the side panel offers the
calendar only to such a host. An older host answers `visit_days` with `unknown_type`, and ignores `visitedFrom` and
`visitedTo` (it would return the unfiltered list), so the capability must be checked before either is sent.

## Measured (reference only, not asserted)

Release build, Windows 11, one host process per request (about 11 ms of that is starting the process), synthetic
vaults with visits spread over three years, best of five; a month of March 2026 in Tokyo.

| Visits | `visit_days` | Day search | Search without a range |
|---|---|---|---|
| 10,000 | 21 ms | 23 ms | 39 ms |
| 100,000 | 40 ms | 40 ms | 209 ms |
| 1,000,000 | 296 ms | 228 ms | 2,333 ms |

The search without a range is the existing query (2,326 ms before and 2,202 ms after this change on the same 1,000,000
visits, the SQL alone). No index was added: one on `visited_at` would be a new schema version.

## Tests

- `native-host/src/visit_days/tests.rs`: exact instants (and what is not one), the SQL validity test agreeing with
  the parser, bounds of 28 to 31 days and every kind of invalid bounds, counting by local day in Tokyo (bounds,
  midnight on both sides, same URL twice, a snapshot not counted), a New York March with its 23-hour day, invalid
  stored times counted nowhere and kept (also 5,000 of them).
- `native-host/src/search.rs`: a day's rows with only that day's visits, title and times; a URL on another day; query
  AND date; no range unchanged (snapshots, `both`, latest title); paging and `hasMore` in a range; invalid times out of
  a range; one end, reversed, too long, malformed ranges refused before any query.
- `native-host/tests/visit_calendar.rs` (the real host binary): the capability; `invalid_bounds` before the vault
  check, `vault_not_configured`, no `requestId`; counts and the same token as `search_pages`; a day search and the
  unfiltered list through the host; counts and token of the new vault after a switch.

## For Slice 2 (the calendar)

- Ask `host_capabilities` for `visit_calendar_v1` first; without it, show no calendar and never send a range.
- Bounds of a month: `Array.from({ length: days + 1 }, (_, i) => new Date(y, m, i + 1).toISOString())`; a day:
  `new Date(y, m, d)` and `new Date(y, m, d + 1)`. Never add 24 hours.
- Drop a month's answer when another month was asked since, or when its `vaultToken` is not the list's.
- Keep the chosen date when the query changes, send both; clear it on a vault switch (PO decision).
- With a date, the list shows the day's title and time; the summary should name the date.
