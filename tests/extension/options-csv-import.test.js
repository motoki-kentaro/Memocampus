import { test } from "node:test";
import assert from "node:assert/strict";
import { SearchError } from "../../extension/search-client.js";
import { rulesToCsv } from "../../extension/policy-csv.js";
import { fakeHost, openPage, v2, pattern, regex, click, flush, check, guarded, html } from "./options-fixtures.js";

const fileOf = (text) => { const bytes = new TextEncoder().encode(text); return { size: bytes.length, arrayBuffer: async () => bytes.buffer }; };
const snapshot = (ui) => ({ draft: ui.page.draft.policy, version: ui.page.draft.version, dirty: ui.page.draft.dirty(), active: structuredClone(ui.page.view.active) });
const deferred = () => { let release; const promise = new Promise((resolve) => { release = resolve; }); return { promise, release }; };
async function until(checking) {
  for (let i = 0; i < 200; i++) { if (checking()) return; await flush(); }
  assert.fail("UI did not settle");
}
function begin(ui, zone = "never-save", file = fileOf("kind,pattern\npattern,new.example")) {
  const input = ui.$(`${zone}-import-file`);
  input.click = () => { input.opened = (input.opened ?? 0) + 1; };
  click(ui.$(`${zone}-import`));
  input.files = file ? [file] : [];
  input.dispatch("change");
}
async function importing(ui, zone, file) {
  begin(ui, zone, file);
  await until(() => ui.page.busy === null);
}

for (const [zone, layer, other] of [["never-save", "neverSave", "saveButNoMcp"], ["save-but-no-mcp", "saveButNoMcp", "neverSave"]]) {
  test(`${zone} adds the entire batch once, keeping duplicates, order, Active and the other zone`, async () => {
    const ui = await openPage();
    const before = snapshot(ui);
    const rules = [pattern("duplicate.example"), regex(""), { kind: "domainWildcard", pattern: "*.example.test" },
      { kind: "exact", pattern: "https://exact.test/a?q=1,2" }, pattern("duplicate.example")];
    await importing(ui, zone, fileOf(rulesToCsv(rules)));
    assert.deepEqual(ui.page.draft.layer(layer).rules, [...before.draft.layers[layer].rules, ...rules]);
    assert.deepEqual(ui.page.draft.layer(other), before.draft.layers[other]);
    assert.deepEqual(ui.page.view.active, before.active);
    assert.equal(ui.page.draft.version, before.version + 1); assert.equal(ui.page.draft.dirty(), true);
    assert.equal(ui.host.writes, 0); assert.equal(ui.host.ofType("policy_validate").length, 1);
    assert.match(ui.$(`${zone}-import-status`).textContent, /5件.*一括追加/);
    assert.equal(ui.host.ofType("policy_apply").length, 0);
    assert.equal(ui.host.allTypes().some((t) => /vault_|publication|policy_start/.test(t)), false);
  });
}

for (const [label, file, code] of [
  ["syntax", fileOf('kind,pattern\npattern,first\nregex,"SECRET'), /CSV 3行目.*引用符/],
  ["kind", fileOf("kind,pattern\npattern,first\n__proto__,SECRET"), /CSV 3行目.*未知/],
  ["columns", fileOf("kind,pattern\npattern,x,neverSave"), /CSV 2行目.*列数/],
  ["header", fileOf("kind,pattern,zone\npattern,x,neverSave"), /CSV 1行目/],
  ["read", { size: 1, arrayBuffer() { throw new Error("SECRET_PATH"); } }, /読み取れません/],
  ["size", { size: 1_048_577, arrayBuffer() { throw new Error("must not read"); } }, /上限1 MiB/],
  ["UTF8", { size: 1, arrayBuffer: async () => Uint8Array.of(255).buffer }, /UTF-8/],
]) {
  test(`${label} failure changes nothing, never sends any candidate, and does not expose contents`, async () => {
    const ui = await openPage(); const before = snapshot(ui); const calls = ui.host.requests.length;
    await importing(ui, "never-save", file);
    assert.deepEqual(snapshot(ui), before); assert.equal(ui.host.requests.length, calls);
    const note = ui.$("never-save-import-status");
    assert.equal(note.className, "error-text"); assert.match(note.textContent, code); assert.doesNotMatch(note.textContent, /SECRET|Error:|stack/);
  });
}

test("Host rule failure uses CSV physical starting line, not candidate index; no rollback or secret leak", async () => {
  const ui = await openPage(); const before = snapshot(ui);
  await importing(ui, "never-save", fileOf('kind,pattern\nregex,"ok\nmultiline"\nregex,badSECRET'));
  assert.deepEqual(snapshot(ui), before);
  assert.match(ui.$("never-save-import-status").textContent, /CSV 4行目.*Policyルール検証/);
  assert.doesNotMatch(ui.$("never-save-import-status").textContent, /badSECRET/);
  assert.equal(ui.host.ofType("policy_validate").length, 1); assert.equal(ui.host.writes, 0);
});
test("invalid existing Draft also prevents the entire candidate append", async () => {
  const ui = await openPage({ policy: v2({ saveButNoMcp: [regex("bad-existing")] }) });
  const before = snapshot(ui);
  await importing(ui, "never-save");
  assert.deepEqual(snapshot(ui), before);
  assert.match(ui.$("never-save-import-status").textContent, /候補全体/);
  assert.doesNotMatch(ui.$("never-save-import-status").textContent, /CSV \d+行目/);
});
for (const failure of ["host_unavailable", "timeout", "invalid_response"]) {
  test(`Host ${failure} refuses without partial mutation`, async () => {
    const host = fakeHost(); host.answers.policy_validate = () => { throw new SearchError(failure, "SECRET"); };
    const ui = await openPage(host); const before = snapshot(ui);
    await importing(ui, "never-save");
    assert.deepEqual(snapshot(ui), before);
    assert.match(ui.$("never-save-import-status").textContent, /Native Hostとの通信/);
    assert.doesNotMatch(ui.$("never-save-import-status").textContent, /SECRET/);
  });
}
test("mismatched Host validation versions cannot authorize a batch", async () => {
  const host = fakeHost(); host.answers.policy_validate = () => ({ ok: true, schemaVersion: 1, engineVersion: 1 });
  const ui = await openPage(host); const before = snapshot(ui);
  await importing(ui, "never-save"); assert.deepEqual(snapshot(ui), before);
  assert.match(ui.$("never-save-import-status").textContent, /Native Host/);
});
test("cancel / no file / header-only have no errors, Host calls, or state mutation", async () => {
  for (const cancel of ["cancel", "empty-selection", "header"]) {
    const ui = await openPage(); const before = snapshot(ui); const calls = ui.host.requests.length;
    if (cancel === "cancel") {
      click(ui.controls("never-save", 0).edit);
      const editing = ui.$("checker-edit").textContent;
      const input = ui.$("never-save-import-file"); input.click = () => {};
      click(ui.$("never-save-import")); input.dispatch("cancel");
      assert.equal(ui.$("checker-edit").textContent, editing, "cancel also preserves checker edit context");
    } else await importing(ui, "never-save", cancel === "header" ? fileOf("kind,pattern\r\n") : null);
    assert.deepEqual(snapshot(ui), before); assert.equal(ui.host.requests.length, calls); assert.equal(ui.page.busy, null);
    assert.notEqual(ui.$("never-save-import-status").className, "error-text");
  }
});
test("file picker failure is generic and unlocks the unchanged Draft", async () => {
  const ui = await openPage(); const before = snapshot(ui);
  ui.$("never-save-import-file").click = () => { throw new Error("SECRET_PATH"); };
  click(ui.$("never-save-import")); assert.deepEqual(snapshot(ui), before); assert.equal(ui.page.busy, null);
  assert.match(ui.$("never-save-import-status").textContent, /ファイル選択を開始できません/);
});
for (const stage of ["file", "validate", "status", "get"]) {
  test(`a direct model edit during ${stage} wait rejects stale results while preserving that edit`, async () => {
    const ui = await openPage(); const waiting = deferred();
    let file = fileOf("kind,pattern\npattern,new.example"), release;
    const initial = snapshot(ui);
    if (stage === "file") file = { size: file.size, arrayBuffer: () => waiting.promise };
    else release = ui.host.hold({ validate: "policy_validate", status: "policy_status", get: "policy_get" }[stage]);
    const startCalls = ui.host.requests.length;
    begin(ui, "never-save", file);
    if (stage !== "file") await until(() => ui.host.requests.slice(startCalls).some((r) => r.type === ({ validate: "policy_validate", status: "policy_status", get: "policy_get" }[stage])));
    ui.page.draft.setCategory("webmail", false); // Bypass disabled UI to prove the version guard.
    const edited = snapshot(ui);
    if (stage === "file") waiting.release(await fileOf("kind,pattern\npattern,new.example").arrayBuffer()); else release();
    await until(() => ui.page.busy === null);
    assert.deepEqual(snapshot(ui), edited); assert.equal(edited.version, initial.version + 1);
    assert.match(ui.$("never-save-import-status").textContent, /状態が変わりました/);
  });
}
test("concurrent Imports, target switching, duplicate change, edit/apply/reload clicks are guarded", async () => {
  const ui = await openPage(); const release = ui.host.hold("policy_validate");
  const initial = snapshot(ui); begin(ui);
  await until(() => ui.host.ofType("policy_validate").length === 1);
  for (const id of ["save-but-no-mcp-import", "never-save-import", "apply", "reload", "validate", "discard"]) {
    assert.equal(ui.$(id).disabled, true, id); click(ui.$(id));
  }
  check(ui.$("category-webmail"), false);
  ui.$("save-but-no-mcp-import-file").files = [fileOf("kind,pattern\npattern,WRONG_ZONE")];
  ui.$("save-but-no-mcp-import-file").dispatch("change");
  ui.$("never-save-import-file").dispatch("change");
  await ui.page.reload();
  assert.deepEqual(snapshot(ui), initial);
  release(); await until(() => ui.page.busy === null);
  assert.equal(ui.host.ofType("policy_validate").length, 1);
  assert.equal(ui.page.draft.version, 1);
  assert.equal(ui.page.draft.layer("neverSave").rules.at(-1).pattern, "new.example");
  assert.deepEqual(ui.page.draft.layer("saveButNoMcp"), initial.draft.layers.saveButNoMcp);
});
test("remote ETag changes abort, keep local Draft and baseline Active, and require reload", async () => {
  const ui = await openPage(); const before = snapshot(ui); const release = ui.host.hold("policy_validate");
  begin(ui); await until(() => ui.host.ofType("policy_validate").length === 1);
  ui.host.commit(v2({ saveButNoMcp: [pattern("external-change")] })); release();
  await until(() => ui.page.busy === null);
  assert.deepEqual(snapshot(ui), before);
  assert.match(ui.$("never-save-import-status").textContent, /ETagが競合/);
  assert.equal(ui.page.blocked, "conflict"); assert.equal(ui.host.ofType("policy_apply").length, 0);
});
test("unreadable / damaged / uninitialized Active during final check never permits commit", async () => {
  for (const state of ["invalid", "uninitialized", "host_unavailable"]) {
    const ui = await openPage(); const before = snapshot(ui);
    ui.host.answers.policy_status = () => {
      if (state === "host_unavailable") throw new SearchError(state);
      return { ok: true, state };
    };
    await importing(ui, "never-save"); assert.deepEqual(snapshot(ui), before);
    assert.match(ui.$("never-save-import-status").textContent, /Active Policyを確認できない/);
  }
});
test("read-only v1, first-run and missing policies cannot import or start recording implicitly", async () => {
  const readonly = await openPage({ policy: v2({}, { schemaVersion: 1, engineVersion: 1 }) });
  for (const ui of [readonly]) {
    const before = snapshot(ui); begin(ui); await flush();
    assert.equal(ui.$("never-save-import").disabled, true); assert.deepEqual(snapshot(ui), before);
  }
  for (const state of ["uninitialized", "invalid", "unsupported", "unavailable"]) {
    const host = fakeHost(); host.answers.policy_status = () => ({ ok: true, state });
    host.answers.policy_preset = () => ({ ok: true, presetId: "default", schemaVersion: 2, engineVersion: 2, policy: v2() });
    const ui = await openPage(host); const calls = host.requests.length;
    begin(ui); await flush();
    assert.equal(ui.$("never-save-import").disabled, true); assert.equal(host.requests.length, calls);
    assert.equal(host.ofType("policy_start").length, 0);
  }
});
test("Import is local / read-only to Host until explicit Apply; Blind protections stay and no publication approval changes", async () => {
  await guarded(async () => {
    const ui = await openPage(); const original = structuredClone(ui.host.policy);
    await importing(ui, "save-but-no-mcp", fileOf("kind,pattern\npattern,private.example\npattern,private.example"));
    assert.deepEqual(ui.host.policy, original);
    assert.deepEqual(ui.page.draft.layer("neverSave"), original.layers.neverSave);
    assert.equal(ui.host.allTypes().some((t) => /vault_|policy_start|policy_domain_approve/.test(t)), false);
    click(ui.$("apply")); await until(() => ui.page.busy === null);
    assert.equal(ui.host.writes, 1); assert.equal(ui.host.ofType("policy_apply").length, 1);
    assert.deepEqual(ui.host.policy.layers.neverSave, original.layers.neverSave);
    assert.deepEqual(ui.host.policy.layers.saveButNoMcp.rules.slice(-2), [pattern("private.example"), pattern("private.example")]);
  });
});
test("import controls have separate explanations of atomic Draft-only append", () => {
  for (const zone of ["never-save", "save-but-no-mcp"]) {
    assert.ok(html.includes(`id="${zone}-import"`)); assert.ok(html.includes(`id="${zone}-import-file" type="file"`));
  }
  assert.match(html, /重複は自動除去せず/); assert.match(html, /不正行があれば全件取り消し/);
  assert.match(html, /既存のローカル履歴も自動削除しません/);
});
