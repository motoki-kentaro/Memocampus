// Editing, checking, testing and applying a draft on the Privacy settings page (#23 Slice 2), against a
// fake DOM and a fake host. Synthetic policies and URLs only. The fake host's answers are scripted:
// the page must show what the host says and judge nothing itself.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { createDocument, extensionDir, settle } from "./fake-dom.js";
import { SearchError } from "../../extension/search-client.js";
import { startOptionsPage } from "../../extension/options-page.js";
import { CATEGORY_KEYS, applyDraft, compareAfterApply, createDraft, readActive, relaxations } from "../../extension/options-state.js";

const html = readFileSync(path.join(extensionDir, "options.html"), "utf8");
/** The text of an element as options.html writes it (the fake DOM does not read the page). */
const htmlText = (id) => new RegExp(`id="${id}"[^>]*>([^<]*)<`).exec(html)?.[1] ?? "";

const allOn = () => Object.fromEntries(CATEGORY_KEYS.map((key) => [key, true]));
const pattern = (text) => ({ kind: "pattern", pattern: text });
const regex = (text) => ({ kind: "regex", pattern: text });
function v2(rules = {}, extra = {}) {
  return {
    schemaVersion: 2,
    engineVersion: 2,
    revision: 7,
    categories: allOn(),
    layers: {
      neverSave: { enabled: true, rules: rules.neverSave ?? [] },
      saveButNoMcp: { enabled: true, rules: rules.saveButNoMcp ?? [] },
    },
    ...extra,
  };
}
const RULES = { neverSave: [pattern("bank.example"), regex("^https://a\\.example/")], saveButNoMcp: [pattern("wiki.example/private")] };

/**
 * A host with a stored policy. `policy_apply` replaces it only for the stored ETag and revision, as
 * the real one does. `answers[type]` overrides a reply; `hold(type)` keeps the next reply of a type
 * until released. Every request is recorded.
 */
function fakeHost({ policy = v2(RULES), etag = "etag-7" } = {}) {
  const host = { requests: [], policy: structuredClone(policy), etag, answers: {}, holds: {}, writes: 0 };
  host.hold = (type) => {
    let release;
    const held = new Promise((resolve) => { release = resolve; });
    (host.holds[type] ??= []).push(held);
    return () => release();
  };
  host.ofType = (type) => host.requests.filter((message) => message.type === type);
  host.types = () => host.requests.map((message) => message.type);
  host.send = async (message) => {
    host.requests.push(structuredClone(message));
    const held = host.holds[message.type]?.shift();
    if (held) await held;
    const base = { type: `${message.type === "policy_status" ? "policy" : message.type}_result`, requestId: message.requestId };
    if (host.answers[message.type]) {
      const answer = await host.answers[message.type](message, host);
      return answer === undefined ? undefined : { ...base, ...answer };
    }
    const p = host.policy;
    const version = { etag: host.etag, revision: p.revision, schemaVersion: p.schemaVersion, engineVersion: p.engineVersion };
    switch (message.type) {
      case "policy_status":
        return { ok: true, ...base, state: "ready", ...version };
      case "policy_get":
        return { ok: true, ...base, state: "ready", ...version, policy: structuredClone(p) };
      case "policy_validate":
        return { ...base, ...validateAnswer(message.policy) };
      case "policy_test_rule":
        return { ok: true, ...base, matched: false };
      case "policy_test_url":
        return { ok: true, ...base, decision: "allow" };
      case "policy_apply": {
        if (message.expectedEtag !== host.etag) return { ok: false, ...base, error: "policy_conflict" };
        const checked = validateAnswer(message.policy);
        if (!checked.ok) return { ...base, ...checked };
        if (message.policy.revision !== p.revision) return { ok: false, ...base, error: "policy_conflict" };
        host.commit(message.policy);
        return { ok: true, ...base, state: "ready", etag: host.etag, revision: host.policy.revision, schemaVersion: 2, engineVersion: 2 };
      }
      default:
        return { ok: false, ...base, error: "unknown_type" };
    }
  };
  host.commit = (policy) => {
    host.policy = { ...structuredClone(policy), revision: host.policy.revision + 1 };
    host.etag = `etag-${host.policy.revision}`;
    host.writes += 1;
  };
  return host;
}

// The test's stand-in for the host's checks: an empty pattern is invalid. Only the fake uses this.
function validateAnswer(policy) {
  const errors = [];
  for (const layer of ["neverSave", "saveButNoMcp"]) {
    policy.layers[layer].rules.forEach((rule, ruleIndex) => {
      if (rule.pattern === "") errors.push({ layer, ruleIndex, code: rule.kind === "regex" ? "invalid_regex" : "invalid_pattern" });
    });
  }
  return errors.length ? { ok: false, error: "policy_invalid", errors } : { ok: true, schemaVersion: 2, engineVersion: 2 };
}

function fakeWindow() {
  const listeners = new Map();
  return {
    addEventListener: (type, listener) => listeners.set(type, [...(listeners.get(type) ?? []), listener]),
    dispatch(type) {
      const event = { type, defaultPrevented: false, returnValue: undefined, preventDefault() { this.defaultPrevented = true; } };
      for (const listener of listeners.get(type) ?? []) listener(event);
      return event;
    },
  };
}

async function openPage(hostOptions = {}) {
  const host = hostOptions.send ? hostOptions : fakeHost(hostOptions);
  const document = createDocument();
  const window = fakeWindow();
  const page = startOptionsPage({ document, window, send: host.send });
  await page.ready;
  const $ = (id) => document.querySelector(`#${id}`);
  const rowsOf = (layer) => $(`${layer}-rules`).children;
  const controls = (layer, index) => {
    const row = rowsOf(layer)[index];
    const [kind, text, remove, test] = row.children;
    return { row, kind: kind.children[0], input: text.children[0], error: text.children[1], remove: remove.children[0], test: test.children[0], result: test.children[1] };
  };
  return { page, host, document, window, $, rowsOf, controls };
}

function type(input, value) {
  input.value = value;
  input.dispatch("input");
}
function choose(select, value) {
  select.value = value;
  select.dispatch("change");
}
function check(input, checked) {
  input.checked = checked;
  input.dispatch("change");
}
const click = (element) => element.dispatch("click");
/** Lets every pending reply of the fake host arrive. */
const flush = () => settle(0);

/** Runs `body` with the console and every storage the page could use turned into traps. */
async function guarded(body) {
  const logged = [];
  const methods = ["log", "info", "warn", "error", "debug", "trace"];
  const saved = methods.map((method) => console[method]);
  for (const method of methods) console[method] = (...args) => logged.push(args);
  const savedStorage = Object.getOwnPropertyDescriptor(globalThis, "localStorage");
  Object.defineProperty(globalThis, "localStorage", { configurable: true, get() { throw new Error("localStorage used"); } });
  globalThis.chrome = { storage: new Proxy({}, { get() { throw new Error("chrome.storage used"); } }) };
  try {
    await body();
  } finally {
    methods.forEach((method, index) => { console[method] = saved[index]; });
    if (savedStorage) Object.defineProperty(globalThis, "localStorage", savedStorage);
    else delete globalThis.localStorage;
    delete globalThis.chrome;
  }
  assert.deepEqual(logged, [], "nothing is logged");
}

// --- Rule editing ---------------------------------------------------------------------------------

test("each group's rules are edited in their own table, in the draft only", async () => {
  const { page, host, $, rowsOf, controls, document } = await openPage();
  const active = structuredClone(page.view.active.policy);

  click($("never-save-add"));
  assert.equal(rowsOf("never-save").length, 3);
  assert.equal(document.activeElement, controls("never-save", 2).input, "the new rule's field takes focus");
  assert.deepEqual(page.draft.layer("neverSave").rules[2], { kind: "pattern", pattern: "" });
  assert.deepEqual(page.draft.layer("saveButNoMcp").rules, active.layers.saveButNoMcp.rules, "the other group is untouched");

  type(controls("never-save", 2).input, "new.example/path");
  choose(controls("never-save", 2).kind, "regex");
  assert.deepEqual(page.draft.layer("neverSave").rules[2], regex("new.example/path"), "the text is kept as written when the kind changes");
  choose(controls("never-save", 0).kind, "regex");
  assert.deepEqual(page.draft.layer("neverSave").rules[0], regex("bank.example"));
  choose(controls("never-save", 0).kind, "pattern");
  assert.deepEqual(page.draft.layer("neverSave").rules[0], pattern("bank.example"));

  click($("save-but-no-mcp-add"));
  type(controls("save-but-no-mcp", 1).input, "private.example");
  click(controls("save-but-no-mcp", 0).remove);
  assert.deepEqual(page.draft.layer("saveButNoMcp").rules, [pattern("private.example")]);
  assert.deepEqual(page.draft.layer("neverSave").rules, [pattern("bank.example"), regex("^https://a\\.example/"), regex("new.example/path")], "deleting a Private rule leaves the Blind ones");
  click(controls("never-save", 1).remove);
  assert.deepEqual(page.draft.layer("saveButNoMcp").rules, [pattern("private.example")], "deleting a Blind rule leaves the Private ones");
  assert.equal(document.activeElement, controls("never-save", 1).remove, "focus moves to the rule now in that place");
  click(controls("never-save", 1).remove);
  assert.equal(document.activeElement, controls("never-save", 0).remove, "then to the one before");

  assert.deepEqual(page.view.active.policy, active, "the active policy is unchanged");
  assert.equal(Object.isFrozen(page.view.active.policy.layers.neverSave.rules), true);
  assert.deepEqual(host.policy.layers, active.layers, "the host's policy is unchanged");
  assert.deepEqual(host.types(), ["policy_status", "policy_get"], "editing sends nothing");
  assert.equal($("dirty-status").textContent, "未適用の変更があります");

  click($("discard"));
  assert.deepEqual(page.draft.policy, active);
  assert.deepEqual(rowsOf("never-save").map((row) => row.children[1].children[0].value), ["bank.example", "^https://a\\.example/"]);
  assert.equal($("dirty-status").textContent, "変更を破棄しました。");
});

test("dirty follows the difference, and deleting the only rule leaves the add button in focus", async () => {
  const { page, $, controls, document } = await openPage({ policy: v2({ saveButNoMcp: [pattern("one.example")] }) });
  type(controls("save-but-no-mcp", 0).input, "two.example");
  assert.equal(page.draft.dirty(), true);
  type(controls("save-but-no-mcp", 0).input, "one.example");
  assert.equal(page.draft.dirty(), false, "typing the old text back is no change");
  click(controls("save-but-no-mcp", 0).remove);
  assert.equal(document.activeElement, $("save-but-no-mcp-add"));
  assert.equal($("save-but-no-mcp-table").hidden, true);
  assert.equal($("save-but-no-mcp-empty").textContent, "ルールはありません。");
  click($("save-but-no-mcp-add"));
  type(document.activeElement, "one.example");
  assert.equal(page.draft.dirty(), false, "the same rule added again is no change");
});

test("typing keeps the rows: no table is built again and the field keeps focus", async () => {
  const many = Array.from({ length: 100 }, (_, index) => pattern(`site${index}.example`));
  const { page, $, rowsOf, controls, document } = await openPage({ policy: v2({ neverSave: many }) });
  const before = [...rowsOf("never-save")];
  const field = controls("never-save", 50).input;
  field.focus();
  for (const text of ["s", "si", "sit", "site", "site50.example/x"]) type(field, text);
  choose(controls("never-save", 50).kind, "regex");
  check($("never-save-enabled"), false);
  check($("category-webmail"), false);
  assert.deepEqual(rowsOf("never-save"), before, "the same rows");
  assert.equal(document.activeElement, field);
  assert.equal(field.value, "site50.example/x");
  assert.deepEqual(page.draft.layer("neverSave").rules[50], regex("site50.example/x"));
});

test("turning a group off keeps its rules and warns; it changes the draft only", async () => {
  const { page, host, $ } = await openPage();
  check($("save-but-no-mcp-enabled"), false);
  assert.equal(page.draft.layer("saveButNoMcp").enabled, false);
  assert.deepEqual(page.draft.layer("saveButNoMcp").rules, RULES.saveButNoMcp, "the rules stay");
  assert.equal(page.view.active.policy.layers.saveButNoMcp.enabled, true);
  assert.equal($("save-but-no-mcp-warning").hidden, false);
  assert.equal($("never-save-warning").hidden, true, "the other group is untouched");
  assert.match($("save-but-no-mcp-state").textContent, /無効です/);
  assert.match($("category-webmail-note").textContent, /無効のため/);
  check($("save-but-no-mcp-enabled"), true);
  assert.equal($("save-but-no-mcp-warning").hidden, true);
  assert.equal(page.draft.dirty(), false);
  assert.deepEqual(host.types(), ["policy_status", "policy_get"]);
});

test("a draft that cannot be edited takes no change through any control or the model", async () => {
  const v1 = { ...v2(), schemaVersion: 1, engineVersion: 1, layers: { neverSave: { enabled: true, rules: ["old.example"] }, saveButNoMcp: { enabled: true, rules: [] } } };
  const { page, host, $ } = await openPage({ policy: v1, etag: "e1" });
  check($("never-save-enabled"), false);
  click($("never-save-add"));
  click($("validate"));
  click($("apply"));
  await flush();
  assert.equal(page.draft.dirty(), false);
  assert.equal(page.draft.addRule("neverSave"), -1);
  assert.equal(page.draft.setLayerEnabled("neverSave", false), false);
  assert.equal(page.draft.setRule("neverSave", 0, { pattern: "x" }), false);
  assert.equal(page.draft.deleteRule("neverSave", 0), false);
  assert.deepEqual(host.types(), ["policy_status", "policy_get"], "nothing validates, applies or migrates");
  assert.equal($("never-save-enabled").checked, true);
});

test("the draft model refuses what is not a rule", () => {
  const active = readActive({ etag: "e", revision: 7, schemaVersion: 2, engineVersion: 2, policy: v2(RULES) });
  const draft = createDraft(active, { editable: true });
  assert.equal(draft.setRule("neverSave", 0, { kind: "glob" }), false);
  assert.equal(draft.setRule("neverSave", 0, { pattern: 1 }), false);
  assert.equal(draft.setRule("neverSave", 9, { pattern: "x" }), false);
  assert.equal(draft.setRule("other", 0, { pattern: "x" }), false);
  assert.equal(draft.deleteRule("neverSave", -1), false);
  assert.equal(draft.addRule("other"), -1);
  assert.equal(draft.version, 0, "a refused change is no change");
  draft.setRule("neverSave", 0, { pattern: "bank.example" });
  assert.equal(draft.version, 0, "nor is setting the same text");
  draft.addRule("saveButNoMcp");
  assert.equal(draft.version, 1);
  assert.deepEqual(active.policy, v2(RULES));
});

// --- Validation -----------------------------------------------------------------------------------

test("checking a valid draft says so and writes nothing", async () => {
  const { host, $, controls } = await openPage();
  type(controls("never-save", 0).input, "bank2.example");
  click($("validate"));
  assert.equal($("validation-status").textContent, "検証しています…");
  assert.equal($("validate").disabled, true, "one check at a time");
  await flush();
  assert.equal($("validation-status").textContent, "検証しました。この下書きは適用できます。");
  assert.deepEqual(host.types(), ["policy_status", "policy_get", "policy_validate"]);
  assert.equal(host.ofType("policy_validate")[0].policy.layers.neverSave.rules[0].pattern, "bank2.example", "the whole draft is sent");
  assert.equal(host.writes, 0);
});

test("problems are shown on their own row and group, and never on another", async () => {
  const policy = v2({ neverSave: [pattern("a.example"), pattern("b.example"), regex("c")], saveButNoMcp: [pattern("d.example")] });
  policy.layers.saveButNoMcp.enabled = false;
  const { host, $, controls } = await openPage({ policy });
  host.answers.policy_validate = () => ({
    ok: false,
    error: "policy_invalid",
    errors: [
      { layer: "neverSave", ruleIndex: 1, code: "invalid_regex" },
      { layer: "saveButNoMcp", ruleIndex: 0, code: "invalid_pattern" },
      { layer: "saveButNoMcp", code: "too_many_rules" },
      { code: "too_many_regex_rules" },
      { layer: "neverSave", ruleIndex: 9, code: "invalid_pattern" },
      { layer: "elsewhere", ruleIndex: 0, code: "invalid_pattern" },
    ],
  });
  type(controls("never-save", 1).input, "b2.example");
  click($("validate"));
  await flush();
  assert.equal(host.ofType("policy_validate")[0].policy.layers.saveButNoMcp.rules.length, 1, "a disabled group's rules are checked too");
  assert.equal(controls("never-save", 0).error.hidden, true);
  assert.equal(controls("never-save", 2).error.hidden, true, "nor on the row after it");
  assert.equal(controls("never-save", 1).error.hidden, false);
  assert.equal(controls("never-save", 1).error.textContent, "エラー: 正規表現の書き方が正しくありません。");
  assert.equal(controls("never-save", 1).input.getAttribute("aria-invalid"), "true", "marked for assistive technology, not by colour only");
  assert.equal(controls("never-save", 2).input.getAttribute("aria-invalid"), null);
  assert.match(controls("save-but-no-mcp", 0).error.textContent, /^エラー: パターンの書き方/, "a rule of a disabled group is shown too");
  assert.equal($("save-but-no-mcp-error").hidden, false);
  assert.match($("save-but-no-mcp-error").textContent, /上限1,000件/);
  assert.equal($("never-save-error").hidden, true);
  const summary = $("validation-status").textContent;
  assert.match(summary, /下書きに問題があります（6件）/);
  assert.match(summary, /正規表現ルールが多すぎます/, "a problem of no place is in the summary");
  assert.equal((summary.match(/パターンの書き方/g) ?? []).length, 2, "places this page does not have go to the summary, not to a row");
  for (const text of ["a.example", "b2.example", "d.example"]) assert.ok(!summary.includes(text));

  type(controls("never-save", 0).input, "a2.example");
  assert.equal(controls("never-save", 1).error.hidden, true, "a change clears what was found for the earlier draft");
  assert.equal($("save-but-no-mcp-error").hidden, true);
  assert.equal($("validation-status").hidden, true);
});

test("an answer about an earlier draft is not shown, and only the latest check counts", async () => {
  const { host, $, controls } = await openPage();
  host.answers.policy_validate = () => ({ ok: false, error: "policy_invalid", errors: [{ layer: "neverSave", ruleIndex: 0, code: "invalid_pattern" }] });
  const release = host.hold("policy_validate");
  click($("validate"));
  type(controls("never-save", 0).input, "fixed.example");
  release();
  await flush();
  assert.equal(controls("never-save", 0).error.hidden, true, "the answer was about the text before the edit");
  assert.match($("validation-status").textContent, /検証中に下書きが変更された/);
  assert.equal($("validate").disabled, false);

  // A reload during a check: the old check's answer is dropped.
  const releaseAgain = host.hold("policy_validate");
  click($("validate"));
  click($("reload"));
  click($("reload-confirm-yes"));
  await flush();
  releaseAgain();
  await flush();
  assert.equal(controls("never-save", 0).error.hidden, true);
  assert.equal($("validation-status").hidden, true);
});

test("a check that gets no answer says so and places nothing", async () => {
  const { host, $, controls } = await openPage();
  host.answers.policy_validate = () => { throw new SearchError("timeout"); };
  type(controls("never-save", 0).input, "x.example");
  click($("validate"));
  await flush();
  assert.match($("validation-status").textContent, /^検証できませんでした。Native Hostから応答がありません/);
  assert.equal(controls("never-save", 0).error.hidden, true);
});

// --- Rule test ------------------------------------------------------------------------------------

test("a rule is tested alone by the host with the shared URL, and the answer is a match only", async () => {
  const { host, $, controls } = await openPage();
  click(controls("never-save", 0).test);
  assert.equal(controls("never-save", 0).result.textContent, "URLテスト欄にURLを入力してください。");
  assert.deepEqual(host.ofType("policy_test_rule"), [], "nothing is sent without a URL");

  type($("test-url"), "  https://Bank.Example/login?q=1  ");
  // The host's answer is shown as it is, whatever the page might think of the rule.
  host.answers.policy_test_rule = (message) => ({ ok: true, matched: message.rule.kind === "regex" });
  click(controls("never-save", 0).test);
  click(controls("never-save", 1).test);
  assert.equal(controls("never-save", 0).result.textContent, "テスト中…");
  await flush();
  assert.deepEqual(host.ofType("policy_test_rule").map(({ rule, url }) => ({ rule, url })), [
    { rule: pattern("bank.example"), url: "https://Bank.Example/login?q=1" },
    { rule: regex("^https://a\\.example/"), url: "https://Bank.Example/login?q=1" },
  ]);
  assert.equal(controls("never-save", 0).result.textContent, "一致しませんでした", "the host said no, though the text looks alike");
  assert.equal(controls("never-save", 1).result.textContent, "一致しました");
  assert.equal(controls("never-save", 1).result.dataset.result, "match");
  assert.match(htmlText("test-url-note"), /単体.*一致しなくても.*保存されないことがあります/);

  host.answers.policy_test_rule = () => ({ ok: false, error: "invalid_url" });
  click(controls("save-but-no-mcp", 0).test);
  await flush();
  assert.equal(controls("save-but-no-mcp", 0).result.textContent, "テストできませんでした。このURLは判定に使えません（長すぎるなど）。");
  assert.equal(host.writes, 0);
});

test("a rule test answer about an earlier rule or URL is dropped", async () => {
  const { host, $, controls } = await openPage();
  host.answers.policy_test_rule = () => ({ ok: true, matched: true });
  type($("test-url"), "https://bank.example/");
  let release = host.hold("policy_test_rule");
  click(controls("never-save", 0).test);
  type(controls("never-save", 0).input, "other.example");
  release();
  await flush();
  assert.equal(controls("never-save", 0).result.textContent, "", "the rule changed meanwhile");

  click(controls("never-save", 0).test);
  await flush();
  assert.equal(controls("never-save", 0).result.textContent, "一致しました");
  type($("test-url"), "https://other.example/");
  assert.equal(controls("never-save", 0).result.textContent, "", "a new URL clears the old answers");

  release = host.hold("policy_test_rule");
  click(controls("never-save", 0).test);
  type($("test-url"), "https://third.example/");
  release();
  await flush();
  assert.equal(controls("never-save", 0).result.textContent, "", "the URL changed meanwhile");
});

// --- URL test -------------------------------------------------------------------------------------

test("the draft decides a URL in the host, and the page says it is a test of the draft", async () => {
  const { host, $, controls } = await openPage();
  click($("test-url-run"));
  assert.equal($("test-url-decision").textContent, "判定するURLを入力してください。");
  assert.deepEqual(host.ofType("policy_test_url"), []);

  type(controls("save-but-no-mcp", 0).input, "wiki.example");
  type($("test-url"), "https://accounts.google.com/");
  for (const [decision, text] of [
    ["never_save", "判定: 保存しません"],
    ["save_but_no_mcp", "判定: Vaultに保存しますが、MCPには見せません"],
    ["allow", "判定: 保存対象です"],
  ]) {
    host.answers.policy_test_url = () => ({ ok: true, decision });
    click($("test-url-run"));
    await flush();
    assert.equal($("test-url-decision").textContent, text);
    assert.equal($("test-url-decision").dataset.decision, decision);
    assert.equal($("test-url-caveat").hidden, false);
    assert.equal(htmlText("test-url-caveat"), "これは編集中の設定によるテスト結果です。現在適用されている設定とは異なる場合があります。");
  }
  const sent = host.ofType("policy_test_url").at(-1);
  assert.equal(sent.url, "https://accounts.google.com/");
  assert.deepEqual(sent.policy.layers.saveButNoMcp.rules, [pattern("wiki.example")], "the draft, not the active policy");
  assert.equal(host.writes, 0, "a decision is no permission and stores nothing");
});

test("an invalid draft gets no decision, and the page does not make one up", async () => {
  const { host, $ } = await openPage();
  host.answers.policy_test_url = () => ({ ok: false, error: "policy_invalid", errors: [{ layer: "neverSave", ruleIndex: 0, code: "invalid_pattern" }] });
  type($("test-url"), "https://bank.example/");
  click($("test-url-run"));
  await flush();
  assert.match($("test-url-decision").textContent, /下書きに問題があるため判定できませんでした/);
  assert.equal($("test-url-decision").dataset.decision, "");
  assert.equal($("test-url-caveat").hidden, true);
  host.answers.policy_test_url = () => ({ ok: true, decision: "maybe" });
  click($("test-url-run"));
  await flush();
  assert.match($("test-url-decision").textContent, /判定できませんでした。Native Hostから不正な応答/);
});

test("a URL decision about an earlier draft or URL, or overtaken by a later one, is dropped", async () => {
  const { host, $, controls } = await openPage();
  host.answers.policy_test_url = (message) => ({ ok: true, decision: message.url.includes("first") ? "never_save" : "allow" });
  type($("test-url"), "https://first.example/");
  let release = host.hold("policy_test_url");
  click($("test-url-run"));
  type(controls("never-save", 0).input, "changed.example");
  release();
  await flush();
  assert.equal($("test-url-result").hidden, true, "the draft changed meanwhile");

  release = host.hold("policy_test_url");
  click($("test-url-run"));
  type($("test-url"), "https://second.example/");
  release();
  await flush();
  assert.equal($("test-url-result").hidden, true, "the URL changed meanwhile");

  // Two runs for the same draft and URL: the first answers last and is dropped.
  release = host.hold("policy_test_url");
  host.answers.policy_test_url = () => ({ ok: true, decision: host.ofType("policy_test_url").length === 1 ? "never_save" : "save_but_no_mcp" });
  click($("test-url-run"));
  click($("test-url-run"));
  await flush();
  release();
  await flush();
  assert.equal($("test-url-decision").dataset.decision, "save_but_no_mcp");
});

// --- Apply ----------------------------------------------------------------------------------------

test("applying checks the draft, replaces the policy of the ETag read, and reads it again", async () => {
  const { page, host, $, controls, document } = await openPage();
  click($("never-save-add"));
  type(controls("never-save", 2).input, "added.example");
  assert.equal($("apply").disabled, false);
  click($("apply"));
  assert.equal($("relax-confirm").hidden, true, "adding a rule protects more: no confirmation");
  await flush();
  assert.deepEqual(host.types(), ["policy_status", "policy_get", "policy_validate", "policy_apply", "policy_status", "policy_get"]);
  const sent = host.ofType("policy_apply")[0];
  assert.equal(sent.expectedEtag, "etag-7");
  assert.equal(sent.policy.revision, 7, "the draft names the revision it was made from");
  assert.deepEqual(sent.policy.layers.neverSave.rules.at(-1), pattern("added.example"));
  assert.equal(host.policy.revision, 8);
  assert.equal(page.view.active.revision, 8);
  assert.equal(page.view.active.etag, "etag-8");
  assert.equal($("policy-revision").textContent, "8");
  assert.equal(page.draft.dirty(), false);
  assert.equal($("dirty-status").textContent, "");
  assert.equal($("apply-status").textContent, "適用しました（Revision 8）。");
  assert.equal(document.activeElement, $("apply-status"));
  assert.equal($("apply").disabled, true);
  assert.equal(controls("never-save", 2).input.value, "added.example", "the rows show the policy read again");
});

test("an invalid draft is not applied, and its problems are shown", async () => {
  const { page, host, $, controls } = await openPage();
  click($("save-but-no-mcp-add"));
  click($("apply"));
  await flush();
  assert.deepEqual(host.ofType("policy_apply"), [], "the empty rule fails the check first");
  assert.equal($("apply-status").textContent, "下書きに問題があるため、適用していません。");
  assert.match(controls("save-but-no-mcp", 1).error.textContent, /パターンの書き方/);
  assert.equal(page.draft.dirty(), true, "the draft stays");
  assert.equal($("apply").disabled, false, "it can be fixed and applied");

  host.answers.policy_validate = () => { throw new SearchError("host_unavailable"); };
  type(controls("save-but-no-mcp", 1).input, "ok.example");
  click($("apply"));
  await flush();
  assert.deepEqual(host.ofType("policy_apply"), [], "no check, no apply");
  assert.match($("apply-status").textContent, /^下書きを検証できなかったため、適用していません。/);
});

test("a host that refuses the draft at apply leaves it and says why", async () => {
  for (const [error, text] of [
    ["policy_version_mismatch", /バージョンが一致しません/],
    ["invalid_etag", /ETag/],
    ["policy_draft_too_large", /上限1 MiB/],
  ]) {
    const { page, host, $, controls } = await openPage();
    host.answers.policy_apply = () => ({ ok: false, error });
    click($("never-save-add"));
    type(controls("never-save", 2).input, "x.example");
    click($("apply"));
    await flush();
    assert.match($("apply-status").textContent, text, error);
    assert.equal(host.ofType("policy_apply").length, 1);
    assert.deepEqual(host.types().slice(-1), ["policy_apply"], "nothing is read: the host answered");
    assert.equal(page.draft.dirty(), true);
    assert.equal(page.blocked, null);
  }
});

test("a conflict keeps the draft, never overwrites, and waits for a reload", async () => {
  const { page, host, $, controls, window } = await openPage();
  click($("never-save-add"));
  type(controls("never-save", 2).input, "mine.example");
  // Someone else applies first.
  host.commit({ ...host.policy, categories: { ...host.policy.categories, aiChats: false } });
  click($("apply"));
  await flush();
  assert.equal(host.ofType("policy_apply").length, 1);
  assert.equal(host.policy.categories.aiChats, false, "the other change stays");
  assert.deepEqual(host.policy.layers.neverSave.rules, RULES.neverSave, "nothing of the draft was written");
  assert.match($("apply-status").textContent, /ほかの画面またはプロセス.*自動では上書きしません.*下書きはそのまま残しています/);
  assert.equal(page.blocked, "conflict");
  assert.deepEqual(page.draft.layer("neverSave").rules.at(-1), pattern("mine.example"));
  assert.equal(controls("never-save", 2).input.value, "mine.example");
  assert.equal($("apply").disabled, true);
  assert.match($("apply-reason").textContent, /再読み込みするまで/);
  click($("apply"));
  await flush();
  assert.equal(host.ofType("policy_apply").length, 1, "not again with the old ETag");
  assert.equal(window.dispatch("beforeunload").defaultPrevented, true, "leaving still asks");

  click($("reload"));
  assert.equal($("reload-confirm").hidden, false, "reloading asks, since it drops the draft");
  click($("reload-confirm-yes"));
  await flush();
  assert.equal(page.view.active.etag, "etag-8");
  assert.equal(page.blocked, null);
  assert.equal($("category-aiChats").checked, false, "the other change is shown");
});

test("one click applies once: everything that could change the draft waits", async () => {
  const { page, host, $, controls, window } = await openPage();
  click($("never-save-add"));
  type(controls("never-save", 2).input, "once.example");
  const release = host.hold("policy_apply");
  click($("apply"));
  await flush();
  assert.equal(page.busy, "applying");
  click($("apply"));
  click($("apply"));
  for (const id of ["apply", "discard", "reload", "validate", "never-save-add", "save-but-no-mcp-add", "never-save-enabled", "category-webmail", "test-url", "test-url-run"]) {
    assert.equal($(id).disabled, true, id);
  }
  for (const control of ["kind", "input", "remove", "test"]) assert.equal(controls("never-save", 0)[control].disabled, true, control);
  // Whatever still reaches the page changes nothing.
  type(controls("never-save", 0).input, "late.example");
  check($("category-webmail"), false);
  click($("never-save-add"));
  click($("discard"));
  click($("reload"));
  assert.equal(window.dispatch("beforeunload").defaultPrevented, true, "leaving asks while applying");
  release();
  await flush();
  assert.equal(host.ofType("policy_apply").length, 1);
  assert.equal(host.ofType("policy_validate").length, 1);
  assert.deepEqual(host.policy.layers.neverSave.rules.map((rule) => rule.pattern), ["bank.example", "^https://a\\.example/", "once.example"]);
  assert.equal(host.policy.categories.webmail, true);
  assert.equal(page.draft.dirty(), false);

  // Nor while the applied policy is being read back.
  click($("never-save-add"));
  type(controls("never-save", 3).input, "twice.example");
  const releaseRead = host.hold("policy_status");
  click($("apply"));
  await flush();
  assert.equal(page.busy, "checking");
  click($("apply"));
  click($("apply-recheck"));
  releaseRead();
  await flush();
  assert.equal(host.ofType("policy_apply").length, 2, "one more click, one more apply");
});

test("applied, then the policy cannot be read: no failure is claimed and nothing is applied again", async () => {
  const { page, host, $, controls } = await openPage();
  click($("never-save-add"));
  type(controls("never-save", 2).input, "kept.example");
  host.answers.policy_status = (message, self) => {
    if (self.ofType("policy_apply").length && !self.statusAgain) throw new SearchError("timeout");
    return { ok: true, state: "ready", etag: self.etag, revision: self.policy.revision, schemaVersion: 2, engineVersion: 2 };
  };
  click($("apply"));
  await flush();
  assert.equal(host.policy.revision, 8, "the host did apply");
  const text = $("apply-status").textContent;
  assert.match(text, /^適用は成功しましたが（Revision 8）、最新状態を取得できませんでした/);
  assert.doesNotMatch(text, /失敗しました/);
  assert.equal(page.blocked, "applied");
  assert.equal(page.draft.dirty(), true, "the draft is not dropped");
  assert.equal($("apply").disabled, true);
  assert.equal($("apply-recheck").hidden, false);
  click($("apply"));
  await flush();
  assert.equal(host.ofType("policy_apply").length, 1);

  host.statusAgain = true;
  click($("apply-recheck"));
  await flush();
  assert.equal(page.view.active.etag, "etag-8");
  assert.equal(page.draft.dirty(), false);
  assert.equal($("apply-recheck").hidden, true);
  assert.equal($("apply-status").textContent, "適用しました（Revision 8）。");
  assert.equal(host.ofType("policy_apply").length, 1);
});

test("an apply whose answer is lost after the host stored it is found applied by reading", async () => {
  const { page, host, $, controls } = await openPage();
  click($("never-save-add"));
  type(controls("never-save", 2).input, "lost.example");
  host.answers.policy_apply = (message, self) => {
    self.commit(message.policy);
    throw new SearchError("timeout");
  };
  click($("apply"));
  await flush();
  assert.equal(host.ofType("policy_apply").length, 1, "never sent again by itself");
  assert.deepEqual(host.types().slice(-3), ["policy_apply", "policy_status", "policy_get"]);
  assert.equal($("apply-status").textContent, "適用されたことを確認しました（Revision 8）。");
  assert.equal(page.view.active.revision, 8);
  assert.equal(page.draft.dirty(), false);
});

test("an apply whose outcome is unknown and found not applied is not sent again by itself", async () => {
  for (const lost of [() => { throw new SearchError("timeout"); }, () => { throw new Error("disconnected"); }, () => undefined, () => ({ ok: true, state: "ready" })]) {
    const { page, host, $, controls } = await openPage();
    click($("never-save-add"));
    type(controls("never-save", 2).input, "maybe.example");
    host.answers.policy_apply = lost;
    click($("apply"));
    await flush();
    assert.equal(host.ofType("policy_apply").length, 1);
    assert.match($("apply-status").textContent, /適用前のままでした（.+）。自動ではもう一度適用しません。下書きは残しています/);
    assert.equal(page.draft.dirty(), true);
    assert.equal(page.blocked, null, "the stored policy is the one the draft was made from, so the ETag still guards an explicit retry");
    await flush();
    assert.equal(host.ofType("policy_apply").length, 1, "no retry");
    delete host.answers.policy_apply;
    click($("apply"));
    await flush();
    assert.equal(host.ofType("policy_apply").length, 2, "only when asked again");
    assert.equal(host.policy.revision, 8);
  }
});

test("an apply whose outcome cannot be checked blocks applying until it is checked", async () => {
  const { page, host, $, controls } = await openPage();
  click($("never-save-add"));
  type(controls("never-save", 2).input, "unknown.example");
  host.answers.policy_apply = (message, self) => {
    self.commit(message.policy);
    throw new SearchError("host_unavailable");
  };
  host.answers.policy_status = () => { throw new SearchError("host_unavailable"); };
  click($("apply"));
  await flush();
  const text = $("apply-status").textContent;
  assert.match(text, /適用の結果が不明です。適用されたかどうかを確認できませんでした/);
  assert.doesNotMatch(text, /失敗しました|適用しました/);
  assert.equal(page.blocked, "unknown");
  assert.equal($("apply").disabled, true);
  assert.equal(page.draft.dirty(), true);
  delete host.answers.policy_status;
  click($("apply-recheck"));
  await flush();
  assert.equal($("apply-status").textContent, "適用されたことを確認しました（Revision 8）。");
  assert.equal(host.ofType("policy_apply").length, 1);
});

test("an apply whose outcome is unknown while another policy was stored is not taken as applied", async () => {
  const { page, host, $, controls } = await openPage();
  click($("never-save-add"));
  type(controls("never-save", 2).input, "mine.example");
  host.answers.policy_apply = (message, self) => {
    self.commit({ ...self.policy, categories: { ...self.policy.categories, webmail: false } });
    throw new SearchError("timeout");
  };
  click($("apply"));
  await flush();
  assert.match($("apply-status").textContent, /この下書きとは異なる内容に変わっていました（Revision 8）。自動では上書きしません/);
  assert.equal(page.blocked, "changed");
  assert.equal(page.view.active.revision, 7, "the draft stays against the policy it was made from");
  assert.equal($("apply").disabled, true);
  assert.equal(host.ofType("policy_apply").length, 1);
});

test("the comparison after a lost answer needs the exact draft at the next revision", () => {
  const submitted = v2(RULES);
  const at = (policy, revision, etag) => ({ etag, revision, policy: { ...policy, revision } });
  assert.equal(compareAfterApply({ expectedEtag: "e7", submitted, loaded: at(submitted, 7, "e7") }), "not_applied");
  assert.equal(compareAfterApply({ expectedEtag: "e7", submitted, loaded: at(submitted, 8, "e8") }), "applied");
  assert.equal(compareAfterApply({ expectedEtag: "e7", submitted, loaded: at(submitted, 9, "e9") }), "changed", "another apply came between");
  assert.equal(compareAfterApply({ expectedEtag: "e7", submitted, loaded: at({ ...submitted, categories: { ...allOn(), webmail: false } }, 8, "e8") }), "changed");
});

test("only an answer from the host after which nothing was written counts as refused", async () => {
  const reply = (fields) => async (message) => ({ type: "policy_apply_result", requestId: message.requestId, ...fields });
  const outcome = async (send) => (await applyDraft({ send, expectedEtag: "e", policy: v2() })).outcome;
  assert.equal(await outcome(reply({ ok: true, state: "ready", etag: "f", revision: 8, schemaVersion: 2, engineVersion: 2 })), "applied");
  for (const error of ["policy_conflict", "invalid_etag", "policy_version_mismatch", "invalid_policy_draft", "policy_draft_too_large", "unknown_type"]) {
    assert.equal(await outcome(reply({ ok: false, error })), "refused", error);
  }
  assert.equal(await outcome(reply({ ok: false, error: "policy_invalid", errors: [{ layer: "neverSave", ruleIndex: 0, code: "invalid_pattern" }] })), "refused");
  for (const error of ["policy_busy", "policy_unavailable", "policy_invalid", "db_error"]) {
    assert.equal(await outcome(reply({ ok: false, error })), "unknown", `${error} may follow the write`);
  }
  assert.equal(await outcome(reply({ ok: true, state: "ready", etag: "", revision: 8 })), "unknown");
  assert.equal(await outcome(async () => { throw new SearchError("timeout"); }), "unknown");
  assert.equal(await outcome(async (message) => ({ ok: true, type: "policy_apply_result", requestId: `${message.requestId}x` })), "unknown");
});

// --- Privacy relaxation ---------------------------------------------------------------------------

const relaxingEdits = {
  "a Blind rule removed": ({ controls }) => click(controls("never-save", 0).remove),
  "a Blind rule's text changed": ({ controls }) => type(controls("never-save", 0).input, "bank.example/only"),
  "a Blind rule's kind changed": ({ controls }) => choose(controls("never-save", 0).kind, "regex"),
  "the Blind group disabled": ({ $ }) => check($("never-save-enabled"), false),
  "a category turned off": ({ $ }) => check($("category-credentials"), false),
  "a Private rule removed": ({ controls }) => click(controls("save-but-no-mcp", 0).remove),
  "the Private group disabled": ({ $ }) => check($("save-but-no-mcp-enabled"), false),
};

for (const [name, edit] of Object.entries(relaxingEdits)) {
  test(`${name}: applying asks first, and cancelling keeps the draft and sends nothing`, async () => {
    const opened = await openPage();
    const { page, host, $, document } = opened;
    edit(opened);
    const before = page.draft.policy;
    click($("apply"));
    assert.equal($("relax-confirm").hidden, false);
    assert.equal(document.activeElement, $("relax-confirm-no"), "cancel has focus first");
    assert.ok($("relax-confirm-list").children.length >= 1);
    for (const item of $("relax-confirm-list").children) assert.doesNotMatch(item.textContent, /bank\.example|wiki\.example/, "no rule text");
    await flush();
    assert.deepEqual(host.types(), ["policy_status", "policy_get"], "nothing before the answer");
    if (name.includes("removed")) $("relax-confirm").dispatch("keydown", { key: "Escape" });
    else click($("relax-confirm-no"));
    assert.equal($("relax-confirm").hidden, true);
    assert.equal(document.activeElement, $("apply"));
    assert.deepEqual(page.draft.policy, before);
    await flush();
    assert.deepEqual(host.types(), ["policy_status", "policy_get"]);

    click($("apply"));
    click($("relax-confirm-yes"));
    await flush();
    assert.deepEqual(host.types().slice(2), ["policy_validate", "policy_apply", "policy_status", "policy_get"]);
    assert.equal(page.draft.dirty(), false);
  });
}

test("the confirmation lists what may protect less, and a change meanwhile asks again", async () => {
  const { page, host, $, controls } = await openPage();
  check($("category-webmail"), false);
  check($("never-save-enabled"), false);
  click(controls("save-but-no-mcp", 0).remove);
  click($("apply"));
  assert.deepEqual($("relax-confirm-list").children.map((item) => item.textContent), [
    "基本保護「Webメール」をOFFにする",
    "「保存しないグループ」を無効にする",
    "「MCPサーバに見せないグループ」の既存のルール1件を削除するか、内容・種類を変更する",
  ]);
  assert.match(htmlText("relax-confirm-text"), /^この変更により、これまで保存されなかったURLが保存されるようになる可能性があります。/);
  type(controls("never-save", 0).input, "bank.example/x");
  assert.equal($("relax-confirm").hidden, true, "an edit closes it");
  click($("relax-confirm-yes"));
  await flush();
  assert.deepEqual(host.ofType("policy_apply"), [], "what was confirmed is not what is there now");
  assert.equal(page.draft.dirty(), true);
  // Even a change that reaches the draft without closing the confirmation is not applied.
  click($("apply"));
  assert.equal($("relax-confirm").hidden, false);
  page.draft.setCategory("aiChats", false);
  click($("relax-confirm-yes"));
  await flush();
  assert.deepEqual(host.ofType("policy_apply"), []);
});

test("relaxations compare rules as a multiset and ignore additions and reordering", () => {
  const active = v2({ neverSave: [pattern("a.example"), pattern("a.example"), regex("b")] });
  const draft = (neverSave, extra = {}) => ({ ...v2({ neverSave }), ...extra });
  assert.deepEqual(relaxations(active, draft([regex("b"), pattern("a.example"), pattern("a.example"), pattern("c.example")])), []);
  assert.deepEqual(relaxations(active, draft([pattern("a.example"), regex("b")])), [{ type: "rules", key: "neverSave", count: 1 }], "one of two copies removed");
  assert.deepEqual(relaxations(active, draft([pattern("a.example"), pattern("a.example"), pattern("b")])), [{ type: "rules", key: "neverSave", count: 1 }], "kind changed");
  const off = draft(active.layers.neverSave.rules);
  off.categories = { ...allOn(), authentication: false };
  off.layers.saveButNoMcp.enabled = false;
  assert.deepEqual(relaxations(active, off), [{ type: "category", key: "authentication" }, { type: "layer", key: "saveButNoMcp" }]);
  const wasOff = v2();
  wasOff.categories.webmail = false;
  wasOff.layers.neverSave.enabled = false;
  assert.deepEqual(relaxations(wasOff, v2()), [], "turning protection on asks nothing");
});

// --- Security -------------------------------------------------------------------------------------

test("rule text and URLs are values or text only, never in an attribute, a message or a log", async () => {
  const hostile = ['<img src=x onerror="alert(1)">', '"><script>alert(1)</script>', "‮evil"];
  await guarded(async () => {
    const opened = await openPage({ policy: v2({ neverSave: hostile.map(regex), saveButNoMcp: [pattern("secret.example")] }) });
    const { host, $, controls, rowsOf } = opened;
    host.answers.policy_validate = () => ({ ok: false, error: "policy_invalid", errors: hostile.map((_, ruleIndex) => ({ layer: "neverSave", ruleIndex, code: "invalid_regex" })) });
    host.answers.policy_test_rule = () => ({ ok: false, error: "invalid_url" });
    host.answers.policy_test_url = () => ({ ok: true, decision: "never_save" });
    const url = "https://secret.example/?token=abc&<b>";
    type($("test-url"), url);
    type(controls("save-but-no-mcp", 0).input, "secret.example/<i>");
    click($("validate"));
    click(controls("never-save", 0).test);
    click($("test-url-run"));
    await flush();
    click($("apply"));
    click($("relax-confirm-yes"));
    await flush();
    assert.deepEqual(rowsOf("never-save").map((row) => row.children[1].children[0].value), hostile);
    const visible = [];
    const visit = (node) => {
      assert.equal(Object.hasOwn(node, "innerHTML"), false);
      for (const value of node.attributes.values()) {
        assert.ok(![...hostile, "secret.example", url].some((text) => value.includes(text)), `attribute ${value}`);
      }
      if (node.tagName !== "INPUT") visible.push(node.textContent);
      node.children.forEach(visit);
    };
    for (const id of ["never-save-rules", "save-but-no-mcp-rules", "validation-status", "apply-status", "test-url-result", "never-save-error", "save-but-no-mcp-error"]) visit($(id));
    for (const text of visible) {
      assert.ok(![...hostile, "secret.example", "token=abc"].some((secret) => text.includes(secret)), `text ${text}`);
    }
    assert.ok(host.requests.length > 4);
  });
});

test("the page sends no message that initializes, starts, migrates or presets", async () => {
  const opened = await openPage();
  const { host, $, controls } = opened;
  for (const edit of Object.values(relaxingEdits)) edit(opened);
  type($("test-url"), "https://x.example/");
  click($("validate"));
  await flush();
  click($("test-url-run"));
  click(controls("never-save", 0).test);
  click($("apply"));
  click($("relax-confirm-yes"));
  await flush();
  const types = new Set(host.types());
  assert.deepEqual([...types].sort(), ["policy_apply", "policy_get", "policy_status", "policy_test_rule", "policy_test_url", "policy_validate"]);
  assert.ok(host.requests.every((message) => typeof message.requestId === "string" && message.requestId !== ""));
});
