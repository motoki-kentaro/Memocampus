import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { extensionDir } from "./fake-dom.js";

const repoRoot = path.resolve(extensionDir, "..");
const manifest = JSON.parse(readFileSync(path.join(extensionDir, "manifest.json"), "utf8"));
const PNG_SIGNATURE = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);

function pngSize(file) {
  const bytes = readFileSync(file);
  assert.ok(bytes.subarray(0, 8).equals(PNG_SIGNATURE), `${file} is a PNG`);
  assert.equal(bytes.toString("latin1", 12, 16), "IHDR");
  return [bytes.readUInt32BE(16), bytes.readUInt32BE(20)];
}

test("the manifest's icons are the generated PNGs at their declared sizes", () => {
  assert.deepEqual(Object.keys(manifest.icons), ["16", "32", "48", "128"]);
  for (const icons of [manifest.icons, manifest.action.default_icon]) {
    for (const [size, file] of Object.entries(icons)) {
      assert.equal(file, `icons/icon-${size}.png`);
      assert.deepEqual(pngSize(path.join(extensionDir, file)), [Number(size), Number(size)], file);
    }
  }
  assert.deepEqual(Object.keys(manifest.action.default_icon), ["16", "32"]);
});

test("the host icon holds 16 to 256 px images and the master stays square", () => {
  const ico = readFileSync(path.join(repoRoot, "native-host", "resources", "memocampus.ico"));
  assert.deepEqual([ico.readUInt16LE(0), ico.readUInt16LE(2)], [0, 1], "an icon file");
  const sizes = [];
  for (let index = 0; index < ico.readUInt16LE(4); index++) {
    const entry = 6 + 16 * index;
    const size = ico[entry] || 256;
    const offset = ico.readUInt32LE(entry + 12);
    const image = ico.subarray(offset, offset + ico.readUInt32LE(entry + 8));
    assert.ok(image.subarray(0, 8).equals(PNG_SIGNATURE), `${size} px entry is a PNG`);
    assert.equal(image.readUInt32BE(16), size);
    sizes.push(size);
  }
  assert.deepEqual(sizes, [16, 24, 32, 48, 64, 128, 256]);
  const [width, height] = pngSize(path.join(repoRoot, "assets", "icon.png"));
  assert.equal(width, height);
});
