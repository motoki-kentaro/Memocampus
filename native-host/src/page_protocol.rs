//! Page-only, bounded in-memory transfer. No persistent retry queue, plaintext spool or MCP path.
use crate::{
    pages::{self, Metadata},
    vault::{self, ConfigLock, Session, VaultTicket},
};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use zeroize::Zeroizing;

pub const MAX_MESSAGE_BYTES: usize = 2 * 1024 * 1024;
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(120);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(15 * 60);

struct Upload {
    _lock: File,
    ticket: VaultTicket,
    generation: String,
    save_id: String,
    meta: Metadata,
    size: Option<usize>,
    bytes: Zeroizing<Vec<u8>>,
    next: u64,
    hash: Sha256,
    prepared: Option<String>,
    opened: Instant,
}

#[derive(Default)]
pub struct Uploads {
    active: Option<Upload>,
}

fn string<'a>(message: &'a Value, key: &str) -> Result<&'a str, &'static str> {
    message
        .get(key)
        .and_then(Value::as_str)
        .ok_or("invalid_message")
}
fn size(message: &Value) -> Result<usize, &'static str> {
    let size = message
        .get("byteLength")
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .ok_or("invalid_page_size")?;
    pages::validate_size(size)?;
    Ok(size)
}
fn metadata(message: &Value) -> Result<Metadata, &'static str> {
    let meta: Metadata =
        serde_json::from_value(message.get("metadata").cloned().ok_or("invalid_metadata")?)
            .map_err(|_| "invalid_metadata")?;
    pages::validate(&meta)?;
    Ok(meta)
}
pub(crate) fn generation(session: &Session) -> Result<String, &'static str> {
    generation_for(&session.env)
}
pub(crate) fn generation_for(env: &vault::Environment) -> Result<String, &'static str> {
    match vault::resolve(env).map_err(vault::VaultError::code)? {
        vault::Active::Configured(config) => {
            config.selection_generation.ok_or("vault_reselect_required")
        }
        vault::Active::Override(_) => Ok("override-fixed-for-process".into()),
        vault::Active::NotConfigured => Err("vault_not_configured"),
    }
}
fn check(session: &mut Session, ticket: &VaultTicket, expected: &str) -> Result<(), &'static str> {
    if generation(session)? != expected {
        return Err("vault_switched");
    }
    // Fresh mode/key/schema check each step (no cached decrypted connection after a lock).
    session
        .read_for(ticket, |_| Ok::<_, vault::VaultError>(()))
        .map_err(vault::VaultError::code)
}
fn saved(receipt: pages::Receipt, id: &str, token: &str, generation: &str) -> Value {
    let mut value = serde_json::to_value(receipt).expect("receipt serializes");
    let obj = value.as_object_mut().unwrap();
    obj.insert("type".into(), json!("page_saved"));
    obj.insert("ok".into(), json!(true));
    obj.insert("saveId".into(), json!(id));
    obj.insert("vaultToken".into(), json!(token));
    obj.insert("generation".into(), json!(generation));
    value
}

impl Uploads {
    pub fn active(&self) -> bool {
        self.active.is_some()
    }
    pub fn handle(&mut self, session: &mut Session, message: &Value) -> Value {
        let result = self.step(session, message);
        match result {
            Ok(value) => value,
            Err(error) => {
                self.active = None; // includes the OS lock and zeroized buffer
                json!({"ok": false, "type": "page_save_error", "saveId": message.get("saveId").and_then(Value::as_str).filter(|id| pages::valid_id(id)), "error": error})
            }
        }
    }
    fn step(&mut self, session: &mut Session, message: &Value) -> Result<Value, &'static str> {
        let id = string(message, "saveId")?;
        if !pages::valid_id(id) {
            return Err("invalid_save_id");
        }
        let kind = string(message, "type")?;
        // Short config lock, same ordering as management (config → SQLite). No lock during capture
        // or transfer waits. Every read/check and COMMIT is serialized against selection changes.
        let _config = ConfigLock::acquire(&session.env).map_err(vault::VaultError::code)?;
        if kind == "page_save_open" {
            if self.active.is_some() {
                return Err("upload_in_progress");
            }
            if message.get("protocol").and_then(Value::as_u64) != Some(1) {
                return Err("unsupported_protocol");
            }
            let meta = metadata(message)?;
            let dir = session
                .env
                .config_path()
                .map_err(vault::VaultError::code)?
                .parent()
                .ok_or("app_dir_unavailable")?
                .to_owned();
            let lock = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(dir.join("page-upload.lock"))
                .map_err(|_| "io_error")?;
            lock.try_lock().map_err(|_| "page_upload_busy")?;
            let ticket = session.ticket().map_err(vault::VaultError::code)?;
            let generation = generation(session)?;
            check(session, &ticket, &generation)?;
            let response = json!({"ok": true,"type":"page_save_opened","saveId":id,"vaultToken":ticket.token,"generation":generation,"chunkBytes":pages::CHUNK_BYTES,"maxBytes":pages::MAX_PAGE_BYTES});
            self.active = Some(Upload {
                _lock: lock,
                ticket,
                generation,
                save_id: id.into(),
                meta,
                size: None,
                bytes: Zeroizing::new(Vec::new()),
                next: 0,
                hash: Sha256::new(),
                prepared: None,
                opened: Instant::now(),
            });
            return Ok(response);
        }
        if kind == "page_save_status" {
            if self.active.is_some() {
                return Err("upload_in_progress");
            }
            let meta = metadata(message)?;
            let size = size(message)?;
            let gen = string(message, "generation")?;
            let ticket = session.ticket().map_err(vault::VaultError::code)?;
            if ticket.token != string(message, "vaultToken")? {
                return Err("vault_switched");
            }
            check(session, &ticket, gen)?;
            let sha = string(message, "sha256")?;
            let receipt = session
                .read_for(&ticket, |db| {
                    pages::lookup(db, id, &meta, sha, size).map_err(PageError)
                })
                .map_err(|e: PageError| e.0)?;
            return Ok(match receipt {
                Some(r) => saved(r, id, &ticket.token, gen),
                None => {
                    json!({"ok":true,"type":"page_save_unknown","saveId":id,"vaultToken":ticket.token,"generation":gen})
                }
            });
        }
        let upload = self.active.as_mut().ok_or("no_upload")?;
        if upload.save_id != id {
            return Err("save_id_mismatch");
        }
        if upload.opened.elapsed() > TOTAL_TIMEOUT {
            return Err("timeout");
        }
        if string(message, "vaultToken")? != upload.ticket.token
            || string(message, "generation")? != upload.generation
        {
            return Err("vault_switched");
        }
        check(session, &upload.ticket, &upload.generation)?;
        let mut response = json!({"ok":true,"saveId":id,"vaultToken":upload.ticket.token,"generation":upload.generation});
        match kind {
            "page_save_begin" => {
                if upload.size.is_some() {
                    return Err("invalid_state");
                }
                let size = size(message)?;
                upload
                    .bytes
                    .try_reserve_exact(size)
                    .map_err(|_| "resource_limit")?;
                upload.size = Some(size);
                response["type"] = json!("page_save_ready");
            }
            "page_save_chunk" => {
                if upload.prepared.is_some() {
                    return Err("invalid_state");
                }
                let size = upload.size.ok_or("invalid_state")?;
                let index = message
                    .get("index")
                    .and_then(Value::as_u64)
                    .ok_or("invalid_chunk")?;
                if index != upload.next {
                    return Err("chunk_out_of_order");
                }
                let data = string(message, "data")?;
                let expected = pages::CHUNK_BYTES.min(size - upload.bytes.len());
                if expected == 0 || data.len() != expected.div_ceil(3) * 4 {
                    return Err("invalid_chunk_size");
                }
                let decoded = Zeroizing::new(STANDARD.decode(data).map_err(|_| "invalid_base64")?);
                if decoded.len() != expected {
                    return Err("invalid_chunk_size");
                }
                upload.hash.update(&*decoded);
                upload.bytes.extend_from_slice(&decoded);
                upload.next += 1;
                response["type"] = json!("page_save_chunk_ack");
                response["index"] = json!(index);
                response["received"] = json!(upload.bytes.len());
            }
            "page_save_prepare" => {
                if upload.prepared.is_some() || upload.size != Some(upload.bytes.len()) {
                    return Err("incomplete_upload");
                }
                let sha = format!("{:x}", upload.hash.clone().finalize());
                response["type"] = json!("page_save_prepared");
                response["sha256"] = json!(sha);
                response["byteLength"] = json!(upload.bytes.len());
                upload.prepared = Some(sha);
            }
            "page_save_commit" => {
                let sha = upload.prepared.as_ref().ok_or("invalid_state")?;
                if string(message, "sha256")? != sha {
                    return Err("hash_mismatch");
                }
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(|_| "clock_error")?
                    .as_millis() as i64;
                let receipt = session
                    .write_for(&upload.ticket, |db| {
                        pages::insert(db, id, &upload.meta, &upload.bytes, sha, now)
                            .map_err(PageError)
                    })
                    .map_err(|e: PageError| e.0)?;
                response = saved(receipt, id, &upload.ticket.token, &upload.generation);
                self.active = None; // only after COMMIT; loss of response uses read-only status
            }
            "page_save_abort" => {
                response["type"] = json!("page_save_aborted");
                self.active = None;
            }
            _ => return Err("invalid_message"),
        }
        Ok(response)
    }
}

struct PageError(&'static str);
impl From<vault::VaultError> for PageError {
    fn from(e: vault::VaultError) -> Self {
        Self(e.code())
    }
}

#[cfg(test)]
mod tests;
