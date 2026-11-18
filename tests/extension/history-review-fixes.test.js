// Regression tests for the independent review of History Slice 2 (FINDING-HISTORY-01/02/03): each reproduction of
// the review is a test here. Fake DOM and an in-memory host, nothing real.
import { test } from "node:test";
import assert from "node:assert/strict";
import { at, openHistory, settle, tokenOf, visit } from "./history-fixtures.js";
import { hostSupportsHistory, historyList, isLegacyUnknownType } from "../../extension/history-client.js";
import { SearchError } from "../../extension/search-client.js";

const walk = (el, out = []) => { out.push(el); el.children.forEach((c) => walk(c, out)); return out; };
const withClass = (root, name) => walk(root).filter((e) => e.className.split(" ").includes(name));
const rowsOf = ($) => $("history-list").children;
const cellOf = ($, day) => withClass($("cal-table"), "cal-day").find((b) => b.dataset.day === String(day));
const lastList = (host) => host.of("history_list").at(-1);
const pageButton = ($, label) => withClass($("history-pager"), "page-button").find((b) => b.textContent === label);

const A = "https://github.com/a";
const MANY = Array.from({ length: 230 }, (_, i) => visit(`p${String(i).padStart(3, "0")}`, A, `visit ${i}`, at(2026, 11, 17, 6 + Math.floor(i / 60), i % 60, 0)));
const FULL = [
  visit("v1", A, "First", at(2026, 11, 17, 8)),
  visit("v3", "https://gist.github.com/x", "Gist", at(2026, 11, 17, 10)),
  visit("o1", A, "Nov 3", at(2026, 11, 3)),
];

const hold = () => { let release; const promise = new Promise((resolve) => { release = resolve; }); return { promise, release }; };
/** A gate that holds only the first call of a type. */
const firstOnly = (gate) => { let first = true; return async () => { if (first) { first = false; await gate.promise; } }; };
const typeInto = ($, value) => { $("domain-input").value = value; $("domain-input").dispatch("input"); };

// ------------------------------------------------------------------------------------------------ HISTORY-01

test("HISTORY-01: a late answer that corrects the page does not wipe a domain being typed", async () => {
  const held = hold();
  const { $, host, app, timers, win } = await openHistory({ search: "?date=2026-11-17&size=25&page=99", visits: { A: MANY }, host: { gates: { history_list: firstOnly(held) } } });
  typeInto($, "new.synthetic.test");
  held.release();
  await settle(20);
  assert.equal($("domain-input").value, "new.synthetic.test", "the draft survives the correction");
  assert.equal(app.state.page, 10, "the page was corrected");
  assert.equal(app.state.domain, "", "and the draft is not confirmed by it");
  assert.ok(host.of("history_list").every((call) => call.domain === ""), "nothing is sent for the draft yet");
  timers.fire();
  await settle(10);
  assert.equal(lastList(host).domain, "new.synthetic.test");
  assert.equal(lastList(host).page, 1, "a confirmed domain starts at page 1");
  assert.equal(app.state.domain, "new.synthetic.test");
  assert.equal(win.log.filter(([kind]) => kind === "push").length, 0, "typing adds no history entry");
});

test("HISTORY-01: another search action confirms the typed domain first, whatever the action", async () => {
  const actions = {
    size: ($) => { $("size-select").value = "100"; $("size-select").dispatch("change"); },
    day: ($) => cellOf($, 3).dispatch("click"),
    mode: ($) => $("mode-regex").dispatch("change"),
    page: ($) => pageButton($, "2").dispatch("click"),
  };
  for (const [name, act] of Object.entries(actions)) {
    const { $, host, app, timers, win } = await openHistory({ search: "?date=2026-11-17&size=25", visits: { A: MANY } });
    typeInto($, "new.synthetic.test");
    act($);
    await settle(10);
    assert.equal($("domain-input").value, "new.synthetic.test", name);
    assert.equal(app.state.domain, "new.synthetic.test", name);
    assert.equal(lastList(host).domain, "new.synthetic.test", name);
    assert.equal(timers.count, 0, `${name}: the debounce is over`);
    assert.match(win.entries[win.index].search, /domain=new\.synthetic\.test/, name);
  }
});

test("HISTORY-01: the size change keeps the draft and returns to page 1; the mode change keeps both", async () => {
  const { $, host, app } = await openHistory({ search: "?date=2026-11-17&size=25&page=2", visits: { A: MANY } });
  typeInto($, "github.com");
  $("size-select").value = "50";
  $("size-select").dispatch("change");
  await settle(10);
  assert.deepEqual([app.state.size, app.state.page, app.state.domain], [50, 1, "github.com"]);
  assert.equal(lastList(host).size, 50);
  $("mode-regex").dispatch("change");
  await settle(10);
  assert.deepEqual([app.state.mode, app.state.domain], ["regex", "github.com"]);
});

test("HISTORY-01: moving the calendar's month keeps the draft and its debounce", async () => {
  const { $, host, timers, app } = await openHistory({ visits: { A: FULL } });
  typeInto($, "gist.gith");
  const lists = host.of("history_list").length;
  $("cal-prev").dispatch("click");
  await settle(10);
  assert.equal($("domain-input").value, "gist.gith");
  assert.equal(timers.count, 1, "the debounce is still pending");
  assert.equal(host.of("history_list").length, lists, "the list is not asked for again");
  assert.equal(app.state.domain, "");
  assert.equal(host.of("history_days").at(-1).domain, "", "the marks use the confirmed condition only");
});

test("HISTORY-01: Back and Forward restore the inputs from the URL and cancel the draft", async () => {
  const { $, win, timers, app, host } = await openHistory({ search: "?date=2026-11-16&domain=old.test", visits: { A: FULL } });
  cellOf($, 17).dispatch("click");
  await settle(10);
  typeInto($, "draft.test");
  win.back();
  await settle(10);
  assert.equal($("domain-input").value, "old.test");
  assert.equal(timers.count, 0, "no debounce is left over");
  timers.fire();
  await settle(10);
  assert.equal(app.state.domain, "old.test");
  assert.equal(lastList(host).domain, "old.test");
});

test("HISTORY-01: a host error while a domain is being typed does not wipe it", async () => {
  const { $, host, clock, win, timers } = await openHistory({ visits: { A: FULL } });
  typeInto($, "typed.test");
  host.errors.history_list = "unavailable";
  clock.advance(5000);
  win.focus();
  await settle(10);
  assert.equal($("domain-input").value, "typed.test");
  assert.equal(timers.count, 1);
});

test("HISTORY-01: the answer for the earlier condition never shows as the new condition's", async () => {
  const held = hold();
  const { $, timers, app } = await openHistory({ visits: { A: FULL }, host: { gates: { history_list: firstOnly(held) } } });
  typeInto($, "gist.github.com");
  timers.fire();
  await settle(10);
  held.release(); // the answer for "no domain" arrives late
  await settle(10);
  assert.equal(app.state.domain, "gist.github.com");
  assert.deepEqual(rowsOf($).map((row) => row.dataset.visitId), ["v3"]);
});

// ------------------------------------------------------------------------------------------------ HISTORY-02

const LONG = `https://long.test/${"a".repeat(3000)}`;
const CUT_VISITS = [
  ...Array.from({ length: 59 }, (_, i) => visit(`c${i}`, A, `plain ${i}`, at(2026, 11, 17, 1, i))),
  visit("cut", LONG, "Long", at(2026, 11, 17, 23)),
  visit("n16", "https://example.org/16", "Nov 16", at(2026, 11, 16, 9)),
  visit("n3", "https://example.org/3", "Nov 3", at(2026, 11, 3, 9)),
];

/** Nov 17 (size 25) with a cut URL on its first page, reached by a push so that Back has somewhere to go. */
async function cutPage(hostOptions = {}) {
  const page = await openHistory({ search: "?date=2026-11-16&size=25", visits: { A: CUT_VISITS, B: [visit("b1", "https://b.test/", "B", at(2026, 11, 17, 8))] }, host: hostOptions });
  cellOf(page.$, 17).dispatch("click");
  await settle(10);
  page.cutButton = () => {
    const row = rowsOf(page.$).find((r) => r.dataset.visitId === "cut");
    return row ? withClass(row, "visit-link")[0] : null;
  };
  assert.ok(page.cutButton(), "the cut row is on the page");
  return page;
}
async function startOpen(page) {
  const gate = hold();
  page.host.gates.history_visit = () => gate.promise;
  const button = page.cutButton();
  button.dispatch("click");
  await settle(5);
  assert.equal(page.host.of("history_visit").length, 1);
  return { gate, button };
}

test("HISTORY-02: on an unchanged screen the full URL opens once, even for a double click", async () => {
  const page = await cutPage();
  const { gate, button } = await startOpen(page);
  button.dispatch("click");
  gate.release();
  await settle(10);
  assert.equal(page.host.of("history_visit").length, 1);
  assert.deepEqual(page.tabs.opened, [{ url: LONG }]);
});

test("HISTORY-02: whatever changes the screen, the late answer opens nothing", async () => {
  const actions = {
    "another day": (p) => cellOf(p.$, 3).dispatch("click"),
    "domain confirmed": (p) => { typeInto(p.$, "github.com"); p.$("domain-input").dispatch("keydown", { key: "Enter" }); },
    "next page": (p) => pageButton(p.$, "2").dispatch("click"),
    "page size": (p) => { p.$("size-select").value = "100"; p.$("size-select").dispatch("change"); },
    "mode": (p) => p.$("mode-regex").dispatch("change"),
    "Back": (p) => p.win.back(),
    "re-read of the list": (p) => { p.clock.advance(5000); p.win.focus(); },
  };
  for (const [name, act] of Object.entries(actions)) {
    const page = await cutPage();
    const { gate } = await startOpen(page);
    act(page);
    await settle(10);
    gate.release();
    await settle(10);
    assert.equal(page.tabs.opened.length, 0, name);
  }
});

test("HISTORY-02: the same visit shown again later is not the one that was asked for", async () => {
  const page = await cutPage();
  const { gate } = await startOpen(page);
  cellOf(page.$, 3).dispatch("click");
  await settle(10);
  page.win.back();
  await settle(10);
  assert.ok(page.cutButton(), "the same id is on the screen again");
  gate.release();
  await settle(10);
  assert.equal(page.tabs.opened.length, 0);
});

test("HISTORY-02: a Vault switch while the token is unknown never opens the old Vault's URL", async () => {
  const page = await cutPage();
  const { gate, button } = await startOpen(page);
  const listGate = hold();
  page.host.vault = "B";
  let daysCalls = 0;
  page.host.gates.history_list = () => listGate.promise;
  page.host.gates.history_days = async () => { if (++daysCalls > 1) await listGate.promise; }; // the marks of the new Vault wait too
  page.clock.advance(5000);
  page.$("cal-prev").dispatch("click"); // a request of the new Vault notices the switch
  await settle(20);
  assert.equal(page.app.vaultToken, null, "the token is unknown while the page reads again");
  gate.release(); // the answer of Vault A
  await settle(10);
  assert.equal(page.tabs.opened.length, 0);
  button.dispatch("click"); // a click on the stale button starts nothing
  await settle(10);
  assert.equal(page.host.of("history_visit").length, 1);
  listGate.release();
  await settle(20);
  assert.equal(page.tabs.opened.length, 0);
  assert.equal(page.app.vaultToken, tokenOf("B"));
});

test("HISTORY-02: an answer for another visit, another Vault, a bad URL or no token opens nothing, and is not retried", async () => {
  const cases = {
    "another visit": (reply) => ({ ...reply, visit: { ...reply.visit, id: "c1" } }),
    "another Vault": (reply) => ({ ...reply, vaultToken: tokenOf("B") }),
    "a script URL": (reply) => ({ ...reply, visit: { ...reply.visit, url: "javascript:alert(1)" } }),
    "a missing token": (reply) => ({ ...reply, vaultToken: undefined }),
  };
  for (const [name, mutate] of Object.entries(cases)) {
    const page = await cutPage({ mutate: { history_visit: mutate } });
    page.cutButton()?.dispatch("click");
    await settle(30);
    assert.equal(page.tabs.opened.length, 0, name);
    assert.equal(page.host.of("history_visit").length, 1, `${name}: no retry loop`);
  }
});

// ------------------------------------------------------------------------------------------------ HISTORY-03

const asking = (reply) => ({ send: async () => reply, createId: () => "req-1" });

test("HISTORY-03: a legacy host's unknown_type means 'history not supported'; a real capability list is read as it is", async () => {
  assert.equal(await hostSupportsHistory(asking({ ok: false, eventId: null, error: "unknown_type" })), false);
  assert.equal(await hostSupportsHistory(asking({ ok: false, type: "host_capabilities_result", requestId: "req-1", error: "unknown_type" })), false);
  assert.equal(await hostSupportsHistory(asking({ ok: true, type: "host_capabilities_result", requestId: "req-1", capabilities: ["history_v1"] })), true);
  assert.equal(await hostSupportsHistory(asking({ ok: true, type: "host_capabilities_result", requestId: "req-1", capabilities: ["navigation_v1"] })), false);
});

test("HISTORY-03: nothing else is taken for 'not supported'", async () => {
  const wrong = {
    "another error": { ok: false, eventId: null, error: "vault_missing" },
    "an event id": { ok: false, eventId: "e1", error: "unknown_type" },
    "ok true": { ok: true, eventId: null, error: "unknown_type" },
    "extra fields": { ok: false, eventId: null, error: "unknown_type", capabilities: ["history_v1"] },
    "an empty body": {},
    "a wrong request id": { ok: false, type: "host_capabilities_result", requestId: "other", error: "unknown_type" },
    "a wrong type": { ok: false, type: "something_result", requestId: "req-1", error: "unknown_type" },
    "null": null,
  };
  for (const [name, reply] of Object.entries(wrong)) {
    await assert.rejects(hostSupportsHistory(asking(reply)), SearchError, name);
  }
  assert.equal(isLegacyUnknownType([]), false);
  assert.equal(await hostSupportsHistory(asking({ ok: true, type: "host_capabilities_result", requestId: "req-1" })), false, "no capabilities never means history");
});

test("HISTORY-03: the legacy shape is not accepted as an answer of the history calls", async () => {
  await assert.rejects(historyList({ visitedFrom: "a", visitedTo: "b", domain: "", mode: "exact", page: 1, size: 50 }, asking({ ok: false, eventId: null, error: "unknown_type" })), (error) => error instanceof SearchError && error.code === "invalid_response");
});

test("HISTORY-03: a legacy host shows 'not supported', sends no history request and does not retry by itself", async () => {
  const { $, host } = await openHistory({ host: { mutate: { host_capabilities: () => ({ ok: false, eventId: null, error: "unknown_type" }) } } });
  assert.match($("history-state-text").textContent, /対応していません/);
  assert.equal(host.of("history_list").length + host.of("history_days").length, 0);
  assert.equal(host.of("host_capabilities").length, 1);
});
