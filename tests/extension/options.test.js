// The Privacy settings page (options.html, options-page.js, options-state.js) against a fake DOM and
// a fake host. Synthetic policies only.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { createDocument, extensionDir, settle } from "./fake-dom.js";
import { SearchError } from "../../extension/search-client.js";
import { startOptionsPage } from "../../extension/options-page.js";
import { CATEGORY_KEYS, createDraft, loadActivePolicy, readActive, sameValue } from "../../extension/options-state.js";

const read = (name) => readFileSync(path.join(extensionDir, name), "utf8");
const manifest = JSON.parse(read("manifest.json"));
const html = read("options.html");
const SOURCES = ["options.js", "options-page.js", "options-state.js"];

const allOn = () => Object.fromEntries(CATEGORY_KEYS.map((key) => [key, true]));
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
function v1(rules = {}) {
  return { ...v2(rules), schemaVersion: 1, engineVersion: 1 };
}

/** A host answering `policy_status` with `state` and `policy_get` with `policy`; it records requests. */
function fakeHost({ state = "ready", policy = v2(), etag = "etag-1", status = null, get = null, delay = 0 } = {}) {
  const host = { requests: [], policy, state, etag };
  host.send = async (message) => {
    host.requests.push(message);
    if (delay) await settle(delay);
    const base = { requestId: message.requestId };
    if (message.type === "policy_status") {
      if (status) return status(message);
      if (host.state !== "ready") return { ok: true, type: "policy_result", ...base, state: host.state };
      const p = host.policy;
      return { ok: true, type: "policy_result", ...base, state: "ready", etag: host.etag, revision: p.revision, schemaVersion: p.schemaVersion, engineVersion: p.engineVersion };
    }
    if (message.type === "policy_get") {
      if (get) return get(message);
      const p = host.policy;
      return { ok: true, type: "policy_get_result", ...base, state: "ready", etag: host.etag, revision: p.revision, schemaVersion: p.schemaVersion, engineVersion: p.engineVersion, policy: structuredClone(p) };
    }
    return { ok: false, type: `${message.type}_result`, ...base, error: "unknown_type" };
  };
  return host;
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

const queried = new Set();
async function openPage(hostOptions) {
  const document = createDocument();
  const original = document.querySelector;
  document.querySelector = (selector) => {
    queried.add(selector);
    return original(selector);
  };
  const window = fakeWindow();
  const host = hostOptions?.send ? hostOptions : fakeHost(hostOptions);
  const page = startOptionsPage({ document, window, send: host.send });
  await page.ready;
  return { page, host, window, document, $: (id) => original(`#${id}`) };
}

function toggle($, key, checked) {
  const input = $(`category-${key}`);
  input.checked = checked;
  input.dispatch("change");
}

const ruleCells = ($, layer) => $(`${layer}-rules`).children.map((row) => row.children.map((cell) => (cell.children[0] ?? cell).textContent));

function* walk(node) {
  yield node;
  for (const child of node.children) yield* walk(child);
}

// --- Manifest and page --------------------------------------------------------------------------

test("the options page is the manifest's, with no new permission", () => {
  assert.equal(manifest.options_page, "options.html");
  assert.equal(manifest.options_ui, undefined);
  assert.deepEqual(manifest.permissions, ["webNavigation", "storage", "nativeMessaging", "tabs", "sidePanel", "contextMenus", "scripting", "notifications"]);
  assert.deepEqual(manifest.host_permissions, ["<all_urls>"]);
  assert.equal(manifest.content_security_policy, undefined, "the default extension CSP applies");
});

test("the page loads only its own module and stylesheet, with no inline script, handler or style", () => {
  const scripts = [...html.matchAll(/<script\b([^>]*)>([\s\S]*?)<\/script>/g)];
  assert.deepEqual(scripts.map(([, attributes, body]) => [attributes.trim(), body.trim()]), [['type="module" src="options.js"', ""]]);
  assert.deepEqual([...html.matchAll(/<link\b[^>]*href="([^"]+)"/g)].map((match) => match[1]), ["options.css"]);
  assert.doesNotMatch(html, /\son[a-z]+\s*=/i);
  assert.doesNotMatch(html, /\sstyle\s*=/i);
  assert.doesNotMatch(html + read("options.css"), /https?:\/\//);
  for (const name of SOURCES) assert.doesNotMatch(read(name), /https?:\/\/|\bimport\(|\beval\(|new Function/, name);
});

test("the page names the groups and buttons as decided", () => {
  for (const text of [
    "保存しないグループ",
    "MCPサーバに見せないグループ",
    "基本保護",
    "変更を破棄",
    "適用",
    "一致したURLはMemocampusに保存されません。",
    "一致したURLはVaultに保存されますが、MCPサーバ向けの公開対象から除外されます。",
    "ルール編集と適用機能は次の開発段階で利用可能になります。",
  ]) {
    assert.ok(html.includes(text), text);
  }
  assert.match(html, /<button id="apply" type="button" class="primary" disabled/);
  for (const key of CATEGORY_KEYS) assert.match(html, new RegExp(`<label for="category-${key}">`), key);
});

// --- Loading ------------------------------------------------------------------------------------

test("a ready version 2 policy is read, shown and becomes an editable draft", async () => {
  const policy = v2({ neverSave: [{ kind: "pattern", pattern: "*.bank.example" }, { kind: "regex", pattern: "^https://a\\.example/" }], saveButNoMcp: [{ kind: "pattern", pattern: "wiki.example/private/*" }] });
  const { page, host, $ } = await openPage({ policy, etag: "abc" });
  assert.deepEqual(host.requests.map((message) => message.type), ["policy_status", "policy_get"]);
  assert.equal(page.view.state, "ready");
  assert.equal(page.view.active.etag, "abc");
  assert.equal($("policy-state").textContent, "Ready（利用可能）");
  assert.equal($("policy-state").dataset.state, "ready");
  assert.equal($("policy-version").textContent, "Schema 2 / Engine 2");
  assert.equal($("policy-revision").textContent, "7");
  for (const key of CATEGORY_KEYS) {
    assert.equal($(`category-${key}`).checked, true);
    assert.equal($(`category-${key}`).disabled, false);
  }
  assert.equal($("categories").hidden, false);
  assert.deepEqual(ruleCells($, "never-save"), [["Pattern", "*.bank.example", "編集不可"], ["Regex", "^https://a\\.example/", "編集不可"]]);
  assert.deepEqual(ruleCells($, "save-but-no-mcp"), [["Pattern", "wiki.example/private/*", "編集不可"]]);
  assert.equal($("never-save-empty").hidden, true);
  assert.match($("never-save-state").textContent, /有効です。ルール 2件/);
  assert.equal($("apply").disabled, true);
  assert.equal($("discard").disabled, true);
});

test("a version 1 policy is shown but not editable and asks for an explicit migration", async () => {
  const { page, host, $ } = await openPage({ policy: v1({ neverSave: ["old.example", "*.old.example/x/*"] }) });
  assert.equal($("policy-version").textContent, "Schema 1 / Engine 1");
  assert.match($("policy-explanation").textContent, /旧形式（v1）.*明示的な移行（Migration）.*自動では移行しません/);
  assert.deepEqual(ruleCells($, "never-save"), [["Pattern（v1）", "old.example", "編集不可"], ["Pattern（v1）", "*.old.example/x/*", "編集不可"]]);
  for (const key of CATEGORY_KEYS) {
    assert.equal($(`category-${key}`).checked, true, "the active values are shown");
    assert.equal($(`category-${key}`).disabled, true);
  }
  toggle($, "webmail", false);
  assert.equal(page.draft.category("webmail"), true, "a change that reaches the page anyway is refused");
  assert.equal(page.draft.dirty(), false);
  assert.equal($("discard").disabled, true);
  assert.deepEqual(host.requests.map((message) => message.type), ["policy_status", "policy_get"], "nothing migrates");
});

for (const [state, label, text] of [
  ["uninitialized", "Not Initialized（未初期化）", /まだ初期化されていません.*自動記録は行われません.*この画面から初期化することはありません/],
  ["invalid", "Invalid（読み取れません）", /自動記録を停止しています.*変更していません/],
  ["unsupported", "Invalid（このバージョンでは使えません）", /自動記録を停止しています/],
  ["busy", "Unavailable（使用中）", /使用中です/],
  ["unavailable", "Unavailable（アクセスできません）", /アクセスできません/],
]) {
  test(`a ${state} policy is explained, never read, initialized or edited`, async () => {
    const { page, host, $ } = await openPage({ state });
    assert.deepEqual(host.requests.map((message) => message.type), ["policy_status"]);
    assert.equal(page.view.state, state);
    assert.equal(page.draft, null);
    assert.equal($("policy-state").textContent, label);
    assert.match($("policy-explanation").textContent, text);
    assert.equal($("policy-version").textContent, "—");
    assert.equal($("policy-revision").textContent, "—");
    assert.equal($("categories").hidden, true, "no value is shown as if it were active");
    assert.equal($("categories-note").hidden, false);
    for (const key of CATEGORY_KEYS) assert.equal($(`category-${key}`).disabled, true);
    for (const layer of ["never-save", "save-but-no-mcp"]) {
      assert.equal($(`${layer}-rules`).children.length, 0);
      assert.equal($(`${layer}-table`).hidden, true);
      assert.match($(`${layer}-empty`).textContent, /取得できていない/);
    }
    toggle($, "authentication", false);
    $("discard").dispatch("click");
    $("reload").dispatch("click");
    await settle(0);
    assert.deepEqual([...new Set(host.requests.map((message) => message.type))], ["policy_status"], "only status, again on reload");
    assert.equal($("apply").disabled, true);
  });
}

test("a host that cannot be reached, times out or disconnects leaves nothing active", async () => {
  for (const [error, state] of [
    [new SearchError("host_unavailable"), "host_unavailable"],
    [new SearchError("timeout"), "host_unavailable"],
    [new Error("disconnected"), "host_unavailable"],
    [new SearchError("unknown_type"), "host_unavailable"],
  ]) {
    const { page, $ } = await openPage({ send: async () => { throw error; } });
    assert.equal(page.view.state, state);
    assert.equal(page.draft, null);
    assert.match($("policy-state").textContent, /^Unavailable/);
    assert.match($("policy-explanation").textContent, /取得できていない/);
    assert.equal($("categories").hidden, true);
  }
});

test("a reply that is malformed in any way is not taken as the active policy", async () => {
  const good = v2();
  const reply = (fields) => (message) => ({ ok: true, type: "policy_get_result", requestId: message.requestId, state: "ready", etag: "e", revision: 7, schemaVersion: 2, engineVersion: 2, policy: good, ...fields });
  const cases = {
    "another requestId": { get: (message) => ({ ...reply({})(message), requestId: "x" }) },
    "another type": { get: (message) => ({ ...reply({})(message), type: "policy_result" }) },
    "no ok flag": { get: (message) => ({ ...reply({})(message), ok: undefined }) },
    "no ETag": { get: reply({ etag: undefined }) },
    "an empty ETag": { get: reply({ etag: "" }) },
    "a revision as text": { get: reply({ revision: "7" }) },
    "a revision unlike the policy's": { get: reply({ revision: 8 }) },
    "versions unlike the policy's": { get: reply({ schemaVersion: 1, engineVersion: 1 }) },
    "an unknown version": { get: reply({ schemaVersion: 3, engineVersion: 3, policy: { ...good, schemaVersion: 3, engineVersion: 3 } }) },
    "no policy": { get: reply({ policy: undefined }) },
    "a policy as text": { get: reply({ policy: JSON.stringify(good) }) },
    "a category missing": { get: reply({ policy: { ...good, categories: { ...good.categories, webmail: undefined } } }) },
    "a category as text": { get: reply({ policy: { ...good, categories: { ...good.categories, webmail: "true" } } }) },
    "a layer missing": { get: reply({ policy: { ...good, layers: { neverSave: good.layers.neverSave } } }) },
    "rules not a list": { get: reply({ policy: { ...good, layers: { ...good.layers, neverSave: { enabled: true, rules: "a.example" } } } }) },
    "a string rule in version 2": { get: reply({ policy: v2({ neverSave: ["a.example"] }) }) },
    "a typed rule of no kind": { get: reply({ policy: v2({ neverSave: [{ kind: "glob", pattern: "a" }] }) }) },
    "a status of no known state": { status: (message) => ({ ok: true, type: "policy_result", requestId: message.requestId, state: "great" }) },
    "a status of another requestId": { status: () => ({ ok: true, type: "policy_result", requestId: "x", state: "ready" }) },
    "nothing": { get: () => undefined },
  };
  for (const [name, options] of Object.entries(cases)) {
    const { page, $ } = await openPage(options);
    assert.equal(page.view.state, "invalid_response", name);
    assert.equal(page.draft, null, name);
    assert.equal($("categories").hidden, true, name);
    assert.equal($("never-save-rules").children.length, 0, name);
  }
  const typed = v2({ neverSave: ["a.example"] });
  assert.equal(readActive({ etag: "e", revision: 7, schemaVersion: 1, engineVersion: 1, policy: { ...typed, schemaVersion: 1, engineVersion: 1 } })?.etag, "e", "a string rule is version 1's");
});

test("a policy that fails when it is read is shown by its state, without content", async () => {
  for (const [error, state] of [["policy_invalid", "invalid"], ["policy_unsupported", "unsupported"], ["policy_not_initialized", "uninitialized"], ["policy_busy", "busy"], ["policy_too_large", "unavailable"]]) {
    const { page, $ } = await openPage({ get: (message) => ({ ok: false, type: "policy_get_result", requestId: message.requestId, error }) });
    assert.equal(page.view.state, state, error);
    assert.equal(page.draft, null);
    assert.equal($("categories").hidden, true);
  }
});

test("only the latest load is shown", async () => {
  const ready = fakeHost();
  const invalid = fakeHost({ state: "invalid" });
  let call = 0;
  // The first load's host is ready but answers after the second load, whose host is invalid, has finished.
  const send = async (message) => {
    if (call++ === 0) {
      await settle(40);
      return ready.send(message);
    }
    return message.type === "policy_status" && call === 2 ? invalid.send(message) : ready.send(message);
  };
  const document = createDocument();
  const page = startOptionsPage({ document, window: fakeWindow(), send });
  await page.reload();
  await page.ready;
  await settle(0);
  assert.equal(page.view.state, "invalid", "the first, late answer does not replace the second");
  assert.equal(page.draft, null);
});

// --- Draft --------------------------------------------------------------------------------------

test("the draft is a copy: editing it changes neither the active policy nor the host", async () => {
  const { page, host, $ } = await openPage({ policy: v2({ neverSave: [{ kind: "pattern", pattern: "keep.example" }] }) });
  const active = page.view.active;
  const before = structuredClone(active.policy);
  toggle($, "webmail", false);
  toggle($, "credentials", false);
  assert.equal(page.draft.category("webmail"), false);
  assert.equal(page.draft.category("credentials"), false);
  assert.deepEqual(active.policy, before, "the active policy is unchanged");
  assert.equal(Object.isFrozen(active.policy.categories), true);
  assert.throws(() => { active.policy.categories.webmail = false; }, TypeError);
  assert.deepEqual(host.policy.categories, allOn(), "the host's policy is unchanged");
  assert.deepEqual(host.requests.map((message) => message.type), ["policy_status", "policy_get"], "editing sends nothing");
  // Neither does any other button.
  $("apply").dispatch("click");
  $("discard").dispatch("click");
  await settle(0);
  assert.deepEqual(host.requests.map((message) => message.type), ["policy_status", "policy_get"]);
});

test("dirty follows the difference from the active policy, and undoing an edit clears it", async () => {
  const { page, $ } = await openPage();
  assert.equal($("dirty-status").textContent, "");
  toggle($, "aiChats", false);
  assert.equal(page.draft.dirty(), true);
  assert.equal($("dirty-status").textContent, "未適用の変更があります");
  assert.equal($("discard").disabled, false);
  assert.equal($("apply").disabled, true, "a dirty draft still cannot be applied");
  toggle($, "government", false);
  toggle($, "aiChats", true);
  assert.equal(page.draft.dirty(), true, "one change remains");
  toggle($, "government", true);
  assert.equal(page.draft.dirty(), false, "back to the active values");
  assert.equal($("dirty-status").textContent, "");
  assert.equal($("discard").disabled, true);
});

test("discarding rebuilds the draft from the active policy and resets the controls", async () => {
  const { page, $ } = await openPage();
  const active = structuredClone(page.view.active.policy);
  toggle($, "authentication", false);
  toggle($, "webmail", false);
  $("discard").dispatch("click");
  assert.equal(page.draft.dirty(), false);
  assert.deepEqual(page.draft.policy, active);
  for (const key of CATEGORY_KEYS) assert.equal($(`category-${key}`).checked, true, key);
  assert.equal($("dirty-status").textContent, "変更を破棄しました。");
  assert.equal($("category-authentication-warning").hidden, true);
  assert.deepEqual(page.view.active.policy, active);
});

test("turning protection off warns, and the choice stands", async () => {
  const policy = v2();
  policy.categories.government = false;
  const { page, $ } = await openPage({ policy });
  for (const key of CATEGORY_KEYS) assert.equal($(`category-${key}-warning`).hidden, true, key);
  toggle($, "credentials", false);
  assert.equal($("category-credentials-warning").hidden, false);
  assert.equal($("category-credentials").checked, false, "not turned back on");
  toggle($, "government", true);
  assert.equal($("category-government-warning").hidden, true, "turning one on is no weakening");
  assert.deepEqual(page.draft.weakened(), ["credentials"]);
});

test("a category of a disabled group says it is not used", async () => {
  const policy = v2();
  policy.layers.saveButNoMcp.enabled = false;
  const { $ } = await openPage({ policy });
  assert.equal($("category-webmail-note").hidden, false);
  assert.match($("category-webmail-note").textContent, /MCPサーバに見せないグループ」が無効/);
  assert.equal($("category-authentication-note").hidden, true);
  assert.match($("save-but-no-mcp-state").textContent, /無効です/);
});

test("fields the page does not show are kept in the draft", () => {
  const active = readActive({ etag: "e", revision: 7, schemaVersion: 2, engineVersion: 2, policy: v2({}, { future: { a: [1, 2] } }) });
  const draft = createDraft({ policy: { ...structuredClone(active.policy), categories: { ...active.policy.categories, later: true } } }, { editable: true });
  draft.setCategory("webmail", false);
  draft.setCategory("webmail", true);
  assert.deepEqual(draft.policy.future, { a: [1, 2] });
  assert.equal(draft.policy.categories.later, true);
  assert.equal(draft.dirty(), false);
  assert.equal(draft.setCategory("later", false), false, "only known categories are edited");
  assert.equal(draft.setCategory("webmail", "no"), false);
});

test("values compare by content, not by key order or identity", () => {
  assert.equal(sameValue({ a: 1, b: [1, { c: 2 }] }, { b: [1, { c: 2 }], a: 1 }), true);
  assert.equal(sameValue({ a: [1, 2] }, { a: [2, 1] }), false);
  assert.equal(sameValue({ a: 1 }, { a: 1, b: undefined }), false);
  assert.equal(sameValue([], {}), false);
  assert.equal(sameValue(null, {}), false);
});

test("the draft is kept nowhere but in the page", async () => {
  for (const name of SOURCES) {
    assert.doesNotMatch(read(name), /chrome\.storage|localStorage|sessionStorage|indexedDB|IDBFactory|document\.cookie|caches\./, name);
  }
  const trap = { get() { throw new Error("storage used"); } };
  const saved = Object.getOwnPropertyDescriptor(globalThis, "localStorage");
  Object.defineProperty(globalThis, "localStorage", { configurable: true, ...trap });
  globalThis.chrome = { storage: new Proxy({}, { get() { throw new Error("chrome.storage used"); } }) };
  try {
    const { $ } = await openPage();
    toggle($, "webmail", false);
    $("discard").dispatch("click");
  } finally {
    if (saved) Object.defineProperty(globalThis, "localStorage", saved);
    else delete globalThis.localStorage;
    delete globalThis.chrome;
  }
});

// --- Rule tables --------------------------------------------------------------------------------

test("rule text is shown as text, whatever it holds, and logged nowhere", async () => {
  const hostile = ['<img src=x onerror="alert(1)">', '"><script>alert(1)</script>', "a.example/&amp;/*", "‮ evil"];
  const policy = v2({ neverSave: hostile.map((pattern) => ({ kind: "regex", pattern })), saveButNoMcp: [{ kind: "pattern", pattern: "<b>x</b>" }] });
  const logged = [];
  const methods = ["log", "info", "warn", "error", "debug", "trace"];
  const saved = methods.map((method) => console[method]);
  for (const method of methods) console[method] = (...args) => logged.push(args);
  let document;
  try {
    ({ document } = await openPage({ policy }));
  } finally {
    methods.forEach((method, index) => { console[method] = saved[index]; });
  }
  const $ = (id) => document.querySelector(`#${id}`);
  assert.deepEqual(ruleCells($, "never-save").map((row) => row[1]), hostile);
  assert.deepEqual(ruleCells($, "save-but-no-mcp").map((row) => row[1]), ["<b>x</b>"]);
  for (const layer of ["never-save", "save-but-no-mcp"]) {
    for (const node of walk($(`${layer}-rules`))) {
      assert.equal(Object.hasOwn(node, "innerHTML"), false);
      assert.equal(Object.hasOwn(node, "outerHTML"), false);
      assert.equal(node.attributes.size, 0, "no rule is copied into an attribute");
    }
  }
  assert.deepEqual(logged, []);
  for (const name of SOURCES) assert.doesNotMatch(read(name), /innerHTML|outerHTML|insertAdjacentHTML|document\.write|console\./, name);
});

test("empty groups say so, and no rule can be edited", async () => {
  const { $, document } = await openPage();
  for (const layer of ["never-save", "save-but-no-mcp"]) {
    assert.equal($(`${layer}-table`).hidden, true);
    assert.equal($(`${layer}-empty`).hidden, false);
    assert.equal($(`${layer}-empty`).textContent, "ルールはありません。");
  }
  const { $: $2 } = await openPage({ policy: v2({ neverSave: [{ kind: "pattern", pattern: "a.example" }] }) });
  const cells = [...walk($2("never-save-rules"))];
  assert.ok(cells.every((node) => !["BUTTON", "INPUT", "SELECT", "TEXTAREA"].includes(node.tagName)), "no control in a rule table");
  assert.ok(document);
});

// --- Leaving and reloading ----------------------------------------------------------------------

test("leaving with unapplied changes asks, and leaving without them does not", async () => {
  const { $, window } = await openPage();
  assert.equal(window.dispatch("beforeunload").defaultPrevented, false);
  toggle($, "webmail", false);
  const event = window.dispatch("beforeunload");
  assert.equal(event.defaultPrevented, true);
  assert.equal(event.returnValue, "");
  toggle($, "webmail", true);
  assert.equal(window.dispatch("beforeunload").defaultPrevented, false);
});

test("reloading with unapplied changes asks first, in the page", async () => {
  const { page, host, $, document } = await openPage();
  toggle($, "webmail", false);
  $("reload").dispatch("click");
  await settle(0);
  assert.equal($("reload-confirm").hidden, false);
  assert.equal(document.activeElement, $("reload-confirm-yes"));
  assert.equal(host.requests.length, 2, "nothing is read before the answer");
  $("reload-confirm-no").dispatch("click");
  assert.equal($("reload-confirm").hidden, true);
  assert.equal(document.activeElement, $("reload"));
  assert.equal(page.draft.category("webmail"), false, "the draft is kept");

  host.policy = { ...v2(), revision: 8 };
  host.etag = "etag-2";
  $("reload").dispatch("click");
  $("reload-confirm-yes").dispatch("click");
  await settle(0);
  assert.equal($("reload-confirm").hidden, true);
  assert.equal(document.activeElement, $("active-heading"), "focus does not stay on the hidden button");
  assert.equal(page.view.active.etag, "etag-2");
  assert.equal($("policy-revision").textContent, "8");
  assert.equal(page.draft.dirty(), false);
  assert.equal($("category-webmail").checked, true);
  assert.deepEqual(host.requests.map((message) => message.type), ["policy_status", "policy_get", "policy_status", "policy_get"]);
});

test("a clean page reloads at once", async () => {
  const { host, $ } = await openPage();
  $("reload").dispatch("click");
  await settle(0);
  assert.equal($("reload-confirm").hidden, true);
  assert.equal(host.requests.length, 4);
});

// --- Host messages ------------------------------------------------------------------------------

test("the page sends only reading messages, and never initializes, starts, applies or migrates", async () => {
  for (const name of SOURCES) {
    assert.doesNotMatch(read(name), /policy_(apply|start|initialize|migrate|validate|test_url|test_rule|preset)|policy-initialize|sendMessage/, name);
  }
  const host = fakeHost({ state: "uninitialized" });
  const { $ } = await openPage({ send: host.send });
  $("reload").dispatch("click");
  await settle(0);
  host.state = "ready";
  $("reload").dispatch("click");
  await settle(0);
  toggle($, "webmail", false);
  $("discard").dispatch("click");
  $("apply").dispatch("click");
  await settle(0);
  assert.deepEqual([...new Set(host.requests.map((message) => message.type))].sort(), ["policy_get", "policy_status"]);
  assert.ok(host.requests.every((message) => typeof message.requestId === "string" && message.requestId !== ""));
});

test("loadActivePolicy never rejects", async () => {
  const result = await loadActivePolicy({ send: () => { throw new TypeError("boom"); } });
  assert.deepEqual(result, { state: "host_unavailable", code: "host_unavailable" });
});

// Last: every element the page used exists in options.html.
test("every element the page looks up is in options.html", () => {
  assert.ok(queried.size > 30);
  for (const selector of queried) {
    assert.match(selector, /^#[\w-]+$/);
    assert.ok(html.includes(`id="${selector.slice(1)}"`), selector);
  }
});
