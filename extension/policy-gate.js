// The service worker's view of the privacy policy. The Native Host alone judges URLs; this file holds
// no rule and evaluates nothing. It separates what must not be confused:
//   allow:     the host's valid policy lets the URL be recorded (this includes save_but_no_mcp)
//   deny:      the host's valid policy refuses the URL, or the URL itself is malformed
//   no answer: no decision exists, so nothing may be recorded, sent, or removed
//
// `evaluate` asks the host before a URL is kept anywhere. The URLs stay in memory, and only the host
// process sees them. An answer is accepted only if it is complete and unambiguous for every URL
// asked, in every part of a split request; otherwise there is no answer for any of them. A decision
// is not a permit: the host judges each event again when it is stored.
//
// `current` asks only whether a policy is usable and which version it is, in memory only, so a
// restarted worker starts from "unknown". A failed check forgets the previous answer.

import { callHost } from "./search-client.js";

export const POLICY_TTL_MS = 30_000;
export const FRESH_REUSE_MS = 1_000;
// As the host's MAX_BATCH_ITEMS, which keeps an answer far below Chrome's 1 MB from a host.
export const MAX_BATCH_ITEMS = 1000;
// Well below Chrome's 64 MiB to a host, which also is the host's own limit.
export const MAX_BATCH_BYTES = 32 * 1024 * 1024;
// The evaluator's limit: longer URLs are never recorded, so they are refused without being sent.
const MAX_URL_LENGTH = 32768;
const ITEM_OVERHEAD_BYTES = 64;
const JUDGEMENT = { allow: "allow", save_but_no_mcp: "allow", never_save: "deny" };

const CODE_OF_STATE = {
  uninitialized: "policy_not_initialized",
  invalid: "policy_invalid",
  busy: "policy_busy",
  unavailable: "policy_unavailable",
  unsupported: "policy_unsupported",
};
const STATE_OF_CODE = Object.fromEntries(Object.entries(CODE_OF_STATE).map(([state, code]) => [code, state]));

function fromResponse(response) {
  if (response.state !== "ready") return { ok: false, code: CODE_OF_STATE[response.state] ?? "invalid_response" };
  if (typeof response.etag !== "string" || response.etag === "") return { ok: false, code: "invalid_response" };
  // The status carries no rule; the extension needs none, as the host alone judges.
  if (!Number.isSafeInteger(response.revision)) return { ok: false, code: "policy_invalid" };
  return { ok: true, version: response.etag, revision: response.revision };
}

/** Lists of URLs for one message each; always at least one, so an empty list still asks the host. */
function splitForHost(urls) {
  const parts = [[]];
  let bytes = 0;
  const encoder = new TextEncoder();
  for (const url of urls) {
    // JSON may write a character in up to 6 bytes; counted as such, to stay on the safe side.
    const size = encoder.encode(JSON.stringify(url)).length * 2 + ITEM_OVERHEAD_BYTES;
    if (parts.at(-1).length >= MAX_BATCH_ITEMS || (parts.at(-1).length > 0 && bytes + size > MAX_BATCH_BYTES)) {
      parts.push([]);
      bytes = 0;
    }
    parts.at(-1).push(url);
    bytes += size;
  }
  return parts;
}

/** The judgement of each url, or null unless the answer covers exactly the ids asked, once each. */
function readBatchAnswer(response, urlOfId) {
  if (typeof response.etag !== "string" || response.etag === "" || !Number.isSafeInteger(response.revision)) return null;
  if (!Array.isArray(response.items) || response.items.length !== urlOfId.size) return null;
  const judged = new Map();
  for (const item of response.items) {
    if (!urlOfId.has(item?.id) || judged.has(item.id) || !Object.hasOwn(JUDGEMENT, item.decision)) return null;
    judged.set(item.id, JUDGEMENT[item.decision]);
  }
  return judged;
}

const failureCode = (error) => (typeof error?.code === "string" ? error.code : "host_unavailable");

export function createPolicyGate({ send, now = () => Date.now(), ttlMs = POLICY_TTL_MS, onChange = () => {} }) {
  let verdict = null;
  let checkedAt = 0;
  let loading = null;
  // A result that was requested before the last change of state must not overwrite it.
  let generation = 0;

  // `strict`: a result that lost to a later change of state gives no decision at all.
  function settle(next, atGeneration, strict = false) {
    if (atGeneration !== generation) {
      if (strict) return { ok: false, code: verdict?.ok === false ? verdict.code : "policy_unavailable" };
      return verdict ?? next;
    }
    const changed = verdict?.ok !== next.ok || verdict?.code !== next.code;
    verdict = next;
    checkedAt = now();
    if (changed) onChange();
    return next;
  }

  function current({ fresh = false, maxAgeMs = fresh ? FRESH_REUSE_MS : ttlMs } = {}) {
    if (verdict?.ok && now() - checkedAt < maxAgeMs) return Promise.resolve(verdict);
    if (loading) return loading;
    const atGeneration = generation;
    const request = callHost({ type: "policy_status" }, { send, resultType: "policy_result" })
      .then(fromResponse, (error) => ({ ok: false, code: failureCode(error) }))
      .then((next) => settle(next, atGeneration))
      .finally(() => {
        if (loading === request) loading = null;
      });
    loading = request;
    return request;
  }

  /**
   * Asks the host about `urls`: `{ ok: true, version, decisions }`, where `decisions` maps each string
   * in `urls` to "allow" or "deny", or `{ ok: false, code }`, which holds for all of them. A list of
   * any length is split as needed, and all parts must come from one version of the policy.
   */
  async function evaluate(urls) {
    const atGeneration = generation;
    const decisions = new Map();
    const asked = new Set();
    for (const url of urls) {
      if (typeof url !== "string") continue;
      if (url.length > MAX_URL_LENGTH) decisions.set(url, "deny");
      else asked.add(url);
    }
    let found = null;
    try {
      for (const part of splitForHost(asked)) {
        const urlOfId = new Map(part.map((url, index) => [String(index), url]));
        const response = await callHost(
          { type: "policy_evaluate_batch", items: [...urlOfId].map(([id, url]) => ({ id, url })) },
          { send, resultType: "policy_evaluate_batch_result" },
        );
        const judged = readBatchAnswer(response, urlOfId);
        if (!judged) return settle({ ok: false, code: "invalid_response" }, atGeneration, true);
        if (found && found.version !== response.etag) return settle({ ok: false, code: "policy_busy" }, atGeneration, true);
        found = { ok: true, version: response.etag, revision: response.revision };
        for (const [id, judgement] of judged) decisions.set(urlOfId.get(id), judgement);
      }
    } catch (error) {
      return settle({ ok: false, code: failureCode(error) }, atGeneration, true);
    }
    const settled = settle(found, atGeneration, true);
    return settled.ok ? { ...settled, decisions } : settled;
  }

  /** The explicit first-use step. The host never replaces an existing policy. */
  async function initialize() {
    generation += 1;
    loading = null;
    const atGeneration = generation;
    let next;
    try {
      next = fromResponse(await callHost({ type: "policy_initialize" }, { send, resultType: "policy_result" }));
    } catch (error) {
      if (failureCode(error) === "policy_already_initialized") return view(await current({ maxAgeMs: 0 }));
      // Whatever happened, the state is unknown until it is read again.
      const after = await current({ maxAgeMs: 0 });
      return after.ok ? view(after) : { ...view(after), code: failureCode(error) };
    }
    return view(settle(next, atGeneration));
  }

  /** Delivery found the policy unusable: stop using the cached one immediately. */
  function reportFailure(code) {
    generation += 1;
    loading = null;
    settle({ ok: false, code }, generation);
  }

  function view(result) {
    if (result.ok) return { state: "ready", revision: result.revision, code: null };
    return { state: STATE_OF_CODE[result.code] ?? "unavailable", revision: null, code: result.code };
  }

  /** The code of the last known failure, without asking the host. */
  const problem = () => (verdict && !verdict.ok ? verdict.code : null);

  return { current, evaluate, initialize, reportFailure, view, problem };
}
