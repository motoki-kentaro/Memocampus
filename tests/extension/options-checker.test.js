// The URL checker of the Privacy settings page (#23 Slice 3), against a fake DOM and a fake host.
// Synthetic policies and URLs only. The host alone matches; a rule enters the draft, or changes in it,
// only after the host has matched exactly that rule against exactly that URL.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { createDocument, extensionDir } from "./fake-dom.js";
import { SearchError } from "../../extension/search-client.js";
import { startOptionsPage } from "../../extension/options-page.js";
import { matchState, sameRule } from "../../extension/options-state.js";
import {
  RULES,
  addRule,
  check,
  checkRule,
  chooseKind,
  click,
  fakeHost,
  fakeWindow,
  flush,
  guarded,
  html,
  htmlText,
  openPage,
  pattern,
  regex,
  type,
  v2,
} from "./options-fixtures.js";

const URL = "https://bank.example/account?x=1";
const addButtons = ($) => [$("checker-add-never-save"), $("checker-add-save-but-no-mcp")];
const enterOn = (input, init = {}) => input.dispatch("keydown", { key: "Enter", ...init });

// --- Page and inputs ------------------------------------------------------------------------------

test("the checker is its own section between the basic protection and the groups, top to bottom", () => {
  const order = ["categories-heading", "checker-heading", "never-save-heading", "save-but-no-mcp-heading", "url-test-heading", "manage-heading"];
  const at = order.map((id) => html.indexOf(`id="${id}"`));
  assert.ok(at.every((position, index) => position > 0 && (index === 0 || position > at[index - 1])), JSON.stringify(at));
  assert.equal(htmlText("checker-heading"), "URLチェッカー");
  const flow = ['name="checker-kind" value="pattern"', 'for="checker-rule"', 'id="checker-rule"', 'for="checker-url"', 'id="checker-url"', 'id="checker-run"', 'id="checker-result"', 'id="checker-add-never-save"', 'id="checker-add-save-but-no-mcp"'];
  const positions = flow.map((text) => html.indexOf(text));
  assert.ok(positions.every((position, index) => position > 0 && (index === 0 || position > positions[index - 1])), "kind, rule, URL, check, result, then the groups");
  assert.match(html, /<input id="checker-kind-pattern" type="radio" name="checker-kind" value="pattern" checked/);
  assert.match(html, /<input id="checker-kind-regex" type="radio" name="checker-kind" value="regex"/);
  assert.match(html, /<legend>ルールの種類<\/legend>/);
  assert.match(html, /<label for="checker-kind-pattern">Pattern<\/label>/);
  assert.match(html, /<label for="checker-kind-regex">Regex<\/label>/);
  for (const id of ["checker-rule", "checker-url"]) {
    assert.match(html, new RegExp(`<input id="${id}" type="text"`), `${id} is a text field`);
    assert.match(html, new RegExp(`<label for="${id}" class="field-label">`));
  }
  assert.doesNotMatch(html, /type="url"/);
  assert.match(html, /<label for="checker-rule" class="field-label">ルール<\/label>/);
  assert.match(html, /<label for="checker-url" class="field-label">チェック対象URL<\/label>/);
  assert.equal(htmlText("checker-run"), "マッチングを確認");
  assert.equal(htmlText("checker-add-never-save"), "保存しないグループに追加");
  assert.equal(htmlText("checker-add-save-but-no-mcp"), "MCPサーバに見せないグループに追加");
  assert.equal(htmlText("checker-caveat"), "この結果はルール単体の一致確認です。実際の保存判定は、適用中のPolicy全体で決まります。");
  assert.equal(htmlText("checker-help"), "PatternはMemocampusのパターン照合です。RegexはRust Regex構文で判定します。先読み・後読みには対応していません。");
  assert.match(html, /<p id="checker-result" class="checker-result" data-result="none" role="status" aria-live="polite"/);
  assert.match(html, /<button id="checker-help-toggle" type="button" class="help-toggle" aria-label="[^"]+" aria-expanded="false" aria-controls="checker-help">\?<\/button>/);
  assert.doesNotMatch(html, /ルール追加/);
  assert.match(htmlText("url-test-description"), /URLチェッカーとは別の機能です/);
});

test("the kind, rule and URL are sent exactly as typed: nothing trimmed, converted or completed", async () => {
  const { host, $ } = await openPage();
  assert.equal($("checker-kind-pattern").checked, true, "Pattern first");
  const rule = "  ^HTTPS://Ｂank\\.example/(a|b)?  ";
  const url = "  https://EXAMPLE.test/%7e/%E3%81%82?q=A+B#x  ";
  await checkRule($, regex(rule), url);
  assert.deepEqual(host.ofType("policy_test_rule").map(({ rule: sent, url: sentUrl }) => ({ sent, sentUrl })), [{ sent: regex(rule), sentUrl: url }]);
  assert.equal($("checker-rule").value, rule);
  assert.equal($("checker-url").value, url);
  chooseKind($, "pattern");
  click($("checker-run"));
  await flush();
  assert.deepEqual(host.ofType("policy_test_rule")[1].rule, pattern(rule), "switching the kind converts nothing");
});

test("Enter checks from either field, but not while composing, not twice and not with a field empty", async () => {
  const { host, $, document } = await openPage();
  enterOn($("checker-rule"));
  assert.deepEqual(host.ofType("policy_test_rule"), [], "nothing to check");
  assert.equal(document.activeElement, $("checker-rule"), "the empty field takes focus");
  type($("checker-rule"), "bank.example");
  enterOn($("checker-rule"));
  assert.equal(document.activeElement, $("checker-url"));
  assert.deepEqual(host.ofType("policy_test_rule"), []);
  type($("checker-url"), URL);

  assert.equal(enterOn($("checker-url"), { isComposing: true }).defaultPrevented, false, "the input method keeps its Enter");
  enterOn($("checker-rule"), { keyCode: 229 });
  enterOn($("checker-url"), { key: "a" });
  await flush();
  assert.deepEqual(host.ofType("policy_test_rule"), []);

  const release = host.hold("policy_test_rule");
  assert.equal(enterOn($("checker-url")).defaultPrevented, true);
  enterOn($("checker-url"));
  enterOn($("checker-rule"));
  click($("checker-run"));
  click($("checker-run"));
  assert.equal($("checker-result").textContent, "確認中…");
  release();
  await flush();
  assert.equal(host.ofType("policy_test_rule").length, 1, "one question while it is on its way");
  assert.equal($("checker-result").textContent, "一致しました");
  enterOn($("checker-rule"));
  await flush();
  assert.equal(host.ofType("policy_test_rule").length, 2, "asking again once answered is allowed");
});

test("a very long URL is sent and kept as it is", async () => {
  const { host, $ } = await openPage();
  const long = `https://long.example/${"a%2F".repeat(10000)}`;
  host.answers.policy_test_rule = (message) => (message.url.length > 32768 ? { ok: false, error: "invalid_url" } : { ok: true, matched: true });
  await checkRule($, pattern("long.example"), long);
  assert.equal(host.ofType("policy_test_rule")[0].url, long);
  assert.equal($("checker-url").value, long);
  assert.match($("checker-result").textContent, /^URLを確認してください。/);
  assert.equal($("checker-result").dataset.result, "url_error");
  for (const button of addButtons($)) assert.equal(button.disabled, true);
});

// --- Matching -------------------------------------------------------------------------------------

test("only the host's match enables the groups: every other answer says why and enables nothing", async () => {
  const { host, $ } = await openPage();
  assert.equal($("checker-result").textContent, "未チェック");
  assert.equal($("checker-result").dataset.result, "none");
  for (const button of addButtons($)) assert.equal(button.disabled, true, "nothing before a check");

  const cases = [
    [() => ({ ok: true, matched: true }), "一致しました", "match"],
    [() => ({ ok: true, matched: false }), "一致しませんでした", "no_match"],
    [() => ({ ok: false, error: "invalid_regex" }), "ルールを確認してください。正規表現の書き方が正しくありません。", "rule_error"],
    [() => ({ ok: false, error: "invalid_pattern" }), /^ルールを確認してください。パターンの書き方/, "rule_error"],
    [() => ({ ok: false, error: "unsupported_regex" }), /^ルールを確認してください。.*先読み・後読み/, "rule_error"],
    [() => ({ ok: false, error: "regex_limit" }), /^ルールを確認してください。正規表現が長すぎるか複雑すぎます/, "rule_error"],
    [() => ({ ok: false, error: "invalid_rule" }), /^ルールを確認してください。ルールの形式/, "rule_error"],
    [() => ({ ok: false, error: "invalid_url" }), "URLを確認してください。このURLは判定に使えません（長すぎるなど）。", "url_error"],
    [() => { throw new SearchError("timeout"); }, "判定できませんでした。Native Hostから応答がありません。", "failed"],
    [() => { throw new SearchError("host_unavailable"); }, "判定できませんでした。Native Hostに接続できません。", "failed"],
    [() => { throw new Error("disconnected"); }, /^判定できませんでした。/, "failed"],
    [() => undefined, /^判定できませんでした。/, "failed"],
    [() => ({ ok: true, matched: "yes" }), "判定できませんでした。Native Hostから不正な応答を受け取りました。", "failed"],
    [() => ({ ok: true }), /^判定できませんでした。/, "failed"],
    [() => ({ ok: true, matched: 1 }), /^判定できませんでした。/, "failed"],
    [() => ({ ok: false, error: "policy_unavailable" }), /^判定できませんでした。/, "failed"],
    [(message) => ({ ok: true, matched: true, requestId: `${message.requestId}-other` }), /^判定できませんでした。/, "failed"],
  ];
  for (const [answer, text, state] of cases) {
    host.answers.policy_test_rule = answer;
    await checkRule($, pattern(`case${host.ofType("policy_test_rule").length}.example`), URL);
    const shown = $("checker-result").textContent;
    if (typeof text === "string") assert.equal(shown, text, state);
    else assert.match(shown, text, state);
    assert.equal($("checker-result").dataset.result, state);
    for (const button of addButtons($)) assert.equal(button.disabled, state !== "match", `${state}: ${shown}`);
    if (state !== "match") assert.match($("checker-gate").textContent, /追加できません/);
  }
  assert.equal(host.writes, 0);
});

test("the page shows the host's answer, whatever the rule looks like", async () => {
  const { host, $ } = await openPage();
  host.answers.policy_test_rule = () => ({ ok: true, matched: false });
  await checkRule($, pattern("bank.example"), "https://bank.example/");
  assert.equal($("checker-result").textContent, "一致しませんでした", "looks alike, but the host said no");
  host.answers.policy_test_rule = () => ({ ok: true, matched: true });
  await checkRule($, regex("^https://nothing\\.example/$"), "https://else.example/");
  assert.equal($("checker-result").textContent, "一致しました", "looks different, but the host said yes");
});

test("the matching state of an answer: only a boolean true is a match", () => {
  assert.equal(matchState({ ok: true, matched: true }), "match");
  assert.equal(matchState({ ok: true, matched: false }), "no_match");
  for (const answer of [{ ok: true, matched: "true" }, { ok: true }, { ok: false, code: "invalid_response" }, { ok: false, code: "timeout" }, null, undefined, { matched: true }]) {
    assert.notEqual(matchState(answer), "match", JSON.stringify(answer));
  }
  for (const code of ["invalid_rule", "invalid_pattern", "invalid_regex", "unsupported_regex", "regex_limit"]) assert.equal(matchState({ ok: false, code }), "rule_error");
  assert.equal(matchState({ ok: false, code: "invalid_url" }), "url_error");
});

for (const [what, change] of [
  ["rule", ($) => type($("checker-rule"), "^https://b\\.example/")],
  ["URL", ($) => type($("checker-url"), "https://other.example/")],
  ["kind", ($) => chooseKind($, "pattern")],
]) {
  test(`an answer about the ${what} before a change is dropped and enables nothing`, async () => {
    const { page, host, $ } = await openPage();
    host.answers.policy_test_rule = (message) => ({ ok: true, matched: message.rule.pattern === "^https://a\\.example/" && message.rule.kind === "regex" && message.url === URL });
    chooseKind($, "regex");
    type($("checker-rule"), "^https://a\\.example/");
    type($("checker-url"), URL);
    const release = host.hold("policy_test_rule");
    click($("checker-run"));
    change($);
    assert.equal($("checker-result").textContent, "未チェック", "the change leaves no answer");
    release();
    await flush();
    assert.equal($("checker-result").textContent, "未チェック", "the late match is not about what is there now");
    for (const button of addButtons($)) {
      assert.equal(button.disabled, true);
      click(button);
    }
    assert.equal(host.ofType("policy_test_rule").length, 1);
    assert.equal(page.draft.dirty(), false);
    assert.equal(host.writes, 0);
  });
}

test("answers that arrive out of order: only the latest question counts", async () => {
  const { page, host, $ } = await openPage();
  host.answers.policy_test_rule = (message) => ({ ok: true, matched: message.rule.pattern === "first.example" });
  const releaseFirst = host.hold("policy_test_rule");
  await checkRule($, pattern("first.example"), URL);
  type($("checker-rule"), "second.example");
  click($("checker-run"));
  await flush();
  assert.equal($("checker-result").textContent, "一致しませんでした");
  releaseFirst();
  await flush();
  assert.equal($("checker-result").textContent, "一致しませんでした", "the earlier question's match came last and is dropped");
  click($("checker-add-never-save"));
  assert.equal(page.draft.dirty(), false);

  // Back to the first text: its old answer is not used either; it is asked again.
  type($("checker-rule"), "first.example");
  assert.equal($("checker-result").textContent, "未チェック");
  assert.equal($("checker-add-never-save").disabled, true);
});

test("a reload, a discard or an apply forgets the match", async () => {
  const opened = await openPage();
  const { page, host, $ } = opened;
  await checkRule($, pattern("new.example"), URL);
  assert.equal($("checker-add-never-save").disabled, false);
  click($("reload"));
  await flush();
  assert.equal($("checker-result").textContent, "未チェック");
  assert.equal($("checker-add-never-save").disabled, true);
  assert.equal($("checker-rule").value, "new.example", "the fields stay");
  click($("checker-add-never-save"));
  assert.equal(page.draft.dirty(), false);

  // An answer on its way during a reload is dropped too.
  let release = host.hold("policy_test_rule");
  click($("checker-run"));
  click($("reload"));
  release();
  await flush();
  assert.equal($("checker-result").textContent, "未チェック");

  await checkRule($, pattern("other.example"), URL);
  click($("checker-add-save-but-no-mcp"));
  await checkRule($, pattern("third.example"), URL);
  click($("discard"));
  assert.equal($("checker-result").textContent, "未チェック", "a discard forgets it");
  click($("checker-add-never-save"));
  assert.equal(page.draft.dirty(), false);

  release = host.hold("policy_test_rule");
  click($("checker-run"));
  click($("discard"));
  await addRule(opened, "never-save", pattern("fourth.example"));
  release();
  await flush();
  await checkRule($, pattern("fifth.example"), URL);
  click($("apply"));
  assert.equal($("checker-add-never-save").disabled, true, "applying forgets it");
  await flush();
  assert.equal(page.view.active.revision, 8);
  assert.equal($("checker-result").textContent, "未チェック");
  click($("checker-add-never-save"));
  assert.equal(page.draft.dirty(), false);
});

test("an apply that is refused forgets the match too", async () => {
  const opened = await openPage();
  const { page, host, $ } = opened;
  host.answers.policy_apply = () => ({ ok: false, error: "policy_version_mismatch" });
  await addRule(opened, "never-save", pattern("first.example"));
  await checkRule($, pattern("second.example"), URL);
  assert.equal($("checker-add-never-save").disabled, false);
  click($("apply"));
  await flush();
  assert.match($("apply-status").textContent, /適用しませんでした/);
  assert.equal(page.view.active.revision, 7, "nothing read again");
  assert.equal($("checker-result").textContent, "未チェック");
  assert.equal($("checker-add-never-save").disabled, true);
  click($("checker-add-never-save"));
  assert.equal(page.draft.hasRule("neverSave", pattern("second.example")), false);
});

// --- Registration gate ----------------------------------------------------------------------------

test("a click that reaches the page without a current match adds nothing", async () => {
  const { page, host, $ } = await openPage();
  type($("checker-rule"), "new.example");
  type($("checker-url"), URL);
  for (const button of addButtons($)) click(button);
  assert.equal(page.draft.dirty(), false, "no check yet");

  host.answers.policy_test_rule = () => ({ ok: true, matched: false });
  click($("checker-run"));
  await flush();
  for (const button of addButtons($)) click(button);
  assert.equal(page.draft.dirty(), false, "no match");

  host.answers.policy_test_rule = () => ({ ok: false, error: "invalid_pattern" });
  click($("checker-run"));
  await flush();
  for (const button of addButtons($)) click(button);
  assert.equal(page.draft.dirty(), false, "an error");

  // A match, then the fields change without any event reaching the page (a script, say).
  delete host.answers.policy_test_rule;
  for (const change of [
    () => { $("checker-rule").value = "other.example"; },
    () => { $("checker-url").value = "https://other.example/"; },
    () => { $("checker-kind-pattern").checked = false; $("checker-kind-regex").checked = true; },
  ]) {
    chooseKind($, "pattern");
    type($("checker-rule"), "new.example");
    type($("checker-url"), URL);
    click($("checker-run"));
    await flush();
    assert.equal($("checker-add-never-save").disabled, false);
    change();
    for (const button of addButtons($)) click(button);
    assert.equal(page.draft.dirty(), false, "the match is about what was there before");
  }
  assert.equal(host.writes, 0);
});

test("a match adds the rule once: a second click, or a copy that got there meanwhile, adds nothing", async () => {
  const { page, $ } = await openPage();
  await checkRule($, pattern("new.example"), URL);
  click($("checker-add-never-save"));
  click($("checker-add-never-save"));
  assert.deepEqual(page.draft.layer("neverSave").rules.filter((rule) => sameRule(rule, pattern("new.example"))).length, 1);
  assert.equal($("checker-add-never-save").disabled, true);
  assert.match($("checker-never-save-note").textContent, /登録済み/);

  // The button was drawn enabled; the draft gets the same rule before the click.
  await checkRule($, pattern("late.example"), URL);
  assert.equal($("checker-add-save-but-no-mcp").disabled, false);
  page.draft.appendRule("saveButNoMcp", pattern("late.example"));
  click($("checker-add-save-but-no-mcp"));
  assert.equal(page.draft.layer("saveButNoMcp").rules.filter((rule) => sameRule(rule, pattern("late.example"))).length, 1, "checked again at the click");
});

test("nothing is checked or added while applying or while the applied policy is read", async () => {
  const opened = await openPage();
  const { page, host, $ } = opened;
  await addRule(opened, "never-save", pattern("first.example"));
  await checkRule($, pattern("second.example"), URL);
  const release = host.hold("policy_apply");
  click($("apply"));
  await flush();
  assert.equal(page.busy, "applying");
  for (const id of ["checker-rule", "checker-url", "checker-run", "checker-kind-pattern", "checker-kind-regex", "checker-add-never-save", "checker-add-save-but-no-mcp"]) {
    assert.equal($(id).disabled, true, id);
  }
  assert.match($("checker-gate").textContent, /適用の処理中/);
  enterOn($("checker-rule"));
  click($("checker-run"));
  click($("checker-add-never-save"));
  const checks = host.ofType("policy_test_rule").length;
  release();
  await flush();
  assert.equal(host.ofType("policy_test_rule").length, checks);
  assert.deepEqual(host.policy.layers.neverSave.rules.at(-1), pattern("first.example"), "only what was applied");
  assert.equal(page.draft.hasRule("neverSave", pattern("second.example")), false);
});

// --- Duplicates -----------------------------------------------------------------------------------

test("a duplicate is the same kind and the very same text, in the same group", async () => {
  const policy = v2({ neverSave: [pattern("bank.example"), pattern("café.example"), regex("^a+$")], saveButNoMcp: [] });
  const { page, $ } = await openPage({ policy });
  const registered = async (rule) => {
    await checkRule($, rule, URL);
    return { never: $("checker-add-never-save").disabled, note: $("checker-never-save-note").textContent };
  };
  const same = await registered(pattern("bank.example"));
  assert.equal(same.never, true);
  assert.match(same.note, /^登録済み/);
  assert.equal($("checker-add-save-but-no-mcp").disabled, false, "the other group is its own");
  click($("checker-add-never-save"));
  assert.equal(page.draft.dirty(), false);

  for (const rule of [
    pattern("Bank.example"),
    pattern(" bank.example"),
    pattern("bank.example "),
    regex("bank.example"),
    pattern("café.example"),
    regex("^aa*$"),
  ]) {
    const result = await registered(rule);
    assert.equal(result.never, false, JSON.stringify(rule));
    assert.doesNotMatch(result.note, /登録済み/);
  }
  assert.equal((await registered(pattern("café.example"))).never, true, "the same Unicode text");
  assert.equal((await registered(regex("^a+$"))).never, true);

  assert.equal(sameRule(pattern("x"), pattern("x")), true);
  assert.equal(sameRule(pattern("x"), regex("x")), false);
  assert.equal(sameRule(pattern("x"), pattern("X")), false);
});

test("a rule of the Blind group may be added to the Private one too, with a note on which wins", async () => {
  const opened = await openPage();
  const { page, $ } = opened;
  await addRule(opened, "never-save", pattern("both.example"));
  assert.equal($("checker-add-save-but-no-mcp").disabled, false);
  assert.match($("checker-save-but-no-mcp-note").textContent, /保存しないグループの判定が優先されるため/);
  click($("checker-add-save-but-no-mcp"));
  assert.equal(page.draft.hasRule("neverSave", pattern("both.example")), true);
  assert.equal(page.draft.hasRule("saveButNoMcp", pattern("both.example")), true);
  assert.equal($("checker-add-save-but-no-mcp").disabled, true);
});

// --- Registration ---------------------------------------------------------------------------------

for (const [layer, key, other] of [["never-save", "neverSave", "saveButNoMcp"], ["save-but-no-mcp", "saveButNoMcp", "neverSave"]]) {
  test(`a matched rule is added to the ${key} draft only, shown, and applied only when asked`, async () => {
    const opened = await openPage();
    const { page, host, $, controls, rowsOf, document } = opened;
    const active = structuredClone(page.view.active.policy);
    await addRule(opened, layer, regex("^https://new\\.example/"), URL);
    const index = active.layers[key].rules.length;
    assert.deepEqual(page.draft.layer(key).rules, [...active.layers[key].rules, regex("^https://new\\.example/")]);
    assert.deepEqual(page.draft.layer(other).rules, active.layers[other].rules, "the other group is untouched");
    assert.deepEqual(page.view.active.policy, active, "the active policy is unchanged");
    assert.equal(host.writes, 0);
    assert.deepEqual(host.ofType("policy_apply"), [], "applying is its own step");
    assert.deepEqual(host.ofType("policy_validate"), []);
    assert.equal(page.draft.dirty(), true);
    assert.equal($("dirty-status").textContent, "未適用の変更があります");
    assert.equal($("apply").disabled, false);
    assert.match($("checker-status").textContent, /下書きに追加しました/);
    assert.equal($("checker-status").hidden, false);
    assert.equal($("checker-rule").value, "^https://new\\.example/", "the fields stay");
    assert.equal($("checker-url").value, URL);
    assert.equal(rowsOf(layer).length, index + 1);
    assert.equal(controls(layer, index).text.textContent, "^https://new\\.example/");
    assert.equal(controls(layer, index).kind, "Regex");
    assert.equal(document.scrolledTo.element, controls(layer, index).row, "scrolled to the new row");
    assert.equal(document.scrolledTo.options.behavior, "smooth");
    assert.equal(controls(layer, index).row.dataset.added, "true", "marked for a moment");
    assert.equal(document.activeElement, controls(layer, index).edit, "the keyboard is on the new row");
  });
}

test("with reduced motion, the scroll to a new row is immediate", async () => {
  const document = createDocument();
  const window = { ...fakeWindow(), matchMedia: (query) => ({ matches: query === "(prefers-reduced-motion: reduce)" }) };
  const page = startOptionsPage({ document, window, send: fakeHost().send });
  await page.ready;
  const $ = (id) => document.querySelector(`#${id}`);
  await checkRule($, pattern("calm.example"), URL);
  click($("checker-add-never-save"));
  assert.equal(document.scrolledTo.options.behavior, "auto");
  assert.equal(page.draft.hasRule("neverSave", pattern("calm.example")), true);
});

test("a disabled group takes the rule and says it is not used until the group is on again", async () => {
  const policy = v2(RULES);
  policy.layers.saveButNoMcp.enabled = false;
  const opened = await openPage({ policy });
  const { page, $ } = opened;
  assert.match($("checker-save-but-no-mcp-note").textContent, /このグループは無効です。ルールは追加されますが、適用後もグループを有効にするまで判定に使われません。/);
  assert.equal($("checker-never-save-note").hidden, true);
  await addRule(opened, "save-but-no-mcp", pattern("off.example"));
  assert.equal(page.draft.hasRule("saveButNoMcp", pattern("off.example")), true);
  assert.equal(page.draft.layerEnabled("saveButNoMcp"), false, "the group is not turned on");
  assert.equal($("save-but-no-mcp-enabled").checked, false);
});

// --- Editing a rule -------------------------------------------------------------------------------

test("editing loads the rule into the checker and changes the draft only after a match", async () => {
  const opened = await openPage();
  const { page, host, $, controls, document } = opened;
  const before = page.draft.policy;
  click(controls("never-save", 1).edit);
  assert.equal($("checker-kind-regex").checked, true);
  assert.equal($("checker-kind-pattern").checked, false);
  assert.equal($("checker-rule").value, "^https://a\\.example/");
  assert.equal(document.activeElement, $("checker-rule"));
  assert.equal($("checker-edit").hidden, false);
  assert.match($("checker-edit").textContent, /^編集中: 「保存しないグループ」の2行目のルール/);
  assert.equal($("checker-add").hidden, true, "no adding while editing");
  assert.equal($("checker-edit-actions").hidden, false);
  assert.equal($("checker-apply-edit").disabled, true, "nothing to reflect before a match");
  assert.equal(controls("never-save", 1).row.dataset.editing, "true");
  assert.deepEqual(page.draft.policy, before, "starting an edit changes nothing");
  assert.equal(page.draft.dirty(), false);

  click($("checker-apply-edit"));
  assert.deepEqual(page.draft.policy, before);
  type($("checker-url"), URL);
  click($("checker-run"));
  await flush();
  assert.match($("checker-gate").textContent, /ルールが変更されていません/);
  assert.equal($("checker-apply-edit").disabled, true, "the same rule again is no change");

  host.answers.policy_test_rule = () => ({ ok: true, matched: false });
  type($("checker-rule"), "^https://a\\.example/x");
  click($("checker-run"));
  await flush();
  assert.equal($("checker-apply-edit").disabled, true);
  click($("checker-apply-edit"));
  assert.deepEqual(page.draft.policy, before, "no match, no change");

  delete host.answers.policy_test_rule;
  click($("checker-run"));
  await flush();
  assert.equal($("checker-apply-edit").disabled, false);
  click($("checker-apply-edit"));
  assert.deepEqual(page.draft.layer("neverSave").rules, [pattern("bank.example"), regex("^https://a\\.example/x")], "that rule, in its place");
  assert.deepEqual(page.draft.layer("saveButNoMcp").rules, RULES.saveButNoMcp);
  assert.equal(page.draft.dirty(), true);
  assert.equal($("checker-edit").hidden, true, "the edit is over");
  assert.equal($("checker-add").hidden, false);
  assert.match($("checker-status").textContent, /2行目のルールを下書きで変更しました/);
  assert.equal(document.activeElement, controls("never-save", 1).edit);
  assert.deepEqual(page.draft.relaxations(), [{ type: "rules", key: "neverSave", count: 1 }], "a changed rule asks before applying");
  assert.equal(host.writes, 0);
});

test("an edit whose rule was deleted, moved or replaced meanwhile is not reflected anywhere", async () => {
  const policy = v2({ neverSave: [pattern("a.example"), pattern("b.example"), pattern("c.example")], saveButNoMcp: [pattern("b.example")] });
  const scenarios = {
    "the rule deleted": ({ controls }) => click(controls("never-save", 1).remove),
    "a rule before it deleted, so another is in its place": ({ controls }) => click(controls("never-save", 0).remove),
    "a rule after it deleted": ({ controls }) => click(controls("never-save", 2).remove),
    "changes discarded": ({ $, controls }) => {
      click(controls("save-but-no-mcp", 0).remove);
      click($("discard"));
    },
    "reloaded": async ({ $ }) => {
      click($("reload"));
      await flush();
    },
  };
  for (const [name, meanwhile] of Object.entries(scenarios)) {
    const opened = await openPage({ policy });
    const { page, $, controls } = opened;
    click(controls("never-save", 1).edit);
    type($("checker-rule"), "b2.example");
    type($("checker-url"), URL);
    click($("checker-run"));
    await flush();
    assert.equal($("checker-apply-edit").disabled, false, name);
    const reflectNow = $("checker-apply-edit");
    await meanwhile(opened);
    const draftBefore = page.draft.policy;
    reflectNow.dispatch("click");
    assert.deepEqual(page.draft.policy, draftBefore, `${name}: nothing changes`);
    assert.equal(page.draft.hasRule("neverSave", pattern("b2.example")), false, name);
    assert.equal(page.draft.hasRule("saveButNoMcp", pattern("b2.example")), false, name);
    assert.equal($("checker-apply-edit").disabled, true, name);
    if (!$("checker-edit").hidden) assert.match($("checker-gate").textContent, /編集対象のルールが変更または削除されたため、反映できません/, name);
  }
});

test("an edit that would copy another rule of the group is refused; one of the other group is not", async () => {
  const opened = await openPage({ policy: v2({ neverSave: [pattern("a.example"), pattern("b.example")], saveButNoMcp: [pattern("p.example")] }) });
  const { page, $, controls } = opened;
  click(controls("never-save", 1).edit);
  await checkRule($, pattern("a.example"), URL);
  assert.match($("checker-gate").textContent, /同じルールがこのグループにすでにあります/);
  click($("checker-apply-edit"));
  assert.deepEqual(page.draft.layer("neverSave").rules, [pattern("a.example"), pattern("b.example")]);
  await checkRule($, pattern("p.example"), URL);
  click($("checker-apply-edit"));
  assert.deepEqual(page.draft.layer("neverSave").rules, [pattern("a.example"), pattern("p.example")]);
});

test("cancelling an edit changes nothing and keeps what was typed", async () => {
  const opened = await openPage();
  const { page, host, $, controls, document } = opened;
  click(controls("save-but-no-mcp", 0).edit);
  await checkRule($, pattern("changed.example"), URL);
  click($("checker-cancel-edit"));
  assert.equal($("checker-edit").hidden, true);
  assert.equal($("checker-add").hidden, false);
  assert.equal(page.draft.dirty(), false);
  assert.equal($("checker-status").textContent, "編集をやめました。下書きは変更していません。");
  assert.equal($("checker-rule").value, "changed.example");
  assert.equal(document.activeElement, $("checker-rule"));
  assert.equal(controls("save-but-no-mcp", 0).row.dataset.editing, "");
  click($("checker-apply-edit"));
  assert.equal(page.draft.dirty(), false, "the reflect button of the ended edit does nothing");
  assert.equal(host.writes, 0);
  assert.equal($("checker-add-never-save").disabled, false, "the match may still be added as a new rule");
});

test("deleting needs no match, and asks before applying", async () => {
  const opened = await openPage();
  const { page, host, $, controls } = opened;
  click(controls("never-save", 0).remove);
  assert.deepEqual(page.draft.layer("neverSave").rules, [regex("^https://a\\.example/")]);
  assert.deepEqual(host.ofType("policy_test_rule"), []);
  click($("apply"));
  assert.equal($("relax-confirm").hidden, false);
});

// --- Help -----------------------------------------------------------------------------------------

test("the help opens on hover, focus or click, and Escape closes it", async () => {
  const { $ } = await openPage();
  const toggle = $("checker-help-toggle");
  const shown = () => !$("checker-help").hidden;
  $("checker-help").hidden = true;
  toggle.dispatch("mouseenter");
  assert.equal(shown(), true);
  assert.equal(toggle.getAttribute("aria-expanded"), "true");
  toggle.dispatch("mouseleave");
  assert.equal(shown(), false);
  toggle.focus();
  toggle.dispatch("focus");
  assert.equal(shown(), true, "keyboard focus shows it");
  toggle.dispatch("blur");
  assert.equal(shown(), false);
  click(toggle);
  assert.equal(shown(), true, "a click keeps it open");
  toggle.dispatch("mouseleave");
  toggle.dispatch("blur");
  assert.equal(shown(), true);
  toggle.dispatch("keydown", { key: "Escape" });
  assert.equal(shown(), false);
  assert.equal(toggle.getAttribute("aria-expanded"), "false");
});

// --- Security -------------------------------------------------------------------------------------

test("the checker logs nothing, stores nothing, and puts no rule or URL in an attribute or a message", async () => {
  const hostile = '<img src=x onerror="alert(1)">';
  const url = "https://secret.example/?token=abc&<b>";
  await guarded(async () => {
    const opened = await openPage();
    const { host, $ } = opened;
    for (const answer of [() => ({ ok: false, error: "invalid_regex" }), () => ({ ok: false, error: "invalid_url" }), () => ({ ok: true, matched: false }), () => ({ ok: true, matched: true })]) {
      host.answers.policy_test_rule = answer;
      await checkRule($, regex(hostile), url);
      for (const id of ["checker-result", "checker-gate", "checker-status", "checker-never-save-note", "checker-save-but-no-mcp-note", "checker-edit"]) {
        const element = $(id);
        assert.ok(![hostile, "secret.example", "token=abc"].some((text) => element.textContent.includes(text)), `${id}: ${element.textContent}`);
        for (const value of element.attributes.values()) assert.ok(!value.includes("secret.example") && !value.includes(hostile));
      }
    }
    click($("checker-add-never-save"));
    assert.match($("checker-status").textContent, /下書きに追加しました/);
    assert.doesNotMatch($("checker-status").textContent, /secret|img/);
  });
});

test("the page has no matching of its own: no regular expression and no URL parsing in its modules", () => {
  for (const name of ["options-page.js", "options-state.js", "options.js"]) {
    const source = readFileSync(path.join(extensionDir, name), "utf8");
    assert.doesNotMatch(source, /\bRegExp\b|new URL\b|URLPattern|\.test\(|\.match\(|\.matchAll\(|\.search\(/, name);
    // Literal regular expressions: a slash after `(`, `=`, `,` or `:` that is not a comment.
    assert.doesNotMatch(source.replace(/\/\/[^\n]*|\/\*[\s\S]*?\*\//g, "").replace(/(["'`])(?:\\.|(?!\1)[^\\])*\1/g, '""'), /[(=,:]\s*\/[^/*\s]/, name);
  }
});
