// #61 Slice 3 — independent audit of a finished E2E run directory. Read-only: it never starts a
// process, never calls a model and never writes. It re-derives, from the files a run left behind,
// the claims docs/issue-061-image-thumbnail-e2e.md makes, so a reviewer need not trust the report.
//
//   node scripts/audit-image-thumbnail-e2e.mjs <run-directory> [docs/issue-061-image-thumbnail-e2e-results.json]
//
// <run-directory> is the `memocampus-61-e2e-<label>` directory under the OS temp directory that
// verify-image-thumbnail-e2e.mjs printed as `artifacts`. Exit code 0 = every check held.
//
// FINDING-E2E-01 (independent review): the first version trusted the aggregated `trial.answer` and
// only counted trials. It now (A) requires exactly the expected eight conditions, each once, from a
// constant written here, never read back from the results it audits; (B) extracts the model's real
// answer from each primary-evidence transcript; (C) re-scores that answer against the verifier-only
// answer key (answers.json) and the relay/Vault facts; and (D) requires the transcript answer, the
// run's aggregated answer and the committed (published) answer to agree. Every earlier safety check
// (delivered-image hashes, public/non-public split, answer isolation, secret/payload scans) is kept.
import { createHash } from "node:crypto";
import { existsSync, readFileSync, readdirSync } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";

// ---- A. The expected trial conditions (evidence: both stored runs hold exactly these eight) ----
export const TRANSPORTS = ["stdio", "streamable-http"];
export const KINDS = ["color", "ocr", "distinguish", "unavailable"];
export const EXPECTED_CONDITIONS = TRANSPORTS.flatMap((transport) => KINDS.map((kind) => `${transport}-${kind}`));

const sha = (data) => createHash("sha256").update(data).digest("hex");
const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
// Key-order-independent serialisation, so "same answer" does not depend on how it was written.
export const canonical = (value) => JSON.stringify(value, (_key, inner) => isObject(inner) ? Object.fromEntries(Object.keys(inner).sort().map((key) => [key, inner[key]])) : inner);
const sameKeys = (value, keys) => isObject(value) && Object.keys(value).sort().join() === [...keys].sort().join();

// ---- B. The model's real answer, taken from the primary-evidence transcript ----
// Strict: the answer must be one JSON object, optionally inside a ```json fence, nothing else.
export function parseAnswerText(text) {
  if (typeof text !== "string") throw new Error("answer is not text");
  const fenced = text.trim().match(/^```(?:json)?\s*\n([\s\S]*?)\n?```$/i);
  const body = (fenced ? fenced[1] : text).trim();
  let value;
  try { value = JSON.parse(body); } catch { throw new Error("answer is not pure JSON"); }
  if (!isObject(value)) throw new Error("answer is not a JSON object");
  return value;
}

// The final answer = the assistant text after the last tool result, and it must equal the result event.
export function extractAnswer(events) {
  if (!Array.isArray(events) || events.length === 0) throw new Error("transcript is empty");
  const results = events.filter((event) => event?.type === "result");
  if (results.length !== 1) throw new Error(`expected exactly one result event, found ${results.length}`);
  const [result] = results;
  if (result.subtype !== "success" || result.is_error !== false) throw new Error(`result event is ${result.subtype}/is_error=${result.is_error}`);
  const lastUser = events.map((event) => event?.type).lastIndexOf("user");
  const finals = events.slice(lastUser + 1).filter((event) => event?.type === "assistant")
    .flatMap((event) => (event.message?.content ?? []).filter((block) => block?.type === "text").map((block) => block.text));
  if (finals.length === 0) throw new Error("no final assistant answer after the last tool result");
  const parsed = finals.map(parseAnswerText);
  if (new Set(parsed.map(canonical)).size !== 1) throw new Error("conflicting final answers");
  const fromResult = parseAnswerText(result.result);
  if (canonical(fromResult) !== canonical(parsed[0])) throw new Error("final assistant answer differs from the result event");
  return parsed[0];
}

// ---- C. Independent scoring against the verifier-only key ----
// Key for fixture A comes from answers.json: "<color> background; <color> <shape>; ..."
export function parseDescription(text) {
  const parts = String(text ?? "").split(";").map((part) => part.trim().toLowerCase()).filter(Boolean);
  const background = parts[0]?.match(/^([a-z]+) background$/)?.[1];
  const objects = parts.slice(1).map((part) => part.match(/^([a-z]+) ([a-z]+)$/)).map((match) => match && { color: match[1], shape: match[2] });
  if (!background || objects.length === 0 || objects.some((object) => !object)) throw new Error(`answer key description is unparsable: ${text}`);
  return { background, objects };
}
// Tolerated expressions: case and surrounding whitespace only; a "square" may be called a "rectangle".
const SHAPE_ALIASES = { square: ["square", "rectangle"] };
const shapeMatches = (key, said) => (SHAPE_ALIASES[key] ?? [key]).includes(said);
const norm = (value) => typeof value === "string" ? value.trim().toLowerCase() : null;

export function scoreColor(answer, keyA) {
  if (!sameKeys(answer, ["visible", "background", "objects"])) return "fields are not {visible, background, objects}";
  if (answer.visible !== true) return "visible is not true";
  if (norm(answer.background) !== keyA.background) return `background ${JSON.stringify(answer.background)} != ${keyA.background}`;
  if (!Array.isArray(answer.objects) || answer.objects.length !== keyA.objects.length) return `expected exactly ${keyA.objects.length} objects`;
  if (!answer.objects.every((object) => sameKeys(object, ["shape", "color"]))) return "an object is not {shape, color}";
  const claimed = answer.objects.map((object) => ({ shape: norm(object.shape), color: norm(object.color) }));
  const assign = (index, used) => index === keyA.objects.length || claimed.some((object, at) => !used.has(at) && object.color === keyA.objects[index].color && shapeMatches(keyA.objects[index].shape, object.shape) && assign(index + 1, new Set([...used, at])));
  return assign(0, new Set()) ? null : "the shapes/colors do not match the key one-to-one";
}
export function scoreOcr(answer, key) {
  if (!sameKeys(answer, ["visible", "text"])) return "fields are not {visible, text}";
  if (answer.visible !== true) return "visible is not true";
  return answer.text === key ? null : `text ${JSON.stringify(answer.text)} is not the exact key`;
}
export function scoreUnavailable(answer) {
  if (!sameKeys(answer, ["visible", "text"])) return "fields are not {visible, text}";
  return answer.visible === false && answer.text === null ? null : "claimed to see something that was never delivered";
}
export function scoreDistinguish(answer, expectedId, catalogIds) {
  if (!sameKeys(answer, ["image_id", "observations"])) return "fields are not {image_id, observations}";
  if (answer.image_id !== expectedId) return `selected ${JSON.stringify(answer.image_id)}, the red-triangle image is ${expectedId}`;
  const seen = Array.isArray(answer.observations) ? answer.observations : [];
  if (seen.length !== catalogIds.length || !seen.every((item) => sameKeys(item, ["image_id", "description"]) && typeof item.description === "string" && item.description.trim())) return "observations are not one {image_id, description} per candidate";
  return [...seen.map((item) => item.image_id)].sort().join() === [...catalogIds].sort().join() ? null : "observations do not cover exactly the candidates the server listed";
}

const imageParts = (content) => (Array.isArray(content) ? content : []).flatMap((block) => block?.type === "tool_result" ? imageParts(block.content) : block?.type === "image" ? [block] : []);

export function audit(root, committedPath) {
  const failures = [];
  const rows = [];
  const rescored = { positives: 0, controls: 0 };   // trials whose transcript answer I re-scored as correct
  const check = (name, held, detail = "") => { rows.push({ name, held: held === true, detail }); if (held !== true) failures.push(name); };
  const read = (...parts) => readFileSync(path.join(root, ...parts), "utf8");
  const readJson = (...parts) => JSON.parse(read(...parts));
  const load = (name, load) => { try { return load(); } catch (error) { check(`run file readable: ${name}`, false, error.message); return null; } };

  const results = load("results.json", () => readJson("results.json"));
  const answers = load("answers.json", () => readJson("answers.json"));       // verifier-only; the model never had it
  const manifest = load("manifest.json", () => readJson("manifest.json"));
  const token = load("app/mcp-http-token", () => read("app", "mcp-http-token").trim());   // this run's throwaway HTTP token
  const committed = committedPath ? load(committedPath, () => JSON.parse(readFileSync(committedPath, "utf8"))) : null;
  if (!results || !answers || !manifest || token === null || (committedPath && !committed)) return { rows, failures, rescored };

  const keyOk = typeof answers.ocr === "string" && /^[A-Z0-9]{8}$/.test(answers.ocr);
  check("answer key: OCR key is the 8-character challenge", keyOk);
  let keyA = null, keyC = null;
  try { keyA = parseDescription(answers.descriptions?.A); keyC = parseDescription(answers.descriptions?.C); } catch (error) { check("answer key: descriptions of A and C are parsable", false, error.message); }
  if (keyA && keyC) check("answer key: descriptions of A and C are parsable", true);
  const ocrKey = keyOk ? answers.ocr : null;

  const saved = Array.isArray(manifest.saved) ? manifest.saved : [];
  const hashOf = (image) => sha(Buffer.from(image.thumbnail.data, "base64"));
  // Thumbnails the Vault holds for the three PUBLIC fixtures (A, B, C), by SHA-256 of the bytes.
  const publicKinds = new Map(saved.filter((image) => "ABC".includes(image.kind) && image.thumbnail).map((image) => [hashOf(image), image.kind]));
  const nonpublicKinds = new Set(saved.filter((image) => !"ABC".includes(image.kind) && image.thumbnail).map(hashOf));
  const hashByKind = (kind) => [...publicKinds].find(([, value]) => value === kind)?.[0];
  const imageA = saved.find((image) => image.kind === "A");
  check("run has exactly 3 public fixture thumbnails", publicKinds.size === 3);

  // ---- A. every expected condition exactly once, nothing else — in the run, its directory and the committed copy ----
  const conditionKey = (entry) => `${entry?.transport}-${entry?.kind}`;
  const complete = (name, list) => {
    const keys = (Array.isArray(list) ? list : []).map(conditionKey);
    const counts = new Map(); for (const key of keys) counts.set(key, (counts.get(key) ?? 0) + 1);
    const missing = EXPECTED_CONDITIONS.filter((key) => !counts.has(key));
    const duplicated = [...counts].filter(([, count]) => count > 1).map(([key]) => key);
    const unknown = [...counts.keys()].filter((key) => !EXPECTED_CONDITIONS.includes(key));
    check(`${name}: exactly the 8 expected conditions, each once`, Array.isArray(list) && keys.length === EXPECTED_CONDITIONS.length && !missing.length && !duplicated.length && !unknown.length,
      `missing [${missing}] duplicated [${duplicated}] unknown [${unknown}] count ${keys.length}`);
    return (key) => (Array.isArray(list) ? list : []).filter((entry) => conditionKey(entry) === key);
  };
  const runTrials = complete("results.json clients", results.clients);
  const dirs = existsSync(path.join(root, "trials")) ? readdirSync(path.join(root, "trials")).sort() : [];
  check("trials/ holds exactly the 8 expected condition directories", dirs.join() === [...EXPECTED_CONDITIONS].sort().join(), dirs.join(","));
  const committedTrials = committed ? complete("committed results clients", committed.clients) : () => [];
  const committedEvidence = committed ? complete("committed results evidence", committed.evidence) : () => [];

  for (const label of EXPECTED_CONDITIONS) {
    const kind = KINDS.find((name) => label.endsWith(`-${name}`));
    const matches = runTrials(label);
    if (matches.length !== 1) { check(`${label}: exactly one trial record to audit`, false, `${matches.length}`); continue; }
    const [trial] = matches;
    const dir = path.join("trials", label);
    try {
      const events = readJson(dir, "transcript.sanitized.json");
      const relay = read(dir, "relay.jsonl").trim().split("\n").filter(Boolean).map((line) => JSON.parse(line));
      const calls = relay.filter((entry) => entry.method === "tools/call");
      const init = events.find((event) => event?.type === "system" && event.subtype === "init");
      const models = new Set(events.filter((event) => event?.type === "assistant").map((event) => event.message?.model));

      check(`${label}: result.json equals results.json`, canonical(readJson(dir, "result.json")) === canonical(trial));
      check(`${label}: model id`, init?.model === trial.model && models.size === 1 && models.has(trial.model), `${init?.model} / assistant ${[...models]}`);
      check(`${label}: Claude Code version`, `${init?.claude_code_version} (Claude Code)` === trial.version, init?.claude_code_version);
      check(`${label}: only the two test MCP tools were offered, server connected`,
        init?.tools?.length === 2 && init.tools.every((name) => name.startsWith("mcp__memocampus_e2e__")) && init.mcp_servers?.every((server) => server.status === "connected"), JSON.stringify(init?.tools));
      check(`${label}: empty working directory`, existsSync(path.join(root, dir, "cwd")) && readdirSync(path.join(root, dir, "cwd")).length === 0);

      // What the MODEL was given: images inside the user (tool-result) messages only. The CLI also copies
      // the payload into `tool_use_result` for diagnostics; that copy is not model input and is ignored.
      const modelImages = events.filter((event) => event?.type === "user").flatMap((event) => imageParts(event.message?.content));
      const modelHashes = modelImages.map((block) => block.source?.data_summary?.sha256);
      const delivered = calls.flatMap((entry) => entry.delivered_images.map((image) => image.sha256));
      const thumbnailCalls = calls.filter((entry) => entry.tool === "memocampus_get_image_thumbnail");
      check(`${label}: model-facing image hashes == relay-delivered hashes`, canonical(modelHashes) === canonical(delivered), `${modelHashes.length} images`);
      check(`${label}: every delivered image is a stored PUBLIC thumbnail, none non-public`, delivered.every((hash) => publicKinds.has(hash) && !nonpublicKinds.has(hash)), delivered.map((hash) => publicKinds.get(hash)).join(","));
      check(`${label}: image media type is image/webp`, modelImages.every((block) => block.source?.media_type === "image/webp"));
      // The aggregated counters must describe this transcript and relay, not stand alone.
      check(`${label}: counters match the evidence (images / delivered / thumbnail calls / tool calls)`,
        trial.client_image_blocks === modelImages.length && trial.relay_image_blocks === delivered.length && trial.get_thumbnail_calls === thumbnailCalls.length && trial.tool_calls === calls.length,
        `trial ${trial.client_image_blocks}/${trial.relay_image_blocks}/${trial.get_thumbnail_calls}/${trial.tool_calls} vs evidence ${modelImages.length}/${delivered.length}/${thumbnailCalls.length}/${calls.length}`);

      // The answer key must not be reachable before the model saw the image.
      const firstImage = events.findIndex((event) => event?.type === "user" && imageParts(event.message?.content).length > 0);
      const before = JSON.stringify(firstImage < 0 ? events.filter((event) => event?.type !== "result") : events.slice(0, firstImage));
      check(`${label}: OCR answer absent from everything before the first image`, ocrKey !== null && !before.includes(ocrKey));
      check(`${label}: answer-table descriptions absent from model input`, !JSON.stringify(events.filter((event) => event?.type === "user")).match(/purple|cyan|orange|ellipse/i), "colors/shapes of C and A are not in any tool result text");
      check(`${label}: OCR answer absent from relay log and metadata`, ocrKey !== null && !JSON.stringify(relay).includes(ocrKey));
      const searchCalls = calls.filter((entry) => entry.tool === "memocampus_search_images");
      const searchText = JSON.stringify(searchCalls);
      check(`${label}: search metadata names only fixture URLs and ids`, [...searchText.matchAll(/"latest_image_url":"([^"]+)"/g)].every((match) => match[1].startsWith("https://fixture.example/vision-fixture/")) && !searchText.includes("CANARY"));

      // ---- B/C. the model's real answer, re-scored independently ----
      let actual = null;
      try { actual = extractAnswer(events); check(`${label}: a unique, well-formed final answer was extracted from the transcript`, true, canonical(actual)); }
      catch (error) { check(`${label}: a unique, well-formed final answer was extracted from the transcript`, false, error.message); }

      let verdict = actual === null ? "no extractable answer" : "unscored";
      if (actual !== null) {
        if (kind === "color") {
          verdict = keyA ? scoreColor(actual, keyA) : "no key";
          check(`${label}: the image of fixture A (not another) reached the model`, delivered.includes(hashByKind("A")));
        } else if (kind === "ocr") {
          verdict = ocrKey ? scoreOcr(actual, ocrKey) : "no key";
          check(`${label}: the image of fixture B (not another) reached the model`, delivered.includes(hashByKind("B")));
        } else if (kind === "unavailable") {
          verdict = scoreUnavailable(actual);
          check(`${label}: control — the real server did return an image, the relay withheld it`, calls.some((entry) => entry.original_images.length > 0) && delivered.length === 0);
          check(`${label}: control — the model saw no image`, modelImages.length === 0);
        } else if (kind === "distinguish") {
          const catalog = searchCalls.flatMap((entry) => entry.public_images ?? []);
          const byUrl = catalog.find((entry) => entry.latest_image_url === imageA?.imageUrl)?.id;
          // Cross-check: the id the model must pick is the one whose delivered bytes are fixture A's thumbnail.
          const byBytes = thumbnailCalls.find((entry) => entry.delivered_images.some((image) => image.sha256 === hashByKind("A")))?.arguments?.image_id;
          check(`${label}: expected id (catalog by URL) agrees with the id whose delivered bytes are A's thumbnail`, !!byUrl && byUrl === byBytes, `${byUrl} / ${byBytes}`);
          verdict = byUrl ? scoreDistinguish(actual, byUrl, catalog.map((entry) => entry.id)) : "no expected id";
          check(`${label}: all three public thumbnails were fetched`, new Set(delivered).size === 3 && ["A", "B", "C"].every((kindName) => delivered.includes(hashByKind(kindName))));
        }
        check(`${label}: independent re-score of the transcript answer`, verdict === null, verdict ?? "correct");
        if (verdict === null) rescored[kind === "unavailable" ? "controls" : "positives"]++;
      }

      // ---- D. transcript answer == run aggregate == committed aggregate (by field meaning, not whole-record equality) ----
      const expectedPass = verdict === null;
      check(`${label}: run trial.answer equals the transcript answer`, actual !== null && canonical(trial.answer) === canonical(actual));
      check(`${label}: run trial.pass equals the independent verdict`, trial.pass === expectedPass, `trial.pass=${trial.pass}, independent=${expectedPass}`);
      if (kind === "ocr") check(`${label}: run exact_ocr_match equals text === key`, actual !== null && trial.exact_ocr_match === (actual.text === ocrKey));
      if (committed) {
        const published = committedTrials(label);
        check(`${label}: exactly one committed record`, published.length === 1, `${published.length}`);
        if (published.length === 1) {
          const [record] = published;
          check(`${label}: committed answer equals the transcript answer`, actual !== null && canonical(record.answer) === canonical(actual));
          check(`${label}: committed pass equals the independent verdict`, record.pass === expectedPass);
          check(`${label}: committed counters equal the run's`, ["client_image_blocks", "relay_image_blocks", "get_thumbnail_calls", "tool_calls", "model", "version", "result_subtype"].every((field) => record[field] === trial[field]));
          if (kind === "ocr") check(`${label}: committed exact_ocr_match equals text === key`, actual !== null && record.exact_ocr_match === (actual.text === ocrKey));
        }
        const published2 = committedEvidence(label);
        const onDisk = existsSync(path.join(root, dir, "transcript.sanitized.json")) ? sha(readFileSync(path.join(root, dir, "transcript.sanitized.json"))) : null;
        check(`${label}: committed transcript hash matches the transcript on disk`, published2.length === 1 && published2[0].transcript_sha256 === onDisk);
      }
    } catch (error) {
      check(`${label}: audit could run on the primary evidence`, false, error.message);
    }
  }

  // Secrets and payloads must not be in anything that may be committed or shared.
  const readable = (...parts) => { try { return read(...parts); } catch { return ""; } };
  const publishable = [["results.json", read("results.json")], ...(committed ? [["committed results", JSON.stringify(committed)]] : []),
    ...EXPECTED_CONDITIONS.map((label) => [`${label} transcript`, readable("trials", label, "transcript.sanitized.json")])];
  for (const [name, text] of publishable) {
    check(`no HTTP test token in ${name}`, token.length > 0 && !text.includes(token));
    check(`no image base64 (WebP header) in ${name}`, !text.includes("UklGR"));
    check(`no original PNG base64 in ${name}`, !text.includes("iVBORw0KGgo")); // PNG base64 prefix: originals are never returned
  }
  if (committed) {
    check("committed results: commit field is a 40-hex SHA", /^[0-9a-f]{40}$/.test(committed.commit));
    check("committed run label matches this directory", path.basename(root).endsWith(committed.run_label ?? "?"), committed.run_label);
  }
  return { rows, failures, results, rescored };
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  const [root, committedPath] = process.argv.slice(2);
  if (!root) { console.error("usage: node scripts/audit-image-thumbnail-e2e.mjs <run-directory> [committed-results.json]"); process.exit(2); }
  const { rows, failures, rescored } = audit(root, committedPath);
  for (const row of rows) console.log(`${row.held ? "PASS" : "FAIL"}  ${row.name}${row.detail ? `  [${row.detail}]` : ""}`);
  console.log(JSON.stringify({ checks: rows.length, failed: failures.length, positive_trials_rescored_correct: rescored.positives, negative_controls_rescored_correct: rescored.controls }));
  process.exit(failures.length ? 1 : 0);
}
