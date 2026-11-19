// #61 Slice 3. Product code is read/imported, never changed. Artifacts and all Vaults are under os.tmpdir().
// node scripts/verify-image-thumbnail-e2e.mjs [--clients] [--model claude-haiku-5-5]
import assert from "node:assert/strict";
import { spawn, execFileSync } from "node:child_process";
import { createHash, randomUUID, randomInt } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync, statSync } from "node:fs";
import { DatabaseSync } from "node:sqlite";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { startMcp, confirmPublication } from "../tests/extension/real-processes.js";

const repo = fileURLToPath(new URL("../", import.meta.url));
const root = mkdtempSync(path.join(os.tmpdir(), "memocampus-61-e2e-"));
const app = path.join(root, "app"); mkdirSync(app);
const env = { ...process.env, MEMOCAMPUS_APP_DIR: app, LOCALAPPDATA: path.join(root, "localappdata") }; delete env.MEMOCAMPUS_DB;
const hostBin = process.env.MEMOCAMPUS_TEST_HOST ?? path.join(repo, "native-host/target/debug/memocampus-native-host.exe");
const mcpBin = process.env.MEMOCAMPUS_TEST_MCP ?? path.join(repo, "mcp-server/target/debug/memocampus-mcp.exe");
assert.ok(existsSync(hostBin) && existsSync(mcpBin), "Specify built Host and current #61 MCP binaries; no build overwrites are performed.");
const sha = (bytes) => createHash("sha256").update(bytes).digest("hex");
const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const write = (name, value) => writeFileSync(path.join(root, name), JSON.stringify(value, null, 2));
const ok = (reply, step) => { assert.equal(reply.ok, true, `${step}: ${reply.error ?? "invalid_response"}`); return reply; };
const denied = (reply, code) => {
  assert.match(JSON.stringify(reply), new RegExp(code));
  assert.ok(reply.isError === true || !!reply.error, "Denied call must carry an error");
  assert.ok(!reply.content?.some((block) => block.type === "image"), "Denied call returned image data");
};
const report = { commit: execFileSync("git", ["rev-parse", "HEAD"], { cwd: repo, encoding: "utf8" }).trim(), tested_at: new Date().toISOString(), node: process.version, platform: `${os.platform()} ${os.release()} ${os.arch()}`, binaries: Object.fromEntries([["host", hostBin], ["mcp", mcpBin]].map(([name, bin]) => [name, { sha256: sha(readFileSync(bin)), bytes: statSync(bin).size }])), protocol: [], privacy: [], clients: [], synthetic_only: true };
console.log(JSON.stringify({ phase: "start", artifacts: root, commit: report.commit }));

class Host {
  constructor() {
    this.child = spawn(hostBin, [], { cwd: root, env, windowsHide: true, stdio: ["pipe", "pipe", "pipe"] });
    this.child.stderr.resume(); this.buffer = Buffer.alloc(0); this.queue = []; this.answers = [];
    this.child.stdout.on("data", (part) => {
      this.buffer = Buffer.concat([this.buffer, part]);
      while (this.buffer.length >= 4 && this.buffer.length >= this.buffer.readUInt32LE(0) + 4) {
        const size = this.buffer.readUInt32LE(0), answer = JSON.parse(this.buffer.subarray(4, size + 4));
        this.buffer = this.buffer.subarray(size + 4);
        const next = this.queue.shift(); if (next) next(answer); else this.answers.push(answer);
      }
    });
  }
  async call(message) {
    const body = Buffer.from(JSON.stringify({ requestId: randomUUID(), ...message }));
    const size = Buffer.alloc(4); size.writeUInt32LE(body.length);
    const answer = new Promise((resolve) => this.answers.length ? resolve(this.answers.shift()) : this.queue.push(resolve));
    this.child.stdin.write(Buffer.concat([size, body]));
    const timer = AbortSignal.timeout(15000);
    return Promise.race([answer, new Promise((_, reject) => timer.addEventListener("abort", () => reject(Error("Synthetic host timeout")), { once: true }))]);
  }
  close() { this.child.stdin.end(); }
}

async function generate() {
  const alphabet = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
  const answer = Array.from({ length: 8 }, () => alphabet[randomInt(alphabet.length)]).join("");
  const specs = ["A", "B", "C", "D", "M"].map((kind) => ({ kind, key: randomUUID(), ...(kind === "B" ? { text: answer } : {}) }));
  write("answers.json", { ocr: answer, descriptions: { A: "blue background; yellow circle; red triangle; green square", C: "purple background; white ellipse; orange star; cyan rectangle" } });
  const profile = path.join(root, "chrome-profile"); mkdirSync(profile);
  let received;
  const done = new Promise((resolve) => { received = resolve; });
  const driver = `
import {inspectImage,makeThumbnail} from '/image-capture.js';
const specs=${JSON.stringify(specs)};
const to64=b=>{let s='';for(const n of b)s+=String.fromCharCode(n);return btoa(s)};
const results=[];
for(const s of specs){
 const c=new OffscreenCanvas(1024,640),x=c.getContext('2d');
 x.fillStyle=s.kind==='A'?'#1565C0':s.kind==='C'?'#6A1B9A':'white';x.fillRect(0,0,1024,640);
 if(s.kind==='A'){
 x.fillStyle='#FFEB3B';x.beginPath();x.arc(190,320,105,0,Math.PI*2);x.fill();
 x.fillStyle='#E53935';x.beginPath();x.moveTo(510,180);x.lineTo(390,445);x.lineTo(635,445);x.closePath();x.fill();
 x.fillStyle='#43A047';x.fillRect(760,230,190,190);
 }else if(s.kind==='B'){
 x.fillStyle='black';x.font='bold 155px monospace';x.textAlign='center';x.textBaseline='middle';x.fillText(s.text,512,320);
 }else if(s.kind==='C'){
 x.fillStyle='white';x.beginPath();x.ellipse(185,320,125,70,0,0,Math.PI*2);x.fill();
 x.fillStyle='#FF9800';x.beginPath();for(let i=0;i<10;i++){const a=-Math.PI/2+i*Math.PI/5,r=i%2?55:130;x.lineTo(510+Math.cos(a)*r,320+Math.sin(a)*r)}x.closePath();x.fill();
 x.fillStyle='#00BCD4';x.fillRect(735,255,230,125);
 }else{ x.fillStyle=s.kind==='D'?'#333333':'#666666';x.fillRect(s.kind==='D'?120:220,200,240,200); }
 const b=new Uint8Array(await(await c.convertToBlob({type:'image/png'})).arrayBuffer());
 const info=await inspectImage(b,{createImageBitmap});const t=await makeThumbnail(b,info.mimeType,{createImageBitmap,OffscreenCanvas});
 results.push({kind:s.kind,key:s.key,original:to64(b),info,thumbnail:{...t,bytes:undefined,data:to64(t.bytes)}});
}
await fetch('/generated',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(results)});
`;
  const server = http.createServer(async (request, response) => {
    if (request.url === "/generated") { const parts = []; for await (const part of request) parts.push(part); received(JSON.parse(Buffer.concat(parts))); response.end("ok"); return; }
    if (request.url === "/") { response.setHeader("Content-Type", "text/html"); response.end('<script type="module" src="/fixture.js"></script>'); return; }
    if (request.url === "/fixture.js") { response.setHeader("Content-Type", "application/javascript"); response.end(driver); return; }
    if (["/image-capture.js", "/image-format.js"].includes(request.url)) { response.setHeader("Content-Type", "application/javascript"); response.end(readFileSync(path.join(repo, "extension", request.url.slice(1)))); return; }
    response.writeHead(404).end();
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const chrome = spawn(process.env.MEMOCAMPUS_TEST_CHROME ?? "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe", ["--headless=new", "--disable-gpu", "--disable-extensions", "--disable-background-networking", "--disable-component-update", "--no-first-run", "--no-default-browser-check", "--no-proxy-server", "--host-resolver-rules=MAP * ~NOTFOUND, EXCLUDE 127.0.0.1", `--user-data-dir=${profile}`, `http://127.0.0.1:${server.address().port}/`], { windowsHide: true, stdio: "ignore" });
  try {
    const results = await Promise.race([done, delay(20000).then(() => { throw Error("Synthetic image generation timed out"); })]);
    write("fixtures.json", results);
    return { results, answer };
  } finally { chrome.kill(); await new Promise((resolve) => server.close(resolve)); }
}

async function startHttp(vault) {
  const child = spawn(mcpBin, ["--vault", vault, "--transport", "streamable-http", "--listen", "127.0.0.1:0"], { cwd: root, env, windowsHide: true, stdio: ["ignore", "ignore", "pipe"] });
  let stderr = ""; child.stderr.on("data", (part) => { stderr += part; });
  for (let i = 0; i < 200 && !/listening on http:\/\/127\.0\.0\.1:(\d+)\/mcp/.test(stderr); i++) { if (child.exitCode !== null) throw Error("Isolated HTTP server exited"); await delay(25); }
  const port = stderr.match(/listening on http:\/\/127\.0\.0\.1:(\d+)\/mcp/)?.[1];
  assert.ok(port, "HTTP test endpoint did not start");
  const url = `http://127.0.0.1:${port}/mcp`, tokenFile = path.join(app, "mcp-http-token");
  const token = readFileSync(tokenFile, "utf8").trim();
  let id = 0;
  const rpc = async (method, params) => {
    const started = performance.now();
    const response = await fetch(url, { method: "POST", headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json", Accept: "application/json, text/event-stream", "MCP-Protocol-Version": "2025-06-18" }, body: JSON.stringify({ jsonrpc: "2.0", id: ++id, method, params }), signal: AbortSignal.timeout(15000) });
    const body = await response.text(); assert.equal(response.status, 200, `HTTP status ${response.status}`);
    return { reply: JSON.parse(body), ms: performance.now() - started, wire_bytes: Buffer.byteLength(body) };
  };
  await rpc("initialize", { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "synthetic-image-e2e", version: "1" } });
  return { child, rpc, url, tokenFile };
}

const host = new Host();
let stdio, httpServer;
try {
  const { results, answer } = await generate();
  const created = ok(await host.call({ type: "vault_create", useDefault: true }), "create");
  const vault = created.status.path ?? path.join(app, "memocampus.sqlite3");
  assert.ok(path.resolve(vault).startsWith(path.resolve(root) + path.sep));
  const preset = ok(await host.call({ type: "policy_preset", presetId: "default" }), "preset");
  ok(await host.call({ type: "policy_start", policy: preset.policy }), "start");
  const saved = [];
  for (const image of [...results].sort((a, b) => a.key.localeCompare(b.key))) {
    const bytes = Buffer.from(image.original, "base64"), saveId = randomUUID();
    const imageUrl = `https://fixture.example/vision-fixture/${image.key}`;
    const ready = ok(await host.call({ type: "image_save_begin", protocol: 2, saveId, sha256: sha(bytes), byteLength: bytes.length, mimeType: image.info.mimeType, width: image.info.width, height: image.info.height, imageUrl, pageUrl: "https://fixture.example/vision-fixture", frameUrl: null, incognito: false }), "image begin");
    if (ready.upload) for (let index = 0, start = 0; start < bytes.length; index++, start += 1048576) ok(await host.call({ type: "image_save_chunk", saveId, index, data: bytes.subarray(start, start + 1048576).toString("base64") }), "image chunk");
    const thumbnail = image.kind === "M" ? null : { ...image.thumbnail }; if (thumbnail) delete thumbnail.bytes;
    const committed = ok(await host.call({ type: "image_save_commit", saveId, thumbnail }), "image commit");
    saved.push({ kind: image.kind, key: image.key, imageUrl, sha256: sha(bytes), savedImageId: committed.savedImageId, blobId: committed.blobId, thumbnail: image.thumbnail });
  }
  // A separate nonpublic save of the same A BLOB: its metadata must never be selected for publication.
  const a = saved.find((image) => image.kind === "A"), aFixture = results.find((image) => image.kind === "A"), duplicateId = randomUUID();
  const dup = ok(await host.call({ type: "image_save_begin", protocol: 2, saveId: duplicateId, sha256: a.sha256, byteLength: Buffer.from(aFixture.original, "base64").length, mimeType: "image/png", width: 1024, height: 640, imageUrl: "https://fixture.example/CANARY_NONPUBLIC/image", pageUrl: "https://fixture.example/CANARY_NONPUBLIC/page", frameUrl: null, incognito: false }), "dedup begin");
  assert.equal(dup.upload, false);
  ok(await host.call({ type: "image_save_commit", saveId: duplicateId, thumbnail: aFixture.thumbnail }), "dedup commit");
  const vaultToken = created.status.vaultToken;
  const setPublic = async (image, value) => ok(await host.call({ type: "image_mcp_public_set", savedImageId: image.savedImageId, mcpPublic: value, vaultToken }), "image permission");
  for (const image of saved) await setPublic(image, true);
  const pending = startMcp(mcpBin, ["--vault", vault], { env });
  await assert.rejects(pending.initialize("pending-synthetic"), (error) => error.kind === "exited" && error.stderr.includes("vault_publication_pending"));
  await pending.close(); report.privacy.push("unapproved Vault refuses MCP startup");
  await confirmPublication((message) => host.call(message));
  stdio = startMcp(mcpBin, ["--vault", vault], { env });
  await stdio.initialize("synthetic-image-e2e");
  httpServer = await startHttp(vault);
  const tools = await stdio.request("tools/list", {});
  assert.ok(["memocampus_search_images", "memocampus_get_image_thumbnail"].every((name) => tools.tools.some((tool) => tool.name === name)));
  const call = (name, args) => stdio.request("tools/call", { name, arguments: args });
  const listing = await call("memocampus_search_images", { query: "vision-fixture", limit: 20 });
  assert.equal(listing.structuredContent.images.length, 5);
  for (const image of saved) image.id = listing.structuredContent.images.find((entry) => entry.latest_image_url === image.imageUrl)?.id;
  assert.ok(saved.every((image) => image.id));
  const httpListing = (await httpServer.rpc("tools/call", { name: "memocampus_search_images", arguments: { query: "vision-fixture", limit: 20 } })).reply.result;
  for (const image of saved) image.httpId = httpListing.structuredContent.images.find((entry) => entry.latest_image_url === image.imageUrl)?.id;
  assert.ok(saved.every((image) => image.httpId));
  const httpThumbnail = async (image) => (await httpServer.rpc("tools/call", { name: "memocampus_get_image_thumbnail", arguments: { image_id: image.httpId } })).reply.result;
  const privateImage = saved.find((image) => image.kind === "D"), missing = saved.find((image) => image.kind === "M");
  await setPublic(privateImage, false);
  const notFound = await call("memocampus_get_image_thumbnail", { image_id: privateImage.id });
  denied(notFound, "not_found");
  denied(await httpThumbnail(privateImage), "not_found");
  report.privacy.push("nonpublic image ID returns not_found and no image");
  await setPublic(a, false);
  denied(await call("memocampus_get_image_thumbnail", { image_id: a.id }), "not_found");
  denied(await httpThumbnail(a), "not_found");
  await setPublic(a, true); report.privacy.push("revoked image is denied despite another nonpublic save sharing its BLOB");
  await setPublic(privateImage, true);
  const current = ok(await host.call({ type: "policy_get" }), "policy get");
  current.policy.layers.saveButNoMcp.rules.push({ kind: "exact", pattern: privateImage.imageUrl });
  ok(await host.call({ type: "policy_apply", expectedEtag: current.etag, policy: current.policy }), "private policy");
  denied(await call("memocampus_get_image_thumbnail", { image_id: privateImage.id }), "not_found");
  denied(await httpThumbnail(privateImage), "not_found");
  report.privacy.push("Private Zone image is denied even with per-save permission");
  const missingResult = await call("memocampus_get_image_thumbnail", { image_id: missing.id });
  assert.equal(missingResult.isError, true); assert.match(JSON.stringify(missingResult), /thumbnail_missing/);
  assert.ok(!missingResult.content.some((block) => block.type === "image"));
  const httpMissing = await httpThumbnail(missing);
  assert.match(JSON.stringify(httpMissing), /thumbnail_missing/); assert.ok(!httpMissing.content.some((block) => block.type === "image"));
  await setPublic(missing, false); report.privacy.push("missing thumbnail never falls back to original");
  const publicList = await call("memocampus_search_images", { query: "vision-fixture", limit: 20 });
  assert.equal(publicList.structuredContent.images.length, 3);
  assert.ok(!JSON.stringify(publicList).includes("CANARY_NONPUBLIC"));
  assert.equal(publicList.structuredContent.images.find((image) => image.id === a.id).public_save_count, 1);
  assert.equal((await call("memocampus_search_images", { query: "CANARY_NONPUBLIC", limit: 20 })).structuredContent.total, 0);
  const httpPublic = (await httpServer.rpc("tools/call", { name: "memocampus_search_images", arguments: { query: "vision-fixture", limit: 20 } })).reply.result;
  assert.equal(httpPublic.structuredContent.images.length, 3); assert.ok(!JSON.stringify(httpPublic).includes("CANARY_NONPUBLIC"));
  assert.equal(httpPublic.structuredContent.images.find((image) => image.id === a.httpId).public_save_count, 1);
  assert.equal((await httpServer.rpc("tools/call", { name: "memocampus_search_images", arguments: { query: "CANARY_NONPUBLIC", limit: 20 } })).reply.result.structuredContent.total, 0);
  report.privacy.push("only three public images; shared-BLOB private metadata and count excluded");
  ok(await host.call({ type: "vault_publication_revoke", expectedVaultToken: vaultToken }), "revoke publication");
  assert.match(JSON.stringify(await stdio.rpc("tools/call", { name: "memocampus_get_image_thumbnail", arguments: { image_id: a.id } })), /vault_publication_pending/);
  denied(await httpThumbnail(a), "vault_publication_pending");
  await confirmPublication((message) => host.call(message)); report.privacy.push("running MCP denies a revoked Vault");
  const httpTools = (await httpServer.rpc("tools/list", {})).reply.result;
  assert.ok(["memocampus_search_images", "memocampus_get_image_thumbnail"].every((name) => httpTools.tools.some((tool) => tool.name === name)));
  const db = new DatabaseSync(vault, { readOnly: true });
  for (const transport of ["stdio", "streamable-http"]) {
    for (const image of saved.filter((image) => "ABC".includes(image.kind))) {
      const arguments_ = { name: "memocampus_get_image_thumbnail", arguments: { image_id: transport === "stdio" ? image.id : image.httpId } };
      const started = performance.now();
      const measured = transport === "stdio" ? { reply: await stdio.rpc("tools/call", arguments_) } : await httpServer.rpc("tools/call", arguments_);
      const result = measured.reply.result;
      assert.notEqual(result.isError, true, `${transport} thumbnail: ${result.content?.find((block) => block.type === "text")?.text ?? "error"}`);
      assert.equal(result.structuredContent, undefined);
      const blocks = result.content.filter((block) => block.type === "image"); assert.equal(blocks.length, 1);
      const bytes = Buffer.from(blocks[0].data, "base64"), metadata = JSON.parse(result.content.find((block) => block.type === "text").text);
      const stored = db.prepare("SELECT t.data, t.mime_type, t.width, t.height FROM image_thumbnails t JOIN image_blobs b ON b.id=t.blob_id WHERE b.sha256=?").get(image.sha256);
      assert.ok(bytes.equals(Buffer.from(stored.data))); assert.equal(blocks[0].mimeType, stored.mime_type);
      assert.equal(metadata.width, stored.width); assert.equal(metadata.height, stored.height);
      assert.equal(blocks[0].data, bytes.toString("base64"));
      report.protocol.push({ transport, fixture: image.kind, mime: blocks[0].mimeType, width: metadata.width, height: metadata.height, bytes: bytes.length, base64_chars: blocks[0].data.length, json_rpc_bytes: measured.wire_bytes ?? Buffer.byteLength(JSON.stringify(measured.reply)) + 1, tool_ms: measured.ms ?? performance.now() - started, sha256: sha(bytes) });
    }
  }
  db.close();
  write("manifest.json", { vault, app, hostBin, mcpBin, saved, http: { url: httpServer.url, tokenFile: httpServer.tokenFile } });
  console.log(JSON.stringify({ phase: "protocol_and_privacy", image_checks: report.protocol.length, privacy_checks: report.privacy.length }));
  if (process.argv.includes("--clients")) {
    const { runClients } = await import("./image-thumbnail-e2e-client.mjs");
    const modelIndex = process.argv.indexOf("--model");
    report.clients = await runClients({ root, app, vault, env, mcpBin, saved, answer, http: httpServer, model: modelIndex >= 0 ? process.argv[modelIndex + 1] : "claude-haiku-5-5" });
    if (report.clients.length !== 8 || report.clients.some((trial) => !trial.pass)) process.exitCode = 1;
  }
  write("results.json", report);
  console.log(JSON.stringify({ phase: "complete", protocol_checks: report.protocol.length, privacy_checks: report.privacy.length, client_trials: report.clients.length, visual_passes: report.clients.filter((trial) => trial.pass && trial.kind !== "unavailable").length, negative_controls: report.clients.filter((trial) => trial.pass && trial.kind === "unavailable").length, artifacts: root }));
} catch (error) {
  report.failure = error.message;
  write("results.json", report);
  console.error(JSON.stringify({ phase: "failed", reason: error.message, artifacts: root }));
  process.exitCode = 1;
} finally { host.close(); if (stdio) await stdio.close(); if (httpServer) httpServer.child.kill(); }
