import { test } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { createPageSaver, registerPageMenu, describePageSaveResult } from "../../extension/page-save.js";
import { captureAndUploadPage, PAGE_CHUNK_BYTES, MAX_PAGE_BYTES } from "../../extension/page-upload.js";
import { readFileSync } from "node:fs";

const token = "a".repeat(64), generation = "synthetic-generation";
const saveId = "00000000-0000-4000-8000-000000000001";
const meta = { url: "https://synthetic.test/private?access_token=DUMMY", title: "日本語ページ", capturedAt: 1, incognito: false };
const event = () => { const listeners = []; return { addListener: (fn) => listeners.push(fn), emit: (...args) => listeners.forEach((fn) => fn(...args)) }; };
function host({ lostCommit = false, missing = false, corruptAck = false, never = false, reject = null } = {}) {
  const messages = [], bytes = [], events = [];
  let committed = false;
  const hash = () => createHash("sha256").update(Buffer.concat(bytes)).digest("hex");
  const receipt = () => ({ sha256: hash(), byteLength: Buffer.concat(bytes).length, savedPageId: 1, blobId: 1, replayed: committed });
  return { messages, events, connect() {
    const port = { onMessage: event(), onDisconnect: event(), disconnect() { port.onDisconnect.emit(); }, postMessage(m) {
      messages.push(structuredClone(m));
      queueMicrotask(() => {
        if (never) return;
        const base = { ok: true, saveId, vaultToken: token, generation };
        if (reject === m.type) { port.onMessage.emit({ ...base, ok: false, error: "synthetic_failure" }); return; }
        let type;
        if (m.type === "page_save_open") { type = "page_save_opened"; Object.assign(base, { chunkBytes: PAGE_CHUNK_BYTES, maxBytes: MAX_PAGE_BYTES }); }
        if (m.type === "page_save_begin") type = "page_save_ready";
        if (m.type === "page_save_chunk") { bytes.push(Buffer.from(m.data, "base64")); type = "page_save_chunk_ack"; Object.assign(base, { index: corruptAck ? 99 : m.index, received: Buffer.concat(bytes).length }); }
        if (m.type === "page_save_prepare") { type = "page_save_prepared"; Object.assign(base, receipt()); }
        if (m.type === "page_save_commit") { committed = true; events.push("commit"); if (lostCommit) { port.disconnect(); return; } type = "page_saved"; Object.assign(base, receipt()); }
        if (m.type === "page_save_status") { type = missing ? "page_save_unknown" : "page_saved"; Object.assign(base, receipt()); }
        port.onMessage.emit({ ...base, type });
      });
    } }; return port;
  } };
}
function setup(options = {}) {
  const h = host(options);
  const tab = { id: 17, url: meta.url, title: meta.title, incognito: false, status: "complete" };
  const frame = { url: tab.url, documentId: "document-A", documentLifecycle: "active" };
  const navigation = { getFrame: async ({ tabId, frameId }) => { assert.equal(tabId,17); assert.equal(frameId,0); return { ...frame }; }, onBeforeNavigate: event(), onCommitted: event(), onHistoryStateUpdated: event(), onReferenceFragmentUpdated: event(), onErrorOccurred: event() };
  let captures = 0;
  const deps = { webNavigation: navigation, tabs: { get: async (id) => { assert.equal(id,17); return { ...tab }; }, onRemoved: event(), onReplaced: event(), onUpdated: event() }, randomUUID: () => saveId, now: () => 1, capture: async ({ tabId }) => { assert.equal(tabId,17); captures++; return new Blob(["MIME-Version: 1.0\r\n合成\0原本"]); }, connect: h.connect, timeoutMs: 100 };
  return { h, tab, frame, navigation, deps, captures: () => captures };
}

test("Page menu and manifest add only pageCapture, leave Image intact", () => {
  const menus = []; registerPageMenu({ create: (item, cb) => { menus.push(item); cb(); } }, {});
  assert.equal(menus[0].title, "Memocampusにページを保存"); assert.ok(menus[0].contexts.includes("frame")); assert.ok(menus[0].contexts.includes("link"));
  const manifest = JSON.parse(readFileSync(new URL("../../extension/manifest.json", import.meta.url)));
  assert.ok(manifest.permissions.includes("pageCapture")); assert.ok(!manifest.permissions.includes("downloads")); assert.ok(!manifest.permissions.includes("offscreen"));
});
test("iframe/link click saves only click-origin whole tab and fixed metadata, after COMMIT", async () => {
  const s = setup(); const saver = createPageSaver(s.deps);
  const result = await saver.save({ frameId: 9, frameUrl: "https://iframe.test/", linkUrl: "https://link.test/" }, s.tab);
  assert.equal(result.ok, true); assert.equal(s.captures(), 1); assert.deepEqual(s.h.messages[0].metadata, meta);
  assert.deepEqual(s.h.events, ["commit"]); assert.equal(s.h.messages.at(-1).type, "page_save_commit");
});
for (const url of ["chrome://settings", "file:///C:/synthetic", "data:text/html,synthetic", "invalid"]) test(`reject unsupported scheme ${url.split(":")[0]}`, async () => {
  const s = setup(); const result = await createPageSaver(s.deps).save({}, { ...s.tab, url }); assert.equal(result.code,"unsupported_url"); assert.equal(s.h.messages.length,0);
});
test("reject incognito and loading tab before opening Host", async () => {
  const s = setup(); const saver = createPageSaver(s.deps);
  assert.equal((await saver.save({}, { ...s.tab, incognito: true })).code,"incognito_not_allowed");
  assert.equal((await saver.save({}, { ...s.tab, status: "loading" })).code,"navigation_changed"); assert.equal(s.h.messages.length,0);
});
for (const trigger of ["reload", "history-A-B-A", "fragment", "removed", "replaced", "document", "vault"]) test(`capture race rejects ${trigger}`, async () => {
  const s = setup(); let saver;
  s.deps.capture = async () => {
    if (trigger === "reload") s.navigation.onBeforeNavigate.emit({ tabId:17, frameId:0 });
    if (trigger === "history-A-B-A") { s.navigation.onHistoryStateUpdated.emit({ tabId:17, frameId:0, url:"https://other.test" }); s.navigation.onHistoryStateUpdated.emit({ tabId:17, frameId:0, url:s.tab.url }); }
    if (trigger === "fragment") s.navigation.onReferenceFragmentUpdated.emit({ tabId:17, frameId:0 });
    if (trigger === "removed") s.deps.tabs.onRemoved.emit(17);
    if (trigger === "replaced") s.deps.tabs.onReplaced.emit(18,17);
    if (trigger === "document") s.frame.documentId = "document-B";
    if (trigger === "vault") saver.cancel();
    return new Blob(["synthetic"]);
  };
  saver = createPageSaver(s.deps); const result = await saver.save({},s.tab);
  assert.equal(result.ok,false); assert.ok(!s.h.messages.some((m) => m.type === "page_save_commit"));
});
test("Blob full arrayBuffer is never read; raw slices preserve all bytes", async () => {
  const h = host(); const original = Buffer.alloc(PAGE_CHUNK_BYTES * 2 + 19, 0xa3); const blob = new Blob([original]);
  const slices = []; const wrapper = { size: blob.size, arrayBuffer() { throw Error("forbidden full read"); }, slice(start,end) { slices.push([start,end]); return blob.slice(start,end); } };
  await captureAndUploadPage({ saveId, metadata:meta, capture:async()=>wrapper, verify:async()=>{}, connect:h.connect, timeoutMs:100 });
  assert.equal(slices.length,3); assert.ok(slices.every(([a,b])=> b-a <= PAGE_CHUNK_BYTES));
  const transferred = Buffer.concat(h.messages.filter((m)=>m.type==="page_save_chunk").map((m)=>Buffer.from(m.data,"base64")));
  assert.deepEqual(transferred,original);
});
for (const size of [0, MAX_PAGE_BYTES+1]) test(`reject size ${size} before chunks`, async () => {
  const h = host(); await assert.rejects(captureAndUploadPage({ saveId, metadata:meta, capture:async()=>({size}), verify:async()=>{}, connect:h.connect, timeoutMs:100 }), { code:"invalid_page_size" });
  assert.ok(!h.messages.some((m)=>m.type==="page_save_chunk"));
});
test("lost COMMIT ACK uses status only; never re-COMMIT or recapture", async () => {
  const s = setup({lostCommit:true}); const result = await createPageSaver(s.deps).save({},s.tab);
  assert.equal(result.ok,true); assert.equal(s.captures(),1); assert.equal(s.h.messages.filter((m)=>m.type==="page_save_commit").length,1); assert.equal(s.h.messages.at(-1).type,"page_save_status");
});
test("unknown after lost COMMIT ACK reports uncertainty without retry", async () => {
  const s = setup({lostCommit:true,missing:true}); const result = await createPageSaver(s.deps).save({},s.tab);
  assert.equal(result.code,"save_state_unknown"); assert.match(describePageSaveResult(result).message,/保存状態未確定/); assert.equal(s.captures(),1);
});
test("Host rejection, ACK mismatch, timeout and capture exception cannot report success", async () => {
  for (const options of [{reject:"page_save_chunk"},{corruptAck:true},{never:true}]) {
    const s = setup(options); s.deps.timeoutMs = 10;
    const result = await createPageSaver(s.deps).save({},s.tab); assert.equal(result.ok,false); assert.equal(s.h.events.length,0);
  }
  const s = setup(); s.deps.capture=async()=>{throw new Error("SECRET synthetic path");};
  const result = await createPageSaver(s.deps).save({},s.tab); assert.equal(result.ok,false); assert.ok(!JSON.stringify(result).includes("SECRET"));
});
test("pending capture timeout and cancellation release worker slot; duplicate click rejected", async () => {
  const s = setup(); s.deps.timeoutMs=10; s.deps.capture=()=>new Promise(()=>{}); const saver=createPageSaver(s.deps);
  const first=saver.save({},s.tab); assert.equal((await saver.save({},s.tab)).code,"page_upload_busy"); assert.equal((await first).code,"timeout");
  const second=saver.save({},s.tab); saver.cancel(); assert.equal((await second).ok,false);
});
test("Page path contains no automatic queue, policy mutation, persistent MHTML or external fetch", () => {
  for (const code of ["__proto__", "constructor", "https://synthetic.test/SECRET"]) {
    const text = describePageSaveResult({ ok: false, code });
    assert.equal(typeof text.message, "string"); assert.ok(!text.message.includes("SECRET"));
  }
  for (const file of ["page-save.js","page-upload.js"]) {
    const source=readFileSync(new URL(`../../extension/${file}`,import.meta.url),"utf8");
    assert.doesNotMatch(source,/storage\.(local|sync)|indexedDB|fetch\(|policy_apply|policy_start|createObjectURL|innerHTML/);
  }
});
