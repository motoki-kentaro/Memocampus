import { describeErrorCode } from "./search-client.js";
import { QueueError, createEventQueue } from "./event-queue.js";

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
async function updateBadge() {
  try {
    const status = await queue.status();
    const problems = status.deadLetters + status.dropped;
    await chrome.action.setBadgeText({ text: problems > 0 ? "!" : "" });
    await chrome.action.setTitle({
      title: problems > 0 ? `Memocampus — 記録できなかったイベント ${status.deadLetters}件・破棄 ${status.dropped}件` : "Memocampus",
    });
  } catch {
    // Badge updates are best effort.
  }
}

const queue = createEventQueue({ storage: chrome.storage.local, send: sendNative, onChange: () => void updateBadge() });

async function enqueueVisit(event) {
  try {
    await queue.enqueue(event);
  } catch {
    // Full queue or storage failure: nowhere durable to keep it, so count it visibly.
    await queue.recordDropped().catch(() => {});
    return;
  }
  await queue.flush();
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
    incognito: tab.incognito === true,
    recordedAt,
  };
}

/**
 * Queues a snapshot of every tab (all or none), then delivers. The result separates
 * "queued" from "stored in SQLite" so the UI never reports unsaved snapshots as saved.
 */
async function recordAllTabs() {
  const tabs = await chrome.tabs.query({});
  const recordedAt = new Date().toISOString();
  const events = tabs.filter((tab) => isRecordableUrl(tab.url || "")).map((tab) => snapshotEvent(tab, recordedAt));
  if (events.length === 0) return { ok: true, queued: 0, stored: 0, pending: 0, deadLettered: 0 };
  await queue.enqueueAll(events);
  const blockedBy = await queue.flush();
  const where = await queue.locate(events.map((event) => event.id));
  return {
    ok: true,
    queued: events.length,
    ...where,
    reason: where.pending > 0 && blockedBy ? describeErrorCode(blockedBy) : null,
  };
}

chrome.webNavigation.onCompleted.addListener(async (details) => {
  if (details.frameId !== 0 || !isRecordableUrl(details.url)) return;
  let title = "";
  try {
    const tab = await chrome.tabs.get(details.tabId);
    title = typeof tab.title === "string" ? tab.title : "";
  } catch {
    // The navigation may finish as a tab is closed; URL capture still remains valid.
  }
  const event = {
    type: "visit",
    id: crypto.randomUUID(),
    url: details.url,
    title,
    visitedAt: new Date().toISOString(),
  };
  void enqueueVisit(event);
});

chrome.runtime.onStartup.addListener(() => void queue.flush());
chrome.runtime.onInstalled.addListener(() => void queue.flush());

// Clicking the toolbar action opens the search side panel. Re-applied on every worker start.
chrome.sidePanel.setPanelBehavior({ openPanelOnActionClick: true }).catch(() => {});
void updateBadge();

chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
  if (message?.type === "flush-queue") {
    queue.flush().then(
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
  return error?.message || "記録に失敗しました。";
}
