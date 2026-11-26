// Issue #93 S3-1: scripts/ci/ci-gate.mjs, the `CI gate` job. Every combination of event, job results and the
// `changes` decision is tried; the gate must pass in exactly the three states below and fail in every other one,
// whatever GitHub's `if:` evaluation did before it.
import { test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { gate } from "../../scripts/ci/ci-gate.mjs";

const script = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../scripts/ci/ci-gate.mjs");
const needs = (changes, decision, javascript, rust) => ({
  changes: { result: changes, outputs: decision === undefined ? {} : { rust: decision, reason: "r" } },
  javascript: { result: javascript, outputs: {} },
  rust: { result: rust, outputs: {} },
});

/** The specification: the only passing states. */
const passes = (event, changes, decision, javascript, rust) =>
  changes === "success" && javascript === "success" &&
  ((event === "pull_request" && decision === "false" && rust === "skipped") ||
    (["pull_request", "push", "workflow_dispatch"].includes(event) && decision === "true" && rust === "success"));

test("every combination of event, results and decision: PASS exactly in the specified states", () => {
  const events = ["pull_request", "push", "workflow_dispatch", "pull_request_target", "schedule", "", undefined];
  const results = ["success", "failure", "cancelled", "skipped", "", "neutral", undefined, null];
  const decisions = ["true", "false", "", "TRUE", "False", "1", "0", "yes", undefined, true, false, null];
  let count = 0, passed = 0;
  for (const event of events) for (const c of results) for (const d of decisions) for (const j of results) for (const r of results) {
    const got = gate({ event, needs: needs(c, d, j, r) });
    const want = passes(event, c, d, j, r);
    assert.equal(got.pass, want, `${event} changes=${c} rust-decision=${String(d)} js=${j} rust=${r}: ${got.reasons.join("; ")}`);
    if (!got.pass) assert.ok(got.reasons.length > 0);
    count++; if (want) passed++;
  }
  assert.equal(passed, 4); // pull_request x2, push, workflow_dispatch
  assert.ok(count > 30000);
});

test("named states from the design: required Rust skipped, Rust run when not required, failures, cancellations", () => {
  const cases = [
    ["pull_request", needs("success", "false", "success", "skipped"), true, "docs-only PR"],
    ["pull_request", needs("success", "true", "success", "success"), true, "Rust PR"],
    ["push", needs("success", "true", "success", "success"), true, "main push"],
    ["workflow_dispatch", needs("success", "true", "success", "success"), true, "manual run"],
    ["pull_request", needs("success", "true", "success", "skipped"), false, "Rust required but skipped"],
    ["push", needs("success", "true", "success", "skipped"), false, "push with Rust skipped"],
    ["push", needs("success", "false", "success", "skipped"), false, "push that decided rust=false"],
    ["pull_request", needs("success", "false", "success", "success"), false, "Rust ran although not required"],
    ["pull_request", needs("success", "false", "success", "failure"), false, "Rust failed although not required"],
    ["pull_request", needs("success", "true", "success", "failure"), false, "Rust failed"],
    ["pull_request", needs("success", "true", "success", "cancelled"), false, "Rust cancelled"],
    ["pull_request", needs("success", "false", "failure", "skipped"), false, "JavaScript failed"],
    ["pull_request", needs("success", "false", "cancelled", "skipped"), false, "JavaScript cancelled"],
    ["pull_request", needs("success", "false", "skipped", "skipped"), false, "JavaScript skipped"],
    ["pull_request", needs("failure", undefined, "success", "success"), false, "changes failed (Rust ran anyway)"],
    ["pull_request", needs("failure", undefined, "success", "skipped"), false, "changes failed"],
    ["pull_request", needs("cancelled", undefined, "success", "skipped"), false, "changes cancelled"],
    ["pull_request", needs("skipped", undefined, "success", "skipped"), false, "changes skipped"],
    ["pull_request", needs("success", undefined, "success", "skipped"), false, "decision missing"],
    ["pull_request", needs("success", "", "success", "success"), false, "decision empty"],
    ["pull_request", needs("success", "maybe", "success", "success"), false, "decision invalid"],
  ];
  for (const [event, n, want, name] of cases) assert.equal(gate({ event, needs: n }).pass, want, name);
});

test("a malformed needs object fails: missing or extra jobs, missing outputs, wrong types", () => {
  const ok = () => needs("success", "true", "success", "success");
  const variants = {
    "null": null, "array": [], "string": "x", "empty": {},
    "no rust job": (({ rust, ...n }) => n)(ok()),
    "no javascript job": (({ javascript, ...n }) => n)(ok()),
    "no changes job": (({ changes, ...n }) => n)(ok()),
    "extra job": { ...ok(), other: { result: "success", outputs: {} } },
    "no outputs": { ...ok(), changes: { result: "success" } },
    "outputs not an object": { ...ok(), changes: { result: "success", outputs: "rust=true" } },
    "job not an object": { ...ok(), rust: "success" },
  };
  for (const [name, n] of Object.entries(variants)) assert.equal(gate({ event: "push", needs: n }).pass, false, name);
  assert.equal(gate({ event: "push", needs: ok() }).pass, true);
});

test("the script exits 0 only on PASS, and fails on unreadable input", () => {
  const run = (env) => spawnSync(process.execPath, [script], { env: { ...process.env, GITHUB_STEP_SUMMARY: "", ...env }, encoding: "utf8" });
  const pass = run({ EVENT_NAME: "pull_request", NEEDS: JSON.stringify(needs("success", "false", "success", "skipped")) });
  assert.equal(pass.status, 0, pass.stdout);
  assert.match(pass.stdout, /CI gate: PASS/);
  const fail = run({ EVENT_NAME: "pull_request", NEEDS: JSON.stringify(needs("success", "true", "success", "skipped")) });
  assert.equal(fail.status, 1);
  assert.match(fail.stdout, /CI gate: FAIL[\s\S]*although Rust was required/);
  for (const env of [{ EVENT_NAME: "push", NEEDS: "{not json" }, { EVENT_NAME: "push", NEEDS: "" }, { EVENT_NAME: "push", NEEDS: "null" }, { EVENT_NAME: "push" }]) {
    const p = run(env);
    assert.equal(p.status, 1, JSON.stringify(env));
  }
});
