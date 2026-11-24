// Page UI text. Every text is a message of the extension's catalogs (Issue #77); keep text out of UI logic.
import { formatFullDate } from "./format.js";
import { formatNumber, messageTable, t } from "./i18n.js";
const PAGE_TEXT_KEYS = Object.freeze({
  tab: "pageTabName", search: "pageSearchPlaceholder", refresh: "pageRefresh", more: "pageMore",
  bulk: "pageDeleteMatching", all: "pageDeleteAll", open: "pageOpen", openNote: "pageOpenNote",
  groupDelete: "pageGroupDelete", eventDelete: "pageEventDelete", untitled: "pageUntitled",
  eventExport: "pageEventExport", exportNote: "pageExportNote",
  exporting: "pageExporting", exportCancelled: "pageExportCancelled",
  loading: "pageLoading", empty: "pageEmpty", initial: "pageInitial",
  scopeNote: "pageScopeNote",
  newItems: "pageNewItems", confirmTitle: "pageConfirmTitle", cancel: "pageCancel", confirm: "pageConfirm",
  deleting: "pageDeleting", deleted: "pageDeleted", retry: "pageRetry", close: "pageClose",
});
export const PAGE_TEXT = messageTable(PAGE_TEXT_KEYS);
// Error codes stay the protocol's; only the text is looked up.
const PAGE_ERROR_TEXT_KEYS = Object.freeze({
  vault_locked: "pageErrorVaultLocked",
  key_unavailable: "pageErrorKeyUnavailable", vault_not_configured: "pageErrorVaultNotConfigured",
  vault_switched: "pageErrorVaultSwitched",
  page_targets_changed: "pageErrorTargetsChanged",
  page_list_changed: "pageErrorListChanged",
  invalid_cursor: "pageErrorInvalidCursor", invalid_review: "pageErrorInvalidReview",
  page_not_found: "pageErrorNotFound", timeout: "pageErrorTimeout",
  host_unavailable: "pageErrorHostUnavailable", unknown_type: "pageErrorUnknownType",
  vault_schema_mismatch: "pageErrorSchemaMismatch",
  page_delete_unknown: "pageErrorDeleteUnknown",
  page_export_changed: "pageErrorExportChanged",
  page_export_unknown: "pageErrorExportUnknown",
  page_blob_corrupt: "pageErrorBlobCorrupt",
  export_file_appeared: "pageErrorExportFileAppeared",
  export_target_busy: "pageErrorExportTargetBusy",
  export_target_readonly: "pageErrorExportTargetReadonly",
  export_invalid_target: "pageErrorExportInvalidTarget",
  parent_missing: "pageErrorParentMissing",
  export_failed: "pageErrorExportFailed",
  export_verify_failed: "pageErrorExportVerifyFailed",
  export_recovery_needed: "pageErrorExportRecoveryNeeded",
  invalid_query: "pageErrorInvalidQuery", page_response_too_large: "pageErrorResponseTooLarge",
});
export const pageErrorText = (code) => t(typeof code === "string" && Object.hasOwn(PAGE_ERROR_TEXT_KEYS, code) ? PAGE_ERROR_TEXT_KEYS[code] : "pageErrorGeneric");
export const pageDate = (ms) => { try { return formatFullDate(new Date(ms).toISOString()); } catch { return "—"; } };
export const pageBytes = (n) => `${formatNumber(Number(n))} bytes`;
export const historyLabel = (item) => t("pageHistoryLabel", formatNumber(item.matchedCount), formatNumber(item.eventCount));
export const summaryText = (n) => t("pageSummary", formatNumber(n));
export const exportedText = (result) => t(result.previousFileLeft ? "pageExportedOldLeft" : "pageExported", result.fileName);
/** The Page confirmation as the sections of delete-ui.js's renderDeletion (name next to value; the web's text only as text). */
export function deletionModel(preview) {
  const { target, counts, event } = preview;
  const num = (v) => t("pageCount", formatNumber(Number(v)));
  const targetRows = target.scope === "event" ? [{ label: t("pageRowTarget"), value: t("pageTargetEvent") }, { label: t("pageRowDateTime"), value: pageDate(event.savedAt) }, { label: t("pageRowTitle"), value: event.title, long: true }, { label: "URL", value: event.url, long: true }]
    : target.scope === "url" ? [{ label: t("pageRowTarget"), value: t("pageTargetUrl") }, { label: "URL", value: target.url, long: true }]
    : target.query === "" ? [{ label: t("pageRowTarget"), value: t("pageTargetAll") }]
    : [{ label: t("pageRowTarget"), value: t("pageTargetMatching") }, { label: t("pageRowQuery"), value: target.query, long: true }];
  return {
    target: targetRows,
    effects: [{ label: t("pageRowEvents"), value: num(counts.events) }, { label: "URL", value: num(counts.urls) }, { label: t("pageRowBlobsRemoved"), value: num(counts.blobsRemoved) }, { label: t("pageRowBlobsKept"), value: num(counts.blobsKept) }, { label: t("pageRowBytesRemoved"), value: pageBytes(counts.bytesRemoved) }],
    notes: [...(target.scope === "event" || target.scope === "url" || target.query === "" ? [] : [t("pageNoteDbScope")]), t("pageNoteSharedKept")],
  };
}
