// Issue #93 S3-1: decides whether a CI run needs the Windows Rust job.
//
// Only a pull request whose every changed path is on the allow list below may skip Rust. Everything else needs it:
// pushes, manual runs, any path not on the list, and any input that cannot be read with certainty (git failing, a
// head commit that is not the pull request's merge commit, a diff that does not parse, an empty diff). The allow list
// holds paths that no Rust crate reads; a file a crate reads (include_str!, fixtures) is on the deny list even when its
// directory is allowed.
//
// As a script (the `changes` job) it writes `rust=true|false` and a one-line `reason` to $GITHUB_OUTPUT. If it fails
// in any way it still tries to write `rust=true`; if even that fails the job fails, and the Rust job runs anyway.
import { appendFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

/** Paths a Rust crate reads although they sit in an allowed directory (compared case-insensitively). */
export const RUST_READS = ["docs/issue-025-policy-engine.md"];

/** Whether a changed path may skip the Rust job (Plan A'). */
export function skipsRust(path) {
  if (typeof path !== "string" || path === "" || path.includes("\\") || path.split("/").some((p) => p === "" || p === "." || p === "..")) return false;
  if (RUST_READS.includes(path.toLowerCase())) return false;
  return path === "README.md" || path === "README.en.md" || path.startsWith("docs/") || path.startsWith("tests/docs/");
}

/**
 * Parses `git diff --name-status -z --no-renames` output: "<status>\0<path>\0" per file. With --no-renames a rename is
 * a deletion of the old path and an addition of the new one, so both are classified. Throws on anything else.
 */
export function parseNameStatus(text) {
  if (typeof text !== "string") throw new Error("diff output is not text");
  if (text === "") return [];
  if (!text.endsWith("\0")) throw new Error("diff output is not NUL-terminated");
  const fields = text.slice(0, -1).split("\0");
  if (fields.length % 2 !== 0) throw new Error("diff output has an odd number of fields");
  const changes = [];
  for (let i = 0; i < fields.length; i += 2) {
    const [status, path] = [fields[i], fields[i + 1]];
    if (!/^[ADMT]$/.test(status)) throw new Error(`unexpected diff status ${JSON.stringify(status)}`);
    if (path === "") throw new Error("empty path in diff output");
    changes.push({ status, path });
  }
  return changes;
}

/** The decision for a pull request's changed paths. */
export function classify(changes) {
  if (!Array.isArray(changes) || changes.length === 0) return { rust: true, reason: "no changed files found" };
  const needed = changes.filter((c) => !skipsRust(c.path));
  if (needed.length > 0) return { rust: true, reason: `${needed.length} of ${changes.length} changed paths need Rust, e.g. ${needed[0].path}` };
  return { rust: false, reason: `all ${changes.length} changed paths are documentation only` };
}

/** One printable ASCII line, so that a path can never add a line (an output) to $GITHUB_OUTPUT. */
export const oneLine = (text) => JSON.stringify(String(text)).slice(1, -1).replace(/[^\x20-\x7e]/g, "?").slice(0, 300);

/** The whole decision. `git(args)` returns {status, stdout} (stdout as UTF-8 text). */
export function decide({ event, prHeadSha, git }) {
  if (event !== "pull_request") return { rust: true, reason: `event ${event || "(none)"} always runs Rust` };
  try {
    // The pull_request checkout is the merge commit: first parent the base branch, second parent the PR head.
    const parents = git(["rev-list", "--parents", "-n", "1", "HEAD"]);
    if (parents.status !== 0) return { rust: true, reason: "git rev-list failed" };
    const ids = parents.stdout.trim().split(" ");
    if (ids.length !== 3) return { rust: true, reason: `HEAD is not a merge commit (${ids.length - 1} parents)` };
    if (!/^[0-9a-f]{40,64}$/.test(prHeadSha ?? "") || ids[2] !== prHeadSha) return { rust: true, reason: "the merge commit's second parent is not the pull request head" };
    const diff = git(["-c", "core.quotePath=false", "diff", "--name-status", "-z", "--no-renames", "HEAD^1", "HEAD", "--"]);
    if (diff.status !== 0) return { rust: true, reason: "git diff failed" };
    return classify(parseNameStatus(diff.stdout));
  } catch (error) {
    return { rust: true, reason: `classification failed: ${error.message}` };
  }
}

function runGit(args) {
  const r = spawnSync("git", args, { encoding: "utf8", maxBuffer: 256 * 1024 * 1024 });
  return { status: r.error ? -1 : r.status, stdout: r.stdout ?? "" };
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  let result;
  try {
    result = decide({ event: process.env.GITHUB_EVENT_NAME, prHeadSha: process.env.PR_HEAD_SHA, git: runGit });
  } catch (error) {
    result = { rust: true, reason: `classification failed: ${error.message}` };
  }
  const rust = result.rust === false ? "false" : "true";
  const lines = `rust=${rust}\nreason=${oneLine(result.reason)}\n`;
  console.log(lines.trim());
  if (process.env.GITHUB_OUTPUT) appendFileSync(process.env.GITHUB_OUTPUT, lines);
  else if (process.env.CI) throw new Error("GITHUB_OUTPUT is not set");
}
