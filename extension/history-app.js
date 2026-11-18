// The full-page History (Slice 2): the controller. The URL is the source of truth for the search (history-state.js);
// the calendar's month is a separate, URL-less state. Two independent request series, the visit list and the
// month's marks, each with its own "latest wins" gate; both are checked against the Vault they were read from.
//
// URL rules (see docs/history-slice-2.md):
//   pushState     a chosen day, a page, the page size, the match mode (a clear step the Back button should undo)
//   replaceState  typing in the domain field (no entry per key), every correction of a bad or out-of-range value
//   nothing       moving the calendar's month, a refresh when the tab is shown again
//   popstate      the state is read from the URL again, the calendar jumps to the month of the chosen day

import { SearchError, VAULT_UNAVAILABLE_CODES, createLatestGate, describeErrorCode, isWebUrl } from "./search-client.js";
import { historyDays, historyList, historyVisit, hostSupportsHistory } from "./history-client.js";
import { buildSearch, parseSearch, sameState } from "./history-state.js";
import { MIN_YEAR, YEARS_AHEAD, addMonths, localDayBounds, localToday, monthBounds } from "./visit-calendar.js";
import { createCalendarView, longDate, renderPager, renderRows, setOpen, showDetail, showDetailError, showDetailLoading } from "./history-view.js";

/** Errors of the domain input itself: shown at the input, and no list is shown for a condition that cannot be applied. */
export const INPUT_CODES = new Set(["invalid_regex", "unsupported_regex", "regex_limit", "regex_too_long", "invalid_domain"]);

const MESSAGES = Object.freeze({
  invalid_regex: "正規表現が正しくありません。構文を確認してください。",
  unsupported_regex: "この正規表現は使えません（先読み・後方参照は未対応です）。",
  regex_limit: "正規表現が複雑すぎます。簡単な式にしてください。",
  regex_too_long: "正規表現が長すぎます（512バイトまで）。",
  invalid_domain: "ドメインの形式が正しくありません。ホスト名だけを入力してください（例: github.com）。",
  response_too_large: "この日の表示データが大きすぎて取得できません。ページサイズを小さくするか、条件を絞ってください。",
  visit_not_found: "この訪問は見つかりません（削除された可能性があります）。",
  vault_unstable: "Vaultが何度も切り替わっているため、表示を止めました。「再読み込み」を押してください。",
});
export const errorText = (code) => (Object.hasOwn(MESSAGES, code) ? MESSAGES[code] : describeErrorCode(code));

const HINTS = Object.freeze({ exact: "完全一致の例: github.com（サブドメインは含みません）", regex: "正規表現の例: (^|\\.)github\\.com$（ホスト名の一部に一致）" });
const MAX_VAULT_RESETS = 3;
const MAX_RANGE_CORRECTIONS = 3;
const REFRESH_MIN_MS = 1000;

// VaultError::code in native-host/src/vault.rs. Busy / transport / DB errors do not prove a Vault state change.
const VAULT_STATE_CODES = new Set([...VAULT_UNAVAILABLE_CODES, "vault_locked", "vault_switched", "key_unavailable", "vault_decrypt_failed", "vault_mode_mismatch", "vault_encryption_unsupported", "vault_legacy_unsupported"]);
const COMMUNICATION_CODES = new Set(["timeout", "host_unavailable"]);
const codeOf = (error) => error instanceof SearchError ? error.code : "host_unavailable";

export function isPickerSelectionValid(view, year, monthIndex, lastYear) {
  return view === "months" && Number.isInteger(year) && Number.isInteger(lastYear) && year >= MIN_YEAR && year <= lastYear && Number.isInteger(monthIndex) && monthIndex >= 0 && monthIndex < 12;
}

export function createHistoryApp({
  document,
  win,
  send,
  tabs,
  openSettings = () => {},
  now = () => new Date(),
  timers = { setTimeout: (fn, ms) => globalThis.setTimeout(fn, ms), clearTimeout: (id) => globalThis.clearTimeout(id) },
  debounceMs = 350,
}) {
  const $ = (id) => document.querySelector(`#${id}`);
  const el = Object.fromEntries(
    ["history-status", "cal-prev", "cal-next", "cal-title", "cal-table", "cal-months", "cal-legend", "cal-jump", "cal-status", "domain-input", "mode-exact", "mode-regex", "domain-hint", "domain-error", "selected-heading", "result-count", "size-select", "history-state", "history-state-text", "history-retry", "history-settings", "history-list", "history-pager"].map((id) => [id, $(id)]),
  );
  const options = { send };
  const listGate = createLatestGate();
  const dayGate = createLatestGate();

  let state = null;
  let shown = null; // the calendar's month: { year, monthIndex }
  let counts = null; // the marks of `shown` for the current domain condition, or null while unknown
  let view = "days"; // or "months"
  let pickerYear = null;
  let supported = null;
  let vaultToken = null;
  let vaultResets = 0;
  let rangeCorrections = 0;
  let typingTimer = null;
  let pendingFocus = null;
  let opening = null; // the full-URL fetch in flight: { id, epoch, token }
  let listEpoch = 0; // bumped whenever what the list shows may no longer be what a pending detail was asked for
  let shownIds = new Set();
  let expanded = new Map(); // visit id -> the context of its open detail: { id, ui, epoch, token }
  let uiById = new Map(); // visit id -> the row's controls, of the list on screen only
  let lastRefresh = 0;
  let started = 0;

  const today = () => localToday(now());
  const maxYear = () => Math.max(today().year + YEARS_AHEAD, state ? state.date.year : today().year + YEARS_AHEAD);
  const inRange = (year) => Number.isInteger(year) && year >= MIN_YEAR && year <= maxYear();
  const calendar = createCalendarView({ table: el["cal-table"], document, onSelect: (date) => commit({ ...withDraft(), date, page: 1 }, { push: true }) });

  const monthButtons = el["cal-months"] ? Array.from({ length: 12 }, (_, monthIndex) => {
    const choice = document.createElement("button");
    choice.type = "button";
    choice.className = "calendar-month";
    choice.textContent = `${monthIndex + 1}月`;
    choice.dataset.month = String(monthIndex);
    choice.addEventListener("click", () => {
      if (choice.disabled || !isPickerSelectionValid(view, pickerYear, monthIndex, maxYear())) return;
      shown = { year: pickerYear, monthIndex };
      view = "days";
      counts = null;
      drawCalendar();
      if (supported === true) void loadDays();
      el["cal-title"].focus();
    });
    el["cal-months"].append(choice);
    return choice;
  }) : [];

  // ---------------------------------------------------------------------------------------------- drawing

  const say = (text) => {
    el["history-status"].textContent = text;
    el["history-status"].hidden = text === "";
  };

  function renderCalendarHead() {
    if (view === "days") {
      const before = addMonths(shown.year, shown.monthIndex, -1);
      const after = addMonths(shown.year, shown.monthIndex, 1);
      el["cal-prev"].disabled = !inRange(before.year);
      el["cal-next"].disabled = !inRange(after.year);
      el["cal-prev"].textContent = "◀";
      el["cal-next"].textContent = "▶";
      el["cal-prev"].setAttribute("aria-label", "前の月");
      el["cal-next"].setAttribute("aria-label", "次の月");
      el["cal-prev"].title = "前の月";
      el["cal-next"].title = "次の月";
      el["cal-title"].textContent = `${shown.year} ▼ ${shown.monthIndex + 1}月`;
      el["cal-title"].setAttribute("aria-label", `${shown.year}年${shown.monthIndex + 1}月、年と月を選ぶ`);
      el["cal-title"].setAttribute("aria-expanded", "false");
    } else {
      el["cal-prev"].disabled = !inRange(pickerYear - 1);
      el["cal-next"].disabled = !inRange(pickerYear + 1);
      el["cal-prev"].textContent = "◀";
      el["cal-next"].textContent = "▶";
      el["cal-prev"].setAttribute("aria-label", "前の年");
      el["cal-next"].setAttribute("aria-label", "次の年");
      el["cal-prev"].title = "前の年";
      el["cal-next"].title = "次の年";
      el["cal-title"].textContent = `${pickerYear} ▲`;
      el["cal-title"].setAttribute("aria-label", `${pickerYear}年、月の一覧を閉じる`);
      el["cal-title"].setAttribute("aria-expanded", "true");
    }
  }

  function renderMonths() {
    const shownHere = pickerYear === shown.year;
    for (const [monthIndex, choice] of monthButtons.entries()) {
      choice.setAttribute("aria-label", `${pickerYear}年${monthIndex + 1}月`);
      if (shownHere && monthIndex === shown.monthIndex) {
        choice.setAttribute("aria-current", "true");
      } else {
        choice.removeAttribute("aria-current");
      }
      choice.disabled = !inRange(pickerYear);
    }
  }

  function drawCalendar() {
    renderCalendarHead();
    el["cal-table"].hidden = view !== "days";
    if (el["cal-months"]) el["cal-months"].hidden = view !== "months";
    if (el["cal-legend"]) el["cal-legend"].hidden = view !== "days";
    if (view === "days") {
      calendar.show({ year: shown.year, monthIndex: shown.monthIndex, counts, selected: state.date, today: today() });
      el["cal-jump"].hidden = shown.year === state.date.year && shown.monthIndex === state.date.monthIndex;
    } else {
      el["cal-jump"].hidden = true;
      renderMonths();
    }
  }

  /** Anything a pending detail answer was bound to has changed: it will no longer be accepted. */
  const invalidateList = () => { listEpoch++; };

  function syncInputs() {
    // A domain being typed (debounce pending) is a draft the page does not own: only the URL (popstate, start) or the
    // confirmation of the draft itself may replace it.
    if (typingTimer === null && el["domain-input"].value !== state.domain) el["domain-input"].value = state.domain;
    el["mode-exact"].checked = state.mode === "exact";
    el["mode-regex"].checked = state.mode === "regex";
    el["domain-hint"].textContent = HINTS[state.mode];
    el["size-select"].value = String(state.size);
    el["selected-heading"].textContent = `${longDate(state.date)}の訪問履歴`;
  }

  function domainError(text) {
    el["domain-error"].textContent = text;
    el["domain-error"].hidden = text === "";
    el["domain-input"].setAttribute("aria-invalid", text === "" ? "false" : "true");
  }

  /** No list: a state line instead (loading, empty, error), with the buttons that can help. */
  function panel(text, { retry = false, settings = false } = {}) {
    el["history-state-text"].textContent = text;
    el["history-state"].hidden = false;
    el["history-retry"].hidden = !retry;
    el["history-settings"].hidden = !settings;
    el["history-list"].hidden = true;
    el["history-list"].replaceChildren();
    shownIds = new Set();
    expanded = new Map();
    uiById = new Map();
    el["history-pager"].replaceChildren();
    el["history-pager"].hidden = true;
    el["result-count"].textContent = "";
  }

  function drawList(result) {
    el["history-state"].hidden = true;
    const filtered = state.domain.trim() !== "";
    if (result.total === 0) {
      panel(filtered ? "条件に一致する訪問はありません。" : "この日の訪問履歴はありません。");
      el["result-count"].textContent = "0件";
      return;
    }
    el["history-list"].hidden = false;
    expanded = new Map(); // a new list: nothing of an old one stays open
    const rows = renderRows({ list: el["history-list"], document, items: result.items, onOpenCut: openCut, onToggle: toggleDetail });
    uiById = new Map(rows.map((row) => [row.ui.id, row.ui]));
    shownIds = new Set(result.items.map((item) => item.id));
    el["result-count"].textContent = `${filtered ? "条件に一致する訪問 " : ""}${result.total.toLocaleString("ja-JP")}件（${result.page} / ${result.pages}ページ）`;
    const current = renderPager({ nav: el["history-pager"], document, page: result.page, pages: result.pages, onPage: (page) => { pendingFocus = "pager"; commit({ ...withDraft(), page }, { push: true }); } });
    if (pendingFocus === "pager") current?.focus();
    pendingFocus = null;
  }

  // ----------------------------------------------------------------------------------------------- loading

  /** Only call after the caller's latest-request / row-context check. No automatic re-fetch. */
  function revokeVault(error) {
    vaultToken = null;
    counts = null;
    opening = null;
    invalidateList();
    listGate.next();
    dayGate.next();
    started++;
    for (const context of expanded.values()) setOpen(context.ui, false);
    panel(errorText(codeOf(error)), { retry: true, settings: VAULT_UNAVAILABLE_CODES.has(codeOf(error)) });
    say("");
    el["cal-status"].textContent = "";
    el["cal-status"].hidden = true;
    drawCalendar();
  }

  function vaultFailed(error) {
    if (!VAULT_STATE_CODES.has(codeOf(error))) return false;
    revokeVault(error);
    return true;
  }

  /** A transport failure is not a Vault switch. Keep completed data, but never adopt pending detail/URL replies. */
  function cancelPendingDetails(error) {
    opening = null;
    for (const context of expanded.values()) {
      if (!context.pending) continue;
      context.pending = false;
      context.cancelled = true;
      showDetailError(context.ui, document, errorText(codeOf(error)), () => retryDetail({ id: context.id }, context.ui, context));
    }
  }

  /** A response of another Vault than the one this page is showing: everything shown is dropped and read again. */
  function accept(token) {
    if (vaultToken === null) {
      vaultToken = token;
      return true;
    }
    if (vaultToken === token) return true;
    vaultToken = null;
    counts = null;
    invalidateList();
    listGate.next();
    dayGate.next();
    if (++vaultResets > MAX_VAULT_RESETS) {
      panel(errorText("vault_unstable"), { retry: true });
      drawCalendar();
      return false;
    }
    say("Vaultが切り替わったため、表示を読み込み直しています。");
    panel("読み込み中…");
    drawCalendar();
    void loadList({ reset: true });
    void loadDays();
    return false;
  }

  function failed(error) {
    if (vaultFailed(error)) return;
    invalidateList();
    shownIds = new Set();
    expanded = new Map();
    uiById = new Map();
    const code = codeOf(error);
    if (INPUT_CODES.has(code)) {
      domainError(errorText(code));
      panel("検索条件を修正してください。");
      return;
    }
    panel(errorText(code), { retry: true, settings: VAULT_UNAVAILABLE_CODES.has(code) });
  }

  async function loadList({ reset }) {
    const ticket = listGate.next();
    invalidateList();
    const asked = { ...state };
    if (reset) {
      panel("読み込み中…");
      el["history-status"].hidden = el["history-status"].textContent === "";
    }
    let result;
    try {
      const { visitedFrom, visitedTo } = localDayBounds(asked.date.year, asked.date.monthIndex, asked.date.day);
      result = await historyList({ visitedFrom, visitedTo, domain: asked.domain, mode: asked.mode, page: asked.page, size: asked.size }, options);
    } catch (error) {
      if (listGate.isCurrent(ticket)) failed(error);
      return;
    }
    if (!listGate.isCurrent(ticket) || !accept(result.vaultToken)) return;
    domainError("");
    // The page asked for lies past the end: the last page instead, written over the entry (never a new one).
    if (result.pages > 0 && asked.page > result.pages && rangeCorrections < MAX_RANGE_CORRECTIONS) {
      rangeCorrections++;
      state = { ...state, page: result.pages };
      win.history.replaceState(null, "", buildSearch(state));
      syncInputs();
      void loadList({ reset: false });
      return;
    }
    if (asked.page > result.pages && result.pages > 0) {
      panel("ページ番号が範囲外です。", { retry: true });
      return;
    }
    say("");
    drawList(result);
  }

  async function loadDays({ keep = false } = {}) {
    const ticket = dayGate.next();
    if (!keep) {
      counts = null;
      drawCalendar();
    }
    const asked = { ...shown };
    let result;
    try {
      result = await historyDays({ bounds: monthBounds(asked.year, asked.monthIndex), domain: state.domain, mode: state.mode }, options);
    } catch (error) {
      if (!dayGate.isCurrent(ticket)) return;
      if (vaultFailed(error)) return;
      if (COMMUNICATION_CODES.has(codeOf(error))) cancelPendingDetails(error);
      counts = null;
      drawCalendar();
      const code = error instanceof SearchError ? error.code : "host_unavailable";
      el["cal-status"].textContent = INPUT_CODES.has(code) ? "" : "記録のある日を取得できませんでした。";
      el["cal-status"].hidden = el["cal-status"].textContent === "";
      return;
    }
    if (!dayGate.isCurrent(ticket) || !accept(result.vaultToken)) return;
    el["cal-status"].hidden = true;
    counts = result.counts;
    drawCalendar();
  }

  // --------------------------------------------------------------------------------------------- state steps

  /** The state changes (a search, not a month): written to the URL, then the list (and the marks, if their condition changed). */
  function commit(next, { push }) {
    if (sameState(state, next)) return;
    const conditionChanged = next.domain !== state.domain || next.mode !== state.mode;
    state = next;
    invalidateList();
    rangeCorrections = 0;
    vaultResets = 0;
    win.history[push ? "pushState" : "replaceState"](null, "", buildSearch(state));
    syncInputs();
    drawCalendar();
    if (supported !== true) return;
    void loadList({ reset: true });
    if (conditionChanged) void loadDays();
  }

  function readUrl() {
    const parsed = parseSearch(win.location.search, today());
    state = parsed.state;
    if (parsed.corrected) win.history.replaceState(null, "", buildSearch(state));
    shown = { year: state.date.year, monthIndex: state.date.monthIndex };
  }

  async function begin() {
    const run = ++started;
    invalidateList();
    listGate.next();
    dayGate.next();
    panel("読み込み中…");
    domainError("");
    try {
      const available = await hostSupportsHistory(options);
      if (run !== started) return;
      supported = available;
    } catch (error) {
      if (run !== started) return;
      supported = null;
      failed(error);
      return;
    }
    if (run !== started) return;
    if (!supported) {
      panel("このNative HostはHistory画面に対応していません。Memocampusを更新してください。", { retry: true });
      return;
    }
    vaultToken = null;
    vaultResets = 0;
    void loadList({ reset: true });
    void loadDays();
  }

  function refresh() {
    if (supported !== true || document.visibilityState === "hidden") return;
    const at = now().getTime();
    if (at - lastRefresh < REFRESH_MIN_MS) return;
    lastRefresh = at;
    vaultResets = 0;
    // Open details belong to the list on screen: reading it again would close them. The marks are still read, which
    // is also how a switch of Vault is noticed.
    if (expanded.size === 0) void loadList({ reset: false });
    void loadDays({ keep: true });
  }

  // ------------------------------------------------------------------------------------------ detail

  const detailErrorText = (error) => errorText(error instanceof SearchError ? error.code : "host_unavailable");

  /**
   * Opens or closes the detail of a row. Every opening has its own context (visit id, list epoch, Vault token), so an
   * answer belongs to that opening only: closing, another list, another Vault, another visit's answer or a reopening
   * all drop it without a trace on the screen.
   */
  function toggleDetail(item, ui) {
    if (expanded.has(item.id)) {
      expanded.delete(item.id);
      setOpen(ui, false);
      return;
    }
    if (supported !== true || vaultToken === null || !shownIds.has(item.id) || uiById.get(item.id) !== ui) return;
    void openDetail(item, ui);
  }

  async function openDetail(item, ui) {
    const context = { id: item.id, ui, epoch: listEpoch, token: vaultToken, pending: true, cancelled: false };
    expanded.set(item.id, context);
    setOpen(ui, true);
    showDetailLoading(ui, document);
    const current = () => !context.cancelled && expanded.get(context.id) === context && uiById.get(context.id) === ui && context.epoch === listEpoch && vaultToken !== null && vaultToken === context.token && shownIds.has(context.id);
    let detail;
    try {
      detail = await historyVisit(item.id, options);
    } catch (error) {
      if (!current() || vaultFailed(error)) return;
      if (COMMUNICATION_CODES.has(codeOf(error))) {
        cancelPendingDetails(error);
      } else {
        context.pending = false;
        showDetailError(ui, document, detailErrorText(error), () => retryDetail(item, ui, context));
      }
      return;
    }
    if (detail.id !== context.id || detail.vaultToken !== context.token || !current()) {
      // Another Vault answered while this page still believes in the old one: read again, show nothing of it.
      if (detail.vaultToken !== context.token && current()) accept(detail.vaultToken);
      return;
    }
    context.pending = false;
    showDetail(ui, document, detail);
  }

  function retryDetail(item, ui, context) {
    if (expanded.get(context.id) !== context || context.epoch !== listEpoch) return;
    expanded.delete(context.id);
    if (vaultToken === null) return;
    void openDetail(item, ui);
  }

  /**
   * Opens the full URL of a row whose URL the list cut. The fetch is bound to the screen it was started from: the
   * visit id, the list's epoch and the Vault token then. The tab opens only if the answer is for that visit and Vault,
   * the list is still the same one and still shows that row, the Vault is known and unchanged, and the URL is http(s).
   * Anything else is dropped without a tab.
   */
  async function openCut(item) {
    if (supported !== true || vaultToken === null || !shownIds.has(item.id)) return;
    if (opening && opening.epoch === listEpoch) return;
    const context = { id: item.id, epoch: listEpoch, token: vaultToken };
    opening = context;
    const stillHere = () => opening === context && context.epoch === listEpoch && vaultToken !== null && vaultToken === context.token && shownIds.has(context.id);
    try {
      const visit = await historyVisit(item.id, options);
      if (visit.id !== context.id || visit.vaultToken !== context.token || !stillHere()) {
        // A different Vault answered while this page still believes in the old one: read again, do not open.
        if (visit.vaultToken !== context.token && stillHere()) accept(visit.vaultToken);
        return;
      }
      if (isWebUrl(visit.url)) {
        say("");
        await tabs.create({ url: visit.url });
      } else {
        say("完全なURLを取得できなかったため、開きません。");
      }
    } catch (error) {
      if (!stillHere() || vaultFailed(error)) return;
      if (COMMUNICATION_CODES.has(codeOf(error))) cancelPendingDetails(error);
      say("完全なURLを取得できなかったため、開きません。");
    } finally {
      if (opening === context) opening = null;
    }
  }

  // ---------------------------------------------------------------------------------------------- events

  /** The state with the domain typed so far confirmed (the debounce, if pending, is over). */
  function withDraft() {
    if (typingTimer !== null) timers.clearTimeout(typingTimer);
    typingTimer = null;
    return { ...state, domain: el["domain-input"].value };
  }
  function commitDomain() {
    const next = withDraft();
    if (next.domain === state.domain) return;
    // Typing never adds a history entry.
    commit({ ...next, page: 1 }, { push: false });
  }
  el["domain-input"].addEventListener("input", () => {
    if (typingTimer !== null) timers.clearTimeout(typingTimer);
    typingTimer = timers.setTimeout(commitDomain, debounceMs);
  });
  el["domain-input"].addEventListener("keydown", (event) => {
    if (event.key !== "Enter") return;
    event.preventDefault?.();
    commitDomain();
  });
  for (const mode of ["exact", "regex"]) {
    el[`mode-${mode}`].addEventListener("change", () => {
      commit({ ...withDraft(), mode, page: 1 }, { push: true });
    });
  }
  el["size-select"].addEventListener("change", () => {
    const size = Number(el["size-select"].value);
    commit({ ...withDraft(), size, page: 1 }, { push: true });
  });
  const moveMonth = (delta) => {
    vaultResets = 0;
    shown = addMonths(shown.year, shown.monthIndex, delta);
    counts = null;
    drawCalendar();
    if (supported === true) void loadDays();
  };
  el["cal-prev"].addEventListener("click", () => {
    if (view === "months") {
      if (!inRange(pickerYear - 1)) return;
      pickerYear -= 1;
      drawCalendar();
    } else {
      moveMonth(-1);
    }
  });
  el["cal-next"].addEventListener("click", () => {
    if (view === "months") {
      if (!inRange(pickerYear + 1)) return;
      pickerYear += 1;
      drawCalendar();
    } else {
      moveMonth(1);
    }
  });
  el["cal-title"].addEventListener("click", () => {
    if (view === "days") {
      view = "months";
      pickerYear = shown.year;
      drawCalendar();
      (monthButtons[shown.monthIndex] ?? el["cal-title"]).focus();
    } else {
      view = "days";
      drawCalendar();
      el["cal-title"].focus();
    }
  });
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && view === "months") {
      event.preventDefault?.();
      view = "days";
      drawCalendar();
      el["cal-title"].focus();
    }
  });
  el["cal-jump"].addEventListener("click", () => {
    view = "days";
    shown = { year: state.date.year, monthIndex: state.date.monthIndex };
    counts = null;
    drawCalendar();
    if (supported === true) void loadDays();
    calendar.cell(state.date.day)?.focus();
  });
  el["history-retry"].addEventListener("click", () => void begin());
  el["history-settings"].addEventListener("click", () => openSettings());
  win.addEventListener("popstate", () => {
    if (typingTimer !== null) timers.clearTimeout(typingTimer);
    typingTimer = null;
    view = "days";
    readUrl();
    invalidateList();
    rangeCorrections = 0;
    syncInputs();
    counts = null;
    drawCalendar();
    if (supported === true) {
      void loadList({ reset: true });
      void loadDays();
    }
  });
  win.addEventListener("focus", refresh);
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible") refresh();
  });

  return {
    /** Reads the URL and shows the page. Resolves when the capability is known and both requests are on their way. */
    async start() {
      readUrl();
      syncInputs();
      drawCalendar();
      await begin();
    },
    get state() {
      return { ...state, date: { ...state.date } };
    },
    get shownMonth() {
      return { ...shown };
    },
    get vaultToken() {
      return vaultToken;
    },
  };
}
