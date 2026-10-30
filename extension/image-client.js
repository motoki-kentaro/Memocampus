// Image tab requests (`image_list`, `image_thumbnails`, `image_sources`) and validation of the answers.
// Nothing from a reply is trusted as markup: values are checked here and rendered as text only.

import { base64ToBytes, sniffImage } from "./image-format.js";
import { SearchError, callHost } from "./search-client.js";

export const LIST_PAGE_SIZE = 60;
export const THUMBNAIL_BATCH = 16;
export const THUMBNAIL_MAX_BYTES = 256 * 1024;
export const IMAGE_TYPES = ["image/png", "image/jpeg", "image/gif", "image/webp", "image/avif", "image/bmp"];
const THUMBNAIL_TYPES = new Set(["image/png", "image/jpeg", "image/webp"]);
const TOKEN = /^[0-9a-f]{64}$/;
const SHA256 = /^[0-9a-f]{64}$/;

const positiveInt = (value) => Number.isSafeInteger(value) && value > 0;
const optionalText = (value) => (typeof value === "string" ? value : null);

function vaultToken(response) {
  if (typeof response.vaultToken !== "string" || !TOKEN.test(response.vaultToken)) throw new SearchError("invalid_response");
  return response.vaultToken;
}

function cursorOf(value) {
  if (value === null || value === undefined) return null;
  if (!Number.isSafeInteger(value.lastSavedAt) || !positiveInt(value.id)) throw new SearchError("invalid_response");
  return { lastSavedAt: value.lastSavedAt, id: value.id };
}

function summary(item) {
  if (!item || !positiveInt(item.blobId) || !SHA256.test(item.sha256 ?? "") || !IMAGE_TYPES.includes(item.mimeType)) return null;
  if (![item.byteLength, item.width, item.height, item.saveCount].every(positiveInt) || !Number.isSafeInteger(item.lastSavedAt)) return null;
  return {
    blobId: item.blobId,
    sha256: item.sha256,
    mimeType: item.mimeType,
    byteLength: item.byteLength,
    width: item.width,
    height: item.height,
    lastSavedAt: item.lastSavedAt,
    saveCount: item.saveCount,
    hasThumbnail: item.hasThumbnail === true,
    latestImageUrl: optionalText(item.latestImageUrl) ?? "",
    latestPageUrl: optionalText(item.latestPageUrl),
  };
}

export async function listImages(cursor, { send }) {
  const response = await callHost({ type: "image_list", cursor: cursor ?? null, limit: LIST_PAGE_SIZE }, { send, resultType: "image_list_result" });
  if (!Array.isArray(response.items)) throw new SearchError("invalid_response");
  return { items: response.items.map(summary).filter(Boolean), next: cursorOf(response.next), vaultToken: vaultToken(response) };
}

/**
 * Thumbnails as Blobs ready for object URLs. A thumbnail is accepted only if its bytes really are
 * PNG, JPEG or WebP of the declared type and size; anything else counts as missing, so SVG or HTML
 * can never be rendered.
 */
export async function fetchThumbnails(blobIds, { send }) {
  const response = await callHost({ type: "image_thumbnails", blobIds }, { send, resultType: "image_thumbnails_result" });
  if (!Array.isArray(response.items)) throw new SearchError("invalid_response");
  const thumbnails = new Map();
  for (const item of response.items) {
    if (!positiveInt(item?.blobId) || !blobIds.includes(item.blobId)) continue;
    thumbnails.set(item.blobId, item.missing === true ? null : decodeThumbnail(item));
  }
  const deferred = Array.isArray(response.deferred) ? response.deferred.filter((id) => blobIds.includes(id)) : [];
  return { thumbnails, deferred, vaultToken: vaultToken(response) };
}

function decodeThumbnail(item) {
  if (!THUMBNAIL_TYPES.has(item.mimeType) || typeof item.data !== "string" || !positiveInt(item.width) || !positiveInt(item.height)) return null;
  let bytes;
  try {
    bytes = base64ToBytes(item.data);
  } catch {
    return null;
  }
  if (bytes.length === 0 || bytes.length > THUMBNAIL_MAX_BYTES) return null;
  try {
    if (sniffImage(bytes).mimeType !== item.mimeType) return null;
  } catch {
    return null;
  }
  return { blob: new Blob([bytes], { type: item.mimeType }), width: item.width, height: item.height };
}

export async function listSources(blobId, cursor, { send }) {
  const response = await callHost({ type: "image_sources", blobId, cursor: cursor ?? null }, { send, resultType: "image_sources_result" });
  if (!Array.isArray(response.items)) throw new SearchError("invalid_response");
  const items = response.items
    .filter((item) => positiveInt(item?.savedImageId) && Number.isSafeInteger(item.savedAt) && typeof item.imageUrl === "string" && typeof item.incognito === "boolean")
    .map((item) => ({ savedImageId: item.savedImageId, imageUrl: item.imageUrl, pageUrl: optionalText(item.pageUrl), frameUrl: optionalText(item.frameUrl), savedAt: item.savedAt, incognito: item.incognito }));
  return { items, next: cursorOf(response.next), vaultToken: vaultToken(response) };
}

/** How a recorded URL is shown: only http(s) URLs become openable links; the rest stay text. */
export function describeSourceUrl(url) {
  if (typeof url !== "string" || url === "") return { text: "（記録なし）", href: null };
  if (url.startsWith("data:")) return { text: `${url}（ページ内に埋め込まれた画像）`, href: null };
  try {
    const parsed = new URL(url);
    if (parsed.protocol === "http:" || parsed.protocol === "https:") return { text: url, href: parsed.href };
  } catch {
    // shown as text below
  }
  return { text: url, href: null };
}

export const shortMime = (mimeType) => ({ "image/jpeg": "JPEG", "image/png": "PNG", "image/gif": "GIF", "image/webp": "WebP", "image/avif": "AVIF", "image/bmp": "BMP" })[mimeType] ?? "画像";
