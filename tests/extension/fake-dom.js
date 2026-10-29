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
  }

  setAttribute(name, value) {
    this.attributes.set(name, String(value));
  }
  getAttribute(name) {
    return this.attributes.has(name) ? this.attributes.get(name) : null;
  }
  removeAttribute(name) {
    this.attributes.delete(name);
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
    dispatch(type) {
      for (const listener of listeners.get(type) ?? []) listener({ type });
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
  for (const id of ["panel-image", "panel-settings", "settings-attention", "error", "empty", "load-more", "open-vault-settings", "record-status", "queue-status", "vault-approval"]) {
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

/** Native host stand-in: `vaults` maps a vault path to its pages; `active` is the open vault. */
export function createHost({ vaults, active, statusDelay = 0 }) {
  const host = {
    vaults,
    active,
    searchDelay: {},
    statusDelay,
    requests: [],
    openedTabs: [],
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
    sendNativeMessage(_name, message, callback) {
      Promise.resolve(host.handle(structuredClone(message))).then((response) => {
        runtime.lastError = undefined;
        callback(response);
      });
    },
    async sendMessage(message) {
      if (message.type === "queue-status") return { ok: true, pending: 0, deadLetters: 0, dropped: 0, blockedReason: null };
      if (message.type === "flush-queue") return { ok: true, blockedBy: null };
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

let instance = 0;

/** Loads a fresh copy of sidepanel.js against a new fake document. */
export async function loadSidePanel(hostOptions) {
  const document = createDocument();
  const host = createHost(hostOptions);
  globalThis.document = document;
  globalThis.window = globalThis;
  globalThis.chrome = createChrome(host);
  globalThis.IntersectionObserver = class {
    observe() {}
  };
  instance += 1;
  await import(`${pathToFileURL(path.join(extensionDir, "sidepanel.js")).href}?instance=${instance}`);
  return { document, host, $: document.$ };
}

export const settle = (ms = 0) => new Promise((resolve) => setTimeout(resolve, ms));
