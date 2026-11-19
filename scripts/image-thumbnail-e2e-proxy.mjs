// Test-only MCP relay: only synthetic isolated servers. No token or image Base64 is logged.
import { spawn } from "node:child_process";
import { appendFileSync, readFileSync } from "node:fs";
import { createHash, timingSafeEqual } from "node:crypto";
import http from "node:http";

const config = JSON.parse(readFileSync(process.argv[2], "utf8"));
const allowed = new Set(["memocampus_search_images", "memocampus_get_image_thumbnail"]);
const digest = (data) => createHash("sha256").update(data).digest("hex");
function images(reply) {
  return (reply?.result?.content ?? []).filter((block) => block.type === "image").map((block) => {
    const data = Buffer.from(block.data, "base64");
    return { mime: block.mimeType, bytes: data.length, base64_chars: block.data.length, sha256: digest(data) };
  });
}
function deliver(message, reply, started) {
  const before = images(reply);
  if (message.method === "tools/list" && reply.result?.tools) reply.result.tools = reply.result.tools.filter((tool) => allowed.has(tool.name));
  if (config.omitImages && before.length) reply = { ...reply, result: { isError: true, content: [{ type: "text", text: "Controlled test: image bytes are unavailable. No image was delivered." }] } };
  appendFileSync(config.log, JSON.stringify({ method: message.method, tool: message.params?.name, arguments: message.params?.arguments, ms: performance.now() - started, original_images: before, delivered_images: images(reply), json_rpc_bytes: Buffer.byteLength(JSON.stringify(reply)), ...(message.params?.name === "memocampus_search_images" ? { public_images: reply.result?.structuredContent?.images?.map(({ id, latest_image_url }) => ({ id, latest_image_url })) } : {}) }) + "\n");
  return reply;
}
function denied(message) {
  return message.method === "tools/call" && !allowed.has(message.params?.name);
}
const refused = (message) => ({ jsonrpc: "2.0", id: message.id, error: { code: -32601, message: "Not available in this isolated image test" } });

if (config.transport === "stdio") {
  const env = { ...process.env, ...config.env };
  delete env.MEMOCAMPUS_DB;
  const child = spawn(config.bin, ["--vault", config.vault], { env, windowsHide: true, stdio: ["pipe", "pipe", "pipe"] });
  child.stderr.resume();
  const pending = new Map();
  let input = "", output = "";
  process.stdin.setEncoding("utf8");
  process.stdin.on("data", (part) => {
    input += part;
    let end;
    while ((end = input.indexOf("\n")) >= 0) {
      const line = input.slice(0, end); input = input.slice(end + 1);
      if (!line.trim()) continue;
      const message = JSON.parse(line);
      if (denied(message)) { process.stdout.write(JSON.stringify(refused(message)) + "\n"); continue; }
      if (message.id !== undefined) pending.set(message.id, { message, started: performance.now() });
      child.stdin.write(line + "\n");
    }
  });
  child.stdout.setEncoding("utf8");
  child.stdout.on("data", (part) => {
    output += part;
    let end;
    while ((end = output.indexOf("\n")) >= 0) {
      const line = output.slice(0, end); output = output.slice(end + 1);
      if (!line.trim()) continue;
      let reply = JSON.parse(line);
      const request = pending.get(reply.id);
      if (request) { pending.delete(reply.id); reply = deliver(request.message, reply, request.started); }
      process.stdout.write(JSON.stringify(reply) + "\n");
    }
  });
  process.stdin.on("end", () => child.stdin.end());
  for (const signal of ["SIGINT", "SIGTERM"]) process.on(signal, () => child.kill());
  child.on("exit", (code) => process.exit(code ?? 1));
} else {
  const token = readFileSync(config.tokenFile, "utf8").trim();
  const expected = Buffer.from(`Bearer ${token}`);
  const server = http.createServer(async (request, response) => {
    const actual = Buffer.from(request.headers.authorization ?? "");
    if (actual.length !== expected.length || !timingSafeEqual(actual, expected)) { response.writeHead(401).end(); return; }
    if (request.url !== "/mcp" || request.method !== "POST") { response.writeHead(405).end(); return; }
    try {
      const parts = []; let bytes = 0;
      for await (const part of request) { bytes += part.length; if (bytes > 65536) throw Error("oversize"); parts.push(part); }
      const body = Buffer.concat(parts).toString("utf8");
      const message = JSON.parse(body);
      const started = performance.now();
      if (denied(message)) { response.writeHead(200, { "Content-Type": "application/json" }).end(JSON.stringify(refused(message))); return; }
      const headers = { Authorization: `Bearer ${token}`, "Content-Type": "application/json", Accept: "application/json, text/event-stream" };
      for (const key of ["mcp-protocol-version", "origin"]) if (request.headers[key]) headers[key] = request.headers[key];
      const upstream = await fetch(config.upstream, { method: "POST", headers, body, signal: AbortSignal.timeout(30000) });
      const data = await upstream.text();
      const result = data.trim() ? JSON.stringify(deliver(message, JSON.parse(data), started)) : "";
      response.writeHead(upstream.status, { "Content-Type": "application/json" }).end(result);
    } catch { response.writeHead(502).end(); }
  });
  server.listen(0, "127.0.0.1", () => process.stdout.write(JSON.stringify({ port: server.address().port }) + "\n"));
  for (const signal of ["SIGINT", "SIGTERM"]) process.on(signal, () => server.close(() => process.exit(0)));
}
