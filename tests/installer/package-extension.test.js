import assert from "node:assert/strict";
import { cpSync, existsSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { crc32, makeZip, packageExtension, zipNames } from "../../scripts/package-extension.mjs";

const REAL = fileURLToPath(new URL("../../extension", import.meta.url));
const sha = (path) => createHash("sha256").update(readFileSync(path)).digest("hex");

function scratch() {
  return mkdtempSync(join(tmpdir(), "memocampus-pkg-test-"));
}

function variant(edit) {
  const base = scratch();
  const src = join(base, "extension");
  cpSync(REAL, src, { recursive: true });
  edit(src);
  return { base, src, out: join(base, "out") };
}

test("crc32 matches the standard check value", () => {
  assert.equal(crc32(Buffer.from("123456789")), 0xcbf43926);
});

test("the ZIP lists exactly the names it was given, in order", () => {
  const zip = makeZip([{ name: "a.txt", data: Buffer.from("x") }, { name: "d/b.txt", data: Buffer.from("yy") }]);
  assert.deepEqual(zipNames(zip), ["a.txt", "d/b.txt"]);
});

test("the real extension packages with the product profile and the working copy is untouched", () => {
  const before = sha(join(REAL, "host-config.js"));
  const base = scratch();
  try {
    const manifest = JSON.parse(readFileSync(join(REAL, "manifest.json"), "utf8"));
    const result = packageExtension({ srcDir: REAL, outDir: join(base, "out"), expectVersion: manifest.version });
    assert.equal(result.profile, "product");
    assert.ok(existsSync(result.zipPath));
    assert.ok(result.files.includes("manifest.json"));
    assert.ok(result.files.includes("host-config.js"));
    assert.equal(result.files.some((f) => /\.(pem|key|map|log|zip|crx)$|\.test\.|^\.env|(^|\/)\.git|node_modules/i.test(f)), false);
    assert.equal(sha(join(REAL, "host-config.js")), before, "the repository copy must not be rewritten");
    assert.match(readFileSync(join(REAL, "host-config.js"), "utf8"), /HOST_PROFILE = "legacyDev"/);
    const again = packageExtension({ srcDir: REAL, outDir: join(base, "out2") });
    assert.equal(again.sha256, result.sha256, "the ZIP is reproducible");
  } finally {
    rmSync(base, { recursive: true, force: true });
  }
});

const refusals = [
  ["a wrong expected version", (src) => {}, "9.9.9", /expected 9\.9\.9/],
  ["a manifest `key`", (src) => {
    const p = join(src, "manifest.json");
    writeFileSync(p, JSON.stringify({ ...JSON.parse(readFileSync(p, "utf8")), key: "AAAA" }));
  }, undefined, /key/],
  ["Manifest V2", (src) => {
    const p = join(src, "manifest.json");
    writeFileSync(p, JSON.stringify({ ...JSON.parse(readFileSync(p, "utf8")), manifest_version: 2 }));
  }, undefined, /manifest_version/],
  ["a non-numeric version", (src) => {
    const p = join(src, "manifest.json");
    writeFileSync(p, JSON.stringify({ ...JSON.parse(readFileSync(p, "utf8")), version: "0.8.0-beta" }));
  }, undefined, /dotted number/],
  ["a private key file", (src) => writeFileSync(join(src, "signing.pem"), "-----BEGIN"), undefined, /must not be in the package/],
  ["an .env file", (src) => writeFileSync(join(src, ".env"), "TOKEN=x"), undefined, /must not be in the package/],
  ["a source map", (src) => writeFileSync(join(src, "history.js.map"), "{}"), undefined, /must not be in the package/],
  ["a test file", (src) => writeFileSync(join(src, "x.test.js"), ""), undefined, /must not be in the package/],
  ["another file naming the dev host", (src) => writeFileSync(join(src, "fallback.js"), 'export const h = "io.github.memocampus.vault.dev";\n'), undefined, /another native host/],
  ["another file naming the legacy host", (src) => writeFileSync(join(src, "fallback.js"), 'const h = "com.memocampus.vault";\n'), undefined, /another native host/],
  ["a missing HOST_PROFILE line", (src) => {
    const p = join(src, "host-config.js");
    writeFileSync(p, readFileSync(p, "utf8").replace(/export const HOST_PROFILE = "[A-Za-z]+";/, ""));
  }, undefined, /HOST_PROFILE line not found/],
  ["a changed product host name", (src) => {
    const p = join(src, "host-config.js");
    writeFileSync(p, readFileSync(p, "utf8").replace('product: "io.github.memocampus.vault"', 'product: "io.github.memocampus.vault2"'));
  }, undefined, /product host name/],
];
for (const [name, edit, expectVersion, pattern] of refusals) {
  test(`refuses ${name} and leaves no ZIP`, () => {
    const v = variant(edit);
    try {
      assert.throws(() => packageExtension({ srcDir: v.src, outDir: v.out, expectVersion }), pattern);
      assert.deepEqual(existsSync(v.out) ? readdirSync(v.out) : [], []);
    } finally {
      rmSync(v.base, { recursive: true, force: true });
    }
  });
}
