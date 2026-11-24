// Opt-in Chrome runtime dogfood for Issue #77 (extension i18n). Fresh temporary profiles and a temporary
// copy of the extension only; no registry, no Vault, no Native Host: in the copy, Native Messaging is
// replaced by stubs that always fail, so nothing installed on the machine can be started.
//
//   node tests/dogfood/i18n-chrome.mjs <chrome.exe> [--keep]
//
// For each language it starts Chrome with --lang=<ja|en> and checks what Chrome's own chrome.i18n
// actually returns (langTag), not only the flag. It then restarts the same profile in the other
// language and checks that the existing context menu items were re-titled (onStartup), not duplicated.
// It also checks Chrome's "$$" (test messages added to the copy's catalogs only), that a registration
// Chrome refuses is reported with both of Chrome's messages, and that a catalog with "$$name$$" (name
// undefined) makes Chrome refuse to load the extension.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { cpSync, existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const [browserBin] = process.argv.slice(2).filter((arg) => !arg.startsWith("--"));
const keep = process.argv.includes("--keep");
assert.ok(browserBin, "pass the chrome.exe path (Chrome for Testing, or Chrome that still accepts --load-extension)");
const root = mkdtempSync(path.join(os.tmpdir(), "memocampus-77-i18n-"));
const extension = path.join(root, "extension");
cpSync(fileURLToPath(new URL("../../extension", import.meta.url)), extension, { recursive: true });

// Isolation: no Native Messaging from this copy, ever. Imported first by the worker and the test page.
writeFileSync(path.join(extension, "dogfood-isolation.js"), `
chrome.runtime.sendNativeMessage = (_name, _message, reply) => { queueMicrotask(() => reply?.(undefined)); };
chrome.runtime.connectNative = () => { throw new Error("dogfood: native messaging is disabled"); };
// The log is kept in storage.session (cleared when Chrome exits), so it survives worker restarts and
// can be read from an extension page even when the worker is not running.
let writing = Promise.resolve();
const record = (kind, value) => {
  writing = writing.then(async () => {
    const log = (await chrome.storage.session.get("__dogfood")).__dogfood ?? { events: [], menus: [] };
    log[kind].push(value);
    await chrome.storage.session.set({ __dogfood: log });
  });
};
if (typeof ServiceWorkerGlobalScope !== "undefined") {
  for (const name of ["onInstalled", "onStartup"]) chrome.runtime[name].addListener((details) => record("events", name + ":" + (details?.reason ?? "")));
  for (const method of ["create", "update"]) {
    const original = chrome.contextMenus[method].bind(chrome.contextMenus);
    chrome.contextMenus[method] = (...args) => {
      const callback = typeof args.at(-1) === "function" ? args.pop() : undefined;
      const entry = { method, args: JSON.parse(JSON.stringify(args)) };
      return original(...args, () => { entry.error = chrome.runtime.lastError?.message ?? null; record("menus", entry); callback?.(); });
    };
  }
}
`);
const worker = path.join(extension, "service-worker.js");
// The worker's own registration, reachable from the test (as onStartup would call it).
writeFileSync(worker, 'import "./dogfood-isolation.js";\nimport * as __dogfoodMenuItem from "./menu-item.js";\n' + readFileSync(worker, "utf8")
  + "\nglobalThis.__dogfoodRegisterMenus = registerMenus;\nglobalThis.__dogfoodMenuItem = __dogfoodMenuItem;\n");
// Messages with Chrome's "$$" (a literal "$"), in this copy's catalogs only: what getMessage really shows.
const DOLLAR_MESSAGES = {
  dogfoodPrice: { message: "Price: $$100", description: "test" },
  dogfoodRegex: { message: "github\\.com$$", description: "test" },
  dogfoodMixed: { message: "$$ $count$", description: "test", placeholders: { count: { content: "$1" } } },
};
for (const locale of ["ja", "en"]) {
  const file = path.join(extension, "_locales", locale, "messages.json");
  writeFileSync(file, JSON.stringify({ ...JSON.parse(readFileSync(file, "utf8")), ...DOLLAR_MESSAGES }, null, 2));
}
// A second copy whose catalog holds "$$name$$" with no placeholder "name": Chrome must refuse to load it.
const refused = path.join(root, "extension-refused");
cpSync(extension, refused, { recursive: true });
{
  const file = path.join(refused, "_locales", "en", "messages.json");
  writeFileSync(file, JSON.stringify({ ...JSON.parse(readFileSync(file, "utf8")), dogfoodLiteralName: { message: "$$name$$", description: "test" } }, null, 2));
}
// A page that uses i18n-dom.js the way the product pages will (Slices 2-4), plus the display modules.
writeFileSync(path.join(extension, "i18n-dogfood.html"), `<!doctype html>
<html lang="ja"><head><meta charset="utf-8"><title>i18n dogfood</title></head><body>
<button id="t" data-i18n="menuSavePage" data-i18n-title="menuSaveImage" data-i18n-aria-label="imageSavedTitle"></button>
<input id="p" data-i18n-placeholder="queueErrorGeneric">
<script src="dogfood-page.js"></script>
<script src="i18n-dom.js"></script>
<script src="dogfood-after.js"></script>
</body></html>`);
writeFileSync(path.join(extension, "dogfood-page.js"), `globalThis.__before = { lang: document.documentElement.lang, text: document.querySelector("#t").textContent };`);
writeFileSync(path.join(extension, "dogfood-after.js"), `globalThis.__after = { lang: document.documentElement.lang, text: document.querySelector("#t").textContent };`);

const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
async function until(check, label, ms = 20_000) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    const result = await check().catch(() => null);
    if (result) return result;
    await pause(100);
  }
  throw new Error(`timed out: ${label}`);
}
class CDP {
  static async connect(url) {
    const cdp = new CDP();
    cdp.ws = new WebSocket(url);
    cdp.next = 0;
    cdp.waiting = new Map();
    cdp.ws.onmessage = ({ data }) => {
      const message = JSON.parse(data);
      const request = cdp.waiting.get(message.id);
      if (!request) return;
      cdp.waiting.delete(message.id);
      if (message.error) request.reject(new Error(JSON.stringify(message.error)));
      else request.resolve(message.result);
    };
    await new Promise((resolve, reject) => { cdp.ws.onopen = resolve; cdp.ws.onerror = reject; });
    return cdp;
  }
  send(method, params = {}) {
    const id = ++this.next;
    return new Promise((resolve, reject) => {
      this.waiting.set(id, { resolve, reject });
      this.ws.send(JSON.stringify({ id, method, params }));
    });
  }
  async evaluate(expression) {
    const answer = await this.send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
    if (answer.exceptionDetails) throw new Error(JSON.stringify(answer.exceptionDetails));
    return answer.result.value;
  }
}

async function session(profile, lang, run, knownBase = null, extensionDir = extension) {
  // A restarted profile still holds the previous run's port file; wait for the new one.
  rmSync(path.join(profile, "DevToolsActivePort"), { force: true });
  const chrome = spawn(browserBin, ["--headless=new", `--lang=${lang}`, "--no-first-run", "--no-default-browser-check", "--disable-sync", "--disable-background-networking",
    "--disable-features=DisableLoadExtensionCommandLineSwitch", `--user-data-dir=${profile}`, "--remote-debugging-port=0", "--enable-logging=stderr",
    `--disable-extensions-except=${extensionDir}`, `--load-extension=${extensionDir}`, "about:blank"], { windowsHide: true, stdio: ["ignore", "ignore", "pipe"] });
  let stderr = "";
  chrome.stderr.on("data", (chunk) => { stderr += chunk; });
  if (knownBase === "refused") {
    // Only Chrome's own verdict: no worker of this extension, and its load error in the log.
    try {
      await until(async () => /Failed to load extension/.test(stderr), "Chrome's load error");
      const port = readFileSync(path.join(profile, "DevToolsActivePort"), "utf8").split("\n")[0];
      const workers = (await fetch(`http://127.0.0.1:${port}/json/list`).then((r) => r.json())).filter((t) => t.url.endsWith("/service-worker.js"));
      return { workers: workers.length, error: stderr.split("\n").find((line) => /Failed to load extension/.test(line)).replace(/^.*Extension error: /, "").replace(/ from: .*?\. /, " from <copy>. ").trim() };
    } finally {
      chrome.kill();
      await new Promise((resolve) => (chrome.exitCode !== null ? resolve() : chrome.once("exit", resolve)));
      await pause(500);
    }
  }
  const sockets = [];
  try {
    const portFile = path.join(profile, "DevToolsActivePort");
    const port = await until(async () => existsSync(portFile) && readFileSync(portFile, "utf8").split("\n")[0], "Chrome debugging port");
    const endpoint = `http://127.0.0.1:${port}`;
    const targets = () => fetch(`${endpoint}/json/list`).then((r) => r.json());
    let sw = null;
    let base = knownBase;
    if (!knownBase) {
      const target = await until(async () => (await targets()).find((t) => t.type === "service_worker" && t.url.endsWith("/service-worker.js")), "extension service worker (Chrome must accept --load-extension)");
      base = `chrome-extension://${new URL(target.url).host}`;
      sw = await CDP.connect(target.webSocketDebuggerUrl);
      sockets.push(sw.ws);
    }
    const open = async (url) => {
      const created = await fetch(`${endpoint}/json/new?${encodeURIComponent(url)}`, { method: "PUT" }).then((r) => r.json());
      const page = await CDP.connect(created.webSocketDebuggerUrl);
      sockets.push(page.ws);
      return page;
    };
    return await run({ sw, open, base, targets });
  } finally {
    for (const socket of sockets) socket.close();
    if (chrome.exitCode === null) chrome.kill();
    await new Promise((resolve) => (chrome.exitCode !== null ? resolve() : chrome.once("exit", resolve)));
    await pause(500);
  }
}

const EXPECTED = {
  ja: { menuImage: "Memocampusへ保存", menuPage: "Memocampusにページを保存", http: "画像の取得に失敗しました（HTTP 404）。", policy: "プライバシー設定にアクセスできないため、自動記録を停止しています。", tooltip: "Memocampus — 画像を保存できませんでした: 画像が大きすぎます（上限 32 MiB）。" },
  en: { menuImage: "Save to Memocampus", menuPage: "Save page to Memocampus", http: "Fetching the image failed (HTTP 404).", policy: "Automatic recording is stopped because the privacy settings cannot be accessed.", tooltip: "Memocampus — Could not save an image: The image is too large (limit: 32 MiB)." },
};

async function checkLanguage({ sw, open, base, targets }, lang) {
  const facts = await sw.evaluate(`({ langTag: chrome.i18n.getMessage("langTag"), ui: chrome.i18n.getUILanguage(), menu: chrome.i18n.getMessage("menuSaveImage"),
    literal: chrome.i18n.getMessage("recordPendingReason", ["$1 $$ $count$ <b>x</b>"]), missing: chrome.i18n.getMessage("noSuchMessage") })`);
  assert.equal(facts.langTag, lang, `Chrome's chrome.i18n chose the ${lang} catalog (UI language ${facts.ui})`);
  assert.equal(facts.menu, EXPECTED[lang].menuImage);
  assert.equal(facts.literal, (lang === "ja" ? "理由: " : "Reason: ") + "$1 $$ $count$ <b>x</b>", "substituted values are not read again");
  assert.equal(facts.missing, "", "Chrome returns an empty string for an unknown key (i18n.js turns that into an error)");
  const dollars = await sw.evaluate(`({ price: chrome.i18n.getMessage("dogfoodPrice"), regex: chrome.i18n.getMessage("dogfoodRegex"), mixed: chrome.i18n.getMessage("dogfoodMixed", ["5"]) })`);
  assert.deepEqual(dollars, { price: "Price: $100", regex: "github\\.com$", mixed: "$ 5" }, "Chrome's \"$$\" is one literal \"$\"");
  // A registration Chrome refuses (an unknown context, so no item) is reported with both of Chrome's messages.
  const failure = await sw.evaluate(`__dogfoodMenuItem.registerMenuItem(chrome.contextMenus, chrome.runtime, { id: "dogfood-bad", title: "x", contexts: ["no-such-context"] })
    .then((outcome) => ({ outcome }), (error) => ({ name: error.name, id: error.id, createError: error.createError, updateError: error.updateError }))`);
  assert.equal(failure.name, "MenuRegistrationError", JSON.stringify(failure));
  assert.equal(failure.id, "dogfood-bad");
  assert.ok(failure.createError && failure.updateError, JSON.stringify(failure));
  const self = await sw.evaluate("new Promise((resolve) => chrome.management.getSelf((info) => resolve(info.description)))");
  assert.equal(self, lang === "ja" ? "ローカルファーストのWeb記憶キャプチャ。" : "Local-first web memory capture.", "manifest __MSG_extDescription__");

  const page = await open(`${base}/i18n-dogfood.html`);
  await until(() => page.evaluate("document.readyState === 'complete' && !!globalThis.__after"), "test page");
  const dom = await page.evaluate(`({ before: globalThis.__before, after: globalThis.__after, title: document.querySelector("#t").title,
    aria: document.querySelector("#t").getAttribute("aria-label"), placeholder: document.querySelector("#p").placeholder })`);
  assert.deepEqual(dom.before, { lang: "ja", text: "" }, "untouched before i18n-dom.js");
  assert.deepEqual(dom.after, { lang, text: EXPECTED[lang].menuPage }, "localized synchronously, before the next script");
  assert.equal(dom.title, EXPECTED[lang].menuImage);
  assert.equal(dom.aria, lang === "ja" ? "Memocampusへ保存しました" : "Saved to Memocampus");
  assert.equal(dom.placeholder, lang === "ja" ? "記録に失敗しました。" : "Recording failed.");
  const display = await page.evaluate(`(async () => {
    await import("/dogfood-isolation.js");
    const i18n = await import("/i18n.js");
    const { describeImageSaveResult } = await import("/image-save.js");
    const { describePageSaveResult } = await import("/page-save.js");
    const { describeErrorCode } = await import("/search-client.js");
    const { formatShortDate } = await import("/format.js");
    const { describeRecordResult } = await import("/queue-text.js");
    let missing = null;
    try { i18n.t("noSuchMessage"); } catch (error) { missing = error.name; }
    return { locale: i18n.intlLocale(), http: describeImageSaveResult({ ok: false, code: "http_error", details: { status: 404 } }).message,
      page: describePageSaveResult({ ok: true }).message, policy: describeErrorCode("policy_unavailable"), missing,
      today: formatShortDate(new Date(new Date().setHours(9, 5, 0, 0)).toISOString()),
      one: describeRecordResult({ ok: true, queued: 1, stored: 1, pending: 0, deadLettered: 0 }) };
  })()`);
  assert.equal(display.locale, lang === "ja" ? "ja-JP" : "en-US");
  assert.equal(display.http, EXPECTED[lang].http);
  assert.equal(display.policy, EXPECTED[lang].policy);
  assert.equal(display.missing, "MissingMessageError");
  assert.match(display.today, lang === "ja" ? /^今日 9:05$/ : /^Today 9:05\sAM$/);
  assert.equal(display.one, lang === "ja" ? "1件のタブをSQLiteに保存しました。タブは閉じていません。" : "Saved 1 tab to SQLite. The tab was not closed.");
  assert.match(display.page, lang === "ja" ? /MCPには公開しません/ : /not published to MCP/);

  // The toolbar tooltip from a failure kept as a code in storage.session, written when a worker starts.
  await page.evaluate(`chrome.storage.session.set({ imageSaveFailure: { code: "too_large" } })`);
  await page.send("ServiceWorker.enable");
  await page.send("ServiceWorker.stopAllWorkers");
  await page.evaluate(`chrome.runtime.sendMessage({ type: "flush-queue" }).catch(() => null)`);
  const restarted = await until(async () => (await targets()).find((t) => t.type === "service_worker" && t.url.endsWith("/service-worker.js")), "restarted worker");
  const sw2 = await CDP.connect(restarted.webSocketDebuggerUrl);
  const tooltip = await until(async () => { const title = await sw2.evaluate("chrome.action.getTitle({})"); return title !== "Memocampus" && title; }, "tooltip");
  sw2.ws.close();
  assert.equal(tooltip, EXPECTED[lang].tooltip);
  return { ...facts, dollars, menuFailure: failure, description: self, dom, display, tooltip };
}

const results = {};
const bases = {};
try {
  for (const [first, second] of [["ja", "en"], ["en", "ja"]]) {
    const profile = path.join(root, `profile-${first}-${second}`);
    results[`${first}`] = await session(profile, first, async (browser) => {
      const log = await until(() => browser.sw.evaluate("chrome.storage.session.get('__dogfood').then((v) => v.__dogfood?.menus.length >= 2 && v.__dogfood)"), "menus registered");
      bases[profile] = browser.base;
      assert.deepEqual(log.menus.map((entry) => [entry.method, entry.args[0].title, entry.error]), [["create", EXPECTED[first].menuImage, null], ["create", EXPECTED[first].menuPage, null]]);
      // Registering again while the items exist (what onStartup does): real Chrome refuses the duplicate
      // id, and the worker then updates only the title.
      const outcomes = await browser.sw.evaluate("__dogfoodRegisterMenus()");
      assert.deepEqual(outcomes, ["retitled", "retitled"], "existing items are re-titled, reported as such");
      const again = await until(() => browser.sw.evaluate("chrome.storage.session.get('__dogfood').then((v) => v.__dogfood?.menus.length >= 6 && v.__dogfood)"), "registered again");
      const repeat = again.menus.slice(2).map((entry) => [entry.method, typeof entry.args[0] === "string" ? entry.args[0] : entry.args[0].id, entry.error === null ? "ok" : entry.error]);
      // Callbacks complete in Chrome's order, so compare as a set.
      assert.deepEqual(repeat.map(([method, id, outcome]) => [method, id, outcome === "ok" ? "ok" : "lastError"]).sort(), [
        ["create", "memocampus-save-image", "lastError"], ["update", "memocampus-save-image", "ok"],
        ["create", "memocampus-save-page", "lastError"], ["update", "memocampus-save-page", "ok"],
      ].sort());
      const checked = await checkLanguage(browser, first);
      return { ...checked, events: log.events, reRegistration: repeat };
    });
    // Same profile, Chrome restarted in the other language. Chrome may keep the items (then onStartup
    // re-titles them) or, for an extension loaded with --load-extension, install it afresh (then the
    // items are created again). Either way the menus end up in the new language, once each.
    results[`${first}->${second}`] = await session(profile, second, async (browser) => {
      // The worker runs only on events; read its log from an extension page of the same extension.
      const page = await browser.open(`${browser.base}/i18n-dogfood.html`);
      await until(() => page.evaluate("document.readyState === 'complete'"), "page after restart");
      const log = await until(() => page.evaluate("chrome.storage.session.get('__dogfood').then((v) => v.__dogfood?.menus.length >= 2 && v.__dogfood)"), "menus registered after restart")
        .catch(async (error) => { throw new Error(`${error.message}; log: ${JSON.stringify(await page.evaluate("chrome.storage.session.get(null)"))}`); });
      const summary = log.menus.map((entry) => [entry.method, typeof entry.args[0] === "string" ? entry.args[0] : entry.args[0].id, (entry.args[1] ?? entry.args[0]).title, entry.error === null ? "ok" : "lastError"]);
      const effective = new Map();
      for (const [, id, title, outcome] of summary) if (outcome === "ok") effective.set(id, title);
      assert.deepEqual([...effective].sort(), [["memocampus-save-image", EXPECTED[second].menuImage], ["memocampus-save-page", EXPECTED[second].menuPage]]);
      assert.ok(["memocampus-save-image", "memocampus-save-page"].every((id) => summary.filter(([method, item, , outcome]) => method === "create" && item === id && outcome === "ok").length <= 1), "never created twice");
      const route = summary.some(([method]) => method === "update") ? "kept items re-titled (onStartup)" : "installed afresh (items created again)";
      assert.equal(await page.evaluate(`chrome.i18n.getMessage("langTag")`), second);
      assert.equal(await page.evaluate("document.documentElement.lang"), second);
      return { events: log.events, path: route, menus: summary };
    }, bases[profile]);
  }
  // Languages without a catalog of their own: Chrome falls back to en (default_locale) for every message.
  for (const lang of ["fr", "en-GB"]) {
    results[lang] = await session(path.join(root, `profile-${lang}`), lang, async ({ sw, open, base }) => {
      // Retried: right after the target appears, the worker's script may not have run yet.
      const facts = await until(() => sw.evaluate(`({ langTag: chrome.i18n.getMessage("langTag"), ui: chrome.i18n.getUILanguage(), menu: chrome.i18n.getMessage("menuSaveImage") })`), "worker ready");
      assert.equal(facts.langTag, "en", `${lang} falls back to English`);
      assert.equal(facts.menu, EXPECTED.en.menuImage);
      const page = await open(`${base}/i18n-dogfood.html`);
      await until(() => page.evaluate("document.readyState === 'complete' && !!globalThis.__after"), "test page");
      const locale = await page.evaluate(`import("/i18n.js").then((i18n) => i18n.intlLocale())`);
      assert.equal(locale, "en-US", "any English UI formats as en-US");
      assert.deepEqual(await page.evaluate("globalThis.__after"), { lang: "en", text: EXPECTED.en.menuPage });
      return { ...facts, locale };
    });
  }
  // "$$name$$" is not a literal "$name$" to Chrome: an undefined name makes it refuse the whole extension.
  results.refused = await session(path.join(root, "profile-refused"), "en", null, "refused", refused);
  assert.equal(results.refused.workers, 0);
  assert.match(results.refused.error, /Variable \$name\$ used but not defined/);
  console.log(JSON.stringify({ result: "PASS", root, results }, null, 2));
} catch (error) {
  console.error(JSON.stringify({ result: "FAIL", root, error: error.message, results }, null, 2));
  process.exitCode = 1;
} finally {
  if (!keep && process.exitCode !== 1) rmSync(root, { recursive: true, force: true });
}
