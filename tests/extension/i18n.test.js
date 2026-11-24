// Issue #77: the shared translation API (i18n.js) and the display helpers moved onto it.
import { test } from "node:test";
import assert from "node:assert/strict";
import { MissingMessageError, formatDateTime, formatNumber, intlLocale, pluralKey, t, uiLanguage } from "../../extension/i18n.js";
import { formatFullDate, formatShortDate } from "../../extension/format.js";
import { describeErrorCode, MAX_QUERY_CHARS, SearchError } from "../../extension/search-client.js";
import { describeImageSaveResult } from "../../extension/image-save.js";
import { describePageSaveResult } from "../../extension/page-save.js";
import { describePolicyStatus, describeQueueStatus, describeRecordResult } from "../../extension/queue-text.js";
import { chromeI18n, createI18n, withLocale } from "./i18n-mock.js";

const JAPANESE = /[　-ヿ一-鿿＀-￯]/u;
const local = (...parts) => new Date(...parts).toISOString();

async function withChrome(chrome, body) {
  const saved = globalThis.chrome;
  globalThis.chrome = chrome;
  try {
    return await body();
  } finally {
    globalThis.chrome = saved;
  }
}

test("t returns the message of the catalog Chrome chose, with substitutions in order", async () => {
  assert.equal(t("menuSaveImage"), "Memocampusへ保存");
  assert.equal(t("badgeEventsNotRecorded", 3, 0), "記録できなかったイベント 3件・破棄 0件");
  await withLocale("en", () => {
    assert.equal(t("menuSaveImage"), "Save to Memocampus");
    assert.equal(t("badgeEventsNotRecorded", 3, 0), "Events not recorded: 3, discarded: 0");
  });
  assert.equal(chromeI18n.locale, "ja", "withLocale restores the language");
});

test("a missing message fails loudly instead of showing an empty or other-language string", async () => {
  assert.throws(() => t("noSuchMessage"), (error) => error instanceof MissingMessageError && error.key === "noSuchMessage");
  await withChrome({}, () => assert.throws(() => t("menuSaveImage"), MissingMessageError));
  await withChrome({ i18n: { getMessage: () => "" } }, () => assert.throws(() => t("menuSaveImage"), MissingMessageError));
  await withChrome({ i18n: { getMessage: () => undefined } }, () => assert.throws(() => t("menuSaveImage"), MissingMessageError));
});

test("substitutions are strings or numbers, at most nine, as Chrome allows", () => {
  assert.throws(() => t("recordStoredCount", 1, 2, 3, 4, 5, 6, 7, 8, 9, 10), RangeError);
  for (const bad of [undefined, null, {}, ["x"], true]) assert.throws(() => t("recordStoredCount", bad), TypeError);
});

test("substituted values are inserted literally: no placeholder, $ or markup is read in them", async () => {
  for (const value of ["$1", "$$", "$count$", "<b>x</b>", "&amp;", "{reason}"]) {
    assert.equal(t("recordPendingReason", value), `理由: ${value}`);
    await withLocale("en", () => assert.equal(t("recordPendingReason", value), `Reason: ${value}`));
  }
});

test("the language in use comes from the catalog's langTag, not from the UI language", async () => {
  assert.equal(uiLanguage(), "ja");
  assert.equal(intlLocale(), "ja-JP");
  await withLocale("en", () => {
    assert.equal(uiLanguage(), "en");
    assert.equal(intlLocale(), "en-US");
  });
  // Chrome's UI is in French: Chrome falls back to the default (English) catalog; langTag says so.
  const french = createI18n({ locale: "en" });
  await withChrome({ i18n: { ...french, getUILanguage: () => "fr", getMessage: french.getMessage } }, () => assert.equal(intlLocale(), "en-US"));
  // An unexpected langTag never selects an unknown locale.
  await withChrome({ i18n: { getMessage: (key) => (key === "langTag" ? "xx" : "x") } }, () => assert.equal(intlLocale(), "en-US"));
});

test("Intl helpers format numbers, dates and plurals for the language in use", async () => {
  assert.equal(formatNumber(1234567), "1,234,567");
  assert.equal(pluralKey(1, { one: "a", other: "b" }), "b", "Japanese has no singular form");
  assert.equal(formatDateTime(new Date(2026, 0, 2, 9, 5), { month: "short", day: "numeric" }), "1月2日");
  await withLocale("en", () => {
    assert.equal(formatNumber(1234567), "1,234,567");
    assert.equal(pluralKey(1, { one: "a", other: "b" }), "a");
    assert.equal(pluralKey(0, { one: "a", other: "b" }), "b");
    assert.equal(pluralKey(2, { one: "a" , other: "b" }), "b");
    assert.equal(pluralKey(2, { other: "b" }), "b");
    assert.equal(formatDateTime(new Date(2026, 0, 2, 9, 5), { month: "short", day: "numeric" }), "Jan 2");
  });
});

test("short dates keep their Japanese form and read naturally in English", async () => {
  const now = new Date(2026, 9, 30, 15, 0);
  assert.equal(formatShortDate(local(2026, 9, 30, 9, 5), now), "今日 9:05");
  assert.equal(formatShortDate(local(2026, 0, 2, 14, 30), now), "1月2日 14:30");
  await withLocale("en", () => {
    assert.match(formatShortDate(local(2026, 9, 30, 9, 5), now), /^Today 9:05\sAM$/);
    assert.match(formatShortDate(local(2026, 9, 29, 23, 59), now), /^Yesterday 11:59\sPM$/);
    assert.match(formatShortDate(local(2026, 0, 2, 14, 30), now), /^Jan 2 2:30\sPM$/);
    assert.match(formatShortDate(local(2025, 11, 31, 8, 0), now), /^Dec 31, 2025 8:00\sAM$/);
    assert.equal(formatShortDate("not-a-date", now), "");
    // The full timestamp is a fixed format in every language.
    assert.equal(formatFullDate(local(2026, 2, 4, 5, 6, 7)), "2026/03/04 05:06:07");
  });
});

test("error codes map to messages in both languages; unknown or prototype names get the generic one", async () => {
  assert.equal(describeErrorCode("vault_missing"), "Vaultファイルが見つかりません。移動・削除されていないか確認し、Vaultを開き直してください。");
  assert.equal(describeErrorCode("query_too_long"), `検索語は${MAX_QUERY_CHARS}文字以内で入力してください。`);
  for (const code of ["no_such_code", "__proto__", "constructor", "toString", "", null, undefined, 42]) assert.equal(describeErrorCode(code), "操作に失敗しました。", String(code));
  await withLocale("en", () => {
    assert.equal(describeErrorCode("query_too_long"), `Enter a search term of ${MAX_QUERY_CHARS} characters or fewer.`);
    assert.equal(describeErrorCode("policy_unavailable"), "Automatic recording is stopped because the privacy settings cannot be accessed.");
    assert.equal(describeErrorCode("constructor"), "The operation failed.");
    const error = new SearchError("vault_locked");
    assert.equal(error.code, "vault_locked", "the code stays the protocol value");
    assert.equal(error.message, "The encrypted Vault is locked. The file was not changed.");
  });
});

test("existing duplicate entries keep the message that was in effect (the later one)", () => {
  // The old table listed these codes twice; JavaScript kept the second text, which is kept here.
  assert.equal(describeErrorCode("unsupported_url"), "このURLのホストはルールで指定できません。");
  assert.equal(describeErrorCode("invalid_vault_token"), "要求が不正です。一覧を更新してください。");
});

test("every error code of the table has a message in both languages", async () => {
  const { readFileSync } = await import("node:fs");
  const source = readFileSync(new URL("../../extension/search-client.js", import.meta.url), "utf8");
  const codes = [...source.slice(source.indexOf("const ERROR_KEYS")).matchAll(/^ {2}(\w+): "error\w+",$/gm)].map((match) => match[1]);
  assert.ok(codes.length > 150);
  for (const locale of ["ja", "en"]) {
    await withLocale(locale, () => {
      for (const code of codes) {
        const text = describeErrorCode(code);
        assert.notEqual(text, locale === "ja" ? "操作に失敗しました。" : "The operation failed.", `${locale} ${code}`);
        if (locale === "en") assert.doesNotMatch(text, JAPANESE, code);
      }
    });
  }
});

test("notifications for image and page saves read in English", async () => {
  await withLocale("en", () => {
    const image = { width: 800, height: 600, byteLength: 2 * 1024 * 1024 };
    assert.deepEqual(describeImageSaveResult({ ok: true, image, saved: { deduplicated: false, replayed: false } }), { title: "Saved to Memocampus", message: "Image 800×600, 2.0 MB" });
    assert.match(describeImageSaveResult({ ok: true, image: { ...image, byteLength: 5000 }, saved: { deduplicated: true, replayed: false } }).message, /^Image 800×600, 5 KB \(the same image was already saved/);
    assert.match(describeImageSaveResult({ ok: true, image, saved: { deduplicated: false, replayed: true } }).message, /already been saved\)$/);
    assert.deepEqual(describeImageSaveResult({ ok: false, code: "http_error", details: { status: 403 } }), { title: "Could not save to Memocampus", message: "Fetching the image failed (HTTP 403)." });
    assert.equal(describeImageSaveResult({ ok: false, code: "http_error", details: { status: "403" } }).message, "Fetching the image failed (HTTP error).", "only a numeric status is shown");
    assert.deepEqual(describePageSaveResult({ ok: true }), { title: "Page saved to Memocampus", message: "Saved the original MHTML and its save record to the Vault. It is not published to MCP." });
    assert.equal(describePageSaveResult({ ok: false, code: "incognito_not_allowed" }).message, "Saving pages from Incognito windows is not supported.");
    for (const code of ["__proto__", "constructor", "no_such_code", undefined]) assert.match(describePageSaveResult({ ok: false, code }).message, /^Capturing the page or saving it/);
  });
});

test("queue and policy texts read in English, with singular and plural forms", async () => {
  await withLocale("en", () => {
    assert.equal(describeRecordResult({ ok: true, queued: 1, stored: 1, pending: 0, deadLettered: 0 }), "Saved 1 tab to SQLite. The tab was not closed.");
    assert.equal(describeRecordResult({ ok: true, queued: 3, stored: 3, pending: 0, deadLettered: 0 }), "Saved 3 tabs to SQLite. The tabs were not closed.");
    assert.equal(describeRecordResult({ ok: true, queued: 3, stored: 1, pending: 2, deadLettered: 0, reason: "No Vault." }),
      "Added 3 tabs to the send queue. The tabs were not closed.\nSaved to SQLite: 1\nNot saved yet (waiting to send, retried automatically): 2\nReason: No Vault.");
    assert.equal(describeRecordResult({ ok: true, queued: 0 }), "There are no tabs in normal windows (http/https) that can be recorded.");
    assert.equal(describeRecordResult({ ok: false }), "Recording failed.\nSaving failed.");
    assert.equal(describePolicyStatus({ ok: false }), "The state of the privacy settings cannot be checked, so automatic recording may be stopped.");
    assert.equal(describePolicyStatus({ ok: true, state: "unavailable" }), "Automatic recording is stopped.");
    assert.equal(describePolicyStatus({ ok: true, state: "ready" }), "");
    assert.equal(describeQueueStatus({ ok: true, pending: 2, blockedReason: "Busy.", deadLetters: 1, dropped: 0, navigationInvalid: 0, navigationNotStored: 0 }),
      "Waiting to send: 2 (Busy.)\nEvents quarantined because they could not be recorded: 1");
  });
  // Japanese output is unchanged.
  assert.equal(describeRecordResult({ ok: true, queued: 1, stored: 1, pending: 0, deadLettered: 0 }), "1件のタブをSQLiteに保存しました。タブは閉じていません。");
});
