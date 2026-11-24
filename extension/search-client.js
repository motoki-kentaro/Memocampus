// Native Messaging client. Transport is injected so this module stays testable.

import { HOST_NAME } from "./host-config.js";
import { t } from "./i18n.js";

export { HOST_NAME };
export const PAGE_SIZE = 50;
export const MAX_QUERY_CHARS = 200;
export const SEARCH_TIMEOUT_MS = 10_000;

// The message key for each host or client error code (Issue #77: the text is in _locales). The codes
// themselves are protocol values and are never translated.
const ERROR_KEYS = {
  host_unavailable: "errorHostUnavailable",
  timeout: "errorTimeout",
  invalid_response: "errorInvalidResponse",
  db_error: "errorDbError",
  query_too_long: "errorQueryTooLong",
  invalid_query: "errorInvalidQuery",
  invalid_limit: "errorInvalidLimit",
  invalid_offset: "errorInvalidOffset",
  invalid_request_id: "errorInvalidRequestId",
  invalid_bounds: "errorInvalidBounds",
  unknown_type: "errorUnknownType",
  app_dir_unavailable: "errorAppDirUnavailable",
  vault_not_configured: "errorVaultNotConfigured",
  vault_missing: "errorVaultMissing",
  vault_invalid: "errorVaultInvalid",
  vault_schema_mismatch: "errorVaultSchemaMismatch",
  config_invalid: "errorConfigInvalid",
  vault_switched: "errorVaultSwitched",
  vault_mode_mismatch: "errorVaultModeMismatch",
  vault_encryption_unsupported: "errorVaultEncryptionUnsupported",
  key_unavailable: "errorKeyUnavailable",
  vault_backup_encrypted_unsupported: "errorVaultBackupEncryptedUnsupported",
  backup_password_required: "errorBackupPasswordRequired",
  backup_password_too_short: "errorBackupPasswordTooShort",
  backup_password_too_long: "errorBackupPasswordTooLong",
  backup_password_rejected: "errorBackupPasswordRejected",
  backup_path_required: "errorBackupPathRequired",
  backup_unsupported_format: "errorBackupUnsupportedFormat",
  backup_unsupported_version: "errorBackupUnsupportedVersion",
  backup_malformed: "errorBackupMalformed",
  backup_incomplete: "errorBackupIncomplete",
  backup_tampered: "errorBackupTampered",
  backup_damaged: "errorBackupDamaged",
  backup_schema_mismatch: "errorBackupSchemaMismatch",
  backup_vault_invalid: "errorBackupVaultInvalid",
  vault_restore_pending: "errorVaultRestorePending",
  vault_restore_not_pending: "errorVaultRestoreNotPending",
  vault_restore_incomplete: "errorVaultRestoreIncomplete",
  vault_restore_damaged: "errorVaultRestoreDamaged",
  restore_failed: "errorRestoreFailed",
  disk_full: "errorDiskFull",
  backup_wal_unsupported: "errorBackupWalUnsupported",
  invalid_request: "errorInvalidRequest",
  vault_creation_pending: "errorVaultCreationPending",
  vault_convert_pending: "errorVaultConvertPending",
  vault_convert_not_pending: "errorVaultConvertNotPending",
  vault_convert_incomplete: "errorVaultConvertIncomplete",
  vault_convert_spell_unconfirmed: "errorVaultConvertSpellUnconfirmed",
  vault_convert_invalid_state: "errorVaultConvertInvalidState",
  vault_convert_damaged: "errorVaultConvertDamaged",
  vault_convert_source_changed: "errorVaultConvertSourceChanged",
  vault_convert_same_as_source: "errorVaultConvertSameAsSource",
  vault_convert_mode_mismatch: "errorVaultConvertModeMismatch",
  vault_convert_plaintext_unacknowledged: "errorVaultConvertPlaintextUnacknowledged",
  vault_convert_verify_failed: "errorVaultConvertVerifyFailed",
  vault_convert_failed: "errorVaultConvertFailed",
  vault_work_file: "errorVaultWorkFile",
  vault_creation_not_pending: "errorVaultCreationNotPending",
  vault_creation_damaged: "errorVaultCreationDamaged",
  recovery_spell_invalid: "errorRecoverySpellInvalid",
  recovery_spell_rejected: "errorRecoverySpellRejected",
  vault_legacy_unsupported: "errorVaultLegacyUnsupported",
  backup_legacy_unsupported: "errorBackupLegacyUnsupported",
  vault_not_encrypted: "errorVaultNotEncrypted",
  key_protector_unavailable: "errorKeyProtectorUnavailable",
  key_protector_failed: "errorKeyProtectorFailed",
  random_unavailable: "errorRandomUnavailable",
  vault_locked: "errorVaultLocked",
  vault_decrypt_failed: "errorVaultDecryptFailed",
  invalid_vault_token: "errorInvalidVaultToken",
  vault_busy: "errorVaultBusy",
  management_disabled: "errorManagementDisabled",
  file_exists: "errorFileExists",
  sidecar_exists: "errorSidecarExists",
  parent_missing: "errorParentMissing",
  invalid_path: "errorInvalidPath",
  cancelled: "errorCancelled",
  backup_failed: "errorBackupFailed",
  location_rejected: "errorLocationRejected",
  location_needs_approval: "errorLocationNeedsApproval",
  io_error: "errorIoError",
  invalid_ack: "errorInvalidAck",
  invalid_url: "errorInvalidUrl",
  invalid_event: "errorInvalidEvent",
  invalid_message: "errorInvalidMessage",
  queue_full: "errorQueueFull",
  storage_write_failed: "errorStorageWriteFailed",
  dead_letter_full: "errorDeadLetterFull",
  dead_letter_write_failed: "errorDeadLetterWriteFailed",
  policy_not_initialized: "errorPolicyNotInitialized",
  policy_invalid: "errorPolicyInvalid",
  policy_busy: "errorPolicyBusy",
  policy_unavailable: "errorPolicyUnavailable",
  policy_unsupported: "errorPolicyUnsupported",
  invalid_policy_batch: "errorInvalidPolicyBatch",
  policy_batch_too_large: "errorPolicyBatchTooLarge",
  policy_already_initialized: "errorPolicyAlreadyInitialized",
  incognito_unknown: "errorIncognitoUnknown",
  invalid_incognito: "errorInvalidIncognito",
  incognito_fixed_at_begin: "errorIncognitoFixedAtBegin",
  unsupported_url: "errorUnsupportedUrl",
  network_error: "errorNetworkError",
  http_error: "errorHttpError",
  page_access_denied: "errorPageAccessDenied",
  too_large: "errorTooLarge",
  empty_image: "errorEmptyImage",
  svg_not_supported: "errorSvgNotSupported",
  unsupported_format: "errorUnsupportedFormat",
  decode_failed: "errorDecodeFailed",
  transfer_interrupted: "errorTransferInterrupted",
  host_outdated: "errorHostOutdated",
  save_failed: "errorSaveFailed",
  saved_image_not_found: "errorSavedImageNotFound",
  incognito_not_publishable: "errorIncognitoNotPublishable",
  invalid_saved_image_id: "errorInvalidSavedImageId",
  invalid_mcp_public: "errorInvalidMcpPublic",
  hash_mismatch: "errorHashMismatch",
  length_mismatch: "errorLengthMismatch",
  mime_mismatch: "errorMimeMismatch",
  dimension_mismatch: "errorDimensionMismatch",
  invalid_dimensions: "errorInvalidDimensions",
  truncated_image: "errorTruncatedImage",
  invalid_thumbnail: "errorInvalidThumbnail",
  save_id_conflict: "errorSaveIdConflict",
  blob_missing: "errorBlobMissing",
  invalid_hash: "errorInvalidHash",
  invalid_save_id: "errorInvalidSaveId",
  unsupported_protocol: "errorUnsupportedProtocol",
  no_upload: "errorNoUpload",
  upload_in_progress: "errorUploadInProgress",
  save_id_mismatch: "errorSaveIdMismatch",
  unexpected_chunk: "errorUnexpectedChunk",
  duplicate_chunk: "errorDuplicateChunk",
  chunk_out_of_order: "errorChunkOutOfOrder",
  invalid_chunk_size: "errorInvalidChunkSize",
  invalid_base64: "errorInvalidBase64",
  upload_too_large: "errorUploadTooLarge",
  incomplete_upload: "errorIncompleteUpload",
  invalid_tag: "errorInvalidTag",
  too_many_tags: "errorTooManyTags",
  policy_conflict: "errorPolicyConflict",
  publish_blocked: "errorPublishBlocked",
  policy_limit: "errorPolicyLimit",
  layer_disabled: "errorLayerDisabled",
  invalid_exact_url: "errorInvalidExactUrl",
  policy_migration_required: "errorPolicyMigrationRequired",
  invalid_action: "errorInvalidAction",
  invalid_scope: "errorInvalidScope",
  invalid_etag: "errorInvalidEtag",
  invalid_blob_id: "errorInvalidBlobId",
  read_incomplete: "errorReadIncomplete",
  read_hash_mismatch: "errorReadHashMismatch",
  clipboard_unavailable: "errorClipboardUnavailable",
  clipboard_denied: "errorClipboardDenied",
  clipboard_failed: "errorClipboardFailed",
  copy_too_large: "errorCopyTooLarge",
  copy_convert_failed: "errorCopyConvertFailed",
  export_failed: "errorExportFailed",
  export_file_appeared: "errorExportFileAppeared",
  export_invalid_target: "errorExportInvalidTarget",
};

/** The message for a host or client error code, in the UI language; unknown codes get a generic one. */
export function describeErrorCode(code) {
  if (code === "query_too_long") return t("errorQueryTooLong", MAX_QUERY_CHARS);
  return t(typeof code === "string" && Object.hasOwn(ERROR_KEYS, code) ? ERROR_KEYS[code] : "errorGeneric");
}

/** Codes meaning "there is no usable vault" — the UI should send the user to vault settings. */
export const VAULT_UNAVAILABLE_CODES = new Set(["vault_not_configured", "vault_missing", "vault_invalid", "vault_schema_mismatch", "config_invalid", "app_dir_unavailable"]);

export class SearchError extends Error {
  constructor(code, detail = "", response = null) {
    super(describeErrorCode(code));
    this.name = "SearchError";
    this.code = code;
    this.detail = detail;
    this.response = response;
  }
}

/** Wraps chrome.runtime.sendNativeMessage; each call starts a short-lived host process. */
export function chromeNativeTransport(runtime) {
  return (message) =>
    new Promise((resolve, reject) => {
      runtime.sendNativeMessage(HOST_NAME, message, (response) => {
        const lastError = runtime.lastError;
        if (lastError) return reject(new SearchError("host_unavailable", lastError.message || ""));
        resolve(response);
      });
    });
}

function withTimeout(promise, timeoutMs) {
  let timer;
  const timeout = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new SearchError("timeout")), timeoutMs);
  });
  return Promise.race([promise, timeout]).finally(() => clearTimeout(timer));
}

/**
 * Sends one request carrying a fresh requestId and validates the tagged response. Resolves to
 * the response on `ok: true`; rejects with SearchError (keeping the response) otherwise.
 */
export async function callHost(message, { send, resultType, timeoutMs = SEARCH_TIMEOUT_MS, createId = () => crypto.randomUUID() }) {
  const requestId = createId();
  const response = await withTimeout(
    Promise.resolve().then(() => send({ ...message, requestId })),
    timeoutMs,
  );
  if (!response || response.type !== resultType || response.requestId !== requestId) {
    throw new SearchError("invalid_response");
  }
  if (response.ok !== true) {
    throw new SearchError(typeof response.error === "string" ? response.error : "invalid_response", "", response);
  }
  return response;
}

function isWebUrl(value) {
  if (typeof value !== "string") return false;
  try {
    const { protocol } = new URL(value);
    return protocol === "http:" || protocol === "https:";
  } catch {
    return false;
  }
}

const optionalString = (value) => (typeof value === "string" ? value : null);

function normalizeItem(item) {
  if (!item || !isWebUrl(item.url) || typeof item.lastActivityAt !== "string") return null;
  return {
    url: item.url,
    title: typeof item.title === "string" ? item.title : "",
    lastVisitedAt: optionalString(item.lastVisitedAt),
    lastCapturedAt: optionalString(item.lastCapturedAt),
    lastActivityAt: item.lastActivityAt,
    source: ["visit", "snapshot", "both"].includes(item.source) ? item.source : "visit",
  };
}

/** A vault token as the host writes it (SHA-256, lowercase hex), or null. */
const validVaultToken = (value) => (typeof value === "string" && /^[0-9a-f]{64}$/.test(value) ? value : null);

/**
 * Sends one search request. Resolves to { items, received, hasMore, vaultToken }, where `received` is the raw
 * row count used to advance the offset. `visitedFrom` and `visitedTo` (Issue #5) narrow it to the visits of a
 * local day: both or neither are sent, and only to a host that names `visit_calendar_v1` (an older host would
 * ignore them and answer the unfiltered list).
 */
export async function searchPages({ query, offset = 0, limit = PAGE_SIZE, visitedFrom = null, visitedTo = null }, options) {
  const message = { type: "search_pages", query, limit, offset };
  if (visitedFrom !== null || visitedTo !== null) {
    if (typeof visitedFrom !== "string" || typeof visitedTo !== "string") throw new SearchError("invalid_bounds");
    Object.assign(message, { visitedFrom, visitedTo });
  }
  const response = await callHost(message, { ...options, resultType: "search_pages_result" });
  if (!Array.isArray(response.items)) throw new SearchError("invalid_response");
  // The vault the page came from (Issue #43); a host from before it sends none.
  const vaultToken = validVaultToken(response.vaultToken);
  return {
    items: response.items.map(normalizeItem).filter(Boolean),
    received: response.items.length,
    hasMore: response.hasMore === true,
    vaultToken,
  };
}

/**
 * Visits per local day of a month (Issue #5): `bounds` are the starts of the days and of the day after the
 * last (29 to 32 instants, see visit-calendar.js). Resolves to { counts, vaultToken }: one non-negative
 * integer per day, and the vault they were counted in. Anything else is `invalid_response`.
 */
export async function visitDays(bounds, options) {
  const response = await callHost({ type: "visit_days", bounds }, { ...options, resultType: "visit_days_result" });
  const counts = response.counts;
  const vaultToken = validVaultToken(response.vaultToken);
  if (!Array.isArray(counts) || counts.length !== bounds.length - 1 || !counts.every((count) => Number.isSafeInteger(count) && count >= 0) || vaultToken === null) {
    throw new SearchError("invalid_response");
  }
  return { counts: [...counts], vaultToken };
}

/** Hands out tokens so that only the most recently started search may update the UI. */
export function createLatestGate() {
  let current = 0;
  return {
    next: () => ++current,
    isCurrent: (token) => token === current,
  };
}

export { isWebUrl };
