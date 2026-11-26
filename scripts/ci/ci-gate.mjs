// Issue #93 S3-1: the `CI gate` job, the one check to require before merging.
//
// It runs after every other job (if: always()) and passes only in these exact states:
//   - pull request, `changes` succeeded and said rust=false, JavaScript succeeded, Rust was skipped;
//   - pull request, `changes` succeeded and said rust=true, JavaScript succeeded, Rust succeeded;
//   - push or manual run, `changes` succeeded and said rust=true, JavaScript succeeded, Rust succeeded.
// Everything else fails: a failed, cancelled or skipped job that was needed, Rust skipped when it was needed, Rust run
// when it was not (the job conditions disagree with the decision), a missing or unknown output, an unknown event or an
// unexpected job. It does not trust GitHub's own `if:` evaluation: it checks the results that came out of it.
import { appendFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

export const JOBS = ["changes", "javascript", "rust"];
const RESULTS = ["success", "failure", "cancelled", "skipped"];

/** `needs` is the parsed `toJSON(needs)`; returns { pass, reasons }. */
export function gate({ event, needs }) {
  const reasons = [];
  if (!["pull_request", "push", "workflow_dispatch"].includes(event)) reasons.push(`unexpected event ${JSON.stringify(event)}`);
  if (needs === null || typeof needs !== "object" || Array.isArray(needs)) return { pass: false, reasons: [...reasons, "needs is not an object"] };
  const names = Object.keys(needs).sort();
  if (names.join(",") !== [...JOBS].sort().join(",")) reasons.push(`expected exactly the jobs ${JOBS.join(", ")}, got ${names.join(", ") || "none"}`);
  const result = (job) => {
    const value = needs[job]?.result;
    if (!RESULTS.includes(value)) reasons.push(`${job}: unexpected result ${JSON.stringify(value)}`);
    return value;
  };
  const [changes, javascript, rust] = JOBS.map(result);
  if (changes !== "success") reasons.push(`changes: ${changes} (must be success)`);
  if (javascript !== "success") reasons.push(`javascript: ${javascript} (must be success)`);
  const outputs = needs.changes?.outputs;
  const decision = outputs && typeof outputs === "object" ? outputs.rust : undefined;
  if (decision !== "true" && decision !== "false") reasons.push(`changes.outputs.rust is ${JSON.stringify(decision)} (must be "true" or "false")`);
  if (event !== "pull_request" && decision !== "true") reasons.push(`a ${event} run must decide rust=true`);
  const required = event !== "pull_request" || decision !== "false";
  if (required && rust !== "success") reasons.push(`rust: ${rust} although Rust was required (must be success)`);
  if (!required && rust !== "skipped") reasons.push(`rust: ${rust} although changes said it was not required (must be skipped)`);
  return { pass: reasons.length === 0, reasons };
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  let verdict;
  try {
    verdict = gate({ event: process.env.EVENT_NAME, needs: JSON.parse(process.env.NEEDS ?? "") });
  } catch (error) {
    verdict = { pass: false, reasons: [`cannot read the job results: ${error.message}`] };
  }
  let needs = {};
  try { needs = JSON.parse(process.env.NEEDS ?? "{}") ?? {}; } catch {}
  const row = (job) => `| ${job} | ${needs[job]?.result ?? "(missing)"} |`;
  const summary = [
    `## CI gate: ${verdict.pass ? "PASS" : "FAIL"}`,
    "",
    `Event: ${process.env.EVENT_NAME || "(none)"}; Rust required: ${needs.changes?.outputs?.rust ?? "(missing)"} (${needs.changes?.outputs?.reason ?? "no reason"})`,
    "",
    "| Job | Result |",
    "|---|---|",
    ...JOBS.map(row),
    "",
    ...verdict.reasons.map((r) => `- ${r}`),
  ].join("\n");
  console.log(summary);
  if (process.env.GITHUB_STEP_SUMMARY) { try { appendFileSync(process.env.GITHUB_STEP_SUMMARY, summary + "\n"); } catch {} }
  process.exitCode = verdict.pass ? 0 : 1;
}
