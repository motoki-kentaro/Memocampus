// Issue #5, Slice 2: the date logic of the visit calendar and its host messages.
//
// Local days depend on the time zone, so the boundary tests run under named zones (process.env.TZ,
// which Node applies at once) and always restore the zone they found. Each assertion is about the
// zone it names, never about the zone of the machine running the tests.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { extensionDir } from "./fake-dom.js";
import { SearchError, searchPages, visitDays } from "../../extension/search-client.js";
import {
  MIN_YEAR,
  addMonths,
  calendarWeeks,
  dayLabel,
  daysInMonth,
  formatDate,
  localDayBounds,
  localToday,
  monthBounds,
  moveDay,
  weekdayOf,
} from "../../extension/visit-calendar.js";

const HOUR = 3_600_000;
const zoneNow = () => Intl.DateTimeFormat().resolvedOptions().timeZone;

/** Runs `body` with the process in time zone `zone`, then puts back exactly what was there. */
function inZone(zone, body) {
  const original = process.env.TZ;
  const before = zoneNow();
  try {
    process.env.TZ = zone;
    assert.equal(zoneNow(), zone, `Node applied ${zone}`);
    return body();
  } finally {
    // Deleting TZ alone would leave the last zone in effect: name the original zone, then unset.
    process.env.TZ = original ?? before;
    if (original === undefined) delete process.env.TZ;
    assert.equal(zoneNow(), before, "the original zone is back");
  }
}

/** Length in hours of each local day of a month (from its bounds). */
const dayLengths = (year, monthIndex) => {
  const bounds = monthBounds(year, monthIndex).map((text) => Date.parse(text));
  return bounds.slice(1).map((end, i) => (end - bounds[i]) / HOUR);
};

// --- Civil calendar (no time zone) --------------------------------------------------------------------

test("days in a month: 28, 29 in leap years (not 2100, yes 2000), 30 and 31", () => {
  assert.equal(daysInMonth(2026, 1), 28);
  assert.equal(daysInMonth(2028, 1), 29);
  assert.equal(daysInMonth(2100, 1), 28);
  assert.equal(daysInMonth(2000, 1), 29);
  assert.equal(daysInMonth(2026, 3), 30);
  assert.equal(daysInMonth(2026, 10), 30);
  assert.equal(daysInMonth(2026, 0), 31);
  assert.equal(daysInMonth(2026, 11), 31);
});

test("months move across years both ways", () => {
  assert.deepEqual(addMonths(2026, 11, 1), { year: 2027, monthIndex: 0 });
  assert.deepEqual(addMonths(2027, 0, -1), { year: 2026, monthIndex: 11 });
  assert.deepEqual(addMonths(2026, 10, -23), { year: 2024, monthIndex: 11 });
  assert.deepEqual(addMonths(2026, 10, 14), { year: 2028, monthIndex: 0 });
});

test("weeks start on Sunday and a month takes 4 to 6 of them", () => {
  // February 2026 starts on a Sunday and has 28 days: exactly four weeks.
  const february = calendarWeeks(2026, 1);
  assert.equal(february.length, 4);
  assert.deepEqual(february[0], [1, 2, 3, 4, 5, 6, 7]);
  assert.deepEqual(february[3], [22, 23, 24, 25, 26, 27, 28]);
  // November 2026: Sunday the 1st, 30 days, five weeks.
  const november = calendarWeeks(2026, 10);
  assert.equal(november.length, 5);
  assert.deepEqual(november[4], [29, 30, null, null, null, null, null]);
  // August 2026: Saturday the 1st, 31 days, six weeks.
  const august = calendarWeeks(2026, 7);
  assert.equal(august.length, 6);
  assert.deepEqual(august[0], [null, null, null, null, null, null, 1]);
  assert.deepEqual(august[5], [30, 31, null, null, null, null, null]);
  // Every day once, in order, in the column of its weekday.
  for (const [year, monthIndex] of [[2026, 1], [2028, 1], [2026, 7], [2026, 10], [2026, 11]]) {
    const weeks = calendarWeeks(year, monthIndex);
    assert.deepEqual(weeks.flat().filter(Boolean), Array.from({ length: daysInMonth(year, monthIndex) }, (_, i) => i + 1));
    for (const week of weeks) for (const [column, day] of week.entries()) if (day) assert.equal(weekdayOf(year, monthIndex, day), column);
  }
});

test("weekdays and dates do not depend on the time zone", () => {
  for (const zone of ["Asia/Tokyo", "America/New_York", "Pacific/Kiritimati", "Pacific/Pago_Pago"]) {
    inZone(zone, () => {
      assert.equal(weekdayOf(2026, 10, 8), 0, zone);
      assert.equal(daysInMonth(2028, 1), 29, zone);
      assert.equal(calendarWeeks(2026, 10)[0][0], 1, zone);
    });
  }
});

test("dates are written YYYY/MM/DD, from local parts", () => {
  assert.equal(formatDate(2026, 10, 8), "2026/11/08");
  assert.equal(formatDate(2027, 0, 1), "2027/01/01");
  assert.equal(formatDate(2026, 11, 31), "2026/12/31");
  // Just after local midnight in Tokyo it is still the previous day in UTC: the local day counts.
  inZone("Asia/Tokyo", () => {
    const now = new Date("2026-11-07T15:30:00.000Z");
    assert.deepEqual(localToday(now), { year: 2026, monthIndex: 10, day: 8 });
    assert.equal(now.toISOString().slice(0, 10), "2026-11-07", "what the UTC date would have said");
  });
});

test("a day's accessible name has its date, weekday, visits, and today or chosen", () => {
  assert.equal(dayLabel(2026, 10, 8, 12), "11月8日（日）閲覧12件");
  assert.equal(dayLabel(2026, 10, 14, 1), "11月14日（土）閲覧1件");
  assert.equal(dayLabel(2026, 10, 9, 0), "11月9日（月）閲覧なし");
  assert.equal(dayLabel(2026, 10, 9, null), "11月9日（月）読み込み中");
  assert.equal(dayLabel(2026, 10, 8, 3, { today: true, selected: true }), "11月8日（日）閲覧3件、今日、選択中");
  // Japanese public holidays get nothing special: 11/3 (文化の日) is a Tuesday like any other.
  assert.equal(dayLabel(2026, 10, 3, 2), "11月3日（火）閲覧2件");
});

test("the keyboard moves among selectable days and stops at the month's edges", () => {
  // November 2026 with visits on 2, 3, 8, 10, 17, 30 (weeks: 1-7, 8-14, 15-21, 22-28, 29-30).
  const visits = new Set([2, 3, 8, 10, 17, 30]);
  const move = (day, key) => moveDay(2026, 10, day, key, (candidate) => visits.has(candidate));
  assert.equal(move(3, "ArrowRight"), 8, "skips days without visits");
  assert.equal(move(8, "ArrowLeft"), 3);
  assert.equal(move(2, "ArrowLeft"), null, "no earlier day: stays");
  assert.equal(move(30, "ArrowRight"), null, "the month's last: stays");
  assert.equal(move(3, "ArrowDown"), 10, "the same weekday a week later");
  assert.equal(move(10, "ArrowDown"), 17);
  assert.equal(move(17, "ArrowDown"), null, "24 has no visit and is the last Tuesday");
  assert.equal(move(17, "ArrowUp"), 10);
  assert.equal(move(2, "ArrowUp"), null, "no week before");
  assert.equal(move(10, "Home"), 8, "the first selectable day of the week");
  assert.equal(move(8, "Home"), null, "already the first");
  assert.equal(move(8, "End"), 10, "the last selectable day of the week");
  assert.equal(move(3, "End"), null, "nothing after it in that week");
  assert.equal(move(3, "Enter"), null);
  // Home and End go to the week's first and last selectable day, not to the nearest one.
  const full = new Set([8, 9, 10, 11]);
  const moveFull = (day, key) => moveDay(2026, 10, day, key, (candidate) => full.has(candidate));
  assert.equal(moveFull(11, "Home"), 8);
  assert.equal(moveFull(8, "End"), 11);
  assert.equal(moveFull(10, "End"), 11);
});

// --- Local day bounds in named time zones ---------------------------------------------------------------

test("Tokyo: a local day starts at 15:00 UTC of the day before, the month has days + 1 bounds", () => {
  inZone("Asia/Tokyo", () => {
    const bounds = monthBounds(2026, 10);
    assert.equal(bounds.length, 31);
    assert.equal(bounds[0], "2026-10-31T15:00:00.000Z");
    assert.equal(bounds[7], "2026-11-07T15:00:00.000Z", "Nov 8 starts on Nov 7 in UTC");
    assert.equal(bounds[30], "2026-11-30T15:00:00.000Z", "the start of December 1st");
    assert.deepEqual(localDayBounds(2026, 10, 8), { visitedFrom: "2026-11-07T15:00:00.000Z", visitedTo: "2026-11-08T15:00:00.000Z" });
    assert.ok(dayLengths(2026, 10).every((hours) => hours === 24));
  });
});

test("New York: the 23-hour and the 25-hour day", () => {
  inZone("America/New_York", () => {
    const march = dayLengths(2026, 2);
    assert.equal(march[7], 23, "Sunday March 8, 2026");
    assert.equal(march.filter((hours) => hours !== 24).length, 1);
    const november = dayLengths(2026, 10);
    assert.equal(november[0], 25, "Sunday November 1, 2026");
    assert.deepEqual(localDayBounds(2026, 10, 1), { visitedFrom: "2026-11-01T04:00:00.000Z", visitedTo: "2026-11-02T05:00:00.000Z" });
    // The next day's start is the next local midnight, never start + 24 hours.
    assert.notEqual(Date.parse(localDayBounds(2026, 10, 1).visitedTo) - Date.parse(localDayBounds(2026, 10, 1).visitedFrom), 24 * HOUR);
  });
});

test("Lord Howe: the 24.5-hour and the 23.5-hour day (half-hour shifts)", () => {
  inZone("Australia/Lord_Howe", () => {
    assert.equal(dayLengths(2026, 3)[4], 24.5, "Sunday April 5, 2026");
    assert.equal(dayLengths(2026, 9)[3], 23.5, "Sunday October 4, 2026");
    assert.equal(monthBounds(2026, 9)[3], "2026-10-03T13:30:00.000Z", "a local midnight at :30 UTC");
  });
});

test("Santiago and Havana: a day whose midnight does not exist starts at 01:00", () => {
  inZone("America/Santiago", () => {
    const start = new Date(localDayBounds(2026, 8, 6).visitedFrom);
    assert.equal(start.getHours(), 1, "September 6, 2026 begins at 01:00");
    assert.equal(start.getDate(), 6);
    assert.equal(dayLengths(2026, 8)[5], 23, "so September 6 lasts 23 hours (01:00 to midnight)");
    assert.equal(dayLengths(2026, 8)[4], 24, "and September 5 ends where September 6 begins");
  });
  inZone("America/Havana", () => {
    const start = new Date(localDayBounds(2026, 2, 8).visitedFrom);
    assert.equal(start.getHours(), 1, "March 8, 2026 begins at 01:00");
    assert.equal(dayLengths(2026, 2)[7], 23);
    assert.equal(dayLengths(2026, 2).filter((hours) => hours !== 24).length, 1);
  });
});

test("bounds of every month have the host's form: 29 to 32 exact, increasing instants", () => {
  for (const zone of ["Asia/Tokyo", "America/New_York", "Australia/Lord_Howe", "America/Santiago", "Pacific/Chatham"]) {
    inZone(zone, () => {
      for (const [year, monthIndex, days] of [[2026, 1, 28], [2028, 1, 29], [2026, 3, 30], [2026, 10, 30], [2026, 11, 31], [2027, 0, 31]]) {
        const bounds = monthBounds(year, monthIndex);
        assert.equal(bounds.length, days + 1, `${zone} ${year}-${monthIndex + 1}`);
        assert.ok(bounds.length >= 29 && bounds.length <= 32);
        for (const text of bounds) assert.match(text, /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/);
        const times = bounds.map((text) => Date.parse(text));
        assert.ok(times.every((time, i) => i === 0 || time > times[i - 1]), "strictly increasing");
        assert.ok(times.at(-1) - times[0] <= 32 * 24 * HOUR, "within the host's 32 days");
        // Each bound is the local midnight (or the day's first instant) of its own day.
        for (const [i, text] of bounds.entries()) {
          const date = new Date(text);
          const expected = new Date(year, monthIndex, i + 1);
          assert.deepEqual([date.getFullYear(), date.getMonth(), date.getDate()], [expected.getFullYear(), expected.getMonth(), expected.getDate()]);
        }
      }
      // December runs into January of the next year.
      assert.equal(monthBounds(2026, 11).at(-1), new Date(2027, 0, 1).toISOString());
      assert.equal(localDayBounds(2026, 11, 31).visitedTo, new Date(2027, 0, 1).toISOString());
      // February 29 of a leap year is a day like any other.
      assert.equal(localDayBounds(2028, 1, 29).visitedTo, new Date(2028, 2, 1).toISOString());
    });
  }
});

test("the earliest year offered stays inside the host's four-digit years", () => {
  assert.ok(MIN_YEAR >= 1000);
  assert.match(monthBounds(MIN_YEAR, 0)[0], /^\d{4}-/);
});

test("the calendar's source never derives days by adding hours or from UTC dates", () => {
  const source = readFileSync(path.join(extensionDir, "visit-calendar.js"), "utf8");
  assert.doesNotMatch(source, /86[_]?400[_]?000|24 \* 60 \* 60|3[_]?600[_]?000/, "no fixed day or hour lengths");
  assert.doesNotMatch(source, /toISOString\(\)\.slice|toISOString\(\)\.substring|toISOString\(\)\.split/, "no UTC date taken as local");
  assert.doesNotMatch(source, /getUTC(Full)?(Year|Month|Date|Hours)\(\)[^;]*toISOString/);
});

// --- visit_days and the date range of search_pages ----------------------------------------------------

const TOKEN = "ab".repeat(32);
const BOUNDS = ["2026-10-31T15:00:00.000Z", "2026-11-01T15:00:00.000Z", "2026-11-02T15:00:00.000Z"];
const daysReply = (message, fields) => ({ ok: true, type: "visit_days_result", requestId: message.requestId, vaultToken: TOKEN, counts: [0, 2], ...fields });

test("visit_days sends the bounds with a request id and returns counts and the vault token", async () => {
  let sent;
  const answer = await visitDays(BOUNDS, { createId: () => "d-1", send: async (message) => ((sent = message), daysReply(message)) });
  assert.deepEqual(sent, { type: "visit_days", requestId: "d-1", bounds: BOUNDS });
  assert.deepEqual(answer, { counts: [0, 2], vaultToken: TOKEN });
});

test("a visit_days answer of another request, length, type or vault form is invalid", async () => {
  const bad = [
    (message) => ({ ...daysReply(message), requestId: "other" }),
    (message) => ({ ...daysReply(message), type: "search_pages_result" }),
    (message) => daysReply(message, { counts: [0] }),
    (message) => daysReply(message, { counts: [0, 2, 3] }),
    (message) => daysReply(message, { counts: [0, "2"] }),
    (message) => daysReply(message, { counts: [0, -1] }),
    (message) => daysReply(message, { counts: [0, 1.5] }),
    (message) => daysReply(message, { counts: null }),
    (message) => daysReply(message, { vaultToken: undefined }),
    (message) => daysReply(message, { vaultToken: "AB".repeat(32) }),
    (message) => daysReply(message, { vaultToken: "ab" }),
    () => undefined,
  ];
  for (const reply of bad) {
    await assert.rejects(visitDays(BOUNDS, { send: async (message) => reply(message) }), (error) => error instanceof SearchError && error.code === "invalid_response", String(reply));
  }
});

test("visit_days errors keep their code and Japanese message", async () => {
  for (const code of ["invalid_bounds", "vault_not_configured", "db_error", "vault_busy"]) {
    await assert.rejects(
      visitDays(BOUNDS, { send: async (message) => ({ ok: false, type: "visit_days_result", requestId: message.requestId, error: code }) }),
      (error) => error.code === code && error.message.length > 0 && error.message !== "操作に失敗しました。",
    );
  }
  await assert.rejects(visitDays(BOUNDS, { timeoutMs: 10, send: () => new Promise(() => {}) }), { code: "timeout" });
  await assert.rejects(visitDays(BOUNDS, { send: () => { throw new SearchError("host_unavailable"); } }), { code: "host_unavailable" });
});

test("search_pages sends a date range only as a pair, and nothing without one", async () => {
  const sent = [];
  const send = async (message) => (sent.push(message), { ok: true, type: "search_pages_result", requestId: message.requestId, items: [], hasMore: false });
  await searchPages({ query: "q" }, { send, createId: () => "s" });
  await searchPages({ query: "q", visitedFrom: BOUNDS[0], visitedTo: BOUNDS[1] }, { send, createId: () => "s" });
  assert.deepEqual(sent, [
    { type: "search_pages", requestId: "s", query: "q", limit: 50, offset: 0 },
    { type: "search_pages", requestId: "s", query: "q", limit: 50, offset: 0, visitedFrom: BOUNDS[0], visitedTo: BOUNDS[1] },
  ]);
  for (const half of [{ visitedFrom: BOUNDS[0] }, { visitedTo: BOUNDS[1] }, { visitedFrom: BOUNDS[0], visitedTo: 3 }]) {
    await assert.rejects(searchPages({ query: "", ...half }, { send }), { code: "invalid_bounds" });
  }
  assert.equal(sent.length, 2, "a half range is never sent");
});
