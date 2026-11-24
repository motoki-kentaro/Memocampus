// Display text for queue results. "Queued" and "stored in SQLite" are always reported separately.
import { pluralKey, t } from "./i18n.js";

export function describeRecordResult(result) {
  if (!result?.ok) return t("recordFailed", result?.error || t("recordFailedDetail"));
  if (result.queued === 0) return t("recordNoTabs");
  if (result.stored === result.queued) return t(pluralKey(result.stored, { one: "recordStoredAllOne", other: "recordStoredAllOther" }), result.stored);
  const lines = [t(pluralKey(result.queued, { one: "recordQueuedOne", other: "recordQueuedOther" }), result.queued), t("recordStoredCount", result.stored)];
  if (result.pending > 0) {
    lines.push(t("recordPendingCount", result.pending));
    if (result.reason) lines.push(t("recordPendingReason", result.reason));
  }
  if (result.deadLettered > 0) lines.push(t("recordDeadLetteredCount", result.deadLettered));
  return lines.join("\n");
}

/** The policy state for the side panel, or "" when automatic recording is running. */
export function describePolicyStatus(status) {
  if (status?.ok !== true) return t("policyStatusUnknown");
  return status.state === "ready" ? "" : status.message || t("policyStatusStopped");
}

/** One line for the side panel, or "" when nothing needs attention. */
export function describeQueueStatus(status) {
  if (!status?.ok) return "";
  const parts = [];
  if (status.pending > 0) parts.push(status.blockedReason ? t("queuePendingReason", status.pending, status.blockedReason) : t("queuePending", status.pending));
  if (status.deadLetters > 0) parts.push(t("queueDeadLetters", status.deadLetters));
  if (status.dropped > 0) parts.push(t("queueDropped", status.dropped));
  if (status.navigationInvalid > 0) parts.push(t("queueNavigationInvalid", status.navigationInvalid));
  if (status.navigationNotStored > 0) parts.push(t("queueNavigationNotStored", status.navigationNotStored));
  return parts.join("\n");
}
