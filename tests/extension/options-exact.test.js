// Exact URL rules on the Privacy settings page (Issue #43, Slice 2): shown, added, changed, removed
// and applied like the other kinds, through the URL checker; the text is the URL as written, never
// escaped. A fake host for the page's own behaviour, and (opt-in) the real host for what it stores.
import { test } from "node:test";
import assert from "node:assert/strict";
import path from "node:path";
import { RULE_KINDS, matchState } from "../../extension/options-state.js";
import { addRule, chooseKind, click, editRule, flush, html, htmlText, openPage, pattern, regex, type, v2 } from "./options-fixtures.js";

const exact = (url) => ({ kind: "exact", pattern: url });
const URL_A = "https://forum.example/thread/123?page=3#c1";
const URL_B = "https://forum.example/thread/123?page=4";

test("Exact URL is the fourth kind of the checker, and the help says what it matches", () => {
  assert.deepEqual(RULE_KINDS, ["pattern", "regex", "domainWildcard", "exact"]);
  assert.match(html, /<input id="checker-kind-exact" type="radio" name="checker-kind" value="exact" disabled><label for="checker-kind-exact">Exact URL<\/label>/);
  assert.match(htmlText("checker-help"), /Exact URLは、スキーム（httpsなど）から始まるURL全体1つだけに一致します/);
  assert.match(htmlText("checker-help"), /末尾の \/ やクエリが違えば別のURLです/);
  assert.equal(matchState({ ok: false, code: "invalid_exact_url" }), "rule_error", "a bad exact URL is a problem of the rule");
});

test("an exact rule is shown as the URL, unescaped, beside the other kinds", async () => {
  const policy = v2({ neverSave: [pattern("bank.example"), exact(URL_A)], saveButNoMcp: [regex("^https://a\\.example/"), exact(URL_B)] });
  const { controls, ruleTexts } = await openPage({ policy });
  assert.deepEqual(ruleTexts("never-save"), ["bank.example", URL_A]);
  assert.deepEqual(ruleTexts("save-but-no-mcp"), ["^https://a\\.example/", URL_B]);
  assert.equal(controls("never-save", 1).kind, "Exact URL");
  assert.equal(controls("save-but-no-mcp", 1).kind, "Exact URL");
  assert.equal(controls("save-but-no-mcp", 0).kind, "Regex", "the other kinds are unchanged");
});

test("an exact rule is added, changed and removed through the checker, and applied", async () => {
  const opened = await openPage({ policy: v2({ neverSave: [pattern("bank.example")] }) });
  const { page, host, $, controls, ruleTexts } = opened;
  await addRule(opened, "save-but-no-mcp", exact(URL_A), URL_A);
  const asked = host.ofType("policy_test_rule").at(-1);
  assert.deepEqual([asked.rule, asked.url], [exact(URL_A), URL_A], "the host judges the rule as written");
  assert.deepEqual(page.draft.layer("saveButNoMcp").rules, [exact(URL_A)]);
  assert.equal(controls("save-but-no-mcp", 0).kind, "Exact URL");
  // Changed to another URL, then to a pattern and back: kind and text as the checker holds them.
  await editRule(opened, "save-but-no-mcp", 0, exact(URL_B), URL_B);
  assert.deepEqual(page.draft.layer("saveButNoMcp").rules, [exact(URL_B)]);
  await addRule(opened, "never-save", exact(URL_A), URL_A);
  click($("apply"));
  await flush();
  const sent = host.ofType("policy_apply")[0].policy.layers;
  assert.deepEqual(sent.neverSave.rules, [pattern("bank.example"), exact(URL_A)]);
  assert.deepEqual(sent.saveButNoMcp.rules, [exact(URL_B)]);
  assert.equal($("apply-status").textContent, "適用しました（Revision 8）。");
  assert.deepEqual(ruleTexts("never-save"), ["bank.example", URL_A], "read again from the host");
  // Removing one asks for confirmation: it may protect less.
  click(controls("never-save", 1).remove);
  click($("apply"));
  assert.equal($("relax-confirm").hidden, false);
  click($("relax-confirm-yes"));
  await flush();
  assert.deepEqual(host.policy.layers.neverSave.rules, [pattern("bank.example")]);
  assert.deepEqual(host.policy.layers.saveButNoMcp.rules, [exact(URL_B)]);
});

test("a bad exact URL is refused with its own message, on its own row", async () => {
  const opened = await openPage({ policy: v2({ neverSave: [exact("bad")] }) });
  const { $, controls, host } = opened;
  host.answers.policy_test_rule = () => ({ ok: false, error: "invalid_exact_url" });
  click($("validate"));
  await flush();
  assert.match(controls("never-save", 0).error.textContent, /^エラー: Exact URLには、スキーム（httpまたはhttps）から始まるURL全体を1つ書いてください/);
  // In the checker: no match, nothing can be added.
  const before = opened.page.draft.layer("saveButNoMcp").rules.length;
  await addRule(opened, "save-but-no-mcp", exact("forum.example/thread"), URL_A);
  assert.equal(opened.page.draft.layer("saveButNoMcp").rules.length, before);
  assert.equal($("checker-add-save-but-no-mcp").disabled, true);
});

// --- With the real host ---------------------------------------------------------------------------
// Opt-in (MEMOCAMPUS_TEST_HOST): the settings page's real code against the real host binary, in a
// temporary MEMOCAMPUS_APP_DIR with a fresh policy. Nothing touches the user's policy.

const HOST_BIN = process.env.MEMOCAMPUS_TEST_HOST;

test("end to end with the real host: an exact rule checked, applied, read again, decided and removed", { skip: HOST_BIN ? false : "set MEMOCAMPUS_TEST_HOST to run" }, async () => {
  const { spawn } = await import("node:child_process");
  const { mkdtempSync, rmSync } = await import("node:fs");
  const os = await import("node:os");
  const appDir = mkdtempSync(path.join(os.tmpdir(), "memocampus-exact-e2e-"));
  const env = { ...process.env, MEMOCAMPUS_APP_DIR: appDir };
  delete env.MEMOCAMPUS_DB;
  const call = (message) =>
    new Promise((resolve, reject) => {
      const child = spawn(HOST_BIN, [], { env, cwd: appDir, stdio: ["pipe", "pipe", "pipe"] });
      const chunks = [];
      child.stdout.on("data", (chunk) => chunks.push(chunk));
      child.on("error", reject);
      child.on("close", () => {
        const out = Buffer.concat(chunks);
        if (out.length < 4) return reject(new Error("no answer"));
        resolve(JSON.parse(out.subarray(4, 4 + out.readUInt32LE(0)).toString("utf8")));
      });
      const body = Buffer.from(JSON.stringify(message), "utf8");
      const length = Buffer.alloc(4);
      length.writeUInt32LE(body.length);
      child.stdin.end(Buffer.concat([length, body]));
    });
  const judge = async (url) => (await call({ type: "policy_evaluate_batch", requestId: "e", items: [{ id: "u", url }] })).items[0].decision;
  try {
    const { seedLegacyPolicy } = await import("./real-processes.js");
    seedLegacyPolicy(appDir);
    const opened = await openPage({ send: call });
    const { page, $, controls, ruleTexts } = opened;
    await settleHost();
    /** Through the checker, waiting for the real host's answer before "add". */
    const add = async (layer, rule, url) => {
      chooseKind($, rule.kind);
      type($("checker-rule"), rule.pattern);
      type($("checker-url"), url);
      click($("checker-run"));
      await settleHost();
      const button = $(`checker-add-${layer}`);
      const enabled = !button.disabled;
      click(button);
      return enabled;
    };
    // The real host refuses a rule that is no URL, and a URL of another query is no match.
    assert.equal(await add("never-save", exact("forum.example/thread/123"), URL_A), false);
    assert.match($("checker-result").textContent, /Exact URLには/);
    assert.equal(await add("never-save", exact(URL_A), URL_B), false);
    assert.equal(page.draft.layer("neverSave").rules.length, 0);
    assert.equal(await add("never-save", exact(URL_A), URL_A), true);
    assert.equal(await add("save-but-no-mcp", exact(URL_B), URL_B), true);
    click($("apply"));
    await settleHost();
    assert.match($("apply-status").textContent, /^適用しました（Revision 2）。/);
    assert.deepEqual([ruleTexts("never-save"), ruleTexts("save-but-no-mcp")], [[URL_A], [URL_B]]);
    assert.equal(controls("never-save", 0).kind, "Exact URL");
    const stored = (await call({ type: "policy_get", requestId: "g" })).policy.layers;
    assert.deepEqual([stored.neverSave.rules, stored.saveButNoMcp.rules], [[exact(URL_A)], [exact(URL_B)]]);
    // What recording and MCP are judged by.
    assert.equal(await judge(URL_A), "never_save");
    assert.equal(await judge("HTTPS://FORUM.EXAMPLE:443/thread/123?page=3#c1"), "never_save");
    assert.equal(await judge("https://forum.example/thread/123?page=3"), "allow");
    assert.equal(await judge(URL_B), "save_but_no_mcp");
    assert.equal(await judge("https://forum.example/thread/123"), "allow");
    // A fresh page shows what is stored; removing one is applied after the confirmation.
    const again = await openPage({ send: call });
    await settleHost();
    assert.deepEqual(again.ruleTexts("never-save"), [URL_A]);
    click(again.controls("never-save", 0).remove);
    click(again.$("apply"));
    click(again.$("relax-confirm-yes"));
    await settleHost();
    assert.equal(await judge(URL_A), "allow");
    assert.deepEqual((await call({ type: "policy_get", requestId: "g" })).policy.layers.saveButNoMcp.rules, [exact(URL_B)]);
  } finally {
    rmSync(appDir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  }
});

/** The real host answers in its own time: wait for every reply of a step. */
async function settleHost() {
  for (let i = 0; i < 10; i += 1) await new Promise((resolve) => setTimeout(resolve, 100));
}
