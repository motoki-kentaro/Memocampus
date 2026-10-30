// Getting the original bytes of a right-clicked image, and checking them before upload.
//
// 1. The service worker fetches the image with the browser's cookies (`credentials: "include"`).
//    Host permissions lift CORS for the worker. Cookies and response headers are never read,
//    stored or forwarded; only the body bytes are used.
// 2. If that fails with a network error or an HTTP 4xx (e.g. hotlink protection), and always for
//    `blob:` URLs, the frame that showed the image fetches it itself (page origin, cookies and
//    referrer). Content scripts follow the page's same-origin rules, so this reaches same-origin
//    and CORS-enabled images only.
// 3. `data:` URLs are decoded locally.

import { ImageSaveError, MAX_IMAGE_BYTES, bytesToBase64, sniffImage } from "./image-format.js";

export const THUMBNAIL_EDGE = 320;
export const THUMBNAIL_MAX_BYTES = 256 * 1024;
export const THUMBNAIL_GENERATOR = 1;

/** "http" | "data" | "blob"; anything else (file:, chrome:, javascript:, …) is refused. */
export function classifyImageUrl(srcUrl) {
  let url;
  try {
    url = new URL(srcUrl);
  } catch {
    throw new ImageSaveError("unsupported_url");
  }
  if (url.protocol === "http:" || url.protocol === "https:") return "http";
  if (url.protocol === "data:") return "data";
  if (url.protocol === "blob:") return "blob";
  throw new ImageSaveError("unsupported_url");
}

/** Reads a response body, refusing it as soon as it exceeds `maxBytes`. */
export async function readLimited(response, maxBytes = MAX_IMAGE_BYTES) {
  const declared = Number(response.headers?.get?.("content-length"));
  if (Number.isFinite(declared) && declared > maxBytes) throw new ImageSaveError("too_large");
  const reader = response.body?.getReader?.();
  if (!reader) {
    const bytes = new Uint8Array(await response.arrayBuffer());
    if (bytes.length > maxBytes) throw new ImageSaveError("too_large");
    return bytes;
  }
  const parts = [];
  let total = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    total += value.length;
    if (total > maxBytes) {
      await reader.cancel().catch(() => {});
      throw new ImageSaveError("too_large");
    }
    parts.push(value);
  }
  const bytes = new Uint8Array(total);
  let offset = 0;
  for (const part of parts) {
    bytes.set(part, offset);
    offset += part.length;
  }
  return bytes;
}

export async function fetchInWorker(url, { fetchImpl, maxBytes = MAX_IMAGE_BYTES }) {
  let response;
  try {
    response = await fetchImpl(url, { credentials: "include", redirect: "follow", cache: "default" });
  } catch {
    throw new ImageSaveError("network_error");
  }
  if (!response.ok) throw new ImageSaveError("http_error", { status: response.status });
  return { bytes: await readLimited(response, maxBytes), redirected: response.redirected === true };
}

/**
 * Runs inside the page's frame via chrome.scripting.executeScript; must be self-contained.
 * Returns plain JSON: { ok, base64 } or { ok: false, error, status? }.
 */
export async function pageFetch(url, maxBytes) {
  try {
    const response = await fetch(url, { credentials: "include" });
    if (!response.ok) return { ok: false, error: "http_error", status: response.status };
    if (Number(response.headers.get("content-length")) > maxBytes) return { ok: false, error: "too_large" };
    const bytes = new Uint8Array(await response.arrayBuffer());
    if (bytes.length > maxBytes) return { ok: false, error: "too_large" };
    let binary = "";
    for (let i = 0; i < bytes.length; i += 0x8000) binary += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
    return { ok: true, base64: btoa(binary) };
  } catch {
    return { ok: false, error: "network_error" };
  }
}

export async function fetchInPage(url, target, { executeScript, maxBytes = MAX_IMAGE_BYTES }) {
  let results;
  try {
    results = await executeScript({ target, func: pageFetch, args: [url, maxBytes] });
  } catch {
    // chrome:// pages, the Web Store, or a frame that has gone away.
    throw new ImageSaveError("page_access_denied");
  }
  const result = results?.[0]?.result;
  if (!result?.ok) throw new ImageSaveError(result?.error || "network_error", { status: result?.status });
  const binary = atob(result.base64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
  if (bytes.length > maxBytes) throw new ImageSaveError("too_large");
  return { bytes };
}

const shouldTryPage = (error) => error.code === "network_error" || (error.code === "http_error" && error.details.status >= 400 && error.details.status < 500);

/** Returns { bytes, via } where via is "worker", "page" or "data". */
export async function acquireImage(srcUrl, target, deps) {
  const kind = classifyImageUrl(srcUrl);
  if (kind === "data") {
    const { bytes } = await fetchInWorker(srcUrl, deps);
    return { bytes, via: "data" };
  }
  if (kind === "blob") {
    if (!target) throw new ImageSaveError("page_access_denied");
    return { ...(await fetchInPage(srcUrl, target, deps)), via: "page" };
  }
  try {
    return { ...(await fetchInWorker(srcUrl, deps)), via: "worker" };
  } catch (error) {
    if (!(error instanceof ImageSaveError) || !shouldTryPage(error) || !target) throw error;
    try {
      return { ...(await fetchInPage(srcUrl, target, deps)), via: "page" };
    } catch (pageError) {
      // Report what the direct fetch saw (it carries the HTTP status), noting the fallback.
      error.details.pageError = pageError.code;
      throw error;
    }
  }
}

/**
 * Checks the bytes and decodes them. Width and height are read without applying EXIF orientation,
 * so they equal the file header (which the host verifies).
 */
export async function inspectImage(bytes, { createImageBitmap }) {
  if (bytes.length === 0) throw new ImageSaveError("empty_image");
  if (bytes.length > MAX_IMAGE_BYTES) throw new ImageSaveError("too_large");
  const format = sniffImage(bytes);
  let bitmap;
  try {
    bitmap = await createImageBitmap(new Blob([bytes], { type: format.mimeType }), { imageOrientation: "none" });
  } catch {
    throw new ImageSaveError("decode_failed");
  }
  const { width, height } = bitmap;
  bitmap.close?.();
  if (!(width > 0 && height > 0)) throw new ImageSaveError("decode_failed");
  return { ...format, width, height };
}

/**
 * A small WebP (or JPEG) preview, drawn with the image's own orientation. Returns null when no
 * thumbnail can be made; the original is still saved and the thumbnail can be rebuilt later.
 */
export async function makeThumbnail(bytes, mimeType, { createImageBitmap, OffscreenCanvas }) {
  if (!OffscreenCanvas) return null;
  try {
    const bitmap = await createImageBitmap(new Blob([bytes], { type: mimeType }));
    const scale = Math.min(1, THUMBNAIL_EDGE / Math.max(bitmap.width, bitmap.height));
    const width = Math.max(1, Math.round(bitmap.width * scale));
    const height = Math.max(1, Math.round(bitmap.height * scale));
    const canvas = new OffscreenCanvas(width, height);
    canvas.getContext("2d").drawImage(bitmap, 0, 0, width, height);
    bitmap.close?.();
    for (const [type, quality] of [["image/webp", 0.8], ["image/webp", 0.5], ["image/jpeg", 0.7]]) {
      const blob = await canvas.convertToBlob({ type, quality });
      if (blob.type !== type || blob.size > THUMBNAIL_MAX_BYTES) continue;
      return { mimeType: type, width, height, generator: THUMBNAIL_GENERATOR, bytes: new Uint8Array(await blob.arrayBuffer()) };
    }
  } catch {
    // fall through
  }
  return null;
}

export { bytesToBase64 };
