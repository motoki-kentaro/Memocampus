// Issue #44, Slice 2: tag editing and tag search in the Image tab, run against the fake DOM and
// host. The fake host keeps tags per vault and normalizes them like the real one (trim, single
// spaces, lowercase); the extension must show what the host answers and never normalize itself.
import { test } from "node:test";
import assert from "node:assert/strict";
import { imageSummary, loadSidePanel, observers, pages, settle } from "./fake-dom.js";
import { changeImageTag, getImageTags, listImages, listTags, searchImagesByTag } from "../../extension/image-client.js";
import { SearchError } from "../../extension/search-client.js";
import { FakeCanvas, fakeDecoder, jpeg, png, sha256 } from "./image-fixtures.js";

const summaries = (count, prefix = 0) => Array.from({ length: count }, (_, i) => imageSummary(prefix + count - i, 1_000_000 + count - i));
const requests = (host, type) => host.requests.filter((m) => m.type === type);
const tiles = ($) => $("#gallery-grid").children;
const ids = ($) => tiles($).map((li) => Number(li.dataset.blobId));
const tileButton = (li) => li.children[0];
const chips = ($) => $("#gallery-tags").children.map((li) => li.children[0].textContent);
const removeButton = ($, name) => $("#gallery-tags").children.map((li) => li.children[1]).find((b) => b.dataset.tag === name);
const options = ($) => $("#gallery-tag-picker-list").children.map((li) => li.children[0]);
const option = ($, name) => options($).find((b) => b.dataset.tag === name);
const TOKEN_A = "41".repeat(32);

function moreObserver($) {
  return observers.filter((o) => o.options.root === $("#panel-image"))[2];
}

async function openImageTab(options) {
  const panel = await loadSidePanel(options);
  await settle(30);
  panel.$("#tab-image").dispatch("click");
  await settle(30);
  return panel;
}

/** Opens the details of the tile at `index` with a mouse click and waits for its tags. */
async function selectTile($, index, wait = 20) {
  $("#gallery-grid").dispatch("click", { target: tileButton(tiles($)[index]), detail: 1 });
  await settle(wait);
}

function typeTag($, text) {
  $("#gallery-tag-input").value = text;
  $("#gallery-tag-add").dispatch("click");
}

const clickOption = ($, name) => $("#gallery-tag-picker-list").dispatch("click", { target: option($, name) });

// --- Editing ---------------------------------------------------------------------------------

test("an untagged image gets its first tag, shown as the host normalized it, only after the host confirmed", async () => {
  const { $, host } = await openImageTab({ vaults: { A: pages("A", 1) }, active: "A", images: { A: summaries(3) } });
  assert.equal($("#gallery-tag-input").disabled, true, "nothing to edit before an image is chosen");
  await selectTile($, 1);
  assert.deepEqual(requests(host, "image_tags_get").map((m) => m.blobId), [2]);
  assert.deepEqual(chips($), []);
  assert.equal($("#gallery-tags-empty").hidden, false, "an untagged image says so");
  assert.equal($("#gallery-tag-input").disabled, false);
  host.imageDelay.A = 40;
  typeTag($, "  Reaction  Face ");
  assert.deepEqual(chips($), [], "not shown before the host answered");
  assert.equal($("#gallery-tags-message").textContent, "タグを追加しています…");
  await settle(80);
  const [add] = requests(host, "image_tag_add");
  assert.deepEqual(
    { blobId: add.blobId, sha256: add.sha256, vaultToken: add.vaultToken, tag: add.tag },
    { blobId: 2, sha256: (2).toString(16).padStart(64, "0"), vaultToken: TOKEN_A, tag: "  Reaction  Face " },
    "the typed text goes to the host as it is, with the image's id, bytes and vault",
  );
  assert.deepEqual(chips($), ["reaction face"]);
  assert.equal($("#gallery-tags-empty").hidden, true);
  assert.equal($("#gallery-tag-input").value, "");
  assert.equal($("#gallery-tags-message").textContent, "タグ「reaction face」を付けました。");
  // The same tag in another spelling: the host's idempotent answer is shown as such.
  typeTag($, "REACTION FACE");
  await settle(80);
  assert.deepEqual(chips($), ["reaction face"]);
  assert.equal($("#gallery-tags-message").textContent, "タグ「reaction face」は既に付いています。");
});

test("several tags are shown, and one is removed only when the host confirmed it", async () => {
  const { $, host } = await openImageTab({ vaults: { A: pages("A", 1) }, active: "A", images: { A: summaries(2) } });
  host.tags.A = { 2: ["それな", "呆れ", "ツッコミ"] };
  await selectTile($, 0);
  assert.deepEqual(chips($), ["それな", "ツッコミ", "呆れ"], "in the host's order");
  assert.equal(removeButton($, "呆れ").getAttribute("aria-label"), "タグ「呆れ」を外す");
  host.imageDelay.A = 40;
  $("#gallery-tags").dispatch("click", { target: removeButton($, "呆れ") });
  assert.deepEqual(chips($), ["それな", "ツッコミ", "呆れ"], "still shown while the host works");
  assert.equal(removeButton($, "それな").disabled, true, "no second change meanwhile");
  $("#gallery-tags").dispatch("click", { target: removeButton($, "それな") });
  await settle(80);
  assert.equal(requests(host, "image_tag_remove").length, 1, "the click during the first request did nothing");
  assert.deepEqual(chips($), ["それな", "ツッコミ"]);
  assert.equal($("#gallery-tags-message").textContent, "タグ「呆れ」を外しました。");
  assert.equal(removeButton($, "それな").disabled, false);
});

test("repeated clicks and Enter send one request; whitespace alone sends none", async () => {
  const { $, host } = await openImageTab({ vaults: { A: pages("A", 1) }, active: "A", images: { A: summaries(1) } });
  await selectTile($, 0);
  typeTag($, "   ");
  assert.equal(requests(host, "image_tag_add").length, 0);
  assert.equal($("#gallery-tags-message").textContent, "タグを入力してください。");
  host.imageDelay.A = 40;
  typeTag($, "漫画");
  $("#gallery-tag-add").dispatch("click");
  $("#gallery-tag-input").dispatch("keydown", { key: "Enter" });
  assert.equal($("#gallery-tag-add").disabled, true);
  await settle(80);
  assert.equal(requests(host, "image_tag_add").length, 1);
  assert.deepEqual(chips($), ["漫画"]);
  // Enter while composing Japanese text does not submit.
  $("#gallery-tag-input").value = "ねこ";
  $("#gallery-tag-input").dispatch("keydown", { key: "Enter", isComposing: true });
  await settle(60);
  assert.equal(requests(host, "image_tag_add").length, 1);
});

test("a refused change keeps the shown tags and the typed text, and says why", async () => {
  const { $, host } = await openImageTab({ vaults: { A: pages("A", 1) }, active: "A", images: { A: summaries(1) } });
  host.tags.A = { 1: ["cat"] };
  await selectTile($, 0);
  for (const [error, pattern] of [
    ["too_many_tags", /64個まで/],
    ["blob_missing", /Vaultに見つかりません。一覧を更新してください/],
    ["db_error", /データベースの操作に失敗しました/],
  ]) {
    host.tagError = error;
    typeTag($, "dog");
    await settle(30);
    assert.deepEqual(chips($), ["cat"], error);
    assert.equal($("#gallery-tag-input").value, "dog", error);
    assert.match($("#gallery-tags-message").textContent, pattern);
    assert.match($("#gallery-tags-message").className, /is-error/);
    assert.equal($("#gallery-tag-add").disabled, false, "usable again after a failure");
  }
  // The host's own rule refuses the name (an invisible character): its reason is shown.
  typeTag($, "a​b");
  await settle(30);
  assert.match($("#gallery-tags-message").textContent, /タグ名が不正です/);
  assert.deepEqual(chips($), ["cat"]);
  host.tagError = "db_error";
  $("#gallery-tags").dispatch("click", { target: removeButton($, "cat") });
  await settle(30);
  assert.deepEqual(chips($), ["cat"], "a failed removal shows the tag still there");
});

test("existing tags are offered for reuse, page by page, and one the image has is not offered again", async () => {
  const { $, host, document } = await openImageTab({ vaults: { A: pages("A", 1) }, active: "A", images: { A: summaries(3) } });
  const names = Array.from({ length: 250 }, (_, i) => `t${String(i).padStart(3, "0")}`);
  host.tags.A = { 2: names, 3: ["t001"] };
  await selectTile($, 0); // blob 3
  $("#gallery-tag-choose").dispatch("click");
  await settle(20);
  assert.equal($("#gallery-tag-picker").hidden, false);
  assert.equal($("#gallery-tag-picker-title").textContent, "既存のタグから選ぶ");
  assert.equal($("#gallery-tag-picker-form").hidden, true, "picking only; typing stays in the details");
  assert.equal(options($).length, 200, "one page, not every tag at once");
  assert.equal($("#gallery-tag-picker-more").hidden, false);
  $("#gallery-tag-picker-more").dispatch("click");
  await settle(20);
  assert.equal(options($).length, 250);
  assert.equal($("#gallery-tag-picker-more").hidden, true);
  assert.deepEqual(requests(host, "image_tag_list").map((m) => [m.cursor, m.limit]), [[null, 200], ["t199", 200]]);
  assert.equal(option($, "t001").getAttribute("aria-disabled"), "true");
  assert.match(option($, "t001").getAttribute("aria-label"), /付与済み/);
  assert.equal(option($, "t000").getAttribute("aria-label"), "t000、1件");
  clickOption($, "t001");
  await settle(20);
  assert.equal(requests(host, "image_tag_add").length, 0, "an owned tag cannot be picked");
  clickOption($, "t249");
  assert.equal($("#gallery-tag-picker").hidden, true);
  assert.equal(document.activeElement, $("#gallery-tag-choose"), "focus returns to the button");
  await settle(20);
  assert.equal(requests(host, "image_tag_add")[0].tag, "t249");
  assert.deepEqual(chips($), ["t001", "t249"]);
});

test("the picker says when there are no tags and closes with Escape", async () => {
  const { $, document } = await openImageTab({ vaults: { A: pages("A", 1) }, active: "A", images: { A: summaries(1) } });
  $("#gallery-tag-filter").dispatch("click");
  await settle(20);
  assert.equal($("#gallery-tag-picker-message").textContent, "登録済みのタグはまだありません。");
  assert.equal(document.activeElement, $("#gallery-tag-picker-input"));
  $("#gallery-tag-picker").dispatch("keydown", { key: "Escape" });
  assert.equal($("#gallery-tag-picker").hidden, true);
  assert.equal(document.activeElement, $("#gallery-tag-filter"));
});

// --- Searching -------------------------------------------------------------------------------

/** 130 images; every even blob id has "cat", blob 1 has "dog". */
function catVault() {
  const images = summaries(130);
  const tags = { 1: ["dog"] };
  for (let id = 2; id <= 130; id += 2) tags[id] = ["cat"];
  return { images, tags };
}

test("a chosen tag filters the same grid through image_tag_search, pages it, and clearing shows all images again", async () => {
  const { images, tags } = catVault();
  const { $, host } = await openImageTab({ vaults: { A: pages("A", 1) }, active: "A", images: { A: images } });
  host.tags.A = tags;
  assert.equal($("#gallery-tag-filter").disabled, false);
  $("#gallery-tag-filter").dispatch("click");
  await settle(20);
  assert.equal($("#gallery-tag-picker-title").textContent, "タグで絞り込み");
  assert.equal(option($, "cat").getAttribute("aria-label"), "cat、65件");
  clickOption($, "cat");
  await settle(30);
  const [search] = requests(host, "image_tag_search");
  assert.deepEqual([search.tag, search.cursor, search.limit], ["cat", null, 60]);
  assert.equal(tiles($).length, 60);
  assert.ok(ids($).every((id) => id % 2 === 0));
  assert.equal($("#gallery-filter").hidden, false);
  assert.equal($("#gallery-filter-name").textContent, "タグ: cat");
  assert.equal($("#gallery-summary").textContent, "60件以上");
  moreObserver($).report([$("#gallery-more")]);
  await settle(30);
  assert.equal(tiles($).length, 65);
  assert.equal(new Set(ids($)).size, 65);
  assert.equal(requests(host, "image_tag_search")[1].cursor.id, ids($)[59]);
  assert.equal(requests(host, "image_list").length, 1, "the plain list was not asked meanwhile");
  // Thumbnails of the filtered grid load like any other.
  observers.filter((o) => o.options.root === $("#panel-image"))[0].report(tiles($).slice(0, 2));
  await settle(60);
  assert.deepEqual(requests(host, "image_thumbnails").at(-1).blobIds, [130, 128]);
  $("#gallery-filter-clear").dispatch("click");
  await settle(30);
  assert.equal($("#gallery-filter").hidden, true);
  assert.equal(requests(host, "image_list").length, 2);
  assert.deepEqual(ids($).slice(0, 3), [130, 129, 128]);
  assert.equal(tiles($).length, 60);
});

test("a typed tag is searched as the host normalizes it; no match or a refused name end naturally", async () => {
  const { images, tags } = catVault();
  const { $, host } = await openImageTab({ vaults: { A: pages("A", 1) }, active: "A", images: { A: images } });
  host.tags.A = tags;
  $("#gallery-tag-filter").dispatch("click");
  await settle(20);
  $("#gallery-tag-picker-input").value = "  CAT ";
  $("#gallery-tag-picker-input").dispatch("keydown", { key: "Enter" });
  await settle(30);
  assert.equal($("#gallery-filter-name").textContent, "タグ: cat", "the host's name, not the typed one");
  moreObserver($).report([$("#gallery-more")]);
  await settle(30);
  assert.deepEqual(requests(host, "image_tag_search").map((m) => m.tag), ["  CAT ", "cat"]);
  // No image has it: the empty state names the tag.
  $("#gallery-tag-filter").dispatch("click");
  await settle(20);
  $("#gallery-tag-picker-input").value = "none";
  $("#gallery-tag-picker-apply").dispatch("click");
  await settle(30);
  assert.equal(tiles($).length, 0);
  assert.equal($("#gallery-empty").hidden, false);
  assert.equal($("#gallery-empty").textContent, "タグ「none」の付いた画像はありません。");
  // A name the host refuses: back to all images, with the reason.
  $("#gallery-tag-filter").dispatch("click");
  await settle(20);
  $("#gallery-tag-picker-input").value = "a\u0001";
  $("#gallery-tag-picker-apply").dispatch("click");
  await settle(40);
  assert.equal($("#gallery-filter").hidden, true);
  assert.equal(tiles($).length, 60);
  assert.match($("#gallery-status").textContent, /タグ名が不正です/);
  // Clearing back to all images restores the usual empty text too.
  assert.match($("#gallery-empty").textContent, /保存された画像はまだありません/);
});

test("a filter changed while an answer is on its way never shows the old answer", async () => {
  const { images, tags } = catVault();
  const { $, host } = await openImageTab({ vaults: { A: pages("A", 1) }, active: "A", images: { A: images } });
  host.tags.A = tags;
  $("#gallery-tag-filter").dispatch("click");
  await settle(20);
  host.imageDelay.A = 80;
  clickOption($, "cat");
  await settle(10);
  $("#gallery-tag-filter").dispatch("click");
  // The picker answer is slow too; the typed name needs no list.
  $("#gallery-tag-picker-input").value = "dog";
  $("#gallery-tag-picker-apply").dispatch("click");
  await settle(200);
  assert.deepEqual(ids($), [1], "only the newer filter's answer");
  assert.equal($("#gallery-filter-name").textContent, "タグ: dog");
  // And clearing while a filtered page loads keeps the plain list.
  $("#gallery-tag-filter").dispatch("click");
  await settle(100);
  clickOption($, "cat");
  await settle(10);
  $("#gallery-filter-clear").dispatch("click");
  await settle(200);
  assert.equal($("#gallery-filter").hidden, true);
  assert.deepEqual(ids($).slice(0, 2), [130, 129]);
});

test("under a filter, removing the tag takes the image out of the grid and adding it back returns it in place", async () => {
  const images = summaries(4);
  const { $, host } = await openImageTab({ vaults: { A: pages("A", 1) }, active: "A", images: { A: images } });
  host.tags.A = { 4: ["cat"], 3: ["cat"], 1: ["cat"] };
  $("#gallery-tag-filter").dispatch("click");
  await settle(20);
  clickOption($, "cat");
  await settle(30);
  assert.deepEqual(ids($), [4, 3, 1]);
  await selectTile($, 1); // blob 3
  $("#gallery-tags").dispatch("click", { target: removeButton($, "cat") });
  await settle(30);
  assert.deepEqual(ids($), [4, 1], "it no longer has the tag");
  assert.equal($("#gallery-details").hidden, false, "its details stay open so the change can be undone");
  assert.equal($("#gallery-summary").textContent, "2件");
  typeTag($, "Cat");
  await settle(30);
  assert.deepEqual(ids($), [4, 3, 1], "back at its place in the list order");
  // Another tag changes nothing in the grid.
  typeTag($, "dog");
  await settle(30);
  assert.deepEqual(ids($), [4, 3, 1]);
  // Out and back again for another image, after the first one was put back: still in list order.
  await selectTile($, 0); // blob 4
  $("#gallery-tags").dispatch("click", { target: removeButton($, "cat") });
  await settle(30);
  assert.deepEqual(ids($), [3, 1]);
  typeTag($, "cat");
  await settle(30);
  assert.deepEqual(ids($), [4, 3, 1]);
  // The last image out: the empty state.
  for (const index of [0, 0, 0]) {
    await selectTile($, index);
    $("#gallery-tags").dispatch("click", { target: removeButton($, "cat") });
    await settle(30);
  }
  assert.deepEqual(ids($), []);
  assert.equal($("#gallery-empty").hidden, false);
  assert.equal($("#gallery-empty").textContent, "タグ「cat」の付いた画像はありません。");
});

/** Clipboard, decoder and canvas the side panel finds on globalThis. */
function installMedia() {
  const writes = [];
  const clipboard = {
    async write(items) {
      for (const item of items) for (const [type, data] of Object.entries(item.data)) writes.push({ type, blob: await data });
    },
  };
  Object.defineProperty(globalThis, "navigator", { value: { clipboard }, configurable: true, writable: true });
  globalThis.ClipboardItem = class {
    constructor(data) {
      this.data = data;
    }
  };
  globalThis.createImageBitmap = fakeDecoder().createImageBitmap;
  globalThis.OffscreenCanvas = FakeCanvas;
  return { writes };
}

test("an image found by tag is copied and exported with the existing menu", async () => {
  const media = installMedia();
  const original = jpeg(40, 30);
  const other = png(4, 4, 500);
  const images = [
    imageSummary(2, 2_000, { sha256: sha256(other), byteLength: other.length, original: other }),
    imageSummary(1, 1_000, { sha256: sha256(original), byteLength: original.length, original, mimeType: "image/jpeg" }),
  ];
  const { $, host } = await openImageTab({ vaults: { A: pages("A", 1) }, active: "A", images: { A: images } });
  host.tags.A = { 1: ["ツッコミ"] };
  $("#gallery-tag-filter").dispatch("click");
  await settle(20);
  clickOption($, "ツッコミ");
  await settle(30);
  assert.deepEqual(ids($), [1]);
  $("#gallery-grid").dispatch("contextmenu", { target: tileButton(tiles($)[0]), button: 2, clientX: 10, clientY: 10 });
  $("#gallery-menu-copy").dispatch("click");
  await settle(40);
  assert.equal($("#gallery-status").textContent, "画像をクリップボードにコピーしました。");
  assert.equal(media.writes.length, 1);
  assert.equal(media.writes[0].type, "image/png");
  assert.ok(requests(host, "image_read").every((m) => m.blobId === 1 && m.sha256 === sha256(original) && m.vaultToken === TOKEN_A));
  $("#gallery-grid").dispatch("contextmenu", { target: tileButton(tiles($)[0]), button: 2, clientX: 10, clientY: 10 });
  $("#gallery-menu-export").dispatch("click");
  await settle(30);
  assert.equal($("#gallery-status").textContent, "「1000.png」として書き出しました。");
  assert.equal(requests(host, "image_export")[0].blobId, 1);
  // The save history (where the image came from) is still there under a filter.
  await selectTile($, 0);
  assert.equal(requests(host, "image_sources").at(-1).blobId, 1);
});

// --- Vault boundaries ------------------------------------------------------------------------

function switchVault($, document, path) {
  $("#tab-settings").dispatch("click");
  $("#vault-path-input").value = path;
  document.querySelector('[data-manual="vault_open"]').dispatch("click");
}

test("a vault switch drops the filter, the picker, the details and every late tag answer of the old vault", async () => {
  const { images, tags } = catVault();
  const { $, host, document } = await openImageTab({ vaults: { A: pages("A", 1), B: pages("B", 1) }, active: "A", images: { A: images, B: summaries(3) } });
  host.tags.A = tags;
  host.tags.B = { 3: ["b-only"] };
  $("#gallery-tag-filter").dispatch("click");
  await settle(20);
  clickOption($, "cat");
  await settle(30);
  await selectTile($, 0);
  // Old-vault work in flight: a tag write and the picker's list of tags.
  host.imageDelay.A = 100;
  typeTag($, "late");
  $("#gallery-tag-filter").dispatch("click");
  assert.equal($("#gallery-tag-picker").hidden, false);
  await settle(10);
  switchVault($, document, "B");
  await settle(20);
  assert.equal($("#gallery-filter").hidden, true, "the filter belonged to vault A");
  assert.equal($("#gallery-tag-picker").hidden, true);
  assert.equal($("#gallery-details").hidden, true);
  assert.equal(tiles($).length, 0);
  $("#tab-image").dispatch("click");
  await settle(250);
  assert.deepEqual(ids($), [3, 2, 1], "vault B, unfiltered");
  assert.deepEqual(chips($), [], "the late answer of A's write changed nothing");
  assert.equal($("#gallery-tags-message").textContent, "");
  assert.equal(requests(host, "image_list").length, 2, "A once, B once: late answers of A did not restart B's list");
  assert.equal(requests(host, "image_tag_search").length, 1, "no search was sent to vault B");
  await selectTile($, 0);
  assert.deepEqual(chips($), ["b-only"], "nothing late from A shows in B");
  assert.equal($("#gallery-tags-message").textContent, "");
  assert.deepEqual(host.tags.B, { 3: ["b-only"] }, "vault B was not written");
  assert.deepEqual(requests(host, "image_tag_add").map((m) => m.vaultToken), [TOKEN_A], "the late write named vault A");
});

test("a write made from a list of a vault switched elsewhere is refused by the host and nothing shows as done", async () => {
  const { $, host } = await openImageTab({ vaults: { A: pages("A", 1), B: pages("B", 1) }, active: "A", images: { A: summaries(2), B: summaries(2) } });
  host.tags.B = {};
  await selectTile($, 0);
  host.active = "B"; // switched from another side panel window: this panel still shows vault A
  typeTag($, "cat");
  await settle(40);
  const [add] = requests(host, "image_tag_add");
  assert.equal(add.vaultToken, TOKEN_A);
  assert.deepEqual(host.tags.B, {}, "vault B's image with the same id was not tagged");
  assert.match($("#gallery-status").textContent, /Vaultが切り替えられました/);
  assert.equal($("#gallery-details").hidden, true, "the old vault's details are gone");
  assert.deepEqual(chips($), []);
  await settle(30);
  assert.equal(requests(host, "image_list").length, 2, "the list restarted in the active vault");
});

test("without a usable vault no tag control can be used", async () => {
  const { $, host } = await openImageTab({ vaults: {}, active: null, images: {} });
  assert.equal($("#gallery-error").hidden, false);
  for (const id of ["#gallery-tag-filter", "#gallery-tag-input", "#gallery-tag-add", "#gallery-tag-choose"]) {
    assert.equal($(id).disabled, true, id);
  }
  $("#gallery-tag-filter").dispatch("click");
  await settle(20);
  assert.equal($("#gallery-tag-picker").hidden, true);
  assert.equal(host.requests.filter((m) => m.type.startsWith("image_tag")).length, 0);
});

test("a vault that becomes unusable while tags load reports it and keeps the controls locked", async () => {
  const { $, host } = await openImageTab({ vaults: { A: pages("A", 1) }, active: "A", images: { A: summaries(1) } });
  host.active = null;
  await selectTile($, 0);
  assert.match($("#gallery-tags-message").textContent, /Vaultが未設定です/);
  assert.equal($("#gallery-tag-input").disabled, true);
  assert.equal(host.requests.filter((m) => m.type === "image_tag_add").length, 0);
});

test("tags that arrive after the vault changed are dropped without touching the new vault's list", async () => {
  const { $, host, document } = await openImageTab({ vaults: { A: pages("A", 1), B: pages("B", 1) }, active: "A", images: { A: summaries(2), B: summaries(2) } });
  host.tags.A = { 2: ["a-only"] };
  host.imageDelay.A = 100;
  $("#gallery-grid").dispatch("click", { target: tileButton(tiles($)[0]), detail: 1 });
  await settle(10);
  switchVault($, document, "B");
  $("#tab-image").dispatch("click");
  await settle(250);
  assert.deepEqual(ids($), [2, 1]);
  assert.deepEqual(chips($), []);
  assert.equal($("#gallery-details").hidden, true);
  assert.equal(requests(host, "image_list").length, 2, "one list per vault, no restart from A's late tags");
});

test("the last loaded image, taken out of a filter and given the tag again, comes back before the next page", async () => {
  const { images, tags } = catVault(); // 65 cat images: 60 on the first page
  const { $, host } = await openImageTab({ vaults: { A: pages("A", 1) }, active: "A", images: { A: images } });
  host.tags.A = tags;
  $("#gallery-tag-filter").dispatch("click");
  await settle(20);
  clickOption($, "cat");
  await settle(30);
  const last = ids($)[59];
  await selectTile($, 59);
  $("#gallery-tags").dispatch("click", { target: removeButton($, "cat") });
  await settle(30);
  assert.equal(ids($).includes(last), false);
  typeTag($, "cat");
  await settle(30);
  assert.equal(ids($)[59], last, "back in place although the next page is not loaded");
  moreObserver($).report([$("#gallery-more")]);
  await settle(30);
  assert.equal(tiles($).length, 65);
  assert.equal(new Set(ids($)).size, 65, "no image twice");
});

test("tag replies that do not match the request are refused", async () => {
  const token = "ab".repeat(32);
  const item = { blobId: 7, sha256: "0".repeat(64) };
  const answering = (fields) => async (message) => ({ ok: true, type: `${message.type}_result`, requestId: message.requestId, vaultToken: token, ...fields });
  const invalid = (promise) => assert.rejects(promise, (error) => error instanceof SearchError && error.code === "invalid_response");
  const good = { blobId: 7, tag: "cat", added: true, removed: true, tags: ["cat"] };
  assert.deepEqual(await changeImageTag("add", item, " Cat ", token, { send: answering(good) }), { tag: "cat", changed: true, tags: ["cat"] });
  for (const bad of [{ blobId: 8 }, { vaultToken: "cd".repeat(32) }, { added: "yes" }, { tag: "" }, { tags: ["cat", 3] }, { tags: null }]) {
    await invalid(changeImageTag("add", item, "cat", token, { send: answering({ ...good, ...bad }) }));
  }
  await invalid(changeImageTag("remove", item, "cat", token, { send: answering({ ...good, removed: undefined }) }));
  await invalid(getImageTags(7, { send: answering({ blobId: 8, tags: [] }) }));
  await invalid(listTags(null, { send: answering({ items: [], next: 3 }) }));
  const listed = await listTags(null, { send: answering({ items: [{ name: "cat", imageCount: 2 }, { name: "", imageCount: 1 }, { name: "x", imageCount: 0 }], next: "cat" }) });
  assert.deepEqual(listed.items, [{ name: "cat", imageCount: 2 }], "unusable entries are skipped");
  await invalid(searchImagesByTag("cat", null, { send: answering({ tag: "", items: [], next: null }) }));
  await invalid(searchImagesByTag("cat", null, { send: answering({ tag: "cat", items: [], next: null, vaultToken: "x" }) }));
});

// --- With the real host ------------------------------------------------------------------------
// Opt-in (MEMOCAMPUS_TEST_HOST): the side panel's real code drives the real host binary, one process
// per message as Chrome does, in a temporary MEMOCAMPUS_APP_DIR with a fresh vault. Nothing touches
// the user's vault, policy or Chrome profile. Export is not run: it opens the Windows save dialog.

const HOST_BIN = process.env.MEMOCAMPUS_TEST_HOST;

test("end to end with the real host: tag, find by tag, copy, and a vault-switched write refused", { skip: HOST_BIN ? false : "set MEMOCAMPUS_TEST_HOST to run" }, async () => {
  const { spawn } = await import("node:child_process");
  const { mkdtempSync, rmSync } = await import("node:fs");
  const os = await import("node:os");
  const path = await import("node:path");
  const { DatabaseSync } = await import("node:sqlite");
  const appDir = mkdtempSync(path.join(os.tmpdir(), "memocampus-tags-e2e-"));
  const env = { ...process.env, MEMOCAMPUS_APP_DIR: appDir };
  delete env.MEMOCAMPUS_DB;
  const running = new Set();
  let closing = false;
  /** One host process for `messages`; resolves with every framed reply. */
  const exchange = (messages) =>
    new Promise((resolve, reject) => {
      if (closing) return reject(new Error("test finished"));
      const child = spawn(HOST_BIN, [], { env, cwd: appDir, stdio: ["pipe", "pipe", "pipe"] });
      running.add(child.pid);
      const chunks = [];
      child.stdout.on("data", (chunk) => chunks.push(chunk));
      child.on("error", reject);
      child.on("close", () => {
        running.delete(child.pid);
        let out = Buffer.concat(chunks);
        const replies = [];
        while (out.length >= 4) {
          const length = out.readUInt32LE(0);
          replies.push(JSON.parse(out.subarray(4, 4 + length).toString("utf8")));
          out = out.subarray(4 + length);
        }
        resolve(replies);
      });
      child.stdin.end(
        Buffer.concat(
          messages.map((message) => {
            const body = Buffer.from(JSON.stringify(message), "utf8");
            const length = Buffer.alloc(4);
            length.writeUInt32LE(body.length);
            return Buffer.concat([length, body]);
          }),
        ),
      );
    });
  const call = async (message) => (await exchange([message]))[0];
  const upload = (bytes, saveId, n) =>
    exchange([
      { type: "image_save_begin", protocol: 2, saveId, sha256: sha256(bytes), byteLength: bytes.length, mimeType: "image/png", width: 40, height: 30, imageUrl: `https://img.test/${n}.png`, pageUrl: `https://page.test/thread/${n}`, incognito: false },
      { type: "image_save_chunk", saveId, index: 0, data: Buffer.from(bytes).toString("base64") },
      { type: "image_save_commit", saveId },
    ]);
  const vault = path.join(appDir, "memocampus.sqlite3");
  try {
    const { seedLegacyPolicy } = await import("./real-processes.js");
    seedLegacyPolicy(appDir);
    assert.equal((await call({ type: "vault_create", requestId: "c", useDefault: true })).ok, true);
    const older = png(40, 30, 900);
    const newer = png(40, 30, 1200);
    assert.equal((await upload(older, "11111111-1111-4111-8111-111111111111", 1)).at(-1).type, "image_saved");
    assert.equal((await upload(newer, "22222222-2222-4222-8222-222222222222", 2)).at(-1).type, "image_saved");

    const media = installMedia();
    const { $, host } = await loadSidePanel({ vaults: { [vault]: [] }, active: vault, images: {} });
    host.handle = (message) => {
      host.requests.push(message);
      return call(message);
    };
    await settle(300);
    $("#tab-image").dispatch("click");
    await settle(500);
    assert.equal(tiles($).length, 2, "both saved images are listed");
    const olderId = Number(tiles($)[1].dataset.blobId);
    // Tag the older one: typed with spacing and capitals, shown as the host stored it.
    $("#gallery-grid").dispatch("click", { target: tileButton(tiles($)[1]), detail: 1 });
    await settle(500);
    assert.equal($("#gallery-tags-empty").hidden, false);
    typeTag($, "  Reaction  ");
    await settle(500);
    assert.deepEqual(chips($), ["reaction"]);
    typeTag($, "漫画");
    await settle(500);
    assert.deepEqual(chips($), ["reaction", "漫画"]);
    // A name the host refuses keeps everything as it was.
    typeTag($, "a​b");
    await settle(500);
    assert.match($("#gallery-tags-message").textContent, /タグ名が不正です/);
    assert.deepEqual(chips($), ["reaction", "漫画"]);
    // Find it by tag through the picker, then copy it from the filtered grid.
    $("#gallery-tag-filter").dispatch("click");
    await settle(500);
    assert.deepEqual(options($).map((o) => o.getAttribute("aria-label")), ["reaction、1件", "漫画、1件"]);
    clickOption($, "漫画");
    await settle(500);
    assert.deepEqual(ids($), [olderId]);
    assert.equal($("#gallery-filter-name").textContent, "タグ: 漫画");
    $("#gallery-grid").dispatch("contextmenu", { target: tileButton(tiles($)[0]), button: 2, clientX: 10, clientY: 10 });
    $("#gallery-menu-copy").dispatch("click");
    await settle(800);
    assert.equal($("#gallery-status").textContent, "画像をクリップボードにコピーしました。");
    assert.deepEqual(Buffer.from(await media.writes[0].blob.arrayBuffer()), Buffer.from(older), "the stored original, unchanged");
    // Untagging under the filter empties it.
    $("#gallery-grid").dispatch("click", { target: tileButton(tiles($)[0]), detail: 1 });
    await settle(500);
    $("#gallery-tags").dispatch("click", { target: removeButton($, "漫画") });
    await settle(500);
    assert.deepEqual(ids($), []);
    assert.equal($("#gallery-empty").textContent, "タグ「漫画」の付いた画像はありません。");
    $("#gallery-filter-clear").dispatch("click");
    await settle(500);
    assert.equal(tiles($).length, 2);

    // A write naming another vault (as after a switch elsewhere) is refused by the real host.
    const [item] = (await listImages(null, { send: call })).items.filter((i) => i.blobId === olderId);
    await assert.rejects(changeImageTag("add", item, "x", "0".repeat(64), { send: call }), (error) => error.code === "vault_switched");

    const db = new DatabaseSync(vault, { readOnly: true });
    try {
      assert.deepEqual(
        db.prepare("SELECT t.name AS name, l.blob_id AS blob FROM image_tag_links l JOIN image_tags t ON t.id = l.tag_id ORDER BY 1").all().map((r) => [r.name, r.blob]),
        [["reaction", olderId]],
      );
      assert.equal(db.prepare("SELECT count(*) AS n FROM image_tags").get().n, 1, "the unused tag is gone");
      assert.equal(db.prepare("PRAGMA integrity_check").get().integrity_check, "ok");
      assert.deepEqual(db.prepare("PRAGMA foreign_key_check").all(), []);
      assert.equal(db.prepare("SELECT count(*) AS n FROM saved_images").get().n, 2, "tagging added no save");
    } finally {
      db.close();
    }
  } finally {
    closing = true;
    for (let wait = 0; running.size > 0 && wait < 100; wait += 1) await settle(100);
    rmSync(appDir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  }
});
