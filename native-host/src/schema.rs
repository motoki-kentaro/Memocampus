//! Vault schema 7: the one format this version reads and writes.
//!
//! Schema 7 is schema 6 with image tags (`image_tags`, `image_tag_links` and the trigger that
//! removes a tag no image uses; Issue #44). Schema 6 was schema 5 with `navigation_events` (Issue
//! #41); schema 5 was schema 4 with `page_metadata` (Issue #22). Like schema 4 it exists only by
//! creation: a schema 6 vault is refused as any other version, by PO decision, and nothing upgrades it. Nothing upgrades an older vault either, because `visits` has no column
//! that tells whether a row predates the incognito fix (#9), so an upgraded vault could carry such rows
//! under a number that later code trusts. `check` compares the stored definition of every object
//! byte for byte with the ones below, which covers types, NOT NULL, keys, UNIQUE, CHECK, DEFAULT and
//! foreign keys at once, and any object added to or missing from the file. It is read-only.
//!
//! It proves the file's shape, not its history: a person editing the file by hand could forge it.

use rusqlite::Connection;

/// `PRAGMA application_id` of vaults created by Memocampus ("MCMP").
pub const APP_ID: i64 = 0x4D43_4D50;
pub const SCHEMA_VERSION: i64 = 7;

/// Keep each `sql` as SQLite stores it: it keeps the text from `CREATE` on, without the final `;`.
struct Object {
    kind: &'static str,
    name: &'static str,
    table: &'static str,
    /// None for the indexes SQLite creates itself for PRIMARY KEY and UNIQUE on non-rowid columns.
    sql: Option<&'static str>,
}

const OBJECTS: [Object; 22] = [
    Object {
        kind: "table",
        name: "visits",
        table: "visits",
        sql: Some(
            "CREATE TABLE visits (event_id TEXT PRIMARY KEY, url TEXT NOT NULL, title TEXT NOT NULL, visited_at TEXT NOT NULL)",
        ),
    },
    Object {
        kind: "index",
        name: "sqlite_autoindex_visits_1",
        table: "visits",
        sql: None,
    },
    Object {
        kind: "table",
        name: "tab_snapshots",
        table: "tab_snapshots",
        sql: Some(
            "CREATE TABLE tab_snapshots (event_id TEXT PRIMARY KEY, window_id INTEGER NOT NULL, tab_index INTEGER NOT NULL, url TEXT NOT NULL, title TEXT NOT NULL, pinned INTEGER NOT NULL, incognito INTEGER NOT NULL, recorded_at TEXT NOT NULL)",
        ),
    },
    Object {
        kind: "index",
        name: "sqlite_autoindex_tab_snapshots_1",
        table: "tab_snapshots",
        sql: None,
    },
    Object {
        kind: "table",
        name: "image_blobs",
        table: "image_blobs",
        sql: Some(
            "CREATE TABLE image_blobs (
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
)",
        ),
    },
    Object {
        kind: "index",
        name: "sqlite_autoindex_image_blobs_1",
        table: "image_blobs",
        sql: None,
    },
    Object {
        kind: "index",
        name: "image_blobs_recent",
        table: "image_blobs",
        sql: Some("CREATE INDEX image_blobs_recent ON image_blobs (last_saved_at DESC, id DESC)"),
    },
    // `incognito` and `mcp_public` belong to one save action and have no default, so a row whose
    // origin or publication was not stated explicitly cannot exist.
    Object {
        kind: "table",
        name: "saved_images",
        table: "saved_images",
        sql: Some(
            "CREATE TABLE saved_images (
  id         INTEGER PRIMARY KEY,
  save_id    TEXT    NOT NULL UNIQUE,
  blob_id    INTEGER NOT NULL REFERENCES image_blobs (id) ON DELETE RESTRICT,
  image_url  TEXT    NOT NULL,
  page_url   TEXT,
  frame_url  TEXT,
  saved_at   INTEGER NOT NULL,
  incognito  INTEGER NOT NULL CHECK (incognito IN (0, 1)),
  mcp_public INTEGER NOT NULL CHECK (mcp_public IN (0, 1))
)",
        ),
    },
    Object {
        kind: "index",
        name: "saved_images_by_blob",
        table: "saved_images",
        sql: Some(
            "CREATE INDEX saved_images_by_blob ON saved_images (blob_id, saved_at DESC, id DESC)",
        ),
    },
    Object {
        kind: "table",
        name: "image_thumbnails",
        table: "image_thumbnails",
        sql: Some(
            "CREATE TABLE image_thumbnails (
  blob_id    INTEGER PRIMARY KEY REFERENCES image_blobs (id) ON DELETE CASCADE,
  mime_type  TEXT    NOT NULL,
  width      INTEGER NOT NULL CHECK (width > 0),
  height     INTEGER NOT NULL CHECK (height > 0),
  generator  INTEGER NOT NULL CHECK (generator > 0),
  data       BLOB    NOT NULL
)",
        ),
    },
    Object {
        kind: "index",
        name: "sqlite_autoindex_saved_images_1",
        table: "saved_images",
        sql: None,
    },
    // One row per URL: the metadata of the latest visit of it whose page could be read. `event_id`
    // is that visit, so a row cannot exist without a stored visit; the host copies `url` from it.
    // The limits are those of `page_metadata.rs`; an empty text is NULL.
    Object {
        kind: "table",
        name: "page_metadata",
        table: "page_metadata",
        sql: Some(
            "CREATE TABLE page_metadata (
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
)",
        ),
    },
    Object {
        kind: "index",
        name: "sqlite_autoindex_page_metadata_1",
        table: "page_metadata",
        sql: None,
    },
    // One row per target visit: the observed navigation that produced it (Issue #41). The row is
    // written in the target visit's transaction, so it cannot exist without that visit. The source
    // is the document shown before in the same tab, or in the tab that opened this one; it is not
    // proof of a click or a hyperlink. It is NULL when it was not known, not stored, or not allowed
    // by the policy. No URL is kept here: both ends are visits, judged again by any future reader.
    Object {
        kind: "table",
        name: "navigation_events",
        table: "navigation_events",
        sql: Some(
            "CREATE TABLE navigation_events (
  target_visit_id  TEXT    NOT NULL PRIMARY KEY REFERENCES visits (event_id) ON DELETE CASCADE,
  source_visit_id  TEXT    REFERENCES visits (event_id) ON DELETE SET NULL,
  kind             TEXT    NOT NULL CHECK (kind IN ('document', 'history_state')),
  transition_type  TEXT    NOT NULL CHECK (transition_type IN ('link', 'typed', 'auto_bookmark', 'auto_subframe', 'manual_subframe', 'generated', 'start_page', 'form_submit', 'reload', 'keyword', 'keyword_generated', 'unknown')),
  client_redirect  INTEGER NOT NULL CHECK (client_redirect IN (0, 1)),
  server_redirect  INTEGER NOT NULL CHECK (server_redirect IN (0, 1)),
  forward_back     INTEGER NOT NULL CHECK (forward_back IN (0, 1)),
  from_address_bar INTEGER NOT NULL CHECK (from_address_bar IN (0, 1)),
  new_tab          INTEGER NOT NULL CHECK (new_tab IN (0, 1)),
  occurred_at      TEXT    NOT NULL CHECK (length(occurred_at) = 24),
  CHECK (source_visit_id IS NULL OR source_visit_id <> target_visit_id)
)",
        ),
    },
    Object {
        kind: "index",
        name: "sqlite_autoindex_navigation_events_1",
        table: "navigation_events",
        sql: None,
    },
    Object {
        kind: "index",
        name: "navigation_events_by_source",
        table: "navigation_events",
        sql: Some("CREATE INDEX navigation_events_by_source ON navigation_events (source_visit_id)"),
    },
    // Tags the user gives a stored image afterwards (Issue #44). A tag belongs to the image (the
    // distinct bytes, `image_blobs`), not to one save action. `name` is the normalized form made
    // by `image_tags::normalize_tag` (lowercase, trimmed, 1 to 64 characters); the CHECK repeats the
    // parts SQLite can verify. Tags hold no URL and are not read by MCP.
    Object {
        kind: "table",
        name: "image_tags",
        table: "image_tags",
        sql: Some(
            "CREATE TABLE image_tags (
  id    INTEGER PRIMARY KEY,
  name  TEXT    NOT NULL UNIQUE CHECK (length(name) BETWEEN 1 AND 64 AND name = trim(name) AND name = lower(name))
)",
        ),
    },
    Object {
        kind: "index",
        name: "sqlite_autoindex_image_tags_1",
        table: "image_tags",
        sql: None,
    },
    // One row per tag on an image. Removing the image removes its tags (CASCADE); a tag still
    // given to an image cannot be removed (RESTRICT).
    Object {
        kind: "table",
        name: "image_tag_links",
        table: "image_tag_links",
        sql: Some(
            "CREATE TABLE image_tag_links (
  blob_id  INTEGER NOT NULL REFERENCES image_blobs (id) ON DELETE CASCADE,
  tag_id   INTEGER NOT NULL REFERENCES image_tags (id) ON DELETE RESTRICT,
  PRIMARY KEY (blob_id, tag_id)
)",
        ),
    },
    Object {
        kind: "index",
        name: "sqlite_autoindex_image_tag_links_1",
        table: "image_tag_links",
        sql: None,
    },
    Object {
        kind: "index",
        name: "image_tag_links_by_tag",
        table: "image_tag_links",
        sql: Some("CREATE INDEX image_tag_links_by_tag ON image_tag_links (tag_id, blob_id)"),
    },
    // A tag lives exactly as long as some image has it: removing its last link, by the host or by
    // the CASCADE of a removed image, removes the tag in the same transaction.
    Object {
        kind: "trigger",
        name: "image_tags_drop_unused",
        table: "image_tag_links",
        sql: Some(
            "CREATE TRIGGER image_tags_drop_unused AFTER DELETE ON image_tag_links
BEGIN
  DELETE FROM image_tags WHERE id = old.tag_id AND NOT EXISTS (SELECT 1 FROM image_tag_links WHERE tag_id = old.tag_id);
END",
        ),
    },
];

/// Everything that makes an empty database a schema 7 vault, for one transaction. There is no form
/// of this that can run against an existing vault: `CREATE TABLE` fails if the table exists.
pub fn create_sql() -> String {
    let mut sql =
        format!("PRAGMA application_id = {APP_ID}; PRAGMA user_version = {SCHEMA_VERSION};");
    for object in &OBJECTS {
        if let Some(statement) = object.sql {
            sql.push(' ');
            sql.push_str(statement);
            sql.push(';');
        }
    }
    sql
}

/// The recorded form of a `data:` image source: `data:image/<subtype>` and never a payload. It names
/// no location, so it is the one stored `image_url` that is not a URL to judge.
pub fn is_data_image_marker(raw: &str) -> bool {
    let Some(mime) = raw.strip_prefix("data:") else {
        return false;
    };
    let (kind, subtype) = mime.split_once('/').unwrap_or(("", ""));
    kind == "image"
        && !subtype.is_empty()
        && subtype
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-'))
}

#[derive(Debug)]
pub enum SchemaError {
    /// Tagged by another application.
    ForeignApplicationId(i64),
    /// Untagged, or any `user_version` other than `SCHEMA_VERSION`.
    UnsupportedVersion(i64),
    /// The named object is missing or differs; `"unexpected_object"` for one that should not exist.
    Shape(&'static str),
    Sqlite(rusqlite::Error),
}

impl From<rusqlite::Error> for SchemaError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

/// Accepts exactly a vault that `create_sql` could have made. Never writes.
pub fn check(db: &Connection) -> Result<(), SchemaError> {
    let app_id: i64 = db.query_row("PRAGMA application_id", [], |row| row.get(0))?;
    if app_id != APP_ID && app_id != 0 {
        return Err(SchemaError::ForeignApplicationId(app_id));
    }
    let version: i64 = db.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if app_id != APP_ID || version != SCHEMA_VERSION {
        return Err(SchemaError::UnsupportedVersion(version));
    }
    let mut statement = db.prepare("SELECT type, name, tbl_name, sql FROM sqlite_master")?;
    let stored = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for object in &OBJECTS {
        let matches = stored.iter().any(|(kind, name, table, sql)| {
            kind == object.kind
                && name == object.name
                && table == object.table
                && sql.as_deref() == object.sql
        });
        if !matches {
            return Err(SchemaError::Shape(object.name));
        }
    }
    if stored.len() != OBJECTS.len() {
        return Err(SchemaError::Shape("unexpected_object"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault(extra: &str) -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(&format!("{} {extra}", create_sql()))
            .unwrap();
        db
    }

    fn column(db: &Connection, table: &str, name: &str) -> (String, bool, Option<String>, i64) {
        db.query_row(
            "SELECT type, \"notnull\", dflt_value, pk FROM pragma_table_info(?1) WHERE name = ?2",
            [table, name],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap()
    }

    #[test]
    fn a_new_database_passes_and_has_the_stated_constraints() {
        let db = vault("");
        check(&db).unwrap();
        assert_eq!(
            column(&db, "saved_images", "mcp_public"),
            ("INTEGER".to_owned(), true, None, 0)
        );
        assert_eq!(
            column(&db, "saved_images", "incognito"),
            ("INTEGER".to_owned(), true, None, 0)
        );
        assert_eq!(column(&db, "visits", "event_id").3, 1);
        for (table, count) in [("saved_images", 2), ("image_blobs", 2)] {
            let indexes: i64 = db
                .query_row(
                    "SELECT count(*) FROM pragma_index_list(?1)",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(indexes, count, "{table}");
        }
        let keys: i64 = db
            .query_row(
                "SELECT count(*) FROM pragma_foreign_key_list('saved_images')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(keys, 1);
    }

    #[test]
    fn the_definitions_really_are_what_sqlite_stores() {
        // Also after a rebuild of the file: backups and VACUUM keep the stored text.
        let db = vault("");
        db.execute_batch("VACUUM;").unwrap();
        check(&db).unwrap();
    }

    #[test]
    fn navigation_events_hold_only_allowed_values_and_follow_their_visits() {
        let db = vault("");
        db.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        assert_eq!(column(&db, "navigation_events", "target_visit_id").3, 1);
        assert_eq!(
            column(&db, "navigation_events", "source_visit_id"),
            ("TEXT".to_owned(), false, None, 0)
        );
        let mut keys = db
            .prepare("SELECT \"from\", \"table\", \"to\", on_delete FROM pragma_foreign_key_list('navigation_events') ORDER BY 1")
            .unwrap()
            .query_map([], |row| {
                Ok(format!(
                    "{}>{}.{} {}",
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        keys.sort();
        assert_eq!(
            keys,
            [
                "source_visit_id>visits.event_id SET NULL",
                "target_visit_id>visits.event_id CASCADE"
            ]
        );
        db.execute_batch(
            "INSERT INTO visits VALUES ('a', 'https://a.test/', '', '2026-11-06T12:00:00.000Z');
             INSERT INTO visits VALUES ('b', 'https://b.test/', '', '2026-11-06T12:00:00.000Z');",
        )
        .unwrap();
        let insert = |values: &str| {
            db.execute(
                &format!("INSERT INTO navigation_events VALUES ({values})"),
                [],
            )
        };
        const OK: &str = "'b', 'a', 'document', 'link', 0, 0, 0, 0, 0, '2026-11-06T12:00:00.000Z'";
        for bad in [
            "'b', 'b', 'document', 'link', 0, 0, 0, 0, 0, '2026-11-06T12:00:00.000Z'",
            "'b', 'a', 'fragment', 'link', 0, 0, 0, 0, 0, '2026-11-06T12:00:00.000Z'",
            "'b', 'a', 'document', 'auto_toplevel', 0, 0, 0, 0, 0, '2026-11-06T12:00:00.000Z'",
            "'b', 'a', 'document', 'link', 2, 0, 0, 0, 0, '2026-11-06T12:00:00.000Z'",
            "'b', 'a', 'document', 'link', 0, 0, 0, 0, NULL, '2026-11-06T12:00:00.000Z'",
            "'b', 'a', 'document', 'link', 0, 0, 0, 0, 0, '2026-11-06T12:00:00Z'",
            "'b', 'missing', 'document', 'link', 0, 0, 0, 0, 0, '2026-11-06T12:00:00.000Z'",
            "'missing', 'a', 'document', 'link', 0, 0, 0, 0, 0, '2026-11-06T12:00:00.000Z'",
            "NULL, 'a', 'document', 'link', 0, 0, 0, 0, 0, '2026-11-06T12:00:00.000Z'",
        ] {
            assert!(insert(bad).is_err(), "{bad}");
        }
        insert(OK).unwrap();
        assert!(insert(OK).is_err(), "one navigation per target visit");
        let source = |db: &Connection| -> Option<String> {
            db.query_row("SELECT source_visit_id FROM navigation_events", [], |row| {
                row.get(0)
            })
            .unwrap()
        };
        assert_eq!(source(&db).as_deref(), Some("a"));
        db.execute("DELETE FROM visits WHERE event_id = 'a'", [])
            .unwrap();
        assert_eq!(source(&db), None, "a removed source becomes NULL");
        db.execute("DELETE FROM visits WHERE event_id = 'b'", [])
            .unwrap();
        let rows: i64 = db
            .query_row("SELECT count(*) FROM navigation_events", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(rows, 0, "a removed target takes its navigation with it");
    }

    #[test]
    fn image_tags_hold_only_normalized_names_follow_their_images_and_never_outlive_their_last_link()
    {
        let db = vault("");
        db.execute_batch(
            "PRAGMA foreign_keys = ON;
             INSERT INTO image_blobs (id, sha256, mime_type, byte_length, width, height, created_at, last_saved_at, data)
               VALUES (1, printf('%064d', 1), 'image/png', 1, 1, 1, 0, 0, x'00'),
                      (2, printf('%064d', 2), 'image/png', 1, 1, 1, 0, 0, x'00');
             INSERT INTO saved_images (id, save_id, blob_id, image_url, saved_at, incognito, mcp_public)
               VALUES (1, 's1', 1, 'https://a.test/1.png', 0, 0, 0);",
        )
        .unwrap();
        let mut keys = db
            .prepare("SELECT \"from\" || '>' || \"table\" || '.' || \"to\" || ' ' || on_delete FROM pragma_foreign_key_list('image_tag_links')")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        keys.sort();
        assert_eq!(
            keys,
            [
                "blob_id>image_blobs.id CASCADE",
                "tag_id>image_tags.id RESTRICT"
            ]
        );
        for bad in [
            "''",
            "' cat'",
            "'cat '",
            "'Cat'",
            "NULL",
            "'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'", // 65
        ] {
            assert!(
                db.execute(&format!("INSERT INTO image_tags (name) VALUES ({bad})"), [])
                    .is_err(),
                "{bad}"
            );
        }
        let count = |sql: &str| -> i64 { db.query_row(sql, [], |row| row.get(0)).unwrap() };
        db.execute_batch(
            "INSERT INTO image_tags (id, name) VALUES (10, 'cat'), (11, 'reaction'), (12, '猫');
             INSERT INTO image_tag_links VALUES (1, 10), (1, 11), (2, 10), (2, 12);",
        )
        .unwrap();
        assert!(db
            .execute("INSERT INTO image_tags (name) VALUES ('cat')", [])
            .is_err());
        assert!(db
            .execute("INSERT INTO image_tag_links VALUES (1, 10)", [])
            .is_err());
        assert!(db
            .execute("INSERT INTO image_tag_links VALUES (3, 10)", [])
            .is_err());
        assert!(db
            .execute("INSERT INTO image_tag_links VALUES (1, 99)", [])
            .is_err());
        assert!(
            db.execute("DELETE FROM image_tags WHERE id = 10", [])
                .is_err(),
            "a tag in use cannot be removed"
        );
        // The last link of a tag takes the tag with it; other links keep theirs.
        db.execute(
            "DELETE FROM image_tag_links WHERE blob_id = 2 AND tag_id = 12",
            [],
        )
        .unwrap();
        assert_eq!(count("SELECT count(*) FROM image_tags WHERE id = 12"), 0);
        db.execute(
            "DELETE FROM image_tag_links WHERE blob_id = 2 AND tag_id = 10",
            [],
        )
        .unwrap();
        assert_eq!(count("SELECT count(*) FROM image_tags WHERE id = 10"), 1);
        // A removed image takes its links, and through them the tags no other image has.
        db.execute_batch("DELETE FROM saved_images; DELETE FROM image_blobs WHERE id = 1;")
            .unwrap();
        assert_eq!(count("SELECT count(*) FROM image_tag_links"), 0);
        assert_eq!(count("SELECT count(*) FROM image_tags"), 0);
        assert_eq!(count("SELECT count(*) FROM pragma_foreign_key_check"), 0);
        let integrity: String = db
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
    }

    #[test]
    fn creating_over_an_existing_vault_fails_instead_of_filling_gaps() {
        let db = vault("");
        assert!(db.execute_batch(&create_sql()).is_err());
    }

    #[test]
    fn differences_are_named_and_extras_are_refused() {
        let replaced = |from: &str, to: &str| {
            let db = Connection::open_in_memory().unwrap();
            let sql = create_sql();
            assert!(sql.contains(from), "{from}");
            db.execute_batch(&sql.replace(from, to)).unwrap();
            check(&db).unwrap_err()
        };
        assert!(matches!(
            replaced(
                "mcp_public INTEGER NOT NULL CHECK (mcp_public IN (0, 1))",
                "mcp_public INTEGER NOT NULL DEFAULT 0 CHECK (mcp_public IN (0, 1))"
            ),
            SchemaError::Shape("saved_images")
        ));
        assert!(matches!(
            replaced("(mcp_public IN (0, 1))", "(mcp_public IN (0, 1, 2))"),
            SchemaError::Shape("saved_images")
        ));
        assert!(matches!(
            replaced(
                "url TEXT NOT NULL, title TEXT NOT NULL, visited_at",
                "url TEXT, title TEXT NOT NULL, visited_at"
            ),
            SchemaError::Shape("visits")
        ));
        assert!(matches!(
            replaced(
                "sha256        TEXT    NOT NULL UNIQUE",
                "sha256        TEXT    NOT NULL"
            ),
            SchemaError::Shape("image_blobs")
        ));
        for extra in [
            "CREATE TABLE more (x);",
            "CREATE VIEW v AS SELECT 1;",
            "CREATE INDEX i ON visits (url);",
            "CREATE TRIGGER t AFTER INSERT ON visits BEGIN SELECT 1; END;",
        ] {
            assert!(
                matches!(
                    check(&vault(extra)).unwrap_err(),
                    SchemaError::Shape("unexpected_object")
                ),
                "{extra}"
            );
        }
        assert!(matches!(
            check(&vault("DROP TABLE image_thumbnails;")).unwrap_err(),
            SchemaError::Shape("image_thumbnails")
        ));
        assert!(matches!(
            check(&vault("DROP TRIGGER image_tags_drop_unused;")).unwrap_err(),
            SchemaError::Shape("image_tags_drop_unused")
        ));
        assert!(matches!(
            replaced(
                "REFERENCES image_blobs (id) ON DELETE CASCADE,\n  tag_id",
                "REFERENCES image_blobs (id) ON DELETE RESTRICT,\n  tag_id"
            ),
            SchemaError::Shape("image_tag_links")
        ));
        assert!(matches!(
            replaced(" AND name = lower(name))", ")"),
            SchemaError::Shape("image_tags")
        ));
    }

    #[test]
    fn versions_and_tags_are_checked_first() {
        for (extra, expected) in [
            ("PRAGMA user_version = 3;", 3),
            ("PRAGMA user_version = 4;", 4),
            ("PRAGMA user_version = 5;", 5),
            ("PRAGMA user_version = 6;", 6),
            ("PRAGMA user_version = 8;", 8),
            ("PRAGMA application_id = 0;", 7),
        ] {
            assert!(
                matches!(check(&vault(extra)).unwrap_err(), SchemaError::UnsupportedVersion(v) if v == expected),
                "{extra}"
            );
        }
        assert!(matches!(
            check(&vault("PRAGMA application_id = 42;")).unwrap_err(),
            SchemaError::ForeignApplicationId(42)
        ));
    }
}
