// The release documents of the Windows app (Issue #102, Slice 2): installer/README.md and installer/E2E-ACCEPTANCE.md.
//
// What this checks: the first public release is named v0.1.0 and the old plan's 0.8.0 is gone; the blockers that are
// resolved (the Chrome Web Store id, the release gate, the first workflow run) are no longer described as open, and A3
// is recorded as no longer blocked but NOT RUN; the acceptance results are not raised (every row is NOT RUN except the
// partial A10, and no row is BLOCKED, PASS or FAIL otherwise); the release rule and the "before the public release"
// sentence are kept word for word; the Store address is the exact published listing; Windows 10 is said to be
// unverified; the Setup program is said to be unsigned and no document tells users to switch a Windows protection off;
// the Inno Setup licence question is listed as open.
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
  assert.match(flat(docs[E2E]), /This gate must be passed before the public release of v0\.1\.0\*\*/);
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
  assert.match(flat(docs[E2E]), /A3 is no longer blocked .* but it has not been run: it is NOT RUN/);
});

test("the release rule and the acceptance rules are kept word for word", () => {
  const e2e = flat(docs[E2E]);
  for (const sentence of [
    "The release candidate is publishable only when **every row of A to D and F is PASS with evidence, section E is observed, and nobody has had to edit a result**. Any FAIL is a release blocker.",
    "This rule applies to v0.1.0 unchanged.",
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

test("Windows 11 is the accepted platform, Windows 10 is unverified, the Setup program is unsigned", () => {
  assert.match(flat(docs[E2E]), /Windows 11 is the platform accepted for v0\.1\.0; Windows 10 is not verified/);
  assert.match(flat(docs[INST]), /isolated Windows 11 environment/);
  // Said in the status and again in the blockers.
  assert.equal(flat(docs[INST]).match(/Windows 10 is not verified/g)?.length, 2);
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
  assert.match(flat(docs[E2E]), /run on the very Setup file that is to be published \(same SHA-256\)/);
});

test("the Inno Setup licence question is open until the product owner decides", () => {
  const inst = flat(docs[INST]);
  assert.match(inst, /\*\*Inno Setup licence \(open question, a release blocker until the product owner decides\)\*\*/);
  assert.match(inst, /`Non-commercial use only`/);
  assert.match(inst, /https:\/\/jrsoftware\.org\/isorder\.php/);
  assert.match(inst, /3\. A decision to ship unsigned \(or code signing\), and on the Inno Setup licence/);
});
