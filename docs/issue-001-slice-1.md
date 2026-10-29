# Issue 001 — automatic local visit recording

## Acceptance criteria

- A completed top-level navigation produces one queued event containing URL, title, and an ISO-8601 timestamp.
- `chrome.storage.local` is the durable queue; an item is removed only after a native-host ACK.
- The Rust host validates the URL scheme and rejects excluded schemes without writing them.
- Replaying the same event id is idempotent.
- SQLite contains the event after a successful ACK.
- No URL or title is written to stdout or normal application logs.
- No tab is closed or modified.

## Explicit non-goals

Tab backup, context-menu page/image saving, tags, search UI, cloud sync, and page-content archiving.
