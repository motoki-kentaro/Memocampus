// Reviewed deletion of visits, URLs and images (Issue #7). One Native Host port owns the preview and its
// review id: a restarted Host, another port or another Vault cannot confirm it, and a delete is never sent
// twice. (Page deletion has its own client, page-client.js, with the same rules.)
// `sent`: the request had been handed to the port when this error arose. `fromHost`: the Host itself answered
// ok:false (it rolled back; nothing was deleted by that request).
export class DeleteError extends Error { constructor(code, { sent = false, fromHost = false } = {}) { super(code); this.code = code; this.sent = sent; this.fromHost = fromHost; } }
const TOKEN = /^[a-f0-9]{64}$/;
const same = (a, b) => a?.vaultToken === b?.vaultToken && a?.generation === b?.generation;
// The Host's proof of a COMMIT: its operationId is the review id that was confirmed, and `deleted` is the
// object of counts it deleted. Anything else (missing, mistyped, of another operation) proves nothing.
const isReceipt = (response, reviewId) => response?.operationId === reviewId && typeof response.deleted === "object" && response.deleted !== null && !Array.isArray(response.deleted);
const VAULT_CODES = ["vault_switched", "vault_locked", "key_unavailable", "vault_missing", "vault_not_configured", "vault_schema_mismatch", "vault_decrypt_failed"];

export function createDeleteClient({ connect, runtime, onInvalidated = () => {}, timeoutMs = 30_000 }) {
  let port = null, binding = null, closes = 0;
  const pending = new Map();
  function close(code = "host_unavailable", notify = true) {
    const old = port; port = null; binding = null; closes++;
    // A DELETE already handed to the port may have been committed: the connection ending says nothing about
    // that. The UI is told so (never "nothing was deleted"), and the delete itself ends as unknown (below).
    const deleteInFlight = [...pending.values()].some((entry) => entry.type === "item_delete" && entry.sent);
    for (const entry of pending.values()) { clearTimeout(entry.timer); entry.reject(new DeleteError(code, { sent: entry.sent })); }
    pending.clear();
    try { old?.disconnect(); } catch {}
    if (notify) onInvalidated(deleteInFlight ? "item_delete_unknown" : code);
  }
  function ensurePort() {
    if (port) return;
    try { port = connect(); } catch { throw new DeleteError("host_unavailable"); }
    const current = port;
    current.onMessage.addListener((response) => {
      if (port !== current) return;
      const entry = pending.get(response?.requestId);
      if (!entry) return; // a timed-out or old answer can never become another request's
      pending.delete(response.requestId); clearTimeout(entry.timer);
      if (response.type !== `${entry.type}_result` || typeof response.ok !== "boolean") entry.reject(new DeleteError("invalid_response", { sent: true }));
      else if (!response.ok) {
        const known = typeof response.error === "string" && /^[a-z][a-z0-9_]{0,63}$/.test(response.error);
        entry.reject(new DeleteError(known ? response.error : "invalid_response", { sent: true, fromHost: known }));
      } else if (!TOKEN.test(response.vaultToken ?? "") || typeof response.generation !== "string" || !response.generation) entry.reject(new DeleteError("invalid_response", { sent: true }));
      else entry.resolve(response);
    });
    current.onDisconnect.addListener(() => { void runtime?.lastError; if (port === current) close(); });
  }
  function raw(type, fields = {}) {
    ensurePort(); const requestId = crypto.randomUUID();
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => { pending.delete(requestId); reject(new DeleteError("timeout", { sent: true })); }, timeoutMs);
      const entry = { resolve, reject, timer, type, sent: false };
      pending.set(requestId, entry);
      // `sent` becomes true only once postMessage returned: from then on the Host may have acted.
      // A postMessage that throws never reached the Host.
      try { port.postMessage({ ...fields, type, requestId }); entry.sent = true; } catch { close(); }
    });
  }
  async function state() {
    const response = await raw("item_state");
    const next = { vaultToken: response.vaultToken, generation: response.generation };
    // A new selection (also A to B to A) invalidates whatever the UI holds from the old one.
    if (binding && !same(binding, next)) onInvalidated("vault_switched");
    binding = next; return next;
  }
  async function request(type, fields) {
    if (!binding) await state();
    const expected = { ...binding }, era = closes;
    try {
      const response = await raw(type, { ...fields, ...expected });
      if (era !== closes || !same(expected, binding) || !same(expected, response)) throw new DeleteError("vault_switched");
      return response;
    } catch (error) {
      // Only the connection this request used: an answer that was still pending when the client was
      // closed (the UI already moved on) must not tear down the connection made after it.
      if (era === closes && VAULT_CODES.includes(error.code)) close(error.code);
      throw error;
    }
  }
  const textOf = (value) => typeof value === "string" ? value : "";
  return {
    state,
    /** One URL's visits, newest first, those the list is filtered to. `after`: the `next` of the page before. */
    async history(url, { query = "", visitedFrom = null, visitedTo = null } = {}, after = null) {
      const response = await request("visit_history", { url, query, visitedFrom, visitedTo, after });
      if (!Array.isArray(response.items)) throw new DeleteError("invalid_response");
      const items = response.items.map((item) => ({ id: textOf(item?.id), title: textOf(item?.title), visitedAt: textOf(item?.visitedAt) }));
      if (items.some((item) => item.id === "")) throw new DeleteError("invalid_response");
      const next = response.next && typeof response.next.id === "string" && typeof response.next.visitedAt === "string" ? { id: response.next.id, visitedAt: response.next.visitedAt } : null;
      const c = response.counts ?? {};
      return { items, next, counts: { matched: Number(c.matched) || 0, total: Number(c.total) || 0, snapshots: Number(c.snapshots) || 0 }, vaultToken: response.vaultToken };
    },
    /** What a target is, what deleting it deletes, and the id that confirms exactly that. Changes nothing. */
    async preview(target) {
      const response = await request("item_delete_preview", { target });
      if (typeof response.reviewId !== "string" || !response.counts || typeof response.counts !== "object") throw new DeleteError("invalid_response");
      return response;
    },
    /**
     * Ends as exactly one of: the Host's receipt (deleted), a Host refusal (`fromHost`: nothing deleted),
     * `delete_not_sent` (the request never left: nothing deleted) or `item_delete_unknown` (it was sent and
     * nothing proves the outcome). A lost connection alone is never a reason for "nothing deleted".
     */
    async delete(reviewId) {
      let sentBeforeFailure = false;
      try {
        const response = await request("item_delete", { reviewId });
        // Only the Host's receipt for THIS review counts (item_delete.rs: { operationId = reviewId, deleted: counts }).
        if (!isReceipt(response, reviewId)) throw new DeleteError("invalid_response", { sent: true });
        return response;
      }
      catch (error) {
        if (error.fromHost && error.code !== "invalid_response") throw error;
        sentBeforeFailure = error.sent === true || error.code === "vault_switched" || error.code === "invalid_response";
        if (!sentBeforeFailure) throw new DeleteError("delete_not_sent");
        // The same live port only. A new Host cannot prove an old COMMIT; never resend.
        if (port && binding) {
          try { const status = await request("item_delete_status", { operationId: reviewId }); if (status.confirmed === true && isReceipt(status, reviewId)) return status; } catch {}
        }
        throw new DeleteError("item_delete_unknown", { sent: true });
      }
    },
    close: () => close("vault_switched", false),
    get binding() { return binding && { ...binding }; },
  };
}
