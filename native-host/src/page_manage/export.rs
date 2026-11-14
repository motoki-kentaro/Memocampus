//! Issue #6 Slice 3: one saved Page's MHTML original, written unchanged to a file the user picks
//! in the OS Save dialog. Nothing goes back to Chrome but the outcome; nothing is added to the
//! Vault, VisibleStore or MCP. No lock, transaction or connection is held while the dialog is open.
use super::{event, Binding, Error, Event, Manager, Result, Target, COLUMNS};
use crate::{
    dialog::Dialogs,
    page_protocol, pages,
    search::request_id,
    vault::{ConfigLock, Session},
};
use rusqlite::{Connection, DatabaseName, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use zeroize::Zeroizing;

mod file;
#[cfg(test)]
mod tests;

const KIND: &str = "page_export";
const FALLBACK_STEM: &str = "memocampus-page";
/// UTF-16 units of the proposed name without `.mhtml`; far below MAX_PATH components (255).
const MAX_STEM_UNITS: usize = 120;

// Test-only fault/race injection at named stages. Production builds contain no hook.
#[cfg(test)]
type Hook = Box<dyn FnMut(&'static str, u64) -> bool>;
#[cfg(test)]
thread_local! {
    pub(super) static HOOK: std::cell::RefCell<Option<Hook>> = const { std::cell::RefCell::new(None) };
}
/// `true`: the test asks for a failure at this stage.
fn fault(_stage: &'static str, _index: u64) -> bool {
    #[cfg(test)]
    {
        let hook = HOOK.with(|slot| slot.borrow_mut().take());
        if let Some(mut hook) = hook {
            let failed = hook(_stage, _index);
            HOOK.with(|slot| {
                slot.borrow_mut().get_or_insert(hook);
            });
            return failed;
        }
    }
    false
}

/// What the request names. All three must match the stored row, so a reused row id, another
/// vault's equal id or a replaced original is never exported in its place.
struct Wanted {
    id: i64,
    save_id: String,
    sha256: String,
}
impl Wanted {
    fn parse(message: &Value) -> Result<Self> {
        let Target::Event { id } =
            Target::checked(&json!({"scope":"event","id":message.get("id")}))?
        else {
            return Err(Error("invalid_target"));
        };
        let text = |key: &str| message.get(key).and_then(Value::as_str);
        let save_id = text("saveId").filter(|id| pages::valid_id(id));
        let sha256 = text("sha256").filter(|sha| {
            sha.len() == 64 && sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        });
        match (save_id, sha256) {
            (Some(save_id), Some(sha256)) => Ok(Self {
                id,
                save_id: save_id.into(),
                sha256: sha256.into(),
            }),
            _ => Err(Error("invalid_target")),
        }
    }
    /// Metadata only; never selects `page_blobs.data`.
    fn find(&self, db: &Connection) -> Result<Event> {
        let found = db
            .query_row(
                &format!("SELECT {COLUMNS} FROM saved_pages p JOIN page_blobs b ON b.id=p.blob_id WHERE p.id=?1"),
                [self.id],
                event,
            )
            .optional()?
            .ok_or(Error("page_not_found"))?;
        if found.save_id != self.save_id || found.sha256 != self.sha256 {
            return Err(Error("page_export_changed"));
        }
        Ok(found)
    }
}

/// Invisible formatting characters (bidi overrides, zero width, tags) are dropped from a name.
fn invisible(c: char) -> bool {
    matches!(c, '\u{ad}' | '\u{61c}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}'
        | '\u{2060}'..='\u{2069}' | '\u{feff}' | '\u{fff9}'..='\u{fffb}' | '\u{e0000}'..='\u{e007f}')
}
/// Windows device names, with or without extensions (`CON`, `nul.txt`, `COM1.mhtml`).
pub(super) fn reserved(name: &str) -> bool {
    let base = name.split('.').next().unwrap_or("").trim_end_matches(' ');
    let base = base.to_uppercase();
    if matches!(
        base.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return true;
    }
    let mut chars = base.chars();
    let prefix: String = chars.by_ref().take(3).collect();
    matches!(prefix.as_str(), "COM" | "LPT")
        && matches!(
            (chars.next(), chars.next()),
            (Some('0'..='9' | '¹' | '²' | '³'), None)
        )
}
/// A page without a title shows its URL as the title; that must not become a file name (query
/// strings, credentials).
fn url_like(title: &str, url: &str) -> bool {
    let bare = |s: &str| {
        s.split_once("://")
            .map_or(s, |(_, rest)| rest)
            .trim_end_matches('/')
            .to_lowercase()
    };
    title.contains("://") || bare(title) == bare(url)
}
/// Proposed name for the Save dialog: from the title at save time only, never from the URL.
pub(super) fn file_name(title: &str, url: &str) -> String {
    let stem = stem(title, url).unwrap_or_else(|| FALLBACK_STEM.into());
    format!("{stem}.mhtml")
}
fn stem(title: &str, url: &str) -> Option<String> {
    if url_like(title.trim(), url) {
        return None;
    }
    let mut text = String::new();
    for c in title.chars().filter(|c| !invisible(*c)) {
        if c.is_control() || c.is_whitespace() {
            if !text.ends_with(' ') {
                text.push(' ');
            }
        } else if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
            text.push('_');
        } else {
            text.push(c);
        }
    }
    let edge = |c: char| c == ' ' || c == '.';
    let mut text = text.trim_end_matches(edge);
    // A title that already ends in the extension must not produce `x.mhtml.mhtml`.
    loop {
        let lower = text.to_ascii_lowercase();
        match [".mhtml", ".mht"].iter().find(|ext| lower.ends_with(*ext)) {
            Some(ext) => text = text[..text.len() - ext.len()].trim_end_matches(edge),
            None => break,
        }
    }
    let text = text.trim_start_matches(edge);
    let mut units = 0;
    let end = text
        .char_indices()
        .find(|(_, c)| {
            units += c.len_utf16();
            units > MAX_STEM_UNITS
        })
        .map_or(text.len(), |(index, _)| index);
    let text = text[..end].trim_matches(edge);
    if text.is_empty() {
        None
    } else if reserved(text) {
        Some(format!("_{text}"))
    } else {
        Some(text.to_owned())
    }
}

/// Streams the original with incremental BLOB I/O in `CHUNK_BYTES` pieces; the whole original is
/// never in memory. The bytes written must be exactly the recorded length and SHA-256.
fn stream(db: &Connection, target: &Event, out: &mut impl Write) -> Result<()> {
    let length = usize::try_from(target.byte_length).map_err(|_| Error("page_blob_corrupt"))?;
    let mut blob = db.blob_open(
        DatabaseName::Main,
        "page_blobs",
        "data",
        target.blob_id,
        true,
    )?;
    if pages::validate_size(length).is_err() || blob.len() != length {
        return Err(Error("page_blob_corrupt"));
    }
    let mut buffer = Zeroizing::new(vec![0u8; pages::CHUNK_BYTES]);
    let (mut hash, mut total, mut chunk) = (Sha256::new(), 0usize, 0u64);
    loop {
        let n = blob.read(&mut buffer).map_err(|_| Error("db_error"))?;
        if n == 0 {
            break;
        }
        total += n;
        hash.update(&buffer[..n]);
        if fault("stream", chunk) || out.write_all(&buffer[..n]).is_err() {
            return Err(Error("export_failed"));
        }
        chunk += 1;
    }
    if total != length || format!("{:x}", hash.finalize()) != target.sha256 {
        return Err(Error("page_blob_corrupt"));
    }
    Ok(())
}

impl Manager {
    /// `alive`: whether the port that asked is still open. A request whose port closed while the
    /// dialog was open (panel closed, Vault changed in the UI) writes nothing.
    pub fn export(
        &mut self,
        session: &mut Session,
        dialogs: &dyn Dialogs,
        alive: &dyn Fn() -> bool,
        message: &Value,
    ) -> Value {
        let id = request_id(message);
        let result = if id.is_none() {
            Err(Error("invalid_request_id"))
        } else {
            self.run_export(session, dialogs, alive, message)
        };
        self.reply(KIND, id, result)
    }
    fn run_export(
        &mut self,
        session: &mut Session,
        dialogs: &dyn Dialogs,
        alive: &dyn Fn() -> bool,
        message: &Value,
    ) -> Result<Value> {
        let wanted = Wanted::parse(message)?;
        // 1. Fix the event and the Vault generation under the config lock, then let go of
        //    everything: the dialog may stay open for any length of time.
        let (binding, target) = {
            let _lock = ConfigLock::acquire(&session.env)?;
            let (ticket, binding) = self.bind(session, message, KIND)?;
            let target = session.read_for(&ticket, |db| wanted.find(db))?;
            (binding, target)
        };
        session.reset();
        let answer = |mut value: Value, binding: &Binding| {
            value["ok"] = json!(true);
            value["id"] = json!(target.id.to_string());
            value["vaultToken"] = json!(binding.vault_token);
            value["generation"] = json!(binding.generation);
            value
        };
        // 2. The OS dialog picks the place and asks about overwriting. Cancel changes nothing.
        let Some(path) = dialogs.choose_page_export_file(&file_name(&target.title, &target.url))
        else {
            return Ok(answer(json!({"cancelled":true}), &binding));
        };
        // 3. Pin the folder and any file the user just agreed to replace, before anything else.
        let destination = file::Destination::observe(&path).map_err(Error)?;
        if !alive() {
            return Err(Error("host_unavailable"));
        }
        // 4. Same Vault selection (A→B→A is a new generation), key and schema, and the same event.
        //    The config lock excludes a selection change until the file is in place; the read
        //    transaction is one snapshot of the event and its original.
        let _lock = ConfigLock::acquire(&session.env)?;
        let (ticket, now) = self.bind(session, message, KIND)?;
        if now != binding {
            return Err(Error("vault_switched"));
        }
        let env = session.env.clone();
        let outcome = session.read_for(&ticket, |db| {
            let tx = db.unchecked_transaction()?;
            if wanted.find(&tx)? != target {
                return Err(Error("page_export_changed"));
            }
            let mut temp = destination.create_temp().map_err(Error)?;
            stream(&tx, &target, &mut temp)?;
            // Last look before the final path changes: selection generation and event.
            if page_protocol::generation_for(&env).map_err(Error)? != binding.generation {
                return Err(Error("vault_switched"));
            }
            if wanted.find(&tx)? != target {
                return Err(Error("page_export_changed"));
            }
            let outcome = destination
                .install(temp, &target.sha256, target.byte_length as u64)
                .map_err(Error)?;
            tx.commit()?;
            Ok::<_, Error>(outcome)
        });
        session.reset();
        let outcome = outcome?;
        // Only the file name goes back to the extension, never the folder.
        Ok(answer(
            json!({"exported":true,"fileName":outcome.file_name,"byteLength":target.byte_length,
                "sha256":target.sha256,"previousFileLeft":outcome.previous_left}),
            &binding,
        ))
    }
}
