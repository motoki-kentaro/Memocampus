// Issue #48: a backup is of the vault the panel shows. The side panel sends that vault's token
// (`expectedVaultToken`, from `vault_status`); the host refuses it if another vault is active (here
// the fake host, which follows the real host's rules; the real host is tested in native-host).
import { test } from "node:test";
import assert from "node:assert/strict";
import { loadSidePanel, pages, settle, tokenOf } from "./fake-dom.js";

const backups = (host) => host.requests.filter((message) => message.type === "vault_backup");

async function openSettings($) {
  $("#tab-settings").dispatch("click");
  await settle(20);
}

test("a backup sends the token of the vault shown and names its source and destination", async () => {
  const { $, host } = await loadSidePanel({ vaults: { A: pages("A", 2), B: pages("B", 1) }, active: "A" });
  await settle(30);
  await openSettings($);
  assert.equal($("#vault-path").textContent, "A");
  assert.equal($("#vault-backup-pick").disabled, false);
  $("#vault-backup-pick").dispatch("click");
  await settle(30);
  assert.deepEqual(backups(host).map(({ expectedVaultToken, directory }) => ({ expectedVaultToken, directory })), [{ expectedVaultToken: tokenOf("A"), directory: undefined }]);
  assert.deepEqual(host.backups, [{ vault: "A", backupPath: "C:\\Backups\\memocampus-backup-0.sqlite3" }]);
  assert.equal($("#vault-message").textContent, "バックアップを作成しました（形式A）。\nバックアップ元: A\n保存先: C:\\Backups\\memocampus-backup-0.sqlite3");

  // The folder typed by hand: the same token goes with it.
  $("#vault-path-input").value = "D:\\Backups";
  document.querySelector('[data-manual="vault_backup"]').dispatch("click");
  await settle(30);
  assert.deepEqual(backups(host)[1].expectedVaultToken, tokenOf("A"));
  assert.equal(backups(host)[1].directory, "D:\\Backups");
});

test("W0: the panel still shows A while another window made B active, so nothing is backed up", async () => {
  const { $, host } = await loadSidePanel({ vaults: { A: pages("A", 2), B: pages("B", 1) }, active: "A" });
  await settle(30);
  await openSettings($);
  host.active = "B"; // another window opened B; this panel was not told
  assert.equal($("#vault-path").textContent, "A");
  $("#vault-backup-pick").dispatch("click");
  await settle(30);
  assert.equal(backups(host)[0].expectedVaultToken, tokenOf("A"));
  assert.deepEqual(host.backups, [], "no backup of B for A");
  assert.match($("#vault-message").textContent, /表示していたVault（A）は、もうActiveではありません。バックアップは作成していません。/);
  assert.equal($("#vault-path").textContent, "B", "the panel now shows the active vault");
  // Asked again, it is B's, as shown now.
  $("#vault-backup-pick").dispatch("click");
  await settle(30);
  assert.equal(backups(host)[1].expectedVaultToken, tokenOf("B"));
  assert.deepEqual(host.backups.map(({ vault }) => vault), ["B"]);
  assert.match($("#vault-message").textContent, /バックアップ元: B/);
});

test("a switch while the folder dialog is open is refused, and the panel follows it", async () => {
  const { $, host } = await loadSidePanel({ vaults: { A: pages("A", 2), B: pages("B", 1) }, active: "A" });
  await settle(30);
  await openSettings($);
  host.backupDelay = 20;
  host.backupDuring = () => { host.active = "B"; };
  $("#vault-backup-pick").dispatch("click");
  await settle(5);
  assert.equal($("#vault-backup-pick").disabled, true, "no second backup while one runs");
  await settle(60);
  assert.deepEqual(host.backups, []);
  assert.match($("#vault-message").textContent, /もうActiveではありません/);
  assert.equal($("#vault-path").textContent, "B");
});

test("without a vault shown, or a status without a token (an older host), no backup is asked", async () => {
  {
    const { $, host } = await loadSidePanel({ vaults: {}, active: null });
    await settle(30);
    assert.equal($("#vault-backup-pick").disabled, true);
    $("#vault-backup-pick").dispatch("click"); // even if a click got through
    await settle(30);
    assert.deepEqual(backups(host), []);
    assert.match($("#vault-message").textContent, /バックアップするVaultを確認できないため、開始しませんでした/);
  }
  {
    const { $, host } = await loadSidePanel({ vaults: { A: pages("A", 1) }, active: "A" });
    const status = host.status;
    host.status = () => ({ ...status(), vaultToken: undefined });
    await settle(30);
    await openSettings($);
    assert.equal($("#vault-path").textContent, "A");
    assert.equal($("#vault-backup-pick").disabled, true);
    assert.equal(document.querySelector('[data-manual="vault_backup"]').disabled, true);
    $("#vault-backup-pick").dispatch("click");
    await settle(30);
    assert.deepEqual(backups(host), []);
  }
});

test("a late status of the vault before never replaces the one shown, nor the token sent", async () => {
  const { $, host } = await loadSidePanel({ vaults: { A: pages("A", 2), B: pages("B", 1) }, active: "A" });
  await settle(30);
  await openSettings($);
  // A slow status answer (A) is overtaken by a fast one (B).
  host.statusDelay = 80;
  $("#tab-url").dispatch("click");
  $("#tab-settings").dispatch("click"); // asks: A, slow
  await settle(5); // the host has answered A (delivered later)
  assert.equal(host.requests.filter((message) => message.type === "vault_status").length >= 2, true);
  host.active = "B";
  host.statusDelay = 0;
  $("#tab-url").dispatch("click");
  $("#tab-settings").dispatch("click"); // asks: B, fast
  await settle(20);
  assert.equal($("#vault-path").textContent, "B");
  await settle(100); // A's answer arrives now
  assert.equal($("#vault-path").textContent, "B", "the older answer is ignored");
  $("#vault-backup-pick").dispatch("click");
  await settle(30);
  assert.equal(backups(host)[0].expectedVaultToken, tokenOf("B"));
  assert.deepEqual(host.backups.map(({ vault }) => vault), ["B"]);
});

test("an answer naming another vault than the one asked for is reported, never shown as success", async () => {
  const { $, host } = await loadSidePanel({ vaults: { A: pages("A", 1) }, active: "A" });
  await settle(30);
  await openSettings($);
  const handle = host.handle;
  host.handle = async (message) => {
    const answer = await handle(message);
    return message.type === "vault_backup" ? { ...answer, vaultToken: "f".repeat(64), vaultPath: "X" } : answer;
  };
  $("#vault-backup-pick").dispatch("click");
  await settle(30);
  assert.match($("#vault-message").textContent, /^バックアップ元が表示中のVaultと一致しません/);
  assert.doesNotMatch($("#vault-message").textContent, /バックアップを作成しました/);
});

// Opt-in (MEMOCAMPUS_TEST_HOST): the side panel's real code drives the real host binary, one process
// per message as Chrome does, in a temporary MEMOCAMPUS_APP_DIR with synthetic vaults. Nothing touches
// the user's vault, policy, backups or Chrome profile.
const HOST_BIN = process.env.MEMOCAMPUS_TEST_HOST;

test("end to end with the real host: the vault shown is backed up; one switched away from is refused", { skip: HOST_BIN ? false : "set MEMOCAMPUS_TEST_HOST to run" }, async () => {
  const { spawn } = await import("node:child_process");
  const { mkdtempSync, mkdirSync, readdirSync, rmSync } = await import("node:fs");
  const os = await import("node:os");
  const path = await import("node:path");
  const { DatabaseSync } = await import("node:sqlite");
  const appDir = mkdtempSync(path.join(os.tmpdir(), "memocampus-backup-e2e-"));
  const env = { ...process.env, MEMOCAMPUS_APP_DIR: appDir };
  delete env.MEMOCAMPUS_DB;
  let closing = false;
  const call = (message) =>
    new Promise((resolve, reject) => {
      if (closing) return reject(new Error("test finished"));
      const child = spawn(HOST_BIN, [], { env, cwd: appDir, stdio: ["pipe", "pipe", "pipe"] });
      const chunks = [];
      child.stdout.on("data", (chunk) => chunks.push(chunk));
      child.on("error", reject);
      child.on("close", () => {
        const out = Buffer.concat(chunks);
        if (out.length < 4) return reject(new Error("no answer"));
        resolve(JSON.parse(out.subarray(4, 4 + out.readUInt32LE(0)).toString("utf8")));
      });
      const body = Buffer.from(JSON.stringify(message), "utf8");
      const length = Buffer.alloc(4);
      length.writeUInt32LE(body.length);
      child.stdin.end(Buffer.concat([length, body]));
    });
  const vaultA = path.join(appDir, "a.sqlite3");
  const vaultB = path.join(appDir, "b.sqlite3");
  const dest = path.join(appDir, "backups");
  mkdirSync(dest);
  const urls = (file) => {
    const db = new DatabaseSync(file, { readOnly: true });
    try {
      return db.prepare("SELECT url FROM visits ORDER BY url").all().map((row) => row.url);
    } finally {
      db.close();
    }
  };
  try {
    for (const [vault, url] of [[vaultB, "https://b.test/"], [vaultA, "https://a.test/"]]) {
      assert.equal((await call({ type: "vault_create", requestId: "c", path: vault })).ok, true);
      const db = new DatabaseSync(vault);
      db.prepare("INSERT INTO visits VALUES ('v', ?, 't', '2026-11-09T00:00:00.000Z')").run(url);
      db.close();
    }
    const { $ } = await loadSidePanel({ vaults: { [vaultA]: [] }, active: vaultA, forward: call });
    await settle(2500);
    $("#tab-settings").dispatch("click");
    await settle(1500);
    assert.match($("#vault-path").textContent, /a\.sqlite3$/);
    $("#vault-path-input").value = dest;
    document.querySelector('[data-manual="vault_backup"]').dispatch("click");
    await settle(2500);
    const done = readdirSync(dest);
    assert.equal(done.length, 1, $("#vault-message").textContent);
    assert.deepEqual(urls(path.join(dest, done[0])), ["https://a.test/"]);
    assert.match($("#vault-message").textContent, /^バックアップを作成しました（形式A）。\nバックアップ元: .*a\.sqlite3\n保存先: .*memocampus-backup-/);

    // Another window opens B; this panel still shows A. The backup it asks for is refused.
    assert.equal((await call({ type: "vault_open", requestId: "o", path: vaultB })).ok, true);
    assert.match($("#vault-path").textContent, /a\.sqlite3$/);
    document.querySelector('[data-manual="vault_backup"]').dispatch("click");
    await settle(2500);
    assert.deepEqual(readdirSync(dest), done, "no new file, no .partial");
    assert.match($("#vault-message").textContent, /もうActiveではありません。バックアップは作成していません。/);
    assert.match($("#vault-path").textContent, /b\.sqlite3$/, "the panel shows the active vault again");
  } finally {
    closing = true;
    await settle(200);
    rmSync(appDir, { recursive: true, force: true });
  }
});
