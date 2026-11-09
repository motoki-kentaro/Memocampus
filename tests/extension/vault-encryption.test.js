// Issue #51 (Slice B): creating an encrypted vault from the side panel (encryption is the default,
// the phrase is shown once and confirmed by re-entering words before anything becomes active), and
// recovering an encrypted vault with its phrase. The fake host follows the real host's rules; the
// real host is tested in native-host (tests/secure_vault_b.rs).
import { test } from "node:test";
import assert from "node:assert/strict";
import { loadSidePanel, pages, settle } from "./fake-dom.js";
import { checkAnswers, describePhraseProblem, pickPositions } from "../../extension/vault-client.js";

const PHRASE = "abandon ability able about above absent absorb abstract absurd abuse access accident";
const WORDS = PHRASE.split(" ");

const sent = (host, type) => host.requests.filter((message) => message.type === type);

async function openSettings($) {
  $("#tab-settings").dispatch("click");
  await settle(20);
}

/** The answer inputs shown for the confirmation, with their 1-based positions. */
function answerInputs($) {
  return $("#vault-phrase-inputs").children.map((label) => label.children.at(-1));
}

function answer($, wordAt = (position) => WORDS[position - 1]) {
  for (const input of answerInputs($)) input.value = wordAt(Number(input.dataset.position));
}

test("encryption is the default: the phrase is shown once, confirmed, and only then is the vault active", async () => {
  const { $, host } = await loadSidePanel({ vaults: { A: pages("A", 1) }, active: "A", encryption: true });
  await settle(30);
  await openSettings($);
  assert.equal($("#vault-encryption").hidden, false);
  assert.equal($("#vault-encryption-encrypted").checked, true);
  assert.equal($("#vault-encryption-encrypted").disabled, false);
  assert.match($("#vault-encryption-note").textContent, /12単語のリカバリーフレーズ/);

  $("#vault-create-default").dispatch("click");
  await settle(30);
  assert.equal(sent(host, "vault_create").length, 0);
  assert.deepEqual(sent(host, "vault_create_encrypted").map(({ useDefault }) => useDefault), [true]);
  assert.equal(host.active, "A", "nothing is active before the confirmation");
  assert.equal($("#vault-onboarding").hidden, false);
  assert.deepEqual($("#vault-phrase").children.map((item) => item.textContent), WORDS);
  assert.match($("#vault-onboarding-title").textContent, /今回だけ表示/);
  assert.match($("#vault-phrase-note").textContent, /コピー機能はありません/);
  assert.equal($("#vault-creation-pending").hidden, true, "the creation in progress is this one");
  assert.equal($("#vault-create-pick").disabled, true, "no other creation meanwhile");

  // On to the check: the words leave the page; three positions are asked.
  $("#vault-phrase-written").dispatch("click");
  await settle(10);
  assert.equal($("#vault-phrase").children.length, 0);
  assert.equal($("#vault-phrase-check").hidden, false);
  const positions = answerInputs($).map((input) => Number(input.dataset.position));
  assert.equal(positions.length, 3);
  assert.equal(new Set(positions).size, 3);
  assert.ok(positions.every((position) => position >= 1 && position <= 12));

  // A wrong word: nothing is sent.
  answer($, () => "zoo");
  $("#vault-phrase-confirm").dispatch("click");
  await settle(20);
  assert.equal(sent(host, "vault_create_encrypted_commit").length, 0);
  assert.match($("#vault-message").textContent, /一致しません/);
  assert.equal(host.active, "A");

  // Back to the words and forward again, then the right words (any case and spacing).
  $("#vault-phrase-back").dispatch("click");
  assert.deepEqual($("#vault-phrase").children.map((item) => item.textContent), WORDS);
  $("#vault-phrase-written").dispatch("click");
  answer($, (position) => `  ${WORDS[position - 1].toUpperCase()} `);
  $("#vault-phrase-confirm").dispatch("click");
  await settle(40);
  const commits = sent(host, "vault_create_encrypted_commit");
  assert.equal(commits.length, 1);
  assert.equal(commits[0].recoveryPhrase, PHRASE, "the host checks the phrase shown against the vault's");
  assert.equal(host.active, "C:\\Vaults\\memocampus.sqlite3");
  assert.equal($("#vault-onboarding").hidden, true);
  assert.equal($("#vault-phrase").children.length, 0);
  assert.equal($("#vault-phrase-inputs").children.length, 0);
  assert.match($("#vault-message").textContent, /暗号化Vaultを作成し、使用を開始しました/);
  assert.match($("#vault-message").textContent, /MCPへの公開は未承認/);
  assert.equal($("#vault-publication").dataset.state, "pending");
  // The phrase went to the host only in the commit, and never into the page's storage.
  const carrying = host.requests.filter((message) => JSON.stringify(message).includes("abandon ability"));
  assert.deepEqual(carrying.map((message) => message.type), ["vault_create_encrypted_commit"]);
  assert.equal(globalThis.localStorage, undefined);
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

test("cancelling forgets the phrase; an interrupted creation can only be discarded", async () => {
  const { $, host } = await loadSidePanel({ vaults: { A: pages("A", 1) }, active: "A", encryption: true });
  await settle(30);
  await openSettings($);
  $("#vault-create-default").dispatch("click");
  await settle(30);
  $("#vault-onboarding-cancel").dispatch("click");
  await settle(30);
  assert.equal(sent(host, "vault_create_encrypted_cancel").length, 1);
  assert.equal($("#vault-onboarding").hidden, true);
  assert.equal($("#vault-phrase").children.length, 0);
  assert.equal(host.creation, null);
  assert.equal(host.active, "A");
  assert.match($("#vault-message").textContent, /破棄しました。Active Vaultは変更していません/);

  // The panel was closed after the phrase was shown: a new panel cannot show it again.
  host.creation = { path: "C:\\Vaults\\x.sqlite3", phrase: PHRASE };
  const reopened = await loadSidePanel({ vaults: { A: pages("A", 1) }, active: "A", encryption: true });
  reopened.host.creation = host.creation;
  await settle(30);
  await openSettings(reopened.$);
  assert.equal(reopened.$("#vault-creation-pending").hidden, false);
  assert.match(reopened.$("#vault-creation-pending-text").textContent, /再表示できない/);
  assert.equal(reopened.$("#vault-onboarding").hidden, true);
  assert.equal(reopened.$("#vault-phrase").children.length, 0);
  assert.equal(reopened.$("#vault-create-default").disabled, true);
  reopened.$("#vault-creation-discard").dispatch("click");
  await settle(30);
  assert.equal(sent(reopened.host, "vault_create_encrypted_cancel").length, 1);
  assert.equal(reopened.$("#vault-creation-pending").hidden, true);
  assert.equal(reopened.$("#vault-create-default").disabled, false);
});

test("an encrypted vault without a key here is recovered with its phrase, and only with it", async () => {
  const { $, host } = await loadSidePanel({ vaults: { A: pages("A", 1), "C:\\Old\\enc.sqlite3": pages("E", 2) }, active: "A", encryption: true });
  host.locked.set("C:\\Old\\enc.sqlite3", PHRASE);
  await settle(30);
  await openSettings($);
  $("#vault-path-input").value = "C:\\Old\\enc.sqlite3";
  document.querySelector('[data-manual="vault_open"]').dispatch("click");
  await settle(30);
  assert.equal(host.active, "A", "nothing changes without the key");
  assert.equal($("#vault-recovery").hidden, false);
  assert.match($("#vault-recovery-title").textContent, /対象: C:\\Old\\enc\.sqlite3/);
  assert.match($("#vault-message").textContent, /リカバリーフレーズで復旧できます/);

  $("#vault-recovery-input").value = WORDS.slice(0, 11).join(" ");
  $("#vault-recovery-submit").dispatch("click");
  await settle(30);
  assert.match($("#vault-message").textContent, /12単語を入力してください（11単語でした）/);
  assert.equal($("#vault-recovery").hidden, false);
  $("#vault-recovery-input").value = PHRASE.replace("accident", "account");
  $("#vault-recovery-submit").dispatch("click");
  await settle(30);
  assert.match($("#vault-message").textContent, /一致しません。Vaultも鍵も変更していません/);
  assert.equal(host.active, "A");

  $("#vault-recovery-input").value = PHRASE.toUpperCase();
  $("#vault-recovery-submit").dispatch("click");
  await settle(40);
  assert.equal(host.active, "C:\\Old\\enc.sqlite3");
  assert.equal($("#vault-recovery").hidden, true);
  assert.equal($("#vault-recovery-input").value, "", "the phrase is not kept in the page");
  assert.match($("#vault-message").textContent, /復旧し、このWindowsユーザーで開けるようにしました/);
  assert.equal($("#vault-publication").dataset.state, "pending");
});

test("the active vault locked for this user offers recovery from its status", async () => {
  const { $, host } = await loadSidePanel({ vaults: { E: pages("E", 1) }, active: "E", encryption: true });
  host.locked.set("E", PHRASE);
  await settle(30);
  await openSettings($);
  // The status was taken before `locked` was set: refresh.
  $("#tab-settings").dispatch("click");
  await settle(30);
  assert.equal($("#vault-recovery-offer").hidden, false);
  $("#vault-recovery-start").dispatch("click");
  assert.equal($("#vault-recovery").hidden, false);
  $("#vault-recovery-cancel").dispatch("click");
  assert.equal($("#vault-recovery").hidden, true);
  assert.match($("#vault-message").textContent, /Vaultも鍵も変更していません/);
  assert.equal(sent(host, "vault_recover").length, 0);
});

test("confirmation positions are distinct, in range and all reachable; answers ignore case and spaces", () => {
  const seen = new Set();
  for (let i = 0; i < 400; i += 1) {
    const positions = pickPositions(3);
    assert.equal(new Set(positions).size, 3);
    assert.deepEqual([...positions].sort((a, b) => a - b), positions);
    for (const position of positions) {
      assert.ok(position >= 1 && position <= 12);
      seen.add(position);
    }
  }
  assert.equal(seen.size, 12);
  // A biased source cannot loop forever or leave the range.
  let calls = 0;
  const biased = (array) => {
    array[0] = calls++ < 5 ? 0xffffffff : calls;
    return array;
  };
  assert.equal(pickPositions(3, biased).length, 3);

  assert.equal(checkAnswers(WORDS, [{ position: 1, word: " ABANDON " }, { position: 12, word: "accident" }]), true);
  assert.equal(checkAnswers(WORDS, [{ position: 1, word: "ability" }]), false);
  assert.equal(checkAnswers(WORDS, []), false);
  assert.equal(checkAnswers(WORDS, [{ position: 13, word: "" }]), false);
  assert.match(describePhraseProblem({ reason: "unknown_word", position: 4 }), /4番目の単語/);
  assert.match(describePhraseProblem({ reason: "checksum" }), /綴りまたは順番/);
});
