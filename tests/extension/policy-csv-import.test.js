import { test } from "node:test";
import assert from "node:assert/strict";
import { CSV_IMPORT_LIMITS as LIMIT, parsePolicyCsv, readPolicyCsvFile, csvImportCandidate, rulesToCsv } from "../../extension/policy-csv.js";
import { createDraft } from "../../extension/options-state.js";
import { pattern, regex, v2 } from "./options-fixtures.js";

export const csvFile = (text) => {
  const bytes = new TextEncoder().encode(text);
  return { size: bytes.length, arrayBuffer: async () => bytes.buffer };
};
const refused = (csv, code, line) => assert.throws(() => parsePolicyCsv(csv), (error) => {
  assert.equal(error.code, code); if (line) assert.equal(error.line, line);
  assert.equal(error.message, "CSV import refused"); return true;
});

test("all four kinds and text round-trip through Export / Import, order and duplicates intact", () => {
  const rules = [pattern("x"), regex("^x[日本語🧠]"), { kind: "domainWildcard", pattern: "*.例え.jp" },
    { kind: "exact", pattern: 'https://x.test/é?q=a,b"c' }, pattern("x")];
  assert.deepEqual(parsePolicyCsv(rulesToCsv(rules)).rules, rules);
});

for (const text of ["", " \t ", " leading and trailing ", 'a,b"c', "CR\rLF\nCRLF\r\nend", "日本語🧠e\u0301", "=1+1", "+1", "-1", "@SUM(A1)", "\uFEFFinside"]) {
  test(`lossless cells ${JSON.stringify(text)}`, () => {
    const rules = ["pattern", "regex", "domainWildcard", "exact"].map((kind) => ({ kind, pattern: text }));
    assert.deepEqual(parsePolicyCsv(rulesToCsv(rules)).rules, rules);
  });
}
for (const ending of ["\r\n", "\n"]) {
  for (const bom of ["", "\uFEFF"]) {
    test(`BOM=${Boolean(bom)}, ending=${JSON.stringify(ending)}, optional final newline`, async () => {
      for (const tail of ["", ending]) {
        const csv = bom + "kind,pattern" + ending + 'regex,"a""b"' + tail;
        assert.deepEqual((await readPolicyCsvFile(csvFile(csv))).rules, [regex('a"b')]);
      }
    });
  }
}
test("header-only CSV with or without a final newline is an empty batch", () => {
  for (const text of ["kind,pattern", "kind,pattern\r\n", "\uFEFFkind,pattern\n"]) assert.deepEqual(parsePolicyCsv(text), { rules: [], lines: [] });
});
test("quoted physical line endings are kept and record starting line numbers tracked", () => {
  assert.deepEqual(parsePolicyCsv('kind,pattern\r\nregex,"a\rb\nc\r\nd"\r\npattern,z'), {
    rules: [regex("a\rb\nc\r\nd"), pattern("z")], lines: [2, 6],
  });
});
for (const [label, text, code, line] of [
  ["empty", "", "csv_empty", 1], ["BOM only", "\uFEFF", "csv_empty", 1],
  ["header case", "Kind,pattern\npattern,x", "csv_header", 1],
  ["header space", "kind,pattern \npattern,x", "csv_header", 1],
  ["quoted header", '"kind",pattern\npattern,x', "csv_header", 1],
  ["old schema", "regex,pattern\nfalse,x", "csv_header", 1],
  ["unknown kind", "kind,pattern\nPattern,x", "csv_kind", 2],
  ["proto kind", "kind,pattern\n__proto__,x", "csv_kind", 2],
  ["zone kind", "kind,pattern\nneverSave,x", "csv_kind", 2],
  ["missing column", "kind,pattern\npattern", "csv_columns", 2],
  ["extra column", "kind,pattern\npattern,x,saveButNoMcp", "csv_columns", 2],
  ["blank record", "kind,pattern\n\n", "csv_columns", 2],
  ["unclosed quote", 'kind,pattern\nregex,"secret\ntext', "csv_quote", 2],
  ["unescaped quote", 'kind,pattern\nregex,a"secret', "csv_quote", 2],
  ["after quote", 'kind,pattern\nregex,"a" secret', "csv_quote", 2],
  ["mixed record endings", "kind,pattern\r\npattern,x\n", "csv_newline", 2],
  ["bare CR", "kind,pattern\rpattern,x", "csv_newline", 1],
  ["NUL", "kind,pattern\npattern,secret\0", "csv_control", 2],
  ["control", "kind,pattern\npattern,secret\u000b", "csv_control", 2],
  ["DEL", "kind,pattern\npattern,secret\u007f", "csv_control", 2],
  ["line after multiline cell", 'kind,pattern\npattern,"a\nb"\nUNKNOWN,secret', "csv_kind", 4],
]) test(`refuses ${label} with a safe line-numbered error`, () => refused(text, code, line));

test("fatal UTF-8 rejects invalid, truncated, overlong and surrogate byte sequences", async () => {
  for (const sequence of [[0xff], [0xc3], [0xc0, 0xaf], [0xed, 0xa0, 0x80]]) {
    const bytes = Uint8Array.from([...new TextEncoder().encode("kind,pattern\npattern,"), ...sequence]);
    await assert.rejects(readPolicyCsvFile({ size: bytes.length, arrayBuffer: async () => bytes.buffer }), { code: "csv_utf8" });
  }
});
test("file size is checked before reading and actual byte length is checked again", async () => {
  let reads = 0;
  await assert.rejects(readPolicyCsvFile({ size: LIMIT.fileBytes + 1, arrayBuffer() { reads++; } }), { code: "csv_size" });
  assert.equal(reads, 0);
  await assert.rejects(readPolicyCsvFile({ size: 1, arrayBuffer: async () => new ArrayBuffer(LIMIT.fileBytes + 1) }), { code: "csv_size" });
  await assert.rejects(readPolicyCsvFile({ size: 1, arrayBuffer() { throw new Error("PRIVATE_PATH"); } }), { code: "csv_read" });
});
test("file / field / Regex byte / record resource ceilings are enforced", () => {
  refused("x".repeat(LIMIT.fileBytes + 1), "csv_size", 1);
  refused("kind,pattern\npattern," + "x".repeat(LIMIT.fieldBytes + 1), "csv_field", 2);
  refused("kind,pattern\npattern," + "🧠".repeat(LIMIT.fieldBytes / 4 + 1), "csv_field", 2);
  refused("kind,pattern\nregex," + "x".repeat(LIMIT.regexBytes + 1), "csv_field", 2);
  assert.equal(parsePolicyCsv("kind,pattern\nregex," + "x".repeat(LIMIT.regexBytes)).rules[0].pattern.length, LIMIT.regexBytes);
  assert.equal(parsePolicyCsv("kind,pattern\n" + "pattern,x\n".repeat(LIMIT.records)).rules.length, LIMIT.records);
  refused("kind,pattern\n" + "pattern,x\n".repeat(LIMIT.records + 1), "csv_records", LIMIT.records + 2);
});
test("candidate append preserves existing rules, duplicate order, categories, disabled state and other zone", () => {
  const snapshot = v2({ neverSave: [pattern("old")], saveButNoMcp: [regex("private")] });
  snapshot.layers.neverSave.enabled = false;
  const original = structuredClone(snapshot);
  const parsed = parsePolicyCsv("kind,pattern\npattern,old\npattern,new\npattern,old");
  const candidate = csvImportCandidate(snapshot, "neverSave", parsed);
  assert.deepEqual(candidate, { ...original, layers: { ...original.layers, neverSave: { enabled: false, rules: [pattern("old"), pattern("old"), pattern("new"), pattern("old")] } } });
  assert.deepEqual(snapshot, original);
  assert.equal({}.polluted, undefined);
  const payload = '{"__proto__":{"polluted":true},"constructor":"secret"}';
  assert.deepEqual(parsePolicyCsv(rulesToCsv([regex(payload)])).rules, [regex(payload)]);
  assert.equal({}.polluted, undefined, "CSV cells are data, never object keys or parsed objects");
});
test("combined candidate limits and non-zone / legacy targets are refused without changing snapshot", () => {
  const parsed = parsePolicyCsv("kind,pattern\npattern,new");
  const full = v2({ neverSave: Array.from({ length: LIMIT.zoneRules }, () => pattern("old")) });
  assert.throws(() => csvImportCandidate(full, "neverSave", parsed), { code: "csv_records" });
  const regexes = v2({ saveButNoMcp: Array.from({ length: LIMIT.regexRules }, () => regex("x")) });
  assert.throws(() => csvImportCandidate(regexes, "neverSave", parsePolicyCsv("kind,pattern\nregex,z")), { code: "csv_regex_count" });
  const large = v2({ saveButNoMcp: [pattern("x".repeat(LIMIT.draftBytes))] });
  assert.throws(() => csvImportCandidate(large, "neverSave", parsed), { code: "csv_draft_size" });
  for (const target of ["__proto__", "constructor", "layers"]) assert.throws(() => csvImportCandidate(v2(), target, parsed), { code: "csv_readonly" });
  assert.throws(() => csvImportCandidate(v2({}, { schemaVersion: 1 }), "neverSave", parsed), { code: "csv_readonly" });
  assert.equal(full.layers.neverSave.rules.length, LIMIT.zoneRules);
});
test("atomic Draft batch appends once and clones input; duplicates are not silently removed", () => {
  const active = { policy: v2({ neverSave: [pattern("old")] }) };
  const draft = createDraft(active, { editable: true });
  const batch = [pattern("old"), regex(""), pattern("old")]; // Empty Regex is valid to the Host.
  assert.equal(draft.appendRulesAtomically("neverSave", batch, { expectedVersion: 0 }), true);
  assert.equal(draft.version, 1); assert.equal(draft.layout("neverSave"), 1); assert.equal(draft.dirty(), true);
  batch[0].pattern = "MUTATED";
  assert.deepEqual(draft.layer("neverSave").rules, [pattern("old"), pattern("old"), regex(""), pattern("old")]);
  assert.deepEqual(active.policy.layers.neverSave.rules, [pattern("old")]);
});
test("atomic batch rejects stale, read-only and malformed updates without partial change", () => {
  const draft = createDraft({ policy: v2() }, { editable: true });
  for (const batch of [[pattern("first"), { kind: "unknown", pattern: "secret" }], [pattern("first"), { kind: "pattern", pattern: null }], new Array(1),
    [pattern("first"), Object.create({ kind: "pattern", pattern: "inherited" })]]) {
    assert.equal(draft.appendRulesAtomically("neverSave", batch, { expectedVersion: 0 }), false);
  }
  assert.equal(draft.appendRulesAtomically("neverSave", [pattern("x")], { expectedVersion: 1 }), false);
  assert.equal(draft.appendRulesAtomically("__proto__", [pattern("x")], { expectedVersion: 0 }), false);
  const readonly = createDraft({ policy: v2() }, { editable: false });
  assert.equal(readonly.appendRulesAtomically("neverSave", [pattern("x")], { expectedVersion: 0 }), false);
  assert.equal(draft.appendRulesAtomically("neverSave", [], { expectedVersion: 0 }), true);
  assert.equal(draft.version, 0); assert.equal(draft.dirty(), false);
});
