// Originals of saved images, for copying, exporting and rebuilding thumbnails (Image tab).
//
// - `readOriginal` fetches the stored bytes in `image_read` chunks of READ_CHUNK_BYTES (each host
//   reply stays far below Chrome's 1 MB limit). Every reply must name the same vault, blob, hash,
//   type and size and continue exactly where the previous one ended; the bytes count only after
//   the total size and SHA-256 match. Any gap, error, timeout or vault change discards them.
// - Copying puts pixels (PNG) on the clipboard. `clipboard.write` is called at once from the menu
//   click, with the PNG as a promise, so the transfer that follows does not run outside the
//   user's action. Only the copy is converted; the vault keeps the original.
// - Exporting is done by the host (save dialog and file writing); only the result comes back.

import { IMAGE_TYPES } from "./image-client.js";
import { MAX_IMAGE_BYTES, base64ToBytes, bytesToBase64, sha256Hex, sniffImage } from "./image-format.js";
import { makeThumbnail } from "./image-capture.js";
import { SearchError, callHost } from "./search-client.js";

/** Must equal READ_CHUNK_BYTES in native-host/src/image_original.rs. */
export const READ_CHUNK_BYTES = 512 * 1024;
/** Decoding beyond this would need several hundred MB of memory; such images are not converted. */
export const MAX_DECODE_PIXELS = 50_000_000;
/** The save dialog stays open while the user chooses; the host answers when it closes. */
export const EXPORT_TIMEOUT_MS = 30 * 60 * 1000;

function checkVault(response, vaultToken) {
  if (response.vaultToken !== vaultToken) throw new SearchError("vault_switched");
}

/**
 * The original bytes of `item` ({ blobId, sha256, byteLength, mimeType }) from the vault named by
 * `vaultToken`. `isCurrent()` is asked between chunks; once it is false the read stops.
 */
export async function readOriginal(item, vaultToken, { send, isCurrent = () => true, subtle }) {
  const total = item.byteLength;
  if (!Number.isSafeInteger(total) || total <= 0 || total > MAX_IMAGE_BYTES) throw new SearchError("too_large");
  const bytes = new Uint8Array(total);
  let offset = 0;
  while (offset < total) {
    const response = await callHost(
      { type: "image_read", blobId: item.blobId, sha256: item.sha256, vaultToken, offset },
      { send, resultType: "image_read_result" },
    );
    if (!isCurrent()) throw new SearchError("vault_switched");
    checkVault(response, vaultToken);
    const same = response.blobId === item.blobId && response.sha256 === item.sha256 && response.byteLength === total && response.mimeType === item.mimeType;
    if (!same || !IMAGE_TYPES.includes(response.mimeType) || response.offset !== offset || typeof response.data !== "string") {
      throw new SearchError("invalid_response");
    }
    let chunk;
    try {
      chunk = base64ToBytes(response.data);
    } catch {
      throw new SearchError("invalid_response");
    }
    if (chunk.length !== Math.min(READ_CHUNK_BYTES, total - offset)) throw new SearchError("read_incomplete");
    bytes.set(chunk, offset);
    offset += chunk.length;
  }
  if ((await sha256Hex(bytes, subtle)) !== item.sha256) throw new SearchError("read_hash_mismatch");
  let format;
  try {
    format = sniffImage(bytes);
  } catch {
    throw new SearchError("read_hash_mismatch");
  }
  if (format.mimeType !== item.mimeType) throw new SearchError("invalid_response");
  return bytes;
}

/** PNG for the clipboard: PNG originals as they are, other formats drawn once and encoded as PNG. */
export async function toPng(bytes, item, { createImageBitmap, OffscreenCanvas }) {
  if (item.mimeType === "image/png") return new Blob([bytes], { type: "image/png" });
  if (item.width * item.height > MAX_DECODE_PIXELS) throw new SearchError("copy_too_large");
  if (!OffscreenCanvas) throw new SearchError("copy_convert_failed");
  let bitmap;
  try {
    bitmap = await createImageBitmap(new Blob([bytes], { type: item.mimeType }));
  } catch {
    throw new SearchError("decode_failed");
  }
  try {
    const canvas = new OffscreenCanvas(bitmap.width, bitmap.height);
    canvas.getContext("2d").drawImage(bitmap, 0, 0);
    const png = await canvas.convertToBlob({ type: "image/png" });
    if (png.type !== "image/png" || png.size === 0) throw new Error("not png");
    return png;
  } catch {
    throw new SearchError("copy_convert_failed");
  } finally {
    bitmap.close?.();
  }
}

/**
 * Copies the image's pixels. Must be called synchronously from the user's click: the clipboard
 * write starts before anything is awaited. Resolves only once the clipboard has accepted the image.
 */
export function copyImage(item, vaultToken, deps) {
  const { clipboard, ClipboardItem } = deps;
  if (!clipboard?.write || !ClipboardItem) return Promise.reject(new SearchError("clipboard_unavailable"));
  const png = readOriginal(item, vaultToken, deps).then((bytes) => toPng(bytes, item, deps));
  png.catch(() => {}); // reported through the write below
  let written;
  try {
    written = clipboard.write([new ClipboardItem({ "image/png": png })]);
  } catch {
    // A ClipboardItem without promise support: write once the PNG is ready (may lose the gesture).
    written = png.then((blob) => clipboard.write([new ClipboardItem({ "image/png": blob })]));
  }
  return Promise.resolve(written).then(
    () => undefined,
    async (error) => {
      // Report why the image could not be prepared, rather than the clipboard's generic refusal.
      const reason = await png.then(() => null, (cause) => cause);
      if (reason) throw reason;
      if (error instanceof SearchError) throw error;
      throw new SearchError(error?.name === "NotAllowedError" ? "clipboard_denied" : "clipboard_failed");
    },
  );
}

/** Asks the host to export the original. Resolves to { cancelled } or { cancelled: false, fileName }. */
export async function exportImage(item, vaultToken, { send }) {
  const response = await callHost(
    { type: "image_export", blobId: item.blobId, sha256: item.sha256, vaultToken },
    { send, resultType: "image_export_result", timeoutMs: EXPORT_TIMEOUT_MS },
  );
  checkVault(response, vaultToken);
  if (response.cancelled === true) return { cancelled: true };
  if (typeof response.fileName !== "string") throw new SearchError("invalid_response");
  return { cancelled: false, fileName: response.fileName };
}

/**
 * Rebuilds a missing thumbnail from the original, the same way as when saving, and adds it to the
 * vault. Resolves to { blob, width, height } for display, or null when none can be made.
 */
export async function rebuildThumbnail(item, vaultToken, deps) {
  if (item.width * item.height > MAX_DECODE_PIXELS) return null;
  const bytes = await readOriginal(item, vaultToken, deps);
  const thumbnail = await makeThumbnail(bytes, item.mimeType, deps);
  if (!thumbnail) return null;
  if (deps.isCurrent && !deps.isCurrent()) throw new SearchError("vault_switched");
  const response = await callHost(
    {
      type: "image_thumbnail_put",
      blobId: item.blobId,
      sha256: item.sha256,
      vaultToken,
      thumbnail: { mimeType: thumbnail.mimeType, width: thumbnail.width, height: thumbnail.height, generator: thumbnail.generator, data: bytesToBase64(thumbnail.bytes) },
    },
    { send: deps.send, resultType: "image_thumbnail_put_result" },
  );
  checkVault(response, vaultToken);
  return { blob: new Blob([thumbnail.bytes], { type: thumbnail.mimeType }), width: thumbnail.width, height: thumbnail.height };
}
