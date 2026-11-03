// The options page's model of the privacy policy: the active policy as the host returns it, and the
// draft the person edits. Nothing here judges a URL or writes anything; the host alone does both.
// The draft lives in this page's memory only: it is never stored, so closing the page loses it.

import { SearchError, callHost } from "./search-client.js";

export const CATEGORY_KEYS = ["authentication", "credentials", "government", "webmail", "aiChats"];
export const LAYER_KEYS = ["neverSave", "saveButNoMcp"];
/** The layer whose builtins each category adds to (see docs/issue-010-privacy-policy.md). */
export const LAYER_OF_CATEGORY = {
  authentication: "neverSave",
  credentials: "neverSave",
  government: "saveButNoMcp",
  webmail: "saveButNoMcp",
  aiChats: "saveButNoMcp",
};

// The host's `policy_status` states; any other value is not an answer.
const HOST_STATES = new Set(["ready", "uninitialized", "invalid", "unsupported", "busy", "unavailable"]);
const STATE_OF_CODE = {
  policy_not_initialized: "uninitialized",
  policy_invalid: "invalid",
  policy_unsupported: "unsupported",
  policy_busy: "busy",
  policy_unavailable: "unavailable",
};
// No answer from a host that runs this protocol.
const NO_HOST_CODES = new Set(["host_unavailable", "timeout", "unknown_type"]);
const RULE_KINDS = new Set(["pattern", "regex"]);

const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
const isCount = (value) => Number.isSafeInteger(value) && value >= 1;

function failure(error) {
  const code = error instanceof SearchError ? error.code : "host_unavailable";
  if (Object.hasOwn(STATE_OF_CODE, code)) return { state: STATE_OF_CODE[code], code };
  if (NO_HOST_CODES.has(code)) return { state: "host_unavailable", code };
  if (code === "invalid_response") return { state: "invalid_response", code };
  return { state: "unavailable", code };
}

function isRule(rule, version) {
  if (version === 1) return typeof rule === "string";
  return isObject(rule) && RULE_KINDS.has(rule.kind) && typeof rule.pattern === "string";
}

/** Whether `policy` has every part this page shows, in the form of its version. */
function isPolicyDocument(policy, version) {
  if (!isObject(policy) || !isObject(policy.categories) || !isObject(policy.layers)) return false;
  if (CATEGORY_KEYS.some((key) => typeof policy.categories[key] !== "boolean")) return false;
  return LAYER_KEYS.every((key) => {
    const layer = policy.layers[key];
    return isObject(layer) && typeof layer.enabled === "boolean" && Array.isArray(layer.rules) && layer.rules.every((rule) => isRule(rule, version));
  });
}

function deepFreeze(value) {
  if (value !== null && typeof value === "object") {
    for (const inner of Object.values(value)) deepFreeze(inner);
    Object.freeze(value);
  }
  return value;
}

/** The active policy of a `policy_get` reply, or null unless the reply is complete and consistent. */
export function readActive(reply) {
  const { etag, revision, schemaVersion, engineVersion, policy } = reply ?? {};
  if (typeof etag !== "string" || etag === "" || !isCount(revision) || !isCount(schemaVersion) || !isCount(engineVersion)) return null;
  if (!isObject(policy) || policy.revision !== revision || policy.schemaVersion !== schemaVersion || policy.engineVersion !== engineVersion) return null;
  if (!((schemaVersion === 1 && engineVersion === 1) || (schemaVersion === 2 && engineVersion === 2))) return null;
  if (!isPolicyDocument(policy, schemaVersion)) return null;
  // A copy of its own, frozen: nothing the page does can change what the host said is active.
  return deepFreeze({ etag, revision, schemaVersion, engineVersion, policy: structuredClone(policy) });
}

/**
 * Reads the active policy: `policy_status`, then `policy_get` only if it is ready. Resolves to
 * `{ state: "ready", active, editable }` or `{ state, code }`; it never rejects and never writes.
 */
export async function loadActivePolicy({ send }) {
  try {
    const status = await callHost({ type: "policy_status" }, { send, resultType: "policy_result" });
    if (!HOST_STATES.has(status.state)) return { state: "invalid_response", code: "invalid_response" };
    if (status.state !== "ready") return { state: status.state, code: null };
    const reply = await callHost({ type: "policy_get" }, { send, resultType: "policy_get_result" });
    const active = readActive(reply);
    if (!active) return { state: "invalid_response", code: "invalid_response" };
    // Version 1 is shown but not edited: changing it to 2 is an explicit migration (Slice 3).
    return { state: "ready", active, editable: active.schemaVersion === 2 };
  } catch (error) {
    return failure(error);
  }
}

/** Equal JSON values: the same keys (in any order), arrays in the same order. */
export function sameValue(a, b) {
  if (a === b) return true;
  if (Array.isArray(a) || Array.isArray(b)) {
    return Array.isArray(a) && Array.isArray(b) && a.length === b.length && a.every((item, index) => sameValue(item, b[index]));
  }
  if (!isObject(a) || !isObject(b)) return false;
  const keys = Object.keys(a);
  return keys.length === Object.keys(b).length && keys.every((key) => Object.hasOwn(b, key) && sameValue(a[key], b[key]));
}

/**
 * The person's draft of `active`: a deep copy, so editing it never touches the active policy. It is
 * dirty exactly while it differs from the active policy, so undoing an edit makes it clean again.
 * Fields this page does not show are kept as they came.
 */
export function createDraft(active, { editable }) {
  let draft = structuredClone(active.policy);
  return {
    editable,
    /** A copy of the draft, for display. */
    get policy() {
      return structuredClone(draft);
    },
    category: (key) => draft.categories[key],
    layer: (key) => structuredClone(draft.layers[key]),
    /** Changes the draft only; false if this draft cannot take the change. */
    setCategory(key, value) {
      if (!editable || !CATEGORY_KEYS.includes(key) || typeof value !== "boolean") return false;
      draft.categories[key] = value;
      return true;
    },
    dirty: () => !sameValue(draft, active.policy),
    /** Categories the draft turns off that the active policy has on. */
    weakened: () => CATEGORY_KEYS.filter((key) => active.policy.categories[key] && !draft.categories[key]),
    discard() {
      draft = structuredClone(active.policy);
    },
  };
}
