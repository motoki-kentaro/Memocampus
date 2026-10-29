import { test } from "node:test";
import assert from "node:assert/strict";
import { SearchError, chromeNativeTransport, createLatestGate, searchPages } from "../../extension/search-client.js";

const item = (url, extra = {}) => ({
  url,
  title: "T",
  lastVisitedAt: "2026-10-30T00:00:00.000Z",
  lastCapturedAt: null,
  lastActivityAt: "2026-10-30T00:00:00.000Z",
  source: "visit",
  ...extra,
});

const reply = (message, fields) => ({ ok: true, type: "search_pages_result", requestId: message.requestId, items: [], hasMore: false, ...fields });

test("sends a search_pages message and returns normalized items", async () => {
  let sent;
  const page = await searchPages(
    { query: "rust", offset: 50, limit: 25 },
    { createId: () => "req-1", send: async (message) => ((sent = message), reply(message, { items: [item("https://a.test/")], hasMore: true })) },
  );
  assert.deepEqual(sent, { type: "search_pages", requestId: "req-1", query: "rust", limit: 25, offset: 50 });
  assert.equal(page.hasMore, true);
  assert.equal(page.received, 1);
  assert.equal(page.items[0].url, "https://a.test/");
  assert.equal(page.items[0].lastCapturedAt, null);
});

test("drops items that are not web URLs but still counts them for the offset", async () => {
  const page = await searchPages({ query: "" }, {
    send: async (message) => reply(message, { items: [item("javascript:alert(1)"), item("https://ok.test/"), { url: "https://x.test/" }] }),
  });
  assert.deepEqual(page.items.map((hit) => hit.url), ["https://ok.test/"]);
  assert.equal(page.received, 3);
});

test("rejects a response that belongs to another request", async () => {
  await assert.rejects(
    searchPages({ query: "" }, { createId: () => "mine", send: async () => ({ ok: true, type: "search_pages_result", requestId: "other", items: [] }) }),
    (error) => error instanceof SearchError && error.code === "invalid_response",
  );
  await assert.rejects(searchPages({ query: "" }, { send: async () => undefined }), { code: "invalid_response" });
});

test("maps host error codes to SearchError", async () => {
  await assert.rejects(
    searchPages({ query: "" }, { send: async (message) => ({ ok: false, type: "search_pages_result", requestId: message.requestId, error: "db_error" }) }),
    (error) => error.code === "db_error" && error.message === "データベースの操作に失敗しました。",
  );
});

test("times out when the host never answers (e.g. an old host that ignores search_pages)", async () => {
  await assert.rejects(searchPages({ query: "" }, { timeoutMs: 20, send: () => new Promise(() => {}) }), { code: "timeout" });
});

test("a throwing transport surfaces as a rejection", async () => {
  await assert.rejects(searchPages({ query: "" }, { send: () => { throw new SearchError("host_unavailable"); } }), { code: "host_unavailable" });
});

test("chromeNativeTransport reports lastError as host_unavailable", async () => {
  const runtime = {
    lastError: undefined,
    sendNativeMessage(host, _message, callback) {
      assert.equal(host, "com.memocampus.vault");
      this.lastError = { message: "Native host has exited." };
      callback(undefined);
    },
  };
  await assert.rejects(chromeNativeTransport(runtime)({}), (error) => error.code === "host_unavailable" && error.detail === "Native host has exited.");
});

test("latest gate discards responses that arrive out of order", async () => {
  const gate = createLatestGate();
  const shown = [];
  const deferred = () => { let resolve; const promise = new Promise((r) => (resolve = r)); return { promise, resolve }; };
  const slow = deferred();
  const fast = deferred();
  const run = async (query, pending) => {
    const token = gate.next();
    const result = await pending.promise;
    if (gate.isCurrent(token)) shown.push(`${query}:${result}`);
  };
  const first = run("r", slow);
  const second = run("ru", fast);
  fast.resolve("new");
  slow.resolve("old");
  await Promise.all([first, second]);
  assert.deepEqual(shown, ["ru:new"]);
});
