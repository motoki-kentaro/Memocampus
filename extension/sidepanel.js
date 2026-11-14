import { PAGE_SIZE, MAX_QUERY_CHARS, SearchError, VAULT_UNAVAILABLE_CODES, chromeNativeTransport, createLatestGate, isWebUrl, searchPages } from "./search-client.js";
import { displayUrl, formatFullDate, formatShortDate } from "./format.js";
import { createVaultPanel } from "./vault-panel.js";
import { describePolicyStatus, describeQueueStatus, describeRecordResult } from "./queue-text.js";
import { createTabs } from "./tabs.js";
import { createImageGallery } from "./image-gallery.js";
import { createPagePanel } from "./page-panel.js";
import { vaultNeedsAttention } from "./vault-client.js";
import { checkHostCapabilities } from "./host-capabilities.js";
import { ACTION_LABEL, DECISION_LABEL, SAME_STATE, createItemDialog, sameState } from "./item-dialog.js";
import { evaluateUrls } from "./vault-items-client.js";
import { createVisitCalendar, formatDate, localDayBounds } from "./visit-calendar.js";

const DEBOUNCE_MS = 200;

const input = document.querySelector("#query");
const summary = document.querySelector("#summary");
const results = document.querySelector("#results");
const empty = document.querySelector("#empty");
const errorBox = document.querySelector("#error");
const errorMessage = document.querySelector("#error-message");
const retryButton = document.querySelector("#retry");
const loadMoreButton = document.querySelector("#load-more");
const recordButton = document.querySelector("#record-tabs");
const recordStatus = document.querySelector("#record-status");
const openVaultSettings = document.querySelector("#open-vault-settings");
const queueStatus = document.querySelector("#queue-status");
const policyStatus = document.querySelector("#policy-status");
const policyMessage = document.querySelector("#policy-message");
const policyInitialize = document.querySelector("#policy-initialize");
const policyRecheck = document.querySelector("#policy-recheck");
const urlScroll = document.querySelector("#url-scroll");
const settingsAttention = document.querySelector("#settings-attention");
const settingsTab = document.querySelector("#tab-settings");
const itemStatus = document.querySelector("#url-item-status");
const calendarButton = document.querySelector("#visit-calendar-open");
const dateChip = document.querySelector("#visit-date-chip");
const dateChipText = document.querySelector("#visit-date-chip-text");
const dateClear = document.querySelector("#visit-date-clear");

const send = chromeNativeTransport(chrome.runtime);
const gate = createLatestGate();
const openPrivacySettings = () => {
  chrome.runtime.openOptionsPage().catch(() => {});
};

// Item actions (Issue #43) are offered only by a host that names `vault_items_v1`: null until known.
let itemsSupported = null;
// The calendar (Issue #5) only with a host that names `visit_calendar_v1`: an older host would ignore a
// date range and answer the unfiltered list. null until known.
let calendarSupported = null;
const itemDialog = createItemDialog({ send, openPrivacySettings, onVaultUnavailable: () => void vaultPanel.refresh() });
const tabs = createTabs(
  [
    { id: "url", tab: document.querySelector("#tab-url"), panel: document.querySelector("#panel-url"), scroller: urlScroll },
    { id: "image", tab: document.querySelector("#tab-image"), panel: document.querySelector("#panel-image") },
    { id: "page", tab: document.querySelector("#tab-page"), panel: document.querySelector("#panel-page") },
    { id: "settings", tab: settingsTab, panel: document.querySelector("#panel-settings") },
  ],
  {
    onChange: (id, previous) => {
      if (id !== "url") calendar.close({ restoreFocus: false });
      if (id === "settings" && previous !== null) void vaultPanel.refresh();
      if (id === "image") gallery.activate();
      if (id === "page") pagePanel.activate();
    },
  },
);

function showVaultAttention(needed) {
  settingsAttention.hidden = !needed;
  // The dot is decorative (aria-hidden); the accessible name and tooltip carry the same message.
  const label = needed ? "設定（Vaultの確認が必要です）" : "設定";
  settingsTab.setAttribute("aria-label", label);
  settingsTab.title = label;
}

// Set once the user presses a key or a pointer, so the startup redirect never takes over their input.
let userActed = false;
for (const type of ["keydown", "pointerdown"]) {
  document.addEventListener(type, () => { userActed = true; }, { capture: true });
}

const gallery = createImageGallery({
  send,
  isActive: () => tabs.current === "image",
  openUrl: (url) => {
    if (isWebUrl(url)) chrome.tabs.create({ url }).catch(() => {});
  },
  openSettings: () => tabs.select("settings", { focus: true }),
  onVaultUnavailable: () => void vaultPanel.refresh(),
  itemDialog,
  itemsSupported: () => itemsSupported,
  onPolicyChanged: () => void evaluateRows(rows()),
});

// Saves finish in the service worker; refresh the Image tab only when that does not disturb the user.
const pagePanel = createPagePanel({
  connect: () => chrome.runtime.connectNative("com.memocampus.vault"), runtime: chrome.runtime,
  isActive: () => tabs.current === "page",
  openUrl: (url) => { if (isWebUrl(url)) chrome.tabs.create({ url }).catch(() => {}); },
});
chrome.runtime.onMessage.addListener((message) => {
  if (message?.type === "image-save-result" && message.ok === true) gallery.imageSaved();
  if (message?.type === "page-save-result") pagePanel.saved(message);
  if (message?.type === "page-vault-changed") pagePanel.vaultChanged();
});

const vaultPanel = createVaultPanel({
  send,
  reveal: () => tabs.select("settings"),
  onStatus: (status) => { showVaultAttention(vaultNeedsAttention(status)); pagePanel.vaultStatus(status); },
  onVaultChanged: () => {
    // Drop the previous vault's results at once so they are never shown as the new vault's.
    clearResults();
    // The chosen day, the open calendar and its counts were the old vault's (PO decision: cleared).
    dropDate();
    calendar.vaultChanged();
    gallery.vaultChanged();
    pagePanel.vaultChanged();
    chrome.runtime.sendMessage({ type: "page-vault-changed" }).catch(() => {});
    void runSearch();
    // Visits queued while no vault was usable can now be delivered.
    chrome.runtime
      .sendMessage({ type: "flush-queue" })
      .catch(() => {})
      .finally(() => void refreshQueueStatus());
  },
});

async function refreshQueueStatus() {
  try {
    const text = describeQueueStatus(await chrome.runtime.sendMessage({ type: "queue-status" }));
    queueStatus.textContent = text;
    queueStatus.hidden = text === "";
  } catch {
    queueStatus.hidden = true;
  }
}

function showPolicyStatus(status) {
  const text = describePolicyStatus(status);
  policyMessage.textContent = text;
  policyStatus.hidden = text === "";
  // Only first use can be initialized; a damaged or lost policy is never offered a reset.
  policyInitialize.hidden = !(status?.ok === true && status.state === "uninitialized");
}

async function requestPolicy(type) {
  policyInitialize.disabled = true;
  policyRecheck.disabled = true;
  try {
    showPolicyStatus(await chrome.runtime.sendMessage({ type }));
  } catch {
    showPolicyStatus(null);
  } finally {
    policyInitialize.disabled = false;
    policyRecheck.disabled = false;
    void refreshQueueStatus();
  }
}

policyInitialize.addEventListener("click", () => void chrome.runtime.openOptionsPage());
policyRecheck.addEventListener("click", () => void requestPolicy("policy-status"));

const state = {
  token: 0,
  vaultToken: null, // the vault the shown results came from (null: a host that does not say)
  query: "",
  // The chosen local day (Issue #5), apart from the query: { year, monthIndex, day, label, visitedFrom,
  // visitedTo, vaultToken } or null. `vaultToken` is the vault whose calendar offered it.
  date: null,
  visited: null, // the range the shown list was searched with ({ visitedFrom, visitedTo }) or null
  offset: 0,
  seenUrls: new Set(),
  count: 0,
  hasMore: false,
  loadingMore: false,
  lastFailedAppend: false,
};

let debounceTimer;

function currentQuery() {
  return input.value.trim().slice(0, MAX_QUERY_CHARS);
}

function showError(error) {
  const vaultUnavailable = error instanceof SearchError && VAULT_UNAVAILABLE_CODES.has(error.code);
  showErrorText(error instanceof SearchError ? error.message : "検索に失敗しました。", vaultUnavailable);
  // Point at the settings tab (badge + button) without pulling the user away from the search box.
  if (vaultUnavailable) void vaultPanel.refresh();
}

function showErrorText(text, offerVaultSettings = false) {
  errorMessage.textContent = text;
  openVaultSettings.hidden = !offerVaultSettings;
  errorBox.hidden = false;
}

function updateSummary(loading) {
  if (loading) {
    summary.textContent = "検索中…";
    return;
  }
  if (state.visited && state.date) {
    // A day filter is always named, also with no result.
    summary.textContent = `${state.count}${state.hasMore ? "件以上" : "件"}（${state.date.label}）`;
    return;
  }
  const label = state.query ? "件" : "件（最近の順）";
  summary.textContent = state.count === 0 ? "" : `${state.count}${state.hasMore ? "件以上" : label}`;
}

// --- The day filter (Issue #5) ------------------------------------------------------------------------

const calendar = createVisitCalendar({
  launcher: calendarButton,
  popover: document.querySelector("#visit-calendar"),
  send,
  getVaultToken: () => state.vaultToken,
  getSelected: () => state.date,
  onSelect: ({ year, monthIndex, day, vaultToken }) => {
    state.date = { year, monthIndex, day, label: formatDate(year, monthIndex, day), ...localDayBounds(year, monthIndex, day), vaultToken };
    renderDateChip();
    void runSearch();
  },
  onVaultMismatch: () => {
    // The host counted in another vault than the list's: another window switched it.
    vaultMismatch();
  },
  onVaultUnavailable: () => void vaultPanel.refresh(),
  fallbackFocus: () => input.focus(),
});

function renderDateChip() {
  dateChip.hidden = state.date === null;
  dateChipText.textContent = state.date?.label ?? "";
  const label = state.date ? `日付の絞り込みを解除（${state.date.label}）` : "日付の絞り込みを解除";
  dateClear.setAttribute("aria-label", label);
  dateClear.title = label;
}

/** Forgets the chosen day (the caller searches again if needed). */
function dropDate() {
  state.date = null;
  renderDateChip();
}

/** The list or the counts came from another vault than expected: start over in the active one. */
function vaultMismatch() {
  calendar.close({ restoreFocus: false });
  if (state.date) {
    dropDate();
    showItemStatus("Vaultが切り替えられたため、日付の絞り込みを解除しました。");
  }
  void runSearch();
}

/** The calendar can be opened with a host that offers it and a list read from a known vault. */
function updateCalendarAvailability() {
  if (calendarSupported === false) calendar.setAvailable(false, "Native Hostが閲覧日カレンダーに対応していません。Hostを更新してください。");
  else if (calendarSupported === null) calendar.setAvailable(false, "Native Hostの対応状況を確認しています。");
  else if (state.vaultToken === null) calendar.setAvailable(false, "Vaultを読み込めないため、閲覧日で絞り込めません。");
  else calendar.setAvailable(true);
}

dateClear.addEventListener("click", () => {
  if (!state.date) return;
  dropDate();
  (calendarButton.disabled ? input : calendarButton).focus();
  void runSearch();
});

function timeLabel(prefix, iso) {
  const time = document.createElement("time");
  time.dateTime = iso;
  time.title = `${prefix}: ${formatFullDate(iso)}`;
  time.textContent = `${prefix} ${formatShortDate(iso)}`;
  return time;
}

// --- Item actions on a result (Issue #43) -----------------------------------------------------------

const ROW_ACTIONS = ["never_save", "save_but_no_mcp", "delete", "publish"];
let rowCounter = 0;
let decisionStamp = 0;

const rows = () => [...results.children];

function actionsOf(li) {
  const box = li.children[1];
  return { box, badge: box.children[0], select: box.children[1] };
}

/** Shows the host's decision for a row, and disables what would not change it. */
function applyDecision(li, decision, stamp) {
  if (Number(li.dataset.decisionStamp ?? 0) > stamp) return;
  li.dataset.decisionStamp = String(stamp);
  const { badge, select } = actionsOf(li);
  badge.dataset.decision = decision ?? "";
  badge.textContent = decision ? DECISION_LABEL[decision] : "状態不明";
  for (const option of select.children) {
    if (!option.value) continue;
    const same = decision ? sameState(option.value, decision) : false;
    option.disabled = same;
    option.textContent = same ? `${ACTION_LABEL[option.value]}（${SAME_STATE[option.value]}）` : ACTION_LABEL[option.value];
  }
}

/** Asks the host how it judges the URLs of `lis`, in one batch; a later answer wins. */
async function evaluateRows(lis) {
  if (itemsSupported !== true || lis.length === 0) return;
  const stamp = ++decisionStamp;
  try {
    const decisions = await evaluateUrls(lis.map((li) => li.dataset.url), { send });
    for (const li of lis) if (results.contains(li)) applyDecision(li, decisions.get(li.dataset.url) ?? null, stamp);
  } catch {
    for (const li of lis) if (results.contains(li)) applyDecision(li, null, stamp);
  }
}

function renderActions(item) {
  rowCounter += 1;
  const box = document.createElement("div");
  box.className = "hit-actions";
  box.hidden = itemsSupported !== true;
  const badge = document.createElement("span");
  badge.className = "hit-state";
  badge.id = `hit-state-${rowCounter}`;
  badge.textContent = "確認中…";
  const select = document.createElement("select");
  select.className = "item-action";
  select.setAttribute("aria-label", `「${item.title || displayUrl(item.url)}」の操作`);
  select.setAttribute("aria-describedby", badge.id);
  const placeholder = document.createElement("option");
  placeholder.value = "";
  placeholder.textContent = "操作を選択…";
  select.append(placeholder);
  for (const action of ROW_ACTIONS) {
    const option = document.createElement("option");
    option.value = action;
    option.textContent = ACTION_LABEL[action];
    select.append(option);
  }
  select.value = "";
  box.append(badge, select);
  return box;
}

function showItemStatus(text) {
  itemStatus.textContent = text;
  itemStatus.hidden = text === "";
}

/** The host deleted `url`: its row goes, and the next page starts one row earlier. */
function removeRow(li, deleted) {
  const index = rows().indexOf(li);
  li.remove();
  state.seenUrls.delete(li.dataset.url);
  state.count = Math.max(0, state.count - 1);
  state.offset = Math.max(0, state.offset - 1);
  renderEmpty();
  updateSummary(false);
  showItemStatus(`Vaultから削除しました（閲覧記録${deleted.visits}件・スナップショット${deleted.tabSnapshots}件）。`);
  const next = rows()[index] ?? rows()[index - 1];
  (next?.querySelector("a.hit") ?? input).focus();
}

function chooseAction(select) {
  const li = select.closest("li");
  const action = select.value;
  if (!li || !results.contains(li) || !ROW_ACTIONS.includes(action) || itemsSupported !== true) {
    select.value = "";
    return;
  }
  showItemStatus("");
  const url = li.dataset.url;
  const label = li.dataset.title || displayUrl(url);
  const onClose = () => {
    select.value = "";
  };
  if (action === "delete") {
    itemDialog.openUrlDelete({
      owner: "url",
      url,
      label,
      vaultToken: state.vaultToken,
      opener: select,
      onClose,
      onDeleted: (deleted) => removeRow(li, deleted),
      onVaultMismatch: () => {
        void runSearch();
        showItemStatus("Vaultが切り替えられたため、削除しませんでした。一覧を読み込み直しています。");
      },
    });
    return;
  }
  itemDialog.openPolicy({
    owner: "url",
    action,
    targets: [{ url, label: "このページ" }],
    targetText: `対象: ${label}\n${url}`,
    opener: select,
    onClose,
    onApplied: (result) => {
      showItemStatus(`「${ACTION_LABEL[action]}」を実行しました。現在の状態: ${DECISION_LABEL[result.decision]}`);
      void evaluateRows(rows());
      gallery.policyChanged();
    },
  });
}

/** Learns once whether the host offers item actions; asked again after a failure. */
async function checkItems() {
  if (itemsSupported !== null) return;
  const answer = await checkHostCapabilities({ send });
  if (!answer.ok || itemsSupported !== null) return;
  itemsSupported = answer.vaultItems;
  calendarSupported = answer.visitCalendar;
  updateCalendarAvailability();
  for (const li of rows()) actionsOf(li).box.hidden = !itemsSupported;
  gallery.itemsChanged();
  void evaluateRows(rows());
}

function renderItem(item) {
  const link = document.createElement("a");
  link.className = "hit";
  link.href = item.url;
  link.title = item.url;
  link.dataset.source = item.source;

  const title = document.createElement("span");
  title.className = "hit-title";
  title.textContent = item.title || displayUrl(item.url);

  const url = document.createElement("span");
  url.className = "hit-url";
  url.textContent = displayUrl(item.url);

  const meta = document.createElement("span");
  meta.className = "hit-meta";
  if (item.lastVisitedAt) meta.append(timeLabel("閲覧", item.lastVisitedAt));
  if (item.lastCapturedAt) meta.append(timeLabel("スナップショット", item.lastCapturedAt));

  link.append(title, url, meta);
  const li = document.createElement("li");
  li.dataset.url = item.url;
  li.dataset.title = item.title;
  li.append(link, renderActions(item));
  return li;
}

function appendItems(items) {
  const fragment = document.createDocumentFragment();
  const added = [];
  for (const item of items) {
    // Offsets can shift while new pages are recorded; never show a URL twice.
    if (state.seenUrls.has(item.url)) continue;
    state.seenUrls.add(item.url);
    const li = renderItem(item);
    added.push(li);
    fragment.append(li);
    state.count += 1;
  }
  results.append(fragment);
  void evaluateRows(added);
}

function clearResults() {
  gate.next(); // responses still in flight belong to the previous vault
  itemDialog.cancel("url");
  state.vaultToken = null;
  state.visited = null;
  updateCalendarAvailability();
  results.replaceChildren();
  state.seenUrls = new Set();
  state.count = 0;
  state.offset = 0;
  state.hasMore = false;
  state.loadingMore = false;
  empty.hidden = true;
  errorBox.hidden = true;
  loadMoreButton.hidden = true;
  summary.textContent = "";
  tabs.setScroll("url", 0);
}

function renderEmpty() {
  empty.hidden = state.count !== 0;
  const day = state.visited && state.date ? state.date.label : null;
  if (day) empty.textContent = state.query ? `「${state.query}」に一致する、${day}に閲覧したページはありません。` : `${day}に閲覧したページはありません。`;
  else empty.textContent = state.query ? `「${state.query}」に一致するページはありません。` : "まだ記録されたページがありません。";
}

async function runSearch() {
  clearTimeout(debounceTimer);
  const token = gate.next();
  state.token = token;
  state.query = currentQuery();
  // The chosen day narrows the search (AND the query), and only with a host that offers it.
  const date = state.date;
  const visited = date && calendarSupported === true ? { visitedFrom: date.visitedFrom, visitedTo: date.visitedTo } : null;
  state.visited = visited;
  state.loadingMore = false;
  state.lastFailedAppend = false;
  state.hasMore = false; // the next page of the previous list must not be asked with the new terms
  errorBox.hidden = true;
  results.setAttribute("aria-busy", "true");
  loadMoreButton.hidden = true;
  updateSummary(true);
  try {
    const page = await searchPages({ query: state.query, offset: 0, limit: PAGE_SIZE, ...visited }, { send });
    if (!gate.isCurrent(token)) return;
    if (date && page.vaultToken !== date.vaultToken) {
      // The day was chosen in another vault's calendar: it is not applied to this one.
      vaultMismatch();
      return;
    }
    // The rows are replaced: a dialog about one of them would name a row that is gone.
    itemDialog.cancel("url");
    state.vaultToken = page.vaultToken;
    updateCalendarAvailability();
    void checkItems();
    results.replaceChildren();
    state.seenUrls = new Set();
    state.count = 0;
    state.offset = page.received;
    state.hasMore = page.hasMore;
    appendItems(page.items);
    renderEmpty();
    tabs.setScroll("url", 0);
  } catch (error) {
    if (!gate.isCurrent(token)) return;
    results.replaceChildren();
    state.count = 0;
    state.hasMore = false;
    // Which vault is active is not known now: the calendar waits for a list again.
    state.vaultToken = null;
    updateCalendarAvailability();
    empty.hidden = true;
    showError(error);
  } finally {
    if (gate.isCurrent(token)) {
      results.removeAttribute("aria-busy");
      loadMoreButton.hidden = !state.hasMore;
      updateSummary(false);
    }
  }
}

async function loadMore() {
  if (!state.hasMore || state.loadingMore) return;
  const token = state.token;
  state.loadingMore = true;
  loadMoreButton.disabled = true;
  loadMoreButton.textContent = "読み込み中…";
  try {
    const page = await searchPages({ query: state.query, offset: state.offset, limit: PAGE_SIZE, ...state.visited }, { send });
    if (!gate.isCurrent(token)) return;
    if (page.vaultToken !== state.vaultToken) {
      // Another vault became active (another window): its rows never join this list.
      vaultMismatch();
      return;
    }
    state.offset += page.received;
    state.hasMore = page.hasMore;
    state.lastFailedAppend = false;
    errorBox.hidden = true;
    appendItems(page.items);
  } catch (error) {
    if (!gate.isCurrent(token)) return;
    state.lastFailedAppend = true;
    showError(error);
  } finally {
    if (gate.isCurrent(token)) {
      state.loadingMore = false;
      loadMoreButton.disabled = false;
      loadMoreButton.textContent = "さらに読み込む";
      loadMoreButton.hidden = !state.hasMore;
      updateSummary(false);
    }
  }
}

function scheduleSearch() {
  clearTimeout(debounceTimer);
  debounceTimer = setTimeout(() => {
    if (currentQuery() !== state.query) void runSearch();
  }, DEBOUNCE_MS);
}

function openResult(event) {
  const link = event.target.closest("a.hit");
  if (!link || !results.contains(link)) return;
  if (event.type === "auxclick" && event.button !== 1) return;
  event.preventDefault();
  if (!isWebUrl(link.href)) return;
  const background = event.button === 1 || event.ctrlKey || event.metaKey;
  chrome.tabs.create({ url: link.href, active: !background }).catch(() => showErrorText("タブを開けませんでした。"));
}

input.addEventListener("input", (event) => {
  // Wait for IME composition to finish before searching on Japanese input.
  if (event.isComposing) return;
  scheduleSearch();
});
input.addEventListener("compositionend", scheduleSearch);
input.addEventListener("keydown", (event) => {
  if (event.key === "Enter" && !event.isComposing) {
    if (currentQuery() !== state.query) void runSearch();
    else results.querySelector("a.hit")?.focus();
  }
});

results.addEventListener("click", openResult);
results.addEventListener("change", (event) => {
  const select = event.target.closest?.("select.item-action");
  if (select && results.contains(select)) chooseAction(select);
});
results.addEventListener("auxclick", openResult);
retryButton.addEventListener("click", () => void (state.lastFailedAppend ? loadMore() : runSearch()));
loadMoreButton.addEventListener("click", () => void loadMore());
openVaultSettings.addEventListener("click", () => tabs.select("settings", { focus: true }));
// The options page opens in a tab of its own (or the open one is focused).
document.querySelector("#open-privacy-settings").addEventListener("click", openPrivacySettings);

// Load the next page automatically as the list end scrolls into view. The URL tab scrolls on its
// own, so it is the observer root; while the tab is hidden nothing intersects.
new IntersectionObserver((entries) => {
  if (entries.some((entry) => entry.isIntersecting)) void loadMore();
}, { root: urlScroll, rootMargin: "200px" }).observe(loadMoreButton);

// Pick up pages recorded while the panel stayed open but hidden, unless the user has scrolled away.
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState !== "visible") return;
  void requestPolicy("policy-status");
  if (!state.loadingMore && tabs.getScroll("url") === 0) void runSearch();
  // Visits recorded meanwhile: the open month is counted again (no polling, no cache).
  calendar.refresh();
});

recordButton.addEventListener("click", async () => {
  recordButton.disabled = true;
  recordStatus.hidden = false;
  recordStatus.textContent = "記録中…";
  try {
    const result = await chrome.runtime.sendMessage({ type: "record-all-tabs" });
    recordStatus.textContent = describeRecordResult(result);
    if (result?.stored > 0) void runSearch();
  } catch (error) {
    recordStatus.textContent = describeRecordResult({ ok: false, error: error.message });
  } finally {
    recordButton.disabled = false;
    void requestPolicy("policy-status");
  }
});

tabs.select(globalThis.location?.hash === "#settings" ? "settings" : "url");
input.focus();
void runSearch();
void checkItems();
void requestPolicy("policy-status");
void vaultPanel.refresh().then((status) => {
  // Redirect (and move focus) only if the user has not started using the panel meanwhile; otherwise
  // the dot on the settings tab and the search error point the way without hiding what they use.
  if (vaultNeedsAttention(status) && !userActed) tabs.select("settings", { focus: true });
});
