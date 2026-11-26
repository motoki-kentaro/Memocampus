// The Windows Rust CI build cache (Issue #93 S2): static checks of .github/workflows/ci.yml and its scripts.
//
// What this checks: the cache is restored with an exact key only (no restore-keys) and saved only by a push to main
// after every step succeeded, never by a pull request; restore and save use the same key and paths; this repository's
// outputs are pruned before saving; the actions are pinned in actions.lock; the key covers every crate CI builds; the
// distribution workflow uses no cache; permissions and the S1 concurrency stay as they were.
//
// What it cannot check: how GitHub Actions behaves at run time (scopes, eviction, the speed of restore and save). That
// is measured on real runs; see the S2 report on Issue #93.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const read = (file) => readFileSync(path.join(root, file), "utf8").replace(/\r\n/g, "\n");
const code = (text) => text.split("\n").filter((line) => !/^\s*#/.test(line)).join("\n");

const ci = code(read(".github/workflows/ci.yml"));
const dist = code(read(".github/workflows/windows-dist.yml"));
const lock = read(".github/workflows/actions.lock");
const keyScript = read("scripts/ci/rust-cache-key.ps1");
const pruneScript = read("scripts/ci/rust-cache-prune.ps1");

// The Rust job's steps, each as its text (a step starts with "      - " at the job's step indent).
const rustStart = ci.indexOf("\n  rust:\n");
const rustEnd = ci.slice(rustStart + 1).search(/\n  [\w-]+:\n/);
const rustJob = rustEnd < 0 ? ci.slice(rustStart) : ci.slice(rustStart, rustStart + 1 + rustEnd + 1);
const steps = rustJob.slice(rustJob.indexOf("\n    steps:\n")).split(/\n(?=      - )/).slice(1);
const stepWith = (re) => {
  const found = steps.filter((s) => re.test(s));
  assert.equal(found.length, 1, `exactly one Rust step matches ${re}`);
  return found[0];
};
const indexOf = (step) => steps.indexOf(step);
const field = (step, name) => (new RegExp(`\\n\\s+${name}: ([^\\n]+)`).exec(step) ?? [])[1];
const block = (step, name) => {
  const m = new RegExp(`\\n(\\s+)${name}: \\|\\n((?:\\1  [^\\n]*\\n?)+)`).exec(step + "\n");
  return m ? m[2].trim().split("\n").map((l) => l.trim()) : null;
};

const keyStep = stepWith(/rust-cache-key\.ps1/);
const restore = stepWith(/uses: actions\/cache\/restore@/);
const prune = stepWith(/rust-cache-prune\.ps1/);
const save = stepWith(/uses: actions\/cache\/save@/);
const cargoSteps = steps.filter((s) => /\brun: cargo /.test(s));
const MAIN_PUSH_ONLY = [/success\(\)/, /github\.event_name == 'push'/, /github\.ref == 'refs\/heads\/main'/, /steps\.rust-cache-key\.outputs\.cacheable == 'true'/, /steps\.rust-cache-restore\.outputs\.cache-hit != 'true'/];

test("the cache is restored before any cargo step, with the exact key only", () => {
  assert.ok(indexOf(keyStep) < indexOf(restore), "the key is made before the restore");
  assert.ok(indexOf(restore) < Math.min(...cargoSteps.map(indexOf)), "the restore comes before every cargo step");
  assert.match(field(restore, "if") ?? "", /^steps\.rust-cache-key\.outputs\.cacheable == 'true'$/);
  assert.equal(field(restore, "key"), "${{ steps.rust-cache-key.outputs.key }}");
  // A prefix match would restore outputs made from other inputs; a cache key is exact or nothing.
  assert.doesNotMatch(ci, /restore-keys/);
  assert.doesNotMatch(ci, /fail-on-cache-miss: true/);
});

test("the cache is saved only by a push to main, after every check, and only when the exact key was missing", () => {
  assert.ok(indexOf(save) === steps.length - 1, "saving is the last step");
  assert.ok(indexOf(prune) === indexOf(save) - 1, "pruning comes right before saving");
  assert.ok(indexOf(prune) > Math.max(...cargoSteps.map(indexOf)), "pruning and saving follow every cargo step");
  for (const step of [prune, save]) {
    const cond = field(step, "if") ?? "";
    for (const re of MAIN_PUSH_ONLY) assert.match(cond, re, `${re} in: ${cond}`);
    assert.doesNotMatch(cond, /always\(\)|failure\(\)|cancelled\(\)|\|\|/, "no condition that would save after a failure");
    assert.doesNotMatch(step, /continue-on-error/);
  }
  assert.equal(field(save, "key"), field(restore, "key"), "save uses the key that was looked up");
  // The cache "version" is derived from the paths: different paths would never match.
  assert.deepEqual(block(save, "path"), block(restore, "path"));
  // Only the combined actions/cache would save in a post step on every event; it is not used.
  assert.doesNotMatch(ci, /uses: actions\/cache@/);
  assert.equal((ci.match(/actions\/cache\/save@/g) ?? []).length, 1);
  // The informational tray ui_process step is the only one allowed to fail without failing the job.
  assert.equal((ci.match(/continue-on-error: true/g) ?? []).length, 1);
  assert.match(stepWith(/continue-on-error: true/), /cargo test --locked --test ui_process/);
});

test("the cache holds the target dir and the Cargo registry only (no credentials, no whole Cargo home)", () => {
  const paths = block(restore, "path");
  assert.deepEqual(paths, [
    "${{ env.CARGO_TARGET_DIR }}",
    "${{ steps.rust-cache-key.outputs.cargo-home }}\\registry\\index",
    "${{ steps.rust-cache-key.outputs.cargo-home }}\\registry\\cache",
  ]);
  assert.match(rustJob, /CARGO_TARGET_DIR: \$\{\{ github\.workspace \}\}\\target-ci/);
  // The prune script removes this repository's crates, test programs and executables, and checks it is in a target dir.
  assert.match(pruneScript, /CACHEDIR\.TAG/);
  assert.match(pruneScript, /memocampus\[-_\]\|page_extract/);
  assert.match(pruneScript, /'incremental'/);
});

test("the key covers every crate the Rust job builds, the toolchain and the build environment", () => {
  const built = [...new Set([...ci.matchAll(/working-directory: ([\w-]+)/g)].map((m) => m[1]))].sort();
  const keyed = /\[string\[\]\]\$Crates = @\(([^)]*)\)/.exec(keyScript)[1].match(/'([\w-]+)'/g).map((s) => s.slice(1, -1)).sort();
  assert.deepEqual(keyed, built, "the key's crates are exactly the crates CI builds");
  for (const file of ["Cargo.toml", "Cargo.lock", ".cargo/config.toml"]) assert.ok(keyScript.includes(`'${file}'`), file);
  const code = keyScript.split("\n").filter((line) => !/^\s*#/.test(line)).join("\n");
  for (const part of ["rustc -Vv", "VCToolsVersion", "Windows Kits", "$env:PERL", "RUST_CACHE_VERSION"]) assert.ok(code.includes(part), part);
  // The environment variables that change a build are matched by one pattern; each family must be in it.
  const envPattern = /\$_\.Name -match '\^\(([^']+)\)\$'/.exec(code)?.[1].split("|") ?? [];
  for (const name of ["RUSTFLAGS", "RUSTC.*", "CARGO_.*", "CC", "CFLAGS.*", "OPENSSL_.*", "PERL"]) assert.ok(envPattern.includes(name), name);
  assert.match(rustJob, /RUST_CACHE_VERSION: v\d+\n/);
  // Nothing in the workflow overrides the crates' rustflags (that would also have to be in the key).
  assert.doesNotMatch(ci, /RUSTFLAGS|CARGO_ENCODED_RUSTFLAGS|CARGO_BUILD_RUSTFLAGS/);
});

test("the cache actions are pinned in actions.lock, at one version, to a commit", () => {
  const versions = [...ci.matchAll(/uses: actions\/cache\/(?:restore|save)@(v\d+\.\d+\.\d+)\n/g)].map((m) => m[1]);
  assert.equal(versions.length, 2);
  assert.equal(new Set(versions).size, 1);
  const pinned = `actions/cache@${versions[0]}`;
  assert.match(lock, new RegExp(`'\\.github/workflows/ci\\.yml':\\n(?:\\s+- '[^']+'\\n)*\\s+- '${pinned.replace(/[./]/g, "\\$&")}'`));
  assert.match(lock, new RegExp(`'${pinned.replace(/[./]/g, "\\$&")}':\\n\\s+ref: '${versions[0]}'\\n\\s+commit: 'sha1-[0-9a-f]{40}'`));
});

test("the gates around the cache are unchanged: permissions, concurrency, no cache in the distribution build", () => {
  assert.match(ci, /\npermissions:\n  contents: read\n/);
  assert.equal((ci.match(/^\s*permissions:/gm) ?? []).length, 1);
  assert.doesNotMatch(ci, /:\s*write\s*$/m);
  assert.match(ci, /cancel-in-progress: \$\{\{ github\.event_name == 'pull_request' \}\}/);
  assert.doesNotMatch(dist, /actions\/cache|Swatinem|sccache|restore-keys/);
  for (const crate of ["native-host", "visible-store", "mcp-server", "page-extract", "tray"]) {
    for (const cmd of ["cargo fmt --all -- --check", "cargo clippy --locked --all-targets -- -D warnings"]) {
      assert.ok(steps.some((s) => s.includes(cmd) && (crate === "native-host" ? !/working-directory/.test(s) : s.includes(`working-directory: ${crate}`))), `${crate}: ${cmd}`);
    }
  }
});
