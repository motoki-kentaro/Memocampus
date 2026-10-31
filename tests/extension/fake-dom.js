// Minimal DOM, Chrome API and native host stand-ins for running extension/sidepanel.js in Node.
// Only what the side panel uses is implemented; this is not a general DOM.

import { pathToFileURL } from "node:url";
import { fileURLToPath } from "node:url";
import path from "node:path";

const here = path.dirname(fileURLToPath(import.meta.url));
export const extensionDir = path.resolve(here, "../../extension");

class FakeElement {
  constructor(document, tag, id = "") {
    this.ownerDocument = document;
    this.tagName = tag.toUpperCase();
    this.id = id;
    this.attributes = new Map();
    this.listeners = new Map();
    this.children = [];
    this.parent = null;
    this.hidden = false;
    this.disabled = false;
    this.textContent = "";
    this.value = "";
    this.className = "";
    this.dataset = {};
    this.tabIndex = 0;
    this.scrollTop = 0;
    this.href = "";
    this.title = "";
    this.dateTime = "";
    this.src = "";
    this.style = {};
  }

  setAttribute(name, value) {
    this.attributes.set(name, String(value));
  }
  getAttribute(name) {
    return this.attributes.has(name) ? this.attributes.get(name) : null;
  }
  removeAttribute(name) {
    this.attributes.delete(name);
    if (name === "src") this.src = "";
  }

  addEventListener(type, listener) {
    if (!this.listeners.has(type)) this.listeners.set(type, []);
    this.listeners.get(type).push(listener);
  }
  dispatch(type, init = {}) {
    const event = {
      type,
      target: this,
      button: 0,
      defaultPrevented: false,
      preventDefault() {
        this.defaultPrevented = true;
      },
      ...init,
    };
    for (const listener of this.listeners.get(type) ?? []) listener(event);
    return event;
  }

  append(...nodes) {
    for (const node of nodes) {
      for (const child of node.isFragment ? node.children : [node]) {
        child.parent = this;
        this.children.push(child);
      }
    }
  }
  replaceChildren(...nodes) {
    this.children = [];
    this.append(...nodes);
  }

  matches(selector) {
    const [tag, cls] = selector.split(".");
    return (!tag || this.tagName === tag.toUpperCase()) && (!cls || this.className.split(" ").includes(cls));
  }
  closest(selector) {
    for (let node = this; node; node = node.parent) if (node.matches?.(selector)) return node;
    return null;
  }
  contains(node) {
    for (; node; node = node.parent) if (node === this) return true;
    return false;
  }
  querySelector(selector) {
    for (const child of this.children) {
      if (child.matches(selector)) return child;
      const found = child.querySelector(selector);
      if (found) return found;
    }
    return null;
  }

  focus() {
    this.ownerDocument.activeElement = this;
  }
  scrollIntoView() {}
}

export function createDocument() {
  const byId = new Map();
  const listeners = new Map();
  const document = {
    activeElement: null,
    visibilityState: "visible",
    createElement: (tag) => new FakeElement(document, tag),
    createDocumentFragment: () => Object.assign(new FakeElement(document, "#fragment"), { isFragment: true }),
    addEventListener(type, listener) {
      if (!listeners.has(type)) listeners.set(type, []);
      listeners.get(type).push(listener);
    },
    dispatch(type, init = {}) {
      for (const listener of listeners.get(type) ?? []) listener({ type, ...init });
    },
    querySelector(selector) {
      if (selector.startsWith("#")) {
        const id = selector.slice(1);
        if (!byId.has(id)) byId.set(id, new FakeElement(document, "div", id));
        return byId.get(id);
      }
      const manual = /^\[data-manual="(.+)"\]$/.exec(selector);
      if (manual) return manualButtons.find((button) => button.dataset.manual === manual[1]) ?? null;
      throw new Error(`fake document does not support selector ${selector}`);
    },
    querySelectorAll(selector) {
      if (selector === "[data-manual]") return [...manualButtons];
      throw new Error(`fake document does not support selector ${selector}`);
    },
  };
  const manualButtons = ["vault_create", "vault_open", "vault_backup"].map((kind) => {
    const button = new FakeElement(document, "button");
    button.dataset.manual = kind;
    return button;
  });
  // Initial `hidden` state as written in sidepanel.html.
  for (const id of [
    "panel-image", "panel-settings", "settings-attention", "error", "empty", "load-more", "open-vault-settings", "record-status", "queue-status", "policy-status", "policy-initialize", "vault-approval",
    "gallery-new-dot", "gallery-error", "gallery-open-settings", "gallery-empty", "gallery-more", "gallery-details", "gallery-sources-more", "gallery-menu",
  ]) {
    document.querySelector(`#${id}`).hidden = true;
  }
  document.$ = (selector) => document.querySelector(selector);
  return document;
}

export function pages(prefix, count) {
  return Array.from({ length: count }, (_, i) => ({
    url: `https://${prefix.toLowerCase()}${i}.test/`,
    title: `${prefix} page ${i}`,
    lastVisitedAt: "2026-10-30T00:00:00.000Z",
    lastCapturedAt: null,
    lastActivityAt: "2026-10-30T00:00:00.000Z",
    source: "visit",
  }));
}

/** A saved-image summary as the host's image_list returns it. */
export function imageSummary(blobId, lastSavedAt, extra = {}) {
  return {
    blobId,
    sha256: blobId.toString(16).padStart(64, "0"),
    mimeType: "image/png",
    byteLength: 1000 + blobId,
    width: 40,
    height: 30,
    createdAt: lastSavedAt,
    lastSavedAt,
    saveCount: 1,
    hasThumbnail: true,
    latestImageUrl: `https://img.test/${blobId}.png`,
    latestPageUrl: `https://page.test/${blobId}`,
    ...extra,
  };
}

const PNG_THUMBNAIL = Buffer.from("89504e470d0a1a0a0000000d4948445200000004000000020806000000000000000000000049454e44ae426082", "hex").toString("base64");

/**
 * Native host stand-in: `vaults` maps a vault path to its pages; `active` is the open vault.
 * `images` maps a vault path to its image summaries (newest first) for the Image tab.
 */
export function createHost({ vaults, active, statusDelay = 0, images = {}, policyStatus = null }) {
  const host = {
    policyStatus,
    vaults,
    active,
    images,
    imageDelay: {},
    thumbnailOverride: null, // (blobId) => item to return instead of a valid thumbnail
    readOverride: null, // (message, reply) => reply to send instead, for image_read
    storedThumbnails: [], // what image_thumbnail_put added
    exportAnswer: { fileName: "1000.png" }, // image_export's answer ({ cancelled: true } to cancel)
    searchDelay: {},
    statusDelay,
    requests: [],
    openedTabs: [],
    runtimeListeners: [],
    status() {
      const base = { managementEnabled: true, overridden: false, defaultPath: "C:\\Vaults\\memocampus.sqlite3", defaultExists: false, locationApproved: false };
      if (!host.active) return { ...base, state: "not_configured", vaultPath: null, counts: null, location: null };
      return { ...base, state: "ready", vaultPath: host.active, counts: { visits: host.vaults[host.active].length, snapshots: 0 }, location: { verdict: "no_issue_detected", reasons: [] } };
    },
    handle(message) {
      host.requests.push(message);
      const reply = (fields) => ({ ok: true, requestId: message.requestId, ...fields });
      if (message.type === "search_pages") {
        if (!host.active) return { ok: false, type: "search_pages_result", requestId: message.requestId, error: "vault_not_configured" };
        const vault = host.active;
        const query = (message.query || "").toLowerCase();
        const hits = host.vaults[vault].filter((page) => !query || page.title.toLowerCase().includes(query) || page.url.includes(query));
        const response = reply({ type: "search_pages_result", items: hits.slice(message.offset, message.offset + message.limit), hasMore: hits.length > message.offset + message.limit });
        return new Promise((resolve) => setTimeout(() => resolve(response), host.searchDelay[vault] ?? 0));
      }
      if (message.type === "vault_status") {
        const response = reply({ type: "vault_result", status: host.status() });
        return new Promise((resolve) => setTimeout(() => resolve(response), host.statusDelay));
      }
      if (message.type.startsWith("image_")) {
        const type = `${message.type}_result`;
        if (!host.active) return { ok: false, type, requestId: message.requestId, error: "vault_not_configured" };
        const vault = host.active;
        const vaultToken = vault.charCodeAt(0).toString(16).padStart(2, "0").repeat(32);
        const all = host.images[vault] ?? [];
        let response;
        if (message.type === "image_list") {
          const start = message.cursor ? all.findIndex((item) => item.blobId === message.cursor.id) + 1 : 0;
          const page = all.slice(start, start + message.limit);
          const last = page.at(-1);
          const more = start + message.limit < all.length;
          response = reply({ type, vaultToken, items: page, next: more ? { lastSavedAt: last.lastSavedAt, id: last.blobId } : null });
        } else if (message.type === "image_thumbnails") {
          const items = message.blobIds.map((blobId) => {
            const item = all.find((candidate) => candidate.blobId === blobId);
            if (host.thumbnailOverride) return host.thumbnailOverride(blobId);
            if (!item?.hasThumbnail) return { blobId, missing: true };
            return { blobId, mimeType: "image/png", width: 4, height: 2, data: PNG_THUMBNAIL };
          });
          response = reply({ type, vaultToken, items, deferred: [] });
        } else if (["image_read", "image_thumbnail_put", "image_export"].includes(message.type)) {
          // Originals: `item.original` holds the bytes (see imageWithOriginal).
          const item = all.find((candidate) => candidate.blobId === message.blobId && candidate.sha256 === message.sha256);
          if (message.vaultToken !== vaultToken) response = { ok: false, type, requestId: message.requestId, error: "vault_switched" };
          else if (!item?.original) response = { ok: false, type, requestId: message.requestId, error: "blob_missing" };
          else if (message.type === "image_read") {
            const data = Buffer.from(item.original.subarray(message.offset, message.offset + 512 * 1024)).toString("base64");
            response = reply({ type, vaultToken, blobId: item.blobId, sha256: item.sha256, mimeType: item.mimeType, byteLength: item.original.length, offset: message.offset, data });
            if (host.readOverride) response = host.readOverride(message, response);
          } else if (message.type === "image_thumbnail_put") {
            const stored = !item.hasThumbnail;
            if (stored) host.storedThumbnails.push({ blobId: item.blobId, ...message.thumbnail });
            item.hasThumbnail = true;
            response = reply({ type, vaultToken, blobId: item.blobId, stored });
          } else {
            response = reply({ type, vaultToken, blobId: item.blobId, ...host.exportAnswer });
          }
        } else {
          const item = all.find((candidate) => candidate.blobId === message.blobId);
          const sources = item?.sources ?? [];
          response = reply({ type, vaultToken, blobId: message.blobId, items: sources, next: null });
        }
        return new Promise((resolve) => setTimeout(() => resolve(response), host.imageDelay[vault] ?? 0));
      }
      if (message.type === "vault_open") {
        host.active = message.path;
        return reply({ type: "vault_result", path: message.path, status: host.status() });
      }
      return { ok: false, type: "vault_result", requestId: message.requestId, error: "unknown_type" };
    },
  };
  return host;
}

function createChrome(host) {
  const runtime = {
    lastError: undefined,
    onMessage: { addListener: (listener) => host.runtimeListeners.push(listener) },
    sendNativeMessage(_name, message, callback) {
      Promise.resolve(host.handle(structuredClone(message))).then((response) => {
        runtime.lastError = undefined;
        callback(response);
      });
    },
    async sendMessage(message) {
      if (message.type === "queue-status") return { ok: true, pending: 0, deadLetters: 0, dropped: 0, blockedReason: null };
      if (message.type === "flush-queue") return { ok: true, blockedBy: null };
      if (message.type === "policy-status") return host.policyStatus ?? { ok: true, state: "ready", revision: 1, code: null, message: null };
      if (message.type === "policy-initialize") {
        host.policyInitializations = (host.policyInitializations ?? 0) + 1;
        host.policyStatus = null;
        return { ok: true, state: "ready", revision: 1, code: null, message: null };
      }
      return { ok: false };
    },
  };
  return {
    runtime,
    tabs: {
      async create(options) {
        host.openedTabs.push(options);
      },
    },
  };
}

/** Every IntersectionObserver the panel created, so tests can report what is on screen. */
export const observers = [];

class FakeIntersectionObserver {
  constructor(callback, options = {}) {
    this.callback = callback;
    this.options = options;
    this.targets = new Set();
    observers.push(this);
  }
  observe(target) {
    this.targets.add(target);
  }
  unobserve(target) {
    this.targets.delete(target);
  }
  /** Reports `targets` as intersecting (or not) to the callback. */
  report(targets, isIntersecting = true) {
    this.callback(targets.filter((target) => this.targets.has(target)).map((target) => ({ target, isIntersecting })));
  }
}

let instance = 0;

/** Loads a fresh copy of sidepanel.js against a new fake document. */
export async function loadSidePanel(hostOptions) {
  const document = createDocument();
  const host = createHost(hostOptions);
  globalThis.document = document;
  globalThis.window = globalThis;
  globalThis.chrome = createChrome(host);
  observers.length = 0;
  globalThis.IntersectionObserver = FakeIntersectionObserver;
  instance += 1;
  await import(`${pathToFileURL(path.join(extensionDir, "sidepanel.js")).href}?instance=${instance}`);
  return { document, host, $: document.$ };
}

export const settle = (ms = 0) => new Promise((resolve) => setTimeout(resolve, ms));
