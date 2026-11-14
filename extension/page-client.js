// One Native Host port owns Page cursors/reviews. Restart invalidates them; writes never retry.
export class PageError extends Error { constructor(code) { super(code); this.code = code; } }
const token = (s) => typeof s === "string" && /^[a-f0-9]{64}$/.test(s);
const same = (a, b) => a?.vaultToken === b?.vaultToken && a?.generation === b?.generation;
export function createPageClient({ connect, runtime, onInvalidated = () => {}, timeoutMs = 30_000 }) {
  let port = null, binding = null, closes = 0;
  const pending = new Map();
  function close(code = "host_unavailable", notify = true) {
    const old = port; port = null; binding = null; closes++;
    for (const entry of pending.values()) { clearTimeout(entry.timer); entry.reject(new PageError(code)); }
    pending.clear();
    try { old?.disconnect(); } catch {}
    if (notify) onInvalidated(code);
  }
  function ensurePort() {
    if (port) return;
    try { port = connect(); } catch { throw new PageError("host_unavailable"); }
    const current = port;
    current.onMessage.addListener((response) => {
      if (port !== current) return;
      const entry = pending.get(response?.requestId);
      if (!entry) return; // timed-out/old responses cannot become another request's answer
      pending.delete(response.requestId); clearTimeout(entry.timer);
      if (response.type !== `${entry.type}_result` || typeof response.ok !== "boolean") entry.reject(new PageError("invalid_response"));
      else if (!response.ok) entry.reject(new PageError(typeof response.error === "string" && /^[a-z][a-z0-9_]{0,63}$/.test(response.error) ? response.error : "invalid_response"));
      else if (!token(response.vaultToken) || typeof response.generation !== "string" || !response.generation) entry.reject(new PageError("invalid_response"));
      else entry.resolve(response);
    });
    current.onDisconnect.addListener(() => { void runtime?.lastError; if (port === current) close(); });
  }
  function raw(type, fields = {}, wait = timeoutMs) {
    ensurePort(); const requestId = crypto.randomUUID();
    return new Promise((resolve, reject) => {
      // wait === null: an OS dialog is open for as long as the user likes; only the port ends it.
      const timer = wait === null ? null : setTimeout(() => { pending.delete(requestId); reject(new PageError("timeout")); }, wait);
      pending.set(requestId, { resolve, reject, timer, type });
      try { port.postMessage({ ...fields, type, requestId }); } catch { close(); }
    });
  }
  async function state() {
    const response = await raw("page_state");
    const next = { vaultToken: response.vaultToken, generation: response.generation };
    if (binding && !same(binding, next)) {
      // Invalidate pending UI work before adopting a freshly selected Vault, even A→B→A.
      onInvalidated("vault_switched");
    }
    binding = next; return next;
  }
  async function request(type, fields, wait) {
    if (!binding) await state();
    const expected = { ...binding }, era = closes;
    try {
      const response = await raw(type, { ...fields, ...expected }, wait);
      if (!same(expected, binding) || !same(expected, response)) throw new PageError("vault_switched");
      return response;
    } catch (error) {
      // Only for the connection this request used: a request that was pending when the client was
      // closed (the UI already moved to another Vault) must not tear down its successor.
      if (era === closes && ["vault_switched", "vault_locked", "key_unavailable", "vault_missing", "vault_not_configured", "vault_schema_mismatch", "vault_decrypt_failed"].includes(error.code)) close(error.code);
      throw error;
    }
  }
  return {
    state,
    list: (query, cursor = null) => request("page_list", { query, cursor }),
    history: (url, query, cursor = null) => request("page_history", { url, query, cursor }),
    preview: (target) => request("page_delete_preview", { target }),
    async delete(reviewId) {
      try { return await request("page_delete", { reviewId }); }
      catch (error) {
        if (!["timeout", "host_unavailable", "invalid_response", "vault_switched"].includes(error.code)) throw error;
        // Same live port only. A new Host cannot prove an old COMMIT; never blindly resend.
        if (port && binding) {
          try { const status = await request("page_delete_status", { operationId: reviewId }); if (status.confirmed === true) return status; } catch {}
        }
        throw new PageError("page_delete_unknown");
      }
    },
    // The Host writes the stored original to the file chosen in its OS dialog; no bytes come here.
    // Sent once. If the answer is lost, the file may or may not exist: unknown, never re-sent.
    async export(event) {
      if (!binding) await state();
      let response;
      try { response = await request("page_export", { id: event.id, saveId: event.saveId, sha256: event.sha256 }, null); }
      catch (error) {
        if (["timeout", "host_unavailable", "invalid_response"].includes(error.code)) throw new PageError("page_export_unknown");
        throw error;
      }
      if (response.id !== event.id) throw new PageError("page_export_unknown");
      if (response.cancelled === true) return { cancelled: true };
      if (response.exported !== true || response.sha256 !== event.sha256 || typeof response.fileName !== "string") throw new PageError("page_export_unknown");
      return { cancelled: false, fileName: response.fileName, previousFileLeft: response.previousFileLeft === true };
    },
    close: () => close("vault_switched", false),
    get binding() { return binding && { ...binding }; },
  };
}
