import { test } from "node:test";
import assert from "node:assert/strict";
import { createDeleteClient } from "../../extension/delete-client.js";
import { createDeletePort, deleteState, visitRow } from "./delete-fixtures.js";
import { settle } from "./fake-dom.js";

const host = (visits = ["a1", "a2", "a3"]) => deleteState({
  active: "A", vaults: { A: [] }, images: { A: [] }, tags: { A: {} }, deletedUrls: [], deletedImages: [],
  normalizeTag: (t) => t.trim().toLowerCase(),
  visits: { A: visits.map((id) => visitRow(id, "https://a.test/", `title ${id}`, `2026-10-1${id.slice(1)}T00:00:00.000Z`)), B: [] }, snapshots: { A: [], B: [] },
});
const client = (h, options = {}) => createDeleteClient({ connect: () => createDeletePort(h), runtime: {}, timeoutMs: 40, ...options });
const reqs = (h, type) => h.deleteRequests.filter((r) => r.type === type);

test("every request is tagged and carries the Vault token and the selection generation; one port serves them all", async () => {
  const h = host();
  const c = client(h);
  const page = await c.history("https://a.test/", {});
  assert.equal(page.items.length, 3);
  assert.equal(h.deletePorts.length, 1);
  assert.equal(h.deleteRequests[0].type, "item_state");
  assert.ok(h.deleteRequests.every((r) => typeof r.requestId === "string"));
  assert.equal(h.deleteRequests[1].generation, "generation-1");
  assert.match(h.deleteRequests[1].vaultToken, /^[0-9a-f]{64}$/);
  c.close();
});

test("a delete is sent once; a reply that cannot be used is proven by the status of the same port", async () => {
  const h = host();
  const c = client(h);
  const preview = await c.preview({ scope: "visit", id: "a1" });
  h.garbleDeleteReply = true;
  const done = await c.delete(preview.reviewId);
  assert.equal(done.confirmed, true);
  assert.equal(reqs(h, "item_delete").length, 1);
  assert.equal(reqs(h, "item_delete_status").length, 1);
  c.close();
});

test("a dropped answer is proven by the status of the same port; an unanswered status is unknown; nothing is resent", async () => {
  const h = host();
  const c = client(h);
  const preview = await c.preview({ scope: "visit", id: "a1" });
  h.dropDeleteReply = true;
  const result = await c.delete(preview.reviewId); // dropped reply, status on the live port proves the COMMIT
  assert.equal(result.confirmed, true);
  assert.equal(reqs(h, "item_delete").length, 1);
  // The status itself unanswered: unknown.
  const second = await c.preview({ scope: "visit", id: "a2" });
  h.dropDeleteReply = true;
  h.deleteDelayFn = { item_delete_status: () => new Promise(() => {}) };
  await assert.rejects(c.delete(second.reviewId), { code: "item_delete_unknown" });
  assert.equal(reqs(h, "item_delete").length, 2);
  c.close();
});

test("losing the port with the answer is unknown: no reconnect, no resend, no success", async () => {
  const h = host();
  const c = client(h);
  const preview = await c.preview({ scope: "visit", id: "a1" });
  h.deleteDelayFn = { item_delete: async () => { h.deletePorts.at(-1).disconnect(); } };
  await assert.rejects(c.delete(preview.reviewId), { code: "item_delete_unknown" });
  assert.equal(h.deletePorts.length, 1, "never reconnects by itself");
  assert.equal(reqs(h, "item_delete").length, 1);
  assert.equal(c.binding, null);
});

test("a new port cannot confirm an old preview (a restarted Host)", async () => {
  const h = host();
  const first = client(h);
  const preview = await first.preview({ scope: "visit", id: "a1" });
  first.close();
  const second = client(h);
  await assert.rejects(second.delete(preview.reviewId), { code: "invalid_review" });
  assert.equal(h.visits.A.length, 3);
  second.close();
});

test("A to B to A while the client waits invalidates its binding and the old review", async () => {
  const h = host();
  const seen = [];
  const c = client(h, { onInvalidated: (code) => seen.push(code) });
  const preview = await c.preview({ scope: "visit", id: "a1" });
  h.generations.A = "generation-2";
  // The Host itself refuses a changed selection before touching anything: a certain "not deleted".
  await assert.rejects(c.delete(preview.reviewId), (error) => error.code === "vault_switched" && error.fromHost === true);
  assert.ok(seen.includes("vault_switched"));
  assert.equal(h.visits.A.length, 3, "nothing deleted");
  assert.equal(c.binding, null);
});

test("a late vault_switched of a closed connection does not close the one made after it", async () => {
  const h = host();
  const c = client(h);
  await c.state();
  h.deleteDelays.visit_history = 30;
  h.generations.A = "generation-2";
  const late = c.history("https://a.test/", {});
  await settle(5);
  c.close(); // the UI moved on (Vault changed)
  await assert.rejects(late, { code: "vault_switched" });
  const next = await c.history("https://a.test/", {});
  assert.equal(next.items.length, 3);
  assert.equal(h.deletePorts.length, 2);
  c.close();
});

test("errors keep their fixed codes and raw text never becomes one", async () => {
  const h = host();
  const c = client(h);
  h.deleteFailure = "https://a.test/SECRET";
  await assert.rejects(c.state(), { code: "invalid_response" });
  c.close();
  const d = client(h);
  await assert.rejects(d.preview({ scope: "visit", id: "missing" }), { code: "item_not_found" });
  d.close();
});

test("visit history pages by keyset and validates what it receives", async () => {
  const h = host(["a1", "a2", "a3", "a4", "a5"]);
  const c = client(h);
  const first = await c.history("https://a.test/", {}, null);
  assert.equal(first.counts.total, 5);
  const all = [];
  let after = null;
  for (let i = 0; i < 4; i += 1) {
    const page = await c.history("https://a.test/", {}, after);
    all.push(...page.items.map((x) => x.id));
    after = page.next;
    if (!after) break;
  }
  assert.deepEqual(all, ["a5", "a4", "a3", "a2", "a1"]);
  c.close();
});

// --- FINDING-DELETE-01: what a delete that left the client may be called -------------------------------------

test("COMMIT, the answer discarded, the port gone: unknown, one DELETE, one COMMIT, and the UI is told so (not 'nothing deleted')", async () => {
  const h = host();
  const invalidated = [];
  const c = client(h, { onInvalidated: (code) => invalidated.push(code) });
  const preview = await c.preview({ scope: "visit", id: "a1" });
  h.commitThenDisconnect = true;
  await assert.rejects(c.delete(preview.reviewId), { code: "item_delete_unknown" });
  assert.equal(h.deleteCommits, 1);
  assert.equal(reqs(h, "item_delete").length, 1);
  assert.equal(reqs(h, "item_delete_status").length, 0, "no port is left to ask");
  assert.equal(h.deletePorts.length, 1, "never reconnects, never resends");
  assert.deepEqual(invalidated, ["item_delete_unknown"]);
});

test("a delete that never left is 'not sent'; a refusal of the Host is a certain 'not deleted'; neither is unknown", async () => {
  const h = host();
  let up = true;
  const c = createDeleteClient({ connect: () => { if (!up) throw new Error("no host"); return createDeletePort(h); }, runtime: {}, timeoutMs: 40 });
  const preview = await c.preview({ scope: "visit", id: "a1" });
  c.close();
  up = false;
  await assert.rejects(c.delete(preview.reviewId), { code: "delete_not_sent" });
  assert.equal(reqs(h, "item_delete").length, 0);
  up = true;
  // The Host refuses (the set changed): it rolled back.
  const again = await c.preview({ scope: "visit", id: "a2" });
  h.visits.A = h.visits.A.filter((v) => v.id !== "a2");
  h.visits.A.push({ id: "a2", url: "https://elsewhere.test/", title: "changed", visitedAt: "2026-10-12T00:00:00.000Z" });
  await assert.rejects(c.delete(again.reviewId), (error) => error.code === "item_targets_changed" && error.fromHost === true);
  assert.equal(h.deleteCommits ?? 0, 0);
  c.close();
});

test("the port ending before the COMMIT is unknown too (the request had been sent): never 'nothing deleted'", async () => {
  const h = host();
  const invalidated = [];
  const c = client(h, { onInvalidated: (code) => invalidated.push(code) });
  const preview = await c.preview({ scope: "visit", id: "a1" });
  h.deleteDelayFn = { item_delete: async () => { h.deletePorts.at(-1).disconnect(); } };
  await assert.rejects(c.delete(preview.reviewId), { code: "item_delete_unknown" });
  assert.equal(h.deleteCommits ?? 0, 0);
  assert.deepEqual(invalidated, ["item_delete_unknown"]);
});

test("a plain disconnect with no delete in flight is still reported as the connection", async () => {
  const h = host();
  const invalidated = [];
  const c = client(h, { onInvalidated: (code) => invalidated.push(code) });
  await c.state();
  h.deletePorts.at(-1).disconnect();
  assert.deepEqual(invalidated, ["host_unavailable"]);
});

// --- FINDING-DELETE-04: only the Host's receipt for this review is a success ----------------------------------

const BAD_RECEIPTS = {
  "no operationId": ({ operationId, ...rest }) => rest,
  "no deleted": ({ deleted, ...rest }) => rest,
  "operationId of another type": (r) => ({ ...r, operationId: 7 }),
  "deleted of another type": (r) => ({ ...r, deleted: "3" }),
  "deleted null": (r) => ({ ...r, deleted: null }),
  "deleted an array": (r) => ({ ...r, deleted: [] }),
  "another operation": (r) => ({ ...r, operationId: "00000000-0000-4000-8000-000000000000" }),
};

for (const [name, shape] of Object.entries(BAD_RECEIPTS)) {
  test(`a receipt with ${name} is no success: the same port's status proves the COMMIT, nothing is resent`, async () => {
    const h = host();
    const c = client(h);
    const preview = await c.preview({ scope: "visit", id: "a1" });
    h.deleteReplyShape = shape;
    const done = await c.delete(preview.reviewId); // the Host did commit; the status says so with a real receipt
    assert.equal(done.operationId, preview.reviewId);
    assert.equal(reqs(h, "item_delete").length, 1);
    assert.equal(reqs(h, "item_delete_status").length, 1);
    assert.equal(h.deleteCommits, 1);
    c.close();
  });

  test(`a receipt with ${name} and no proof by status is unknown, never a success`, async () => {
    const h = host();
    const c = client(h);
    const preview = await c.preview({ scope: "visit", id: "a1" });
    h.deleteReplyShape = shape;
    h.statusReplyShape = (s) => ({ ...s, confirmed: false });
    await assert.rejects(c.delete(preview.reviewId), { code: "item_delete_unknown" });
    assert.equal(reqs(h, "item_delete").length, 1, "never resent");
    c.close();
  });
}

const BAD_STATUS = {
  "another operation": (s) => ({ ...s, operationId: "00000000-0000-4000-8000-000000000000" }),
  "no operationId": ({ operationId, ...rest }) => rest,
  "no deleted": ({ deleted, ...rest }) => rest,
  "deleted of another type": (s) => ({ ...s, deleted: 1 }),
  "confirmed of another type": (s) => ({ ...s, confirmed: "true" }),
};
for (const [name, shape] of Object.entries(BAD_STATUS)) {
  test(`a status with ${name} proves nothing: unknown`, async () => {
    const h = host();
    const c = client(h);
    const preview = await c.preview({ scope: "visit", id: "a1" });
    h.dropDeleteReply = true;
    h.statusReplyShape = shape;
    await assert.rejects(c.delete(preview.reviewId), { code: "item_delete_unknown" });
    assert.equal(reqs(h, "item_delete").length, 1);
    c.close();
  });
}

test("the Host's own receipt is accepted, also with extra fields", async () => {
  const h = host();
  const c = client(h);
  const preview = await c.preview({ scope: "visit", id: "a1" });
  h.deleteReplyShape = (r) => ({ ...r, extra: { fine: true } });
  const done = await c.delete(preview.reviewId);
  assert.equal(done.operationId, preview.reviewId);
  assert.equal(reqs(h, "item_delete_status").length, 0, "no status needed");
  const second = await c.preview({ scope: "visit", id: "a2" });
  h.dropDeleteReply = true;
  h.statusReplyShape = (s) => ({ ...s, extra: 1 });
  assert.equal((await c.delete(second.reviewId)).confirmed, true);
  c.close();
});
