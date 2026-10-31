// Runs the real service worker, queue, policy gate and evaluator against a fake chrome and host.
import { test } from "node:test";
import assert from "node:assert/strict";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { DEAD_LETTER_KEY, QUEUE_KEY, classifyResponse, createEventQueue } from "../../extension/event-queue.js";
import { createPolicyGate } from "../../extension/policy-gate.js";
import { describePolicyStatus, describeRecordResult } from "../../extension/queue-text.js";
import defaults from "../../extension/policy-defaults.json" with { type: "json" };
import { extensionDir, loadSidePanel, settle } from "./fake-dom.js";

const AUTH_URL = "https://accounts.google.com/signin/secret-path";
const TOKEN_URL = "https://a.test/page?access_token=SECRET-VALUE";
const B_URL = "https://mail.google.com/mail/u/0/";
const NORMAL_URL = "https://normal.test/article";
const SEARCH_URL = "https://www.google.com/search?q=rust";
const SECRET_TITLE = "Sensitive title";

const clone = (value) => structuredClone(value);
function withRule(rule, base = defaults) {
  const policy = clone(base);
  policy.revision += 1;
  policy.layers.neverSave.rules.push(rule);
  return policy;
}

/** A fake host: `state` and `policy` can change at any time, as the real files can. */
function fakeHost(overrides = {}) {
  return {
    state: "ready",
    policy: clone(defaults),
    down: false,
    initialized: false,
    statusRequests: 0,
    initializations: 0,
    received: [],
    answer: null,
    respond(message) {
      if (message.type === "policy_status") {
        this.statusRequests += 1;
        return { ok: true, type: "policy_result", requestId: message.requestId, state: this.state, ...(this.state === "ready" ? { policy: this.policy, etag: `rev-${this.policy.revision}` } : {}) };
      }
      if (message.type === "policy_initialize") {
        this.initializations += 1;
        if (this.state !== "uninitialized") return { ok: false, type: "policy_result", requestId: message.requestId, error: "policy_already_initialized" };
        this.state = "ready";
        return { ok: true, type: "policy_result", requestId: message.requestId, state: "ready", policy: this.policy, etag: "rev-1" };
      }
      this.received.push(message);
      return this.answer ? this.answer(message) : { ok: true, eventId: message.id };
    },
    ...overrides,
  };
}

const tabOf = (id, url, extra = {}) => ({ id, windowId: 1, index: id, url, title: SECRET_TITLE, pinned: false, incognito: false, ...extra });

function startWorker({ host = fakeHost(), tabs = {}, query = [], data = new Map() } = {}) {
  const listeners = {};
  const on = (name) => ({ addListener: (fn) => ((listeners[name] ??= []).push(fn)) });
  const badge = { text: "", title: "" };
  const writes = [];
  const runtime = {
    onStartup: on("startup"),
    onInstalled: on("installed"),
    onMessage: on("message"),
    sendMessage: async () => {},
    connectNative() {},
    lastError: undefined,
    sendNativeMessage(_name, message, reply) {
      if (host.down) {
        runtime.lastError = { message: "Native host has exited." };
        reply(undefined);
        runtime.lastError = undefined;
        return;
      }
      reply(clone(host.respond(clone(message))));
    },
  };
  globalThis.chrome = {
    runtime,
    webNavigation: { onCompleted: on("navigation") },
    contextMenus: { create() {}, onClicked: on("menuClick") },
    notifications: { create: async () => {} },
    action: { setBadgeText: async ({ text }) => void (badge.text = text), setTitle: async ({ title }) => void (badge.title = title) },
    sidePanel: { setPanelBehavior: async () => {} },
    scripting: { executeScript: async () => [] },
    tabs: { get: async (id) => tabs[id], query: async () => query },
    storage: {
      local: {
        async get(keys) {
          const out = {};
          for (const key of Array.isArray(keys) ? keys : [keys]) if (data.has(key)) out[key] = clone(data.get(key));
          return out;
        },
        async set(items) {
          writes.push(JSON.stringify(items));
          for (const [key, value] of Object.entries(items)) data.set(key, clone(value));
        },
      },
      session: { get: async () => ({}), set: async () => {}, remove: async () => {} },
    },
  };
  return import(`${pathToFileURL(path.join(extensionDir, "service-worker.js")).href}?ingress=${Math.random()}`).then(() => ({
    host,
    data,
    badge,
    writes,
    stored: () => JSON.stringify([...data.entries()]),
    pending: () => data.get(QUEUE_KEY) ?? [],
    dead: () => data.get(DEAD_LETTER_KEY) ?? [],
    async navigate(url, id = 1) {
      tabs[id] ??= tabOf(id, url);
      await Promise.all(listeners.navigation.map((fn) => fn({ frameId: 0, tabId: id, url })));
      await settle(20);
    },
    message: (message) => new Promise((resolve) => listeners.message.forEach((fn) => fn(message, {}, resolve))),
  }));
}

const visit = (id, url, title = SECRET_TITLE) => ({ type: "visit", id, url, title, incognito: false, visitedAt: "2026-10-30T00:00:00.000Z" });
const snapshot = (id, url) => ({ type: "tab_snapshot", id, windowId: 1, tabIndex: 0, url, title: SECRET_TITLE, pinned: false, incognito: false, recordedAt: "2026-10-30T00:00:00.000Z" });
const seeded = (queue = [], dead = []) => new Map([[QUEUE_KEY, queue], [DEAD_LETTER_KEY, dead]]);

function assertNoTrace(worker, ...secrets) {
  for (const secret of [...secrets, SECRET_TITLE]) {
    assert.ok(!worker.stored().includes(secret), "not in extension storage");
    assert.ok(!worker.writes.some((write) => write.includes(secret)), "and never written, even briefly");
    assert.ok(!JSON.stringify(worker.host.received).includes(secret), "not sent to the host");
  }
  assert.deepEqual(worker.pending(), []);
  assert.deepEqual(worker.dead(), []);
}

async function withConsoleWatch(run) {
  const seen = [];
  const original = {};
  for (const method of ["log", "info", "warn", "error", "debug"]) {
    original[method] = console[method];
    console[method] = (...args) => seen.push(args.join(" "));
  }
  try {
    await run();
  } finally {
    Object.assign(console, original);
  }
  return seen.join("\n");
}

// --- Before the queue ---------------------------------------------------------------------------

for (const [name, url] of [["an authentication host", AUTH_URL], ["a credential-bearing URL", TOKEN_URL], ["a malformed URL", "https://a.test/?q=%FF"]]) {
  test(`${name} leaves no trace in the queue, dead letters, host or console`, async () => {
    const worker = await startWorker();
    const logged = await withConsoleWatch(() => worker.navigate(url));
    assertNoTrace(worker, url);
    assert.deepEqual(worker.host.received, []);
    assert.equal(logged, "");
    assert.equal(worker.badge.text, "", "a refusal is not a problem to report");
  });
}

for (const [name, url] of [["a search result", SEARCH_URL], ["an ordinary site", NORMAL_URL], ["a save-but-no-MCP (layer B) site", B_URL]]) {
  test(`${name} is recorded`, async () => {
    const worker = await startWorker();
    await worker.navigate(url);
    assert.deepEqual(worker.host.received.map((event) => [event.type, event.url, event.title]), [["visit", url, SECRET_TITLE]]);
    assert.ok(typeof worker.host.received[0].id === "string" && worker.host.received[0].id !== "");
    assert.deepEqual(worker.pending(), []);
  });
}

test("incognito stays excluded before the policy is even asked", async () => {
  const worker = await startWorker({ tabs: { 7: tabOf(7, NORMAL_URL, { incognito: true }) } });
  await worker.navigate(NORMAL_URL, 7);
  assert.equal(worker.host.statusRequests, 0);
  assertNoTrace(worker, NORMAL_URL);
});

for (const [name, change] of [
  ["an uninitialized policy", (host) => (host.state = "uninitialized")],
  ["a damaged policy", (host) => (host.state = "invalid")],
  ["a busy policy", (host) => (host.state = "busy")],
  ["an unavailable host", (host) => (host.down = true)],
  ["a document that fails validation", (host) => (host.policy = { ...host.policy, engineVersion: 2 })],
  ["an unrecognized state", (host) => (host.state = "something-new")],
]) {
  test(`${name} records nothing, keeps nothing, and is shown`, async () => {
    const host = fakeHost();
    change(host);
    const worker = await startWorker({ host });
    await worker.navigate(NORMAL_URL);
    assertNoTrace(worker, NORMAL_URL);
    assert.deepEqual(host.received, []);
    assert.equal(worker.badge.text, "!");
    assert.ok(!worker.badge.title.includes(NORMAL_URL) && !worker.badge.title.includes(SECRET_TITLE));
    const status = await worker.message({ type: "policy-status" });
    assert.equal(status.ok, true);
    assert.notEqual(status.state, "ready");
    assert.ok(describePolicyStatus(status) !== "");
  });
}

test("a failed policy check does not fall back to a policy that was cached earlier", async () => {
  const host = fakeHost();
  const worker = await startWorker({ host });
  await worker.navigate(NORMAL_URL, 1);
  assert.equal(host.received.length, 1);
  host.down = true;
  await worker.navigate(NORMAL_URL, 2);
  assert.deepEqual(worker.pending(), [], "the very next visit is not kept on the strength of the old answer");
  assert.equal(host.received.length, 1);
  await worker.message({ type: "flush-queue" });
  assert.equal(host.received.length, 1, "still nothing delivered");
});

test("a restarted worker starts from nothing and asks the host again", async () => {
  const host = fakeHost();
  const first = await startWorker({ host });
  await first.navigate(NORMAL_URL);
  const asked = host.statusRequests;
  host.policy = withRule("normal.test");
  const second = await startWorker({ host, data: first.data });
  await second.navigate(NORMAL_URL);
  assert.ok(host.statusRequests > asked, "the new worker fetched the policy");
  assert.equal(host.received.length, 1, "and applied the new rule");
});

// --- 全タブを記録 ----------------------------------------------------------------------------------

test("recording all tabs keeps allowed tabs and layer B, and drops the rest without a trace", async () => {
  const query = [
    tabOf(1, NORMAL_URL),
    tabOf(2, AUTH_URL),
    tabOf(3, B_URL),
    tabOf(4, TOKEN_URL),
    tabOf(5, "https://incognito.test/", { incognito: true }),
    tabOf(6, "https://a.test/?q=%FF"),
    tabOf(7, "chrome://settings/"),
  ];
  const worker = await startWorker({ query });
  const result = await worker.message({ type: "record-all-tabs" });
  assert.deepEqual(worker.host.received.map((event) => event.url), [NORMAL_URL, B_URL]);
  assert.deepEqual(result, { ok: true, queued: 2, pending: 0, deadLettered: 0, stored: 2, reason: null });
  const trace = worker.stored() + JSON.stringify(worker.host.received);
  for (const secret of [AUTH_URL, TOKEN_URL, "incognito.test", "%FF"]) assert.ok(!trace.includes(secret));
});

test("all tabs excluded: nothing is stored or sent, and the count is zero", async () => {
  const worker = await startWorker({ query: [tabOf(2, AUTH_URL), tabOf(4, TOKEN_URL)] });
  const result = await worker.message({ type: "record-all-tabs" });
  assert.deepEqual(result, { ok: true, queued: 0, stored: 0, pending: 0, deadLettered: 0 });
  assertNoTrace(worker, AUTH_URL, TOKEN_URL);
  assert.deepEqual(worker.host.received, []);
});

test("recording all tabs without a usable policy stops entirely and says so", async () => {
  for (const change of [(host) => (host.state = "uninitialized"), (host) => (host.state = "invalid"), (host) => (host.down = true)]) {
    const host = fakeHost();
    change(host);
    const worker = await startWorker({ host, query: [tabOf(1, NORMAL_URL)] });
    const result = await worker.message({ type: "record-all-tabs" });
    assert.equal(result.ok, false);
    assert.match(describeRecordResult(result), /1件も登録していません/);
    assertNoTrace(worker, NORMAL_URL);
  }
});

test("a tab the host refuses (newer policy) is not counted as stored", async () => {
  const host = fakeHost({ answer: (event) => (event.url === B_URL ? { ok: true, eventId: event.id, dropped: "policy" } : { ok: true, eventId: event.id }) });
  const worker = await startWorker({ host, query: [tabOf(1, NORMAL_URL), tabOf(3, B_URL)] });
  const result = await worker.message({ type: "record-all-tabs" });
  assert.deepEqual(result, { ok: true, queued: 1, pending: 0, deadLettered: 0, stored: 1, reason: null });
  assert.deepEqual(worker.pending(), []);
  assert.deepEqual(worker.dead(), []);
});

// --- The stored queue ----------------------------------------------------------------------------

test("events queued under an older policy are judged again; refused ones vanish without a dead letter", async () => {
  const host = fakeHost();
  host.policy = withRule("newly-private.test");
  const queue = [visit("v1", "https://newly-private.test/x"), snapshot("s1", "https://newly-private.test/y"), visit("v2", NORMAL_URL, "Kept"), visit("v3", AUTH_URL), snapshot("s2", B_URL)];
  const worker = await startWorker({ host, data: seeded(queue) });
  const logged = await withConsoleWatch(() => worker.message({ type: "flush-queue" }));
  assert.deepEqual(host.received.map((event) => event.id), ["v2", "s2"]);
  assert.deepEqual(worker.pending(), []);
  assert.deepEqual(worker.dead(), []);
  assert.ok(!worker.stored().includes("newly-private") && !worker.stored().includes(AUTH_URL));
  assert.equal(logged, "");
});

test("the storage is cleaned even when delivery is paused at the head", async () => {
  const host = fakeHost({ answer: (event) => ({ ok: false, eventId: event.id, error: "vault_busy" }) });
  const queue = [visit("v1", NORMAL_URL, "Kept"), visit("v2", AUTH_URL), snapshot("s1", TOKEN_URL)];
  const dead = [{ errorCode: "invalid_event", eventId: "d1", failedAt: "t", event: visit("d1", AUTH_URL) }, { errorCode: "invalid_event", eventId: "d2", failedAt: "t", event: visit("d2", NORMAL_URL, "Kept") }];
  const worker = await startWorker({ host, data: seeded(queue, dead) });
  const reply = await worker.message({ type: "flush-queue" });
  assert.equal(reply.blockedBy, "vault_busy");
  assert.deepEqual(worker.pending().map((event) => event.id), ["v1"]);
  assert.deepEqual(worker.dead().map((entry) => entry.eventId), ["d2"]);
  assert.ok(!worker.stored().includes(AUTH_URL) && !worker.stored().includes("SECRET-VALUE") && !worker.stored().includes(SECRET_TITLE));
});

test("old dead letters are cleaned even when nothing is pending", async () => {
  const dead = [{ errorCode: "invalid_event", eventId: "d1", failedAt: "t", event: visit("d1", AUTH_URL) }];
  const worker = await startWorker({ data: seeded([], dead) });
  await worker.message({ type: "flush-queue" });
  assert.deepEqual(worker.dead(), []);
  assert.ok(!worker.stored().includes(AUTH_URL));
});

for (const [name, change, code] of [
  ["uninitialized", (host) => (host.state = "uninitialized"), "policy_not_initialized"],
  ["damaged", (host) => (host.state = "invalid"), "policy_invalid"],
  ["busy", (host) => (host.state = "busy"), "policy_busy"],
  ["unreachable", (host) => (host.down = true), "host_unavailable"],
]) {
  test(`a ${name} policy pauses delivery and removes nothing, then recovery delivers`, async () => {
    const host = fakeHost();
    change(host);
    const queue = [visit("v1", NORMAL_URL, "Kept"), visit("v2", AUTH_URL)];
    const worker = await startWorker({ host, data: seeded(queue) });
    const reply = await worker.message({ type: "flush-queue" });
    assert.equal(reply.blockedBy, code);
    assert.deepEqual(worker.pending().map((event) => event.id), ["v1", "v2"], "undecided events are not lost");
    assert.deepEqual(host.received, []);
    assert.deepEqual(worker.dead(), []);

    host.state = "ready";
    host.down = false;
    const status = await worker.message({ type: "policy-status" });
    assert.equal(status.state, "ready");
    assert.deepEqual(host.received.map((event) => event.id), ["v1"]);
    assert.deepEqual(worker.pending(), []);
    assert.ok(!worker.stored().includes(AUTH_URL));
    assert.equal(worker.badge.text, "");
  });
}

test("a policy update during a drain applies to the events still waiting", async () => {
  const host = fakeHost();
  host.answer = (event) => {
    // Simulates another writer: the new rule arrives right after the first delivery.
    host.policy = withRule("later.test");
    return { ok: true, eventId: event.id };
  };
  const worker = await startWorker({ host, data: seeded([visit("v1", NORMAL_URL), visit("v2", "https://later.test/x")]) });
  await worker.message({ type: "flush-queue" });
  // Within one drain the worker may use its cached answer; the host decides for itself on arrival.
  assert.deepEqual(worker.pending(), []);
  assert.deepEqual(worker.dead(), []);
});

test("a policy that breaks during delivery stops collection at once, without waiting for the cache", async () => {
  const host = fakeHost();
  const worker = await startWorker({ host });
  await worker.navigate(NORMAL_URL, 1);
  host.answer = (event) => ({ ok: false, eventId: event.id, error: "policy_invalid" });
  await worker.navigate(NORMAL_URL, 2);
  assert.equal(worker.badge.text, "!");
  assert.equal(worker.pending().length, 1, "that event is undecided, so it stays");
  host.state = "invalid";
  await worker.navigate("https://after-failure.test/", 3);
  assert.equal(worker.pending().length, 1, "nothing new is collected");
  assert.ok(!worker.stored().includes("after-failure"));
});

test("an event that reaches the head after the sweep is still judged, and leaves no dead letter", async () => {
  const data = new Map();
  const storage = {
    get: async (keys) => Object.fromEntries((Array.isArray(keys) ? keys : [keys]).filter((key) => data.has(key)).map((key) => [key, clone(data.get(key))])),
    set: async (items) => Object.entries(items).forEach(([key, value]) => data.set(key, clone(value))),
  };
  const sent = [];
  const policy = { current: async () => ({ ok: true, version: "same", decide: (url) => (url === AUTH_URL ? "deny" : "allow") }) };
  const queue = createEventQueue({ storage, policy, send: async (event) => (sent.push(event.id), { ok: true, eventId: event.id }) });
  await queue.enqueue(visit("first", NORMAL_URL));
  await queue.flush(); // this version is now swept, so only the check of the head can catch the next one
  await queue.enqueue(visit("late", AUTH_URL));
  assert.equal(await queue.flush(), null);
  assert.deepEqual(sent, ["first"]);
  assert.deepEqual(data.get(QUEUE_KEY), []);
  assert.deepEqual(data.get(DEAD_LETTER_KEY) ?? [], []);
  assert.deepEqual(await queue.locate(["late"]), { pending: 0, deadLettered: 0, stored: 0, refused: 1 });
});

// --- The drop acknowledgement --------------------------------------------------------------------

test("only a policy drop ACK discards; a policy failure and unknown answers never do", () => {
  const event = visit("e1", NORMAL_URL);
  assert.equal(classifyResponse(event, { ok: true, eventId: "e1", dropped: "policy" }).kind, "discard");
  assert.equal(classifyResponse(event, { ok: true, eventId: null, dropped: "policy" }).kind, "discard");
  assert.equal(classifyResponse(event, { ok: true, eventId: "other", dropped: "policy" }).kind, "temporary");
  assert.equal(classifyResponse(event, { ok: true, eventId: "e1", dropped: "other" }).kind, "temporary");
  for (const error of ["policy_not_initialized", "policy_invalid", "policy_busy", "policy_unavailable"]) {
    assert.deepEqual(classifyResponse(event, { ok: false, eventId: "e1", error }), { kind: "temporary", code: error });
  }
  assert.equal(classifyResponse(event, { ok: true, eventId: "e1" }).kind, "delivered");
});

test("an event the host drops is removed, with no dead letter", async () => {
  const host = fakeHost({ answer: (event) => ({ ok: true, eventId: event.id, dropped: "policy" }) });
  const worker = await startWorker({ host, data: seeded([visit("v1", NORMAL_URL)]) });
  const reply = await worker.message({ type: "flush-queue" });
  assert.equal(reply.blockedBy, null);
  assert.deepEqual(worker.pending(), []);
  assert.deepEqual(worker.dead(), []);
});

test("a policy failure from the host keeps the event queued", async () => {
  const host = fakeHost({ answer: (event) => ({ ok: false, eventId: event.id, error: "policy_busy" }) });
  const worker = await startWorker({ host, data: seeded([visit("v1", NORMAL_URL)]) });
  const reply = await worker.message({ type: "flush-queue" });
  assert.equal(reply.blockedBy, "policy_busy");
  assert.equal(worker.pending().length, 1);
  assert.deepEqual(worker.dead(), []);
});

// --- Initialization ------------------------------------------------------------------------------

test("nothing initializes the policy except the explicit request; afterwards recording starts", async () => {
  const host = fakeHost({ state: "uninitialized" });
  const worker = await startWorker({ host, data: seeded([visit("v1", NORMAL_URL, "Kept"), visit("v2", AUTH_URL)]) });
  await worker.navigate(NORMAL_URL);
  await worker.message({ type: "flush-queue" });
  await worker.message({ type: "policy-status" });
  assert.equal(host.initializations, 0);
  assert.deepEqual(worker.pending().map((event) => event.id), ["v1", "v2"]);

  const result = await worker.message({ type: "policy-initialize" });
  assert.equal(result.state, "ready");
  assert.equal(host.initializations, 1);
  assert.deepEqual(host.received.map((event) => event.id), ["v1"], "queued events are judged and delivered");
  assert.ok(!worker.stored().includes(AUTH_URL), "and cleaned");
  await worker.navigate(SEARCH_URL, 2);
  assert.equal(host.received.at(-1).url, SEARCH_URL);
});

test("initializing over a damaged policy is refused and recording stays stopped", async () => {
  const host = fakeHost({ state: "invalid" });
  const worker = await startWorker({ host });
  const result = await worker.message({ type: "policy-initialize" });
  assert.equal(result.state, "invalid");
  assert.equal(result.code, "policy_invalid");
  await worker.navigate(NORMAL_URL);
  assert.deepEqual(host.received, []);
});

test("the side panel offers initialization only on first use, and only runs it when asked", async () => {
  const panel = (policyStatus) => loadSidePanel({ vaults: {}, active: null, policyStatus });
  const first = await panel({ ok: true, state: "uninitialized", code: "policy_not_initialized", message: "初期化が必要", revision: null });
  await settle(20);
  assert.equal(first.$("#policy-status").hidden, false);
  assert.equal(first.$("#policy-message").textContent, "初期化が必要");
  assert.equal(first.$("#policy-initialize").hidden, false);
  assert.equal(first.host.policyInitializations ?? 0, 0, "opening the panel initializes nothing");
  first.$("#policy-initialize").dispatch("click");
  await settle(20);
  assert.equal(first.host.policyInitializations, 1);
  assert.equal(first.$("#policy-status").hidden, true);

  const damaged = await panel({ ok: true, state: "invalid", code: "policy_invalid", message: "破損", revision: null });
  await settle(20);
  assert.equal(damaged.$("#policy-status").hidden, false);
  assert.equal(damaged.$("#policy-initialize").hidden, true, "a damaged policy is never offered a reset");

  const fine = await panel({ ok: true, state: "ready", code: null, message: null, revision: 1 });
  await settle(20);
  assert.equal(fine.$("#policy-status").hidden, true);
});

// --- Cache rules ---------------------------------------------------------------------------------

function gateWith(host, clock) {
  const send = async (message) => clone(host.respond(message));
  return createPolicyGate({ send, now: () => clock.now, ttlMs: 1000 });
}

test("the cache expires, and an expired or failed check forgets the policy", async () => {
  const host = fakeHost();
  const clock = { now: 0 };
  const gate = gateWith(host, clock);
  assert.equal(gate.problem(), null);
  assert.equal((await gate.current()).ok, true);
  assert.equal((await gate.current()).ok, true);
  assert.equal(host.statusRequests, 1, "served from the cache");
  clock.now = 999;
  await gate.current();
  assert.equal(host.statusRequests, 1);
  clock.now = 1000;
  await gate.current();
  assert.equal(host.statusRequests, 2, "expired");
  await gate.current({ maxAgeMs: 0 });
  assert.equal(host.statusRequests, 3, "an entrance always asks");
  await gate.current({ fresh: true });
  assert.equal(host.statusRequests, 3, "a drain right after it may reuse the answer");
  clock.now += 1000;
  await gate.current({ fresh: true });
  assert.equal(host.statusRequests, 4, "but not an older one");

  host.state = "invalid";
  assert.deepEqual(await gate.current({ maxAgeMs: 0 }), { ok: false, code: "policy_invalid" });
  assert.equal(gate.problem(), "policy_invalid");
  host.state = "ready";
  assert.equal((await gate.current()).ok, true, "a failure is not cached");
  assert.equal(gate.problem(), null);
});

test("only the state ready gives a decision, whatever else the answer contains", async () => {
  const gate = createPolicyGate({ send: async (message) => ({ ok: true, type: "policy_result", requestId: message.requestId, state: "something-new", policy: defaults, etag: "e" }) });
  assert.equal((await gate.current()).ok, false);
});

test("a check started before a failure was reported cannot restore the old policy", async () => {
  const host = fakeHost();
  let release;
  const gate = createPolicyGate({ send: (message) => new Promise((resolve) => (release = () => resolve(clone(host.respond(message))))) });
  const inFlight = gate.current();
  await settle(0);
  gate.reportFailure("policy_invalid");
  release();
  await inFlight;
  assert.equal(gate.problem(), "policy_invalid");
});

test("the decision separates a refused URL from a missing policy, for shared-vector kinds of URL", async () => {
  const gate = gateWith(fakeHost(), { now: 0 });
  const { decide, ok } = await gate.current();
  assert.equal(ok, true);
  for (const url of [AUTH_URL, TOKEN_URL, "https://a.test/#%FF", "https://a.test/%ZZ", "not a url", undefined, null]) assert.equal(decide(url), "deny", String(url));
  for (const url of [NORMAL_URL, SEARCH_URL, B_URL]) assert.equal(decide(url), "allow", url);
});
