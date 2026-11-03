// The options page's model of the privacy policy: the active policy as the host returns it, the
// draft the person edits, and the messages that check, test and apply a draft. Nothing here judges a
// URL or a rule: the host alone does, and only `policy_apply` writes. The draft lives in this page's
// memory only: it is never stored, so closing the page loses it.

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
export const RULE_KINDS = ["pattern", "regex"];
export const DECISIONS = ["never_save", "save_but_no_mcp", "allow"];

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

const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
const isCount = (value) => Number.isSafeInteger(value) && value >= 1;
const codeOf = (error) => (error instanceof SearchError ? error.code : "host_unavailable");

function failure(error) {
  const code = codeOf(error);
  if (Object.hasOwn(STATE_OF_CODE, code)) return { state: STATE_OF_CODE[code], code };
  if (NO_HOST_CODES.has(code)) return { state: "host_unavailable", code };
  if (code === "invalid_response") return { state: "invalid_response", code };
  return { state: "unavailable", code };
}

function isRule(rule, version) {
  if (version === 1) return typeof rule === "string";
  return isObject(rule) && RULE_KINDS.includes(rule.kind) && typeof rule.pattern === "string";
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

const ruleKey = (rule) => (typeof rule === "string" ? `pattern\n${rule}` : `${rule.kind}\n${rule.pattern}`);

/**
 * The changes from `active` to `draft` that may protect less: a category turned off, a layer
 * disabled, or a rule of the active policy that the draft no longer has as it was (removed, its
 * text or its kind changed). A plain comparison: it says "may", never that protection is lost, and
 * it never looks at what a rule means.
 */
export function relaxations(active, draft) {
  const found = [];
  for (const key of CATEGORY_KEYS) {
    if (active.categories[key] && !draft.categories[key]) found.push({ type: "category", key });
  }
  for (const key of LAYER_KEYS) {
    if (active.layers[key].enabled && !draft.layers[key].enabled) found.push({ type: "layer", key });
  }
  for (const key of LAYER_KEYS) {
    const left = new Map();
    for (const rule of draft.layers[key].rules) left.set(ruleKey(rule), (left.get(ruleKey(rule)) ?? 0) + 1);
    let count = 0;
    for (const rule of active.layers[key].rules) {
      const kept = left.get(ruleKey(rule)) ?? 0;
      if (kept > 0) left.set(ruleKey(rule), kept - 1);
      else count += 1;
    }
    if (count > 0) found.push({ type: "rules", key, count });
  }
  return found;
}

/**
 * The person's draft of `active`: a deep copy, so editing it never touches the active policy. It is
 * dirty exactly while it differs from the active policy, so undoing an edit makes it clean again.
 * Fields this page does not show are kept as they came. `version` counts the changes, so an answer
 * about an earlier draft can be told from one about this draft.
 */
export function createDraft(active, { editable }) {
  let draft = structuredClone(active.policy);
  let version = 0;
  const changed = () => {
    version += 1;
    return true;
  };
  const rulesOf = (layerKey) => (LAYER_KEYS.includes(layerKey) ? draft.layers[layerKey].rules : null);
  const isIndex = (rules, index) => Number.isInteger(index) && index >= 0 && index < rules.length;
  return {
    editable,
    get version() {
      return version;
    },
    /** A copy of the draft, for display and for sending. */
    get policy() {
      return structuredClone(draft);
    },
    category: (key) => draft.categories[key],
    layer: (key) => structuredClone(draft.layers[key]),
    layerEnabled: (key) => draft.layers[key].enabled,
    /** Each change below changes the draft only; false (or -1) if this draft cannot take it. */
    setCategory(key, value) {
      if (!editable || !CATEGORY_KEYS.includes(key) || typeof value !== "boolean") return false;
      draft.categories[key] = value;
      return changed();
    },
    setLayerEnabled(layerKey, enabled) {
      if (!editable || !rulesOf(layerKey) || typeof enabled !== "boolean") return false;
      // The rules stay: a disabled layer keeps them, and the host still checks them.
      draft.layers[layerKey].enabled = enabled;
      return changed();
    },
    /** Adds `{ kind: "pattern", pattern: "" }` at the end of the layer; returns its index. */
    addRule(layerKey) {
      const rules = rulesOf(layerKey);
      if (!editable || !rules) return -1;
      rules.push({ kind: "pattern", pattern: "" });
      changed();
      return rules.length - 1;
    },
    /** Sets the kind or the text of one rule. The text is kept as written, whatever the kind. */
    setRule(layerKey, index, { kind, pattern }) {
      const rules = rulesOf(layerKey);
      if (!editable || !rules || !isIndex(rules, index)) return false;
      if (kind !== undefined && !RULE_KINDS.includes(kind)) return false;
      if (pattern !== undefined && typeof pattern !== "string") return false;
      const rule = rules[index];
      if ((kind ?? rule.kind) === rule.kind && (pattern ?? rule.pattern) === rule.pattern) return true;
      if (kind !== undefined) rule.kind = kind;
      if (pattern !== undefined) rule.pattern = pattern;
      return changed();
    },
    deleteRule(layerKey, index) {
      const rules = rulesOf(layerKey);
      if (!editable || !rules || !isIndex(rules, index)) return false;
      rules.splice(index, 1);
      return changed();
    },
    dirty: () => !sameValue(draft, active.policy),
    /** Categories the draft turns off that the active policy has on. */
    weakened: () => CATEGORY_KEYS.filter((key) => active.policy.categories[key] && !draft.categories[key]),
    relaxations: () => relaxations(active.policy, draft),
    discard() {
      draft = structuredClone(active.policy);
      changed();
    },
  };
}

// --- Checking, testing and applying a draft -------------------------------------------------------

/** The `errors` of a refused draft, each `{ layer, ruleIndex, code }` with only what is well formed. */
function problemsOf(error) {
  const errors = error instanceof SearchError ? error.response?.errors : null;
  if (!Array.isArray(errors)) return [];
  return errors.filter(isObject).map((problem) => ({
    layer: LAYER_KEYS.includes(problem.layer) ? problem.layer : null,
    ruleIndex: Number.isSafeInteger(problem.ruleIndex) && problem.ruleIndex >= 0 ? problem.ruleIndex : null,
    code: typeof problem.code === "string" ? problem.code : "invalid_policy",
  }));
}

const refused = (error) => ({ ok: false, code: codeOf(error), problems: problemsOf(error) });

/** Whether the host could apply `policy` as it is. Resolves to `{ ok }` or `{ ok: false, code, problems }`. */
export async function validateDraft({ send, policy }) {
  try {
    const reply = await callHost({ type: "policy_validate", policy }, { send, resultType: "policy_validate_result" });
    if (!isCount(reply.schemaVersion) || !isCount(reply.engineVersion)) return { ok: false, code: "invalid_response", problems: [] };
    return { ok: true };
  } catch (error) {
    return refused(error);
  }
}

/** Whether one rule alone matches `url`, as the host judges it. A match, never a decision. */
export async function testRule({ send, rule, url }) {
  try {
    const reply = await callHost({ type: "policy_test_rule", rule, url }, { send, resultType: "policy_test_rule_result" });
    if (typeof reply.matched !== "boolean") return { ok: false, code: "invalid_response" };
    return { ok: true, matched: reply.matched };
  } catch (error) {
    return { ok: false, code: codeOf(error) };
  }
}

/** The decision the draft `policy` would give `url`, as the host judges it. It permits nothing. */
export async function testUrl({ send, policy, url }) {
  try {
    const reply = await callHost({ type: "policy_test_url", policy, url }, { send, resultType: "policy_test_url_result" });
    if (!DECISIONS.includes(reply.decision)) return { ok: false, code: "invalid_response", problems: [] };
    return { ok: true, decision: reply.decision };
  } catch (error) {
    return refused(error);
  }
}

// Answers after which the host has written nothing: it refused before replacing the file. Any other
// failure (no answer, a lost answer, a malformed one, or an error that may follow the write) leaves
// the outcome unknown until the active policy is read again.
const REFUSED_BEFORE_WRITE = new Set([
  "policy_conflict",
  "invalid_etag",
  "policy_version_mismatch",
  "invalid_policy_draft",
  "policy_draft_too_large",
  "policy_not_initialized",
  "unknown_type",
  "invalid_request_id",
]);

/**
 * Sends `policy_apply` once; never again by itself. Resolves to
 * - `{ outcome: "applied", etag, revision }`: the host replaced the policy;
 * - `{ outcome: "refused", code, problems }`: the host answered and wrote nothing;
 * - `{ outcome: "unknown", code }`: the policy may or may not have been replaced.
 */
export async function applyDraft({ send, expectedEtag, policy }) {
  try {
    const reply = await callHost({ type: "policy_apply", expectedEtag, policy }, { send, resultType: "policy_apply_result" });
    if (reply.state !== "ready" || typeof reply.etag !== "string" || reply.etag === "" || !isCount(reply.revision)) {
      return { outcome: "unknown", code: "invalid_response" };
    }
    return { outcome: "applied", etag: reply.etag, revision: reply.revision };
  } catch (error) {
    const code = codeOf(error);
    const problems = problemsOf(error);
    // A draft the host found invalid, with where, was refused before anything was written.
    if (REFUSED_BEFORE_WRITE.has(code) || problems.length > 0) return { outcome: "refused", code, problems };
    return { outcome: "unknown", code };
  }
}

/**
 * What the active policy `loaded` says about an apply of `submitted` in place of `expectedEtag`
 * whose answer was lost:
 * - `"not_applied"`: the policy that was replaced is still the stored one;
 * - `"applied"`: the stored policy is exactly the draft, at the next revision;
 * - `"changed"`: the stored policy is another one, so someone else may have changed it.
 */
export function compareAfterApply({ expectedEtag, submitted, loaded }) {
  if (loaded.etag === expectedEtag && loaded.revision === submitted.revision) return "not_applied";
  if (loaded.revision === submitted.revision + 1 && sameValue({ ...loaded.policy, revision: submitted.revision }, submitted)) return "applied";
  return "changed";
}
