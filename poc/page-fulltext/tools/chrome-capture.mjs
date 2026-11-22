// Generates Chrome-made MHTML from the SYNTHETIC pages in tools/pages (Slice 1 PoC).
//
//   node tools/chrome-capture.mjs --chrome "C:\Program Files\Google\Chrome\Application\chrome.exe" --out <dir> [--profile <empty dir>]
//
// - Pages are served from 127.0.0.1 only; no external site is contacted by this script.
// - Chrome runs headless with a fresh, throw-away profile and background networking disabled.
// - The archive is made with the DevTools Protocol `Page.captureSnapshot({format: "mhtml"})`.
//   That is Chrome's own MHTML serializer, but NOT the extension API `chrome.pageCapture.saveAsMHTML`
//   that Memocampus uses; results are reported with that distinction.
// - An existing output file is never overwritten.
import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const args = new Map();
for (let i = 2; i < process.argv.length; i += 2) args.set(process.argv[i], process.argv[i + 1]);
const chrome = args.get("--chrome");
const out = args.get("--out");
if (!chrome || !out) {
  console.error("usage: chrome-capture.mjs --chrome <chrome.exe> --out <dir> [--profile <dir>]");
  process.exit(2);
}

// 1x1 transparent PNG.
const PIXEL = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==", "base64");
function largePage() {
  const para = "大規模ページの段落です。Large page paragraph with mixed 日本語 and English text for size measurement. ";
  const body = Array.from({ length: 8000 }, (_, i) => `<p>${i}: ${para}</p>`).join("\n");
  return `<!DOCTYPE html><html lang="ja"><head><meta charset="utf-8"><title>大規模な合成ページ</title></head><body><h1>大規模</h1>${body}</body></html>`;
}
const routes = {
  "/ja-article.html": ["text/html; charset=utf-8", () => readFileSync(join(here, "pages", "ja-article.html"))],
  "/en-article.html": ["text/html; charset=utf-8", () => readFileSync(join(here, "pages", "en-article.html"))],
  "/complex.html": ["text/html; charset=utf-8", () => readFileSync(join(here, "pages", "complex.html"))],
  "/frame.html": ["text/html; charset=utf-8", () => readFileSync(join(here, "pages", "frame.html"))],
  "/js-heavy.html": ["text/html; charset=utf-8", () => readFileSync(join(here, "pages", "js-heavy.html"))],
  // Served WITHOUT a charset parameter: the page's own <meta> declares Shift_JIS.
  "/sjis-news.html": ["text/html", () => readFileSync(join(here, "pages", "sjis-news.html"))],
  "/large.html": ["text/html; charset=utf-8", () => Buffer.from(largePage(), "utf8")],
  "/style.css": ["text/css", () => Buffer.from("body{font-family:sans-serif}.x::after{content:'CSS-CONTENT-CANARY'}")],
  "/pixel.png": ["image/png", () => PIXEL],
};
const captures = ["ja-article", "en-article", "complex", "js-heavy", "sjis-news", "large"];

const server = createServer((req, res) => {
  const route = routes[new URL(req.url, "http://127.0.0.1").pathname];
  if (!route) { res.writeHead(404).end(); return; }
  res.writeHead(200, { "Content-Type": route[0] }).end(route[1]());
});
await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
const origin = `http://127.0.0.1:${server.address().port}`;

const profile = args.get("--profile") ?? mkdtempSync(join(tmpdir(), "memocampus-mhtml-poc-"));
const child = spawn(chrome, [
  "--headless=new", "--remote-debugging-port=0", `--user-data-dir=${profile}`,
  "--no-first-run", "--no-default-browser-check", "--disable-extensions", "--disable-sync",
  "--disable-background-networking", "--disable-component-update", "--disable-default-apps",
  "--disable-domain-reliability", "--no-pings", "--metrics-recording-only", "about:blank",
], { stdio: "ignore" });

async function devtoolsUrl() {
  const file = join(profile, "DevToolsActivePort");
  for (let i = 0; i < 200; i++) {
    if (existsSync(file)) {
      const [port, path] = readFileSync(file, "utf8").split(/\r?\n/);
      if (port && path) return `ws://127.0.0.1:${port}${path}`;
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error("Chrome did not open its DevTools port");
}

const ws = new WebSocket(await devtoolsUrl());
await new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = reject; });
let nextId = 1;
const pending = new Map();
const listeners = new Set();
ws.onmessage = (event) => {
  const msg = JSON.parse(event.data);
  if (msg.id && pending.has(msg.id)) {
    const { resolve, reject } = pending.get(msg.id);
    pending.delete(msg.id);
    if (msg.error) reject(new Error(JSON.stringify(msg.error))); else resolve(msg.result);
  } else for (const l of listeners) l(msg);
};
const send = (method, params = {}, sessionId) => new Promise((resolve, reject) => {
  const id = nextId++;
  pending.set(id, { resolve, reject });
  ws.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }));
});

const results = [];
try {
  const version = await send("Browser.getVersion");
  for (const name of captures) {
    const target = join(out, `${name}.mhtml`);
    if (existsSync(target)) { results.push({ name, skipped: "exists" }); continue; }
    const { targetId } = await send("Target.createTarget", { url: "about:blank" });
    const { sessionId } = await send("Target.attachToTarget", { targetId, flatten: true });
    await send("Page.enable", {}, sessionId);
    const loaded = new Promise((resolve) => {
      const l = (m) => { if (m.sessionId === sessionId && m.method === "Page.loadEventFired") { listeners.delete(l); resolve(); } };
      listeners.add(l);
    });
    await send("Page.navigate", { url: `${origin}/${name}.html` }, sessionId);
    await loaded;
    await new Promise((r) => setTimeout(r, 500));
    const { data } = await send("Page.captureSnapshot", { format: "mhtml" }, sessionId);
    const bytes = Buffer.from(data, "utf8");
    const nonAscii = [...bytes].some((b) => b > 0x7e && b !== 0x0a && b !== 0x0d);
    writeFileSync(target, bytes, { flag: "wx" });
    results.push({ name, bytes: bytes.length, nonAscii });
    await send("Target.closeTarget", { targetId });
  }
  console.log(JSON.stringify({ chrome: version.product, method: "Page.captureSnapshot(mhtml)", results }, null, 2));
} finally {
  ws.close();
  child.kill();
  server.close();
  await new Promise((r) => setTimeout(r, 500));
  if (!args.get("--profile")) rmSync(profile, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
}
