# Issue #12 — VisibleStore design

Status: **Slices 1 to 3 implemented, for review. VisibleStore answers visits and image metadata; no original and no MCP server exist.** Sections 1–4 are the Slice 0 investigation, kept as the
record of why B′ was chosen. Sections 5–11 are the current design and replace the Slice 0 recommendations. Labels:
**[verified]** was run, **[code]** was read in the source, **[inferred]** is reasoning not run, **[open]** is not determined.

The PoC is a throwaway program outside the repository, using rusqlite 0.32 / bundled SQLite 3.46.0
(the versions in `native-host/Cargo.lock`), the real `memocampus_native_host::policy` evaluator,
fictional canary data, and temporary directories, on one Windows 11 machine with a local disk.

## 1. Existing structure

| Area | Finding | |
|---|---|---|
| Connections | `vault::open_vault` is READ_WRITE (no CREATE), busy timeout 3 s, `validate_schema`. No read-only open exists | [code] |
| Journal | No `journal_mode` is set: `delete` (rollback journal). WAL is not used | [code][verified] |
| Boundary | `vault`, `search`, `images` live in the `main.rs` binary. The library crate exposes only `policy` and `policy_store` | [code] |
| `visits` | `event_id TEXT PRIMARY KEY, url, title, visited_at`. **No incognito column** | [code] |
| `tab_snapshots` | One row per tab with `incognito`. No parent row or count | [code] |
| `saved_images` | One row per save action, with `incognito`. **No MCP publication flag** | [code] |
| `image_blobs` | `save_count`, `created_at`, `last_saved_at` are totals over all rows, public or not | [code] |
| Policy | Global `policy.json`, independent of the vault. `Policy::evaluate` and `PolicyStore::evaluate` turn failure into `NeverSave`; `load()` keeps errors apart, as `policy_gate.rs` (#11) relies on | [code] |
| #11 | Entrance control covers layer A only; `saveButNoMcp` rows are stored, so B must be enforced on the way out | [code] |

Differences between the code and Issues #8/#12:

1. **The publication flag does not exist.** "Initially private" and "public does not override B" have
   nothing to read. A schema change is needed (D2), and until a flag can be set, image tools can return
   only an empty result.
2. **Visits do not record incognito.** Since #9 none are stored from incognito, but older rows cannot be
   told apart. The Issue's wording, "identifiable", permits this (D6).
3. **Existing readers cannot be reused.** `images::ensure_schema` writes tables and `user_version`,
   `open_vault` needs write access, `LIST_SQL` reads the totals on `image_blobs`, and `SEARCH_SQL` is unfiltered.
4. **"Unevaluable" cannot be derived from `evaluate`,** which answers `NeverSave` for both a match and a
   malformed URL. `normalize_url` must be called too (D3).
5. **#8 asks for `blob_id` and visit IDs.** Sequential IDs have gaps that reveal private rows (D4). Visit
   `rowid`s are not stable on a table with a TEXT primary key [inferred]; `event_id` is.
6. **WAL and network locking,** mentioned by the Issues, have no current application use. Local behaviour
   was measured; a network file was **not** tested (section 9).

Vault lifecycle: `--vault` (#8) takes a fixed path and does not read `config.json`, so it cannot follow
the Active Vault [inferred from the design; test required by Issue #12].

## 2. Security invariants

- **I1** The MCP side has no way to obtain a connection to the vault, or to issue SQL.
- **I2** Search, count, sort, paging and rollup run only over rows already judged public.
- **I3** Visit public = `Decision::None` from a loaded policy. Image row public = not incognito, flag public,
  and the URL rule of D3. Everything else is private.
- **I4** No decision (policy or vault failure) means no data. No stale result is served.
- **I5** Totals on `image_blobs` (`save_count`, `created_at`, `last_saved_at`) and `data` are never read.
- **I6** Private and absent IDs give byte-identical answers.
- **I7** A policy change takes effect on the next read. Already-returned data cannot be recalled.
- **I8** MCP never writes the vault or the policy.

## 3. PoC conditions and results

### Candidate A: authorizer + SQL function + TEMP VIEW

| Check | Result | |
|---|---|---|
| READ_ONLY connection can create TEMP VIEWs using a `SQLITE_DIRECTONLY` function | Works | [verified] |
| Function exists only on the registering connection | Yes | [verified] |
| 28 of 29 hostile statements refused (direct read, `count(*)`, `SELECT 1`, CTE, subquery, UNION, `sqlite_master`, `pragma_table_info`, PRAGMA, ATTACH, CREATE, DROP, `load_extension`, `dbstat`, recursive CTE, INSERT, `image_blobs.data`) | No canary in results or errors. The one that ran, `sqlite_temp_master`, shows view definitions only | [verified] |
| Statement prepared before the authorizer | Expired and refused on re-prepare (the PoC predicted otherwise) | [verified] |
| Rollup, paging, incognito, mixed blob (1 public + 52 private rows: count 1, dates of the public row only) | Correct | [verified] |
| Policy change on next read through the same connection | Works | [verified] |
| **Plain view + outer predicate order** | The outer function saw 0 private URLs in 10 query shapes, including `likelihood` and rowid lookups. This is an observation, not a guarantee: SQLite documents no such order | [verified, limited] |
| **`MATERIALIZED` CTE in a view, the form that would force the order** | **Refused by the authorizer**: reads inside it arrive without the view as accessor | [verified] |
| Authorizer removal | Whoever holds the connection can call `authorizer(None)`. It restricts SQL text, not code | [verified] |
| 100k visits: search | 1.15 s, against 11 ms unfiltered, since each query evaluates every row | [verified] |

**Conclusion: A works only in the form that relies on an undocumented predicate order.** Forcing the order
breaks the authorizer rule, and allowing that read would admit hostile CTEs too. Safety cannot be
demonstrated, so A does not pass.

### Candidate B: visible set copied to TEMP tables on the vault connection

| Check | Result | |
|---|---|---|
| READ_ONLY connection can write TEMP tables | Works | [verified] |
| Same 29 statements, with all of `main` denied afterwards | 28 refused, none leaked | [verified] |
| Temporary tables contain no canary | Confirmed | [verified] |
| Blob rollup recomputed from visible rows | Count 1, first = last = public row. Blobs with no public row, only `data:` URLs, or an A-layer frame are absent | [verified] |
| Policy change | Applied by re-materializing | [verified] |
| `PRAGMA data_version` | Detects another connection's commit, on the same connection only | [verified] |
| Vault cannot be renamed while the connection is open | `os error 32` on Windows | [verified] |

### Candidate B′: visible set in a separate `:memory:` database (recommended)

| Check | Result | |
|---|---|---|
| Query database holds no private row and has no vault tables | Confirmed | [verified] |
| `ATTACH` from the query connection | **Succeeds without protection**; `SQLITE_LIMIT_ATTACHED = 0` refuses it | [verified] |
| Vault file unchanged, no sidecars after a read | Confirmed by SHA-256 | [verified] |
| Vault can be renamed between reads; the next read errors and creates nothing | Confirmed | [verified] |
| Header change counter (offset 24) across connections | Stable without a commit, changes after one | [verified] |
| FTS5 over the visible set only | Works | [verified] |

B′ was run for visits. Its image rollup uses the SQL verified under B, but was not run in this database [open].

### Concurrency and failure (local disk, `delete` journal)

| Situation | Result | |
|---|---|---|
| Unfinished read while a writer commits | Writer: `database is locked`. **Materialization delays the Native Host** | [verified] |
| Read during an open RESERVED write | Returns the last committed state | [verified] |
| Read during EXCLUSIVE | Error after the busy timeout, no partial data | [verified] |
| Hot journal on a read-only connection | Error `attempt to write a readonly database`; file unchanged. Clears when a read-write open recovers it [inferred] | [verified] |
| WAL database opened read-only, no writer | Read works, but **`-wal`/`-shm` files existed afterwards** | [verified] |
| Missing path / non-database file | Errors; nothing created | [verified] |

### Measurements (100,000 unique URLs, 10.2 MiB, release build, median of 3)

| Operation | Time |
|---|---|
| `Policy::evaluate`, default policy | 11 µs per URL |
| **`Policy::evaluate`, 1,000 user rules** | **1.49 ms per URL** |
| B: materialize | 1.26 s |
| B′: snapshot (vault lock held throughout) | 1.40 s |
| B′: search + sort + LIMIT 50 | 0.45 ms (15.2 MiB memory) |
| A: search | 1.15 s, every query |
| Open + first read + close | 0.17 ms |

[inferred] With 1,000 rules a 100k snapshot takes about 150 s if evaluated under the lock, because `evaluate`
validates and parses every rule per URL. At the default policy, the host's 3 s busy timeout would be reached
near 200k rows. Hence the two-phase read in section 5.

## 4. Comparison

| | A | B | B′ |
|---|---|---|---|
| Safety basis | Filter inside each query; order unproven | Private rows never copied; authorizer guards `main` | Private rows never copied; query connection has no vault |
| Failure if the query is wrong | May read private rows | Authorizer refuses | Table does not exist |
| Complexity | Authorizer table and views | Moderate | Moderate; two connections |
| First read, 100k | 1.15 s | 1.26 s | 1.40 s |
| Each later read | 1.15 s | ~5 ms | ~0.5 ms |
| Blocks the Native Host | On every query | On refresh | On refresh only |
| Holds the vault file open | Yes | Yes | No |
| Testability | Needs planner cases | Set can be dumped | Set can be dumped |
| FTS5 | External content table cannot be limited simply [inferred] | Possible | Verified; index holds public rows only |
| Policy change | Immediate | Re-materialize | Re-snapshot |
| Read-only | OK | OK (TEMP) | OK (no TEMP needed) |
| Costs | CPU per query | Memory, refresh | Memory, refresh |

## 5. Final decisions (D1–D10)

| | Decision | State |
|---|---|---|
| **D1** | **B′.** Public rows are copied to an independent `:memory:` database, the only thing queries touch. A is rejected: it works only with a plain view whose predicate order SQLite does not document, and the `MATERIALIZED` form that would force the order is refused by the authorizer | Implemented |
| **D2** | `saved_images.mcp_public`, no DEFAULT, written private, per save row, schema v4, no migration | Implemented (sections 12 and 13) |
| **D3** | Unevaluable means private. Visits: a URL that does not normalize is not public. Images, later: only NULL and the recorded `data:<mime>` may be skipped; any other invalid URL, any A/B match, or no evaluable source makes the row private | Implemented (section 13) |
| **D4** | No internal ID is published (section 8). `blob_id` and raw SHA-256 are not the standard external IDs | Implemented (section 13) |
| **D5** | Slice 1 reads `visits` only | Implemented |
| **D6** | Schema 3 and older are refused (section 6) | Implemented |
| **D7** | Policy is URL-based. A title on a private URL is never published. A title on a public URL is published as stored: **titles are not sanitized and may contain secrets.** Issue #14 is to consider this | Implemented as stated; no title safety is claimed |
| **D8** | Package `visible-store` (below) | Implemented |
| **D9** | WAL and every volume other than a fixed local drive are refused (section 6) | Implemented |
| **D10** | The policy is global and independent of the vault: `%LOCALAPPDATA%\Memocampus`, or `MEMOCAMPUS_APP_DIR` as for the host. `VisibleStore::new` takes the directory explicitly, which tests use | Implemented |

### Code boundary (D8)

```text
memocampus-mcp, future (depends on visible-store only; declares no rusqlite)
   │  typed calls only
   ▼
memocampus-visible-store (visible-store/)     rusqlite is a private dependency
   ├─ lib.rs          public API, read orchestration
   ├─ snapshot.rs     the only code that opens a vault (private module)
   ├─ location.rs     vault location allowlist (private module)
   ├─ visible_set.rs  the :memory: set of visits (private module)
   ├─ visible_images.rs  the :memory: set of images (private module)
   └─ time.rs         strict timestamps (private module)
   │
   ▼
memocampus-native-host (library only: policy, policy_store)
```

The compiler enforces this: all modules are private, and nothing public returns a `Connection` or takes SQL.
`compile_fail` doctests pin the absence, and a source test rejects public items that mention `Connection`. Rust
does not let a package use a transitive dependency it does not declare, so an MCP package without `rusqlite` in its
manifest cannot name a connection type at all [inferred; that package does not exist yet]. The Native Host's `vault` and
`images` modules remain unreachable, since its library exports only the two policy modules [code].

Limit: the dependency on the native-host library also builds what that package depends on (`rfd`, and so on). Moving
policy to its own package would remove this, but is a refactor of existing code and was not done.

## 6. Reading the vault

Each API call runs these steps. **Nothing is cached**, so there is no older set to fall back on.

1. Load the policy (5 retries on Busy). Any failure: `policy_unavailable`.
2. Resolve the location, refusing a UNC path before any file access. Check the raw file header.
3. Open with `SQLITE_OPEN_READ_ONLY`; no create or URI flag, so `immutable=1` cannot be reached. No `ensure_schema`.
4. `BEGIN`, then a first read that takes the shared lock. Every later SELECT sees the same committed state.
5. Validate the schema, read `event_id, url, title, visited_at` from `visits` only, `COMMIT`, **close**.
6. Judge each row with no lock held; write public rows to a new `:memory:` database.
7. Load the policy again. If the ETag differs, restart from step 1 (3 attempts, then `policy_unavailable`). If it cannot
   be loaded: `policy_unavailable`.
8. Query the set, return typed records, drop the set.

The set is local to one call and has no way out, so nothing can see one while it is being built.

### Supported vaults (D6)

Accepted only if all hold; otherwise `vault_schema_mismatch`:

- `application_id` is Memocampus, `user_version` is exactly **4**.
- `visits` is a table with exactly the four expected columns, in order, with type, NOT NULL and primary key as created.

Since Slice 2 this is `schema::check`, shared with the host (section 12), which also covers `saved_images.mcp_public`.

**The current real vault does not qualify and must be regenerated by the PO.** This is deliberate. Until Slice 2,
`images::ensure_schema` upgraded vaults of schema 0 and 1 in place to 3 on the first image save, and the
header tag has existed since schema 1 [code]. So schema 3 with a Memocampus tag can hold visits from before #9, when
incognito navigation was recorded; `visits` has no column that identifies them, and they cannot be told apart by any
value in the file. A timestamp cutoff is no proof either: `visited_at` is sent by the extension, and an old queue can
deliver late. Safety cannot be shown, so these vaults are refused.

Schema 4 is creation-only since Slice 2 (section 12). It relies on the host's rule that visits
are declared non-incognito by the extension, which the host cannot see for itself [code].

### Location and mode (D9)

| Case | Result |
|---|---|
| UNC, `\\?\UNC\`, device paths | `vault_unsupported`, before any file access. [verified] |
| Final path (`canonicalize`: junctions, links, mapped drives) not on a drive letter | `vault_unsupported` [code] |
| `GetDriveTypeW` other than `DRIVE_FIXED`: remote, removable, CD-ROM, RAM disk, unknown | `vault_unsupported`. The decision is tested per value [verified]; no such real drive was used |
| WAL in the header, `-wal` or `-shm` beside the file, or `journal_mode` not `delete` | `vault_unsupported`, with no sidecar created. The three checks are independent, because SQLite itself enters WAL mode when it sees a `-wal` file [verified] |

This is an allowlist because no check can prove a file is local. A network-backed disk presented as fixed (iSCSI, a VHD on
a share) is not recognized. A vault on a USB drive is refused, which is stricter than the host; a PO decision could relax that.

### Row-level rules

A row is left out, never failed on, if `event_id`, `url`, `title` or `visited_at` is not valid text, or if `visited_at` is not
exactly the `Date#toISOString()` form. Text order and day boundaries mean nothing for another form. Database-level
failures fail the whole read.

## 7. Freshness and races

The Slice 0 proposal to reuse a set keyed on the policy ETag and the header change counter is **not adopted**. The counter's
safety across file replacement cannot be shown: a restored backup can carry an equal counter, and file identity and
counter cannot be read atomically against a rename. Therefore every call rebuilds. Performance is the price (section 10).

| Race | Handling | |
|---|---|---|
| Host keeps writing | The read sees whole transactions only; host commits all succeeded | [verified] |
| Write open (RESERVED) during a read | Last committed state | [verified] |
| EXCLUSIVE during a read | `vault_unavailable` after 3 s, no partial data | [verified] |
| Hot journal | `vault_unavailable`; vault and journal unchanged | [verified] |
| Write during the snapshot | Blocked by the shared lock; after close, not blocked and not part of this read | [verified] |
| Vault replaced between reads, or between policy load and open | The next open reads only the new file | [verified] |
| Vault removed or replaced with junk | Specific error; earlier data is not used, nothing is created | [verified] |
| Policy changes at any step of a read | Rebuilt under the new policy, in the same call | [verified, at 4 points] |
| Policy keeps changing, breaks or locks after the build | `policy_unavailable`, no data | [verified] |
| Reads in parallel | Each has its own set; `VisibleStore` holds no mutable state | [verified] |

Generations: with no reuse, a snapshot generation and a set generation exist only inside one call, and cannot be
confused. They become real when a cache or cursors are added.

Reads take effect at step 7. A policy update completed before a call starts always applies. One landing after step 7 applies
from the next call. Data already returned cannot be recalled (I7).

### Conditions for adding a cache later

All of these would need to be shown first: detection of replacement that does not rely on the counter alone; reading identity,
counter and ETag so that a change between them is caught; dropping the set on every failure; and tests for each row above.

## 8. API

```rust
VisibleStore::new(vault, app_dir) / from_process(vault)
search_visits(&SearchVisits { query, from, to, limit }) -> Result<VisitPage, VisibleError>
get_visits_by_day(&VisitsByDay { date, utc_offset, limit }) -> Result<VisitPage, VisibleError>

VisitPage { items: Vec<VisibleVisit>, total, has_more }
VisibleVisit { id, url, title, visited_at }
```

- `query`: trimmed, at most 200 characters, literal substring of URL or title, ASCII case-insensitive, as in `search.rs`.
- `from` is inclusive, `to` exclusive, both strict `toISOString` text. `date` is `YYYY-MM-DD`; `utc_offset` is `+09:00`, from
  `-12:00` to `+14:00`.
- `limit`: default 50, maximum 100. Order: time descending, then ID. `total` is the count of matching public rows.
- **Time zone [open]:** an explicit fixed offset was chosen; #8 asks for the zone to be explicit and does not name a form. IANA
  names would need a time zone database dependency, and a client must pick the right offset for a DST date.
- **Paging is deferred to Slice 4.** A safe cursor needs a generation to bind to, and sets do not outlive a call. Slice 1 is one
  bounded page, with `total` and `has_more` to show that more exist.

### External IDs (D4)

`visit_` + HMAC-SHA-256(random per-store key, `"visit\0" + event_id`), truncated to 128 bits, in hex.

- `event_id` is a `crypto.randomUUID()` from the extension [code], so it is not sequential, but it is the vault's own key, and
  older or foreign values are not guaranteed to be UUIDs. It is therefore not published.
- An ID is computed only for a public row, independently of any other row, so gaps and neighbours do not exist in it.
- **IDs are stable only for one `VisibleStore`, not across a restart.** A stable ID needs a persisted secret, and MCP must not
  write (I8). No API takes an ID yet. Whether IDs must survive a restart is for Slice 4 to decide.
- The HMAC is written over `sha2`, to avoid a new dependency for ten lines. It has no known-answer test, only behavioural ones,
  and it provides opacity, not authentication.

### Errors

Each is a fixed code; the type holds no URL, title, path, count or SQLite text.

| Code | When |
|---|---|
| `invalid_argument` | Input fails validation; checked before the vault or policy is touched |
| `policy_unavailable` | Uninitialized, missing, damaged, unsupported, Busy after retries, or kept changing |
| `vault_unavailable` | Missing, not a file, relative path, locked after timeout, hot journal, I/O |
| `vault_schema_mismatch` | Not a database, wrong application ID or version, other `visits` shape |
| `vault_unsupported` | WAL, network or other non-fixed location |
| `vault_corrupt` | SQLite reports corruption |
| `visible_set_failed` | The memory database could not be built or read |

`not_found` (Slice 3) is the one answer for an ID that is not public. `invalid_cursor` has no use until cursors exist.
Failure and absence remain distinguishable by design.

## 9. Test strategy and results

`cargo test` in `visible-store`: 40 unit, 1 process-environment and 6 doc tests. Temporary vaults and policies only; every private
value contains `CANARY`, and responses and errors are checked for it.

| Area | Covered |
|---|---|
| Public/private | Ordinary, A, B, credentials, userinfo, auth path, malformed, `data:`, `file:`; their titles; wrong-typed values; bad timestamps; all shared vectors from `tests/privacy/vectors.json` |
| Vault | Missing, directory, relative, UNC; schema 0–3 and 5; foreign and absent tags; 6 other shapes; text, empty, fake-header, damaged, truncated files; WAL; leftover `-wal`; file hash, mtime and directory listing unchanged |
| Consistency | Section 7 |
| Policy | Uninitialized, corrupt, unsupported, missing, marker lost, lock held; recovery on the next read |
| Boundary | Differential: private rows at every position change no total, order or page. Memory DB lists one table, holds no canary, refuses ATTACH and writes. `compile_fail` doctests; source scans for public `Connection`, logging macros, write flags and image tables |

Mutation checks on a copy outside the repository: 24 of 26 killed. The 2 survivors are defense in depth, with nothing to
distinguish them in a test: the table-type check (a view reports nullable columns, so the column check refuses it too) and
`trusted_schema = OFF` (the fixed SQL uses no schema-defined function).

Not done: a run against the real Native Host process (a host-like writer was used); CI; a network share, mapped, removable or SUBST
drive; other machines or SQLite versions; a real MCP client. Logs: the crate contains no logging call, checked by source scan, not by capturing a live stream.

## 10. Measurements

Release build, one Windows 11 machine, local disk, median of 5 reads, 10% private rows, with `search_visits` for a term that scans
every row. Peak is the whole test process, including creating the fixture. **Single-machine figures, not guarantees.**

| Rows | User rules | One call | Shared lock held | Judge + build | Query | Set | Peak | Host commits during reads | Slowest commit |
|---|---|---|---|---|---|---|---|---|---|
| 10,000 | 0 | **0.18 s** | 5.6 ms | 173 ms | 3.6 ms | 1.6 MiB | 13.8 MiB | 17, 0 failed | 20 ms |
| 100,000 | 0 | **2.16 s** | 43 ms | 2.08 s | 32 ms | 16.7 MiB | 43.5 MiB | 165, 0 failed | 59 ms |
| 10,000 | 100 | 1.66 s | 5.0 ms | 1.65 s | 3.6 ms | 1.6 MiB | 12.8 MiB | 156, 0 failed | 10 ms |
| 100,000 | 100 | 16.8 s | 42 ms | 16.7 s | 35 ms | 16.7 MiB | 43.4 MiB | 1559, 0 failed | 271 ms |
| 10,000 | 999 | 15.3 s | 5.0 ms | 15.3 s | 4.7 ms | 1.8 MiB | not measured | 1469, 0 failed | 286 ms |

- **Rebuild frequency:** once per API call, more if the policy changes during it.
- **Native Host:** the two-phase read reduced the lock from 1.40 s (Slice 0) to 43 ms at 100k rows. The host's 3 s busy timeout
  would be reached near 7 million rows [inferred, linear].
- **Judging dominates:** `Policy::evaluate` parses every rule for every URL. 100k rows with 999 rules would take about 150 s per
  call [inferred]. A compiled rule set is the next optimization and must pass the shared vectors. It changes `policy.rs`, shared with
  recording, so it was not done here. At 2 s per call with no rules, a cache is also worth reconsidering under the conditions above.

## 11. Slices and handoff

Slice 2 is complete: see section 12. The list below was written at the end of Slice 1.

Implemented in Slice 1: read-only snapshot, location and mode refusal, schema check, visits gate, set, the two APIs, opaque IDs, errors.

Not implemented: MCP server, `saved_images.mcp_public`, schema 4 in the host, images, `tab_snapshots`, MHTML, paging, `not_found`, a cache,
IANA zones.

Slice 2 must:

1. **Make schema 4 creation-only.** `create_vault` writes 4. `ensure_schema` must stop upgrading older vaults in place and refuse them;
   otherwise pre-#9 visits gain a schema number that this package trusts. Today it upgrades schema 0 and 1 to 3.
2. Add `saved_images.mcp_public` (D2) and have `snapshot.rs` require the new shape, so that a hand-numbered vault fails too.
3. Change `IMAGE_SCHEMA_VERSION` and its callers: at 4 today, the host refuses image saves as `VaultNewer` [code].
4. Have the PO regenerate the real vault. No migration or backfill.

Then: Slice 3 images (row gate, rollup from public rows, IDs); Slice 4 paging, ID lookup, `not_found`, ID lifetime, zone format; Slice 5
hardening, rule compilation and independent review; then #8.

## 12. Slice 2: schema 4 and the publication flag

### DDL

The definition lives in one place, `native-host/src/schema.rs`, in the library that the host and VisibleStore share. Tables as created:

```sql
PRAGMA application_id = 1296256336; -- 0x4D434D50
PRAGMA user_version = 4;
CREATE TABLE visits (event_id TEXT PRIMARY KEY, url TEXT NOT NULL, title TEXT NOT NULL, visited_at TEXT NOT NULL);
CREATE TABLE tab_snapshots (event_id TEXT PRIMARY KEY, window_id INTEGER NOT NULL, tab_index INTEGER NOT NULL, url TEXT NOT NULL, title TEXT NOT NULL, pinned INTEGER NOT NULL, incognito INTEGER NOT NULL, recorded_at TEXT NOT NULL);
CREATE TABLE image_blobs (
  id            INTEGER PRIMARY KEY,
  sha256        TEXT    NOT NULL UNIQUE CHECK (length(sha256) = 64),
  mime_type     TEXT    NOT NULL,
  byte_length   INTEGER NOT NULL CHECK (byte_length > 0),
  width         INTEGER NOT NULL CHECK (width > 0),
  height        INTEGER NOT NULL CHECK (height > 0),
  created_at    INTEGER NOT NULL,
  last_saved_at INTEGER NOT NULL,
  save_count    INTEGER NOT NULL DEFAULT 1 CHECK (save_count > 0),
  data          BLOB    NOT NULL
);
CREATE INDEX image_blobs_recent ON image_blobs (last_saved_at DESC, id DESC);
CREATE TABLE saved_images (
  id         INTEGER PRIMARY KEY,
  save_id    TEXT    NOT NULL UNIQUE,
  blob_id    INTEGER NOT NULL REFERENCES image_blobs (id) ON DELETE RESTRICT,
  image_url  TEXT    NOT NULL,
  page_url   TEXT,
  frame_url  TEXT,
  saved_at   INTEGER NOT NULL,
  incognito  INTEGER NOT NULL CHECK (incognito IN (0, 1)),
  mcp_public INTEGER NOT NULL CHECK (mcp_public IN (0, 1))
);
CREATE INDEX saved_images_by_blob ON saved_images (blob_id, saved_at DESC, id DESC);
CREATE TABLE image_thumbnails (
  blob_id    INTEGER PRIMARY KEY REFERENCES image_blobs (id) ON DELETE CASCADE,
  mime_type  TEXT    NOT NULL,
  width      INTEGER NOT NULL CHECK (width > 0),
  height     INTEGER NOT NULL CHECK (height > 0),
  generator  INTEGER NOT NULL CHECK (generator > 0),
  data       BLOB    NOT NULL
);
```

### Creation only

- `create_vault` runs the whole definition in one transaction on a newly created file. Schema 3 is never an intermediate state. There
  is no `IF NOT EXISTS`, so the definition fails on any existing table.
- `ensure_schema`, `readable`, `IMAGE_SCHEMA_VERSION`, `vault_newer` and `image_schema_mismatch` are removed. **No code path changes a vault's schema.**
- **All host paths pass `vault::open_vault_detailed`:** `Session` (all reads and writes), `vault_status`, `vault_open`, the switch fence. So the refusal
  is at one choke point. A backup's copy is checked with the same function.

| Vault | Result |
|---|---|
| Correct schema 4 | Opens normally. Opening writes nothing |
| Untagged, any other `user_version` (0–3, 5+), or any difference in shape | `vault_schema_mismatch` |
| Tagged by another application, not a database, corrupt | `vault_invalid` |
| Missing | `vault_missing`; nothing is created |

A refused vault can still be left: `vault_create` and `vault_open` of another file work, since a refused vault is known to have no writer
(`proves_unwritable`). The panel explains the state and offers no conversion, recreation or deletion. Queued visits stay queued in the extension as for any unusable vault,
and go to the next usable vault.

### What the check proves

`schema::check` compares `application_id`, `user_version`, and **the stored text of every object in `sqlite_master`, byte for byte, and their number**.

- **Guaranteed:** columns, order, types, NOT NULL, PRIMARY KEY, UNIQUE, CHECK, DEFAULT, foreign keys, indexes, and that no other table, view, index or trigger exists. A relabelled schema 3 vault fails. Tests also confirm the constraints by behaviour and through `pragma_table_info`.
- **Not guaranteed:** history. A person with SQLite and `writable_schema`, or who creates a correct file and copies old rows in, passes. The trust rests on the only creation path in the product. Row contents are not checked.
- **Cost of strictness:** any future harmless change to a definition's text is a new schema version, which is intended.

### The flag

`mcp_public` is a permission on one save action. Every INSERT writes the literal `0`, for normal and incognito saves. A retried save action keeps the row's value; a new save of the same bytes is a new private row. `image_blobs` has no flag, and setting it touches neither the blob nor its counters.

```text
→ image_mcp_public_set        { requestId, savedImageId, mcpPublic, vaultToken }
← image_mcp_public_set_result { requestId, vaultToken, savedImageId, mcpPublic }
errors: invalid_saved_image_id, invalid_mcp_public, invalid_vault_token, vault_switched, saved_image_not_found, vault errors
```

`mcpPublic` must be a JSON boolean. `image_sources` now also reports `mcpPublic` per row.

UI: the existing save history shows one row per save, so each row got one checkbox, labelled to say that privacy settings always take priority and that MCP is not implemented. A failed change returns the box to its previous state and shows the reason.

**The flag publishes nothing.** No code reads it for MCP yet. Slice 3 must evaluate it together with the URLs; a row with the flag set and an A/B match stays private.

### Implemented and not

Implemented: schema 4, creation-only, refusal everywhere, the flag, the message, the checkbox, VisibleStore's use of the same check.

Not implemented at the end of Slice 2: images in VisibleStore, MCP server, any migration tool. Chrome was not exercised by automated tests (a fake DOM and host); the host binary was, over stdio.

### Operations

The real vault must be regenerated by the PO: create a new vault in the panel. History in the old file stays there and is not carried over. **An old vault cannot be backed up from the app any more**, since backup goes through the same refusal; copy the file instead. The host and extension must be updated together; an old host answers the checkbox with `unknown_type`, and a new host refuses an old vault.

### Handoff to Slice 3

1. Read `saved_images` and `image_blobs` metadata in `snapshot.rs`, never `data`.
2. Row public only if `mcp_public = 1`, and D3 holds for all URLs.
3. Incognito rows: decided in Slice 3, private whatever the flag (section 13).
4. Recompute counts and dates from public rows only; choose image IDs under D4.

## 13. Slice 3: publishing image metadata

No original, thumbnail or byte of `image_blobs.data` is read or published.

### The decision for one row

The unit is one `saved_images` row. A row is public only if all of these hold (`visible_images::save_is_public`):

1. `mcp_public` is exactly 1 and `incognito` is exactly 0. An incognito row is private even with the flag set.
2. Its metadata is usable: known image type, both edges 1 to 65535, a 64-digit hexadecimal hash, a non-empty `save_id`, `saved_at` from 1970 to 9999, and every URL at most 8192 characters.
3. At least one source was judged, and every judged source is `Decision::None`.

| Stored source | Treatment |
|---|---|
| `page_url` or `frame_url` is NULL | Absent, not judged |
| `image_url` is exactly `data:image/<subtype>` (`schema::is_data_image_marker`, the same function the host validates saves with) | Not judged: it names no location |
| Only skipped sources | **Private** |
| `http`, `https` or `blob:` of those | Judged by the policy |
| A or B match, credentials, authentication paths | Private |
| Empty string, unknown scheme, malformed URL, `data:` with a payload or another type, the marker in `page_url` or `frame_url` | Private: the policy answers `NeverSave` |
| Value that is not text (BLOB, number) or invalid UTF-8, also in an optional column | Private: an unreadable source is not an absent one |
| Row whose blob is missing | Private |

Nothing is corrected. A row that fails any check is simply not in the set.

### Native Host and panel

`image_mcp_public_set` with `mcpPublic: true` on an incognito row answers the fixed code `incognito_not_publishable`. The condition is in the UPDATE itself (`?2 = 0 OR incognito = 0`). `mcpPublic: false` is always allowed, which clears a value left by an earlier version. Saving in incognito is unchanged.

The panel shows an incognito row unchecked and disabled, labelled 「シークレット保存のためMCP非公開」, whatever value is stored, and sends nothing for it. Normal rows are unchanged.

### Recomputation

`visible_images` and `visible_image_sources` are the only tables of a private `:memory:` database. They receive public rows only. Count and dates are computed from `visible_image_sources` after every row is in, and sources are read only from there. The vault's `save_count`, `created_at`, `last_saved_at` and `byte_length` are never selected, so they cannot be reused. An image with no public row has no row at all.

Build and freshness work as for visits (sections 6 and 7): one read transaction per call, connection closed before judging, `ATTACH` limited to 0, `query_only` after the build, policy ETag checked again after the build, no cache.

### External IDs

| ID | Input | Form |
|---|---|---|
| Image | HMAC of `sha256`, domain `image` | `image_<32 hex>` |
| Save | HMAC of `save_id`, domain `source` | `source_<32 hex>` |

The key is random per `VisibleStore`, as for visits. Neither `rowid` is read from the vault, so no internal ID, gap or order can reach an ID. An ID is computed for public rows only, from values of that row, so private rows do not affect it. It lasts as long as the store instance.

**SHA-256 is not published.** No described use needs it, and it would let a reader test whether a known image is in the vault. It is read only as the ID input and is not kept in the set.

### API

```rust
store.search_images(&SearchImages { query, from, to, limit }) -> Result<ImagePage, VisibleError>
// ImagePage { items: [VisibleImage], total, has_more }
// VisibleImage { id, mime_type, width, height, public_save_count, first_saved_at, last_saved_at,
//                latest_image_url, latest_page_url }

store.get_image_sources(&GetImageSources { image_id, limit }) -> Result<ImageSourcePage, VisibleError>
// ImageSourcePage { items: [VisibleImageSource], total, has_more }
// VisibleImageSource { id, image_url, page_url, frame_url, saved_at }
```

- Limits: `limit` 1 to 100, default 50. `query` at most 200 characters. URLs at most 8192 characters each, by the rule above.
- An image matches if one public save contains the text, literally, in a source URL and lies in the half-open range. The returned values still describe **all** public saves of the image, so the same image does not change its count between searches.
- Order: newest public save first, then ID. `total` and the limit apply to the public set only.
- Unknown ID, ID of a private image, ID from another store, source ID and malformed ID all give `not_found`.
- Other errors are the fixed codes of section 8.

### Implemented and not

Implemented: the decision, both APIs, recomputation, IDs, the host refusal, the panel.

Not implemented: paging cursors, MCP server, tool definitions, originals or thumbnails through MCP, page MHTML.

### Measurements

Release build, one Windows 11 machine, median of 5, temporary vaults, five saves per blob, two URLs per row, the policy's default rules plus 2 test rules unless noted. `perf_images` reproduces them. Totals include building the set.

| Rows | Public | Extra rules | Total | Vault read (lock held) | Judge and build | Query | Set size | Host commits / failures, worst commit |
|---|---|---|---|---|---|---|---|---|
| 10,000 | 0% | 0 | 9.5 ms | 7.6 ms | 1.2 ms | 0.5 ms | 0.0 MiB | 2 / 0, 21 ms |
| 10,000 | 50% | 0 | 189 ms | 7.4 ms | 175 ms | 6.7 ms | 1.7 MiB | 18 / 0, 24 ms |
| 10,000 | 100% | 0 | 395 ms | 8.1 ms | 373 ms | 13 ms | 3.4 MiB | 48 / 0, 18 ms |
| 100,000 | 0% | 0 | 81 ms | 71 ms | 9.9 ms | 0.7 ms | 0.0 MiB | 5 / 0, 104 ms |
| 100,000 | 50% | 0 | 3.58 s | 155 ms | 3.21 s | 210 ms | 17.4 MiB | 282 / 0, 145 ms |
| 100,000 | 100% | 0 | 7.20 s | 143 ms | 6.72 s | 331 ms | 34.1 MiB | 581 / 0, 275 ms |
| 10,000 | 50% | 100 | 2.81 s | 19 ms | 2.78 s | 11 ms | 1.7 MiB | 260 / 0, 32 ms |
| 10,000 | 50% | 999 (the limit per layer) | 26.3 s | 13 ms | 26.3 s | 11 ms | 1.7 MiB | 2237 / 0, 313 ms |

Cost grows with public rows and rules, not with private rows: a private flag ends the decision before the policy runs. The time is mostly `Policy::evaluate`, which validates the whole policy on every call. The lock is held only for the vault read. "Lock held" also includes the policy-independent steps up to closing the vault. Memory for raw rows during a read was not measured separately.

At many rules a call takes tens of seconds, which an MCP client may treat as a timeout [inferred]. No lock is held during that time, so host writes continue. **Candidates for Slice 5, none adopted:** validate the policy once per read rather than per URL; compile rules once per read. Both must keep the current fail-closed result.

### Remaining risks

- **URLs are published as stored.** The policy judges URLs, it does not clean them: a secret in a query parameter outside the credential rules stays in a public URL. As with titles (D7), no safety of a public URL's contents is claimed.
- **A public save shows the image's type and size,** taken from the shared blob. This is the metadata of the image the user chose to publish, not of a private save.
- **Two public saves of one image are linked by the image ID.** This is the purpose of the ID.
- Private URLs are in this process's memory during the read, before the decision, as for visits. They are dropped with the row list at the end of the build and are not written anywhere.
- Logs and stderr were checked by a source scan banning print and log calls, not by capturing a running MCP process, as none exists.
- The checks prove nothing about hand-edited vaults beyond their shape (section 12). A hand-edited incognito column cannot be detected.

### Handoff to Slice 4

1. Add cursors if needed, computed after the public set is complete. A cursor must carry no internal value.
2. The MCP server depends on this package only and declares no `rusqlite`.
3. `not_found` must stay one answer for every ID that is not public.
4. Decide how long IDs must live: they end with the `VisibleStore` instance (section 8).
