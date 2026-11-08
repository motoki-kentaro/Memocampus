// Issue #5, Slice 2: the visit calendar in the real side panel (sidepanel.js, visit-calendar.js) against
// the fake DOM and host. Each page of the fake host stands for one visit at its `lastVisitedAt`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { extensionDir, loadSidePanel, settle } from "./fake-dom.js";
import { localDayBounds, localToday, monthBounds } from "../../extension/visit-calendar.js";

const CALENDAR_HOST = ["navigation_v1", "visit_calendar_v1"];

/** A page visited at local `year-month-day hour:00` (monthIndex 0-11). */
const visited = (name, year, monthIndex, day, hour = 10) => ({
  url: `https://${name}.test/`,
  title: `${name} page`,
  lastVisitedAt: new Date(year, monthIndex, day, hour).toISOString(),
  lastCapturedAt: null,
  lastActivityAt: new Date(year, monthIndex, day, hour).toISOString(),
  source: "visit",
});

/** November 2026: two visits on the 8th (Sunday), one on the 3rd, the 14th (Saturday) and the 30th. */
const NOVEMBER = [
  visited("late", 2026, 10, 30, 23),
  visited("rust", 2026, 10, 14, 9),
  visited("rust-book", 2026, 10, 8, 23),
  visited("news", 2026, 10, 8, 0),
  visited("holiday", 2026, 10, 3),
  visited("october", 2026, 9, 31, 23),
];

/** A day's accessible name as the calendar writes it, today marked when the test runs on that day. */
function nameOf(base, year, monthIndex, day) {
  const today = localToday();
  return today.year === year && today.monthIndex === monthIndex && today.day === day ? `${base}、今日` : base;
}

const all = (root, cls) => root.children.flatMap((child) => [...(child.className.split(" ").includes(cls) ? [child] : []), ...all(child, cls)]);
const one = (root, cls) => all(root, cls)[0];
const popover = ($) => $("#visit-calendar");
const day = ($, number) => all(popover($), "calendar-day").find((button) => button.dataset.day === String(number));
const titles = ($) => $("#results").children.map((li) => li.children[0].children[0].textContent);
const searches = (host) => host.requests.filter((message) => message.type === "search_pages");
const dayRequests = (host) => host.requests.filter((message) => message.type === "visit_days");

async function panel({ vaults = { A: NOVEMBER }, active = "A", capabilities = CALENDAR_HOST, ...rest } = {}) {
  const loaded = await loadSidePanel({ vaults, active, capabilities, ...rest });
  await settle(30);
  return loaded;
}

async function openCalendar($) {
  $("#visit-calendar-open").dispatch("click");
  await settle(10);
}

/** Opens the calendar on `year-monthIndex` through the year-month picker. */
async function openOn($, year, monthIndex) {
  if (popover($).hidden) await openCalendar($);
  const title = one(popover($), "calendar-title");
  title.dispatch("click");
  const steps = year - Number.parseInt(title.textContent, 10); // the picker starts at the shown year
  const arrow = one(popover($), steps < 0 ? "calendar-prev" : "calendar-next");
  for (let i = 0; i < Math.abs(steps); i++) arrow.dispatch("click");
  all(popover($), "calendar-month")[monthIndex].dispatch("click");
  await settle(10);
}

async function choose($, number) {
  const grid = one(popover($), "calendar-grid");
  grid.dispatch("click", { target: day($, number) });
  await settle(20);
}

async function type($, text) {
  $("#query").value = text;
  $("#query").dispatch("input", { isComposing: false });
  await settle(260);
}

function openVault($, document, path) {
  $("#vault-path-input").value = path;
  document.querySelector('[data-manual="vault_open"]').dispatch("click");
}

// --- Capability -----------------------------------------------------------------------------------------

test("a host without visit_calendar_v1 gets no calendar and never a date range", async () => {
  const { $, host } = await panel({ capabilities: ["navigation_v1"] });
  const launcher = $("#visit-calendar-open");
  assert.equal(launcher.disabled, true);
  assert.match(launcher.title, /対応していません/);
  launcher.dispatch("click");
  await settle(10);
  assert.equal(popover($).hidden, true);
  await type($, "rust");
  assert.equal(dayRequests(host).length, 0);
  assert.ok(searches(host).every((message) => !("visitedFrom" in message) && !("visitedTo" in message)));
});

test("the launcher waits for the host's answer and a list from a vault; without a vault it stays off", async () => {
  const { $ } = await panel({ vaults: {}, active: null });
  assert.equal($("#visit-calendar-open").disabled, true);
  assert.match($("#visit-calendar-open").title, /Vault/);
  const ready = await panel();
  assert.equal(ready.$("#visit-calendar-open").disabled, false);
  assert.equal(ready.$("#visit-calendar-open").title, "閲覧日で絞り込み");
});

test("the markup: a labelled launcher at the start of the count row, a non-modal dialog below the toolbar", () => {
  const html = readFileSync(path.join(extensionDir, "sidepanel.html"), "utf8");
  const launcher = /<button id="visit-calendar-open"[^>]*>/.exec(html)[0];
  for (const attribute of ['aria-label="閲覧日で絞り込み"', 'aria-expanded="false"', 'aria-haspopup="dialog"', 'aria-controls="visit-calendar"', "disabled", 'type="button"']) {
    assert.ok(launcher.includes(attribute), attribute);
  }
  const row = html.slice(html.indexOf('<div class="toolbar-row">'), html.indexOf('<div id="record-status"'));
  const order = ["visit-calendar-open", "visit-date-chip", 'id="summary"', "record-tabs"].map((needle) => row.indexOf(needle));
  assert.ok(order.every((index, i) => index >= 0 && (i === 0 || index > order[i - 1])), "[calendar][date ×][count …][全タブを記録]");
  const dialog = /<section id="visit-calendar"[^>]*>/.exec(html)[0];
  for (const attribute of ['role="dialog"', 'aria-label="閲覧日で絞り込み"', "hidden"]) assert.ok(dialog.includes(attribute), attribute);
  assert.ok(!dialog.includes("aria-modal"), "non-modal");
  const toolbar = html.slice(html.indexOf('<header class="toolbar">'), html.indexOf("</header>"));
  assert.ok(toolbar.includes('<section id="visit-calendar"'), "inside the toolbar, so it hangs over the list without moving it");
  assert.ok(/<button id="visit-date-clear" type="button"/.test(html), "the chip's × is a real button (keyboard)");
  const css = readFileSync(path.join(extensionDir, "sidepanel.css"), "utf8");
  assert.match(css, /\.visit-calendar \{[^}]*position: absolute;[^}]*top: calc\(100% \+ 2px\);[^}]*width: min\(300px, calc\(100% - 16px\)\);[^}]*overflow: auto;[^}]*background: var\(--bg\);/s);
  assert.match(css, /\.toolbar \{\s*position: relative;/);
  for (const token of ["--sunday", "--saturday", "--on-accent"]) assert.equal(css.split(`${token}:`).length - 1, 2, `${token} for light and dark`);
});

// --- The popover ----------------------------------------------------------------------------------------

test("it opens on this month, asks for its counts each time, and Escape returns to the launcher", async () => {
  const { $, host, document } = await panel();
  const today = localToday();
  await openCalendar($);
  assert.equal(popover($).hidden, false);
  assert.equal($("#visit-calendar-open").getAttribute("aria-expanded"), "true");
  assert.equal(one(popover($), "calendar-title").textContent, `${today.year} ▼ ${today.monthIndex + 1}月`);
  assert.deepEqual(dayRequests(host).map((message) => message.bounds), [monthBounds(today.year, today.monthIndex)]);
  assert.ok(popover($).contains(document.activeElement), "focus is in the calendar");
  const todayButton = day($, today.day);
  assert.match(todayButton.className, /is-today/);
  assert.equal(todayButton.getAttribute("aria-current"), "date");
  assert.match(todayButton.getAttribute("aria-label"), /、今日$/);

  popover($).dispatch("keydown", { key: "Escape" });
  assert.equal(popover($).hidden, true);
  assert.equal($("#visit-calendar-open").getAttribute("aria-expanded"), "false");
  assert.equal(document.activeElement, $("#visit-calendar-open"));
  await openCalendar($);
  assert.equal(dayRequests(host).length, 2, "no cache: opening again asks again");
});

test("November 2026: Sunday first, colored weekends, only days with visits selectable, a dot each", async () => {
  const { $ } = await panel();
  await openOn($, 2026, 10);
  const grid = one(popover($), "calendar-grid");
  const headers = grid.children[0].children[0].children;
  assert.deepEqual(headers.map((th) => th.textContent), ["日", "月", "火", "水", "木", "金", "土"]);
  assert.equal(headers[0].className, "is-sun");
  assert.equal(headers[6].className, "is-sat");
  assert.equal(headers[0].getAttribute("abbr"), "日曜日");
  const rows = grid.children[1].children;
  assert.equal(rows.length, 5);
  assert.equal(rows[0].children[0].children[0].dataset.day, "1", "the 1st is a Sunday");
  assert.equal(all(popover($), "calendar-day").length, 30);
  const enabled = all(popover($), "calendar-day").filter((button) => !button.disabled).map((button) => Number(button.dataset.day));
  assert.deepEqual(enabled, [3, 8, 14, 30], "a visit at 23:00 on the 30th is the 30th, at 0:00 on the 8th the 8th");
  assert.equal(day($, 8).getAttribute("aria-label"), nameOf("11月8日（日）閲覧2件", 2026, 10, 8));
  assert.equal(day($, 14).getAttribute("aria-label"), nameOf("11月14日（土）閲覧1件", 2026, 10, 14));
  assert.equal(day($, 3).getAttribute("aria-label"), nameOf("11月3日（火）閲覧1件", 2026, 10, 3), "no holiday treatment");
  assert.equal(day($, 9).getAttribute("aria-label"), nameOf("11月9日（月）閲覧なし", 2026, 10, 9));
  assert.match(day($, 8).className, /is-sun/);
  assert.match(day($, 14).className, /is-sat/);
  assert.doesNotMatch(day($, 3).className, /is-sun|is-sat/);
  // One dot of the same size for 1 or 2 visits; none without.
  assert.equal(all(day($, 8), "calendar-dot").length, 1);
  assert.equal(all(day($, 3), "calendar-dot").length, 1);
  assert.equal(all(day($, 9), "calendar-dot").length, 0);
  assert.equal(all(day($, 8), "calendar-dot")[0].getAttribute("aria-hidden"), "true");
  assert.match(one(popover($), "calendar-status").textContent, /4日/);
});

test("the year-month picker moves years and months without searching until a day is chosen", async () => {
  const { $, host } = await panel();
  await openCalendar($);
  const searchesBefore = searches(host).length;
  const daysBefore = dayRequests(host).length;
  const title = one(popover($), "calendar-title");
  title.dispatch("click");
  assert.equal(title.getAttribute("aria-expanded"), "true");
  assert.equal(one(popover($), "calendar-months").hidden, false);
  assert.equal(one(popover($), "calendar-grid").hidden, true);
  const monthsShown = all(popover($), "calendar-month");
  assert.deepEqual(monthsShown.map((button) => button.textContent), ["1月", "2月", "3月", "4月", "5月", "6月", "7月", "8月", "9月", "10月", "11月", "12月"]);
  assert.equal(monthsShown[localToday().monthIndex].getAttribute("aria-current"), "true");
  one(popover($), "calendar-prev").dispatch("click");
  one(popover($), "calendar-prev").dispatch("click");
  assert.equal(title.textContent, `${localToday().year - 2} ▲`);
  assert.equal(one(popover($), "calendar-prev").getAttribute("aria-label"), "前の年");
  assert.ok(monthsShown.every((button) => button.getAttribute("aria-current") === null));
  assert.equal(dayRequests(host).length, daysBefore, "moving years asks nothing");
  // Closing the picker keeps the month as it was.
  title.dispatch("click");
  assert.equal(title.textContent, `${localToday().year} ▼ ${localToday().monthIndex + 1}月`);
  assert.equal(one(popover($), "calendar-prev").getAttribute("aria-label"), "前の月");
  // Choosing a month shows it and asks for it.
  await openOn($, 2026, 10);
  assert.equal(title.textContent, "2026 ▼ 11月");
  assert.deepEqual(dayRequests(host).at(-1).bounds, monthBounds(2026, 10));
  // ◀ ▶ move a month, across years.
  await openOn($, 2026, 11);
  one(popover($), "calendar-next").dispatch("click");
  await settle(10);
  assert.equal(title.textContent, "2027 ▼ 1月");
  assert.deepEqual(dayRequests(host).at(-1).bounds, monthBounds(2027, 0));
  one(popover($), "calendar-prev").dispatch("click");
  one(popover($), "calendar-prev").dispatch("click");
  await settle(10);
  assert.equal(title.textContent, "2026 ▼ 11月");
  assert.equal(searches(host).length, searchesBefore, "the list is not searched by moving months");
});

test("a click outside closes it without moving focus; focus leaving closes it; the launcher toggles", async () => {
  const { $, document } = await panel();
  await openCalendar($);
  $("#query").focus();
  document.dispatch("pointerdown", { target: $("#query") });
  assert.equal(popover($).hidden, true);
  assert.equal(document.activeElement, $("#query"));
  await openCalendar($);
  document.dispatch("pointerdown", { target: one(popover($), "calendar-next") });
  assert.equal(popover($).hidden, false, "a click inside keeps it");
  document.dispatch("pointerdown", { target: $("#visit-calendar-open") });
  assert.equal(popover($).hidden, false, "the launcher's own press does not close it first");
  $("#visit-calendar-open").dispatch("click");
  assert.equal(popover($).hidden, true, "and its click closes it");
  await openCalendar($);
  popover($).dispatch("focusout", { relatedTarget: $("#record-tabs") });
  assert.equal(popover($).hidden, true, "Tab out of the calendar closes it");
  await openCalendar($);
  popover($).dispatch("focusout", { relatedTarget: null });
  assert.equal(popover($).hidden, false, "leaving the window does not");
  $("#tab-image").dispatch("click");
  assert.equal(popover($).hidden, true, "another tab closes it");
});

// --- Choosing a day ---------------------------------------------------------------------------------------

test("choosing a day closes the calendar, shows YYYY/MM/DD × and narrows the list to that local day", async () => {
  const { $, host, document } = await panel();
  await openOn($, 2026, 10);
  await choose($, 8);
  assert.equal(popover($).hidden, true);
  assert.equal(document.activeElement, $("#visit-calendar-open"));
  assert.equal($("#visit-date-chip").hidden, false);
  assert.equal($("#visit-date-chip-text").textContent, "2026/11/08");
  assert.equal($("#visit-date-clear").getAttribute("aria-label"), "日付の絞り込みを解除（2026/11/08）");
  const sent = searches(host).at(-1);
  assert.deepEqual({ from: sent.visitedFrom, to: sent.visitedTo, offset: sent.offset }, { from: localDayBounds(2026, 10, 8).visitedFrom, to: localDayBounds(2026, 10, 8).visitedTo, offset: 0 });
  assert.deepEqual(titles($), ["rust-book page", "news page"]);
  assert.equal($("#summary").textContent, "2件（2026/11/08）");

  // The query narrows the day further (AND), and the day stays.
  await type($, "rust");
  assert.deepEqual(titles($), ["rust-book page"]);
  assert.equal(searches(host).at(-1).visitedFrom, localDayBounds(2026, 10, 8).visitedFrom);
  assert.equal(searches(host).at(-1).query, "rust");
  assert.equal($("#visit-date-chip-text").textContent, "2026/11/08");
  await type($, "nothing");
  assert.equal($("#empty").textContent, "「nothing」に一致する、2026/11/08に閲覧したページはありません。");
  assert.equal($("#summary").textContent, "0件（2026/11/08）");

  // × goes back to the plain search with the same query.
  await type($, "rust");
  $("#visit-date-clear").dispatch("click");
  await settle(20);
  assert.equal($("#visit-date-chip").hidden, true);
  assert.equal(document.activeElement, $("#visit-calendar-open"));
  const plain = searches(host).at(-1);
  assert.equal(plain.query, "rust");
  assert.ok(!("visitedFrom" in plain) && !("visitedTo" in plain));
  assert.deepEqual(titles($), ["rust page", "rust-book page"]);
  assert.equal($("#summary").textContent, "2件");
});

test("reopening shows the chosen day's month with that day marked; months move without touching the filter", async () => {
  const { $, host } = await panel();
  await openOn($, 2026, 10);
  await choose($, 14);
  await openCalendar($);
  assert.equal(one(popover($), "calendar-title").textContent, "2026 ▼ 11月");
  assert.match(day($, 14).className, /is-selected/);
  assert.equal(day($, 14).getAttribute("aria-label"), `${nameOf("11月14日（土）閲覧1件", 2026, 10, 14)}、選択中`);
  assert.equal(day($, 14).tabIndex, 0, "the chosen day holds the tab stop");
  assert.ok(all(popover($), "calendar-day").filter((button) => button !== day($, 14)).every((button) => button.tabIndex === -1));
  const before = searches(host).length;
  one(popover($), "calendar-prev").dispatch("click");
  await settle(10);
  assert.equal(searches(host).length, before);
  assert.equal($("#visit-date-chip-text").textContent, "2026/11/14");
  // October has the visit of the 31st.
  assert.deepEqual(all(popover($), "calendar-day").filter((button) => !button.disabled).map((button) => button.dataset.day), ["31"]);
});

test("paging goes on inside the day; another day starts a new list", async () => {
  const many = Array.from({ length: 70 }, (_, i) => visited(`p${i}`, 2026, 10, 8, i % 24));
  const { $, host } = await panel({ vaults: { A: [...many, visited("other", 2026, 10, 9)] } });
  await openOn($, 2026, 10);
  assert.equal(day($, 8).getAttribute("aria-label"), nameOf("11月8日（日）閲覧70件", 2026, 10, 8));
  await choose($, 8);
  assert.equal($("#results").children.length, 50);
  assert.equal($("#summary").textContent, "50件以上（2026/11/08）");
  $("#load-more").dispatch("click");
  await settle(20);
  const next = searches(host).at(-1);
  assert.equal(next.offset, 50);
  assert.equal(next.visitedFrom, localDayBounds(2026, 10, 8).visitedFrom);
  assert.equal($("#results").children.length, 70);
  assert.equal($("#summary").textContent, "70件（2026/11/08）");
  await openCalendar($);
  await choose($, 9);
  assert.deepEqual(titles($), ["other page"], "the new day's list starts empty (no URL kept as seen)");
  assert.equal(searches(host).at(-1).offset, 0);
  assert.equal($("#load-more").hidden, true);
});

// --- Keyboard -------------------------------------------------------------------------------------------

test("arrow keys, Home and End move one tab stop among the selectable days", async () => {
  const { $, document } = await panel();
  await openOn($, 2026, 10);
  const grid = one(popover($), "calendar-grid");
  const key = (name) => grid.dispatch("keydown", { key: name, target: document.activeElement });
  day($, 3).focus();
  key("ArrowRight");
  assert.equal(document.activeElement, day($, 8));
  key("ArrowRight");
  assert.equal(document.activeElement, day($, 14));
  key("ArrowLeft");
  assert.equal(document.activeElement, day($, 8));
  key("End");
  assert.equal(document.activeElement, day($, 14), "the week's last selectable day");
  key("Home");
  assert.equal(document.activeElement, day($, 8));
  key("ArrowUp");
  assert.equal(document.activeElement, day($, 8), "the 1st has no visit: stays");
  key("ArrowDown");
  assert.equal(document.activeElement, day($, 8), "no later Sunday with visits: stays");
  key("ArrowRight");
  key("ArrowRight");
  assert.equal(document.activeElement, day($, 30));
  key("ArrowRight");
  assert.equal(document.activeElement, day($, 30), "the month's edge is not crossed");
  assert.equal(all(popover($), "calendar-day").filter((button) => button.tabIndex === 0).length, 1);
  assert.equal(day($, 30).tabIndex, 0);
  // Enter or Space on the focused day is the native button's click.
  await choose($, 30);
  assert.equal($("#visit-date-chip-text").textContent, "2026/11/30");
});

// --- Late answers, vaults and the host --------------------------------------------------------------------

test("moving months quickly shows only the latest month's counts", async () => {
  const { $, host } = await panel();
  await openOn($, 2026, 10);
  // October answers slowly, December at once: the late October answer is dropped.
  host.daysOverride = (message, reply) => (message.bounds[0] === monthBounds(2026, 9)[0] ? new Promise((resolve) => setTimeout(() => resolve({ ...reply, counts: reply.counts.map(() => 9) }), 60)) : reply);
  one(popover($), "calendar-prev").dispatch("click");
  one(popover($), "calendar-next").dispatch("click");
  one(popover($), "calendar-next").dispatch("click");
  await settle(120);
  assert.equal(one(popover($), "calendar-title").textContent, "2026 ▼ 12月");
  assert.ok(all(popover($), "calendar-day").every((button) => button.disabled), "December has no visits; October's 9s never appear");
  assert.match(one(popover($), "calendar-status").textContent, /ありません/);
});

test("an answer that arrives after closing is not kept for the next opening", async () => {
  const { $, host } = await panel();
  host.daysDelay.A = 60;
  await openCalendar($);
  popover($).dispatch("keydown", { key: "Escape" });
  host.daysDelay.A = 200;
  await settle(80); // the first answer arrives while closed
  await openCalendar($);
  assert.ok(all(popover($), "calendar-day").every((button) => button.disabled), "still loading: nothing from the closed popover");
  assert.equal(one(popover($), "calendar-grid").getAttribute("aria-busy"), "true");
  await settle(220);
  assert.equal(one(popover($), "calendar-grid").getAttribute("aria-busy"), "false");
});

test("a vault switch drops the chosen day, the open calendar and late counts of the old vault", async () => {
  const B = [visited("b", 2026, 10, 8)];
  const { $, host, document } = await panel({ vaults: { A: NOVEMBER, B } });
  await openOn($, 2026, 10);
  await choose($, 8);
  host.daysDelay.A = 80;
  await openCalendar($);
  const sentBefore = host.requests.length;
  openVault($, document, "B"); // (the settings panel's controls; the URL tab stays shown)
  await settle(20);
  assert.ok(host.requests.slice(sentBefore).every((message) => !("visitedFrom" in message)), "A's day is never sent to B");
  assert.equal($("#url-item-status").hidden, true, "dropped by the switch itself, not found out later");
  assert.equal(popover($).hidden, true, "closed by the switch");
  assert.equal($("#visit-date-chip").hidden, true, "the day was the old vault's");
  await settle(120); // A's late counts arrive now
  const last = searches(host).at(-1);
  assert.ok(!("visitedFrom" in last), "the new vault's list is not filtered");
  assert.deepEqual(titles($), ["b page"]);
  await openOn($, 2026, 10);
  assert.equal(day($, 8).getAttribute("aria-label"), nameOf("11月8日（日）閲覧1件", 2026, 10, 8), "B's counts, not A's 2");
  assert.ok(day($, 14).disabled);
});

test("counts of another vault than the list's are never shown, and the list starts over", async () => {
  const B = [visited("b", 2026, 10, 20)];
  const { $, host } = await panel({ vaults: { A: NOVEMBER, B } });
  await openOn($, 2026, 10);
  await choose($, 8);
  // Another window opens vault B; this panel still lists A.
  host.active = "B";
  await openCalendar($);
  await settle(30);
  assert.equal(popover($).hidden, true, "B's counts are not shown as A's");
  assert.equal($("#visit-date-chip").hidden, true);
  assert.match($("#url-item-status").textContent, /Vaultが切り替えられた/);
  assert.deepEqual(titles($), ["b page"], "the list is now B's, unfiltered");
  assert.ok(!("visitedFrom" in searches(host).at(-1)));
  await openOn($, 2026, 10);
  assert.ok(!day($, 20).disabled, "B's own counts after the list followed");
  assert.equal(dayRequests(host).at(-1).bounds.length, 31);
});

test("a day chosen in one vault is not applied to another vault's search", async () => {
  const B = [visited("b-rust", 2026, 10, 8), visited("b-other", 2026, 10, 2)];
  const { $, host } = await panel({ vaults: { A: NOVEMBER, B } });
  await openOn($, 2026, 10);
  await choose($, 8);
  host.active = "B"; // switched elsewhere, unseen
  await type($, "b");
  await settle(30);
  assert.equal($("#visit-date-chip").hidden, true, "the day of vault A is dropped");
  assert.ok(!("visitedFrom" in searches(host).at(-1)));
  assert.deepEqual(titles($), ["b-rust page", "b-other page"]);
  assert.equal(host.requests.filter((message) => message.type === "search_pages" && message.visitedFrom).at(-1).query, "b", "only the refused search carried the day");
});

test("a stopped host leaves no counts behind; after it is back the calendar reads afresh", async () => {
  const { $, host } = await panel();
  await openOn($, 2026, 10);
  assert.ok(!day($, 8).disabled);
  popover($).dispatch("keydown", { key: "Escape" });
  host.disconnected = true;
  await openOn($, 2026, 10);
  assert.ok(all(popover($), "calendar-day").every((button) => button.disabled), "the counts seen before do not come back");
  assert.match(one(popover($), "calendar-status").textContent, /Native Hostに接続できません/);
  const retry = one(popover($), "calendar-retry");
  assert.equal(retry.hidden, false);
  host.disconnected = false;
  retry.dispatch("click");
  await settle(20);
  assert.ok(!day($, 8).disabled);
  assert.equal(retry.hidden, true);
});

test("a malformed or failed count answer shows no days", async () => {
  const { $, host } = await panel();
  host.daysOverride = (_message, reply) => ({ ...reply, counts: reply.counts.slice(1) });
  await openOn($, 2026, 10);
  assert.ok(all(popover($), "calendar-day").every((button) => button.disabled));
  assert.match(one(popover($), "calendar-status").textContent, /不正な応答/);
  host.daysOverride = (_message, reply) => ({ ok: false, type: "visit_days_result", requestId: reply.requestId, error: "db_error" });
  one(popover($), "calendar-retry").dispatch("click");
  await settle(20);
  assert.ok(all(popover($), "calendar-day").every((button) => button.disabled));
  assert.match(one(popover($), "calendar-status").textContent, /データベース/);
  host.daysOverride = (_message, reply) => ({ ...reply, vaultToken: "zz".repeat(32) });
  one(popover($), "calendar-retry").dispatch("click");
  await settle(20);
  assert.ok(all(popover($), "calendar-day").every((button) => button.disabled), "a token of no valid form is not the list's");
});

test("new visits show when the calendar is opened again, or when the panel becomes visible", async () => {
  const { $, host, document } = await panel();
  await openOn($, 2026, 10);
  assert.ok(day($, 20).disabled);
  popover($).dispatch("keydown", { key: "Escape" });
  host.vaults.A.push(visited("new", 2026, 10, 20));
  await openOn($, 2026, 10);
  assert.equal(day($, 20).getAttribute("aria-label"), nameOf("11月20日（金）閲覧1件", 2026, 10, 20));
  host.vaults.A.push(visited("newer", 2026, 10, 20, 11));
  document.dispatch("visibilitychange");
  await settle(20);
  assert.equal(day($, 20).getAttribute("aria-label"), nameOf("11月20日（金）閲覧2件", 2026, 10, 20));
  assert.equal(popover($).hidden, false);
});

// --- With the real host -------------------------------------------------------------------------------
// Opt-in (MEMOCAMPUS_TEST_HOST): the side panel's real code drives the real host binary, one process
// per message as Chrome does, in a temporary MEMOCAMPUS_APP_DIR with a fresh vault. Nothing touches the
// user's vault, policy or Chrome profile.

const HOST_BIN = process.env.MEMOCAMPUS_TEST_HOST;

test("end to end with the real host: counts per local day, a day's list AND a query, back to all", { skip: HOST_BIN ? false : "set MEMOCAMPUS_TEST_HOST to run" }, async () => {
  const { spawn } = await import("node:child_process");
  const { mkdtempSync, rmSync } = await import("node:fs");
  const os = await import("node:os");
  const { DatabaseSync } = await import("node:sqlite");
  const appDir = mkdtempSync(path.join(os.tmpdir(), "memocampus-calendar-e2e-"));
  const env = { ...process.env, MEMOCAMPUS_APP_DIR: appDir };
  delete env.MEMOCAMPUS_DB;
  let closing = false;
  const call = (message) =>
    new Promise((resolve, reject) => {
      if (closing) return reject(new Error("test finished"));
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
  const vault = path.join(appDir, "memocampus.sqlite3");
  try {
    assert.equal((await call({ type: "vault_create", requestId: "c", useDefault: true })).ok, true);
    const db = new DatabaseSync(vault);
    try {
      const visit = db.prepare("INSERT INTO visits VALUES (?, ?, ?, ?)");
      // Local times of this machine's zone: the first instant of the 8th, its last hour, the 9th.
      visit.run("v1", "https://rust.test/", "Rust", new Date(2026, 10, 8, 0, 0, 0, 0).toISOString());
      visit.run("v2", "https://rust.test/", "Rust", new Date(2026, 10, 8, 23, 59, 59, 999).toISOString());
      visit.run("v3", "https://news.test/", "News", new Date(2026, 10, 8, 12).toISOString());
      visit.run("v4", "https://rust.test/", "Rust again", new Date(2026, 10, 9, 0).toISOString());
      visit.run("v5", "https://old.test/", "Old", new Date(2026, 9, 31, 23, 59).toISOString());
      db.prepare("INSERT INTO tab_snapshots VALUES ('s1', 1, 0, 'https://snap.test/', 'Snap', 0, 0, ?)").run(new Date(2026, 10, 8, 10).toISOString());
    } finally {
      db.close();
    }
    const { $, host } = await loadSidePanel({ vaults: { [vault]: [] }, active: vault, capabilities: CALENDAR_HOST, forward: call });
    await settle(2500);
    assert.equal($("#visit-calendar-open").disabled, false, "the real host names visit_calendar_v1");
    $("#visit-calendar-open").dispatch("click");
    await settle(1500);
    const title = one(popover($), "calendar-title");
    title.dispatch("click");
    const steps = 2026 - Number.parseInt(title.textContent, 10);
    for (let i = 0; i < Math.abs(steps); i++) one(popover($), steps < 0 ? "calendar-prev" : "calendar-next").dispatch("click");
    all(popover($), "calendar-month")[10].dispatch("click");
    await settle(1500);
    const counted = all(popover($), "calendar-day").filter((button) => !button.disabled).map((button) => [Number(button.dataset.day), button.getAttribute("aria-label")]);
    assert.deepEqual(counted.map(([number]) => number), [8, 9], "the snapshot and October's last minute are not November visits");
    assert.equal(counted[0][1], nameOf("11月8日（日）閲覧3件", 2026, 10, 8), "every visit counted, the day's first and last instants included");
    await choose($, 8);
    await settle(1500);
    assert.deepEqual(titles($).sort(), ["News", "Rust"], "the 8th's visits only, one row per URL, its title of that day");
    assert.equal($("#summary").textContent, "2件（2026/11/08）");
    await type($, "rust");
    await settle(1500);
    assert.deepEqual(titles($), ["Rust"]);
    $("#visit-date-clear").dispatch("click");
    await settle(1500);
    assert.deepEqual(titles($), ["Rust again"], "the plain search: the URL's latest title");
    assert.ok(host.requests.filter((message) => message.type === "search_pages").some((message) => message.visitedFrom === localDayBounds(2026, 10, 8).visitedFrom));
  } finally {
    closing = true;
    await settle(500);
    rmSync(appDir, { recursive: true, force: true });
  }
});
