// Shared by the opt-in end-to-end tests that run the real host and MCP server (Issue #51, F-01).
//
// Nothing here waits forever: every MCP request ends in its answer or in a `ProcessFailure` whose
// `kind` says why —
//   "spawn_failed"   the process could not be started,
//   "exited"         it ended (or closed its output) before answering; `code`, `signal`, `stderr`,
//   "timeout"        no answer within the limit; the process is still running,
//   "response_error" `request` only: the server answered with a JSON-RPC error (`error`),
//   "protocol_error" it wrote a line that is not JSON.
// Only test code imports this file; it is not a test itself.

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import os from "node:os";
import defaults from "../../extension/policy-defaults.json" with { type: "json" };

/** Existing-user regression fixture, deliberately using the pre-onboarding marker and dates.
 * Only isolated temp app directories; fresh onboarding tests use the real policy_start instead. */
export function seedLegacyPolicy(appDir) {
  const relative = path.relative(os.tmpdir(), appDir);
  assert.ok(relative && !relative.startsWith("..") && !path.isAbsolute(relative));
  mkdirSync(appDir, { recursive: true });
  for (const [name, value] of [["policy.json", JSON.stringify(defaults)], ["policy.initialized", "Memocampus policy initialization v1\n"], ["policy.lock", ""]]) {
    writeFileSync(path.join(appDir, name), value, { flag: "wx" });
  }
}

export class ProcessFailure extends Error {
  constructor(kind, message, details = {}) {
    super(message);
    this.name = "ProcessFailure";
    this.kind = kind;
    Object.assign(this, details);
  }
}

const MAX_STDERR = 64 * 1024;

/**
 * Starts an MCP server speaking newline-delimited JSON-RPC on stdio.
 * `rpc` resolves with the raw message (result or error), as the tests always read it;
 * `request` also rejects a JSON-RPC error as `response_error`.
 */
export function startMcp(bin, args, { env, timeoutMs = 15_000 } = {}) {
  const child = spawn(bin, args, { env, stdio: ["pipe", "pipe", "pipe"] });
  const waiting = new Map();
  let next = 0;
  let buffer = "";
  let stderr = "";
  let ended = null;
  let closed = false;
  const whenClosed = new Promise((resolve) => child.once("close", resolve));

  const failAll = (failure) => {
    ended ??= failure;
    for (const entry of waiting.values()) {
      clearTimeout(entry.timer);
      entry.reject(failure);
    }
    waiting.clear();
  };

  child.stdin.on("error", () => {}); // EPIPE after an early exit; reported by "close" below.
  child.stderr.setEncoding("utf8");
  child.stderr.on("data", (chunk) => {
    if (stderr.length < MAX_STDERR) stderr += chunk;
  });
  child.stdout.setEncoding("utf8");
  child.stdout.on("data", (chunk) => {
    buffer += chunk;
    let index;
    while ((index = buffer.indexOf("\n")) >= 0) {
      const line = buffer.slice(0, index);
      buffer = buffer.slice(index + 1);
      if (!line.trim()) continue;
      let message;
      try {
        message = JSON.parse(line);
      } catch {
        failAll(new ProcessFailure("protocol_error", `MCP wrote a line that is not JSON: ${line.slice(0, 200)}`));
        continue;
      }
      const entry = waiting.get(message.id);
      if (entry) {
        waiting.delete(message.id);
        clearTimeout(entry.timer);
        entry.resolve(message);
      }
    }
  });
  child.once("error", (error) => {
    failAll(new ProcessFailure("spawn_failed", `MCP could not be started (${bin}): ${error.message}`, { cause: error }));
  });
  // After stdout and stderr are closed, so stderr is complete.
  child.once("close", (code, signal) => {
    closed = true;
    const details = { code, signal, stderr: stderr.trim() };
    failAll(new ProcessFailure("exited", `MCP exited (code ${code}, signal ${signal}) before answering${details.stderr ? `: ${details.stderr}` : ""}`, details));
  });

  const rpc = (method, params) => {
    if (ended) return Promise.reject(ended);
    const id = ++next;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        waiting.delete(id);
        reject(new ProcessFailure("timeout", `MCP did not answer ${method} within ${timeoutMs} ms${stderr ? `; stderr: ${stderr.trim()}` : ""}`, { method }));
      }, timeoutMs);
      waiting.set(id, { resolve, reject, timer });
      child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`);
    });
  };

  const request = async (method, params) => {
    const message = await rpc(method, params);
    if (message.error) {
      throw new ProcessFailure("response_error", `MCP answered ${method} with error ${message.error.code}: ${message.error.message}`, { error: message.error });
    }
    return message.result;
  };

  const notify = (method, params) => {
    if (!ended) child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", method, ...(params ? { params } : {}) })}\n`);
  };

  /** The 2025-06-18 handshake every test starts with. */
  const initialize = async (name) => {
    const result = await request("initialize", { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name, version: "0" } });
    notify("notifications/initialized");
    return result;
  };

  /** Stops the server (if it still runs) and waits until it is gone. Safe to call twice. */
  const close = async () => {
    if (!closed && child.exitCode === null && child.signalCode === null) child.kill();
    if (child.pid !== undefined) await whenClosed;
  };

  return { child, rpc, request, notify, initialize, close, stderr: () => stderr };
}

/**
 * Confirms MCP publication of the active vault through the real host, as the side panel does:
 * the token is the one `vault_status` shows. `call` sends one Native Messaging message.
 */
export async function confirmPublication(call) {
  const shown = await call({ type: "vault_status", requestId: "s" });
  const token = shown.status?.vaultToken;
  assert.equal(typeof token, "string", JSON.stringify(shown));
  assert.equal(shown.status.publication, "pending", "a vault is published only by an explicit confirmation");
  const confirmed = await call({ type: "vault_publication_confirm", requestId: "c", expectedVaultToken: token });
  assert.equal(confirmed.ok, true, JSON.stringify(confirmed));
  assert.equal(confirmed.status.publication, "confirmed");
  return token;
}
