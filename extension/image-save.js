// "Save to Memocampus" for images: from the context-menu click to the host's COMMIT.

import { acquireImage, inspectImage, makeThumbnail } from "./image-capture.js";
import { ImageSaveError, sha256Hex } from "./image-format.js";
import { uploadImage } from "./image-upload.js";
import { formatNumber, t } from "./i18n.js";
import { registerMenuItem } from "./menu-item.js";
import { describeErrorCode } from "./search-client.js";

export const SAVE_IMAGE_MENU_ID = "memocampus-save-image";

/** Creates the menu item, or only updates its title to the UI language if it already exists. */
export function registerImageMenu(contextMenus, runtime) {
  return registerMenuItem(contextMenus, runtime, { id: SAVE_IMAGE_MENU_ID, title: t("menuSaveImage"), contexts: ["image"] });
}

const webUrl = (value) => {
  try {
    const { protocol } = new URL(value);
    return protocol === "http:" || protocol === "https:" ? value : null;
  } catch {
    return null;
  }
};

/**
 * Saves the image behind a context-menu click. Resolves to { ok: true, saved, image } only after the
 * host committed it; every failure resolves to { ok: false, code, details }.
 */
export async function saveImageFromClick(info, tab, deps) {
  try {
    // Fixed at the click: every attempt of this save action reports the same origin.
    const incognito = tab?.incognito;
    if (typeof incognito !== "boolean") throw new ImageSaveError("incognito_unknown");
    const srcUrl = info?.srcUrl;
    if (typeof srcUrl !== "string" || srcUrl === "") throw new ImageSaveError("unsupported_url");
    const target = Number.isInteger(tab?.id) && tab.id >= 0 ? { tabId: tab.id, frameIds: [Number.isInteger(info.frameId) ? info.frameId : 0] } : null;

    const { bytes } = await acquireImage(srcUrl, target, deps, { incognito });
    const format = await inspectImage(bytes, deps);
    const sha256 = await sha256Hex(bytes, deps.subtle);
    const thumbnail = await makeThumbnail(bytes, format.mimeType, deps);
    const pageUrl = webUrl(info.pageUrl);
    const frameUrl = webUrl(info.frameUrl);
    const image = {
      saveId: deps.randomUUID(),
      bytes,
      sha256,
      mimeType: format.mimeType,
      width: format.width,
      height: format.height,
      // A data: URL's payload is the image itself; only its type is recorded as the source.
      imageUrl: srcUrl.startsWith("data:") ? `data:${format.mimeType}` : srcUrl,
      pageUrl,
      frameUrl: frameUrl && frameUrl !== pageUrl ? frameUrl : null,
      incognito,
      thumbnail,
    };
    const saved = await uploadImage(image, deps);
    return {
      ok: true,
      saved,
      image: { mimeType: format.mimeType, width: format.width, height: format.height, byteLength: bytes.length },
      thumbnail,
    };
  } catch (error) {
    if (error instanceof ImageSaveError) return { ok: false, code: error.code, details: error.details };
    return { ok: false, code: "save_failed", details: {} };
  }
}

const formatBytes = (n) => (n >= 1024 * 1024
  ? `${formatNumber(n / 1024 / 1024, { minimumFractionDigits: 1, maximumFractionDigits: 1 })} MB`
  : `${formatNumber(Math.max(1, Math.round(n / 1024)))} KB`);

/** Title and message for the system notification (and the badge tooltip). */
export function describeImageSaveResult(result) {
  if (result.ok) {
    const { width, height, byteLength } = result.image;
    const key = result.saved.replayed ? "imageSavedMessageReplayed" : result.saved.deduplicated ? "imageSavedMessageDeduplicated" : "imageSavedMessage";
    return { title: t("imageSavedTitle"), message: t(key, width, height, formatBytes(byteLength)) };
  }
  const status = Number.isInteger(result.details?.status) ? result.details.status : 0;
  const message = result.code === "http_error" && status > 0 ? t("imageSaveFailedHttp", status) : describeErrorCode(result.code);
  return { title: t("imageSaveFailedTitle"), message };
}
