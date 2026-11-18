// Issue #41, Slice 2: the full-page History, against a fake Host that keeps the Slice 1 contract
// (tests/extension/history-fixtures.js), a fake `window` with a real session history, and the fake DOM.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { extensionDir, loadSidePanel } from "./fake-dom.js";
import { at, openHistory, settle, tokenOf, visit } from "./history-fixtures.js";
import { DEFAULT_SIZE, MAX_DOMAIN_CHARS, buildSearch, defaultState, pageNumbers, parseDate, parseSearch } from "../../extension/history-state.js";
import { localDayBounds, monthBounds } from "../../extension/visit-calendar.js";

const TODAY = { year: 2026, monthIndex: 10, day: 17 };
const text = (el) => [el.textContent, ...el.children.map(text)].join(" ").replace(/\s+/g, " ").trim();
const walk = (el, out = []) => { out.push(el); el.children.forEach((c) => walk(c, out)); return out; };
const withClass = (root, name) => walk(root).filter((e) => e.className.split(" ").includes(name));
const rowsOf = ($) => $("history-list").children;
const cellOf = ($, day) => withClass($("cal-table"), "cal-day").find((b) => b.dataset.day === String(day));
const marked = ($) => withClass($("cal-table"), "has-visits").map((b) => Number(b.dataset.day));
const lastList = (host) => host.of("history_list").at(-1);

const A = "https://github.com/a";
const NOV17 = [
  visit("v1", A, "First", at(2026, 11, 17, 9, 0)),
  visit("v2", A, "Again", at(2026, 11, 17, 10, 0)),
  visit("v3", "https://gist.github.com/g", "Gist", at(2026, 11, 17, 11, 0)),
  visit("v4", "https://example.org/", "", at(2026, 11, 17, 11, 0)),
];
const OTHER_DAYS = [visit("o1", A, "Nov 3", at(2026, 11, 3)), visit("o2", "https://example.org/", "Oct 20", at(2026, 10, 20)), visit("o3", "https://example.org/", "Next day", at(2026, 11, 18, 0, 0, 1))];
const FULL = [...NOV17, ...OTHER_DAYS];

// ---------------------------------------------------------------------------------------------- URL state

test("the five parameters have their defaults and an explicit URL is restored exactly", () => {
  const { state, corrected } = parseSearch("", TODAY);
  assert.deepEqual([state, corrected], [{ date: TODAY, domain: "", mode: "exact", page: 1, size: 50 }, false]);
  const restored = parseSearch("?date=2026-11-17&domain=github.com&mode=regex&page=3&size=100", TODAY);
  assert.deepEqual(restored, { state: { date: TODAY, domain: "github.com", mode: "regex", page: 3, size: 100 }, corrected: false });
  assert.equal(buildSearch(defaultState(TODAY)), "?date=2026-11-17", "the day is always named; the rest only when it differs");
  assert.equal(buildSearch(restored.state), "?date=2026-11-17&domain=github.com&mode=regex&page=3&size=100");
});

test("a bad date, mode, page or size is corrected to its default, and a good one beside it is kept", () => {
  for (const date of ["2026-02-30", "2026-13-01", "2026-00-10", "2026-1-1", "20261117", "garbage", "", "1969-12-31", "99999-01-01", "2026-11-17T00:00", "2026-11-31"]) {
    const { state, corrected } = parseSearch(`?date=${encodeURIComponent(date)}&page=4`, TODAY);
    assert.deepEqual([state.date, state.page, corrected], [TODAY, 4, true], date);
  }
  assert.equal(parseSearch("?date=2028-02-29", TODAY).state.date.day, 29, "a leap day exists");
  assert.equal(parseSearch("?date=2027-02-29", TODAY).corrected, true);
  assert.deepEqual(parseSearch("?date=2030-01-05", TODAY).state.date, { year: 2030, monthIndex: 0, day: 5 }, "a future day is a day");
  for (const mode of ["wildcard", "", "REGEX", "exact,regex"]) assert.deepEqual([parseSearch(`?mode=${encodeURIComponent(mode)}`, TODAY).state.mode, parseSearch(`?mode=${encodeURIComponent(mode)}`, TODAY).corrected], ["exact", true]);
  for (const page of ["0", "-1", "1.5", "abc", "", " 2", "2 ", "1e3", "9".repeat(30), "+3"]) {
    const parsed = parseSearch(`?page=${encodeURIComponent(page)}`, TODAY);
    assert.deepEqual([parsed.state.page, parsed.corrected], [1, true], JSON.stringify(page));
  }
  assert.equal(parseSearch("?page=000012", TODAY).state.page, 12);
  assert.equal(parseSearch("?page=999999999", TODAY).state.page, 999999999, "nine digits are a page (the host answers an empty one)");
  for (const size of ["10", "0", "-50", "50.5", "abc", "", "1000", "9".repeat(40)]) {
    const parsed = parseSearch(`?size=${encodeURIComponent(size)}`, TODAY);
    assert.deepEqual([parsed.state.size, parsed.corrected], [DEFAULT_SIZE, true], size);
  }
  for (const size of [25, 50, 100]) assert.equal(parseSearch(`?size=${size}`, TODAY).state.size, size);
});

test("a pattern with special characters survives the URL, and an over-long domain is cut and said to be corrected", () => {
  for (const domain of ["(^|\\.)github\\.com$", "a b&c=d#e?f+g%h", "日本語.jp", "[a-z]+\\.(com|org)$", "100%"]) {
    const url = buildSearch({ ...defaultState(TODAY), domain, mode: "regex" });
    assert.equal(parseSearch(url, TODAY).state.domain, domain, domain);
    assert.ok(!url.includes(" ") && !url.includes("#"), url);
  }
  const long = parseSearch(`?domain=${"a".repeat(MAX_DOMAIN_CHARS + 50)}`, TODAY);
  assert.equal([...long.state.domain].length, MAX_DOMAIN_CHARS);
  assert.equal(long.corrected, true);
  assert.equal(parseSearch("?date=2026-11-01&date=2026-11-02", TODAY).state.date.day, 1, "the first of a repeated parameter");
  assert.equal(parseDate("2026-11-17").day, 17);
});

test("page numbers: the first, the last and the neighbours, never every page", () => {
  assert.deepEqual(pageNumbers(8, 20), [1, null, 6, 7, 8, 9, 10, null, 20]);
  assert.deepEqual(pageNumbers(1, 3), [1, 2, 3]);
  assert.deepEqual(pageNumbers(1, 20), [1, 2, 3, null, 20]);
  assert.deepEqual(pageNumbers(20, 20), [1, null, 18, 19, 20]);
  assert.deepEqual(pageNumbers(4, 8), [1, 2, 3, 4, 5, 6, 7, 8], "a gap of one page shows that page");
  assert.deepEqual(pageNumbers(5, 10), [1, 2, 3, 4, 5, 6, 7, null, 10]);
  assert.deepEqual(pageNumbers(1, 0), []);
  assert.ok(pageNumbers(500, 1000).length <= 9);
});

// ------------------------------------------------------------------------------------------------ first load

test("an explicit URL is the state: the host is asked for that day, domain, mode, page and size, and nothing is rewritten", async () => {
  const { $, host, win } = await openHistory({ search: "?date=2026-11-17&domain=github.com&mode=exact&size=25", visits: { A: FULL } });
  assert.equal(host.calls[0].type, "host_capabilities", "the capability is asked first");
  const { visitedFrom, visitedTo } = localDayBounds(2026, 10, 17);
  assert.deepEqual(lastList(host), { type: "history_list", visitedFrom, visitedTo, domain: "github.com", mode: "exact", page: 1, size: 25, requestId: lastList(host).requestId });
  assert.equal(host.of("history_days").length, 1);
  assert.deepEqual(host.of("history_days")[0].bounds, monthBounds(2026, 10));
  assert.equal(host.of("history_days")[0].domain, "github.com");
  assert.deepEqual(win.log, [], "a good URL is not rewritten");
  assert.equal($("domain-input").value, "github.com");
  assert.equal($("size-select").value, "25");
  assert.match($("selected-heading").textContent, /2026年11月17日（火）/);
});

test("one visit is one row, newest first, a URL visited twice is two rows, an empty title has a stand-in", async () => {
  const { $ } = await openHistory({ visits: { A: FULL } });
  const rows = rowsOf($);
  assert.deepEqual(rows.map((r) => r.dataset.visitId), ["v4", "v3", "v2", "v1"], "same time: the larger id first, as the host orders");
  assert.match(text(rows[0]), /（タイトルなし）/);
  assert.match(text(rows[3]), /First/);
  assert.match(text(rows[2]), /Again/);
  assert.equal(rows.filter((r) => /github\.com/.test(text(r))).length, 3);
  assert.match(text($("result-count")), /4件/);
  assert.equal($("history-pager").children.length > 0, true);
  const time = withClass(rows[3], "visit-time")[0];
  assert.equal(time.textContent, "09:00:00", "local time");
});

test("with no parameters the day is today and the month the calendar shows is today's", async () => {
  const { $, app, win } = await openHistory({ visits: { A: FULL } });
  assert.deepEqual(app.state.date, TODAY);
  assert.deepEqual(app.shownMonth, { year: 2026, monthIndex: 10 });
  assert.equal($("cal-title").textContent, "2026年11月");
  assert.deepEqual(win.log, []);
});

test("every bad parameter is corrected once with replaceState (no new history entry)", async () => {
  const { win, app, host } = await openHistory({ search: "?date=2026-02-30&mode=glob&page=-3&size=7&domain=x.test", visits: { A: FULL } });
  assert.deepEqual(win.log, [["replace", "?date=2026-11-17&domain=x.test"]]);
  assert.equal(win.entries.length, 1);
  assert.equal(app.state.page, 1);
  assert.equal(lastList(host).mode, "exact");
});

// ------------------------------------------------------------------------------------------------- calendar

test("the calendar places every day under its weekday, in months of 28, 29, 30 and 31 days", async () => {
  const { $ } = await openHistory({ visits: { A: FULL } });
  for (const [year, monthIndex, days, first] of [[2026, 10, 30, 0], [2026, 11, 31, 2], [2028, 1, 29, 2], [2027, 1, 28, 1], [2026, 0, 31, 4], [2026, 7, 31, 6]]) {
    // Move the shown month by clicking previous/next from November 2026.
    const delta = (year - 2026) * 12 + monthIndex - 10;
    for (let i = 0; i < Math.abs(delta); i++) $(delta < 0 ? "cal-prev" : "cal-next").dispatch("click");
    await settle(5);
    const buttons = withClass($("cal-table"), "cal-day");
    assert.equal(buttons.length, days, `${year}-${monthIndex + 1}`);
    const body = $("cal-table").children[1];
    const firstCell = body.children[0].children.findIndex((cell) => cell.children.length > 0);
    assert.equal(firstCell, first, `the 1st of ${year}-${monthIndex + 1} is a ${["日", "月", "火", "水", "木", "金", "土"][first]}`);
    for (let i = 0; i < Math.abs(delta); i++) $(delta < 0 ? "cal-next" : "cal-prev").dispatch("click");
    await settle(5);
  }
  assert.deepEqual(withClass($("cal-table"), "cal-day").length, 30);
});

test("days with visits carry a dot and a label, for the current domain condition only", async () => {
  const { $ } = await openHistory({ visits: { A: FULL } });
  assert.deepEqual(marked($), [3, 17, 18]);
  assert.equal(withClass(cellOf($, 17), "cal-dot").length, 1);
  assert.equal(cellOf($, 4).getAttribute("aria-label").includes("閲覧なし"), true);
  assert.match(cellOf($, 17).getAttribute("aria-label"), /11月17日（火）閲覧4件、今日、選択中/);
});

test("the month moves without touching the chosen day, the list or the URL; the marks are asked for that month", async () => {
  const { $, host, win, app } = await openHistory({ visits: { A: FULL } });
  const lists = host.of("history_list").length;
  $("cal-prev").dispatch("click");
  await settle(10);
  assert.equal($("cal-title").textContent, "2026年10月");
  assert.deepEqual(marked($), [20]);
  assert.equal(host.of("history_list").length, lists, "no list request");
  assert.deepEqual(win.log, [], "the URL did not change");
  assert.deepEqual(app.state.date, TODAY, "the chosen day is still the 17th of November");
  assert.match($("selected-heading").textContent, /11月17日/);
  assert.equal(rowsOf($).length, 4, "the right side is the 17th still");
  assert.equal(withClass($("cal-table"), "is-selected").length, 0, "the chosen day is not in this month");
  assert.equal($("cal-jump").hidden, false);
  assert.deepEqual(host.of("history_days").at(-1).bounds, monthBounds(2026, 9));
  $("cal-jump").dispatch("click");
  await settle(10);
  assert.equal($("cal-title").textContent, "2026年11月");
  assert.equal(withClass($("cal-table"), "is-selected").length, 1);
  assert.equal($("cal-jump").hidden, true);
});

test("a late answer for another month never overwrites the month shown", async () => {
  let releaseNovember;
  const { $, host } = await openHistory({ visits: { A: FULL } });
  host.gates.history_days = (message) => (message.bounds[0] === monthBounds(2026, 10)[0] ? new Promise((resolve) => { releaseNovember = resolve; }) : Promise.resolve());
  // Back to November (its answer is held), then straight to October (answered at once).
  $("cal-next").dispatch("click");
  await settle(5);
  $("cal-prev").dispatch("click");
  $("cal-prev").dispatch("click");
  await settle(10);
  assert.equal($("cal-title").textContent, "2026年10月");
  assert.deepEqual(marked($), [20]);
  releaseNovember();
  await settle(10);
  assert.deepEqual(marked($), [20], "the held answer of another month is dropped");
});

test("today and the chosen day look different, and a chosen day is told by aria-pressed and the label", async () => {
  const { $ } = await openHistory({ search: "?date=2026-11-03", visits: { A: FULL } });
  const today = cellOf($, 17);
  const chosen = cellOf($, 3);
  assert.ok(today.className.includes("is-today") && !today.className.includes("is-selected"));
  assert.ok(chosen.className.includes("is-selected") && !chosen.className.includes("is-today"));
  assert.equal(today.getAttribute("aria-current"), "date");
  assert.equal(chosen.getAttribute("aria-pressed"), "true");
  assert.equal(today.getAttribute("aria-pressed"), "false");
  assert.match(chosen.getAttribute("aria-label"), /選択中/);
  assert.match(today.getAttribute("aria-label"), /今日/);
});

test("the calendar is a keyboard grid: arrows move between days, Enter chooses", async () => {
  const { $, document, win } = await openHistory({ visits: { A: FULL } });
  const key = (cell, k) => $("cal-table").dispatch("keydown", { key: k, target: cell });
  key(cellOf($, 17), "ArrowRight");
  assert.equal(document.activeElement, cellOf($, 18));
  key(cellOf($, 18), "ArrowDown");
  assert.equal(document.activeElement, cellOf($, 25));
  key(cellOf($, 25), "ArrowLeft");
  assert.equal(document.activeElement, cellOf($, 24));
  assert.equal(cellOf($, 24).getAttribute("tabindex"), "0");
  assert.equal(cellOf($, 17).getAttribute("tabindex"), "-1");
  cellOf($, 24).dispatch("click");
  await settle(10);
  assert.deepEqual(win.log.at(-1), ["push", "?date=2026-11-24"]);
});

// ----------------------------------------------------------------------------------------- choosing a day

test("a chosen day is pushed with page 1, asks for that day's list, and leaves the marks alone", async () => {
  const { $, host, win, app } = await openHistory({ search: "?date=2026-11-17&page=1&size=25", visits: { A: FULL } });
  const days = host.of("history_days").length;
  cellOf($, 3).dispatch("click");
  await settle(10);
  assert.deepEqual(win.log, [["push", "?date=2026-11-03&size=25"]]);
  assert.deepEqual(app.state.date, { year: 2026, monthIndex: 10, day: 3 });
  assert.equal(rowsOf($).length, 1);
  assert.match(text(rowsOf($)[0]), /Nov 3/);
  assert.equal(host.of("history_days").length, days, "the marks do not depend on the day");
  assert.deepEqual(marked($), [3, 17, 18]);
  assert.ok(cellOf($, 3).className.includes("is-selected"));
});

test("a page 3 is reset to 1 by choosing another day", async () => {
  const many = Array.from({ length: 130 }, (_, i) => visit(`m${String(i).padStart(3, "0")}`, A, `m${i}`, at(2026, 11, 17, 8, i % 60, 0)));
  const { $, win, host } = await openHistory({ search: "?date=2026-11-17&page=3&size=25", visits: { A: many } });
  assert.equal(lastList(host).page, 3);
  cellOf($, 18).dispatch("click");
  await settle(10);
  assert.equal(lastList(host).page, 1);
  assert.deepEqual(win.log.at(-1), ["push", "?date=2026-11-18&size=25"]);
});

// ------------------------------------------------------------------------------------------------- domain

test("typing is debounced into one request, never adds a history entry, resets the page and updates the marks", async () => {
  const { $, host, win, timers } = await openHistory({ visits: { A: FULL } });
  const lists = host.of("history_list").length;
  const days = host.of("history_days").length;
  for (const value of ["g", "gi", "gist.github.com"]) {
    $("domain-input").value = value;
    $("domain-input").dispatch("input");
  }
  assert.equal(timers.count, 1, "one timer: each key restarts it");
  assert.equal(host.of("history_list").length, lists, "nothing is sent while typing");
  timers.fire();
  await settle(10);
  assert.equal(host.of("history_list").length, lists + 1);
  assert.equal(host.of("history_days").length, days + 1);
  assert.equal(lastList(host).domain, "gist.github.com");
  assert.deepEqual(win.log, [["replace", "?date=2026-11-17&domain=gist.github.com"]], "typing replaces the entry");
  assert.deepEqual(rowsOf($).map((r) => r.dataset.visitId), ["v3"]);
  assert.deepEqual(marked($), [17], "only the days of that domain");
  assert.match(text($("result-count")), /条件に一致する訪問 1件/);
});

test("Enter applies the domain at once; an unchanged domain asks for nothing", async () => {
  const { $, host, timers } = await openHistory({ visits: { A: FULL } });
  $("domain-input").value = "example.org";
  $("domain-input").dispatch("input");
  $("domain-input").dispatch("keydown", { key: "Enter" });
  await settle(10);
  assert.equal(timers.count, 0);
  assert.equal(lastList(host).domain, "example.org");
  const lists = host.of("history_list").length;
  $("domain-input").dispatch("input");
  timers.fire();
  await settle(10);
  assert.equal(host.of("history_list").length, lists);
});

test("the mode is pushed with page 1 and applied to the list and the marks; blank means every domain", async () => {
  const { $, host, win } = await openHistory({ visits: { A: FULL } });
  $("domain-input").value = "^gist";
  $("domain-input").dispatch("keydown", { key: "Enter" });
  await settle(10);
  assert.deepEqual(rowsOf($).map((r) => r.dataset.visitId), [], "exact: no host is called ^gist");
  assert.match($("history-state-text").textContent, /条件に一致する訪問はありません/);
  $("mode-regex").dispatch("change");
  await settle(10);
  assert.equal(lastList(host).mode, "regex");
  assert.equal(host.of("history_days").at(-1).mode, "regex");
  assert.deepEqual(rowsOf($).map((r) => r.dataset.visitId), ["v3"]);
  assert.deepEqual(win.log.at(-1), ["push", "?date=2026-11-17&domain=%5Egist&mode=regex"]);
  assert.equal($("mode-regex").checked, true);
  assert.match($("domain-hint").textContent, /正規表現/);
  assert.equal($("domain-input").getAttribute("placeholder") ?? "", "");
});

test("a bad pattern is an error at the input, the old list is not left standing, and nothing falls back to every visit", async () => {
  const { $, host, timers } = await openHistory({ visits: { A: FULL } });
  $("mode-regex").dispatch("change");
  await settle(10);
  $("domain-input").value = "(";
  $("domain-input").dispatch("input");
  timers.fire();
  await settle(10);
  assert.equal($("domain-error").hidden, false);
  assert.match($("domain-error").textContent, /正規表現が正しくありません/);
  assert.equal($("domain-input").getAttribute("aria-invalid"), "true");
  assert.equal($("history-list").hidden, true);
  assert.equal(rowsOf($).length, 0, "no stale row");
  assert.match($("history-state-text").textContent, /検索条件を修正/);
  assert.deepEqual(marked($), [], "no stale marks either");
  host.errors.history_list = "unsupported_regex";
  $("domain-input").value = "(?=x)a";
  $("domain-input").dispatch("keydown", { key: "Enter" });
  await settle(10);
  assert.match($("domain-error").textContent, /先読み・後方参照/);
  $("domain-input").value = "github";
  $("domain-input").dispatch("keydown", { key: "Enter" });
  await settle(10);
  assert.equal($("domain-error").hidden, true);
  assert.equal(rowsOf($).length, 3);
  assert.equal(host.of("history_list").every((m) => m.requestId), true);
});

test("a search without any visit says so, and an empty day says that", async () => {
  const { $ } = await openHistory({ search: "?date=2026-11-10", visits: { A: FULL } });
  assert.match($("history-state-text").textContent, /この日の訪問履歴はありません/);
  assert.equal($("history-pager").hidden, true);
  assert.equal(withClass($("history-pager"), "page-button").length, 0, "no pager for no pages");
  assert.equal($("history-list").hidden, true);
});

// ---------------------------------------------------------------------------------------------- paging

const MANY = Array.from({ length: 230 }, (_, i) => visit(`p${String(i).padStart(3, "0")}`, A, `visit ${i}`, at(2026, 11, 17, 6 + Math.floor(i / 60), i % 60, 0)));

test("the pager shows the neighbours of the page, marks the current one and disables what is impossible", async () => {
  const { $ } = await openHistory({ search: "?date=2026-11-17&size=25&page=5", visits: { A: MANY } });
  const labels = withClass($("history-pager"), "page-button").map((b) => b.textContent);
  assert.deepEqual(labels, ["«", "‹", "1", "2", "3", "4", "5", "6", "7", "10", "›", "»"], "1 2 3 4 [5] 6 7 … 10");
  assert.equal(withClass($("history-pager"), "page-gap").length, 1);
  const current = withClass($("history-pager"), "is-current");
  assert.deepEqual(current.map((b) => [b.textContent, b.getAttribute("aria-current")]), [["5", "page"]]);
  assert.match(text($("result-count")), /230件（5 \/ 10ページ）/);
  const first = await openHistory({ search: "?date=2026-11-17&size=25", visits: { A: MANY } });
  const [firstButton, prevButton] = withClass(first.$("history-pager"), "page-button");
  assert.deepEqual([firstButton.disabled, prevButton.disabled], [true, true]);
  const last = await openHistory({ search: "?date=2026-11-17&size=25&page=10", visits: { A: MANY } });
  const buttons = withClass(last.$("history-pager"), "page-button");
  assert.deepEqual([buttons.at(-2).disabled, buttons.at(-1).disabled], [true, true]);
});

test("next, previous, first and last pages push only the page, and the focus stays on the current page's button", async () => {
  const { $, win, host, document } = await openHistory({ search: "?date=2026-11-17&size=25", visits: { A: MANY } });
  const button = (label) => withClass($("history-pager"), "page-button").find((b) => b.getAttribute("aria-label") === label);
  button("次のページ").dispatch("click");
  await settle(10);
  assert.deepEqual(win.log.at(-1), ["push", "?date=2026-11-17&page=2&size=25"]);
  assert.equal(lastList(host).page, 2);
  assert.equal(document.activeElement.getAttribute("aria-current"), "page", "the focus did not vanish with the old button");
  button("最後のページ").dispatch("click");
  await settle(10);
  assert.equal(lastList(host).page, 10);
  button("最初のページ").dispatch("click");
  await settle(10);
  assert.equal(lastList(host).page, 1);
  assert.deepEqual(win.log.map((entry) => entry[0]), ["push", "push", "push"]);
  withClass($("history-pager"), "page-button").find((b) => b.textContent === "3").dispatch("click");
  await settle(10);
  assert.equal(rowsOf($)[0].dataset.visitId, "p179", "page 3 of 25 starts at the 51st newest visit");
  assert.equal(rowsOf($).length, 25);
});

test("the page size is pushed with page 1 and sent to the host", async () => {
  const { $, win, host } = await openHistory({ search: "?date=2026-11-17&size=25&page=4", visits: { A: MANY } });
  $("size-select").value = "100";
  $("size-select").dispatch("change");
  await settle(10);
  assert.deepEqual(win.log.at(-1), ["push", "?date=2026-11-17&size=100"]);
  assert.deepEqual([lastList(host).page, lastList(host).size], [1, 100]);
  assert.equal(rowsOf($).length, 100);
  assert.match(text($("result-count")), /1 \/ 3ページ/);
  $("size-select").value = "50";
  $("size-select").dispatch("change");
  await settle(10);
  assert.deepEqual(win.log.at(-1), ["push", "?date=2026-11-17"], "the default is not written");
});

test("a page past the end is replaced by the last page once, with no new history entry and no loop", async () => {
  const { host, win, app, $ } = await openHistory({ search: "?date=2026-11-17&size=25&page=99", visits: { A: MANY } });
  assert.deepEqual(win.log, [["replace", "?date=2026-11-17&page=10&size=25"]]);
  assert.equal(win.entries.length, 1);
  assert.equal(host.of("history_list").length, 2, "the first answer was empty, the corrected request is the second");
  assert.equal(app.state.page, 10);
  assert.equal(rowsOf($).length, 5);
});

test("when everything of a day is gone the page is simply empty (pages 0), never an error", async () => {
  const { $, host, win, clock } = await openHistory({ search: "?date=2026-11-17&page=2", visits: { A: NOV17 } });
  assert.equal(rowsOf($).length, 4, "page 2 of one page: the last page (1) instead");
  host.visits.A = [];
  clock.advance(5000);
  win.focus();
  await settle(10);
  assert.match($("history-state-text").textContent, /この日の訪問履歴はありません/);
  assert.equal($("history-pager").hidden, true);
  assert.equal($("history-retry").hidden, true, "empty is not a failure");
});

// ---------------------------------------------------------------------------------------- back / forward

test("Back and Forward restore the search, the inputs and the calendar's month from the URL", async () => {
  const { $, win, app, host } = await openHistory({ search: "?date=2026-11-17", visits: { A: FULL } });
  cellOf($, 3).dispatch("click");
  await settle(10);
  $("mode-regex").dispatch("change");
  await settle(10);
  $("cal-prev").dispatch("click"); // the month moves; the entry does not
  await settle(10);
  assert.equal(win.entries.length, 3);
  win.back();
  await settle(10);
  assert.deepEqual([app.state.date.day, app.state.mode], [3, "exact"]);
  assert.equal($("mode-exact").checked, true);
  assert.deepEqual(app.shownMonth, { year: 2026, monthIndex: 10 }, "the month of the restored day");
  assert.equal($("cal-title").textContent, "2026年11月");
  win.back();
  await settle(10);
  assert.equal(app.state.date.day, 17);
  assert.equal(rowsOf($).length, 4);
  win.forward();
  win.forward();
  await settle(10);
  assert.equal(app.state.mode, "regex");
  assert.equal(win.entries.length, 3, "going back and forth wrote nothing");
  assert.deepEqual(win.log.map((e) => e[0]), ["push", "push"]);
  assert.equal(lastList(host).mode, "regex");
});

test("a reload is a new page over the same URL and shows the same search", async () => {
  const first = await openHistory({ search: "?date=2026-11-03&domain=github.com&mode=regex&page=1&size=25", visits: { A: FULL } });
  const again = await openHistory({ search: first.win.location.search, visits: { A: FULL } });
  assert.deepEqual(again.app.state, first.app.state);
  assert.deepEqual(rowsOf(again.$).map((r) => r.dataset.visitId), rowsOf(first.$).map((r) => r.dataset.visitId));
});

// ------------------------------------------------------------------------------------ stale answers

test("the answer of an earlier search never replaces the later one", async () => {
  const { $, host } = await openHistory({ visits: { A: FULL } });
  const held = [];
  host.gates.history_list = (message) => (message.domain === "example.org" ? new Promise((resolve) => held.push(resolve)) : Promise.resolve());
  $("domain-input").value = "example.org";
  $("domain-input").dispatch("keydown", { key: "Enter" });
  await settle(5);
  $("domain-input").value = "gist.github.com";
  $("domain-input").dispatch("keydown", { key: "Enter" });
  await settle(10);
  assert.deepEqual(rowsOf($).map((r) => r.dataset.visitId), ["v3"]);
  held.forEach((release) => release());
  await settle(10);
  assert.deepEqual(rowsOf($).map((r) => r.dataset.visitId), ["v3"], "the late answer is dropped");
});

test("a list answered for an earlier page is dropped when the person has moved on", async () => {
  const { $, host, win } = await openHistory({ search: "?date=2026-11-17&size=25", visits: { A: MANY } });
  const held = [];
  host.gates.history_list = (message) => (message.page === 2 ? new Promise((resolve) => held.push(resolve)) : Promise.resolve());
  withClass($("history-pager"), "page-button").find((b) => b.getAttribute("aria-label") === "次のページ").dispatch("click");
  await settle(5);
  win.back();
  await settle(10);
  held.forEach((release) => release());
  await settle(10);
  assert.equal(lastList(host).page, 1);
  assert.match(text($("result-count")), /1 \/ 10ページ/);
});

// -------------------------------------------------------------------------------------------- Vault

test("an answer of another Vault is never mixed in: the page drops what it shows and reads both again", async () => {
  const B = [visit("b1", "https://b.test/", "Other vault", at(2026, 11, 17, 8))];
  const { $, host, app, clock } = await openHistory({ visits: { A: FULL, B } });
  assert.equal(app.vaultToken, tokenOf("A"));
  host.vault = "B";
  clock.advance(5000);
  $("cal-prev").dispatch("click"); // any request of the new Vault is enough to notice
  await settle(20);
  assert.equal(app.vaultToken, tokenOf("B"));
  assert.deepEqual(rowsOf($).map((r) => r.dataset.visitId), ["b1"]);
  assert.deepEqual(marked($), [], "October of B has nothing");
  $("cal-next").dispatch("click");
  await settle(10);
  assert.deepEqual(marked($), [17]);
  assert.ok(!text($("history-list")).includes("First"), "nothing of A is left");
});

test("returning to the tab (visibility or focus) reads again, at most once a second, and not while hidden", async () => {
  const { $, host, document, win, clock } = await openHistory({ visits: { A: FULL } });
  const lists = host.of("history_list").length;
  clock.advance(5000);
  document.visibilityState = "hidden";
  document.dispatch("visibilitychange");
  win.focus();
  await settle(10);
  assert.equal(host.of("history_list").length, lists, "hidden: nothing");
  document.visibilityState = "visible";
  document.dispatch("visibilitychange");
  win.focus();
  await settle(10);
  assert.equal(host.of("history_list").length, lists + 1, "once, not twice");
  host.visits.A = host.visits.A.filter((v) => v.id !== "v1");
  clock.advance(5000);
  win.focus();
  await settle(10);
  assert.deepEqual(rowsOf($).map((r) => r.dataset.visitId), ["v4", "v3", "v2"], "a visit deleted elsewhere is gone");
});

test("a Vault that keeps changing stops the loop and offers a reload", async () => {
  const { $, host, clock } = await openHistory({ visits: { A: FULL, B: FULL } });
  const original = host.send;
  host.send = async (message) => {
    if (message.type.startsWith("history_")) host.vault = host.vault === "A" ? "B" : "A"; // every answer comes from the other Vault
    return original(message);
  };
  clock.advance(5000);
  $("cal-prev").dispatch("click");
  await settle(80);
  assert.match($("history-state-text").textContent, /何度も切り替わっている/);
  assert.equal($("history-retry").hidden, false);
  const before = host.of("history_list").length + host.of("history_days").length;
  await settle(50);
  assert.equal(host.of("history_list").length + host.of("history_days").length, before, "it stopped by itself");
  assert.ok(before < 20, "bounded");
});

// ----------------------------------------------------------------------------------- Host and errors

test("a Host without history_v1 is told so, nothing else is asked, and Retry asks the capability again", async () => {
  const { $, host } = await openHistory({ host: { capabilities: ["navigation_v1"] } });
  assert.match($("history-state-text").textContent, /対応していません/);
  assert.equal(host.of("history_list").length + host.of("history_days").length, 0);
  assert.equal($("history-retry").hidden, false);
  host.capabilities = ["history_v1"];
  $("history-retry").dispatch("click");
  await settle(10);
  assert.equal(host.of("host_capabilities").length, 2);
  assert.equal(host.of("history_list").length, 1);
});

test("an old Host that does not know host_capabilities is unsupported, not broken", async () => {
  const { $, host } = await openHistory({ host: { capabilitiesFail: true } });
  assert.match($("history-state-text").textContent, /対応していません/);
  assert.equal(host.of("history_list").length, 0);
});

test("a Host that cannot be reached is an error with a Retry, and there is no automatic retry", async () => {
  const { $, host } = await openHistory({ host: { errors: { host_capabilities: "unavailable" } } });
  assert.match($("history-state-text").textContent, /Native Hostに接続できません/);
  assert.equal($("history-retry").hidden, false);
  await settle(50);
  assert.equal(host.of("host_capabilities").length, 1, "nothing retries by itself");
  $("history-retry").dispatch("click");
  await settle(10);
  assert.equal(host.of("host_capabilities").length, 2);
  assert.equal(rowsOf($).length, 0);
});

test("no Vault, a locked Vault and a Vault of another schema are told apart, and the first two lead to the settings", async () => {
  for (const [code, pattern, settings] of [["vault_not_configured", /Vaultが未設定/, true], ["vault_locked", /ロックされています/, false], ["vault_schema_mismatch", /現在の形式に対応していません/, true]]) {
    const { $, settings: opened } = await openHistory({ host: { errors: { history_list: code } } });
    assert.match($("history-state-text").textContent, pattern, code);
    assert.equal($("history-settings").hidden, !settings, code);
    if (settings) { $("history-settings").dispatch("click"); assert.equal(opened.opened, 1); }
    assert.equal($("history-retry").hidden, false);
  }
});

test("an answer that is too large is told, with what to do, and no row is shown", async () => {
  const { $ } = await openHistory({ host: { errors: { history_list: "response_too_large" } } });
  assert.match($("history-state-text").textContent, /大きすぎて取得できません/);
  assert.equal(rowsOf($).length, 0);
});

test("an answer that breaks the contract is refused as such, and shows no rows", async () => {
  const { $ } = await openHistory({ visits: { A: FULL }, host: { gates: {} }, now: new Date(2026, 10, 17, 9, 30) , });
  assert.equal(rowsOf($).length, 4);
  // The same page again, now over a host whose list answer lies about its page count.
  const lying = await (async () => {
    const { createHistoryHost } = await import("./history-fixtures.js");
    return createHistoryHost({ visits: { A: FULL, B: [] } });
  })();
  const original = lying.send;
  lying.send = async (message) => {
    const response = await original(message);
    return message.type === "history_list" ? { ...response, pages: response.pages + 1 } : response;
  };
  const { createDocument } = await import("./fake-dom.js");
  const { createHistoryApp } = await import("../../extension/history-app.js");
  const { createWin, createTimers } = await import("./history-fixtures.js");
  const document = createDocument();
  const app = createHistoryApp({ document, win: createWin(""), send: lying.send, tabs: { create() {} }, now: () => new Date(2026, 10, 17, 9, 30), timers: createTimers() });
  await app.start();
  await settle(10);
  assert.match(document.querySelector("#history-state-text").textContent, /不正な応答/);
  assert.equal(document.querySelector("#history-list").children.length, 0);
});

test("a marks failure leaves the list alone and says so; a bad request id is an invalid response", async () => {
  const { $ } = await openHistory({ host: { errors: { history_days: "db_error" } }, visits: { A: FULL } });
  assert.equal(rowsOf($).length, 4);
  assert.equal($("cal-status").hidden, false);
  assert.deepEqual(marked($), []);
});

// -------------------------------------------------------------------------------------------- opening

test("a complete http(s) URL opens in a new tab by a link; a cut URL is fetched whole first; anything else never opens", async () => {
  const long = `https://long.test/${"a".repeat(3000)}`;
  const { $, host, tabs } = await openHistory({
    visits: { A: [visit("w1", "https://web.test/p", "Web", at(2026, 11, 17, 8)), visit("w2", long, "Long", at(2026, 11, 17, 9)), visit("w3", "javascript:alert(1)", "Script", at(2026, 11, 17, 10)), visit("w4", "chrome://settings", "Chrome", at(2026, 11, 17, 11)), visit("w5", "not a url", "Bad", at(2026, 11, 17, 12))] },
  });
  const rows = Object.fromEntries(rowsOf($).map((r) => [r.dataset.visitId, r]));
  const link = (id) => withClass(rows[id], "visit-link")[0];
  assert.equal(link("w1").tagName, "A");
  assert.deepEqual([link("w1").getAttribute("href"), link("w1").getAttribute("target"), link("w1").getAttribute("rel")], ["https://web.test/p", "_blank", "noopener noreferrer"]);
  assert.equal(link("w2").tagName, "BUTTON", "a cut URL is no link");
  assert.equal(link("w2").getAttribute("href"), null);
  for (const id of ["w3", "w4", "w5"]) {
    assert.equal(link(id).getAttribute("href"), null, id);
    assert.equal(link(id).getAttribute("aria-disabled"), "true", id);
    assert.equal(link(id).tagName, "SPAN", id);
  }
  host.fullUrls.w2 = long;
  link("w2").dispatch("click");
  await settle(10);
  assert.deepEqual(tabs.opened, [{ url: long }], "the whole URL, not the one cut for the list");
  assert.equal(host.of("history_visit").length, 1);
  // The full URL could not be had: nothing opens, and the cut one is never used.
  host.errors.history_visit = "visit_not_found";
  link("w2").dispatch("click");
  await settle(10);
  assert.equal(tabs.opened.length, 1);
  assert.match($("history-status").textContent, /開きません/);
  // A "full" URL that is no web URL never opens either.
  host.fullUrls.w2 = "javascript:alert(1)";
  link("w2").dispatch("click");
  await settle(10);
  assert.equal(tabs.opened.length, 1);
});

test("a cut title and URL say so; rows keep their visit id for the details of the next slice", async () => {
  const longTitle = "t".repeat(600);
  const { $ } = await openHistory({ visits: { A: [visit("c1", `https://c.test/${"x".repeat(2100)}`, longTitle, at(2026, 11, 17, 8))] } });
  const row = rowsOf($)[0];
  assert.equal(row.dataset.visitId, "c1");
  assert.ok(text(row).includes("…"));
  assert.equal(withClass(row, "visit-link")[0].textContent.endsWith("…"), true);
  // No expand control is offered yet.
  assert.equal(walk(row).some((e) => e.tagName === "BUTTON" && !e.className.includes("visit-link")), false);
});

// -------------------------------------------------------------------------------------------- security

test("a title, URL or host from the web is text only", async () => {
  const hostile = '<img src=x onerror="alert(1)"><script>alert(2)</script>';
  const { $ } = await openHistory({ visits: { A: [visit("h1", `https://evil.test/?q=${encodeURIComponent(hostile)}`, hostile, at(2026, 11, 17, 8)), visit("h2", "https://x.test/", "<b>bold</b>", at(2026, 11, 17, 9))] } });
  const tags = walk($("history-list")).map((e) => e.tagName);
  assert.ok(!tags.includes("IMG") && !tags.includes("SCRIPT") && !tags.includes("B"), tags.join());
  assert.ok(text(rowsOf($)[1]).includes(hostile));
  assert.ok(text(rowsOf($)[0]).includes("<b>bold</b>"));
  const sources = ["history-app.js", "history-view.js", "history-client.js", "history-state.js", "history.js"].map((f) => readFileSync(path.join(extensionDir, f), "utf8")).join("\n");
  assert.doesNotMatch(sources, /innerHTML|insertAdjacentHTML|outerHTML|document\.write|eval\(|new Function/);
});

test("a domain typed with markup is only ever sent as text and shown as text", async () => {
  const { $, host } = await openHistory({ visits: { A: FULL } });
  $("domain-input").value = "<img src=x onerror=alert(1)>";
  $("domain-input").dispatch("keydown", { key: "Enter" });
  await settle(10);
  assert.equal(lastList(host).domain, "<img src=x onerror=alert(1)>");
  assert.ok(!walk($("history-list")).some((e) => e.tagName === "IMG"));
});

// ------------------------------------------------------------------------------------------- the page

test("history.html declares every element the page uses, loads its script as a module and has no inline script or style", () => {
  const html = readFileSync(path.join(extensionDir, "history.html"), "utf8");
  for (const id of ["history-status", "cal-prev", "cal-next", "cal-title", "cal-table", "cal-jump", "cal-status", "domain-input", "mode-exact", "mode-regex", "domain-hint", "domain-error", "selected-heading", "result-count", "size-select", "history-state", "history-state-text", "history-retry", "history-settings", "history-list", "history-pager"]) {
    assert.match(html, new RegExp(`id="${id}"`), id);
  }
  assert.match(html, /placeholder="未入力ならすべてのドメイン"/);
  assert.match(html, /<script type="module" src="history\.js"><\/script>/);
  assert.doesNotMatch(html, /<script(?![^>]*src=)|\sstyle=|\son\w+=/i);
  assert.match(html, /<option value="25">/);
  assert.match(html, /<option value="50" selected>/);
  assert.match(html, /<option value="100">/);
  const manifest = JSON.parse(readFileSync(path.join(extensionDir, "manifest.json"), "utf8"));
  assert.ok(!manifest.permissions.some((p) => /history|bookmarks/.test(p)), "no new permission was needed");
});

test("the stylesheet stacks the page in a narrow window, wraps long text and follows the dark theme", () => {
  const css = readFileSync(path.join(extensionDir, "history.css"), "utf8");
  assert.match(css, /@media \(max-width: 720px\) \{\s*\.history-layout \{ grid-template-columns: minmax\(0, 1fr\)/);
  assert.match(css, /prefers-color-scheme: dark/);
  assert.match(css, /overflow-wrap: anywhere/);
  assert.match(css, /\.cal-day\.is-today \{ border-color/);
  assert.match(css, /\.cal-day\.is-selected \{ background/);
  assert.match(css, /forced-colors: active/);
  assert.match(css, /:focus-visible/);
});

// ----------------------------------------------------------------------------------- the side panel

test("the side panel opens History in a new tab, once for a double click, and the panel stays", async () => {
  const { $, host } = await loadSidePanel({ vaults: { A: [] }, active: "A" });
  globalThis.chrome.runtime.getURL = (file) => `chrome-extension://memocampus/${file}`;
  $("#open-history").dispatch("click");
  $("#open-history").dispatch("click");
  await settle(10);
  assert.deepEqual(host.openedTabs, [{ url: "chrome-extension://memocampus/history.html" }]);
  const html = readFileSync(path.join(extensionDir, "sidepanel.html"), "utf8");
  assert.match(html, /<button id="open-history" type="button" class="secondary"/);
});

