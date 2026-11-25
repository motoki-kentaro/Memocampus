// Issue #77 Slice 2: the Side Panel in Japanese and English. Static text comes from sidepanel.html through
// i18n-dom.js; dynamic text from the modules through t(). The Vault section is localized in Slice 3
// (i18n-vault.test.js). The language never changes what is sent to the host or what a choice means.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import vm from "node:vm";
import { extensionDir, loadSidePanel, pages, settle } from "./fake-dom.js";
import { deleteState, visitRow } from "./delete-fixtures.js";
import { createI18n, withLocale } from "./i18n-mock.js";
import { dayLabel, monthName, weekdayNames, yearMonthName } from "../../extension/visit-calendar.js";
import { DELETE_TEXT, deleteErrorText, deletionModel } from "../../extension/delete-ui.js";
import { pageErrorText } from "../../extension/page-text.js";
import { messageTable } from "../../extension/i18n.js";

const JAPANESE = /[ぁ-んァ-ヶ一-龠（）「」、。・]/u;
const html = readFileSync(path.join(extensionDir, "sidepanel.html"), "utf8");
const domScript = readFileSync(path.join(extensionDir, "i18n-dom.js"), "utf8");
const VAULT_START = "<!-- i18n: the Vault section";
const VAULT_END = "<!-- i18n: end of the Vault section";
const ITEMS = ["navigation_v1", "vault_items_v1", "visit_calendar_v1", "item_delete_v1"];
const text = (el) => [el.textContent, ...el.children.map(text)].join(" ").replace(/\s+/g, " ").trim();

/** The elements of sidepanel.html that carry data-i18n*, as attribute maps, localized by the real i18n-dom.js. */
function localizeHtml(locale) {
  const elements = [];
  for (const [, tag, attributes] of html.matchAll(/<([a-z0-9]+)\b([^>]*\bdata-i18n[^>]*)>/g)) {
    const attrs = new Map([...attributes.matchAll(/([\w-]+)="([^"]*)"/g)].map(([, name, value]) => [name, value]));
    elements.push({ tag, attrs, textContent: "", getAttribute: (name) => (attrs.has(name) ? attrs.get(name) : null), setAttribute: (name, value) => attrs.set(name, String(value)) });
  }
  const root = { lang: "ja" };
  const document = { documentElement: root, querySelectorAll: () => elements };
  vm.runInContext(domScript, vm.createContext({ document, chrome: { i18n: createI18n({ locale }) } }));
  return { elements, lang: root.lang };
}

// --- Static HTML -------------------------------------------------------------------------------------------

test("sidepanel.html: no Japanese text anywhere, and the Vault section follows the page language (Slice 3)", () => {
  assert.deepEqual(html.split("\n").filter((line) => JAPANESE.test(line) && !/^\s*<!--/.test(line)), []);
  assert.ok(!html.includes(VAULT_START) && !html.includes(VAULT_END), "the Slice 2 markers are gone");
  assert.match(html, /<section id="vault-panel" class="vault-panel" aria-labelledby="vault-heading">/);
  // Only <html lang> (set from the catalog by i18n-dom.js); no part of the panel keeps a language of its own.
  assert.deepEqual(html.match(/\slang="[^"]*"/g), [' lang="ja"']);
  assert.match(html, /^<!doctype html>\n<html lang="ja">/i);
  // i18n-dom.js runs as a classic script after the markup and before the page's module.
  assert.ok(html.indexOf('<script src="i18n-dom.js"></script>') < html.indexOf('<script type="module" src="sidepanel.js"></script>'));
  assert.equal((html.match(/<script/g) ?? []).length, 2, "no other script; CSP unchanged");
});

test("sidepanel.html is localized in both languages, and <html lang> follows the catalog", () => {
  for (const locale of ["ja", "en"]) {
    const { elements, lang } = localizeHtml(locale);
    assert.equal(lang, locale);
    assert.ok(elements.length > 50);
    for (const element of elements) {
      for (const [name, value] of element.attrs) if (/^(aria-label|title|placeholder)$/.test(name)) assert.notEqual(value, "", `${element.tag} ${name}`);
      if (element.attrs.has("data-i18n")) assert.notEqual(element.textContent, "");
      if (locale === "en") {
        assert.doesNotMatch(element.textContent, JAPANESE);
        for (const [name, value] of element.attrs) assert.doesNotMatch(value, JAPANESE, `${element.tag} ${name}`);
      }
    }
  }
});

test("the privacy choices keep their values in every language; only their text changes", () => {
  const scopes = [...html.matchAll(/<input id="item-scope-(\w+)" type="radio" name="item-scope" value="(\w+)"/g)].map(([, id, value]) => [id, value]);
  assert.deepEqual(scopes, [["url", "url"], ["host", "host"]]);
  const options = [...html.matchAll(/<option value="(\w*)"[^>]*data-i18n="(\w+)"/g)].map(([, value, key]) => [value, key]);
  assert.deepEqual(options, [["", "rowChooseAction"], ["never_save", "policyActionNeverSave"], ["save_but_no_mcp", "policyActionSaveButNoMcp"], ["publish", "policyActionPublish"]]);
  const label = (locale, key) => localizeHtml(locale).elements.find((element) => element.attrs.get("data-i18n") === key).textContent;
  assert.equal(label("ja", "policyScopeUrlOption"), "このURLのみ");
  assert.equal(label("ja", "policyScopeHostOption"), "同じホスト全体（ほかのURLにも適用）");
  assert.equal(label("en", "policyScopeUrlOption"), "This URL only");
  assert.equal(label("en", "policyScopeHostOption"), "The whole host (also applies to other URLs)", "the wider scope never reads narrower");
  assert.equal(label("en", "policyActionSaveButNoMcp"), "Add to the Hide from MCP group");
});

// --- Shared modules ----------------------------------------------------------------------------------------

test("calendar names: Japanese exactly as before, English from Intl (en-US)", async () => {
  assert.deepEqual(weekdayNames(), ["日", "月", "火", "水", "木", "金", "土"]);
  assert.deepEqual(weekdayNames("long"), ["日曜日", "月曜日", "火曜日", "水曜日", "木曜日", "金曜日", "土曜日"]);
  assert.equal(monthName(10), "11月");
  assert.equal(yearMonthName(2026, 10), "2026年11月");
  await withLocale("en", () => {
    assert.deepEqual(weekdayNames(), ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"]);
    assert.equal(monthName(10), "Nov");
    assert.equal(yearMonthName(2026, 10), "November 2026");
    assert.equal(dayLabel(2026, 10, 8, 12), "Sun, November 8: 12 visits");
    assert.equal(dayLabel(2026, 10, 14, 1), "Sat, November 14: 1 visit");
    assert.equal(dayLabel(2026, 10, 9, 0), "Mon, November 9: no visits");
    assert.equal(dayLabel(2026, 10, 9, null), "Mon, November 9: loading");
    assert.equal(dayLabel(2026, 10, 8, 3, { today: true, selected: true }), "Sun, November 8: 3 visits, today, selected");
  });
});

test("deletion texts in English keep what is and is not deleted; codes still decide", async () => {
  await withLocale("en", () => {
    const visits = deletionModel({ target: { scope: "visits" }, counts: { visits: 3, urls: 1, snapshotsKept: 1, navigations: 0, navigationSourcesCleared: 0, pageMetadata: 0 } }, "search “alpha”");
    const shown = JSON.stringify(visits);
    assert.doesNotMatch(shown, JAPANESE);
    assert.match(shown, /Visits to delete/);
    assert.match(shown, /Snapshots are not included \(the snapshots of these URLs remain: 1\)/);
    assert.equal(deleteErrorText("item_delete_unknown").startsWith("The deletion state cannot be confirmed."), true);
    assert.doesNotMatch(deleteErrorText("item_delete_unknown"), /Nothing was deleted/, "an unknown outcome never claims nothing was deleted");
    for (const code of ["__proto__", "constructor", "no_such", undefined]) {
      assert.match(deleteErrorText(code), /^Deletion failed\./);
      assert.match(pageErrorText(code), /^The page operation failed\./);
    }
    assert.equal(DELETE_TEXT.visitsAll, "Delete all visits");
  });
  assert.equal(DELETE_TEXT.visitsAll, "全閲覧履歴を削除", "a table follows the language when read, not when loaded");
});

test("messageTable reads each message when used and fails loudly for a missing one", async () => {
  const table = messageTable({ save: "menuSaveImage", broken: "noSuchMessage" });
  assert.equal(table.save, "Memocampusへ保存");
  await withLocale("en", () => assert.equal(table.save, "Save to Memocampus"));
  assert.throws(() => table.broken, /Missing extension message: noSuchMessage/);
  assert.ok(Object.isFrozen(table));
});

// --- The panel, in English ----------------------------------------------------------------------------------

test("en: the URL tab's own texts are English; the page's data is shown as it is", async () => {
  await withLocale("en", async () => {
    const rows = [{ url: "https://x.test/a", title: "<img src=x onerror=alert(1)> 日本語のタイトル", lastVisitedAt: "2026-10-12T00:00:00.000Z", lastCapturedAt: null, lastActivityAt: "2026-10-12T00:00:00.000Z", source: "visit" }];
    const { $ } = await loadSidePanel({ vaults: { A: rows }, active: "A", capabilities: ITEMS });
    await settle(40);
    assert.equal($("#summary").textContent, "1 · newest first");
    const li = $("#results").children[0];
    const select = li.children[1].children[1];
    assert.equal(select.getAttribute("aria-label"), "Actions for “<img src=x onerror=alert(1)> 日本語のタイトル”", "web text is inserted as text, untranslated");
    assert.equal(select.children[0].textContent, "Choose an action…");
    assert.deepEqual(select.children.map((option) => option.value), ["", "never_save", "save_but_no_mcp", "publish"]);
    assert.equal(li.children[1].children[0].textContent, "Visible to MCP", "MCP visibility, never a general “Public”");
    assert.equal(text($("#url-delete-all")), "Delete all visits");
    assert.equal($("#url-delete-all").getAttribute("aria-label"), "Delete all visits (snapshots are not included)");
    assert.equal($("#tab-settings").getAttribute("aria-label") ?? "Settings", "Settings");
  });
});

test("en: an empty vault and a search without match say so in English", async () => {
  await withLocale("en", async () => {
    const { $ } = await loadSidePanel({ vaults: { A: [] }, active: "A" });
    await settle(30);
    assert.equal($("#empty").textContent, "No pages have been recorded yet.");
    $("#query").value = "zzz";
    $("#query").dispatch("keydown", { key: "Enter" });
    await settle(40);
    assert.equal($("#empty").textContent, "No pages match “zzz”.");
  });
});

/** Runs the same host-wide rule flow and returns what was sent to the host and what was shown. */
async function hostRuleFlow() {
  const { $, host } = await loadSidePanel({ vaults: { A: pages("A", 2) }, active: "A", capabilities: ITEMS });
  await settle(30);
  const select = $("#results").children[0].children[1].children[1];
  select.value = "save_but_no_mcp";
  $("#results").dispatch("change", { target: select });
  await settle(20);
  $("#item-scope-url").checked = false;
  $("#item-scope-host").checked = true;
  $("#item-scope-host").dispatch("change");
  await settle(20);
  const shown = { body: text($("#item-dialog-body")), ack: $("#item-dialog-ack-text").textContent, title: $("#item-dialog-title").textContent, confirm: $("#item-dialog-confirm").textContent, before: $("#item-dialog-confirm").disabled };
  $("#item-dialog-ack-box").checked = true;
  $("#item-dialog-ack-box").dispatch("change");
  $("#item-dialog-confirm").dispatch("click");
  await settle(30);
  const sent = host.requests.filter((m) => m.type.startsWith("policy_item_")).map(({ type, url, action, scope, expectedEtag }) => ({ type, url, action, scope, expectedEtag }));
  return { sent, shown, status: $("#url-item-status").textContent, policy: structuredClone(host.itemPolicy.privateHosts) };
}

test("en: the host-wide rule reads as wide as in Japanese, needs the same acknowledgement and sends the same request", async () => {
  const ja = await hostRuleFlow();
  const en = await withLocale("en", hostRuleFlow);
  assert.deepEqual(en.sent, ja.sent, "the language changes no value sent to the host");
  assert.deepEqual(en.policy, ja.policy);
  assert.equal(ja.shown.before, true);
  assert.equal(en.shown.before, true, "not confirmable before the acknowledgement");
  assert.match(ja.shown.body, /適用範囲: 同じホスト全体（このURL以外にも適用されます）/);
  assert.match(en.shown.body, /Scope: the whole host \(also applies to URLs other than this one\)/);
  assert.match(en.shown.body, /Hidden from the MCP server \(still recorded in the Vault\)/);
  assert.equal(en.shown.ack, "I understand that this applies to every URL of this host (including other pages)");
  assert.equal(en.shown.title, "Add to the Hide from MCP group");
  assert.doesNotMatch(Object.values(en.shown).join(" "), JAPANESE);
  assert.equal(en.status, "“Add to the Hide from MCP group” was done. Current state: Hidden from MCP server");
});

/** Opens the range deletion with a search, as a person does, and returns what was asked and shown. */
async function rangeDeleteFlow() {
  const A = "https://a.test/", B = "https://b.test/";
  const rows = [
    { url: A, title: "Alpha", lastVisitedAt: "2026-10-12T00:00:00.000Z", lastCapturedAt: null, lastActivityAt: "2026-10-12T00:00:00.000Z", source: "visit" },
    { url: B, title: "Beta", lastVisitedAt: "2026-10-11T00:00:00.000Z", lastCapturedAt: null, lastActivityAt: "2026-10-11T00:00:00.000Z", source: "visit" },
  ];
  const { $, host } = await loadSidePanel({ vaults: { A: rows }, active: "A", capabilities: ITEMS });
  deleteState(host);
  host.visits = { A: [visitRow("a1", A, "Alpha one"), visitRow("a2", A, "Alpha two"), visitRow("b1", B, "Beta one")] };
  host.snapshots = { A: [A] };
  await settle(30);
  $("#query").value = "alpha";
  $("#query").dispatch("keydown", { key: "Enter" });
  await settle(60);
  $("#url-delete-all").dispatch("click");
  await settle(30);
  const shown = text($("#item-delete-description"));
  const targets = host.deleteRequests.filter((m) => m.type === "item_delete_preview").map((m) => m.target);
  return { shown, targets, title: $("#item-delete-title").textContent, confirm: text($("#item-delete-confirm")) };
}

test("en: the deletion dialog names the same target and keeps snapshots out, as in Japanese", async () => {
  const ja = await rangeDeleteFlow();
  const en = await withLocale("en", rangeDeleteFlow);
  assert.deepEqual(en.targets, ja.targets);
  assert.deepEqual(en.targets, [{ scope: "visits", query: "alpha", visitedFrom: null, visitedTo: null }]);
  assert.match(ja.shown, /条件 検索「alpha」/);
  assert.match(en.shown, /Condition search “alpha”/);
  assert.match(en.shown, /Visits to delete 2/);
  assert.match(en.shown, /Snapshots are not included/);
  assert.match(en.shown, /Deletion cannot be undone\./);
  assert.doesNotMatch(en.shown.replace(/Alpha|alpha/g, ""), JAPANESE);
  assert.equal(en.title, "Confirm deletion");
  assert.equal(en.confirm, "Delete");
});

// --- Translation review (Slice 2) ------------------------------------------------------------------------

test("en: MCP visibility is never worded as general publication", () => {
  const en = JSON.parse(readFileSync(path.join(extensionDir, "_locales", "en", "messages.json"), "utf8"));
  const mcpKeys = ["policyStateAllow", "policyStateAllowLong", "policyActionPublish", "policyConfirmPublish", "policySamePublish", "policyAlreadyPublic",
    "policyReasonPublishBlocked", "policyBlockersHeading", "imageStatesTally", "imagePublicationRule", "imagePublicationNone", "imageIncognitoNotPublic", "imageManageUpdateHost"];
  for (const key of mcpKeys) {
    // "publishing to MCP" names its destination; a bare public / publish / private does not.
    assert.doesNotMatch(en[key].message.replace(/publishing to MCP|publishing permission/g, ""), /\b(public|publish|published|private)\b/i, key);
    assert.match(en[key].message, /MCP/, key);
  }
  assert.equal(en.policyStateAllow.message, "Visible to MCP");
  assert.equal(en.policyActionPublish.message, "Allow MCP access");
  assert.equal(en.policyStateNeverSave.message, "Not recorded automatically", "never read as deleted");
  // The image condition stays an AND of the save record's permission and the URLs' visibility.
  assert.match(en.imagePublicationRule.message, /only when “Allow publishing to MCP” is on for a save record and that save's image URL, page URL and frame URL are all visible to MCP\./);
});

test("the visit history note reads as one sentence, with and without the filter remark", async () => {
  const { t } = await import("../../extension/i18n.js");
  await withLocale("en", () => {
    const button = t("deleteUrlAll");
    assert.equal(t("urlHistoryNote", "3", t("urlHistoryNoteScope", "10"), "1", button),
      "Visits: 3 (matching the search and date filters, out of 10 visits for this URL). Snapshots (1) cannot be deleted one by one; “Delete everything for this URL” deletes them.");
    assert.equal(t("urlHistoryNote", "3", "", "1", button), "Visits: 3. Snapshots (1) cannot be deleted one by one; “Delete everything for this URL” deletes them.");
  });
  assert.equal(t("urlHistoryNote", "3件", t("urlHistoryNoteScope", "10"), "1件", t("deleteUrlAll")),
    "閲覧履歴 3件（このURL全10件のうち、検索・日付の条件に一致するもの）。スナップショット 1件は個別削除の対象外で、「URL全件削除」で削除されます。");
});

test("en: a search-based Page deletion names save records, which is what the host deletes", async () => {
  const { deletionModel: pageDeletion } = await import("../../extension/page-text.js");
  await withLocale("en", () => {
    const model = pageDeletion({ target: { scope: "filter", query: "news" }, counts: { events: 2, urls: 1, blobsRemoved: 1, blobsKept: 1, bytesRemoved: 10 } });
    assert.deepEqual(model.target.map((row) => row.value), ["Page save records matching the search", "news"]);
  });
});
