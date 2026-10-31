import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import defaults from "../../extension/policy-defaults.json" with { type: "json" };
import { evaluatePolicy, normalizeUrl, parseRule, validatePolicy } from "../../extension/policy.js";
const INVALID_URL = { name: "Error", message: "invalid_url" };
const vectors = JSON.parse(readFileSync(new URL("../privacy/vectors.json", import.meta.url), "utf8"));
test("browser builtin module matches the canonical shared JSON", () => {
  execFileSync(process.execPath, [fileURLToPath(new URL("../../scripts/generate-policy-builtins.mjs", import.meta.url)), "--check"]);
});
for (const vector of vectors.decisions) {
  test(`privacy decision: ${vector.name}`, () => {
    assert.equal(evaluatePolicy(vector.policy ?? defaults, vector.url), vector.expected);
  });
}
for (const vector of vectors.normalization) {
  test(`privacy normalization: ${vector.url}`, () => {
    if (vector.expected === null) assert.throws(() => normalizeUrl(vector.url), INVALID_URL);
    else assert.deepEqual(normalizeUrl(vector.url), vector.expected);
  });
}
for (const vector of vectors.rules) {
  test(`privacy rule: ${vector.rule}`, () => {
    if (vector.valid) assert.doesNotThrow(() => parseRule(vector.rule));
    else assert.throws(() => parseRule(vector.rule));
  });
}
for (const vector of vectors.invalidPolicies) {
  test(`privacy schema: ${vector.name}`, () => {
    assert.throws(() => validatePolicy(vector.policy));
    assert.equal(evaluatePolicy(vector.policy, "https://example.com/"), "neverSave");
  });
}

for (const [where, url] of [
  ["query with invalid UTF-8", "https://example.com/?q=%FF"],
  ["query with an invalid escape", "https://example.com/?q=%ZZ"],
  ["query with a truncated sequence", "https://example.com/?q=%E3%81"],
  ["fragment with invalid UTF-8", "https://example.com/#%FF"],
  ["fragment with an invalid escape", "https://example.com/#%ZZ"],
  ["path with invalid UTF-8", "https://example.com/%FF"],
  ["path with an invalid escape", "https://example.com/%ZZ"],
  ["path with a lone percent", "https://example.com/a%"],
  ["blob URL with invalid UTF-8", "blob:https://example.com/%FF"],
  ["unparsable URL", "not a url"],
  ["unparsable blob URL", "blob:null/id"],
]) {
  test(`malformed URL reports invalid_url and fails closed: ${where}`, () => {
    assert.throws(() => normalizeUrl(url), INVALID_URL);
    assert.equal(evaluatePolicy(defaults, url), "neverSave");
  });
}

test("valid percent encoding still normalizes as before", () => {
  const { path, query, fragment } = normalizeUrl("https://example.com/%E7%A7%98/a?q=%E5%AF%86#%E3%81%82");
  assert.deepEqual({ path, query, fragment }, { path: "/秘/a", query: "q=%E5%AF%86", fragment: "%E3%81%82" });
  assert.equal(evaluatePolicy(defaults, "https://example.com/%E7%A7%98?q=%E5%AF%86#%E3%81%82"), "none");
});

test("exceptions that are not URL rejections are not reported as invalid_url", () => {
  const realDecode = globalThis.decodeURIComponent;
  const RealUrl = globalThis.URL;
  try {
    globalThis.decodeURIComponent = () => { throw new RangeError("bug"); };
    assert.throws(() => normalizeUrl("https://example.com/"), { name: "RangeError", message: "bug" });
    globalThis.decodeURIComponent = realDecode;
    globalThis.URL = class { constructor() { throw new RangeError("bug"); } };
    assert.throws(() => normalizeUrl("https://example.com/"), { name: "RangeError", message: "bug" });
  } finally {
    globalThis.decodeURIComponent = realDecode;
    globalThis.URL = RealUrl;
  }
});
