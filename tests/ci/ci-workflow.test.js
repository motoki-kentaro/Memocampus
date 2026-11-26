// Issue #93 S3-1: static checks of the jobs in .github/workflows/ci.yml that decide and gate the Windows Rust job.
//
// What this checks: `changes` runs the classifier from a depth-2 checkout with a read-only token; Rust is skipped only
// on an explicit rust=false of a pull request; JavaScript always runs; `CI gate` needs every job, runs with always()
// and runs the gate script with the results; nothing gained a write permission, pull_request_target, a path filter or
// a third-party action; S1's concurrency and the distribution workflow are unchanged.
//
// What it cannot check: how GitHub evaluates these expressions at run time. The gate does not depend on that: it
// fails on any result that does not match the decision (tests/ci/ci-gate.test.js).
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const read = (file) => readFileSync(path.join(root, file), "utf8").replace(/\r\n/g, "\n");
const ci = read(".github/workflows/ci.yml").split("\n").filter((l) => !/^\s*#/.test(l)).join("\n");

/** The text of each top-level job, by id. */
const jobs = Object.fromEntries(
  ci.slice(ci.indexOf("\njobs:\n") + 7).split(/\n(?=  [\w-]+:\n)/).filter((t) => /^  [\w-]+:\n/.test(t)).map((t) => [t.match(/^  ([\w-]+):/)[1], t]),
);
const field = (text, name, indent = 4) => (new RegExp(`\\n {${indent}}${name}: ([^\\n]+)`).exec("\n" + text) ?? [])[1];

test("the workflow has exactly the four jobs, and the gate has a stable name", () => {
  assert.deepEqual(Object.keys(jobs), ["changes", "javascript", "rust", "ci-gate"]);
  assert.equal(field(jobs["ci-gate"], "name"), "CI gate");
  assert.equal(field(jobs.rust, "name"), "Rust (stable MSVC, Windows)");
  assert.equal(field(jobs.javascript, "name"), "JavaScript (Node 24, Ubuntu)");
});

test("changes: classifier on a depth-2 checkout without credentials, outputs from the classify step", () => {
  const c = jobs.changes;
  assert.match(c, /uses: actions\/checkout@v[\d.]+\n\s+with:\n\s+persist-credentials: false\n\s+fetch-depth: 2\n/);
  assert.match(c, /\n {6}- id: classify\n {8}run: node scripts\/ci\/classify-changes\.mjs\n/);
  assert.match(c, /PR_HEAD_SHA: \$\{\{ github\.event\.pull_request\.head\.sha \}\}/);
  assert.equal(field(c, "rust", 6), "${{ steps.classify.outputs.rust }}");
  assert.equal(field(c, "reason", 6), "${{ steps.classify.outputs.reason }}");
  assert.equal(field(c, "if"), undefined, "changes always runs");
  assert.doesNotMatch(c, /continue-on-error/);
});

test("rust: needs changes; skipped only on a pull request's explicit rust=false, never on a failed or missing decision", () => {
  const r = jobs.rust;
  assert.equal(field(r, "needs"), "changes");
  assert.equal(field(r, "if"), "${{ !cancelled() && (github.event_name != 'pull_request' || needs.changes.outputs.rust != 'false') }}");
  // Only this job may be skipped by the decision; JavaScript always runs and does not wait for changes.
  assert.equal(field(jobs.javascript, "if"), undefined);
  assert.equal(field(jobs.javascript, "needs"), undefined);
  assert.equal((ci.match(/needs\.changes\.outputs\.rust/g) ?? []).length, 1);
});

test("CI gate: needs all three jobs, always() (a skipped gate would count as passed), runs the gate script on toJSON(needs)", () => {
  const g = jobs["ci-gate"];
  assert.equal(field(g, "needs"), "[changes, javascript, rust]");
  assert.equal(field(g, "if"), "always()");
  assert.match(g, /\n {6}- run: node scripts\/ci\/ci-gate\.mjs\n {8}env:\n {10}EVENT_NAME: \$\{\{ github\.event_name \}\}\n {10}NEEDS: \$\{\{ toJSON\(needs\) \}\}\n/);
  assert.match(g, /persist-credentials: false/);
  assert.doesNotMatch(g, /continue-on-error/);
  assert.equal(field(g, "timeout-minutes"), "5");
});

test("no new privileges or triggers: read-only token, no pull_request_target, no path filter, only pinned first-party actions", () => {
  assert.match(ci, /\non:\n  pull_request:\n    branches: \[main\]\n  push:\n    branches: \[main\]\n  workflow_dispatch:\n/);
  assert.doesNotMatch(ci, /pull_request_target|paths(-ignore)?:/);
  assert.match(ci, /\npermissions:\n  contents: read\n/);
  assert.equal((ci.match(/^\s*permissions:/gm) ?? []).length, 1);
  const actions = [...new Set([...ci.matchAll(/uses: ([\w./-]+)@/g)].map((m) => m[1]))].sort();
  assert.deepEqual(actions, ["actions/cache/restore", "actions/cache/save", "actions/checkout", "actions/setup-node"]);
  for (const m of ci.matchAll(/^\s+- uses: actions\/checkout@[^\n]+\n\s+with:\n\s+persist-credentials: (\w+)/gm)) assert.equal(m[1], "false");
  assert.equal((ci.match(/uses: actions\/checkout@/g) ?? []).length, 4);
});

test("S1 concurrency and S2 cache stay as they were", () => {
  assert.match(ci, /\nconcurrency:\n  group: ci-\$\{\{ github\.event_name == 'pull_request' && format\('pr-\{0\}', github\.event\.pull_request\.number\) \|\| format\('run-\{0\}', github\.run_id\) \}\}\n  cancel-in-progress: \$\{\{ github\.event_name == 'pull_request' \}\}\n/);
  const saveIf = "success() && github.event_name == 'push' && github.ref == 'refs/heads/main' && steps.rust-cache-key.outputs.cacheable == 'true' && steps.rust-cache-restore.outputs.cache-hit != 'true'";
  assert.equal(jobs.rust.split(`if: ${saveIf}`).length - 1, 2, "prune and save keep their main-push-only condition");
  assert.match(jobs.rust, /RUST_CACHE_VERSION: v1\n/);
  assert.match(jobs.rust, /run: \.\.\/scripts\/ci\/rust-cache-key\.ps1\n/);
  for (const job of ["changes", "javascript", "ci-gate"]) assert.doesNotMatch(jobs[job], /actions\/cache/);
  const dist = read(".github/workflows/windows-dist.yml");
  assert.doesNotMatch(dist, /classify-changes|ci-gate|actions\/cache/);
});
