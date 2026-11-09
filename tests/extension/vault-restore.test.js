// Issue #51 (Slice C): password-protected backups and restoring a backup as a new vault from the
// side panel. The fake host follows the real host's rules; the real host is tested in native-host
// (manage::backup_restore_tests) and, end to end, in mcp-process.test.js.
import { test } from "node:test";
import assert from "node:assert/strict";
import { loadSidePanel, pages, settle } from "./fake-dom.js";
import { backupFormat, checkBackupPassword, describePolicyCopy } from "../../extension/vault-client.js";

const PHRASE = "abandon ability able about above absent absorb abstract absurd abuse access accident";
const BACKUP = "C:\\Backups\\memocampus-backup.mcbackup";
const TARGET = "C:\\Restored\\memocampus-restored.sqlite3";

const sent = (host, type) => host.requests.filter((message) => message.type === type);

async function openSettings($) {
  $("#tab-settings").dispatch("click");
  await settle(20);
}

function choose($, id) {
  for (const other of ["#vault-backup-normal", "#vault-backup-password"]) $(other).checked = other === id;
  $(id).dispatch("change");
}

test("the backup format follows the vault's mode and the protection chosen", () => {
  assert.equal(backupFormat("plain", "normal"), "A");
  assert.equal(backupFormat("encrypted", "normal"), "B");
  assert.equal(backupFormat("plain", "password"), "C");
  assert.equal(backupFormat("encrypted", "password"), "D");
  assert.equal(checkBackupPassword("1234567", "1234567"), "パスワードは8文字以上で入力してください。");
  assert.equal(checkBackupPassword("12345678", "12345679"), "確認用のパスワードが一致しません。");
  assert.equal(checkBackupPassword("パスワード長めです", "パスワード長めです"), null);
  assert.match(describePolicyCopy({ state: "valid", revision: 3 }), /改訂3.*自動で適用しません/);
  assert.match(describePolicyCopy({ state: "absent" }), /含まれていません/);
});

test("a password backup needs the password twice, sends it once and forgets it", async () => {
  const { $, host } = await loadSidePanel({ vaults: { A: pages("A", 1) }, active: "A", restore: true });
  await settle(30);
  await openSettings($);
  assert.equal($("#vault-backup-normal").checked, true, "a normal backup is the default");
  assert.match($("#vault-backup-note").textContent, /^形式A/);
  assert.equal($("#vault-backup-password-fields").hidden, true);

  choose($, "#vault-backup-password");
  assert.equal($("#vault-backup-password-fields").hidden, false);
  assert.match($("#vault-backup-note").textContent, /^形式C/);
  assert.match($("#vault-backup-password-note").textContent, /保存しません/);

  // Too short, or not confirmed: nothing is sent.
  $("#vault-backup-password-input").value = "short";
  $("#vault-backup-password-confirm").value = "short";
  $("#vault-backup-pick").dispatch("click");
  await settle(20);
  assert.equal(sent(host, "vault_backup").length, 0);
  assert.match($("#vault-message").textContent, /8文字以上/);
  $("#vault-backup-password-input").value = "correct horse";
  $("#vault-backup-password-confirm").value = "correct horsf";
  $("#vault-backup-pick").dispatch("click");
  await settle(20);
  assert.equal(sent(host, "vault_backup").length, 0);
  assert.match($("#vault-message").textContent, /一致しません/);

  $("#vault-backup-password-confirm").value = "correct horse";
  $("#vault-backup-pick").dispatch("click");
  await settle(40);
  const [request] = sent(host, "vault_backup");
  assert.equal(request.protection, "password");
  assert.equal(request.password, "correct horse");
  assert.deepEqual(host.backupFormats.map(({ format }) => format), ["C"]);
  assert.match($("#vault-message").textContent, /^バックアップを作成しました（形式C）。/);
  assert.match($("#vault-message").textContent, /プライバシー設定のコピーを含めました/);
  assert.equal($("#vault-backup-password-input").value, "", "the password is used once");
  assert.equal($("#vault-backup-password-confirm").value, "");
  // Only that request ever carried it.
  const carriers = host.requests.filter((message) => JSON.stringify(message).includes("correct horse"));
  assert.deepEqual(carriers.map((message) => message.type), ["vault_backup"]);

  // Back to normal: the fields are emptied and hidden.
  $("#vault-backup-password-input").value = "typed again";
  choose($, "#vault-backup-normal");
  assert.equal($("#vault-backup-password-input").value, "");
  assert.equal($("#vault-backup-password-fields").hidden, true);
  $("#vault-backup-pick").dispatch("click");
  await settle(40);
  assert.equal(sent(host, "vault_backup")[1].protection, "normal");
  assert.equal(sent(host, "vault_backup")[1].password, undefined);
});

test("an encrypted vault's backup (B or D) needs the phrase that recovers it, sent once and forgotten", async () => {
  const { $, host } = await loadSidePanel({ vaults: { A: pages("A", 1), P: pages("P", 1) }, active: "A", restore: true, encryption: true });
  host.encrypted.add("A");
  await settle(30);
  await openSettings($);
  assert.match($("#vault-backup-note").textContent, /^形式B.*リカバリーフレーズ/);
  assert.equal($("#vault-backup-phrase-row").hidden, false, "B asks for the phrase");
  assert.match($("#vault-backup-phrase-note").textContent, /復旧できるフレーズでなければ/);

  // No phrase, or not 12 words: nothing is sent.
  $("#vault-backup-pick").dispatch("click");
  await settle(20);
  assert.equal(sent(host, "vault_backup").length, 0);
  assert.match($("#vault-message").textContent, /12単語/);
  $("#vault-backup-phrase").value = "abandon ability able";
  $("#vault-backup-pick").dispatch("click");
  await settle(20);
  assert.equal(sent(host, "vault_backup").length, 0);

  // A phrase that is not this vault's: refused by the host, no backup, the phrase forgotten.
  $("#vault-backup-phrase").value = "zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo wrong";
  $("#vault-backup-pick").dispatch("click");
  await settle(40);
  assert.equal(sent(host, "vault_backup").length, 1);
  assert.match($("#vault-message").textContent, /一致しません/);
  assert.deepEqual(host.backups, []);
  assert.equal($("#vault-backup-phrase").value, "");

  // D: the password and the phrase.
  choose($, "#vault-backup-password");
  assert.match($("#vault-backup-note").textContent, /^形式D.*両方/);
  $("#vault-backup-password-input").value = "password-123";
  $("#vault-backup-password-confirm").value = "password-123";
  $("#vault-backup-phrase").value = ` ${PHRASE.toUpperCase()} `;
  $("#vault-backup-pick").dispatch("click");
  await settle(40);
  const request = sent(host, "vault_backup").at(-1);
  assert.equal(request.recoveryPhrase, ` ${PHRASE.toUpperCase()} `);
  assert.equal(request.password, "password-123");
  assert.deepEqual(host.backupFormats.map(({ format }) => format), ["D"]);
  assert.equal($("#vault-backup-phrase").value, "", "the phrase is used once");
  assert.equal($("#vault-backup-password-input").value, "");
  const carriers = host.requests.filter((message) => JSON.stringify(message).toLowerCase().includes("absurd abuse"));
  assert.deepEqual(carriers.map((message) => message.type), ["vault_backup"]);

  // A plain vault asks for no phrase.
  host.active = "P";
  $("#tab-url").dispatch("click");
  await settle(10);
  await openSettings($);
  assert.equal($("#vault-backup-phrase-row").hidden, true);
});

test("restoring: the backup is checked first, secrets are asked only as needed, and nothing is active before the user decides", async () => {
  const { $, host } = await loadSidePanel({ vaults: { A: pages("A", 2) }, active: "A", restore: true });
  host.backupFiles[BACKUP] = {
    format: "D",
    mode: "encrypted",
    password: "outer password",
    phrase: PHRASE,
    pages: pages("R", 3),
    policyCopy: { state: "valid", revision: 7, policy: { revision: 7, rule: "CANARY-rule" }, approvedDomains: ["example.com"] },
  };
  await settle(30);
  await openSettings($);
  $("#vault-restore-pick").dispatch("click");
  await settle(30);
  const [inspect] = sent(host, "vault_restore_inspect");
  assert.equal(inspect.path, undefined, "the backup file is picked in a Windows dialog");
  assert.equal($("#vault-restore").hidden, false);
  assert.match($("#vault-restore-title").textContent, /形式D/);
  assert.match($("#vault-restore-title").textContent, /上書きしません/);
  assert.equal($("#vault-restore-password-row").hidden, false);
  assert.equal($("#vault-restore-phrase-row").hidden, false);

  // No password: nothing is sent.
  $("#vault-restore-submit").dispatch("click");
  await settle(20);
  assert.equal(sent(host, "vault_restore").length, 0);
  assert.match($("#vault-message").textContent, /パスワードを入力/);

  // A wrong password: refused, the box stays for another try, nothing changed.
  $("#vault-restore-password").value = "wrong password";
  $("#vault-restore-phrase").value = PHRASE;
  $("#vault-restore-submit").dispatch("click");
  await settle(40);
  assert.match($("#vault-message").textContent, /パスワードが違います/);
  assert.equal($("#vault-restore").hidden, false);
  assert.equal($("#vault-restored").hidden, true);
  assert.equal(host.active, "A");

  $("#vault-restore-password").value = "outer password";
  $("#vault-restore-submit").dispatch("click");
  await settle(40);
  const requests = sent(host, "vault_restore");
  assert.equal(requests.length, 2);
  assert.deepEqual(
    { backupPath: requests[1].backupPath, password: requests[1].password, recoveryPhrase: requests[1].recoveryPhrase, path: requests[1].path },
    { backupPath: BACKUP, password: "outer password", recoveryPhrase: PHRASE, path: undefined },
  );
  // Restored, not active: the panel shows what was restored and the policy copy (never applied).
  assert.equal(host.active, "A");
  assert.equal($("#vault-restore").hidden, true);
  assert.equal($("#vault-restore-password").value, "", "secrets are forgotten once used");
  assert.equal($("#vault-restore-phrase").value, "");
  assert.equal($("#vault-restored").hidden, false);
  assert.match($("#vault-restored-title").textContent, /まだ使用していません/);
  assert.match($("#vault-restored-title").textContent, /閲覧 3件/);
  assert.match($("#vault-restored-policy").textContent, /改訂7.*自動で適用しません/);
  assert.equal($("#vault-restored-policy-details").hidden, false);
  assert.match($("#vault-restored-policy-json").textContent, /CANARY-rule/);
  assert.equal($("#vault-restore-pick").disabled, true, "one restore at a time");
  assert.equal(sent(host, "policy_apply").length, 0);

  $("#vault-restore-activate").dispatch("click");
  await settle(40);
  const [activation] = sent(host, "vault_restore_activate");
  assert.equal(activation.expectedVaultToken, host.status().vaultToken);
  assert.equal(host.active, TARGET);
  assert.equal($("#vault-restored").hidden, true);
  assert.equal($("#vault-restored-policy-json").textContent, "");
  assert.match($("#vault-message").textContent, /使い始めました[\s\S]*MCPへの公開は未承認/);
  assert.match($("#vault-publication-state").textContent, /未承認/);
  assert.equal($("#vault-restore-pick").disabled, false);
  // Only the restore request carried the password and the phrase.
  const carriers = host.requests.filter((message) => {
    const text = JSON.stringify(message);
    return text.includes("outer password") || text.includes("absurd abuse");
  });
  assert.deepEqual(carriers.map((message) => message.type), ["vault_restore", "vault_restore"]);
});

test("a restored vault can be discarded, and a restore found at start is offered for a decision", async () => {
  const { $, host } = await loadSidePanel({ vaults: { A: pages("A", 1), [TARGET]: [] }, active: "A", restore: true });
  host.restorePending = { path: TARGET, phase: "restored", format: "B", mode: "encrypted", vaultToken: "f".repeat(64) };
  await settle(30);
  await openSettings($);
  assert.equal($("#vault-restored").hidden, false);
  assert.match($("#vault-restored-title").textContent, /まだ使用していないVault/);
  assert.equal($("#vault-restore-activate").hidden, false);
  assert.equal($("#vault-restore-pick").disabled, true);
  $("#vault-restore-discard").dispatch("click");
  await settle(40);
  assert.equal(sent(host, "vault_restore_discard").length, 1);
  assert.equal(host.restorePending, null);
  assert.equal(host.active, "A", "discarding never touches the active vault");
  assert.equal($("#vault-restored").hidden, true);
  assert.match($("#vault-message").textContent, /破棄しました/);

  // An interrupted restore: only discarding is offered.
  host.restorePending = { path: TARGET, phase: "restoring", format: "C", mode: "plain", vaultToken: null };
  $("#tab-url").dispatch("click");
  await settle(10);
  await openSettings($);
  await settle(30);
  assert.equal($("#vault-restored").hidden, false);
  assert.match($("#vault-restored-title").textContent, /中断された復元/);
  assert.equal($("#vault-restore-activate").hidden, true);
});

test("a cancelled restore forgets the backup and what was typed", async () => {
  const { $, host } = await loadSidePanel({ vaults: { A: pages("A", 1) }, active: "A", restore: true });
  host.backupFiles[BACKUP] = { format: "B", mode: "encrypted", phrase: PHRASE, pages: [] };
  await settle(30);
  await openSettings($);
  $("#vault-restore-pick").dispatch("click");
  await settle(30);
  assert.equal($("#vault-restore-password-row").hidden, true, "a B backup needs no password");
  assert.equal($("#vault-restore-phrase-row").hidden, false);
  $("#vault-restore-phrase").value = PHRASE;
  $("#vault-restore-cancel").dispatch("click");
  assert.equal($("#vault-restore").hidden, true);
  assert.equal($("#vault-restore-phrase").value, "");
  assert.equal(sent(host, "vault_restore").length, 0);
  // A file that is not a backup is reported, and nothing is asked.
  host.restoreDialog = "C:\\Downloads\\notes.txt";
  $("#vault-restore-pick").dispatch("click");
  await settle(30);
  assert.match($("#vault-message").textContent, /Memocampusのバックアップではありません/);
  assert.equal($("#vault-restore").hidden, true);
});
