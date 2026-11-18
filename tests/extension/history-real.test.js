// Issue #41, Slice 2 against the REAL Native Host (opt-in): MEMOCAMPUS_TEST_HOST=<built memocampus-native-host>.
// A temporary MEMOCAMPUS_APP_DIR with a fresh vault and synthetic visits; one host process per message, as
// `sendNativeMessage` starts one. Nothing touches the user's vault, policy or Chrome profile.
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

test("the full-page History over the real host: list, marks, domain modes, page correction, errors", { skip: HOST_BIN ? false : "set MEMOCAMPUS_TEST_HOST to run" }, async () => {
  const appDir = mkdtempSync(path.join(os.tmpdir(), "memocampus-history-e2e-"));
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
    const insert = db.prepare("INSERT INTO visits VALUES (?, ?, ?, ?)");
    for (let i = 0; i < 60; i++) insert.run(`g${String(i).padStart(2, "0")}`, `https://github.com/p/${i}`, `gh ${i}`, at(2026, 11, 17, 8, i, 0));
    insert.run("s1", "https://gist.github.com/x", "gist", at(2026, 11, 17, 9, 0, 0));
    insert.run("e1", "https://example.org/", "", at(2026, 11, 17, 9, 30, 0));
    insert.run("n1", "https://github.com/next", "next day", at(2026, 11, 18, 0, 0, 1));
    insert.run("o1", "https://github.com/old", "october", at(2026, 10, 20, 12, 0, 0));
    insert.run("bad", "https://github.com/bad", "no instant", "2026-11-17T08:00:00.000Zjunk");
    db.close();

    const document = createDocument();
    const win = createWin("?date=2026-11-17&size=25");
    const app = createHistoryApp({ document, win, send: call, tabs: { create() {} }, now: () => new Date(2026, 10, 17, 9, 30), timers: createTimers() });
    await app.start();
    await settle(1500);
    const $ = (id) => document.querySelector(`#${id}`);
    const ids = () => $("history-list").children.map((r) => r.dataset.visitId);
    assert.equal(ids().length, 25);
    assert.equal(ids()[0], "e1", "newest first");
    assert.match($("result-count").textContent, /62件（1 \/ 3ページ）/, "60 + gist + example, not the next day, not the bad instant");
    const marks = () => walk($("cal-table")).filter((e) => e.className.split(" ").includes("has-visits")).map((b) => Number(b.dataset.day));
    const walk = (el, out = []) => { out.push(el); el.children.forEach((c) => walk(c, out)); return out; };
    assert.deepEqual(marks(), [17, 18]);
    // Exact domain, through the host's own rule: no subdomain.
    $("domain-input").value = "github.com";
    $("domain-input").dispatch("keydown", { key: "Enter" });
    await settle(1500);
    assert.match($("result-count").textContent, /60件/);
    // Regex over the host name only.
    $("mode-regex").dispatch("change");
    $("domain-input").value = "gist|example";
    $("domain-input").dispatch("keydown", { key: "Enter" });
    await settle(1500);
    assert.deepEqual(ids(), ["e1", "s1"]);
    assert.deepEqual(marks(), [17]);
    // A pattern the host refuses is an error at the input, never every visit.
    $("domain-input").value = "(?=x)";
    $("domain-input").dispatch("keydown", { key: "Enter" });
    await settle(1500);
    assert.match($("domain-error").textContent, /先読み・後方参照/);
    assert.equal(ids().length, 0);
    // A page past the end is replaced by the last one.
    const far = createWin("?date=2026-11-17&size=25&page=50");
    const doc2 = createDocument();
    const app2 = createHistoryApp({ document: doc2, win: far, send: call, tabs: { create() {} }, now: () => new Date(2026, 10, 17, 9, 30), timers: createTimers() });
    await app2.start();
    await settle(2000);
    assert.deepEqual(far.log, [["replace", "?date=2026-11-17&page=3&size=25"]]);
    assert.equal(doc2.querySelector("#history-list").children.length, 12);
    assert.equal(app2.vaultToken.length, 64);
  } finally {
    rmSync(appDir, { recursive: true, force: true });
  }
});
