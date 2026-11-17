// Issue #7: the red delete buttons of the URL and Image tabs, against the fake DOM and a fake lasting-port
// Host that keeps the Host's review contract (tests/extension/delete-fixtures.js). The real Host is in
// vault-items-ui.test.js (opt-in) and native-host/src/item_delete.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { imageSummary, loadSidePanel, pages, settle } from "./fake-dom.js";
import { extensionDir } from "./fake-dom.js";
import { deleteState, visitRow } from "./delete-fixtures.js";

const CAPS = ["navigation_v1", "vault_items_v1", "visit_calendar_v1", "item_delete_v1"];
const requests = (host, type) => host.deleteRequests.filter((m) => m.type === type);
const rows = ($) => $("#results").children;
const tools = (li) => li.children[2];
const body = (li) => li.children[3];
const events = (li) => body(li).children[1].children;
const text = (el) => [el.textContent, ...el.children.map(text)].join(" ").replace(/\s+/g, " ").trim();
const allText = (el) => [el.textContent, ...el.children.map(allText)].join(" ").replace(/\s+/g, " ").trim();
const classes = (el) => el.className.split(" ");
const hasIcon = (el) => el.children.some((c) => c.className === "danger-icon" && c.getAttribute("aria-hidden") === "true");
const A = "https://a.test/page";
const B = "https://b.test/page";
const dialogText = ($) => allText($("#item-delete-description"));

/** Two URLs with visits (A three, B one), one snapshot of A; the URL list as the search answers it. */
async function openUrl(options = {}) {
  const urlRows = [
    { url: A, title: "Alpha", lastVisitedAt: "2026-10-12T00:00:00.000Z", lastCapturedAt: null, lastActivityAt: "2026-10-12T00:00:00.000Z", source: "visit" },
    { url: B, title: "Beta", lastVisitedAt: "2026-10-11T00:00:00.000Z", lastCapturedAt: null, lastActivityAt: "2026-10-11T00:00:00.000Z", source: "visit" },
  ];
  const panel = await loadSidePanel({ vaults: { A: options.rows ?? urlRows, B: [{ ...urlRows[0], title: "Other vault" }] }, active: "A", capabilities: options.capabilities ?? CAPS });
  const { host } = panel;
  deleteState(host);
  host.visits = { A: [visitRow("a1", A, "Alpha one", "2026-10-10T00:00:00.000Z"), visitRow("a2", A, "Alpha two", "2026-10-11T00:00:00.000Z"), visitRow("a3", A, "Alpha three", "2026-10-12T00:00:00.000Z"), visitRow("b1", B, "Beta one", "2026-10-11T00:00:00.000Z")], B: [visitRow("z1", A, "Other", "2026-10-12T00:00:00.000Z")] };
  host.snapshots = { A: [A], B: [] };
  await settle(30);
  return panel;
}
const expand = async (li) => { tools(li).children[0].dispatch("click"); await settle(20); };
const confirm = async ($, wait = 30) => { $("#item-delete-confirm").dispatch("click"); await settle(wait); };

// --- The shared button design ----------------------------------------------------------------------------

test("the delete button is red, uses assets/delete.svg through the stylesheet, and the SVG is inert", () => {
  const css = readFileSync(new URL("sidepanel.css", `file:///${extensionDir.replaceAll("\\", "/")}/`), "utf8");
  assert.match(css, /mask: url\("assets\/delete\.svg"\)/);
  assert.match(css, /--danger: #b3261e/);
  assert.match(css, /@media \(prefers-color-scheme: dark\) \{ :root \{ --danger: #f2b8b5/, "visible in the dark theme too");
  assert.match(css, /forced-colors: active[\s\S]*button\.danger-button/);
  assert.match(css, /button\.danger-button:focus-visible/);
  assert.match(css, /button\.danger-button:disabled/);
  const svg = readFileSync(new URL("assets/delete.svg", `file:///${extensionDir.replaceAll("\\", "/")}/`), "utf8");
  assert.doesNotMatch(svg, /<script|<foreignObject|<image|<use|\bon\w+=|href=|url\(|@import|<style/i);
  const html = readFileSync(new URL("sidepanel.html", `file:///${extensionDir.replaceAll("\\", "/")}/`), "utf8");
  for (const id of ["url-delete-all", "gallery-delete-all", "page-delete-filter", "item-delete-confirm"]) {
    assert.match(html, new RegExp(`<button id="${id}"[^>]*class="danger-button"`), id);
  }
  assert.doesNotMatch(html, /Clear All|Delete Forever/);
});

// --- URL tab ---------------------------------------------------------------------------------------------

test("a URL group has a labelled red URL delete, a history toggle and no delete in the operation drop-down", async () => {
  const { $ } = await openUrl();
  const li = rows($)[0];
  const [toggle, urlDelete] = tools(li).children;
  assert.equal(toggle.getAttribute("aria-expanded"), "false");
  assert.equal(body(li).hidden, true);
  assert.ok(classes(urlDelete).includes("danger-button") && !classes(urlDelete).includes("icon-only") && hasIcon(urlDelete));
  assert.equal(text(urlDelete), "URL全件削除");
  assert.match(urlDelete.getAttribute("aria-label"), /Alpha.*URL全件/);
  const select = li.children[1].children[1];
  assert.ok(!select.children.some((option) => option.value === "delete" || /削除/.test(option.textContent)), "Vaultから削除 is gone from the drop-down");
  assert.equal(tools(li).hidden, false);
});

test("expanding lists that URL's visits, one per row with an icon-only red delete and an accessible name", async () => {
  const { $, host } = await openUrl();
  const li = rows($)[0];
  await expand(li);
  assert.equal(body(li).hidden, false);
  assert.equal(tools(li).children[0].getAttribute("aria-expanded"), "true");
  const items = events(li);
  assert.deepEqual(items.map((e) => text(e.children[0])).map((t) => /Alpha (one|two|three)/.exec(t)[1]), ["three", "two", "one"], "newest first");
  for (const e of items) {
    const trash = e.children[1];
    assert.ok(classes(trash).includes("danger-button") && classes(trash).includes("icon-only") && hasIcon(trash));
    assert.equal(text(trash), "", "icon only: no visible label");
    assert.match(trash.getAttribute("aria-label"), /閲覧履歴を削除/);
    assert.match(trash.title, /この閲覧履歴を削除/);
  }
  assert.match(text(body(li).children[0]), /閲覧履歴 3件.*スナップショット 1件は個別削除の対象外/);
  assert.deepEqual(requests(host, "visit_history").map((m) => m.url), [A]);
  assert.equal(requests(host, "item_delete").length, 0);
  // Another URL's visits never appear in this group.
  assert.ok(!items.some((e) => /Beta/.test(text(e))));
});

test("a visit is deleted alone: preview of that event, a confirmation that names it, the group reopens, the snapshot stays", async () => {
  const { $, host } = await openUrl();
  await expand(rows($)[0]);
  const trash = events(rows($)[0])[1].children[1]; // a2
  trash.dispatch("click");
  await settle(30);
  assert.deepEqual(requests(host, "item_delete_preview").map((m) => m.target), [{ scope: "visit", id: "a2" }]);
  assert.equal($("#item-delete-dialog").hidden, false);
  assert.match(dialogText($), /閲覧履歴1件/);
  assert.match(dialogText($), /Alpha two/);
  assert.match(dialogText($), /同じURLの他の閲覧履歴とスナップショットは削除しません/);
  assert.match(dialogText($), /元に戻せません/);
  assert.equal(requests(host, "item_delete").length, 0, "a preview deletes nothing");
  await confirm($, 60);
  assert.equal(requests(host, "item_delete").length, 1);
  assert.deepEqual(host.visits.A.map((v) => v.id), ["a1", "a3", "b1"], "only that event");
  assert.equal(host.snapshots.A.length, 1);
  assert.equal($("#item-delete-dialog").hidden, true);
  const reopened = rows($).find((r) => r.dataset.url === A);
  assert.ok(reopened, "the URL stays in the list: other visits and a snapshot remain");
  assert.equal(body(reopened).hidden, false, "the group is open again");
  assert.deepEqual(events(reopened).map((e) => /Alpha (\w+)/.exec(text(e))[1]), ["three", "one"]);
  assert.match(text($("#url-item-status")), /閲覧履歴1件を削除しました/);
});

test("deleting the last visit leaves a URL that still has a snapshot, and removes a URL that has nothing left", async () => {
  const { $, host } = await openUrl();
  host.visits.A = host.visits.A.filter((v) => v.url !== A || v.id === "a1");
  await expand(rows($)[0]);
  events(rows($)[0])[0].children[1].dispatch("click");
  await settle(30);
  await confirm($, 60);
  assert.deepEqual(host.visits.A.filter((v) => v.url === A), []);
  assert.ok(rows($).some((r) => r.dataset.url === A), "the snapshot keeps the URL in the list: not treated as fully deleted");
  assert.equal(host.snapshots.A.length, 1);
  // B has no snapshot: its last visit takes the URL away.
  const b = rows($).findIndex((r) => r.dataset.url === B);
  await expand(rows($)[b]);
  events(rows($)[b])[0].children[1].dispatch("click");
  await settle(30);
  await confirm($, 60);
  assert.ok(!rows($).some((r) => r.dataset.url === B));
});

test("cancelling or Escape deletes nothing and gives the focus back; the confirm button is never focused first", async () => {
  const { $, host, document } = await openUrl();
  await expand(rows($)[0]);
  const trash = events(rows($)[0])[0].children[1];
  trash.dispatch("click");
  await settle(30);
  assert.equal(document.activeElement, $("#item-delete-cancel"), "no destructive control is focused");
  $("#item-delete-dialog").dispatch("keydown", { key: "Tab" });
  assert.equal(document.activeElement, $("#item-delete-confirm"));
  $("#item-delete-dialog").dispatch("keydown", { key: "Escape" });
  assert.equal($("#item-delete-dialog").hidden, true);
  assert.equal(document.activeElement, trash, "focus returns to the button that opened it");
  trash.dispatch("click");
  await settle(30);
  $("#item-delete-cancel").dispatch("click");
  assert.equal(document.activeElement, trash);
  assert.equal(requests(host, "item_delete").length, 0);
  assert.equal(host.visits.A.length, 4);
});

test("the confirm button deletes once, even clicked twice, and everything waits meanwhile", async () => {
  const { $, host } = await openUrl();
  await expand(rows($)[0]);
  events(rows($)[0])[0].children[1].dispatch("click");
  await settle(30);
  host.deleteDelays.item_delete = 40;
  $("#item-delete-confirm").dispatch("click");
  $("#item-delete-confirm").dispatch("click");
  await settle(5);
  assert.equal(requests(host, "item_delete").length, 1);
  assert.equal($("#item-delete-confirm").disabled, true);
  assert.equal($("#item-delete-cancel").disabled, true, "no cancel while the host commits");
  assert.equal($("#item-delete-dialog").hidden, false, "the dialog stays until the host answered");
  $("#item-delete-dialog").dispatch("keydown", { key: "Escape" });
  assert.equal($("#item-delete-dialog").hidden, false);
  await settle(120);
  assert.equal($("#item-delete-dialog").hidden, true);
  assert.equal(host.visits.A.length, 3);
});

test("the URL delete removes the whole URL (visits and snapshot) and the row only after the host did", async () => {
  const { $, host } = await openUrl();
  const li = rows($)[0];
  tools(li).children[1].dispatch("click");
  await settle(30);
  assert.deepEqual(requests(host, "item_delete_preview").map((m) => m.target), [{ scope: "url", url: A }]);
  assert.match(dialogText($), /URL全件/);
  assert.match(dialogText($), /削除する閲覧履歴 3件 削除するスナップショット 1件/);
  assert.equal(rows($).length, 2, "nothing is removed before the host confirmed");
  host.deleteDelays.item_delete = 30;
  $("#item-delete-confirm").dispatch("click");
  await settle(5);
  assert.equal(rows($).length, 2, "still there while the host works");
  await settle(80);
  assert.deepEqual(rows($).map((r) => r.dataset.url), [B]);
  assert.deepEqual(host.visits.A.map((v) => v.url), [B]);
  assert.deepEqual(host.snapshots.A, []);
  assert.match(text($("#url-item-status")), /URL全件を削除しました/);
});

// --- Range deletion --------------------------------------------------------------------------------------

test("the range button says what it deletes: every visit without a filter, the matching visits with one", async () => {
  const { $ } = await openUrl();
  const all = $("#url-delete-all");
  assert.equal(all.hidden, false);
  assert.ok(hasIcon(all));
  assert.equal(text(all), "全閲覧履歴を削除");
  assert.match(all.getAttribute("aria-label"), /スナップショットは対象外/);
  $("#query").value = "alpha";
  $("#query").dispatch("keydown", { key: "Enter" });
  await settle(60);
  assert.equal(text(all), "条件に一致する閲覧履歴を削除");
  $("#query").value = "";
  $("#query").dispatch("keydown", { key: "Enter" });
  await settle(60);
  assert.equal(text(all), "全閲覧履歴を削除");
});

test("a filtered range deletes the matching visits only: its count is visits, not rows, and snapshots are out", async () => {
  const { $, host } = await openUrl();
  $("#query").value = "alpha";
  $("#query").dispatch("keydown", { key: "Enter" });
  await settle(60);
  $("#url-delete-all").dispatch("click");
  await settle(30);
  assert.deepEqual(requests(host, "item_delete_preview").map((m) => m.target), [{ scope: "visits", query: "alpha", visitedFrom: null, visitedTo: null }]);
  const shown = dialogText($);
  assert.match(shown, /条件 検索「alpha」/);
  assert.match(shown, /削除する閲覧履歴 3件/, "the DB's matching visits, not the one row shown");
  assert.match(shown, /スナップショットは対象外/);
  assert.match(shown, /スナップショット 1件は残ります/);
  assert.match(shown, /条件に一致しない同じURLの閲覧履歴も残ります/);
  await confirm($, 60);
  assert.deepEqual(host.visits.A.map((v) => v.id), ["b1"]);
  assert.equal(host.snapshots.A.length, 1, "snapshots are never part of a range");
});

test("with no filter the range is every visit, said so, and the snapshot still stays", async () => {
  const { $, host } = await openUrl();
  $("#url-delete-all").dispatch("click");
  await settle(30);
  assert.match(dialogText($), /条件 なし（すべての閲覧履歴）/);
  assert.match(dialogText($), /削除する閲覧履歴 4件/);
  await confirm($, 60);
  assert.deepEqual(host.visits.A, []);
  assert.equal(host.snapshots.A.length, 1);
  assert.equal(host.deleteCommits, 1);
});

test("a chosen day narrows the range exactly as it narrows the list (the same window, sent as it was searched)", async () => {
  const { $, host } = await openUrl();
  $("#tab-url").dispatch("click");
  const bounds = (m) => [m.visitedFrom, m.visitedTo];
  // The calendar picks a local day; the shown list was searched with its bounds, and the range uses those.
  const search = host.requests.filter((m) => m.type === "search_pages").at(-1);
  assert.ok(search);
  $("#url-delete-all").dispatch("click");
  await settle(30);
  const preview = requests(host, "item_delete_preview")[0].target;
  assert.deepEqual([preview.visitedFrom, preview.visitedTo], bounds(search).map((v) => v ?? null), "no day: no window");
});

// --- Boundaries ------------------------------------------------------------------------------------------

test("a set changed after the preview is refused: nothing is deleted and the list is asked for again", async () => {
  const { $, host } = await openUrl();
  $("#query").value = "alpha";
  $("#query").dispatch("keydown", { key: "Enter" });
  await settle(60);
  $("#url-delete-all").dispatch("click");
  await settle(30);
  // Same count, other rows: a1..a3 stop matching, three others start.
  host.visits.A = host.visits.A.map((v) => (v.url === A ? { ...v, title: "plain" } : v)).concat([visitRow("n1", B, "alpha n1"), visitRow("n2", B, "alpha n2"), visitRow("n3", B, "alpha n3")]);
  const before = host.visits.A.length;
  const searches = host.requests.filter((m) => m.type === "search_pages").length;
  await confirm($, 60);
  assert.equal(host.visits.A.length, before, "nothing deleted");
  assert.equal($("#item-delete-dialog").hidden, false, "the reason is shown in the dialog");
  assert.match($("#item-delete-error").textContent, /確認後に削除対象が変更されました/);
  assert.equal($("#item-delete-confirm").disabled, true, "only a new preview can confirm again");
  $("#item-delete-cancel").dispatch("click");
  await settle(60);
  assert.ok(host.requests.filter((m) => m.type === "search_pages").length > searches, "the list is read again");
});

test("an unusable answer after the COMMIT is proven by the status of the same port; nothing is resent", async () => {
  const { $, host } = await openUrl();
  await expand(rows($)[0]);
  events(rows($)[0])[0].children[1].dispatch("click");
  await settle(30);
  host.garbleDeleteReply = true;
  await confirm($, 100);
  // The COMMIT happened; the status on the live port proves it, so the delete is reported as done.
  assert.equal(host.deleteCommits, 1);
  assert.equal(requests(host, "item_delete").length, 1, "never sent twice");
  assert.equal(requests(host, "item_delete_status").length, 1);
  assert.equal($("#item-delete-dialog").hidden, true);
  assert.equal(host.visits.A.length, 3);
});

test("when the port is lost with the answer the deletion is unknown, never claimed and never resent", async () => {
  const { $, host } = await openUrl();
  await expand(rows($)[0]);
  events(rows($)[0])[0].children[1].dispatch("click");
  await settle(30);
  host.deleteDelayFn = { item_delete: async () => { host.deletePorts.at(-1).disconnect(); } };
  await confirm($, 60);
  assert.equal(requests(host, "item_delete").length, 1);
  assert.equal(host.deleteCommits ?? 0, 0);
  assert.equal($("#item-delete-dialog").hidden, true, "the port is gone: the dialog was withdrawn");
  assert.match(text($("#url-item-status")), /削除状態を確認できません/);
});

test("a Vault switch while a preview is open withdraws the dialog, and the old answer fills nothing", async () => {
  const { $, host, document } = await openUrl();
  $("#url-delete-all").dispatch("click");
  await settle(30);
  assert.equal($("#item-delete-dialog").hidden, false);
  $("#tab-settings").dispatch("click");
  $("#vault-path-input").value = "B";
  document.querySelector('[data-manual="vault_open"]').dispatch("click");
  await settle(60);
  assert.equal($("#item-delete-dialog").hidden, true);
  assert.equal(host.visits.A.length, 4, "nothing of A is deleted");
  // The review of A cannot be confirmed on a new port in B either.
  assert.equal(requests(host, "item_delete").length, 0);
  assert.deepEqual(rows($).map((r) => r.dataset.title), ["Other vault"]);
});

test("a late preview of a list that has been replaced never opens a dialog", async () => {
  const { $, host } = await openUrl();
  host.deleteDelays.item_delete_preview = 60;
  $("#url-delete-all").dispatch("click");
  await settle(5);
  $("#query").value = "beta";
  $("#query").dispatch("keydown", { key: "Enter" });
  await settle(120);
  assert.equal($("#item-delete-dialog").hidden, true, "the new list withdrew the pending preview");
  assert.equal(requests(host, "item_delete").length, 0);
  assert.deepEqual(rows($).map((r) => r.dataset.url), [B]);
});

test("a host without item_delete_v1 shows no delete button and sends nothing on the lasting port", async () => {
  const { $, host } = await openUrl({ capabilities: ["navigation_v1", "vault_items_v1"] });
  assert.equal($("#url-delete-all").hidden, true);
  assert.ok(rows($).every((li) => tools(li).hidden));
  assert.equal(host.deleteRequests.length, 0);
});

test("a preview that finds nothing says so and deletes nothing", async () => {
  const { $, host } = await openUrl();
  host.visits.A = [];
  $("#url-delete-all").dispatch("click");
  await settle(30);
  assert.equal($("#item-delete-dialog").hidden, true);
  assert.match(text($("#url-item-status")), /既に存在しません/);
  assert.equal(requests(host, "item_delete").length, 0);
});

test("titles, URLs and ids from the web are shown as text only", async () => {
  const { $, host } = await openUrl();
  host.visits.A = [visitRow("x1", A, "<img src=x onerror=alert(1)>", "2026-10-12T00:00:00.000Z")];
  await expand(rows($)[0]);
  const tags = (el) => [el.tagName, ...el.children.flatMap(tags)];
  assert.ok(!tags(rows($)[0]).includes("IMG"));
  assert.match(text(events(rows($)[0])[0]), /<img src=x onerror=alert\(1\)>/);
  const source = readFileSync(new URL("url-history.js", `file:///${extensionDir.replaceAll("\\", "/")}/`), "utf8") + readFileSync(new URL("delete-ui.js", `file:///${extensionDir.replaceAll("\\", "/")}/`), "utf8");
  assert.doesNotMatch(source, /innerHTML|insertAdjacentHTML|outerHTML|eval\(/);
});

// --- Image tab -------------------------------------------------------------------------------------------

const source = (savedImageId) => ({ savedImageId, imageUrl: `https://img.test/${savedImageId}.png`, pageUrl: "https://page.test/a", frameUrl: null, savedAt: 1_000_000 + savedImageId, incognito: false, mcpPublic: true });
async function openImage(options = {}) {
  const images = [imageSummary(3, 1_000_003, { sources: [source(31), source(30)], saveCount: 2 }), imageSummary(2, 1_000_002, { sources: [source(20)] }), imageSummary(1, 1_000_001, { sources: [source(10)] })];
  const panel = await loadSidePanel({ vaults: { A: pages("A", 1), B: pages("B", 1) }, active: "A", images: { A: images, B: [imageSummary(9, 1)] }, capabilities: options.capabilities ?? CAPS });
  panel.host.tags = { A: { 3: ["shared", "only-three"], 2: ["shared"], 1: [] }, B: {} };
  await settle(30);
  panel.$("#tab-image").dispatch("click");
  await settle(40);
  return panel;
}
const tiles = ($) => $("#gallery-grid").children;
const ids = ($) => tiles($).map((li) => Number(li.dataset.blobId));

test("every image tile has its own icon-only red delete beside the tile, never inside it", async () => {
  const { $ } = await openImage();
  for (const li of tiles($)) {
    assert.equal(li.children.length, 2);
    const [tile, trash] = li.children;
    assert.ok(classes(tile).includes("tile") && !tile.children.some((c) => c.tagName === "BUTTON"), "a button is never nested in a button");
    assert.ok(classes(trash).includes("danger-button") && classes(trash).includes("icon-only") && hasIcon(trash) && classes(trash).includes("tile-trash"));
    assert.equal(text(trash), "");
    assert.match(trash.getAttribute("aria-label"), /この画像を削除/);
    assert.match(trash.title, /この画像を削除/);
    assert.equal(trash.hidden, false);
  }
});

test("the trash does not open the image, and deleting an image removes its tile, details and only its own tags", async () => {
  const { $, host, document } = await openImage();
  $("#gallery-grid").dispatch("click", { target: tiles($)[0].children[0], detail: 1 });
  await settle(30);
  assert.equal($("#gallery-details").hidden, false);
  // The details of image 3 are open; deleting image 2 through its own trash does not change them.
  const trash2 = tiles($)[1].children[1];
  trash2.dispatch("click");
  await settle(30);
  assert.deepEqual(requests(host, "item_delete_preview").map((m) => m.target), [{ scope: "image", blobId: 2, sha256: (2).toString(16).padStart(64, "0") }]);
  assert.match(allText($("#item-delete-description")), /削除する画像 1件/);
  assert.match(allText($("#item-delete-description")), /閲覧履歴・URL・他の画像は削除しません/);
  assert.equal(document.activeElement, $("#item-delete-cancel"));
  await confirm($, 60);
  assert.deepEqual(ids($), [3, 1]);
  assert.deepEqual(host.deletedImages, [2]);
  assert.equal($("#gallery-details").hidden, false, "the open details were another image's");
  assert.deepEqual(host.tags.A[3], ["shared", "only-three"], "other images keep their tags");
  // Now the open image itself.
  tiles($)[0].children[1].dispatch("click");
  await settle(30);
  assert.match(allText($("#item-delete-description")), /保存履歴 2件/);
  assert.match(allText($("#item-delete-description")), /他に使われず消えるタグ 2件/);
  await confirm($, 60);
  assert.deepEqual(ids($), [1]);
  assert.equal($("#gallery-details").hidden, true, "the details of a deleted image close");
});

test("the range label follows the tag filter and deletes the images the grid shows", async () => {
  const { $, host } = await openImage();
  const all = $("#gallery-delete-all");
  assert.equal(all.hidden, false);
  assert.equal(text(all), "全画像を削除");
  assert.match(all.getAttribute("aria-label"), /保存した画像すべて/);
  $("#gallery-tag-filter").dispatch("click");
  await settle(20);
  $("#gallery-tag-picker-input").value = "Shared";
  $("#gallery-tag-picker-apply").dispatch("click");
  await settle(60);
  assert.deepEqual(ids($), [3, 2]);
  assert.equal(text(all), "検索結果をすべて削除");
  all.dispatch("click");
  await settle(30);
  assert.deepEqual(requests(host, "item_delete_preview").at(-1).target, { scope: "images", tag: "shared" });
  assert.match(allText($("#item-delete-description")), /条件 タグ「shared」の付いた画像/);
  assert.match(allText($("#item-delete-description")), /削除する画像 2件/);
  await confirm($, 80);
  assert.deepEqual(host.images.A.map((i) => i.blobId), [1]);
});

test("with no filter the range is every image, said in the dialog", async () => {
  const { $, host } = await openImage();
  $("#gallery-delete-all").dispatch("click");
  await settle(30);
  assert.deepEqual(requests(host, "item_delete_preview").at(-1).target, { scope: "images", tag: null });
  assert.match(allText($("#item-delete-description")), /条件 なし（すべての画像）/);
  assert.match(allText($("#item-delete-description")), /削除する画像 3件/);
  $("#item-delete-cancel").dispatch("click");
  assert.equal(requests(host, "item_delete").length, 0);
});

test("an image set changed after the preview is refused and the grid is read again", async () => {
  const { $, host } = await openImage();
  $("#gallery-delete-all").dispatch("click");
  await settle(30);
  host.images.A = [...host.images.A, imageSummary(7, 1_000_007, { sources: [source(70)] })];
  const lists = host.requests.filter((m) => m.type === "image_list").length;
  await confirm($, 60);
  assert.equal(host.images.A.length, 4, "nothing deleted");
  assert.match($("#item-delete-error").textContent, /確認後に削除対象が変更されました/);
  $("#item-delete-cancel").dispatch("click");
  await settle(60);
  assert.ok(host.requests.filter((m) => m.type === "image_list").length > lists);
});

test("an image deletion previewed in another Vault than the grid's deletes nothing", async () => {
  const { $, host, document } = await openImage();
  host.active = "B";
  tiles($)[0].children[1].dispatch("click");
  await settle(40);
  assert.equal($("#item-delete-dialog").hidden, true);
  assert.equal(requests(host, "item_delete").length, 0);
  assert.equal(host.images.A.length, 3);
  assert.ok(document);
});

test("without item_delete_v1 the image tab shows no delete button", async () => {
  const { $ } = await openImage({ capabilities: ["navigation_v1", "vault_items_v1"] });
  assert.equal($("#gallery-delete-all").hidden, true);
  assert.ok(tiles($).every((li) => li.children[1].hidden));
});

test("the Image manage drop-down no longer offers deleting", async () => {
  const { $ } = await openImage();
  tiles($)[0].children[0].dispatch("click", { detail: 1 });
  $("#gallery-grid").dispatch("click", { target: tiles($)[0].children[0], detail: 1 });
  await settle(30);
  assert.deepEqual($("#gallery-manage").children.map((o) => o.value), ["", "never_save", "save_but_no_mcp", "publish"]);
});

// --- Navigation records of a visit deletion (CENSORED slice) ----------------------------------------------------

test("the confirmation says how many visits go, how many navigation records stay censored and how many go with them", async () => {
  const { $, host, document } = await openUrl();
  host.navigationCounts = { navigationSourcesCleared: 5, navigations: 2 };
  $("#url-delete-all").dispatch("click");
  await settle(30);
  const shown = dialogText($);
  assert.match(shown, /削除する閲覧履歴 4件/);
  assert.match(shown, /遷移元が検閲済み（\[CENSORED\]）になる遷移記録 5件/);
  assert.match(shown, /一緒に削除される遷移記録 2件/);
  assert.match(shown, /削除した閲覧は復元しません/);
  assert.equal(document.activeElement, $("#item-delete-cancel"), "the focus starts on cancel");
  $("#item-delete-dialog").dispatch("keydown", { key: "Escape" });
  assert.equal(requests(host, "item_delete").length, 0);
});

test("a count of zero is left out, and an individual visit says the same three things", async () => {
  const { $, host } = await openUrl();
  host.navigationCounts = { navigationSourcesCleared: 0, navigations: 1 };
  await expand(rows($)[0]);
  events(rows($)[0])[0].children[1].dispatch("click");
  await settle(30);
  const shown = dialogText($);
  assert.match(shown, /削除する閲覧履歴 1件/);
  assert.doesNotMatch(shown, /検閲済み/, "no censored record: no mention");
  assert.match(shown, /一緒に削除される遷移記録 1件/);
  $("#item-delete-cancel").dispatch("click");
  host.navigationCounts = { navigationSourcesCleared: 0, navigations: 0 };
  events(rows($)[0])[0].children[1].dispatch("click");
  await settle(30);
  assert.doesNotMatch(dialogText($), /遷移記録/);
});

test("an affected navigation set that changed after the preview is refused and nothing is deleted", async () => {
  const { $, host } = await openUrl();
  host.navigationCounts = { navigationSourcesCleared: 1, navigations: 0 };
  $("#url-delete-all").dispatch("click");
  await settle(30);
  host.navigationCounts = { navigationSourcesCleared: 2, navigations: 0 };
  await confirm($, 60);
  assert.equal(host.visits.A.length, 4);
  assert.match($("#item-delete-error").textContent, /確認後に削除対象が変更されました/);
});

test("no pseudo URL called [CENSORED] ever reaches the URL list", async () => {
  const { $ } = await openUrl();
  assert.ok(!rows($).some((r) => /CENSORED/.test(r.dataset.url) || /CENSORED/.test(r.dataset.title)));
  const source = readFileSync(new URL("sidepanel.js", `file:///${extensionDir.replaceAll("\\", "/")}/`), "utf8");
  assert.doesNotMatch(source, /CENSORED/, "the list code never produces the label");
});

// --- FINDING-DELETE-01: the dialog's life is not the delete's life -------------------------------------------

const UNKNOWN = /削除状態を確認できません/;
const CERTAIN = /何も削除していません|Native Hostに接続できません/;
const galleryLine = ($) => $("#gallery-status");

async function commitThenLose($, host) {
  host.commitThenDisconnect = true;
  await confirm($, 80);
}

test("URL: COMMIT, answer discarded, port gone: the dialog is withdrawn but the unknown notice stays, DELETE sent once", async () => {
  const { $, host } = await openUrl();
  await expand(rows($)[0]);
  events(rows($)[0])[0].children[1].dispatch("click");
  await settle(30);
  await commitThenLose($, host);
  assert.equal(host.deleteCommits, 1);
  assert.equal(requests(host, "item_delete").length, 1);
  assert.equal($("#item-delete-dialog").hidden, true, "the dialog was withdrawn by the lost port");
  const status = $("#url-item-status");
  assert.equal(status.hidden, false);
  assert.match(text(status), UNKNOWN);
  assert.match(text(status), /削除が完了している可能性があります/);
  assert.doesNotMatch(text(status), CERTAIN);
  assert.equal(requests(host, "item_delete").length, 1, "nothing is resent");
});

test("URL: an unknown notice is not replaced by a later ordinary message or a new preview", async () => {
  const { $, host } = await openUrl();
  await expand(rows($)[0]);
  events(rows($)[0])[0].children[1].dispatch("click");
  await settle(30);
  await commitThenLose($, host);
  assert.match(text($("#url-item-status")), UNKNOWN);
  host.deleteFailure = "item_not_found";
  $("#url-delete-all").dispatch("click");
  await settle(40);
  assert.match(text($("#url-item-status")), UNKNOWN);
  assert.equal(requests(host, "item_delete").length, 1);
});

test("URL: leaving the tab while the DELETE is on its way does not end it; the unknown still reaches the URL tab", async () => {
  const { $, host } = await openUrl();
  await expand(rows($)[0]);
  events(rows($)[0])[0].children[1].dispatch("click");
  await settle(30);
  host.deleteDelayFn = { item_delete: async () => { await new Promise((r) => setTimeout(r, 40)); host.deletePorts.at(-1).disconnect(); } };
  $("#item-delete-confirm").dispatch("click");
  await settle(5);
  $("#tab-settings").dispatch("click");
  await settle(120);
  $("#tab-url").dispatch("click");
  assert.match(text($("#url-item-status")), UNKNOWN);
  assert.equal(host.deleteCommits ?? 0, 0, "this Host never committed: still not claimed as 'nothing deleted'");
});

test("Image: COMMIT, answer discarded, port gone: unknown notice on the Image tab, one DELETE", async () => {
  const { $, host } = await openImage();
  tiles($)[1].children[1].dispatch("click");
  await settle(30);
  await commitThenLose($, host);
  assert.equal(host.deleteCommits, 1);
  assert.equal(requests(host, "item_delete").length, 1);
  assert.equal($("#item-delete-dialog").hidden, true);
  const shown = text(galleryLine($));
  assert.match(shown, UNKNOWN);
  assert.doesNotMatch(shown, CERTAIN);
  assert.equal(galleryLine($).className.includes("is-error"), true);
});

test("Image: a Host refusal is a certain 'not deleted', not an unknown", async () => {
  const { $, host } = await openImage();
  tiles($)[1].children[1].dispatch("click");
  await settle(30);
  host.deleteFailure = "item_targets_changed";
  await confirm($, 60);
  assert.equal(host.deleteCommits ?? 0, 0);
  assert.doesNotMatch(text(galleryLine($)), UNKNOWN);
});

test("a cancelled confirmation leaves no notice and sends no DELETE", async () => {
  const { $, host } = await openUrl();
  await expand(rows($)[0]);
  events(rows($)[0])[0].children[1].dispatch("click");
  await settle(30);
  $("#item-delete-cancel").dispatch("click");
  await settle(20);
  assert.doesNotMatch(text($("#url-item-status")), UNKNOWN);
  assert.equal(requests(host, "item_delete").length, 0);
});

// --- FINDING-DELETE-04 in the UI: without the Host's receipt nothing is shown as deleted ---------------------

test("Image: a receipt of another operation and no proof leaves the tile, says unknown and sends one DELETE", async () => {
  const { $, host } = await openImage();
  tiles($)[1].children[1].dispatch("click");
  await settle(30);
  host.deleteReplyShape = (r) => ({ ...r, operationId: "00000000-0000-4000-8000-000000000000" });
  host.statusReplyShape = (s) => ({ ...s, confirmed: false });
  const before = ids($);
  await confirm($, 100);
  assert.deepEqual(ids($).sort(), before.sort(), "no tile is removed without proof");
  assert.equal(requests(host, "item_delete").length, 1);
  assert.match(text($("#item-delete-error")) + text($("#gallery-status")), /削除状態を確認できません/);
  assert.doesNotMatch(text($("#gallery-status")), /画像を削除しました/);
});

test("URL: a receipt without operationId and deleted is no success message", async () => {
  const { $, host } = await openUrl();
  await expand(rows($)[0]);
  events(rows($)[0])[0].children[1].dispatch("click");
  await settle(30);
  host.deleteReplyShape = ({ operationId, deleted, ...rest }) => rest;
  host.statusReplyShape = (s) => ({ ...s, confirmed: false });
  await confirm($, 100);
  assert.doesNotMatch(text($("#url-item-status")), /削除しました/);
  assert.match(text($("#item-delete-error")) + text($("#url-item-status")), /削除状態を確認できません/);
  assert.equal(requests(host, "item_delete").length, 1);
});

test("Image: a faulty receipt proven by the status removes the tile once", async () => {
  const { $, host } = await openImage();
  tiles($)[1].children[1].dispatch("click");
  await settle(30);
  host.deleteReplyShape = ({ deleted, ...rest }) => rest;
  await confirm($, 100);
  assert.deepEqual(ids($), [3, 1]);
  assert.equal(requests(host, "item_delete").length, 1);
});

// --- The confirmation's layout: sections, rows, a separate warning (PO's device check) -----------------------

const walk = (el, out = []) => { out.push(el); el.children.forEach((c) => walk(c, out)); return out; };
const byClass = (root, name) => walk(root).filter((el) => el.className.split(" ").includes(name));
const sectionTitles = (root) => byClass(root, "delete-section").map((s) => s.children[0].textContent);
const factsOf = (section) => section.children[1].children.map((g) => [g.children[0].textContent, g.children[1].textContent]);

test("a visit confirmation separates target, effects and notes; date, title and URL are rows of their own; the warning stands apart", async () => {
  const { $, host } = await openUrl();
  host.navigationCounts = { navigationSourcesCleared: 2, navigations: 1 };
  host.visits.A[2].title = "<b>Title</b> & more";
  await expand(rows($)[0]);
  events(rows($)[0])[0].children[1].dispatch("click");
  await settle(30);
  const root = $("#item-delete-description");
  assert.deepEqual(sectionTitles(root), ["削除対象", "削除による影響", "注意事項"]);
  const [target, effects] = byClass(root, "delete-section");
  const facts = factsOf(target);
  assert.deepEqual(facts.map(([name]) => name), ["対象", "日時", "タイトル", "URL"], "each on its own row");
  assert.equal(facts[2][1], "<b>Title</b> & more", "the title is text, never markup");
  assert.equal(facts[3][1], A);
  assert.ok(byClass(target, "delete-long").length === 2, "title and URL wrap inside the dialog");
  assert.deepEqual(factsOf(effects), [["削除する閲覧履歴", "1件"], ["遷移元が検閲済み（[CENSORED]）になる遷移記録", "2件"], ["一緒に削除される遷移記録", "1件"]]);
  const warning = byClass(root, "delete-warning")[0];
  assert.equal(warning.getAttribute("role"), "note");
  assert.match(text(warning), /^削除すると元に戻せません。/);
  assert.equal(walk(root).some((el) => el.tagName === "B" || el.tagName === "IMG"), false, "no element comes from the web's text");
  assert.equal(walk(root).filter((el) => el.className.split(" ").includes("delete-warning")).length, 1);
});

test("the confirm button is short, red and carries the trash icon; cancel has the initial focus, Tab and Escape work", async () => {
  const { $, document } = await openUrl();
  $("#url-delete-all").dispatch("click");
  await settle(30);
  const confirmButton = $("#item-delete-confirm");
  assert.equal(text(confirmButton), "削除する");
  assert.ok(hasIcon(confirmButton), "the icon is assets/delete.svg through the stylesheet");
  assert.equal(document.activeElement, $("#item-delete-cancel"));
  $("#item-delete-dialog").dispatch("keydown", { key: "Tab" });
  assert.equal(document.activeElement, confirmButton);
  $("#item-delete-dialog").dispatch("keydown", { key: "Tab" });
  assert.equal(document.activeElement, $("#item-delete-cancel"));
  $("#item-delete-dialog").dispatch("keydown", { key: "Escape" });
  assert.equal($("#item-delete-dialog").hidden, true);
});

test("the Image and Page confirmations use the same sections, rows and warning", async () => {
  const { $ } = await openImage();
  tiles($)[1].children[1].dispatch("click");
  await settle(30);
  const root = $("#item-delete-description");
  assert.deepEqual(sectionTitles(root), ["削除対象", "削除による影響", "注意事項"]);
  assert.equal(factsOf(byClass(root, "delete-section")[1])[0][0], "削除する画像");
  assert.equal(byClass(root, "delete-warning").length, 1);
  const html = readFileSync(new URL("sidepanel.html", `file:///${extensionDir.replaceAll("\\", "/")}/`), "utf8");
  assert.match(html, /<div id="page-delete-description">/);
  assert.match(html, /<button id="page-delete-confirm"[^>]*class="danger-button"[^>]*><span class="danger-icon"/);
});

test("the stylesheet wraps long values, stacks rows in a narrow panel, keeps the warning red in both themes and handles forced colours", () => {
  const css = readFileSync(new URL("sidepanel.css", `file:///${extensionDir.replaceAll("\\", "/")}/`), "utf8");
  assert.match(css, /\.delete-fact dd\.delete-long \{[^}]*word-break: break-all/);
  assert.match(css, /\.delete-fact dd \{[^}]*overflow-wrap: anywhere/);
  assert.match(css, /@media \(max-width: 300px\) \{ \.delete-fact \{ grid-template-columns: 1fr/);
  assert.match(css, /\.delete-warning \{[^}]*var\(--danger\)[^}]*var\(--error-bg\)[^}]*var\(--error-fg\)/);
  assert.match(css, /forced-colors: active\) \{ \.delete-warning/);
  const sources = ["delete-ui.js", "page-text.js", "page-panel.js"].map((f) => readFileSync(new URL(f, `file:///${extensionDir.replaceAll("\\", "/")}/`), "utf8")).join("\n");
  assert.doesNotMatch(sources, /innerHTML|insertAdjacentHTML|outerHTML/);
});
