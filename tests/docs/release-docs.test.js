// The release documents of the Windows app (Issue #102, Slice 2): installer/README.md and installer/E2E-ACCEPTANCE.md.
//
// What this checks: the first public release is named v0.1.0 and the old plan's 0.8.0 is gone; the blockers that are
// resolved (the Chrome Web Store id, the release gate, the first workflow run) are no longer described as open, and A3
// is recorded as no longer blocked but NOT RUN; the acceptance results are not raised (every row is NOT RUN except the
// partial A10, and no row is BLOCKED, PASS or FAIL otherwise); the product owner's VM observations are PO-CONFIRMED,
// defined apart from PASS, with the evidence status stated, and cross-referenced both ways with the rows they touch;
// the v0.1.0 release rule (R1 to R7, Issue #102) is the risk-based one the product owner chose, states the accepted C5
// residual risk and keeps the automated gates, and no rule requires every row for later releases; the Store address is
// the exact published listing; Windows 11 x64 is the verified platform, Windows 10 unverified, Windows Server
// unsupported; review-build users are told to uninstall first; the Setup program is said to be unsigned and no document
// tells users to switch a Windows protection off; the Inno Setup licence question is listed as open.
//
// What it cannot check: whether a sentence is true. These are key phrases; a statement reversed in other words still
// passes. The documents need a person's review whenever they or the release process change.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const read = (file) => readFileSync(path.join(root, file), "utf8").replace(/\r\n/g, "\n");
const E2E = "installer/E2E-ACCEPTANCE.md";
const INST = "installer/README.md";
const docs = { [E2E]: read(E2E), [INST]: read(INST) };
const flat = (s) => s.replace(/\s+/g, " ");
const ID = "cdklofjfcjagkaclekmicliailmlnfla";
const STORE_URL = `https://chromewebstore.google.com/detail/memocampus/${ID}`;

// The rows of the acceptance tables: [id, result cell].
const rows = [...docs[E2E].matchAll(/^\| ([A-F]\d+) \|(.*)\|\s*$/gm)].map(([, id, rest]) => [id, rest.split("|").at(-1).trim()]);

test("the release gate holds the published extension id the documents name", () => {
  assert.equal(JSON.parse(read("installer/release-gate.json")).extensionId, ID);
  assert.equal(JSON.parse(read("extension/manifest.json")).version, "0.1.0");
  for (const file of [E2E, INST]) {
    assert.ok(docs[file].includes(ID), `${file} names the recorded id`);
    // Any Store address is the published listing, or its template with <release id> / <extension id>.
    for (const [url] of docs[file].matchAll(/https:\/\/chromewebstore\.google\.com\/[^\s`)|,]*(?: id>)?/g)) {
      const templates = ["<release id>", "<extension id>"].map((t) => `https://chromewebstore.google.com/detail/memocampus/${t}`);
      assert.ok(url === STORE_URL || templates.includes(url), `${file}: ${url}`);
    }
    assert.ok(!docs[file].includes("chrome.google.com/webstore"), `${file} uses the old Store host`);
  }
});

test("the first public release is v0.1.0 and the old plan's 0.8.0 is not named as a release", () => {
  for (const file of [E2E, INST]) {
    assert.doesNotMatch(docs[file], /\bv?0\.8\.0\b/, `${file} names 0.8.0`);
    assert.match(docs[file], /v0\.1\.0/, `${file} names v0.1.0`);
  }
  assert.match(flat(docs[E2E]), /\*\*The v0\.1\.0 release rule below must be met before the public release of v0\.1\.0\*\*/);
  assert.match(docs[INST], /^### Before v0\.1\.0 is made public \(blockers\)$/m);
  // The review pre-release is kept apart from the release.
  for (const file of [E2E, INST]) assert.match(flat(docs[file]), /v0\.1\.0-review\.1/, `${file} keeps the review build apart`);
  assert.match(flat(docs[INST]), /its assets are never reused/);
});

test("resolved blockers are not described as open", () => {
  const stale = [
    /no Chrome Web Store extension id exists yet/i,
    /`extensionId` is `null` until/,
    /extensionId` is null, so no product build/,
    /Chrome Web Store id is not recorded yet/i,
    /before a Web Store id exists/i,
    /blocked\s+until `release-gate\.json` has the id/i,
    /until the workflow has run once on GitHub/i,
    /The real Chrome Web Store extension id, recorded in/,
  ];
  for (const file of [E2E, INST]) for (const re of stale) assert.doesNotMatch(docs[file], re, `${file}: ${re}`);
  assert.doesNotMatch(docs[E2E], /BLOCKED/, "no row or status is BLOCKED");
});

test("acceptance results are not raised: only the partial A10 is recorded, everything else is NOT RUN", () => {
  const ids = rows.map(([id]) => id);
  assert.equal(new Set(ids).size, ids.length, "row ids are unique");
  for (const section of ["A", "B", "C", "D", "F"]) assert.ok(ids.some((id) => id.startsWith(section)), `section ${section} has rows`);
  assert.equal(ids.length, 40);
  for (const [id, result] of rows) {
    if (id === "A10") {
      assert.match(result, /^PASS for the C runtime start \(/);
      continue;
    }
    assert.match(result, /^NOT RUN\b/, `${id} is ${result}`);
    assert.doesNotMatch(result, /\b(PASS|FAIL|BLOCKED)\b/, `${id} result`);
  }
  const a3 = rows.find(([id]) => id === "A3")[1];
  assert.match(a3, /^NOT RUN \(no longer blocked: the id is recorded in `installer\\release-gate\.json`/);
  assert.match(flat(docs[E2E]), /without recorded evidence, so no row has been recorded as PASS from that run/);
  assert.match(flat(docs[E2E]), /A3 is no longer blocked \(the Chrome Web Store id `cdklofjfcjagkaclekmicliailmlnfla`/);
});

test("PO-CONFIRMED is defined apart from PASS, states its evidence, and matches the rows it touches both ways", () => {
  const e2e = flat(docs[E2E]);
  for (const sentence of [
    "**PO-CONFIRMED** is a different kind of record: the product owner's own report of what they saw, written as `PO-CONFIRMED (date, who, environment, evidence: <file> | none, statement only)`. It always says whether evidence exists. It is never written as PASS, never counted as a direct verification of a row, and covers only what the report names: the rest of a row it touches stays NOT RUN.",
    "**Evidence: none attached (statement only)** unless a file is named here later. Nothing below is PASS.",
    "for the release candidate `Memocampus-Setup-x64.exe`, SHA-256 `0a0bd02bd1daf4df0ed67e6f4fd20f28155088d2d145f6b1a15b6ba593146f5d`",
  ]) assert.ok(e2e.includes(sentence), `PO-CONFIRMED: ${sentence}`);
  // The observations table: O1..O9, four columns each.
  const obs = [...docs[E2E].matchAll(/^\| (O\d+) \|(.*)\|\s*$/gm)].map(([, id, rest]) => [id, rest.split("|").map((c) => c.trim())]);
  assert.deepEqual(obs.map(([id]) => id), ["O1", "O2", "O3", "O4", "O5", "O6", "O7", "O8", "O9"]);
  for (const [id, cells] of obs) assert.equal(cells.length, 3, `${id} has its columns`);
  // Both ways: a row annotated with O# is listed by O#, and every row O# lists carries the annotation.
  const listed = new Map(obs.map(([id, cells]) => [id, new Set([...cells[1].matchAll(/\b([A-F]\d+)\b/g)].map((m) => m[1]))]));
  const annotated = new Map();
  for (const [row, result] of rows) {
    const m = /PO-CONFIRMED in part(?: for the product build)?:? \(?((?:O\d+(?:, )?)+)\)?/.exec(result);
    if (!m) continue;
    assert.match(result, /^(NOT RUN|PASS for the C runtime start) /, `${row}: PO-CONFIRMED stays apart from the row's own result`);
    for (const o of m[1].split(", ")) {
      assert.ok(listed.get(o)?.has(row), `${row} cites ${o}, which does not list it`);
      annotated.set(o, (annotated.get(o) ?? new Set()).add(row));
    }
  }
  for (const [o, set] of listed) for (const row of set) assert.ok(annotated.get(o)?.has(row), `${o} lists ${row}, which does not cite it`);
  // PO-CONFIRMED is never written as a PASS.
  assert.doesNotMatch(docs[E2E], /PASS \(PO|PO-CONFIRMED\)? ?PASS|PASS[^|\n]{0,3}PO-CONFIRMED/);
  // The uninstall observation names the data as not confirmed.
  assert.match(flat(obs.find(([id]) => id === "O9")[1][2]), /\*\*the data kept \(C5\)\*\*/);
});

test("the v0.1.0 release rule is the risk-based one, keeps the gates, and states the accepted residual risk", () => {
  const e2e = flat(docs[E2E]);
  const rule = e2e.slice(e2e.indexOf("## Release rule"));
  const req = [...docs[E2E].matchAll(/^\| (R\d+) \|(.*)\|\s*$/gm)].map(([, id]) => id);
  assert.deepEqual(req, ["R1", "R2", "R3", "R4", "R5", "R6", "R7"]);
  for (const sentence of [
    "### v0.1.0 (the first public release, Issue #102)",
    "The release candidate is publishable only when **all** of these hold:",
    "**That the data is kept (C5) has not been checked directly on a real machine**: it rests on the static checks (the installer refers to no vault, policy, settings, token or data folder; the uninstall removes only its own registrations) and on the uninstall having succeeded. The product owner accepts this residual risk for v0.1.0 (2026-11-28)",
    "The rows not listed above stay NOT RUN; none is written as PASS. Any FAIL is a release blocker.",
    "### Later releases",
    "There is no rule that every row must pass. For each release the cases are chosen from what changed and the risk of it, and the choice and its reasons are recorded in that release's issue",
    "The automated gates of R6 are never relaxed.",
  ]) assert.ok(rule.includes(sentence), `rule: ${sentence}`);
  // R6 names every automated gate.
  const r6 = flat(docs[E2E].split("\n").find((l) => l.startsWith("| R6 |")));
  for (const gate of ["CI (JavaScript, Rust, CI gate)", "`windows-dist.yml` (checks and the product build)", "`verify-static.ps1` with `-Negative` and `-Compile`", "`verify-dist.ps1` with `-Negative`", "the Pascal self-tests", "the release gate"]) {
    assert.ok(r6.includes(gate), `R6: ${gate}`);
  }
  // The old all-rows rule is gone, for v0.1.0 and for later releases.
  assert.doesNotMatch(e2e, /every row of A to D and F is PASS/);
  assert.doesNotMatch(e2e, /This rule applies to v0\.1\.0 unchanged/);
  assert.doesNotMatch(flat(docs[INST]), /Every case of `installer\\E2E-ACCEPTANCE\.md`/);
  assert.match(flat(docs[INST]), /1\. The v0\.1\.0 release rule of `installer\\E2E-ACCEPTANCE\.md` \(R1 to R7\), on \*\*that\*\* Setup file \(same SHA-256\)/);
  assert.match(flat(docs[INST]), /that the uninstall keeps the user's data \(C5\) has not been checked directly on a real machine/);
});

test("the acceptance rules are kept word for word", () => {
  const e2e = flat(docs[E2E]);
  for (const sentence of [
    "Nothing here may be written up as PASS until someone has run it and kept the evidence.",
    "The result column is filled in by whoever ran the case: `PASS (date, who, evidence file)`, `FAIL (what)` or `NOT RUN`.",
    "These are the documented contracts the design relies on. Each must be seen once:",
  ]) assert.ok(e2e.includes(sentence), `kept: ${sentence}`);
  // The three cases of #100's last page, as the user interface says them.
  const row = (id) => flat(docs[E2E].split("\n").find((l) => l.startsWith(`| ${id} |`)));
  assert.ok(row("A4").includes("two **unticked** boxes"));
  for (const key of ["McLaunchTray", "McOpenStorePage", "FinishedLabel"]) assert.ok(row("A4").includes(key), `A4 names ${key}`);
  const isl = (lang) => read(`installer/i18n/${lang}.isl`);
  const label = (lang, key) => new RegExp(`^${key}=(.*)$`, "m").exec(isl(lang))[1];
  assert.ok(row("A4").includes(`"${label("en", "McOpenStorePage").split(" (")[0]}"`), "A4 quotes the English Store choice");
  assert.ok(row("A4").includes(`「${label("ja", "McOpenStorePage").split("（")[0]}」`), "A4 quotes the Japanese Store choice");
  assert.ok(row("A6").includes(STORE_URL), "A6 names the exact listing");
  assert.match(row("A6"), /Box left unticked: no browser opens/);
  assert.match(row("A7"), /`\/VERYSILENT`.*Neither the tray nor the browser is started/);
});

test("Windows 11 x64 is the verified platform, Windows 10 unverified, Windows Server unsupported; the Setup program is unsigned", () => {
  assert.match(flat(docs[E2E]), /Windows 11 x64 is the platform verified for v0\.1\.0; Windows 10 is not verified, and nothing here may be recorded as proving it; Windows Server is not supported, and no result obtained on it counts/);
  assert.match(flat(docs[E2E]), /Windows 11 x64 is the verified platform, Windows 10 is not verified and Windows Server is not supported/);
  assert.match(flat(docs[INST]), /isolated Windows 11 x64 VM/);
  // Said in the status and again in the blockers.
  for (const re of [/Windows 11 x64 is the verified platform; Windows 10 is not verified; Windows Server is not supported\./g]) {
    assert.equal(flat(docs[INST]).match(re)?.length, 2);
  }
  assert.doesNotMatch(flat(docs[E2E] + docs[INST]), /Windows Server (is|are) supported|supports? Windows Server/i);
  // Users of the review build uninstall it first.
  assert.match(flat(docs[E2E]), /a user of the review build `v0\.1\.0-review\.1` uninstalls it before installing v0\.1\.0/);
  assert.match(flat(docs[INST]), /tell users of the review build `v0\.1\.0-review\.1` to uninstall it before installing v0\.1\.0/);
  assert.match(flat(docs[INST]), /The Setup program is \*\*not signed\*\*/);
  assert.match(flat(docs[INST]), /SmartScreen will show/);
  assert.match(flat(docs[INST]), /v0\.1\.0 is planned to ship unsigned; that is approved, or not, by the product owner/);
  // No document tells users to switch a Windows protection off.
  const off = [
    /\b(disable|turn off|switch off|deactivate)\s+(the\s+)?(Windows\s+)?(SmartScreen|(Microsoft\s+)?Defender|antivirus|real-time protection)/i,
    /(SmartScreen|Defender|ウイルス対策|リアルタイム保護)[^。\n]{0,20}(無効|オフ|停止)/,
  ];
  for (const file of [E2E, INST, "README.md", "README.en.md"]) {
    const text = file.startsWith("README") ? read(file) : docs[file];
    for (const re of off) assert.doesNotMatch(text, re, `${file}: ${re}`);
  }
});

test("the tag and the published files are tied to the accepted release candidate", () => {
  const inst = flat(docs[INST]);
  assert.match(inst, /the annotated tag `v0\.1\.0` on the commit the accepted release candidate was built from \(its `build-info\.json` `gitCommit`\), never moved/);
  assert.match(inst, /exactly the accepted files, not a rebuild/);
  assert.match(flat(docs[E2E]), /on the very Setup file that is to be published \(same SHA-256\), not on the review pre-release `v0\.1\.0-review\.1`/);
});

test("the Inno Setup licence question is open until the product owner decides", () => {
  const inst = flat(docs[INST]);
  assert.match(inst, /\*\*Inno Setup licence \(open question, a release blocker until the product owner decides\)\*\*/);
  assert.match(inst, /`Non-commercial use only`/);
  assert.match(inst, /https:\/\/jrsoftware\.org\/isorder\.php/);
  assert.match(inst, /3\. A decision to ship unsigned \(or code signing\), and on the Inno Setup licence/);
});
