// Display helpers for the side panel. All dates are rendered in the viewer's local time zone.
import { formatDateTime, t } from "./i18n.js";

const pad = (n) => String(n).padStart(2, "0");

function parse(iso) {
  if (typeof iso !== "string") return null;
  const date = new Date(iso);
  return Number.isNaN(date.getTime()) ? null : date;
}

function startOfDay(date) {
  return new Date(date.getFullYear(), date.getMonth(), date.getDate()).getTime();
}

/**
 * Short, natural date in the UI language, for display only (never compared or stored). Japanese:
 * "today 14:05", "yesterday 9:30", "10/28 14:05" and "2025/3/1 9:30" written the Japanese way;
 * English (en-US): "Today 2:05 PM", "Oct 28 2:05 PM", "Mar 1, 2025 9:30 AM".
 */
export function formatShortDate(iso, now = new Date()) {
  const date = parse(iso);
  if (!date) return "";
  const time = formatDateTime(date, { hour: "numeric", minute: "2-digit" });
  const dayDiff = Math.round((startOfDay(now) - startOfDay(date)) / 86_400_000);
  if (dayDiff === 0) return t("dateToday", time);
  if (dayDiff === 1) return t("dateYesterday", time);
  const day = date.getFullYear() === now.getFullYear()
    ? formatDateTime(date, { month: "short", day: "numeric" })
    : formatDateTime(date, { year: "numeric", month: "short", day: "numeric" });
  return t("dateWithTime", day, time);
}

/** Full timestamp for tooltips, the same in every language: "2026/10/30 14:05:09". */
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
