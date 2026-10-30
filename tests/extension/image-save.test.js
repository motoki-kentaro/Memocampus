import { test } from "node:test";
import assert from "node:assert/strict";
import { pathToFileURL } from "node:url";
import path from "node:path";
import { SAVE_IMAGE_MENU_ID, describeImageSaveResult, registerImageMenu, saveImageFromClick } from "../../extension/image-save.js";
import { bytesToBase64 } from "../../extension/image-format.js";
import { FakeCanvas, fakeDecoder, fakeImageHost, gif, jpeg, png } from "./image-fixtures.js";
import { extensionDir } from "./fake-dom.js";

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

function deps(host, bytes, overrides = {}) {
  return {
    fetchImpl: async () => ({ ok: true, status: 200, redirected: false, headers: new Headers(), body: new Response(bytes).body }),
    executeScript: async () => [{ result: { ok: true, base64: bytesToBase64(bytes) } }],
    ...fakeDecoder(),
    OffscreenCanvas: FakeCanvas,
    subtle: crypto.subtle,
    randomUUID: () => crypto.randomUUID(),
    connect: host.connect,
    runtime: { lastError: undefined },
    timeoutMs: 500,
    ...overrides,
  };
}

const click = (srcUrl, extra = {}) => ({ menuItemId: SAVE_IMAGE_MENU_ID, srcUrl, pageUrl: "https://page.test/thread/1", frameId: 0, ...extra });
const tab = { id: 5, incognito: false };

test("the menu item appears only on images", () => {
  const created = [];
  registerImageMenu({ create: (item, callback) => (created.push(item), callback()) }, {});
  assert.deepEqual(created, [{ id: "memocampus-save-image", title: "Memocampusへ保存", contexts: ["image"] }]);
});

test("a saved image records its URLs, real type and size under a fresh UUID save id", async () => {
  const host = fakeImageHost();
  const bytes = jpeg(800, 600);
  const result = await saveImageFromClick(click("https://pbs.twimg.com/media/abc?format=jpg&name=small", { frameUrl: "https://embed.test/frame" }), tab, deps(host, bytes));
  assert.equal(result.ok, true, JSON.stringify(result));
  assert.deepEqual(result.image, { mimeType: "image/jpeg", width: 800, height: 600, byteLength: bytes.length });
  const begin = host.received.find((m) => m.type === "image_save_begin");
  assert.match(begin.saveId, UUID);
  assert.equal(begin.imageUrl, "https://pbs.twimg.com/media/abc?format=jpg&name=small");
  assert.equal(begin.pageUrl, "https://page.test/thread/1");
  assert.equal(begin.frameUrl, "https://embed.test/frame");
  assert.equal(begin.mimeType, "image/jpeg", "from the bytes, not from any header");
  const ids = new Set();
  for (let i = 0; i < 3; i += 1) {
    await saveImageFromClick(click("https://a.test/x.jpg"), tab, deps(host, bytes));
    ids.add(host.received.filter((m) => m.type === "image_save_begin").at(-1).saveId);
  }
  assert.equal(ids.size, 3, "every click is its own save action");
});

test("data: images store only their type as the source URL", async () => {
  const host = fakeImageHost();
  const bytes = png(3, 3);
  const result = await saveImageFromClick(click(`data:image/png;base64,${bytesToBase64(bytes)}`, { pageUrl: "file:///C:/page.html" }), tab, deps(host, bytes));
  assert.equal(result.ok, true);
  const begin = host.received.find((m) => m.type === "image_save_begin");
  assert.equal(begin.imageUrl, "data:image/png");
  assert.equal(begin.pageUrl, null, "non-web page URLs are not recorded");
});

test("incognito, bad URLs, oversize, SVG, corrupt and unreachable images fail with a reason and nothing is sent", async () => {
  const cases = [
    [click("https://a.test/x.png"), { id: 1, incognito: true }, {}, "incognito_not_allowed"],
    [click("chrome://favicon/x"), tab, {}, "unsupported_url"],
    [click(undefined), tab, {}, "unsupported_url"],
    [click("https://a.test/x.png"), tab, { fetchImpl: async () => ({ ok: true, status: 200, headers: new Headers({ "content-length": String(32 * 1024 * 1024 + 1) }), body: null }) }, "too_large"],
    [click("https://a.test/x.svg"), tab, { fetchImpl: async () => ({ ok: true, status: 200, headers: new Headers(), body: new Response("<svg/>").body }) }, "svg_not_supported"],
    [click("https://a.test/x.png"), tab, { ...fakeDecoder({ broken: true }) }, "decode_failed"],
    [click("https://a.test/x.png"), tab, { fetchImpl: async () => { throw new TypeError("offline"); }, executeScript: async () => [{ result: { ok: false, error: "network_error" } }] }, "network_error"],
  ];
  for (const [info, tabInfo, overrides, code] of cases) {
    const host = fakeImageHost();
    const result = await saveImageFromClick(info, tabInfo, deps(host, png(2, 2), overrides));
    assert.equal(result.ok, false, code);
    assert.equal(result.code, code);
    assert.equal(host.received.length, 0, `${code}: nothing reaches the host`);
  }
});

test("a save is reported as failed when the host does not commit", async () => {
  const host = fakeImageHost();
  host.failCommit = "vault_switched";
  const result = await saveImageFromClick(click("https://a.test/x.gif"), tab, deps(host, gif(2, 2)));
  assert.deepEqual([result.ok, result.code], [false, "vault_switched"]);
  assert.equal(host.history.length, 0);
});

test("notification texts distinguish new saves, history-only saves and failures", () => {
  const image = { width: 800, height: 600, byteLength: 2 * 1024 * 1024 };
  assert.deepEqual(describeImageSaveResult({ ok: true, image, saved: { deduplicated: false, replayed: false } }), { title: "Memocampusへ保存しました", message: "画像 800×600・2.0 MB" });
  assert.match(describeImageSaveResult({ ok: true, image, saved: { deduplicated: true, replayed: false } }).message, /保存履歴を追加しました/);
  assert.deepEqual(describeImageSaveResult({ ok: false, code: "http_error", details: { status: 403 } }), { title: "Memocampusへ保存できませんでした", message: "画像の取得に失敗しました（HTTP 403）。" });
  for (const code of ["vault_not_configured", "too_large", "incognito_not_allowed", "transfer_interrupted", "decode_failed", "svg_not_supported", "host_outdated"]) {
    const { message } = describeImageSaveResult({ ok: false, code, details: {} });
    assert.notEqual(message, "操作に失敗しました。", `${code} has a specific message`);
  }
});

// --- Service worker wiring with a fake chrome API -------------------------------------------------

function fakeChrome() {
  const listeners = {};
  const on = (name) => ({ addListener: (fn) => ((listeners[name] ??= []).push(fn)) });
  const calls = { menus: [], notifications: [], badge: [], session: {} };
  const chrome = {
    runtime: { onStartup: on("startup"), onInstalled: on("installed"), onMessage: on("message"), sendMessage: async () => {}, sendNativeMessage() {}, connectNative() {}, lastError: undefined },
    webNavigation: { onCompleted: on("navigation") },
    contextMenus: { create: (item, cb) => (calls.menus.push(item), cb?.()), onClicked: on("menuClick") },
    notifications: { create: async (id, options) => calls.notifications.push(options) },
    action: { setBadgeText: async ({ text }) => calls.badge.push(text), setTitle: async () => {} },
    sidePanel: { setPanelBehavior: async () => {} },
    scripting: { executeScript: async () => [] },
    storage: {
      local: { get: async () => ({}), set: async () => {} },
      session: {
        get: async (key) => ({ [key]: calls.session[key] }),
        set: async (items) => Object.assign(calls.session, items),
        remove: async (key) => delete calls.session[key],
      },
    },
  };
  return { chrome, listeners, calls };
}

test("the service worker registers the menu and reports a refused save visibly", async () => {
  const { chrome, listeners, calls } = fakeChrome();
  globalThis.chrome = chrome;
  await import(`${pathToFileURL(path.join(extensionDir, "service-worker.js")).href}?sw=${Date.now()}`);
  listeners.installed.forEach((fn) => fn());
  assert.deepEqual(calls.menus.map((m) => m.contexts), [["image"]]);
  listeners.menuClick.forEach((fn) => fn(click("https://a.test/x.png"), { id: 1, incognito: true }));
  await new Promise((resolve) => setTimeout(resolve, 50));
  assert.equal(calls.notifications.length, 1);
  assert.equal(calls.notifications[0].title, "Memocampusへ保存できませんでした");
  assert.match(calls.notifications[0].iconUrl, /^data:image\/png;base64,/);
  assert.match(calls.session.imageSaveFailure, /シークレット/);
  assert.equal(calls.badge.at(-1), "!", "a failure stays visible on the toolbar even without notifications");
  // Other menu items are ignored.
  listeners.menuClick.forEach((fn) => fn({ menuItemId: "other" }, { id: 1 }));
  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.equal(calls.notifications.length, 1);
});
