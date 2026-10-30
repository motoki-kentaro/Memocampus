import { test } from "node:test";
import assert from "node:assert/strict";
import { EXPORT_TIMEOUT_MS, MAX_DECODE_PIXELS, READ_CHUNK_BYTES, copyImage, exportImage, readOriginal, rebuildThumbnail, toPng } from "../../extension/image-original.js";
import { MAX_IMAGE_BYTES } from "../../extension/image-format.js";
import { SearchError } from "../../extension/search-client.js";
import { FakeCanvas, fakeDecoder, gif, jpeg, png, sha256 } from "./image-fixtures.js";

const TOKEN = "ab".repeat(32);
const OTHER = "cd".repeat(32);

function itemOf(bytes, mimeType = "image/png", extra = {}) {
  return { blobId: 7, sha256: sha256(bytes), byteLength: bytes.length, mimeType, width: 40, height: 30, ...extra };
}

/** One-shot host for image_read; `tamper(reply, message, index)` may change or replace each reply. */
function readHost(bytes, item, { tamper = (reply) => reply, chunk = READ_CHUNK_BYTES } = {}) {
  const requests = [];
  const send = async (message) => {
    requests.push(message);
    const data = Buffer.from(bytes.subarray(message.offset, message.offset + chunk)).toString("base64");
    const reply = { ok: true, type: "image_read_result", requestId: message.requestId, vaultToken: TOKEN, blobId: item.blobId, sha256: item.sha256, mimeType: item.mimeType, byteLength: bytes.length, offset: message.offset, data };
    return tamper(reply, message, requests.length - 1);
  };
  return { send, requests };
}

const fails = (promise, code) => assert.rejects(promise, (error) => error instanceof SearchError && error.code === code);

test("the host's chunk size and the reply budget agree", () => {
  assert.equal(READ_CHUNK_BYTES, 512 * 1024);
  const base64 = Math.ceil(READ_CHUNK_BYTES / 3) * 4;
  assert.ok(base64 + 1000 < 1_000_000 * 0.75, "a chunk reply leaves over 25 % of Chrome's 1 MB limit free");
  assert.equal(MAX_IMAGE_BYTES / READ_CHUNK_BYTES, 64);
});

test("an original is read in order, chunk by chunk, and must match its size and hash", async () => {
  const bytes = png(40, 30, 3 * READ_CHUNK_BYTES + 777);
  const item = itemOf(bytes);
  const host = readHost(bytes, item);
  const read = await readOriginal(item, TOKEN, { send: host.send });
  assert.deepEqual(Buffer.from(read), Buffer.from(bytes));
  assert.deepEqual(host.requests.map((m) => m.offset), [0, 1, 2, 3].map((i) => i * READ_CHUNK_BYTES));
  for (const message of host.requests) {
    assert.equal(message.type, "image_read");
    assert.equal(message.vaultToken, TOKEN, "every chunk request names the vault");
    assert.equal(message.sha256, item.sha256);
    assert.equal(message.blobId, 7);
  }
});

test("missing, reordered, short, oversized or altered chunks are rejected", async () => {
  const bytes = png(40, 30, 2 * READ_CHUNK_BYTES + 10);
  const item = itemOf(bytes);
  const cases = [
    ["a chunk from the wrong offset", (r, m, i) => (i === 1 ? { ...r, offset: 0 } : r), "invalid_response"],
    ["a repeated first chunk", (r, m, i) => (i === 1 ? { ...r, data: Buffer.from(bytes.subarray(0, READ_CHUNK_BYTES)).toString("base64") } : r), "read_hash_mismatch"],
    ["a short chunk", (r, m, i) => (i === 0 ? { ...r, data: Buffer.from(bytes.subarray(0, 1000)).toString("base64") } : r), "read_incomplete"],
    ["an empty last chunk", (r, m, i) => (i === 2 ? { ...r, data: "" } : r), "read_incomplete"],
    ["a flipped byte", (r, m, i) => {
      if (i !== 1) return r;
      const copy = Buffer.from(r.data, "base64");
      copy[5] ^= 1;
      return { ...r, data: copy.toString("base64") };
    }, "read_hash_mismatch"],
    ["another size", (r) => ({ ...r, byteLength: bytes.length + 1 }), "invalid_response"],
    ["another hash", (r) => ({ ...r, sha256: "0".repeat(64) }), "invalid_response"],
    ["another type", (r) => ({ ...r, mimeType: "image/jpeg" }), "invalid_response"],
    ["another blob", (r) => ({ ...r, blobId: 8 }), "invalid_response"],
    ["broken Base64", (r, m, i) => (i === 1 ? { ...r, data: "@@@" } : r), "invalid_response"],
    ["a reply for another request", (r) => ({ ...r, requestId: "other" }), "invalid_response"],
    ["a host error mid-way", (r, m, i) => (i === 1 ? { ok: false, type: "image_read_result", requestId: m.requestId, error: "db_error" } : r), "db_error"],
  ];
  for (const [name, tamper, code] of cases) {
    await fails(readOriginal(item, TOKEN, { send: readHost(bytes, item, { tamper }).send }), code).catch((error) => {
      throw new Error(`${name}: ${error.message}`);
    });
  }
});

test("a vault switch during the read stops it; chunks from two vaults are never combined", async () => {
  const bytes = png(40, 30, 2 * READ_CHUNK_BYTES + 10);
  const item = itemOf(bytes);
  // The host answers from another vault from the second chunk on.
  await fails(readOriginal(item, TOKEN, { send: readHost(bytes, item, { tamper: (r, m, i) => (i >= 1 ? { ...r, vaultToken: OTHER } : r) }).send }), "vault_switched");
  // The panel switched vaults: the next answer is dropped and nothing more is asked.
  let current = true;
  const host = readHost(bytes, item, { tamper: (r) => ((current = false), r) });
  await fails(readOriginal(item, TOKEN, { send: host.send, isCurrent: () => current }), "vault_switched");
  assert.equal(host.requests.length, 1);
});

test("a lost host fails the read instead of returning part of the image", async () => {
  const bytes = png(40, 30, READ_CHUNK_BYTES + 10);
  const item = itemOf(bytes);
  let calls = 0;
  const send = async (message) => {
    calls += 1;
    if (calls === 2) throw new SearchError("host_unavailable");
    return readHost(bytes, item).send(message);
  };
  await fails(readOriginal(item, TOKEN, { send }), "host_unavailable");
});

test("sizes beyond the vault limit are refused before any memory is reserved", async () => {
  const host = readHost(new Uint8Array(1), itemOf(new Uint8Array(1)));
  await fails(readOriginal({ ...itemOf(new Uint8Array(1)), byteLength: MAX_IMAGE_BYTES + 1 }, TOKEN, { send: host.send }), "too_large");
  await fails(readOriginal({ ...itemOf(new Uint8Array(1)), byteLength: 0 }, TOKEN, { send: host.send }), "too_large");
  assert.equal(host.requests.length, 0);
});

// --- Clipboard ---------------------------------------------------------------------------------

function fakeClipboard({ reject = null, promiseItems = true } = {}) {
  const clipboard = {
    writes: [],
    calledAt: [],
    async write(items) {
      clipboard.calledAt.push(clipboard.stage);
      if (reject) {
        // Like Chrome: the write settles only after the item's data does.
        await Promise.allSettled(items.flatMap((item) => Object.values(item.data)));
        throw reject;
      }
      const blobs = [];
      for (const item of items) for (const [type, data] of Object.entries(item.data)) blobs.push({ type, blob: await data });
      clipboard.writes.push(blobs);
    },
    stage: "sync",
  };
  class ClipboardItem {
    constructor(data) {
      if (!promiseItems && Object.values(data).some((value) => typeof value?.then === "function")) throw new TypeError("promises not supported");
      this.data = data;
    }
  }
  return { clipboard, ClipboardItem };
}

async function copyWith(bytes, mimeType, options = {}) {
  const item = itemOf(bytes, mimeType);
  const host = readHost(bytes, item);
  const fake = fakeClipboard(options);
  const decoder = fakeDecoder(options.decoder);
  const promise = copyImage(item, TOKEN, { send: host.send, ...fake, createImageBitmap: decoder.createImageBitmap, OffscreenCanvas: options.noCanvas ? undefined : FakeCanvas });
  fake.clipboard.stage = "later";
  return { promise, fake, host, decoder };
}

test("copying writes PNG pixels to the clipboard, starting the write within the click", async () => {
  const bytes = png(40, 30, 5000);
  const { promise, fake, decoder } = await copyWith(bytes, "image/png");
  await promise;
  assert.deepEqual(fake.clipboard.calledAt, ["sync"], "clipboard.write is called before anything is awaited");
  const [[entry]] = fake.clipboard.writes;
  assert.equal(entry.type, "image/png");
  assert.deepEqual(Buffer.from(await entry.blob.arrayBuffer()), Buffer.from(bytes), "a PNG original is copied as it is");
  assert.equal(decoder.calls.length, 0);
});

test("JPEG, GIF, WebP and AVIF originals are converted to PNG for the clipboard only", async () => {
  for (const [bytes, mimeType] of [[jpeg(40, 30), "image/jpeg"], [gif(40, 30), "image/gif"]]) {
    const original = Buffer.from(bytes);
    const { promise, fake, decoder } = await copyWith(bytes, mimeType);
    await promise;
    const [[entry]] = fake.clipboard.writes;
    assert.equal(entry.type, "image/png");
    const copied = new Uint8Array(await entry.blob.arrayBuffer());
    assert.equal(copied[0], 0x89, `${mimeType} becomes PNG`);
    assert.equal(decoder.calls[0].type, mimeType);
    assert.deepEqual(Buffer.from(bytes), original, "the original bytes are not changed");
  }
  // WebP and AVIF go the same way (decoded by the browser, encoded as PNG).
  for (const mimeType of ["image/webp", "image/avif", "image/bmp"]) {
    const png8 = await toPng(new Uint8Array([1, 2, 3]), { mimeType, width: 2, height: 2 }, { createImageBitmap: async () => ({ width: 2, height: 2, close() {} }), OffscreenCanvas: FakeCanvas });
    assert.equal(png8.type, "image/png", mimeType);
  }
});

test("copy failures are reported, never shown as success", async () => {
  const bytes = jpeg(40, 30);
  const denied = Object.assign(new Error("Document is not focused."), { name: "NotAllowedError" });
  await fails((await copyWith(bytes, "image/jpeg", { reject: denied })).promise, "clipboard_denied");
  await fails((await copyWith(bytes, "image/jpeg", { reject: new Error("boom") })).promise, "clipboard_failed");
  await fails((await copyWith(bytes, "image/jpeg", { decoder: { broken: true } })).promise, "decode_failed");
  await fails((await copyWith(bytes, "image/jpeg", { noCanvas: true })).promise, "copy_convert_failed");
  const huge = itemOf(bytes, "image/jpeg", { width: 10_000, height: MAX_DECODE_PIXELS / 10_000 + 1 });
  await fails(toPng(bytes, huge, { createImageBitmap: async () => assert.fail("not decoded"), OffscreenCanvas: FakeCanvas }), "copy_too_large");
  // A read that fails is the reason given, not the clipboard's refusal that follows from it.
  const item = itemOf(bytes, "image/jpeg");
  const host = readHost(bytes, item, { tamper: (r) => ({ ...r, vaultToken: OTHER }) });
  const fake = fakeClipboard({ reject: Object.assign(new Error("x"), { name: "NotAllowedError" }) });
  await fails(copyImage(item, TOKEN, { send: host.send, ...fake, createImageBitmap: fakeDecoder().createImageBitmap, OffscreenCanvas: FakeCanvas }), "vault_switched");
  await fails(copyImage(item, TOKEN, { send: host.send }), "clipboard_unavailable");
});

test("without promise support in ClipboardItem the PNG is written once it is ready", async () => {
  const bytes = png(40, 30, 3000);
  const { promise, fake } = await copyWith(bytes, "image/png", { promiseItems: false });
  await promise;
  assert.deepEqual(fake.clipboard.calledAt, ["later"]);
  assert.equal(fake.clipboard.writes.length, 1);
});

// --- Export and thumbnails -----------------------------------------------------------------------

test("export asks the host with the vault and hash, waits for the dialog, and reports cancel", async () => {
  const item = itemOf(png(4, 4));
  const seen = [];
  const answer = (fields) => async (message) => (seen.push(message), { ok: true, type: "image_export_result", requestId: message.requestId, vaultToken: TOKEN, blobId: 7, ...fields });
  assert.deepEqual(await exportImage(item, TOKEN, { send: answer({ fileName: "1730000000.png" }) }), { cancelled: false, fileName: "1730000000.png" });
  assert.deepEqual(await exportImage(item, TOKEN, { send: answer({ cancelled: true }) }), { cancelled: true });
  assert.deepEqual(Object.keys(seen[0]).sort(), ["blobId", "requestId", "sha256", "type", "vaultToken"], "no path is ever sent");
  await fails(exportImage(item, TOKEN, { send: answer({ vaultToken: OTHER, fileName: "x.png" }) }), "vault_switched");
  await fails(exportImage(item, TOKEN, { send: answer({}) }), "invalid_response");
  assert.ok(EXPORT_TIMEOUT_MS >= 10 * 60 * 1000, "the dialog may stay open for minutes");
});

test("a thumbnail is rebuilt from the original like at save time and sent for that vault", async () => {
  const bytes = png(640, 320, 2000);
  const item = itemOf(bytes, "image/png", { width: 640, height: 320 });
  const read = readHost(bytes, item);
  const puts = [];
  const send = async (message) => {
    if (message.type === "image_read") return read.send(message);
    puts.push(message);
    return { ok: true, type: "image_thumbnail_put_result", requestId: message.requestId, vaultToken: TOKEN, blobId: 7, stored: true };
  };
  const thumbnail = await rebuildThumbnail(item, TOKEN, { send, createImageBitmap: fakeDecoder().createImageBitmap, OffscreenCanvas: FakeCanvas });
  assert.equal(thumbnail.width, 320);
  assert.equal(thumbnail.height, 160);
  assert.equal(puts.length, 1);
  assert.equal(puts[0].vaultToken, TOKEN);
  assert.equal(puts[0].sha256, item.sha256);
  assert.equal(puts[0].thumbnail.generator, 1);
  assert.deepEqual(Object.keys(puts[0]).sort(), ["blobId", "requestId", "sha256", "thumbnail", "type", "vaultToken"]);
  // The panel switched vaults while decoding: nothing is sent.
  puts.length = 0;
  await fails(rebuildThumbnail(item, TOKEN, { send, isCurrent: () => false, createImageBitmap: fakeDecoder().createImageBitmap, OffscreenCanvas: FakeCanvas }), "vault_switched");
  assert.equal(puts.length, 0);
  // Undecodable originals give no thumbnail and send nothing.
  assert.equal(await rebuildThumbnail(item, TOKEN, { send, createImageBitmap: fakeDecoder({ broken: true }).createImageBitmap, OffscreenCanvas: FakeCanvas }), null);
  assert.equal(puts.length, 0);
});
