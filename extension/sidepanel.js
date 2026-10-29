import { PAGE_SIZE, MAX_QUERY_CHARS, SearchError, VAULT_UNAVAILABLE_CODES, chromeNativeTransport, createLatestGate, isWebUrl, searchPages } from "./search-client.js";
import { displayUrl, formatFullDate, formatShortDate } from "./format.js";
import { createVaultPanel } from "./vault-panel.js";
import { describeQueueStatus, describeRecordResult } from "./queue-text.js";
import { createTabs } from "./tabs.js";
import { vaultNeedsAttention } from "./vault-client.js";

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
const urlScroll = document.querySelector("#url-scroll");
const settingsAttention = document.querySelector("#settings-attention");
const settingsTab = document.querySelector("#tab-settings");

const send = chromeNativeTransport(chrome.runtime);
const gate = createLatestGate();
const tabs = createTabs(
  [
    { id: "url", tab: document.querySelector("#tab-url"), panel: document.querySelector("#panel-url"), scroller: urlScroll },
    { id: "image", tab: document.querySelector("#tab-image"), panel: document.querySelector("#panel-image") },
    { id: "settings", tab: settingsTab, panel: document.querySelector("#panel-settings") },
  ],
  {
    onChange: (id, previous) => {
      if (id === "settings" && previous !== null) void vaultPanel.refresh();
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

const vaultPanel = createVaultPanel({
  send,
  reveal: () => tabs.select("settings"),
  onStatus: (status) => showVaultAttention(vaultNeedsAttention(status)),
  onVaultChanged: () => {
    // Drop the previous vault's results at once so they are never shown as the new vault's.
    clearResults();
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

const state = {
  token: 0,
  query: "",
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
  const label = state.query ? "件" : "件（最近の順）";
  summary.textContent = state.count === 0 ? "" : `${state.count}${state.hasMore ? "件以上" : label}`;
}

function timeLabel(prefix, iso) {
  const time = document.createElement("time");
  time.dateTime = iso;
  time.title = `${prefix}: ${formatFullDate(iso)}`;
  time.textContent = `${prefix} ${formatShortDate(iso)}`;
  return time;
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
  li.append(link);
  return li;
}

function appendItems(items) {
  const fragment = document.createDocumentFragment();
  for (const item of items) {
    // Offsets can shift while new pages are recorded; never show a URL twice.
    if (state.seenUrls.has(item.url)) continue;
    state.seenUrls.add(item.url);
    fragment.append(renderItem(item));
    state.count += 1;
  }
  results.append(fragment);
}

function clearResults() {
  gate.next(); // responses still in flight belong to the previous vault
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
  empty.textContent = state.query ? `「${state.query}」に一致するページはありません。` : "まだ記録されたページがありません。";
}

async function runSearch() {
  clearTimeout(debounceTimer);
  const token = gate.next();
  state.token = token;
  state.query = currentQuery();
  state.loadingMore = false;
  state.lastFailedAppend = false;
  errorBox.hidden = true;
  results.setAttribute("aria-busy", "true");
  loadMoreButton.hidden = true;
  updateSummary(true);
  try {
    const page = await searchPages({ query: state.query, offset: 0, limit: PAGE_SIZE }, { send });
    if (!gate.isCurrent(token)) return;
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
    const page = await searchPages({ query: state.query, offset: state.offset, limit: PAGE_SIZE }, { send });
    if (!gate.isCurrent(token)) return;
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
results.addEventListener("auxclick", openResult);
retryButton.addEventListener("click", () => void (state.lastFailedAppend ? loadMore() : runSearch()));
loadMoreButton.addEventListener("click", () => void loadMore());
openVaultSettings.addEventListener("click", () => tabs.select("settings", { focus: true }));

// Load the next page automatically as the list end scrolls into view. The URL tab scrolls on its
// own, so it is the observer root; while the tab is hidden nothing intersects.
new IntersectionObserver((entries) => {
  if (entries.some((entry) => entry.isIntersecting)) void loadMore();
}, { root: urlScroll, rootMargin: "200px" }).observe(loadMoreButton);

// Pick up pages recorded while the panel stayed open but hidden, unless the user has scrolled away.
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState !== "visible") return;
  void refreshQueueStatus();
  if (!state.loadingMore && tabs.getScroll("url") === 0) void runSearch();
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
    void refreshQueueStatus();
  }
});

tabs.select("url");
input.focus();
void runSearch();
void refreshQueueStatus();
void vaultPanel.refresh().then((status) => {
  // Redirect (and move focus) only if the user has not started using the panel meanwhile; otherwise
  // the dot on the settings tab and the search error point the way without hiding what they use.
  if (vaultNeedsAttention(status) && !userActed) tabs.select("settings", { focus: true });
});
