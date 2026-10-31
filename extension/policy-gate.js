// The service worker's view of the privacy policy. It separates three things evaluatePolicy() alone
// cannot tell apart, because that function answers "neverSave" for a failure too:
//   allow:   a valid policy lets the URL be recorded (this includes saveButNoMcp)
//   deny:    a valid policy refuses the URL, or the URL itself is malformed
//   no answer: no valid policy is known, so nothing may be recorded, sent, or removed
//
// Only a document that came from the host and passed validation is cached, in memory only, so a
// restarted worker starts from "unknown". A failed fetch forgets the previous policy. The host
// sends no change notification, so the entrances ask it every time (`maxAgeMs: 0`). The cache only
// spares a queue drain from asking per event: FRESH_REUSE_MS right after an entrance's check, and
// POLICY_TTL_MS during a long drain, where the host still judges every event itself.

import { evaluatePolicy, validatePolicy } from "./policy.js";
import { callHost } from "./search-client.js";

export const POLICY_TTL_MS = 30_000;
export const FRESH_REUSE_MS = 1_000;

const CODE_OF_STATE = {
  uninitialized: "policy_not_initialized",
  invalid: "policy_invalid",
  busy: "policy_busy",
  unavailable: "policy_unavailable",
};
const STATE_OF_CODE = Object.fromEntries(Object.entries(CODE_OF_STATE).map(([state, code]) => [code, state]));

function fromResponse(response) {
  if (response.state !== "ready") return { ok: false, code: CODE_OF_STATE[response.state] ?? "invalid_response" };
  if (typeof response.etag !== "string" || response.etag === "") return { ok: false, code: "invalid_response" };
  try {
    validatePolicy(response.policy);
  } catch {
    return { ok: false, code: "policy_invalid" };
  }
  const { policy } = response;
  return {
    ok: true,
    version: response.etag,
    revision: policy.revision,
    decide: (url) => (evaluatePolicy(policy, url) === "neverSave" ? "deny" : "allow"),
  };
}

const failureCode = (error) => (typeof error?.code === "string" ? error.code : "host_unavailable");

export function createPolicyGate({ send, now = () => Date.now(), ttlMs = POLICY_TTL_MS, onChange = () => {} }) {
  let verdict = null;
  let checkedAt = 0;
  let loading = null;
  // A result that was requested before the last change of state must not overwrite it.
  let generation = 0;

  function settle(next, atGeneration) {
    if (atGeneration !== generation) return verdict ?? next;
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

  return { current, initialize, reportFailure, view, problem };
}
