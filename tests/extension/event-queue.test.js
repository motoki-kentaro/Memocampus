import { test } from "node:test";
import assert from "node:assert/strict";
import {
  DEAD_LETTER_KEY,
  MAX_DEAD_LETTERS,
  MAX_PENDING,
  QUEUE_KEY,
  QueueError,
  classifyResponse,
  createEventQueue,
} from "../../extension/event-queue.js";
import { describeQueueStatus, describeRecordResult } from "../../extension/queue-text.js";

/** chrome.storage.local stand-in: async, copies values, optional delays and injected failures. */
function fakeStorage({ delay = 0, failSet = () => false } = {}) {
  const data = new Map();
  const pause = () => (delay ? new Promise((resolve) => setTimeout(resolve, Math.random() * delay)) : Promise.resolve());
  return {
    data,
    failSet,
    async get(keys) {
      await pause();
      const out = {};
      for (const key of Array.isArray(keys) ? keys : [keys]) if (data.has(key)) out[key] = structuredClone(data.get(key));
      return out;
    },
    async set(items) {
      await pause();
      if (this.failSet(items)) throw new Error("QUOTA_BYTES quota exceeded");
      for (const [key, value] of Object.entries(items)) data.set(key, structuredClone(value));
    },
  };
}

/** Native host stand-in: validates like the Rust host and stores each event id once. */
function fakeHost() {
  const stored = new Map();
  const host = {
    stored,
    down: false,
    vaultReady: true,
    loseAckOnce: new Set(),
    calls: 0,
    async send(event) {
      host.calls += 1;
      if (host.down) throw new QueueError("host_unavailable");
      if (typeof event.id !== "string" || typeof event.url !== "string") return { ok: false, eventId: typeof event.id === "string" ? event.id : null, error: "invalid_event" };
      if (!/^https?:/.test(event.url)) return { ok: false, eventId: event.id, error: "invalid_url" };
      if (!host.vaultReady) return { ok: false, eventId: event.id, error: "vault_not_configured" };
      if (!stored.has(event.id)) stored.set(event.id, event); // INSERT OR IGNORE
      if (host.loseAckOnce.delete(event.id)) throw new QueueError("host_unavailable");
      return { ok: true, eventId: event.id };
    },
  };
  return host;
}

let counter = 0;
const visit = (url = "https://a.test/", id = `e${++counter}`) => ({ type: "visit", id, url, title: "T", visitedAt: "2026-10-30T00:00:00.000Z" });
const pending = (storage) => storage.data.get(QUEUE_KEY) ?? [];
const deadLetters = (storage) => storage.data.get(DEAD_LETTER_KEY) ?? [];

function setup(options = {}) {
  const storage = options.storage ?? fakeStorage(options);
  const host = options.host ?? fakeHost();
  const queue = createEventQueue({ storage, send: host.send, now: () => new Date("2026-10-30T01:02:03.000Z"), timeoutMs: options.timeoutMs ?? 1000 });
  return { storage, host, queue };
}

test("an invalid URL ahead of a valid one is quarantined and the valid one is stored", async () => {
  const { storage, host, queue } = setup();
  const bad = visit("javascript:alert(1)");
  const good = visit("https://good.test/");
  await queue.enqueueAll([bad, good]);
  assert.equal(await queue.flush(), null);
  assert.deepEqual([...host.stored.keys()], [good.id]);
  assert.deepEqual(pending(storage), []);
  assert.deepEqual(deadLetters(storage), [{ errorCode: "invalid_url", eventId: bad.id, failedAt: "2026-10-30T01:02:03.000Z", event: bad }]);
});

test("malformed events are quarantined as permanent errors", async () => {
  const { storage, queue } = setup();
  const noUrl = { type: "visit", id: "no-url", title: "T" };
  const noId = { type: "visit", url: "https://a.test/" };
  await queue.enqueueAll([noUrl, noId, null]);
  assert.equal(await queue.flush(), null);
  assert.deepEqual(pending(storage), []);
  assert.deepEqual(
    deadLetters(storage).map(({ errorCode, eventId }) => [errorCode, eventId]),
    [["invalid_event", "no-url"], ["invalid_event", null], ["invalid_event", null]],
  );
});

test("a disconnected host keeps the whole queue for a later retry", async () => {
  const { storage, host, queue } = setup();
  host.down = true;
  const events = [visit(), visit()];
  await queue.enqueueAll(events);
  assert.equal(await queue.flush(), "host_unavailable");
  assert.deepEqual(pending(storage), events);
  host.down = false;
  assert.equal(await queue.flush(), null);
  assert.equal(host.stored.size, 2);
});

test("no vault, busy vault, unknown codes and bad acks are temporary, never quarantined", async () => {
  for (const response of [
    { ok: false, eventId: "x", error: "vault_not_configured" },
    { ok: false, eventId: "x", error: "vault_busy" },
    { ok: false, eventId: "x", error: "some_future_code" },
    { ok: false, eventId: "other", error: "invalid_url" }, // not this event
    { ok: true, eventId: "other" },
    undefined,
  ]) {
    assert.equal(classifyResponse({ id: "x" }, response).kind, "temporary", JSON.stringify(response));
  }
  const { storage, host, queue } = setup();
  host.vaultReady = false;
  await queue.enqueue(visit());
  assert.equal(await queue.flush(), "vault_not_configured");
  assert.equal(pending(storage).length, 1);
  assert.deepEqual(deadLetters(storage), []);
});

test("a timed-out send is temporary", async () => {
  const { storage, queue } = setup({ host: { send: () => new Promise(() => {}) }, timeoutMs: 20 });
  await queue.enqueue(visit());
  assert.equal(await queue.flush(), "timeout");
  assert.equal(pending(storage).length, 1);
});

test("if quarantining fails the original event is kept and delivery pauses", async () => {
  const storage = fakeStorage({ failSet: (items) => DEAD_LETTER_KEY in items });
  const { host, queue } = setup({ storage });
  const bad = visit("javascript:void(0)");
  const good = visit();
  await queue.enqueueAll([bad, good]);
  assert.equal(await queue.flush(), "dead_letter_write_failed");
  assert.deepEqual(pending(storage), [bad, good]);
  assert.equal(host.stored.size, 0, "nothing behind the head is skipped silently");
  storage.failSet = () => false;
  assert.equal(await queue.flush(), null);
  assert.deepEqual(pending(storage), []);
  assert.equal(deadLetters(storage).length, 1);
});

test("a full dead-letter queue pauses delivery instead of dropping the event", async () => {
  const storage = fakeStorage();
  storage.data.set(DEAD_LETTER_KEY, Array.from({ length: MAX_DEAD_LETTERS }, (_, i) => ({ eventId: `d${i}` })));
  const { queue } = setup({ storage });
  const bad = visit("ftp://x/");
  await queue.enqueue(bad);
  assert.equal(await queue.flush(), "dead_letter_full");
  assert.deepEqual(pending(storage), [bad]);
  const status = await queue.status();
  assert.equal(status.blockedBy, "dead_letter_full");
  assert.match(describeQueueStatus({ ok: true, ...status, blockedReason: "x" }), /隔離したイベント 1000件/);
});

test("a crash between the dead-letter write and the queue removal is repaired without duplicates", async () => {
  const storage = fakeStorage();
  const bad = visit("javascript:1");
  storage.data.set(QUEUE_KEY, [bad]);
  storage.data.set(DEAD_LETTER_KEY, [{ errorCode: "invalid_url", eventId: bad.id, failedAt: "x", event: bad }]);
  const { queue } = setup({ storage });
  assert.equal(await queue.flush(), null);
  assert.deepEqual(pending(storage), []);
  assert.equal(deadLetters(storage).length, 1);
});

test("queue and dead letters survive a service worker restart", async () => {
  const storage = fakeStorage();
  const first = setup({ storage });
  first.host.down = true;
  const bad = visit("javascript:1");
  const good = visit();
  await first.queue.enqueueAll([bad, good]);
  await first.queue.flush();
  // New queue object over the same storage = restarted worker with no in-memory state.
  const second = setup({ storage });
  assert.deepEqual(pending(storage), [bad, good]);
  assert.equal(await second.queue.flush(), null);
  assert.deepEqual([...second.host.stored.keys()], [good.id]);
  const restarted = setup({ storage });
  const status = await restarted.queue.status();
  assert.deepEqual([status.pending, status.deadLetters, status.deadLetterCodes], [0, 1, ["invalid_url"]]);
});

test("a lost ACK leads to a resend that the host stores only once", async () => {
  const { storage, host, queue } = setup();
  const event = visit();
  host.loseAckOnce.add(event.id);
  await queue.enqueue(event);
  assert.equal(await queue.flush(), "host_unavailable");
  assert.equal(host.stored.size, 1, "the host stored it although the ACK was lost");
  assert.deepEqual(pending(storage), [event]);
  assert.equal(await queue.flush(), null);
  assert.equal(host.stored.size, 1);
  assert.equal(host.calls, 2);
  assert.deepEqual(pending(storage), []);
});

test("concurrent enqueues and flushes never lose an event", async () => {
  const { storage, host, queue } = setup({ delay: 3 });
  const events = Array.from({ length: 60 }, () => visit());
  await Promise.all(events.map(async (event, i) => {
    await queue.enqueue(event);
    if (i % 3 === 0) await queue.flush();
  }));
  await queue.flush();
  assert.equal(host.stored.size, events.length);
  assert.deepEqual(pending(storage), []);
  // And with the host down, every concurrently enqueued event is still queued.
  const down = setup({ delay: 3 });
  down.host.down = true;
  const more = Array.from({ length: 40 }, () => visit());
  await Promise.all(more.map((event) => down.queue.enqueue(event).then(() => down.queue.flush())));
  assert.deepEqual(new Set(pending(down.storage).map((event) => event.id)), new Set(more.map((event) => event.id)));
});

test("enqueueAll stores every event or none and deduplicates by id", async () => {
  const { storage, queue } = setup();
  const event = visit();
  assert.equal(await queue.enqueueAll([event, event, visit()]), 2);
  assert.equal(await queue.enqueue(event), 0);
  assert.equal(pending(storage).length, 2);

  storage.data.set(QUEUE_KEY, Array.from({ length: MAX_PENDING - 1 }, () => visit()));
  await assert.rejects(queue.enqueueAll([visit(), visit()]), { code: "queue_full" });
  assert.equal(pending(storage).length, MAX_PENDING - 1, "no partial batch");

  storage.data.set(QUEUE_KEY, []);
  storage.failSet = (items) => QUEUE_KEY in items;
  await assert.rejects(queue.enqueueAll([visit(), visit()]), { code: "storage_write_failed" });
  assert.deepEqual(pending(storage), []);
});

test("dropped events are counted, not silently discarded", async () => {
  const { queue } = setup();
  await queue.recordDropped();
  await queue.recordDropped();
  const status = await queue.status();
  assert.equal(status.dropped, 2);
  assert.match(describeQueueStatus({ ok: true, ...status }), /破棄したイベント 2件/);
});

test("locate separates stored, pending and quarantined snapshots", async () => {
  const { host, queue } = setup();
  const snaps = [visit(), visit("javascript:x"), visit()];
  host.loseAckOnce.add(snaps[2].id);
  await queue.enqueueAll(snaps);
  await queue.flush();
  assert.deepEqual(await queue.locate(snaps.map((event) => event.id)), { pending: 1, deadLettered: 1, stored: 1 });
});

test("record results never call queued snapshots saved", () => {
  assert.equal(describeRecordResult({ ok: true, queued: 3, stored: 3, pending: 0, deadLettered: 0 }), "3件のタブをSQLiteに保存しました。タブは閉じていません。");
  const partial = describeRecordResult({ ok: true, queued: 3, stored: 1, pending: 2, deadLettered: 0, reason: "Vaultが未設定です。" });
  assert.match(partial, /送信キューに登録/);
  assert.match(partial, /SQLiteに保存済み: 1件/);
  assert.match(partial, /未保存（送信待ち・自動で再送）: 2件/);
  assert.match(partial, /理由: Vaultが未設定です。/);
  assert.doesNotMatch(partial, /SQLiteに保存しました/);
  assert.match(describeRecordResult({ ok: false, error: "送信待ちキューが満杯のため、記録しませんでした（1件も登録していません）。" }), /1件も登録していません/);
  assert.equal(describeRecordResult({ ok: true, queued: 0 }), "記録できるタブ（http/https）がありません。");
});
