// Editing, checking, testing and applying a draft on the Privacy settings page (#23 Slice 2), against a
// fake DOM and a fake host. Synthetic policies and URLs only. The fake host's answers are scripted:
// the page must show what the host says and judge nothing itself. Since Slice 3 a rule is added or
// changed through the URL checker only (see options-checker.test.js for the checker itself).
import { test } from "node:test";
import assert from "node:assert/strict";
import { SearchError } from "../../extension/search-client.js";
import { applyDraft, compareAfterApply, createDraft, readActive, relaxations } from "../../extension/options-state.js";
import { RULES, addRule, allOn, check, checkRule, click, editRule, flush, guarded, htmlText, openPage, pattern, regex, type, v2 } from "./options-fixtures.js";

/** What the page sent, leaving out the URL checker's questions. */
const sentBesidesChecks = (host) => host.types().filter((type) => type !== "policy_test_rule");

// --- Rule editing ---------------------------------------------------------------------------------

test("each group's rules are edited in their own table, in the draft only", async () => {
  const opened = await openPage();
  const { page, host, $, rowsOf, controls, ruleTexts, document } = opened;
  const active = structuredClone(page.view.active.policy);

  await addRule(opened, "never-save", regex("new.example/path"));
  assert.equal(rowsOf("never-save").length, 3);
  assert.equal(document.activeElement, controls("never-save", 2).edit, "the new rule takes focus");
  assert.deepEqual(page.draft.layer("neverSave").rules[2], regex("new.example/path"));
  assert.deepEqual(page.draft.layer("saveButNoMcp").rules, active.layers.saveButNoMcp.rules, "the other group is untouched");

  await editRule(opened, "never-save", 0, regex("bank.example"));
  assert.deepEqual(page.draft.layer("neverSave").rules[0], regex("bank.example"), "the text is kept as written when the kind changes");
  await editRule(opened, "never-save", 0, pattern("bank.example"));
  assert.deepEqual(page.draft.layer("neverSave").rules[0], pattern("bank.example"));
  assert.equal(controls("never-save", 0).kind, "Pattern");

  await addRule(opened, "save-but-no-mcp", pattern("private.example"));
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
  assert.deepEqual(sentBesidesChecks(host), ["policy_status", "policy_get"], "editing sends nothing but the checker's questions");
  assert.equal($("dirty-status").textContent, "未適用の変更があります");

  click($("discard"));
  assert.deepEqual(page.draft.policy, active);
  assert.deepEqual(ruleTexts("never-save"), ["bank.example", "^https://a\\.example/"]);
  assert.equal($("dirty-status").textContent, "変更を破棄しました。");
});

test("dirty follows the difference, and deleting the only rule leaves the group's heading in focus", async () => {
  const opened = await openPage({ policy: v2({ saveButNoMcp: [pattern("one.example")] }) });
  const { page, $, controls, document } = opened;
  await editRule(opened, "save-but-no-mcp", 0, pattern("two.example"));
  assert.equal(page.draft.dirty(), true);
  await editRule(opened, "save-but-no-mcp", 0, pattern("one.example"));
  assert.equal(page.draft.dirty(), false, "changing the old text back is no change");
  click(controls("save-but-no-mcp", 0).remove);
  assert.equal(document.activeElement, $("save-but-no-mcp-heading"));
  assert.equal($("save-but-no-mcp-table").hidden, true);
  assert.equal($("save-but-no-mcp-empty").textContent, "ルールはありません。");
  await addRule(opened, "save-but-no-mcp", pattern("one.example"));
  assert.equal(page.draft.dirty(), false, "the same rule added again is no change");
});

test("typing in the checker keeps the rows: no table is built again and the field keeps focus", async () => {
  const many = Array.from({ length: 100 }, (_, index) => pattern(`site${index}.example`));
  const { page, $, rowsOf, document } = await openPage({ policy: v2({ neverSave: many }) });
  const before = [...rowsOf("never-save")];
  const field = $("checker-rule");
  field.focus();
  for (const text of ["s", "si", "sit", "site", "site50.example/x"]) type(field, text);
  type($("checker-url"), "https://site50.example/x");
  field.focus();
  check($("never-save-enabled"), false);
  check($("category-webmail"), false);
  assert.deepEqual(rowsOf("never-save"), before, "the same rows");
  assert.equal(document.activeElement, field);
  assert.equal(field.value, "site50.example/x");
  assert.deepEqual(page.draft.layer("neverSave").rules, many, "typing changes no rule");
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
  $("checker-rule").value = "x.example";
  $("checker-url").value = "https://x.example/";
  click($("checker-run"));
  click($("checker-add-never-save"));
  click($("validate"));
  click($("apply"));
  await flush();
  assert.equal(page.draft.dirty(), false);
  assert.equal(page.draft.appendRule("neverSave", pattern("x.example")), -1);
  assert.equal(page.draft.setLayerEnabled("neverSave", false), false);
  assert.equal(page.draft.replaceRule("neverSave", 0, { expected: pattern("old.example"), next: pattern("x"), layoutSeen: 0 }), "refused");
  assert.equal(page.draft.deleteRule("neverSave", 0), false);
  assert.deepEqual(host.types(), ["policy_status", "policy_get"], "nothing checks, validates, applies or migrates");
  assert.equal($("never-save-enabled").checked, true);
});

test("the draft model refuses what is not a rule, an empty one and one the group has", () => {
  const active = readActive({ etag: "e", revision: 7, schemaVersion: 2, engineVersion: 2, policy: v2(RULES) });
  const draft = createDraft(active, { editable: true });
  for (const rule of [{ kind: "glob", pattern: "x" }, { kind: "pattern", pattern: 1 }, pattern(""), null, "x.example", pattern("bank.example")]) {
    assert.equal(draft.appendRule("neverSave", rule), -1, JSON.stringify(rule));
  }
  assert.equal(draft.appendRule("other", pattern("x.example")), -1);
  const seen = draft.layout("neverSave");
  const replace = (index, expected, next, layoutSeen = seen) => draft.replaceRule("neverSave", index, { expected, next, layoutSeen });
  assert.equal(replace(0, pattern("bank.example"), { kind: "glob", pattern: "x" }), "refused");
  assert.equal(replace(0, pattern("bank.example"), pattern("")), "refused");
  assert.equal(replace(9, pattern("bank.example"), pattern("x")), "missing");
  assert.equal(replace(0, pattern("other.example"), pattern("x")), "missing", "the place holds another rule");
  assert.equal(replace(0, pattern("bank.example"), pattern("x"), seen - 1), "missing", "the group changed since");
  assert.equal(replace(0, pattern("bank.example"), pattern("bank.example")), "unchanged");
  assert.equal(replace(0, pattern("bank.example"), regex("^https://a\\.example/")), "duplicate");
  assert.equal(draft.deleteRule("neverSave", -1), false);
  assert.equal(draft.version, 0, "a refused change is no change");
  assert.equal(draft.layout("neverSave"), seen);
  assert.equal(draft.appendRule("saveButNoMcp", pattern("new.example")), 1);
  assert.equal(draft.version, 1);
  assert.equal(draft.layout("neverSave"), seen, "a change of another group leaves this one's layout");
  assert.equal(replace(0, pattern("bank.example"), regex("bank.example")), "replaced");
  assert.equal(draft.layout("neverSave"), seen + 1);
  assert.deepEqual(active.policy, v2(RULES));
});

// --- Validation -----------------------------------------------------------------------------------

test("checking a valid draft says so and writes nothing", async () => {
  const opened = await openPage();
  const { host, $ } = opened;
  await editRule(opened, "never-save", 0, pattern("bank2.example"));
  click($("validate"));
  assert.equal($("validation-status").textContent, "検証しています…");
  assert.equal($("validate").disabled, true, "one check at a time");
  await flush();
  assert.equal($("validation-status").textContent, "検証しました。この下書きは適用できます。");
  assert.deepEqual(host.types(), ["policy_status", "policy_get", "policy_test_rule", "policy_validate"]);
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
  click($("validate"));
  await flush();
  assert.equal(host.ofType("policy_validate")[0].policy.layers.saveButNoMcp.rules.length, 1, "a disabled group's rules are checked too");
  assert.equal(controls("never-save", 0).error.hidden, true);
  assert.equal(controls("never-save", 2).error.hidden, true, "nor on the row after it");
  assert.equal(controls("never-save", 1).error.hidden, false);
  assert.equal(controls("never-save", 1).error.textContent, "エラー: 正規表現の書き方が正しくありません。");
  assert.equal(controls("never-save", 1).text.dataset.invalid, "true", "marked, and told in words, not by colour only");
  assert.equal(controls("never-save", 1).edit.getAttribute("aria-describedby"), controls("never-save", 1).error.id, "the error describes the row's edit button");
  assert.equal(controls("never-save", 2).text.dataset.invalid, "");
  assert.match(controls("save-but-no-mcp", 0).error.textContent, /^エラー: パターンの書き方/, "a rule of a disabled group is shown too");
  assert.equal($("save-but-no-mcp-error").hidden, false);
  assert.match($("save-but-no-mcp-error").textContent, /上限1,000件/);
  assert.equal($("never-save-error").hidden, true);
  const summary = $("validation-status").textContent;
  assert.match(summary, /下書きに問題があります（6件）/);
  assert.match(summary, /正規表現ルールが多すぎます/, "a problem of no place is in the summary");
  assert.equal((summary.match(/パターンの書き方/g) ?? []).length, 2, "places this page does not have go to the summary, not to a row");
  for (const text of ["a.example", "b.example", "d.example"]) assert.ok(!summary.includes(text));

  check($("category-webmail"), false);
  assert.equal(controls("never-save", 1).error.hidden, true, "a change clears what was found for the earlier draft");
  assert.equal($("save-but-no-mcp-error").hidden, true);
  assert.equal($("validation-status").hidden, true);
});

test("an answer about an earlier draft is not shown, and only the latest check counts", async () => {
  const { host, $, controls } = await openPage();
  host.answers.policy_validate = () => ({ ok: false, error: "policy_invalid", errors: [{ layer: "neverSave", ruleIndex: 0, code: "invalid_pattern" }] });
  const release = host.hold("policy_validate");
  click($("validate"));
  check($("category-webmail"), false);
  release();
  await flush();
  assert.equal(controls("never-save", 0).error.hidden, true, "the answer was about the draft before the edit");
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
  click($("validate"));
  await flush();
  assert.match($("validation-status").textContent, /^検証できませんでした。Native Hostから応答がありません/);
  assert.equal(controls("never-save", 0).error.hidden, true);
});

// --- URL test -------------------------------------------------------------------------------------

test("the draft decides a URL in the host, and the page says it is a test of the draft", async () => {
  const opened = await openPage();
  const { host, $ } = opened;
  click($("test-url-run"));
  assert.equal($("test-url-decision").textContent, "判定するURLを入力してください。");
  assert.deepEqual(host.ofType("policy_test_url"), []);

  await editRule(opened, "save-but-no-mcp", 0, pattern("wiki.example"));
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
  const { host, $ } = await openPage();
  host.answers.policy_test_url = (message) => ({ ok: true, decision: message.url.includes("first") ? "never_save" : "allow" });
  type($("test-url"), "https://first.example/");
  let release = host.hold("policy_test_url");
  click($("test-url-run"));
  check($("category-webmail"), false);
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
  const opened = await openPage();
  const { page, host, $, ruleTexts, document } = opened;
  await addRule(opened, "never-save", pattern("added.example"));
  assert.equal($("apply").disabled, false);
  assert.equal(host.writes, 0, "adding is to the draft only");
  click($("apply"));
  assert.equal($("relax-confirm").hidden, true, "adding a rule protects more: no confirmation");
  await flush();
  assert.deepEqual(host.types(), ["policy_status", "policy_get", "policy_test_rule", "policy_validate", "policy_apply", "policy_status", "policy_get"]);
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
  assert.equal(ruleTexts("never-save")[2], "added.example", "the rows show the policy read again");
});

test("an invalid draft is not applied, and its problems are shown", async () => {
  const opened = await openPage();
  const { page, host, $, controls } = opened;
  // The host matched the rule alone; the check of the whole draft still refuses it.
  await addRule(opened, "save-but-no-mcp", pattern("bad.example"));
  click($("apply"));
  await flush();
  assert.deepEqual(host.ofType("policy_apply"), [], "the invalid rule fails the check first");
  assert.equal($("apply-status").textContent, "下書きに問題があるため、適用していません。");
  assert.match(controls("save-but-no-mcp", 1).error.textContent, /パターンの書き方/);
  assert.equal(page.draft.dirty(), true, "the draft stays");
  assert.equal($("apply").disabled, false, "it can be fixed and applied");

  host.answers.policy_validate = () => { throw new SearchError("host_unavailable"); };
  await editRule(opened, "save-but-no-mcp", 1, pattern("ok.example"));
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
    const opened = await openPage();
    const { page, host, $ } = opened;
    host.answers.policy_apply = () => ({ ok: false, error });
    await addRule(opened, "never-save", pattern("x.example"));
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
  const opened = await openPage();
  const { page, host, $, ruleTexts, window } = opened;
  await addRule(opened, "never-save", pattern("mine.example"));
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
  assert.equal(ruleTexts("never-save")[2], "mine.example");
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
  const opened = await openPage();
  const { page, host, $, controls, window } = opened;
  await addRule(opened, "never-save", pattern("once.example"));
  const release = host.hold("policy_apply");
  click($("apply"));
  await flush();
  assert.equal(page.busy, "applying");
  click($("apply"));
  click($("apply"));
  for (const id of [
    "apply", "discard", "reload", "validate", "never-save-enabled", "category-webmail", "test-url", "test-url-run",
    "checker-rule", "checker-url", "checker-run", "checker-kind-regex", "checker-add-never-save", "checker-add-save-but-no-mcp",
  ]) {
    assert.equal($(id).disabled, true, id);
  }
  for (const control of ["edit", "remove"]) assert.equal(controls("never-save", 0)[control].disabled, true, control);
  // Whatever still reaches the page changes nothing.
  click(controls("never-save", 0).edit);
  click(controls("never-save", 0).remove);
  check($("category-webmail"), false);
  click($("checker-run"));
  click($("checker-add-never-save"));
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

  assert.equal(host.ofType("policy_test_rule").length, 1, "no check while applying");

  // Nor while the applied policy is being read back.
  await addRule(opened, "never-save", pattern("twice.example"));
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
  const opened = await openPage();
  const { page, host, $ } = opened;
  await addRule(opened, "never-save", pattern("kept.example"));
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
  const opened = await openPage();
  const { page, host, $ } = opened;
  await addRule(opened, "never-save", pattern("lost.example"));
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
    const opened = await openPage();
    const { page, host, $ } = opened;
    await addRule(opened, "never-save", pattern("maybe.example"));
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
  const opened = await openPage();
  const { page, host, $ } = opened;
  await addRule(opened, "never-save", pattern("unknown.example"));
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
  const opened = await openPage();
  const { page, host, $ } = opened;
  await addRule(opened, "never-save", pattern("mine.example"));
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
  "a Blind rule's text changed": (opened) => editRule(opened, "never-save", 0, pattern("bank.example/only")),
  "a Blind rule's kind changed": (opened) => editRule(opened, "never-save", 0, regex("bank.example")),
  "a Private rule's text changed": (opened) => editRule(opened, "save-but-no-mcp", 0, pattern("wiki.example")),
  "the Blind group disabled": ({ $ }) => check($("never-save-enabled"), false),
  "a category turned off": ({ $ }) => check($("category-credentials"), false),
  "a Private rule removed": ({ controls }) => click(controls("save-but-no-mcp", 0).remove),
  "the Private group disabled": ({ $ }) => check($("save-but-no-mcp-enabled"), false),
};

for (const [name, edit] of Object.entries(relaxingEdits)) {
  test(`${name}: applying asks first, and cancelling keeps the draft and sends nothing`, async () => {
    const opened = await openPage();
    const { page, host, $, document } = opened;
    await edit(opened);
    const before = page.draft.policy;
    click($("apply"));
    assert.equal($("relax-confirm").hidden, false);
    assert.equal(document.activeElement, $("relax-confirm-no"), "cancel has focus first");
    assert.ok($("relax-confirm-list").children.length >= 1);
    for (const item of $("relax-confirm-list").children) assert.doesNotMatch(item.textContent, /bank\.example|wiki\.example/, "no rule text");
    await flush();
    assert.deepEqual(sentBesidesChecks(host), ["policy_status", "policy_get"], "nothing before the answer");
    if (name.includes("removed")) $("relax-confirm").dispatch("keydown", { key: "Escape" });
    else click($("relax-confirm-no"));
    assert.equal($("relax-confirm").hidden, true);
    assert.equal(document.activeElement, $("apply"));
    assert.deepEqual(page.draft.policy, before);
    await flush();
    assert.deepEqual(sentBesidesChecks(host), ["policy_status", "policy_get"]);

    click($("apply"));
    click($("relax-confirm-yes"));
    await flush();
    assert.deepEqual(sentBesidesChecks(host).slice(2), ["policy_validate", "policy_apply", "policy_status", "policy_get"]);
    assert.equal(page.draft.dirty(), false);
  });
}

test("the confirmation lists what may protect less, and a change meanwhile asks again", async () => {
  const opened = await openPage();
  const { page, host, $, controls } = opened;
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
  await editRule(opened, "never-save", 0, pattern("bank.example/x"));
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
  const hostile = ['<img src=x onerror="alert(1)">', '"><script>alert(1)</script>', "\u202eevil"];
  await guarded(async () => {
    const opened = await openPage({ policy: v2({ neverSave: hostile.map(regex), saveButNoMcp: [pattern("secret.example")] }) });
    const { host, $, controls, ruleTexts } = opened;
    host.answers.policy_validate = () => ({ ok: false, error: "policy_invalid", errors: hostile.map((_, ruleIndex) => ({ layer: "neverSave", ruleIndex, code: "invalid_regex" })) });
    host.answers.policy_test_url = () => ({ ok: true, decision: "never_save" });
    const url = "https://secret.example/?token=abc&<b>";
    type($("test-url"), url);
    await editRule(opened, "save-but-no-mcp", 0, pattern("secret.example/<i>"), url);
    click(controls("never-save", 0).edit);
    host.answers.policy_test_rule = () => ({ ok: false, error: "invalid_regex" });
    await checkRule($, regex(hostile[0]), url);
    click($("validate"));
    click($("test-url-run"));
    await flush();
    click($("checker-cancel-edit"));
    click($("apply"));
    click($("relax-confirm-yes"));
    await flush();
    assert.deepEqual(ruleTexts("never-save"), hostile, "shown as text");
    const visible = [];
    const visit = (node) => {
      assert.equal(Object.hasOwn(node, "innerHTML"), false);
      for (const value of node.attributes.values()) {
        assert.ok(![...hostile, "secret.example", url].some((text) => value.includes(text)), `attribute ${value}`);
      }
      // A rule's own cell shows its text; nothing else does.
      if (node.className !== "rule-text") visible.push(node.textContent);
      node.children.forEach(visit);
    };
    for (const id of [
      "never-save-rules", "save-but-no-mcp-rules", "validation-status", "apply-status", "test-url-result", "never-save-error", "save-but-no-mcp-error",
      "checker-result", "checker-gate", "checker-status", "checker-edit", "checker-never-save-note", "checker-save-but-no-mcp-note",
    ]) visit($(id));
    for (const text of visible) {
      assert.ok(![...hostile, "secret.example", "token=abc"].some((secret) => text.includes(secret)), `text ${text}`);
    }
    assert.ok(host.requests.length > 4);
  });
});

test("the page sends no message that initializes, starts, migrates or presets", async () => {
  const opened = await openPage();
  const { host, $ } = opened;
  for (const edit of Object.values(relaxingEdits)) await edit(opened);
  await addRule(opened, "never-save", pattern("added.example"));
  type($("test-url"), "https://x.example/");
  click($("validate"));
  await flush();
  click($("test-url-run"));
  click($("apply"));
  click($("relax-confirm-yes"));
  await flush();
  const types = new Set(host.types());
  assert.deepEqual([...types].sort(), ["policy_apply", "policy_get", "policy_status", "policy_test_rule", "policy_test_url", "policy_validate"]);
  assert.ok(host.requests.every((message) => typeof message.requestId === "string" && message.requestId !== ""));
});
