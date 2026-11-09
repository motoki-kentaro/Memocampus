// Issue #41: observing, correlating and delivering navigation facts of documents. Chrome is faked;
// every test states its event sequence. Nothing here touches a real vault, policy or Chrome profile.
import { test } from "node:test";
import assert from "node:assert/strict";
import path from "node:path";
import { pathToFileURL } from "node:url";
import {
  BRANCH_TTL_MS,
  MAX_PENDING_COMMITS,
  SOURCELESS_TRANSITIONS,
  TRANSITION_TYPES,
  buildNavigation,
  createNavigationTracker,
  flagsOf,
  occurredAtOf,
  transitionOf,
} from "../../extension/navigation-tracker.js";
import { CAPABILITY_NAVIGATION, checkHostCapabilities } from "../../extension/host-capabilities.js";
import { DEAD_LETTER_KEY, QUEUE_KEY, QueueError, STATS_KEY, carriesNavigation, classifyResponse, createEventQueue } from "../../extension/event-queue.js";
import { describeQueueStatus } from "../../extension/queue-text.js";
import { evaluatePolicy } from "../../extension/policy.js";
import defaults from "../../extension/policy-defaults.json" with { type: "json" };
import { extensionDir } from "./fake-dom.js";
import { confirmPublication, startMcp } from "./real-processes.js";

const T0 = Date.UTC(2026, 10, 6, 12, 0, 0);
const at = (seconds) => T0 + seconds * 1000;
const iso = (seconds) => new Date(at(seconds)).toISOString();
const A = "https://a.test/one";
const B = "https://b.test/two";
const C = "https://c.test/three";
const D = "https://d.test/four";
const NEVER = "https://accounts.google.com/signin/secret-path"; // NeverSave in the default policy
const NO_MCP = "https://mail.google.com/mail/u/0/"; // SaveButNoMcp in the default policy
const settle = (ms = 25) => new Promise((resolve) => setTimeout(resolve, ms));
const clone = (value) => structuredClone(value);
const KEYS = ["sourceVisitId", "kind", "transitionType", "clientRedirect", "serverRedirect", "forwardBack", "fromAddressBar", "newTab", "occurredAt"];

/** chrome.storage.session stand-in (the worker still uses it for image-save failures). */
function fakeSession(data = new Map()) {
  return {
    data,
    writes: [],
    async get(keys) {
      const out = {};
      for (const key of Array.isArray(keys) ? keys : [keys]) if (data.has(key)) out[key] = clone(data.get(key));
      return out;
    },
    async set(items) {
      this.writes.push(JSON.stringify(items));
      for (const [key, value] of Object.entries(items)) data.set(key, clone(value));
    },
    async remove(keys) {
      for (const key of Array.isArray(keys) ? keys : [keys]) data.delete(key);
    },
  };
}

function makeTracker({ incognito = new Set(), slow = new Map() } = {}) {
  let tokens = 0;
  const tracker = createNavigationTracker({
    isNormalTab: async (tabId) => {
      if (slow.has(tabId)) await slow.get(tabId);
      if (tabId === 404) throw new Error("No tab with id: 404");
      return !incognito.has(tabId);
    },
    randomId: () => `token-${++tokens}`,
  });
  return { tracker, incognito };
}

const commit = (tabId, documentId, url, { transitionType = "link", qualifiers = [], seconds = 0, ...rest } = {}) => ({
  tabId,
  frameId: 0,
  documentId,
  url,
  transitionType,
  transitionQualifiers: qualifiers,
  timeStamp: at(seconds),
  ...rest,
});
const history = (tabId, documentId, url, { transitionType = "link", qualifiers = [], seconds = 0 } = {}) => ({ tabId, frameId: 0, documentId, url, transitionType, transitionQualifiers: qualifiers, timeStamp: at(seconds) });
const created = (sourceTabId, tabId, url, { sourceFrameId = 0, seconds = 0 } = {}) => ({ sourceTabId, sourceFrameId, sourceProcessId: 1, tabId, url, timeStamp: at(seconds) });
const done = (tabId, documentId, url = A) => ({ tabId, frameId: 0, documentId, url, timeStamp: 0 });

/** Commit, complete and (unless `record: false`) record one document; resolves to its navigation. */
async function visitDocument(tracker, tabId, documentId, url, visitId, options = {}) {
  await tracker.committed(commit(tabId, documentId, url, options));
  const observed = await tracker.completed(done(tabId, documentId, url));
  if (observed && options.record !== false) await tracker.recorded(tabId, observed.token, visitId);
  return observed?.navigation ?? null;
}

const nav = (fields) => ({ sourceVisitId: null, kind: "document", transitionType: "link", clientRedirect: false, serverRedirect: false, forwardBack: false, fromAddressBar: false, newTab: false, occurredAt: iso(0), ...fields });

// --- Payload form -------------------------------------------------------------------------------

test("a navigation has exactly the host's nine keys, JSON booleans and a null (never absent) source", () => {
  const built = buildNavigation({ kind: "document", transitionType: "link", flags: flagsOf([]), sourceVisitId: undefined, newTab: false, occurredAt: iso(0) });
  assert.deepEqual(Object.keys(built), KEYS);
  assert.equal(built.sourceVisitId, null);
  assert.ok(Object.hasOwn(built, "sourceVisitId"));
  for (const key of ["clientRedirect", "serverRedirect", "forwardBack", "fromAddressBar", "newTab"]) assert.equal(typeof built[key], "boolean");
});

test("transition types are Chrome's eleven values; anything else is sent as unknown and gets no source", () => {
  assert.equal(TRANSITION_TYPES.size, 11);
  for (const value of TRANSITION_TYPES) assert.equal(transitionOf(value), value);
  // auto_toplevel (an outside application), case variants and missing values are not classified.
  for (const raw of ["auto_toplevel", "LINK", "", undefined, null, 3, "unknown"]) assert.equal(transitionOf(raw), "unknown");
  assert.ok(SOURCELESS_TRANSITIONS.has("unknown"));
  for (const value of ["typed", "auto_bookmark", "generated", "start_page", "keyword", "keyword_generated"]) assert.ok(SOURCELESS_TRANSITIONS.has(value), value);
  for (const value of ["link", "form_submit", "reload"]) assert.ok(!SOURCELESS_TRANSITIONS.has(value), value);
});

test("qualifiers map one to one onto the four flags; unknown or malformed ones are ignored", () => {
  assert.deepEqual(flagsOf(["client_redirect", "server_redirect", "forward_back", "from_address_bar"]), { clientRedirect: true, serverRedirect: true, forwardBack: true, fromAddressBar: true });
  assert.deepEqual(flagsOf(["forward_back"]), { clientRedirect: false, serverRedirect: false, forwardBack: true, fromAddressBar: false });
  for (const raw of [undefined, null, "forward_back", [], ["FORWARD_BACK", "back", 1]]) assert.deepEqual(flagsOf(raw), { clientRedirect: false, serverRedirect: false, forwardBack: false, fromAddressBar: false });
});

test("Chrome timestamps become the host's 24-character UTC form; impossible ones give no navigation time", () => {
  assert.equal(occurredAtOf(at(0)), "2026-11-06T12:00:00.000Z");
  assert.equal(occurredAtOf(at(0) + 123.987), "2026-11-06T12:00:00.123Z", "fractions of a millisecond are cut, never rounded up");
  assert.equal(occurredAtOf(Date.UTC(2028, 1, 29, 23, 59, 59, 999)), "2028-02-29T23:59:59.999Z");
  for (const raw of [undefined, null, "1762430400000", Number.NaN, Infinity, -1, 0, 8.64e15 + 1, 253402300800000]) assert.equal(occurredAtOf(raw), null, String(raw));
  for (const value of [at(0), 1, 253402300799999]) assert.match(occurredAtOf(value), /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/);
});

// --- A. Document navigations ----------------------------------------------------------------------

test("a link names the visit of the document shown before; the address bar starts without a source", async () => {
  const { tracker } = makeTracker();
  assert.deepEqual(await visitDocument(tracker, 1, "d1", A, "v1", { transitionType: "typed", qualifiers: ["from_address_bar"] }), nav({ transitionType: "typed", fromAddressBar: true }));
  assert.deepEqual(await visitDocument(tracker, 1, "d2", B, "v2", { seconds: 5 }), nav({ sourceVisitId: "v1", occurredAt: iso(5) }));
  assert.deepEqual(await visitDocument(tracker, 1, "d3", C, "v3", { seconds: 9 }), nav({ sourceVisitId: "v2", occurredAt: iso(9) }));
});

test("revisiting the same URL is a new visit whose source is the previous document, not a merge", async () => {
  const { tracker } = makeTracker();
  await visitDocument(tracker, 1, "d1", A, "v1", { transitionType: "typed" });
  assert.deepEqual(await visitDocument(tracker, 1, "d2", A, "v2"), nav({ sourceVisitId: "v1" }));
  assert.deepEqual(await visitDocument(tracker, 1, "d3", A, "v3"), nav({ sourceVisitId: "v2" }));
});

for (const [name, transitionType, qualifiers, expected] of [
  ["a form submission", "form_submit", [], { transitionType: "form_submit", sourceVisitId: "v1" }],
  ["a reload", "reload", [], { transitionType: "reload", sourceVisitId: "v1" }],
  ["a server redirect", "link", ["server_redirect"], { serverRedirect: true, sourceVisitId: "v1" }],
  ["a client redirect", "link", ["client_redirect"], { clientRedirect: true, sourceVisitId: "v1" }],
  ["back or forward (not told apart)", "link", ["forward_back"], { forwardBack: true, sourceVisitId: "v1" }],
  ["a URL typed in the address bar", "typed", ["from_address_bar"], { transitionType: "typed", fromAddressBar: true }],
  ["a link pasted into the address bar", "link", ["from_address_bar"], { fromAddressBar: true }],
  ["a bookmark", "auto_bookmark", [], { transitionType: "auto_bookmark" }],
  ["an omnibox search", "generated", ["from_address_bar"], { transitionType: "generated", fromAddressBar: true }],
  ["a keyword search", "keyword", [], { transitionType: "keyword" }],
  ["the start page", "start_page", [], { transitionType: "start_page" }],
  ["an unknown transition type", "auto_toplevel", [], { transitionType: "unknown" }],
  ["a missing (null) transition type", null, [], { transitionType: "unknown" }],
]) {
  test(`${name} keeps Chrome's facts as reported and ${expected.sourceVisitId ? "names the previous visit" : "has no source"}`, async () => {
    const { tracker } = makeTracker();
    await visitDocument(tracker, 1, "d1", A, "v1", { transitionType: "typed" });
    assert.deepEqual(await visitDocument(tracker, 1, "d2", B, "v2", { transitionType, qualifiers }), nav(expected));
  });
}

test("back to a document from the back/forward cache: same documentId, forwardBack, the page left as source", async () => {
  const { tracker } = makeTracker();
  await visitDocument(tracker, 1, "d1", A, "v1", { transitionType: "typed" });
  await visitDocument(tracker, 1, "d2", B, "v2", { seconds: 1 });
  // Restored: Chrome commits d1 again. The same commit reported twice changes nothing.
  await tracker.committed(commit(1, "d1", A, { qualifiers: ["forward_back"], seconds: 2 }));
  await tracker.committed(commit(1, "d1", A, { qualifiers: ["forward_back"], seconds: 3 }));
  const restored = await tracker.completed(done(1, "d1", A));
  assert.deepEqual(restored.navigation, nav({ sourceVisitId: "v2", forwardBack: true, occurredAt: iso(2) }));
  assert.equal(await tracker.completed(done(1, "d1", A)), null, "a repeated onCompleted carries no second navigation");
  // If the restore fires no onCompleted (no visit), the next document has no source.
  await tracker.committed(commit(1, "d2", B, { qualifiers: ["forward_back"], seconds: 4 }));
  assert.deepEqual(await visitDocument(tracker, 1, "d4", C, "v4", { seconds: 5 }), nav({ occurredAt: iso(5) }));
});

test("a previous document that was not recorded (NeverSave, policy failure, incognito check) is never a source", async () => {
  const { tracker } = makeTracker();
  await visitDocument(tracker, 1, "d1", A, "v1", { transitionType: "typed" });
  await visitDocument(tracker, 1, "d2", NEVER, null, { record: false });
  assert.deepEqual(await visitDocument(tracker, 1, "d3", B, "v3"), nav(), "not v1: the page shown before was d2");
});

test("a document that never finished loading (H8) breaks the chain: the next one has no source", async () => {
  const { tracker } = makeTracker();
  await visitDocument(tracker, 1, "d1", A, "v1", { transitionType: "typed" });
  await tracker.committed(commit(1, "d2", B));
  assert.deepEqual(await visitDocument(tracker, 1, "d3", C, "v3"), nav());
  // Reported late, d2 still gets exactly its own facts: it followed d1.
  assert.equal((await tracker.completed(done(1, "d2", B))).navigation.sourceVisitId, "v1", "pending commits stay findable by document");
});

test("a visit recorded after a newer navigation started does not become the newer one's source", async () => {
  const { tracker } = makeTracker();
  await tracker.committed(commit(1, "d1", A, { transitionType: "typed" }));
  const first = await tracker.completed(done(1, "d1", A));
  await tracker.committed(commit(1, "d2", B));
  await tracker.recorded(1, first.token, "v1");
  const second = await tracker.completed(done(1, "d2", B));
  assert.equal(second.navigation.sourceVisitId, null);
});

test("a late recorded visit never becomes the head again once a newer document is shown", async () => {
  const { tracker } = makeTracker();
  await tracker.committed(commit(1, "d1", A, { transitionType: "typed" }));
  const first = await tracker.completed(done(1, "d1", A));
  await tracker.committed(commit(1, "d2", B));
  await tracker.recorded(1, first.token, "v1"); // queued only now
  await tracker.completed(done(1, "d2", B)); // d2 itself is not recorded (e.g. NeverSave)
  assert.equal((await visitDocument(tracker, 1, "d3", C, "v3")).sourceVisitId, null, "not v1: d2 was shown in between");
});

test("a commit without a usable time or document id gives a visit without navigation, never a made-up one", async () => {
  const { tracker } = makeTracker();
  await tracker.committed(commit(1, "d1", A, { timeStamp: Number.NaN }));
  assert.equal(await tracker.completed(done(1, "d1", A)), null);
  await tracker.committed({ ...commit(1, "d2", B), documentId: undefined });
  assert.equal(await tracker.completed(done(1, "d2", B)), null);
  assert.equal(await tracker.completed(done(1, "never-committed", C)), null);
  assert.deepEqual(await visitDocument(tracker, 1, "d3", C, "v3"), nav(), "and the next one has no source");
});

// --- B. New tabs ----------------------------------------------------------------------------------

async function openerTab(tracker) {
  await visitDocument(tracker, 1, "d1", A, "v1", { transitionType: "typed" });
}

test("a middle or Ctrl+click (same Chrome events): branch first, then the new tab's commit", async () => {
  const { tracker } = makeTracker();
  await openerTab(tracker);
  await tracker.createdNavigationTarget(created(1, 2, B, { seconds: 1 }));
  assert.deepEqual(await visitDocument(tracker, 2, "n1", B, "v2", { seconds: 2 }), nav({ sourceVisitId: "v1", newTab: true, occurredAt: iso(2) }));
  // Only the first navigation of the new tab is the branch.
  assert.deepEqual(await visitDocument(tracker, 2, "n2", C, "v3", { seconds: 3 }), nav({ sourceVisitId: "v2", occurredAt: iso(3) }));
});

test("the new tab's commit may arrive before the branch; it takes it while still waiting to complete", async () => {
  const { tracker } = makeTracker();
  await openerTab(tracker);
  await tracker.committed(commit(2, "n1", B, { seconds: 2 }));
  await tracker.createdNavigationTarget(created(1, 2, B, { seconds: 1 }));
  const observed = await tracker.completed(done(2, "n1", B));
  assert.deepEqual(observed.navigation, nav({ sourceVisitId: "v1", newTab: true, occurredAt: iso(2) }));
});

test("a branch that arrives after its navigation completed is dropped, not applied to another one", async () => {
  const { tracker } = makeTracker();
  await openerTab(tracker);
  assert.deepEqual(await visitDocument(tracker, 2, "n1", B, "v2", { seconds: 2 }), nav({ occurredAt: iso(2) }));
  await tracker.createdNavigationTarget(created(1, 2, B, { seconds: 1 }));
  assert.deepEqual(await visitDocument(tracker, 2, "n2", B, "v3", { seconds: 3 }), nav({ sourceVisitId: "v2", occurredAt: iso(3) }));
});

test("an about:blank document before the target keeps the branch for the first http(s) commit", async () => {
  const { tracker } = makeTracker();
  await openerTab(tracker);
  await tracker.createdNavigationTarget(created(1, 2, B, { seconds: 1 }));
  await tracker.committed(commit(2, "blank", "about:blank", { seconds: 1 }));
  assert.deepEqual(await visitDocument(tracker, 2, "n1", B, "v2", { seconds: 2 }), nav({ sourceVisitId: "v1", newTab: true, occurredAt: iso(2) }));
});

test("about:blank first, then the commit, then the branch: still the new tab's first navigation", async () => {
  const { tracker } = makeTracker();
  await openerTab(tracker);
  await tracker.committed(commit(2, "blank", "about:blank", { seconds: 1 }));
  await tracker.committed(commit(2, "n1", B, { seconds: 2 }));
  await tracker.createdNavigationTarget(created(1, 2, B, { seconds: 1 }));
  assert.deepEqual((await tracker.completed(done(2, "n1", B))).navigation, nav({ sourceVisitId: "v1", newTab: true, occurredAt: iso(2) }));
});

test("a link opened from an iframe is a new tab without a source (H5)", async () => {
  const { tracker } = makeTracker();
  await openerTab(tracker);
  await tracker.createdNavigationTarget(created(1, 2, B, { sourceFrameId: 7 }));
  assert.deepEqual(await visitDocument(tracker, 2, "n1", B, "v2"), nav({ newTab: true }));
});

test("an opener the tracker does not know, or whose page was not recorded, gives a new tab without a source", async () => {
  const { tracker, incognito } = makeTracker();
  await tracker.createdNavigationTarget(created(5, 2, B));
  assert.deepEqual(await visitDocument(tracker, 2, "n1", B, "v2"), nav({ newTab: true }));
  await visitDocument(tracker, 1, "d1", NEVER, null, { record: false });
  await tracker.createdNavigationTarget(created(1, 3, C));
  assert.deepEqual(await visitDocument(tracker, 3, "n3", C, "v3"), nav({ newTab: true }));
  // An incognito opener has no state at all.
  incognito.add(8);
  await visitDocument(tracker, 8, "i1", A, "secret");
  await tracker.createdNavigationTarget(created(8, 4, D));
  assert.deepEqual(await visitDocument(tracker, 4, "n4", D, "v4"), nav({ newTab: true }));
});

test("a branch is applied only to a navigation it can be matched with", async () => {
  // Another URL without a redirect: not the branch's navigation.
  let { tracker } = makeTracker();
  await openerTab(tracker);
  await tracker.createdNavigationTarget(created(1, 2, B));
  assert.deepEqual(await visitDocument(tracker, 2, "n1", C, "v2"), nav());
  // A server redirect from the branch's URL: the first commit carries the final URL.
  ({ tracker } = makeTracker());
  await openerTab(tracker);
  await tracker.createdNavigationTarget(created(1, 2, B));
  assert.deepEqual(await visitDocument(tracker, 2, "n1", C, "v2", { qualifiers: ["server_redirect"] }), nav({ sourceVisitId: "v1", newTab: true, serverRedirect: true }));
  // The user typed something else into the new tab first.
  ({ tracker } = makeTracker());
  await openerTab(tracker);
  await tracker.createdNavigationTarget(created(1, 2, B));
  assert.deepEqual(await visitDocument(tracker, 2, "n1", B, "v2", { transitionType: "typed", qualifiers: ["from_address_bar"] }), nav({ transitionType: "typed", fromAddressBar: true }));
  // Too late: the branch has expired.
  ({ tracker } = makeTracker());
  await openerTab(tracker);
  await tracker.createdNavigationTarget(created(1, 2, B, { seconds: 0 }));
  const late = BRANCH_TTL_MS / 1000 + 1;
  assert.deepEqual(await visitDocument(tracker, 2, "n1", B, "v2", { seconds: late }), nav({ occurredAt: iso(late) }));
});

test("several tabs opened and navigating at once each get their own opener", async () => {
  const { tracker } = makeTracker();
  await openerTab(tracker);
  await visitDocument(tracker, 10, "x1", D, "v10", { transitionType: "typed" });
  // Interleaved, not awaited one by one.
  await Promise.all([
    tracker.createdNavigationTarget(created(1, 2, B, { seconds: 1 })),
    tracker.createdNavigationTarget(created(10, 3, C, { seconds: 1 })),
    tracker.committed(commit(3, "n3", C, { seconds: 2 })),
    tracker.createdNavigationTarget(created(1, 4, D, { seconds: 1 })),
    tracker.committed(commit(2, "n2", B, { seconds: 2 })),
    tracker.committed(commit(4, "n4", D, { seconds: 2 })),
  ]);
  const [two, three, four] = await Promise.all([tracker.completed(done(2, "n2", B)), tracker.completed(done(3, "n3", C)), tracker.completed(done(4, "n4", D))]);
  assert.deepEqual([two.navigation.sourceVisitId, three.navigation.sourceVisitId, four.navigation.sourceVisitId], ["v1", "v10", "v1"]);
  assert.ok([two, three, four].every((observed) => observed.navigation.newTab === true));
});

// --- C. SPA (history updates) ---------------------------------------------------------------------

// --- F. Worker restarts and restored state --------------------------------------------------------

test("events of one tab keep their order while another tab is waiting", async () => {
  let release;
  const slow = new Map([[1, new Promise((resolve) => (release = resolve))]]);
  const { tracker } = makeTracker({ slow });
  const results = [];
  const committed = tracker.committed(commit(1, "d1", A)).then(() => results.push("tab1 commit"));
  const completed = tracker.completed(done(1, "d1", A)).then((observed) => results.push(observed ? "tab1 completed with navigation" : "tab1 lost"));
  await visitDocument(tracker, 2, "e1", B, "v2", { transitionType: "typed" });
  results.push("tab2 done");
  release();
  await Promise.all([committed, completed]);
  assert.deepEqual(results, ["tab2 done", "tab1 commit", "tab1 completed with navigation"]);
});

// --- Host capabilities ----------------------------------------------------------------------------

test("the host's capabilities: navigation_v1 only when named; an old host is told apart from a stopped one", async () => {
  const id = () => "r1";
  const answer = (response) => checkHostCapabilities({ send: async () => response, createId: id });
  assert.deepEqual(await answer({ ok: true, type: "host_capabilities_result", requestId: "r1", capabilities: [CAPABILITY_NAVIGATION] }), { ok: true, navigation: true, vaultItems: false, visitCalendar: false });
  assert.deepEqual(await answer({ ok: true, type: "host_capabilities_result", requestId: "r1", capabilities: [CAPABILITY_NAVIGATION, "vault_items_v1"] }), { ok: true, navigation: true, vaultItems: true, visitCalendar: false });
  assert.deepEqual(await answer({ ok: true, type: "host_capabilities_result", requestId: "r1", capabilities: [CAPABILITY_NAVIGATION, "vault_items_v1", "visit_calendar_v1"] }), { ok: true, navigation: true, vaultItems: true, visitCalendar: true });
  assert.deepEqual(await answer({ ok: true, type: "host_capabilities_result", requestId: "r1", capabilities: ["visit_calendar_v1"] }), { ok: true, navigation: false, vaultItems: false, visitCalendar: true });
  assert.deepEqual(await answer({ ok: true, type: "host_capabilities_result", requestId: "r1", capabilities: ["something_else"] }), { ok: true, navigation: false, vaultItems: false, visitCalendar: false });
  assert.deepEqual(await answer({ ok: false, eventId: null, error: "unknown_type" }), { ok: true, navigation: false, vaultItems: false, visitCalendar: false }, "a host from before #41");
  for (const response of [
    { ok: true, type: "host_capabilities_result", requestId: "other", capabilities: [CAPABILITY_NAVIGATION] },
    { ok: true, type: "policy_result", requestId: "r1", capabilities: [CAPABILITY_NAVIGATION] },
    { ok: true, type: "host_capabilities_result", requestId: "r1", capabilities: CAPABILITY_NAVIGATION },
    { ok: true, eventId: undefined },
    { ok: false, eventId: "x", error: "unknown_type" },
    undefined,
  ]) {
    assert.deepEqual(await answer(response), { ok: false, code: "invalid_response" }, JSON.stringify(response));
  }
  assert.deepEqual(await checkHostCapabilities({ send: async () => Promise.reject(new QueueError("host_unavailable")), createId: id }), { ok: false, code: "host_unavailable" });
  assert.deepEqual(await checkHostCapabilities({ send: () => new Promise(() => {}), createId: id, timeoutMs: 5 }), { ok: false, code: "timeout" });
  const sent = [];
  await checkHostCapabilities({ send: async (message) => sent.push(message), createId: id });
  assert.deepEqual(sent, [{ type: "host_capabilities", requestId: "r1" }]);
});

// --- D. Queue -------------------------------------------------------------------------------------

function fakeStorage() {
  const data = new Map();
  return {
    data,
    async get(keys) {
      const out = {};
      for (const key of Array.isArray(keys) ? keys : [keys]) if (data.has(key)) out[key] = clone(data.get(key));
      return out;
    },
    async set(items) {
      for (const [key, value] of Object.entries(items)) data.set(key, clone(value));
    },
  };
}

const allowAll = { current: async () => ({ ok: true, version: "v" }), evaluate: async (urls) => ({ ok: true, version: "v", decisions: new Map(urls.map((url) => [url, "allow"])) }) };
const plainVisit = (id, url = A) => ({ type: "visit", id, url, title: "t", incognito: false, visitedAt: iso(0) });
const navVisit = (id, url = A, navigation = nav({ sourceVisitId: null })) => ({ ...plainVisit(id, url), navigation });

/** A host like the Rust one: `mode` "modern" stores navigations, "old" ignores them. */
function queueHost({ mode = "modern" } = {}) {
  const host = {
    mode,
    sent: [],
    stored: new Map(),
    capabilityChecks: 0,
    down: false,
    loseAckOnce: new Set(),
    navigationAnswer: null,
    async capabilities() {
      host.capabilityChecks += 1;
      if (host.down) return { ok: false, code: "host_unavailable" };
      return { ok: true, navigation: host.mode === "modern" };
    },
    async send(event) {
      if (host.down) throw new QueueError("host_unavailable");
      host.sent.push(clone(event));
      if (!host.stored.has(event.id)) host.stored.set(event.id, clone(event));
      if (host.loseAckOnce.delete(event.id)) throw new QueueError("host_unavailable");
      if (host.mode === "old" || !Object.hasOwn(event, "navigation")) return { ok: true, eventId: event.id };
      return { ok: true, eventId: event.id, navigation: host.navigationAnswer?.(event) ?? "stored" };
    },
  };
  return host;
}

function makeQueue(host, { policy = allowAll } = {}) {
  const storage = fakeStorage();
  const queue = createEventQueue({ storage, send: host.send, policy, capabilities: host.capabilities, now: () => new Date(at(0)) });
  return { queue, storage, pending: () => storage.data.get(QUEUE_KEY) ?? [], dead: () => storage.data.get(DEAD_LETTER_KEY) ?? [], stats: () => storage.data.get(STATS_KEY) ?? {} };
}

test("plain visits are delivered as before, without asking for capabilities", async () => {
  const host = queueHost();
  const { queue, pending } = makeQueue(host);
  await queue.enqueueAll([plainVisit("p1"), plainVisit("p2")]);
  assert.equal(await queue.flush(), null);
  assert.deepEqual(pending(), []);
  assert.equal(host.capabilityChecks, 0);
  assert.ok(host.sent.every((event) => !Object.hasOwn(event, "navigation")));
});

test("a navigation_v1 host is asked once per delivery run and every navigation is acknowledged as stored", async () => {
  const host = queueHost();
  const { queue, pending, stats } = makeQueue(host);
  await queue.enqueueAll([navVisit("n1"), plainVisit("p1"), navVisit("n2", B, nav({ sourceVisitId: "n1" }))]);
  assert.equal(await queue.flush(), null);
  assert.equal(host.capabilityChecks, 1);
  assert.deepEqual(host.sent.map((event) => [event.id, event.navigation?.sourceVisitId]), [["n1", null], ["p1", undefined], ["n2", "n1"]], "in queue order: a source before its target");
  assert.deepEqual(pending(), []);
  assert.deepEqual(stats(), {});
  await queue.enqueue(navVisit("n3"));
  await queue.flush();
  assert.equal(host.capabilityChecks, 2, "a new delivery run asks again (the host may have been replaced)");
});

test("an old host never gets a navigation: the visit is still stored and the loss is counted and shown", async () => {
  const host = queueHost({ mode: "old" });
  const { queue, pending, dead } = makeQueue(host);
  await queue.enqueueAll([navVisit("n1"), plainVisit("p1"), navVisit("n2")]);
  assert.equal(await queue.flush(), null, "visits keep flowing");
  assert.ok(host.sent.every((event) => !Object.hasOwn(event, "navigation")));
  assert.deepEqual([...host.stored.keys()], ["n1", "p1", "n2"]);
  assert.deepEqual([pending(), dead()], [[], []]);
  const status = await queue.status();
  assert.deepEqual([status.navigationNotStored, status.navigationInvalid], [2, 0]);
  assert.match(describeQueueStatus({ ok: true, ...status }), /未対応のため遷移情報を保存できなかった訪問 2件（訪問は保存済み）/);
});

test("without a capability answer nothing is sent; a stopped host and a timeout are reported apart", async () => {
  for (const code of ["host_unavailable", "timeout", "invalid_response"]) {
    const host = queueHost();
    host.capabilities = async () => ({ ok: false, code });
    const { queue, pending } = makeQueue(host);
    await queue.enqueue(navVisit("n1"));
    assert.equal(await queue.flush(), code);
    assert.deepEqual(host.sent, []);
    assert.equal(pending().length, 1, "kept for a later run");
  }
  // A queue built without a capability check treats the host as unable to store navigations.
  const host = queueHost();
  const storage = fakeStorage();
  const queue = createEventQueue({ storage, send: host.send, policy: allowAll });
  await queue.enqueue(navVisit("n1"));
  await queue.flush();
  assert.ok(!Object.hasOwn(host.sent[0], "navigation"));
  assert.equal((await queue.status()).navigationNotStored, 1);
});

test("an ACK without navigation is not a stored navigation: counted, and the host is asked again", async () => {
  const host = queueHost();
  const send = host.send;
  // A host swapped for an old one between the capability answer and this event.
  host.send = async (event) => {
    const answer = await send(event);
    if (event.id === "n1") delete answer.navigation;
    return answer;
  };
  const { queue, pending } = makeQueue(host);
  await queue.enqueueAll([navVisit("n1"), navVisit("n2")]);
  assert.equal(await queue.flush(), null);
  const status = await queue.status();
  assert.deepEqual([status.navigationNotStored, status.navigationInvalid], [1, 0]);
  assert.equal(host.capabilityChecks, 2, "asked again before n2");
  assert.equal(host.sent.filter((event) => event.id === "n1").length, 1, "not resent: the visit is stored and a resend could not add it");
  assert.deepEqual(pending(), []);
});

test("navigation: invalid is not a success, is never resent, and does not hold back what follows", async () => {
  const host = queueHost();
  host.navigationAnswer = (event) => (event.id === "bad" ? "invalid" : "stored");
  const { queue, pending, dead } = makeQueue(host);
  await queue.enqueueAll([navVisit("bad"), navVisit("n2"), plainVisit("p3")]);
  assert.equal(await queue.flush(), null);
  assert.deepEqual(host.sent.map((event) => event.id), ["bad", "n2", "p3"]);
  await queue.flush();
  assert.equal(host.sent.filter((event) => event.id === "bad").length, 1);
  assert.deepEqual([pending(), dead()], [[], []], "no copy of the URL is kept for it");
  const status = await queue.status();
  assert.deepEqual([status.navigationInvalid, status.navigationNotStored], [1, 0]);
  assert.match(describeQueueStatus({ ok: true, ...status }), /遷移情報がHostに拒否された訪問 1件/);
});

test("an ACK naming any other navigation outcome is an invalid ACK and the event stays queued", async () => {
  const host = queueHost();
  host.navigationAnswer = () => "maybe";
  const { queue, pending } = makeQueue(host);
  await queue.enqueue(navVisit("n1"));
  assert.equal(await queue.flush(), "invalid_ack");
  assert.equal(pending().length, 1);
  for (const response of [{ ok: true, eventId: "n1", navigation: null }, { ok: true, eventId: "n1", navigation: true }, { ok: true, eventId: "n1", navigation: "STORED" }]) {
    assert.deepEqual(classifyResponse(navVisit("n1"), response), { kind: "temporary", code: "invalid_ack" }, JSON.stringify(response));
  }
  assert.deepEqual(classifyResponse(navVisit("n1"), { ok: true, eventId: "n1", navigation: "stored" }), { kind: "delivered" });
  assert.deepEqual(classifyResponse(navVisit("n1"), { ok: true, eventId: "other", navigation: "stored" }), { kind: "temporary", code: "invalid_ack" });
  assert.deepEqual(classifyResponse(plainVisit("p1"), { ok: true, eventId: "p1" }), { kind: "delivered" }, "unchanged for plain visits");
  assert.ok(carriesNavigation(navVisit("x", A, null)), "a navigation key is a navigation, whatever its value");
  assert.ok(!carriesNavigation({ type: "tab_snapshot", navigation: {} }));
});

test("a disconnected host and a lost ACK lead to resends of the very same event", async () => {
  const host = queueHost();
  const { queue, pending } = makeQueue(host);
  const event = navVisit("n1", B, nav({ sourceVisitId: "v0", forwardBack: true }));
  await queue.enqueue(event);
  host.down = true;
  assert.equal(await queue.flush(), "host_unavailable");
  host.down = false;
  host.loseAckOnce.add("n1");
  assert.equal(await queue.flush(), "host_unavailable");
  assert.equal(await queue.flush(), null);
  assert.equal(host.sent.length, 2);
  assert.deepEqual(host.sent[0], event);
  assert.deepEqual(host.sent[1], event, "same id, same source, same facts");
  assert.deepEqual(pending(), []);
});

test("the policy still decides: dropped, refused or swept navigation visits leave no copy", async () => {
  // Refused on arrival.
  const host = queueHost();
  host.send = async (event) => ({ ok: true, eventId: event.id, dropped: "policy" });
  let made = makeQueue(host);
  await made.queue.enqueue(navVisit("n1"));
  assert.equal(await made.queue.flush(), null);
  assert.deepEqual([made.pending(), made.dead(), made.stats()], [[], [], {}]);
  // A new policy version refuses a queued one: swept before anything is sent.
  const deny = { current: async () => ({ ok: true, version: "v2" }), evaluate: async (urls) => ({ ok: true, version: "v2", decisions: new Map(urls.map((url) => [url, url === NEVER ? "deny" : "allow"])) }) };
  const sweeping = queueHost();
  made = makeQueue(sweeping, { policy: deny });
  await made.queue.enqueueAll([navVisit("n1", NEVER), navVisit("n2", B)]);
  assert.equal(await made.queue.flush(), null);
  assert.deepEqual(sweeping.sent.map((event) => event.id), ["n2"]);
  assert.ok(!JSON.stringify([...made.storage.data]).includes(NEVER));
  // An incognito visit with a navigation is discarded without asking anything.
  const incognitoHost = queueHost();
  made = makeQueue(incognitoHost);
  await made.queue.enqueue({ ...navVisit("i1"), incognito: true });
  assert.equal(await made.queue.flush(), null);
  assert.deepEqual([incognitoHost.sent, incognitoHost.capabilityChecks, made.dead()], [[], 0, []]);
});

test("a navigation visit the host rejects permanently is quarantined whole, as any event", async () => {
  const host = queueHost();
  host.send = async (event) => ({ ok: false, eventId: event.id, error: "invalid_event" });
  const { queue, dead } = makeQueue(host);
  const event = navVisit("n1");
  await queue.enqueue(event);
  await queue.flush();
  assert.deepEqual(dead().map((entry) => [entry.errorCode, entry.event]), [["invalid_event", event]]);
});

// --- Service worker with a fake chrome and a fake host ---------------------------------------------

function swHost({ capability = "modern" } = {}) {
  const host = {
    capability,
    state: "ready",
    policy: clone(defaults),
    down: false,
    received: [],
    batchUrls: [],
    respond(message) {
      if (message.type === "policy_status") {
        if (host.state !== "ready") return { ok: true, type: "policy_result", requestId: message.requestId, state: host.state };
        return { ok: true, type: "policy_result", requestId: message.requestId, state: "ready", revision: host.policy.revision, etag: `rev-${host.policy.revision}`, schemaVersion: 1, engineVersion: 1 };
      }
      if (message.type === "policy_evaluate_batch") {
        host.batchUrls.push(...message.items.map((item) => item.url));
        const base = { type: "policy_evaluate_batch_result", requestId: message.requestId };
        if (host.state !== "ready") return { ...base, ok: false, error: "policy_invalid" };
        const names = { none: "allow", saveButNoMcp: "save_but_no_mcp", neverSave: "never_save" };
        return { ...base, ok: true, etag: `rev-${host.policy.revision}`, revision: host.policy.revision, items: message.items.map((item) => ({ id: item.id, decision: names[evaluatePolicy(host.policy, item.url)] })) };
      }
      if (message.type === "host_capabilities") {
        if (host.capability === "modern") return { ok: true, type: "host_capabilities_result", requestId: message.requestId, capabilities: [CAPABILITY_NAVIGATION] };
        return { ok: false, eventId: null, error: "unknown_type" };
      }
      host.received.push(message);
      if (host.capability === "modern" && Object.hasOwn(message, "navigation")) return { ok: true, eventId: message.id, navigation: "stored" };
      return { ok: true, eventId: message.id };
    },
  };
  return host;
}

async function startWorker({ host = swHost(), session = new Map(), local = new Map(), tabs = {}, frames = {}, transport = null, readPage = null } = {}) {
  const listeners = {};
  const on = (name) => ({ addListener: (fn) => ((listeners[name] ??= []).push(fn)) });
  const badge = { text: "", title: "" };
  const scripts = [];
  const sessionStore = fakeSession(session);
  const runtime = {
    onStartup: on("startup"),
    onInstalled: on("installed"),
    onMessage: on("message"),
    sendMessage: async () => {},
    connectNative() {},
    lastError: undefined,
    sendNativeMessage(_name, message, reply) {
      if (transport) {
        transport(message).then(reply, () => {
          runtime.lastError = { message: "Native host has exited." };
          reply(undefined);
          runtime.lastError = undefined;
        });
        return;
      }
      if (host.down || (host.capabilityDown && message.type === "host_capabilities")) {
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
    webNavigation: {
      onCompleted: on("completed"),
      onCommitted: on("committed"),
      onCreatedNavigationTarget: on("createdTarget"),
      onHistoryStateUpdated: on("historyState"),
    },
    contextMenus: { create() {}, onClicked: on("menuClick") },
    notifications: { create: async () => {} },
    action: { setBadgeText: async ({ text }) => void (badge.text = text), setTitle: async ({ title }) => void (badge.title = title) },
    sidePanel: { setPanelBehavior: async () => {} },
    scripting: {
      async executeScript(options) {
        scripts.push(options);
        return readPage ? readPage(options) : [];
      },
    },
    tabs: {
      onRemoved: on("tabRemoved"),
      onReplaced: on("tabReplaced"),
      get: async (id) => {
        if (!(id in tabs)) throw new Error(`No tab with id: ${id}`);
        return tabs[id];
      },
      query: async () => Object.values(tabs),
    },
    storage: { local: fakeStorage(), session: sessionStore },
  };
  globalThis.chrome.storage.local.data = local;
  // fakeStorage keeps its own map; use the given one so a restart keeps the queue.
  const localStore = globalThis.chrome.storage.local;
  localStore.get = async (keys) => {
    const out = {};
    for (const key of Array.isArray(keys) ? keys : [keys]) if (local.has(key)) out[key] = clone(local.get(key));
    return out;
  };
  localStore.set = async (items) => {
    for (const [key, value] of Object.entries(items)) local.set(key, clone(value));
  };
  await import(`${pathToFileURL(path.join(extensionDir, "service-worker.js")).href}?navigation=${Math.random()}`);
  const fire = async (name, ...args) => {
    await Promise.all((listeners[name] ?? []).map((fn) => fn(...args)));
    await settle();
  };
  const tabOf = (id, incognito = false) => (tabs[id] ??= { id, windowId: 1, index: id, url: "", title: `Title ${id}`, pinned: false, incognito });
  return {
    host,
    session,
    local,
    sessionStore,
    badge,
    scripts,
    tabs,
    frames,
    visits: () => host.received.filter((event) => event.type === "visit"),
    async document(tabId, documentId, url, options = {}) {
      tabOf(tabId, options.incognito);
      tabs[tabId].url = url;
      frames[tabId] = documentId;
      await fire("committed", commit(tabId, documentId, url, options));
      await fire("completed", { tabId, frameId: 0, documentId, url, timeStamp: at(options.seconds ?? 0) + 500 });
    },
    commitOnly: (tabId, documentId, url, options = {}) => {
      tabOf(tabId, options.incognito);
      frames[tabId] = documentId;
      return fire("committed", commit(tabId, documentId, url, options));
    },
    complete: (tabId, documentId, url) => fire("completed", { tabId, frameId: 0, documentId, url, timeStamp: at(1) }),
    history: (tabId, documentId, url, options = {}) => fire("historyState", history(tabId, documentId, url, options)),
    created: (sourceTabId, tabId, url, options = {}) => {
      tabOf(tabId);
      return fire("createdTarget", created(sourceTabId, tabId, url, options));
    },
    removed: (tabId) => fire("tabRemoved", tabId, { windowId: 1, isWindowClosing: false }),
    message: (message) => new Promise((resolve) => listeners.message.forEach((fn) => fn(message, {}, resolve))),
  };
}


test("with a host from before #41 visits are still recorded, without navigation, and the loss is shown", async () => {
  const worker = await startWorker({ host: swHost({ capability: "old" }) });
  await worker.document(1, "d1", A, { transitionType: "typed" });
  await worker.document(1, "d2", B);
  assert.deepEqual(worker.visits().map((event) => [event.url, Object.hasOwn(event, "navigation")]), [[A, false], [B, false]]);
  const status = await worker.message({ type: "queue-status" });
  assert.equal(status.navigationNotStored, 2);
  assert.match(worker.badge.title, /遷移情報を保存できなかった訪問 2件/);
  assert.equal(worker.badge.text, "!");
});

test("a host that stops before the capability answer keeps the visit queued with its navigation", async () => {
  const worker = await startWorker();
  worker.host.capabilityDown = true;
  await worker.document(1, "d1", A, { transitionType: "typed" });
  assert.deepEqual(worker.visits(), []);
  assert.equal((await worker.message({ type: "queue-status" })).blockedBy, "host_unavailable");
  const [queued] = worker.local.get(QUEUE_KEY);
  assert.deepEqual(queued.navigation, nav({ transitionType: "typed" }));
  worker.host.capabilityDown = false;
  await worker.message({ type: "flush-queue" });
  assert.deepEqual(worker.visits()[0], queued, "sent exactly as queued");
});

// --- Against the real Native Host and a synthetic vault (opt-in) ------------------------------------
//
// MEMOCAMPUS_TEST_HOST=<path to a built memocampus-native-host> runs the service worker against that
// host, one process per message as Chrome does, in a temporary MEMOCAMPUS_APP_DIR (its own policy and
// vault). Without it the test is skipped.

const HOST_BIN = process.env.MEMOCAMPUS_TEST_HOST;

test("end to end with the real host: navigations stored in a fresh schema 7 vault, privacy kept", { skip: HOST_BIN ? false : "set MEMOCAMPUS_TEST_HOST to run" }, async () => {
  const { spawn } = await import("node:child_process");
  const { mkdtempSync, rmSync } = await import("node:fs");
  const os = await import("node:os");
  const { DatabaseSync } = await import("node:sqlite");
  const appDir = mkdtempSync(path.join(os.tmpdir(), "memocampus-nav-e2e-"));
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
    assert.equal((await call({ type: "policy_initialize", requestId: "p" })).ok, true);
    assert.equal((await call({ type: "vault_create", requestId: "c", useDefault: true })).ok, true);
    assert.deepEqual((await call({ type: "host_capabilities", requestId: "h" })).capabilities, [CAPABILITY_NAVIGATION, "vault_items_v1", "visit_calendar_v1"]);
    const worker = await startWorker({ transport: call });
    await worker.document(1, "d1", A, { transitionType: "typed", qualifiers: ["from_address_bar"] });
    await worker.document(1, "d2", B, { seconds: 1 });
    await worker.document(1, "d3", NEVER, { seconds: 2 });
    await worker.document(1, "d4", C, { seconds: 3 });
    await worker.created(1, 2, D, { seconds: 4 });
    await worker.document(2, "n1", D, { seconds: 5 });
    await worker.history(2, "n1", NO_MCP, { seconds: 6 }); // never a visit
    await worker.document(1, "d5", A, { qualifiers: ["forward_back"], seconds: 7 });
    await settle(500);
    await worker.message({ type: "flush-queue" });
    assert.deepEqual(worker.local.get(QUEUE_KEY), [], "everything acknowledged");
    const status = await worker.message({ type: "queue-status" });
    assert.deepEqual([status.navigationInvalid, status.navigationNotStored, status.deadLetters], [0, 0, 0]);
    const db = new DatabaseSync(path.join(appDir, "memocampus.sqlite3"), { readOnly: true });
    try {
      assert.equal(db.prepare("PRAGMA user_version").get().user_version, 7);
      const rows = db
        .prepare(
          `SELECT t.url AS target, s.url AS source, n.kind, n.transition_type, n.from_address_bar, n.new_tab, n.forward_back, n.occurred_at
           FROM navigation_events n JOIN visits t ON t.event_id = n.target_visit_id LEFT JOIN visits s ON s.event_id = n.source_visit_id
           ORDER BY n.occurred_at`,
        )
        .all()
        .map((row) => ({ ...row }));
      assert.deepEqual(rows, [
        { target: A, source: null, kind: "document", transition_type: "typed", from_address_bar: 1, new_tab: 0, forward_back: 0, occurred_at: iso(0) },
        { target: B, source: A, kind: "document", transition_type: "link", from_address_bar: 0, new_tab: 0, forward_back: 0, occurred_at: iso(1) },
        { target: C, source: null, kind: "document", transition_type: "link", from_address_bar: 0, new_tab: 0, forward_back: 0, occurred_at: iso(3) },
        { target: D, source: C, kind: "document", transition_type: "link", from_address_bar: 0, new_tab: 1, forward_back: 0, occurred_at: iso(5) },
        { target: A, source: C, kind: "document", transition_type: "link", from_address_bar: 0, new_tab: 0, forward_back: 1, occurred_at: iso(7) },
      ]);
      assert.equal(db.prepare("SELECT count(*) AS n FROM visits WHERE url LIKE '%accounts.google.com%' OR url LIKE '%mail.google.com%'").get().n, 0);
      assert.deepEqual(db.prepare("PRAGMA foreign_key_check").all(), []);
      assert.equal(db.prepare("PRAGMA integrity_check").get().integrity_check, "ok");
    } finally {
      db.close();
    }
  } finally {
    rmSync(appDir, { recursive: true, force: true });
  }
});

// --- Slice 3 fixes: event orders observed in Chromium 159 ------------------------------------------
//
// I-1: `window.open("about:blank")` then `w.location.href = <http URL>`. Chrome reported the branch
// with url "about:blank", then the about:blank commit and completion, then the http commit (`link`).
// I-2: back and forward report the history entry's original transition (`typed`, `reload`, ...)
// with `forward_back`, and restore the same documentId from the back/forward cache.

async function aboutBlankBranch(tracker, { opener = 1, tabId = 2, sourceFrameId = 0, seconds = 1 } = {}) {
  await tracker.createdNavigationTarget(created(opener, tabId, "about:blank", { sourceFrameId, seconds }));
  await tracker.committed(commit(tabId, `blank-${tabId}`, "about:blank", { seconds }));
  assert.equal(await tracker.completed(done(tabId, `blank-${tabId}`, "about:blank")), null, "about:blank is never a visit");
}

test("I-1: a new tab opened as about:blank and then sent to an http URL is a new tab with its opener as source", async () => {
  const { tracker } = makeTracker();
  await openerTab(tracker);
  await aboutBlankBranch(tracker);
  assert.deepEqual(await visitDocument(tracker, 2, "n1", B, "v2", { seconds: 2 }), nav({ sourceVisitId: "v1", newTab: true, occurredAt: iso(2) }));
  assert.deepEqual(await visitDocument(tracker, 2, "n2", C, "v3", { seconds: 3 }), nav({ sourceVisitId: "v2", occurredAt: iso(3) }), "only the first http navigation");
});

test("I-1: the about:blank commit may come before the branch event", async () => {
  const { tracker } = makeTracker();
  await openerTab(tracker);
  await tracker.committed(commit(2, "blank-2", "about:blank", { seconds: 1 }));
  await tracker.createdNavigationTarget(created(1, 2, "about:blank", { seconds: 1 }));
  assert.deepEqual(await visitDocument(tracker, 2, "n1", B, "v2", { seconds: 2 }), nav({ sourceVisitId: "v1", newTab: true, occurredAt: iso(2) }));
});

test("I-1: without a known source the about:blank tab is still a new tab; from an iframe the source is null", async () => {
  let { tracker } = makeTracker();
  await aboutBlankBranch(tracker, { opener: 5 });
  assert.deepEqual(await visitDocument(tracker, 2, "n1", B, "v2", { seconds: 2 }), nav({ newTab: true, occurredAt: iso(2) }));
  ({ tracker } = makeTracker());
  await openerTab(tracker);
  await aboutBlankBranch(tracker, { sourceFrameId: 4 });
  assert.deepEqual(await visitDocument(tracker, 2, "n1", B, "v2", { seconds: 2 }), nav({ newTab: true, occurredAt: iso(2) }));
});

for (const transitionType of ["typed", "auto_bookmark", "reload", "link", "generated", "keyword", "unknown-from-chrome"]) {
  test(`I-2: ${transitionType} with forward_back names the visit shown before; the transition stays as reported`, async () => {
    const { tracker } = makeTracker();
    await visitDocument(tracker, 1, "d1", A, "v1", { transitionType: "typed", qualifiers: ["from_address_bar"] });
    await visitDocument(tracker, 1, "d2", B, "v2", { seconds: 1 });
    assert.deepEqual(
      await visitDocument(tracker, 1, "d1", A, "v3", { transitionType, qualifiers: ["forward_back"], seconds: 2 }),
      nav({ sourceVisitId: "v2", transitionType: transitionOf(transitionType), forwardBack: true, occurredAt: iso(2) }),
    );
  });
}

test("I-2: forward_back keeps Chrome's other qualifiers, and without it typed, bookmark and address bar stay sourceless", async () => {
  const { tracker } = makeTracker();
  await visitDocument(tracker, 1, "d1", A, "v1", { transitionType: "typed" });
  await visitDocument(tracker, 1, "d2", B, "v2");
  assert.deepEqual(
    await visitDocument(tracker, 1, "d1", A, "v3", { transitionType: "typed", qualifiers: ["forward_back", "from_address_bar"] }),
    nav({ sourceVisitId: "v2", transitionType: "typed", forwardBack: true, fromAddressBar: true }),
  );
  assert.equal((await visitDocument(tracker, 1, "d4", B, "v4", { transitionType: "typed", qualifiers: ["from_address_bar"] })).sourceVisitId, null);
  assert.equal((await visitDocument(tracker, 1, "d5", C, "v5", { transitionType: "auto_bookmark" })).sourceVisitId, null);
  assert.equal((await visitDocument(tracker, 1, "d6", D, "v6", { transitionType: "generated" })).sourceVisitId, null);
});

test("I-2: back or forward has no source when the page shown before was not recorded or is unknown", async () => {
  // Nothing before.
  let { tracker } = makeTracker();
  assert.equal((await visitDocument(tracker, 1, "d1", A, "v1", { transitionType: "typed", qualifiers: ["forward_back"] })).sourceVisitId, null);
  // NeverSave or a policy failure: the visit was never recorded.
  ({ tracker } = makeTracker());
  await visitDocument(tracker, 1, "d1", A, "v1", { transitionType: "typed" });
  await visitDocument(tracker, 1, "d2", NEVER, null, { record: false });
  assert.equal((await visitDocument(tracker, 1, "d1", A, "v3", { transitionType: "typed", qualifiers: ["forward_back"] })).sourceVisitId, null);
  // A page that never completed in between.
  ({ tracker } = makeTracker());
  await visitDocument(tracker, 1, "d1", A, "v1", { transitionType: "typed" });
  await tracker.committed(commit(1, "d2", B));
  assert.equal((await visitDocument(tracker, 1, "d1", A, "v3", { transitionType: "typed", qualifiers: ["forward_back"] })).sourceVisitId, null);
});

test("I-2: back and forward through the back/forward cache (same documentIds) chain new visits, never the old ones", async () => {
  const { tracker } = makeTracker();
  await visitDocument(tracker, 1, "d1", A, "v1", { transitionType: "typed" });
  await visitDocument(tracker, 1, "d2", B, "v2", { seconds: 1 });
  // Back: Chrome restores d1 and reports A's original transition.
  assert.deepEqual(await visitDocument(tracker, 1, "d1", A, "v3", { transitionType: "typed", qualifiers: ["forward_back"], seconds: 2 }), nav({ sourceVisitId: "v2", transitionType: "typed", forwardBack: true, occurredAt: iso(2) }));
  // Forward: d2 again, reported as B's original transition.
  assert.deepEqual(await visitDocument(tracker, 1, "d2", B, "v4", { transitionType: "link", qualifiers: ["forward_back"], seconds: 3 }), nav({ sourceVisitId: "v3", forwardBack: true, occurredAt: iso(3) }), "v3, not v1 of the same document");
  // Back again, then a plain link: the chain continues from the newest visits.
  assert.equal((await visitDocument(tracker, 1, "d1", A, "v5", { transitionType: "typed", qualifiers: ["forward_back"], seconds: 4 })).sourceVisitId, "v4");
  assert.deepEqual(await visitDocument(tracker, 1, "d3", C, "v6", { seconds: 5 }), nav({ sourceVisitId: "v5", occurredAt: iso(5) }));
});

test("I-1 and I-2 in the service worker: the observed Chromium sequences become these visits", async () => {
  const worker = await startWorker();
  await worker.document(1, "d1", A, { transitionType: "typed", qualifiers: ["from_address_bar"] });
  // window.open("about:blank"), then location.href = C
  await worker.created(1, 2, "about:blank", { seconds: 1 });
  await worker.document(2, "blank", "about:blank", { seconds: 1 });
  await worker.document(2, "n1", C, { seconds: 2 });
  // In tab 1: link to B, back (typed + forward_back, same documentId), forward (link + forward_back).
  await worker.document(1, "d2", B, { seconds: 3 });
  await worker.document(1, "d1", A, { transitionType: "typed", qualifiers: ["forward_back"], seconds: 4 });
  await worker.document(1, "d2", B, { qualifiers: ["forward_back"], seconds: 5 });
  const visits = worker.visits();
  assert.deepEqual(visits.map((event) => event.url), [A, C, B, A, B], "no visit for about:blank");
  const [a, c, b, back, forward] = visits;
  assert.deepEqual(c.navigation, nav({ sourceVisitId: a.id, newTab: true, occurredAt: iso(2) }));
  assert.equal(b.navigation.sourceVisitId, a.id);
  assert.deepEqual(back.navigation, nav({ sourceVisitId: b.id, transitionType: "typed", forwardBack: true, occurredAt: iso(4) }));
  assert.deepEqual(forward.navigation, nav({ sourceVisitId: back.id, forwardBack: true, occurredAt: iso(5) }));
  assert.equal(new Set(visits.map((event) => event.id)).size, 5);
});

// --- Independent audit findings (F-1 to F-4) ------------------------------------------------------

const PRIVATE_TITLE = "SYNTHETIC_PRIVATE_TITLE";

test("F-1: a document visit keeps the tab's title only while the tab still shows that URL", async () => {
  const worker = await startWorker();
  await worker.document(1, "d1", A, { transitionType: "typed" });
  assert.equal(worker.visits().at(-1).title, "Title 1", "unchanged when the tab shows the visit's URL");
  // The page moved on (pushState to a NeverSave URL) before the visit of B was built.
  worker.tabs[1] = { ...worker.tabs[1], url: NEVER, title: PRIVATE_TITLE };
  await worker.commitOnly(1, "d2", B);
  await worker.complete(1, "d2", B);
  assert.equal(worker.visits().at(-1).url, B);
  assert.equal(worker.visits().at(-1).title, "");
  assert.ok(!JSON.stringify(worker.host.received).includes(PRIVATE_TITLE));
});

test("F-3: an onCompleted at another URL than its commit carries no navigation; the matching one still does", async () => {
  const { tracker } = makeTracker();
  await visitDocument(tracker, 1, "a", A, "visit-a", { transitionType: "typed" });
  await tracker.committed(commit(1, "b", NO_MCP, { seconds: 1 }));
  assert.equal(await tracker.completed(done(1, "b", C)), null);
  await tracker.committed(commit(1, "c", B, { seconds: 2 }));
  assert.equal((await tracker.completed(done(1, "c", B))).navigation.sourceVisitId, null, "b was not recorded");
});

test("F-4: a sweep that refuses a source event also drops every queued reference to it", async () => {
  const S = "https://s.test/source";
  const T2 = "https://t.test/target";
  const S2 = "https://nomcp.test/kept";
  const deny = { current: async () => ({ ok: true, version: "v2" }), evaluate: async (urls) => ({ ok: true, version: "v2", decisions: new Map(urls.map((url) => [url, url === S ? "deny" : "allow"])) }) };
  const host = queueHost();
  host.down = true;
  const made = makeQueue(host, { policy: deny });
  const source = plainVisit("visit-s", S);
  const target = navVisit("visit-t", T2, nav({ sourceVisitId: "visit-s" }));
  const keptSource = navVisit("visit-k", S2);
  const keptTarget = navVisit("visit-u", T2, nav({ sourceVisitId: "visit-k" }));
  await made.queue.enqueueAll([source, target, keptSource, keptTarget]);
  // A dead letter that names the refused source too.
  made.storage.data.set(DEAD_LETTER_KEY, [{ errorCode: "invalid_event", eventId: "dead-1", failedAt: iso(0), event: navVisit("dead-1", T2, nav({ sourceVisitId: "visit-s" })) }]);
  assert.equal(await made.queue.flush(), "host_unavailable");
  const pending = made.pending();
  assert.deepEqual(pending.map((event) => [event.id, Object.hasOwn(event, "navigation") ? event.navigation.sourceVisitId : "-"]), [["visit-t", null], ["visit-k", null], ["visit-u", "visit-k"]], "same ids, FIFO kept");
  assert.equal(made.dead()[0].event.navigation.sourceVisitId, null);
  assert.ok(!JSON.stringify([...made.storage.data]).includes("visit-s") && !JSON.stringify([...made.storage.data]).includes(S));
  host.down = false;
  assert.equal(await made.queue.flush(), null);
  assert.deepEqual(host.sent.map((event) => [event.id, event.navigation.sourceVisitId]), [["visit-t", null], ["visit-k", null], ["visit-u", "visit-k"]]);
});

test("F-4: without a complete policy answer nothing is swept, rewritten or sent", async () => {
  const failing = { current: async () => ({ ok: true, version: "v3" }), evaluate: async () => ({ ok: false, code: "policy_invalid" }) };
  const host = queueHost();
  const made = makeQueue(host, { policy: failing });
  const target = navVisit("visit-t", B, nav({ sourceVisitId: "visit-s" }));
  await made.queue.enqueueAll([plainVisit("visit-s", A), target]);
  assert.equal(await made.queue.flush(), "policy_invalid");
  assert.deepEqual(made.pending()[1], target);
  assert.deepEqual(host.sent, []);
});


// --- Documents only (PO decision after the audit): behaviour once kept in storage.session tests ------

test("subframes and prerendered documents never touch the tab's correlation", async () => {
  const { tracker } = makeTracker();
  await visitDocument(tracker, 1, "d1", A, "v1", { transitionType: "typed" });
  await tracker.committed({ ...commit(1, "f1", B), frameId: 5, transitionType: "auto_subframe" });
  await tracker.committed({ ...commit(1, "p1", C), documentLifecycle: "prerender" });
  assert.equal(await tracker.completed({ ...done(1, "p1", C), documentLifecycle: "prerender" }), null);
  assert.deepEqual(await visitDocument(tracker, 1, "d2", D, "v2"), nav({ sourceVisitId: "v1" }));
});

test("only a bounded number of commits wait for onCompleted", async () => {
  const { tracker } = makeTracker();
  for (let index = 0; index < MAX_PENDING_COMMITS + 3; index += 1) await tracker.committed(commit(1, `d${index}`, A));
  assert.equal(await tracker.completed(done(1, "d0", A)), null, "the oldest was dropped");
  assert.ok(await tracker.completed(done(1, `d${MAX_PENDING_COMMITS + 2}`, A)), "the newest still waits");
});

test("a new tab closed at once takes its branch with it, and the opener is unaffected", async () => {
  const { tracker } = makeTracker();
  await openerTab(tracker);
  await tracker.createdNavigationTarget(created(1, 2, B));
  await tracker.tabRemoved(2);
  assert.deepEqual(await visitDocument(tracker, 2, "n1", B, "v2"), nav(), "no branch left for tab 2");
  assert.deepEqual(await visitDocument(tracker, 1, "d2", C, "v3"), nav({ sourceVisitId: "v1" }));
});

test("an incognito tab, or one whose kind cannot be read, is never correlated", async () => {
  const { tracker, incognito } = makeTracker();
  incognito.add(3);
  await tracker.createdNavigationTarget(created(1, 3, B));
  await tracker.historyStateUpdated(history(3, "i1", B));
  assert.equal(await visitDocument(tracker, 3, "i1", A, "x"), null);
  assert.equal(await visitDocument(tracker, 404, "g1", A, "y"), null);
});

test("I-1: an about:blank branch expires, dies with its tab, and never reaches another tab or an unrelated navigation", async () => {
  // Expired.
  let { tracker } = makeTracker();
  await openerTab(tracker);
  await aboutBlankBranch(tracker, { seconds: 0 });
  const late = BRANCH_TTL_MS / 1000 + 1;
  assert.deepEqual(await visitDocument(tracker, 2, "n1", B, "v2", { seconds: late }), nav({ occurredAt: iso(late) }));
  // Closed before its navigation.
  ({ tracker } = makeTracker());
  await openerTab(tracker);
  await aboutBlankBranch(tracker);
  await tracker.tabRemoved(2);
  assert.deepEqual(await visitDocument(tracker, 2, "n1", B, "v2", { seconds: 2 }), nav({ occurredAt: iso(2) }));
  // Another tab.
  ({ tracker } = makeTracker());
  await openerTab(tracker);
  await aboutBlankBranch(tracker);
  assert.deepEqual(await visitDocument(tracker, 3, "x1", B, "v3", { seconds: 2 }), nav({ occurredAt: iso(2) }));
  // Another page in between (not about:blank, not http): the branch is dropped.
  ({ tracker } = makeTracker());
  await openerTab(tracker);
  await aboutBlankBranch(tracker);
  await tracker.committed(commit(2, "ntp", "chrome://new-tab-page/", { transitionType: "auto_toplevel", seconds: 2 }));
  assert.deepEqual(await visitDocument(tracker, 2, "n1", B, "v2", { seconds: 3 }), nav({ occurredAt: iso(3) }));
  // The user typed a URL into the blank tab: not the opener's navigation.
  ({ tracker } = makeTracker());
  await openerTab(tracker);
  await aboutBlankBranch(tracker);
  assert.deepEqual(await visitDocument(tracker, 2, "n1", B, "v2", { transitionType: "typed", qualifiers: ["from_address_bar"], seconds: 2 }), nav({ transitionType: "typed", fromAddressBar: true, occurredAt: iso(2) }));
});

test("the service worker sends each document visit with its observed navigation, sources before targets", async () => {
  const worker = await startWorker();
  await worker.document(1, "d1", A, { transitionType: "typed", qualifiers: ["from_address_bar"] });
  await worker.document(1, "d2", B, { seconds: 2 });
  await worker.created(1, 2, C, { seconds: 3 });
  await worker.document(2, "n1", C, { seconds: 4 });
  const [first, second, third] = worker.visits();
  assert.equal(worker.visits().length, 3);
  assert.deepEqual(first.navigation, nav({ transitionType: "typed", fromAddressBar: true }));
  assert.deepEqual(second.navigation, nav({ sourceVisitId: first.id, occurredAt: iso(2) }));
  assert.deepEqual(third.navigation, nav({ sourceVisitId: second.id, newTab: true, occurredAt: iso(4) }));
  assert.equal(worker.scripts.length, 3, "each document is read once");
  assert.deepEqual(worker.local.get(QUEUE_KEY), []);
});

test("NeverSave and SaveButNoMcp: no visit for a refused URL and no source through it; an allowed-but-private one is a source", async () => {
  const worker = await startWorker();
  await worker.document(1, "d1", A, { transitionType: "typed" });
  await worker.document(1, "d2", NEVER);
  await worker.document(1, "d3", B);
  await worker.document(1, "d4", NO_MCP);
  await worker.document(1, "d5", C);
  const visits = worker.visits();
  assert.deepEqual(visits.map((event) => event.url), [A, B, NO_MCP, C]);
  assert.equal(visits[1].navigation.sourceVisitId, null, "the page shown before B was NeverSave");
  assert.equal(visits[2].navigation.sourceVisitId, visits[1].id, "a SaveButNoMcp destination is recorded with its navigation");
  assert.equal(visits[3].navigation.sourceVisitId, visits[2].id, "a SaveButNoMcp source is kept; the host and readers judge it again");
  const everywhere = JSON.stringify([...worker.local.entries()]) + JSON.stringify([...worker.session.entries()]) + JSON.stringify(worker.host.received);
  assert.ok(!everywhere.includes("accounts.google.com"), "the refused URL is in no event, queue or storage");
});

test("incognito tabs and an unusable policy record nothing, and what follows has no source", async () => {
  const worker = await startWorker();
  await worker.document(3, "i1", A, { incognito: true });
  await worker.created(3, 4, B);
  assert.deepEqual(worker.visits(), []);
  await worker.document(1, "d1", A, { transitionType: "typed" });
  worker.host.state = "invalid";
  await worker.message({ type: "policy-status" });
  await worker.document(1, "d2", B);
  assert.equal(worker.visits().length, 1);
  worker.host.state = "ready";
  await worker.message({ type: "policy-status" });
  await worker.document(1, "d3", D);
  assert.equal(worker.visits()[1].navigation.sourceVisitId, null);
});

// --- SPA: never recorded, only forgotten ------------------------------------------------------------

test("SPA: a history update makes nothing, and the next document does not name the visit before it", async () => {
  for (const updates of [[B], [B, C], [A]]) {
    const { tracker } = makeTracker();
    await visitDocument(tracker, 1, "d1", A, "v1", { transitionType: "typed" });
    for (const url of updates) assert.equal(await tracker.historyStateUpdated(history(1, "d1", url)), undefined, "no visit, no navigation");
    assert.equal((await visitDocument(tracker, 1, "d2", D, "v2", { seconds: 1 })).sourceVisitId, null, `A -> ${updates.join(" -> ")} -> D`);
    assert.equal((await visitDocument(tracker, 1, "d3", C, "v3", { seconds: 2 })).sourceVisitId, "v2", "the chain starts again from the next recorded document");
  }
});

test("SPA: a history update in another tab, or in a subframe, changes nothing here", async () => {
  const { tracker } = makeTracker();
  await visitDocument(tracker, 1, "d1", A, "v1", { transitionType: "typed" });
  await tracker.historyStateUpdated(history(2, "x", B));
  await tracker.historyStateUpdated({ ...history(1, "f1", B), frameId: 3 });
  assert.equal((await visitDocument(tracker, 1, "d2", C, "v2")).sourceVisitId, "v1");
});

test("SPA URL ABA: a commit dropped by a history update never comes back, even at its own URL", async () => {
  const { tracker } = makeTracker();
  await visitDocument(tracker, 1, "d0", D, "v0", { transitionType: "typed" });
  await tracker.committed(commit(1, "d1", A, { seconds: 1 }));
  await tracker.historyStateUpdated(history(1, "d1", B, { seconds: 2 }));
  await tracker.historyStateUpdated(history(1, "d1", A, { seconds: 3 }));
  assert.equal(await tracker.completed(done(1, "d1", A)), null, "no navigation for A: its facts were dropped at B");
  assert.equal((await visitDocument(tracker, 1, "d2", C, "v2", { seconds: 4 })).sourceVisitId, null);
  assert.equal(await tracker.pristine(1, "d1"), false);
  assert.equal(await tracker.pristine(1, "d2"), true);
  assert.equal(await tracker.pristine(1, undefined), false, "a document that cannot be named is never pristine");
});

test("SPA in the service worker: pushState, replaceState and fragments send and queue nothing", async () => {
  const worker = await startWorker();
  await worker.document(1, "spa", A, { transitionType: "typed" });
  const before = JSON.stringify([...worker.local.entries()]);
  worker.tabs[1] = { ...worker.tabs[1], url: NEVER, title: PRIVATE_TITLE };
  await worker.history(1, "spa", C, { seconds: 1 });
  await worker.history(1, "spa", NEVER, { seconds: 2 });
  await worker.history(1, "spa", `${C}#part`, { seconds: 3 });
  assert.deepEqual(worker.visits().map((event) => event.url), [A]);
  assert.equal(JSON.stringify([...worker.local.entries()]), before, "nothing queued or counted");
  const everywhere = JSON.stringify(worker.host.received) + JSON.stringify([...worker.local.entries()]) + JSON.stringify([...worker.session.entries()]);
  assert.ok(!everywhere.includes(PRIVATE_TITLE) && !everywhere.includes("accounts.google.com"));
  await worker.document(1, "d2", B, { seconds: 4 });
  assert.equal(worker.visits()[1].navigation.sourceVisitId, null, "not A: the tab showed SPA states since");
});

// --- Page metadata and title: kept only from a document with no history update ----------------------

const rawPage = (url, title) => ({ href: url, baseUri: url, title, description: [`${title} description`], ogTitle: [`${title} og`], ogDescription: [], ogSiteName: [], ogImage: [], icon: [] });

test("a document read with no history update keeps its title and page", async () => {
  const worker = await startWorker({ readPage: (options) => [{ documentId: options.target.documentIds[0], frameId: 0, result: rawPage(A, "Public A") }] });
  await worker.document(1, "d1", A, { transitionType: "typed" });
  const [visit] = worker.visits();
  assert.equal(visit.title, "Title 1");
  assert.equal(visit.page.ogTitle, "Public A og");
});

test("ABA while the page is read (allowed A -> NeverSave -> A): neither its title nor its page is kept", async () => {
  let worker;
  const readPage = async (options) => {
    // The page moves to a NeverSave URL with its own title and OGP, then back to A, before the answer.
    worker.tabs[1] = { ...worker.tabs[1], title: PRIVATE_TITLE };
    await worker.history(1, "d1", NEVER, { seconds: 1 });
    await worker.history(1, "d1", A, { seconds: 2 });
    return [{ documentId: options.target.documentIds[0], frameId: 0, result: rawPage(A, PRIVATE_TITLE) }];
  };
  worker = await startWorker({ readPage });
  await worker.document(1, "d1", A, { transitionType: "typed" });
  const [visit] = worker.visits();
  assert.equal(visit.url, A, "the visit itself is recorded");
  assert.equal(visit.title, "");
  assert.equal(visit.page, undefined);
  assert.ok(!JSON.stringify(worker.host.received).includes(PRIVATE_TITLE) && !JSON.stringify([...worker.local.entries()]).includes(PRIVATE_TITLE));
});

test("a document that changed URL while loading is recorded without navigation, title or page", async () => {
  const worker = await startWorker({ readPage: (options) => [{ documentId: options.target.documentIds[0], frameId: 0, result: rawPage(A, PRIVATE_TITLE) }] });
  await worker.document(1, "d0", D, { transitionType: "typed" });
  await worker.commitOnly(1, "d1", A, { seconds: 1 });
  await worker.history(1, "d1", NEVER, { seconds: 2 });
  await worker.history(1, "d1", A, { seconds: 3 });
  worker.tabs[1] = { ...worker.tabs[1], url: A, title: PRIVATE_TITLE };
  await worker.complete(1, "d1", A);
  const visit = worker.visits().at(-1);
  assert.equal(visit.url, A);
  assert.deepEqual([visit.title, visit.page, Object.hasOwn(visit, "navigation")], ["", undefined, false]);
});

// --- Worker restarts and storage.session: nothing is restored -----------------------------------------

test("a restarted tracker starts with nothing: no replayed commit, no source", async () => {
  const before = makeTracker().tracker;
  await visitDocument(before, 1, "d1", A, "v1", { transitionType: "typed" });
  await before.committed(commit(1, "d2", B, { seconds: 1 }));
  const after = makeTracker().tracker;
  assert.equal(await after.completed(done(1, "d2", B)), null, "the commit seen before the restart is not replayed");
  assert.equal((await visitDocument(after, 1, "d3", C, "v3", { seconds: 2 })).sourceVisitId, null);
  assert.equal((await visitDocument(after, 1, "d4", D, "v4", { seconds: 3 })).sourceVisitId, "v3", "and correlates again from there");
});

test("a restarted worker still records the document, without its navigation, and writes no tracker state", async () => {
  const session = new Map();
  const local = new Map();
  const host = swHost();
  const before = await startWorker({ host, session, local });
  await before.document(1, "d1", A, { transitionType: "typed" });
  await before.commitOnly(1, "d2", B, { seconds: 3 });
  const after = await startWorker({ host, session, local, tabs: before.tabs, frames: before.frames });
  await after.complete(1, "d2", B);
  const visits = host.received.filter((event) => event.type === "visit");
  assert.deepEqual(visits.map((event) => [event.url, Object.hasOwn(event, "navigation")]), [[A, true], [B, false]]);
  assert.ok(![...session.keys()].some((key) => key.startsWith("navigation")), "nothing of the tracker in storage.session");
});

test("a failing storage.session changes nothing in correlation", async () => {
  const worker = await startWorker();
  worker.sessionStore.set = async () => {
    throw new Error("synthetic storage.session write failure");
  };
  worker.sessionStore.remove = async () => {
    throw new Error("synthetic storage.session remove failure");
  };
  await worker.document(1, "d1", A, { transitionType: "typed" });
  await worker.document(1, "d2", NEVER);
  await worker.document(1, "d3", C);
  const visits = worker.visits();
  assert.deepEqual(visits.map((event) => [event.url, event.navigation.sourceVisitId === null]), [[A, true], [C, true]], "C does not name A across NeverSave");
});

// Audit F-1/F-3 and the SPA decision end to end (opt-in, as above): the real host into a fresh vault,
// then the real MCP server. MEMOCAMPUS_TEST_MCP=<built memocampus-mcp> as well.
const MCP_BIN = process.env.MEMOCAMPUS_TEST_MCP;

test("end to end without SPA visits: no private title or page in the vault or MCP, no source across SPA states", { skip: HOST_BIN && MCP_BIN ? false : "set MEMOCAMPUS_TEST_HOST and MEMOCAMPUS_TEST_MCP to run" }, async () => {
  const { spawn } = await import("node:child_process");
  const { mkdtempSync, rmSync } = await import("node:fs");
  const os = await import("node:os");
  const { DatabaseSync } = await import("node:sqlite");
  const appDir = mkdtempSync(path.join(os.tmpdir(), "memocampus-audit-e2e-"));
  const env = { ...process.env, MEMOCAMPUS_APP_DIR: appDir };
  delete env.MEMOCAMPUS_DB;
  // The worker keeps delivering in the background; at the end no new host is started and the running
  // ones are awaited, so the temporary directory (their working directory) can be removed.
  const running = new Set();
  let closing = false;
  const call = (message) =>
    new Promise((resolve, reject) => {
      if (closing) return reject(new Error("test finished"));
      const child = spawn(HOST_BIN, [], { env, cwd: appDir, stdio: ["pipe", "pipe", "pipe"] });
      running.add(child.pid);
      child.on("close", () => running.delete(child.pid));
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
  try {
    assert.equal((await call({ type: "policy_initialize", requestId: "p" })).ok, true);
    const created = await call({ type: "vault_create", requestId: "c", useDefault: true });
    assert.equal(created.ok, true);
    assert.equal(created.status.publication, "pending", "a new vault starts unpublished");
    let worker;
    let abaWhileReading = false;
    const readPage = async (options) => {
      if (abaWhileReading) {
        worker.tabs[1] = { ...worker.tabs[1], title: PRIVATE_TITLE };
        await worker.history(1, options.target.documentIds[0], NEVER);
        await worker.history(1, options.target.documentIds[0], A);
      }
      return [{ documentId: options.target.documentIds[0], frameId: 0, result: rawPage(A, abaWhileReading ? PRIVATE_TITLE : "Public A") }];
    };
    worker = await startWorker({ transport: call, readPage });
    // Tab 1: a pristine document (title and page kept), then the same URL read during an ABA.
    await worker.document(1, "a1", A, { transitionType: "typed" });
    abaWhileReading = true;
    await worker.document(1, "a2", A, { seconds: 1 });
    abaWhileReading = false;
    // Tab 2: A, pushState to a NeverSave URL, then a new document C: C names nobody.
    await worker.document(2, "b1", A, { transitionType: "typed", seconds: 2 });
    await worker.history(2, "b1", NEVER, { seconds: 3 });
    await worker.document(2, "b2", C, { seconds: 4 });
    // Tab 3: committed at D, moved to C by pushState before completing (audit F-3).
    await worker.document(3, "c0", B, { transitionType: "typed", seconds: 5 });
    await worker.commitOnly(3, "c1", D, { seconds: 6 });
    await worker.history(3, "c1", C, { seconds: 7 });
    worker.tabs[3] = { ...worker.tabs[3], url: C, title: PRIVATE_TITLE };
    await worker.complete(3, "c1", C);
    await settle(500);
    await worker.message({ type: "flush-queue" });
    assert.deepEqual(worker.local.get(QUEUE_KEY), [], "everything acknowledged");

    const db = new DatabaseSync(vault, { readOnly: true });
    try {
      const rows = db
        .prepare(
          `SELECT v.url, v.title, n.kind, s.url AS source, (SELECT count(*) FROM page_metadata p WHERE p.event_id = v.event_id) AS page
           FROM visits v LEFT JOIN navigation_events n ON n.target_visit_id = v.event_id LEFT JOIN visits s ON s.event_id = n.source_visit_id
           ORDER BY v.visited_at`,
        )
        .all()
        .map((row) => ({ ...row }));
      assert.deepEqual(
        rows.map((row) => [row.url, row.title, row.kind, row.source]),
        [
          [A, "Title 1", "document", null],
          [A, "", "document", A],
          [A, "Title 2", "document", null],
          [C, "Title 2", "document", null],
          [B, "Title 3", "document", null],
          [C, "", null, null],
        ],
      );
      assert.equal(rows[1].page, 0, "the read during the ABA is not stored");
      assert.deepEqual({ ...db.prepare("SELECT title, og_title FROM page_metadata WHERE url = ?").get(A) }, { title: "Public A", og_title: "Public A og" }, "A's stored page is a pristine read");
      assert.equal(db.prepare("SELECT count(*) AS n FROM navigation_events WHERE kind = 'history_state'").get().n, 0);
      assert.equal(db.prepare("SELECT count(*) AS n FROM visits WHERE title LIKE ? OR url LIKE '%accounts.google.com%'").get(`%${PRIVATE_TITLE}%`).n, 0);
      assert.equal(db.prepare("SELECT count(*) AS n FROM page_metadata WHERE title LIKE ? OR og_title LIKE ?").get(`%${PRIVATE_TITLE}%`, `%${PRIVATE_TITLE}%`).n, 0);
      assert.deepEqual(db.prepare("PRAGMA foreign_key_check").all(), []);
      assert.equal(db.prepare("PRAGMA integrity_check").get().integrity_check, "ok");
    } finally {
      db.close();
    }

    // Issue #51: a new vault is pending; MCP reads it only after the side panel's confirmation.
    await confirmPublication(call);
    const mcp = startMcp(MCP_BIN, ["--vault", vault], { env });
    const { rpc } = mcp;
    try {
      await mcp.initialize("audit-e2e");
      const { tools } = (await rpc("tools/list", {})).result;
      let answers = "";
      for (const query of ["", "Title", "a.test", PRIVATE_TITLE]) answers += JSON.stringify(await rpc("tools/call", { name: "memocampus_search_visits", arguments: query ? { query } : {} }));
      answers += JSON.stringify(await rpc("tools/call", { name: "memocampus_get_domain_stats", arguments: {} }));
      assert.ok(answers.includes("a.test"), "the allowed visits are searchable");
      assert.ok(!answers.includes(PRIVATE_TITLE), "the private title never reaches MCP");
      assert.ok(!JSON.stringify(tools).toLowerCase().includes("navigation"), "no tool reads navigations");
    } finally {
      await mcp.close();
    }
  } finally {
    closing = true;
    for (let wait = 0; running.size > 0 && wait < 100; wait += 1) await settle(100);
    rmSync(appDir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  }
});
