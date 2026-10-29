# Issue 003 — vault location independent of the host executable

Migration of the pre-#003 database is out of scope: the old `native-host/target/release/memocampus.sqlite3`
is never read, written, moved, or deleted by the new host. Users start by creating a new vault.

## Files

- Config: `%LOCALAPPDATA%\Memocampus\config.json` — `{ "version": 1, "activeVault": "<absolute path>", "locationApproved": false }`.
  Written atomically (temp file + flush + rename).
- Default vault: `%LOCALAPPDATA%\Memocampus\memocampus.sqlite3` (only created by an explicit request).
- Lock: `%LOCALAPPDATA%\Memocampus\config.lock`, opened with no sharing; serializes vault creation and
  config updates across host processes and is released by the OS if a host dies.
- `MEMOCAMPUS_DB` (development only) overrides the vault and disables all management operations.
  `MEMOCAMPUS_APP_DIR` (tests only) replaces `%LOCALAPPDATA%\Memocampus`.

## Rules

- Vaults are opened without SQLite's create flag. A missing vault is an error (`vault_missing`); nothing is recreated.
- A vault is valid when `application_id` is `0x4D434D50` (created by Memocampus) or `0` (untagged), and the
  `visits` / `tab_snapshots` tables have the expected columns. Opening a user-chosen file also requires `PRAGMA quick_check` = ok.
- New vaults use exclusive file creation (`create_new`); an existing file, or a leftover `-journal` / `-wal` / `-shm`
  beside the target, aborts creation.
- Switching vaults write-locks the previously active vault while `config.json` is replaced. Writers take the write
  lock and re-read the config before inserting; if the vault changed they write nothing and return
  `vault_switched` without an ACK, so the extension retries against the new vault.
- Backups (SQLite Backup API) are copied under one read transaction into a hidden `.partial` file in the chosen
  folder, checked (`integrity_check`, schema, row counts equal to the snapshot), flushed, then renamed without
  replacement to `memocampus-backup-YYYYMMDD-HHMMSSZ.sqlite3` (`-1`, `-2`, … on collision). On failure the partial
  file is removed and no success is reported. The active vault does not change.

## Active vault location check

| Verdict | Conditions | Handling |
|---|---|---|
| `rejected` | UNC path, network drive, optical drive, RAM disk, unknown drive type, unresolvable parent, the host executable's directory | refused |
| `needs_approval` | registered Cloud Files sync root (`SyncRootManager`), OneDrive env vars, cloud-file attributes, removable drive, not NTFS/ReFS, volume label "Google Drive", volume info unavailable | allowed only with explicit approval (`approveWarnings: true`) |
| `no_issue_detected` | none of the above | allowed; the UI states that unregistered sync tools cannot be detected |

Symlinks, junctions and SUBST drives are resolved with `canonicalize` before checking. Backup destinations
(NAS, sync folders) are not restricted.

## Consistency with a vault switch

- Writes are exact: a write commits to a vault only while `config.json` still names it, checked under that
  vault's write lock; the switch holds the same lock while replacing the config. Every write therefore lands
  before the switch (old vault) or after it (new vault).
- The fence is skipped only when the old vault is missing or definitively not a vault (writers would be refused
  too). Lock contention, I/O or other database errors while opening it abort the switch; the config is unchanged
  and the operation can be retried.
- Reads are revalidated, not excluded: before each request a cached connection is checked against the config and
  reopened if the vault changed. A read concurrent with a switch may still return the old vault's data (stale by
  at most that request). Today every native message is its own host process, so this matters for future
  long-lived hosts; the search UI also restarts its result list after a switch it initiates.

## Event delivery queue (extension)

- `chrome.storage.local` `pendingVisitEvents` holds visits and tab snapshots (max 5000). All updates are
  serialized in the service worker; only one flush runs at a time.
- Host error codes `invalid_url`, `invalid_event`, `invalid_message`, `unknown_type` are permanent: the event is
  copied to `deadLetterEvents` (`{ errorCode, eventId, failedAt, event }`, max 1000) and only then removed from
  the queue. If that copy cannot be stored (full or write error) the event stays at the head and delivery pauses.
- Everything else (host unreachable, 30 s timeout, no/missing/busy vault, unknown codes, malformed ACKs) is
  temporary: the event stays queued. Resending after a lost ACK is safe (`INSERT OR IGNORE` by event id).
- A visit that cannot be queued at all (queue full, storage failure) is counted in `queueStats.dropped`.
  Quarantined or dropped events set a "!" badge on the toolbar icon and are listed in the side panel.
- "Record all tabs" queues every snapshot in one write (all or none), then delivers, and reports stored,
  still-pending and quarantined counts separately.

## Native Messaging

Existing `visit` / `tab_snapshot` / `search_pages` are unchanged on success. Every `visit` / `tab_snapshot` (and
any unrecognised message type) now gets exactly one answer: `{ "ok": false, "eventId": "…" | null, "error": "<code>" }`
on failure — `invalid_url`, `invalid_event` (undecodable fields or empty id), `unknown_type`, a vault code when no
vault is usable — and undecodable JSON gets `{ "ok": false, "eventId": null, "error": "invalid_message" }`.
`search_pages` answers a `search_pages_result` with `error` when no vault is usable.

New messages, each answered by one `{ "ok", "type": "vault_result", "requestId", … }`:

- `vault_status` → `status: { state, vaultPath, counts, location, locationApproved, defaultPath, defaultExists, managementEnabled, overridden }`
  (`state`: `ready`, `not_configured`, `missing`, `invalid`, `config_invalid`, `app_dir_unavailable`). Read-only.
- `vault_create` `{ path? | useDefault?, approveWarnings? }` → `{ path, status }`. Without `path`/`useDefault` a native save dialog is shown.
- `vault_open` `{ path?, approveWarnings? }` → `{ path, status }`. Without `path` a native open dialog is shown.
- `vault_backup` `{ directory? }` → `{ backupPath }`. Without `directory` a native folder dialog is shown.

Error codes: `invalid_request_id`, `unknown_type`, `management_disabled`, `invalid_path`, `parent_missing`, `file_exists`,
`sidecar_exists`, `location_rejected` / `location_needs_approval` (with `path`, `location`), `vault_missing`, `vault_invalid`,
`vault_busy`, `cancelled`, `backup_failed`, `io_error`, `db_error`.

## Follow-up (not in this issue)

- F5: backups interrupted by a crash can leave hidden `.memocampus-backup-*.partial` files in the backup folder.
  They are never deleted automatically. A later issue should list such candidates and delete them only after
  explicit user confirmation.
- Temporary delivery failures are retried on the next navigation, browser start, or vault change; there is no
  timer-based retry yet.

## Explicit non-goals

Migrating or deleting the old database, automatic backup, multi-device sync, vaults on network shares.
