// Builds the Chrome Web Store submission ZIP of the extension (Issue #67, Slice 3B-4).
//
//   node scripts/package-extension.mjs --out <dir> [--expect-version 0.8.0] [--src <extension dir>]
//
// The working copy is never changed. The files are copied to a private temporary folder, the host
// profile is switched to `product` THERE, and only that copy is zipped. Anything that is wrong stops
// the run and leaves no ZIP behind: a ZIP exists only if every check passed.
//
// Checks: the manifest is Manifest V3 with a numeric dotted version (equal to --expect-version when
// given) and no `key` (a key would pin another extension id); HOST_PROFILE in the copy is `product`
// and the host it selects is io.github.memocampus.vault; no other file names a native host (so there
// is no second host to fall back to); nothing that looks like a secret, a source map or a test is in
// the package. The ZIP is deterministic (sorted names, fixed time stamps).
import { cpSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, renameSync, rmSync, statSync, writeFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { tmpdir } from "node:os";
import { dirname, join, relative, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { deflateRawSync } from "node:zlib";
import { profileOf, withProfile } from "./set-host-profile.mjs";

const PRODUCT_HOST = "io.github.memocampus.vault";
const OTHER_HOSTS = ["io.github.memocampus.vault.dev", "com.memocampus.vault"];
const FORBIDDEN_NAME = /(^|[\\/])(\.env[^\\/]*|\.git[^\\/]*|node_modules|[^\\/]*\.(pem|key|pfx|p12|map|log|zip|crx)|[^\\/]*\.test\.[a-z]+)$/i;
const VERSION = /^(0|[1-9][0-9]{0,4})(\.(0|[1-9][0-9]{0,4})){0,3}$/;

function walk(dir) {
  const out = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) out.push(...walk(full));
    else if (entry.isFile()) out.push(full);
    else throw new Error(`not a plain file: ${full}`);
  }
  return out;
}

const CRC_TABLE = (() => {
  const table = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    table[n] = c >>> 0;
  }
  return table;
})();

export function crc32(buffer) {
  let c = 0xffffffff;
  for (const byte of buffer) c = CRC_TABLE[(c ^ byte) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

// A minimal ZIP writer: deflate, UTF-8 names, the DOS time 1980-01-01 00:00 on every entry.
export function makeZip(entries) {
  const locals = [];
  const centrals = [];
  let offset = 0;
  for (const { name, data } of entries) {
    const nameBytes = Buffer.from(name, "utf8");
    const packed = deflateRawSync(data, { level: 9 });
    const crc = crc32(data);
    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0);
    local.writeUInt16LE(20, 4);
    local.writeUInt16LE(0x0800, 6);
    local.writeUInt16LE(8, 8);
    local.writeUInt16LE(0, 10);
    local.writeUInt16LE(0x21, 12);
    local.writeUInt32LE(crc, 14);
    local.writeUInt32LE(packed.length, 18);
    local.writeUInt32LE(data.length, 22);
    local.writeUInt16LE(nameBytes.length, 26);
    locals.push(local, nameBytes, packed);
    const central = Buffer.alloc(46);
    central.writeUInt32LE(0x02014b50, 0);
    central.writeUInt16LE(20, 4);
    central.writeUInt16LE(20, 6);
    central.writeUInt16LE(0x0800, 8);
    central.writeUInt16LE(8, 10);
    central.writeUInt16LE(0, 12);
    central.writeUInt16LE(0x21, 14);
    central.writeUInt32LE(crc, 16);
    central.writeUInt32LE(packed.length, 20);
    central.writeUInt32LE(data.length, 24);
    central.writeUInt16LE(nameBytes.length, 28);
    central.writeUInt32LE(offset, 42);
    centrals.push(central, nameBytes);
    offset += local.length + nameBytes.length + packed.length;
  }
  const centralBytes = Buffer.concat(centrals);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(entries.length, 8);
  end.writeUInt16LE(entries.length, 10);
  end.writeUInt32LE(centralBytes.length, 12);
  end.writeUInt32LE(offset, 16);
  return Buffer.concat([...locals, centralBytes, end]);
}

// Reads the entry names back out of a ZIP made by makeZip (used by the tests and the post-check).
export function zipNames(buffer) {
  const end = buffer.lastIndexOf(Buffer.from([0x50, 0x4b, 0x05, 0x06]));
  if (end < 0) throw new Error("not a zip");
  const count = buffer.readUInt16LE(end + 10);
  let at = buffer.readUInt32LE(end + 16);
  const names = [];
  for (let i = 0; i < count; i++) {
    const nameLength = buffer.readUInt16LE(at + 28);
    const extra = buffer.readUInt16LE(at + 30) + buffer.readUInt16LE(at + 32);
    names.push(buffer.toString("utf8", at + 46, at + 46 + nameLength));
    at += 46 + nameLength + extra;
  }
  return names;
}

export function packageExtension({ srcDir, outDir, expectVersion }) {
  if (!existsSync(join(srcDir, "manifest.json"))) throw new Error(`no manifest.json in ${srcDir}`);
  mkdirSync(outDir, { recursive: true });
  const work = mkdtempSync(join(tmpdir(), "memocampus-ext-"));
  const finalZip = { path: null };
  try {
    const copy = join(work, "extension");
    cpSync(srcDir, copy, { recursive: true });

    const files = walk(copy).map((f) => relative(copy, f).split(sep).join("/")).sort();
    for (const f of files) if (FORBIDDEN_NAME.test(f)) throw new Error(`file must not be in the package: ${f}`);

    const manifest = JSON.parse(readFileSync(join(copy, "manifest.json"), "utf8"));
    if (manifest.manifest_version !== 3) throw new Error("manifest_version must be 3");
    if (typeof manifest.version !== "string" || !VERSION.test(manifest.version)) throw new Error(`manifest.version is not a dotted number: ${manifest.version}`);
    if (expectVersion !== undefined && manifest.version !== expectVersion) {
      throw new Error(`manifest.version is ${manifest.version}, expected ${expectVersion}`);
    }
    if ("key" in manifest) throw new Error("manifest.json must not carry a `key` (it would pin another extension id)");

    const configPath = join(copy, "host-config.js");
    const config = readFileSync(configPath, "utf8");
    writeFileSync(configPath, withProfile(config, "product"));
    const after = readFileSync(configPath, "utf8");
    if (profileOf(after) !== "product") throw new Error("the copy did not switch to the product profile");
    if (!after.includes(`product: "${PRODUCT_HOST}"`)) throw new Error("the product host name is not io.github.memocampus.vault");

    for (const f of files) {
      if (f === "host-config.js") continue;
      if (!/\.(js|json|html|css|md|txt)$/i.test(f)) continue;
      const text = readFileSync(join(copy, f), "utf8");
      for (const other of OTHER_HOSTS) {
        if (text.includes(other) && f !== "THIRD_PARTY_NOTICES.md") throw new Error(`${f} names another native host (${other})`);
      }
    }
    for (const other of OTHER_HOSTS) {
      // host-config.js lists them as the other, never-selected profiles; the selected profile is `product`.
      if (!after.includes(other)) throw new Error(`host-config.js lost the profile table (${other})`);
    }

    const entries = files.map((name) => ({ name, data: readFileSync(join(copy, name)) }));
    const zip = makeZip(entries);
    const names = zipNames(zip);
    if (names.join("\n") !== files.join("\n")) throw new Error("the ZIP does not list the files that were packed");
    const zipName = `memocampus-extension-${manifest.version}.zip`;
    const tmpZip = join(outDir, `${zipName}.partial`);
    finalZip.path = join(outDir, zipName);
    writeFileSync(tmpZip, zip);
    renameSync(tmpZip, finalZip.path);
    const sha256 = createHash("sha256").update(zip).digest("hex");
    writeFileSync(join(outDir, `${zipName}.sha256`), `${sha256}  ${zipName}\n`);
    return { zipPath: finalZip.path, version: manifest.version, files: names, sha256, profile: "product" };
  } catch (error) {
    // A failed run leaves no ZIP (and no partial one) behind.
    if (finalZip.path) rmSync(finalZip.path, { force: true });
    for (const stray of existsSync(outDir) ? readdirSync(outDir) : []) {
      if (stray.endsWith(".partial")) rmSync(join(outDir, stray), { force: true });
    }
    throw error;
  } finally {
    rmSync(work, { recursive: true, force: true });
  }
}

function main(argv) {
  const args = new Map();
  for (let i = 0; i < argv.length; i += 2) args.set(argv[i], argv[i + 1]);
  const here = dirname(fileURLToPath(import.meta.url));
  const out = args.get("--out");
  if (!out) {
    console.error("usage: package-extension.mjs --out <dir> [--expect-version x.y.z] [--src <dir>]");
    return 2;
  }
  try {
    const result = packageExtension({ srcDir: args.get("--src") ?? join(here, "..", "extension"), outDir: out, expectVersion: args.get("--expect-version") });
    console.log(`profile=${result.profile} version=${result.version} files=${result.files.length}`);
    console.log(`${result.sha256}  ${result.zipPath}`);
    return 0;
  } catch (error) {
    console.error(`PACKAGE REFUSED: ${error.message}`);
    return 1;
  }
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) process.exit(main(process.argv.slice(2)));
