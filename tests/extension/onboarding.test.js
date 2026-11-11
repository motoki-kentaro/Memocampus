import { test } from "node:test";
import assert from "node:assert/strict";
import { SearchError } from "../../extension/search-client.js";
import { fakeHost, openPage, v2, check, click, flush, validateAnswer } from "./options-fixtures.js";

function firstRun({ vault = true, lost = false, invalid = false } = {}) {
  const host = fakeHost({ policy: v2({}, { revision: 1 }) });
  host.state = "uninitialized";
  host.vault = vault;
  host.answers.policy_status = () => ({ ok: true, state: host.state });
  host.answers.policy_preset = () => ({ ok: true, presetId: "default", schemaVersion: 2, engineVersion: 2, policy: v2(invalid ? { neverSave: [{ kind: "regex", pattern: "bad(" }] } : {}, { revision: 1 }) });
  host.answers.vault_status = () => ({ ok: true, type: "vault_result", status: { state: host.vault ? "ready" : "not_configured" } });
  host.answers.policy_start = (message) => {
    if (host.state !== "uninitialized") return { ok: false, error: "policy_already_initialized" };
    if (!host.vault) return { ok: false, error: "vault_not_configured" };
    const checked = validateAnswer(message.policy);
    if (!checked.ok) return checked;
    host.policy = { ...structuredClone(message.policy), revision: 1 };
    host.writes += 1;
    host.state = "ready";
    if (lost) throw new SearchError("host_unavailable");
    return { ok: true, state: "ready", etag: host.etag, revision: 1 };
  };
  return host;
}

test("first-run preview is a host preset, editable only as a draft; preview/validate never starts", async () => {
  const host = firstRun();
  const { page, $ } = await openPage(host);
  assert.equal($("onboarding").hidden, false);
  assert.equal(page.view.state, "uninitialized");
  assert.equal($("policy-revision").textContent, "—");
  assert.equal($("apply").disabled, true);
  assert.equal($("start-recording").disabled, false);
  check($("category-webmail"), false);
  click($("validate"));
  await flush();
  assert.equal(page.draft.category("webmail"), false);
  assert.equal(host.ofType("policy_preset").length, 1);
  assert.equal(host.ofType("policy_start").length, 0);
  assert.equal(host.writes, 0);
  assert.equal(host.allTypes().some((type) => /vault_(create|open|publication_confirm)/.test(type)), false);
});

test("no vault and invalid drafts cannot start; an existing usable vault is reused", async () => {
  const noVault = firstRun({ vault: false });
  const a = await openPage(noVault);
  assert.equal(a.$("start-recording").disabled, true);
  click(a.$("start-recording"));
  await flush();
  assert.equal(noVault.ofType("policy_start").length, 0);
  noVault.vault = true;
  click(a.$("vault-recheck"));
  await flush();
  assert.equal(a.$("start-recording").disabled, false);
  const bad = firstRun({ invalid: true });
  const b = await openPage(bad);
  click(b.$("start-recording"));
  await flush();
  assert.equal(bad.ofType("policy_start").length, 0);
  assert.equal(bad.writes, 0);
  assert.match(b.$("apply-status").textContent, /検証できない/);
});

test("double clicks submit exactly one validated draft; a lost start reply is reconciled by reads", async () => {
  for (const lost of [false, true]) {
    const host = firstRun({ lost });
    const { page, $ } = await openPage(host);
    check($("category-aiChats"), false);
    const release = host.hold("policy_start");
    click($("start-recording"));
    click($("start-recording"));
    await flush();
    assert.equal($("category-aiChats").disabled, true);
    assert.equal(host.ofType("policy_start").length, 1);
    release();
    await flush();
    assert.equal(page.view.state, "ready");
    assert.equal($("onboarding").hidden, true);
    assert.equal(host.writes, 1);
    assert.equal(host.policy.categories.aiChats, false);
    assert.equal(host.ofType("policy_start").length, 1);
    assert.equal(host.ofType("vault_publication_confirm").length, 0);
  }
});

test("competing pages retain the winner and never apply their draft over it", async () => {
  const host = firstRun();
  const a = await openPage(host);
  const b = await openPage(host);
  check(a.$("category-webmail"), false);
  const release = host.hold("policy_start");
  click(a.$("start-recording"));
  await flush();
  click(b.$("start-recording"));
  await flush();
  release();
  await flush();
  assert.equal(host.writes, 1);
  assert.equal(host.policy.categories.webmail, true);
  assert.equal(a.page.view.state, "ready");
  assert.equal(b.page.view.state, "ready");
  assert.equal(host.ofType("policy_apply").length, 0);
});

test("a disconnected or ambiguous start blocks resending until current state is read", async () => {
  const host = firstRun();
  const { page, $ } = await openPage(host);
  host.answers.policy_start = () => {
    host.answers.policy_status = () => { throw new SearchError("host_unavailable"); };
    throw new SearchError("host_unavailable");
  };
  click($("start-recording"));
  await flush();
  assert.equal($("start-recording").disabled, true);
  assert.equal($("start-recheck").hidden, false);
  click($("start-recording"));
  await flush();
  assert.equal(host.ofType("policy_start").length, 1);
  host.answers.policy_status = () => ({ ok: true, state: "uninitialized" });
  click($("start-recheck"));
  await flush();
  assert.equal(page.view.state, "uninitialized");
  assert.equal($("start-recording").disabled, false);
  assert.equal(host.ofType("policy_start").length, 1);
});

test("existing and corrupt policies never fetch a preset or offer start; corruption during start stays closed", async () => {
  for (const state of ["ready", "invalid", "unsupported"]) {
    const host = firstRun();
    host.state = state;
    const { $ } = await openPage(host);
    assert.equal($("onboarding").hidden, true);
    assert.equal(host.ofType("policy_preset").length, 0);
    assert.equal(host.ofType("policy_start").length, 0);
  }
  const host = firstRun();
  const { page, $ } = await openPage(host);
  host.state = "invalid";
  click($("start-recording"));
  await flush();
  assert.equal(page.view.state, "invalid");
  assert.equal(page.draft, null);
  assert.equal($("onboarding").hidden, true);
  assert.equal(host.ofType("policy_start").length, 0);
});
