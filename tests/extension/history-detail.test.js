// Issue #41, Slice 3: the detail of a visit, expanded under its row. Fake DOM and an in-memory host with the
// history_visit contract of Slice 1 (source present / censored / unknown, targets with a cap, page metadata).
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { extensionDir } from "./fake-dom.js";
import { at, openHistory, settle, tokenOf, visit } from "./history-fixtures.js";
import { historyVisit } from "../../extension/history-client.js";
import { SearchError } from "../../extension/search-client.js";

const text = (el) => [el.textContent, ...el.children.map(text)].join(" ").replace(/\s+/g, " ").trim();
const walk = (el, out = []) => { out.push(el); el.children.forEach((c) => walk(c, out)); return out; };
const withClass = (root, name) => walk(root).filter((e) => e.className.split(" ").includes(name));
const rowsOf = ($) => $("history-list").children;
const rowOf = ($, id) => rowsOf($).find((r) => r.dataset.visitId === id);
const toggleOf = ($, id) => withClass(rowOf($, id), "visit-toggle")[0];
const regionOf = ($, id) => withClass(rowOf($, id), "visit-detail")[0];
const cellOf = ($, day) => withClass($("cal-table"), "cal-day").find((b) => b.dataset.day === String(day));
const pageButton = ($, label) => withClass($("history-pager"), "page-button").find((b) => b.textContent === label);
const hold = () => { let release; const promise = new Promise((resolve) => { release = resolve; }); return { promise, release }; };

const T = (h, m) => at(2026, 11, 17, h, m);
const FILLERS = Array.from({ length: 40 }, (_, i) => visit(`f${i}`, "https://filler.test/", `filler ${i}`, T(1, i)));
const VISITS = [
  ...FILLERS,
  visit("src", "https://source.test/start", "Source page", T(11, 0)),
  visit("cur", "https://cur.test/page", "Current page", T(12, 0)),
  visit("t1", "https://target.test/one", "Target one", T(12, 5)),
  visit("t2", "https://target.test/two", "Target two", T(12, 6)),
  visit("t3", "https://target.test/three", "Target three", T(12, 7)),
  visit("cz", "https://cz.test/", "Came from a deleted page", T(12, 10)),
  visit("uk", "https://uk.test/", "Nobody knows", T(12, 11)),
  visit("pm", "https://pm.test/", "With metadata", T(12, 12)),
  visit("big", "https://big.test/", "Many targets", T(12, 13)),
  visit("lonely", "https://lonely.test/", "No targets", T(12, 14)),
  visit("evil", "https://evil.test/", "<img src=x onerror=alert(1)>", T(12, 15)),
  visit("jsrc", "javascript:alert(1)", "Script source", T(12, 16)),
];
const RELATIONS = {
  cur: { source: "src", targets: ["t1", "t2", "t3"] },
  cz: { source: "censored", leak: { url: "https://secret.test/", title: "Secret", id: "gone" } },
  uk: {},
  pm: { pageMetadata: { title: "Meta title", description: "Meta description", ogTitle: null, ogDescription: null, ogSiteName: "Site", ogImageUrl: null, faviconUrl: null, capturedAt: "2026-11-17T12:30:00.000Z" } },
  big: { source: "src", targets: ["t1", "t2", "t3"], targetCount: 5 },
  evil: { source: "jsrc", targets: ["t1"] },
};

async function detailPage(hostOptions = {}) {
  return openHistory({ search: "?date=2026-11-17&size=25", visits: { A: VISITS, B: [visit("b1", "https://b.test/", "B", T(8, 0))] }, host: { relations: RELATIONS, ...hostOptions } });
}
const open = async ($, id) => { toggleOf($, id).dispatch("click"); await settle(10); };

// ------------------------------------------------------------------------------------------------ display

test("rows start closed; opening shows loading, then the whole visit; closing empties it; reopening asks again", async () => {
  const { $, host } = await detailPage({ fullUrls: { cur: `https://cur.test/page?${"q".repeat(3000)}` } });
  const toggle = toggleOf($, "cur");
  assert.equal(toggle.getAttribute("aria-expanded"), "false");
  assert.equal(toggle.getAttribute("aria-controls"), regionOf($, "cur").getAttribute("id"));
  assert.equal(regionOf($, "cur").hidden, true);
  assert.equal(host.of("history_visit").length, 0, "nothing is read before a row is opened");
  const gate = hold();
  host.gates.history_visit = () => gate.promise;
  toggle.dispatch("click");
  await settle(5);
  assert.equal(toggle.getAttribute("aria-expanded"), "true");
  assert.match(text(regionOf($, "cur")), /読み込み中/);
  gate.release();
  await settle(10);
  const shown = text(regionOf($, "cur"));
  assert.ok(shown.includes("q".repeat(3000)), "the whole URL, not the one cut for the list");
  assert.ok(shown.includes("Current page"));
  toggle.dispatch("click");
  assert.equal(toggle.getAttribute("aria-expanded"), "false");
  assert.equal(regionOf($, "cur").hidden, true);
  assert.equal(regionOf($, "cur").children.length, 0, "nothing is kept");
  host.gates.history_visit = undefined;
  await open($, "cur");
  assert.equal(host.of("history_visit").length, 2, "a new opening is a new request");
  assert.match(text(regionOf($, "cur")), /Current page/);
});

test("several rows can be open, each on its own", async () => {
  const { $, host } = await detailPage();
  await open($, "cur");
  await open($, "pm");
  assert.match(text(regionOf($, "cur")), /Current page/);
  assert.match(text(regionOf($, "pm")), /With metadata/);
  toggleOf($, "cur").dispatch("click");
  assert.equal(regionOf($, "cur").hidden, true);
  assert.equal(regionOf($, "pm").hidden, false);
  assert.equal(host.of("history_visit").length, 2);
});

test("the source is told in three ways that never stand in for each other", async () => {
  const { $ } = await detailPage();
  await open($, "cur");
  await open($, "cz");
  await open($, "uk");
  const present = text(regionOf($, "cur"));
  assert.match(present, /遷移元/);
  assert.ok(present.includes("Source page") && present.includes("https://source.test/start"));
  assert.ok(!present.includes("[CENSORED]") && !present.includes("遷移元：不明"));
  const censored = text(regionOf($, "cz"));
  assert.ok(censored.includes("[CENSORED]") && censored.includes("遷移元の閲覧履歴は削除されています"));
  assert.ok(!censored.includes("secret.test") && !censored.includes("Secret") && !censored.includes("gone"), "nothing the host might carry beyond the state is shown");
  assert.ok(!censored.includes("遷移元：不明"));
  assert.equal(withClass(regionOf($, "cz"), "detail-card-source")[0].children.some((c) => c.tagName === "A"), false);
  const unknown = text(regionOf($, "uk"));
  assert.ok(unknown.includes("遷移元：不明") && !unknown.includes("[CENSORED]"));
  assert.ok(!unknown.includes("直接"), "unknown is never called direct access");
});

test("a source state outside the contract is an error of the detail, never taken for another state", async () => {
  const { $, host } = await detailPage({ mutate: { history_visit: (reply) => ({ ...reply, source: { state: "weird" } }) } });
  await open($, "cur");
  const shown = text(regionOf($, "cur"));
  assert.match(shown, /不正な応答/);
  assert.ok(!shown.includes("[CENSORED]") && !shown.includes("遷移元：不明") && !shown.includes("Source page"));
  assert.equal(rowsOf($).length, 25, "the list stays");
  assert.equal(host.of("history_visit").length, 1, "no automatic retry");
});

test("targets: none, many in the host's order, and a cut one that says so", async () => {
  const { $ } = await detailPage();
  await open($, "lonely");
  assert.match(text(regionOf($, "lonely")), /記録された遷移先はありません/);
  await open($, "cur");
  const targets = withClass(regionOf($, "cur"), "detail-target");
  assert.deepEqual(targets.map((t) => t.dataset.visitId), ["t1", "t2", "t3"]);
  assert.match(text(regionOf($, "cur")), /遷移先 3件/);
  assert.ok(!text(regionOf($, "cur")).includes("表示"), "complete: nothing is said to be cut");
  await open($, "big");
  assert.match(text(regionOf($, "big")), /遷移先 5件（先頭3件を表示）/, "the numbers are the ones received");
});

test("the cap is the host's: a cap of 2 over 5 targets, a cap of 3 over 3", async () => {
  const two = await detailPage({ targetLimit: 2 });
  await open(two.$, "big");
  assert.match(text(regionOf(two.$, "big")), /遷移先 5件（先頭2件を表示）/);
  assert.equal(withClass(regionOf(two.$, "big"), "detail-target").length, 2);
  const three = await detailPage({ targetLimit: 3 });
  await open(three.$, "cur");
  assert.ok(!text(regionOf(three.$, "cur")).includes("先頭"));
});

test("page metadata only when the visit has it, and labelled as fetched, not as the page at that time", async () => {
  const { $ } = await detailPage();
  await open($, "pm");
  const shown = text(regionOf($, "pm"));
  assert.ok(shown.includes("取得したページ情報") && shown.includes("Meta title") && shown.includes("Meta description") && shown.includes("Site"));
  assert.match(shown, /訪問時点の内容とは限りません/);
  await open($, "cur");
  assert.ok(!text(regionOf($, "cur")).includes("取得したページ情報"));
});

test("text from the web is text, and only complete http(s) URLs are links", async () => {
  const { $ } = await detailPage();
  await open($, "evil");
  const region = regionOf($, "evil");
  assert.ok(text(region).includes("<img src=x onerror=alert(1)>"));
  assert.equal(walk(region).some((e) => e.tagName === "IMG"), false);
  const anchors = walk(region).filter((e) => e.tagName === "A");
  assert.ok(anchors.every((a) => /^https?:/.test(a.getAttribute("href"))), "no javascript: link");
  assert.ok(anchors.every((a) => a.getAttribute("target") === "_blank" && a.getAttribute("rel") === "noopener noreferrer"));
  assert.ok(text(region).includes("javascript:alert(1)"), "shown as text");
  const long = await detailPage();
  long.host.visits.A.push(visit("lt", "https://t.test/", "x", T(12, 20)));
  assert.ok(long);
});

test("opening a detail does not close the others when the tab is shown again, and the marks are still read", async () => {
  const { $, host, clock, win } = await detailPage();
  await open($, "cur");
  const lists = host.of("history_list").length;
  const days = host.of("history_days").length;
  clock.advance(5000);
  win.focus();
  await settle(20);
  assert.equal(host.of("history_list").length, lists, "the list under an open detail is not read again");
  assert.equal(host.of("history_days").length, days + 1);
  assert.match(text(regionOf($, "cur")), /Current page/);
});

// ------------------------------------------------------------------------------------------------ races

test("whatever changes the list or the Vault while a detail loads, its answer shows nowhere", async () => {
  const actions = {
    "another day": (p) => cellOf(p.$, 3).dispatch("click"),
    "domain confirmed": (p) => { p.$("domain-input").value = "cur.test"; p.$("domain-input").dispatch("keydown", { key: "Enter" }); },
    "mode": (p) => p.$("mode-regex").dispatch("change"),
    "next page": (p) => pageButton(p.$, "2").dispatch("click"),
    "page size": (p) => { p.$("size-select").value = "100"; p.$("size-select").dispatch("change"); },
  };
  for (const [name, act] of Object.entries(actions)) {
    const page = await detailPage();
    const gate = hold();
    page.host.gates.history_visit = () => gate.promise;
    toggleOf(page.$, "cur").dispatch("click");
    await settle(5);
    act(page);
    await settle(10);
    gate.release();
    await settle(10);
    assert.equal(withClass(page.$("history-list"), "detail-card").length, 0, `${name}: no detail on screen`);
    assert.ok(withClass(page.$("history-list"), "visit-detail").every((region) => region.hidden && region.children.length === 0), name);
  }
});

test("Back, a re-read of the list and the same id on a new list never inherit an old detail", async () => {
  const page = await openHistory({ search: "?date=2026-11-16&size=25", visits: { A: VISITS }, host: { relations: RELATIONS } });
  cellOf(page.$, 17).dispatch("click");
  await settle(10);
  const gate = hold();
  page.host.gates.history_visit = () => gate.promise;
  toggleOf(page.$, "cur").dispatch("click");
  await settle(5);
  page.win.back();
  await settle(10);
  page.win.forward();
  await settle(10);
  assert.ok(rowOf(page.$, "cur"), "the same id is on screen again");
  gate.release();
  await settle(10);
  assert.equal(regionOf(page.$, "cur").hidden, true);
  assert.equal(regionOf(page.$, "cur").children.length, 0);
  assert.equal(toggleOf(page.$, "cur").getAttribute("aria-expanded"), "false");
});

test("a Vault switch, with the token unknown, drops the old Vault's answer", async () => {
  const page = await detailPage();
  const gate = hold();
  const listGate = hold();
  page.host.gates.history_visit = () => gate.promise;
  toggleOf(page.$, "cur").dispatch("click");
  await settle(5);
  let days = 0;
  page.host.vault = "B";
  page.host.gates.history_list = () => listGate.promise;
  page.host.gates.history_days = async () => { if (++days > 1) await listGate.promise; };
  page.clock.advance(5000);
  page.$("cal-prev").dispatch("click"); // a request of the new Vault notices the switch
  await settle(20);
  assert.equal(page.app.vaultToken, null);
  gate.release();
  await settle(10);
  assert.ok(!text(page.$("history-list")).includes("Source page"));
  listGate.release();
  await settle(20);
  assert.equal(page.app.vaultToken, tokenOf("B"));
  assert.ok(!text(page.$("history-list")).includes("Source page"), "nothing of Vault A");
  assert.deepEqual(rowsOf(page.$).map((r) => r.dataset.visitId), ["b1"]);
});

test("an answer for another visit or another Vault is never shown", async () => {
  const cases = {
    "another visit": (reply) => ({ ...reply, visit: { ...reply.visit, id: "src" } }),
    "another Vault": (reply) => ({ ...reply, vaultToken: tokenOf("B") }),
  };
  for (const [name, mutate] of Object.entries(cases)) {
    const page = await detailPage({ mutate: { history_visit: mutate } });
    toggleOf(page.$, "cur").dispatch("click");
    await settle(30);
    assert.ok(!text(regionOf(page.$, "cur")).includes("https://source.test/start"), name);
  }
});

test("closing while it loads drops the answer; closing and opening at once shows only the second answer, whatever the order", async () => {
  const page = await detailPage();
  const first = hold();
  const second = hold();
  let calls = 0;
  page.host.gates.history_visit = () => (++calls === 1 ? first.promise : second.promise);
  toggleOf(page.$, "cur").dispatch("click"); // opening 1
  await settle(5);
  toggleOf(page.$, "cur").dispatch("click"); // closed
  toggleOf(page.$, "cur").dispatch("click"); // opening 2
  await settle(5);
  assert.equal(page.host.of("history_visit").length, 2);
  page.host.fullUrls.cur = "https://second.test/page";
  second.release();
  await settle(10);
  assert.ok(text(regionOf(page.$, "cur")).includes("https://second.test/page"));
  page.host.fullUrls.cur = "https://first.test/page";
  first.release(); // the first answer arrives last
  await settle(10);
  assert.ok(text(regionOf(page.$, "cur")).includes("https://second.test/page"));
  assert.ok(!text(regionOf(page.$, "cur")).includes("first.test"));
});

test("a click on a row that is loading closes it, and its answer is dropped", async () => {
  const page = await detailPage();
  const gate = hold();
  page.host.gates.history_visit = () => gate.promise;
  toggleOf(page.$, "cur").dispatch("click");
  await settle(5);
  toggleOf(page.$, "cur").dispatch("click");
  gate.release();
  await settle(10);
  assert.equal(regionOf(page.$, "cur").hidden, true);
  assert.equal(regionOf(page.$, "cur").children.length, 0, "the late answer drew nothing");
  assert.equal(page.host.of("history_visit").length, 1);
});

test("answers released in reverse order each land on their own row", async () => {
  const page = await detailPage();
  const gates = { cur: hold(), pm: hold() };
  page.host.gates.history_visit = (message) => gates[message.id].promise;
  toggleOf(page.$, "cur").dispatch("click");
  toggleOf(page.$, "pm").dispatch("click");
  await settle(5);
  gates.pm.release();
  await settle(10);
  gates.cur.release();
  await settle(10);
  assert.ok(text(regionOf(page.$, "cur")).includes("Current page") && !text(regionOf(page.$, "cur")).includes("With metadata"));
  assert.ok(text(regionOf(page.$, "pm")).includes("With metadata") && !text(regionOf(page.$, "pm")).includes("Current page"));
});

// ------------------------------------------------------------------------------------------------ errors

test("a failed detail is told inside its row, the list stays, nothing opens, and Retry reads again", async () => {
  // Vault-state failures now revoke the whole display (HISTORY-04), rather than leaving a local retry in old data.
  const codes = { visit_not_found: /見つかりません/, response_too_large: /大きすぎ/, unavailable: /Native Host|接続|起動/ };
  for (const [code, pattern] of Object.entries(codes)) {
    const page = await detailPage();
    page.host.errors.history_visit = code;
    await open(page.$, "cur");
    const region = regionOf(page.$, "cur");
    assert.match(text(region), pattern, code);
    assert.ok(!text(region).includes("[CENSORED]") && !text(region).includes("遷移元：不明"), `${code} is neither censored nor unknown`);
    assert.equal(rowsOf(page.$).length, 25, code);
    assert.equal(page.tabs.opened.length, 0);
    assert.equal(page.host.of("history_visit").length, 1, `${code}: no automatic retry`);
    withClass(region, "link-button")[0].dispatch("click");
    await settle(10);
    assert.equal(page.host.of("history_visit").length, 2, code);
    assert.match(text(regionOf(page.$, "cur")), /Current page/, `${code}: the retry succeeds`);
  }
});

test("a failed detail that is closed has nothing to retry", async () => {
  const page = await detailPage();
  page.host.errors.history_visit = "unavailable";
  await open(page.$, "cur");
  const retry = withClass(regionOf(page.$, "cur"), "link-button")[0];
  toggleOf(page.$, "cur").dispatch("click");
  retry.dispatch("click");
  await settle(10);
  assert.equal(page.host.of("history_visit").length, 1);
  assert.equal(regionOf(page.$, "cur").hidden, true);
});

// ------------------------------------------------------------------------------------------------ contract

const asking = (reply) => ({ send: async () => reply, createId: () => "req-1" });
const ok = (extra) => ({ ok: true, type: "history_visit_result", requestId: "req-1", vaultToken: tokenOf("A"), visit: { id: "x", visitedAt: T(1, 1), url: "https://x.test/", title: "", host: "x.test" }, navigation: null, source: { state: "unknown" }, targets: [], targetCount: 0, targetsTruncated: false, pageMetadata: null, ...extra });

test("the client keeps only what a state carries and refuses what breaks the contract", async () => {
  const detail = await historyVisit("x", asking(ok({ source: { state: "censored", url: "https://secret.test/", id: "gone", title: "Secret" } })));
  assert.deepEqual(detail.source, { state: "censored" });
  const broken = [
    { source: { state: "present" } },
    { source: { state: "PRESENT", visit: {} } },
    { source: null },
    { targets: "none" },
    { targetCount: -1 },
    { targetCount: 0, targets: [{ id: "t", visitedAt: T(1, 1), url: "https://t/", title: "", host: "t" }] },
    { targetsTruncated: "yes" },
    { pageMetadata: "text" },
    { vaultToken: "short" },
  ];
  for (const extra of broken) await assert.rejects(historyVisit("x", asking(ok(extra))), (error) => error instanceof SearchError && error.code === "invalid_response", JSON.stringify(extra));
});

test("history.css and history-view.js: the detail wraps, scrolls in itself, and builds no markup from strings", () => {
  const css = readFileSync(path.join(extensionDir, "history.css"), "utf8");
  assert.match(css, /\.detail-targets\s*\{[^}]*overflow-y:\s*auto/);
  assert.match(css, /\.detail-url[^{]*\{[^}]*overflow-wrap:\s*anywhere/);
  const view = readFileSync(path.join(extensionDir, "history-view.js"), "utf8");
  assert.doesNotMatch(view, /innerHTML|outerHTML|insertAdjacentHTML|document\.write/);
});
