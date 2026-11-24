// Issue #77: the real service worker in Japanese and English, against a fake chrome. Checks the
// context menus (created once, re-titled on startup), notifications, the toolbar tooltip, the
// language-independent failure records in storage.session, and that no language - and no broken
// catalog - changes what the privacy gate lets through.
import { test } from "node:test";
import assert from "node:assert/strict";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { extensionDir, settle } from "./fake-dom.js";
import { QUEUE_KEY } from "../../extension/event-queue.js";
import { chromeI18n, createI18n, withLocale } from "./i18n-mock.js";

const ALLOWED = "https://normal.test/article";
const REFUSED = "https://refused.test/secret";
const SECRET_TITLE = "Sensitive title";
const JAPANESE = /[　-ヿ一-鿿＀-￯]/u;

/**
 * Chrome's context menus: ids are unique, a second create of an id fails through lastError (set only
 * while the callback runs, as in Chrome). `fail.create` / `fail.update` make every such call fail with
 * that text instead, like a missing permission or a broken API would.
 */
function fakeMenus(runtime) {
  let current = runtime;
  const items = new Map();
  const log = [];
  const fail = {};
  const withError = (message, callback) => {
    current.lastError = message ? { message } : undefined;
    try { callback?.(); } finally { current.lastError = undefined; }
  };
  return {
    items,
    log,
    fail,
    rebind(next) { current = next; },
    create(item, callback) {
      log.push(["create", structuredClone(item)]);
      if (fail.create) return withError(fail.create, callback);
      if (items.has(item.id)) return withError(`Cannot create item with duplicate id ${item.id}`, callback);
      items.set(item.id, structuredClone(item));
      withError(null, callback);
    },
    update(id, change, callback) {
      log.push(["update", id, structuredClone(change)]);
      if (fail.update) return withError(fail.update, callback);
      if (!items.has(id)) return withError(`Cannot find menu item with id ${id}`, callback);
      Object.assign(items.get(id), change);
      withError(null, callback);
    },
    onClicked: { addListener() {} },
  };
}

/** A host that judges URLs by a fixed table and stores what it is sent. */
function fakeHost() {
  return {
    state: "ready",
    received: [],
    respond(message) {
      if (message.type === "policy_evaluate_batch") {
        const base = { type: "policy_evaluate_batch_result", requestId: message.requestId };
        if (this.state !== "ready") return { ...base, ok: false, error: "policy_unavailable" };
        return { ...base, ok: true, etag: "rev-1", revision: 1, items: message.items.map((item) => ({ id: item.id, decision: item.url === REFUSED ? "never_save" : "allow" })) };
      }
      if (message.type === "policy_status") return this.state === "ready"
        ? { ok: true, type: "policy_result", requestId: message.requestId, state: "ready", revision: 1, etag: "rev-1", schemaVersion: 1, engineVersion: 1 }
        : { ok: true, type: "policy_result", requestId: message.requestId, state: "unavailable" };
      if (message.type === "host_capabilities") return { ok: true, type: "host_capabilities_result", requestId: message.requestId, capabilities: [] };
      this.received.push(message);
      return { ok: true, eventId: message.id };
    },
  };
}

async function startWorker({ i18n = chromeI18n, host = fakeHost(), menus: existing = null, session = {} } = {}) {
  const listeners = {};
  const on = (name) => ({ addListener: (fn) => ((listeners[name] ??= []).push(fn)) });
  const data = new Map();
  const writes = [];
  const calls = { notifications: [], badge: [], titles: [] };
  const runtime = {
    onStartup: on("startup"), onInstalled: on("installed"), onMessage: on("message"),
    sendMessage: async () => {}, connectNative() {}, lastError: undefined,
    sendNativeMessage: (_name, message, reply) => reply(structuredClone(host.respond(structuredClone(message)))),
  };
  const contextMenus = existing ?? fakeMenus(runtime);
  if (existing) existing.rebind?.(runtime);
  const tabs = {};
  globalThis.chrome = {
    i18n,
    runtime,
    contextMenus: Object.assign(contextMenus, { onClicked: on("menuClick") }),
    webNavigation: { onBeforeNavigate: on("before"), onErrorOccurred: on("failed"), onCompleted: on("navigation"), onCommitted: on("committed"), onCreatedNavigationTarget: on("createdTarget"), onHistoryStateUpdated: on("historyState") },
    notifications: { create: async (_id, options) => calls.notifications.push(options) },
    action: { setBadgeText: async ({ text }) => calls.badge.push(text), setTitle: async ({ title }) => calls.titles.push(title) },
    sidePanel: { setPanelBehavior: async () => {} },
    scripting: { executeScript: async () => [] },
    tabs: { onRemoved: on("tabRemoved"), onReplaced: on("tabReplaced"), get: async (id) => tabs[id] },
    storage: {
      local: {
        get: async (keys) => Object.fromEntries((Array.isArray(keys) ? keys : [keys]).filter((key) => data.has(key)).map((key) => [key, structuredClone(data.get(key))])),
        set: async (items) => { writes.push(JSON.stringify(items)); for (const [key, value] of Object.entries(items)) data.set(key, structuredClone(value)); },
      },
      session: {
        get: async (key) => ({ [key]: session[key] }),
        set: async (items) => Object.assign(session, structuredClone(items)),
        remove: async (key) => delete session[key],
      },
    },
  };
  // Left in place for the test: the worker reads the global chrome whenever it runs.
  await import(`${pathToFileURL(path.join(extensionDir, "service-worker.js")).href}?i18n=${Math.random()}`);
  await settle(20);
  return {
    host, listeners, calls, session, menus: contextMenus, writes,
    fire: async (name, ...args) => { for (const fn of listeners[name] ?? []) fn(...args); await settle(30); },
    async navigate(url, id = 1) {
      tabs[id] = { id, windowId: 1, index: 0, url, title: SECRET_TITLE, pinned: false, incognito: false };
      await Promise.all(listeners.navigation.map((fn) => fn({ frameId: 0, tabId: id, url, timeStamp: Date.now(), documentId: `doc-${id}` })));
      await settle(30);
    },
    stored: () => JSON.stringify([...data.entries()]) + writes.join(""),
    queued: () => data.get(QUEUE_KEY) ?? [],
  };
}

test("menus are created once and only re-titled when Chrome starts in another language", async () => {
  const first = await startWorker();
  await first.fire("installed", { reason: "install" });
  assert.deepEqual([...first.menus.items.values()].map(({ id, title, contexts }) => [id, title, contexts.length]), [
    ["memocampus-save-image", "Memocampusへ保存", 1],
    ["memocampus-save-page", "Memocampusにページを保存", 7],
  ]);
  const contexts = [...first.menus.items.values()].map((item) => item.contexts);
  // Chrome keeps the items; the browser restarts in English.
  await withLocale("en", async () => {
    const menus = first.menus;
    menus.log.length = 0;
    const second = await startWorker({ menus });
    await second.fire("startup");
    assert.deepEqual([...menus.items.values()].map((item) => item.title), ["Save to Memocampus", "Save page to Memocampus"]);
    assert.deepEqual([...menus.items.values()].map((item) => item.contexts), contexts, "contexts are unchanged");
    // Chrome calls back asynchronously, so the two items' calls interleave; compare as a set.
    assert.deepEqual(menus.log.map(([kind, id]) => [kind, typeof id === "string" ? id : id.id]).sort(), [
      ["create", "memocampus-save-image"], ["update", "memocampus-save-image"],
      ["create", "memocampus-save-page"], ["update", "memocampus-save-page"],
    ].sort());
    assert.ok(menus.log.filter(([kind]) => kind === "update").every(([, , change]) => Object.keys(change).join() === "title"), "only the title is updated");
    // Install and startup in the same run, in either order: still one item per id.
    await second.fire("installed", { reason: "update" });
    await second.fire("startup");
    assert.equal(menus.items.size, 2);
  });
});

/** Runs `body` with console.error captured; returns what was logged. */
async function capturingErrors(body) {
  const original = console.error;
  const logged = [];
  console.error = (...args) => logged.push(args.map(String).join(" "));
  try {
    await body(logged);
  } finally {
    console.error = original;
  }
  return logged;
}

test("repeated onInstalled / onStartup keep one item per id, in the current language, and log nothing", async () => {
  const logged = await capturingErrors(async () => {
    const worker = await startWorker();
    await worker.fire("installed", { reason: "install" });
    await worker.fire("startup");
    await worker.fire("installed", { reason: "update" });
    await worker.fire("startup");
    assert.deepEqual([...worker.menus.items.keys()], ["memocampus-save-image", "memocampus-save-page"]);
    assert.equal(worker.menus.log.filter(([kind]) => kind === "create").length, 8, "every registration starts with create");
    assert.equal(worker.menus.log.filter(([kind]) => kind === "update").length, 6, "the duplicates were re-titled");
    await withLocale("en", async () => {
      const again = await startWorker({ menus: worker.menus });
      await again.fire("startup");
      await again.fire("installed", { reason: "update" });
      assert.deepEqual([...worker.menus.items.values()].map((item) => item.title), ["Save to Memocampus", "Save page to Memocampus"]);
    });
    assert.equal(worker.menus.items.size, 2);
  });
  assert.deepEqual(logged, [], "a duplicate id is the normal case on startup, not an error");
});

test("an unexpected create failure with no item is reported with both of Chrome's messages", async () => {
  const logged = await capturingErrors(async () => {
    const worker = await startWorker();
    worker.menus.fail.create = "Permission 'contextMenus' is not available";
    await worker.fire("installed", { reason: "install" });
    assert.equal(worker.menus.items.size, 0, "nothing is reported as created");
    // The worker keeps working: recording is unaffected.
    await worker.navigate(ALLOWED);
    assert.deepEqual(worker.host.received.map((event) => event.url), [ALLOWED]);
  });
  assert.equal(logged.length, 2, logged.join("\n"));
  for (const [index, id] of ["memocampus-save-image", "memocampus-save-page"].entries()) {
    assert.equal(logged[index], `Memocampus: Context menu item "${id}" was not registered (create: Permission 'contextMenus' is not available; update: Cannot find menu item with id ${id})`);
  }
  assert.ok(logged.every((line) => !line.includes(ALLOWED) && !line.includes(SECRET_TITLE)), "only ids and Chrome's messages");
});

test("an update failure on an existing item is reported; the other item is still registered", async () => {
  const worker = await startWorker();
  await worker.fire("installed", { reason: "install" });
  const logged = await capturingErrors(async () => {
    await withLocale("en", async () => {
      const again = await startWorker({ menus: worker.menus });
      // Only the image item fails to update.
      const update = worker.menus.update;
      worker.menus.update = (id, change, callback) => {
        worker.menus.fail.update = id === "memocampus-save-image" ? "Unexpected API failure" : undefined;
        return update.call(worker.menus, id, change, callback);
      };
      await again.fire("startup");
    });
  });
  assert.deepEqual(logged, ['Memocampus: Context menu item "memocampus-save-image" was not registered (create: Cannot create item with duplicate id memocampus-save-image; update: Unexpected API failure)']);
  assert.deepEqual([...worker.menus.items.values()].map((item) => item.title), ["Memocampusへ保存", "Save page to Memocampus"], "the failed item keeps its old title, the other one is re-titled");
});

test("registerMenuItem reports what happened, reads lastError inside the callback and survives a throw", async () => {
  const { registerMenuItem, MenuRegistrationError } = await import("../../extension/menu-item.js");
  const runtime = {};
  const menus = fakeMenus(runtime);
  const item = { id: "x", title: "X", contexts: ["page"] };
  assert.equal(await registerMenuItem(menus, runtime, item), "created");
  assert.equal(await registerMenuItem(menus, runtime, { ...item, title: "Y" }), "retitled");
  assert.equal(menus.items.get("x").title, "Y");
  assert.deepEqual(menus.items.get("x").contexts, ["page"], "contexts are never changed by a re-title");
  // Chrome validates arguments synchronously and throws; that is a failure like any other.
  const throwing = { create() { throw new TypeError("Invalid value for argument 1"); }, update(_id, _change, callback) { runtime.lastError = { message: "Cannot find menu item with id z" }; callback(); runtime.lastError = undefined; } };
  await assert.rejects(registerMenuItem(throwing, runtime, { ...item, id: "z" }), (error) => error instanceof MenuRegistrationError
    && error.id === "z" && error.createError === "Invalid value for argument 1" && error.updateError === "Cannot find menu item with id z");
});

test("clicks still reach the save handlers after the items were re-titled in another language", async () => {
  const first = await startWorker();
  await first.fire("installed", { reason: "install" });
  await withLocale("en", async () => {
    const worker = await startWorker({ menus: first.menus });
    await worker.fire("startup");
    assert.equal(worker.listeners.menuClick.length, 2, "the image and page listeners, each added once at load (not per registration)");
    for (const fn of worker.listeners.menuClick) fn({ menuItemId: "memocampus-save-image", srcUrl: "https://a.test/x.png", pageUrl: "https://a.test/" }, { id: 1 });
    await settle(50);
    for (const fn of worker.listeners.menuClick) fn({ menuItemId: "memocampus-save-page", pageUrl: "https://a.test/" }, { id: 2 });
    await settle(50);
    for (const fn of worker.listeners.menuClick) fn({ menuItemId: "some-other-item", pageUrl: "https://a.test/" }, { id: 3 });
    await settle(50);
    // Both handlers ran (each refuses a tab it cannot inspect); another item's click does nothing.
    assert.deepEqual(worker.calls.notifications.map((n) => n.title), ["Could not save to Memocampus", "Could not save the page"]);
  });
});

test("a startup without the items (none persisted) creates them, with no update", async () => {
  await withLocale("en", async () => {
    const worker = await startWorker();
    await worker.fire("startup");
    assert.deepEqual([...worker.menus.items.values()].map((item) => item.title), ["Save to Memocampus", "Save page to Memocampus"]);
    assert.deepEqual(worker.menus.log.map(([kind]) => kind), ["create", "create"]);
  });
});

test("notifications, the stored failure and the tooltip are in English; the record holds a code", async () => {
  await withLocale("en", async () => {
    const worker = await startWorker();
    // A click on an image in a tab of unknown kind is refused before anything is fetched.
    for (const fn of worker.listeners.menuClick) fn({ menuItemId: "memocampus-save-image", srcUrl: "https://a.test/x.png", pageUrl: "https://a.test/" }, { id: 1 });
    await settle(50);
    assert.equal(worker.calls.notifications.length, 1);
    assert.equal(worker.calls.notifications[0].title, "Could not save to Memocampus");
    assert.equal(worker.calls.notifications[0].message, "Not saved because the type of the source window could not be determined.");
    assert.deepEqual(worker.session.imageSaveFailure, { code: "incognito_unknown" });
    assert.equal(worker.calls.titles.at(-1), "Memocampus — Could not save an image: Not saved because the type of the source window could not be determined.");
    assert.doesNotMatch(worker.calls.titles.at(-1), JAPANESE);
  });
});

test("stored failures are shown in the language of the worker that shows them; old text and junk are safe", async () => {
  const session = { imageSaveFailure: { code: "http_error", status: 404 }, pageSaveFailure: { code: "vault_locked" } };
  await withLocale("en", async () => {
    const worker = await startWorker({ session: { ...session } });
    assert.equal(worker.calls.titles.at(-1), "Memocampus — Could not save an image: Fetching the image failed (HTTP 404). / Page save: Cannot save because the Vault is locked. Check its state in the settings.");
  });
  const ja = await startWorker({ session: { ...session } });
  assert.equal(ja.calls.titles.at(-1), "Memocampus — 画像を保存できませんでした: 画像の取得に失敗しました（HTTP 404）。 / ページ保存: Vaultがロックされているため保存できません。設定画面で状態を確認してください。");
  // A record written by the previous version in this browser session is display text: shown as it is.
  const legacy = await startWorker({ session: { imageSaveFailure: "旧版の表示文字列", pageSaveFailure: { code: 5 } } });
  assert.equal(legacy.calls.titles.at(-1), "Memocampus — 画像を保存できませんでした: 旧版の表示文字列");
  const junk = await startWorker({ session: { imageSaveFailure: { status: 1 }, pageSaveFailure: null } });
  assert.equal(junk.calls.titles.at(-1), "Memocampus");
  assert.equal(junk.calls.badge.at(-1), "");
});

for (const locale of ["ja", "en"]) {
  test(`${locale}: the privacy gate decides the same; a refusal or an unusable policy keeps nothing`, async () => {
    await withLocale(locale, async () => {
      const worker = await startWorker();
      await worker.navigate(REFUSED);
      assert.equal(worker.host.received.length, 0);
      assert.ok(!worker.stored().includes(REFUSED) && !worker.stored().includes(SECRET_TITLE));
      await worker.navigate(ALLOWED, 2);
      assert.deepEqual(worker.host.received.map((event) => [event.type, event.url]), [["visit", ALLOWED]]);
      worker.host.state = "unavailable";
      await worker.navigate(`${ALLOWED}?later`, 3);
      assert.equal(worker.host.received.length, 1, "nothing is recorded while the policy is unusable");
      assert.deepEqual(worker.queued(), []);
      assert.equal(worker.calls.badge.at(-1), "!");
      const expected = locale === "en" ? "Automatic recording is stopped because the privacy settings cannot be accessed." : "プライバシー設定にアクセスできないため、自動記録を停止しています。";
      assert.equal(worker.calls.titles.at(-1), `Memocampus — ${expected}`);
    });
  });
}

test("with no usable messages at all, the gate still decides exactly the same", async () => {
  const broken = { getMessage: () => "", getUILanguage: () => "fr" };
  const worker = await startWorker({ i18n: broken });
  await worker.navigate(REFUSED);
  await worker.navigate(ALLOWED, 2);
  assert.deepEqual(worker.host.received.map((event) => [event.type, event.url]), [["visit", ALLOWED]], "only the allowed visit, as with a catalog");
  worker.host.state = "unavailable";
  await worker.navigate(`${ALLOWED}?later`, 3);
  assert.equal(worker.host.received.length, 1);
  assert.deepEqual(worker.queued(), []);
  assert.ok(!worker.stored().includes(REFUSED));
  // A text that cannot be built is not shown: no empty or made-up tooltip, and no menu without a title.
  assert.ok(worker.calls.titles.every((title) => title === "Memocampus" || title.startsWith("Memocampus — ")));
  await assert.rejects(Promise.resolve().then(() => { for (const fn of worker.listeners.installed) fn({ reason: "install" }); }), /Missing extension message/);
  assert.equal(worker.menus.items.size, 0);
});

test("the stand-in substitutes like Chrome even for a catalog in another folder", () => {
  const i18n = createI18n({ locale: "en" });
  assert.equal(i18n.getMessage("badgeEventsNotRecorded", ["1", "2"]), "Events not recorded: 1, discarded: 2");
  assert.equal(i18n.getMessage("noSuch"), "");
  assert.deepEqual(i18n.missing, ["noSuch"]);
  assert.throws(() => i18n.getMessage("badgeEventsNotRecorded", ["1"]), /takes 2 substitution/);
});
