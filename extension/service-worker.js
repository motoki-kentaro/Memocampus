import { describeErrorCode } from "./search-client.js";
import { QueueError, createEventQueue } from "./event-queue.js";
import { bytesToBase64 } from "./image-format.js";
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
    if (problems > 0) titles.push(`記録できなかったイベント ${status.deadLetters}件・破棄 ${status.dropped}件`);
    if (imageFailure) titles.push(`画像を保存できませんでした: ${imageFailure}`);
    await chrome.action.setBadgeText({ text: titles.length > 0 ? "!" : "" });
    await chrome.action.setTitle({ title: titles.length > 0 ? `Memocampus — ${titles.join(" / ")}` : "Memocampus" });
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
chrome.runtime.onInstalled.addListener(() => {
  void queue.flush();
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
