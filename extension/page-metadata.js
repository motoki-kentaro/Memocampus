// Light page metadata (Issue #22): read once from a loaded page's DOM and sent with its visit.
// "OGP があればラッキー": anything missing is null, anything failing means "no metadata", and the
// visit is recorded either way. Nothing here fetches, follows or guesses a URL, and nothing is logged.
//
// Identity: the script runs in exactly the document that finished loading (`documentIds`), and its
// answer counts only if that document is still at the visit's URL. A document that is gone (tab
// closed, next navigation) or moved on (pushState) gives no metadata, never another page's.
//
// The host shapes every value again with the same rules (`native-host/src/page_metadata.rs`).

export const MAX_TITLE = 500;
export const MAX_SITE_NAME = 200;
export const MAX_DESCRIPTION = 1000;
export const MAX_URL = 2048;
export const CAPTURE_TIMEOUT_MS = 2000;
/** Candidates read per field, and characters read per candidate, before any shaping. */
export const MAX_CANDIDATES = 8;
export const MAX_RAW = 8192;

/**
 * Runs in the page (chrome.scripting.executeScript); must be self-contained. Reads attributes only,
 * never the body. Plain JSON out.
 */
export function readPageDom(maxCandidates, maxRaw) {
  const cut = (value) => (typeof value === "string" ? value.slice(0, maxRaw) : null);
  const all = (selector, attribute) =>
    Array.from(document.querySelectorAll(selector))
      .slice(0, maxCandidates)
      .map((element) => cut(element.getAttribute(attribute)))
      .filter((value) => value !== null);
  return {
    href: cut(location.href),
    baseUri: cut(document.baseURI),
    title: cut(document.title),
    description: all('meta[name="description" i]', "content"),
    ogTitle: all('meta[property="og:title" i]', "content"),
    ogDescription: all('meta[property="og:description" i]', "content"),
    ogSiteName: all('meta[property="og:site_name" i]', "content"),
    ogImage: all('meta[property="og:image" i]', "content"),
    icon: all('link[rel~="icon" i]', "href"),
  };
}

const WHITE_SPACE = /^\p{White_Space}$/u;
const CONTROL = /^\p{Cc}$/u;

/** White_Space to one space, other controls out, trimmed, cut to `limit` characters; "" is null. */
export function cleanText(raw, limit) {
  if (typeof raw !== "string") return null;
  let out = "";
  let count = 0;
  let pendingSpace = false;
  for (const char of raw.toWellFormed()) {
    if (WHITE_SPACE.test(char)) {
      pendingSpace = out !== "";
      continue;
    }
    if (CONTROL.test(char)) continue;
    if (pendingSpace) {
      if (count + 1 >= limit) break;
      out += " ";
      count += 1;
      pendingSpace = false;
    }
    if (count >= limit) break;
    out += char;
    count += 1;
  }
  return out === "" ? null : out;
}

/** An absolute http(s) URL without credentials, as the page's base resolves it; otherwise null. */
export function cleanUrl(raw, baseUri) {
  if (typeof raw !== "string" || typeof baseUri !== "string") return null;
  const trimmed = raw.trim();
  if (trimmed === "") return null;
  let url;
  try {
    url = new URL(trimmed, baseUri);
  } catch {
    return null;
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") return null;
  if (url.username !== "" || url.password !== "" || url.hostname === "") return null;
  const href = url.href;
  if (href.length > MAX_URL || /[\p{White_Space}\p{Cc}]/u.test(href)) return null;
  return href;
}

const firstText = (candidates, limit) => {
  for (const candidate of Array.isArray(candidates) ? candidates : []) {
    const text = cleanText(candidate, limit);
    if (text !== null) return text;
  }
  return null;
};

const firstUrl = (candidates, baseUri) => {
  for (const candidate of Array.isArray(candidates) ? candidates : []) {
    const url = cleanUrl(candidate, baseUri);
    if (url !== null) return url;
  }
  return null;
};

/** The `page` of a visit to `url` from what `readPageDom` answered, or null if it is not that page's. */
export function shapePageMetadata(raw, url, capturedAt) {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return null;
  if (raw.href !== url || typeof raw.baseUri !== "string") return null;
  return {
    capturedAt,
    title: cleanText(raw.title, MAX_TITLE),
    description: firstText(raw.description, MAX_DESCRIPTION),
    ogTitle: firstText(raw.ogTitle, MAX_TITLE),
    ogDescription: firstText(raw.ogDescription, MAX_DESCRIPTION),
    ogSiteName: firstText(raw.ogSiteName, MAX_SITE_NAME),
    ogImageUrl: firstUrl(raw.ogImage, raw.baseUri),
    faviconUrl: firstUrl(raw.icon, raw.baseUri),
  };
}

/**
 * Reads the metadata of the document `documentId` of `tabId` once, for a visit to `url`. Resolves to
 * the visit's `page`, or null; never rejects. Call only after the host allowed the URL.
 */
export async function capturePageMetadata({ executeScript, tabId, documentId, url, now = () => new Date(), timeoutMs = CAPTURE_TIMEOUT_MS }) {
  if (typeof executeScript !== "function" || !Number.isInteger(tabId) || typeof documentId !== "string" || documentId === "") return null;
  let timer;
  try {
    // A failure after the timeout is dropped here, never reported (its message could name the page).
    const injected = Promise.resolve(
      executeScript({
        target: { tabId, documentIds: [documentId] },
        func: readPageDom,
        args: [MAX_CANDIDATES, MAX_RAW],
        world: "ISOLATED",
      }),
    ).catch(() => null);
    const timeout = new Promise((resolve) => {
      timer = setTimeout(() => resolve(null), timeoutMs);
    });
    const results = await Promise.race([injected, timeout]);
    if (!Array.isArray(results) || results.length !== 1) return null;
    const [only] = results;
    if (only?.documentId !== undefined && only.documentId !== documentId) return null;
    return shapePageMetadata(only?.result, url, now().toISOString());
  } catch {
    return null;
  } finally {
    clearTimeout(timer);
  }
}
