//! Image upload over one `chrome.runtime.connectNative` port (protocol version 2).
//!
//! A port is one host process: the extension sends `image_save_begin`, raw-byte chunks of at most
//! 1 MiB (Base64 in JSON), then `image_save_commit`. Bytes are buffered in memory only; nothing
//! reaches SQLite before the commit's single transaction, and the vault ticket taken at `begin`
//! makes that transaction fail if the active vault changed meanwhile. Any error, an abort, or the
//! port closing (process exit) discards the buffer. Every reply is far below Chrome's 1 MB limit.
//!
//! ```text
//! → image_save_begin  { protocol, saveId, sha256, byteLength, mimeType, width, height, imageUrl, pageUrl?, frameUrl?,
//!                       incognito, vaultToken? }                            vaultToken: from an earlier reply of this save action
//! ← image_save_ready  { saveId, vaultToken, upload, chunkBytes, maxBytes }   upload=false: bytes already stored
//! ← image_saved       { saveId, vaultToken, ..., replayed: true }            this save action was committed before
//! → image_save_chunk  { saveId, index, data }                    index 0,1,2,…; every chunk but the last is exactly chunkBytes
//! ← image_save_chunk_ack { saveId, index, received }
//! → image_save_commit { saveId, thumbnail? { mimeType, width, height, generator, data } }
//! ← image_saved       { saveId, vaultToken, blobId, savedImageId, deduplicated, replayed }   sent only after COMMIT
//!
//! A retry of the same save action on a new port (after a lost connection or answer) sends the
//! `vaultToken` it was given. If the active vault is another one by then, the retry fails with
//! `vault_switched` before anything is looked up or written: a save never moves to another vault.
//! `incognito` (required, true or false) is sent only with `begin` and fixed there for the upload. A
//! chunk or commit carrying it fails the upload, and a retry of a committed save action with the
//! other value fails with `save_id_conflict`, so a save's origin can never change.
//! → image_save_abort  { saveId }  ← image_save_aborted { saveId }
//! ← image_save_error  { saveId, error }                          the upload is discarded
//! ```

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde_json::{json, Value};

use crate::images::{
    self, ImageError, Precheck, SaveOutcome, SaveRequest, ThumbnailInput, MAX_IMAGE_BYTES,
    MAX_THUMBNAIL_BYTES,
};
use crate::vault::{Session, VaultTicket};

pub const PROTOCOL_VERSION: u64 = 2;
/// Raw bytes per chunk (Base64 makes each chunk message about 1.4 MB, far below the 64 MiB limit).
pub const CHUNK_BYTES: usize = 1024 * 1024;

const fn base64_len(raw: usize) -> usize {
    raw.div_ceil(3) * 4
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProtocolError {
    Image(ImageError),
    UnsupportedProtocol,
    InvalidMessage,
    InvalidIncognito,
    IncognitoFixedAtBegin,
    NoUpload,
    UploadInProgress,
    SaveIdMismatch,
    UnexpectedChunk,
    DuplicateChunk,
    ChunkOutOfOrder,
    InvalidChunkSize,
    InvalidBase64,
    UploadTooLarge,
    IncompleteUpload,
}

impl ProtocolError {
    fn code(self) -> &'static str {
        match self {
            Self::Image(error) => error.code(),
            Self::UnsupportedProtocol => "unsupported_protocol",
            Self::InvalidMessage => "invalid_message",
            Self::InvalidIncognito => "invalid_incognito",
            Self::IncognitoFixedAtBegin => "incognito_fixed_at_begin",
            Self::NoUpload => "no_upload",
            Self::UploadInProgress => "upload_in_progress",
            Self::SaveIdMismatch => "save_id_mismatch",
            Self::UnexpectedChunk => "unexpected_chunk",
            Self::DuplicateChunk => "duplicate_chunk",
            Self::ChunkOutOfOrder => "chunk_out_of_order",
            Self::InvalidChunkSize => "invalid_chunk_size",
            Self::InvalidBase64 => "invalid_base64",
            Self::UploadTooLarge => "upload_too_large",
            Self::IncompleteUpload => "incomplete_upload",
        }
    }
}

impl From<ImageError> for ProtocolError {
    fn from(error: ImageError) -> Self {
        Self::Image(error)
    }
}

impl From<crate::vault::VaultError> for ProtocolError {
    fn from(error: crate::vault::VaultError) -> Self {
        Self::Image(error.into())
    }
}

struct Upload {
    request: SaveRequest,
    ticket: VaultTicket,
    /// False when the vault already holds these bytes: only a history row will be committed.
    needs_bytes: bool,
    buffer: Vec<u8>,
    next_index: u64,
}

/// Upload state of one host process (one port). At most one upload is open at a time.
#[derive(Default)]
pub struct Uploads {
    active: Option<Upload>,
}

/// `crypto.randomUUID()` form: 8-4-4-4-12 lower-case hex.
fn is_uuid(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    groups.len() == 5
        && groups.iter().zip([8, 4, 4, 4, 12]).all(|(group, len)| {
            group.len() == len
                && group
                    .chars()
                    .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        })
}

fn string(message: &Value, key: &str) -> Result<String, ProtocolError> {
    message
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(ProtocolError::InvalidMessage)
}

fn optional_string(message: &Value, key: &str) -> Result<Option<String>, ProtocolError> {
    match message.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(ProtocolError::InvalidMessage),
    }
}

fn number(message: &Value, key: &str) -> Result<u64, ProtocolError> {
    message
        .get(key)
        .and_then(Value::as_u64)
        .ok_or(ProtocolError::InvalidMessage)
}

fn dimension(message: &Value, key: &str) -> Result<u32, ProtocolError> {
    u32::try_from(number(message, key)?).map_err(|_| ImageError::InvalidDimensions.into())
}

fn decode_base64(message: &Value, key: &str, max_raw: usize) -> Result<Vec<u8>, ProtocolError> {
    let text = message
        .get(key)
        .and_then(Value::as_str)
        .ok_or(ProtocolError::InvalidMessage)?;
    // Refuse oversized text before decoding it.
    if text.len() > base64_len(max_raw) {
        return Err(ProtocolError::InvalidChunkSize);
    }
    STANDARD
        .decode(text)
        .map_err(|_| ProtocolError::InvalidBase64)
}

fn parse_begin(message: &Value) -> Result<SaveRequest, ProtocolError> {
    if message.get("protocol").and_then(Value::as_u64) != Some(PROTOCOL_VERSION) {
        return Err(ProtocolError::UnsupportedProtocol);
    }
    let save_id = string(message, "saveId")?;
    if !is_uuid(&save_id) {
        return Err(ImageError::InvalidSaveId.into());
    }
    let incognito = message
        .get("incognito")
        .and_then(Value::as_bool)
        .ok_or(ProtocolError::InvalidIncognito)?;
    let request = SaveRequest {
        save_id,
        sha256: string(message, "sha256")?,
        byte_length: number(message, "byteLength")?,
        mime_type: string(message, "mimeType")?,
        width: dimension(message, "width")?,
        height: dimension(message, "height")?,
        image_url: string(message, "imageUrl")?,
        page_url: optional_string(message, "pageUrl")?,
        frame_url: optional_string(message, "frameUrl")?,
        incognito,
    };
    images::validate_request(&request)?;
    Ok(request)
}

fn parse_thumbnail(message: &Value) -> Result<Option<ThumbnailInput>, ProtocolError> {
    let Some(value) = message.get("thumbnail").filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    fn invalid<E>(_: E) -> ProtocolError {
        ProtocolError::Image(ImageError::InvalidThumbnail)
    }
    let thumbnail = ThumbnailInput {
        mime_type: string(value, "mimeType").map_err(invalid)?,
        width: dimension(value, "width").map_err(invalid)?,
        height: dimension(value, "height").map_err(invalid)?,
        generator: u32::try_from(number(value, "generator").map_err(invalid)?).map_err(invalid)?,
        data: decode_base64(value, "data", MAX_THUMBNAIL_BYTES).map_err(invalid)?,
    };
    Ok(Some(thumbnail))
}

fn saved(save_id: &str, ticket: &VaultTicket, outcome: SaveOutcome) -> Value {
    json!({
        "ok": true,
        "type": "image_saved",
        "saveId": save_id,
        "vaultToken": ticket.token,
        "blobId": outcome.blob_id,
        "savedImageId": outcome.saved_image_id,
        "deduplicated": outcome.deduplicated,
        "replayed": outcome.replayed,
    })
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

impl Uploads {
    pub fn handle(&mut self, session: &mut Session, message: &Value) -> Value {
        self.handle_at(session, message, now_ms())
    }

    fn handle_at(&mut self, session: &mut Session, message: &Value, now: i64) -> Value {
        let save_id = message
            .get("saveId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let result = match message.get("type").and_then(Value::as_str) {
            Some("image_save_begin") => self.begin(session, message),
            Some("image_save_chunk") => self.chunk(message),
            Some("image_save_commit") => self.commit(session, message, now),
            Some("image_save_abort") => self.abort(message),
            _ => Err(ProtocolError::InvalidMessage),
        };
        result.unwrap_or_else(|error| {
            // Whatever went wrong, the unconfirmed bytes are dropped here.
            self.active = None;
            json!({ "ok": false, "type": "image_save_error", "saveId": save_id, "error": error.code() })
        })
    }

    fn current(&mut self, message: &Value) -> Result<&mut Upload, ProtocolError> {
        if message.get("incognito").is_some() {
            return Err(ProtocolError::IncognitoFixedAtBegin);
        }
        let save_id = string(message, "saveId")?;
        let upload = self.active.as_mut().ok_or(ProtocolError::NoUpload)?;
        if upload.request.save_id != save_id {
            return Err(ProtocolError::SaveIdMismatch);
        }
        Ok(upload)
    }

    fn begin(&mut self, session: &mut Session, message: &Value) -> Result<Value, ProtocolError> {
        if self.active.is_some() {
            return Err(ProtocolError::UploadInProgress);
        }
        let request = parse_begin(message)?;
        let expected = optional_string(message, "vaultToken")?;
        // The vault this save belongs to; the commit is refused if another vault is active then.
        let ticket = session.ticket()?;
        // A retry must reach the vault the save action started in, before any lookup or write.
        if expected.is_some_and(|expected| expected != ticket.token) {
            return Err(crate::vault::VaultError::Switched.into());
        }
        let precheck = session.read(|db| images::precheck(db, &request))??;
        let needs_bytes = match precheck {
            Precheck::AlreadySaved(outcome) => {
                return Ok(saved(&request.save_id, &ticket, outcome))
            }
            Precheck::BlobExists { .. } => false,
            Precheck::NeedUpload => true,
        };
        let reply = json!({
            "ok": true,
            "type": "image_save_ready",
            "saveId": request.save_id,
            "vaultToken": ticket.token,
            "protocol": PROTOCOL_VERSION,
            "upload": needs_bytes,
            "chunkBytes": CHUNK_BYTES,
            "maxBytes": MAX_IMAGE_BYTES,
        });
        self.active = Some(Upload {
            request,
            ticket,
            needs_bytes,
            buffer: Vec::new(),
            next_index: 0,
        });
        Ok(reply)
    }

    fn chunk(&mut self, message: &Value) -> Result<Value, ProtocolError> {
        let upload = self.current(message)?;
        if !upload.needs_bytes {
            return Err(ProtocolError::UnexpectedChunk);
        }
        let index = number(message, "index")?;
        if index < upload.next_index {
            return Err(ProtocolError::DuplicateChunk);
        }
        if index > upload.next_index {
            return Err(ProtocolError::ChunkOutOfOrder);
        }
        let remaining = upload.request.byte_length as usize - upload.buffer.len();
        if remaining == 0 {
            return Err(ProtocolError::UploadTooLarge);
        }
        let bytes = decode_base64(message, "data", CHUNK_BYTES)?;
        if bytes.len() > remaining {
            return Err(ProtocolError::UploadTooLarge);
        }
        // Fixed chunking bounds the number of chunks: all but the last are exactly CHUNK_BYTES.
        if bytes.len() != remaining.min(CHUNK_BYTES) {
            return Err(ProtocolError::InvalidChunkSize);
        }
        upload.buffer.extend_from_slice(&bytes);
        upload.next_index += 1;
        Ok(json!({
            "ok": true,
            "type": "image_save_chunk_ack",
            "saveId": upload.request.save_id,
            "index": index,
            "received": upload.buffer.len(),
        }))
    }

    fn commit(
        &mut self,
        session: &mut Session,
        message: &Value,
        now: i64,
    ) -> Result<Value, ProtocolError> {
        self.current(message)?;
        let thumbnail = parse_thumbnail(message)?;
        let upload = self.active.take().ok_or(ProtocolError::NoUpload)?;
        let original = if upload.needs_bytes {
            if upload.buffer.len() as u64 != upload.request.byte_length {
                return Err(ProtocolError::IncompleteUpload);
            }
            Some(upload.buffer.as_slice())
        } else {
            None
        };
        let outcome = images::save_image(
            session,
            &upload.ticket,
            &upload.request,
            original,
            thumbnail.as_ref(),
            now,
        )?;
        Ok(saved(&upload.request.save_id, &upload.ticket, outcome))
    }

    fn abort(&mut self, message: &Value) -> Result<Value, ProtocolError> {
        let save_id = string(message, "saveId")?;
        self.current(message)?;
        self.active = None;
        Ok(json!({ "ok": true, "type": "image_save_aborted", "saveId": save_id }))
    }
}

#[cfg(test)]
mod tests;
