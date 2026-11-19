// Regression tests for scripts/audit-image-thumbnail-e2e.mjs (FINDING-E2E-01).
//
//   node --test scripts/audit-image-thumbnail-e2e.test.mjs
//
// Every test builds a SYNTHETIC, minimal run directory in the OS temp directory (no real Vault, no real
// model output, no token, no image bytes — thumbnails are fake ASCII and transcripts carry only hashes),
// corrupts one thing, and requires the audit to fail on exactly the check that should catch it. The
// original primary evidence is never read or modified here.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { EXPECTED_CONDITIONS, audit, extractAnswer, parseAnswerText, scoreColor, scoreOcr } from "./audit-image-thumbnail-e2e.mjs";

const sha = (text) => createHash("sha256").update(text).digest("hex");
const TOKEN = "synthetic-test-token-not-a-secret";
const OCR = "ZZ9QQ8WW";
const THUMB = { A: "thumb-A-synthetic", B: "thumb-B-synthetic", C: "thumb-C-synthetic", D: "thumb-D-synthetic" };
const ID = { stdio: { A: "image_a1", B: "image_b1", C: "image_c1" }, "streamable-http": { A: "image_a2", B: "image_b2", C: "image_c2" } };
const URL = (kind) => `https://fixture.example/vision-fixture/${kind.toLowerCase()}-key`;
const hashOf = (kind) => sha(Buffer.from(Buffer.from(THUMB[kind]).toString("base64"), "base64"));
const fenced = (value) => "```json\n" + JSON.stringify(value) + "\n```";

const PLAN = { color: ["A"], ocr: ["B"], distinguish: ["B", "C", "A"], unavailable: ["B"] };
function answerFor(transport, kind) {
  const ids = ID[transport];
  if (kind === "color") return { visible: true, background: "blue", objects: [{ shape: "circle", color: "yellow" }, { shape: "triangle", color: "red" }, { shape: "square", color: "green" }] };
  if (kind === "ocr") return { visible: true, text: OCR };
  if (kind === "distinguish") return { image_id: ids.A, observations: [{ image_id: ids.B, description: "text on white" }, { image_id: ids.C, description: "shapes on a darker background" }, { image_id: ids.A, description: "shapes on a blue background" }] };
  return { visible: false, text: null };
}

function buildTrial(transport, kind) {
  const ids = ID[transport];
  const fetched = PLAN[kind];
  const catalog = ["A", "B", "C"].map((name) => ({ id: ids[name], latest_image_url: URL(name) }));
  const events = [
    { type: "system", subtype: "init", model: "model-x", claude_code_version: "9.9.9", tools: ["mcp__memocampus_e2e__memocampus_search_images", "mcp__memocampus_e2e__memocampus_get_image_thumbnail"], mcp_servers: [{ name: "memocampus_e2e", status: "connected" }] },
    { type: "assistant", message: { model: "model-x", content: [{ type: "tool_use", id: "t0", name: "search", input: {} }] } },
    { type: "user", message: { content: [{ type: "tool_result", tool_use_id: "t0", content: JSON.stringify({ images: catalog.map((entry) => ({ id: entry.id, latest_image_url: entry.latest_image_url })) }) }] } },
  ];
  const relay = [{ method: "tools/list", original_images: [], delivered_images: [] },
    { method: "tools/call", tool: "memocampus_search_images", arguments: { query: "vision-fixture" }, original_images: [], delivered_images: [], public_images: catalog }];
  let images = 0;
  fetched.forEach((name, index) => {
    const image = { mime: "image/webp", bytes: 10, base64_chars: 14, sha256: hashOf(name) };
    relay.push({ method: "tools/call", tool: "memocampus_get_image_thumbnail", arguments: { image_id: ids[name] }, original_images: [image], delivered_images: kind === "unavailable" ? [] : [image] });
    events.push({ type: "assistant", message: { model: "model-x", content: [{ type: "tool_use", id: `t${index + 1}`, name: "get", input: { image_id: ids[name] } }] } });
    const content = [{ type: "text", text: JSON.stringify({ image_id: ids[name] }) }];
    if (kind !== "unavailable") { content.push({ type: "image", source: { type: "base64", media_type: "image/webp", data_summary: { sha256: hashOf(name) } } }); images++; }
    events.push({ type: "user", message: { content: [{ type: "tool_result", tool_use_id: `t${index + 1}`, content }] } });
  });
  const text = transport === "stdio" ? JSON.stringify(answerFor(transport, kind)) : fenced(answerFor(transport, kind));
  events.push({ type: "assistant", message: { model: "model-x", content: [{ type: "text", text }] } });
  events.push({ type: "result", subtype: "success", is_error: false, result: text });
  const record = { client: "Claude Code", version: "9.9.9 (Claude Code)", model: "model-x", transport, kind, pass: true, client_image_blocks: images, relay_image_blocks: images,
    tool_calls: relay.filter((entry) => entry.method === "tools/call").length, get_thumbnail_calls: fetched.length, result_subtype: "success", answer: answerFor(transport, kind) };
  if (kind === "ocr") record.exact_ocr_match = true;
  return { events, relay, record };
}

function buildFixture() {
  const trials = {};
  for (const label of EXPECTED_CONDITIONS) {
    const transport = label.startsWith("stdio") ? "stdio" : "streamable-http";
    trials[label] = buildTrial(transport, label.slice(transport.length + 1));
  }
  const clients = EXPECTED_CONDITIONS.map((label) => structuredClone(trials[label].record));
  return {
    answers: { ocr: OCR, descriptions: { A: "blue background; yellow circle; red triangle; green square", C: "purple background; white ellipse; orange star; cyan rectangle" } },
    manifest: { saved: ["A", "B", "C", "D", "M"].map((kind) => ({ kind, imageUrl: URL(kind), thumbnail: THUMB[kind] ? { mimeType: "image/webp", data: Buffer.from(THUMB[kind]).toString("base64") } : null })) },
    trials, clients, committedClients: structuredClone(clients), extraDirs: [], token: TOKEN,
    committedCommit: "0123456789abcdef0123456789abcdef01234567",
  };
}

// Writes the fixture and returns [runDir, committedPath]. The committed evidence hashes are computed from
// the transcript text written, so a corrupted transcript is not also a hash mismatch unless a test says so.
function write(fx, { skipTranscript = [], committedHashOverride = {}, runName = "SYNTH1" } = {}) {
  const root = mkdtempSync(path.join(tmpdir(), "audit-test-"));
  const run = path.join(root, `memocampus-61-e2e-${runName}`);
  mkdirSync(path.join(run, "app"), { recursive: true });
  writeFileSync(path.join(run, "app", "mcp-http-token"), fx.token);
  writeFileSync(path.join(run, "answers.json"), JSON.stringify(fx.answers));
  writeFileSync(path.join(run, "manifest.json"), JSON.stringify(fx.manifest));
  writeFileSync(path.join(run, "results.json"), JSON.stringify({ clients: fx.clients }));
  const evidence = [];
  for (const [label, trial] of Object.entries(fx.trials)) {
    const dir = path.join(run, "trials", label);
    mkdirSync(path.join(dir, "cwd"), { recursive: true });
    const transcript = JSON.stringify(trial.events);
    if (!skipTranscript.includes(label)) writeFileSync(path.join(dir, "transcript.sanitized.json"), transcript);
    writeFileSync(path.join(dir, "relay.jsonl"), trial.relay.map((entry) => JSON.stringify(entry)).join("\n") + "\n");
    writeFileSync(path.join(dir, "result.json"), JSON.stringify(fx.clients.find((client) => `${client.transport}-${client.kind}` === label) ?? trial.record));
    const [transport, kind] = [trial.record.transport, trial.record.kind];
    evidence.push({ transport, kind, transcript_sha256: committedHashOverride[label] ?? sha(transcript) });
  }
  for (const extra of fx.extraDirs) mkdirSync(path.join(run, "trials", extra, "cwd"), { recursive: true });
  const committedPath = path.join(root, "committed.json");
  writeFileSync(committedPath, JSON.stringify({ commit: fx.committedCommit, run_label: runName, clients: fx.committedClients, evidence }));
  return [run, committedPath, root];
}

// Runs the audit on a (possibly corrupted) fixture; returns the names of the failed checks.
function failuresOf(mutate, options) {
  const fx = buildFixture();
  mutate?.(fx);
  const [run, committed, root] = write(fx, options);
  try { return audit(run, committed); } finally { rmSync(root, { recursive: true, force: true }); }
}
const failedNames = (outcome) => outcome.failures;
const caught = (outcome, fragment) => failedNames(outcome).some((name) => name.includes(fragment));
const trialOf = (fx, label) => fx.clients.find((client) => `${client.transport}-${client.kind}` === label);
const committedOf = (fx, label) => fx.committedClients.find((client) => `${client.transport}-${client.kind}` === label);

test("baseline: the untouched synthetic run passes everything, 6 positives and 2 controls re-scored", () => {
  const outcome = failuresOf();
  assert.deepEqual(failedNames(outcome), []);
  assert.deepEqual(outcome.rescored, { positives: 6, controls: 2 });
});

// ---- The four anomalies Codex reproduced ----
test("Codex 1: a wrong colour in the aggregated trial answer is rejected", () => {
  const outcome = failuresOf((fx) => { trialOf(fx, "stdio-color").answer.background = "red"; });
  assert.ok(caught(outcome, "stdio-color: run trial.answer equals the transcript answer"), failedNames(outcome).join("\n"));
});
test("Codex 2: a wrong selected image id in the aggregated trial answer is rejected", () => {
  const outcome = failuresOf((fx) => { trialOf(fx, "streamable-http-distinguish").answer.image_id = "image_c2"; });
  assert.ok(caught(outcome, "streamable-http-distinguish: run trial.answer equals the transcript answer"), failedNames(outcome).join("\n"));
});
test("Codex 3: a wrong OCR answer in the committed (published) results is rejected", () => {
  const outcome = failuresOf((fx) => { committedOf(fx, "stdio-ocr").answer.text = "AAAAAAAA"; });
  assert.ok(caught(outcome, "stdio-ocr: committed answer equals the transcript answer"), failedNames(outcome).join("\n"));
});
test("Codex 4: duplicated trial keys that drop a condition are rejected (count alone is still 8)", () => {
  const outcome = failuresOf((fx) => { fx.clients[1].kind = "color"; fx.committedClients[1].kind = "color"; });
  assert.ok(caught(outcome, "results.json clients: exactly the 8 expected conditions, each once"), failedNames(outcome).join("\n"));
  assert.ok(caught(outcome, "committed results clients: exactly the 8 expected conditions, each once"));
  assert.ok(caught(outcome, "stdio-ocr: exactly one trial record to audit"));
});

// ---- Further anomalies ----
test("a wrong answer inside the transcript itself is rejected by the independent re-score", () => {
  const outcome = failuresOf((fx) => {
    const wrong = JSON.stringify({ visible: true, text: "3K8KLQDX" });
    const events = fx.trials["stdio-ocr"].events;
    events.at(-2).message.content[0].text = wrong; events.at(-1).result = wrong;
  });
  assert.ok(caught(outcome, "stdio-ocr: independent re-score of the transcript answer"), failedNames(outcome).join("\n"));
  assert.ok(caught(outcome, "stdio-ocr: run trial.answer equals the transcript answer"), "the aggregate no longer matches either");
});
test("a transcript answer and an aggregate that agree on a wrong answer are still rejected", () => {
  const outcome = failuresOf((fx) => {
    const wrong = { visible: true, text: OCR + "X" };   // extra character: not an exact match
    const text = JSON.stringify(wrong);
    const events = fx.trials["stdio-ocr"].events; events.at(-2).message.content[0].text = text; events.at(-1).result = text;
    trialOf(fx, "stdio-ocr").answer = wrong; committedOf(fx, "stdio-ocr").answer = wrong;
  });
  assert.ok(caught(outcome, "stdio-ocr: independent re-score of the transcript answer"), failedNames(outcome).join("\n"));
  assert.ok(caught(outcome, "stdio-ocr: run trial.pass equals the independent verdict"), "pass:true over a wrong answer is flagged");
});
test("a missing transcript is rejected", () => {
  const outcome = failuresOf(null, { skipTranscript: ["stdio-color"] });
  assert.ok(caught(outcome, "stdio-color: audit could run on the primary evidence"), failedNames(outcome).join("\n"));
});
test("an assistant answer that contradicts the final result event is rejected", () => {
  const outcome = failuresOf((fx) => { fx.trials["stdio-ocr"].events.at(-1).result = JSON.stringify({ visible: true, text: "AAAAAAAA" }); });
  assert.ok(caught(outcome, "stdio-ocr: a unique, well-formed final answer was extracted from the transcript"), failedNames(outcome).join("\n"));
});
test("two conflicting final answers are rejected", () => {
  const outcome = failuresOf((fx) => {
    const events = fx.trials["stdio-ocr"].events;
    events.splice(events.length - 1, 0, { type: "assistant", message: { model: "model-x", content: [{ type: "text", text: JSON.stringify({ visible: true, text: "AAAAAAAA" }) }] } });
  });
  assert.ok(caught(outcome, "stdio-ocr: a unique, well-formed final answer was extracted from the transcript"), failedNames(outcome).join("\n"));
});
test("a missing final answer is rejected", () => {
  const outcome = failuresOf((fx) => { const events = fx.trials["stdio-ocr"].events; events.splice(events.length - 2, 1); });
  assert.ok(caught(outcome, "stdio-ocr: a unique, well-formed final answer was extracted from the transcript"), failedNames(outcome).join("\n"));
});
test("a malformed (non-JSON) answer is rejected", () => {
  const outcome = failuresOf((fx) => { const events = fx.trials["stdio-ocr"].events; events.at(-2).message.content[0].text = "The text is ZZ9QQ8WW"; events.at(-1).result = "The text is ZZ9QQ8WW"; });
  assert.ok(caught(outcome, "stdio-ocr: a unique, well-formed final answer was extracted from the transcript"), failedNames(outcome).join("\n"));
});
test("a missing OCR answer key is rejected (fail-closed)", () => {
  const outcome = failuresOf((fx) => { delete fx.answers.ocr; });
  assert.ok(caught(outcome, "answer key: OCR key is the 8-character challenge"), failedNames(outcome).join("\n"));
  assert.ok(caught(outcome, "stdio-ocr: independent re-score of the transcript answer"), "and nothing is scored as a success");
});
test("an unknown trial directory is rejected", () => {
  const outcome = failuresOf((fx) => { fx.extraDirs.push("stdio-extra"); });
  assert.ok(caught(outcome, "trials/ holds exactly the 8 expected condition directories"), failedNames(outcome).join("\n"));
});
test("an unknown condition key among the trials is rejected", () => {
  const outcome = failuresOf((fx) => { fx.clients[0].transport = "websocket"; });
  assert.ok(caught(outcome, "results.json clients: exactly the 8 expected conditions, each once"), failedNames(outcome).join("\n"));
});
test("an inconsistent model-facing image count is rejected", () => {
  const outcome = failuresOf((fx) => { trialOf(fx, "stdio-color").client_image_blocks = 2; });
  assert.ok(caught(outcome, "stdio-color: counters match the evidence"), failedNames(outcome).join("\n"));
});
test("the wrong image delivered for the question (B instead of A) is rejected", () => {
  const outcome = failuresOf((fx) => {
    const trial = fx.trials["stdio-color"];
    for (const entry of trial.relay) for (const list of [entry.original_images, entry.delivered_images]) for (const image of list ?? []) image.sha256 = hashOf("B");
    trial.events.find((event) => event.type === "user" && JSON.stringify(event).includes("image/webp")).message.content[0].content[1].source.data_summary.sha256 = hashOf("B");
  });
  assert.ok(caught(outcome, "stdio-color: the image of fixture A (not another) reached the model"), failedNames(outcome).join("\n"));
});
test("a wrong id chosen consistently in transcript and aggregates is rejected", () => {
  const outcome = failuresOf((fx) => {
    const wrong = answerFor("stdio", "distinguish"); wrong.image_id = ID.stdio.C;
    const text = JSON.stringify(wrong);
    const events = fx.trials["stdio-distinguish"].events; events.at(-2).message.content[0].text = text; events.at(-1).result = text;
    trialOf(fx, "stdio-distinguish").answer = wrong; committedOf(fx, "stdio-distinguish").answer = wrong;
  });
  assert.ok(caught(outcome, "stdio-distinguish: independent re-score of the transcript answer"), failedNames(outcome).join("\n"));
});
test("a wrong shape/colour pairing chosen consistently everywhere is rejected", () => {
  const outcome = failuresOf((fx) => {
    const wrong = answerFor("stdio", "color"); wrong.objects[1].color = "green"; wrong.objects[2].color = "red";   // triangle<->square colours swapped
    const text = JSON.stringify(wrong);
    const events = fx.trials["stdio-color"].events; events.at(-2).message.content[0].text = text; events.at(-1).result = text;
    trialOf(fx, "stdio-color").answer = wrong; committedOf(fx, "stdio-color").answer = wrong;
  });
  assert.ok(caught(outcome, "stdio-color: independent re-score of the transcript answer"), failedNames(outcome).join("\n"));
});
test("a control that claims to have seen text is rejected", () => {
  const outcome = failuresOf((fx) => {
    const wrong = { visible: true, text: OCR }, text = JSON.stringify(wrong);
    const events = fx.trials["stdio-unavailable"].events; events.at(-2).message.content[0].text = text; events.at(-1).result = text;
    trialOf(fx, "stdio-unavailable").answer = wrong; committedOf(fx, "stdio-unavailable").answer = wrong;
  });
  assert.ok(caught(outcome, "stdio-unavailable: independent re-score of the transcript answer"), failedNames(outcome).join("\n"));
});
test("a committed transcript hash that does not match the transcript is rejected", () => {
  const outcome = failuresOf(null, { committedHashOverride: { "stdio-ocr": "0".repeat(64) } });
  assert.ok(caught(outcome, "stdio-ocr: committed transcript hash matches the transcript on disk"), failedNames(outcome).join("\n"));
});
test("the throwaway HTTP token leaking into a transcript is rejected", () => {
  const outcome = failuresOf((fx) => { fx.trials["stdio-ocr"].events[0].note = `Bearer ${TOKEN}`; });
  assert.ok(caught(outcome, "no HTTP test token in stdio-ocr transcript"), failedNames(outcome).join("\n"));
});
test("a pass flag the transcript does not support is rejected", () => {
  const outcome = failuresOf((fx) => { trialOf(fx, "stdio-color").answer.visible = false; trialOf(fx, "stdio-color").pass = true; });
  assert.ok(caught(outcome, "stdio-color: run trial.answer equals the transcript answer"), failedNames(outcome).join("\n"));
});

// ---- FINDING-E2E-02: no run-name exception exists; every counter must equal the evidence ----
test("E2E-02: a main-measurement-shaped run passes with no exception applied", () => {
  const outcome = failuresOf(null, { runName: "4inr07" });
  assert.deepEqual(failedNames(outcome), []);
});
test("E2E-02: a doubled client_image_blocks fails for any run name", () => {
  for (const runName of ["SYNTH1", "4inr07", "OTHER99"]) {
    const outcome = failuresOf((fx) => { for (const client of [...fx.clients, ...fx.committedClients]) client.client_image_blocks *= 2; }, { runName });
    assert.ok(caught(outcome, "stdio-color: counters match the evidence"), `${runName}: ${failedNames(outcome).join(", ")}`);
  }
});
test("E2E-02: a run directory ending in DTeZsi gets no special treatment for a doubled counter", () => {
  const outcome = failuresOf((fx) => { for (const client of fx.clients) client.client_image_blocks *= 2; }, { runName: "DTeZsi" });
  assert.ok(caught(outcome, "stdio-color: counters match the evidence"), failedNames(outcome).join(String.fromCharCode(10)));
  assert.ok(caught(outcome, "streamable-http-distinguish: counters match the evidence"));
});
test("E2E-02: the audit source contains no per-run exception table", () => {
  const source = readFileSync(path.join(path.dirname(fileURLToPath(import.meta.url)), "audit-image-thumbnail-e2e.mjs"), "utf8");
  for (const needle of ["KNOWN_RUN_DEVIATIONS", "client_image_blocks_factor", "DTeZsi", "imageFactor"]) assert.ok(!source.includes(needle), needle);
});
test("E2E-02: a transcript/aggregate mismatch on any counter fails", () => {
  for (const field of ["relay_image_blocks", "get_thumbnail_calls", "tool_calls"]) {
    const outcome = failuresOf((fx) => { trialOf(fx, "stdio-ocr")[field] += 1; });
    assert.ok(caught(outcome, "stdio-ocr: counters match the evidence"), field);
  }
});

// ---- Unit level: the extraction and scoring helpers ----
test("parseAnswerText accepts a fenced or bare JSON object and nothing else", () => {
  assert.deepEqual(parseAnswerText("```json\n{\"a\":1}\n```"), { a: 1 });
  assert.deepEqual(parseAnswerText(" {\"a\":1} "), { a: 1 });
  for (const bad of ["answer: {\"a\":1}", "[1]", "null", "", "{not json}"]) assert.throws(() => parseAnswerText(bad), undefined, bad);
});
test("extractAnswer refuses an empty transcript, no result event and an error result", () => {
  assert.throws(() => extractAnswer([]));
  assert.throws(() => extractAnswer([{ type: "assistant", message: { content: [{ type: "text", text: "{}" }] } }]));
  assert.throws(() => extractAnswer([{ type: "assistant", message: { content: [{ type: "text", text: "{}" }] } }, { type: "result", subtype: "error_max_turns", is_error: true, result: "{}" }]));
});
test("scoreColor/scoreOcr are exact where it matters", () => {
  const key = { background: "blue", objects: [{ color: "yellow", shape: "circle" }, { color: "red", shape: "triangle" }, { color: "green", shape: "square" }] };
  const good = answerFor("stdio", "color");
  assert.equal(scoreColor(good, key), null);
  assert.equal(scoreColor({ ...good, objects: [...good.objects.slice(0, 2), { shape: "rectangle", color: "Green " }] }, key), null, "rectangle for square, case/space tolerated");
  assert.notEqual(scoreColor({ ...good, objects: good.objects.slice(0, 2) }, key), null, "a missing object");
  assert.notEqual(scoreColor({ ...good, objects: [...good.objects, { shape: "star", color: "pink" }] }, key), null, "an extra object");
  assert.notEqual(scoreColor({ ...good, objects: [good.objects[0], good.objects[0], good.objects[2]] }, key), null, "the same object twice");
  assert.notEqual(scoreColor({ ...good, extra: 1 }, key), null, "an extra field");
  assert.equal(scoreOcr({ visible: true, text: OCR }, OCR), null);
  for (const text of [OCR.slice(1), OCR + "X", OCR.toLowerCase(), ` ${OCR}`, null]) assert.notEqual(scoreOcr({ visible: true, text }, OCR), null, String(text));
});
