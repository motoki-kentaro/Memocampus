import { test } from "node:test";
import assert from "node:assert/strict";
import { ImageSaveError } from "../../extension/image-format.js";
import { MAX_ATTEMPTS, MAX_CHUNK_BYTES, uploadImage } from "../../extension/image-upload.js";
import { fakeImageHost, png, sha256 } from "./image-fixtures.js";

const SAVE_ID = "6a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d";

function image(bytes, overrides = {}) {
  return {
    saveId: SAVE_ID,
    bytes,
    sha256: sha256(bytes),
    mimeType: "image/png",
    width: 10,
    height: 10,
    imageUrl: "https://img.test/a.png",
    pageUrl: "https://page.test/",
    frameUrl: null,
    incognito: false,
    thumbnail: { mimeType: "image/webp", width: 4, height: 4, generator: 1, bytes: png(4, 4) },
    ...overrides,
  };
}

const deps = (host, extra = {}) => ({ connect: host.connect, runtime: { lastError: undefined }, timeoutMs: 500, ...extra });
const sent = (host, type) => host.received.filter((message) => message.type === type);

test("a large image is sent as ordered chunks of at most 1 MiB and saved once", async () => {
  const host = fakeImageHost();
  const bytes = png(10, 10, 2 * MAX_CHUNK_BYTES + 4321);
  const saved = await uploadImage(image(bytes), deps(host));
  assert.equal(saved.type, "image_saved");
  assert.equal(saved.replayed, false);
  const chunks = sent(host, "image_save_chunk");
  assert.deepEqual(chunks.map((c) => c.index), [0, 1, 2]);
  const raw = chunks.map((c) => Buffer.from(c.data, "base64"));
  assert.ok(raw.every((part) => part.length <= MAX_CHUNK_BYTES));
  assert.ok(chunks.every((c) => c.data.length < 1.5 * 1024 * 1024), "Base64 message size stays bounded");
  assert.deepEqual(new Uint8Array(Buffer.concat(raw)), bytes);
  const begin = sent(host, "image_save_begin")[0];
  assert.deepEqual(
    { protocol: begin.protocol, byteLength: begin.byteLength, sha256: begin.sha256, imageUrl: begin.imageUrl, pageUrl: begin.pageUrl },
    { protocol: 2, byteLength: bytes.length, sha256: sha256(bytes), imageUrl: "https://img.test/a.png", pageUrl: "https://page.test/" },
  );
  assert.equal(sent(host, "image_save_commit")[0].thumbnail.mimeType, "image/webp");
  assert.equal(host.history.length, 1);
});

test("bytes already in the vault are not sent again", async () => {
  const host = fakeImageHost();
  const bytes = png(10, 10, 5000);
  await uploadImage(image(bytes), deps(host));
  const before = sent(host, "image_save_chunk").length;
  const second = await uploadImage(image(bytes, { saveId: "7b2c3d4e-5f60-4b7c-9d8e-0f1a2b3c4d5e" }), deps(host));
  assert.equal(second.deduplicated, true);
  assert.equal(sent(host, "image_save_chunk").length, before, "no chunk for a known blob");
  assert.equal(host.history.length, 2, "but a new history entry");
});

test("a connection lost mid-transfer is retried with the same save id and saved exactly once", async () => {
  const host = fakeImageHost();
  let dropped = false;
  host.dropAfter = (message) => message.type === "image_save_chunk" && message.index === 1 && !dropped && (dropped = true);
  const bytes = png(10, 10, 3 * MAX_CHUNK_BYTES);
  const saved = await uploadImage(image(bytes), deps(host));
  assert.equal(saved.type, "image_saved");
  assert.equal(host.ports, 2);
  assert.ok(sent(host, "image_save_begin").every((m) => m.saveId === SAVE_ID));
  assert.equal(host.history.length, 1);
});

test("an answer lost after the commit is recovered without a second history entry", async () => {
  const host = fakeImageHost();
  let dropped = false;
  host.dropAfter = (message) => message.type === "image_save_commit" && !dropped && (dropped = true);
  const saved = await uploadImage(image(png(10, 10)), deps(host));
  assert.equal(saved.replayed, true, "the retry learns the first commit succeeded");
  assert.equal(host.history.length, 1);
});

test("a second loss is reported, not retried forever", async () => {
  const host = fakeImageHost();
  host.dropAfter = (message) => message.type === "image_save_chunk";
  await assert.rejects(uploadImage(image(png(10, 10)), deps(host)), { code: "transfer_interrupted" });
  assert.equal(host.ports, 2);
  assert.equal(host.history.length, 0);
});

test("a connection lost before the host answered is not retried: the vault was never fixed", async () => {
  const host = fakeImageHost();
  host.dropAfter = (message) => message.type === "image_save_begin";
  await assert.rejects(uploadImage(image(png(10, 10)), deps(host)), { code: "transfer_interrupted" });
  assert.equal(host.ports, 1);
});

test("never more than three attempts, all with the same save id", async () => {
  const host = fakeImageHost();
  // 1st: thumbnail rejected at commit. 2nd (no thumbnail): connection lost. 3rd: lost again.
  host.rejectThumbnail = true;
  host.dropAfter = (message) => message.type === "image_save_chunk" && sent(host, "image_save_commit").length > 0;
  await assert.rejects(uploadImage(image(png(10, 10)), deps(host)), { code: "transfer_interrupted" });
  assert.equal(host.ports, 3);
  assert.equal(MAX_ATTEMPTS, 3);
  assert.ok(sent(host, "image_save_begin").every((m) => m.saveId === SAVE_ID));
});

test("a reconnect that finds another vault active stops the save instead of moving it", async () => {
  const host = fakeImageHost();
  let dropped = false;
  host.dropAfter = (message) => {
    if (message.type !== "image_save_chunk" || dropped) return false;
    dropped = true;
    host.vaultToken = "b".repeat(64); // the user switched vaults while the port was down
    return true;
  };
  await assert.rejects(uploadImage(image(png(10, 10)), deps(host)), { code: "vault_switched" });
  const begins = sent(host, "image_save_begin");
  assert.equal(begins.length, 2);
  assert.equal(begins[0].vaultToken, undefined, "the first attempt fixes the vault");
  assert.equal(begins[1].vaultToken, "a".repeat(64), "the retry insists on it");
  assert.equal(host.history.length, 0);
});

test("a resend after a lost commit answer obeys the same vault rule", async () => {
  const host = fakeImageHost();
  let dropped = false;
  host.dropAfter = (message) => {
    if (message.type !== "image_save_commit" || dropped) return false;
    dropped = true;
    host.vaultToken = "c".repeat(64);
    return true;
  };
  await assert.rejects(uploadImage(image(png(10, 10)), deps(host)), { code: "vault_switched" });
  assert.equal(host.history.length, 1, "the first commit stands; nothing is added elsewhere");
});

test("a reply naming another vault than the one fixed earlier is refused", async () => {
  const host = fakeImageHost();
  const realConnect = host.connect;
  host.connect = () => {
    const port = realConnect();
    const post = port.postMessage.bind(port);
    port.postMessage = (message) => {
      if (message.type === "image_save_commit") host.vaultToken = "d".repeat(64); // host misbehaves
      return post(message);
    };
    return port;
  };
  await assert.rejects(uploadImage(image(png(10, 10)), deps(host)), { code: "vault_switched" });
});

test("host errors fail the save, and a rejected thumbnail is retried without it", async () => {
  const host = fakeImageHost();
  host.failCommit = "vault_not_configured";
  await assert.rejects(uploadImage(image(png(10, 10)), deps(host)), (e) => e instanceof ImageSaveError && e.code === "vault_not_configured");
  assert.equal(host.history.length, 0);

  const picky = fakeImageHost();
  picky.rejectThumbnail = true;
  const saved = await uploadImage(image(png(10, 10)), deps(picky));
  assert.equal(saved.type, "image_saved");
  assert.equal(picky.history[0].thumbnail, null, "saved without the rejected thumbnail");
});

test("an old host without image support and a missing host are told apart", async () => {
  const old = fakeImageHost();
  old.unknown = true;
  await assert.rejects(uploadImage(image(png(10, 10)), deps(old)), { code: "host_outdated" });

  const missing = {
    connect() {
      const listeners = [];
      setTimeout(() => listeners.forEach((l) => l()), 0);
      return { onMessage: { addListener() {} }, onDisconnect: { addListener: (l) => listeners.push(l) }, postMessage() {}, disconnect() {} };
    },
  };
  await assert.rejects(
    uploadImage(image(png(10, 10)), { connect: missing.connect, runtime: { lastError: { message: "Specified native messaging host not found." } }, timeoutMs: 200 }),
    { code: "host_unavailable" },
  );
});

test("a silent host times out instead of hanging", async () => {
  const silent = { connect: () => ({ onMessage: { addListener() {} }, onDisconnect: { addListener() {} }, postMessage() {}, disconnect() {} }) };
  await assert.rejects(uploadImage(image(png(10, 10)), { connect: silent.connect, runtime: {}, timeoutMs: 30 }), { code: "timeout" });
});

test("success is reported only after the host's commit answer", async () => {
  const host = fakeImageHost();
  let release;
  host.commitDelay = new Promise((resolve) => (release = resolve));
  let settled = false;
  const pending = uploadImage(image(png(10, 10)), deps(host, { timeoutMs: 2000 })).then((r) => ((settled = true), r));
  await new Promise((resolve) => setTimeout(resolve, 50));
  assert.equal(sent(host, "image_save_commit").length, 1, "the commit was sent");
  assert.equal(settled, false, "but nothing is reported before the host confirms it");
  release();
  assert.equal((await pending).type, "image_saved");
});
