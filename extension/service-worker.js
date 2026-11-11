import { describeErrorCode } from "./search-client.js";
import { QueueError, createEventQueue } from "./event-queue.js";
import { createPolicyGate } from "./policy-gate.js";
import { chromeNativeTransport } from "./search-client.js";
import { bytesToBase64 } from "./image-format.js";
import { capturePageMetadata } from "./page-metadata.js";
import { createNavigationTracker } from "./navigation-tracker.js";
import { createNavigationStarts } from "./navigation-start.js";
import { checkHostCapabilities } from "./host-capabilities.js";
import { SAVE_IMAGE_MENU_ID, describeImageSaveResult, registerImageMenu, saveImageFromClick } from "./image-save.js";

const HOST_NAME = "com.memocampus.vault";
const EXCLUDED_SCHEMES = new Set(["chrome:", "edge:", "about:", "chrome-extension:", "file:", "devtools:"]);

function isRecordableUrl(rawUrl) {
  try {
    const parsed = new URL(rawUrl);
    return parsed.protocol === "http:" || parsed.protocol === "https:";
  } catch {
    return false;
  }
}

function sendNative(event) {
  return new Promise((resolve, reject) => {
    chrome.runtime.sendNativeMessage(HOST_NAME, event, (response) => {
      if (chrome.runtime.lastError) return reject(new QueueError("host_unavailable"));
      resolve(response);
    });
  });
}

// Quarantined or dropped events are surfaced on the toolbar icon, never discarded silently.
const IMAGE_FAILURE_KEY = "imageSaveFailure";

async function updateBadge() {
  try {
    const status = await queue.status();
    const problems = status.deadLetters + status.dropped;
    // The last failed image save stays visible until an image is saved successfully.
    const imageFailure = (await chrome.storage.session.get(IMAGE_FAILURE_KEY))[IMAGE_FAILURE_KEY];
    const titles = [];
    const policyProblem = policy.problem();
    if (policyProblem) titles.push(describeErrorCode(policyProblem));
    if (problems > 0) titles.push(`記録できなかったイベント ${status.deadLetters}件・破棄 ${status.dropped}件`);
    const navigationLost = status.navigationInvalid + status.navigationNotStored;
    if (navigationLost > 0) titles.push(`遷移情報を保存できなかった訪問 ${navigationLost}件`);
    if (imageFailure) titles.push(`画像を保存できませんでした: ${imageFailure}`);
    await chrome.action.setBadgeText({ text: titles.length > 0 ? "!" : "" });
    await chrome.action.setTitle({ title: titles.length > 0 ? `Memocampus — ${titles.join(" / ")}` : "Memocampus" });
  } catch {
    // Badge updates are best effort.
  }
}

const policy = createPolicyGate({ send: chromeNativeTransport(chrome.runtime), onChange: () => void updateBadge() });
const queue = createEventQueue({
  storage: chrome.storage.local,
  send: sendNative,
  policy,
  capabilities: () => checkHostCapabilities({ send: sendNative }),
  onChange: () => void updateBadge(),
});

// Observed navigation facts (Issue #41), documents only. The tracker keeps its state in memory and
// decides no privacy: every visit below is judged by the policy first, as before, and only then
// carries its navigation.
const tracker = createNavigationTracker({
  isNormalTab: async (tabId) => (await chrome.tabs.get(tabId))?.incognito === false,
});
const navigationStarts = createNavigationStarts();

/** A policy failure the host reported while delivering also stops collection at once. */
async function flushQueue() {
  const blockedBy = await queue.flush();
  if (typeof blockedBy === "string" && blockedBy.startsWith("policy_")) policy.reportFailure(blockedBy);
  return blockedBy;
}

/** Queues a visit; true if it is now in the queue (with or without its page metadata). */
async function enqueueVisit(event) {
  try {
    await queue.enqueue(event);
  } catch (error) {
    // The visit comes first: if storage could not take it with its metadata, it is kept without.
    let kept = false;
    if (event.page !== undefined && error?.code === "storage_write_failed") {
      const { page: _page, ...visit } = event;
      kept = await queue.enqueue(visit).then(
        () => true,
        () => false,
      );
    }
    // Full queue or storage failure: nowhere durable to keep it, so count it visibly.
    if (!kept) {
      await queue.recordDropped().catch(() => {});
      return false;
    }
  }
  return true;
}

/**
 * Queues a visit, then lets the tracker name it as the source of what follows in its tab. Only a
 * queued visit becomes a source, so a source is always queued before the visit that names it.
 */
async function recordVisit(event, tabId, token) {
  const kept = await enqueueVisit(event);
  if (kept && token) await tracker.recorded(tabId, token, event.id).catch(() => {});
  await flushQueue();
}

function snapshotEvent(tab, recordedAt) {
  return {
    type: "tab_snapshot",
    id: crypto.randomUUID(),
    windowId: tab.windowId,
    tabIndex: tab.index,
    url: tab.url,
    title: typeof tab.title === "string" ? tab.title : "",
    pinned: tab.pinned === true,
    incognito: tab.incognito,
    recordedAt,
  };
}

/**
 * Queues a snapshot of every tab (all or none), then delivers. The result separates
 * "queued" from "stored in SQLite" so the UI never reports unsaved snapshots as saved.
 */
async function recordAllTabs() {
  // Only tabs known to be normal are shown to the host, and only those it allows are kept: the others
  // leave no trace, not even a count. Without a complete answer nothing is recorded at all.
  const tabs = (await chrome.tabs.query({})).filter((tab) => tab.incognito === false && isRecordableUrl(tab.url || ""));
  const judged = await policy.evaluate(tabs.map((tab) => tab.url));
  if (!judged.ok) throw new QueueError(judged.code);
  const recordedAt = new Date().toISOString();
  const events = tabs.filter((tab) => judged.decisions.get(tab.url) === "allow").map((tab) => snapshotEvent(tab, recordedAt));
  if (events.length === 0) return { ok: true, queued: 0, stored: 0, pending: 0, deadLettered: 0 };
  await queue.enqueueAll(events);
  const blockedBy = await flushQueue();
  const { refused = 0, ...where } = await queue.locate(events.map((event) => event.id));
  return {
    ok: true,
    queued: events.length - refused,
    ...where,
    reason: where.pending > 0 && blockedBy ? describeErrorCode(blockedBy) : null,
  };
}

chrome.webNavigation.onCompleted.addListener(async (details) => {
  if (details.frameId !== 0 || !isRecordableUrl(details.url)) return;
  // Read before any await, bound to this document. Completion/commit times cannot prove that a
  // navigation began after first start. Missing evidence (including worker restart) stays unknown.
  const occurredAt = navigationStarts.completed(details);
  // Taken first, in event order: the commit this document came from, if it was observed.
  const observed = await tracker.completed(details).catch(() => null);
  // webNavigation does not say whether the tab is incognito. A tab that is gone (closed meanwhile)
  // or of unknown state is treated like an incognito one: nothing is recorded.
  let tab;
  try {
    tab = await chrome.tabs.get(details.tabId);
  } catch {
    return;
  }
  if (tab?.incognito !== false) return;
  // Decided before anything is written: a refused or undecidable visit is not kept anywhere.
  const judged = await policy.evaluate([details.url], { occurredAt });
  if (!judged.ok || judged.decisions.get(details.url) !== "allow") return;
  const event = {
    type: "visit",
    id: crypto.randomUUID(),
    url: details.url,
    // The tab's title is the tab's now, not the event's: kept only while the tab still shows this URL,
    // so a title of a later (possibly NeverSave) URL never lands on this one (audit F-1).
    title: typeof tab.title === "string" && tab.url === details.url ? tab.title : "",
    incognito: tab.incognito,
    visitedAt: new Date().toISOString(),
    // Navigation start time is distinct from the visit timestamp; retries keep it unchanged.
    recordingOccurredAt: occurredAt === null ? null : new Date(occurredAt).toISOString(),
  };
  // Only now, for an allowed URL in a normal tab, is the page read: once, in this very document.
  // Without an answer the visit is recorded as before, and a stored row is left as it is.
  const page = await capturePageMetadata({
    executeScript: (options) => chrome.scripting.executeScript(options),
    tabId: details.tabId,
    documentId: details.documentId,
    url: details.url,
  });
  // The title and page were read after the load, while the page could still change its URL and content
  // (pushState, even to a NeverSave URL and back). Kept only if no history update was seen in this
  // document, up to now; otherwise the visit is recorded without them.
  if (await tracker.pristine(details.tabId, details.documentId).catch(() => false)) {
    if (page) event.page = page;
  } else {
    event.title = "";
  }
  if (observed?.navigation) event.navigation = observed.navigation;
  void recordVisit(event, details.tabId, observed?.token);
});

chrome.webNavigation.onBeforeNavigate.addListener((details) => navigationStarts.before(details));
chrome.webNavigation.onCommitted.addListener((details) => {
  navigationStarts.committed(details);
  void tracker.committed(details).catch(() => {});
});
chrome.webNavigation.onErrorOccurred.addListener((details) => navigationStarts.failed(details));
chrome.webNavigation.onCreatedNavigationTarget.addListener((details) => void tracker.createdNavigationTarget(details).catch(() => {}));
chrome.tabs.onRemoved.addListener((tabId) => {
  navigationStarts.removed(tabId);
  void tracker.tabRemoved(tabId).catch(() => {});
});
chrome.tabs.onReplaced.addListener((addedTabId, removedTabId) => {
  navigationStarts.removed(addedTabId);
  navigationStarts.removed(removedTabId);
  void tracker.tabReplaced(addedTabId, removedTabId).catch(() => {});
});

// A URL change inside a document (pushState / replaceState) is never recorded: the application keeps
// its own state. It only makes the tracker forget what the tab showed (PO decision after the audit).
chrome.webNavigation.onHistoryStateUpdated.addListener((details) => void tracker.historyStateUpdated(details).catch(() => {}));

chrome.runtime.onStartup.addListener(() => void flushQueue());
chrome.runtime.onInstalled.addListener(() => {
  void flushQueue();
  registerImageMenu(chrome.contextMenus, chrome.runtime);
});

// --- Saving images from the context menu --------------------------------------------------------

const FALLBACK_ICON = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";

/** A plain generated icon (no bundled asset): the saved thumbnail on success, a mark otherwise. */
async function notificationIcon(result) {
  if (result.ok && result.thumbnail) return `data:${result.thumbnail.mimeType};base64,${bytesToBase64(result.thumbnail.bytes)}`;
  try {
    const canvas = new OffscreenCanvas(64, 64);
    const context = canvas.getContext("2d");
    context.fillStyle = result.ok ? "#0b57d0" : "#a50e0e";
    context.fillRect(0, 0, 64, 64);
    context.fillStyle = "#ffffff";
    context.font = "bold 40px sans-serif";
    context.textAlign = "center";
    context.textBaseline = "middle";
    context.fillText(result.ok ? "M" : "!", 32, 34);
    const blob = await canvas.convertToBlob({ type: "image/png" });
    return `data:image/png;base64,${bytesToBase64(new Uint8Array(await blob.arrayBuffer()))}`;
  } catch {
    return FALLBACK_ICON;
  }
}

async function reportImageSave(result) {
  const { title, message } = describeImageSaveResult(result);
  try {
    if (result.ok) await chrome.storage.session.remove(IMAGE_FAILURE_KEY);
    else await chrome.storage.session.set({ [IMAGE_FAILURE_KEY]: message });
  } catch {
    // storage.session unavailable: the notification below still reports it
  }
  await updateBadge();
  try {
    await chrome.notifications.create(`memocampus-image-${Date.now()}`, {
      type: "basic",
      iconUrl: await notificationIcon(result),
      title,
      message,
      priority: result.ok ? 0 : 1,
    });
  } catch {
    // Notifications disabled or failing: a failure is still visible on the toolbar badge.
  }
  // Lets an open side panel react (used by the Image tab in a later slice).
  chrome.runtime.sendMessage({ type: "image-save-result", ok: result.ok, code: result.code ?? null }).catch(() => {});
}

const imageDeps = {
  fetchImpl: (...args) => fetch(...args),
  executeScript: (options) => chrome.scripting.executeScript(options),
  createImageBitmap: (...args) => createImageBitmap(...args),
  OffscreenCanvas: globalThis.OffscreenCanvas,
  subtle: crypto.subtle,
  randomUUID: () => crypto.randomUUID(),
  connect: () => chrome.runtime.connectNative(HOST_NAME),
  runtime: chrome.runtime,
};

chrome.contextMenus.onClicked.addListener((info, tab) => {
  if (info.menuItemId !== SAVE_IMAGE_MENU_ID) return;
  void saveImageFromClick(info, tab, imageDeps).then(reportImageSave);
});

// Clicking the toolbar action opens the search side panel. Re-applied on every worker start.
chrome.sidePanel.setPanelBehavior({ openPanelOnActionClick: true }).catch(() => {});
void updateBadge();

chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
  if (message?.type === "policy-initialize") {
    sendResponse({ ok: false, code: "policy_start_required" });
    return false;
  }
  if (message?.type === "policy-status") {
    const request = policy.current({ maxAgeMs: 0 }).then((verdict) => policy.view(verdict));
    request.then(
      async (view) => {
        // A policy that became usable releases what was held back while it was not.
        if (view.state === "ready") await flushQueue().catch(() => {});
        sendResponse({ ok: true, ...view, message: view.code ? describeErrorCode(view.code) : null });
      },
      () => sendResponse({ ok: false }),
    );
    return true;
  }
  if (message?.type === "flush-queue") {
    flushQueue().then(
      (blockedBy) => sendResponse({ ok: true, blockedBy }),
      () => sendResponse({ ok: false }),
    );
    return true;
  }
  if (message?.type === "queue-status") {
    queue.status().then(
      (status) => sendResponse({ ok: true, ...status, blockedReason: status.blockedBy ? describeErrorCode(status.blockedBy) : null }),
      () => sendResponse({ ok: false }),
    );
    return true;
  }
  if (message?.type !== "record-all-tabs") return false;
  recordAllTabs()
    .then((result) => sendResponse(result))
    .catch((error) => sendResponse({ ok: false, code: error?.code || null, error: queueErrorText(error) }));
  return true;
});

function queueErrorText(error) {
  if (error?.code === "queue_full") return "送信待ちキューが満杯のため、記録しませんでした（1件も登録していません）。";
  if (error?.code === "storage_write_failed") return "キューへの保存に失敗したため、記録しませんでした（1件も登録していません）。";
  if (typeof error?.code === "string") return `${describeErrorCode(error.code)}（1件も登録していません）`;
  return error?.message || "記録に失敗しました。";
}
