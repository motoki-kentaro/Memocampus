// Page-only protocol. Blob slices stay <=1MiB; no persistent queue, automatic recapture or
// automatic COMMIT retry. A lost COMMIT reply is resolved by a single read-only status request.
import { bytesToBase64 } from "./image-format.js";

export const MAX_PAGE_BYTES = 128 * 1024 * 1024;
export const PAGE_CHUNK_BYTES = 1024 * 1024;
export const PAGE_TIMEOUT_MS = 120_000;
export class PageSaveError extends Error {
  constructor(code) { super(code); this.code = code; }
}

function connection(connect, timeoutMs, signal) {
  const port = connect();
  let waiter = null;
  let closed = false;
  // There is never more than one outstanding request; unsolicited messages fail closed.
  port.onMessage.addListener((message) => {
    if (!waiter) { closed = true; try { port.disconnect(); } catch {} return; }
    const pending = waiter; waiter = null; pending.resolve(message);
  });
  const fail = (code) => {
    closed = true;
    if (waiter) { const pending = waiter; waiter = null; pending.reject(new PageSaveError(code)); }
  };
  port.onDisconnect.addListener(() => fail("transfer_interrupted"));
  const abort = () => { fail(signal.reason?.code ?? "save_cancelled"); try { port.disconnect(); } catch {} };
  signal?.addEventListener("abort", abort, { once: true });
  return {
    async send(message, type) {
      if (signal?.aborted) throw new PageSaveError(signal.reason?.code ?? "save_cancelled");
      if (closed) throw new PageSaveError("transfer_interrupted");
      const answer = await new Promise((resolve, reject) => {
        const timer = setTimeout(() => { fail("timeout"); try { port.disconnect(); } catch {} }, timeoutMs);
        waiter = { resolve: (value) => { clearTimeout(timer); resolve(value); }, reject: (error) => { clearTimeout(timer); reject(error); } };
        try { port.postMessage(message); } catch { fail("transfer_interrupted"); }
      });
      if (answer?.ok === false) {
        // Error codes are never echoed as human-readable raw host text.
        throw new PageSaveError(typeof answer.error === "string" && /^[a-z][a-z0-9_]{0,63}$/.test(answer.error) ? answer.error : "invalid_response");
      }
      if (answer?.type !== type || answer.saveId !== message.saveId) throw new PageSaveError("invalid_response");
      return answer;
    },
    close() { signal?.removeEventListener("abort", abort); try { port.disconnect(); } catch {} },
  };
}

function captureWithin(capture, signal, timeoutMs) {
  return new Promise((resolve, reject) => {
    const abort = () => finish(reject, new PageSaveError(signal.reason?.code ?? "save_cancelled"));
    const timer = setTimeout(() => finish(reject, new PageSaveError("timeout")), timeoutMs);
    function finish(callback, value) { clearTimeout(timer); signal?.removeEventListener("abort", abort); callback(value); }
    signal?.addEventListener("abort", abort, { once: true });
    if (signal?.aborted) { abort(); return; }
    Promise.resolve().then(capture).then((blob) => finish(resolve, blob), () => finish(reject, new PageSaveError("capture_failed")));
  });
}

export async function captureAndUploadPage({ saveId, metadata, capture, verify, connect, signal, timeoutMs = PAGE_TIMEOUT_MS }) {
  let channel = connection(connect, timeoutMs, signal);
  let binding = null;
  let prepared = null;
  let byteLength = null;
  let committing = false;
  const base = (type, extra = {}) => ({ type, saveId, ...binding, ...extra });
  const bind = (answer) => {
    if (!/^[a-f0-9]{64}$/.test(answer?.vaultToken) || typeof answer?.generation !== "string" || !answer.generation) throw new PageSaveError("invalid_response");
    if (!binding) binding = { vaultToken: answer.vaultToken, generation: answer.generation };
    else if (answer.vaultToken !== binding.vaultToken || answer.generation !== binding.generation) throw new PageSaveError("vault_switched");
    return answer;
  };
  try {
    const opened = bind(await channel.send(base("page_save_open", { protocol: 1, metadata }), "page_save_opened"));
    if (opened.chunkBytes !== PAGE_CHUNK_BYTES || opened.maxBytes !== MAX_PAGE_BYTES) throw new PageSaveError("invalid_response");
    await verify();
    const blob = await captureWithin(capture, signal, timeoutMs);
    await verify();
    if (!blob || !Number.isSafeInteger(blob.size) || blob.size < 1 || blob.size > MAX_PAGE_BYTES) throw new PageSaveError("invalid_page_size");
    byteLength = blob.size;
    bind(await channel.send(base("page_save_begin", { byteLength }), "page_save_ready"));
    for (let offset = 0, index = 0; offset < byteLength; offset += PAGE_CHUNK_BYTES, index += 1) {
      if (signal?.aborted) throw new PageSaveError(signal.reason?.code ?? "save_cancelled");
      const bytes = new Uint8Array(await blob.slice(offset, Math.min(offset + PAGE_CHUNK_BYTES, byteLength)).arrayBuffer());
      const expected = Math.min(PAGE_CHUNK_BYTES, byteLength - offset);
      if (bytes.length !== expected) throw new PageSaveError("invalid_page_size");
      const ack = bind(await channel.send(base("page_save_chunk", { index, data: bytesToBase64(bytes) }), "page_save_chunk_ack"));
      if (ack.index !== index || ack.received !== offset + bytes.length) throw new PageSaveError("invalid_response");
    }
    prepared = bind(await channel.send(base("page_save_prepare"), "page_save_prepared"));
    if (!/^[a-f0-9]{64}$/.test(prepared.sha256) || prepared.byteLength !== byteLength) throw new PageSaveError("invalid_response");
    await verify();
    committing = true;
    const saved = bind(await channel.send(base("page_save_commit", { sha256: prepared.sha256 }), "page_saved"));
    if (saved.sha256 !== prepared.sha256 || saved.byteLength !== byteLength) throw new PageSaveError("invalid_response");
    return saved;
  } catch (error) {
    if (!committing) throw error;
    // A negative COMMIT response is definite; a lost/malformed reply may follow a committed write.
    if (!(error instanceof PageSaveError) || !["transfer_interrupted", "timeout", "invalid_response", "vault_switched", "save_cancelled", "navigation_changed"].includes(error.code)) throw error;
    channel.close();
    try {
      // Status is read-only even when the local cancellation signal has fired.
      channel = connection(connect, timeoutMs, null);
      const saved = bind(await channel.send(base("page_save_status", { metadata, byteLength, sha256: prepared.sha256 }), "page_saved"));
      if (saved.sha256 !== prepared.sha256 || saved.byteLength !== byteLength) throw new PageSaveError("invalid_response");
      return saved;
    } catch { throw new PageSaveError("save_state_unknown"); }
  } finally { channel.close(); }
}
