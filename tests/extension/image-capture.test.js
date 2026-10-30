import { test } from "node:test";
import assert from "node:assert/strict";
import { acquireImage, classifyImageUrl, fetchInWorker, inspectImage, makeThumbnail, pageFetch, readLimited } from "../../extension/image-capture.js";
import { ImageSaveError, MAX_IMAGE_BYTES, base64ToBytes, bytesToBase64, sha256Hex, sniffImage } from "../../extension/image-format.js";
import { FakeCanvas, avif, bmp, fakeDecoder, gif, jpeg, png, sha256, webp } from "./image-fixtures.js";

function fakeResponse(bytes, { status = 200, headers = {}, redirected = false } = {}) {
  const real = new Response(bytes, { status, headers });
  return { ok: real.ok, status, redirected, headers: real.headers, body: real.body, arrayBuffer: () => real.arrayBuffer() };
}

const rejectsWith = (promise, code, extra = {}) =>
  assert.rejects(promise, (error) => error instanceof ImageSaveError && error.code === code && Object.entries(extra).every(([k, v]) => error.details[k] === v));

test("formats are detected from bytes, mirroring the host", () => {
  assert.equal(sniffImage(png(1, 1)).mimeType, "image/png");
  assert.equal(sniffImage(jpeg(1, 1)).extension, "jpg");
  assert.equal(sniffImage(gif(1, 1)).mimeType, "image/gif");
  assert.equal(sniffImage(webp()).mimeType, "image/webp");
  assert.equal(sniffImage(avif()).mimeType, "image/avif");
  assert.equal(sniffImage(bmp(2, 2)).mimeType, "image/bmp");
  const text = (s) => new TextEncoder().encode(s);
  assert.throws(() => sniffImage(text('<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg"/>')), { code: "svg_not_supported" });
  assert.throws(() => sniffImage(text("\ufeff  <svg><script>alert(1)</script></svg>")), { code: "svg_not_supported" });
  assert.throws(() => sniffImage(text("<!doctype html><html></html>")), { code: "unsupported_format" });
  assert.throws(() => sniffImage(new Uint8Array([0, 1, 2, 3])), { code: "unsupported_format" });
});

test("Base64 and SHA-256 helpers agree with Node", async () => {
  const bytes = png(3, 3, 100_000);
  assert.equal(bytesToBase64(bytes), Buffer.from(bytes).toString("base64"));
  assert.deepEqual(base64ToBytes(bytesToBase64(bytes)), bytes);
  assert.equal(await sha256Hex(bytes), sha256(bytes));
});

test("only http(s), data: and blob: image URLs are accepted", () => {
  assert.equal(classifyImageUrl("https://pbs.twimg.com/media/x?format=jpg&name=small"), "http");
  assert.equal(classifyImageUrl("data:image/png;base64,AAAA"), "data");
  assert.equal(classifyImageUrl("blob:https://example.test/1234"), "blob");
  for (const url of ["file:///C:/a.png", "chrome://favicon/x", "javascript:alert(1)", "ftp://a/b.png", "not a url"]) {
    assert.throws(() => classifyImageUrl(url), { code: "unsupported_url" }, url);
  }
});

test("the worker fetch sends the browser's cookies and keeps only the body", async () => {
  let seen;
  const bytes = png(2, 2);
  const result = await fetchInWorker("https://img.test/a.png", { fetchImpl: async (url, init) => ((seen = { url, init }), fakeResponse(bytes, { redirected: true })) });
  assert.deepEqual(seen.init, { credentials: "include", redirect: "follow", cache: "default" });
  assert.deepEqual(result.bytes, bytes);
  assert.equal(result.redirected, true);
});

test("worker fetch failures are classified", async () => {
  await rejectsWith(fetchInWorker("https://a.test/x", { fetchImpl: async () => { throw new TypeError("Failed to fetch"); } }), "network_error");
  await rejectsWith(fetchInWorker("https://a.test/x", { fetchImpl: async () => fakeResponse(new Uint8Array(1), { status: 403 }) }), "http_error", { status: 403 });
  await rejectsWith(fetchInWorker("https://a.test/x", { fetchImpl: async () => fakeResponse(new Uint8Array(1), { status: 503 }) }), "http_error", { status: 503 });
});

test("the 32 MiB limit is enforced before and while reading the body", async () => {
  const declared = fakeResponse(new Uint8Array(1), { headers: { "content-length": String(MAX_IMAGE_BYTES + 1) } });
  await rejectsWith(readLimited(declared), "too_large");
  await rejectsWith(readLimited(fakeResponse(new Uint8Array(101)), 100), "too_large");
  assert.equal((await readLimited(fakeResponse(new Uint8Array(100)), 100)).length, 100, "exactly at the limit is fine");
  const exact = new Uint8Array(MAX_IMAGE_BYTES);
  assert.equal((await readLimited(fakeResponse(exact))).length, MAX_IMAGE_BYTES);
  await rejectsWith(readLimited(fakeResponse(new Uint8Array(MAX_IMAGE_BYTES + 1))), "too_large");
});

test("an HTTP 4xx or network failure falls back to fetching inside the page's frame", async () => {
  const bytes = gif(4, 4);
  let injected;
  const deps = {
    fetchImpl: async () => fakeResponse(new Uint8Array(1), { status: 403 }),
    executeScript: async (options) => ((injected = options), [{ result: { ok: true, base64: bytesToBase64(bytes) } }]),
  };
  const result = await acquireImage("https://futaba.test/src/1.gif", { tabId: 7, frameIds: [3] }, deps);
  assert.equal(result.via, "page");
  assert.deepEqual(result.bytes, bytes);
  assert.deepEqual(injected.target, { tabId: 7, frameIds: [3] });
  assert.equal(injected.func, pageFetch);
  assert.deepEqual(injected.args, ["https://futaba.test/src/1.gif", MAX_IMAGE_BYTES]);

  // Both failing reports the direct fetch's status, noting the fallback result.
  deps.executeScript = async () => [{ result: { ok: false, error: "network_error" } }];
  await rejectsWith(acquireImage("https://cdn.test/x.jpg", { tabId: 1, frameIds: [0] }, deps), "http_error", { status: 403, pageError: "network_error" });
  // Pages where scripts cannot run (chrome://, Web Store).
  deps.executeScript = async () => { throw new Error("Cannot access contents of the page"); };
  await rejectsWith(acquireImage("https://cdn.test/x.jpg", { tabId: 1, frameIds: [0] }, deps), "http_error", { pageError: "page_access_denied" });
});

test("server errors and oversize images are not retried in the page", async () => {
  let pageCalls = 0;
  const deps = { executeScript: async () => (pageCalls += 1, [{ result: { ok: true, base64: "" } }]) };
  deps.fetchImpl = async () => fakeResponse(new Uint8Array(1), { status: 500 });
  await rejectsWith(acquireImage("https://a.test/x.png", { tabId: 1, frameIds: [0] }, deps), "http_error", { status: 500 });
  deps.fetchImpl = async () => fakeResponse(new Uint8Array(1), { headers: { "content-length": String(MAX_IMAGE_BYTES + 1) } });
  await rejectsWith(acquireImage("https://a.test/x.png", { tabId: 1, frameIds: [0] }, deps), "too_large");
  assert.equal(pageCalls, 0);
});

test("blob: URLs are only readable inside their page; data: URLs are decoded locally", async () => {
  const bytes = png(1, 1);
  let workerCalls = 0;
  const deps = {
    fetchImpl: async (url) => (workerCalls += 1, fakeResponse(url.startsWith("data:") ? bytes : new Uint8Array(0))),
    executeScript: async () => [{ result: { ok: true, base64: bytesToBase64(bytes) } }],
  };
  assert.equal((await acquireImage("blob:https://x.test/1", { tabId: 1, frameIds: [0] }, deps)).via, "page");
  assert.equal(workerCalls, 0);
  await rejectsWith(acquireImage("blob:https://x.test/1", null, deps), "page_access_denied");
  const data = await acquireImage(`data:image/png;base64,${bytesToBase64(bytes)}`, null, deps);
  assert.equal(data.via, "data");
  assert.deepEqual(data.bytes, bytes);
});

test("the in-page fetch returns plain data and enforces the limit there too", async () => {
  const realFetch = globalThis.fetch;
  try {
    globalThis.fetch = async () => fakeResponse(png(2, 2));
    const ok = await pageFetch("https://a.test/x.png", MAX_IMAGE_BYTES);
    assert.equal(ok.ok, true);
    assert.deepEqual(base64ToBytes(ok.base64), png(2, 2));
    globalThis.fetch = async () => fakeResponse(new Uint8Array(1), { status: 404 });
    assert.deepEqual(await pageFetch("https://a.test/x.png", 10), { ok: false, error: "http_error", status: 404 });
    globalThis.fetch = async () => fakeResponse(new Uint8Array(11));
    assert.deepEqual(await pageFetch("https://a.test/x.png", 10), { ok: false, error: "too_large" });
    globalThis.fetch = async () => { throw new TypeError("blocked by CORS"); };
    assert.deepEqual(await pageFetch("https://other.test/x.png", 10), { ok: false, error: "network_error" });
  } finally {
    globalThis.fetch = realFetch;
  }
});

test("decoding validates every format, including JPEG and AVIF, with raw (unrotated) dimensions", async () => {
  const decoder = fakeDecoder({ rotated: true });
  const info = await inspectImage(jpeg(640, 480), decoder);
  assert.deepEqual(info, { mimeType: "image/jpeg", extension: "jpg", width: 640, height: 480 });
  assert.equal(decoder.calls[0].options.imageOrientation, "none", "dimensions match the JPEG header the host checks");
  assert.equal(decoder.calls[0].type, "image/jpeg");
  assert.equal((await inspectImage(avif(), fakeDecoder())).mimeType, "image/avif");
  await rejectsWith(inspectImage(jpeg(5, 5), fakeDecoder({ broken: true })), "decode_failed");
  await rejectsWith(inspectImage(new Uint8Array(0), fakeDecoder()), "empty_image");
  await rejectsWith(inspectImage(new TextEncoder().encode("<svg/>"), fakeDecoder()), "svg_not_supported");
  const tooBig = new Uint8Array(MAX_IMAGE_BYTES + 1);
  tooBig.set(png(1, 1));
  await rejectsWith(inspectImage(tooBig, fakeDecoder()), "too_large");
});

test("thumbnails keep the image's own orientation, fit 320 px and are optional", async () => {
  const thumb = await makeThumbnail(jpeg(1600, 1200), "image/jpeg", { ...fakeDecoder({ rotated: true }), OffscreenCanvas: FakeCanvas });
  assert.deepEqual([thumb.width, thumb.height], [240, 320], "rotated by EXIF, then scaled");
  assert.equal(thumb.mimeType, "image/webp");
  assert.equal(thumb.generator, 1);
  assert.equal(await makeThumbnail(png(1, 1), "image/png", { ...fakeDecoder({ broken: true }), OffscreenCanvas: FakeCanvas }), null);
  assert.equal(await makeThumbnail(png(1, 1), "image/png", { ...fakeDecoder(), OffscreenCanvas: undefined }), null);
  const small = await makeThumbnail(png(10, 5), "image/png", { ...fakeDecoder(), OffscreenCanvas: FakeCanvas });
  assert.deepEqual([small.width, small.height], [10, 5], "never upscaled");
});
