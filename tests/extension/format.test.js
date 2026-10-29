import { test } from "node:test";
import assert from "node:assert/strict";
import { displayUrl, formatFullDate, formatShortDate } from "../../extension/format.js";

// Built from local-time components so the assertions hold in any time zone.
const local = (...parts) => new Date(...parts).toISOString();
const now = new Date(2026, 9, 30, 15, 0);

test("formatShortDate uses natural Japanese relative forms", () => {
  assert.equal(formatShortDate(local(2026, 9, 30, 9, 5), now), "今日 9:05");
  assert.equal(formatShortDate(local(2026, 9, 29, 23, 59), now), "昨日 23:59");
  assert.equal(formatShortDate(local(2026, 0, 2, 14, 30), now), "1月2日 14:30");
  assert.equal(formatShortDate(local(2025, 11, 31, 8, 0), now), "2025年12月31日 8:00");
});

test("formatShortDate handles the new-year boundary as yesterday", () => {
  assert.equal(formatShortDate(local(2025, 11, 31, 22, 0), new Date(2026, 0, 1, 1, 0)), "昨日 22:00");
});

test("invalid timestamps render as empty strings", () => {
  for (const value of [null, undefined, "", "not-a-date", 42]) {
    assert.equal(formatShortDate(value, now), "");
    assert.equal(formatFullDate(value), "");
  }
});

test("formatFullDate is zero padded", () => {
  assert.equal(formatFullDate(local(2026, 2, 4, 5, 6, 7)), "2026/03/04 05:06:07");
});

test("displayUrl strips scheme and www and decodes valid escapes", () => {
  assert.equal(displayUrl("https://www.example.test/"), "example.test");
  assert.equal(displayUrl("http://example.test/a/b?q=1"), "example.test/a/b?q=1");
  assert.equal(displayUrl("https://ja.wikipedia.org/wiki/%E6%97%A5%E6%9C%AC"), "ja.wikipedia.org/wiki/日本");
  assert.equal(displayUrl("https://example.test/%E0%A4%A"), "example.test/%E0%A4%A");
});
