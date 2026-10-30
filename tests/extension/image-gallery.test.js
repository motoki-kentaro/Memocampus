// Runs the real side panel with the Image tab against the fake DOM and host.
import { listSources } from "../../extension/image-client.js";
import { test } from "node:test";
import assert from "node:assert/strict";
import { imageSummary, loadSidePanel, observers, pages, settle } from "./fake-dom.js";
import { FakeCanvas, fakeDecoder, jpeg, png, sha256 } from "./image-fixtures.js";

const summaries = (count, prefix = 0) => Array.from({ length: count }, (_, i) => imageSummary(prefix + count - i, 1_000_000 + count - i));
const requests = (host, type) => host.requests.filter((m) => m.type === type);
const tiles = ($) => $("#gallery-grid").children;
const ids = ($) => tiles($).map((li) => Number(li.dataset.blobId));
const button = (li) => li.children[0];
const img = (li) => button(li).children.find((child) => child.tagName === "IMG");

function galleryObservers($) {
  const mine = observers.filter((o) => o.options.root === $("#panel-image"));
  return { load: mine[0], keep: mine[1], more: mine[2] };
}

function trackObjectUrls() {
  const created = [];
  const revoked = [];
  const { createObjectURL, revokeObjectURL } = URL;
  URL.createObjectURL = (blob) => {
    const url = createObjectURL(blob);
    created.push({ url, type: blob.type });
    return url;
  };
  URL.revokeObjectURL = (url) => {
    revoked.push(url);
    revokeObjectURL(url);
  };
  return { created, revoked, restore: () => Object.assign(URL, { createObjectURL, revokeObjectURL }) };
}

async function openImageTab(options) {
  const panel = await loadSidePanel(options);
  await settle(30);
  panel.$("#tab-image").dispatch("click");
  await settle(30);
  return panel;
}

test("the Image tab loads lazily, newest first, metadata only, 60 per page", async () => {
  const { $, host } = await loadSidePanel({ vaults: { A: pages("A", 1) }, active: "A", images: { A: summaries(130) } });
  await settle(30);
  assert.equal(requests(host, "image_list").length, 0, "nothing is fetched before the tab is shown");
  $("#tab-image").dispatch("click");
  await settle(30);
  const list = requests(host, "image_list");
  assert.equal(list.length, 1);
  assert.equal(list[0].limit, 60);
  assert.deepEqual(ids($).slice(0, 3), [130, 129, 128]);
  assert.equal(tiles($).length, 60);
  assert.equal(requests(host, "image_thumbnails").length, 0, "no thumbnail until a tile is on screen");
  assert.match(button(tiles($)[0]).getAttribute("aria-label"), /PNG 40×30、最終保存 .*保存1回/);
  assert.equal($("#gallery-summary").textContent, "60件以上");
});

test("pages continue from the cursor without repeats", async () => {
  const { $, host } = await openImageTab({ vaults: { A: pages("A", 1) }, active: "A", images: { A: summaries(130) } });
  const { more } = galleryObservers($);
  more.report([$("#gallery-more")]);
  await settle(30);
  more.report([$("#gallery-more")]);
  await settle(30);
  assert.equal(tiles($).length, 130);
  assert.equal(new Set(ids($)).size, 130);
  assert.deepEqual(requests(host, "image_list").map((m) => m.cursor?.id ?? null), [null, 71, 11]);
  assert.equal($("#gallery-more").hidden, true);
  assert.equal($("#gallery-summary").textContent, "130件");
});

test("an empty vault shows the empty state", async () => {
  const { $ } = await openImageTab({ vaults: { A: pages("A", 1) }, active: "A", images: { A: [] } });
  assert.equal($("#gallery-empty").hidden, false);
  assert.equal(tiles($).length, 0);
});

test("thumbnails are requested only for tiles near the viewport, in batches, once", async () => {
  const urls = trackObjectUrls();
  try {
    const { $, host } = await openImageTab({ vaults: { A: pages("A", 1) }, active: "A", images: { A: summaries(60) } });
    const { load } = galleryObservers($);
    load.report(tiles($).slice(0, 5));
    await settle(60);
    assert.deepEqual(requests(host, "image_thumbnails").map((m) => m.blobIds), [[60, 59, 58, 57, 56]]);
    assert.match(img(tiles($)[0]).src, /^blob:/);
    assert.ok(urls.created.every((u) => u.type === "image/png"));
    load.report(tiles($).slice(0, 5));
    await settle(60);
    assert.equal(requests(host, "image_thumbnails").length, 1, "already loaded thumbnails are not fetched again");
    load.report(tiles($).slice(5, 30));
    await settle(120);
    const batches = requests(host, "image_thumbnails").slice(1).map((m) => m.blobIds.length);
    assert.deepEqual(batches, [16, 9], "at most 16 per request");
  } finally {
    urls.restore();
  }
});

test("far-away tiles release their object URLs and reload when they return", async () => {
  const urls = trackObjectUrls();
  try {
    const { $, host } = await openImageTab({ vaults: { A: pages("A", 1) }, active: "A", images: { A: summaries(10) } });
    const { load, keep } = galleryObservers($);
    const first = tiles($)[0];
    load.report([first]);
    await settle(60);
    const url = img(first).src;
    keep.report([first], false);
    assert.deepEqual(urls.revoked, [url]);
    assert.equal(img(first).src, "");
    load.report([first]);
    await settle(60);
    assert.equal(requests(host, "image_thumbnails").length, 2);
    assert.match(img(first).src, /^blob:/);
  } finally {
    urls.restore();
  }
});

test("images without a usable thumbnail stay in the grid with a type label", async () => {
  const urls = trackObjectUrls();
  try {
    const images = [imageSummary(3, 3, { hasThumbnail: false, mimeType: "image/avif" }), imageSummary(2, 2), imageSummary(1, 1)];
    const { $, host } = await openImageTab({ vaults: { A: pages("A", 1) }, active: "A", images: { A: images } });
    const svg = Buffer.from('<svg xmlns="http://www.w3.org/2000/svg" onload="alert(1)"/>').toString("base64");
    host.thumbnailOverride = (blobId) =>
      blobId === 2
        ? { blobId, mimeType: "image/png", width: 4, height: 2, data: svg } // claims PNG, is SVG
        : { blobId, mimeType: "image/svg+xml", width: 4, height: 2, data: svg };
    galleryObservers($).load.report(tiles($));
    await settle(60);
    assert.deepEqual(ids($), [3, 2, 1], "all images remain listed");
    assert.equal(requests(host, "image_thumbnails")[0].blobIds.includes(3), false, "known-missing thumbnails are not requested");
    for (const li of tiles($)) {
      assert.equal(img(li), undefined, "nothing is rendered as an image");
      assert.ok(button(li).children[0].textContent.length > 0);
    }
    assert.equal(button(tiles($)[0]).children[0].textContent, "AVIF");
    assert.equal(urls.created.length, 0);
  } finally {
    urls.restore();
  }
});

test("a vault switch clears the images at once and ignores late answers from the old vault", async () => {
  const urls = trackObjectUrls();
  try {
    const { $, host } = await openImageTab({
      vaults: { A: pages("A", 1), B: pages("B", 1) },
      active: "A",
      // Blob ids start at 1 in every vault, so ids alone cannot tell the vaults apart.
      images: { A: summaries(80), B: summaries(3) },
    });
    const { load, more } = galleryObservers($);
    load.report(tiles($).slice(0, 4));
    await settle(60);
    const loaded = urls.created.map((u) => u.url);
    assert.equal(loaded.length, 4);
    // Old-vault work still in flight: the next page and more thumbnails.
    host.imageDelay.A = 120;
    more.report([$("#gallery-more")]);
    load.report(tiles($).slice(4, 8));
    await settle(40);
    $("#tab-settings").dispatch("click");
    $("#vault-path-input").value = "B";
    document.querySelector('[data-manual="vault_open"]').dispatch("click");
    await settle(20);
    assert.ok(loaded.every((url) => urls.revoked.includes(url)), "object URLs of vault A are released");
    assert.equal(tiles($).length, 0, "vault A's images are gone before anything else happens");
    $("#tab-image").dispatch("click");
    await settle(250);
    assert.deepEqual(ids($), [3, 2, 1], "only vault B's images, nothing late from A");
    assert.equal(urls.created.length, 4, "no object URL was created for A's late thumbnails");
    assert.equal(requests(host, "image_list").length, 3, "A's late answers were dropped, not used to trigger reloads");
  } finally {
    urls.restore();
  }
});

test("a vault changed elsewhere between pages restarts the list instead of mixing", async () => {
  const { $, host } = await openImageTab({ vaults: { A: pages("A", 1), B: pages("B", 1) }, active: "A", images: { A: summaries(70, 500), B: summaries(2) } });
  assert.equal(tiles($).length, 60);
  host.active = "B"; // switched from another side panel window
  galleryObservers($).more.report([$("#gallery-more")]);
  await settle(80);
  assert.deepEqual(ids($), [2, 1]);
});

test("a new save refreshes the list only when that does not disturb the user", async () => {
  const images = summaries(3);
  const { $, host } = await openImageTab({ vaults: { A: pages("A", 1) }, active: "A", images: { A: images } });
  const notify = () => host.runtimeListeners.forEach((listener) => listener({ type: "image-save-result", ok: true }));
  images.unshift(imageSummary(4, 2_000_000));
  notify();
  await settle(30);
  assert.deepEqual(ids($), [4, 3, 2, 1], "at the top with nothing open: refreshed");

  images.unshift(imageSummary(5, 3_000_000));
  $("#panel-image").scrollTop = 250;
  notify();
  await settle(30);
  assert.deepEqual(ids($), [4, 3, 2, 1], "scrolled down: left alone");
  assert.equal($("#gallery-new-dot").hidden, false);
  $("#gallery-refresh").dispatch("click");
  await settle(30);
  assert.deepEqual(ids($), [5, 4, 3, 2, 1]);
  assert.equal($("#panel-image").scrollTop, 0);

  // With the save history open the list is not reloaded under the user either.
  $("#gallery-grid").dispatch("click", { target: button(tiles($)[0]) });
  await settle(30);
  images.unshift(imageSummary(6, 4_000_000));
  notify();
  await settle(30);
  assert.deepEqual(ids($).slice(0, 1), [5]);
  assert.equal($("#gallery-new-dot").hidden, false);
  $("#gallery-details-close").dispatch("click");
  $("#gallery-refresh").dispatch("click");
  await settle(30);

  // Failures and saves while another tab is shown do not reload anything now.
  const before = requests(host, "image_list").length;
  host.runtimeListeners.forEach((listener) => listener({ type: "image-save-result", ok: false }));
  $("#tab-url").dispatch("click");
  notify();
  await settle(30);
  assert.equal(requests(host, "image_list").length, before);
  $("#tab-image").dispatch("click");
  await settle(30);
  assert.equal(requests(host, "image_list").length, before + 1, "reloaded when the tab is shown again");
});

test("the save history lists every save with URLs as text and only web URLs openable", async () => {
  const sources = [
    { savedImageId: 3, imageUrl: "https://evil.test/<img src=x onerror=alert(1)>.png", pageUrl: "https://page.test/a", frameUrl: null, savedAt: 3_000, incognito: false },
    { savedImageId: 2, imageUrl: "data:image/png", pageUrl: null, frameUrl: null, savedAt: 2_000, incognito: false },
    { savedImageId: 1, imageUrl: "javascript:alert(1)", pageUrl: "https://page.test/b", frameUrl: "https://frame.test/", savedAt: 1_000, incognito: false },
  ];
  const { $, host } = await openImageTab({ vaults: { A: pages("A", 1) }, active: "A", images: { A: [imageSummary(9, 3_000, { saveCount: 3, sources })] } });
  assert.equal(button(tiles($)[0]).children.at(-1).textContent, "×3");
  $("#gallery-grid").dispatch("click", { target: button(tiles($)[0]) });
  await settle(30);
  assert.equal($("#gallery-details").hidden, false);
  assert.match($("#gallery-details-meta").textContent, /PNG（image\/png）・40×30・.*保存3回/);
  const entries = $("#gallery-sources").children;
  assert.equal(entries.length, 3);
  const lines = (li) => li.children.filter((c) => c.className === "source-line").map((line) => line.children[1]);
  const [first] = lines(entries[0]);
  assert.equal(first.tagName, "A", "web URLs are links");
  assert.equal(first.textContent, sources[0].imageUrl, "shown verbatim as text, never parsed as HTML");
  assert.equal(lines(entries[1])[0].tagName, "SPAN");
  assert.match(lines(entries[1])[0].textContent, /埋め込まれた画像/);
  assert.equal(lines(entries[2])[0].tagName, "SPAN", "javascript: is never a link");
  assert.equal(lines(entries[2]).length, 3, "frame URL is listed too");
  $("#gallery-sources").dispatch("click", { target: lines(entries[0])[1] });
  await settle(0);
  assert.deepEqual(host.openedTabs, [{ url: "https://page.test/a" }]);
  $("#gallery-details-close").dispatch("click");
  assert.equal($("#gallery-details").hidden, true);
});

test("without a vault the Image tab points to the settings tab", async () => {
  const { $ } = await loadSidePanel({ vaults: {}, active: null });
  await settle(30);
  $("#tab-image").dispatch("click");
  await settle(30);
  assert.equal($("#gallery-error").hidden, false);
  assert.match($("#gallery-error-message").textContent, /Vaultが未設定/);
  assert.equal($("#gallery-open-settings").hidden, false);
  $("#gallery-open-settings").dispatch("click");
  assert.equal($("#tab-settings").getAttribute("aria-selected"), "true");
});

const refreshState = ($) => ({
  label: $("#gallery-refresh").getAttribute("aria-label"),
  title: $("#gallery-refresh").title,
  dot: !$("#gallery-new-dot").hidden,
});

test("the refresh button names its state; the dot is never the only signal", async () => {
  const images = summaries(2);
  const { $, host } = await openImageTab({ vaults: { A: pages("A", 1) }, active: "A", images: { A: images } });
  assert.deepEqual(refreshState($), { label: "画像一覧を更新", title: "画像一覧を更新", dot: false });
  $("#panel-image").scrollTop = 100;
  images.unshift(imageSummary(3, 2_000_000));
  host.runtimeListeners.forEach((listener) => listener({ type: "image-save-result", ok: true }));
  await settle(20);
  assert.deepEqual(refreshState($), { label: "新しい画像があります", title: "新しい画像があります", dot: true });
  $("#gallery-refresh").dispatch("click");
  await settle(30);
  assert.deepEqual(refreshState($), { label: "画像一覧を更新", title: "画像一覧を更新", dot: false }, "cleared after a successful refresh");
  assert.deepEqual(ids($), [3, 2, 1]);
});

test("a failed refresh keeps the new-images state and shows the error", async () => {
  const images = summaries(2);
  const { $, host } = await openImageTab({ vaults: { A: pages("A", 1) }, active: "A", images: { A: images } });
  $("#panel-image").scrollTop = 100;
  host.runtimeListeners.forEach((listener) => listener({ type: "image-save-result", ok: true }));
  await settle(20);
  host.active = null; // the host cannot answer from a vault any more
  $("#gallery-refresh").dispatch("click");
  await settle(30);
  assert.equal(refreshState($).dot, true);
  assert.equal(refreshState($).label, "新しい画像があります");
  assert.equal($("#gallery-error").hidden, false);
  assert.match($("#gallery-error-message").textContent, /Vault/);
  assert.equal($("#gallery-refresh").disabled, false, "usable again after the failure");
});

test("repeated clicks while refreshing send one request", async () => {
  const { $, host } = await openImageTab({ vaults: { A: pages("A", 1) }, active: "A", images: { A: summaries(2) } });
  host.imageDelay.A = 80;
  const before = requests(host, "image_list").length;
  for (let i = 0; i < 4; i += 1) $("#gallery-refresh").dispatch("click");
  await settle(10);
  assert.equal($("#gallery-refresh").disabled, true);
  assert.equal($("#gallery-refresh").getAttribute("aria-busy"), "true");
  await settle(120);
  assert.equal(requests(host, "image_list").length, before + 1);
  assert.equal($("#gallery-refresh").disabled, false);
  assert.equal($("#gallery-refresh").getAttribute("aria-busy"), null);
});

test("a vault switch during a refresh drops the old answer and the old new-images state", async () => {
  const { $, host } = await openImageTab({ vaults: { A: pages("A", 1), B: pages("B", 1) }, active: "A", images: { A: summaries(5), B: summaries(2) } });
  $("#panel-image").scrollTop = 100;
  host.runtimeListeners.forEach((listener) => listener({ type: "image-save-result", ok: true }));
  await settle(20);
  host.imageDelay.A = 120;
  $("#gallery-refresh").dispatch("click");
  await settle(20);
  $("#tab-settings").dispatch("click");
  $("#vault-path-input").value = "B";
  document.querySelector('[data-manual="vault_open"]').dispatch("click");
  await settle(20);
  assert.equal(tiles($).length, 0);
  assert.equal(refreshState($).dot, false, "the notice was about the previous vault");
  $("#tab-image").dispatch("click");
  await settle(200);
  assert.deepEqual(ids($), [2, 1], "only vault B, nothing from A's late answer");
  assert.equal($("#gallery-refresh").disabled, false);
});

// --- Slice 4: focus, tile menu, copy, export, thumbnail rebuilding ----------------------------

/** A summary whose original bytes the fake host can serve. */
function withOriginal(blobId, lastSavedAt, bytes, extra = {}) {
  return imageSummary(blobId, lastSavedAt, { sha256: sha256(bytes), byteLength: bytes.length, original: bytes, ...extra });
}

/** Clipboard, decoder and canvas the side panel finds on globalThis. */
function installMedia({ clipboardError = null } = {}) {
  const writes = [];
  const clipboard = {
    async write(items) {
      if (clipboardError) {
        await Promise.allSettled(items.flatMap((item) => Object.values(item.data)));
        throw clipboardError;
      }
      for (const item of items) for (const [type, data] of Object.entries(item.data)) writes.push({ type, blob: await data });
    },
  };
  Object.defineProperty(globalThis, "navigator", { value: { clipboard }, configurable: true, writable: true });
  globalThis.ClipboardItem = class {
    constructor(data) {
      this.data = data;
    }
  };
  const decoder = fakeDecoder();
  globalThis.createImageBitmap = decoder.createImageBitmap;
  globalThis.OffscreenCanvas = FakeCanvas;
  return { writes, decoder };
}

const openMenuOn = ($, li, init = { button: 2, clientX: 30, clientY: 40 }) => $("#gallery-grid").dispatch("contextmenu", { target: button(li), ...init });

test("history opened from the keyboard takes focus and gives it back on close", async () => {
  const { $, document } = await openImageTab({ vaults: { A: pages("A", 1) }, active: "A", images: { A: summaries(3) } });
  const details = $("#gallery-details");
  details.append($("#gallery-details-close")); // as nested in sidepanel.html
  const tile = button(tiles($)[1]);
  tile.focus();
  $("#gallery-grid").dispatch("click", { target: tile, detail: 0 }); // Enter or Space
  assert.equal(details.hidden, false);
  assert.equal(document.activeElement, details, "the history panel is focused");
  details.dispatch("keydown", { key: "Escape" });
  assert.equal(details.hidden, true);
  assert.equal(document.activeElement, tile, "focus is back on the tile");
  // The close button returns focus too.
  $("#gallery-grid").dispatch("click", { target: tile, detail: 0 });
  $("#gallery-details-close").focus();
  $("#gallery-details-close").dispatch("click");
  assert.equal(document.activeElement, tile);
});

test("a mouse click does not move focus, and a late history never takes it", async () => {
  const { $, host, document } = await openImageTab({ vaults: { A: pages("A", 1) }, active: "A", images: { A: summaries(3) } });
  const tile = button(tiles($)[0]);
  tile.focus(); // a mouse click focuses the button itself
  $("#gallery-grid").dispatch("click", { target: tile, detail: 1 });
  assert.equal(document.activeElement, tile);
  // Opened from the keyboard while the history loads slowly; the user moves on meanwhile.
  host.imageDelay.A = 60;
  $("#gallery-grid").dispatch("click", { target: tile, detail: 0 });
  $("#query").focus();
  await settle(100);
  assert.equal(document.activeElement, $("#query"), "the late answer does not pull focus back");
  // Closing while focus is elsewhere leaves it there.
  $("#gallery-details-close").dispatch("click");
  assert.equal(document.activeElement, $("#query"));
});

test("when the tile is gone, focus falls back to the Image tab instead of the document", async () => {
  const { $, document } = await openImageTab({ vaults: { A: pages("A", 1), B: pages("B", 1) }, active: "A", images: { A: summaries(3), B: summaries(1, 50) } });
  const details = $("#gallery-details");
  $("#gallery-grid").dispatch("click", { target: button(tiles($)[0]), detail: 0 });
  assert.equal(document.activeElement, details);
  // A reload replaces the tiles while the history has focus.
  $("#gallery-refresh").dispatch("click");
  assert.equal(details.hidden, true);
  assert.equal(document.activeElement, $("#panel-image"));
  await settle(30);
  // A vault switch while focus is on a tile.
  button(tiles($)[0]).focus();
  $("#vault-path-input").value = "B";
  document.querySelector('[data-manual="vault_open"]').dispatch("click");
  await settle(20);
  assert.equal(document.activeElement, $("#panel-image"));
  // Focus elsewhere is not touched by a reload.
  $("#query").focus();
  $("#gallery-refresh").dispatch("click");
  assert.equal(document.activeElement, $("#query"));
});

test("right-click opens the tile menu inside the panel; keys move, Escape returns focus", async () => {
  const { $, document } = await openImageTab({ vaults: { A: pages("A", 1) }, active: "A", images: { A: summaries(3) } });
  const menu = $("#gallery-menu");
  const [copy, exportItem] = [$("#gallery-menu-copy"), $("#gallery-menu-export")];
  Object.assign(menu, { offsetWidth: 180, offsetHeight: 80 });
  Object.assign(globalThis, { innerWidth: 300, innerHeight: 400 });
  const tile = button(tiles($)[2]);
  const event = openMenuOn($, tiles($)[2], { button: 2, clientX: 250, clientY: 380 });
  assert.equal(event.defaultPrevented, true, "the browser menu is replaced");
  assert.equal(menu.hidden, false);
  assert.deepEqual(menu.style, { left: "116px", top: "300px" }, "shifted left and opened upwards at the edges");
  assert.equal(document.activeElement, copy);
  copy.dispatch("keydown", { key: "ArrowDown" });
  assert.equal(document.activeElement, exportItem);
  exportItem.dispatch("keydown", { key: "ArrowDown" });
  assert.equal(document.activeElement, copy, "wraps around");
  copy.dispatch("keydown", { key: "End" });
  assert.equal(document.activeElement, exportItem);
  exportItem.dispatch("keydown", { key: "Escape" });
  assert.equal(menu.hidden, true);
  assert.equal(document.activeElement, tile);
  // From the keyboard (context-menu key) the menu opens below the tile.
  tile.getBoundingClientRect = () => ({ left: 12, bottom: 60 });
  openMenuOn($, tiles($)[2], { button: 0, clientX: 0, clientY: 0 });
  assert.deepEqual(menu.style, { left: "12px", top: "60px" });
  // A click elsewhere closes it without moving focus.
  $("#query").focus();
  document.dispatch("pointerdown", { target: $("#query") });
  assert.equal(menu.hidden, true);
  assert.equal(document.activeElement, $("#query"));
  // The left click still opens the save history.
  $("#gallery-grid").dispatch("click", { target: tile, detail: 1 });
  assert.equal($("#gallery-details").hidden, false);
});

test("copy from the menu puts PNG pixels on the clipboard and says so only after it worked", async () => {
  const media = installMedia();
  const original = jpeg(40, 30);
  const { $, host } = await openImageTab({ vaults: { A: pages("A", 1) }, active: "A", images: { A: [withOriginal(1, 1_000, original, { mimeType: "image/jpeg" })] } });
  openMenuOn($, tiles($)[0]);
  $("#gallery-menu-copy").dispatch("click");
  assert.equal($("#gallery-status").textContent, "画像をコピーしています…");
  await settle(30);
  assert.equal(media.writes.length, 1);
  assert.equal(media.writes[0].type, "image/png");
  assert.equal(new Uint8Array(await media.writes[0].blob.arrayBuffer())[0], 0x89);
  assert.equal($("#gallery-status").textContent, "画像をクリップボードにコピーしました。");
  const reads = requests(host, "image_read");
  assert.ok(reads.length >= 1 && reads.every((m) => m.vaultToken && m.sha256 === sha256(original)));
  assert.equal(requests(host, "image_thumbnail_put").length, 0, "copying never writes to the vault");
});

test("a refused clipboard write is shown as an error", async () => {
  installMedia({ clipboardError: Object.assign(new Error("Document is not focused."), { name: "NotAllowedError" }) });
  const { $ } = await openImageTab({ vaults: { A: pages("A", 1) }, active: "A", images: { A: [withOriginal(1, 1_000, png(4, 4, 500))] } });
  openMenuOn($, tiles($)[0]);
  $("#gallery-menu-copy").dispatch("click");
  await settle(30);
  assert.match($("#gallery-status").textContent, /許可されませんでした/);
  assert.match($("#gallery-status").className, /is-error/);
});

test("export from the menu reports the file name, a cancel, or an error", async () => {
  const { $, host } = await openImageTab({ vaults: { A: pages("A", 1) }, active: "A", images: { A: [withOriginal(1, 1_000, png(4, 4, 500))] } });
  openMenuOn($, tiles($)[0]);
  $("#gallery-menu-export").dispatch("click");
  await settle(20);
  assert.equal($("#gallery-status").textContent, "「1000.png」として書き出しました。");
  const [message] = requests(host, "image_export");
  assert.deepEqual(Object.keys(message).sort(), ["blobId", "requestId", "sha256", "type", "vaultToken"], "no path comes from the extension");
  host.exportAnswer = { cancelled: true };
  openMenuOn($, tiles($)[0]);
  $("#gallery-menu-export").dispatch("click");
  await settle(20);
  assert.equal($("#gallery-status").textContent, "書き出しをキャンセルしました。");
  host.exportAnswer = { ok: false, error: "export_failed" };
  openMenuOn($, tiles($)[0]);
  $("#gallery-menu-export").dispatch("click");
  await settle(20);
  assert.match($("#gallery-status").textContent, /既存のファイルは変更されていません/);
});

test("while one action runs, the menu items are disabled and do nothing", async () => {
  const { $, host } = await openImageTab({ vaults: { A: pages("A", 1) }, active: "A", images: { A: [withOriginal(1, 1_000, png(4, 4, 500))] } });
  host.imageDelay.A = 80;
  openMenuOn($, tiles($)[0]);
  $("#gallery-menu-export").dispatch("click");
  openMenuOn($, tiles($)[0]);
  assert.equal($("#gallery-menu-export").getAttribute("aria-disabled"), "true");
  $("#gallery-menu-export").dispatch("click");
  await settle(120);
  assert.equal(requests(host, "image_export").length, 1);
});

test("a missing thumbnail is rebuilt once when needed and only the thumbnail is added", async () => {
  installMedia();
  const urls = trackObjectUrls();
  try {
    const images = [withOriginal(2, 2_000, png(40, 30, 900), { hasThumbnail: false }), withOriginal(1, 1_000, png(40, 30, 800))];
    const { $, host } = await openImageTab({ vaults: { A: pages("A", 1) }, active: "A", images: { A: images } });
    assert.equal(requests(host, "image_read").length, 0, "nothing is rebuilt before it is on screen");
    const { load } = galleryObservers($);
    load.report(tiles($));
    await settle(60);
    assert.ok(img(tiles($)[0]), "the rebuilt thumbnail is shown");
    assert.equal(host.storedThumbnails.length, 1);
    assert.equal(host.storedThumbnails[0].blobId, 2);
    assert.ok(requests(host, "image_read").every((m) => m.blobId === 2), "only the image that needed it");
    const [put] = requests(host, "image_thumbnail_put");
    assert.equal(put.sha256, images[0].sha256);
    assert.deepEqual(Object.keys(put).sort(), ["blobId", "requestId", "sha256", "thumbnail", "type", "vaultToken"], "nothing but the thumbnail is sent");
    load.report(tiles($));
    await settle(60);
    assert.equal(requests(host, "image_thumbnail_put").length, 1, "not rebuilt twice");
  } finally {
    urls.restore();
  }
});

test("a thumbnail that cannot be rebuilt keeps the type label and is not retried", async () => {
  installMedia();
  const { $, host } = await openImageTab({ vaults: { A: pages("A", 1) }, active: "A", images: { A: [withOriginal(1, 1_000, png(40, 30, 900), { hasThumbnail: false })] } });
  host.readOverride = (message, reply) => ({ ...reply, data: Buffer.from("tampered").toString("base64") });
  const { load } = galleryObservers($);
  load.report(tiles($));
  await settle(60);
  load.report(tiles($));
  await settle(60);
  assert.equal(img(tiles($)[0]), undefined);
  assert.equal(button(tiles($)[0]).children[0].textContent, "PNG");
  assert.equal(requests(host, "image_read").length, 1);
  assert.equal(requests(host, "image_thumbnail_put").length, 0);
});

test("a rebuild in progress during a vault switch writes nothing to either vault", async () => {
  installMedia();
  const bytes = png(40, 30, 900);
  const { $, host, document } = await openImageTab({
    vaults: { A: pages("A", 1), B: pages("B", 1) },
    active: "A",
    images: { A: [withOriginal(1, 1_000, bytes, { hasThumbnail: false })], B: [withOriginal(1, 1_000, bytes)] }, // B has its thumbnail
  });
  host.imageDelay.A = 60;
  galleryObservers($).load.report(tiles($));
  await settle(10);
  $("#vault-path-input").value = "B";
  document.querySelector('[data-manual="vault_open"]').dispatch("click");
  await settle(150);
  assert.equal(requests(host, "image_thumbnail_put").length, 0);
  assert.equal(host.storedThumbnails.length, 0);
});

test("the history marks each incognito save, not the image; other rows and invalid answers are not marked", async () => {
  const sources = [
    { savedImageId: 3, imageUrl: "https://a.test/3.png", pageUrl: null, frameUrl: null, savedAt: 3_000, incognito: true },
    { savedImageId: 2, imageUrl: "https://a.test/2.png", pageUrl: null, frameUrl: null, savedAt: 2_000, incognito: false },
  ];
  const { $ } = await openImageTab({ vaults: { A: pages("A", 1) }, active: "A", images: { A: [imageSummary(9, 3_000, { saveCount: 2, sources })] } });
  assert.ok(!button(tiles($)[0]).textContent.includes("シークレット"), "the tile describes the image, not one save");
  $("#gallery-grid").dispatch("click", { target: button(tiles($)[0]) });
  await settle(30);
  const entries = $("#gallery-sources").children;
  const marks = (li) => li.children.filter((child) => child.className === "source-origin").map((child) => child.textContent);
  assert.deepEqual(entries.map(marks), [["シークレット"], []]);
  assert.ok(!$("#gallery-details-meta").textContent.includes("シークレット"));
});

test("a history row without a boolean origin is not shown as a normal save", async () => {
  const send = async () => ({
    ok: true,
    type: "image_sources_result",
    requestId: "r",
    vaultToken: "a".repeat(64),
    blobId: 9,
    next: null,
    items: [
      { savedImageId: 1, imageUrl: "https://a.test/1.png", savedAt: 1 },
      { savedImageId: 2, imageUrl: "https://a.test/2.png", savedAt: 2, incognito: "true" },
      { savedImageId: 3, imageUrl: "https://a.test/3.png", savedAt: 3, incognito: true },
    ],
  });
  const page = await listSources(9, null, { send: (message) => send().then((reply) => ({ ...reply, requestId: message.requestId })) });
  assert.deepEqual(page.items.map((item) => [item.savedImageId, item.incognito]), [[3, true]]);
});
