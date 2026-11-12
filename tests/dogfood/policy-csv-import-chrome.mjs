// Same isolated-profile / temporary-extension / loopback-native bridge as onboarding-chrome.mjs.
// Unlike the Export fixture harness, this uses the REAL Host for validation, apply and Gate checks.
// node tests/dogfood/policy-csv-import-chrome.mjs <chrome-for-testing.exe> <native-host.exe>
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { mkdtempSync, mkdirSync, cpSync, readFileSync, writeFileSync, existsSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { rulesToCsv } from "../../extension/policy-csv.js";

const [browserBin, hostBin] = process.argv.slice(2);
assert.ok(browserBin && hostBin, "pass Chrome and Native Host executables");
const root = mkdtempSync(path.join(os.tmpdir(), "memocampus-23-import-chrome-"));
const app = path.join(root, "app"), profile = path.join(root, "profile"), extension = path.join(root, "extension");
mkdirSync(app);
cpSync(fileURLToPath(new URL("../../extension", import.meta.url)), extension, { recursive: true });
const env = { ...process.env, MEMOCAMPUS_APP_DIR: app };
delete env.MEMOCAMPUS_DB;
const calls = [];
function native(message) {
  calls.push(structuredClone(message));
  return new Promise((resolve, reject) => {
    const child = spawn(hostBin, [], { env, cwd: app, windowsHide: true, stdio: ["pipe", "pipe", "pipe"] });
    const chunks = [];
    const timer = setTimeout(() => { child.kill(); reject(new Error("Host timeout")); }, 15_000);
    child.on("error", reject);
    child.stdout.on("data", (chunk) => chunks.push(chunk));
    child.on("close", () => {
      clearTimeout(timer);
      const bytes = Buffer.concat(chunks);
      if (bytes.length < 4) return reject(new Error("Host gave no response"));
      resolve(JSON.parse(bytes.subarray(4, bytes.readUInt32LE(0) + 4)));
    });
    const body = Buffer.from(JSON.stringify({ requestId: crypto.randomUUID(), ...message }));
    const length = Buffer.alloc(4); length.writeUInt32LE(body.length);
    child.stdin.end(Buffer.concat([length, body]));
  });
}
const bridge = createServer(async (req, res) => {
  res.setHeader("Access-Control-Allow-Origin", "*");
  res.setHeader("Access-Control-Allow-Headers", "content-type");
  if (req.method === "OPTIONS") return res.end();
  if (req.method !== "POST" || req.url !== "/native") { res.statusCode = 404; res.end(); return; }
  const chunks = []; for await (const chunk of req) chunks.push(chunk);
  try {
    const result = await native(JSON.parse(Buffer.concat(chunks)));
    res.setHeader("Content-Type", "application/json"); res.end(JSON.stringify(result));
  } catch { res.statusCode = 503; res.end(); }
});
await new Promise((resolve) => bridge.listen(0, "127.0.0.1", resolve));
const origin = `http://127.0.0.1:${bridge.address().port}`;
writeFileSync(path.join(extension, "import-test-transport.js"), `
chrome.runtime.sendNativeMessage = (_name, message, reply) => {
  fetch(${JSON.stringify(origin + "/native")}, {method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(message)})
    .then(response=>response.json()).then(reply,()=>reply(undefined));
};`);
for (const entry of ["options.js", "sidepanel.js", "service-worker.js"]) {
  const target = path.join(extension, entry);
  let source = readFileSync(target, "utf8");
  if (entry === "options.js") source = source.replace("startOptionsPage({", "globalThis.__importPage = startOptionsPage({");
  writeFileSync(target, 'import "./import-test-transport.js";\n' + source);
}

const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
async function until(check, label) {
  const deadline = Date.now() + 20_000;
  while (Date.now() < deadline) { const result = await check(); if (result) return result; await pause(100); }
  throw new Error(`timeout: ${label}`);
}
class CDP {
  static async connect(url) {
    const cdp = new CDP(); cdp.ws = new WebSocket(url); cdp.next = 0; cdp.pending = new Map(); cdp.events = [];
    cdp.ws.onmessage = ({ data }) => {
      const message = JSON.parse(data);
      if (message.method) { cdp.events.push(message); return; }
      const request = cdp.pending.get(message.id); if (!request) return;
      cdp.pending.delete(message.id); clearTimeout(request.timer);
      if (message.error) request.reject(new Error(JSON.stringify(message.error))); else request.resolve(message.result);
    };
    await new Promise((resolve, reject) => { cdp.ws.onopen = resolve; cdp.ws.onerror = reject; });
    return cdp;
  }
  send(method, params = {}) {
    const id = ++this.next;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => { this.pending.delete(id); reject(new Error(`CDP timeout: ${method}`)); }, 20_000);
      this.pending.set(id, { resolve, reject, timer }); this.ws.send(JSON.stringify({ id, method, params }));
    });
  }
  async evaluate(expression) {
    const answer = await this.send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true, userGesture: true });
    if (answer.exceptionDetails) throw new Error(JSON.stringify(answer.exceptionDetails));
    return answer.result.value;
  }
}

let chrome;
const sockets = [];
try {
  // A disposable Secure Vault; never change installed Native Messaging registration or user data.
  const creation = await native({ type: "vault_create_encrypted", path: path.join(root, "synthetic.sqlite3") });
  assert.equal(creation.ok, true);
  const committed = await native({ type: "vault_create_encrypted_commit", recoverySpell: creation.recoverySpell });
  assert.equal(committed.ok, true);
  const preset = await native({ type: "policy_preset", presetId: "default" }); assert.equal(preset.ok, true);
  const seed = preset.policy;
  seed.layers.neverSave.rules = [{ kind: "pattern", pattern: "bank.synthetic.test" }];
  seed.layers.saveButNoMcp.rules = [{ kind: "pattern", pattern: "private-old.synthetic.test" }];
  assert.equal((await native({ type: "policy_start", policy: seed })).ok, true);
  assert.equal((await native({ type: "vault_status" })).status.publication, "pending");
  const policyPath = path.join(app, "policy.json");
  const storedBefore = readFileSync(policyPath, "utf8");
  chrome = spawn(browserBin, ["--headless=new", "--window-size=1100,900", "--no-first-run", "--no-default-browser-check", "--disable-sync", "--disable-background-networking",
    `--user-data-dir=${profile}`, "--remote-debugging-port=0", `--disable-extensions-except=${extension}`, `--load-extension=${extension}`, "about:blank"], { env, windowsHide: true, stdio: "ignore" });
  let launchError; chrome.on("error", (error) => { launchError = error; });
  const portFile = path.join(profile, "DevToolsActivePort");
  const port = await until(() => { if (launchError) throw launchError; return existsSync(portFile) && readFileSync(portFile, "utf8").split("\n")[0]; }, "debug port");
  const endpoint = `http://127.0.0.1:${port}`;
  const version = await fetch(`${endpoint}/json/version`).then((r) => r.json());
  const worker = await until(async () => (await fetch(`${endpoint}/json/list`).then((r) => r.json())).find((t) => t.type === "service_worker" && t.url.endsWith("/service-worker.js")), "extension worker");
  const target = await fetch(`${endpoint}/json/new?${encodeURIComponent(`chrome-extension://${new URL(worker.url).host}/options.html`)}`, { method: "PUT" }).then((r) => r.json());
  const page = await CDP.connect(target.webSocketDebuggerUrl); sockets.push(page.ws);
  await page.send("Page.enable"); await page.send("Page.setInterceptFileChooserDialog", { enabled: true });
  await until(() => page.evaluate("Boolean(globalThis.__importPage?.draft) && !document.querySelector('#never-save-import').disabled"), "ready Options Page");
  const snapshot = () => page.evaluate("({draft:__importPage.draft.policy,version:__importPage.draft.version,dirty:__importPage.draft.dirty(),active:__importPage.view.active})");
  const select = async (zone, file) => {
    const seen = page.events.filter((e) => e.method === "Page.fileChooserOpened").length;
    await page.evaluate(`document.querySelector('#${zone}-import').click()`);
    await until(() => page.events.filter((e) => e.method === "Page.fileChooserOpened").length > seen, "real file chooser opened by Import button");
    const { root: document } = await page.send("DOM.getDocument");
    const { nodeId } = await page.send("DOM.querySelector", { nodeId: document.nodeId, selector: `#${zone}-import-file` });
    await page.send("DOM.setFileInputFiles", { nodeId, files: [file] });
    await until(() => page.evaluate("__importPage.busy === null"), "Import finished");
  };
  const before = await snapshot();
  const bad = path.join(root, "bad.csv");
  writeFileSync(bad, 'kind,pattern\r\npattern,valid.synthetic.test\r\nregex,"SECRET');
  let checked = calls.filter((c) => c.type === "policy_validate").length;
  await select("never-save", bad);
  assert.deepEqual(await snapshot(), before);
  assert.equal(calls.filter((c) => c.type === "policy_validate").length, checked);
  assert.match(await page.evaluate("document.querySelector('#never-save-import-status').textContent"), /CSV 3行目/);
  // CSV syntax succeeds but actual Rust validation refuses unsupported regex; no partial append.
  writeFileSync(bad, "kind,pattern\npattern,valid.synthetic.test\nregex,(?<=SECRET)");
  await select("never-save", bad);
  assert.deepEqual(await snapshot(), before);
  assert.match(await page.evaluate("document.querySelector('#never-save-import-status').textContent"), /CSV 3行目.*Policyルール検証/);
  assert.equal((await page.evaluate("document.querySelector('#never-save-import-status').textContent")).includes("SECRET"), false);
  // Restricted domains must not be auto-approved by CSV.
  writeFileSync(bad, "kind,pattern\npattern,valid.synthetic.test\ndomainWildcard,*.com");
  await select("save-but-no-mcp", bad); assert.deepEqual(await snapshot(), before);
  const blindRules = [
    { kind: "pattern", pattern: "new.synthetic.test/private/*" },
    { kind: "regex", pattern: " ^https://regex\\.synthetic\\.test/日本語🧠$ \r\n" },
    { kind: "domainWildcard", pattern: "*.example.com" },
    { kind: "exact", pattern: "https://exact.synthetic.test/é?q=a,b&emoji=🧠#frag" },
    { kind: "pattern", pattern: "bank.synthetic.test" },
  ];
  const blind = path.join(root, "blind.csv"); writeFileSync(blind, "\uFEFF" + rulesToCsv(blindRules));
  await select("never-save", blind);
  assert.equal(await page.evaluate("document.querySelector('#never-save-import-status').className"), "status",
    await page.evaluate("document.querySelector('#never-save-import-status').textContent"));
  const afterBlind = await snapshot();
  assert.deepEqual(afterBlind.draft.layers.neverSave.rules, [...before.draft.layers.neverSave.rules, ...blindRules]);
  assert.deepEqual(afterBlind.draft.layers.saveButNoMcp, before.draft.layers.saveButNoMcp);
  assert.deepEqual(afterBlind.active, before.active); assert.equal(afterBlind.version, 1); assert.equal(afterBlind.dirty, true);
  const privateRules = [{ kind: "pattern", pattern: "new-private.synthetic.test" }, { kind: "pattern", pattern: "new-private.synthetic.test" },
    { kind: "regex", pattern: "" }]; // Rust accepts empty Regex: the Import API must not re-judge it.
  const privateFile = path.join(root, "private.csv"); writeFileSync(privateFile, rulesToCsv(privateRules));
  await select("save-but-no-mcp", privateFile);
  const afterBoth = await snapshot();
  assert.equal(afterBoth.version, 2);
  assert.deepEqual(afterBoth.draft.layers.saveButNoMcp.rules, [...before.draft.layers.saveButNoMcp.rules, ...privateRules]);
  assert.deepEqual(afterBoth.draft.layers.neverSave, afterBlind.draft.layers.neverSave);
  assert.deepEqual(afterBoth.active, before.active);
  assert.equal(readFileSync(policyPath, "utf8"), storedBefore);
  assert.equal(calls.filter((c) => c.type === "policy_apply").length, 0);
  assert.equal((await native({ type: "vault_status" })).status.publication, "pending");
  const decide = async () => {
    const result = await native({ type: "policy_evaluate_batch", items: [
      { id: "old", url: "https://bank.synthetic.test/" }, { id: "blind", url: "https://new.synthetic.test/private/x" },
      { id: "private", url: "https://new-private.synthetic.test/" } ] });
    assert.equal(result.ok, true); return result.items.map((item) => item.decision);
  };
  assert.deepEqual(await decide(), ["never_save", "allow", "allow"]);
  const image = await page.send("Page.captureScreenshot", { format: "png", captureBeyondViewport: true });
  writeFileSync(path.join(root, "import-before-apply.png"), Buffer.from(image.data, "base64"));
  await page.evaluate("document.querySelector('#apply').click()");
  await until(() => page.evaluate("__importPage.busy === null && !__importPage.draft.dirty()"), "explicit Apply finished");
  assert.equal(calls.filter((c) => c.type === "policy_apply").length, 1);
  assert.notEqual(readFileSync(policyPath, "utf8"), storedBefore);
  assert.deepEqual(await decide(), ["never_save", "never_save", "save_but_no_mcp"]);
  const stored = (await native({ type: "policy_get" })).policy;
  assert.deepEqual(stored.layers.neverSave.rules, afterBoth.draft.layers.neverSave.rules);
  assert.deepEqual(stored.layers.saveButNoMcp.rules, afterBoth.draft.layers.saveButNoMcp.rules);
  assert.equal((await native({ type: "vault_status" })).status.publication, "pending");
  assert.equal(calls.some((c) => c.type === "vault_publication_confirm" || c.type === "policy_domain_approve"), false);
  const result = { root, browser: version.Browser, nativeTransport: "real Host via test-only loopback bridge", secureVault: "temporary only, publication remains pending", cases: ["actual file selection", "syntax refusal before Host call", "real Rust validation refusal", "restricted wildcard refusal without approval", "all four kinds lossless", "duplicates preserved", "one Draft version bump per batch", "Active file and ETag unchanged until explicit Apply", "existing Blind protection retained", "new Blind/Private decisions only after Apply", "MCP approval independent"] };
  writeFileSync(path.join(root, "result.json"), JSON.stringify(result, null, 2)); console.log(JSON.stringify(result, null, 2));
} finally {
  for (const ws of sockets) ws.close(); chrome?.kill(); bridge.closeAllConnections(); bridge.close();
  console.log(`isolated artifacts: ${root}`);
}
