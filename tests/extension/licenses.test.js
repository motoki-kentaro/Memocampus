import { test } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync, readdirSync, existsSync } from "node:fs";
import path from "node:path";
import { extensionDir } from "./fake-dom.js";

const repoRoot = path.resolve(extensionDir, "..");
const read = (...parts) => readFileSync(path.join(...parts));
// SHA-256 of https://www.apache.org/licenses/LICENSE-2.0.txt as downloaded on 2026-10-30.
const APACHE_2_0_SHA256 = "cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30";

test("the full, unmodified Apache License 2.0 text ships with the repository and the extension", () => {
  for (const dir of [repoRoot, extensionDir]) {
    const text = read(dir, "LICENSES", "Apache-2.0.txt");
    assert.equal(createHash("sha256").update(text).digest("hex"), APACHE_2_0_SHA256, dir);
    const body = text.toString("utf8");
    assert.match(body, /Apache License\s+Version 2\.0, January 2004/);
    assert.match(body, /TERMS AND CONDITIONS FOR USE, REPRODUCTION, AND DISTRIBUTION/);
    assert.match(body, /4\. Redistribution\./);
    assert.match(body, /END OF TERMS AND CONDITIONS/);
    assert.match(body, /APPENDIX: How to apply the Apache License to your work\./);
  }
});

test("the extension folder carries the project's MIT License", () => {
  const root = read(repoRoot, "LICENSE");
  assert.ok(root.equals(read(extensionDir, "LICENSE")), "extension/LICENSE must equal LICENSE");
  assert.match(root.toString("utf8"), /^MIT License\r?\n/);
});

test("third-party notices are identical in both places and describe the settings icon", () => {
  const notices = read(repoRoot, "THIRD_PARTY_NOTICES.md");
  assert.ok(notices.equals(read(extensionDir, "THIRD_PARTY_NOTICES.md")), "extension/THIRD_PARTY_NOTICES.md must equal the root copy");
  const text = notices.toString("utf8");
  for (const required of [
    "assets/settings.svg",
    "Material Symbols",
    "“settings”",
    "Google LLC",
    "https://developers.google.com/fonts/docs/material_symbols",
    "Apache License, Version 2.0",
    "LICENSES/Apache-2.0.txt",
    "not** covered by the MIT License",
    "NOTICE",
    "not byte-identical",
  ]) {
    assert.ok(text.includes(required), `notice mentions ${required}`);
  }
  // Every relative path the notice names exists from both the repository root and extension/.
  for (const dir of [repoRoot, extensionDir]) {
    for (const file of ["LICENSE", "LICENSES/Apache-2.0.txt", "assets/settings.svg"]) {
      assert.ok(existsSync(path.join(dir, file)), `${dir}: ${file}`);
    }
  }
});

test("every packaged asset is covered by a third-party notice", () => {
  const notices = read(extensionDir, "THIRD_PARTY_NOTICES.md").toString("utf8");
  for (const file of readdirSync(path.join(extensionDir, "assets"))) {
    assert.ok(notices.includes(`assets/${file}`), `extension/assets/${file} needs an entry in THIRD_PARTY_NOTICES.md`);
  }
});

test("the README separates the MIT License from the Apache-licensed icon", () => {
  const readme = read(repoRoot, "README.md").toString("utf8");
  assert.match(readme, /MIT License/);
  assert.match(readme, /settings\.svg[^\n]*Apache License 2\.0/);
  assert.match(readme, /THIRD_PARTY_NOTICES\.md/);
});
