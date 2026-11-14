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
mod image_tags;
mod images;
mod launch;
mod location;
mod manage;
mod page_manage;
mod page_protocol;
mod pages;
mod policy_gate;
#[cfg(test)]
mod read_token_tests;
mod search;
mod vault;
mod vault_items;
mod visit_days;

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
            if is_recordable_url(url) {
                let decision = if kind == Some("visit") {
                    gate.decide_visit(
                        url,
                        value.get("recordingOccurredAt").and_then(|at| at.as_str()),
                    )?
                } else {
                    gate.decide(url)?
                };
                if decision == Decision::NeverSave {
                    return Ok(Recorded::DroppedByPolicy);
                }
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

/// One `search_pages` answer. A successful page names its vault (`vaultToken`, as `image_list` does),
/// so a later `url_delete` can only reach the vault the results came from (Issue #43). The token
/// and the rows come from one resolution of the vault (`Session::read_with_token`): never one
/// vault's token with another's rows.
fn search_response(session: &mut vault::Session, value: &serde_json::Value) -> serde_json::Value {
    session
        .read_with_token(|db| search::handle(db, value))
        .map(|(token, mut response)| {
            if response["ok"] == true {
                response["vaultToken"] = token.into();
            }
            response
        })
        .unwrap_or_else(|error| search::failure(value, error.code()))
}

/// One `visit_days` answer (Issue #5). The bounds are checked before any vault is opened; the
/// counts and their token come from one resolution of the vault, as for `search_pages`. A failed
/// read is a failure, never counts.
fn visit_days_response(
    session: &mut vault::Session,
    value: &serde_json::Value,
) -> serde_json::Value {
    let bounds = match search::request_id(value)
        .ok_or("invalid_request_id")
        .and_then(|_| visit_days::parse_request(value))
    {
        Ok(bounds) => bounds,
        Err(code) => return visit_days::failure(value, code),
    };
    match session.read_with_token(|db| visit_days::count_visits(db, &bounds)) {
        Ok((token, Ok(counts))) => visit_days::success(value, token, counts),
        Ok((_, Err(_))) => visit_days::failure(value, "db_error"),
        Err(error) => visit_days::failure(value, error.code()),
    }
}

/// Visits may carry `navigation` (`navigation.rs`) and are acknowledged with `navigation`.
const CAPABILITY_NAVIGATION: &str = "navigation_v1";
/// `policy_item_*`, `url_delete*` and `image_delete*` (Issue #43; `vault_items.rs`,
/// `policy_gate/items.rs`).
const CAPABILITY_VAULT_ITEMS: &str = "vault_items_v1";
/// `visit_days`, and the date range of `search_pages` (Issue #5; `visit_days.rs`, `search.rs`).
const CAPABILITY_VISIT_CALENDAR: &str = "visit_calendar_v1";

/// What this host can store, for an extension to ask before it sends anything new:
///
/// ```text
/// → { type: "host_capabilities", requestId }
/// ← { ok: true, type: "host_capabilities_result", requestId, capabilities: ["navigation_v1", "vault_items_v1", "visit_calendar_v1"] }
/// ```
///
/// A host from before Issue #41 answers this as an unknown event: `{ ok: false, eventId: null,
/// error: "unknown_type" }`. Reads neither the vault nor the policy.
fn capabilities(message: &serde_json::Value) -> serde_json::Value {
    const RESULT: &str = "host_capabilities_result";
    match search::request_id(message) {
        Some(id) => serde_json::json!({
            "ok": true, "type": RESULT, "requestId": id, "capabilities": [CAPABILITY_NAVIGATION, CAPABILITY_VAULT_ITEMS, CAPABILITY_VISIT_CALENDAR],
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

fn read_message(max_size: usize) -> io::Result<Option<Vec<u8>>> {
    let mut length = [0u8; 4];
    let mut stdin = io::stdin().lock();
    if stdin.read_exact(&mut length).is_err() {
        return Ok(None);
    }
    let size = u32::from_ne_bytes(length) as usize;
    if size > max_size {
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

/// Whether Chrome still holds the other end of stdin. Asked after a file dialog closes: the main
/// thread reads nothing while a request is handled, so a closed port is otherwise unnoticed.
#[cfg(windows)]
fn port_open() -> bool {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_BROKEN_PIPE};
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_INPUT_HANDLE};
    use windows_sys::Win32::System::Pipes::PeekNamedPipe;
    let none = std::ptr::null_mut::<u32>();
    // SAFETY: no buffer is passed; only the pipe's state is queried.
    unsafe {
        let stdin = GetStdHandle(STD_INPUT_HANDLE);
        PeekNamedPipe(stdin, std::ptr::null_mut(), 0, none, none, none) != 0
            || GetLastError() != ERROR_BROKEN_PIPE
    }
}

#[cfg(not(windows))]
fn port_open() -> bool {
    true
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Double-clicked in Explorer: explain and exit before touching stdin, stdout or the vault.
    if launch::guide_if_direct() {
        return Ok(());
    }
    // The vault is resolved from config.json and opened lazily; nothing is created implicitly.
    let mut session = vault::Session::new(vault::Environment::from_process());
    // A config from before Issue #51 becomes version 2 once (PO decision P3). It never waits for a
    // lock and never touches a vault's contents; whatever it cannot decide now is retried later.
    let _ = vault::migrate_legacy_config(&session.env, || manage::policy_readiness(&session.env));
    // Upload state lives only as long as this process, i.e. one connectNative port: when Chrome
    // closes the port, the process exits and any unconfirmed image bytes are gone with it.
    let mut uploads = image_protocol::Uploads::default();
    let mut page_uploads = page_protocol::Uploads::default();
    let mut page_manager = page_manage::Manager::default();
    // One read permit at a time: no speculative allocations while a Page port is waiting. The
    // main thread expires an abandoned Page operation even if stdin remains open forever.
    let (permit, permits) = std::sync::mpsc::sync_channel(0);
    let (reader, messages) = std::sync::mpsc::sync_channel(0);
    std::thread::spawn(move || {
        while let Ok(limit) = permits.recv() {
            let message = read_message(limit);
            let done = !matches!(&message, Ok(Some(_)));
            if reader.send(message).is_err() || done {
                break;
            }
        }
    });
    let mut page_port = false;
    // The policy is global, independent of the vault, and read again for every decision.
    let gate = policy_gate::PolicyGate::from_process();
    loop {
        permit.send(if page_port {
            page_protocol::MAX_MESSAGE_BYTES
        } else {
            64 * 1024 * 1024
        })?;
        let incoming = if page_uploads.active() {
            match messages.recv_timeout(page_protocol::IDLE_TIMEOUT) {
                Ok(value) => value,
                Err(_) => break,
            }
        } else {
            match messages.recv() {
                Ok(value) => value,
                Err(_) => break,
            }
        };
        let Some(body) = incoming? else {
            break;
        };
        let value: serde_json::Value = match serde_json::from_slice(&body) {
            Ok(value) => value,
            Err(_) => {
                if page_port {
                    break;
                } // malformed Page input discards the upload, not just a frame
                write_ack(&Ack::from_result(None, Err(INVALID_MESSAGE)))?;
                continue;
            }
        };
        let message_type = value.get("type").and_then(|value| value.as_str());
        if message_type.is_some_and(|kind| kind.starts_with("page_save_")) {
            page_port = true;
            write_message(&page_uploads.handle(&mut session, &value))?;
            continue;
        }
        if page_port {
            break;
        }
        match message_type {
            Some(
                "page_state"
                | "page_list"
                | "page_history"
                | "page_delete_preview"
                | "page_delete"
                | "page_delete_status",
            ) => write_message(&page_manager.handle(&mut session, &value))?,
            Some("page_export") => write_message(&page_manager.export(
                &mut session,
                &dialog::NativeDialogs,
                &port_open,
                &value,
            ))?,
            Some("search_pages") => write_message(&search_response(&mut session, &value))?,
            Some("visit_days") => write_message(&visit_days_response(&mut session, &value))?,
            Some(
                "image_save_begin" | "image_save_chunk" | "image_save_commit" | "image_save_abort",
            ) => {
                write_message(&uploads.handle(&mut session, &value))?;
            }
            Some("image_list" | "image_thumbnails" | "image_sources") => {
                write_message(&image_query::handle(&mut session, &value))?;
            }
            Some(
                "image_tags_get" | "image_tag_add" | "image_tag_remove" | "image_tag_list"
                | "image_tag_search",
            ) => {
                write_message(&image_tags::handle(&mut session, &value))?;
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
            Some(
                "vault_status"
                | "vault_create"
                | "vault_open"
                | "vault_backup"
                | "vault_publication_confirm"
                | "vault_publication_revoke"
                | "vault_create_encrypted"
                | "vault_create_encrypted_commit"
                | "vault_create_encrypted_cancel"
                | "vault_recover"
                | "vault_restore_inspect"
                | "vault_restore"
                | "vault_restore_activate"
                | "vault_restore_discard"
                | "vault_convert_start"
                | "vault_convert_confirm"
                | "vault_convert_activate"
                | "vault_convert_keep"
                | "vault_convert_discard",
            ) => {
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
            Some("policy_item_preview") => write_message(&gate.item_preview(&value))?,
            Some("policy_item_apply") => write_message(&gate.item_apply(&value))?,
            Some("url_delete_preview" | "url_delete" | "image_delete_preview" | "image_delete") => {
                write_message(&vault_items::handle(&mut session, &value))?;
            }
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

    // --- One vault per answer (Issue #5 review): the token and the data of a read ------------------

    mod one_vault_per_answer {
        use crate::vault::{
            self, create_vault, switch_active, tests::TempDir, vault_token, ConfigLock,
            Environment, Session,
        };
        use rusqlite::Connection;
        use serde_json::{json, Value};
        use std::path::{Path, PathBuf};

        /// November 2026 in Tokyo, as the extension sends it.
        fn bounds() -> Vec<String> {
            std::iter::once("2026-10-31T15:00:00.000Z".to_owned())
                .chain((1..=30).map(|day| format!("2026-11-{day:02}T15:00:00.000Z")))
                .collect()
        }

        fn search() -> Value {
            json!({ "type": "search_pages", "requestId": "s" })
        }

        fn days() -> Value {
            json!({ "type": "visit_days", "requestId": "d", "bounds": bounds() })
        }

        /// Vaults A (one visit on Nov 3 in Tokyo) and B (two visits on Nov 20), A active.
        struct Two {
            _tmp: TempDir,
            env: Environment,
            a: PathBuf,
            b: PathBuf,
        }

        fn activate(env: &Environment, path: &Path) {
            let lock = ConfigLock::acquire(env).unwrap();
            switch_active(env, &lock, path, true).unwrap();
        }

        fn two() -> Two {
            let tmp = TempDir::new();
            let env = tmp.env();
            let (a, b) = (tmp.0.join("a.sqlite3"), tmp.0.join("b.sqlite3"));
            for (path, rows) in [
                (
                    &a,
                    vec![("a1", "https://a.test/", "2026-11-03T01:00:00.000Z")],
                ),
                (
                    &b,
                    vec![
                        ("b1", "https://b.test/", "2026-11-20T01:00:00.000Z"),
                        ("b2", "https://b.test/", "2026-11-20T02:00:00.000Z"),
                    ],
                ),
            ] {
                create_vault(path).unwrap();
                let db = Connection::open(path).unwrap();
                for (id, url, at) in rows {
                    db.execute("INSERT INTO visits VALUES (?1, ?2, 't', ?3)", [id, url, at])
                        .unwrap();
                }
            }
            activate(&env, &a);
            Two {
                _tmp: tmp,
                env,
                a,
                b,
            }
        }

        fn token(path: &Path) -> String {
            vault_token(path).unwrap()
        }

        fn urls(answer: &Value) -> Vec<String> {
            answer["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["url"].as_str().unwrap().to_owned())
                .collect()
        }

        fn days_with(answer: &Value) -> Vec<(usize, u64)> {
            answer["counts"]
                .as_array()
                .unwrap()
                .iter()
                .enumerate()
                .filter(|(_, count)| count.as_u64() != Some(0))
                .map(|(index, count)| (index + 1, count.as_u64().unwrap()))
                .collect()
        }

        /// A session whose next read sees the active vault switched to `to` by another process,
        /// right after it resolved and opened its vault: the window of the finding.
        fn switching(t: &Two, to: &Path) -> Session {
            let mut session = Session::new(t.env.clone());
            let (env, to) = (t.env.clone(), to.to_owned());
            let mut done = false;
            session.after_open = Some(Box::new(move || {
                if !done {
                    done = true;
                    activate(&env, &to);
                }
            }));
            session
        }

        #[test]
        fn the_window_is_real_for_a_ticket_then_a_read() {
            // Why `read_with_token` exists: resolving twice pairs one vault's token with the other's
            // data when the switch falls between the two.
            let t = two();
            let mut session = switching(&t, &t.b);
            let ticket = session.ticket().unwrap();
            let rows = session
                .read(|db| super::super::search::handle(db, &search()))
                .unwrap();
            assert_eq!(ticket.token, token(&t.a));
            assert_eq!(urls(&rows), ["https://b.test/"], "B's rows under A's token");
        }

        #[test]
        fn search_pages_names_the_vault_it_read_even_when_switched_in_the_window() {
            let t = two();
            let mut session = switching(&t, &t.b);
            let first = super::super::search_response(&mut session, &search());
            assert_eq!(urls(&first), ["https://a.test/"]);
            assert_eq!(first["vaultToken"], token(&t.a), "A's rows, A's token");
            // The switch counts from the next request on: B's rows with B's token.
            let second = super::super::search_response(&mut session, &search());
            assert_eq!(urls(&second), ["https://b.test/"]);
            assert_eq!(second["vaultToken"], token(&t.b));
            // And back, the other way round.
            let mut session = switching(&t, &t.a);
            let third = super::super::search_response(&mut session, &search());
            assert_eq!(
                (urls(&third), third["vaultToken"].clone()),
                (vec!["https://b.test/".to_owned()], json!(token(&t.b)))
            );
        }

        #[test]
        fn visit_days_names_the_vault_it_counted_even_when_switched_in_the_window() {
            let t = two();
            let mut session = switching(&t, &t.b);
            let first = super::super::visit_days_response(&mut session, &days());
            assert_eq!(days_with(&first), [(3, 1)]);
            assert_eq!(first["vaultToken"], token(&t.a));
            let second = super::super::visit_days_response(&mut session, &days());
            assert_eq!(days_with(&second), [(20, 2)]);
            assert_eq!(second["vaultToken"], token(&t.b));
        }

        #[test]
        fn a_read_that_fails_in_the_window_is_a_failure_without_a_token() {
            // Another connection removes the table the read needs, after the vault was opened.
            let t = two();
            for (request, respond) in [
                (
                    days(),
                    super::super::visit_days_response as fn(&mut Session, &Value) -> Value,
                ),
                (search(), super::super::search_response),
            ] {
                let t2 = two();
                let mut session = Session::new(t2.env.clone());
                let a = t2.a.clone();
                session.after_open = Some(Box::new(move || {
                    Connection::open(&a)
                        .unwrap()
                        .execute_batch("PRAGMA foreign_keys = OFF; DROP TABLE navigation_events; DROP TABLE page_metadata; DROP TABLE visits;")
                        .unwrap();
                }));
                let answer = respond(&mut session, &request);
                assert_eq!(answer["ok"], false, "{answer}");
                assert_eq!(answer["error"], "db_error");
                assert!(
                    answer.get("vaultToken").is_none()
                        && answer.get("counts").is_none()
                        && answer.get("items").is_none()
                );
            }
            drop(t);
        }

        #[test]
        fn no_vault_or_a_missing_one_is_the_error_it_was() {
            let tmp = TempDir::new();
            let mut session = Session::new(tmp.env());
            for (answer, kind) in [
                (
                    super::super::visit_days_response(&mut session, &days()),
                    "visit_days_result",
                ),
                (
                    super::super::search_response(&mut session, &search()),
                    "search_pages_result",
                ),
            ] {
                assert_eq!(
                    answer,
                    json!({ "ok": false, "type": kind, "requestId": answer["requestId"], "error": "vault_not_configured" })
                );
            }
            let t = two();
            let mut session = Session::new(t.env.clone());
            std::fs::remove_file(&t.a).unwrap();
            for answer in [
                super::super::visit_days_response(&mut session, &days()),
                super::super::search_response(&mut session, &search()),
            ] {
                assert_eq!(answer["ok"], false);
                assert_eq!(answer["error"], "vault_missing");
                assert!(answer.get("vaultToken").is_none());
            }
            // Bounds are still judged before any vault.
            let refused = super::super::visit_days_response(
                &mut session,
                &json!({ "type": "visit_days", "requestId": "d", "bounds": [] }),
            );
            assert_eq!(refused["error"], "invalid_bounds");
        }

        #[test]
        fn a_write_after_the_switch_still_goes_by_the_lock_of_the_new_vault() {
            // `read_with_token` changes nothing of writes: a ticket of A (B became active right
            // after it) is refused, while the next read is B's rows with B's token.
            let t = two();
            let mut session = switching(&t, &t.b);
            let ticket = session.ticket().unwrap();
            assert_eq!(ticket.token, token(&t.a));
            let read = super::super::search_response(&mut session, &search());
            assert_eq!(urls(&read), ["https://b.test/"]);
            assert_eq!(read["vaultToken"], token(&t.b));
            let refused = session.write_for(&ticket, |_| Ok::<_, vault::VaultError>(()));
            assert_eq!(refused, Err(vault::VaultError::Switched));
        }
    }
}
