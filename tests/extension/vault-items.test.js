// Issue #43, Slice 1: policy changes for one URL and deletions, as MCP sees them next (opt-in).
//
// MEMOCAMPUS_TEST_HOST=<built memocampus-native-host> and MEMOCAMPUS_TEST_MCP=<built memocampus-mcp>
// run the real host and the real MCP server in a temporary MEMOCAMPUS_APP_DIR with a fresh vault and
// policy. One MCP process serves the whole test: every answer must follow the change made just before
// it, with no restart. Nothing touches the user's vault, policy or Chrome profile.

import test from "node:test";
import assert from "node:assert/strict";
import path from "node:path";

const HOST_BIN = process.env.MEMOCAMPUS_TEST_HOST;
const MCP_BIN = process.env.MEMOCAMPUS_TEST_MCP;

const A = "https://forum.test/thread/1?page=2";
const A_OTHER_QUERY = "https://forum.test/thread/1?page=3";
const B = "https://blog.test/post";
const PAGE = "https://gallery.test/album";
const IMAGE = "https://img.test/cat.png";
const T = "2026-11-07T10:00:00.000Z";

test("end to end with the real host and MCP: a URL hidden, published, deleted; an image hidden and deleted", { skip: HOST_BIN && MCP_BIN ? false : "set MEMOCAMPUS_TEST_HOST and MEMOCAMPUS_TEST_MCP to run" }, async () => {
  const { spawn } = await import("node:child_process");
  const { mkdtempSync, rmSync } = await import("node:fs");
  const os = await import("node:os");
  const { DatabaseSync } = await import("node:sqlite");
  const appDir = mkdtempSync(path.join(os.tmpdir(), "memocampus-items-e2e-"));
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
  const vault = path.join(appDir, "memocampus.sqlite3");
  let mcp;
  try {
    assert.equal((await call({ type: "policy_initialize", requestId: "p" })).ok, true);
    assert.equal((await call({ type: "vault_create", requestId: "c", useDefault: true })).ok, true);
    const db = new DatabaseSync(vault);
    try {
      db.exec("PRAGMA foreign_keys = ON;");
      const visit = db.prepare("INSERT INTO visits VALUES (?, ?, ?, ?)");
      for (const [id, url] of [["a1", A], ["a2", A], ["q1", A_OTHER_QUERY], ["b1", B]]) visit.run(id, url, `title ${id}`, T);
      db.exec(`INSERT INTO image_blobs VALUES (1, '${"a".repeat(64)}', 'image/png', 3, 40, 30, 1762509600000, 1762509600000, 1, x'010203')`);
      db.prepare("INSERT INTO saved_images (save_id, blob_id, image_url, page_url, saved_at, incognito, mcp_public) VALUES ('s1', 1, ?, ?, 1762509600000, 0, 1)").run(IMAGE, PAGE);
    } finally {
      db.close();
    }

    mcp = spawn(MCP_BIN, ["--vault", vault], { env, stdio: ["pipe", "pipe", "pipe"] });
    let buffer = "";
    const waiting = new Map();
    mcp.stdout.on("data", (chunk) => {
      buffer += chunk;
      let index;
      while ((index = buffer.indexOf("\n")) >= 0) {
        const line = buffer.slice(0, index);
        buffer = buffer.slice(index + 1);
        if (line.trim()) {
          const message = JSON.parse(line);
          waiting.get(message.id)?.(message);
        }
      }
    });
    let next = 0;
    const rpc = (method, params) =>
      new Promise((resolve) => {
        const id = ++next;
        waiting.set(id, resolve);
        mcp.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`);
      });
    await rpc("initialize", { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "items-e2e", version: "0" } });
    mcp.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" })}\n`);
    const visible = async () => {
      const answer = JSON.stringify(await rpc("tools/call", { name: "memocampus_search_visits", arguments: { query: "" } }));
      return [A, A_OTHER_QUERY, B].filter((url) => answer.includes(JSON.stringify(url).slice(1, -1)));
    };
    const images = async () => JSON.stringify(await rpc("tools/call", { name: "memocampus_search_images", arguments: { query: "" } })).includes("img.test");
    const change = async (url, action, scope) => {
      const preview = await call({ type: "policy_item_preview", requestId: "p", url, action, scope });
      assert.equal(preview.executable, true, JSON.stringify(preview));
      const done = await call({ type: "policy_item_apply", requestId: "a", url, action, scope, expectedEtag: preview.etag });
      assert.equal(done.ok, true, JSON.stringify(done));
      return done;
    };

    assert.deepEqual(await visible(), [A, A_OTHER_QUERY, B]);
    assert.equal(await images(), true);

    // Save but no MCP, URL exact: hidden from MCP at once; the vault keeps it, other queries stay.
    assert.equal((await change(A, "save_but_no_mcp")).decision, "save_but_no_mcp");
    assert.deepEqual(await visible(), [A_OTHER_QUERY, B]);
    const kept = new DatabaseSync(vault, { readOnly: true });
    assert.equal(kept.prepare("SELECT count(*) AS n FROM visits WHERE url = ?").get(A).n, 2, "a rule deletes nothing");
    kept.close();
    // Published again: back in MCP.
    assert.equal((await change(A, "publish")).decision, "allow");
    assert.deepEqual(await visible(), [A, A_OTHER_QUERY, B]);
    // A host rule, explicitly: every URL of the host, and publishing one of them is refused.
    await change(A, "never_save", "host");
    assert.deepEqual(await visible(), [B]);
    const blocked = await call({ type: "policy_item_preview", requestId: "p", url: A, action: "publish" });
    assert.equal(blocked.reason, "publish_blocked");
    assert.deepEqual(await visible(), [B]);

    // An image is judged by its page too: hiding the page hides the image.
    await change(PAGE, "save_but_no_mcp");
    assert.equal(await images(), false);
    await change(PAGE, "publish");
    assert.equal(await images(), true);

    // Deletions: gone from MCP too.
    const preview = await call({ type: "url_delete_preview", requestId: "d", url: B });
    assert.equal(preview.counts.visits, 1);
    assert.equal((await call({ type: "url_delete", requestId: "d", url: B, vaultToken: preview.vaultToken })).ok, true);
    assert.deepEqual(await visible(), []);
    const deleted = await call({ type: "image_delete", requestId: "i", blobId: 1, sha256: "a".repeat(64), vaultToken: preview.vaultToken });
    assert.equal(deleted.ok, true, JSON.stringify(deleted));
    assert.equal(await images(), false);

    const check = new DatabaseSync(vault, { readOnly: true });
    try {
      assert.equal(check.prepare("PRAGMA integrity_check").get().integrity_check, "ok");
      assert.deepEqual(check.prepare("PRAGMA foreign_key_check").all(), []);
      assert.equal(check.prepare("SELECT count(*) AS n FROM image_blobs").get().n, 0);
      assert.deepEqual(check.prepare("SELECT url FROM visits ORDER BY url").all().map((row) => row.url), [A, A, A_OTHER_QUERY]);
    } finally {
      check.close();
    }
  } finally {
    if (mcp) {
      const exited = new Promise((resolve) => mcp.once("exit", resolve));
      mcp.kill();
      await exited;
    }
    rmSync(appDir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  }
});
