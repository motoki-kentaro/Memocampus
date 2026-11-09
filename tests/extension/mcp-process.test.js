// Issue #51, F-01: an MCP server that cannot start, stops or never answers ends a test with a
// clear reason instead of hanging it. The first tests use small Node stand-ins and always run; the
// last runs the real host and MCP server (opt-in, as the other end-to-end tests) to show that an
// unpublished vault is refused at once and a confirmed one is served.

import test from "node:test";
import assert from "node:assert/strict";
import path from "node:path";
import { ProcessFailure, confirmPublication, startMcp } from "./real-processes.js";

const NODE = process.execPath;
const stub = (source) => [NODE, ["-e", source]];

async function failure(promise) {
  try {
    await promise;
  } catch (error) {
    assert.ok(error instanceof ProcessFailure, String(error));
    return error;
  }
  assert.fail("expected a failure");
}

test("a server that exits at start rejects every request with its exit code and stderr", async () => {
  const [bin, args] = stub("process.stderr.write('memocampus-mcp: vault refused: vault_publication_pending\\n'); process.exit(2)");
  const mcp = startMcp(bin, args, { timeoutMs: 10_000 });
  const started = Date.now();
  const error = await failure(mcp.initialize("exit-test"));
  assert.equal(error.kind, "exited");
  assert.equal(error.code, 2);
  assert.match(error.stderr, /vault_publication_pending/);
  assert.match(error.message, /exited \(code 2/);
  assert.ok(Date.now() - started < 5_000, "reported when the process ends, not at the time limit");
  // Later requests fail at once with the same reason.
  assert.equal((await failure(mcp.rpc("tools/list", {}))).kind, "exited");
  await mcp.close();
  await mcp.close();
});

test("a server that crashes while a request is waiting rejects that request", async () => {
  const [bin, args] = stub("process.stdin.once('data', () => process.exit(7))");
  const mcp = startMcp(bin, args, { timeoutMs: 10_000 });
  const error = await failure(mcp.rpc("tools/list", {}));
  assert.equal(error.kind, "exited");
  assert.equal(error.code, 7);
  await mcp.close();
});

test("a server that never answers is reported as a timeout and still stopped", async () => {
  const [bin, args] = stub("process.stdin.resume(); setInterval(() => {}, 1000)");
  const mcp = startMcp(bin, args, { timeoutMs: 200 });
  const error = await failure(mcp.rpc("initialize", {}));
  assert.equal(error.kind, "timeout");
  assert.match(error.message, /did not answer initialize within 200 ms/);
  await mcp.close();
  assert.notEqual(mcp.child.exitCode ?? mcp.child.signalCode, null, "the process is gone");
});

test("a JSON-RPC error answer is distinguished from a dead or silent server", async () => {
  const [bin, args] = stub(
    "let b='';process.stdin.on('data',c=>{b+=c;let i;while((i=b.indexOf('\\n'))>=0){const m=JSON.parse(b.slice(0,i));b=b.slice(i+1);" +
      "process.stdout.write(JSON.stringify({jsonrpc:'2.0',id:m.id,error:{code:-32601,message:'Method not found'}})+'\\n')}})",
  );
  const mcp = startMcp(bin, args, { timeoutMs: 10_000 });
  // `rpc` hands the raw answer to tests that inspect errors themselves.
  assert.equal((await mcp.rpc("nope", {})).error.code, -32601);
  const error = await failure(mcp.request("nope", {}));
  assert.equal(error.kind, "response_error");
  assert.equal(error.error.code, -32601);
  await mcp.close();
});

test("a server that cannot be started is reported as such", async () => {
  const mcp = startMcp(path.join(process.cwd(), "no-such-memocampus-mcp.exe"), [], { timeoutMs: 10_000 });
  const error = await failure(mcp.initialize("spawn-test"));
  assert.equal(error.kind, "spawn_failed");
  await mcp.close();
});

const HOST_BIN = process.env.MEMOCAMPUS_TEST_HOST;
const MCP_BIN = process.env.MEMOCAMPUS_TEST_MCP;

test("end to end with the real host and MCP: a new vault is refused until confirmed, and again after revocation", { skip: HOST_BIN && MCP_BIN ? false : "set MEMOCAMPUS_TEST_HOST and MEMOCAMPUS_TEST_MCP to run" }, async () => {
  const { spawn } = await import("node:child_process");
  const { mkdtempSync, rmSync } = await import("node:fs");
  const os = await import("node:os");
  const appDir = mkdtempSync(path.join(os.tmpdir(), "memocampus-mcp-gate-e2e-"));
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
  const servers = [];
  const start = (args) => {
    const mcp = startMcp(MCP_BIN, args, { env, timeoutMs: 15_000 });
    servers.push(mcp);
    return mcp;
  };
  try {
    assert.equal((await call({ type: "policy_initialize", requestId: "p" })).ok, true);
    const created = await call({ type: "vault_create", requestId: "c", useDefault: true });
    assert.equal(created.status.publication, "pending");

    // Pending: the server refuses to start, with a fixed path and following the active vault.
    for (const args of [["--vault", vault], []]) {
      const error = await failure(start(args).initialize("gate-e2e"));
      assert.equal(error.kind, "exited", String(error));
      assert.equal(error.code, 2);
      assert.match(error.stderr, /vault refused: vault_publication_pending/);
    }

    // Confirmed: served.
    const token = await confirmPublication(call);
    const mcp = start([]);
    await mcp.initialize("gate-e2e");
    const listed = await mcp.request("tools/call", { name: "memocampus_search_visits", arguments: { query: "" } });
    assert.notEqual(listed.isError, true, JSON.stringify(listed));

    // Revoked in the side panel: the running server refuses the next call.
    const revoked = await call({ type: "vault_publication_revoke", requestId: "r", expectedVaultToken: token });
    assert.equal(revoked.status.publication, "pending");
    const refused = await mcp.rpc("tools/call", { name: "memocampus_search_visits", arguments: { query: "" } });
    assert.match(JSON.stringify(refused), /vault_publication_pending/);
  } finally {
    for (const mcp of servers) await mcp.close();
    rmSync(appDir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  }
});

test("end to end with the real host and MCP: an encrypted vault written by the host is read by MCP only once confirmed", { skip: HOST_BIN && MCP_BIN ? false : "set MEMOCAMPUS_TEST_HOST and MEMOCAMPUS_TEST_MCP to run" }, async () => {
  const { spawn } = await import("node:child_process");
  const { mkdtempSync, readFileSync, readdirSync, rmSync, statSync } = await import("node:fs");
  const os = await import("node:os");
  const appDir = mkdtempSync(path.join(os.tmpdir(), "memocampus-mcp-enc-e2e-"));
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
      const body = Buffer.from(JSON.stringify({ requestId: "r", ...message }), "utf8");
      const length = Buffer.alloc(4);
      length.writeUInt32LE(body.length);
      child.stdin.end(Buffer.concat([length, body]));
    });
  const vault = path.join(appDir, "encrypted.sqlite3");
  const servers = [];
  try {
    assert.equal((await call({ type: "policy_initialize" })).ok, true);
    const begun = await call({ type: "vault_create_encrypted", path: vault });
    assert.equal(begun.ok, true, JSON.stringify(begun));
    assert.equal(begun.recoveryPhrase.length, 12);
    const phrase = begun.recoveryPhrase.join(" ");
    const created = await call({ type: "vault_create_encrypted_commit", recoveryPhrase: phrase });
    assert.equal(created.ok, true, JSON.stringify(created));
    assert.equal(created.status.mode, "encrypted");
    assert.equal(created.status.publication, "pending");
    for (const [id, url, title] of [
      ["v1", "https://public.test/encrypted-row", "Public encrypted row"],
      ["v2", "https://accounts.google.com/CANARY", "CANARY private title"],
    ]) {
      const ack = await call({ type: "visit", id, url, title, incognito: false, visitedAt: "2026-11-09T10:00:00.000Z" });
      assert.equal(ack.ok, true, JSON.stringify(ack));
    }
    // At rest: neither the rows nor the phrase are readable in any file of the app directory.
    for (const name of readdirSync(appDir, { recursive: true })) {
      const file = path.join(appDir, String(name));
      if (!statSync(file).isFile()) continue;
      const bytes = readFileSync(file);
      assert.equal(bytes.includes("encrypted-row"), false, file);
      assert.equal(bytes.includes(phrase), false, file);
    }

    const pending = await failure(startMcpServer(servers, env, []).initialize("enc-e2e"));
    assert.match(pending.stderr, /vault refused: vault_publication_pending/);
    await confirmPublication(call);
    const mcp = startMcpServer(servers, env, []);
    await mcp.initialize("enc-e2e");
    const found = await mcp.request("tools/call", { name: "memocampus_search_visits", arguments: { query: "" } });
    const text = JSON.stringify(found);
    assert.match(text, /encrypted-row/);
    assert.doesNotMatch(text, /CANARY/, "the privacy gate applies to encrypted vaults too");
    assert.equal(text.includes(phrase), false);
  } finally {
    for (const server of servers) await server.close();
    rmSync(appDir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  }
});

function startMcpServer(servers, env, args) {
  const mcp = startMcp(MCP_BIN, args, { env, timeoutMs: 15_000 });
  servers.push(mcp);
  return mcp;
}
