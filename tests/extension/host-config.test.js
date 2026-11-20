import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { HOST_NAME, HOST_NAMES, HOST_PROFILE, hostNameFor } from "../../extension/host-config.js";
import { profileOf, withProfile } from "../../scripts/set-host-profile.mjs";

const extensionDir = new URL("../../extension/", import.meta.url);

test("the three host names are distinct and the product one is the io.github name", () => {
  assert.equal(HOST_NAMES.product, "io.github.memocampus.vault");
  assert.equal(HOST_NAMES.dev, "io.github.memocampus.vault.dev");
  assert.equal(HOST_NAMES.legacyDev, "com.memocampus.vault");
  assert.equal(new Set(Object.values(HOST_NAMES)).size, 3);
});

test("the committed profile keeps the existing development registration working", () => {
  assert.equal(HOST_PROFILE, "legacyDev");
  assert.equal(HOST_NAME, "com.memocampus.vault");
});

test("an unknown profile is an error, never another host", () => {
  for (const bad of ["", "prod", "PRODUCT", "__proto__", "toString", undefined, null]) {
    assert.throws(() => hostNameFor(bad), /Unknown native host profile/);
  }
});

test("no extension script names a host except host-config.js", () => {
  const names = Object.values(HOST_NAMES);
  for (const entry of readdirSync(extensionDir)) {
    if (!entry.endsWith(".js") || entry === "host-config.js") continue;
    const text = readFileSync(new URL(entry, extensionDir), "utf8");
    for (const name of names) assert.ok(!text.includes(name), `${entry} names ${name}`);
    assert.ok(!/com\.memocampus|io\.github\.memocampus/.test(text), `${entry} has a host-like literal`);
  }
});

test("every connectNative and sendNativeMessage call uses HOST_NAME", () => {
  for (const entry of readdirSync(extensionDir)) {
    if (!entry.endsWith(".js")) continue;
    const text = readFileSync(new URL(entry, extensionDir), "utf8");
    for (const match of text.matchAll(/(connectNative|sendNativeMessage)\(\s*([^,)\s]+)/g)) {
      assert.equal(match[2], "HOST_NAME", `${entry}: ${match[0]}`);
    }
  }
});

test("the manifest has no host name and no key (the extension id stays what the store or path gives)", () => {
  const manifest = JSON.parse(readFileSync(new URL("manifest.json", extensionDir), "utf8"));
  assert.ok(!("key" in manifest));
  assert.ok(manifest.permissions.includes("nativeMessaging"));
});

test("set-host-profile rewrites exactly the one line and nothing else", () => {
  const text = readFileSync(new URL("host-config.js", extensionDir), "utf8");
  assert.equal(profileOf(text), "legacyDev");
  const product = withProfile(text, "product");
  assert.equal(profileOf(product), "product");
  assert.equal(product.split("\n").length, text.split("\n").length);
  assert.equal(withProfile(product, "legacyDev"), text);
  for (const bad of ["Product", "", "com.memocampus.vault", "dev; x"]) assert.throws(() => withProfile(text, bad));
  assert.throws(() => profileOf("export const HOST_PROFILE = prod;"));
});
