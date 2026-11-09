// Issue #51 (Slice B0): the side panel shows whether the vault shown is published to MCP, and the
// user confirms or revokes that for exactly the vault shown (its token). Here the fake host follows
// the real host's rules; the real host is tested in native-host.
import { test } from "node:test";
import assert from "node:assert/strict";
import { loadSidePanel, pages, settle, tokenOf } from "./fake-dom.js";

const decisions = (host) => host.requests.filter((message) => message.type.startsWith("vault_publication_"));

async function openSettings($) {
  $("#tab-settings").dispatch("click");
  await settle(20);
}

test("a pending vault says MCP reads nothing, and confirming takes a review step first", async () => {
  const { $, host } = await loadSidePanel({ vaults: { A: pages("A", 2) }, active: "A" });
  await settle(30);
  await openSettings($);
  assert.equal($("#vault-publication").hidden, false);
  assert.equal($("#vault-publication").dataset.state, "pending");
  assert.equal($("#vault-publication-state").textContent, "MCPへの公開: 未承認");
  assert.match($("#vault-publication-note").textContent, /一切返しません/);
  assert.equal($("#vault-publication-confirm").hidden, false);
  assert.equal($("#vault-publication-revoke").hidden, true);

  // The first click only explains; nothing is sent.
  $("#vault-publication-confirm").dispatch("click");
  await settle(10);
  assert.equal($("#vault-publication-review").hidden, false);
  assert.match($("#vault-publication-review-title").textContent, /プライバシー設定/);
  assert.match($("#vault-publication-review-title").textContent, /対象: A$/);
  assert.deepEqual(decisions(host), []);
  $("#vault-publication-cancel").dispatch("click");
  await settle(10);
  assert.equal($("#vault-publication-review").hidden, true);
  assert.deepEqual(decisions(host), []);
  assert.match($("#vault-message").textContent, /承認していません/);

  $("#vault-publication-confirm").dispatch("click");
  await settle(10);
  $("#vault-publication-approve").dispatch("click");
  await settle(30);
  assert.deepEqual(decisions(host).map(({ type, expectedVaultToken }) => ({ type, expectedVaultToken })), [
    { type: "vault_publication_confirm", expectedVaultToken: tokenOf("A") },
  ]);
  assert.equal($("#vault-publication").dataset.state, "confirmed");
  assert.equal($("#vault-publication-state").textContent, "MCPへの公開: 承認済み（プライバシー設定 改訂4の時点で承認）");
  assert.equal($("#vault-publication-confirm").hidden, true);
  assert.equal($("#vault-publication-revoke").hidden, false);
  assert.match($("#vault-message").textContent, /MCPに公開しました/);

  $("#vault-publication-revoke").dispatch("click");
  await settle(30);
  assert.equal(decisions(host)[1].type, "vault_publication_revoke");
  assert.equal(decisions(host)[1].expectedVaultToken, tokenOf("A"));
  assert.equal($("#vault-publication").dataset.state, "pending");
  assert.match($("#vault-message").textContent, /停止しました/);
});

test("a decision never lands on another vault than the one shown", async () => {
  const { $, host } = await loadSidePanel({ vaults: { A: pages("A", 2), B: pages("B", 1) }, active: "A" });
  await settle(30);
  await openSettings($);
  $("#vault-publication-confirm").dispatch("click");
  await settle(10);
  host.active = "B"; // another window switched while the review was open
  $("#vault-publication-approve").dispatch("click");
  await settle(30);
  assert.equal(decisions(host)[0].expectedVaultToken, tokenOf("A"));
  assert.deepEqual([...host.published], [], "neither A nor B was published");
  assert.match($("#vault-message").textContent, /表示していたVault（A）は、もうActiveではありません。MCPへの公開の設定は変更していません。/);
  assert.equal($("#vault-path").textContent, "B");
  assert.equal($("#vault-publication").dataset.state, "pending", "B is shown, pending");
});

test("without a readable policy nothing is confirmed, and the panel says why", async () => {
  const { $, host } = await loadSidePanel({ vaults: { A: pages("A", 2) }, active: "A" });
  host.policyReady = false;
  await settle(30);
  await openSettings($);
  $("#vault-publication-confirm").dispatch("click");
  await settle(10);
  $("#vault-publication-approve").dispatch("click");
  await settle(30);
  assert.deepEqual([...host.published], []);
  assert.match($("#vault-message").textContent, /プライバシー設定を読み取れない.*承認していません/);
  assert.equal($("#vault-publication").dataset.state, "pending");
});

test("an older host shows no publication controls, and nothing is sent", async () => {
  const { $, host } = await loadSidePanel({ vaults: { A: pages("A", 2) }, active: "A" });
  host.knowsPublication = false;
  await settle(30);
  await openSettings($);
  assert.equal($("#vault-publication").hidden, true);
  $("#vault-publication-confirm").dispatch("click"); // even if a click got through
  await settle(20);
  assert.deepEqual(decisions(host), []);
});

test("without a vault there is nothing to publish", async () => {
  const { $, host } = await loadSidePanel({ vaults: {}, active: null });
  await settle(30);
  await openSettings($);
  assert.equal($("#vault-publication").hidden, true);
  $("#vault-publication-revoke").dispatch("click");
  await settle(20);
  assert.deepEqual(decisions(host), []);
  assert.match($("#vault-message").textContent, /MCPへの公開の設定は変更していません/);
});

test("key and mode refusals keep captured events queued: never dead-lettered or dropped", async () => {
  const { classifyResponse } = await import("../../extension/event-queue.js");
  const event = { type: "visit", id: "e1", url: "https://a.test/", incognito: false };
  for (const code of ["key_unavailable", "vault_locked", "vault_decrypt_failed", "vault_mode_mismatch", "vault_encryption_unsupported", "config_invalid"]) {
    assert.deepEqual(classifyResponse(event, { ok: false, eventId: "e1", error: code }), { kind: "temporary", code }, code);
  }
});

test("only the side panel's own requests decide publication: the service worker relays none", async () => {
  const { readFile } = await import("node:fs/promises");
  const worker = await readFile(new URL("../../extension/service-worker.js", import.meta.url), "utf8");
  assert.equal(worker.includes("vault_publication"), false);
  const panel = await readFile(new URL("../../extension/vault-panel.js", import.meta.url), "utf8");
  assert.match(panel, /vault_publication_confirm/);
});
