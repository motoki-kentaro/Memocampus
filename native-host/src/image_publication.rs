//! The user's explicit choice for one save history row (one-shot message, like `image_query`).
//!
//! ```text
//! → image_mcp_public_set        { savedImageId, mcpPublic, vaultToken }
//! ← image_mcp_public_set_result { vaultToken, savedImageId, mcpPublic }
//! ```
//!
//! This records a permission and nothing else. It does not publish anything: a row is exposed only
//! if the MCP read path (Issue #12, Slice 3) also finds its URLs allowed by the privacy policy.
//! An incognito row is refused with `incognito_not_publishable` when asked to become public.

use serde_json::{json, Value};

use crate::image_original::ticket_for;
use crate::images::{self, ImageError};
use crate::search::request_id;
use crate::vault::Session;

const RESULT_TYPE: &str = "image_mcp_public_set_result";

pub fn handle(session: &mut Session, message: &Value) -> Value {
    let id = request_id(message);
    match id
        .ok_or("invalid_request_id")
        .and_then(|_| answer(session, message))
    {
        Ok((vault_token, saved_image_id, mcp_public)) => json!({
            "ok": true, "type": RESULT_TYPE, "requestId": id, "vaultToken": vault_token,
            "savedImageId": saved_image_id, "mcpPublic": mcp_public,
        }),
        Err(code) => json!({ "ok": false, "type": RESULT_TYPE, "requestId": id, "error": code }),
    }
}

fn answer(session: &mut Session, message: &Value) -> Result<(String, i64, bool), &'static str> {
    let saved_image_id = message
        .get("savedImageId")
        .and_then(Value::as_i64)
        .filter(|id| *id > 0)
        .ok_or("invalid_saved_image_id")?;
    // Only a JSON boolean counts as the user's choice: not 1, "true" or a missing value.
    let mcp_public = message
        .get("mcpPublic")
        .and_then(Value::as_bool)
        .ok_or("invalid_mcp_public")?;
    let ticket = ticket_for(session, message)?;
    images::set_mcp_public(session, &ticket, saved_image_id, mcp_public)
        .map_err(ImageError::code)?;
    Ok((ticket.token, saved_image_id, mcp_public))
}
