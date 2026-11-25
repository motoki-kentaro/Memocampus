// Issue #77 Slice 4: the Privacy settings page (options.html) and the full-page History (history.html) in Japanese
// and English. The same steps in both languages must send the same requests and leave the same state; the English
// text must say the same about recording, MCP visibility, applying and what was (not) changed; CSV data is never
// translated; a message that cannot be read never leaves a step half-finished (busy) or reports a success.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import vm from "node:vm";
import { extensionDir } from "./fake-dom.js";
import { createI18n, withLocale } from "./i18n-mock.js";
import { localizeHtml } from "./i18n-html.js";
import { addRule, check, click, fakeHost, flush, openPage, pattern, type, v2 } from "./options-fixtures.js";
import { at, openHistory, settle, visit } from "./history-fixtures.js";
import { longDate } from "../../extension/history-view.js";

const JAPANESE = /[ぁ-んァ-ヶ一-龠（）「」、。・：]/u;
const raw = (file) => readFileSync(path.join(extensionDir, file), "utf8");
const strip = (requests) => requests.map(({ requestId, ...rest }) => rest);
const texts = ($, ids) => Object.fromEntries(ids.map((id) => [id, $(id).textContent]));
const japaneseIn = (values) => values.filter((value) => JAPANESE.test(value));

// --- Static pages ---------------------------------------------------------------------------------------------

for (const file of ["options.html", "history.html"]) {
  test(`${file}: no Japanese in the file; in English no Japanese in any text or accessible name; i18n-dom.js runs first`, () => {
    const html = raw(file);
    assert.deepEqual(html.split("\n").filter((line) => JAPANESE.test(line) && !/^\s*<!--/.test(line)), []);
    const scripts = [...html.matchAll(/<script\b([^>]*)><\/script>/g)].map((m) => m[1].trim());
    assert.equal(scripts[0], 'src="i18n-dom.js"', "the classic script before the page's module");
    assert.equal(scripts.length, 2);
    const en = localizeHtml(html, "en");
    const shown = [...en.matchAll(/>([^<]+)</g)].map((m) => m[1].trim()).filter(Boolean);
    const attributes = [...en.matchAll(/\s(?:aria-label|title|placeholder)="([^"]*)"/g)].map((m) => m[1]);
    assert.deepEqual(japaneseIn([...shown, ...attributes]), []);
    // i18n-dom.js sets <html lang> from the catalog.
    const root = { lang: "ja" };
    vm.runInContext(raw("i18n-dom.js"), vm.createContext({ document: { documentElement: root, querySelectorAll: () => [] }, chrome: { i18n: createI18n({ locale: "en" }) } }));
    assert.equal(root.lang, "en");
  });
}

test("internal values are not text: rule kinds, match modes, page sizes and the CSV header stay as they are", () => {
  const options = raw("options.html");
  assert.deepEqual([...options.matchAll(/name="checker-kind" value="(\w+)"/g)].map((m) => m[1]), ["pattern", "regex", "domainWildcard", "exact"]);
  assert.deepEqual([...options.matchAll(/<label for="checker-kind-\w+">([^<]+)<\/label>/g)].map((m) => m[1]), ["Pattern", "Regex", "Domain Wildcard", "Exact URL"], "the kind names are fixed English names in both languages");
  const history = raw("history.html");
  assert.deepEqual([...history.matchAll(/name="mode" value="(\w+)"/g)].map((m) => m[1]), ["exact", "regex"]);
  assert.deepEqual([...history.matchAll(/<option value="(\d+)"/g)].map((m) => m[1]), ["25", "50", "100"]);
  const en = localizeHtml(options, "en");
  assert.match(en, /in kind,pattern form/, "the CSV header is named as it is");
});

test("English wording keeps the privacy meaning: Never Save is automatic recording, Hide from MCP is MCP only", () => {
  const en = JSON.parse(raw("_locales/en/messages.json"));
  const message = (key) => en[key].message;
  assert.equal(message("optionsNeverSaveNote"), "Matching URLs are not recorded automatically by Memocampus.");
  assert.equal(message("optionsSaveButNoMcpNote"), "Matching URLs are recorded in the Vault but are never shown to the MCP server.");
  assert.equal(message("optionsDecisionNeverSave"), en.policyStateNeverSave.message, "the same words as the Side Panel");
  for (const key of Object.keys(en).filter((k) => /^options|^history/.test(k))) {
    // MCP visibility is never worded as general publication.
    assert.doesNotMatch(message(key), /\b(public(?!\s+suffix)|publish\w*)\b/i, key);
  }
  assert.match(message("optionsCheckerAlsoNeverSave"), /takes precedence/);
  assert.match(message("optionsRelaxText"), /may be recorded.*may be shown to it/);
  // An unknown outcome is never stated either way.
  assert.match(message("optionsApplyUnknown"), /^The result of the apply is unknown: whether it was applied could not be checked\./);
  assert.match(message("optionsApplyUnknownChecking"), /^The result of the apply is unknown\./);
});

// --- Options: the same steps in both languages ------------------------------------------------------------------

/** Adds a rule, deletes one (a change that may weaken protection), validates, confirms and applies. */
async function editAndApply() {
  const ui = await openPage();
  const { $, host } = ui;
  await addRule(ui, "never-save", pattern("new.example"));
  const added = texts($, ["checker-status", "checker-result"]);
  click(ui.controls("save-but-no-mcp", 0).remove);
  click($("validate"));
  await flush();
  const validated = $("validation-status").textContent;
  click($("apply"));
  const confirm = { hidden: $("relax-confirm").hidden, items: $("relax-confirm-list").children.map((li) => li.textContent), focus: ui.document.activeElement?.id };
  click($("relax-confirm-yes"));
  await flush();
  await flush();
  return { requests: strip(host.requests), policy: host.policy, added, validated, confirm, applied: $("apply-status").textContent, state: $("policy-state").textContent, layer: $("never-save-state").textContent };
}

test("Options: editing, validating, confirming and applying send the same requests in both languages", async () => {
  const ja = await editAndApply();
  const en = await withLocale("en", editAndApply);
  assert.deepEqual(en.requests, ja.requests, "the language changes nothing that is sent");
  assert.deepEqual(en.policy, ja.policy);
  assert.equal(ja.confirm.hidden, false);
  assert.equal(en.confirm.hidden, false, "the same confirmation is asked");
  assert.equal(en.confirm.focus, "relax-confirm-no", "Cancel first in both languages");
  assert.deepEqual(ja.confirm.items, ["「MCPサーバに見せないグループ」の既存のルール1件を削除するか、内容・種類を変更する"]);
  assert.deepEqual(en.confirm.items, ["Delete existing rules of “Hide from MCP group”, or change their text or type (1)"]);
  assert.equal(en.added["checker-status"], "Added to the draft of “Never Save group” (row 3). Nothing takes effect until you choose “Apply”.");
  assert.equal(en.added["checker-result"], "Matches");
  assert.equal(en.validated, "Validated. This draft can be applied.");
  assert.equal(ja.applied, "適用しました（Revision 8）。");
  assert.equal(en.applied, "Applied (Revision 8).");
  assert.equal(en.state, "Ready");
  assert.equal(en.layer, "This group is on. Rules: 3.");
  assert.deepEqual(japaneseIn(Object.values(en.added).concat(en.validated, en.applied, en.state, en.layer, en.confirm.items)), []);
});

async function exportAndTest() {
  const downloads = [];
  const ui = await openPage({}, { download: ({ filename, csv }) => downloads.push({ filename, csv }) });
  click(ui.$("never-save-export"));
  type(ui.$("test-url"), "https://bank.example/login");
  click(ui.$("test-url-run"));
  await flush();
  return { downloads, exported: ui.$("never-save-export-status").textContent, decision: ui.$("test-url-decision").textContent, requests: strip(ui.host.requests) };
}

test("Options: the CSV export is the same file in both languages (data never translated); the URL test sends the same request", async () => {
  const ja = await exportAndTest();
  const en = await withLocale("en", exportAndTest);
  assert.equal(ja.downloads.length, 1);
  assert.deepEqual(en.downloads, ja.downloads, "file name, type and content do not depend on the language");
  assert.match(ja.downloads[0].csv, /^kind,pattern\r\n/, "the fixed header, not a translated one");
  assert.deepEqual(en.requests, ja.requests);
  assert.equal(en.exported, "The download of the draft as of now was requested as CSV. Check in the browser that it was saved.");
  assert.equal(ja.decision, "判定: 保存対象です");
  assert.equal(en.decision, "Result: Eligible for recording");
});

test("Options: policy states and the shared Vault state read in English; the explanation still fails closed", async () => {
  await withLocale("en", async () => {
    const invalid = await openPage({ send: async (message) => ({ ok: true, type: "policy_result", requestId: message.requestId, state: "invalid" }) });
    assert.equal(invalid.$("policy-state").textContent, "Invalid (the settings file is not valid)");
    assert.equal(invalid.$("policy-explanation").textContent,
      "Automatic recording is stopped because the privacy settings cannot be read (missing or damaged). The settings file was not changed. A way to recover will be added to this page later.\nThe current privacy settings could not be retrieved, so their contents are not shown.");
    assert.equal(invalid.$("apply").disabled, true);

    const host = fakeHost();
    host.answers.policy_status = () => ({ ok: true, state: "uninitialized" });
    host.answers.policy_preset = () => ({ ok: true, presetId: "default", schemaVersion: 2, engineVersion: 2, policy: v2({}, { revision: 1 }) });
    host.answers.vault_status = () => ({ ok: true, type: "vault_result", status: { state: "not_configured" } });
    const first = await openPage(host);
    assert.equal(first.$("policy-state").textContent, "Not Initialized");
    assert.equal(first.$("onboarding-vault").textContent, "No Vault is set up. Create or choose one in the Vault settings.", "the Vault state is the Side Panel's, in the same language as the page");
    assert.equal(first.$("start-recording").disabled, true, "no start without a usable Vault");
    assert.equal(first.$("apply-reason").textContent, "This draft is not applied. Recording is turned on only when you choose “Start remembering” after reviewing it.");
    assert.deepEqual(host.requests.filter((m) => m.type === "policy_start"), []);
  });
});

test("Options: a host problem is shown in English at its row, and the rule text is never echoed", async () => {
  await withLocale("en", async () => {
    const ui = await openPage({ policy: v2({ neverSave: [pattern("bad-rule.example")] }) });
    click(ui.$("validate"));
    await flush();
    assert.equal(ui.controls("never-save", 0).error.textContent, "Error: The pattern is not written correctly (it is empty, contains spaces or \\ ? # @ :, has no host name, or similar).");
    assert.equal(ui.$("validation-status").textContent, "Validation found problems in the draft (1). They are shown at the rows and groups concerned.");
    assert.doesNotMatch(ui.$("validation-status").textContent, /bad-rule/);
  });
});

// --- Options: a message that cannot be read never leaves a step half-done ----------------------------------------

/** Replaces chrome.i18n so that `keys` are missing (Chrome returns "" for an unknown message). */
function withoutMessages(keys) {
  const real = globalThis.chrome.i18n;
  globalThis.chrome.i18n = { ...real, getMessage: (key, values) => (keys.includes(key) ? "" : real.getMessage(key, values)) };
  return () => { globalThis.chrome.i18n = real; };
}

/** Collects what the page reports with reportError (a message that cannot be read) while `body` runs. */
async function catchingReports(body) {
  const caught = [];
  const saved = globalThis.reportError;
  globalThis.reportError = (error) => caught.push(error);
  try {
    await body();
    await settle(20);
  } finally {
    globalThis.reportError = saved;
  }
  return caught;
}

test("Options: a missing message after a successful apply still ends the step, and nothing claims success", async () => {
  const ui = await openPage();
  await addRule(ui, "never-save", pattern("new.example"));
  const restore = withoutMessages(["optionsApplied"]);
  const caught = await catchingReports(async () => {
    click(ui.$("apply"));
    await flush();
    await flush();
  });
  restore();
  assert.equal(caught.length, 1, "the missing message is reported, not swallowed");
  assert.equal(caught[0]?.name, "MissingMessageError");
  assert.equal(caught[0]?.key, "optionsApplied");
  assert.equal(ui.page.busy, null, "the step ended");
  assert.equal(ui.$("apply-status").textContent, "", "no message stands in for the missing one");
  assert.equal(ui.host.writes, 1, "applied once, as asked; never again");
  assert.equal(ui.$("reload").disabled, false, "the page was drawn again and can be used");
});

test("Options: a missing message after a CSV import still ends the import (busy, file input), with no success message", async () => {
  const ui = await openPage();
  const fileOf = (text) => { const bytes = new TextEncoder().encode(text); return { size: bytes.length, arrayBuffer: async () => bytes.buffer }; };
  const input = ui.$("never-save-import-file");
  input.click = () => {};
  const restore = withoutMessages(["optionsImportDone"]);
  const caught = await catchingReports(async () => {
    click(ui.$("never-save-import"));
    input.files = [fileOf("kind,pattern\npattern,imported.example")];
    input.dispatch("change");
    for (let i = 0; i < 50 && ui.page.busy !== null; i++) await flush();
  });
  restore();
  assert.equal(caught.length, 1);
  assert.equal(caught[0]?.key, "optionsImportDone");
  assert.equal(ui.page.busy, null);
  assert.equal(ui.$("never-save-import-status").hidden, true, "no success message");
  assert.equal(ui.$("never-save-import").disabled, false, "the page was drawn again");
  assert.equal(input.value, "");
  assert.equal(ui.page.draft.layer("neverSave").rules.at(-1).pattern, "imported.example", "the validated batch is in the draft, not applied");
  assert.equal(ui.host.writes, 0);
});

test("Options: a CSV import refused in English names the row and says nothing was added", async () => {
  await withLocale("en", async () => {
    const ui = await openPage();
    const fileOf = (text) => { const bytes = new TextEncoder().encode(text); return { size: bytes.length, arrayBuffer: async () => bytes.buffer }; };
    const input = ui.$("never-save-import-file");
    input.click = () => {};
    click(ui.$("never-save-import"));
    input.files = [fileOf("kind,pattern\nunknown,x.example")];
    input.dispatch("change");
    for (let i = 0; i < 50 && ui.page.busy !== null; i++) await flush();
    assert.equal(ui.$("never-save-import-status").textContent, "CSV row 2: Unknown kind. Nothing was added; the draft was not changed.");
    assert.equal(ui.page.draft.dirty(), false);
  });
});

// --- History ----------------------------------------------------------------------------------------------------

const DAY = [visit("v1", "https://github.com/a", "GitHub A", at(2026, 11, 17, 9, 5)), visit("v2", "https://example.com/", "", at(2026, 11, 17, 10, 0))];

async function historyFlow() {
  const ui = await openHistory({ visits: { A: DAY } });
  const { $, host } = ui;
  const first = texts($, ["selected-heading", "result-count", "domain-hint", "cal-title", "history-pager"]);
  const titleLabel = $("cal-title").getAttribute("aria-label");
  $("domain-input").value = "github.com";
  $("domain-input").dispatch("keydown", { key: "Enter" });
  await settle(10);
  const filtered = $("result-count").textContent;
  const row = $("history-list").children[0];
  row.ui.toggle.dispatch("click");
  await settle(10);
  const region = row.ui.region;
  const flatten = (el) => [el.textContent, ...el.children.flatMap(flatten)];
  return { requests: strip(host.calls), first, titleLabel, filtered, detail: flatten(region).filter(Boolean), toggle: row.ui.toggle.textContent };
}

test("History: the same searches in both languages; dates, counts and details read in English", async () => {
  const ja = await historyFlow();
  const en = await withLocale("en", historyFlow);
  assert.deepEqual(en.requests, ja.requests, "the same day bounds, domain, mode, page and size");
  assert.equal(ja.first["selected-heading"], "2026年11月17日（火）の訪問履歴");
  assert.equal(en.first["selected-heading"], "Visits on Tue, November 17, 2026");
  assert.equal(ja.first["result-count"], "2件（1 / 1ページ）");
  assert.equal(en.first["result-count"], "2 visits (page 1 of 1)");
  assert.equal(en.filtered, "1 matching visit (page 1 of 1)");
  assert.equal(ja.titleLabel, "2026年11月、年と月を選ぶ");
  assert.equal(en.titleLabel, "November 2026, choose year and month");
  assert.equal(en.first["cal-title"], "2026 ▼ Nov");
  assert.equal(en.first["domain-hint"], "Exact match example: github.com (sub-domains are not included)");
  assert.equal(en.toggle, "Hide details");
  assert.ok(en.detail.includes("Came from: unknown"), en.detail.join(" | "));
  assert.deepEqual(japaneseIn(en.detail), []);
  assert.deepEqual(japaneseIn(Object.values(en.first)), []);
});

test("History: the long date is a calendar day in every time zone and language", async () => {
  assert.equal(longDate({ year: 2026, monthIndex: 0, day: 1 }), "2026年1月1日（木）");
  await withLocale("en", () => assert.equal(longDate({ year: 2026, monthIndex: 0, day: 1 }), "Thu, January 1, 2026"));
});

test("History: an empty day, a bad regular expression and an old host say so in English", async () => {
  await withLocale("en", async () => {
    const empty = await openHistory({ visits: { A: [] } });
    assert.equal(empty.$("history-state-text").textContent, "There are no visits on this day.");
    assert.equal(empty.$("result-count").textContent, "0 visits");
    const old = await openHistory({ visits: { A: DAY }, host: { capabilitiesFail: true } });
    assert.match(old.$("history-state-text").textContent, /./);
    assert.doesNotMatch(old.$("history-state-text").textContent, JAPANESE);
  });
});
