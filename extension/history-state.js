// The search state of the full-page History and its URL (Slice 2). The URL is the source of truth: five
// parameters, read with a correction for every bad value and written back only when they differ from what the
// address bar holds. Pure functions only (no DOM, no Chrome), so they can be tested without either.
//
//   date    the chosen local day, YYYY-MM-DD (a real day); missing or bad: today
//   domain  the domain input, as typed (exact: a host name; regex: a pattern); missing: blank (every domain)
//   mode    "exact" | "regex"; missing or bad: "exact"
//   page    1, 2, 3 …; missing, 0, negative, fractional, not a number or absurdly long: 1
//   size    25 | 50 | 100; missing or bad: 50
//
// The month the calendar shows is NOT part of the state: moving through months changes nothing here.

import { MIN_YEAR, daysInMonth } from "./visit-calendar.js";

export const SIZES = Object.freeze([25, 50, 100]);
export const DEFAULT_SIZE = 50;
export const MODES = Object.freeze(["exact", "regex"]);
/** The input's length limit (characters); the host allows 512 bytes of a pattern. */
export const MAX_DOMAIN_CHARS = 256;
/** The most digits of a page number taken as one: a longer text is no page. */
const MAX_PAGE_DIGITS = 9;
/** The last year a chosen day may have: `new Date(y, m, d + 1).toISOString()` must stay inside 0000–9999. */
const MAX_YEAR = 9998;

const pad = (n, width = 2) => String(n).padStart(width, "0");

/** `YYYY-MM-DD` of a local date. */
export function dateText({ year, monthIndex, day }) {
  return `${pad(year, 4)}-${pad(monthIndex + 1)}-${pad(day)}`;
}

/** `{ year, monthIndex, day }` of a text that is exactly `YYYY-MM-DD` and a day that exists, else null. */
export function parseDate(text) {
  const match = typeof text === "string" ? /^(\d{4})-(\d{2})-(\d{2})$/.exec(text) : null;
  if (!match) return null;
  const year = Number(match[1]);
  const monthIndex = Number(match[2]) - 1;
  const day = Number(match[3]);
  if (year < MIN_YEAR || year > MAX_YEAR || monthIndex < 0 || monthIndex > 11) return null;
  if (day < 1 || day > daysInMonth(year, monthIndex)) return null;
  return { year, monthIndex, day };
}

/** The defaults of a state: today, no domain, exact, page 1, 50 per page. */
export function defaultState(today) {
  return { date: { ...today }, domain: "", mode: "exact", page: 1, size: DEFAULT_SIZE };
}

/**
 * The state of a query string (`location.search`), and whether anything had to be corrected. A parameter
 * given twice counts as its first value. Nothing here can throw.
 */
export function parseSearch(search, today) {
  const state = defaultState(today);
  let corrected = false;
  let params;
  try {
    params = new URLSearchParams(typeof search === "string" ? search : "");
  } catch {
    return { state, corrected: true };
  }
  if (params.has("date")) {
    const date = parseDate(params.get("date"));
    if (date) state.date = date;
    else corrected = true;
  }
  if (params.has("domain")) {
    const domain = params.get("domain");
    if ([...domain].length > MAX_DOMAIN_CHARS) {
      state.domain = [...domain].slice(0, MAX_DOMAIN_CHARS).join("");
      corrected = true;
    } else {
      state.domain = domain;
    }
  }
  if (params.has("mode")) {
    const mode = params.get("mode");
    if (MODES.includes(mode)) state.mode = mode;
    else corrected = true;
  }
  if (params.has("page")) {
    const text = params.get("page");
    const page = /^\d+$/.test(text) && text.length <= MAX_PAGE_DIGITS ? Number(text) : 0;
    if (page >= 1) state.page = page;
    else corrected = true;
  }
  if (params.has("size")) {
    const size = /^\d+$/.test(params.get("size")) ? Number(params.get("size")) : 0;
    if (SIZES.includes(size)) state.size = size;
    else corrected = true;
  }
  return { state, corrected };
}

/**
 * The query string of a state (`?date=…`, always naming the day; the others only when they differ from
 * their defaults, so a plain History URL stays short). Encoded by `URLSearchParams`: a pattern such as
 * `(^|\.)github\.com$` survives the round trip.
 */
export function buildSearch(state) {
  const params = new URLSearchParams();
  params.set("date", dateText(state.date));
  if (state.domain !== "") params.set("domain", state.domain);
  if (state.mode !== "exact") params.set("mode", state.mode);
  if (state.page !== 1) params.set("page", String(state.page));
  if (state.size !== DEFAULT_SIZE) params.set("size", String(state.size));
  return `?${params.toString()}`;
}

/** Whether two states are the same search. */
export function sameState(a, b) {
  return buildSearch(a) === buildSearch(b);
}

/**
 * The page numbers to show: the first, the last and the ones around `current`, `null` for a gap
 * (`1 … 6 7 [8] 9 10 … 20`). Never every page.
 */
export function pageNumbers(current, pages, around = 2) {
  if (pages < 1) return [];
  const wanted = new Set([1, pages]);
  for (let page = current - around; page <= current + around; page++) if (page >= 1 && page <= pages) wanted.add(page);
  const sorted = [...wanted].sort((a, b) => a - b);
  const result = [];
  sorted.forEach((page, index) => {
    if (index > 0 && page - sorted[index - 1] === 2) result.push(page - 1);
    else if (index > 0 && page - sorted[index - 1] > 2) result.push(null);
    result.push(page);
  });
  return result;
}
