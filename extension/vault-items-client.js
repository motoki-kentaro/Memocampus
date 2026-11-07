// Requests for managing one item (Issue #43): policy changes for one URL, and deleting one URL or one
// image. The host judges and decides everything; this module only sends, and checks that an answer
// has the form it promises before anything is shown from it. See docs/issue-043-vault-item-management.md.

import { SearchError, callHost } from "./search-client.js";

export const DECISIONS = ["allow", "save_but_no_mcp", "never_save"];
export const POLICY_ACTIONS = ["never_save", "save_but_no_mcp", "publish"];
const LAYERS = ["neverSave", "saveButNoMcp"];
const RULE_KINDS = ["pattern", "regex", "domainWildcard", "exact"];
const TOKEN = /^[0-9a-f]{64}$/;
/** The host judges at most this many URLs in one `policy_evaluate_batch`. */
export const EVALUATE_BATCH = 1000;

const count = (value) => Number.isSafeInteger(value) && value >= 0;
const invalid = () => new SearchError("invalid_response");

function decision(value) {
  if (!DECISIONS.includes(value)) throw invalid();
  return value;
}

function token(value) {
  if (typeof value !== "string" || !TOKEN.test(value)) throw invalid();
  return value;
}

function changes(value) {
  if (!Array.isArray(value)) throw invalid();
  return value.map((change) => {
    const rule = change?.rule;
    if (!["add", "remove"].includes(change?.op) || !LAYERS.includes(change.layer) || !RULE_KINDS.includes(rule?.kind) || typeof rule.pattern !== "string") throw invalid();
    if (change.ruleIndex !== undefined && !count(change.ruleIndex)) throw invalid();
    return { op: change.op, layer: change.layer, rule: { kind: rule.kind, pattern: rule.pattern }, ruleIndex: change.ruleIndex ?? null };
  });
}

function blockers(value) {
  if (value === undefined) return [];
  if (!Array.isArray(value)) throw invalid();
  return value.map((blocker) => {
    if (!LAYERS.includes(blocker?.layer)) throw invalid();
    if (blocker.builtin === true) return { layer: blocker.layer, builtin: true };
    if (!count(blocker.ruleIndex) || !RULE_KINDS.includes(blocker.kind)) throw invalid();
    return { layer: blocker.layer, ruleIndex: blocker.ruleIndex, kind: blocker.kind };
  });
}

function sameRequest(response, request) {
  if (response.url !== request.url || response.action !== request.action || response.scope !== request.scope) throw invalid();
}

/**
 * What `action` would do for `url` (scope `url`: that URL only; `host`: its whole host), as the host
 * judges it with the stored policy. Changes nothing.
 */
export async function previewPolicyItem({ url, action, scope = "url" }, { send }) {
  const response = await callHost({ type: "policy_item_preview", url, action, scope }, { send, resultType: "policy_item_preview_result" });
  sameRequest(response, { url, action, scope });
  if (typeof response.etag !== "string" || response.etag === "" || typeof response.executable !== "boolean" || typeof response.affectsOtherUrls !== "boolean") throw invalid();
  if (!response.executable && (typeof response.reason !== "string" || response.reason === "")) throw invalid();
  return {
    url,
    action,
    scope,
    etag: response.etag,
    current: decision(response.current),
    expected: decision(response.expected),
    changes: changes(response.changes),
    affectsOtherUrls: response.affectsOtherUrls,
    executable: response.executable,
    reason: response.executable ? null : response.reason,
    blockers: blockers(response.blockers),
  };
}

/** The previewed change, only if the stored policy is still the one of `etag` (else `policy_conflict`). */
export async function applyPolicyItem({ url, action, scope = "url", etag }, { send }) {
  const response = await callHost({ type: "policy_item_apply", url, action, scope, expectedEtag: etag }, { send, resultType: "policy_item_apply_result" });
  sameRequest(response, { url, action, scope });
  if (typeof response.changed !== "boolean") throw invalid();
  return { previous: decision(response.previous), decision: decision(response.decision), changed: response.changed, changes: changes(response.changes) };
}

/**
 * The host's decision for each URL, as recording and MCP use it: a Map from URL to `allow`,
 * `save_but_no_mcp` or `never_save`. Asked in batches the host takes; all or nothing.
 */
export async function evaluateUrls(urls, { send }) {
  const unique = [...new Set(urls)];
  const decisions = new Map();
  for (let start = 0; start < unique.length; start += EVALUATE_BATCH) {
    const part = unique.slice(start, start + EVALUATE_BATCH);
    const items = part.map((url, index) => ({ id: `u${index}`, url }));
    const response = await callHost({ type: "policy_evaluate_batch", items }, { send, resultType: "policy_evaluate_batch_result" });
    if (!Array.isArray(response.items) || response.items.length !== items.length) throw invalid();
    response.items.forEach((item, index) => {
      if (item?.id !== items[index].id) throw invalid();
      decisions.set(part[index], decision(item.decision));
    });
  }
  return decisions;
}

const URL_COUNTS = ["visits", "tabSnapshots", "pageMetadata", "navigations", "navigationSourcesCleared", "imageSavesKept"];
const IMAGE_COUNTS = ["saves", "thumbnails", "tagLinks", "tagsRemoved"];

function counts(value, keys) {
  if (!keys.every((key) => count(value?.[key]))) throw invalid();
  return Object.fromEntries(keys.map((key) => [key, value[key]]));
}

/** What deleting `url` would delete, in the active vault, and which vault that is. Changes nothing. */
export async function previewUrlDelete(url, { send }) {
  const response = await callHost({ type: "url_delete_preview", url }, { send, resultType: "url_delete_preview_result" });
  if (response.url !== url || response.irreversible !== true) throw invalid();
  return { vaultToken: token(response.vaultToken), counts: counts(response.counts, URL_COUNTS) };
}

/** Deletes everything stored of `url`, only in the vault named by `vaultToken`. */
export async function deleteUrl(url, vaultToken, { send }) {
  const response = await callHost({ type: "url_delete", url, vaultToken }, { send, resultType: "url_delete_result" });
  if (response.url !== url || response.vaultToken !== vaultToken || response.irreversible !== true) throw invalid();
  return counts(response.deleted, URL_COUNTS);
}

/** What deleting the image `item` (its id and bytes) would delete, and in which vault. */
export async function previewImageDelete(item, { send }) {
  const response = await callHost({ type: "image_delete_preview", blobId: item.blobId, sha256: item.sha256 }, { send, resultType: "image_delete_preview_result" });
  if (response.blobId !== item.blobId || response.sha256 !== item.sha256 || response.irreversible !== true || !count(response.byteLength)) throw invalid();
  return { vaultToken: token(response.vaultToken), byteLength: response.byteLength, counts: counts(response.counts, IMAGE_COUNTS) };
}

/** Deletes the image `item`, only if it still has its bytes, only in the vault of `vaultToken`. */
export async function deleteImage(item, vaultToken, { send }) {
  const response = await callHost({ type: "image_delete", blobId: item.blobId, sha256: item.sha256, vaultToken }, { send, resultType: "image_delete_result" });
  if (response.blobId !== item.blobId || response.sha256 !== item.sha256 || response.vaultToken !== vaultToken || response.irreversible !== true) throw invalid();
  return counts(response.deleted, IMAGE_COUNTS);
}
