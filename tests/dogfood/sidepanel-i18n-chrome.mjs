// Opt-in Chrome runtime dogfood for Issue #77 Slice 2 (Side Panel i18n). A fresh temporary profile and a
// temporary copy of the extension only; no registry, no Vault, no installed Native Host. In the copy, the
// Side Panel's Native Messaging is bridged (CDP Runtime.addBinding) to the tests' fake host
// (tests/extension/fake-dom.js createHost, delete-fixtures.js), and the service worker's is a stub that
// always fails, so nothing installed on the machine can be started.
//
//   node tests/dogfood/sidepanel-i18n-chrome.mjs <chrome.exe> [outDir] [--keep]
//
// For ja and en, and for widths of 320, 400 and 600 px, it opens sidepanel.html and checks: i18n-dom.js
// applied (<html lang>, the Vault section lang="ja"), no horizontal scroll, no control outside the viewport,
// no two controls overlapping, no clipped control text, no console error; in English no Japanese outside
// the Vault section. Scenarios: URL tab, host-wide rule dialog, deletion dialog, calendar, Image details,
// Page tab. Screenshots and the measurements are written to outDir.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createHost, imageSummary } from "../extension/fake-dom.js";
import { createDeletePort, visitRow } from "../extension/delete-fixtures.js";

const args = process.argv.slice(2).filter((arg) => !arg.startsWith("--"));
const [browserBin] = args;
const keep = process.argv.includes("--keep");
assert.ok(browserBin, "pass the chrome.exe path (Chrome for Testing, or Chrome that still accepts --load-extension)");
const root = mkdtempSync(path.join(os.tmpdir(), "memocampus-77-sidepanel-"));
const outDir = args[1] ?? path.join(root, "out");
mkdirSync(outDir, { recursive: true });
const extension = path.join(root, "extension");
cpSync(fileURLToPath(new URL("../../extension", import.meta.url)), extension, { recursive: true });

// The worker: Native Messaging always fails (as in tests/dogfood/i18n-chrome.mjs).
writeFileSync(path.join(extension, "dogfood-isolation.js"), `
chrome.runtime.sendNativeMessage = (_name, _message, reply) => { queueMicrotask(() => reply?.(undefined)); };
chrome.runtime.connectNative = () => { throw new Error("dogfood: native messaging is disabled"); };
`);
const worker = path.join(extension, "service-worker.js");
writeFileSync(worker, 'import "./dogfood-isolation.js";\n' + readFileSync(worker, "utf8"));
// The Side Panel: Native Messaging goes to the harness (the fake host), never to a real one.
writeFileSync(path.join(extension, "dogfood-bridge.js"), `
const pending = new Map(), ports = new Map();
let seq = 0;
const call = (payload) => globalThis.__native(JSON.stringify(payload));
globalThis.__nativeReply = (id, response) => { const done = pending.get(id); pending.delete(id); done?.(response); };
globalThis.__portMessage = (id, message) => ports.get(id)?.message.forEach((fn) => fn(message));
globalThis.__portDisconnect = (id) => { const port = ports.get(id); ports.delete(id); port?.disconnect.forEach((fn) => fn()); };
chrome.runtime.sendNativeMessage = (_name, message, reply) => { const id = ++seq; pending.set(id, (response) => reply?.(response)); call({ kind: "send", id, message }); };
chrome.runtime.connectNative = () => {
  const id = ++seq, listeners = { message: [], disconnect: [] };
  ports.set(id, listeners);
  call({ kind: "connect", id });
  return {
    postMessage: (message) => call({ kind: "post", id, message }),
    disconnect: () => { ports.delete(id); call({ kind: "disconnect", id }); },
    onMessage: { addListener: (fn) => listeners.message.push(fn), removeListener() {} },
    onDisconnect: { addListener: (fn) => listeners.disconnect.push(fn), removeListener() {} },
  };
};
`);
const panelScript = path.join(extension, "sidepanel.js");
writeFileSync(panelScript, 'import "./dogfood-bridge.js";\n' + readFileSync(panelScript, "utf8"));

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

// The fake host's data: long English and Japanese titles, so wrapping is exercised; web text is never translated.
function fakeHost() {
  const rows = [
    { url: "https://docs.example.test/guides/a-very-long-path/with/many/segments?query=string&and=more", title: "A rather long English page title that has to wrap inside a narrow side panel", lastVisitedAt: new Date().toISOString(), lastCapturedAt: new Date().toISOString(), lastActivityAt: new Date().toISOString(), source: "visit" },
    { url: "https://news.example.test/article", title: "Short title", lastVisitedAt: "2026-10-12T00:00:00.000Z", lastCapturedAt: null, lastActivityAt: "2026-10-12T00:00:00.000Z", source: "visit" },
    { url: "https://jp.example.test/", title: "日本語のページタイトル（翻訳しない）", lastVisitedAt: "2026-10-11T00:00:00.000Z", lastCapturedAt: null, lastActivityAt: "2026-10-11T00:00:00.000Z", source: "visit" },
  ];
  const host = createHost({
    vaults: { "C:\\dogfood\\memocampus.sqlite3": rows }, active: "C:\\dogfood\\memocampus.sqlite3",
    capabilities: ["navigation_v1", "vault_items_v1", "visit_calendar_v1", "item_delete_v1"],
    images: { "C:\\dogfood\\memocampus.sqlite3": Array.from({ length: 7 }, (_, i) => imageSummary(i + 1, Date.now() - i * 60_000)) },
  });
  host.visits = { [host.active]: rows.flatMap((row, i) => [visitRow(`v${i}a`, row.url, row.title), visitRow(`v${i}b`, row.url, row.title)]) };
  host.snapshots = { [host.active]: [rows[0].url] };
  // All three privacy states on screen, so their longest English labels are measured too.
  host.itemPolicy.never.push(rows[1].url);
  host.itemPolicy.private.push(rows[2].url);
  return host;
}

const JAPANESE = /[ぁ-んァ-ヶ一-龠（）「」、。・]/u;
const WIDTHS = [320, 400, 600];

// Runs in the page: what a person would see go wrong in the visible part of the layout.
const MEASURE = `(() => {
  const width = innerWidth;
  const visible = (el) => { const r = el.getBoundingClientRect(); const s = getComputedStyle(el); return r.width > 0 && r.height > 0 && s.visibility !== "hidden" && s.display !== "none"; };
  const controls = [...document.querySelectorAll("button, input, select, label, a.hit, summary, .tag-chip")].filter(visible);
  const outside = controls.filter((el) => { const r = el.getBoundingClientRect(); return r.left < -0.5 || r.right > width + 0.5; }).map((el) => el.id || el.className || el.tagName);
  const clipped = [...document.querySelectorAll("button, label, .summary, .tab, legend, h2, h3")].filter(visible)
    .filter((el) => el.scrollWidth > el.clientWidth + 1 && getComputedStyle(el).overflowX !== "visible" || el.scrollHeight > el.clientHeight + 1 && getComputedStyle(el).overflowY === "hidden")
    .map((el) => (el.id || el.className || el.tagName) + ": " + el.textContent.trim().slice(0, 40));
  // Only what is on top counts: a popover or dialog over the list is the design, not an overlap.
  const onTop = (el) => { const r = el.getBoundingClientRect(); const hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2); return hit !== null && (hit === el || el.contains(hit)); };
  const boxes = controls.filter((el) => el.matches("button, input, select") && onTop(el)).map((el) => [el, el.getBoundingClientRect()]);
  const overlaps = [];
  for (let i = 0; i < boxes.length; i++) for (let j = i + 1; j < boxes.length; j++) {
    const [a, ra] = boxes[i], [b, rb] = boxes[j];
    if (a.contains(b) || b.contains(a)) continue;
    // A popover or dialog lies over the list by design: only controls on the same layer can collide.
    const layer = (el) => el.closest("dialog, [role=dialog], [role=alertdialog], [role=menu]");
    if (layer(a) !== layer(b)) continue;
    // The trash icon sits on the corner of its own tile by design.
    if (a.parentElement === b.parentElement && [a, b].some((el) => el.classList.contains("tile-trash"))) continue;
    const x = Math.min(ra.right, rb.right) - Math.max(ra.left, rb.left), y = Math.min(ra.bottom, rb.bottom) - Math.max(ra.top, rb.top);
    if (x > 1 && y > 1) overlaps.push((a.id || a.className) + " × " + (b.id || b.className));
  }
  const vault = document.querySelector("#vault-panel");
  const text = [...document.querySelectorAll("body *")].filter((el) => !vault.contains(el) && visible(el) && el.children.length === 0).map((el) => el.textContent.trim()).filter(Boolean);
  const attributes = [...document.querySelectorAll("[aria-label], [title], [placeholder]")].filter((el) => !vault.contains(el)).flatMap((el) => ["aria-label", "title", "placeholder"].map((a) => el.getAttribute(a)).filter(Boolean));
  return { width, scrollWidth: document.documentElement.scrollWidth, outside, clipped, overlaps, text, attributes,
    lang: document.documentElement.lang, vaultLang: vault.getAttribute("lang") };
})()`;

async function session(lang, run) {
  const profile = path.join(root, `profile-${lang}`);
  rmSync(path.join(profile, "DevToolsActivePort"), { force: true });
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
    const open = async (width) => {
      const created = await fetch(`${endpoint}/json/new?about:blank`, { method: "PUT" }).then((r) => r.json());
      const page = await CDP.connect(created.webSocketDebuggerUrl);
      sockets.push(page.ws);
      const host = fakeHost();
      const ports = new Map();
      const errors = [];
      page.on("Runtime.exceptionThrown", (p) => errors.push(`exception: ${p.exceptionDetails.exception?.description ?? p.exceptionDetails.text}`));
      page.on("Runtime.consoleAPICalled", (p) => { if (p.type === "error" || p.type === "assert") errors.push(`console.${p.type}: ${p.args.map((a) => a.value ?? a.description).join(" ")}`); });
      page.on("Log.entryAdded", (p) => { if (p.entry.level === "error") errors.push(`log: ${p.entry.text} ${p.entry.url ?? ""}`); });
      page.on("Runtime.bindingCalled", (p) => {
        if (p.name !== "__native") return;
        const call = JSON.parse(p.payload);
        const reply = (expression) => void page.evaluate(expression).catch(() => {});
        if (call.kind === "send") Promise.resolve(host.handle(structuredClone(call.message))).then((response) => reply(`__nativeReply(${call.id}, ${JSON.stringify(response ?? null)})`));
        if (call.kind === "connect") {
          const fake = createDeletePort(host);
          fake.onMessage.addListener((message) => reply(`__portMessage(${call.id}, ${JSON.stringify(message)})`));
          fake.onDisconnect.addListener(() => reply(`__portDisconnect(${call.id})`));
          ports.set(call.id, fake);
        }
        if (call.kind === "post") ports.get(call.id)?.postMessage(structuredClone(call.message));
        if (call.kind === "disconnect") ports.get(call.id)?.disconnect();
      });
      await page.send("Runtime.enable");
      await page.send("Log.enable");
      await page.send("Runtime.addBinding", { name: "__native" });
      await page.send("Emulation.setDeviceMetricsOverride", { width, height: 900, deviceScaleFactor: 1, mobile: false });
      await page.send("Page.enable");
      await page.send("Page.navigate", { url: `${base}/sidepanel.html` });
      await until(() => page.evaluate("document.readyState === 'complete' && document.querySelector('#results').children.length > 0"), "side panel with results");
      await pause(300);
      return { page, host, errors };
    };
    return await run({ open });
  } finally {
    for (const socket of sockets) socket.close();
    if (chrome.exitCode === null) chrome.kill();
    await new Promise((resolve) => (chrome.exitCode !== null ? resolve() : chrome.once("exit", resolve)));
    await pause(500);
  }
}

const SCENARIOS = {
  url: async () => {},
  policy: async (page) => {
    await page.evaluate(`(() => { const select = document.querySelector("#results li select.item-action"); select.value = "save_but_no_mcp"; select.dispatchEvent(new Event("change", { bubbles: true })); })()`);
    await until(() => page.evaluate(`!document.querySelector("#item-dialog").hidden && !document.querySelector("#item-dialog-confirm").disabled`), "policy preview");
    await page.evaluate(`document.querySelector("#item-scope-host").click()`);
    await until(() => page.evaluate(`!document.querySelector("#item-dialog-ack").hidden`), "host-wide preview");
    await page.evaluate(`document.querySelector("#item-dialog").scrollIntoView()`);
  },
  delete: async (page) => {
    await page.evaluate(`document.querySelector("#url-delete-all").click()`);
    await until(() => page.evaluate(`!document.querySelector("#item-delete-dialog").hidden && document.querySelector("#item-delete-description").textContent.length > 0`), "deletion preview");
  },
  calendar: async (page) => {
    await until(() => page.evaluate(`!document.querySelector("#visit-calendar-open").disabled`), "calendar available");
    await page.evaluate(`document.querySelector("#visit-calendar-open").click()`);
    await until(() => page.evaluate(`!document.querySelector("#visit-calendar").hidden && document.querySelectorAll("#visit-calendar .calendar-day").length > 27`), "calendar open");
  },
  image: async (page) => {
    await page.evaluate(`document.querySelector("#tab-image").click()`);
    await until(() => page.evaluate(`document.querySelectorAll("#gallery-grid li").length > 0`), "image tiles");
    await page.evaluate(`document.querySelector("#gallery-grid li button").click()`);
    await until(() => page.evaluate(`!document.querySelector("#gallery-details").hidden`), "image details");
    await pause(300);
    await page.evaluate(`document.querySelector("#gallery-details").scrollIntoView()`);
  },
  page: async (page) => {
    await page.evaluate(`document.querySelector("#tab-page").click()`);
    await pause(600);
  },
};

const results = { root, outDir, sessions: {} };
const problems = [];
try {
  for (const lang of ["ja", "en"]) {
    results.sessions[lang] = await session(lang, async ({ open }) => {
      const out = {};
      for (const width of WIDTHS) {
        for (const [name, scenario] of Object.entries(SCENARIOS)) {
          const { page, errors } = await open(width);
          await scenario(page);
          await pause(200);
          const m = await page.evaluate(MEASURE);
          const shot = await page.send("Page.captureScreenshot", { format: "png", captureBeyondViewport: false });
          const file = path.join(outDir, `sidepanel-${lang}-${width}-${name}.png`);
          writeFileSync(file, Buffer.from(shot.data, "base64"));
          const japanese = lang === "en" ? [...m.text, ...m.attributes].filter((s) => JAPANESE.test(s) && !s.includes("日本語のページタイトル")) : [];
          const entry = { scrollWidth: m.scrollWidth, outside: m.outside, clipped: m.clipped, overlaps: m.overlaps, errors, japanese, lang: m.lang, vaultLang: m.vaultLang, screenshot: path.basename(file) };
          out[`${width}-${name}`] = entry;
          const where = `${lang} ${width}px ${name}`;
          if (m.lang !== lang) problems.push(`${where}: <html lang="${m.lang}">`);
          if (m.vaultLang !== "ja") problems.push(`${where}: the Vault section is not lang="ja"`);
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
