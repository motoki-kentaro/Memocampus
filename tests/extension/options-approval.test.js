// Approving a domain the Public Suffix List restricts, on the Privacy settings page. Synthetic policies,
// domains and URLs only. The fake host scripts which domains are restricted: the page judges none
// itself, and an approval adds no rule, applies nothing and skips no check.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import path from "node:path";
import { extensionDir } from "./fake-dom.js";
import { SearchError } from "../../extension/search-client.js";
import { matchState } from "../../extension/options-state.js";
import { addRule, checkRule, click, flush, guarded, html, htmlText, openPage, v2 } from "./options-fixtures.js";

const domain = (text) => ({ kind: "domainWildcard", pattern: text });
const URL = "https://shop.co.jp/item";
const addButtons = ($) => [$("checker-add-never-save"), $("checker-add-save-but-no-mcp")];
const approvalRows = ($) => $("approvals-list").children.map((li) => ({ domain: li.children[0].textContent, state: li.children[1].textContent, button: li.children[2], note: li.children[3] }));

async function restrictedPage(options = {}) {
  const opened = await openPage(options);
  opened.host.restricted["*.co.jp"] = { domain: "co.jp", reason: "public_suffix" };
  opened.host.restricted["*.example.newtld"] = { domain: "example.newtld", reason: "unknown_suffix" };
  opened.host.restricted["*.kawasaki.jp"] = { domain: "kawasaki.jp", reason: "delegated_boundary" };
  opened.host.restricted["*.www.example.com"] = { domain: "www.example.com", reason: "not_registrable" };
  return opened;
}

test("the approval offer and the approved domains have their own place on the page", () => {
  assert.match(html, /<div id="checker-approval" class="checker-approval" role="group" aria-labelledby="checker-approval-title" hidden>/);
  assert.equal(htmlText("checker-approve"), "このドメインを承認する");
  assert.match(htmlText("checker-approval-effect"), /^制限を理解したうえで、このドメインを承認できます。承認はこのドメインだけに有効で、このドメインを指定する今後のDomain Wildcardルールにも使われます。承認だけではルールは追加されず/);
  const order = ["checker-heading", "never-save-heading", "url-test-heading", "approvals-heading", "manage-heading"];
  const at = order.map((id) => html.indexOf(`id="${id}"`));
  assert.ok(at.every((position, index) => position > 0 && (index === 0 || position > at[index - 1])), JSON.stringify(at));
  assert.match(htmlText("approvals-description"), /承認はルールではなく、ルールの一覧とは別に管理します/);
});

for (const [pattern, reason, text] of [
  ["*.co.jp", "public_suffix", /^この指定は、複数の登録者が利用する可能性がある広いドメイン範囲に一致します。2026-10-22時点のPublic Suffix Listでは、「co\.jp」は公開サフィックス/],
  ["*.kawasaki.jp", "delegated_boundary", /^この指定は、複数の登録者が利用する可能性がある広いドメイン範囲に一致します。2026-10-22時点のPublic Suffix Listでは、「kawasaki\.jp」の配下に公開サフィックスがあります/],
  ["*.example.newtld", "unknown_suffix", /^2026-10-22時点のPublic Suffix Listでは、このドメイン（example\.newtld）の登録境界を確認できません/],
  ["*.www.example.com", "not_registrable", /^2026-10-22時点のPublic Suffix Listでは、「www\.example\.com」は登録可能なドメインそのものではなく/],
]) {
  test(`a ${reason} restriction offers an approval of the host's domain, and nothing can be added`, async () => {
    const { host, $, page } = await restrictedPage();
    await checkRule($, domain(pattern), URL);
    assert.deepEqual(host.types().slice(-2), ["policy_test_rule", "policy_domain_check"], "the reason comes from the host");
    assert.deepEqual(host.ofType("policy_domain_check")[0].rule, domain(pattern));
    assert.equal($("checker-result").dataset.result, "approval_needed");
    assert.equal($("checker-result").textContent, "承認が必要です（Public Suffix Listによる制限）");
    assert.equal($("checker-approval").hidden, false);
    assert.match($("checker-approval-reason").textContent, text);
    assert.equal($("checker-approval-domain").textContent, `承認するドメイン: ${host.restricted[pattern].domain}`);
    assert.equal($("checker-approve").disabled, false);
    for (const button of addButtons($)) {
      assert.equal(button.disabled, true);
      click(button);
    }
    assert.equal(page.draft.dirty(), false);
  });
}

test("a syntax error offers no approval, whatever the host would restrict", async () => {
  const { host, $ } = await restrictedPage();
  host.answers.policy_test_rule = () => ({ ok: false, error: "invalid_domain_wildcard" });
  await checkRule($, domain("*.co.jp/*"), URL);
  assert.equal($("checker-result").dataset.result, "rule_error");
  assert.match($("checker-result").textContent, /ドメインのみを指定してください/);
  assert.equal($("checker-approval").hidden, true);
  assert.equal($("checker-approve").disabled, true);
  assert.equal(host.ofType("policy_domain_check").length, 0);
  assert.equal(matchState({ ok: false, code: "domain_wildcard_not_approved" }), "approval_needed");
  assert.equal(matchState({ ok: false, code: "psl_unavailable" }), "failed", "an unreadable list is no restriction");
});

test("approving stores the domain the host named, adds no rule, and a new check is needed", async () => {
  await guarded(async () => {
    const opened = await restrictedPage();
    const { page, host, $ } = opened;
    await checkRule($, domain("*.CO.JP"), URL);
    host.restricted["*.CO.JP"] = host.restricted["*.co.jp"];
    await checkRule($, domain("*.CO.JP"), URL);
    click($("checker-approve"));
    click($("checker-approve"));
    await flush();
    assert.deepEqual(host.ofType("policy_domain_approve").map(({ rule, domain: named }) => ({ rule, named })), [{ rule: domain("*.CO.JP"), named: "co.jp" }], "once, for the domain shown");
    assert.deepEqual(Object.keys(host.approved), ["co.jp"]);
    assert.equal($("checker-result").textContent, "未チェック", "the approval is no match");
    assert.match($("checker-status").textContent, /「co\.jp」を承認しました。ルールはまだ追加されていません。もう一度「マッチングを確認」/);
    for (const button of addButtons($)) assert.equal(button.disabled, true);
    assert.equal(page.draft.dirty(), false, "nothing added");
    assert.equal(host.ofType("policy_apply").length, 0, "nothing applied");
    assert.equal(host.writes, 0);
    assert.deepEqual(approvalRows($).map((row) => row.domain), ["co.jp"], "the list is read again");

    // Now the host matches it, and the rule is added through the gate as any other.
    click($("checker-run"));
    await flush();
    assert.equal($("checker-result").textContent, "一致しました");
    click($("checker-add-save-but-no-mcp"));
    assert.deepEqual(page.draft.layer("saveButNoMcp").rules.at(-1), domain("*.CO.JP"));
    click($("apply"));
    await flush();
    assert.deepEqual(host.policy.layers.saveButNoMcp.rules.at(-1), domain("*.CO.JP"));
    assert.equal(host.ofType("policy_validate").length, 1);
  });
});

test("an approval answer that changes or fails leaves the check as it was and approves nothing", async () => {
  const { host, $ } = await restrictedPage();
  for (const [answer, text] of [
    [() => ({ ok: false, error: "domain_mismatch" }), /承認しませんでした。確認したドメインと一致しない/],
    [() => ({ ok: false, error: "approvals_unavailable" }), /承認しませんでした。承認したドメインの情報を読み書きできません/],
    [() => { throw new SearchError("host_unavailable"); }, /承認しませんでした。Native Hostに接続できません/],
    [() => ({ ok: true, domain: "ne.jp", approvedAt: "x" }), /承認しませんでした。Native Hostから不正な応答/],
  ]) {
    host.answers.policy_domain_approve = answer;
    await checkRule($, domain("*.co.jp"), URL);
    click($("checker-approve"));
    await flush();
    assert.match($("checker-status").textContent, text);
    assert.equal($("checker-result").dataset.result, "approval_needed");
    assert.equal($("checker-approve").disabled, false);
  }
  assert.deepEqual(host.approved, {});
});

test("a change of the input drops the offer, and an approval answer after it enables nothing", async () => {
  const { host, $, page } = await restrictedPage();
  await checkRule($, domain("*.co.jp"), URL);
  $("checker-rule").value = "*.co.jpx";
  $("checker-rule").dispatch("input");
  assert.equal($("checker-approval").hidden, true, "the offer was about the text before");
  click($("checker-approve"));
  await flush();
  assert.equal(host.ofType("policy_domain_approve").length, 0);

  // A value changed without an input event (a script) is caught on the click as well.
  await checkRule($, domain("*.co.jp"), URL);
  $("checker-rule").value = "*.ne.jp";
  click($("checker-approve"));
  await flush();
  assert.equal(host.ofType("policy_domain_approve").length, 0);
  assert.equal($("checker-approval").hidden, true, "the offer is drawn for what the checker holds");

  await checkRule($, domain("*.co.jp"), URL);
  const release = host.hold("policy_domain_approve");
  click($("checker-approve"));
  await flush();
  assert.equal($("checker-approve").disabled, true, "one request at a time");
  $("checker-url").value = "https://other.co.jp/";
  $("checker-url").dispatch("input");
  release();
  await flush();
  assert.equal($("checker-result").textContent, "未チェック");
  for (const button of addButtons($)) assert.equal(button.disabled, true);
  assert.equal(page.draft.dirty(), false);
});

test("a check the host cannot explain after refusing the domain offers nothing", async () => {
  const { host, $ } = await restrictedPage();
  host.answers.policy_domain_check = () => { throw new SearchError("timeout"); };
  await checkRule($, domain("*.co.jp"), URL);
  assert.equal($("checker-result").dataset.result, "failed");
  assert.equal($("checker-approval").hidden, true);
  host.answers.policy_domain_check = () => ({ ok: true, status: "restricted", domain: "co.jp", reason: "public_suffix", approved: true, psl: { source: "s", sourceCommit: "c", snapshotDate: "d" } });
  await checkRule($, domain("*.co.jp"), "https://shop.co.jp/other");
  assert.equal($("checker-result").dataset.result, "failed", "approved already, yet refused: no offer");
  host.answers.policy_domain_check = () => ({ ok: true, status: "restricted", domain: "co.jp", reason: "made_up", approved: false, psl: { source: "s", sourceCommit: "c", snapshotDate: "d" } });
  await checkRule($, domain("*.co.jp"), "https://shop.co.jp/third");
  assert.equal($("checker-approval").hidden, true, "an unknown reason is no answer");
});

test("approved domains are listed with their use, and one the active policy uses cannot be revoked here", async () => {
  const opened = await restrictedPage();
  const { host, $, page } = opened;
  host.approved = { "co.jp": "2026-11-01T00:00:00.000Z", "example.newtld": "2026-11-02T00:00:00.000Z" };
  host.inUse = new Set(["co.jp"]);
  await page.reload();
  await flush();
  const [used, free] = approvalRows($);
  assert.equal(used.domain, "co.jp");
  assert.match(used.state, /承認済み（2026-11-01T00:00:00\.000Z）/);
  assert.equal(used.button.disabled, true);
  assert.match(used.note.textContent, /適用中の設定で使われています。取り消すには、先にこのドメインのルールを削除または変更して適用してください/);
  assert.equal(used.button.getAttribute("aria-label"), "co.jp の承認を取り消す");
  assert.equal(free.button.disabled, false);
  assert.equal(free.note.hidden, true);
  assert.equal($("approvals-psl").textContent, "判定に使うPublic Suffix List: 2026-10-22時点");

  click(free.button);
  await flush();
  assert.deepEqual(host.ofType("policy_domain_revoke").map((m) => m.domain), ["example.newtld"]);
  assert.deepEqual(approvalRows($).map((row) => row.domain), ["co.jp"]);
  assert.match($("approvals-status").textContent, /「example\.newtld」の承認を取り消しました/);
  assert.equal(page.draft.dirty(), false, "a revocation is no rule change");

  // The host refuses one in use even if the page offered it.
  host.inUse = new Set(["co.jp"]);
  host.answers.policy_domain_approvals = () => ({ ok: true, approvals: [{ domain: "co.jp", approvedAt: "x", inUse: null }], psl: { source: "s", sourceCommit: "c", snapshotDate: "2026-10-22" } });
  await page.reload();
  await flush();
  assert.match(approvalRows($)[0].note.textContent, /確認できません/);
  click(approvalRows($)[0].button);
  await flush();
  assert.match($("approvals-status").textContent, /取り消しませんでした。適用中の設定でこのドメインのルールが使われているため/);
  assert.ok("co.jp" in host.approved);
});

test("a list that cannot be read says so and offers nothing to revoke", async () => {
  const { host, $, page } = await restrictedPage();
  host.answers.policy_domain_approvals = () => ({ ok: false, error: "approvals_unavailable" });
  await page.reload();
  await flush();
  assert.match($("approvals-state").textContent, /承認したドメインを読み込めませんでした。承認したドメインの情報を読み書きできません/);
  assert.equal($("approvals-list").children.length, 0);
  host.answers.policy_domain_approvals = () => ({ ok: true, approvals: [{ domain: "co.jp" }], psl: { snapshotDate: "d", source: "s", sourceCommit: "c" } });
  await page.reload();
  await flush();
  assert.match($("approvals-state").textContent, /読み込めませんでした。Native Hostから不正な応答/);
});

test("a draft rule whose approval is gone is named by the host's problem", async () => {
  const opened = await openPage({ policy: v2({ neverSave: [], saveButNoMcp: [] }) });
  const { host, $, controls } = opened;
  host.answers.policy_validate = () => ({ ok: false, error: "policy_invalid", errors: [{ layer: "neverSave", ruleIndex: 0, code: "domain_wildcard_not_approved" }] });
  await addRule(opened, "never-save", domain("*.co.jp"), URL);
  click($("apply"));
  await flush();
  assert.equal(host.ofType("policy_apply").length, 0);
  assert.match(controls("never-save", 0).error.textContent, /Public Suffix Listによる制限があり、承認されていません/);
});

test("approving is locked while applying, like every change", async () => {
  const opened = await restrictedPage();
  const { host, $ } = opened;
  await addRule(opened, "never-save", domain("*.example.com"), URL);
  await checkRule($, domain("*.co.jp"), URL);
  const release = host.hold("policy_apply");
  click($("apply"));
  await flush();
  assert.equal($("checker-approve").disabled, true);
  click($("checker-approve"));
  release();
  await flush();
  assert.equal(host.ofType("policy_domain_approve").length, 0);
});

test("only the settings page asks about approved domains", () => {
  const settings = new Set(["options-page.js", "options-state.js", "options.js"]);
  for (const file of readdirSync(extensionDir).filter((name) => name.endsWith(".js"))) {
    const source = readFileSync(path.join(extensionDir, file), "utf8");
    if (!settings.has(file)) assert.doesNotMatch(source, /policy_domain_/, `${file} never asks for approvals`);
  }
  const worker = readFileSync(path.join(extensionDir, "service-worker.js"), "utf8");
  assert.doesNotMatch(worker, /options-(page|state)\.js/);
});
