use memocampus_native_host::navigation::{self, Navigation};
use memocampus_native_host::page_metadata::{self, PageMetadata};
use memocampus_native_host::policy::Decision;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

mod dialog;
mod image_original;
mod image_protocol;
mod image_publication;
mod image_query;
mod images;
mod launch;
mod location;
mod manage;
mod policy_gate;
mod search;
mod vault;

#[derive(Debug, Deserialize)]
struct VisitEvent {
    id: String,
    url: String,
    title: String,
    #[serde(rename = "visitedAt")]
    visited_at: String,
}

#[derive(Debug, Deserialize)]
struct TabSnapshot {
    id: String,
    #[serde(rename = "windowId")]
    window_id: i64,
    #[serde(rename = "tabIndex")]
    tab_index: i64,
    url: String,
    title: String,
    pinned: bool,
    incognito: bool,
    #[serde(rename = "recordedAt")]
    recorded_at: String,
}

#[derive(Debug, Serialize)]
struct Ack<'a> {
    ok: bool,
    /// `null` only when the message carried no usable string id.
    #[serde(rename = "eventId")]
    event_id: Option<&'a str>,
    /// Set only on failure, so successful acknowledgements are unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'static str>,
    /// `"policy"` only for an event a valid policy refused: nothing was stored, and resending is
    /// pointless. A policy that could not be evaluated is an `error`, never this.
    #[serde(skip_serializing_if = "Option::is_none")]
    dropped: Option<&'static str>,
    /// Only for a stored visit that came with a `navigation`: `"stored"` when its row is in the
    /// vault (also on a resend), `"invalid"` when it was refused and nothing of it was kept. Absent
    /// otherwise, so acknowledgements without a navigation are unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    navigation: Option<&'static str>,
}

#[derive(Debug, PartialEq, Eq)]
enum Recorded {
    Stored { navigation: Option<&'static str> },
    DroppedByPolicy,
}

const NAVIGATION_STORED: &str = "stored";
const NAVIGATION_INVALID: &str = "invalid";

impl<'a> Ack<'a> {
    fn from_result(event_id: Option<&'a str>, result: Result<Recorded, &'static str>) -> Self {
        let dropped = matches!(result, Ok(Recorded::DroppedByPolicy)).then_some("policy");
        let navigation = match result {
            Ok(Recorded::Stored { navigation }) => navigation,
            _ => None,
        };
        Self {
            ok: result.is_ok(),
            event_id,
            error: result.err(),
            dropped,
            navigation,
        }
    }
}

// Permanent rejections: resending the same event can never succeed, so the extension
// quarantines it instead of retrying. Vault errors (`VaultError::code`) are all temporary.
const INVALID_URL: &str = "invalid_url";
const INVALID_EVENT: &str = "invalid_event";
const INVALID_MESSAGE: &str = "invalid_message";
const UNKNOWN_TYPE: &str = "unknown_type";
/// A visit or snapshot not declared normal with `"incognito": false`. Also permanent, but the
/// extension discards the event instead of quarantining it, so the code carries no event data.
const INCOGNITO_NOT_ALLOWED: &str = "incognito_not_allowed";

/// Stores one queued event. Every outcome is reported, so a bad event cannot stall the queue.
fn record_event(
    session: &mut vault::Session,
    gate: &policy_gate::PolicyGate,
    value: serde_json::Value,
) -> Result<Recorded, &'static str> {
    let kind = value.get("type").and_then(|value| value.as_str());
    // Checked first: a refusal for any other reason would have the extension keep the URL. The
    // host cannot see the browser, so it can only enforce what the extension declares.
    if matches!(kind, Some("visit" | "tab_snapshot"))
        && value.get("incognito").and_then(|value| value.as_bool()) != Some(false)
    {
        return Err(INCOGNITO_NOT_ALLOWED);
    }
    // Before the event is parsed: a refused URL must not come back as an invalid event, which the
    // extension would quarantine with its URL. Before any INSERT, in every case.
    if matches!(kind, Some("visit" | "tab_snapshot")) {
        if let Some(url) = value.get("url").and_then(|url| url.as_str()) {
            if is_recordable_url(url) && gate.decide(url)? == Decision::NeverSave {
                return Ok(Recorded::DroppedByPolicy);
            }
        }
    }
    match kind {
        Some("visit") => {
            // Best effort and judged on its own: a page that cannot be used never refuses the visit.
            let page = value.get("page").and_then(page_metadata::parse);
            // Present but unusable is reported (`"invalid"`); it never refuses the visit either.
            let navigation_sent = value.get("navigation").cloned();
            let event: VisitEvent = serde_json::from_value(value).map_err(|_| INVALID_EVENT)?;
            if event.id.is_empty() {
                return Err(INVALID_EVENT);
            }
            if !is_recordable_url(&event.url) {
                return Err(INVALID_URL);
            }
            let navigation = navigation_sent.map(|raw| navigation::parse(&raw, &event.id));
            // Before the write: a source the policy cannot judge stops the whole event (temporary).
            let source = match &navigation {
                Some(Some(Navigation {
                    source_visit_id: Some(id),
                    ..
                })) => allowed_source(session, gate, id)?,
                _ => None,
            };
            let mut navigation_result = navigation.as_ref().map(|_| NAVIGATION_INVALID);
            session
                .write(|db| {
                    let stored = db.execute("INSERT OR IGNORE INTO visits (event_id, url, title, visited_at) VALUES (?1, ?2, ?3, ?4)", params![event.id, event.url, event.title, event.visited_at])?;
                    if let Some(page) = &page {
                        store_page(db, &event.id, &event.url, page)?;
                    }
                    if let Some(Some(found)) = &navigation {
                        if store_navigation(db, &event.id, &event.url, found, source.as_ref())? {
                            navigation_result = Some(NAVIGATION_STORED);
                        }
                    }
                    Ok(stored)
                })
                .map(|()| Recorded::Stored {
                    navigation: navigation_result,
                })
                .map_err(vault::VaultError::code)
        }
        Some("tab_snapshot") => {
            let event: TabSnapshot = serde_json::from_value(value).map_err(|_| INVALID_EVENT)?;
            if event.id.is_empty() {
                return Err(INVALID_EVENT);
            }
            if !is_recordable_url(&event.url) {
                return Err(INVALID_URL);
            }
            session
                .write(|db| db.execute("INSERT OR IGNORE INTO tab_snapshots (event_id, window_id, tab_index, url, title, pinned, incognito, recorded_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)", params![event.id, event.window_id, event.tab_index, event.url, event.title, event.pinned, event.incognito, event.recorded_at]))
                .map(|()| Recorded::Stored { navigation: None })
                .map_err(vault::VaultError::code)
        }
        _ => Err(UNKNOWN_TYPE),
    }
}

/// Upserts the metadata of the visit `event_id`, in the visit's transaction. The row's URL is the
/// stored visit's, and only a visit stored with exactly this URL gives one. A capture older than the
/// stored one changes nothing, so a late or resent event cannot replace newer metadata; resending
/// the same event writes the same values. Never `INSERT OR REPLACE`, which deletes the row first.
fn store_page(
    db: &rusqlite::Connection,
    event_id: &str,
    url: &str,
    page: &PageMetadata,
) -> rusqlite::Result<usize> {
    db.execute(
        "INSERT INTO page_metadata (url, event_id, title, description, og_title, og_description, og_site_name, og_image_url, favicon_url, captured_at)
         SELECT url, event_id, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10 FROM visits WHERE event_id = ?1 AND url = ?2
         ON CONFLICT (url) DO UPDATE SET
           event_id = excluded.event_id, title = excluded.title, description = excluded.description,
           og_title = excluded.og_title, og_description = excluded.og_description,
           og_site_name = excluded.og_site_name, og_image_url = excluded.og_image_url,
           favicon_url = excluded.favicon_url, captured_at = excluded.captured_at
         WHERE excluded.captured_at >= page_metadata.captured_at",
        params![
            event_id,
            url,
            page.title,
            page.description,
            page.og_title,
            page.og_description,
            page.og_site_name,
            page.og_image_url,
            page.favicon_url,
            page.captured_at
        ],
    )
}

/// The source visit `id` with its URL, if it is stored and the current policy does not refuse its
/// URL; otherwise None, which is stored as NULL. NeverSave gives None, SaveButNoMcp keeps the
/// source (a future reader must judge both ends again). Without a decision, the event fails.
fn allowed_source(
    session: &mut vault::Session,
    gate: &policy_gate::PolicyGate,
    id: &str,
) -> Result<Option<(String, String)>, &'static str> {
    let url = session
        .read(|db| {
            db.query_row("SELECT url FROM visits WHERE event_id = ?1", [id], |row| {
                row.get::<_, String>(0)
            })
            .optional()
        })
        .map_err(vault::VaultError::code)?
        .map_err(|_| vault::VaultError::Db.code())?;
    match url {
        Some(url) if gate.decide(&url)? != Decision::NeverSave => Ok(Some((id.to_owned(), url))),
        _ => Ok(None),
    }
}

/// Writes the navigation of the visit `target_id`, in the visit's transaction; true when the vault
/// holds it afterwards. Only a stored visit with exactly this id and URL gets one, so an id reused
/// with another URL gets nothing (false). The source is bound to the id and URL that were judged:
/// if that visit is not in this vault, it is NULL, never a foreign-key failure. A resend changes
/// nothing (`DO NOTHING` on the target only: every other constraint still fails and rolls the
/// whole event back, unlike `INSERT OR IGNORE`).
fn store_navigation(
    db: &rusqlite::Connection,
    target_id: &str,
    url: &str,
    navigation: &Navigation,
    source: Option<&(String, String)>,
) -> rusqlite::Result<bool> {
    let (source_id, source_url) = match source {
        Some((id, url)) => (Some(id.as_str()), Some(url.as_str())),
        None => (None, None),
    };
    db.execute(
        "INSERT INTO navigation_events (target_visit_id, source_visit_id, kind, transition_type, client_redirect, server_redirect, forward_back, from_address_bar, new_tab, occurred_at)
         SELECT v.event_id, (SELECT s.event_id FROM visits s WHERE s.event_id = ?3 AND s.url = ?4), ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12
         FROM visits v WHERE v.event_id = ?1 AND v.url = ?2
         ON CONFLICT (target_visit_id) DO NOTHING",
        params![
            target_id,
            url,
            source_id,
            source_url,
            navigation.kind,
            navigation.transition_type,
            navigation.client_redirect,
            navigation.server_redirect,
            navigation.forward_back,
            navigation.from_address_bar,
            navigation.new_tab,
            navigation.occurred_at
        ],
    )?;
    db.query_row(
        "SELECT EXISTS (SELECT 1 FROM navigation_events n JOIN visits v ON v.event_id = n.target_visit_id WHERE n.target_visit_id = ?1 AND v.url = ?2)",
        params![target_id, url],
        |row| row.get(0),
    )
}

/// Visits may carry `navigation` (`navigation.rs`) and are acknowledged with `navigation`.
const CAPABILITY_NAVIGATION: &str = "navigation_v1";

/// What this host can store, for an extension to ask before it sends anything new:
///
/// ```text
/// → { type: "host_capabilities", requestId }
/// ← { ok: true, type: "host_capabilities_result", requestId, capabilities: ["navigation_v1"] }
/// ```
///
/// A host from before Issue #41 answers this as an unknown event: `{ ok: false, eventId: null,
/// error: "unknown_type" }`. Reads neither the vault nor the policy.
fn capabilities(message: &serde_json::Value) -> serde_json::Value {
    const RESULT: &str = "host_capabilities_result";
    match search::request_id(message) {
        Some(id) => serde_json::json!({
            "ok": true, "type": RESULT, "requestId": id, "capabilities": [CAPABILITY_NAVIGATION],
        }),
        None => serde_json::json!({
            "ok": false, "type": RESULT, "requestId": null, "error": "invalid_request_id",
        }),
    }
}

fn is_recordable_url(raw: &str) -> bool {
    match url::Url::parse(raw) {
        Ok(parsed) => matches!(parsed.scheme(), "http" | "https"),
        Err(_) => false,
    }
}

fn read_message() -> io::Result<Option<Vec<u8>>> {
    let mut length = [0u8; 4];
    let mut stdin = io::stdin().lock();
    if stdin.read_exact(&mut length).is_err() {
        return Ok(None);
    }
    let size = u32::from_ne_bytes(length) as usize;
    if size > 64 * 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message too large",
        ));
    }
    let mut body = vec![0u8; size];
    stdin.read_exact(&mut body)?;
    Ok(Some(body))
}

fn write_ack(ack: &Ack<'_>) -> io::Result<()> {
    write_message(ack)
}

fn write_message<T: Serialize>(message: &T) -> io::Result<()> {
    let body =
        serde_json::to_vec(message).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let mut stdout = io::stdout().lock();
    stdout.write_all(&(body.len() as u32).to_ne_bytes())?;
    stdout.write_all(&body)?;
    stdout.flush()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Double-clicked in Explorer: explain and exit before touching stdin, stdout or the vault.
    if launch::guide_if_direct() {
        return Ok(());
    }
    // The vault is resolved from config.json and opened lazily; nothing is created implicitly.
    let mut session = vault::Session::new(vault::Environment::from_process());
    // Upload state lives only as long as this process, i.e. one connectNative port: when Chrome
    // closes the port, the process exits and any unconfirmed image bytes are gone with it.
    let mut uploads = image_protocol::Uploads::default();
    // The policy is global, independent of the vault, and read again for every decision.
    let gate = policy_gate::PolicyGate::from_process();
    while let Some(body) = read_message()? {
        let value: serde_json::Value = match serde_json::from_slice(&body) {
            Ok(value) => value,
            Err(_) => {
                write_ack(&Ack::from_result(None, Err(INVALID_MESSAGE)))?;
                continue;
            }
        };
        match value.get("type").and_then(|value| value.as_str()) {
            Some("search_pages") => {
                let response = session
                    .read(|db| search::handle(db, &value))
                    .unwrap_or_else(|error| search::failure(&value, error.code()));
                write_message(&response)?;
            }
            Some(
                "image_save_begin" | "image_save_chunk" | "image_save_commit" | "image_save_abort",
            ) => {
                write_message(&uploads.handle(&mut session, &value))?;
            }
            Some("image_list" | "image_thumbnails" | "image_sources") => {
                write_message(&image_query::handle(&mut session, &value))?;
            }
            Some("image_mcp_public_set") => {
                write_message(&image_publication::handle(&mut session, &value))?;
            }
            Some("image_read" | "image_thumbnail_put" | "image_export") => {
                write_message(&image_original::handle(
                    &mut session,
                    &dialog::NativeDialogs,
                    &value,
                ))?;
            }
            Some("vault_status" | "vault_create" | "vault_open" | "vault_backup") => {
                write_message(&manage::handle(
                    &mut session,
                    &dialog::NativeDialogs,
                    &value,
                ))?;
            }
            Some("host_capabilities") => write_message(&capabilities(&value))?,
            Some("policy_status" | "policy_initialize") => {
                write_message(&gate.handle(&value))?;
            }
            Some("policy_get") => {
                write_message(&gate.get(&value))?;
            }
            Some("policy_migrate") => {
                write_message(&gate.migrate(&value))?;
            }
            Some("policy_evaluate_batch") => {
                write_message(&gate.evaluate_batch(&value))?;
            }
            Some("policy_validate") => write_message(&gate.validate(&value))?,
            Some("policy_test_rule") => write_message(&gate.test_rule(&value))?,
            Some("policy_test_url") => write_message(&gate.test_url(&value))?,
            Some("policy_domain_check") => write_message(&gate.domain_check(&value))?,
            Some("policy_domain_approvals") => write_message(&gate.domain_approvals(&value))?,
            Some("policy_domain_approve") => write_message(&gate.domain_approve(&value))?,
            Some("policy_domain_revoke") => write_message(&gate.domain_revoke(&value))?,
            Some("policy_preset") => write_message(&gate.preset(&value))?,
            Some("policy_apply") => write_message(&gate.apply(&value))?,
            Some("policy_start") => {
                // Opening the active vault creates nothing; without one, recording cannot start.
                let vault = session.read(|_| ()).map_err(vault::VaultError::code);
                write_message(&gate.start(&value, vault))?;
            }
            // `visit`, `tab_snapshot`, and anything unrecognised.
            _ => {
                let event_id = value
                    .get("id")
                    .and_then(|id| id.as_str())
                    .map(str::to_owned);
                let result = record_event(&mut session, &gate, value);
                write_ack(&Ack::from_result(event_id.as_deref(), result))?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::is_recordable_url;
    use rusqlite::{params, Connection};

    #[test]
    fn only_web_urls_are_recordable() {
        assert!(is_recordable_url("https://example.test/path"));
        assert!(is_recordable_url("http://localhost:8080"));
        assert!(!is_recordable_url("chrome://settings"));
        assert!(!is_recordable_url("file:///C:/secret.txt"));
        assert!(!is_recordable_url("not a url"));
    }

    #[test]
    fn duplicate_event_id_is_idempotent() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE visits (event_id TEXT PRIMARY KEY, url TEXT, title TEXT, visited_at TEXT);").unwrap();
        let sql = "INSERT OR IGNORE INTO visits VALUES (?1, ?2, ?3, ?4)";
        let event = [
            "event-1",
            "https://example.test",
            "Example",
            "2026-10-30T00:00:00Z",
        ];
        db.execute(sql, params![event[0], event[1], event[2], event[3]])
            .unwrap();
        db.execute(sql, params![event[0], event[1], event[2], event[3]])
            .unwrap();
        let count: i64 = db
            .query_row("SELECT count(*) FROM visits", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    fn navigation(kind: &'static str, transition_type: &'static str) -> super::Navigation {
        super::Navigation {
            source_visit_id: None,
            kind,
            transition_type,
            client_redirect: false,
            server_redirect: false,
            forward_back: false,
            from_address_bar: false,
            new_tab: false,
            occurred_at: "2026-11-06T12:00:00.000Z".to_owned(),
        }
    }

    #[test]
    fn a_navigation_the_table_refuses_fails_loudly_and_takes_its_visit_back() {
        // `ON CONFLICT (target_visit_id) DO NOTHING` absorbs a resend only: unlike `INSERT OR
        // IGNORE`, a CHECK failure is an error, and the visit written before it is rolled back.
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch(&memocampus_native_host::schema::create_sql())
            .unwrap();
        db.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        for bad in [
            navigation("fragment", "link"),
            navigation("document", "auto_toplevel"),
        ] {
            let tx = db.transaction().unwrap();
            tx.execute("INSERT INTO visits VALUES ('t', 'https://a.test/', '', '2026-11-06T12:00:00.000Z')", []).unwrap();
            assert!(super::store_navigation(&tx, "t", "https://a.test/", &bad, None).is_err());
            drop(tx);
            let rows: i64 = db
                .query_row("SELECT (SELECT count(*) FROM visits) + (SELECT count(*) FROM navigation_events)", [], |row| row.get(0))
                .unwrap();
            assert_eq!(rows, 0);
        }
        // The same insert with allowed values goes in, and again changes nothing.
        let tx = db.transaction().unwrap();
        tx.execute(
            "INSERT INTO visits VALUES ('t', 'https://a.test/', '', '2026-11-06T12:00:00.000Z')",
            [],
        )
        .unwrap();
        let good = navigation("document", "link");
        assert!(super::store_navigation(&tx, "t", "https://a.test/", &good, None).unwrap());
        assert!(super::store_navigation(&tx, "t", "https://a.test/", &good, None).unwrap());
        // Another URL under the same id is never "stored".
        assert!(!super::store_navigation(&tx, "t", "https://b.test/", &good, None).unwrap());
        tx.commit().unwrap();
        let rows: i64 = db
            .query_row("SELECT count(*) FROM navigation_events", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[test]
    fn the_write_keeps_a_source_only_as_it_was_judged() {
        // The source was judged by id and URL before the transaction. If the vault written to has no
        // visit with exactly that id and URL (another vault became active meanwhile), it is NULL:
        // never another visit's id, and never a foreign-key failure.
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch(&memocampus_native_host::schema::create_sql())
            .unwrap();
        db.execute_batch(
            "PRAGMA foreign_keys = ON;
             INSERT INTO visits VALUES ('s', 'https://s.test/other', '', '2026-11-06T12:00:00.000Z');",
        )
        .unwrap();
        let tx = db.transaction().unwrap();
        let stored = |target: &str, source: (&str, &str)| -> Option<String> {
            tx.execute(
                "INSERT INTO visits VALUES (?1, 'https://t.test/', '', '2026-11-06T12:00:00.000Z')",
                [target],
            )
            .unwrap();
            let judged = (source.0.to_owned(), source.1.to_owned());
            let nav = navigation("document", "link");
            assert!(
                super::store_navigation(&tx, target, "https://t.test/", &nav, Some(&judged))
                    .unwrap()
            );
            tx.query_row(
                "SELECT source_visit_id FROM navigation_events WHERE target_visit_id = ?1",
                [target],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert_eq!(stored("t1", ("s", "https://s.test/judged")), None);
        assert_eq!(stored("t2", ("gone", "https://s.test/other")), None);
        assert_eq!(
            stored("t3", ("s", "https://s.test/other")).as_deref(),
            Some("s")
        );
    }

    #[test]
    fn a_source_the_policy_cannot_judge_stops_the_event_and_an_unknown_one_is_null() {
        use crate::vault::{self, tests::TempDir, Environment, Session};
        use memocampus_native_host::policy_store::PolicyStore;
        let tmp = TempDir::new();
        let path = tmp.0.join("vault.sqlite3");
        vault::create_vault(&path).unwrap();
        Connection::open(&path)
            .unwrap()
            .execute("INSERT INTO visits VALUES ('s', 'https://a.test/', '', '2026-11-06T12:00:00.000Z')", [])
            .unwrap();
        let mut session = Session::new(Environment {
            app_dir: Some(tmp.0.join("app")),
            db_override: Some(path),
            exe_dir: None,
        });
        // A directory without a policy: no decision exists.
        std::fs::create_dir_all(tmp.0.join("policy")).unwrap();
        let gate = crate::policy_gate::PolicyGate::with_store(
            PolicyStore::new(tmp.0.join("policy")).unwrap(),
        );
        assert_eq!(
            super::allowed_source(&mut session, &gate, "s"),
            Err("policy_not_initialized")
        );
        // A visit that is not stored has no URL to judge: NULL, whatever the policy.
        assert_eq!(
            super::allowed_source(&mut session, &gate, "missing"),
            Ok(None)
        );
    }
}
