// Uploads one image over a chrome.runtime.connectNative port (see native-host/src/image_protocol.rs).
// Success means the host answered `image_saved`, which it sends only after the SQLite COMMIT.

import { ImageSaveError, bytesToBase64 } from "./image-format.js";

export const PROTOCOL_VERSION = 2;
export const MAX_CHUNK_BYTES = 1024 * 1024;
export const STEP_TIMEOUT_MS = 30_000;
/** Ports opened for one save action at most (first try + thumbnail-less retry + one reconnect). */
export const MAX_ATTEMPTS = 3;

const VAULT_TOKEN = /^[0-9a-f]{64}$/;

/**
 * The vault a save action is bound to: fixed by the host's first answer and sent with every later
 * attempt, so a reconnect that finds another vault active fails instead of saving there.
 */
function bindVault(state, message) {
  const token = message?.vaultToken;
  if (typeof token !== "string" || !VAULT_TOKEN.test(token)) throw new ImageSaveError("invalid_response");
  if (state.vaultToken === null) state.vaultToken = token;
  else if (state.vaultToken !== token) throw new ImageSaveError("vault_switched");
}

/** Turns a port into "wait for the next message" calls that fail on disconnect or timeout. */
function portReader(port, runtime) {
  const queue = [];
  let waiting = null;
  let closed = null;
  let received = 0;
  port.onMessage.addListener((message) => {
    received += 1;
    if (waiting) {
      const { resolve } = waiting;
      waiting = null;
      resolve(message);
    } else {
      queue.push(message);
    }
  });
  port.onDisconnect.addListener(() => {
    const text = runtime?.lastError?.message || "";
    // Nothing ever arrived and Chrome could not start the host: it is missing or not registered.
    closed = received === 0 && /not found|forbidden|not allowed/i.test(text) ? "host_unavailable" : "transfer_interrupted";
    if (waiting) {
      const { reject } = waiting;
      waiting = null;
      reject(new ImageSaveError(closed));
    }
  });
  return {
    next(timeoutMs) {
      if (queue.length) return Promise.resolve(queue.shift());
      if (closed) return Promise.reject(new ImageSaveError(closed));
      return new Promise((resolve, reject) => {
        const timer = setTimeout(() => {
          waiting = null;
          reject(new ImageSaveError("timeout"));
        }, timeoutMs);
        waiting = {
          resolve: (message) => {
            clearTimeout(timer);
            resolve(message);
          },
          reject: (error) => {
            clearTimeout(timer);
            reject(error);
          },
        };
      });
    },
  };
}

function expect(message, type, saveId) {
  if (message?.type === "image_save_error" || message?.ok === false) {
    // An older host without image support answers unknown types like this.
    if (message.error === "unknown_type") throw new ImageSaveError("host_outdated");
    throw new ImageSaveError(typeof message.error === "string" ? message.error : "invalid_response", { fromHost: true });
  }
  if (message?.type !== type || message.saveId !== saveId) throw new ImageSaveError("invalid_response");
  return message;
}

async function attempt(image, { connect, runtime, timeoutMs, withThumbnail, state }) {
  const port = connect();
  const reader = portReader(port, runtime);
  const { saveId } = image;
  try {
    port.postMessage({
      type: "image_save_begin",
      protocol: PROTOCOL_VERSION,
      saveId,
      sha256: image.sha256,
      byteLength: image.bytes.length,
      mimeType: image.mimeType,
      width: image.width,
      height: image.height,
      imageUrl: image.imageUrl,
      pageUrl: image.pageUrl ?? null,
      frameUrl: image.frameUrl ?? null,
      incognito: image.incognito,
      ...(state.vaultToken ? { vaultToken: state.vaultToken } : {}),
    });
    const first = await reader.next(timeoutMs);
    if (first?.type === "image_saved" && first.saveId === saveId) {
      bindVault(state, first);
      return first; // committed before (an earlier answer was lost)
    }
    const ready = expect(first, "image_save_ready", saveId);
    bindVault(state, ready);
    if (ready.upload) {
      const chunkBytes = Math.min(Number(ready.chunkBytes) || MAX_CHUNK_BYTES, MAX_CHUNK_BYTES);
      for (let index = 0, offset = 0; offset < image.bytes.length; index += 1, offset += chunkBytes) {
        port.postMessage({ type: "image_save_chunk", saveId, index, data: bytesToBase64(image.bytes.subarray(offset, offset + chunkBytes)) });
        const ack = expect(await reader.next(timeoutMs), "image_save_chunk_ack", saveId);
        if (ack.index !== index) throw new ImageSaveError("invalid_response");
      }
    }
    const thumbnail = withThumbnail && image.thumbnail
      ? { mimeType: image.thumbnail.mimeType, width: image.thumbnail.width, height: image.thumbnail.height, generator: image.thumbnail.generator, data: bytesToBase64(image.thumbnail.bytes) }
      : null;
    port.postMessage({ type: "image_save_commit", saveId, thumbnail });
    const saved = expect(await reader.next(timeoutMs), "image_saved", saveId);
    bindVault(state, saved);
    return saved;
  } finally {
    try {
      port.disconnect();
    } catch {
      // already closed
    }
  }
}

/**
 * Resolves with the host's `image_saved` message. Every attempt reuses the same saveId, which the
 * host treats idempotently. A rejected thumbnail is retried once without it; a lost connection or
 * answer is retried once, and only after the save is bound to a vault (so it cannot move to
 * another one). At most MAX_ATTEMPTS ports are opened.
 */
export async function uploadImage(image, { connect, runtime, timeoutMs = STEP_TIMEOUT_MS }) {
  if (typeof image.incognito !== "boolean") throw new ImageSaveError("incognito_unknown");
  const state = { vaultToken: null };
  let withThumbnail = true;
  let retriedTransfer = false;
  let lastError = new ImageSaveError("save_failed");
  for (let attempts = 1; attempts <= MAX_ATTEMPTS; attempts += 1) {
    try {
      return await attempt(image, { connect, runtime, timeoutMs, withThumbnail, state });
    } catch (error) {
      if (!(error instanceof ImageSaveError)) throw error;
      lastError = error;
      if (error.code === "invalid_thumbnail" && withThumbnail) {
        withThumbnail = false;
        continue;
      }
      const lost = error.code === "transfer_interrupted" || error.code === "timeout";
      if (lost && !retriedTransfer && state.vaultToken !== null) {
        retriedTransfer = true;
        continue;
      }
      throw error;
    }
  }
  throw lastError;
}
