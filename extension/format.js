// Display helpers for the side panel. All dates are rendered in the viewer's local time zone.

const pad = (n) => String(n).padStart(2, "0");

function parse(iso) {
  if (typeof iso !== "string") return null;
  const date = new Date(iso);
  return Number.isNaN(date.getTime()) ? null : date;
}

function startOfDay(date) {
  return new Date(date.getFullYear(), date.getMonth(), date.getDate()).getTime();
}

/** Short, natural Japanese date: 「今日 14:05」「昨日 9:30」「10月28日 14:05」「2025年3月1日 9:30」. */
export function formatShortDate(iso, now = new Date()) {
  const date = parse(iso);
  if (!date) return "";
  const time = `${date.getHours()}:${pad(date.getMinutes())}`;
  const dayDiff = Math.round((startOfDay(now) - startOfDay(date)) / 86_400_000);
  if (dayDiff === 0) return `今日 ${time}`;
  if (dayDiff === 1) return `昨日 ${time}`;
  if (date.getFullYear() === now.getFullYear()) return `${date.getMonth() + 1}月${date.getDate()}日 ${time}`;
  return `${date.getFullYear()}年${date.getMonth() + 1}月${date.getDate()}日 ${time}`;
}

/** Full timestamp for tooltips: 「2026/10/30 14:05:09」. */
export function formatFullDate(iso) {
  const date = parse(iso);
  if (!date) return "";
  return `${date.getFullYear()}/${pad(date.getMonth() + 1)}/${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
}

/** Readable URL without scheme or a leading "www.", with percent-escapes decoded where valid. */
export function displayUrl(url) {
  let text = url.replace(/^https?:\/\//i, "").replace(/^www\./i, "");
  try {
    text = decodeURI(text);
  } catch {
    // Malformed escapes: keep the raw form.
  }
  return text.endsWith("/") && text.indexOf("/") === text.length - 1 ? text.slice(0, -1) : text;
}
