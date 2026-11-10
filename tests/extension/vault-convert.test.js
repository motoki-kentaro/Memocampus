// Issue #51 Slice D: converting the vault shown between plain and encrypted from the side panel.
// The source is never changed and nothing becomes active until the user activates the converted
// vault; D1 shows the new vault's Recovery Spell once and needs it entered again in full; D2 needs
// the user's explicit acknowledgement that the new vault is plaintext on disk. The fake host follows
// the real host's rules; the real host is tested in native-host (manage::convert_tests,
// tests/secure_vault_d.rs).
import { test } from "node:test";
import assert from "node:assert/strict";
import { OTHER_SPELL, TEST_SPELL, loadSidePanel, pages, settle, tokenOf } from "./fake-dom.js";
import { CONVERT_SPELL_NOTE, REISSUE_SPELL_NOTE, SPELL_CAUTION, SPELL_LEAD, SPELL_NOTE, convertDirection, convertMakesSpell, describeConversionPending, describeConverted, reissueOffered } from "../../extension/vault-client.js";

const sent = (host, type) => host.requests.filter((message) => message.type === type);
const NEW_SPELL = OTHER_SPELL;
const TARGET = "C:\\Converted\\memocampus-converted.sqlite3";

async function openSettings($) {
  $("#tab-settings").dispatch("click");
  await settle(20);
}

function installClipboard() {
  const writes = [];
  Object.defineProperty(globalThis, "navigator", {
    value: { clipboard: { async writeText(text) { writes.push(text); } } },
    configurable: true,
    writable: true,
  });
  return writes;
}

/** Every text and value the new spell could linger in after its step ends. */
function spellLeftovers($) {
  return ["#vault-spell", "#vault-spell-confirm-input", "#vault-onboarding-path", "#vault-message", "#vault-converted-title"]
    .map((selector) => `${$(selector).textContent}|${$(selector).value}`)
    .filter((text) => text.includes(NEW_SPELL.slice(0, 12)));
}

test("D1: a plain vault is encrypted into a new vault; the new spell is shown once and confirmed in full before the user may activate it", async () => {
  const writes = installClipboard();
  const { $, host } = await loadSidePanel({ vaults: { A: pages("A", 3) }, active: "A", encryption: true, restore: true, convert: true });
  await settle(30);
  await openSettings($);
  assert.equal($("#vault-convert").hidden, false);
  assert.equal($("#vault-convert-title").textContent, "このVaultを暗号化する");
  assert.match($("#vault-convert-note").textContent, /^現在のVaultの内容を、新しい暗号化Vaultへコピーします。元のVaultは削除されません。/);
  assert.match($("#vault-convert-source").textContent, /変換元（このVault・そのまま残ります）: A/);
  assert.equal($("#vault-convert-ack-row").hidden, true, "no plaintext acknowledgement for encrypting");
  assert.equal($("#vault-convert-pick").textContent, "保存先を選んで暗号化…");
  assert.equal($("#vault-convert-pick").disabled, false);

  $("#vault-convert-pick").dispatch("click");
  await settle(30);
  const [start] = sent(host, "vault_convert_start");
  assert.deepEqual({ direction: start.direction, token: start.expectedVaultToken, path: start.path, ack: start.acknowledgePlaintext }, { direction: "encrypt", token: tokenOf("A"), path: undefined, ack: undefined });
  assert.equal(host.active, "A", "nothing switches");
  // The official wording, the new spell, and which vault is which.
  assert.equal($("#vault-onboarding").hidden, false);
  assert.equal($("#vault-onboarding-title").textContent, "ふっかつのじゅもん（Recovery Spell）");
  assert.equal($("#vault-spell-lead").textContent, SPELL_LEAD);
  assert.equal($("#vault-spell-caution").textContent, SPELL_CAUTION);
  assert.equal($("#vault-spell").textContent, NEW_SPELL);
  assert.match($("#vault-onboarding-path").textContent, /変換先（新しい暗号化Vault）: C:\\Converted\\memocampus-converted\.sqlite3/);
  assert.match($("#vault-onboarding-path").textContent, /変換元（そのまま残ります）: A/);
  assert.ok($("#vault-onboarding-path").textContent.includes(CONVERT_SPELL_NOTE));
  assert.equal($("#vault-spell-confirm").textContent, "確認して変換を完了する");
  assert.match($("#vault-onboarding-cancel").textContent, /変換をやめる/);
  assert.equal($("#vault-convert-pick").disabled, true, "one conversion at a time");
  assert.equal($("#vault-restore-pick").disabled, true, "no restore beside it");
  assert.equal($("#vault-converted").hidden, true, "the decision comes after the spell");

  $("#vault-spell-copy").dispatch("click");
  await settle(10);
  assert.deepEqual(writes, [NEW_SPELL]);
  $("#vault-spell-written").dispatch("click");
  await settle(10);
  assert.equal($("#vault-spell").textContent, "");
  // The spell of another vault (or anything else): nothing is sent.
  for (const typed of [TEST_SPELL, NEW_SPELL.slice(0, 42), ""]) {
    $("#vault-spell-confirm-input").value = typed;
    $("#vault-spell-confirm").dispatch("click");
    await settle(20);
    assert.equal(sent(host, "vault_convert_confirm").length, 0, typed);
    assert.match($("#vault-message").textContent, /変換はまだ完了していません。元のVaultは変更していません/);
  }
  $("#vault-spell-confirm-input").value = ` ${NEW_SPELL} `;
  $("#vault-spell-confirm").dispatch("click");
  await settle(40);
  assert.deepEqual(sent(host, "vault_convert_confirm").map(({ recoverySpell }) => recoverySpell), [NEW_SPELL]);
  assert.equal(host.active, "A", "still not active");
  assert.equal($("#vault-onboarding").hidden, true);
  assert.deepEqual(spellLeftovers($), [], "the spell is forgotten once confirmed");
  assert.equal($("#vault-spell-confirm").textContent, "確認して作成を完了する", "the creation's labels are back");
  // The decision: what was converted, where from, verified counts.
  assert.equal($("#vault-converted").hidden, false);
  assert.match($("#vault-converted-title").textContent, /変換しました（まだ使用していません）: C:\\Converted/);
  assert.match($("#vault-converted-title").textContent, /変換元（そのまま残ります）: A/);
  assert.match($("#vault-converted-title").textContent, /検証済み: 閲覧 3件/);
  assert.equal($("#vault-convert-activate").hidden, false);
  assert.equal($("#vault-convert-keep").hidden, false);

  $("#vault-convert-activate").dispatch("click");
  await settle(40);
  assert.deepEqual(sent(host, "vault_convert_activate").map(({ expectedVaultToken }) => expectedVaultToken), [tokenOf(TARGET)]);
  assert.equal(host.active, TARGET);
  assert.match($("#vault-message").textContent, /変換したVaultを使い始めました[\s\S]*変換元のVaultは削除していません[\s\S]*未承認/);
  assert.equal($("#vault-converted").hidden, true);
  assert.equal($("#vault-publication-state").textContent, "MCPへの公開: 未承認");
  // The spell travelled only in the start answer and the one confirmation; never to the worker.
  const carriers = host.requests.filter((message) => JSON.stringify(message).includes(NEW_SPELL));
  assert.deepEqual(carriers.map(({ type }) => type), ["vault_convert_confirm"]);
  assert.ok(!JSON.stringify(host.workerMessages ?? []).includes(NEW_SPELL.slice(0, 12)));
  // Now encrypted: the panel offers the other direction.
  assert.equal($("#vault-convert-title").textContent, "このVaultの暗号化を解除する");
});

test("D1: cancelling at the spell discards the converted vault and forgets the spell", async () => {
  installClipboard();
  const { $, host } = await loadSidePanel({ vaults: { A: pages("A", 1) }, active: "A", encryption: true, restore: true, convert: true });
  await settle(30);
  await openSettings($);
  $("#vault-convert-pick").dispatch("click");
  await settle(30);
  assert.equal($("#vault-spell").textContent, NEW_SPELL);
  $("#vault-onboarding-cancel").dispatch("click");
  await settle(40);
  assert.equal(sent(host, "vault_convert_discard").length, 1);
  assert.equal(sent(host, "vault_create_encrypted_cancel").length, 0);
  assert.equal(host.conversion, null);
  assert.equal(host.active, "A");
  assert.equal($("#vault-onboarding").hidden, true);
  assert.deepEqual(spellLeftovers($), []);
  assert.match($("#vault-message").textContent, /破棄しました。元のVaultとActive Vaultは変更していません/);
  assert.equal($("#vault-convert-pick").disabled, false);
});

test("D2: decrypting needs the plaintext acknowledgement, given for one request only; the user may keep the copy without switching", async () => {
  const { $, host } = await loadSidePanel({ vaults: { E: pages("E", 2) }, active: "E", encryption: true, restore: true, convert: true });
  host.encrypted.add("E");
  await settle(30);
  await openSettings($);
  assert.equal($("#vault-convert-title").textContent, "このVaultの暗号化を解除する");
  assert.match(
    $("#vault-convert-note").textContent,
    /^新しい平文Vaultを作成します。閲覧履歴や保存画像などが暗号化されずにディスクへ保存されます。元の暗号化Vaultは削除されません。/,
  );
  assert.equal($("#vault-convert-ack-row").hidden, false);
  assert.equal($("#vault-convert-ack").checked, false);
  assert.equal($("#vault-convert-pick").disabled, true, "not without the acknowledgement");
  assert.equal($("#vault-convert-pick").textContent, "保存先を選んで暗号化を解除…");
  $("#vault-convert-pick").dispatch("click");
  await settle(20);
  assert.equal(sent(host, "vault_convert_start").length, 0);
  assert.match($("#vault-message").textContent, /確認のチェックを入れてから/);

  $("#vault-convert-ack").checked = true;
  $("#vault-convert-ack").dispatch("change");
  assert.equal($("#vault-convert-pick").disabled, false);
  $("#vault-convert-pick").dispatch("click");
  await settle(40);
  const [start] = sent(host, "vault_convert_start");
  assert.equal(start.direction, "decrypt");
  assert.equal(start.acknowledgePlaintext, true);
  assert.equal(start.expectedVaultToken, tokenOf("E"));
  assert.equal(start.recoverySpell, undefined);
  assert.equal($("#vault-convert-ack").checked, false, "acknowledged for that one request");
  assert.equal($("#vault-onboarding").hidden, true, "no spell for a plain vault");
  assert.equal($("#vault-converted").hidden, false);
  assert.match($("#vault-converted-title").textContent, /新しい平文Vault（暗号化されていません）/);
  assert.match($("#vault-converted-title").textContent, /変換元（そのまま残ります）: E/);
  assert.equal(host.active, "E");

  $("#vault-convert-keep").dispatch("click");
  await settle(40);
  assert.equal(sent(host, "vault_convert_keep").length, 1);
  assert.equal(host.active, "E", "kept, not switched");
  assert.match($("#vault-message").textContent, /切り替えずに保存しました[\s\S]*Active Vaultは変更していません/);
  assert.equal($("#vault-converted").hidden, true);
});

test("a conversion recorded by the host is shown after the panel reopens: an unconfirmed spell can only be discarded", async () => {
  const { $, host } = await loadSidePanel({ vaults: { A: pages("A", 1) }, active: "A", encryption: true, restore: true, convert: true });
  host.conversion = { direction: "encrypt", phase: "verified", path: TARGET, sourcePath: "A", vaultToken: tokenOf(TARGET), spell: NEW_SPELL, pages: [] };
  await settle(30);
  await openSettings($);
  assert.equal($("#vault-converted").hidden, false);
  assert.equal($("#vault-converted-title").textContent, describeConversionPending(host.status().conversionPending));
  assert.match($("#vault-converted-title").textContent, /呪文は再表示できないため、破棄してから/);
  assert.equal($("#vault-convert-activate").hidden, true);
  assert.equal($("#vault-convert-keep").hidden, true);
  assert.equal($("#vault-convert-pick").disabled, true);
  assert.equal($("#vault-restore-pick").disabled, true);
  assert.ok(!$("#vault-converted-title").textContent.includes(NEW_SPELL));
  $("#vault-convert-discard").dispatch("click");
  await settle(40);
  assert.equal(sent(host, "vault_convert_discard").length, 1);
  assert.equal($("#vault-converted").hidden, true);
  assert.equal($("#vault-convert-pick").disabled, false);

  // A converted one waits for the decision: it can be activated.
  host.conversion = { direction: "decrypt", phase: "converted", path: TARGET, sourcePath: "A", vaultToken: tokenOf(TARGET), spell: null, pages: [] };
  $("#tab-url").dispatch("click");
  await settle(10);
  await openSettings($);
  assert.equal($("#vault-convert-activate").hidden, false);
  $("#vault-convert-activate").dispatch("click");
  await settle(40);
  assert.equal(host.active, TARGET);
});

test("an older host (no conversions) offers none; texts and directions", async () => {
  const { $ } = await loadSidePanel({ vaults: { A: pages("A", 1) }, active: "A", encryption: true, restore: true });
  await settle(30);
  await openSettings($);
  assert.equal($("#vault-convert").hidden, true);
  assert.equal(convertDirection("plain"), "encrypt");
  assert.equal(convertDirection("encrypted"), "decrypt");
  assert.equal(convertDirection(undefined), null);
  const text = describeConverted({ direction: "encrypt", path: "P", sourcePath: "S", counts: { visits: 1, snapshots: 2, images: 3 } });
  assert.match(text, /新しい暗号化Vault/);
  assert.match(text, /閲覧 1件・スナップショット 2件・画像 3件/);
  assert.match(describeConversionPending({ phase: "converting", path: "P" }), /中断された変換/);
});

// Issue #51 Slice E: re-issuing the Recovery Spell of an encrypted vault makes a new vault under a
// new spell; the old spell is not revoked, and the panel says so.
test("re-issuing the spell: offered for an encrypted vault only, a new spell shown once and confirmed in full, the old spell not revoked", async () => {
  const writes = installClipboard();
  const plain = await loadSidePanel({ vaults: { A: pages("A", 1) }, active: "A", encryption: true, restore: true, convert: true });
  await settle(30);
  await openSettings(plain.$);
  assert.equal(plain.$("#vault-reissue").hidden, true, "a plain vault has no spell to re-issue");
  const { $, host } = await loadSidePanel({ vaults: { E: pages("E", 2) }, active: "E", encryption: true, restore: true, convert: true });
  host.encrypted.add("E");
  await settle(30);
  await openSettings($);
  assert.equal($("#vault-reissue").hidden, false);
  assert.equal($("#vault-reissue-title").textContent, "ふっかつのじゅもんを再発行する");
  assert.match($("#vault-reissue-note").textContent, /古い呪文は失効しません/);
  assert.match($("#vault-reissue-note").textContent, /元のVaultは削除されません/);
  assert.equal($("#vault-reissue-pick").disabled, false);

  $("#vault-reissue-pick").dispatch("click");
  await settle(30);
  const [start] = sent(host, "vault_convert_start");
  assert.deepEqual({ direction: start.direction, token: start.expectedVaultToken, ack: start.acknowledgePlaintext }, { direction: "reissue", token: tokenOf("E"), ack: undefined });
  assert.equal(host.active, "E", "nothing switches");
  assert.equal($("#vault-spell").textContent, NEW_SPELL);
  assert.ok($("#vault-onboarding-path").textContent.includes(CONVERT_SPELL_NOTE));
  assert.ok($("#vault-onboarding-path").textContent.includes(REISSUE_SPELL_NOTE));
  assert.equal($("#vault-reissue-pick").disabled, true, "one at a time");
  assert.equal($("#vault-convert-pick").disabled, true);
  $("#vault-spell-copy").dispatch("click");
  await settle(10);
  assert.deepEqual(writes, [NEW_SPELL]);
  $("#vault-spell-written").dispatch("click");
  await settle(10);
  $("#vault-spell-confirm-input").value = NEW_SPELL;
  $("#vault-spell-confirm").dispatch("click");
  await settle(40);
  assert.deepEqual(sent(host, "vault_convert_confirm").map(({ recoverySpell }) => recoverySpell), [NEW_SPELL]);
  assert.deepEqual(spellLeftovers($), []);
  assert.match($("#vault-converted-title").textContent, /再発行しました（まだ使用していません）/);
  assert.match($("#vault-converted-title").textContent, /古い呪文では開けません/);
  assert.ok($("#vault-converted-title").textContent.includes(REISSUE_SPELL_NOTE));
  $("#vault-convert-activate").dispatch("click");
  await settle(40);
  assert.equal(host.active, TARGET);
  assert.equal($("#vault-publication-state").textContent, "MCPへの公開: 未承認");
  assert.equal($("#vault-reissue").hidden, false, "the new vault is encrypted too");
});

test("Slice E texts: the spell and the vault file are all a recovery needs; earlier schemes are named, never taken for a wrong spell", async () => {
  assert.ok(!SPELL_NOTE.includes(".mckey"));
  assert.match(SPELL_NOTE, /暗号化Vaultのファイル1個（またはそのバックアップ）があれば/);
  assert.match(SPELL_NOTE, /ふっかつのじゅもんを再発行する/);
  assert.equal(convertMakesSpell("encrypt"), true);
  assert.equal(convertMakesSpell("reissue"), true);
  assert.equal(convertMakesSpell("decrypt"), false);
  assert.equal(reissueOffered("encrypted"), true);
  assert.equal(reissueOffered("plain"), false);
  assert.match(describeConversionPending({ direction: "reissue", phase: "verified", path: "C:/x" }), /呪文は再表示できない/);
  assert.match(describeConversionPending({ direction: "reissue", phase: "converted", path: "C:/x" }), /新しいふっかつのじゅもんのVault/);
  const { describeErrorCode } = await import("../../extension/search-client.js");
  assert.match(describeErrorCode("vault_legacy_unsupported"), /以前の鍵管理方式/);
  assert.match(describeErrorCode("backup_legacy_unsupported"), /以前の鍵管理方式/);
});
