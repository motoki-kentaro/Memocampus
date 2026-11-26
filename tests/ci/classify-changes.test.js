// Issue #93 S3-1: scripts/ci/classify-changes.mjs decides whether a CI run needs the Windows Rust job.
// Only a pull request that changes nothing but the READMEs, docs/ or tests/docs/ may skip Rust; every other input,
// including any the classifier cannot read with certainty, must give rust=true.
import { test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { classify, decide, oneLine, parseNameStatus, skipsRust } from "../../scripts/ci/classify-changes.mjs";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const script = path.join(root, "scripts/ci/classify-changes.mjs");
const z = (...pairs) => pairs.map(([s, p]) => `${s}\0${p}\0`).join("");
const pr = (paths) => classify(paths.map((p) => ({ status: "M", path: p })));

test("README, docs/ and tests/docs/ changes skip Rust", () => {
  for (const paths of [["README.md"], ["README.en.md"], ["README.md", "README.en.md"], ["docs/development.md"], ["docs/secure-vault/slice-e.md"], ["README.md", "tests/docs/readme.test.js"], ["README.en.md", "docs/adr/new.md", "tests/docs/readme.test.js"]]) {
    assert.equal(pr(paths).rust, false, paths.join(" "));
  }
});

test("files a Rust crate reads need Rust, even inside an allowed directory", () => {
  // include_str!: native-host/src/policy/tests.rs (docs), policy.rs (extension JSON), vector tests (tests/ JSON).
  for (const p of ["docs/issue-025-policy-engine.md", "docs/ISSUE-025-POLICY-ENGINE.md", "docs/Issue-025-Policy-Engine.md", "extension/policy-defaults.json", "extension/policy-builtins.json", "tests/privacy/vectors.json", "tests/page-metadata/vectors.json"]) {
    assert.equal(skipsRust(p), false, p);
    assert.equal(pr(["README.md", p]).rust, true, p);
  }
});

test("every other path needs Rust: code, Cargo files, CI, scripts, installer, extension, other tests, unknown paths", () => {
  for (const p of [
    "native-host/src/policy.rs", "visible-store/Cargo.lock", "mcp-server/Cargo.toml", "tray/.cargo/config.toml", "native-host/build.rs",
    "page-extract/tests/fixtures/chrome/a.mhtml", ".github/workflows/ci.yml", ".github/workflows/windows-dist.yml", ".github/workflows/actions.lock",
    "scripts/ci/rust-cache-key.ps1", "scripts/ci/classify-changes.mjs", "scripts/ci/ci-gate.mjs", "installer/memocampus.iss", "extension/sidepanel.js",
    "extension/manifest.json", "tests/extension/policy.test.js", "tests/ci/rust-cache.test.js", "tests/installer/pe-imports.test.js", ".gitattributes",
    "package.json", "package-lock.json", "assets/icon.png", "worker/src/index.ts", "poc/page-fulltext/Cargo.toml", "LICENSE", "THIRD_PARTY_NOTICES.md",
    "readme.md", "README.md.bak", "docs", "tests/docs", "tests/docsx/a.js", "docsx/a.md", "src/docs/a.md", "native-host/docs/a.md", "README.ja.md",
  ]) {
    assert.equal(skipsRust(p), false, p);
    assert.equal(pr(["docs/development.md", p]).rust, true, p);
  }
});

test("odd paths need Rust: backslashes, empty, dot and dot-dot segments, non-strings", () => {
  for (const p of ["docs\\a.md", "", "docs//a.md", "docs/./a.md", "docs/../native-host/src/lib.rs", "./README.md", "docs/", null, undefined, 1]) {
    assert.equal(skipsRust(p), false, String(p));
  }
});

test("names with spaces, newlines, quotes and Japanese are classified by their real path", () => {
  const text = z(["A", "docs/メモ 1.md"], ["M", "docs/line\nbreak.md"], ["D", 'docs/"quoted"\ttab.md']);
  assert.deepEqual(parseNameStatus(text).map((c) => c.path), ["docs/メモ 1.md", "docs/line\nbreak.md", 'docs/"quoted"\ttab.md']);
  assert.equal(classify(parseNameStatus(text)).rust, false);
  // A newline in a name cannot turn a Rust path into a docs path or the reverse.
  assert.equal(classify(parseNameStatus(z(["M", "native-host/x\ndocs/a.md"]))).rust, true);
  assert.equal(classify(parseNameStatus(z(["M", "docs/a\nnative-host/src/lib.rs"]))).rust, false);
});

test("deletions and renames are classified by every path involved (--no-renames: old path deleted, new path added)", () => {
  assert.equal(classify(parseNameStatus(z(["D", "README.en.md"]))).rust, false);
  assert.equal(classify(parseNameStatus(z(["D", "native-host/src/lib.rs"]))).rust, true);
  // docs -> code, code -> docs, docs -> the Rust-read doc: all need Rust.
  assert.equal(classify(parseNameStatus(z(["D", "docs/a.md"], ["A", "native-host/a.md"]))).rust, true);
  assert.equal(classify(parseNameStatus(z(["D", "native-host/a.md"], ["A", "docs/a.md"]))).rust, true);
  assert.equal(classify(parseNameStatus(z(["D", "docs/a.md"], ["A", "docs/issue-025-policy-engine.md"]))).rust, true);
  assert.equal(classify(parseNameStatus(z(["D", "docs/a.md"], ["A", "docs/b.md"], ["T", "docs/c.md"]))).rust, false);
});

test("an empty diff or output that does not parse needs Rust", () => {
  assert.equal(classify([]).rust, true);
  assert.equal(classify(undefined).rust, true);
  for (const bad of ["M\0README.md", "M\0README.md\0X", "R100\0docs/a.md\0docs/b.md\0", "C\0docs/a.md\0", "U\0docs/a.md\0", "X\0docs/a.md\0", "MM\0docs/a.md\0", "M\0\0", "M docs/a.md\n", "\0"]) {
    assert.throws(() => parseNameStatus(bad), JSON.stringify(bad));
  }
  assert.throws(() => parseNameStatus(undefined));
});

// decide() with a scripted git.
const HEAD = "a".repeat(40), BASE = "b".repeat(40), PRH = "c".repeat(40);
const fakeGit = ({ parents = `${HEAD} ${BASE} ${PRH}\n`, revStatus = 0, diff = z(["M", "README.md"]), diffStatus = 0 } = {}) => (args) =>
  args[0] === "rev-list" ? { status: revStatus, stdout: parents } : { status: diffStatus, stdout: diff };

test("pushes, manual runs and unknown events always need Rust, without looking at git", () => {
  // This git would make a docs-only pull request: only the event can make these runs need Rust.
  let calls = 0;
  const git = (args) => (calls++, fakeGit()(args));
  for (const event of ["push", "workflow_dispatch", "pull_request_target", "schedule", "", undefined]) {
    assert.equal(decide({ event, prHeadSha: PRH, git }).rust, true, String(event));
  }
  assert.equal(calls, 0, "git is not consulted outside pull requests");
});

test("a pull request is classified from its merge commit, and any doubt needs Rust", () => {
  assert.deepEqual(decide({ event: "pull_request", prHeadSha: PRH, git: fakeGit() }).rust, false);
  const cases = {
    "rev-list fails": fakeGit({ revStatus: 128 }),
    "not a merge commit": fakeGit({ parents: `${HEAD} ${BASE}\n` }),
    "three parents": fakeGit({ parents: `${HEAD} ${BASE} ${PRH} ${"d".repeat(40)}\n` }),
    "diff fails": fakeGit({ diffStatus: 128 }),
    "diff unparsable": fakeGit({ diff: "garbage" }),
    "diff empty": fakeGit({ diff: "" }),
    "git throws": () => { throw new Error("spawn failed"); },
  };
  for (const [name, git] of Object.entries(cases)) assert.equal(decide({ event: "pull_request", prHeadSha: PRH, git }).rust, true, name);
  // The merge commit must be the one for this pull request head.
  for (const head of [undefined, "", "c".repeat(39), "d".repeat(40)]) assert.equal(decide({ event: "pull_request", prHeadSha: head, git: fakeGit() }).rust, true, String(head));
});

test("the diff compares the merge commit with its first parent, without renames, NUL-separated", () => {
  let diffArgs;
  decide({ event: "pull_request", prHeadSha: PRH, git: (args) => (args[0] === "rev-list" ? fakeGit()(args) : ((diffArgs = args), fakeGit()(args))) });
  assert.ok(diffArgs.includes("--no-renames") && diffArgs.includes("-z") && diffArgs.includes("--name-status"), diffArgs.join(" "));
  assert.deepEqual(diffArgs.slice(-3), ["HEAD^1", "HEAD", "--"]);
});

test("the reason is one printable line, so a path cannot add outputs", () => {
  const reason = oneLine(classify([{ status: "A", path: "native-host/x\nrust=false\r メモ" }]).reason);
  assert.match(reason, /^[\x20-\x7e]*$/);
  assert.ok(!reason.includes("\n"));
});

// The script itself, on a real git repository built with plumbing (names that a Windows checkout could not hold).
function repo() {
  const dir = mkdtempSync(path.join(tmpdir(), "mc-classify-"));
  const env = { ...process.env, GIT_AUTHOR_NAME: "t", GIT_AUTHOR_EMAIL: "t@example.invalid", GIT_COMMITTER_NAME: "t", GIT_COMMITTER_EMAIL: "t@example.invalid", GIT_CONFIG_NOSYSTEM: "1" };
  const git = (args, input) => {
    const r = spawnSync("git", args, { cwd: dir, env, input, encoding: "utf8" });
    assert.equal(r.status, 0, `git ${args.join(" ")}: ${r.stderr}`);
    return r.stdout.trim();
  };
  git(["init", "-q"]);
  const blob = git(["hash-object", "-w", "--stdin"], "x\n");
  const tree = (paths) => {
    const index = path.join(dir, `index-${Math.random()}`);
    const r = spawnSync("git", ["update-index", "-z", "--index-info"], { cwd: dir, env: { ...env, GIT_INDEX_FILE: index }, input: paths.map((p) => `100644 blob ${blob}\t${p}\0`).join("") });
    assert.equal(r.status, 0, String(r.stderr));
    return spawnSync("git", ["write-tree"], { cwd: dir, env: { ...env, GIT_INDEX_FILE: index }, encoding: "utf8" }).stdout.trim();
  };
  const commit = (t, ...parents) => git(["commit-tree", t, ...parents.flatMap((p) => ["-p", p]), "-m", "c"]);
  return { dir, git, tree, commit, done: () => rmSync(dir, { recursive: true, force: true }) };
}

function runScript(r, { event = "pull_request", headSha } = {}) {
  const out = path.join(r.dir, "github-output");
  writeFileSync(out, "");
  const env = { ...process.env, GITHUB_EVENT_NAME: event, PR_HEAD_SHA: headSha ?? "", GITHUB_OUTPUT: out };
  const p = spawnSync(process.execPath, [script], { cwd: r.dir, env, encoding: "utf8" });
  assert.equal(p.status, 0, p.stderr);
  return Object.fromEntries(readFileSync(out, "utf8").trim().split("\n").map((l) => l.split(/=(.*)/s).slice(0, 2)));
}

test("the script on a real merge commit: docs-only skips Rust, a Rust path or a rename into code does not", () => {
  const r = repo();
  try {
    const base = r.commit(r.tree(["README.md", "docs/a.md", "native-host/src/lib.rs", "docs/issue-025-policy-engine.md"]));
    const merge = (paths) => {
      const head = r.commit(r.tree(paths), base);
      const m = r.commit(r.tree(paths), base, head);
      r.git(["update-ref", "HEAD", m]);
      return head;
    };
    const keep = ["native-host/src/lib.rs", "docs/issue-025-policy-engine.md"];
    let head = merge(["README.md", "docs/a.md", "docs/メモ\nnative-host.md", "tests/docs/x.test.js", ...keep]);
    assert.deepEqual(runScript(r, { headSha: head }).rust, "false");
    head = merge(["README.md", "docs/a.md", "native-host/src/lib.rs", "docs/issue-025-policy-engine.md", "native-host/src/new.rs"]);
    assert.deepEqual(runScript(r, { headSha: head }).rust, "true");
    head = merge(["README.md", "native-host/a.md", ...keep]); // docs/a.md renamed into native-host/
    assert.deepEqual(runScript(r, { headSha: head }).rust, "true");
    head = merge(["README.md", "docs/a.md", "native-host/src/lib.rs"]); // the Rust-read doc deleted
    assert.deepEqual(runScript(r, { headSha: head }).rust, "true");
    head = merge(["README.md", "docs/a.md", "native-host/src/lib.rs", "docs/issue-025-policy-engine.md"]); // no change at all
    assert.deepEqual(runScript(r, { headSha: head }), { rust: "true", reason: "no changed files found" });
    // A head that is not this pull request's head, and a HEAD that is not a merge commit.
    merge(["README.md", "docs/b.md", ...keep]);
    assert.equal(runScript(r, { headSha: "d".repeat(40) }).rust, "true");
    r.git(["update-ref", "HEAD", base]);
    assert.equal(runScript(r, { headSha: base }).rust, "true");
    assert.equal(runScript(r, { event: "push" }).rust, "true");
  } finally {
    r.done();
  }
});

test("the script outside a git repository needs Rust and still writes its outputs", () => {
  const dir = mkdtempSync(path.join(tmpdir(), "mc-classify-nogit-"));
  try {
    const out = path.join(dir, "o");
    writeFileSync(out, "");
    const p = spawnSync(process.execPath, [script], { cwd: dir, env: { ...process.env, GITHUB_EVENT_NAME: "pull_request", PR_HEAD_SHA: "c".repeat(40), GITHUB_OUTPUT: out, GIT_CEILING_DIRECTORIES: path.dirname(dir) }, encoding: "utf8" });
    assert.equal(p.status, 0, p.stderr);
    assert.match(readFileSync(out, "utf8"), /^rust=true\nreason=[^\n]+\n$/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
