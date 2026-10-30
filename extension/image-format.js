// Image format detection and encoding helpers shared by capture and upload.
// The rules mirror native-host/src/images.rs: formats are identified from their bytes only.

/** Limit for one original image, in raw bytes (before Base64). */
export const MAX_IMAGE_BYTES = 32 * 1024 * 1024;

export class ImageSaveError extends Error {
  constructor(code, details = {}) {
    super(code);
    this.name = "ImageSaveError";
    this.code = code;
    this.details = details;
  }
}

const startsWith = (bytes, prefix, at = 0) => prefix.every((value, i) => bytes[at + i] === value);
const ascii = (text) => [...text].map((c) => c.charCodeAt(0));
const u32be = (bytes, at) => ((bytes[at] << 24) | (bytes[at + 1] << 16) | (bytes[at + 2] << 8) | bytes[at + 3]) >>> 0;
const u32le = (bytes, at) => (bytes[at] | (bytes[at + 1] << 8) | (bytes[at + 2] << 16) | (bytes[at + 3] << 24)) >>> 0;

const FORMATS = {
  png: { mimeType: "image/png", extension: "png" },
  jpeg: { mimeType: "image/jpeg", extension: "jpg" },
  gif: { mimeType: "image/gif", extension: "gif" },
  webp: { mimeType: "image/webp", extension: "webp" },
  avif: { mimeType: "image/avif", extension: "avif" },
  bmp: { mimeType: "image/bmp", extension: "bmp" },
};

function looksLikeMarkup(bytes) {
  const head = new TextDecoder("utf-8", { fatal: false }).decode(bytes.subarray(0, 1024)).toLowerCase();
  const trimmed = head.replace(/^﻿/, "").trimStart();
  return trimmed.startsWith("<") && (trimmed.includes("<svg") || trimmed.startsWith("<?xml"));
}

/** Returns { mimeType, extension } or throws svg_not_supported / unsupported_format. */
export function sniffImage(bytes) {
  if (startsWith(bytes, [0x89, ...ascii("PNG\r\n\x1a\n")])) return FORMATS.png;
  if (startsWith(bytes, [0xff, 0xd8, 0xff])) return FORMATS.jpeg;
  if (startsWith(bytes, ascii("GIF87a")) || startsWith(bytes, ascii("GIF89a"))) return FORMATS.gif;
  if (bytes.length >= 12 && startsWith(bytes, ascii("RIFF")) && startsWith(bytes, ascii("WEBP"), 8)) return FORMATS.webp;
  if (bytes.length >= 12 && startsWith(bytes, ascii("ftyp"), 4)) {
    const boxEnd = Math.min(Math.max(u32be(bytes, 0), 16), bytes.length, 256);
    for (let at = 8; at + 4 <= boxEnd; at += 4) {
      if (startsWith(bytes, ascii("avif"), at) || startsWith(bytes, ascii("avis"), at)) return FORMATS.avif;
    }
  }
  if (bytes.length >= 26 && startsWith(bytes, ascii("BM")) && [12, 40, 52, 56, 108, 124].includes(u32le(bytes, 14))) {
    return FORMATS.bmp;
  }
  if (looksLikeMarkup(bytes)) throw new ImageSaveError("svg_not_supported");
  throw new ImageSaveError("unsupported_format");
}

export function bytesToBase64(bytes) {
  let binary = "";
  const step = 0x8000; // stay below the argument limit of String.fromCharCode
  for (let i = 0; i < bytes.length; i += step) {
    binary += String.fromCharCode.apply(null, bytes.subarray(i, i + step));
  }
  return btoa(binary);
}

export function base64ToBytes(text) {
  const binary = atob(text);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
  return bytes;
}

export async function sha256Hex(bytes, subtle = crypto.subtle) {
  const digest = new Uint8Array(await subtle.digest("SHA-256", bytes));
  return [...digest].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}
