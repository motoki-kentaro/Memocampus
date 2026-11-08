# Issue #5: a calendar of visit days in the URL tab — Slice 1 (native host), Slice 2 (side panel)

Find a page by **when it was visited**: a month calendar above the URL tab's list; days with visits are selectable;
choosing one narrows the existing list to the visits of that local day. Slice 0 settled the design (PO approved);
Slice 1 is the native host only: one new message, a date range for `search_pages`, and a capability. The calendar
itself is Slice 2 (at the end).

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

## Slice 2: the calendar in the side panel

`extension/visit-calendar.js` (date logic and the popover), wired in `sidepanel.js`; `visitDays` and the range of
`searchPages` in `search-client.js`; `visitCalendar` in `host-capabilities.js`. The host is unchanged.

**Layout.** The URL tab's count row is `[calendar][YYYY/MM/DD ×][count …][全タブを記録]`. The calendar button
(「閲覧日で絞り込み」, `aria-expanded`, an inline SVG drawn for this, no asset) is disabled until the host named
`visit_calendar_v1` and a list was read from a known vault (its tooltip says why). The popover is a non-modal
`role="dialog"` inside the toolbar, `position: absolute` below it, so it covers the list without moving it; at most
300 px wide, never past the panel's bottom (it scrolls). Light and dark colors for Sunday, Saturday, today and the
chosen day; forced colors handled.

**Popover.** Header `◀ 2026 ▼ 11月 ▶`: the arrows move a month (across years), the middle opens the year-month
picker (`◀ 2026 ▶` and 1月–12月; choosing a month returns to the days, closing it changes nothing). Years from 1970
to next year. Opens on the chosen day's month, else this month. Moving months never changes the filter. The grid
starts on Sunday (日 月 火 水 木 金 土, Sunday red, Saturday blue, with `abbr` names), 4 to 6 weeks; only days with
visits are enabled, with one dot of the same size whatever the count; today has a ring and `aria-current="date"`;
the chosen day is filled. A day's name is 「11月8日（日）閲覧12件」 (+「、今日」「、選択中」). No holidays.

**Closing.** Escape (focus back on the button), a press outside (focus stays where it went), focus moving out
(Tab), another tab, choosing a day (focus back on the button), a vault switch.

**Keyboard.** One tab stop in the grid (roving `tabindex`): the chosen day, else today, else the first day with
visits. Left/Right go to the previous/next day with visits, Up/Down to the same weekday in other weeks (skipping
weeks without a visit that day), Home/End to the week's first/last day with visits. The month's edge is not
crossed (◀ ▶ change months); where no day qualifies, focus stays. Enter and Space are the buttons' own click.

**Search.** The chosen day is state of its own (`state.date`), apart from the query. Choosing it searches from the
top with `visitedFrom`/`visitedTo` of that local day (AND the query); the query changing keeps it; × searches the
same query without the range (no extra fields at all). The next page asks with the same range. `seenUrls`, offset
and `hasMore` start over with each new list (also `hasMore` is cleared as a search starts, so no next page of the
previous terms can be asked meanwhile). The count reads `N件（2026/11/08）` (also 0), the empty text names the day.

**Host and vaults.** `visit_days` is sent only with the capability, with the month's bounds; the answer must be of
its `requestId` and type, have exactly one non-negative integer per day and a valid `vaultToken` (else
`invalid_response`). It is kept only when it is the latest request of the open popover (a generation counter that
moving months, closing and switching vaults all advance) and its `vaultToken` is the token of the list shown. No
token known (no list, or the last search failed) disables the calendar. Counts of another vault are never shown:
the calendar closes, the day is dropped (with a note) and the list is searched again in the active vault. A day is
kept with the vault it was chosen in; a search whose answer comes from another vault drops the day and searches
again without it. A vault switch in the panel drops the day, the popover and its counts at once, before anything
is asked of the new vault. No cache: each month shown is asked for; a failed request (the host stopped, an error)
leaves no days enabled and offers 再読み込み, so counts seen before never come back. New visits: reopening the
calendar, or the panel becoming visible while it is open, counts the month again (there is no visit event to
follow, and no polling was added).

**Tests.** `tests/extension/visit-calendar.test.js`: days per month, leap years, year crossing, Sunday-first weeks
(4, 5 and 6), `YYYY/MM/DD`, names, keyboard moves; bounds in Tokyo (a local day starting the UTC day before), New
York (23 and 25 hours), Lord Howe (23.5 and 24.5), Santiago and Havana (a day starting at 01:00), every month's 29
to 32 exact increasing bounds in five zones, the time zone restored after each; `visit_days` and range messages
and every malformed answer. `tests/extension/visit-calendar-ui.test.js`: the side panel with the fake host
(capability, markup, popover, picker, grid, choosing, AND, ×, paging, keyboard, late answers, vault switches,
counts of another vault, a stopped host, malformed answers, new visits) and, opt-in, with the real host binary.
Both pass under New York, Lord Howe, Santiago, Kiritimati and UTC as well as the machine's zone.
