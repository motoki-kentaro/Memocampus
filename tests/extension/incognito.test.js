import { test } from "node:test";
import assert from "node:assert/strict";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { DEAD_LETTER_KEY, QUEUE_KEY, classifyResponse, createEventQueue } from "../../extension/event-queue.js";
import { describeRecordResult } from "../../extension/queue-text.js";
import { extensionDir } from "./fake-dom.js";

const SECRET_URL = "https://secret.test/private-page";
const SECRET_TITLE = "Private title";
const normalTab = { id: 1, windowId: 1, index: 0, url: "https://normal.test/", title: "Normal", pinned: false, incognito: false };
const secretTab = { id: 2, windowId: 9, index: 0, url: SECRET_URL, title: SECRET_TITLE, pinned: false, incognito: true };

/** Starts the real service worker with a fake chrome; the host accepts everything it is sent. */
async function startWorker({ tabs = {}, query = [], seed = {} } = {}) {
  const listeners = {};
  const on = (name) => ({ addListener: (fn) => ((listeners[name] ??= []).push(fn)) });
  const data = new Map(Object.entries(seed));
  const sent = [];
  globalThis.chrome = {
    runtime: {
      onStartup: on("startup"),
      onInstalled: on("installed"),
      onMessage: on("message"),
      sendMessage: async () => {},
      connectNative() {},
      lastError: undefined,
      sendNativeMessage(_host, event, reply) {
        sent.push(event);
        reply({ ok: true, eventId: event.id });
      },
    },
    webNavigation: { onCompleted: on("navigation") },
    contextMenus: { create() {}, onClicked: on("menuClick") },
    notifications: { create: async () => {} },
    action: { setBadgeText: async () => {}, setTitle: async () => {} },
    sidePanel: { setPanelBehavior: async () => {} },
    scripting: { executeScript: async () => [] },
    tabs: {
      get: async (id) => {
        if (!(id in tabs)) throw new Error(`No tab with id: ${id}`);
        return tabs[id];
      },
      query: async () => query,
    },
    storage: {
      local: {
        async get(keys) {
          const out = {};
          for (const key of Array.isArray(keys) ? keys : [keys]) if (data.has(key)) out[key] = structuredClone(data.get(key));
          return out;
        },
        async set(items) {
          for (const [key, value] of Object.entries(items)) data.set(key, structuredClone(value));
        },
      },
      session: { get: async () => ({}), set: async () => {}, remove: async () => {} },
    },
  };
  await import(`${pathToFileURL(path.join(extensionDir, "service-worker.js")).href}?incognito=${Math.random()}`);
  return {
    sent,
    data,
    stored: () => JSON.stringify([...data.entries()]),
    async navigate(tabId, url = SECRET_URL) {
      await Promise.all(listeners.navigation.map((fn) => fn({ frameId: 0, tabId, url })));
      await new Promise((resolve) => setTimeout(resolve, 30));
    },
    message: (message) => new Promise((resolve) => listeners.message.forEach((fn) => fn(message, {}, resolve))),
  };
}

function assertNoTrace(worker) {
  assert.deepEqual(worker.sent, [], "nothing reaches the host");
  assert.deepEqual(worker.data.get(QUEUE_KEY) ?? [], []);
  assert.deepEqual(worker.data.get(DEAD_LETTER_KEY) ?? [], []);
  assert.ok(!worker.stored().includes(SECRET_URL), "no URL in extension storage");
  assert.ok(!worker.stored().includes(SECRET_TITLE), "no title in extension storage");
}

test("a visit in a normal tab is recorded with its title and declared normal", async () => {
  const worker = await startWorker({ tabs: { 1: normalTab } });
  await worker.navigate(1, normalTab.url);
  assert.equal(worker.sent.length, 1);
  assert.equal(worker.sent[0].type, "visit");
  assert.equal(worker.sent[0].url, normalTab.url);
  assert.equal(worker.sent[0].title, "Normal");
  assert.strictEqual(worker.sent[0].incognito, false);
});

test("a visit in an incognito tab (extension allowed in incognito) leaves no trace", async () => {
  const worker = await startWorker({ tabs: { 2: secretTab } });
  await worker.navigate(2);
  assertNoTrace(worker);
});

test("a tab that cannot be read (closed meanwhile) is not recorded", async () => {
  const worker = await startWorker();
  await worker.navigate(404);
  assertNoTrace(worker);
});

test("a tab whose incognito state is missing or invalid is not recorded", async () => {
  for (const incognito of [undefined, null, "false", 0, 1, {}]) {
    const worker = await startWorker({ tabs: { 3: { ...normalTab, id: 3, title: SECRET_TITLE, incognito } } });
    await worker.navigate(3);
    assertNoTrace(worker);
  }
  const worker = await startWorker({ tabs: { 3: null } });
  await worker.navigate(3);
  assertNoTrace(worker);
});

test("recording all tabs keeps only normal tabs when incognito ones are open too", async () => {
  const other = { ...normalTab, id: 4, index: 1, url: "https://other.test/" };
  const unknown = { ...secretTab, id: 5, incognito: undefined };
  const invalid = { ...secretTab, id: 6, incognito: "no" };
  const worker = await startWorker({ query: [normalTab, secretTab, other, unknown, invalid] });
  const result = await worker.message({ type: "record-all-tabs" });
  assert.deepEqual(worker.sent.map((event) => event.url), [normalTab.url, other.url]);
  assert.ok(worker.sent.every((event) => event.incognito === false && event.windowId === 1));
  assert.deepEqual(result, { ok: true, queued: 2, pending: 0, deadLettered: 0, stored: 2, reason: null });
  assert.ok(!worker.stored().includes(SECRET_URL) && !worker.stored().includes(SECRET_TITLE));
});

test("recording only incognito tabs stores nothing and says so without mentioning them", async () => {
  const worker = await startWorker({ query: [secretTab] });
  const result = await worker.message({ type: "record-all-tabs" });
  assert.deepEqual(result, { ok: true, queued: 0, stored: 0, pending: 0, deadLettered: 0 });
  assertNoTrace(worker);
  assert.equal(describeRecordResult(result), "記録できる通常ウィンドウのタブ（http/https）がありません。");
});

test("queued events from an earlier version without a normal declaration are discarded, not sent", async () => {
  const good = { type: "visit", id: "good", url: "https://normal.test/", title: "N", incognito: false, visitedAt: "2026-10-30T00:00:00.000Z" };
  const seed = {
    [QUEUE_KEY]: [
      { type: "visit", id: "old-visit", url: SECRET_URL, title: SECRET_TITLE, visitedAt: "2026-10-30T00:00:00.000Z" },
      { type: "tab_snapshot", id: "old-snapshot", url: SECRET_URL, title: SECRET_TITLE, incognito: true },
      { type: "tab_snapshot", id: "bad-snapshot", url: SECRET_URL, title: SECRET_TITLE, incognito: "no" },
      good,
    ],
  };
  const worker = await startWorker({ seed });
  await worker.message({ type: "flush-queue" });
  assert.deepEqual(worker.sent, [good]);
  assert.deepEqual(worker.data.get(QUEUE_KEY), []);
  assert.deepEqual(worker.data.get(DEAD_LETTER_KEY) ?? [], []);
  assert.ok(!worker.stored().includes(SECRET_URL));
});

test("a refusal by the host discards the event without a dead letter; a failure keeps normal events queued", async () => {
  const event = { type: "visit", id: "e1", url: normalTab.url, title: "T", incognito: false };
  assert.equal(classifyResponse(event, { ok: false, eventId: "e1", error: "incognito_not_allowed" }).kind, "discard");
  assert.equal(classifyResponse(event, { ok: false, eventId: null, error: "incognito_not_allowed" }).kind, "discard");
  assert.equal(classifyResponse(event, { ok: false, eventId: "other", error: "incognito_not_allowed" }).kind, "temporary", "not this event's answer");

  const data = new Map();
  const storage = {
    get: async (keys) => Object.fromEntries((Array.isArray(keys) ? keys : [keys]).filter((key) => data.has(key)).map((key) => [key, data.get(key)])),
    set: async (items) => Object.entries(items).forEach(([key, value]) => data.set(key, value)),
  };
  const refusing = createEventQueue({ storage, send: async (sent) => ({ ok: false, eventId: sent.id, error: "incognito_not_allowed" }) });
  await refusing.enqueue(event);
  assert.equal(await refusing.flush(), null, "delivery does not pause");
  assert.deepEqual(await refusing.status(), { pending: 0, deadLetters: 0, deadLetterCodes: [], dropped: 0, blockedBy: null });

  const busy = createEventQueue({ storage, send: async (sent) => ({ ok: false, eventId: sent.id, error: "vault_busy" }) });
  await busy.enqueue(event);
  assert.equal(await busy.flush(), "vault_busy");
  assert.equal((await busy.status()).pending, 1);
});
