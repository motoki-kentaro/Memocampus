// Issue #41, Slice 3 against the REAL Native Host (opt-in): MEMOCAMPUS_TEST_HOST=<built memocampus-native-host>.
// A temporary MEMOCAMPUS_APP_DIR with a fresh vault and synthetic visits and navigation; one host process per
// message. Nothing touches the user's vault, policy or Chrome profile.
import test from "node:test";
import assert from "node:assert/strict";
import path from "node:path";
import { spawn } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import os from "node:os";
import { DatabaseSync } from "node:sqlite";
import { createDocument } from "./fake-dom.js";
import { createHistoryApp } from "../../extension/history-app.js";
import { at, createTimers, createWin, settle } from "./history-fixtures.js";
import { seedLegacyPolicy } from "./real-processes.js";

const HOST_BIN = process.env.MEMOCAMPUS_TEST_HOST;
const text = (el) => [el.textContent, ...el.children.map(text)].join(" ").replace(/\s+/g, " ").trim();
const walk = (el, out = []) => { out.push(el); el.children.forEach((c) => walk(c, out)); return out; };
const withClass = (root, name) => walk(root).filter((e) => e.className.split(" ").includes(name));

test("the visit detail over the real host: present, [CENSORED], unknown, many targets, a deleted source never restored", { skip: HOST_BIN ? false : "set MEMOCAMPUS_TEST_HOST to run" }, async () => {
  const appDir = mkdtempSync(path.join(os.tmpdir(), "memocampus-history-detail-e2e-"));
  const env = { ...process.env, MEMOCAMPUS_APP_DIR: appDir };
  delete env.MEMOCAMPUS_DB;
  const call = (message) =>
    new Promise((resolve, reject) => {
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
  try {
    seedLegacyPolicy(appDir);
    const created = await call({ type: "vault_create", requestId: "c", useDefault: true });
    assert.equal(created.ok, true);
    const db = new DatabaseSync(path.join(appDir, "memocampus.sqlite3"));
    db.exec("PRAGMA foreign_keys = ON");
    const insert = db.prepare("INSERT INTO visits VALUES (?, ?, ?, ?)");
    const nav = db.prepare("INSERT INTO navigation_events (target_visit_id, source_visit_id, kind, transition_type, client_redirect, server_redirect, forward_back, from_address_bar, new_tab, occurred_at, source_censored) VALUES (?, ?, 'document', 'link', 0, 0, 0, 0, 0, ?, ?)");
    const visits = [["src", "https://source.test/start", "Source page", 8], ["cur", "https://cur.test/page", "Current page", 9], ["t1", "https://target.test/1", "Target 1", 10], ["t2", "https://target.test/2", "Target 2", 11], ["gone", "https://deleted.test/secret", "Deleted page", 12], ["cz", "https://cz.test/", "Censored source", 13], ["uk", "https://uk.test/", "No navigation", 14], ["orph", "https://orph.test/", "Source row removed", 15]];
    for (const [id, url, title, hour] of visits) insert.run(id, url, title, at(2026, 11, 17, hour, 0, 0));
    const occurred = at(2026, 11, 17, 9, 0, 0);
    nav.run("cur", "src", occurred, 0);
    nav.run("t1", "cur", occurred, 0);
    nav.run("t2", "cur", occurred, 0);
    nav.run("cz", null, occurred, 1);
    nav.run("orph", "gone", occurred, 0);
    db.exec("DELETE FROM visits WHERE event_id = 'gone'"); // ON DELETE SET NULL: the reference is gone, nothing marks it censored
    db.close();

    const document = createDocument();
    const win = createWin("?date=2026-11-17&size=25");
    const app = createHistoryApp({ document, win, send: call, tabs: { create() {} }, now: () => new Date(2026, 10, 17, 9, 30), timers: createTimers() });
    await app.start();
    await settle(1500);
    const $ = (id) => document.querySelector(`#${id}`);
    const row = (id) => $("history-list").children.find((r) => r.dataset.visitId === id);
    const open = async (id) => { withClass(row(id), "visit-toggle")[0].dispatch("click"); await settle(800); return text(withClass(row(id), "visit-detail")[0]); };

    const present = await open("cur");
    assert.ok(present.includes("Source page") && present.includes("https://source.test/start"));
    assert.match(present, /遷移先 2件/);
    assert.deepEqual(withClass(row("cur"), "detail-target").map((t) => t.dataset.visitId), ["t1", "t2"]);
    const censored = await open("cz");
    assert.ok(censored.includes("[CENSORED]") && censored.includes("削除されています"));
    assert.ok(!censored.includes("遷移元：不明"));
    const unknown = await open("uk");
    assert.ok(unknown.includes("遷移元：不明") && !unknown.includes("[CENSORED]") && !unknown.includes("直接"));
    const orphan = await open("orph");
    assert.ok(orphan.includes("遷移元：不明"), "a deleted source is not restored, and not called censored unless the host marked it");
    assert.ok(!orphan.includes("deleted.test") && !orphan.includes("Deleted page") && !orphan.includes("[CENSORED]"));
    assert.match(await open("t1"), /Current page/, "a target's source is the visit it came from");
  } finally {
    rmSync(appDir, { recursive: true, force: true });
  }
});
