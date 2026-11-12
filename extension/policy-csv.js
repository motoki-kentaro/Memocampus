// CSV is a local snapshot, not policy validation or authorization. No host or storage access.
// Existing Policy wire identifiers (TypedRule, serde camelCase). Keep this leaf module free of
// options-state's Host-management APIs; tests cross-check these names against the editor model.
const RULE_KINDS = ["pattern", "regex", "domainWildcard", "exact"];

export const CSV_FILENAMES = Object.freeze({
  neverSave: "memocampus-blind-zone.csv",
  saveButNoMcp: "memocampus-private-zone.csv",
});

const field = (text) => /[",\r\n]/.test(text) ? `"${text.replaceAll('"', '""')}"` : text;

function invalidData() {
  const error = new Error("Invalid CSV rule data");
  error.code = "csv_invalid_data";
  return error;
}

/** Preserve text, order and duplicates, including invalid policy input. Never normalize rules. */
export function rulesToCsv(rules, { schemaVersion = 2 } = {}) {
  if (!Array.isArray(rules) || ![1, 2].includes(schemaVersion)) throw invalidData();
  const records = ["kind,pattern"];
  for (const rule of rules) {
    // Only the explicit v1 representation uses strings. Never coerce malformed v2 data.
    const legacy = schemaVersion === 1;
    if (legacy ? typeof rule !== "string" :
      rule === null || typeof rule !== "object" || Array.isArray(rule) ||
      !Object.hasOwn(rule, "kind") || !Object.hasOwn(rule, "pattern") ||
      Object.keys(rule).length !== 2 ||
      typeof rule.kind !== "string" || typeof rule.pattern !== "string") throw invalidData();
    const kind = legacy ? "pattern" : rule.kind;
    const pattern = legacy ? rule : rule.pattern;
    if (!RULE_KINDS.includes(kind)) {
      const error = new Error("Unknown CSV rule kind");
      error.code = "csv_unsupported_kind";
      throw error;
    }
    records.push(`${kind},${field(pattern)}`);
  }
  return records.join("\r\n") + "\r\n";
}

/** Only the selected layer is read. No categories, ETag, other zone or Vault data are accepted. */
export function draftZoneCsv(draft, layerKey, { schemaVersion = 2 } = {}) {
  if (!Object.hasOwn(CSV_FILENAMES, layerKey)) throw new Error("Invalid CSV zone");
  const layer = draft.layer(layerKey);
  if (!layer || !Object.hasOwn(layer, "rules")) throw invalidData();
  return { filename: CSV_FILENAMES[layerKey], csv: rulesToCsv(layer.rules, { schemaVersion }) };
}

/** Request a browser download; browsers do not report a later cancellation/save failure here. */
export function downloadCsv({ document, window, csv, filename }) {
  // Blob's default transparent endings preserve embedded CR/LF; UTF-8, with no BOM.
  const blob = new window.Blob([csv], { type: "text/csv;charset=utf-8" });
  const url = window.URL.createObjectURL(blob);
  let anchor;
  try {
    anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = filename;
    anchor.hidden = true;
    document.body.append(anchor);
    anchor.click();
    // Do not revoke in the click's task: Chrome still needs to consume the URL.
    // The document also owns/releases its object URLs when closed.
    window.setTimeout(() => window.URL.revokeObjectURL(url), 60_000);
  } catch (error) {
    window.URL.revokeObjectURL(url);
    throw error;
  } finally {
    anchor?.remove();
  }
}

// Transport/resource bounds, not a second rule evaluator. The Host remains the validator.
// Sources: policy_gate.rs MAX_DRAFT_BYTES/MAX_URL_CHARS; policy.rs header/regex limits.
export const CSV_IMPORT_LIMITS = Object.freeze({
  fileBytes: 1_048_576,
  records: 1000,
  fieldBytes: 32_768, // Conservative byte ceiling; Host's URL ceiling is UTF-16 units.
  regexBytes: 4096,
  regexRules: 128,
  zoneRules: 1000,
  draftBytes: 1_048_576,
});

export class CsvImportError extends Error {
  constructor(code, line = 1) {
    super("CSV import refused"); // Never carry a filename, cell, Host message, or stack into the UI.
    this.code = code;
    this.line = line;
  }
}
const utf8Size = (text) => new TextEncoder().encode(text).length;

/** Bounded single-pass CSV state machine. `lines` are physical starting lines of data records. */
export function parsePolicyCsv(input) {
  if (typeof input !== "string") throw new CsvImportError("csv_read");
  if (utf8Size(input) > CSV_IMPORT_LIMITS.fileBytes) throw new CsvImportError("csv_size");
  const text = input.startsWith("\uFEFF") ? input.slice(1) : input;
  if (text === "") throw new CsvImportError("csv_empty");
  const rules = [], lines = [];
  let cells = [], value = "", state = "start", line = 1, startLine = 1, startOffset = 0;
  let header = false, recordEnding = null;
  const fail = (code, at = line) => { throw new CsvImportError(code, at); };
  const endField = () => {
    if (utf8Size(value) > CSV_IMPORT_LIMITS.fieldBytes) fail("csv_field", startLine);
    cells.push(value); value = ""; state = "start";
  };
  const endRecord = (offset) => {
    endField();
    if (!header) {
      if (text.slice(startOffset, offset) !== "kind,pattern") fail("csv_header", startLine);
      header = true;
    } else {
      if (cells.length !== 2) fail("csv_columns", startLine);
      if (!RULE_KINDS.includes(cells[0])) fail("csv_kind", startLine);
      if (rules.length >= CSV_IMPORT_LIMITS.records) fail("csv_records", startLine);
      if (cells[0] === "regex" && utf8Size(cells[1]) > CSV_IMPORT_LIMITS.regexBytes) fail("csv_field", startLine);
      rules.push({ kind: cells[0], pattern: cells[1] }); lines.push(startLine);
    }
    cells = [];
  };
  for (let i = 0; i < text.length; i += 1) {
    const ch = text[i], code = text.charCodeAt(i);
    if ((code < 32 && ![9, 10, 13].includes(code)) || (code >= 127 && code <= 159)) fail("csv_control");
    if (state === "quoted") {
      if (ch === '"') state = "afterQuote";
      else {
        value += ch;
        if (ch === "\r" || (ch === "\n" && text[i - 1] !== "\r")) line += 1;
      }
    } else if (state === "afterQuote" && ch === '"') {
      value += '"'; state = "quoted";
    } else if (ch === ",") {
      if (cells.length >= 1) fail("csv_columns", startLine);
      endField();
    } else if (ch === "\r" || ch === "\n") {
      const ending = ch === "\r" ? "crlf" : "lf";
      if (ch === "\r" && text[i + 1] !== "\n") fail("csv_newline");
      if (recordEnding && recordEnding !== ending) fail("csv_newline");
      recordEnding = ending;
      endRecord(i);
      if (ch === "\r") i += 1;
      line += 1; startLine = line; startOffset = i + 1;
    } else if (state === "afterQuote") fail("csv_quote");
    else if (ch === '"') {
      if (state !== "start") fail("csv_quote");
      state = "quoted";
    } else { value += ch; state = "unquoted"; }
    // Limit growth before a field closes, including an unclosed quoted field.
    if (value.length > CSV_IMPORT_LIMITS.fieldBytes) fail("csv_field", startLine);
  }
  if (state === "quoted") fail("csv_quote", startLine);
  if (startOffset < text.length) endRecord(text.length);
  return { rules, lines };
}

/** Fatal UTF-8 decoding, bounded before file contents are read. Selection cancellation is separate. */
export async function readPolicyCsvFile(file) {
  if (!file || !Number.isSafeInteger(file.size) || file.size < 0) throw new CsvImportError("csv_read");
  if (file.size > CSV_IMPORT_LIMITS.fileBytes) throw new CsvImportError("csv_size");
  let bytes;
  try { bytes = new Uint8Array(await file.arrayBuffer()); }
  catch { throw new CsvImportError("csv_read"); }
  if (bytes.byteLength > CSV_IMPORT_LIMITS.fileBytes) throw new CsvImportError("csv_size");
  let text;
  try { text = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(bytes); }
  catch { throw new CsvImportError("csv_utf8"); }
  return parsePolicyCsv(text);
}

/** Candidate-only append. No current Draft is changed here; the Host checks the whole candidate. */
export function csvImportCandidate(snapshot, layerKey, parsed) {
  if (!Object.hasOwn(CSV_FILENAMES, layerKey) || snapshot.schemaVersion !== 2) throw new CsvImportError("csv_readonly");
  const candidate = structuredClone(snapshot);
  const existing = candidate.layers[layerKey].rules.length;
  if (existing + parsed.rules.length > CSV_IMPORT_LIMITS.zoneRules) throw new CsvImportError("csv_records", parsed.lines[0]);
  candidate.layers[layerKey].rules.push(...structuredClone(parsed.rules));
  const regexCount = candidate.layers.neverSave.rules.concat(candidate.layers.saveButNoMcp.rules).filter((rule) => rule.kind === "regex").length;
  if (regexCount > CSV_IMPORT_LIMITS.regexRules) throw new CsvImportError("csv_regex_count", parsed.lines[0]);
  if (utf8Size(JSON.stringify(candidate)) > CSV_IMPORT_LIMITS.draftBytes) throw new CsvImportError("csv_draft_size", parsed.lines[0]);
  return candidate;
}
