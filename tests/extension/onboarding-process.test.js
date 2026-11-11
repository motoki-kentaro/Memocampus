// Real host + options controller + MCP, all in a fresh temporary APP_DIR and vault.
import { test } from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtempSync, rmSync, existsSync, readFileSync } from "node:fs";
import path from "node:path";
import os from "node:os";
import { openPage, click } from "./options-fixtures.js";
import { startMcp, confirmPublication } from "./real-processes.js";

const hostBin = process.env.MEMOCAMPUS_TEST_HOST;
const mcpBin = process.env.MEMOCAMPUS_TEST_MCP;
test("fresh onboarding with a lost response starts once, never backfills, and keeps MCP pending until separately approved", {
  skip: hostBin && mcpBin ? false : "set MEMOCAMPUS_TEST_HOST and MEMOCAMPUS_TEST_MCP to run",
}, async () => {
  const app = mkdtempSync(path.join(os.tmpdir(), "memocampus-24-process-"));
  const env = { ...process.env, MEMOCAMPUS_APP_DIR: app };
  delete env.MEMOCAMPUS_DB;
  const calls = [];
  const call = (message) => new Promise((resolve, reject) => {
    calls.push(message);
    const child = spawn(hostBin, [], { env, cwd: app, windowsHide: true, stdio: ["pipe", "pipe", "pipe"] });
    const chunks = [];
    const timeout = setTimeout(() => { child.kill(); reject(new Error("host timeout")); }, 15_000);
    child.on("error", reject);
    child.stdout.on("data", (chunk) => chunks.push(chunk));
    child.on("close", () => {
      clearTimeout(timeout);
      const out = Buffer.concat(chunks);
      if (out.length < 4) return reject(new Error("no response"));
      resolve(JSON.parse(out.subarray(4, out.readUInt32LE(0) + 4)));
    });
    const body = Buffer.from(JSON.stringify({ requestId: crypto.randomUUID(), ...message }));
    const size = Buffer.alloc(4);
    size.writeUInt32LE(body.length);
    child.stdin.end(Buffer.concat([size, body]));
  });
  const servers = [];
  try {
    const early = new Date().toISOString();
    const page = await openPage({ send: call });
    assert.equal(page.$("onboarding").hidden, false);
    assert.equal(existsSync(path.join(app, "policy.json")), false);
    assert.equal((await call({ type: "vault_create", useDefault: true })).ok, true);
    await page.page.reload();
    const lossy = await openPage({ send: async (message) => {
      const result = await call(message);
      return message.type === "policy_start" ? undefined : result;
    } });
    click(lossy.$("start-recording"));
    click(lossy.$("start-recording"));
    const deadline = Date.now() + 15_000;
    while (lossy.page.busy !== null && Date.now() < deadline) await new Promise((resolve) => setTimeout(resolve, 10));
    assert.equal(lossy.page.view.state, "ready");
    assert.equal(calls.filter((m) => m.type === "policy_start").length, 1);
    const bytes = readFileSync(path.join(app, "policy.json"));
    assert.equal((await call({ type: "visit", id: "early", url: "https://synthetic.test/early", title: "early", incognito: false,
      recordingOccurredAt: early, visitedAt: new Date().toISOString() })).dropped, "policy");
    assert.equal((await call({ type: "visit", id: "later", url: "https://synthetic.test/later", title: "later", incognito: false,
      recordingOccurredAt: new Date().toISOString(), visitedAt: new Date().toISOString() })).ok, true);
    const pending = startMcp(mcpBin, [], { env });
    servers.push(pending);
    await assert.rejects(pending.initialize("onboarding-test"), (error) => error.kind === "exited" && error.stderr.includes("vault_publication_pending"));
    await confirmPublication(call);
    const published = startMcp(mcpBin, [], { env });
    servers.push(published);
    await published.initialize("onboarding-test");
    const reply = await published.request("tools/call", { name: "memocampus_search_visits", arguments: { query: "" } });
    assert.ok(JSON.stringify(reply).includes("synthetic.test/later"));
    assert.ok(!JSON.stringify(reply).includes("synthetic.test/early"));
    const existing = await openPage({ send: call });
    assert.equal(existing.$("onboarding").hidden, true);
    assert.deepEqual(readFileSync(path.join(app, "policy.json")), bytes);
  } finally {
    for (const server of servers) await server.close();
    rmSync(app, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  }
});
