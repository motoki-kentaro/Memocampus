// Test images (structure only, like the host's tests) and fakes for decoding and the native host.
import { createHash } from "node:crypto";

const be32 = (n) => [(n >>> 24) & 255, (n >>> 16) & 255, (n >>> 8) & 255, n & 255];
const le16 = (n) => [n & 255, (n >>> 8) & 255];
const ascii = (text) => [...text].map((c) => c.charCodeAt(0));

/** PNG with the given header size and total length (payload in one IDAT chunk). */
export function png(width, height, size = 0) {
  const head = [0x89, ...ascii("PNG\r\n\x1a\n"), ...be32(13), ...ascii("IHDR"), ...be32(width), ...be32(height), 8, 6, 0, 0, 0, 0, 0, 0, 0];
  const tail = [0, 0, 0, 0, ...ascii("IEND"), 0xae, 0x42, 0x60, 0x82];
  const payload = Math.max(1, size - head.length - 12 - tail.length);
  const bytes = new Uint8Array(head.length + 12 + payload + tail.length);
  bytes.set(head, 0);
  bytes.set([...be32(payload), ...ascii("IDAT")], head.length);
  for (let i = 0; i < payload; i += 1) bytes[head.length + 8 + i] = i % 251;
  bytes.set(tail, bytes.length - tail.length);
  return bytes;
}

export function gif(width, height) {
  return new Uint8Array([...ascii("GIF89a"), ...le16(width), ...le16(height), 0, 0, 0, 0x2c, 0, 0, 0, 0, 1, 0, 1, 0, 0, 2, 2, 0x44, 1, 0, 0x3b]);
}

export function jpeg(width, height) {
  return new Uint8Array([0xff, 0xd8, 0xff, 0xe0, 0, 16, ...ascii("JFIF"), 0, 1, 1, 0, 0, 1, 0, 1, 0, 0, 0xff, 0xc0, 0, 11, 8, height >> 8, height & 255, width >> 8, width & 255, 1, 1, 0x11, 0, 0xff, 0xd9]);
}

export function webp() {
  const chunk = [0x2f, 0, 0, 0, 0, 0];
  return new Uint8Array([...ascii("RIFF"), 4 + 8 + chunk.length, 0, 0, 0, ...ascii("WEBPVP8L"), chunk.length, 0, 0, 0, ...chunk]);
}

export function avif() {
  return new Uint8Array([0, 0, 0, 0x1c, ...ascii("ftypavif"), 0, 0, 0, 0, ...ascii("avifmif1miaf"), 0, 0, 0, 8, ...ascii("meta")]);
}

export function bmp(width, height) {
  const bytes = new Uint8Array(58);
  bytes.set([...ascii("BM"), 58, 0, 0, 0, 0, 0, 0, 0, 54, 0, 0, 0, 40, 0, 0, 0, width & 255, (width >> 8) & 255, 0, 0, height & 255, (height >> 8) & 255, 0, 0]);
  return bytes;
}

export const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");

/** Reads the dimensions the way a decoder would (PNG, GIF, JPEG headers only). */
function headerSize(bytes) {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  if (bytes[0] === 0x89) return [view.getUint32(16), view.getUint32(20)];
  if (bytes[0] === 0x47) return [view.getUint16(6, true), view.getUint16(8, true)];
  if (bytes[0] === 0xff) return [view.getUint16(27), view.getUint16(25)];
  return [10, 10];
}

/**
 * createImageBitmap stand-in. `rotated` simulates EXIF orientation: applied unless the caller asks
 * for imageOrientation "none". `broken` makes decoding fail like a corrupt file.
 */
export function fakeDecoder({ rotated = false, broken = false } = {}) {
  const calls = [];
  const decode = async (blob, options = {}) => {
    calls.push({ type: blob.type, options });
    if (broken) throw new Error("The source image could not be decoded.");
    const bytes = new Uint8Array(await blob.arrayBuffer());
    let [width, height] = headerSize(bytes);
    if (rotated && options.imageOrientation !== "none") [width, height] = [height, width];
    return { width, height, close() {} };
  };
  return { createImageBitmap: decode, calls };
}

/** OffscreenCanvas stand-in producing a valid PNG of the canvas size. */
export class FakeCanvas {
  constructor(width, height) {
    this.width = width;
    this.height = height;
  }
  getContext() {
    return { drawImage() {}, fillRect() {}, fillText() {} };
  }
  async convertToBlob({ type }) {
    // Pretend WebP encoding is unavailable and fall back to what we can produce: a PNG named as asked.
    return new Blob([png(this.width, this.height)], { type: type === "image/webp" ? "image/webp" : type });
  }
}

/**
 * In-memory native host speaking the image protocol over fake ports, with hooks to cut the
 * connection or lose answers.
 */
export function fakeImageHost() {
  const host = {
    blobs: new Map(), // sha256 -> bytes
    saves: new Map(), // saveId -> { blobId, savedImageId, sha256 }
    history: [],
    ports: 0,
    received: [],
    dropAfter: null, // (message) => true to close the port instead of answering
    failCommit: null, // error code for the next commit
    rejectThumbnail: false,
    unknown: false, // behave like a host without image support
    commitDelay: null, // Promise to wait for before answering a commit
    vaultToken: "a".repeat(64), // identity of the active vault; change it to simulate a switch
  };
  let nextId = 1;
  host.connect = () => {
    host.ports += 1;
    const messageListeners = [];
    const disconnectListeners = [];
    let upload = null;
    let open = true;
    const reply = (message) => setTimeout(() => open && messageListeners.forEach((l) => l(message)), 0);
    const close = () => {
      if (!open) return;
      open = false;
      setTimeout(() => disconnectListeners.forEach((l) => l()), 0);
    };
    const port = {
      onMessage: { addListener: (listener) => messageListeners.push(listener) },
      onDisconnect: { addListener: (listener) => disconnectListeners.push(listener) },
      disconnect() {
        open = false;
      },
      async postMessage(message) {
        host.received.push(message);
        if (host.dropAfter?.(message)) {
          // The host stores the commit but the port breaks before the answer is delivered.
          if (message.type === "image_save_commit") commit(message);
          return close();
        }
        if (host.unknown) return reply({ ok: false, eventId: null, error: "unknown_type" });
        const error = (code) => reply({ ok: false, type: "image_save_error", saveId: message.saveId, error: code });
        const vaultToken = host.vaultToken;
        if (message.type === "image_save_begin") {
          if (message.vaultToken && message.vaultToken !== vaultToken) return error("vault_switched");
          const done = host.saves.get(message.saveId);
          if (done) return reply({ ok: true, type: "image_saved", saveId: message.saveId, vaultToken, ...done, deduplicated: true, replayed: true });
          upload = { begin: message, parts: [], next: 0, needsBytes: !host.blobs.has(message.sha256) };
          return reply({ ok: true, type: "image_save_ready", saveId: message.saveId, vaultToken, upload: upload.needsBytes, chunkBytes: 1048576, maxBytes: 33554432 });
        }
        if (message.type === "image_save_chunk") {
          if (!upload || message.index !== upload.next) return error("chunk_out_of_order");
          upload.parts.push(Buffer.from(message.data, "base64"));
          upload.next += 1;
          return reply({ ok: true, type: "image_save_chunk_ack", saveId: message.saveId, index: message.index, received: upload.parts.reduce((n, p) => n + p.length, 0) });
        }
        if (message.type === "image_save_commit") {
          if (host.commitDelay) await host.commitDelay;
          if (host.rejectThumbnail && message.thumbnail) return error("invalid_thumbnail");
          if (host.failCommit) {
            const code = host.failCommit;
            host.failCommit = null;
            return error(code);
          }
          const saved = commit(message);
          return reply({ ok: true, type: "image_saved", saveId: message.saveId, vaultToken, ...saved, replayed: false });
        }
        return error("invalid_message");
      },
    };
    function commit(message) {
      const begin = upload.begin;
      const deduplicated = !upload.needsBytes;
      if (upload.needsBytes) {
        const bytes = Buffer.concat(upload.parts);
        if (sha256(bytes) !== begin.sha256) throw new Error("hash mismatch in fake host");
        host.blobs.set(begin.sha256, bytes);
      }
      const saved = { blobId: [...host.blobs.keys()].indexOf(begin.sha256) + 1, savedImageId: nextId++ };
      host.saves.set(begin.saveId, saved);
      host.history.push({ ...begin, thumbnail: message.thumbnail });
      upload = null;
      return { ...saved, deduplicated };
    }
    return port;
  };
  return host;
}
