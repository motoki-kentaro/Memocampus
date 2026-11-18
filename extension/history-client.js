// The Native Host's History API (history.rs, Slice 1) for the full-page History: capability, list, days of a
// month, one visit. Built on search-client.js (`callHost`: one request id per call, the answer must carry it,
// a fixed error code otherwise). Every answer is validated; anything else is `invalid_response`, never a guess.

import { SearchError, callHost } from "./search-client.js";

export const CAPABILITY_HISTORY = "history_v1";
const TOKEN = /^[0-9a-f]{64}$/;
const isCount = (value) => Number.isSafeInteger(value) && value >= 0;

function bad() {
  return new SearchError("invalid_response");
}

/**
 * The one answer an old host gives to a type it does not know: `{ ok: false, eventId: null, error: "unknown_type" }`
 * (no request id, no result type). Exactly that shape, nothing looser.
 */
export function isLegacyUnknownType(response) {
  if (!response || typeof response !== "object" || Array.isArray(response)) return false;
  const keys = Object.keys(response);
  return response.ok === false && response.error === "unknown_type" && response.eventId === null && keys.every((key) => key === "ok" || key === "eventId" || key === "error");
}

/**
 * Whether the host names `history_v1`. A host that does not know `host_capabilities` answers `unknown_type`, in the
 * tagged form or in the legacy one above: not supported, not an error. Any other malformed answer stays an error.
 */
export async function hostSupportsHistory(options) {
  let raw;
  const send = async (message) => (raw = await options.send(message));
  try {
    const response = await callHost({ type: "host_capabilities" }, { ...options, send, resultType: "host_capabilities_result" });
    return Array.isArray(response.capabilities) && response.capabilities.includes(CAPABILITY_HISTORY);
  } catch (error) {
    if (error instanceof SearchError && error.code === "unknown_type") return false;
    if (error instanceof SearchError && error.code === "invalid_response" && isLegacyUnknownType(raw)) return false;
    throw error;
  }
}

function checkedItem(item) {
  if (!item || typeof item !== "object") throw bad();
  const { id, visitedAt, url, title, host, urlTruncated, titleTruncated } = item;
  if (typeof id !== "string" || id === "" || typeof visitedAt !== "string" || typeof url !== "string" || typeof title !== "string" || typeof host !== "string") throw bad();
  return { id, visitedAt, url, title, host, urlTruncated: urlTruncated === true, titleTruncated: titleTruncated === true };
}

/**
 * One page of one local day's visits. `visitedFrom` / `visitedTo` are the UTC instants of the day's start and of
 * the next day's start. Resolves to `{ items, total, page, size, pages, vaultToken }`.
 */
export async function historyList({ visitedFrom, visitedTo, domain, mode, page, size }, options) {
  const response = await callHost({ type: "history_list", visitedFrom, visitedTo, domain, mode, page, size }, { ...options, resultType: "history_list_result" });
  const { items, total, vaultToken } = response;
  if (!Array.isArray(items) || !isCount(total) || !isCount(response.pages) || !isCount(response.page) || !isCount(response.size) || !TOKEN.test(vaultToken ?? "")) throw bad();
  if (response.page !== page || response.size !== size || items.length > size || response.pages !== Math.ceil(total / size)) throw bad();
  return { items: items.map(checkedItem), total, page: response.page, size: response.size, pages: response.pages, vaultToken };
}

/**
 * The visits per local day of a month matching a domain condition: `bounds` as visit-calendar.js's `monthBounds`
 * makes them. Resolves to `{ counts, total, vaultToken }`, one count per day.
 */
export async function historyDays({ bounds, domain, mode }, options) {
  const response = await callHost({ type: "history_days", bounds, domain, mode }, { ...options, resultType: "history_days_result" });
  const { counts, vaultToken } = response;
  if (!Array.isArray(counts) || counts.length !== bounds.length - 1 || !counts.every(isCount) || !TOKEN.test(vaultToken ?? "")) throw bad();
  return { counts: [...counts], total: counts.reduce((a, b) => a + b, 0), vaultToken };
}

const SOURCE_STATES = new Set(["present", "censored", "unknown"]);
const optionalText = (value) => (typeof value === "string" ? value : null);

function checkedTarget(item) {
  const base = checkedItem(item);
  return { ...base, kind: optionalText(item.kind), transitionType: optionalText(item.transitionType), occurredAt: optionalText(item.occurredAt) };
}

function checkedNavigation(value) {
  if (value === null) return null;
  if (!value || typeof value !== "object" || Array.isArray(value)) throw bad();
  return { kind: optionalText(value.kind), transitionType: optionalText(value.transitionType), occurredAt: optionalText(value.occurredAt) };
}

/**
 * `source.state` is one of three, and only `present` carries a visit. A `censored` or `unknown` source keeps nothing
 * but its state (whatever else the answer carries is dropped); any other state is `invalid_response`, never guessed.
 */
function checkedSource(value) {
  if (!value || typeof value !== "object" || !SOURCE_STATES.has(value.state)) throw bad();
  if (value.state === "present") return { state: "present", visit: checkedItem(value.visit) };
  return { state: value.state };
}

function checkedMetadata(value) {
  if (value === null) return null;
  if (!value || typeof value !== "object" || Array.isArray(value)) throw bad();
  const { title, description, ogTitle, ogDescription, ogSiteName, ogImageUrl, faviconUrl, capturedAt } = value;
  return { title: optionalText(title), description: optionalText(description), ogTitle: optionalText(ogTitle), ogDescription: optionalText(ogDescription), ogSiteName: optionalText(ogSiteName), ogImageUrl: optionalText(ogImageUrl), faviconUrl: optionalText(faviconUrl), capturedAt: optionalText(capturedAt) };
}

/**
 * One visit in full: the visit with its whole URL and title, how it was reached (`navigation`, `source`), the visits
 * reached from it (`targets`, at most the host's cap, with `targetCount` the real total) and the page metadata
 * recorded for this very visit. `url` and `vaultToken` stay at the top for callers that only need the URL.
 */
export async function historyVisit(id, options) {
  const response = await callHost({ type: "history_visit", id }, { ...options, resultType: "history_visit_result" });
  const visit = response.visit;
  if (!visit || visit.id !== id || typeof visit.url !== "string" || typeof visit.title !== "string" || typeof visit.visitedAt !== "string" || !TOKEN.test(response.vaultToken ?? "")) throw bad();
  const { targets, targetCount, targetsTruncated } = response;
  if (!Array.isArray(targets) || !isCount(targetCount) || targetCount < targets.length || typeof targetsTruncated !== "boolean") throw bad();
  return {
    id,
    url: visit.url,
    vaultToken: response.vaultToken,
    visit: { id, url: visit.url, title: visit.title, visitedAt: visit.visitedAt, host: typeof visit.host === "string" ? visit.host : "" },
    navigation: checkedNavigation(response.navigation ?? null),
    source: checkedSource(response.source),
    targets: targets.map(checkedTarget),
    targetCount,
    targetsTruncated,
    pageMetadata: checkedMetadata(response.pageMetadata ?? null),
  };
}
