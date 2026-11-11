// Issue #43, Slice 3: the four item actions in the side panel (URL results and image details), against
// the fake DOM and a fake host that keeps a small policy of exact URLs and hosts. The page must show
// what the host previews, send a change only after an explicit confirmation, bound to what was
// previewed (ETag, vault token), and never show anything as done before the host confirmed it.
import { test } from "node:test";
import assert from "node:assert/strict";
import { imageSummary, loadSidePanel, pages, settle, tokenOf } from "./fake-dom.js";

const ITEMS = ["navigation_v1", "vault_items_v1"];
const requests = (host, type) => host.requests.filter((m) => m.type === type);
const rows = ($) => $("#results").children;
const urlOf = (li) => li.dataset.url;
const actions = (li) => ({ badge: li.children[1].children[0], select: li.children[1].children[1] });
const optionOf = (select, value) => select.children.find((option) => option.value === value);
/** All text of an element and its descendants, as a person reads it. */
const text = (element) => [element.textContent, ...element.children.map(text)].join(" ");
const dialogText = ($) => `${text($("#item-dialog-body"))} ${$("#item-dialog-error").textContent}`;

async function openUrlTab(options = {}) {
  const panel = await loadSidePanel({ vaults: { A: pages("A", 3) }, active: "A", capabilities: ITEMS, ...options });
  await settle(30);
  return panel;
}

/** Chooses `action` in the drop-down of row `index`, as a person does; the dialog then previews. */
async function choose($, index, action, wait = 20) {
  const { select } = actions(rows($)[index]);
  select.value = action;
  $("#results").dispatch("change", { target: select });
  await settle(wait);
  return select;
}

const confirm = async ($, wait = 20) => {
  $("#item-dialog-confirm").dispatch("click");
  await settle(wait);
};

// --- URL results: state and drop-down -------------------------------------------------------------

test("each URL result shows the host's decision and disables only what would not change it", async () => {
  const urls = pages("A", 3).map((page) => page.url);
  const panel = await loadSidePanel({ vaults: { A: pages("A", 3) }, active: "A", capabilities: ITEMS });
  panel.host.itemPolicy.never.push(urls[0]);
  panel.host.itemPolicy.private.push(urls[1]);
  await settle(30);
  const { $, host } = panel;
  assert.deepEqual(requests(host, "policy_evaluate_batch").map((m) => m.items.map((item) => item.url)), [urls], "one batch for the page, URLs only");
  const states = rows($).map((li) => actions(li).badge.textContent);
  assert.deepEqual(states, ["保存しない", "MCPサーバに見せない", "公開"]);
  const disabled = rows($).map((li) => actions(li).select.children.filter((o) => o.disabled).map((o) => o.value));
  assert.deepEqual(disabled, [["never_save", "save_but_no_mcp"], ["save_but_no_mcp"], ["publish"]], "a stricter rule stays possible; deleting always");
  const first = actions(rows($)[0]).select;
  assert.equal(first.value, "", "the placeholder is chosen");
  assert.equal(first.children[0].textContent, "操作を選択…");
  assert.equal(optionOf(first, "never_save").textContent, "保存しないグループへ追加（既に保存しない）");
  assert.equal(first.getAttribute("aria-label"), "「A page 0」の操作");
  assert.equal(first.getAttribute("aria-describedby"), actions(rows($)[0]).badge.id);
  assert.deepEqual(rows($)[0].children[1].hidden, false);
});

test("a host without vault_items_v1 shows no item action and is asked nothing about the policy", async () => {
  const { $, host } = await openUrlTab({ capabilities: ["navigation_v1"] });
  assert.equal(rows($).length, 3);
  assert.ok(rows($).every((li) => li.children[1].hidden));
  assert.deepEqual(requests(host, "policy_evaluate_batch"), []);
  $("#tab-image").dispatch("click");
  await settle(20);
});

// --- Policy changes for a URL -----------------------------------------------------------------------

test("choosing an action only previews it; the exact URL rule is added after the confirmation only", async () => {
  const { $, host } = await openUrlTab();
  const url = urlOf(rows($)[2]);
  const select = await choose($, 2, "never_save");
  assert.equal($("#item-dialog").hidden, false);
  assert.deepEqual(requests(host, "policy_item_preview").map((m) => [m.url, m.action, m.scope]), [[url, "never_save", "url"]]);
  assert.deepEqual(requests(host, "policy_item_apply"), [], "nothing is changed by choosing");
  assert.equal($("#item-dialog-title").textContent, "保存しないグループへ追加");
  assert.match(dialogText($), /適用範囲: このURLのみ（Exact URL）/);
  assert.match(dialogText($), /現在の状態: 公開/);
  assert.match(dialogText($), /実行後の状態: 保存しない/);
  assert.ok(dialogText($).includes(`追加: 保存しないグループ・Exact URL「${url}」`));
  assert.match(dialogText($), /保存済みのデータを削除しません/);
  assert.equal($("#item-dialog-scope").hidden, false);
  assert.equal($("#item-dialog-ack").hidden, true, "a URL rule needs no extra acknowledgement");
  assert.equal($("#item-dialog-confirm").disabled, false);
  await confirm($);
  assert.deepEqual(requests(host, "policy_item_apply").map((m) => [m.url, m.action, m.scope, m.expectedEtag]), [[url, "never_save", "url", "etag-1"]]);
  assert.deepEqual(host.itemPolicy.never, [url]);
  assert.equal($("#item-dialog").hidden, true);
  assert.equal(select.value, "", "the drop-down is reset");
  assert.equal($("#url-item-status").textContent, "「保存しないグループへ追加」を実行しました。現在の状態: 保存しない");
  assert.equal(actions(rows($)[2]).badge.textContent, "保存しない", "judged again by the host");
});

test("the whole host only when chosen, and only after acknowledging that other URLs are affected", async () => {
  const { $, host } = await openUrlTab();
  const url = urlOf(rows($)[0]);
  await choose($, 0, "save_but_no_mcp");
  $("#item-scope-url").checked = false;
  $("#item-scope-host").checked = true;
  $("#item-scope-host").dispatch("change");
  await settle(20);
  assert.deepEqual(requests(host, "policy_item_preview").map((m) => m.scope), ["url", "host"]);
  assert.match(dialogText($), /同じホスト全体（このURL以外にも適用されます）/);
  assert.equal($("#item-dialog-ack").hidden, false);
  assert.equal($("#item-dialog-confirm").disabled, true, "not as light as a URL rule");
  await confirm($);
  assert.deepEqual(requests(host, "policy_item_apply"), []);
  $("#item-dialog-ack-box").checked = true;
  $("#item-dialog-ack-box").dispatch("change");
  assert.equal($("#item-dialog-confirm").disabled, false);
  await confirm($);
  assert.deepEqual(requests(host, "policy_item_apply").map((m) => m.scope), ["host"]);
  assert.deepEqual(host.itemPolicy.privateHosts, [new URL(url).hostname]);
});

test("publishing removes the URL's exact rule; a broader rule blocks it, says why and leads to the settings", async () => {
  const urls = pages("A", 3).map((page) => page.url);
  const panel = await loadSidePanel({ vaults: { A: pages("A", 3) }, active: "A", capabilities: ITEMS });
  panel.host.itemPolicy.never.push(urls[0], urls[1]);
  panel.host.itemPolicy.broad.set(urls[1], "saveButNoMcp");
  await settle(30);
  const { $, host } = panel;
  // Blocked: never sent, the settings page is one click away.
  await choose($, 1, "publish");
  assert.equal($("#item-dialog-scope").hidden, true, "publishing has no scope");
  assert.match(dialogText($), /ほかのルールまたは基本保護によっても非公開/);
  assert.match(dialogText($), /MCPサーバに見せないグループ: 1番目のルール（Pattern）/);
  assert.equal($("#item-dialog-confirm").disabled, true);
  assert.equal($("#item-dialog-settings").hidden, false);
  $("#item-dialog-settings").dispatch("click");
  await settle();
  assert.equal(host.optionsOpened, 1);
  await confirm($);
  assert.deepEqual(requests(host, "policy_item_apply"), []);
  $("#item-dialog-cancel").dispatch("click");
  // Allowed: its exact rule goes, nothing else.
  await choose($, 0, "publish");
  assert.ok(dialogText($).includes(`削除: 保存しないグループ・Exact URL「${urls[0]}」`));
  await confirm($);
  assert.deepEqual(host.itemPolicy.never, [urls[1]]);
  assert.equal(actions(rows($)[0]).badge.textContent, "公開");
});

test("a policy changed elsewhere after the preview is not overwritten: confirm again against the new one", async () => {
  const { $, host } = await openUrlTab();
  await choose($, 0, "never_save");
  host.itemPolicy.revision += 1; // the settings page applied something meanwhile
  await confirm($);
  assert.deepEqual(requests(host, "policy_item_apply").map((m) => m.expectedEtag), ["etag-1"]);
  assert.equal(host.policyWrites, 0);
  assert.match($("#item-dialog-error").textContent, /ほかの画面で変更されました/);
  assert.equal($("#item-dialog-confirm").disabled, true, "the old preview is not sent again");
  assert.equal($("#item-dialog-repreview").hidden, false);
  await confirm($);
  assert.equal(requests(host, "policy_item_apply").length, 1);
  $("#item-dialog-repreview").dispatch("click");
  await settle(20);
  await confirm($);
  assert.deepEqual(requests(host, "policy_item_apply").map((m) => m.expectedEtag), ["etag-1", "etag-2"]);
  assert.equal(host.policyWrites, 1);
});

test("a refused or failed change is never shown as done, and its error has words", async () => {
  const { $, host } = await openUrlTab();
  for (const [code, words] of [["policy_limit", /上限に達している/], ["layer_disabled", /無効/], ["host_unavailable", /Native Hostに接続できません/], ["db_error", /データベース/]]) {
    const select = await choose($, 0, "never_save");
    host.itemErrors.policy_item_apply = code;
    await confirm($);
    assert.match($("#item-dialog-error").textContent, words, code);
    assert.equal($("#item-dialog").hidden, false);
    assert.equal(actions(rows($)[0]).badge.textContent, "公開", code);
    assert.equal($("#url-item-status").hidden, true, code);
    $("#item-dialog-cancel").dispatch("click");
    assert.equal(select.value, "");
  }
  assert.deepEqual(host.itemPolicy.never, []);
  // A preview the host refuses: nothing to confirm.
  host.itemErrors.policy_item_preview = "invalid_url";
  await choose($, 0, "never_save");
  assert.equal($("#item-dialog-confirm").disabled, true);
  assert.match($("#item-dialog-error").textContent, /記録できないURL/);
});

test("Escape and cancel close without changing anything; one confirmation sends one request", async () => {
  const { $, host } = await openUrlTab();
  const select = await choose($, 0, "never_save");
  $("#item-dialog").dispatch("keydown", { key: "Escape" });
  assert.equal($("#item-dialog").hidden, true);
  assert.equal(select.value, "");
  await choose($, 0, "never_save");
  host.itemDelay = 40;
  $("#item-dialog-confirm").dispatch("click");
  $("#item-dialog-confirm").dispatch("click");
  $("#item-dialog").dispatch("keydown", { key: "Escape" });
  $("#item-dialog-cancel").dispatch("click");
  assert.equal($("#item-dialog").hidden, false, "not closed while the host works");
  assert.equal($("#item-dialog-cancel").disabled, true);
  await settle(80);
  assert.equal(requests(host, "policy_item_apply").length, 1);
  assert.equal($("#item-dialog").hidden, true);
});

// --- Deleting a URL -----------------------------------------------------------------------------------

test("deleting a URL shows the host's counts, says it cannot be undone, and removes the row after the host did", async () => {
  const { $, host } = await openUrlTab();
  host.navigationSources = 2;
  host.imageSavesKept = 1;
  const url = urlOf(rows($)[1]);
  await choose($, 1, "delete");
  assert.deepEqual(requests(host, "url_delete_preview").map((m) => m.url), [url]);
  const said = dialogText($);
  for (const words of [/完全に削除します。取り消せません/, /閲覧記録: 1件/, /タブのスナップショット: 0件/, /ページ情報（タイトル・説明など）: 1件/, /移動の記録: 0件/, /2件は残り、移動元が「不明」になります/, /保存履歴 1件は削除されません/, /バックアップファイルには、この記録が残ります/]) {
    assert.match(said, words);
  }
  assert.equal($("#item-dialog-confirm").textContent, "削除する（取り消せません）");
  assert.equal(rows($).length, 3, "nothing is removed by the preview");
  host.itemDelay = 30;
  $("#item-dialog-confirm").dispatch("click");
  await settle(5);
  assert.equal(rows($).length, 3, "not before the host's answer");
  await settle(50);
  assert.deepEqual(requests(host, "url_delete").map((m) => [m.url, m.vaultToken]), [[url, tokenOf("A")]]);
  assert.deepEqual(rows($).map(urlOf), pages("A", 3).map((page) => page.url).filter((u) => u !== url));
  assert.equal($("#summary").textContent, "2件（最近の順）");
  assert.match($("#url-item-status").textContent, /Vaultから削除しました/);
});

test("after a deletion the next page starts one row earlier: no URL is skipped or shown twice", async () => {
  const all = pages("P", 120);
  const { $, host } = await openUrlTab({ vaults: { A: all } });
  assert.equal(rows($).length, 50);
  await choose($, 10, "delete");
  await confirm($);
  assert.equal(rows($).length, 49);
  $("#load-more").dispatch("click");
  await settle(30);
  assert.deepEqual(requests(host, "search_pages").at(-1).offset, 49);
  $("#load-more").dispatch("click");
  await settle(30);
  const shown = rows($).map(urlOf);
  assert.equal(shown.length, 119);
  assert.equal(new Set(shown).size, 119);
  assert.deepEqual(shown, all.map((page) => page.url).filter((u) => u !== all[10].url));
});

test("a deletion previewed in another vault than the list's deletes nothing and reloads the list", async () => {
  const { $, host } = await openUrlTab({ vaults: { A: pages("A", 3), B: pages("A", 3) } });
  host.active = "B"; // another window switched the vault; this panel has not heard of it
  await choose($, 0, "delete");
  assert.equal($("#item-dialog").hidden, true);
  assert.deepEqual(requests(host, "url_delete"), []);
  assert.match($("#url-item-status").textContent, /Vaultが切り替えられたため、削除しませんでした/);
  await settle(30);
  assert.equal(requests(host, "search_pages").length, 2, "the list starts again in the active vault");
  assert.equal(host.vaults.B.length, 3);
});

test("a deletion refused after its preview keeps the row and asks for a new preview", async () => {
  const { $, host } = await openUrlTab({ vaults: { A: pages("A", 3), B: pages("A", 3) } });
  await choose($, 0, "delete");
  host.active = "B"; // switched between preview and confirmation
  await confirm($);
  assert.deepEqual(requests(host, "url_delete").map((m) => m.vaultToken), [tokenOf("A")]);
  assert.equal(host.vaults.B.length, 3, "nothing deleted in the other vault");
  assert.equal($("#item-dialog").hidden, true);
  await settle(30);
  assert.equal(requests(host, "search_pages").length, 2, "the list starts again, in the active vault");
});

test("a deletion that fails keeps the row and asks for a new preview", async () => {
  const { $, host } = await openUrlTab();
  await choose($, 0, "delete");
  host.itemErrors.url_delete = "db_error";
  await confirm($);
  assert.equal(rows($).length, 3);
  assert.match($("#item-dialog-error").textContent, /データベース/);
  assert.equal($("#item-dialog-confirm").disabled, true);
  assert.equal($("#item-dialog-repreview").hidden, false);
});

test("a new search or a vault switch closes the dialog, and its late answer fills nothing", async () => {
  const { $, host, document } = await openUrlTab({ vaults: { A: pages("A", 3), B: pages("B", 2) } });
  host.itemDelay = 40;
  await choose($, 0, "delete", 5);
  $("#query").value = "page 1";
  $("#query").dispatch("keydown", { key: "Enter" });
  await settle(60);
  assert.equal($("#item-dialog").hidden, true);
  assert.equal(text($("#item-dialog-body")).trim(), "");
  host.itemDelay = 40;
  await choose($, 0, "never_save", 5);
  $("#tab-settings").dispatch("click");
  $("#vault-path-input").value = "B";
  document.querySelector('[data-manual="vault_open"]').dispatch("click");
  await settle(80);
  assert.equal($("#item-dialog").hidden, true);
  assert.deepEqual(requests(host, "policy_item_apply"), []);
  // The search typed before still applies, now in vault B.
  assert.deepEqual(rows($).map(urlOf), [pages("B", 2)[1].url]);
});

test("one confirmation deletes once, even clicked twice", async () => {
  const { $, host } = await openUrlTab();
  await choose($, 0, "delete");
  host.itemDelay = 40;
  $("#item-dialog-confirm").dispatch("click");
  $("#item-dialog-confirm").dispatch("click");
  await settle(80);
  assert.equal(requests(host, "url_delete").length, 1);
  assert.equal(rows($).length, 2);
});

test("a late preview of the item chosen before never fills the dialog of the item chosen after", async () => {
  const { $, host } = await openUrlTab();
  const [first, second] = rows($).map(urlOf);
  host.itemDelay = 40;
  await choose($, 0, "never_save", 5);
  host.itemDelay = 0;
  await choose($, 1, "save_but_no_mcp", 80);
  assert.equal($("#item-dialog-title").textContent, "MCPサーバに見せないグループへ追加");
  assert.ok(dialogText($).includes(second));
  assert.ok(!dialogText($).includes(first), "the first item's answer was dropped");
  assert.equal(actions(rows($)[0]).select.value, "", "the first drop-down was reset when its dialog closed");
});

test("a rule added elsewhere since the list was judged: the preview says nothing changes, and nothing is sent", async () => {
  const { $, host } = await openUrlTab();
  const url = urlOf(rows($)[0]);
  host.itemPolicy.never.push(url); // the settings page or another window
  await choose($, 0, "never_save");
  assert.match(dialogText($), /同じルールが既にあります。変更はありません/);
  assert.equal($("#item-dialog-confirm").disabled, true);
  await confirm($);
  assert.deepEqual(requests(host, "policy_item_apply"), []);
});

test("a vault switch closes the dialog at once, before the new list arrives", async () => {
  const { $, host, document } = await openUrlTab({ vaults: { A: pages("A", 3), B: pages("B", 2) } });
  await choose($, 0, "delete");
  assert.equal($("#item-dialog").hidden, false);
  host.searchDelay.B = 200;
  $("#tab-settings").dispatch("click");
  $("#vault-path-input").value = "B";
  document.querySelector('[data-manual="vault_open"]').dispatch("click");
  await settle(40);
  assert.equal($("#item-dialog").hidden, true, "closed with the old vault's rows");
  assert.deepEqual(requests(host, "url_delete"), []);
});

test("a next page from another vault (switched in another window) is not appended: the list starts again", async () => {
  const { $, host } = await openUrlTab({ vaults: { A: pages("A", 80), B: pages("B", 80) } });
  assert.equal(rows($).length, 50);
  host.active = "B";
  $("#load-more").dispatch("click");
  await settle(60);
  const shown = rows($).map(urlOf);
  assert.ok(shown.every((url) => url.startsWith("https://b")), "no row of vault A next to vault B's");
  assert.equal(shown.length, 50);
});

// --- Images ---------------------------------------------------------------------------------------------

const source = (savedImageId, fields = {}) => ({ savedImageId, imageUrl: `https://img.test/${savedImageId}.png`, pageUrl: "https://page.test/a", frameUrl: null, savedAt: 1_000_000 + savedImageId, incognito: false, mcpPublic: true, ...fields });

async function openImage(index = 0, options = {}) {
  const images = options.images ?? [
    imageSummary(3, 1_000_003, { sources: [source(31, { imageUrl: "data:image/png", pageUrl: "https://page.test/b", frameUrl: "https://frame.test/f" }), source(30)] }),
    imageSummary(2, 1_000_002, { sources: [source(20)] }),
  ];
  const panel = await loadSidePanel({ vaults: { A: pages("A", 1), B: pages("B", 1) }, active: "A", images: { A: images, B: options.imagesB ?? [imageSummary(9, 1)] }, capabilities: options.capabilities ?? ITEMS });
  await settle(30);
  panel.$("#tab-image").dispatch("click");
  await settle(30);
  panel.$("#gallery-grid").dispatch("click", { target: panel.$("#gallery-grid").children[index].children[0], detail: 1 });
  await settle(30);
  return panel;
}

async function manage($, action, wait = 30) {
  $("#gallery-manage").value = action;
  $("#gallery-manage").dispatch("change");
  await settle(wait);
}

test("the image details offer the four actions, with what the host says of the image's URLs", async () => {
  const { $, host } = await openImage();
  assert.equal($("#gallery-manage-section").hidden, false);
  assert.equal($("#gallery-manage").disabled, false);
  assert.deepEqual($("#gallery-manage").children.map((o) => o.value), ["", "never_save", "save_but_no_mcp", "delete", "publish"]);
  const judged = requests(host, "policy_evaluate_batch").at(-1).items.map((item) => item.url);
  assert.deepEqual(judged.sort(), ["https://frame.test/f", "https://img.test/30.png", "https://page.test/a", "https://page.test/b"], "a data: marker is no URL");
  assert.match($("#gallery-manage-note").textContent, /公開 4・MCPサーバに見せない 0・保存しない 0/);
  assert.equal(optionOf($("#gallery-manage"), "publish").disabled, true, "every URL is public already");
  assert.match(optionOf($("#gallery-manage"), "publish").textContent, /すべてのURLが既に公開/);
});

test("a rule for an image is for one URL the person picks from its save history, never for the image", async () => {
  const { $, host } = await openImage();
  await manage($, "save_but_no_mcp");
  assert.equal($("#item-dialog").hidden, false);
  assert.equal($("#item-dialog-urls").hidden, false);
  const radios = $("#item-dialog-url-list").children.map((row) => row.children[0]);
  assert.deepEqual(radios.map((r) => r.value), ["https://page.test/b", "https://frame.test/f", "https://img.test/30.png", "https://page.test/a"]);
  const labels = $("#item-dialog-url-list").children.map((row) => row.children[1].textContent);
  assert.match(labels[0], /^ページURL（保存1件）（現在: 公開）/);
  assert.match(labels[1], /^フレームURL/);
  assert.match(labels[2], /^画像URL/);
  assert.ok(radios.every((r) => !r.checked), "no URL is chosen for the person");
  assert.deepEqual(requests(host, "policy_item_preview"), [], "nothing previewed before a URL is chosen");
  assert.equal($("#item-dialog-confirm").disabled, true);
  assert.match(dialogText($), /画像そのものではなく、選んだURLにルールが適用されます/);
  radios[1].checked = true;
  $("#item-dialog-url-list").dispatch("change", { target: radios[1] });
  await settle(20);
  assert.deepEqual(requests(host, "policy_item_preview").map((m) => [m.url, m.scope]), [["https://frame.test/f", "url"]]);
  assert.ok(dialogText($).includes("対象URL: https://frame.test/f"));
  await confirm($);
  assert.deepEqual(host.itemPolicy.private, ["https://frame.test/f"]);
  assert.equal($("#gallery-manage").value, "");
  assert.match($("#gallery-status").textContent, /https:\/\/frame\.test\/f/);
  assert.match($("#gallery-manage-note").textContent, /MCPサーバに見せない 1/);
});

test("publishing for an image says what MCP also needs, and URLs already public cannot be chosen", async () => {
  const images = [imageSummary(5, 1_000_005, { sources: [source(50, { mcpPublic: false }), source(51, { pageUrl: "https://page.test/c", mcpPublic: false })] })];
  const panel = await loadSidePanel({ vaults: { A: pages("A", 1) }, active: "A", images: { A: images }, capabilities: ITEMS });
  panel.host.itemPolicy.never.push("https://page.test/c");
  await settle(30);
  const { $, host } = panel;
  $("#tab-image").dispatch("click");
  await settle(30);
  $("#gallery-grid").dispatch("click", { target: $("#gallery-grid").children[0].children[0], detail: 1 });
  await settle(30);
  await manage($, "publish");
  const radios = $("#item-dialog-url-list").children.map((row) => row.children[0]);
  const byUrl = Object.fromEntries(radios.map((r) => [r.value, r]));
  assert.equal(byUrl["https://page.test/a"].disabled, true, "already public");
  assert.equal(byUrl["https://page.test/c"].disabled, false);
  byUrl["https://page.test/c"].checked = true;
  $("#item-dialog-url-list").dispatch("change", { target: byUrl["https://page.test/c"] });
  await settle(20);
  assert.match(dialogText($), /公開を許可した保存履歴がないため、URLを公開してもMCPサーバには出ません/);
  await confirm($);
  assert.deepEqual(host.itemPolicy.never, []);
  assert.deepEqual(requests(host, "image_mcp_public_set"), [], "the permission per save is not changed");
});

test("deleting an image shows the host's counts and removes it, its details and only its tags", async () => {
  const { $, host } = await openImage();
  host.tags.A = { 3: ["cat", "only"], 2: ["cat"] };
  await manage($, "delete");
  assert.deepEqual(requests(host, "image_delete_preview").map((m) => [m.blobId, m.sha256]), [[3, imageSummary(3, 0).sha256]]);
  const said = dialogText($);
  for (const words of [/完全に削除します。取り消せません/, /保存履歴: 2件（すべて）/, /サムネイル: 1件/, /タグの付与: 2件（ほかの画像に付いていないタグ 1件はタグ一覧からも消えます）/, /バックアップファイルには、この画像が残ります/]) assert.match(said, words);
  assert.equal($("#gallery-grid").children.length, 2, "nothing removed by the preview");
  await confirm($);
  assert.deepEqual(requests(host, "image_delete").map((m) => [m.blobId, m.vaultToken]), [[3, tokenOf("A")]]);
  assert.deepEqual($("#gallery-grid").children.map((li) => Number(li.dataset.blobId)), [2]);
  assert.equal($("#gallery-details").hidden, true);
  assert.equal($("#gallery-status").textContent, "画像をVaultから削除しました。");
  assert.deepEqual(host.tags.A, { 2: ["cat"] }, "the other image keeps its tags");
});

test("an image deletion for a vault switched elsewhere deletes nothing; another selection drops a late preview", async () => {
  // Vault B (made active in another window) holds the same image, and one more.
  const { $, host } = await openImage(0, { imagesB: [imageSummary(3, 1_000_003), imageSummary(9, 1)] });
  host.active = "B";
  await manage($, "delete");
  assert.equal($("#item-dialog").hidden, true);
  assert.deepEqual(requests(host, "image_delete"), []);
  await settle(30);
  assert.deepEqual($("#gallery-grid").children.map((li) => Number(li.dataset.blobId)), [3, 9], "the list follows the active vault");
  assert.match($("#gallery-status").textContent, /Vaultが切り替えられたため、削除しませんでした/);
  assert.equal(host.images.B.length, 2);
  host.active = "A";
  $("#gallery-refresh").dispatch("click");
  await settle(40);
  $("#gallery-grid").dispatch("click", { target: $("#gallery-grid").children[0].children[0], detail: 1 });
  await settle(30);
  host.itemDelay = 40;
  await manage($, "delete", 5);
  $("#gallery-grid").dispatch("click", { target: $("#gallery-grid").children[1].children[0], detail: 1 });
  await settle(80);
  assert.equal($("#item-dialog").hidden, true, "the dialog belonged to the other image");
  assert.equal(text($("#item-dialog-body")).trim(), "");
  assert.deepEqual(requests(host, "image_delete"), []);
});

test("without vault_items_v1 the image details say the host must be updated, and copying still works as before", async () => {
  const { $, host } = await openImage(0, { capabilities: ["navigation_v1"] });
  assert.equal($("#gallery-manage-section").hidden, false);
  assert.equal($("#gallery-manage").disabled, true);
  assert.match($("#gallery-manage-note").textContent, /Native Hostを更新すると/);
  assert.deepEqual(requests(host, "policy_evaluate_batch"), []);
});

test("an image the vault no longer has is not offered again for deletion", async () => {
  const { $, host } = await openImage();
  host.images.A = host.images.A.filter((item) => item.blobId !== 3); // deleted elsewhere
  await manage($, "delete");
  assert.equal($("#item-dialog").hidden, false);
  assert.match($("#item-dialog-error").textContent, /この画像がVaultに見つかりません.*画像一覧を更新してください/);
  assert.equal($("#item-dialog-confirm").disabled, true);
  assert.equal($("#item-dialog-repreview").hidden, true);
  assert.deepEqual(requests(host, "image_delete"), []);
});

// --- With the real host -------------------------------------------------------------------------------
// Opt-in (MEMOCAMPUS_TEST_HOST): the side panel's real code drives the real host binary, one process
// per message as Chrome does, in a temporary MEMOCAMPUS_APP_DIR with a fresh vault and policy. Nothing
// touches the user's vault, policy or Chrome profile.

const HOST_BIN = process.env.MEMOCAMPUS_TEST_HOST;

test("end to end with the real host: a URL hidden, published and deleted, an image deleted, all from the panel", { skip: HOST_BIN ? false : "set MEMOCAMPUS_TEST_HOST to run" }, async () => {
  const { spawn } = await import("node:child_process");
  const { mkdtempSync, rmSync } = await import("node:fs");
  const os = await import("node:os");
  const path = await import("node:path");
  const { DatabaseSync } = await import("node:sqlite");
  const appDir = mkdtempSync(path.join(os.tmpdir(), "memocampus-items-ui-e2e-"));
  const env = { ...process.env, MEMOCAMPUS_APP_DIR: appDir };
  delete env.MEMOCAMPUS_DB;
  const running = new Set();
  let closing = false;
  const call = (message) =>
    new Promise((resolve, reject) => {
      if (closing) return reject(new Error("test finished"));
      const child = spawn(HOST_BIN, [], { env, cwd: appDir, stdio: ["pipe", "pipe", "pipe"] });
      running.add(child.pid);
      const chunks = [];
      child.stdout.on("data", (chunk) => chunks.push(chunk));
      child.on("error", reject);
      child.on("close", () => {
        running.delete(child.pid);
        const out = Buffer.concat(chunks);
        if (out.length < 4) return reject(new Error("no answer"));
        resolve(JSON.parse(out.subarray(4, 4 + out.readUInt32LE(0)).toString("utf8")));
      });
      const body = Buffer.from(JSON.stringify(message), "utf8");
      const length = Buffer.alloc(4);
      length.writeUInt32LE(body.length);
      child.stdin.end(Buffer.concat([length, body]));
    });
  const vault = path.join(appDir, "memocampus.sqlite3");
  const A = "https://forum.test/thread/1?page=2";
  const B = "https://blog.test/post";
  try {
    assert.equal((await call({ type: "policy_initialize", requestId: "p" })).ok, true);
    assert.equal((await call({ type: "vault_create", requestId: "c", useDefault: true })).ok, true);
    const db = new DatabaseSync(vault);
    try {
      db.exec("PRAGMA foreign_keys = ON;");
      const visit = db.prepare("INSERT INTO visits VALUES (?, ?, ?, '2026-11-07T10:00:00.000Z')");
      for (const [id, url, title] of [["a1", A, "Thread"], ["a2", A, "Thread"], ["b1", B, "Blog"]]) visit.run(id, url, title);
      db.exec(`INSERT INTO image_blobs VALUES (1, '${"a".repeat(64)}', 'image/png', 3, 40, 30, 1762509600000, 1762509600000, 2, x'010203')`);
      db.exec(`INSERT INTO image_blobs VALUES (2, '${"b".repeat(64)}', 'image/png', 3, 40, 30, 1762509500000, 1762509500000, 1, x'040506')`);
      const save = db.prepare("INSERT INTO saved_images (save_id, blob_id, image_url, page_url, saved_at, incognito, mcp_public) VALUES (?, ?, ?, ?, ?, 0, 1)");
      save.run("s1", 1, "https://img.test/1.png", A, 1762509600000);
      save.run("s2", 1, "https://img.test/1.png", B, 1762509500000);
      save.run("s3", 2, "https://img.test/2.png", B, 1762509400000);
    } finally {
      db.close();
    }
    const { $ } = await loadSidePanel({ vaults: { [vault]: [] }, active: vault, images: {}, forward: call });
    await settle(2500);
    assert.deepEqual(rows($).map(urlOf).sort(), [B, A].sort());
    const rowOf = (url) => rows($).findIndex((li) => urlOf(li) === url);
    assert.deepEqual(rows($).map((li) => actions(li).badge.textContent), ["公開", "公開"]);

    await choose($, rowOf(A), "never_save", 800);
    assert.match(dialogText($), /Exact URL「https:\/\/forum\.test\/thread\/1\?page=2」/);
    await confirm($, 1500);
    assert.equal(actions(rows($)[rowOf(A)]).badge.textContent, "保存しない");
    let rules = (await call({ type: "policy_get", requestId: "g" })).policy.layers;
    assert.deepEqual(rules.neverSave.rules, [{ kind: "exact", pattern: A }]);

    await choose($, rowOf(A), "publish", 800);
    await confirm($, 1500);
    assert.equal(actions(rows($)[rowOf(A)]).badge.textContent, "公開");
    rules = (await call({ type: "policy_get", requestId: "g" })).policy.layers;
    assert.deepEqual(rules.neverSave.rules, []);

    await choose($, rowOf(B), "delete", 800);
    assert.match(dialogText($), /閲覧記録: 1件/);
    assert.match(dialogText($), /保存履歴 2件は削除されません/);
    await confirm($, 1500);
    assert.deepEqual(rows($).map(urlOf), [A]);

    $("#tab-image").dispatch("click");
    await settle(1500);
    assert.deepEqual($("#gallery-grid").children.map((li) => Number(li.dataset.blobId)), [1, 2]);
    $("#gallery-grid").dispatch("click", { target: $("#gallery-grid").children[0].children[0], detail: 1 });
    await settle(1500);
    await manage($, "delete", 800);
    assert.match(dialogText($), /保存履歴: 2件（すべて）/);
    await confirm($, 1500);
    assert.deepEqual($("#gallery-grid").children.map((li) => Number(li.dataset.blobId)), [2]);

    const check = new DatabaseSync(vault, { readOnly: true });
    try {
      assert.deepEqual(check.prepare("SELECT url FROM visits ORDER BY event_id").all().map((row) => row.url), [A, A]);
      assert.deepEqual(check.prepare("SELECT id FROM image_blobs").all().map((row) => row.id), [2]);
      assert.deepEqual(check.prepare("SELECT save_id FROM saved_images").all().map((row) => row.save_id), ["s3"], "B's image save stays: only the image deleted went");
      assert.equal(check.prepare("PRAGMA integrity_check").get().integrity_check, "ok");
      assert.deepEqual(check.prepare("PRAGMA foreign_key_check").all(), []);
    } finally {
      check.close();
    }
  } finally {
    closing = true;
    for (let wait = 0; running.size > 0 && wait < 100; wait += 1) await settle(100);
    rmSync(appDir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  }
});
