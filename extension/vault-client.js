// Vault management requests (`vault_*`) and their display text. Text comes from the catalogs
// (Issue #77 Slice 3); nothing here reads display text to decide anything.

import { SEARCH_TIMEOUT_MS, callHost } from "./search-client.js";
import { formatNumber, messageTable, t } from "./i18n.js";

/** Operations that may show a Windows file dialog wait for the user, so they get a long timeout. */
export const DIALOG_TIMEOUT_MS = 10 * 60_000;

export function vaultRequest(type, params, options) {
  const interactive = type !== "vault_status";
  return callHost(
    { type, ...params },
    { timeoutMs: interactive ? DIALOG_TIMEOUT_MS : SEARCH_TIMEOUT_MS, ...options, resultType: "vault_result" },
  );
}

const STATE_TEXT_KEYS = Object.freeze({
  ready: "vaultStateReady",
  not_configured: "vaultStateNotConfigured",
  missing: "vaultStateMissing",
  invalid: "vaultStateInvalid",
  vault_schema_mismatch: "vaultStateSchemaMismatch",
  config_invalid: "vaultStateConfigInvalid",
  app_dir_unavailable: "vaultStateAppDirUnavailable",
  // Issue #51: an encrypted or misdescribed vault is reported as such, never as invalid.
  vault_mode_mismatch: "vaultStateModeMismatch",
  vault_encryption_unsupported: "vaultStateEncryptionUnsupported",
  key_unavailable: "vaultStateKeyUnavailable",
  vault_locked: "vaultStateLocked",
  vault_decrypt_failed: "vaultStateDecryptFailed",
});
/** One-line state text by `status.state`; only own entries count (no prototype names). */
export const STATE_TEXT = messageTable(STATE_TEXT_KEYS);

/** The text of `table` for `code`, or undefined when `code` is not one of its own entries. */
const lookup = (table, code) => (typeof code === "string" && Object.hasOwn(table, code) ? table[code] : undefined);

/** Issue #51 Slice B: encrypted vaults. */
const NOTE_KEYS = Object.freeze({
  encryption: "vaultEncryptionNote",
  spellTitle: "vaultSpellTitle",
  spellLead: "vaultSpellLead",
  spellCaution: "vaultSpellCaution",
  spellNote: "vaultSpellNote",
  backupSpell: "vaultBackupSpellNote",
  restore: "vaultRestoreNote",
  convertSpell: "vaultConvertSpellNote",
  reissueSpell: "vaultReissueSpellNote",
  publicationReview: "vaultPublicationReview",
  location: "vaultLocationNote",
});
const NOTE = messageTable(NOTE_KEYS);

/** Issue #51: the Recovery Spell (ふっかつのじゅもん), 256 random bits as 43 base64url characters. */
export const SPELL_LENGTH = 43;
const SPELL_ALPHABET = /^[A-Za-z0-9_-]$/;
// The last of 43 characters carries 4 data bits and 2 unused bits, which must be zero.
const SPELL_LAST = "AEIMQUYcgkosw048";

// Notes are read from the catalog where they are used (the getters follow the catalog in use).
export const texts = Object.freeze({
  get ENCRYPTION_NOTE() { return NOTE.encryption; },
  get SPELL_TITLE() { return NOTE.spellTitle; },
  get SPELL_LEAD() { return NOTE.spellLead; },
  get SPELL_CAUTION() { return NOTE.spellCaution; },
  get SPELL_NOTE() { return NOTE.spellNote; },
  get SPELL_CHECK_TITLE() { return t("vaultSpellCheckTitle", SPELL_LENGTH); },
  get RECOVERY_TITLE() { return t("vaultRecoveryTitle", SPELL_LENGTH); },
  get PASSWORD_NOTE() { return t("vaultBackupPasswordNote", PASSWORD_MIN_CHARS); },
  get BACKUP_SPELL_NOTE() { return NOTE.backupSpell; },
  get RESTORE_NOTE() { return NOTE.restore; },
  get CONVERT_SPELL_NOTE() { return NOTE.convertSpell; },
  get REISSUE_SPELL_NOTE() { return NOTE.reissueSpell; },
  get PUBLICATION_REVIEW() { return NOTE.publicationReview; },
  get LOCATION_NOTE() { return NOTE.location; },
});

/** What the user typed or pasted, with surrounding whitespace removed (nothing else is changed). */
export function normalizeSpellInput(text) {
  return typeof text === "string" ? text.trim() : "";
}

/**
 * Null when `text` (after `normalizeSpellInput`) has the form of a Recovery Spell; otherwise what is
 * wrong. The host checks the same rules again and whether the spell opens the vault. The message never
 * contains the input.
 */
export function checkSpell(text) {
  const chars = [...normalizeSpellInput(text)];
  if (chars.length !== SPELL_LENGTH) return t("vaultSpellLength", SPELL_LENGTH, chars.length);
  const bad = chars.findIndex((c) => !SPELL_ALPHABET.test(c));
  if (bad >= 0) return t("vaultSpellCharacter", bad + 1);
  if (!SPELL_LAST.includes(chars[SPELL_LENGTH - 1])) return t("vaultSpellLastCharacter");
  return null;
}

/** True only when `typed` is exactly the spell shown (surrounding whitespace aside). */
export function spellMatches(shown, typed) {
  return typeof shown === "string" && shown.length === SPELL_LENGTH && normalizeSpellInput(typed) === shown;
}

/** Message for a spell the host refused (`recovery_spell_invalid` details). Never echoes the input. */
export function describeSpellProblem(response) {
  if (response?.reason === "length") return t("vaultSpellLength", SPELL_LENGTH, Number(response.count) || 0);
  if (response?.reason === "character") return t("vaultSpellCharacterHost", Number(response.position) || "?");
  if (response?.reason === "non_canonical") return t("vaultSpellLastCharacter");
  return t("errorRecoverySpellInvalid");
}

/** Issue #51 Slice C: backup formats. A plain vault's normal backup is A; see `backupFormat`. */
export function backupFormat(mode, protection) {
  const encrypted = mode === "encrypted";
  if (protection === "password") return encrypted ? "D" : "C";
  return encrypted ? "B" : "A";
}

const BACKUP_FORMAT_NOTE_KEYS = Object.freeze({ A: "vaultBackupFormatA", B: "vaultBackupFormatB", C: "vaultBackupFormatC", D: "vaultBackupFormatD" });
export const BACKUP_FORMAT_NOTE = messageTable(BACKUP_FORMAT_NOTE_KEYS);

export const PASSWORD_MIN_CHARS = 8;

/** Null when the two entries make a usable backup password, otherwise what is wrong. */
export function checkBackupPassword(password, confirmation) {
  if (typeof password !== "string" || [...password].length < PASSWORD_MIN_CHARS) return t("vaultBackupPasswordShort", PASSWORD_MIN_CHARS);
  if (password !== confirmation) return t("vaultBackupPasswordMismatch");
  return null;
}

const RESTORE_FORMAT_TEXT_KEYS = Object.freeze({ A: "vaultRestoreFormatA", B: "vaultRestoreFormatB", C: "vaultRestoreFormatC", D: "vaultRestoreFormatD" });
export const RESTORE_FORMAT_TEXT = messageTable(RESTORE_FORMAT_TEXT_KEYS);

/** "閲覧 …件・スナップショット …件・画像 …件" (and the saved pages), numbers formatted for the language. */
function countLines(counts, key) {
  const lines = [];
  if (counts && Number.isSafeInteger(counts.visits)) {
    lines.push(t(key, formatNumber(counts.visits), formatNumber(counts.snapshots), formatNumber(counts.images)));
    if (Number.isSafeInteger(counts.pages)) lines.push(t("vaultPageCounts", formatNumber(counts.pages), formatNumber(counts.pageBlobs)));
  }
  return lines;
}

/** What the user is asked before a restore (`vault_restore_inspect` answer). */
export function describeRestoreSource(info) {
  const lines = [t("vaultRestoreSourcePath", String(info.backupPath)), lookup(RESTORE_FORMAT_TEXT, info.format) ?? t("vaultRestoreFormatUnknown")];
  if (info.policyCopy === true) lines.push(t("vaultRestorePolicyIncluded"));
  lines.push(texts.RESTORE_NOTE);
  return lines.join("\n");
}

/** The restored vault, for the user's confirmation before it becomes active. */
export function describeRestored(restored) {
  const lines = [t("vaultRestoredPath", String(restored?.path ?? ""))];
  lines.push(restored?.mode === "encrypted" ? t("vaultRestoredEncrypted") : t("vaultRestoredPlain"));
  lines.push(...countLines(restored?.counts, "vaultRestoredCounts"));
  lines.push(t("vaultRestoredStartNote"));
  return lines.join("\n");
}

/** The policy copy a restore found: never applied, only shown. */
export function describePolicyCopy(copy) {
  switch (copy?.state) {
    case "valid":
      return t("vaultPolicyCopyValid", Number(copy.revision) || "?");
    case "invalid":
      return t("vaultPolicyCopyInvalid");
    case "absent":
      return t("vaultPolicyCopyAbsent");
    case "not_supported":
      return t("vaultPolicyCopyNotSupported");
    default:
      return "";
  }
}

/** A restore recorded by the host but not activated (`status.restorePending`). */
export function describeRestorePending(pending) {
  if (pending?.phase === "restored" || pending?.phase === "verified") return t("vaultRestorePendingReady", String(pending.path));
  return pending?.path ? t("vaultRestoreInterruptedPath", String(pending.path)) : t("vaultRestoreInterrupted");
}

/** Issue #51 Slice D: converting the vault shown between plain and encrypted. */
const CONVERT_ENCRYPT_KEYS = Object.freeze({ title: "vaultConvertEncryptTitle", note: "vaultConvertEncryptNote", pick: "vaultConvertEncryptPick" });
const CONVERT_DECRYPT_KEYS = Object.freeze({ title: "vaultConvertDecryptTitle", note: "vaultConvertDecryptNote", pick: "vaultConvertDecryptPick" });
// Issue #51 Slice E: a new spell means a new vault (a new key and a new salt).
const CONVERT_REISSUE_KEYS = Object.freeze({ title: "vaultReissueTitle", note: "vaultReissueNote", pick: "vaultReissuePick" });
export const CONVERT_TEXT = Object.freeze({
  encrypt: messageTable(CONVERT_ENCRYPT_KEYS),
  decrypt: messageTable(CONVERT_DECRYPT_KEYS),
  reissue: messageTable(CONVERT_REISSUE_KEYS),
});

/** Which conversion the vault shown offers: by its mode. */
export function convertDirection(mode) {
  if (mode === "plain") return "encrypt";
  if (mode === "encrypted") return "decrypt";
  return null;
}

/** Whether the vault shown offers re-issuing its spell (Slice E): an encrypted vault. */
export function reissueOffered(mode) {
  return mode === "encrypted";
}

/** Whether a conversion of `direction` makes a new spell (shown once, confirmed in full). */
export function convertMakesSpell(direction) {
  return direction === "encrypt" || direction === "reissue";
}

/** A converted vault (start or confirm answer), for the user's decision. */
export function describeConverted(converted) {
  const reissue = converted?.direction === "reissue";
  const encrypt = convertMakesSpell(converted?.direction);
  const lines = [t(reissue ? "vaultReissuedPath" : "vaultConvertedPath", String(converted?.path ?? ""))];
  if (converted?.sourcePath) lines.push(t(reissue ? "vaultReissuedSource" : "vaultConvertedSource", String(converted.sourcePath)));
  if (reissue) {
    lines.push(t("vaultReissuedKind"));
    lines.push(texts.REISSUE_SPELL_NOTE);
  } else {
    lines.push(encrypt ? t("vaultConvertedEncrypted") : t("vaultConvertedPlain"));
  }
  lines.push(...countLines(converted?.counts, "vaultConvertedCounts"));
  lines.push(t("vaultConvertedActivateNote"), t("vaultConvertedKeepNote"));
  return lines.join("\n");
}

/** A conversion recorded by the host but not finished in this page (`status.conversionPending`). */
export function describeConversionPending(pending) {
  const ready =
    pending?.direction === "decrypt" ? "vaultConversionPendingDecrypt" : pending?.direction === "reissue" ? "vaultConversionPendingReissue" : "vaultConversionPendingEncrypt";
  if (pending?.phase === "converted") return t(ready, String(pending.path));
  if (pending?.phase === "verified" && convertMakesSpell(pending.direction)) return t("vaultConversionPendingSpell", String(pending.path));
  if (pending?.phase === "verified") return t(ready, String(pending.path));
  return pending?.path ? t("vaultConversionInterruptedPath", String(pending.path)) : t("vaultConversionInterrupted");
}

/** MCP access to the vault shown (Issue #51): `status.publication` from a host that knows it. */
const PUBLICATION_TEXT_KEYS = Object.freeze({ confirmed: "vaultPublicationConfirmed", pending: "vaultPublicationPending" });
export const PUBLICATION_TEXT = messageTable(PUBLICATION_TEXT_KEYS);

const PUBLICATION_NOTE_KEYS = Object.freeze({
  confirmed: "vaultPublicationNoteConfirmed",
  pending: "vaultPublicationNotePending",
  unusable: "vaultPublicationNoteUnusable",
});
export const PUBLICATION_NOTE = messageTable(PUBLICATION_NOTE_KEYS);

export function describePublication(status) {
  const text = lookup(PUBLICATION_TEXT, status?.publication);
  if (!text) return "";
  const revision = status.publicationPolicyRevision;
  return status.publication === "confirmed" && Number.isSafeInteger(revision) ? t("vaultPublicationConfirmedRevision", formatNumber(revision, { useGrouping: false })) : text;
}

const REASON_TEXT_KEYS = Object.freeze({
  network_path: "vaultReasonNetworkPath",
  network_drive: "vaultReasonNetworkDrive",
  optical_drive: "vaultReasonOpticalDrive",
  ram_disk: "vaultReasonRamDisk",
  unknown_drive: "vaultReasonUnknownDrive",
  executable_directory: "vaultReasonExecutableDirectory",
  removable_drive: "vaultReasonRemovableDrive",
  non_local_filesystem: "vaultReasonNonLocalFilesystem",
  volume_info_unavailable: "vaultReasonVolumeInfoUnavailable",
  google_drive: "vaultReasonGoogleDrive",
  sync_folder: "vaultReasonSyncFolder",
  cloud_files: "vaultReasonCloudFiles",
});
export const REASON_TEXT = messageTable(REASON_TEXT_KEYS);

const VERDICT_TEXT_KEYS = Object.freeze({
  no_issue_detected: "vaultVerdictNoIssue",
  needs_approval: "vaultVerdictNeedsApproval",
  rejected: "vaultVerdictRejected",
});
export const VERDICT_TEXT = messageTable(VERDICT_TEXT_KEYS);

/** The text of a location verdict; an unknown verdict is shown as its code. */
export function describeVerdict(verdict) {
  return lookup(VERDICT_TEXT, verdict) ?? String(verdict);
}

export function describeReasons(reasons) {
  return Array.isArray(reasons) ? reasons.map((reason) => lookup(REASON_TEXT, reason) ?? reason) : [];
}

/** Text for the one-line summary at the top of the vault settings. */
export function describeStatus(status) {
  if (!status) return "";
  if (status.overridden) return t("vaultStateOverridden", lookup(STATE_TEXT, status.state) ?? String(status.state));
  return lookup(STATE_TEXT, status.state) ?? t("vaultStateUnknown");
}

/** True when the host answered and no usable vault is active, so the settings tab needs a visit. */
export function vaultNeedsAttention(status) {
  return Boolean(status) && status.state !== "ready";
}

export function formatCounts(counts) {
  if (!counts) return "";
  if (Number.isSafeInteger(counts.pages)) {
    return t("vaultCountsWithPages", formatNumber(counts.visits), formatNumber(counts.snapshots), formatNumber(counts.pages), formatNumber(counts.pageBlobs));
  }
  return t("vaultCounts", formatNumber(counts.visits), formatNumber(counts.snapshots));
}
