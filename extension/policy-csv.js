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
