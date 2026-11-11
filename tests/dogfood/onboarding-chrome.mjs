// Opt-in Chrome runtime dogfood. A fresh profile, APP_DIR and vault only. No registry changes.
// Native Messaging is replaced ONLY in a temporary extension copy by a loopback bridge to the
// real host process. This verifies Chrome UI/worker behavior, not installed-host registration.
// node tests/dogfood/onboarding-chrome.mjs <chrome.exe> <native-host.exe>
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { mkdtempSync, mkdirSync, cpSync, readFileSync, writeFileSync, existsSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { DatabaseSync } from "node:sqlite";

const [browserBin, hostBin] = process.argv.slice(2);
assert.ok(browserBin && hostBin, "pass browser and host executable paths");
const root = mkdtempSync(path.join(os.tmpdir(), "memocampus-24-chrome-"));
const app = path.join(root, "app");
const profile = path.join(root, "profile");
const extension = path.join(root, "extension");
mkdirSync(app);
cpSync(fileURLToPath(new URL("../../extension", import.meta.url)), extension, { recursive: true });
const env = { ...process.env, MEMOCAMPUS_APP_DIR: app };
delete env.MEMOCAMPUS_DB;
const calls = [];
const traces = [];
const storageWrites = [];
const heldResponses = new Map();
const pausedPaths = new Set(["/paused-before-start", "/paused-start-lost-on-restart"]);
const testPage = "<!doctype html><title>Isolated onboarding test</title><p>Synthetic local page</p>";
function releaseResponse(url) {
  const response = heldResponses.get(url);
  assert.ok(response, `request was not held: ${url}`);
  response.setHeader("Content-Type", "text/html; charset=utf-8");
  response.end(testPage);
  heldResponses.delete(url);
}
let loseStart = false;
function native(message) {
  calls.push(structuredClone(message));
  return new Promise((resolve, reject) => {
    const child = spawn(hostBin, [], { env, cwd: app, windowsHide: true, stdio: ["pipe", "pipe", "pipe"] });
    const chunks = [];
    const timer = setTimeout(() => { child.kill(); reject(new Error("host timeout")); }, 15_000);
    child.on("error", reject);
    child.stdout.on("data", (chunk) => chunks.push(chunk));
    child.on("close", () => {
      clearTimeout(timer);
      const bytes = Buffer.concat(chunks);
      if (bytes.length < 4) return reject(new Error("host gave no response"));
      resolve(JSON.parse(bytes.subarray(4, bytes.readUInt32LE(0) + 4)));
    });
    const body = Buffer.from(JSON.stringify({ requestId: crypto.randomUUID(), ...message }));
    const length = Buffer.alloc(4);
    length.writeUInt32LE(body.length);
    child.stdin.end(Buffer.concat([length, body]));
  });
}
const bridge = createServer(async (req, res) => {
  res.setHeader("Access-Control-Allow-Origin", "*");
  res.setHeader("Access-Control-Allow-Headers", "content-type");
  if (req.method === "OPTIONS") return res.end();
  if (req.url === "/observe" && req.method === "POST") {
    const parts = [];
    for await (const part of req) parts.push(part);
    const observation = JSON.parse(Buffer.concat(parts));
    if (observation.kind === "storage") storageWrites.push(observation.items);
    else traces.push(observation);
    res.end();
  } else if (pausedPaths.has(req.url)) {
    // Do not even send headers. Both onCommitted and onCompleted must happen AFTER first start.
    heldResponses.set(req.url, res);
  } else if (req.url === "/native" && req.method === "POST") {
    const parts = [];
    for await (const part of req) parts.push(part);
    const message = JSON.parse(Buffer.concat(parts));
    try {
      const reply = await native(message);
      // Return no Native Message reply, avoiding HTTP's own transport retries of a reset socket.
      if (loseStart && message.type === "policy_start") { res.setHeader("Content-Type", "application/json"); res.end("null"); return; }
      res.setHeader("Content-Type", "application/json");
      res.end(JSON.stringify(reply));
    } catch { res.statusCode = 503; res.end(); }
  } else {
    res.setHeader("Content-Type", "text/html; charset=utf-8");
    res.end(testPage);
  }
});
await new Promise((resolve) => bridge.listen(0, "127.0.0.1", resolve));
const origin = `http://127.0.0.1:${bridge.address().port}`;
writeFileSync(path.join(extension, "dogfood-transport.js"), `chrome.runtime.sendNativeMessage = (_name, message, reply) => {
  fetch(${JSON.stringify(origin + "/native")}, {method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify(message)})
    .then(response=>response.json()).then(reply,()=>reply(undefined));
};
if (typeof ServiceWorkerGlobalScope !== "undefined") {
  const observations = [];
  const observe = (value) => {
    const observed = fetch(${JSON.stringify(origin + "/observe")}, {method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify(value)}).catch(()=>{});
    observations.push(observed);
    return observed;
  };
  globalThis.__dogfoodDrainObservations = () => Promise.all(observations.slice());
  const set = chrome.storage.local.set.bind(chrome.storage.local);
  chrome.storage.local.set = (items) => { observe({kind:"storage",items}); return set(items); };
  for (const [event, kind] of [[chrome.webNavigation.onBeforeNavigate,"before"],[chrome.webNavigation.onCommitted,"committed"],[chrome.webNavigation.onCompleted,"completed"]]) {
    event.addListener((details) => { if (details.frameId === 0) observe({kind,...details,observedAt:Date.now()}); });
  }
  const completed = chrome.webNavigation.onCompleted;
  const listen = completed.addListener.bind(completed);
  completed.addListener = (handler, ...args) => listen((details) => Promise.resolve(handler(details)).finally(() => observe({kind:"completed-handler",...details})), ...args);
}`);
for (const entry of ["options.js", "sidepanel.js", "service-worker.js"]) {
  const target = path.join(extension, entry);
  let source = readFileSync(target, "utf8");
  // Negative control: reintroduce only the old completion-time decision in this throwaway copy.
  // The paused-response regression must fail, proving it detects the reported bug.
  if (entry === "service-worker.js" && process.argv.includes("--completion-time-regression")) {
    assert.ok(source.includes("const occurredAt = navigationStarts.completed(details);"));
    source = source.replace("const occurredAt = navigationStarts.completed(details);", "const occurredAt = Math.min(Date.now(), Math.floor(details.timeStamp));");
  }
  writeFileSync(target, 'import "./dogfood-transport.js";\n' + source);
}

const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
async function until(check, label, ms = 15_000) {
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
      clearTimeout(request.timer);
      if (message.error) request.reject(new Error(JSON.stringify(message.error)));
      else request.resolve(message.result);
    };
    await new Promise((resolve, reject) => { cdp.ws.onopen = resolve; cdp.ws.onerror = reject; });
    return cdp;
  }
  send(method, params = {}) {
    const id = ++this.next;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => { this.waiting.delete(id); reject(new Error(`CDP timeout: ${method}`)); }, 15_000);
      this.waiting.set(id, { resolve, reject, timer });
      this.ws.send(JSON.stringify({ id, method, params }));
    });
  }
  async evaluate(expression) {
    const answer = await this.send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
    if (answer.exceptionDetails) throw new Error(JSON.stringify(answer.exceptionDetails));
    return answer.result.value;
  }
}
let chrome;
const sockets = [];
try {
  chrome = spawn(browserBin, ["--headless=new", "--window-size=1100,900", "--no-first-run", "--no-default-browser-check", "--disable-sync", "--disable-background-networking",
    `--user-data-dir=${profile}`, "--remote-debugging-port=0", `--disable-extensions-except=${extension}`, `--load-extension=${extension}`, "about:blank"],
    { windowsHide: true, stdio: "ignore" });
  chrome.on("error", (error) => { console.error(error.message); });
  const portFile = path.join(profile, "DevToolsActivePort");
  const port = await until(async () => existsSync(portFile) && readFileSync(portFile, "utf8").split("\n")[0], "Chrome debugging port");
  const endpoint = `http://127.0.0.1:${port}`;
  const targets = () => fetch(`${endpoint}/json/list`).then((r) => r.json());
  const worker = await until(async () => (await targets()).find((t) => t.type === "service_worker" && t.url.endsWith("/service-worker.js")), "unpacked extension worker (use Chrome for Testing if stable Chrome disables --load-extension)");
  // URL.origin is null for chrome-extension in Node, so use the observed host explicitly.
  const base = `chrome-extension://${new URL(worker.url).host}`;
  const open = async (url) => {
    const target = await fetch(`${endpoint}/json/new?${encodeURIComponent(url)}`, { method: "PUT" }).then((r) => r.json());
    const cdp = await CDP.connect(target.webSocketDebuggerUrl);
    sockets.push(cdp.ws);
    return cdp;
  };
  const page = await open(`${base}/options.html`);
  await until(() => page.evaluate("document.querySelector('#onboarding')?.hidden === false && !document.querySelector('#vault-recheck').disabled"), "first-run page");
  assert.equal(await page.evaluate("document.querySelector('#start-recording').disabled"), true);
  assert.equal(existsSync(path.join(app, "policy.json")), false);
  const earlyPage = await open(`${origin}/before-start`);
  await pause(500);
  let sw = await CDP.connect(worker.webSocketDebuggerUrl);
  sockets.push(sw.ws);
  const storageBefore = await sw.evaluate("chrome.storage.local.get(null)");
  assert.equal(JSON.stringify(storageBefore).includes("before-start"), false);
  assert.equal(calls.some((m) => m.type === "visit"), false);
  assert.equal((await native({ type: "vault_create", path: path.join(root, "test.sqlite3") })).ok, true);
  await page.evaluate("document.querySelector('#vault-recheck').click()");
  await until(() => page.evaluate("!document.querySelector('#start-recording').disabled"), "usable vault reused");
  const second = await open(`${base}/options.html`);
  await until(() => second.evaluate("!document.querySelector('#start-recording').disabled"), "second onboarding page");
  // Capture the actual Japanese layout before starting, for visual inspection.
  const image = await page.send("Page.captureScreenshot", { format: "png", captureBeyondViewport: true });
  writeFileSync(path.join(root, "onboarding.png"), Buffer.from(image.data, "base64"));
  const heldPage = await open("about:blank");
  const restartPage = await open("about:blank");
  const navigating = heldPage.send("Page.navigate", { url: `${origin}/paused-before-start` });
  const navigatingAcrossRestart = restartPage.send("Page.navigate", { url: `${origin}/paused-start-lost-on-restart` });
  await until(async () => pausedPaths.size === heldResponses.size && [...pausedPaths].every((url) => traces.some((e) => e.kind === "before" && e.url === origin + url)), "main-frame requests started and responses held");
  assert.ok([...pausedPaths].every((url) => !traces.some((e) => ["committed", "completed"].includes(e.kind) && e.url === origin + url)));
  loseStart = true;
  await page.evaluate("document.querySelector('#start-recording').click(); document.querySelector('#start-recording').click()");
  await until(() => page.evaluate("document.querySelector('#policy-state').dataset.state === 'ready'"), "lost start response reconciled");
  assert.equal(calls.filter((m) => m.type === "policy_start").length, 1);
  const policy = readFileSync(path.join(app, "policy.json"), "utf8");
  await second.evaluate("document.querySelector('#start-recording').click()");
  await until(() => second.evaluate("document.querySelector('#policy-state').dataset.state === 'ready'"), "competing screen reconciled");
  assert.equal(readFileSync(path.join(app, "policy.json"), "utf8"), policy);
  assert.equal(calls.filter((m) => m.type === "policy_start").length, 1);
  assert.equal((await native({ type: "vault_status" })).status.publication, "pending");
  const startedAt = Number(readFileSync(path.join(app, "policy.initialized"), "utf8").trim().split("\n").at(-1));
  assert.ok(Number.isSafeInteger(startedAt));
  const checkedBatch = (url) => calls.find((m) => m.type === "policy_evaluate_batch" && m.items.some((item) => item.url === origin + url));
  const assertAbsent = async (url, expectedAt) => {
    await until(async () => traces.some((e) => e.kind === "completed-handler" && e.url === origin + url) && checkedBatch(url), "completion judged after response release");
    const lifecycle = traces.filter((e) => e.url === origin + url);
    assert.ok(lifecycle.some((e) => e.kind === "before" && e.observedAt <= startedAt), "request started before recording");
    for (const kind of ["committed", "completed"]) assert.ok(lifecycle.some((e) => e.kind === kind && e.observedAt > startedAt && e.timeStamp > startedAt), `${kind} happened after recording start`);
    // Wait for the actual completion handler, then drain the queue's existing exclusive chain and
    // all observation requests. No fixed delay and no final-state-only assertion can hide a write.
    await page.evaluate("chrome.runtime.sendMessage({type:'flush-queue'})");
    const workerTarget = await until(async () => (await targets()).find((t) => t.type === "service_worker" && t.url === worker.url), "active worker");
    const inspect = await CDP.connect(workerTarget.webSocketDebuggerUrl);
    sockets.push(inspect.ws);
    await inspect.evaluate("globalThis.__dogfoodDrainObservations()");
    assert.equal(JSON.stringify(storageWrites).includes(origin + url), false, "pre-start navigation reached durable storage after release");
    const stored = await inspect.evaluate("chrome.storage.local.get(['pendingVisitEvents','deadLetterEvents'])");
    assert.equal(JSON.stringify(stored).includes(origin + url), false, "absent from queue and dead letters");
    assert.equal(calls.some((m) => m.type === "visit" && m.url === origin + url), false, "never delivered as a visit");
    const db = new DatabaseSync(path.join(root, "test.sqlite3"), { readOnly: true });
    try { assert.equal(db.prepare("SELECT count(*) AS n FROM visits WHERE url = ?").get(origin + url).n, 0, "absent from Vault"); }
    finally { db.close(); }
    const batch = checkedBatch(url);
    if (expectedAt === null) assert.equal(batch.occurredAt, null, "lost start evidence is not replaced by commit/completion time");
    else assert.ok(batch.occurredAt <= startedAt, "start timestamp predates first recording");
    writeFileSync(path.join(root, url.slice(1) + ".json"), JSON.stringify({ lifecycle, startedAt, occurredAt: batch.occurredAt, absentFromStorageAndVault: true }, null, 2));
    inspect.ws.close();
  };
  releaseResponse("/paused-before-start");
  await navigating;
  await assertAbsent("/paused-before-start", "before");
  await earlyPage.send("Page.navigate", { url: `${origin}/after-start` });
  await until(async () => calls.some((m) => m.type === "visit" && m.url.endsWith("/after-start")), "post-start event delivered");
  await until(async () => {
    const db = new DatabaseSync(path.join(root, "test.sqlite3"), { readOnly: true });
    try { return db.prepare("SELECT count(*) AS n FROM visits WHERE url LIKE '%/after-start'").get().n === 1; }
    finally { db.close(); }
  }, "post-start row committed");
  await sw.evaluate("globalThis.__dogfoodDrainObservations()");
  assert.ok(JSON.stringify(storageWrites).includes(origin + "/after-start"), "storage observer saw the recorded control URL");
  const db = new DatabaseSync(path.join(root, "test.sqlite3"), { readOnly: true });
  assert.equal(db.prepare("SELECT count(*) AS n FROM visits WHERE url LIKE '%/before-start'").get().n, 0);
  db.close();
  // Terminate just this test worker. Chrome wakes a new instance on the next navigation.
  await sw.evaluate("globalThis.__dogfoodWorkerEpoch = 'before-restart'");
  sw.ws.close();
  await page.send("ServiceWorker.enable");
  await page.send("ServiceWorker.stopAllWorkers");
  releaseResponse("/paused-start-lost-on-restart");
  await navigatingAcrossRestart;
  await assertAbsent("/paused-start-lost-on-restart", null);
  await earlyPage.send("Page.navigate", { url: `${origin}/after-worker-restart` });
  await until(async () => calls.some((m) => m.type === "visit" && m.url.endsWith("/after-worker-restart")), "worker restart delivery");
  const restartedWorker = await until(async () => (await targets()).find((t) => t.type === "service_worker" && t.url === worker.url), "restarted worker target");
  sw = await CDP.connect(restartedWorker.webSocketDebuggerUrl);
  sockets.push(sw.ws);
  assert.equal(await sw.evaluate("typeof globalThis.__dogfoodWorkerEpoch"), "undefined", "worker memory was discarded");
  assert.equal(readFileSync(path.join(app, "policy.json"), "utf8"), policy);
  assert.equal(calls.some((m) => m.type === "vault_publication_confirm"), false);
  console.log(JSON.stringify({ result: "PASS", root, screenshot: path.join(root, "onboarding.png"), startRequests: 1,
    pausedBeforeStart: "queue/dead-letter/storage-write-history/Vault absent", lostStartAfterWorkerRestart: "refused", afterStartControl: "recorded",
    transport: "loopback bridge to real Native Host; installed Native Messaging registration untested" }));
} catch (error) {
  console.error(JSON.stringify({ result: "FAIL", root, error: error.message }));
  throw error;
} finally {
  for (const response of heldResponses.values()) response.destroy();
  for (const socket of sockets) socket.close();
  // This process owns only the isolated headless browser; no user browser tabs are closed.
  if (chrome && chrome.exitCode === null) chrome.kill();
  await new Promise((resolve) => bridge.close(resolve));
}
