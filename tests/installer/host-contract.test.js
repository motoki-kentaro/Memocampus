// The Native Messaging host-name contract across the extension, the packager, the installer and the
// development scripts (Issue #67). Each file is read as it is in the repository; nothing is changed.
//
// Product: the extension's HOST_NAMES.product, the packager's PRODUCT_HOST, the installer's
// ProductHostName and the product host of the Inno self-tests (and run-selftest.ps1's manifest check)
// must be one name. Development: the host names the registration scripts accept must be exactly the
// extension's dev and legacyDev names, their default and the template name must be legacyDev, and none of
// them may accept the product name. The packaged ZIP's host-config.js must, when loaded, select the
// installer's product host.
import assert from "node:assert/strict";
import test from "node:test";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { inflateRawSync } from "node:zlib";
import { packageExtension } from "../../scripts/package-extension.mjs";

const ROOT = fileURLToPath(new URL("../../", import.meta.url));

const FILES = {
  hostConfig: "extension/host-config.js",
  packager: "scripts/package-extension.mjs",
  iss: "installer/memocampus.iss",
  nmhSelftest: "installer/nmh-selftest.iss",
  lifecycleSelftest: "installer/lifecycle-selftest.iss",
  runSelftest: "installer/run-selftest.ps1",
  installScript: "native-host/install-native-host.ps1",
  uninstallScript: "native-host/uninstall-native-host.ps1",
  template: "native-host/com.memocampus.vault.template.json",
};

function readAll() {
  return Object.fromEntries(Object.entries(FILES).map(([k, f]) => [k, readFileSync(join(ROOT, f), "utf8")]));
}

// Every value is taken from exactly one match; a missing or repeated definition is itself a problem.
function only(text, re, what) {
  const all = [...text.matchAll(new RegExp(re.source, re.flags.includes("g") ? re.flags : re.flags + "g"))];
  if (all.length !== 1) throw new Error(`${what}: expected one definition, found ${all.length}`);
  return all[0];
}

function hostNamesOf(text) {
  const table = only(text, /export const HOST_NAMES = Object\.freeze\(\{([^}]*)\}\)/, "host-config.js HOST_NAMES")[1];
  const names = {};
  for (const [, key, value] of table.matchAll(/(\w+):\s*"([^"]+)"/g)) names[key] = value;
  return names;
}

function validateSetOf(text, what) {
  const body = only(text, /\[ValidateSet\(([^)]*)\)\]\s*\[string\]\$HostName/, `${what} ValidateSet`)[1];
  return [...body.matchAll(/'([^']+)'/g)].map((m) => m[1]).sort();
}

export function contractOf(t) {
  return {
    hostNames: hostNamesOf(t.hostConfig),
    packagerProduct: only(t.packager, /const PRODUCT_HOST = "([^"]+)";/, "package-extension.mjs PRODUCT_HOST")[1],
    packagerOthers: JSON.parse(only(t.packager, /const OTHER_HOSTS = (\[[^\]]*\]);/, "package-extension.mjs OTHER_HOSTS")[1]).sort(),
    issProduct: only(t.iss, /^#define ProductHostName "([^"]+)"/m, "memocampus.iss ProductHostName")[1],
    nmhSelftestHost: only(t.nmhSelftest, /^\s*Host = '([^']+)';/m, "nmh-selftest.iss Host")[1],
    nmhSelftestManifest: only(t.nmhSelftest, /'  "name": "([^"]+)",'/, "nmh-selftest.iss manifest name")[1],
    lifecycleSelftestHost: only(t.lifecycleSelftest, /^\s*Host = '([^']+)';/m, "lifecycle-selftest.iss Host")[1],
    runSelftestName: only(t.runSelftest, /\$doc\.name -eq '([^']+)'/, "run-selftest.ps1 manifest name")[1],
    installSet: validateSetOf(t.installScript, "install-native-host.ps1"),
    installDefault: only(t.installScript, /\[string\]\$HostName = '([^']+)'/, "install-native-host.ps1 default")[1],
    installRefusesTemplateOtherThan: only(t.installScript, /if \(\$template\.name -ne '([^']+)'/, "install-native-host.ps1 template check")[1],
    uninstallSet: validateSetOf(t.uninstallScript, "uninstall-native-host.ps1"),
    uninstallDefault: only(t.uninstallScript, /\[string\]\$HostName = '([^']+)'/, "uninstall-native-host.ps1 default")[1],
    templateName: JSON.parse(t.template).name,
  };
}

export function contractProblems(c) {
  const problems = [];
  const { product, dev, legacyDev } = c.hostNames;
  if (!product || !dev || !legacyDev || Object.keys(c.hostNames).length !== 3) problems.push("HOST_NAMES must hold exactly product, dev and legacyDev");
  if (new Set([product, dev, legacyDev]).size !== 3) problems.push("the three host names must differ");
  const productSites = {
    "package-extension.mjs PRODUCT_HOST": c.packagerProduct,
    "memocampus.iss ProductHostName": c.issProduct,
    "nmh-selftest.iss Host": c.nmhSelftestHost,
    "nmh-selftest.iss manifest name": c.nmhSelftestManifest,
    "lifecycle-selftest.iss Host": c.lifecycleSelftestHost,
    "run-selftest.ps1 manifest name": c.runSelftestName,
  };
  for (const [site, value] of Object.entries(productSites)) {
    if (value !== product) problems.push(`product host mismatch: ${site} is ${value}, HOST_NAMES.product is ${product}`);
  }
  const devPair = [dev, legacyDev].sort();
  if (c.packagerOthers.join() !== devPair.join()) problems.push(`package-extension.mjs OTHER_HOSTS ${c.packagerOthers} != dev/legacyDev ${devPair}`);
  for (const [site, set] of [["install-native-host.ps1", c.installSet], ["uninstall-native-host.ps1", c.uninstallSet]]) {
    if (set.join() !== devPair.join()) problems.push(`dev host mismatch: ${site} accepts ${set}, extension dev/legacyDev are ${devPair}`);
    if (set.includes(product)) problems.push(`${site} accepts the product host ${product}`);
  }
  for (const [site, value] of [
    ["install-native-host.ps1 default", c.installDefault],
    ["uninstall-native-host.ps1 default", c.uninstallDefault],
    ["template name", c.templateName],
    ["install-native-host.ps1 template check", c.installRefusesTemplateOtherThan],
  ]) {
    if (value !== legacyDev) problems.push(`legacy dev host mismatch: ${site} is ${value}, HOST_NAMES.legacyDev is ${legacyDev}`);
  }
  return problems;
}

// Reads one entry back out of a ZIP written by package-extension.mjs (deflate, no extra fields).
function zipEntry(buffer, wanted) {
  const end = buffer.lastIndexOf(Buffer.from([0x50, 0x4b, 0x05, 0x06]));
  assert.ok(end >= 0, "not a zip");
  const count = buffer.readUInt16LE(end + 10);
  let at = buffer.readUInt32LE(end + 16);
  for (let i = 0; i < count; i++) {
    assert.equal(buffer.readUInt32LE(at), 0x02014b50, "central directory entry");
    const method = buffer.readUInt16LE(at + 10);
    const packed = buffer.readUInt32LE(at + 20);
    const nameLength = buffer.readUInt16LE(at + 28);
    const extra = buffer.readUInt16LE(at + 30) + buffer.readUInt16LE(at + 32);
    const local = buffer.readUInt32LE(at + 42);
    const name = buffer.toString("utf8", at + 46, at + 46 + nameLength);
    if (name === wanted) {
      assert.equal(buffer.readUInt32LE(local), 0x04034b50, "local header");
      const start = local + 30 + buffer.readUInt16LE(local + 26) + buffer.readUInt16LE(local + 28);
      const data = buffer.subarray(start, start + packed);
      return method === 8 ? inflateRawSync(data) : Buffer.from(data);
    }
    at += 46 + nameLength + extra;
  }
  throw new Error(`${wanted} is not in the zip`);
}

test("the repository satisfies the host-name contract", () => {
  const c = contractOf(readAll());
  assert.deepEqual(contractProblems(c), []);
  // Pinned once more so a coordinated rename of every site still needs a deliberate edit here.
  assert.equal(c.issProduct, "io.github.memocampus.vault");
});

const mutations = [
  ["installer product host renamed", "iss", '#define ProductHostName "io.github.memocampus.vault"', '#define ProductHostName "io.github.memocampus.vault2"', /memocampus\.iss ProductHostName is io\.github\.memocampus\.vault2/],
  ["extension product host renamed", "hostConfig", 'product: "io.github.memocampus.vault"', 'product: "io.github.memocampus.app"', /product host mismatch/],
  ["packager product host renamed", "packager", 'const PRODUCT_HOST = "io.github.memocampus.vault";', 'const PRODUCT_HOST = "com.memocampus.vault";', /package-extension\.mjs PRODUCT_HOST is com\.memocampus\.vault/],
  ["nmh self-test host renamed", "nmhSelftest", "Host = 'io.github.memocampus.vault';", "Host = 'io.github.memocampus.vaultx';", /nmh-selftest\.iss Host/],
  ["lifecycle self-test host renamed", "lifecycleSelftest", "Host = 'io.github.memocampus.vault';", "Host = 'com.memocampus.vault';", /lifecycle-selftest\.iss Host/],
  ["run-selftest manifest name changed", "runSelftest", "$doc.name -eq 'io.github.memocampus.vault'", "$doc.name -eq 'io.github.memocampus.vault.dev'", /run-selftest\.ps1 manifest name/],
  ["extension dev host renamed", "hostConfig", 'dev: "io.github.memocampus.vault.dev"', 'dev: "io.github.memocampus.vault.devel"', /dev host mismatch: install-native-host\.ps1/],
  ["extension legacy host renamed", "hostConfig", 'legacyDev: "com.memocampus.vault"', 'legacyDev: "com.memocampus.vault2"', /legacy dev host mismatch/],
  ["install script accepts the product host", "installScript", "[ValidateSet('com.memocampus.vault', 'io.github.memocampus.vault.dev')]", "[ValidateSet('com.memocampus.vault', 'io.github.memocampus.vault.dev', 'io.github.memocampus.vault')]", /install-native-host\.ps1 accepts/],
  ["uninstall script default changed", "uninstallScript", "[string]$HostName = 'com.memocampus.vault'", "[string]$HostName = 'io.github.memocampus.vault.dev'", /uninstall-native-host\.ps1 default/],
  ["template renamed", "template", '"name": "com.memocampus.vault"', '"name": "io.github.memocampus.vault"', /template name/],
  ["a second ProductHostName define", "iss", '#define ProductHostName "io.github.memocampus.vault"', '#define ProductHostName "io.github.memocampus.vault"\n#define ProductHostName "x"', /expected one definition, found 2/],
];

for (const [name, key, from, to, expected] of mutations) {
  test(`a contract mismatch is detected: ${name}`, () => {
    const texts = readAll();
    assert.ok(texts[key].includes(from), `the mutation anchor is still in ${FILES[key]}`);
    texts[key] = texts[key].replace(from, to);
    let problems;
    try {
      problems = contractProblems(contractOf(texts));
    } catch (error) {
      problems = [error.message];
    }
    assert.ok(problems.some((p) => expected.test(p)), `expected ${expected}, got ${JSON.stringify(problems)}`);
  });
}

test("the packaged ZIP's host-config.js, when loaded, selects the installer's product host", async () => {
  const issProduct = contractOf(readAll()).issProduct;
  const base = mkdtempSync(join(tmpdir(), "memocampus-contract-"));
  try {
    const { zipPath } = packageExtension({ srcDir: join(ROOT, "extension"), outDir: join(base, "out") });
    const config = zipEntry(readFileSync(zipPath), "host-config.js");
    const module = join(base, "packaged-host-config.mjs");
    writeFileSync(module, config);
    const loaded = await import(pathToFileURL(module).href);
    assert.equal(loaded.HOST_PROFILE, "product");
    assert.equal(loaded.HOST_NAME, issProduct);
    assert.equal(loaded.HOST_NAMES.product, issProduct);
  } finally {
    rmSync(base, { recursive: true, force: true });
  }
});
