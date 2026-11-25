// Issue #77 Slice 3: the Vault section of the Side Panel in Japanese and English. The same steps are taken in
// both languages: what is sent to the host must be the same, the English text must say the same about
// encryption, the Recovery Spell, what was (not) changed and when a Vault becomes active, and a missing
// message must never let a step through that the Japanese UI would stop. The fake host follows the real
// host's rules (see fake-dom.js); the spells are the fixed test values, never a real one.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import vm from "node:vm";
import { OTHER_SPELL, TEST_SPELL, extensionDir, loadSidePanel, pages, settle, tokenOf } from "./fake-dom.js";
import { createI18n, withLocale } from "./i18n-mock.js";
import {
  describeConversionPending, describePolicyCopy, describePublication, describeReasons, describeRestorePending, describeStatus, describeVerdict, formatCounts, texts,
} from "../../extension/vault-client.js";

const JAPANESE = /[ぁ-んァ-ヶ一-龠（）「」、。・：]/u;
const BACKUP = "C:\\Backups\\memocampus-backup.mcbackup";
const html = readFileSync(path.join(extensionDir, "sidepanel.html"), "utf8");
const vaultHtml = html.slice(html.indexOf('<section id="vault-panel"'), html.indexOf("</section>", html.indexOf('<section id="vault-panel"')));
const catalog = (locale) => JSON.parse(readFileSync(path.join(extensionDir, "_locales", locale, "messages.json"), "utf8"));

/** Every element of the Vault section whose text a person can read (the fake DOM holds them by id). */
const SHOWN = [
  "vault-state", "vault-path", "vault-counts", "vault-verdict", "vault-publication-state", "vault-publication-note", "vault-publication-review-title",
  "vault-creation-pending-text", "vault-encryption-note", "vault-backup-note", "vault-backup-password-note", "vault-backup-spell-note", "vault-default-path",
  "vault-convert-title", "vault-convert-note", "vault-convert-source", "vault-convert-pick", "vault-reissue-title", "vault-reissue-note", "vault-reissue-pick",
  "vault-onboarding-title", "vault-spell-lead", "vault-spell-caution", "vault-onboarding-path", "vault-spell-note", "vault-spell-check-title",
  "vault-spell-confirm", "vault-onboarding-cancel", "vault-recovery-title", "vault-restore-title", "vault-restored-title", "vault-restored-policy",
  "vault-converted-title", "vault-approval-title", "vault-message", "vault-note",
];
const shownText = ($) => Object.fromEntries(SHOWN.map((id) => [id, $(`#${id}`).textContent]));
/** What was sent to the host, without the request ids (which differ from run to run). */
const vaultRequests = (host) => host.requests.filter((m) => m.type.startsWith("vault_") && m.type !== "vault_status").map(({ requestId, ...rest }) => rest);

async function openSettings($) {
  $("#tab-settings").dispatch("click");
  await settle(20);
}

async function click($, id, ms = 40) {
  $(`#${id}`).dispatch("click");
  await settle(ms);
}

/** Collects every message shown along the way (they replace each other). */
function recorder($) {
  const seen = [];
  const take = () => seen.push(...Object.values(shownText($)).filter(Boolean));
  return { take, seen };
}

// --- The same steps in both languages ------------------------------------------------------------------------

/** Encrypted creation: cancel once, then create, a wrong spell, the right one. */
async function createFlow() {
  Object.defineProperty(globalThis, "navigator", { value: { clipboard: { writeText: async () => {} } }, configurable: true, writable: true });
  const { $, host } = await loadSidePanel({ vaults: { A: pages("A", 1) }, active: "A", encryption: true });
  const r = recorder($);
  await settle(30);
  await openSettings($);
  r.take();
  await click($, "vault-create-default");
  r.take();
  await click($, "vault-onboarding-cancel");
  r.take();
  await click($, "vault-create-default");
  await click($, "vault-spell-copy", 10);
  r.take();
  await click($, "vault-spell-written", 10);
  r.take();
  $("#vault-spell-confirm-input").value = OTHER_SPELL;
  await click($, "vault-spell-confirm", 20);
  r.take();
  const mismatch = $("#vault-message").textContent;
  $("#vault-spell-confirm-input").value = TEST_SPELL.slice(0, 40);
  await click($, "vault-spell-confirm", 20);
  const tooShort = $("#vault-message").textContent;
  r.take();
  $("#vault-spell-confirm-input").value = TEST_SPELL;
  await click($, "vault-spell-confirm");
  r.take();
  return { requests: vaultRequests(host), seen: r.seen, mismatch, tooShort, done: $("#vault-message").textContent, active: host.active };
}

/** Restore: inspect, secrets, restore, discard; then again and start using it. */
async function restoreFlow() {
  const { $, host } = await loadSidePanel({ vaults: { A: pages("A", 2) }, active: "A", restore: true });
  host.backupFiles[BACKUP] = { format: "D", mode: "encrypted", password: "outer password", spell: TEST_SPELL, pages: pages("R", 1234), policyCopy: { state: "valid", revision: 7, policy: { revision: 7 }, approvedDomains: [] } };
  const r = recorder($);
  await settle(30);
  await openSettings($);
  await click($, "vault-restore-pick", 30);
  r.take();
  const source = $("#vault-restore-title").textContent;
  await click($, "vault-restore-submit", 20);
  const noPassword = $("#vault-message").textContent;
  $("#vault-restore-password").value = "outer password";
  $("#vault-restore-spell").value = TEST_SPELL;
  await click($, "vault-restore-submit");
  r.take();
  const restored = { title: $("#vault-restored-title").textContent, policy: $("#vault-restored-policy").textContent, message: $("#vault-message").textContent, active: host.active };
  await click($, "vault-restore-discard");
  r.take();
  const discarded = { message: $("#vault-message").textContent, active: host.active };
  await click($, "vault-restore-pick", 30);
  $("#vault-restore-password").value = "outer password";
  $("#vault-restore-spell").value = TEST_SPELL;
  await click($, "vault-restore-submit");
  await click($, "vault-restore-activate");
  r.take();
  return { requests: vaultRequests(host), seen: r.seen, source, noPassword, restored, discarded, activated: $("#vault-message").textContent, active: host.active };
}

/** Removing encryption: refused without the acknowledgement, then done and kept without switching. */
async function decryptFlow() {
  const { $, host } = await loadSidePanel({ vaults: { E: pages("E", 2) }, active: "E", encryption: true, restore: true, convert: true });
  host.encrypted.add("E");
  const r = recorder($);
  await settle(30);
  await openSettings($);
  r.take();
  const before = { title: $("#vault-convert-title").textContent, note: $("#vault-convert-note").textContent, pick: $("#vault-convert-pick").textContent, disabled: $("#vault-convert-pick").disabled };
  await click($, "vault-convert-pick", 20);
  const refused = { message: $("#vault-message").textContent, sent: vaultRequests(host).length };
  $("#vault-convert-ack").checked = true;
  $("#vault-convert-ack").dispatch("change");
  await click($, "vault-convert-pick");
  r.take();
  const converted = $("#vault-converted-title").textContent;
  await click($, "vault-convert-keep");
  r.take();
  return { requests: vaultRequests(host), seen: r.seen, before, refused, converted, kept: $("#vault-message").textContent, active: host.active };
}

/** Encrypting a plain Vault, stopped at the spell step: the new Vault is discarded, the original kept. */
async function encryptDiscardFlow() {
  const { $, host } = await loadSidePanel({ vaults: { A: pages("A", 1) }, active: "A", encryption: true, restore: true, convert: true });
  const r = recorder($);
  await settle(30);
  await openSettings($);
  await click($, "vault-convert-pick");
  r.take();
  const step = { path: $("#vault-onboarding-path").textContent, confirm: $("#vault-spell-confirm").textContent, cancel: $("#vault-onboarding-cancel").textContent };
  await click($, "vault-onboarding-cancel");
  r.take();
  return { requests: vaultRequests(host), seen: r.seen, step, discarded: $("#vault-message").textContent, active: host.active, cancelLabel: $("#vault-onboarding-cancel").textContent };
}

const noJapanese = (texts) => texts.filter((text) => JAPANESE.test(text.replace(/C:\\[^\n]*/g, "")));

test("Vault creation: the same requests in both languages; the spell is never part of a message", async () => {
  const ja = await createFlow();
  const en = await withLocale("en", createFlow);
  assert.deepEqual(en.requests, ja.requests, "the language changes nothing that is sent");
  assert.deepEqual(ja.requests.map((m) => m.type), ["vault_create_encrypted", "vault_create_encrypted_cancel", "vault_create_encrypted", "vault_create_encrypted_commit"]);
  assert.equal(en.active, ja.active);
  assert.deepEqual(noJapanese(en.seen), []);
  for (const shown of [...ja.seen, ...en.seen].filter((text) => text !== TEST_SPELL)) assert.ok(!shown.includes(TEST_SPELL.slice(0, 12)) && !shown.includes(OTHER_SPELL.slice(0, 12)), shown);
  assert.match(ja.mismatch, /一致しません[\s\S]*Vaultの作成はまだ完了していません/);
  assert.equal(en.mismatch, "The spell entered does not match the Recovery Spell shown.\nCheck what you wrote down and enter it again (Vault creation is not yet complete).");
  assert.equal(en.tooShort, "The Recovery Spell has 43 characters (40 were entered).\nCheck what you wrote down and enter it again (Vault creation is not yet complete).");
  assert.equal(en.done, "Created an encrypted Vault and started using it:\nC:\\Vaults\\memocampus.sqlite3\nMCP access is not approved. Approve it below if needed.");
  assert.ok(en.seen.includes("Discarded the partially created encrypted Vault. The Active Vault was not changed."));
  assert.ok(en.seen.includes("Recovery Spell"), "the title is the product name");
  assert.ok(en.seen.some((text) => /Recovery Spell/.test(text) && !/recovery phrase/i.test(text)));
});

test("Restore: the restored Vault is a separate step from using it, in both languages; requests are the same", async () => {
  const ja = await restoreFlow();
  const en = await withLocale("en", restoreFlow);
  assert.deepEqual(en.requests, ja.requests);
  assert.deepEqual(ja.requests.map((m) => m.type), ["vault_restore_inspect", "vault_restore", "vault_restore_discard", "vault_restore_inspect", "vault_restore", "vault_restore_activate"]);
  assert.equal(ja.restored.active, "A");
  assert.equal(en.restored.active, "A", "restored, not active, until Start using");
  assert.equal(ja.discarded.active, "A");
  assert.equal(en.discarded.active, "A");
  assert.equal(en.active, ja.active);
  assert.deepEqual(noJapanese(en.seen), []);
  assert.equal(en.noPassword, "Enter the backup's password.");
  assert.match(en.source, /^Backup to restore: C:\\Backups\\memocampus-backup\.mcbackup\nFormat D \(encrypted Vault \+ password protection\)\. Restoring needs both the password and the Recovery Spell\./);
  assert.match(en.source, /A restore always creates a new Vault\. Existing files and Vaults are not overwritten\. The current Vault does not change until you choose “Start using” for the restored Vault\./);
  assert.match(en.restored.title, /^Restored \(not in use yet\): C:\\Restored\\memocampus-restored\.sqlite3\nEncrypted Vault/);
  assert.match(en.restored.title, /Visits: 1,234 · Snapshots: 0 · Images: 0/, "numbers in English format");
  assert.match(ja.restored.title, /閲覧 1,234件・スナップショット 0件・画像 0件/);
  assert.match(en.restored.title, /Choosing “Start using” makes this Vault the Active Vault\. MCP access stays not approved\.$/);
  assert.match(en.restored.policy, /revision 7\)\. It is not applied to the current privacy settings automatically\./);
  assert.equal(en.restored.message, "The restore is complete. Review the contents, then start using it or discard it.");
  assert.equal(en.discarded.message, "Discarded the restored Vault (or the interrupted restore). The Active Vault was not changed.");
  assert.match(en.activated, /^Started using the restored Vault:\nC:\\Restored\\memocampus-restored\.sqlite3\nMCP access is not approved\./);
});

test("Removing encryption: the same acknowledgement is required in both languages and the same request is sent", async () => {
  const ja = await decryptFlow();
  const en = await withLocale("en", decryptFlow);
  assert.deepEqual(en.requests, ja.requests);
  assert.deepEqual(ja.requests.map(({ type, direction, acknowledgePlaintext, expectedVaultToken }) => ({ type, direction, acknowledgePlaintext, expectedVaultToken })), [
    { type: "vault_convert_start", direction: "decrypt", acknowledgePlaintext: true, expectedVaultToken: tokenOf("E") },
    { type: "vault_convert_keep", direction: undefined, acknowledgePlaintext: undefined, expectedVaultToken: undefined },
  ]);
  for (const flow of [ja, en]) {
    assert.equal(flow.before.disabled, true, "not without the acknowledgement");
    assert.equal(flow.refused.sent, 0, "a click without the acknowledgement sends nothing");
    assert.equal(flow.active, "E", "kept, not switched");
  }
  assert.deepEqual(noJapanese(en.seen), []);
  assert.equal(en.before.title, "Remove encryption from this Vault");
  assert.match(en.before.note, /^Creates a new unencrypted Vault\. Browsing history, saved images and other data are stored on disk without encryption\. The original encrypted Vault is not deleted\./);
  assert.equal(en.before.pick, "Choose where to save and remove encryption…");
  assert.equal(en.refused.message, "Removing encryption stores browsing history and images on disk without encryption. Select the confirmation check box, then try again.");
  assert.match(en.converted, /^Converted \(not in use yet\): /);
  assert.match(en.converted, /\nSource \(kept as it is\): E\nA new unencrypted Vault \(its contents are not encrypted\)\n/);
  assert.match(en.kept, /^Saved the converted Vault without switching to it:\n[\s\S]*\nThe Active Vault was not changed\./);
});

test("Encrypting, stopped at the spell: the new Vault is discarded, the original kept, in both languages", async () => {
  const ja = await encryptDiscardFlow();
  const en = await withLocale("en", encryptDiscardFlow);
  assert.deepEqual(en.requests, ja.requests);
  assert.deepEqual(ja.requests.map((m) => m.type), ["vault_convert_start", "vault_convert_discard"]);
  assert.equal(en.active, "A");
  assert.deepEqual(noJapanese(en.seen), []);
  assert.match(en.step.path, /^Destination \(new encrypted Vault\): .+\nSource \(kept as it is\): A\nThis is the Recovery Spell of the new encrypted Vault\./);
  assert.equal(en.step.confirm, "Confirm and finish converting");
  assert.equal(en.step.cancel, "Stop converting (discard the encrypted Vault that was created)");
  assert.equal(en.discarded, "Discarded the converted Vault (or the partially converted file). The original Vault and the Active Vault were not changed.");
  assert.equal(en.cancelLabel, "Stop creating", "the label returns to the creation step's");
  assert.equal(ja.cancelLabel, "作成をやめる");
});

// --- Static text ----------------------------------------------------------------------------------------------

/** The Vault section's elements with data-i18n*, localized by the real i18n-dom.js. */
function localizeVault(locale) {
  const elements = [];
  for (const [, tag, attributes] of vaultHtml.matchAll(/<([a-z0-9]+)\b([^>]*\bdata-i18n[^>]*)>/g)) {
    const attrs = new Map([...attributes.matchAll(/([\w-]+)="([^"]*)"/g)].map(([, name, value]) => [name, value]));
    elements.push({ tag, attrs, textContent: "", getAttribute: (name) => (attrs.has(name) ? attrs.get(name) : null), setAttribute: (name, value) => attrs.set(name, String(value)) });
  }
  const document = { documentElement: {}, querySelectorAll: () => elements };
  vm.runInContext(readFileSync(path.join(extensionDir, "i18n-dom.js"), "utf8"), vm.createContext({ document, chrome: { i18n: createI18n({ locale }) } }));
  return (key) => elements.find((element) => element.attrs.get("data-i18n") === key || element.attrs.get("data-i18n-aria-label") === key);
}

test("sidepanel.html: the Vault section's labels, buttons and accessible names in English", () => {
  const en = localizeVault("en");
  assert.equal(en("vaultDecryptAck").textContent, "I understand that browsing history, saved images and other data will be stored on disk without encryption");
  assert.equal(en("vaultEncryptChoice").textContent, "Encrypt (recommended)");
  assert.equal(en("vaultOpenPickButton").textContent, "Open existing Vault…");
  assert.equal(en("vaultRestorePickButton").textContent, "Restore from backup…");
  assert.equal(en("vaultRestoreActivateButton").textContent, "Start using this Vault (MCP access not approved)");
  assert.equal(en("vaultConvertKeepButton").textContent, "Save without switching and finish");
  assert.equal(en("vaultConvertDiscardButton").textContent, "Discard the converted Vault");
  assert.equal(en("vaultPathInputLabel").attrs.get("aria-label"), "Path of the Vault or of the backup folder");
  assert.equal(en("vaultSpellInputLabel").textContent, "Recovery Spell (43 characters)");
  // The recovery input's accessible name and the visible labels of the other spell inputs say the same.
  const recovery = [...vaultHtml.matchAll(/<input id="vault-recovery-input"[^>]*data-i18n-aria-label="(\w+)"/g)].map((m) => m[1]);
  assert.deepEqual(recovery, ["vaultSpellInputLabel"]);
  // Quoted button names in messages match the buttons.
  const messages = catalog("en");
  for (const [key, quoted] of [["vaultRestoreNote", "Start using"], ["vaultConvertedKeepNote", "Save without switching"], ["vaultConvertKept", "Open existing Vault"], ["vaultSpellNote", "Reissue the Recovery Spell"], ["vaultSpellNote", "Copy"]]) {
    assert.ok(messages[key].message.includes(`“${quoted}”`), key);
  }
  assert.ok(messages.vaultRestoreActivateButton.message.startsWith("Start using"));
  assert.ok(messages.vaultConvertKeepButton.message.startsWith("Save without switching"));
  assert.ok(messages.vaultOpenPickButton.message.startsWith("Open existing Vault"));
  assert.equal(messages.vaultReissueTitle.message, "Reissue the Recovery Spell");
  assert.equal(messages.vaultSpellCopyButton.message, "Copy");
});

test("the radio and checkbox values and the manual buttons' operations are not text", () => {
  assert.deepEqual([...vaultHtml.matchAll(/<input id="(vault-[\w-]+)" type="(radio|checkbox)"(?: name="[\w-]+")?(?: value="(\w+)")?/g)].map((m) => m.slice(1)), [
    ["vault-encryption-encrypted", "radio", "encrypted"], ["vault-encryption-plain", "radio", "plain"],
    ["vault-backup-normal", "radio", "normal"], ["vault-backup-password", "radio", "password"],
    ["vault-convert-ack", "checkbox", undefined],
  ]);
  assert.deepEqual([...vaultHtml.matchAll(/data-manual="(\w+)" data-i18n="(\w+)"/g)].map((m) => [m[1], m[2]]), [
    ["vault_create", "vaultManualCreateButton"], ["vault_open", "vaultManualOpenButton"], ["vault_backup", "vaultManualBackupButton"],
  ]);
  // Secrets are never filled from a message: no data-i18n on an input, and the spell code element stays empty.
  assert.doesNotMatch(vaultHtml, /<input[^>]*\sdata-i18n="/);
  assert.match(vaultHtml, /<code id="vault-spell" class="vault-spell" translate="no"><\/code>/);
});

// --- Display helpers ---------------------------------------------------------------------------------------

test("states, verdicts, reasons and pending steps read in English; unknown or prototype names are not looked up", async () => {
  await withLocale("en", () => {
    assert.equal(describeStatus({ state: "ready" }), "Connected");
    assert.equal(describeStatus({ state: "not_configured" }), "No Vault is set up");
    assert.equal(describeStatus({ state: "missing" }), "The Vault file cannot be found");
    assert.equal(describeStatus({ state: "vault_locked" }), "The encrypted Vault is locked. The file was not changed");
    assert.equal(describeStatus({ state: "ready", overridden: true }), "Using the development database MEMOCAMPUS_DB (Connected)");
    for (const state of ["constructor", "__proto__", "toString", "no_such_state", 7]) assert.equal(describeStatus({ state }), "The Vault's state cannot be checked", String(state));
    assert.equal(describeStatus({ state: "constructor", overridden: true }), "Using the development database MEMOCAMPUS_DB (constructor)");
    assert.equal(describeVerdict("needs_approval"), "This location may be risky");
    assert.equal(describeVerdict("constructor"), "constructor");
    assert.deepEqual(describeReasons(["sync_folder", "toString", "new_reason"]), ["It is inside a sync folder (OneDrive or similar)", "toString", "new_reason"]);
    assert.equal(describePublication({ publication: "confirmed", publicationPolicyRevision: 12 }), "MCP access: approved (as of privacy settings revision 12)");
    assert.equal(describePublication({ publication: "pending" }), "MCP access: not approved");
    assert.equal(describePublication({ publication: "toString" }), "");
    assert.equal(describePolicyCopy({ state: "valid", revision: "x" }), "This backup includes a copy of the privacy settings (revision ?). It is not applied to the current privacy settings automatically. Review it and, if needed, make the same settings in “Privacy settings”.");
    assert.equal(formatCounts({ visits: 12345, snapshots: 6, pages: 1, pageBlobs: 1 }), "Visits: 12,345 · Snapshots: 6 · Saved pages: 1 · Original MHTML files: 1");
    assert.equal(describeRestorePending({ phase: "restoring" }), "There is an interrupted restore.\nDiscard the partial file, then restore again.");
    assert.equal(describeConversionPending({ phase: "verified", direction: "encrypt", path: "C:\\N.sqlite3" }),
      "There is a conversion that was interrupted before its Recovery Spell was confirmed:\nC:\\N.sqlite3\nThe spell cannot be shown again, so discard it, then convert again. The original Vault was not changed.");
    assert.match(describeConversionPending({ phase: "verified", direction: "decrypt", path: "C:\\P.sqlite3" }), /^There is an unencrypted Vault \(encryption removed\) that was converted but is not in use yet:/);
    assert.equal(texts.SPELL_TITLE, "Recovery Spell");
  });
  // Japanese is unchanged.
  assert.equal(describeStatus({ state: "ready", overridden: true }), "開発用DB(MEMOCAMPUS_DB)を使用中（接続中）");
  assert.equal(formatCounts({ visits: 12345, snapshots: 6 }), "閲覧 12,345件・スナップショット 6件");
  assert.equal(describePublication({ publication: "confirmed", publicationPolicyRevision: 12 }), "MCPへの公開: 承認済み（プライバシー設定 改訂12の時点で承認）");
  assert.equal(texts.SPELL_TITLE, "ふっかつのじゅもん（Recovery Spell）");
});

test("paths and host values are shown as text: no markup, placeholder or $ in them is interpreted", async () => {
  const tricky = "C:\\<img src=x onerror=alert(1)>\\$1 $$ $path$ {path}.sqlite3";
  for (const locale of ["ja", "en"]) {
    await withLocale(locale, () => {
      assert.ok(describeRestorePending({ phase: "restored", path: tricky }).includes(`\n${tricky}\n`), locale);
      assert.ok(describeConversionPending({ phase: "converted", direction: "encrypt", path: tricky }).includes(`\n${tricky}\n`), locale);
      assert.ok(describeReasons([tricky])[0] === tricky);
    });
  }
  for (const file of ["vault-panel.js", "vault-client.js"]) {
    assert.doesNotMatch(readFileSync(path.join(extensionDir, file), "utf8"), /innerHTML|outerHTML|insertAdjacentHTML|document\.write|eval\(|new Function/, file);
  }
});

// --- A missing message fails closed --------------------------------------------------------------------------

/** Replaces chrome.i18n so that `keys` are missing (Chrome returns "" for an unknown message). */
function withoutMessages(keys) {
  const real = globalThis.chrome.i18n;
  globalThis.chrome.i18n = { ...real, getMessage: (key, values) => (keys.includes(key) ? "" : real.getMessage(key, values)) };
  return () => { globalThis.chrome.i18n = real; };
}

test("a missing message never lets encryption be removed without the acknowledgement", async () => {
  const { $, host } = await loadSidePanel({ vaults: { E: pages("E", 2) }, active: "E", encryption: true, restore: true, convert: true });
  host.encrypted.add("E");
  await settle(30);
  await openSettings($);
  withoutMessages(["vaultDecryptNotAcknowledged"]);
  // Even if the button were enabled by mistake, the click handler refuses and sends nothing.
  $("#vault-convert-pick").disabled = false;
  assert.throws(() => $("#vault-convert-pick").dispatch("click"), /Missing extension message: vaultDecryptNotAcknowledged/);
  await settle(30);
  assert.equal(vaultRequests(host).length, 0);
  assert.equal(host.active, "E");
});

test("a render that fails on a missing message still sets every button from the status", async () => {
  const { $, host } = await loadSidePanel({ vaults: { E: pages("E", 2) }, active: "E", encryption: true, restore: true, convert: true });
  host.encrypted.add("E");
  await settle(30);
  await openSettings($);
  assert.equal($("#vault-convert-pick").disabled, true);
  withoutMessages(["vaultConvertSourceLine"]);
  // Stale state as if an earlier step had left these enabled.
  $("#vault-convert-pick").disabled = false;
  $("#vault-restore-activate").disabled = false;
  assert.throws(() => $("#vault-backup-normal").dispatch("change"), /Missing extension message: vaultConvertSourceLine/);
  assert.equal($("#vault-convert-pick").disabled, true, "no acknowledgement: still locked");
  assert.equal($("#vault-restore-activate").disabled, true, "nothing restored: nothing to start using");
  assert.equal(vaultRequests(host).length, 0);
});

test("a missing message during a restore sends nothing and starts nothing", async () => {
  const { $, host } = await loadSidePanel({ vaults: { A: pages("A", 1) }, active: "A", restore: true });
  host.backupFiles[BACKUP] = { format: "C", mode: "plain", password: "outer password", pages: pages("R", 1) };
  await settle(30);
  await openSettings($);
  await click($, "vault-restore-pick", 30);
  withoutMessages(["vaultRestorePasswordRequired"]);
  assert.throws(() => $("#vault-restore-submit").dispatch("click"), /vaultRestorePasswordRequired/);
  await settle(30);
  assert.deepEqual(vaultRequests(host).map((m) => m.type), ["vault_restore_inspect"]);
  assert.equal(host.active, "A");
  assert.equal(host.restorePending, null);
});

// --- Review F-77-S3-01: the spell leaves the page even when a label message is missing -------------------------

const GENERIC_JA = "操作に失敗しました。";
const spellOnPage = ($) => ["#vault-spell", "#vault-spell-confirm-input", "#vault-onboarding-path", "#vault-message"]
  .filter((selector) => `${$(selector).textContent}|${$(selector).value}`.includes(TEST_SPELL.slice(0, 12)));

/** An encrypted creation at its spell step: the spell shown, and typed into the confirmation as well. */
async function atSpellStep() {
  const loaded = await loadSidePanel({ vaults: { A: pages("A", 1) }, active: "A", encryption: true });
  await settle(30);
  await openSettings(loaded.$);
  await click(loaded.$, "vault-create-default");
  assert.equal(loaded.$("#vault-spell").textContent, TEST_SPELL);
  loaded.$("#vault-spell-confirm-input").value = TEST_SPELL;
  return loaded;
}

test("F-77-S3-01: stopping the creation clears the spell and hides the step even if vaultSpellConfirmCreate is missing", async () => {
  const { $, host } = await atSpellStep();
  const restore = withoutMessages(["vaultSpellConfirmCreate"]);
  await click($, "vault-onboarding-cancel");
  restore();
  assert.deepEqual(spellOnPage($), [], "neither the shown nor the typed spell is left");
  assert.equal($("#vault-spell").textContent, "");
  assert.equal($("#vault-spell-confirm-input").value, "");
  assert.equal($("#vault-onboarding-path").textContent, "");
  assert.equal($("#vault-onboarding").hidden, true);
  assert.deepEqual(vaultRequests(host).map((m) => m.type), ["vault_create_encrypted", "vault_create_encrypted_cancel"], "no commit");
  assert.equal($("#vault-message").textContent, GENERIC_JA, "not reported as done");
  assert.equal(host.active, "A");
  // The step ended: a confirmation click now sends nothing; a new creation can start.
  $("#vault-spell-confirm-input").value = TEST_SPELL;
  await click($, "vault-spell-confirm", 20);
  assert.equal(vaultRequests(host).filter((m) => m.type === "vault_create_encrypted_commit").length, 0);
  await click($, "vault-create-default");
  assert.equal($("#vault-onboarding").hidden, false);
  assert.equal($("#vault-spell-confirm").textContent, "確認して作成を完了する");
});

test("F-77-S3-01: finishing the creation clears the spell even if vaultOnboardingCancelCreate is missing; the commit is sent once", async () => {
  const { $, host } = await atSpellStep();
  await click($, "vault-spell-written", 10);
  $("#vault-spell-confirm-input").value = TEST_SPELL;
  const restore = withoutMessages(["vaultOnboardingCancelCreate"]);
  await click($, "vault-spell-confirm");
  restore();
  assert.deepEqual(spellOnPage($), []);
  assert.equal($("#vault-spell-confirm-input").value, "");
  assert.equal($("#vault-onboarding-path").textContent, "");
  assert.equal($("#vault-onboarding").hidden, true);
  assert.equal(vaultRequests(host).filter((m) => m.type === "vault_create_encrypted_commit").length, 1, "the person's one confirmation, never repeated");
  assert.equal(host.active, "C:\\Vaults\\memocampus.sqlite3");
});

test("F-77-S3-01: without missing messages, stopping and finishing behave as before", async () => {
  const stopped = await atSpellStep();
  await click(stopped.$, "vault-onboarding-cancel");
  assert.deepEqual(spellOnPage(stopped.$), []);
  assert.equal(stopped.$("#vault-message").textContent, "作成途中の暗号化Vaultを破棄しました。Active Vaultは変更していません。");
  assert.equal(stopped.$("#vault-onboarding-cancel").textContent, "作成をやめる");
  const finished = await atSpellStep();
  await click(finished.$, "vault-spell-written", 10);
  finished.$("#vault-spell-confirm-input").value = TEST_SPELL;
  await click(finished.$, "vault-spell-confirm");
  assert.deepEqual(spellOnPage(finished.$), []);
  assert.match(finished.$("#vault-message").textContent, /^暗号化Vaultを作成し、使用を開始しました/);
});

// --- Review F-77-S3-02: a missing message while a step starts sends nothing and still ends the step -------------

test("F-77-S3-02: vaultWorking missing: no backup is sent, the secrets are cleared, and the panel is usable again", async () => {
  const { $, host, document } = await loadSidePanel({ vaults: { A: pages("A", 1) }, active: "A" });
  await settle(30);
  await openSettings($);
  const choosePassword = () => {
    $("#vault-backup-normal").checked = false;
    $("#vault-backup-password").checked = true;
    $("#vault-backup-password").dispatch("change");
    $("#vault-backup-password-input").value = "password-123";
    $("#vault-backup-password-confirm").value = "password-123";
  };
  choosePassword();
  $("#vault-path-input").value = "C:\\Backups";
  const restore = withoutMessages(["vaultWorking"]);
  document.querySelector('[data-manual="vault_backup"]').dispatch("click");
  await settle(40);
  restore();
  assert.equal(vaultRequests(host).length, 0, "nothing sent");
  assert.equal($("#vault-backup-password-input").value, "");
  assert.equal($("#vault-backup-password-confirm").value, "");
  assert.equal($("#vault-message").textContent, GENERIC_JA, "a failure, not a host error and not a success");
  assert.equal($("#vault-backup-pick").disabled, false, "busy was released");
  assert.equal($("#vault-create-pick").disabled, false);
  choosePassword();
  await click($, "vault-backup-pick");
  assert.deepEqual(vaultRequests(host).map(({ type, protection, password }) => ({ type, protection, password })), [{ type: "vault_backup", protection: "password", password: "password-123" }]);
});

test("F-77-S3-02: vaultChooseInDialog missing: removing encryption sends nothing and the acknowledgement is reset", async () => {
  const { $, host } = await loadSidePanel({ vaults: { E: pages("E", 2) }, active: "E", encryption: true, restore: true, convert: true });
  host.encrypted.add("E");
  await settle(30);
  await openSettings($);
  const acknowledge = () => {
    $("#vault-convert-ack").checked = true;
    $("#vault-convert-ack").dispatch("change");
  };
  acknowledge();
  const restore = withoutMessages(["vaultChooseInDialog"]);
  await click($, "vault-convert-pick");
  restore();
  assert.equal(vaultRequests(host).length, 0);
  assert.equal($("#vault-convert-ack").checked, false, "given for one request only, also when it failed");
  assert.equal($("#vault-convert-pick").disabled, true, "locked again until acknowledged again");
  assert.equal($("#vault-message").textContent, GENERIC_JA);
  assert.equal($("#vault-converted").hidden, true);
  assert.equal(host.active, "E");
  acknowledge();
  await click($, "vault-convert-pick");
  assert.deepEqual(vaultRequests(host).map(({ type, direction, acknowledgePlaintext, expectedVaultToken }) => ({ type, direction, acknowledgePlaintext, expectedVaultToken })),
    [{ type: "vault_convert_start", direction: "decrypt", acknowledgePlaintext: true, expectedVaultToken: tokenOf("E") }]);
});

// --- Review blind spot A: a missing message when the spell step starts --------------------------------------

test("a missing message when the spell step starts: no step, no confirmation, nothing usable that should not be", async () => {
  const cases = [
    { name: "create", missing: "vaultSpellConfirmCreate", vaults: { A: pages("A", 1) }, active: "A", start: "vault-create-default", confirm: "vault_create_encrypted_commit" },
    { name: "encrypt", missing: "vaultOnboardingConvertPaths", vaults: { A: pages("A", 1) }, active: "A", start: "vault-convert-pick", confirm: "vault_convert_confirm" },
    { name: "reissue", missing: "vaultReissueSpellNote", vaults: { E: pages("E", 1) }, active: "E", encrypted: true, start: "vault-reissue-pick", confirm: "vault_convert_confirm" },
  ];
  for (const c of cases) {
    const { $, host } = await loadSidePanel({ vaults: c.vaults, active: c.active, encryption: true, restore: true, convert: true });
    if (c.encrypted) host.encrypted.add(c.active);
    await settle(30);
    await openSettings($);
    const restore = withoutMessages([c.missing]);
    await click($, c.start);
    restore();
    assert.equal($("#vault-onboarding").hidden, true, c.name);
    assert.equal($("#vault-spell").textContent, "", `${c.name}: the spell is not shown`);
    assert.ok(!$("#vault-message").textContent.includes(TEST_SPELL.slice(0, 12)) && !$("#vault-message").textContent.includes(OTHER_SPELL.slice(0, 12)), c.name);
    $("#vault-spell-confirm-input").value = TEST_SPELL;
    await click($, "vault-spell-confirm", 20);
    assert.equal(vaultRequests(host).filter((m) => m.type === c.confirm).length, 0, `${c.name}: no confirmation without the step`);
    assert.equal(host.active, c.active, `${c.name}: nothing switched`);
    // What the host keeps is offered only for discarding: the spell can never be shown again.
    if (c.name === "create") assert.equal($("#vault-creation-pending").hidden, false);
    else {
      assert.equal($("#vault-converted").hidden, false, c.name);
      assert.equal($("#vault-convert-activate").hidden, true, `${c.name}: cannot be put into use`);
      assert.equal($("#vault-convert-keep").hidden, true, c.name);
    }
  }
});

// --- Review blind spot B: re-issuing sends the same requests in both languages -----------------------------

async function reissueFlow() {
  Object.defineProperty(globalThis, "navigator", { value: { clipboard: { writeText: async () => {} } }, configurable: true, writable: true });
  const { $, host } = await loadSidePanel({ vaults: { E: pages("E", 2) }, active: "E", encryption: true, restore: true, convert: true });
  host.encrypted.add("E");
  await settle(30);
  await openSettings($);
  await click($, "vault-reissue-pick");
  const spell = $("#vault-spell").textContent;
  await click($, "vault-spell-written", 10);
  $("#vault-spell-confirm-input").value = spell.slice(0, 42);
  await click($, "vault-spell-confirm", 20);
  const beforeConfirm = vaultRequests(host).map((m) => m.type);
  $("#vault-spell-confirm-input").value = spell;
  await click($, "vault-spell-confirm");
  return { requests: vaultRequests(host), beforeConfirm, spell, shown: Object.values(shownText($)) };
}

test("re-issuing the spell: the same requests in both languages, the confirmation only after the full spell", async () => {
  const ja = await reissueFlow();
  const en = await withLocale("en", reissueFlow);
  assert.deepEqual(en.requests, ja.requests);
  assert.equal(en.spell, ja.spell);
  assert.deepEqual(ja.beforeConfirm, ["vault_convert_start"], "nothing confirmed before the whole spell matches");
  assert.deepEqual(en.beforeConfirm, ["vault_convert_start"]);
  const [start, confirm] = en.requests;
  assert.deepEqual({ type: start.type, direction: start.direction, token: start.expectedVaultToken, ack: start.acknowledgePlaintext }, { type: "vault_convert_start", direction: "reissue", token: tokenOf("E"), ack: undefined });
  assert.deepEqual({ type: confirm.type, recoverySpell: confirm.recoverySpell }, { type: "vault_convert_confirm", recoverySpell: en.spell });
  assert.equal(en.requests.length, 2);
  // No display text in any request.
  const sentText = JSON.stringify(en.requests) + JSON.stringify(ja.requests);
  assert.doesNotMatch(sentText, JAPANESE);
  // (the protocol value "reissue" is expected; the English texts start with capitals or are whole phrases)
  assert.doesNotMatch(sentText, /Recovery Spell|Reissue|Reissued|not in use|Choose where/);
  assert.deepEqual(noJapanese(en.shown), []);
});

// --- Catalog --------------------------------------------------------------------------------------------------

test("Slice 3 keys: ja and en agree, Recovery Spell is the product name, and every security key says what it protects", () => {
  const ja = catalog("ja"), en = catalog("en");
  const vaultKeys = Object.keys(ja).filter((key) => key.startsWith("vault") && !["vaultItemsUnavailable"].includes(key));
  assert.ok(vaultKeys.length >= 200, String(vaultKeys.length));
  for (const key of vaultKeys) {
    assert.ok(en[key], key);
    assert.doesNotMatch(en[key].message, /recovery (phrase|key|code)|seed/i, key);
    if (/ふっかつのじゅもん/.test(ja[key].message)) assert.match(en[key].message, /Recovery Spell/, key);
    if (/暗号化を解除/.test(ja[key].message)) assert.match(en[key].message, /[Rr]emov\w+ encryption|encryption removed/, key);
    if (/変更していません/.test(ja[key].message)) assert.match(en[key].message, /not changed|Neither .+ was changed/, key);
    if (/まだ使用していません/.test(ja[key].message)) assert.match(en[key].message, /not in use yet/, key);
  }
  for (const key of ["vaultBackupPasswordNote", "vaultPasswordLabel", "vaultPasswordConfirmLabel", "vaultRestorePasswordLabel"]) assert.match(en[key].description, /SECRET/, key);
  for (const key of ["vaultCreationDiscardButton", "vaultRestoreDiscardButton", "vaultConvertDiscardButton", "vaultOnboardingCancelCreate", "vaultOnboardingCancelConvert"]) assert.match(en[key].description, /DESTRUCTIVE/, key);
  for (const key of ["vaultDecryptAck", "vaultDecryptNotAcknowledged", "vaultConvertDecryptNote"]) assert.match(en[key].description, /most important/, key);
});
