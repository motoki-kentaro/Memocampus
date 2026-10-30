//! Originals for the side panel's Image tab (one-shot messages, like `image_query`).
//!
//! ```text
//! → image_read            { blobId, sha256, vaultToken, offset }
//! ← image_read_result     { vaultToken, blobId, sha256, mimeType, byteLength, offset, data }
//!                           data: Base64 of bytes [offset, offset + min(READ_CHUNK_BYTES, byteLength - offset))
//! → image_thumbnail_put   { blobId, sha256, vaultToken, thumbnail { mimeType, width, height, generator, data } }
//! ← image_thumbnail_put_result { vaultToken, blobId, stored }         stored=false: it already had one
//! → image_export          { blobId, sha256, vaultToken }
//! ← image_export_result   { vaultToken, blobId, fileName }  or  { cancelled: true }
//! ```
//!
//! Every request names the vault (`vaultToken`) and the bytes (`sha256`) it means. A request for
//! another vault fails with `vault_switched`; a blob id that now names other bytes with
//! `blob_missing`. So a chunk, a thumbnail or an exported file can never come from, or go to, a
//! vault other than the one the panel listed. Originals are only read here, never written.
//!
//! Exports: the host shows the Windows save dialog itself (starting in Documents, named after the
//! last save time) and writes exactly the stored bytes to the path the user chose there; the
//! extension cannot name a path. The file is written to a temporary file next to it and moved into
//! place only when complete, so a failed write never damages an existing file.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde_json::{Map, Value};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::dialog::Dialogs;
use crate::images::{self, ImageError, ThumbnailInput, MAX_THUMBNAIL_BYTES};
use crate::search::request_id;
use crate::vault::{self, Session, VaultTicket};

/// Raw bytes per `image_read` reply: about 700 KB as Base64, leaving room below Chrome's 1 MB.
pub const READ_CHUNK_BYTES: usize = 512 * 1024;

fn result_type(kind: &str) -> &'static str {
    match kind {
        "image_read" => "image_read_result",
        "image_thumbnail_put" => "image_thumbnail_put_result",
        _ => "image_export_result",
    }
}

pub fn handle(session: &mut Session, dialogs: &dyn Dialogs, message: &Value) -> Value {
    let kind = message.get("type").and_then(Value::as_str).unwrap_or("");
    let id = request_id(message);
    let mut reply = match id {
        None => Err("invalid_request_id"),
        Some(_) => answer(session, dialogs, kind, message),
    }
    .unwrap_or_else(|code| {
        let mut fields = Map::new();
        fields.insert("error".into(), code.into());
        fields
    });
    let ok = !reply.contains_key("error");
    reply.insert("ok".into(), ok.into());
    reply.insert("type".into(), result_type(kind).into());
    reply.insert("requestId".into(), id.into());
    Value::Object(reply)
}

fn is_hex64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

struct Target {
    blob_id: i64,
    sha256: String,
}

fn target(message: &Value) -> Result<Target, &'static str> {
    let blob_id = message
        .get("blobId")
        .and_then(Value::as_i64)
        .filter(|id| *id > 0)
        .ok_or("invalid_blob_id")?;
    let sha256 = message
        .get("sha256")
        .and_then(Value::as_str)
        .filter(|sha| is_hex64(sha))
        .ok_or("invalid_hash")?;
    Ok(Target {
        blob_id,
        sha256: sha256.to_owned(),
    })
}

/// The active vault, provided it is the one the request was made for.
fn ticket_for(session: &mut Session, message: &Value) -> Result<VaultTicket, &'static str> {
    let expected = message
        .get("vaultToken")
        .and_then(Value::as_str)
        .filter(|token| is_hex64(token))
        .ok_or("invalid_vault_token")?;
    let ticket = session.ticket().map_err(|error| error.code())?;
    if ticket.token != expected {
        return Err("vault_switched");
    }
    Ok(ticket)
}

fn answer(
    session: &mut Session,
    dialogs: &dyn Dialogs,
    kind: &str,
    message: &Value,
) -> Result<Map<String, Value>, &'static str> {
    match kind {
        "image_read" => read(session, message),
        "image_thumbnail_put" => put_thumbnail(session, message),
        "image_export" => export(session, dialogs, message),
        _ => Err("unknown_type"),
    }
}

fn read(session: &mut Session, message: &Value) -> Result<Map<String, Value>, &'static str> {
    let target = target(message)?;
    let offset = message
        .get("offset")
        .and_then(Value::as_u64)
        .ok_or("invalid_offset")?;
    let ticket = ticket_for(session, message)?;
    let (info, bytes) = session
        .read(|db| {
            let info = images::original_info(db, target.blob_id, &target.sha256)?;
            let total = info.byte_length as u64;
            if offset >= total {
                return Err(ImageError::LengthMismatch);
            }
            let length = (total - offset).min(READ_CHUNK_BYTES as u64) as usize;
            let bytes = images::read_original_range(db, target.blob_id, offset as usize, length)?;
            Ok((info, bytes))
        })
        .map_err(|error| error.code())?
        .map_err(|error| match error {
            ImageError::LengthMismatch => "invalid_offset",
            other => other.code(),
        })?;
    let mut fields = Map::new();
    fields.insert("vaultToken".into(), ticket.token.into());
    fields.insert("blobId".into(), target.blob_id.into());
    fields.insert("sha256".into(), info.sha256.into());
    fields.insert("mimeType".into(), info.mime_type.into());
    fields.insert("byteLength".into(), info.byte_length.into());
    fields.insert("offset".into(), offset.into());
    fields.insert("data".into(), STANDARD.encode(&bytes).into());
    Ok(fields)
}

fn parse_thumbnail(value: Option<&Value>) -> Result<ThumbnailInput, &'static str> {
    const INVALID: &str = "invalid_thumbnail";
    let value = value.filter(|value| value.is_object()).ok_or(INVALID)?;
    let text = |key: &str| value.get(key).and_then(Value::as_str).ok_or(INVALID);
    let small = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .ok_or(INVALID)
    };
    let data = text("data")?;
    if data.len() > MAX_THUMBNAIL_BYTES.div_ceil(3) * 4 {
        return Err(INVALID);
    }
    Ok(ThumbnailInput {
        mime_type: text("mimeType")?.to_owned(),
        width: small("width")?,
        height: small("height")?,
        generator: small("generator")?,
        data: STANDARD.decode(data).map_err(|_| INVALID)?,
    })
}

fn put_thumbnail(
    session: &mut Session,
    message: &Value,
) -> Result<Map<String, Value>, &'static str> {
    let target = target(message)?;
    let thumbnail = parse_thumbnail(message.get("thumbnail"))?;
    let ticket = ticket_for(session, message)?;
    let stored =
        images::add_thumbnail(session, &ticket, target.blob_id, &target.sha256, &thumbnail)
            .map_err(ImageError::code)?;
    let mut fields = Map::new();
    fields.insert("vaultToken".into(), ticket.token.into());
    fields.insert("blobId".into(), target.blob_id.into());
    fields.insert("stored".into(), stored.into());
    Ok(fields)
}

/// `<last save time in Unix epoch seconds>.<extension of the real format>`.
pub fn export_file_name(last_saved_at_ms: i64, extension: &str) -> String {
    format!("{}.{extension}", last_saved_at_ms.div_euclid(1000))
}

fn export(
    session: &mut Session,
    dialogs: &dyn Dialogs,
    message: &Value,
) -> Result<Map<String, Value>, &'static str> {
    let target = target(message)?;
    let ticket = ticket_for(session, message)?;
    // The bytes are read and verified before the dialog opens, so a vault switch while the dialog
    // is open cannot change what is written.
    let (info, bytes) = session
        .read(|db| {
            let info = images::original_info(db, target.blob_id, &target.sha256)?;
            let bytes = images::read_original(db, target.blob_id, &info)?;
            Ok::<_, ImageError>((info, bytes))
        })
        .map_err(|error| error.code())?
        .map_err(ImageError::code)?;
    // The read must have come from the vault the request named.
    if session.ticket().map_err(|error| error.code())?.token != ticket.token {
        return Err("vault_switched");
    }
    let format = images::sniff(&bytes).map_err(ImageError::code)?;
    let file_name = export_file_name(info.last_saved_at, format.extension());
    let mut fields = Map::new();
    fields.insert("vaultToken".into(), ticket.token.into());
    fields.insert("blobId".into(), target.blob_id.into());
    let Some(path) = dialogs.choose_export_file(&file_name, format.extension()) else {
        fields.insert("cancelled".into(), true.into());
        return Ok(fields);
    };
    // The dialog asks before an existing file is chosen; only then may it be replaced.
    let confirmed_overwrite = path.exists();
    write_export(&path, &bytes, confirmed_overwrite)?;
    let shown = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    // Only the file name goes back to the extension, never the folder.
    fields.insert("fileName".into(), shown.into());
    Ok(fields)
}

fn temp_path(path: &Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    let name = path.file_name()?.to_string_lossy();
    Some(parent.join(format!(
        ".{name}.{}.memocampus-tmp",
        &uuid::Uuid::new_v4().simple().to_string()[..12]
    )))
}

/// Writes `bytes` to `path` without ever leaving a partial file there: a temporary file in the
/// same folder is written and flushed first, then moved onto `path`. An existing file is replaced
/// only with `replace`; a file that appeared after the dialog closed is never overwritten.
pub fn write_export(path: &Path, bytes: &[u8], replace: bool) -> Result<(), &'static str> {
    if path.is_dir() {
        return Err("export_invalid_target");
    }
    if !path.parent().is_some_and(Path::is_dir) {
        return Err("parent_missing");
    }
    let temp = temp_path(path).ok_or("export_invalid_target")?;
    let written = (|| -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temp);
        return Err("export_failed");
    }
    let moved = if replace {
        replace_file(&temp, path)
    } else {
        vault::rename_no_replace(&temp, path)
    };
    if let Err(error) = moved {
        let _ = fs::remove_file(&temp);
        return Err(if error.kind() == io::ErrorKind::AlreadyExists {
            "export_file_appeared"
        } else {
            "export_failed"
        });
    }
    Ok(())
}

/// Moves `from` onto `to`, replacing it in one step (or failing with `to` unchanged).
#[cfg(windows)]
fn replace_file(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let wide = |path: &Path| {
        path.as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<u16>>()
    };
    let (from, to) = (wide(from), wide(to));
    let flags = MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH;
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), flags) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace_file(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)
}

#[cfg(test)]
mod tests;
