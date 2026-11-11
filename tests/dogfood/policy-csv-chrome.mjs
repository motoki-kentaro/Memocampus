// Opt-in actual Chrome download check, not installed Native Messaging integration.
// Only a temporary extension copy/profile/APP_DIR/Vault directory and synthetic policy are used.
// The copy replaces native transport with a fixture that rejects all writes. No registry changes.
// node tests/dogfood/policy-csv-chrome.mjs <chrome-for-testing.exe>
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtempSync, mkdirSync, cpSync, readFileSync, writeFileSync, existsSync, readdirSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const [browserBin] = process.argv.slice(2);
assert.ok(browserBin, "pass a Chrome for Testing executable");
const root = mkdtempSync(path.join(os.tmpdir(), "memocampus-23-csv-chrome-"));
const extension = path.join(root, "extension");
const profile = path.join(root, "profile");
const app = path.join(root, "app");
const vault = path.join(root, "vault");
mkdirSync(app); mkdirSync(vault);
cpSync(fileURLToPath(new URL("../../extension", import.meta.url)), extension, { recursive: true });
const pattern = (pattern) => ({ kind: "pattern", pattern });
const regex = (pattern) => ({ kind: "regex", pattern });
const special = ['a,b"c\r\nd\ne\rf', "日本語/🧠/e\u0301", " spaced ", "", "=1+1", "+1", "-1", "@SUM(A1)"];
const policy = {
  schemaVersion: 2, engineVersion: 2, revision: 7,
  categories: { authentication: true, credentials: true, government: true, webmail: true, aiChats: true },
  layers: { neverSave: { enabled: true, rules: special.map(pattern).concat([regex("[invalid"),
    { kind: "domainWildcard", pattern: "*.example.test" }, { kind: "exact", pattern: "https://exact.test/a?q=1,2" },
    pattern("duplicate"), pattern("duplicate"), { kind: "exact", pattern: "duplicate" }]) },
    saveButNoMcp: { enabled: true, rules: [regex("private-only"), { kind: "domainWildcard", pattern: "*.private.test" },
      pattern("private-pattern"), { kind: "exact", pattern: "https://private.test/🧠" }] } },
};
writeFileSync(path.join(extension, "csv-test-transport.js"), `
globalThis.__csvCalls = [];
globalThis.__csvFixture = ${JSON.stringify(policy)};
chrome.runtime.sendNativeMessage = (_name, message, callback) => {
  globalThis.__csvCalls.push(structuredClone(message));
  const base = {ok:true,requestId:message.requestId,type:message.type+'_result'};
  const p = globalThis.__csvFixture;
  const version = {etag:'csv-test-etag',revision:p.revision,schemaVersion:2,engineVersion:2};
  let reply;
  switch(message.type) {
    case 'policy_status': reply = {...base,type:'policy_result',state:'ready',...version}; break;
    case 'policy_get': reply = {...base,state:'ready',...version,policy:structuredClone(p)}; break;
    case 'policy_domain_approvals': reply = {...base,approvals:[],psl:{source:'fixture',sourceCommit:'fixture',snapshotDate:'2026-11-11'}}; break;
    case 'policy_test_rule': reply = {...base,matched:true}; break; // Scripted, NOT a real validator.
    case 'policy_validate': reply = {...base,ok:false,error:'policy_invalid',errors:[{layer:'neverSave',ruleIndex:8,code:'invalid_regex'}]}; break;
    default: reply = {...base,ok:false,error:'unknown_type'}; // Never write/apply/start/publication.
  }
  queueMicrotask(()=>callback(reply));
};
`);
for (const entry of ["options.js", "sidepanel.js", "service-worker.js"]) {
  const target = path.join(extension, entry);
  let source = readFileSync(target, "utf8");
  if (entry === "options.js") source = source.replace("startOptionsPage({", "globalThis.__csvPage = startOptionsPage({");
  writeFileSync(target, 'import "./csv-test-transport.js";\n' + source);
}

const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
async function until(check, label) {
  const deadline = Date.now() + 15_000;
  while (Date.now() < deadline) {
    const result = await check();
    if (result) return result;
    await pause(100);
  }
  throw new Error(`timeout: ${label}`);
}
class CDP {
  static async connect(url) {
    const cdp = new CDP();
    cdp.ws = new WebSocket(url); cdp.next = 0; cdp.pending = new Map();
    cdp.ws.onmessage = ({ data }) => {
      const message = JSON.parse(data), request = cdp.pending.get(message.id);
      if (!request) return;
      cdp.pending.delete(message.id); clearTimeout(request.timer);
      if (message.error) request.reject(new Error(JSON.stringify(message.error)));
      else request.resolve(message.result);
    };
    await new Promise((resolve, reject) => { cdp.ws.onopen = resolve; cdp.ws.onerror = reject; });
    return cdp;
  }
  send(method, params = {}) {
    const id = ++this.next;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => { this.pending.delete(id); reject(new Error(`CDP timeout: ${method}`)); }, 15_000);
      this.pending.set(id, { resolve, reject, timer });
      this.ws.send(JSON.stringify({ id, method, params }));
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
  const env = { ...process.env, MEMOCAMPUS_APP_DIR: app };
  delete env.MEMOCAMPUS_DB;
  chrome = spawn(browserBin, ["--headless=new", "--window-size=1100,900", "--no-first-run", "--no-default-browser-check", "--disable-sync", "--disable-background-networking",
    `--user-data-dir=${profile}`, "--remote-debugging-port=0", `--disable-extensions-except=${extension}`, `--load-extension=${extension}`, "about:blank"], { env, windowsHide: true, stdio: "ignore" });
  let launchError;
  chrome.on("error", (error) => { launchError = error; });
  const portFile = path.join(profile, "DevToolsActivePort");
  const port = await until(() => { if (launchError) throw launchError; return existsSync(portFile) && readFileSync(portFile, "utf8").split("\n")[0]; }, "debug port");
  const endpoint = `http://127.0.0.1:${port}`;
  const version = await fetch(`${endpoint}/json/version`).then((r) => r.json());
  const browser = await CDP.connect(version.webSocketDebuggerUrl); sockets.push(browser.ws);
  const worker = await until(async () => (await fetch(`${endpoint}/json/list`).then((r) => r.json())).find((t) => t.type === "service_worker" && t.url.endsWith("/service-worker.js")), "extension worker");
  const target = await fetch(`${endpoint}/json/new?${encodeURIComponent(`chrome-extension://${new URL(worker.url).host}/options.html`)}`, { method: "PUT" }).then((r) => r.json());
  const page = await CDP.connect(target.webSocketDebuggerUrl); sockets.push(page.ws);
  await until(() => page.evaluate("Boolean(globalThis.__csvPage?.draft) && !document.querySelector('#never-save-export').disabled"), "Draft loaded");
  const snapshot = () => page.evaluate("({draft:__csvPage.draft.policy,version:__csvPage.draft.version,dirty:__csvPage.draft.dirty(),active:__csvPage.view.active,calls:structuredClone(__csvCalls)})");
  const checkDownload = async (label, zone, expected) => {
    const directory = path.join(root, label); mkdirSync(directory);
    await browser.send("Browser.setDownloadBehavior", { behavior: "allow", downloadPath: directory });
    const before = await snapshot();
    await page.evaluate(`document.querySelector('#${zone}-export').click()`);
    const filename = zone === "never-save" ? "memocampus-blind-zone.csv" : "memocampus-private-zone.csv";
    await until(() => existsSync(path.join(directory, filename)) && readdirSync(directory).length === 1, `${label} download`);
    assert.deepEqual(readFileSync(path.join(directory, filename)), Buffer.from(expected, "utf8"));
    assert.deepEqual(await snapshot(), before, "Export changed Draft / Active / ETag or called Host");
    return filename;
  };
  // Browser fixture deliberately includes policy-invalid text; validation fails, Export still works.
  await page.evaluate("document.querySelector('#validate').click()");
  await until(() => page.evaluate("__csvPage.busy === null && document.querySelector('#validation-status').textContent.includes('問題')"), "invalid Draft validation displayed");
  // Add an unapplied rule through the actual checker UI, with its scripted match response.
  await page.evaluate(`document.querySelector('#checker-rule').value='unapplied.example'; document.querySelector('#checker-rule').dispatchEvent(new Event('input')); document.querySelector('#checker-url').value='https://unapplied.example/'; document.querySelector('#checker-url').dispatchEvent(new Event('input')); document.querySelector('#checker-run').click()`);
  await until(() => page.evaluate("!document.querySelector('#checker-add-never-save').disabled"), "checker match");
  await page.evaluate("document.querySelector('#checker-add-never-save').click()");
  // Unreflected checker input is NOT part of the Draft.
  await page.evaluate("document.querySelector('#checker-rule').value='UNREFLECTED'; document.querySelector('#checker-rule').dispatchEvent(new Event('input'))");
  const quote = (text) => /[",\r\n]/.test(text) ? '"' + text.replaceAll('"', '""') + '"' : text;
  const expectedBlind = "kind,pattern\r\n" + special.map((text) => `pattern,${quote(text)}\r\n`).join("") + 'regex,[invalid\r\ndomainWildcard,*.example.test\r\nexact,"https://exact.test/a?q=1,2"\r\npattern,duplicate\r\npattern,duplicate\r\nexact,duplicate\r\npattern,unapplied.example\r\n';
  await checkDownload("blind", "never-save", expectedBlind);
  await checkDownload("private", "save-but-no-mcp", "kind,pattern\r\nregex,private-only\r\ndomainWildcard,*.private.test\r\npattern,private-pattern\r\nexact,https://private.test/🧠\r\n");
  const shot = await page.send("Page.captureScreenshot", { format: "png", captureBeyondViewport: true });
  writeFileSync(path.join(root, "options-csv.png"), Buffer.from(shot.data, "base64"));
  // Browser API failure injection is confined to this isolated page, preserving the real downloader.
  const beforeFailure = await snapshot();
  await page.evaluate("globalThis.__createUrl = URL.createObjectURL; URL.createObjectURL = () => {throw new Error('SECRET_RULE_TEXT')}; document.querySelector('#never-save-export').click(); URL.createObjectURL = __createUrl");
  assert.equal(await page.evaluate("document.querySelector('#never-save-export-status').className"), "error-text");
  assert.equal(await page.evaluate("document.querySelector('#never-save-export-status').textContent.includes('SECRET_RULE_TEXT')"), false);
  assert.deepEqual(await snapshot(), beforeFailure);
  await checkDownload("retry-after-failure", "never-save", expectedBlind);
  // Unknown kinds and corrupt internal rule data must refuse the entire zone without a download.
  const refused = path.join(root, "refused"); mkdirSync(refused);
  await browser.send("Browser.setDownloadBehavior", { behavior: "allow", downloadPath: refused });
  for (const [rule, reason] of [[{ kind: "SECRET_UNKNOWN_KIND", pattern: "SECRET_PATTERN" }, "未知のルール型"],
    [{ kind: "pattern", pattern: null }, "内部データ形式が不正"]]) {
    const before = await snapshot();
    await page.evaluate(`globalThis.__originalLayer = __csvPage.draft.layer;
      __csvPage.draft.layer = (key) => key === 'neverSave' ? {...__originalLayer(key), rules:[${JSON.stringify(rule)}]} : __originalLayer(key);
      document.querySelector('#never-save-export').click(); __csvPage.draft.layer = __originalLayer;`);
    assert.equal(await page.evaluate(`document.querySelector('#never-save-export-status').textContent.includes(${JSON.stringify(reason)})`), true);
    assert.equal(await page.evaluate("document.querySelector('#never-save-export-status').textContent.includes('SECRET')"), false);
    assert.deepEqual(await snapshot(), before);
  }
  // Drain pending browser download tasks before checking absence.
  await page.evaluate("new Promise(resolve => setTimeout(resolve, 100))");
  assert.deepEqual(readdirSync(refused), []);
  // Empty zone through real delete controls.
  await page.evaluate("while(document.querySelector('#save-but-no-mcp-rules .rule-delete')) document.querySelector('#save-but-no-mcp-rules .rule-delete').click()");
  await checkDownload("empty", "save-but-no-mcp", "kind,pattern\r\n");
  assert.deepEqual(readdirSync(app), []);
  assert.deepEqual(readdirSync(vault), []);
  const result = { browser: version.Browser, root, header: "kind,pattern", kinds: ["pattern", "regex", "domainWildcard", "exact"], cases: ["all four kinds in each zone", "invalid and unapplied Draft", "Blind only", "Private only", "no zone contamination", "CR LF CRLF quotes Unicode spaces empty formulas duplicates", "Draft Active ETag unchanged and no export Host calls", "unknown kind and malformed data refusal", "download failure and retry", "empty zone"], nativeTransport: "scripted read-only fixture; installed host not tested", actualBrowserDownload: true };
  writeFileSync(path.join(root, "result.json"), JSON.stringify(result, null, 2));
  console.log(JSON.stringify(result, null, 2));
} finally {
  for (const ws of sockets) ws.close();
  chrome?.kill();
  console.log(`isolated artifacts: ${root}`);
}
