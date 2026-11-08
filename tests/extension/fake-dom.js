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
    for (const child of this.children) child.parent = null;
    this.children = [];
    this.append(...nodes);
  }
  insertBefore(node, reference) {
    const index = this.children.indexOf(reference);
    if (index < 0) throw new Error("reference is not a child");
    node.parent = this;
    this.children.splice(index, 0, node);
    return node;
  }
  remove() {
    if (!this.parent) return;
    this.parent.children = this.parent.children.filter((child) => child !== this);
    this.parent = null;
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
  scrollIntoView(options) {
    this.ownerDocument.scrolledTo = { element: this, options };
  }
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
    "url-item-status", "gallery-manage-section", "item-dialog", "item-dialog-urls", "item-dialog-scope", "item-dialog-ack", "item-dialog-error", "item-dialog-repreview", "item-dialog-settings",
    "panel-image", "panel-settings", "settings-attention", "error", "empty", "load-more", "open-vault-settings", "record-status", "queue-status", "policy-status", "policy-initialize", "vault-approval",
    "gallery-new-dot", "gallery-error", "gallery-open-settings", "gallery-empty", "gallery-more", "gallery-details", "gallery-sources-more", "gallery-menu",
    "gallery-filter", "gallery-tags-empty", "gallery-tag-picker", "gallery-tag-picker-more",
    "visit-calendar", "visit-date-chip",
  ]) {
    document.querySelector(`#${id}`).hidden = true;
  }
  // Initial `disabled` state as written in sidepanel.html.
  for (const id of ["visit-calendar-open", "gallery-tag-filter", "gallery-tag-input", "gallery-tag-add", "gallery-tag-choose", "gallery-manage", "item-dialog-confirm"]) {
    document.querySelector(`#${id}`).disabled = true;
  }
  // The options of the image's drop-down, and the checked scope, as written in sidepanel.html.
  const manage = document.querySelector("#gallery-manage");
  for (const [value, text] of [["", "操作を選択…"], ["never_save", "保存しないグループへ追加"], ["save_but_no_mcp", "MCPサーバに見せないグループへ追加"], ["delete", "Vaultから削除"], ["publish", "公開"]]) {
    const option = new FakeElement(document, "option");
    option.value = value;
    option.textContent = text;
    manage.append(option);
  }
  document.querySelector("#item-scope-url").checked = true;
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


/** The vault token the fake host gives a vault (as for images). */
export const tokenOf = (vault) => vault.charCodeAt(0).toString(16).padStart(2, "0").repeat(32);

/**
 * Item actions (Issue #43) in the fake host: a policy of exact URLs and hosts per layer, plus `broad`
 * (URL -> layer) standing for rules or builtins that cover a URL and that publishing cannot remove.
 */
function createItemPolicy() {
  return { revision: 1, never: [], private: [], neverHosts: [], privateHosts: [], broad: new Map() };
}

function itemDecision(policy, url) {
  let host = "";
  try {
    host = new URL(url).hostname;
  } catch {
    return "never_save";
  }
  const broad = policy.broad.get(url);
  if (broad === "neverSave" || policy.never.includes(url) || policy.neverHosts.includes(host)) return "never_save";
  if (broad === "saveButNoMcp" || policy.private.includes(url) || policy.privateHosts.includes(host)) return "save_but_no_mcp";
  return "allow";
}

/** The plan of one item action, as the real host shapes it. */
function itemPlan(policy, { url, action, scope }) {
  const current = itemDecision(policy, url);
  const next = structuredClone({ ...policy, broad: [...policy.broad] });
  next.broad = new Map(next.broad);
  const changes = [];
  let reason = null;
  let blockers = [];
  if (action === "publish") {
    for (const [layer, key] of [["neverSave", "never"], ["saveButNoMcp", "private"]]) {
      const index = next[key].indexOf(url);
      if (index >= 0) {
        changes.push({ op: "remove", layer, ruleIndex: index, rule: { kind: "exact", pattern: url } });
        next[key].splice(index, 1);
      }
    }
    if (current !== "allow" && itemDecision(next, url) !== "allow") {
      reason = "publish_blocked";
      const layer = policy.broad.get(url) ?? (itemDecision(next, url) === "never_save" ? "neverSave" : "saveButNoMcp");
      blockers = [{ layer, ruleIndex: 0, kind: "pattern" }];
    }
    if (current === "allow") changes.length = 0;
  } else {
    const layer = action === "never_save" ? "neverSave" : "saveButNoMcp";
    const key = scope === "host" ? (action === "never_save" ? "neverHosts" : "privateHosts") : action === "never_save" ? "never" : "private";
    const value = scope === "host" ? new URL(url).hostname : url;
    if (!next[key].includes(value)) {
      next[key].push(value);
      changes.push({ op: "add", layer, rule: { kind: scope === "host" ? "pattern" : "exact", pattern: value } });
    }
  }
  const expected = reason ? current : itemDecision(next, url);
  return { current, expected, changes, reason, blockers, next };
}

/** `capabilities` names what the fake host offers; item actions only with "vault_items_v1". */
function handleItems(host, message) {
  const type = `${message.type}_result`;
  const fail = (error) => ({ ok: false, type, requestId: message.requestId, error });
  const reply = (fields) => ({ ok: true, type, requestId: message.requestId, ...fields });
  if (message.type === "host_capabilities") return reply({ capabilities: host.capabilities });
  if (host.itemErrors[message.type]) {
    const error = host.itemErrors[message.type];
    delete host.itemErrors[message.type];
    return fail(error);
  }
  const policy = host.itemPolicy;
  const etag = `etag-${policy.revision}`;
  if (message.type === "policy_evaluate_batch") {
    return reply({ etag, revision: policy.revision, items: message.items.map((item) => ({ id: item.id, decision: itemDecision(policy, item.url) })) });
  }
  if (message.type === "policy_item_preview" || message.type === "policy_item_apply") {
    const plan = itemPlan(policy, message);
    const echo = { url: message.url, action: message.action, scope: message.scope };
    if (message.type === "policy_item_preview") {
      return reply({ etag, revision: policy.revision, ...echo, current: plan.current, expected: plan.expected, changes: plan.changes, affectsOtherUrls: message.scope === "host", executable: plan.reason === null, ...(plan.reason ? { reason: plan.reason, blockers: plan.blockers } : {}) });
    }
    if (message.expectedEtag !== etag) return fail("policy_conflict");
    if (plan.reason) return fail(plan.reason);
    if (plan.changes.length > 0) host.itemPolicy = { ...plan.next, revision: policy.revision + 1 };
    host.policyWrites += plan.changes.length > 0 ? 1 : 0;
    return reply({ etag: `etag-${host.itemPolicy.revision}`, revision: host.itemPolicy.revision, ...echo, previous: plan.current, decision: itemDecision(host.itemPolicy, message.url), changed: plan.changes.length > 0, changes: plan.changes });
  }
  if (!host.active) return fail("vault_not_configured");
  const vault = host.active;
  const vaultToken = tokenOf(vault);
  if (message.type === "url_delete_preview" || message.type === "url_delete") {
    const stored = host.vaults[vault].filter((page) => page.url === message.url).length;
    const counts = { visits: stored, tabSnapshots: 0, pageMetadata: stored, navigations: 0, navigationSourcesCleared: host.navigationSources ?? 0, imageSavesKept: host.imageSavesKept ?? 0 };
    if (message.type === "url_delete_preview") return reply({ vaultToken, url: message.url, counts, irreversible: true });
    if (message.vaultToken !== vaultToken) return fail("vault_switched");
    host.vaults[vault] = host.vaults[vault].filter((page) => page.url !== message.url);
    host.deletedUrls.push(message.url);
    return reply({ vaultToken, url: message.url, deleted: counts, irreversible: true });
  }
  if (message.type === "image_delete_preview" || message.type === "image_delete") {
    const all = host.images[vault] ?? [];
    const item = all.find((candidate) => candidate.blobId === message.blobId && candidate.sha256 === message.sha256);
    if (message.type === "image_delete" && message.vaultToken !== vaultToken) return fail("vault_switched");
    if (!item) return fail("blob_missing");
    const tags = host.tags[vault]?.[item.blobId] ?? [];
    const others = Object.entries(host.tags[vault] ?? {}).filter(([id]) => Number(id) !== item.blobId).flatMap(([, names]) => names);
    const counts = { saves: item.sources?.length ?? item.saveCount, thumbnails: item.hasThumbnail ? 1 : 0, tagLinks: tags.length, tagsRemoved: tags.filter((name) => !others.includes(name)).length };
    const fields = { vaultToken, blobId: item.blobId, sha256: item.sha256, irreversible: true };
    if (message.type === "image_delete_preview") return reply({ ...fields, byteLength: item.byteLength, counts });
    host.images[vault] = all.filter((candidate) => candidate !== item);
    if (host.tags[vault]) delete host.tags[vault][item.blobId];
    host.deletedImages.push(item.blobId);
    return reply({ ...fields, deleted: counts });
  }
  return fail("unknown_type");
}

const ITEM_TYPES = new Set(["host_capabilities", "policy_evaluate_batch", "policy_item_preview", "policy_item_apply", "url_delete_preview", "url_delete", "image_delete_preview", "image_delete"]);

const PNG_THUMBNAIL = Buffer.from("89504e470d0a1a0a0000000d4948445200000004000000020806000000000000000000000049454e44ae426082", "hex").toString("base64");

/**
 * Native host stand-in: `vaults` maps a vault path to its pages; `active` is the open vault.
 * `images` maps a vault path to its image summaries (newest first) for the Image tab.
 */
export function createHost({ vaults, active, statusDelay = 0, images = {}, policyStatus = null, capabilities = ["navigation_v1"], forward = null }) {
  const host = {
    capabilities,
    itemPolicy: createItemPolicy(),
    itemErrors: {}, // type -> error code, answered once
    itemDelay: 0,
    policyWrites: 0,
    deletedUrls: [],
    deletedImages: [],
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
    /** visit_days (Issue #5): delay per vault, and `daysOverride(message, reply)` to answer otherwise. */
    daysDelay: {},
    daysOverride: null,
    /** While true, every native message fails as when the host cannot be started (lastError). */
    disconnected: false,
    statusDelay,
    /** vault_backup (Issue #48): what was backed up, the answer's delay, and a hook while it runs. */
    backups: [],
    backupDelay: 0,
    backupDuring: null,
    requests: [],
    openedTabs: [],
    runtimeListeners: [],
    status() {
      const base = { managementEnabled: true, overridden: false, defaultPath: "C:\\Vaults\\memocampus.sqlite3", defaultExists: false, locationApproved: false };
      if (!host.active) return { ...base, state: "not_configured", vaultPath: null, counts: null, location: null };
      return { ...base, state: "ready", vaultPath: host.active, vaultToken: tokenOf(host.active), counts: { visits: host.vaults[host.active].length, snapshots: 0 }, location: { verdict: "no_issue_detected", reasons: [] } };
    },
    /** `tags[vault]` maps a blob id to its tag names; `tagError` makes the next tag write fail. */
    tags: {},
    tagError: null,
    tagWriteDelay: 0,
    /** A stand-in for the host's normalization: trim, single spaces, lowercase, 1..64 characters. */
    normalizeTag(raw) {
      if (typeof raw !== "string" || /[\u0000-\u001f\u200b]/.test(raw)) return null;
      const name = raw.split(/\s+/).filter(Boolean).join(" ").toLowerCase();
      return name.length >= 1 && [...name].length <= 64 ? name : null;
    },
    handleTag(message, vault, vaultToken, all) {
      const type = `${message.type}_result`;
      const fail = (error) => ({ ok: false, type, requestId: message.requestId, error });
      const reply = (fields) => ({ ok: true, type, requestId: message.requestId, vaultToken, ...fields });
      const tags = (host.tags[vault] ??= {});
      const of = (blobId) => [...(tags[blobId] ?? [])].sort();
      if (message.type === "image_tags_get") {
        if (!all.some((item) => item.blobId === message.blobId)) return fail("blob_missing");
        return reply({ blobId: message.blobId, tags: of(message.blobId) });
      }
      if (message.type === "image_tag_list") {
        const counts = new Map();
        for (const names of Object.values(tags)) for (const name of names) counts.set(name, (counts.get(name) ?? 0) + 1);
        const names = [...counts.keys()].sort().filter((name) => message.cursor === null || name > message.cursor);
        const page = names.slice(0, message.limit);
        return reply({ items: page.map((name) => ({ name, imageCount: counts.get(name) })), next: names.length > message.limit ? page.at(-1) : null });
      }
      const name = host.normalizeTag(message.tag);
      if (name === null) return fail("invalid_tag");
      if (message.type === "image_tag_search") {
        const found = all.filter((item) => (tags[item.blobId] ?? []).includes(name));
        const start = message.cursor ? found.findIndex((item) => item.blobId === message.cursor.id) + 1 : 0;
        const page = found.slice(start, start + message.limit);
        const more = start + message.limit < found.length;
        return reply({ tag: name, items: page, next: more ? { lastSavedAt: page.at(-1).lastSavedAt, id: page.at(-1).blobId } : null });
      }
      // image_tag_add / image_tag_remove
      if (message.vaultToken !== vaultToken) return fail("vault_switched");
      if (host.tagError) {
        const error = host.tagError;
        host.tagError = null;
        return fail(error);
      }
      if (!all.some((item) => item.blobId === message.blobId && item.sha256 === message.sha256)) return fail("blob_missing");
      const current = new Set(tags[message.blobId] ?? []);
      let changed;
      if (message.type === "image_tag_add") {
        changed = !current.has(name);
        if (changed && current.size >= 64) return fail("too_many_tags");
        current.add(name);
      } else {
        changed = current.delete(name);
      }
      tags[message.blobId] = [...current];
      return reply({ blobId: message.blobId, tag: name, [message.type === "image_tag_add" ? "added" : "removed"]: changed, tags: of(message.blobId) });
    },
    handle(message) {
      host.requests.push(message);
      const reply = (fields) => ({ ok: true, requestId: message.requestId, ...fields });
      if (message.type === "search_pages") {
        if (!host.active) return { ok: false, type: "search_pages_result", requestId: message.requestId, error: "vault_not_configured" };
        const vault = host.active;
        const query = (message.query || "").toLowerCase();
        // A date range (Issue #5) only where the host knows it; an older host ignores the fields.
        const ranged = host.capabilities.includes("visit_calendar_v1") && "visitedFrom" in message;
        const inRange = (page) => !ranged || (page.lastVisitedAt !== null && page.lastVisitedAt >= message.visitedFrom && page.lastVisitedAt < message.visitedTo);
        const hits = host.vaults[vault].filter((page) => (!query || page.title.toLowerCase().includes(query) || page.url.includes(query)) && inRange(page));
        const response = reply({ type: "search_pages_result", vaultToken: tokenOf(vault), items: hits.slice(message.offset, message.offset + message.limit), hasMore: hits.length > message.offset + message.limit });
        return new Promise((resolve) => setTimeout(() => resolve(response), host.searchDelay[vault] ?? 0));
      }
      if (message.type === "visit_days") {
        const fail = (error) => ({ ok: false, type: "visit_days_result", requestId: message.requestId, error });
        if (!host.capabilities.includes("visit_calendar_v1")) return { ok: false, eventId: null, error: "unknown_type" };
        if (!host.active) return fail("vault_not_configured");
        const vault = host.active;
        const bounds = message.bounds;
        // Each page stands for one visit at its lastVisitedAt, counted in the day that holds it.
        const counts = bounds.slice(1).map((end, i) => host.vaults[vault].filter((page) => page.lastVisitedAt !== null && page.lastVisitedAt >= bounds[i] && page.lastVisitedAt < end).length);
        let response = reply({ type: "visit_days_result", vaultToken: tokenOf(vault), counts });
        if (host.daysOverride) response = host.daysOverride(message, response);
        return new Promise((resolve) => setTimeout(() => resolve(response), host.daysDelay[vault] ?? 0));
      }
      if (message.type === "vault_status") {
        const response = reply({ type: "vault_result", status: host.status() });
        return new Promise((resolve) => setTimeout(() => resolve(response), host.statusDelay));
      }
      if (ITEM_TYPES.has(message.type)) {
        const response = handleItems(host, message);
        return new Promise((resolve) => setTimeout(() => resolve(response), host.itemDelay));
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
        } else if (message.type.startsWith("image_tag")) {
          response = host.handleTag(message, vault, vaultToken, all);
        } else if (message.type === "image_mcp_public_set") {
          if (host.publicationError) response = { ok: false, type, requestId: message.requestId, error: host.publicationError };
          else response = reply({ type, vaultToken, savedImageId: message.savedImageId, mcpPublic: message.mcpPublic });
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
      if (message.type === "vault_backup") {
        const fail = (error) => ({ ok: false, type: "vault_result", requestId: message.requestId, error });
        const expected = message.expectedVaultToken;
        if (typeof expected !== "string" || !/^[0-9a-f]{64}$/.test(expected)) return fail("invalid_vault_token");
        if (!host.active) return fail("vault_not_configured");
        if (tokenOf(host.active) !== expected) return fail("vault_switched");
        // `backupDuring` runs while the folder dialog is open (e.g. another window switching).
        return new Promise((resolve) => setTimeout(() => {
          host.backupDuring?.();
          if (!host.active || tokenOf(host.active) !== expected) return resolve(fail("vault_switched"));
          const backupPath = `C:\\Backups\\memocampus-backup-${host.backups.length}.sqlite3`;
          host.backups.push({ vault: host.active, backupPath });
          resolve(reply({ type: "vault_result", backupPath, vaultPath: host.active, vaultToken: expected }));
        }, host.backupDelay));
      }
      if (message.type === "vault_open") {
        host.active = message.path;
        return reply({ type: "vault_result", path: message.path, status: host.status() });
      }
      return { ok: false, type: "vault_result", requestId: message.requestId, error: "unknown_type" };
    },
  };
  // `forward`: every message goes to it instead (a real host), from the panel's first request on.
  if (forward) {
    host.handle = (message) => {
      host.requests.push(message);
      return forward(message);
    };
  }
  return host;
}

function createChrome(host) {
  const runtime = {
    lastError: undefined,
    async openOptionsPage() {
      host.optionsOpened = (host.optionsOpened ?? 0) + 1;
    },
    onMessage: { addListener: (listener) => host.runtimeListeners.push(listener) },
    sendNativeMessage(_name, message, callback) {
      if (host.disconnected) {
        host.requests.push(structuredClone(message));
        setTimeout(() => {
          runtime.lastError = { message: "Native host has exited." };
          callback(undefined);
          runtime.lastError = undefined;
        }, 0);
        return;
      }
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
