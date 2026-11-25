// Opt-in Chrome runtime dogfood for Issue #77 Slice 4 (the Privacy settings page and the full-page History in
// Japanese and English). A fresh temporary profile and a temporary copy of the extension only; no registry, no Vault,
// no installed Native Host. In the copy, the pages' Native Messaging is bridged (CDP Runtime.addBinding) to the tests'
// fake hosts (options-fixtures.js fakeHost, history-fixtures.js createHistoryHost), and the service worker's is a stub
// that always fails, so nothing installed on the machine can be started.
//
//   node tests/dogfood/options-history-i18n-chrome.mjs <chrome.exe> [outDir] [--keep]
//
// For ja and en, and for widths of 360, 720 and 1280 px, it opens options.html and history.html in a normal tab (the
// width is emulated) and checks: i18n-dom.js applied (<html lang>, the title), no horizontal scroll, no control outside
// the viewport, no two controls overlapping, no clipped control text, no console error; in English no Japanese in any
// visible text or accessible name. Screenshots and the measurements are written to outDir.
// #83: the calendar's "back to the selected day's month" button is checked as Chrome renders it (computed style,
// size, hit test), not only by its hidden attribute, and the steps must give the same states in both languages.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
// The fake hosts' modules read text through chrome.i18n when they load (in this Node process, not in Chrome).
import "../extension/i18n-setup.js";
import { fakeHost as fakePolicyHost, pattern, v2 } from "../extension/options-fixtures.js";
import { createHistoryHost, visit } from "../extension/history-fixtures.js";

const args = process.argv.slice(2).filter((arg) => !arg.startsWith("--"));
const [browserBin] = args;
const keep = process.argv.includes("--keep");
assert.ok(browserBin, "pass the chrome.exe path (Chrome for Testing, or Chrome that still accepts --load-extension)");
const root = mkdtempSync(path.join(os.tmpdir(), "memocampus-77-options-history-"));
const outDir = args[1] ?? path.join(root, "out");
mkdirSync(outDir, { recursive: true });
const extension = path.join(root, "extension");
cpSync(fileURLToPath(new URL("../../extension", import.meta.url)), extension, { recursive: true });

writeFileSync(path.join(extension, "dogfood-isolation.js"), `
chrome.runtime.sendNativeMessage = (_name, _message, reply) => { queueMicrotask(() => reply?.(undefined)); };
chrome.runtime.connectNative = () => { throw new Error("dogfood: native messaging is disabled"); };
`);
const worker = path.join(extension, "service-worker.js");
writeFileSync(worker, 'import "./dogfood-isolation.js";\n' + readFileSync(worker, "utf8"));
// The pages: Native Messaging goes to the harness (a fake host), never to a real one.
writeFileSync(path.join(extension, "dogfood-bridge.js"), `
const pending = new Map();
let seq = 0;
globalThis.__nativeReply = (id, response) => { const done = pending.get(id); pending.delete(id); done?.(response); };
chrome.runtime.sendNativeMessage = (_name, message, reply) => { const id = ++seq; pending.set(id, (response) => reply?.(response)); globalThis.__native(JSON.stringify({ id, message })); };
`);
for (const script of ["options.js", "history.js"]) {
  const file = path.join(extension, script);
  writeFileSync(file, 'import "./dogfood-bridge.js";\n' + readFileSync(file, "utf8"));
}

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
    cdp.handlers = new Map();
    cdp.ws.onmessage = ({ data }) => {
      const message = JSON.parse(data);
      if (message.method) for (const fn of cdp.handlers.get(message.method) ?? []) fn(message.params);
      const request = cdp.waiting.get(message.id);
      if (!request) return;
      cdp.waiting.delete(message.id);
      if (message.error) request.reject(new Error(JSON.stringify(message.error)));
      else request.resolve(message.result);
    };
    await new Promise((resolve, reject) => { cdp.ws.onopen = resolve; cdp.ws.onerror = reject; });
    return cdp;
  }
  on(method, fn) { (this.handlers.get(method) ?? this.handlers.set(method, []).get(method)).push(fn); }
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

// --- Fake hosts ----------------------------------------------------------------------------------------------
const today = new Date();
const atToday = (hour, minute) => new Date(today.getFullYear(), today.getMonth(), today.getDate(), hour, minute).toISOString();
const VISITS = [
  visit("v1", "https://docs.example.test/guides/a-very-long-path/with/many/segments?query=string&and=more", "A rather long English page title that has to wrap inside a narrow window", atToday(9, 5)),
  visit("v2", "https://github.com/memocampus/issues/77", "Issue 77", atToday(10, 30)),
  visit("v3", "https://news.example.test/", "", atToday(11, 0)),
];
const HOSTS = {
  "options-ready": () => fakePolicyHost(),
  "options-checker": () => fakePolicyHost(),
  "options-relax": () => fakePolicyHost(),
  "options-validation": () => fakePolicyHost({ policy: v2({ neverSave: [pattern("bad-rule.example")] }) }),
  "options-approval": () => {
    const host = fakePolicyHost();
    host.restricted["*.github.io"] = { domain: "github.io", reason: "public_suffix" };
    return host;
  },
  "options-invalid": () => ({ send: async (message) => ({ ok: true, type: "policy_result", requestId: message.requestId, state: "invalid" }) }),
  "options-onboarding": () => {
    const host = fakePolicyHost();
    host.answers.policy_status = () => ({ ok: true, state: "uninitialized" });
    host.answers.policy_preset = () => ({ ok: true, presetId: "default", schemaVersion: 2, engineVersion: 2, policy: v2({}, { revision: 1 }) });
    host.answers.vault_status = () => ({ ok: true, type: "vault_result", status: { state: "not_configured" } });
    return host;
  },
  "history-list": () => createHistoryHost({ visits: { A: VISITS, B: [] } }),
  "history-detail": () => createHistoryHost({ visits: { A: VISITS, B: [] }, relations: { v2: { source: "v1", targets: ["v3"], pageMetadata: { title: "Issue 77", description: "An issue", capturedAt: atToday(10, 31) } } } }),
  "history-months": () => createHistoryHost({ visits: { A: VISITS, B: [] } }),
  "history-empty": () => createHistoryHost({ visits: { A: [], B: [] } }),
  "history-error": () => createHistoryHost({ visits: { A: VISITS, B: [] }, errors: { history_list: "vault_locked" } }),
  "history-jump": () => createHistoryHost({ visits: { A: VISITS, B: [] } }),
};
const JUMP_LABEL = Object.fromEntries(["ja", "en"].map((lang) => [lang,
  JSON.parse(readFileSync(fileURLToPath(new URL(`../../extension/_locales/${lang}/messages.json`, import.meta.url)), "utf8")).historyJumpToSelected.message]));

// --- Scenarios (in the page) ----------------------------------------------------------------------------------
const click = (page, selector) => page.evaluate(`document.querySelector(${JSON.stringify(selector)}).click()`);
const typeInto = (page, selector, value) => page.evaluate(`(() => { const el = document.querySelector(${JSON.stringify(selector)}); el.value = ${JSON.stringify(value)}; el.dispatchEvent(new Event("input", { bubbles: true })); })()`);
const show = (page, selector) => page.evaluate(`document.querySelector(${JSON.stringify(selector)}).scrollIntoView({ block: "start" })`);
const SCENARIOS = {
  "options-ready": async () => {},
  "options-checker": async (page) => {
    await typeInto(page, "#checker-rule", "new.example");
    await typeInto(page, "#checker-url", "https://new.example/page");
    await click(page, "#checker-run");
    await until(() => page.evaluate(`!document.querySelector("#checker-add-never-save").disabled`), "match");
    await click(page, "#checker-add-never-save");
    await show(page, "#checker");
  },
  "options-relax": async (page) => {
    await until(() => page.evaluate(`document.querySelectorAll("#save-but-no-mcp-rules .rule-delete").length > 0`), "rules");
    await click(page, "#save-but-no-mcp-rules .rule-delete");
    await click(page, "#apply");
    await until(() => page.evaluate(`!document.querySelector("#relax-confirm").hidden`), "confirmation");
  },
  "options-validation": async (page) => {
    await click(page, "#validate");
    await until(() => page.evaluate(`!document.querySelector("#validation-status").hidden`), "validation");
    await show(page, "#never-save-heading");
  },
  "options-approval": async (page) => {
    await page.evaluate(`document.querySelector("#checker-kind-domainWildcard").click()`);
    await typeInto(page, "#checker-rule", "*.github.io");
    await typeInto(page, "#checker-url", "https://user.github.io/");
    await click(page, "#checker-run");
    await until(() => page.evaluate(`!document.querySelector("#checker-approval").hidden`), "approval");
    await show(page, "#checker-approval");
  },
  "options-invalid": async () => {},
  "options-onboarding": async (page) => {
    await until(() => page.evaluate(`!document.querySelector("#onboarding").hidden && document.querySelector("#onboarding-vault").textContent.length > 0`), "onboarding");
  },
  "history-list": async () => {},
  "history-detail": async (page) => {
    await until(() => page.evaluate(`document.querySelectorAll(".visit-toggle").length > 1`), "rows");
    await page.evaluate(`[...document.querySelectorAll(".visit-row")].find((row) => row.dataset.visitId === "v2").querySelector(".visit-toggle").click()`);
    await until(() => page.evaluate(`document.querySelector(".detail-card-visit") !== null`), "detail");
    await show(page, ".visit-row.is-expanded");
  },
  "history-months": async (page) => {
    await click(page, "#cal-title");
    await until(() => page.evaluate(`!document.querySelector("#cal-months").hidden`), "months");
  },
  "history-empty": async () => {},
  "history-error": async () => {},
  // #83 cases A to C; the returned record must be the same in ja and en (case D).
  "history-jump": async (page, { lang, host, problems }) => {
    const fail = (text) => problems.push(`history-jump: ${text}`);
    const jump = () => page.evaluate(`(() => {
      const b = document.querySelector("#cal-jump");
      b.scrollIntoView({ block: "center" });
      const s = getComputedStyle(b), r = b.getBoundingClientRect();
      const hit = r.width > 0 && r.height > 0 ? document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2) : null;
      return { hiddenAttr: b.hidden, display: s.display, visibility: s.visibility, width: r.width, height: r.height,
        x: r.left + r.width / 2, y: r.top + r.height / 2, onTop: hit !== null && (hit === b || b.contains(hit)), disabled: b.disabled,
        text: b.textContent, ariaLabel: b.getAttribute("aria-label"), title: document.querySelector("#cal-title").textContent };
    })()`);
    const selection = () => page.evaluate(`({ search: location.search, heading: document.querySelector("#selected-heading").textContent,
      selected: [...document.querySelectorAll("#cal-table .is-selected")].map((el) => el.textContent) })`);
    const notShown = (s, where) => {
      if (!s.hiddenAttr) fail(`${where}: no hidden attribute`);
      if (s.display !== "none" || s.width > 0 || s.height > 0 || s.onTop) fail(`${where}: still rendered (display ${s.display}, ${s.width}x${s.height})`);
    };
    const start = await selection();
    const a = await jump();
    notShown(a, "A, the selected day's month");
    if (start.selected.length !== 1) fail("A: the selected day is not marked");
    // B: another month.
    await click(page, "#cal-prev");
    await until(async () => (await jump()).title !== a.title, "previous month");
    const b = await jump();
    if (b.hiddenAttr) fail("B, another month: hidden attribute kept");
    if (b.display === "none" || b.visibility !== "visible" || !(b.width > 0 && b.height > 0)) fail(`B: not rendered (display ${b.display}, ${b.width}x${b.height})`);
    if (!b.onTop || b.disabled) fail("B: not clickable");
    if (b.text !== JUMP_LABEL[lang]) fail(`B: label ${JSON.stringify(b.text)}`);
    const away = await selection();
    // C: a real mouse click at the button's centre.
    for (const type of ["mousePressed", "mouseReleased"]) await page.send("Input.dispatchMouseEvent", { type, x: b.x, y: b.y, button: "left", clickCount: 1 });
    await until(async () => (await jump()).title === a.title, "back to the selected day's month");
    await pause(200);
    const c = await jump();
    notShown(c, "C, after the jump");
    const end = await selection();
    if (JSON.stringify(end) !== JSON.stringify(start)) fail(`C: the selection changed ${JSON.stringify(start)} -> ${JSON.stringify(end)}`);
    if (JSON.stringify(away) !== JSON.stringify({ ...start, selected: [] })) fail(`B: the selection changed ${JSON.stringify(away)}`);
    const requests = (type) => host.of(type).map(({ requestId, ...rest }) => rest);
    return {
      shown: { A: a.display !== "none", B: b.display !== "none" && b.onTop, C: c.display !== "none" },
      hiddenAttr: { A: a.hiddenAttr, B: b.hiddenAttr, C: c.hiddenAttr },
      ariaLabel: [a.ariaLabel, b.ariaLabel, c.ariaLabel],
      search: [start.search, away.search, end.search], selected: [start.selected, away.selected, end.selected],
      history_list: requests("history_list"), history_days: requests("history_days"),
    };
  },
};

const JAPANESE = /[ぁ-んァ-ヶ一-龠（）「」、。・]/u;
const WIDTHS = [360, 720, 1280];
const MEASURE = `(() => {
  const width = innerWidth;
  const visible = (el) => { const r = el.getBoundingClientRect(); const s = getComputedStyle(el); return r.width > 0 && r.height > 0 && s.visibility !== "hidden" && s.display !== "none"; };
  const controls = [...document.querySelectorAll("button, input, select, label, a, summary")].filter(visible);
  const outside = controls.filter((el) => { const r = el.getBoundingClientRect(); return r.left < -0.5 || r.right > width + 0.5; }).map((el) => el.id || el.className || el.tagName);
  const clipped = [...document.querySelectorAll("button, label, legend, h1, h2, h3, h4, option")].filter(visible)
    .filter((el) => el.scrollWidth > el.clientWidth + 1 && getComputedStyle(el).overflowX !== "visible" || el.scrollHeight > el.clientHeight + 1 && getComputedStyle(el).overflowY === "hidden")
    .map((el) => (el.id || el.className || el.tagName) + ": " + el.textContent.trim().slice(0, 40));
  const seen = (el) => {
    let { left, top, right, bottom } = el.getBoundingClientRect();
    for (let p = el.parentElement; p; p = p.parentElement) {
      if (!/(auto|scroll|hidden)/.test(getComputedStyle(p).overflowY)) continue;
      const c = p.getBoundingClientRect();
      left = Math.max(left, c.left); top = Math.max(top, c.top); right = Math.min(right, c.right); bottom = Math.min(bottom, c.bottom);
    }
    return { left, top, right, bottom };
  };
  const onTop = (el) => { const r = el.getBoundingClientRect(); const hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2); return hit !== null && (hit === el || el.contains(hit) || hit.closest("label")?.control === el); };
  const boxes = controls.filter((el) => el.matches("button, input:not([type=radio]):not([type=checkbox]), select") && onTop(el)).map((el) => [el, seen(el)]);
  const overlaps = [];
  for (let i = 0; i < boxes.length; i++) for (let j = i + 1; j < boxes.length; j++) {
    const [a, ra] = boxes[i], [b, rb] = boxes[j];
    if (a.contains(b) || b.contains(a)) continue;
    // A sticky or fixed bar (the apply bar) lies over the scrolled page by design: only controls of one layer collide.
    const layer = (el) => { for (let p = el; p; p = p.parentElement) if (/^(sticky|fixed)$/.test(getComputedStyle(p).position)) return p; return null; };
    if (layer(a) !== layer(b)) continue;
    const x = Math.min(ra.right, rb.right) - Math.max(ra.left, rb.left), y = Math.min(ra.bottom, rb.bottom) - Math.max(ra.top, rb.top);
    if (x > 1 && y > 1) overlaps.push((a.id || a.className) + " × " + (b.id || b.className));
  }
  const text = [...document.querySelectorAll("body *")].filter(visible)
    .map((el) => [...el.childNodes].filter((n) => n.nodeType === 3).map((n) => n.textContent).join("").trim()).filter(Boolean);
  const attributes = [...document.querySelectorAll("[aria-label], [title], [placeholder]")].flatMap((el) => ["aria-label", "title", "placeholder"].map((a) => el.getAttribute(a)).filter(Boolean));
  return { width, scrollWidth: document.documentElement.scrollWidth, outside, clipped, overlaps, text, attributes, lang: document.documentElement.lang, title: document.title,
    partLangs: [...document.querySelectorAll("body [lang]")].map((el) => el.id || el.tagName) };
})()`;

async function session(lang, run) {
  const profile = path.join(root, `profile-${lang}`);
  const chrome = spawn(browserBin, ["--headless=new", `--lang=${lang}`, "--no-first-run", "--no-default-browser-check", "--disable-sync", "--disable-background-networking",
    "--disable-features=DisableLoadExtensionCommandLineSwitch", `--user-data-dir=${profile}`, "--remote-debugging-port=0", "--hide-scrollbars",
    `--disable-extensions-except=${extension}`, `--load-extension=${extension}`, "about:blank"], { windowsHide: true, stdio: "ignore" });
  const sockets = [];
  try {
    const portFile = path.join(profile, "DevToolsActivePort");
    const port = await until(async () => existsSync(portFile) && readFileSync(portFile, "utf8").split("\n")[0], "Chrome debugging port");
    const endpoint = `http://127.0.0.1:${port}`;
    const target = await until(async () => (await fetch(`${endpoint}/json/list`).then((r) => r.json())).find((t) => t.type === "service_worker" && t.url.endsWith("/service-worker.js")), "extension service worker");
    const base = `chrome-extension://${new URL(target.url).host}`;
    const open = async (width, name) => {
      const created = await fetch(`${endpoint}/json/new?about:blank`, { method: "PUT" }).then((r) => r.json());
      const page = await CDP.connect(created.webSocketDebuggerUrl);
      sockets.push(page.ws);
      const host = HOSTS[name]();
      const errors = [];
      page.on("Runtime.exceptionThrown", (p) => errors.push(`exception: ${p.exceptionDetails.exception?.description ?? p.exceptionDetails.text}`));
      page.on("Runtime.consoleAPICalled", (p) => { if (p.type === "error" || p.type === "assert") errors.push(`console.${p.type}: ${p.args.map((a) => a.value ?? a.description).join(" ")}`); });
      page.on("Log.entryAdded", (p) => { if (p.entry.level === "error") errors.push(`log: ${p.entry.text} ${p.entry.url ?? ""}`); });
      page.on("Runtime.bindingCalled", (p) => {
        if (p.name !== "__native") return;
        const call = JSON.parse(p.payload);
        Promise.resolve(host.send(structuredClone(call.message))).then((response) => void page.evaluate(`__nativeReply(${call.id}, ${JSON.stringify(response ?? null)})`).catch(() => {}));
      });
      await page.send("Runtime.enable");
      await page.send("Log.enable");
      await page.send("Runtime.addBinding", { name: "__native" });
      await page.send("Emulation.setDeviceMetricsOverride", { width, height: 900, deviceScaleFactor: 1, mobile: false });
      await page.send("Page.enable");
      const file = name.startsWith("options") ? "options.html" : "history.html";
      await page.send("Page.navigate", { url: `${base}/${file}` });
      const ready = file === "options.html"
        ? `document.readyState === 'complete' && document.querySelector('#policy-state').dataset.state !== 'loading'`
        : `document.readyState === 'complete' && document.querySelector('#selected-heading').textContent.length > 0 && (document.querySelector('#history-list').children.length > 0 || !document.querySelector('#history-state').hidden && !/…$/.test(document.querySelector('#history-state-text').textContent))`;
      await until(() => page.evaluate(ready), `${name} loaded`);
      await pause(300);
      return { page, errors, host };
    };
    return await run({ open });
  } finally {
    for (const socket of sockets) socket.close();
    if (chrome.exitCode === null) chrome.kill();
    await new Promise((resolve) => (chrome.exitCode !== null ? resolve() : chrome.once("exit", resolve)));
    await pause(800);
  }
}

const results = { root, outDir, sessions: {} };
const problems = [];
const jumps = {};
try {
  for (const lang of ["ja", "en"]) {
    results.sessions[lang] = await session(lang, async ({ open }) => {
      const out = {};
      for (const width of WIDTHS) {
        for (const [name, scenario] of Object.entries(SCENARIOS)) {
          const { page, errors, host } = await open(width, name);
          const own = [];
          const record = await scenario(page, { lang, host, problems: own });
          if (record !== undefined) (jumps[width] ??= {})[lang] = record;
          for (const item of own) problems.push(`${lang} ${width}px ${item}`);
          await pause(250);
          const m = await page.evaluate(MEASURE);
          const shot = await page.send("Page.captureScreenshot", { format: "png", captureBeyondViewport: false });
          const file = path.join(outDir, `${name}-${lang}-${width}.png`);
          writeFileSync(file, Buffer.from(shot.data, "base64"));
          const japanese = lang === "en" ? [...m.text, ...m.attributes, m.title].filter((s) => JAPANESE.test(s)) : [];
          out[`${width}-${name}`] = { scrollWidth: m.scrollWidth, outside: m.outside, clipped: m.clipped, overlaps: m.overlaps, errors, japanese, lang: m.lang, title: m.title, screenshot: path.basename(file) };
          const where = `${lang} ${width}px ${name}`;
          if (m.lang !== lang) problems.push(`${where}: <html lang="${m.lang}">`);
          if (!m.title) problems.push(`${where}: empty title`);
          for (const item of m.partLangs) problems.push(`${where}: ${item} keeps a language of its own`);
          if (m.scrollWidth > width) problems.push(`${where}: horizontal scroll (${m.scrollWidth}px)`);
          for (const item of m.outside) problems.push(`${where}: outside the viewport: ${item}`);
          for (const item of m.overlaps) problems.push(`${where}: overlap: ${item}`);
          for (const item of m.clipped) problems.push(`${where}: clipped: ${item}`);
          for (const item of errors) problems.push(`${where}: ${item}`);
          for (const item of japanese) problems.push(`${where}: Japanese in English UI: ${item}`);
        }
      }
      return out;
    });
  }
  for (const [width, { ja, en }] of Object.entries(jumps)) {
    try { assert.deepEqual(en, ja); } catch { problems.push(`${width}px history-jump: ja and en differ`); }
  }
  results.jumps = jumps;
  results.problems = problems;
  writeFileSync(path.join(outDir, "measurements.json"), JSON.stringify(results, null, 2));
  if (problems.length) throw new Error(`${problems.length} problem(s):\n${problems.join("\n")}`);
  console.log(JSON.stringify({ result: "PASS", outDir, screenshots: WIDTHS.length * Object.keys(SCENARIOS).length * 2 }, null, 2));
} catch (error) {
  console.error(JSON.stringify({ result: "FAIL", root, outDir, error: error.message }, null, 2));
  process.exitCode = 1;
} finally {
  if (!keep && process.exitCode !== 1 && !args[1]) rmSync(root, { recursive: true, force: true });
  else if (!keep && process.exitCode !== 1) rmSync(path.join(root, "extension"), { recursive: true, force: true });
}
