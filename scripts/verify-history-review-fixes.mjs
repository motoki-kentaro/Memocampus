// Isolated headless Chrome check for HISTORY-05/06. No Native Messaging registration or real Vault/profile.
// Run: node scripts/verify-history-review-fixes.mjs [path-to-chrome.exe]
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const chrome = process.argv[2] ?? process.env.MEMOCAMPUS_TEST_CHROME ?? "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe";
assert.ok(existsSync(chrome), "Specify the Chrome executable; no browser is downloaded.");
const extension = fileURLToPath(new URL("../extension/", import.meta.url));
const driver = `
import { createHistoryApp } from './history-app.js';
const at = new Date(2026,10,17,12).toISOString();
const item = (id) => ({id,url:'https://'+id+'.synthetic.test/',title:id==='cur'?'Synthetic current visit':'Synthetic '+id,visitedAt:at,host:id+'.synthetic.test',urlTruncated:false,titleTruncated:false});
const send = async (m) => {
  window.probeCalls.push(m);
  const base = {ok:true,type:m.type+'_result',requestId:m.requestId,vaultToken:'ab'.repeat(32)};
  if(m.type==='host_capabilities')return {...base,capabilities:['history_v1']};
  if(m.type==='history_days')return {...base,counts:m.bounds.slice(1).map(()=>0)};
  if(m.type==='history_list')return {...base,items:['cur','cz','uk'].map(item),total:3,page:m.page,size:m.size,pages:1};
  return {...base,visit:item(m.id),navigation:{kind:'document',transitionType:'link',occurredAt:at},
    source:m.id==='cur'?{state:'present',visit:item('src')}:m.id==='cz'?{state:'censored',visit:item('HIDDEN')}:{state:'unknown'},
    targets:[{...item('dest'),kind:'document',transitionType:'link',occurredAt:at}],targetCount:1,targetsTruncated:false,
    pageMetadata:{ogTitle:'Synthetic metadata',capturedAt:at}};
};
window.probeCalls=[];
window.probeApp=createHistoryApp({document,win:window,send,tabs:{create:async()=>{throw Error('Unexpected tab creation');}},now:()=>new Date(2026,10,17,12)});
await window.probeApp.start();
window.probeReady=true;
`;
const server = http.createServer((request, response) => {
  const name = new URL(request.url, "http://127.0.0.1").pathname;
  if (name === "/history.js") {
    response.setHeader("Content-Type", "application/javascript");
    response.end(driver);
    return;
  }
  const target = path.resolve(extension, name === "/" ? "history.html" : `.${name}`);
  if (!target.startsWith(path.resolve(extension) + path.sep) || !/\.(html|css|js)$/.test(target)) {
    response.writeHead(404).end();
    return;
  }
  try {
    response.setHeader("Content-Type", target.endsWith(".js") ? "application/javascript" : target.endsWith(".css") ? "text/css" : "text/html");
    response.end(readFileSync(target));
  } catch { response.writeHead(404).end(); }
});
const profile = mkdtempSync(path.join(os.tmpdir(), "memocampus-history-fixes-chrome-"));
const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
let child, cdp, exit;
const checks = [];
try {
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  child = spawn(chrome, ["--headless=new", "--disable-gpu", "--disable-extensions", "--no-first-run", "--no-default-browser-check", "--disable-background-networking", "--disable-component-update", "--no-proxy-server", "--host-resolver-rules=MAP * ~NOTFOUND, EXCLUDE 127.0.0.1", `--user-data-dir=${profile}`, "--remote-debugging-address=127.0.0.1", "--remote-debugging-port=0", "about:blank"], { windowsHide: true, stdio: "ignore" });
  exit = new Promise((resolve) => { child.on("error", resolve); child.on("exit", resolve); });
  const portFile = path.join(profile, "DevToolsActivePort");
  for (let i = 0; !existsSync(portFile) && i < 100; i++) await delay(50);
  assert.ok(existsSync(portFile), "The isolated Chrome did not start its test endpoint.");
  const port = Number(readFileSync(portFile, "utf8").split("\n")[0]);
  const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
  const socket = new WebSocket(targets.find((target) => target.type === "page").webSocketDebuggerUrl);
  await new Promise((resolve, reject) => { socket.onopen = resolve; socket.onerror = reject; });
  let serial = 0;
  const pending = new Map();
  socket.onmessage = (event) => {
    const message = JSON.parse(event.data);
    const waiting = pending.get(message.id);
    if (!waiting) return;
    pending.delete(message.id);
    clearTimeout(waiting.timer);
    if (message.error) waiting.reject(new Error("Chrome test protocol error")); else waiting.resolve(message.result);
  };
  socket.onclose = () => { for (const wait of pending.values()) { clearTimeout(wait.timer); wait.reject(new Error("Chrome test endpoint closed")); } pending.clear(); };
  cdp = (method, params = {}) => new Promise((resolve, reject) => {
    const id = ++serial;
    const timer = setTimeout(() => { pending.delete(id); reject(new Error(`Chrome test timeout: ${method}`)); }, 10000);
    pending.set(id, { resolve, reject, timer });
    socket.send(JSON.stringify({ id, method, params }));
  });
  const evaluate = async (expression) => {
    const result = await cdp("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true });
    assert.ok(!result.exceptionDetails, "Browser assertion script failed.");
    return result.result.value;
  };
  const waitFor = async (expression) => {
    for (let i = 0; i < 100; i++) { if (await evaluate(expression)) return; await delay(20); }
    throw new Error("Browser test state was not reached.");
  };
  const click = (selector) => evaluate(`document.querySelector(${JSON.stringify(selector)}).click()`);
  await cdp("Page.navigate", { url: `http://127.0.0.1:${server.address().port}/?date=2026-11-17` });
  await waitFor("window.probeReady && document.querySelectorAll('.visit-row').length===3");
  const hidden = await evaluate("({hidden:document.querySelector('#cal-months').hidden,display:getComputedStyle(document.querySelector('#cal-months')).display,visible:document.querySelector('#cal-months').checkVisibility()})");
  assert.deepEqual(hidden, { hidden: true, display: "none", visible: false });
  checks.push("initial hidden=true, display=none, invisible");
  const baseline = await evaluate("JSON.stringify({shown:probeApp.shownMonth,state:probeApp.state,url:location.search,calls:probeCalls.length})");
  await click("#cal-months button");
  assert.equal(await evaluate("JSON.stringify({shown:probeApp.shownMonth,state:probeApp.state,url:location.search,calls:probeCalls.length})"), baseline);
  checks.push("days-view accidental month click is inert (no null year)");
  await click("#cal-title");
  assert.equal(await evaluate("getComputedStyle(document.querySelector('#cal-months')).display"), "grid");
  const calls = await evaluate("probeCalls.length");
  await click("#cal-prev"); await click("#cal-next");
  assert.equal(await evaluate("probeCalls.length"), calls);
  await click('#cal-months [data-month="1"]');
  assert.deepEqual(await evaluate("probeApp.shownMonth"), { year: 2026, monthIndex: 1 });
  assert.equal(await evaluate("probeApp.state.date.monthIndex"), 10);
  assert.equal(await evaluate("location.search"), "?date=2026-11-17");
  await click("#cal-title");
  await cdp("Input.dispatchKeyEvent", { type: "keyDown", key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
  await cdp("Input.dispatchKeyEvent", { type: "keyUp", key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
  assert.equal(await evaluate("document.activeElement.id"), "cal-title");
  assert.equal(await evaluate("getComputedStyle(document.querySelector('#cal-months')).display"), "none");
  checks.push("picker open/year/month/Escape and selected-day/URL independence");
  await click("#cal-jump");
  await click('[data-visit-id="cur"] .visit-toggle');
  await waitFor("document.querySelector('.detail-card-source a')");
  assert.deepEqual(await evaluate("Array.from(document.querySelector('[data-visit-id=cur] .visit-detail').children,c=>c.className)"), ["detail-card detail-card-source", "detail-card detail-card-visit", "detail-card detail-card-targets", "detail-card detail-card-meta"]);
  await evaluate("document.querySelector('.detail-card-source a').focus()");
  for (const type of ["keyDown", "keyUp"]) await cdp("Input.dispatchKeyEvent", { type, key: "Tab", code: "Tab", windowsVirtualKeyCode: 9 });
  assert.equal(await evaluate("document.activeElement.closest('.detail-card').className"), "detail-card detail-card-visit");
  assert.equal(await evaluate("document.querySelectorAll('.visit-detail [tabindex]').length"), 0);
  checks.push("pipeline DOM order and real Tab: source URL -> current URL");
  await click('[data-visit-id="cz"] .visit-toggle');
  await waitFor("document.querySelector('.detail-censored')");
  for (const theme of ["light", "dark"]) {
    await cdp("Emulation.setEmulatedMedia", { features: [{ name: "prefers-color-scheme", value: theme }] });
    const badge = await evaluate("(()=>{const e=document.querySelector('.detail-censored'),s=getComputedStyle(e);return {text:e.textContent,bg:s.backgroundColor,fg:s.color,weight:s.fontWeight,animation:s.animationName}})()");
    assert.deepEqual(badge, { text: "[CENSORED]", bg: "rgb(255, 235, 59)", fg: "rgb(198, 40, 40)", weight: "700", animation: "none" });
    for (const width of [720, 1280]) {
      await cdp("Emulation.setDeviceMetricsOverride", { width, height: 900, deviceScaleFactor: 1, mobile: false });
      assert.equal(await evaluate("document.documentElement.scrollWidth<=innerWidth"), true);
    }
    checks.push(`${theme}: CENSORED colors, no animation, 720/1280px no horizontal overflow`);
  }
  await cdp("Emulation.setEmulatedMedia", { features: [{ name: "forced-colors", value: "active" }] });
  assert.equal(await evaluate("(()=>{const s=getComputedStyle(document.querySelector('.detail-censored'));return s.color!==s.backgroundColor})()"), true);
  checks.push("forced-colors badge foreground differs from background");
  await cdp("Emulation.setEmulatedMedia", { features: [{ name: "prefers-reduced-motion", value: "reduce" }] });
  assert.equal(await evaluate("getComputedStyle(document.querySelector('.calendar-month')).transitionDuration"), "0s");
  checks.push("reduced-motion disables month transitions");
  console.log(JSON.stringify({ passed: checks.length, checks }, null, 2));
} finally {
  if (cdp) await cdp("Browser.close").catch(() => {});
  if (child) {
    const closed = await Promise.race([exit.then(() => true), delay(3000).then(() => false)]);
    if (!closed) { child.kill(); await exit; } // only this script's isolated child, never an existing Chrome/Host
  }
  await new Promise((resolve) => server.close(resolve));
  const absolute = path.resolve(profile);
  assert.ok(absolute.startsWith(path.resolve(os.tmpdir()) + path.sep) && path.basename(absolute).startsWith("memocampus-history-fixes-chrome-"));
  rmSync(absolute, { recursive: true, force: true });
}
