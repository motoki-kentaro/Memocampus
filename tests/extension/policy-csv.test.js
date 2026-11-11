import { test } from "node:test";
import assert from "node:assert/strict";
import { rulesToCsv, draftZoneCsv, downloadCsv } from "../../extension/policy-csv.js";
import { RULE_KINDS, createDraft, readActive } from "../../extension/options-state.js";
import { pattern, regex, v2, openPage, click, addRule, guarded, html, fakeHost, flush, type } from "./options-fixtures.js";

// Independent CSV reader for the test oracle; it does not split quoted embedded line endings.
function parse(csv) {
  const rows = [];
  let row = [], cell = "", quoted = false;
  for (let i = 0; i < csv.length; i += 1) {
    const ch = csv[i];
    if (ch === '"') {
      if (quoted && csv[i + 1] === '"') { cell += '"'; i += 1; }
      else quoted = !quoted;
    } else if (!quoted && ch === ",") { row.push(cell); cell = ""; }
    else if (!quoted && ch === "\r") {
      assert.equal(csv[++i], "\n", "record separators must be CRLF");
      row.push(cell); rows.push(row); row = []; cell = "";
    } else {
      assert.ok(quoted || ch !== "\n", "bare LF outside quotes");
      cell += ch;
    }
  }
  assert.equal(quoted, false);
  assert.deepEqual(row, []);
  assert.equal(cell, "");
  return rows;
}

test("CSV header only for an empty Draft, with CRLF and no BOM", () => {
  assert.equal(rulesToCsv([]), "kind,pattern\r\n");
  assert.deepEqual(parse(rulesToCsv([])), [["kind", "pattern"]]);
});

for (const [label, text] of [
  ["comma", "example.test/a,b"], ["quotes", 'a"b"c'], ["CR", "a\rb"],
  ["LF", "a\nb"], ["CRLF", "a\r\nb"], ["mixed", 'a,"b\r\nc\nd\re'],
  ["Japanese Unicode emoji", "日本語/é/e\u0301/🧠/𠮷"], ["spaces", " \t "],
  ["leading trailing spaces", " example.test "], ["empty string", ""],
  ["invalid Pattern", "bad @ : \\?#"], ["invalid Regex", "(?<=["],
  ["formula equals", '=HYPERLINK("https://example.test")'], ["formula plus", "+1+1"],
  ["formula minus", "-1+1"], ["formula at", "@SUM(A1)"], ["formula tab", "\t=1+1"],
]) {
  test(`CSV preserves ${label} verbatim, including after UTF-8 encoding`, () => {
    const csv = rulesToCsv(RULE_KINDS.map((kind) => ({ kind, pattern: text })));
    const bytes = Buffer.from(csv, "utf8");
    assert.notDeepEqual([...bytes.subarray(0, 3)], [0xef, 0xbb, 0xbf]);
    assert.deepEqual(parse(bytes.toString("utf8")), [["kind", "pattern"], ...RULE_KINDS.map((kind) => [kind, text])]);
  });
}

test("RFC 4180 escaping doubles quotes and quotes comma / CR / LF fields", () => {
  assert.equal(rulesToCsv([pattern('a,"b\r\nc')]), 'kind,pattern\r\npattern,"a,""b\r\nc"\r\n');
});

test("CSV preserves all four kinds, their text associations, order, and duplicates", () => {
  const rules = [regex("z"), { kind: "exact", pattern: "same-text" }, pattern("same-text"),
    { kind: "domainWildcard", pattern: "*.example.test" }, regex("z"), { kind: "exact", pattern: "same-text" }];
  assert.deepEqual(parse(rulesToCsv(rules)), [
    ["kind", "pattern"], ["regex", "z"], ["exact", "same-text"], ["pattern", "same-text"],
    ["domainWildcard", "*.example.test"], ["regex", "z"], ["exact", "same-text"],
  ]);
  assert.deepEqual(parse(rulesToCsv(["v1"], { schemaVersion: 1 })), [["kind", "pattern"], ["pattern", "v1"]]);
});

for (const [key, other, filename] of [
  ["neverSave", "saveButNoMcp", "memocampus-blind-zone.csv"],
  ["saveButNoMcp", "neverSave", "memocampus-private-zone.csv"],
]) {
  test(`only ${key} Draft is read, not the other zone or Active`, () => {
    const reads = [];
    const draft = { layer(zone) { reads.push(zone); assert.notEqual(zone, other); return { rules: [pattern(key)] }; } };
    assert.deepEqual(draftZoneCsv(draft, key), { filename, csv: `kind,pattern\r\npattern,${key}\r\n` });
    assert.deepEqual(reads, [key]);
    assert.throws(() => draftZoneCsv(draft, "unknown"));
  });
}

for (const kind of ["unknown", "DomainWildcard", "Exact URL"]) {
  test(`CSV refuses ${kind} without silent conversion or partial download`, () => {
    assert.throws(() => rulesToCsv([pattern("first"), { kind, pattern: "secret" }]), { code: "csv_unsupported_kind" });
  });
}

test("malformed internal data is refused without coercion, omission, or partial output", () => {
  const malformed = [null, undefined, 1, false, "v2 is not a string rule", [], {},
    { kind: "pattern" }, { pattern: "secret" }, { kind: null, pattern: "secret" },
    { kind: "regex", pattern: 1 }, { kind: "exact", pattern: null },
    { kind: "pattern", pattern: "secret", extra: "must not silently omit" },
    Object.create({ kind: "pattern", pattern: "inherited" })];
  for (const rule of malformed) {
    assert.throws(() => rulesToCsv([pattern("first"), rule]), { code: "csv_invalid_data" });
  }
  for (const rules of [null, undefined, {}, "not an array", new Array(1)]) {
    assert.throws(() => rulesToCsv(rules), { code: "csv_invalid_data" });
  }
  assert.throws(() => rulesToCsv([pattern("not v1")], { schemaVersion: 1 }), { code: "csv_invalid_data" });
  assert.throws(() => rulesToCsv([], { schemaVersion: 3 }), { code: "csv_invalid_data" });
  for (const layer of [null, {}, { rules: null }]) {
    assert.throws(() => draftZoneCsv({ layer: () => layer }, "neverSave"), { code: "csv_invalid_data" });
  }
});

for (const [label, rule, reason] of [
  ["unknown kind", { kind: "SECRET_UNKNOWN_KIND", pattern: "SECRET_PATTERN" }, /未知のルール型/],
  ["malformed data", { kind: "pattern", pattern: 123, extra: "SECRET_PATTERN" }, /内部データ形式が不正/],
]) {
  test(`${label} displays a safe refusal, downloads nothing, and keeps both states`, async () => {
    const downloads = [];
    const ui = await openPage({}, { download: (snapshot) => downloads.push(snapshot) });
    const originalLayer = ui.page.draft.layer;
    // Simulate corruption after loading: invalid Host replies are already rejected by readActive.
    ui.page.draft.layer = (key) => key === "neverSave"
      ? { ...originalLayer(key), rules: [pattern("first"), rule] } : originalLayer(key);
    const before = { draft: ui.page.draft.policy, active: structuredClone(ui.page.view.active),
      version: ui.page.draft.version, dirty: ui.page.draft.dirty(), calls: structuredClone(ui.host.requests) };
    click(ui.$("never-save-export"));
    assert.equal(downloads.length, 0);
    const status = ui.$("never-save-export-status");
    assert.equal(status.className, "error-text");
    assert.match(status.textContent, reason);
    assert.doesNotMatch(status.textContent, /SECRET/);
    assert.deepEqual({ draft: ui.page.draft.policy, active: ui.page.view.active,
      version: ui.page.draft.version, dirty: ui.page.draft.dirty(), calls: ui.host.requests }, before);
    click(ui.$("save-but-no-mcp-export"));
    assert.equal(downloads.length, 1, "the other zone remains independent");
    assert.equal(downloads[0].filename, "memocampus-private-zone.csv");
  });
}

test("serialization leaves Draft / Active / ETag / version / dirty unchanged", () => {
  const policy = v2({ neverSave: [pattern("a"), regex("["), { kind: "domainWildcard", pattern: "bad @" }, { kind: "exact", pattern: "" }], saveButNoMcp: [pattern("private")] });
  const active = readActive({ policy, etag: "original", revision: 7, schemaVersion: 2, engineVersion: 2 });
  const draft = createDraft(active, { editable: true });
  draft.appendRule("neverSave", pattern("not applied"));
  const before = { policy: draft.policy, active: structuredClone(active), version: draft.version, dirty: draft.dirty() };
  draftZoneCsv(draft, "neverSave"); draftZoneCsv(draft, "saveButNoMcp");
  assert.deepEqual({ policy: draft.policy, active, version: draft.version, dirty: draft.dirty() }, before);
});

test("zone buttons export unapplied Draft without Host calls, storage, logging, or state changes", async () => {
  await guarded(async () => {
    const downloads = [];
    const ui = await openPage({}, { download: (snapshot) => downloads.push(snapshot) });
    await addRule(ui, "never-save", regex("invalid[")); // fake checker says match; export must not validate.
    await addRule(ui, "never-save", { kind: "domainWildcard", pattern: "*.example.test" });
    await addRule(ui, "save-but-no-mcp", { kind: "exact", pattern: "https://private.test/path?q=1,2" });
    const { page, host, $ } = ui;
    const before = { draft: page.draft.policy, active: structuredClone(page.view.active), version: page.draft.version, dirty: page.draft.dirty(), calls: structuredClone(host.requests) };
    click($("never-save-export")); click($("save-but-no-mcp-export"));
    assert.equal(downloads.length, 2);
    assert.equal(downloads[0].filename, "memocampus-blind-zone.csv");
    assert.deepEqual(parse(downloads[0].csv).slice(1), [["pattern", "bank.example"], ["regex", "^https://a\\.example/"], ["regex", "invalid["], ["domainWildcard", "*.example.test"]]);
    assert.equal(downloads[1].filename, "memocampus-private-zone.csv");
    assert.deepEqual(parse(downloads[1].csv).slice(1), [["pattern", "wiki.example/private"], ["exact", "https://private.test/path?q=1,2"]]);
    assert.deepEqual({ draft: page.draft.policy, active: page.view.active, version: page.draft.version, dirty: page.draft.dirty(), calls: host.requests }, before);
    assert.equal(host.writes, 0);
    assert.equal($("never-save-export-status").hidden, false);
    assert.match($("never-save-export-status").textContent, /要求/);
  });
});

test("download failure is generic, per-zone, and keeps Draft and Active intact", async () => {
  const ui = await openPage({}, { download() { throw new Error("SECRET_RULE_TEXT"); } });
  const before = { draft: ui.page.draft.policy, active: structuredClone(ui.page.view.active), calls: ui.host.requests.length };
  click(ui.$("save-but-no-mcp-export"));
  const status = ui.$("save-but-no-mcp-export-status");
  assert.equal(status.hidden, false);
  assert.equal(status.className, "error-text");
  assert.match(status.textContent, /開始できません/);
  assert.doesNotMatch(status.textContent, /SECRET_RULE_TEXT/);
  assert.equal(ui.$("never-save-export-status").hidden, true);
  assert.deepEqual({ draft: ui.page.draft.policy, active: ui.page.view.active, calls: ui.host.requests.length }, before);
});

test("unreflected checker input is not exported and a stopped Host does not block export", async () => {
  const downloads = [];
  const ui = await openPage({}, { download: (snapshot) => downloads.push(snapshot) });
  type(ui.$("checker-rule"), "not-in-draft");
  for (const name of ["policy_status", "policy_get", "policy_validate", "policy_apply"]) {
    ui.host.answers[name] = () => { throw new Error("Host must not be contacted"); };
  }
  const calls = ui.host.requests.length;
  click(ui.$("never-save-export"));
  assert.equal(downloads.length, 1);
  assert.doesNotMatch(downloads[0].csv, /not-in-draft/);
  assert.equal(ui.host.requests.length, calls);
});

test("Export does not invalidate pending validation or change its result", async () => {
  const host = fakeHost();
  const release = host.hold("policy_validate");
  const downloads = [];
  const ui = await openPage(host, { download: (snapshot) => downloads.push(snapshot) });
  click(ui.$("validate"));
  assert.equal(ui.page.busy, "validating");
  const version = ui.page.draft.version;
  click(ui.$("never-save-export"));
  assert.equal(downloads.length, 1);
  assert.equal(ui.page.busy, "validating");
  assert.equal(ui.page.draft.version, version);
  release(); await flush();
  assert.equal(ui.page.busy, null);
  assert.match(ui.$("validation-status").textContent, /適用できます/);
});

test("read-only v1 Draft can be exported without migration or Host writes", async () => {
  const downloads = [];
  const ui = await openPage({ policy: v2({ neverSave: ["legacy.example"] }, { schemaVersion: 1, engineVersion: 1 }) }, { download: (snapshot) => downloads.push(snapshot) });
  assert.equal(ui.page.draft.editable, false);
  assert.equal(ui.$("never-save-export").disabled, false);
  const calls = ui.host.requests.length;
  click(ui.$("never-save-export"));
  assert.equal(downloads[0].csv, "kind,pattern\r\npattern,legacy.example\r\n");
  assert.equal(ui.page.view.active.schemaVersion, 1);
  assert.equal(ui.host.requests.length, calls);
  assert.equal(ui.host.writes, 0);
});

test("all known kinds export in one zone while the other zone remains independent", async () => {
  const downloads = [];
  const ui = await openPage({ policy: v2({ neverSave: [pattern("a"), { kind: "exact", pattern: "https://example.test/" }, regex("["), { kind: "domainWildcard", pattern: "*.example.test" }] }) }, { download: (snapshot) => downloads.push(snapshot) });
  click(ui.$("never-save-export"));
  assert.equal(downloads.length, 1);
  assert.equal(downloads[0].csv, "kind,pattern\r\npattern,a\r\nexact,https://example.test/\r\nregex,[\r\ndomainWildcard,*.example.test\r\n");
  click(ui.$("save-but-no-mcp-export"));
  assert.equal(downloads.length, 2);
  assert.equal(downloads[1].csv, "kind,pattern\r\n");
});

test("no Draft means disabled export buttons and no download", async () => {
  const downloads = [];
  const ui = await openPage({ send: async (message) => ({ ok: true, type: "policy_result", requestId: message.requestId, state: "invalid" }) }, { download: (snapshot) => downloads.push(snapshot) });
  for (const id of ["never-save", "save-but-no-mcp"]) {
    assert.equal(ui.$(`${id}-export`).disabled, true);
    click(ui.$(`${id}-export`));
  }
  assert.equal(downloads.length, 0);
});

test("UI explains Draft-only export and spreadsheet formula injection without altering values", () => {
  for (const id of ["never-save-export", "save-but-no-mcp-export", "csv-safety-note"]) assert.ok(html.includes(`id="${id}"`));
  assert.match(html, /未適用の変更・不正な文字列もそのまま/);
  assert.match(html, /kind,pattern形式/);
  assert.doesNotMatch(html, /種類を保持できないため出力できません/);
  assert.match(html, /URLチェッカーの未反映入力/);
  assert.match(html, /Excel等で開くと数式として実行される危険/);
});

function browserStandIn(failure = null) {
  const events = [], timers = [];
  let blob;
  const anchor = { remove() { events.push("remove"); }, click() { events.push("click"); if (failure === "click") throw new Error("private browser details"); } };
  const window = {
    Blob,
    URL: { createObjectURL(value) { blob = value; events.push("create"); return "blob:test"; }, revokeObjectURL(url) { assert.equal(url, "blob:test"); events.push("revoke"); } },
    setTimeout(fn, ms) { timers.push({ fn, ms }); },
  };
  const document = { body: { append() { events.push("append"); if (failure === "append") throw new Error("append failed"); } }, createElement(tag) { assert.equal(tag, "a"); return anchor; } };
  return { window, document, events, timers, anchor, get blob() { return blob; } };
}

test("native download uses UTF-8 Blob and anchor; removes anchor and defers URL revocation", async () => {
  const browser = browserStandIn();
  const csv = rulesToCsv([pattern("日本語\r\n🧠")]);
  downloadCsv({ ...browser, csv, filename: "memocampus-blind-zone.csv" });
  assert.equal(browser.blob.type, "text/csv;charset=utf-8");
  assert.equal(await browser.blob.text(), csv);
  assert.deepEqual(Buffer.from(await browser.blob.arrayBuffer()), Buffer.from(csv));
  assert.equal(browser.anchor.download, "memocampus-blind-zone.csv");
  assert.equal(browser.anchor.href, "blob:test");
  assert.deepEqual(browser.events, ["create", "append", "click", "remove"]);
  assert.equal(browser.timers.length, 1);
  assert.equal(browser.timers[0].ms, 60_000);
  browser.timers[0].fn();
  assert.deepEqual(browser.events, ["create", "append", "click", "remove", "revoke"]);
});

for (const failure of ["append", "click"]) {
  test(`download ${failure} failure cleans anchor and URL, and propagates for UI handling`, () => {
    const browser = browserStandIn(failure);
    assert.throws(() => downloadCsv({ ...browser, csv: "kind,pattern\r\n", filename: "test.csv" }));
    assert.deepEqual(browser.events.slice(-2), ["revoke", "remove"]);
    assert.equal(browser.timers.length, 0);
  });
}

test("Blob and object URL creation failures propagate before any anchor or timer exists", () => {
  for (const failure of ["blob", "url"]) {
    const browser = browserStandIn();
    if (failure === "blob") browser.window.Blob = class { constructor() { throw new Error("Blob unavailable"); } };
    else browser.window.URL.createObjectURL = () => { throw new Error("URL unavailable"); };
    assert.throws(() => downloadCsv({ ...browser, csv: "kind,pattern\r\n", filename: "test.csv" }));
    assert.deepEqual(browser.events, []);
    assert.deepEqual(browser.timers, []);
  }
});
