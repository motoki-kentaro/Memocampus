// Issue #51: creating an encrypted vault from the side panel (encryption is the default, the
// Recovery Spell — ふっかつのじゅもん — is shown once and confirmed by entering all 43 characters
// again before anything becomes active), and recovering an encrypted vault with its spell. The fake
// host follows the real host's rules; the real host is tested in native-host
// (tests/secure_vault_b.rs).
import { test } from "node:test";
import assert from "node:assert/strict";
import { OTHER_SPELL, TEST_SPELL, loadSidePanel, pages, settle } from "./fake-dom.js";
import { SPELL_CAUTION, SPELL_LEAD, checkSpell, describeSpellProblem, normalizeSpellInput, spellMatches } from "../../extension/vault-client.js";

const sent = (host, type) => host.requests.filter((message) => message.type === type);

async function openSettings($) {
  $("#tab-settings").dispatch("click");
  await settle(20);
}

/** Replaces the navigator for one test: records what the panel puts on the clipboard. */
function installClipboard({ fail = false } = {}) {
  const writes = [];
  const clipboard = {
    async writeText(text) {
      if (fail) throw Object.assign(new Error("Document is not focused."), { name: "NotAllowedError" });
      writes.push(text);
    },
  };
  Object.defineProperty(globalThis, "navigator", { value: { clipboard }, configurable: true, writable: true });
  return writes;
}

/** Every text and value the spell could linger in, after the onboarding ends. */
function spellLeftovers($) {
  return ["#vault-spell", "#vault-spell-confirm-input", "#vault-onboarding-path", "#vault-message", "#vault-recovery-input", "#vault-backup-spell", "#vault-restore-spell"]
    .map((selector) => `${$(selector).textContent}|${$(selector).value}`)
    .filter((text) => text.includes(TEST_SPELL.slice(0, 12)));
}

test("encryption is the default: the spell is shown once, entered again in full, and only then is the vault active", async () => {
  const writes = installClipboard();
  const { $, host } = await loadSidePanel({ vaults: { A: pages("A", 1) }, active: "A", encryption: true });
  await settle(30);
  await openSettings($);
  assert.equal($("#vault-encryption").hidden, false);
  assert.equal($("#vault-encryption-encrypted").checked, true);
  assert.equal($("#vault-encryption-encrypted").disabled, false);
  assert.match($("#vault-encryption-note").textContent, /ふっかつのじゅもん（Recovery Spell）/);
  assert.doesNotMatch($("#vault-encryption-note").textContent, /12単語|リカバリーフレーズ/);

  $("#vault-create-default").dispatch("click");
  await settle(30);
  assert.equal(sent(host, "vault_create").length, 0);
  assert.deepEqual(sent(host, "vault_create_encrypted").map(({ useDefault }) => useDefault), [true]);
  assert.equal(host.active, "A", "nothing is active before the confirmation");
  assert.equal($("#vault-onboarding").hidden, false);
  // The wording the PO chose, and the spell itself in one piece.
  assert.equal($("#vault-onboarding-title").textContent, "ふっかつのじゅもん（Recovery Spell）");
  assert.equal($("#vault-spell-lead").textContent, "あなたの記憶を取り戻すための、復活の呪文を記録してください。決して忘れないでくださいね。");
  assert.equal($("#vault-spell-lead").textContent, SPELL_LEAD);
  assert.equal($("#vault-spell-caution").textContent, "この呪文は一度しか表示されません。安全な場所に保管してください。失うとバックアップから復元できなくなる場合があります。");
  assert.equal($("#vault-spell-caution").textContent, SPELL_CAUTION);
  assert.equal($("#vault-spell").textContent, TEST_SPELL);
  assert.match($("#vault-onboarding-path").textContent, /保存先: C:\\Vaults\\memocampus\.sqlite3/);
  assert.match($("#vault-spell-note").textContent, /大文字と小文字は区別されます/);
  assert.match($("#vault-spell-note").textContent, /クリップボード履歴/);
  assert.equal($("#vault-creation-pending").hidden, true, "the creation in progress is this one");
  assert.equal($("#vault-create-pick").disabled, true, "no other creation meanwhile");

  // Copy only on the user's click.
  assert.deepEqual(writes, [], "nothing is copied by itself");
  $("#vault-spell-copy").dispatch("click");
  await settle(10);
  assert.deepEqual(writes, [TEST_SPELL]);
  assert.match($("#vault-message").textContent, /コピーしました/);
  assert.match($("#vault-message").textContent, /消去してください/);

  // On to the check: the spell leaves the page; all 43 characters are asked for.
  $("#vault-spell-written").dispatch("click");
  await settle(10);
  assert.equal($("#vault-spell").textContent, "");
  assert.equal($("#vault-spell-show").hidden, true);
  assert.equal($("#vault-spell-check").hidden, false);
  assert.match($("#vault-spell-check-title").textContent, /43文字すべて/);
  assert.match($("#vault-spell-check-title").textContent, /証明にはなりません/);

  // Anything but the whole spell: nothing is sent, the user can try again.
  for (const [typed, message] of [
    ["", /43文字です（0文字でした）/],
    [TEST_SPELL.slice(0, 10), /43文字です（10文字でした）/],
    [TEST_SPELL.slice(0, 42), /43文字です（42文字でした）/],
    [`${TEST_SPELL.slice(0, 20)} ${TEST_SPELL.slice(21)}`, /21文字目に使えない文字/],
    [OTHER_SPELL, /一致しません/],
    [TEST_SPELL.toUpperCase(), /一致しません|最後の文字/],
    [TEST_SPELL.replace("_", "-"), /一致しません/],
  ]) {
    $("#vault-spell-confirm-input").value = typed;
    $("#vault-spell-confirm").dispatch("click");
    await settle(20);
    assert.equal(sent(host, "vault_create_encrypted_commit").length, 0, typed);
    assert.match($("#vault-message").textContent, message, typed);
    assert.match($("#vault-message").textContent, /Vaultはまだ作成していません/);
    assert.equal($("#vault-onboarding").hidden, false);
    assert.equal(host.active, "A");
  }

  // Back to the spell and forward again, then the spell pasted with surrounding spaces.
  $("#vault-spell-back").dispatch("click");
  assert.equal($("#vault-spell").textContent, TEST_SPELL);
  assert.equal($("#vault-spell-confirm-input").value, "", "what was typed is cleared");
  $("#vault-spell-written").dispatch("click");
  $("#vault-spell-confirm-input").value = `  ${TEST_SPELL}\n`;
  $("#vault-spell-confirm").dispatch("click");
  await settle(40);
  const commits = sent(host, "vault_create_encrypted_commit");
  assert.equal(commits.length, 1);
  assert.equal(commits[0].recoverySpell, TEST_SPELL, "the host checks the whole spell against the vault's");
  assert.equal(commits[0].recoveryPhrase, undefined);
  assert.equal(host.active, "C:\\Vaults\\memocampus.sqlite3");
  assert.equal($("#vault-onboarding").hidden, true);
  assert.deepEqual(spellLeftovers($), [], "the spell is gone from the page");
  assert.match($("#vault-message").textContent, /暗号化Vaultを作成し、使用を開始しました/);
  assert.match($("#vault-message").textContent, /MCPへの公開は未承認/);
  assert.equal($("#vault-publication").dataset.state, "pending");
  // The spell went to the host only in the creation answer and the commit; never to the service
  // worker (which owns the durable queue), and the page has no storage of its own.
  const carrying = host.requests.filter((message) => JSON.stringify(message).includes(TEST_SPELL));
  assert.deepEqual(carrying.map((message) => message.type), ["vault_create_encrypted_commit"]);
  assert.ok(!JSON.stringify(host.workerMessages ?? []).includes(TEST_SPELL.slice(0, 12)));
  assert.equal(globalThis.localStorage, undefined);
  assert.equal(globalThis.chrome.storage, undefined, "the side panel stores nothing itself");
});

test("a refused copy is reported and changes nothing", async () => {
  installClipboard({ fail: true });
  const { $, host } = await loadSidePanel({ vaults: { A: pages("A", 1) }, active: "A", encryption: true });
  await settle(30);
  await openSettings($);
  $("#vault-create-default").dispatch("click");
  await settle(30);
  $("#vault-spell-copy").dispatch("click");
  await settle(10);
  assert.match($("#vault-message").textContent, /コピーできませんでした。表示された呪文を書き写してください/);
  assert.equal($("#vault-spell").textContent, TEST_SPELL);
  assert.equal(host.active, "A");
});

test("a commit the host refuses keeps the creation pending for another try", async () => {
  installClipboard();
  const { $, host } = await loadSidePanel({ vaults: { A: pages("A", 1) }, active: "A", encryption: true });
  await settle(30);
  await openSettings($);
  $("#vault-create-default").dispatch("click");
  await settle(30);
  // The host's spell differs from what the page holds (it never does; the host is the judge).
  host.creation.spell = OTHER_SPELL;
  $("#vault-spell-written").dispatch("click");
  $("#vault-spell-confirm-input").value = TEST_SPELL;
  $("#vault-spell-confirm").dispatch("click");
  await settle(30);
  assert.equal(sent(host, "vault_create_encrypted_commit").length, 1);
  assert.match($("#vault-message").textContent, /一致しません。Vaultも鍵も変更していません/);
  assert.equal($("#vault-onboarding").hidden, false, "the user can try again");
  assert.equal(host.active, "A");
});

test("plain is still available, and is used where encryption is not", async () => {
  const { $, host } = await loadSidePanel({ vaults: { A: pages("A", 1) }, active: "A", encryption: true });
  await settle(30);
  await openSettings($);
  $("#vault-encryption-encrypted").checked = false;
  $("#vault-encryption-plain").checked = true;
  $("#vault-create-pick").dispatch("click");
  await settle(30);
  assert.equal(sent(host, "vault_create").length, 1);
  assert.equal(sent(host, "vault_create_encrypted").length, 0);

  const older = await loadSidePanel({ vaults: { A: pages("A", 1) }, active: "A" });
  await settle(30);
  await openSettings(older.$);
  assert.equal(older.$("#vault-encryption-encrypted").disabled, true, "a host without encryption");
  assert.equal(older.$("#vault-encryption-plain").checked, true);
  older.$("#vault-create-default").dispatch("click");
  await settle(30);
  assert.equal(sent(older.host, "vault_create").length, 1);
  assert.equal(sent(older.host, "vault_create_encrypted").length, 0);
});

test("cancelling forgets the spell; an interrupted creation can only be discarded", async () => {
  const { $, host } = await loadSidePanel({ vaults: { A: pages("A", 1) }, active: "A", encryption: true });
  await settle(30);
  await openSettings($);
  $("#vault-create-default").dispatch("click");
  await settle(30);
  $("#vault-spell-written").dispatch("click");
  $("#vault-spell-confirm-input").value = TEST_SPELL.slice(0, 30);
  $("#vault-onboarding-cancel").dispatch("click");
  await settle(30);
  assert.equal(sent(host, "vault_create_encrypted_cancel").length, 1);
  assert.equal($("#vault-onboarding").hidden, true);
  assert.deepEqual(spellLeftovers($), []);
  assert.equal($("#vault-spell-confirm-input").value, "", "what was typed is cleared too");
  assert.equal(host.creation, null);
  assert.equal(host.active, "A");
  assert.match($("#vault-message").textContent, /破棄しました。Active Vaultは変更していません/);

  // The panel was closed after the spell was shown: a new panel cannot show it again.
  host.creation = { path: "C:\\Vaults\\x.sqlite3", spell: TEST_SPELL };
  const reopened = await loadSidePanel({ vaults: { A: pages("A", 1) }, active: "A", encryption: true });
  reopened.host.creation = host.creation;
  await settle(30);
  await openSettings(reopened.$);
  assert.equal(reopened.$("#vault-creation-pending").hidden, false);
  assert.match(reopened.$("#vault-creation-pending-text").textContent, /ふっかつのじゅもんの確認前に中断/);
  assert.match(reopened.$("#vault-creation-pending-text").textContent, /再表示できない/);
  assert.equal(reopened.$("#vault-onboarding").hidden, true);
  assert.equal(reopened.$("#vault-spell").textContent, "");
  assert.ok(!JSON.stringify(reopened.host.requests).includes(TEST_SPELL), "no request returns the spell again");
  assert.equal(reopened.$("#vault-create-default").disabled, true);
  reopened.$("#vault-creation-discard").dispatch("click");
  await settle(30);
  assert.equal(sent(reopened.host, "vault_create_encrypted_cancel").length, 1);
  assert.equal(reopened.$("#vault-creation-pending").hidden, true);
  assert.equal(reopened.$("#vault-create-default").disabled, false);
});

test("an encrypted vault without a key here is recovered with its spell, and only with it", async () => {
  const { $, host } = await loadSidePanel({ vaults: { A: pages("A", 1), "C:\\Old\\enc.sqlite3": pages("E", 2) }, active: "A", encryption: true });
  host.locked.set("C:\\Old\\enc.sqlite3", TEST_SPELL);
  await settle(30);
  await openSettings($);
  $("#vault-path-input").value = "C:\\Old\\enc.sqlite3";
  document.querySelector('[data-manual="vault_open"]').dispatch("click");
  await settle(30);
  assert.equal(host.active, "A", "nothing changes without the key");
  assert.equal($("#vault-recovery").hidden, false);
  assert.match($("#vault-recovery-title").textContent, /対象: C:\\Old\\enc\.sqlite3/);
  assert.match($("#vault-recovery-title").textContent, /ふっかつのじゅもん（43文字）/);
  assert.match($("#vault-message").textContent, /ふっかつのじゅもんで復旧できます/);

  // Malformed input is caught in the page; nothing is sent.
  $("#vault-recovery-input").value = TEST_SPELL.slice(0, 42);
  $("#vault-recovery-submit").dispatch("click");
  await settle(30);
  assert.match($("#vault-message").textContent, /43文字です（42文字でした）/);
  assert.equal(sent(host, "vault_recover").length, 0);
  $("#vault-recovery-input").value = `${TEST_SPELL.slice(0, 5)}＋${TEST_SPELL.slice(6)}`;
  $("#vault-recovery-submit").dispatch("click");
  await settle(30);
  assert.match($("#vault-message").textContent, /6文字目に使えない文字/);
  assert.equal(sent(host, "vault_recover").length, 0);
  // Well-formed but not this vault's: refused by the host.
  $("#vault-recovery-input").value = OTHER_SPELL;
  $("#vault-recovery-submit").dispatch("click");
  await settle(30);
  assert.match($("#vault-message").textContent, /一致しません。Vaultも鍵も変更していません/);
  assert.equal(host.active, "A");
  assert.equal($("#vault-recovery").hidden, false);

  $("#vault-recovery-input").value = ` ${TEST_SPELL} `;
  $("#vault-recovery-submit").dispatch("click");
  await settle(40);
  assert.deepEqual(sent(host, "vault_recover").map((message) => message.recoverySpell), [OTHER_SPELL, TEST_SPELL], "trimmed, nothing else changed");
  assert.equal(host.active, "C:\\Old\\enc.sqlite3");
  assert.equal($("#vault-recovery").hidden, true);
  assert.equal($("#vault-recovery-input").value, "", "the spell is not kept in the page");
  assert.match($("#vault-message").textContent, /ふっかつのじゅもんで復旧し、このWindowsユーザーで開けるようにしました/);
  assert.equal($("#vault-publication").dataset.state, "pending");
  assert.ok(!JSON.stringify(host.workerMessages ?? []).includes(TEST_SPELL.slice(0, 12)));
});

test("the active vault locked for this user offers recovery from its status", async () => {
  const { $, host } = await loadSidePanel({ vaults: { E: pages("E", 1) }, active: "E", encryption: true });
  host.locked.set("E", TEST_SPELL);
  await settle(30);
  await openSettings($);
  // The status was taken before `locked` was set: refresh.
  $("#tab-settings").dispatch("click");
  await settle(30);
  assert.equal($("#vault-recovery-offer").hidden, false);
  $("#vault-recovery-start").dispatch("click");
  assert.equal($("#vault-recovery").hidden, false);
  $("#vault-recovery-input").value = TEST_SPELL;
  $("#vault-recovery-cancel").dispatch("click");
  assert.equal($("#vault-recovery").hidden, true);
  assert.equal($("#vault-recovery-input").value, "", "cancelling forgets what was typed");
  assert.match($("#vault-message").textContent, /Vaultも鍵も変更していません/);
  assert.equal(sent(host, "vault_recover").length, 0);
});

test("the page checks a spell the way the host does, without repairing it", () => {
  assert.equal(checkSpell(TEST_SPELL), null);
  assert.equal(checkSpell(` ${TEST_SPELL}\t`), null, "only surrounding whitespace is removed");
  assert.equal(normalizeSpellInput(` ${TEST_SPELL}\n`), TEST_SPELL);
  assert.equal(normalizeSpellInput(null), "");
  assert.match(checkSpell(TEST_SPELL.slice(1)), /42文字でした/);
  assert.match(checkSpell(`${TEST_SPELL}A`), /44文字でした/);
  assert.match(checkSpell(`${TEST_SPELL.slice(0, 42)}=`), /43文字目に使えない文字/);
  for (const [position, bad] of [[1, "+"], [2, "/"], [10, " "], [20, "Ａ"], [30, "０"], [40, "é"]]) {
    const typed = `${TEST_SPELL.slice(0, position - 1)}${bad}${TEST_SPELL.slice(position)}`;
    assert.equal([...typed].length, 43);
    assert.match(checkSpell(typed), new RegExp(`${position}文字目に使えない文字`), bad);
  }
  // The last character carries two unused bits: one with them set is refused, not rounded.
  assert.match(checkSpell(`${TEST_SPELL.slice(0, 42)}5`), /最後の文字/);
  assert.equal(checkSpell(`${TEST_SPELL.slice(0, 42)}A`), null);
  // Exact match only: case, a single character, a short prefix.
  assert.equal(spellMatches(TEST_SPELL, TEST_SPELL), true);
  assert.equal(spellMatches(TEST_SPELL, `  ${TEST_SPELL} `), true);
  assert.equal(spellMatches(TEST_SPELL, TEST_SPELL.toLowerCase()), false);
  assert.equal(spellMatches(TEST_SPELL, TEST_SPELL.slice(0, 8)), false);
  assert.equal(spellMatches(TEST_SPELL, `${TEST_SPELL.slice(0, 42)}A`), false);
  assert.equal(spellMatches("", ""), false);
  assert.match(describeSpellProblem({ reason: "length", count: 12 }), /43文字です（12文字でした）/);
  assert.match(describeSpellProblem({ reason: "character", position: 4 }), /4文字目/);
  assert.match(describeSpellProblem({ reason: "non_canonical" }), /最後の文字/);
  assert.match(describeSpellProblem({}), /ふっかつのじゅもんの形式/);
});
