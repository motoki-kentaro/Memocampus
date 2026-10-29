import { test } from "node:test";
import assert from "node:assert/strict";
import { SearchError, VAULT_UNAVAILABLE_CODES, callHost, describeErrorCode } from "../../extension/search-client.js";
import { DIALOG_TIMEOUT_MS, describeReasons, describeStatus, formatCounts, vaultRequest } from "../../extension/vault-client.js";

const result = (message, fields) => ({ ok: true, type: "vault_result", requestId: message.requestId, ...fields });

test("vault requests carry their parameters and a fresh requestId", async () => {
  let sent;
  const response = await vaultRequest("vault_create", { useDefault: true }, {
    createId: () => "id-1",
    send: async (message) => ((sent = message), result(message, { path: "C:\v.sqlite3" })),
  });
  assert.deepEqual(sent, { type: "vault_create", useDefault: true, requestId: "id-1" });
  assert.equal(response.path, "C:\v.sqlite3");
});

test("failures keep the host response so the UI can ask for approval", async () => {
  const location = { verdict: "needs_approval", reasons: ["sync_folder"] };
  await assert.rejects(
    vaultRequest("vault_open", { path: "C:\OneDrive\v.sqlite3" }, {
      send: async (message) => ({ ok: false, type: "vault_result", requestId: message.requestId, error: "location_needs_approval", path: "C:\OneDrive\v.sqlite3", location }),
    }),
    (error) => error instanceof SearchError && error.code === "location_needs_approval" && error.response.location === location,
  );
});

test("a search_pages_result is not accepted as a vault_result", async () => {
  await assert.rejects(
    vaultRequest("vault_status", {}, { send: async (message) => ({ ok: true, type: "search_pages_result", requestId: message.requestId }) }),
    { code: "invalid_response" },
  );
});

test("interactive vault operations wait long enough for a file dialog; status does not", async () => {
  const timeouts = [];
  const realSetTimeout = globalThis.setTimeout;
  // Record the requested timeout but never fire it.
  globalThis.setTimeout = (_fn, ms) => (timeouts.push(ms), realSetTimeout(() => {}, 0));
  try {
    const send = async (message) => result(message, {});
    await vaultRequest("vault_backup", {}, { send });
    await vaultRequest("vault_status", {}, { send });
  } finally {
    globalThis.setTimeout = realSetTimeout;
  }
  assert.deepEqual(timeouts, [DIALOG_TIMEOUT_MS, 10_000]);
});

test("an old host that ignores vault messages surfaces as a timeout", async () => {
  await assert.rejects(callHost({ type: "vault_status" }, { resultType: "vault_result", timeoutMs: 20, send: () => new Promise(() => {}) }), { code: "timeout" });
});

test("every vault-unavailable code has a specific Japanese message", () => {
  for (const code of VAULT_UNAVAILABLE_CODES) {
    assert.notEqual(describeErrorCode(code), describeErrorCode("no_such_code"), code);
  }
  assert.match(describeErrorCode("file_exists"), /上書きはしません/);
  assert.equal(describeErrorCode("no_such_code"), "操作に失敗しました。");
});

test("status, reasons and counts render as Japanese text", () => {
  assert.equal(describeStatus({ state: "ready" }), "接続中");
  assert.equal(describeStatus({ state: "not_configured" }), "Vaultが未設定です");
  assert.equal(describeStatus({ state: "missing", overridden: true }), "開発用DB(MEMOCAMPUS_DB)を使用中（Vaultファイルが見つかりません）");
  assert.deepEqual(describeReasons(["sync_folder", "future_reason"]), ["同期フォルダ(OneDrive等)の中です", "future_reason"]);
  assert.deepEqual(describeReasons(undefined), []);
  assert.equal(formatCounts({ visits: 1234, snapshots: 5 }), "閲覧 1,234件・スナップショット 5件");
  assert.equal(formatCounts(null), "");
});
