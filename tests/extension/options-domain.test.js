// The Domain Wildcard kind on the Privacy settings page: chosen, checked, added and edited through the
// URL checker like the other kinds. Synthetic policies and URLs only. The fake host's answers are
// scripted, so the page is shown to judge nothing itself: the real matching is tested in the host.
import { test } from "node:test";
import assert from "node:assert/strict";
import { SearchError } from "../../extension/search-client.js";
import { matchState, sameRule, RULE_KINDS } from "../../extension/options-state.js";
import { evaluatePolicy, validatePolicy } from "../../extension/policy.js";
import {
  RULES,
  addRule,
  checkRule,
  chooseKind,
  click,
  editRule,
  flush,
  html,
  htmlText,
  openPage,
  pattern,
  regex,
  type,
  v2,
} from "./options-fixtures.js";

const domain = (text) => ({ kind: "domainWildcard", pattern: text });
const URL = "https://www.example.com/page";
const addButtons = ($) => [$("checker-add-never-save"), $("checker-add-save-but-no-mcp")];

test("the kind is the third of one radio group, after Pattern and Regex", () => {
  assert.deepEqual(RULE_KINDS, ["pattern", "regex", "domainWildcard"]);
  const radios = [...html.matchAll(/<input id="checker-kind-(\w+)" type="radio" name="checker-kind" value="(\w+)"/g)].map((m) => [m[1], m[2]]);
  assert.deepEqual(radios, [["pattern", "pattern"], ["regex", "regex"], ["domainWildcard", "domainWildcard"]]);
  assert.match(html, /<label for="checker-kind-domainWildcard">Domain Wildcard<\/label>/);
  assert.match(html, /<input id="checker-rule" type="text"/, "one line of text");
  assert.match(htmlText("checker-kind-note"), /スキーム.*パスは含めず.*example\.com 自身にも一致します/);
  assert.match(htmlText("checker-help"), /Domain Wildcardは「\*\.example\.com」の形で、example\.com 自身とそのすべてのサブドメインに一致します/);
  assert.match(htmlText("checker-help"), /\*\.com や \*\.co\.jp のような公開サフィックスは指定できません/);
  assert.match(html, /id="checker-rule"[^>]*aria-describedby="checker-gate"/, "Pattern first: no note");
});

test("choosing Domain Wildcard shows its example and note, and another kind takes them away", async () => {
  const { $ } = await openPage();
  assert.equal($("checker-rule").placeholder, "");
  assert.equal($("checker-kind-note").hidden, true);
  chooseKind($, "domainWildcard");
  assert.equal($("checker-rule").placeholder, "*.example.com");
  assert.equal($("checker-kind-note").hidden, false);
  assert.equal($("checker-rule").getAttribute("aria-describedby"), "checker-kind-note checker-gate");
  assert.equal($("checker-kind-domainWildcard").disabled, false);
  for (const kind of ["pattern", "regex"]) {
    chooseKind($, kind);
    assert.equal($("checker-rule").placeholder, "", kind);
    assert.equal($("checker-kind-note").hidden, true, kind);
    assert.equal($("checker-rule").getAttribute("aria-describedby"), "checker-gate", `${kind}: a hidden note is not read`);
  }
});

test("the rule goes to the host as the kind and the text typed, nothing trimmed or lowered", async () => {
  const { host, $ } = await openPage();
  const rule = domain("  *.Example.COM. ");
  const url = " https://WWW.example.com:8443/a?b#c ";
  await checkRule($, rule, url);
  assert.deepEqual(host.ofType("policy_test_rule").map((m) => ({ rule: m.rule, url: m.url })), [{ rule, url }]);
});

test("a match enables both groups; adding puts the very rule in the draft only", async () => {
  const opened = await openPage();
  const { page, host, $, controls } = opened;
  await checkRule($, domain("*.example.com"), URL);
  assert.equal($("checker-result").textContent, "一致しました");
  for (const button of addButtons($)) assert.equal(button.disabled, false);
  click($("checker-add-save-but-no-mcp"));
  const rules = page.draft.layer("saveButNoMcp").rules;
  assert.deepEqual(rules.at(-1), domain("*.example.com"));
  const row = controls("save-but-no-mcp", rules.length - 1);
  assert.equal(row.kind, "Domain Wildcard");
  assert.equal(row.text.textContent, "*.example.com");
  assert.equal(host.writes, 0, "nothing applied");
  assert.equal(host.ofType("policy_apply").length, 0);
  assert.deepEqual(host.policy.layers.saveButNoMcp.rules, RULES.saveButNoMcp, "the active policy is unchanged");
});

test("no match, an invalid rule, a bad URL and a host failure each leave both groups closed", async () => {
  const { page, host, $ } = await openPage();
  const cases = [
    [() => ({ ok: true, matched: false }), "一致しませんでした", "no_match"],
    [() => ({ ok: false, error: "invalid_domain_wildcard" }), /^ルールを確認してください。Domain Wildcardの書き方が正しくありません。ドメインのみを指定してください.*承認によっても使えるようにはなりません/, "rule_error"],
    [() => ({ ok: false, error: "invalid_url" }), /^URLを確認してください。/, "url_error"],
    [() => { throw new SearchError("host_unavailable"); }, /^判定できませんでした。/, "failed"],
    [() => ({ ok: true, matched: "true" }), /^判定できませんでした。/, "failed"],
  ];
  for (const [answer, text, state] of cases) {
    host.answers.policy_test_rule = answer;
    await checkRule($, domain(`*.case${host.ofType("policy_test_rule").length}.com`), URL);
    const shown = $("checker-result").textContent;
    if (typeof text === "string") assert.equal(shown, text);
    else assert.match(shown, text);
    assert.equal($("checker-result").dataset.result, state);
    for (const button of addButtons($)) {
      assert.equal(button.disabled, true, state);
      click(button);
    }
  }
  assert.equal(page.draft.dirty(), false, "a click on a closed button adds nothing");
  assert.equal(matchState({ ok: false, code: "invalid_domain_wildcard" }), "rule_error");
  assert.doesNotMatch($("checker-result").textContent, /case\d/, "the rule is not echoed");
});

test("the page shows what the host says, not what the rule looks like", async () => {
  const { host, $ } = await openPage();
  host.answers.policy_test_rule = () => ({ ok: true, matched: false });
  await checkRule($, domain("*.example.com"), "https://www.example.com/");
  assert.equal($("checker-result").textContent, "一致しませんでした", "looks like a match, but the host said no");
  host.answers.policy_test_rule = () => ({ ok: true, matched: true });
  await checkRule($, domain("*.com"), "https://fake-example.com/");
  assert.equal($("checker-result").textContent, "一致しました", "looks wrong, but the host said yes");
});

for (const [what, change] of [
  ["rule", ($) => type($("checker-rule"), "*.example.org")],
  ["URL", ($) => type($("checker-url"), "https://www.example.org/")],
  ["kind to Pattern", ($) => chooseKind($, "pattern")],
  ["kind to Regex", ($) => chooseKind($, "regex")],
]) {
  test(`a Domain Wildcard answer is dropped when the ${what} changes`, async () => {
    const { page, host, $ } = await openPage();
    chooseKind($, "domainWildcard");
    type($("checker-rule"), "*.example.com");
    type($("checker-url"), URL);
    const release = host.hold("policy_test_rule");
    click($("checker-run"));
    await flush();
    change($);
    release();
    await flush();
    assert.equal($("checker-result").textContent, "未チェック", "the late answer is not shown");
    for (const button of addButtons($)) assert.equal(button.disabled, true);

    // A match found before the change is dropped the same way.
    chooseKind($, "domainWildcard");
    type($("checker-rule"), "*.example.com");
    type($("checker-url"), URL);
    click($("checker-run"));
    await flush();
    assert.equal($("checker-add-never-save").disabled, false);
    change($);
    assert.equal($("checker-add-never-save").disabled, true);
    click($("checker-add-never-save"));
    assert.equal(page.draft.dirty(), false);
  });
}

test("a duplicate is the same kind and the very same text, in the same group", async () => {
  const opened = await openPage({ policy: v2({ neverSave: [domain("*.example.com")], saveButNoMcp: [] }) });
  const { page, $ } = opened;
  await checkRule($, domain("*.example.com"), URL);
  assert.equal($("checker-add-never-save").disabled, true);
  assert.match($("checker-never-save-note").textContent, /登録済み/);
  assert.equal($("checker-add-save-but-no-mcp").disabled, false, "the other group does not have it");
  assert.match($("checker-save-but-no-mcp-note").textContent, /保存しないグループの判定が優先/);
  click($("checker-add-never-save"));
  assert.equal(page.draft.layer("neverSave").rules.length, 1);

  // Another case or another kind is another rule, even where the host would match alike.
  for (const other of [domain("*.Example.com"), pattern("*.example.com")]) {
    await addRule(opened, "never-save", other, URL);
    assert.deepEqual(page.draft.layer("neverSave").rules.at(-1), other);
  }
  assert.equal(page.draft.layer("neverSave").rules.length, 3);
  assert.equal(sameRule(domain("*.example.com"), pattern("*.example.com")), false);
  assert.equal(page.draft.appendRule("neverSave", domain("*.example.com")), -1, "the model refuses it too");
});

test("a Domain Wildcard rule is edited through the checker, and its change asks before applying", async () => {
  const opened = await openPage({ policy: v2({ neverSave: [domain("*.example.com")], saveButNoMcp: [] }) });
  const { page, host, $, controls, document } = opened;
  click(controls("never-save", 0).edit);
  assert.equal($("checker-kind-domainWildcard").checked, true, "the kind is loaded");
  assert.equal($("checker-rule").value, "*.example.com");
  assert.equal($("checker-rule").placeholder, "*.example.com");
  assert.equal($("checker-apply-edit").disabled, true, "no check yet");
  type($("checker-rule"), "*.example.org");
  assert.equal($("checker-apply-edit").disabled, true);
  click($("checker-apply-edit"));
  assert.deepEqual(page.draft.layer("neverSave").rules, [domain("*.example.com")], "nothing without a match");
  type($("checker-url"), "https://www.example.org/");
  click($("checker-run"));
  await flush();
  click($("checker-apply-edit"));
  assert.deepEqual(page.draft.layer("neverSave").rules, [domain("*.example.org")]);
  assert.deepEqual(host.policy.layers.neverSave.rules, [domain("*.example.com")], "draft only");

  click($("apply"));
  assert.equal($("relax-confirm").hidden, false, "the old domain is no longer refused: ask first");
  assert.equal(document.activeElement, $("relax-confirm-no"));
  click($("relax-confirm-yes"));
  await flush();
  assert.deepEqual(host.policy.layers.neverSave.rules, [domain("*.example.org")]);
  assert.equal(host.ofType("policy_apply").length, 1);
  assert.equal(host.ofType("policy_apply")[0].expectedEtag, "etag-7");
});

test("changing a Pattern rule into a Domain Wildcard is a change of rule as well", async () => {
  const opened = await openPage();
  const { page, $ } = opened;
  await editRule(opened, "never-save", 0, domain("*.bank.example"), "https://bank.example/");
  assert.deepEqual(page.draft.layer("neverSave").rules[0], domain("*.bank.example"));
  click($("apply"));
  assert.equal($("relax-confirm").hidden, false);
  click($("relax-confirm-no"));
});

test("adding a Domain Wildcard applies atomically and a conflict keeps the draft", async () => {
  const opened = await openPage();
  const { page, host, $ } = opened;
  await addRule(opened, "never-save", domain("*.example.com"), URL);
  assert.equal($("relax-confirm").hidden, true);
  host.commit({ ...host.policy, categories: { ...host.policy.categories, aiChats: false } });
  click($("apply"));
  await flush();
  assert.equal(page.blocked, "conflict");
  assert.deepEqual(host.policy.layers.neverSave.rules, RULES.neverSave, "nothing of the draft was written");
  assert.deepEqual(page.draft.layer("neverSave").rules.at(-1), domain("*.example.com"));

  const fresh = await openPage();
  await addRule(fresh, "never-save", domain("*.example.com"), URL);
  click(fresh.$("apply"));
  await flush();
  assert.deepEqual(fresh.host.policy.layers.neverSave.rules, [...RULES.neverSave, domain("*.example.com")]);
  assert.equal(fresh.host.ofType("policy_validate").length, 1, "validated before applying");
  assert.equal(fresh.$("checker-result").textContent, "未チェック", "the match is forgotten after applying");
});

test("a version 2 policy with Domain Wildcard rules is shown with their kind", async () => {
  const policy = v2({ neverSave: [domain("*.example.com"), pattern("bank.example")], saveButNoMcp: [domain("*.例え.jp")] });
  const { controls, $ } = await openPage({ policy });
  assert.equal(controls("never-save", 0).kind, "Domain Wildcard");
  assert.equal(controls("never-save", 0).text.textContent, "*.example.com");
  assert.equal(controls("never-save", 1).kind, "Pattern");
  assert.equal(controls("save-but-no-mcp", 0).kind, "Domain Wildcard");
  assert.equal(controls("save-but-no-mcp", 0).text.textContent, "*.例え.jp", "shown as stored");
  assert.equal($("checker-kind-domainWildcard").disabled, false);
});

test("a rule the host says is invalid in the draft names the Domain Wildcard reason", async () => {
  const opened = await openPage();
  const { host, $, controls } = opened;
  host.answers.policy_validate = () => ({ ok: false, error: "policy_invalid", errors: [{ layer: "neverSave", ruleIndex: 2, code: "invalid_domain_wildcard" }] });
  await addRule(opened, "never-save", domain("*.example.com"), URL);
  click($("apply"));
  await flush();
  assert.equal(host.ofType("policy_apply").length, 0, "an invalid draft is not applied");
  assert.match(controls("never-save", 2).error.textContent, /Domain Wildcardの書き方が正しくありません/);
});

test("the extension's own evaluator knows no Domain Wildcard and refuses such a policy", () => {
  const policy = v2({ neverSave: [], saveButNoMcp: [domain("*.example.com")] });
  assert.throws(() => validatePolicy(policy), /invalid_policy/);
  for (const url of ["https://www.example.com/", "https://public.test/"]) assert.equal(evaluatePolicy(policy, url), "neverSave");
});
