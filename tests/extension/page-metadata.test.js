// Issue #22: light page metadata. The DOM reader runs against a small fake document; the service worker
// runs for real against a fake chrome and host, as in privacy-ingress.test.js.
import { test } from "node:test";
import assert from "node:assert/strict";
import path from "node:path";
import { readFileSync, readdirSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { DEAD_LETTER_KEY, QUEUE_KEY } from "../../extension/event-queue.js";
import { evaluatePolicy } from "../../extension/policy.js";
import defaults from "../../extension/policy-defaults.json" with { type: "json" };
import {
  MAX_CANDIDATES,
  MAX_DESCRIPTION,
  MAX_RAW,
  MAX_SITE_NAME,
  MAX_TITLE,
  MAX_URL,
  capturePageMetadata,
  cleanText,
  cleanUrl,
  readPageDom,
  shapePageMetadata,
} from "../../extension/page-metadata.js";
import { extensionDir, settle } from "./fake-dom.js";

const vectors = JSON.parse(readFileSync(new URL("../page-metadata/vectors.json", import.meta.url), "utf8"));
const PAGE_URL = "https://news.test/article?id=7&utm_source=keep";
const AT = "2026-11-05T01:02:03.456Z";
const clone = (value) => structuredClone(value);

// --- A fake document: elements with attributes, and the selectors readPageDom uses -------------

function fakeDocument({ href = PAGE_URL, baseUri = href, title = "", elements = [] } = {}) {
  const matches = (selector, element) => {
    const parsed = /^(\w+)\[([\w-]+)(~?)="([^"]+)" i\]$/.exec(selector);
    assert.ok(parsed, `selector used: ${selector}`);
    const [, tag, attribute, tokens, value] = parsed;
    if (element.tag !== tag) return false;
    const actual = element.attrs[attribute];
    if (typeof actual !== "string") return false;
    if (tokens) return actual.toLowerCase().split(/[\t\n\f\r ]+/).includes(value.toLowerCase());
    return actual.toLowerCase() === value.toLowerCase();
  };
  const document = {
    title,
    baseURI: baseUri,
    querySelectorAll: (selector) => elements.filter((element) => matches(selector, element)).map((element) => ({ getAttribute: (name) => element.attrs[name] ?? null })),
  };
  return { document, location: { href } };
}

/** Runs readPageDom as Chrome would inject it: only `document` and `location` from the page. */
function readFrom(page) {
  const { document, location } = fakeDocument(page);
  const saved = { document: globalThis.document, location: globalThis.location };
  globalThis.document = document;
  globalThis.location = location;
  try {
    return clone(readPageDom(MAX_CANDIDATES, MAX_RAW));
  } finally {
    globalThis.document = saved.document;
    globalThis.location = saved.location;
  }
}

const meta = (key, value, attribute = key.startsWith("og:") ? "property" : "name") => ({ tag: "meta", attrs: { [attribute]: key, content: value } });
const icon = (href, rel = "icon") => ({ tag: "link", attrs: { rel, href } });
const extract = (page, url = page.href ?? PAGE_URL) => shapePageMetadata(readFrom(page), url, AT);
const nothing = { title: null, description: null, ogTitle: null, ogDescription: null, ogSiteName: null, ogImageUrl: null, faviconUrl: null };

test("a page with OGP gives every field, each from its own tag", () => {
  const got = extract({
    title: "Document title",
    elements: [
      meta("description", "Plain description"),
      meta("og:title", "OG title"),
      meta("og:description", "OG description"),
      meta("og:site_name", "News Site"),
      meta("og:image", "https://cdn.test/og.png?w=1200&sig=keep"),
      icon("https://news.test/favicon.png"),
    ],
  });
  assert.deepEqual(got, {
    capturedAt: AT,
    title: "Document title",
    description: "Plain description",
    ogTitle: "OG title",
    ogDescription: "OG description",
    ogSiteName: "News Site",
    ogImageUrl: "https://cdn.test/og.png?w=1200&sig=keep",
    faviconUrl: "https://news.test/favicon.png",
  });
});

test("without OGP, or with only a title or a description, the rest is null and nothing is an error", () => {
  assert.deepEqual(extract({ title: "Only title" }), { ...nothing, capturedAt: AT, title: "Only title" });
  assert.deepEqual(extract({ elements: [meta("description", "Only description")] }), { ...nothing, capturedAt: AT, description: "Only description" });
  assert.deepEqual(extract({ title: "T", elements: [meta("og:title", "OG only")] }), { ...nothing, capturedAt: AT, title: "T", ogTitle: "OG only" });
  assert.deepEqual(extract({}), { ...nothing, capturedAt: AT });
});

test("a favicon is taken only from the page, never guessed", () => {
  assert.equal(extract({ elements: [icon("/static/icon.svg", "shortcut icon")] }).faviconUrl, "https://news.test/static/icon.svg");
  assert.equal(extract({ elements: [icon("/x.png", "ICON")] }).faviconUrl, "https://news.test/x.png");
  // apple-touch-icon is another token, and no link at all is no favicon: never /favicon.ico.
  assert.equal(extract({ elements: [icon("/touch.png", "apple-touch-icon")] }).faviconUrl, null);
  assert.equal(extract({ title: "No icon" }).faviconUrl, null);
});

test("relative URLs are resolved against the document's base, <base href> included", () => {
  const page = {
    href: "https://news.test/a/b/page.html",
    elements: [meta("og:image", "../img/og.png?x=1"), icon("favicon.ico")],
  };
  assert.deepEqual([extract(page).ogImageUrl, extract(page).faviconUrl], ["https://news.test/a/img/og.png?x=1", "https://news.test/a/b/favicon.ico"]);
  const based = { ...page, baseUri: "https://static.test/assets/" };
  assert.deepEqual([extract(based).ogImageUrl, extract(based).faviconUrl], ["https://static.test/img/og.png?x=1", "https://static.test/assets/favicon.ico"]);
  assert.equal(extract({ elements: [meta("og:image", "//cdn.test/p.png")] }).ogImageUrl, "https://cdn.test/p.png");
});

test("only http(s) URLs without credentials are kept, and the first valid candidate wins", () => {
  for (const bad of ["javascript:alert(1)", "data:image/png;base64,AAAA", "blob:https://news.test/1", "file:///C:/x.png", "chrome://favicon/x", "https://u:p@cdn.test/x.png", "http://[::1", ""]) {
    assert.equal(extract({ elements: [meta("og:image", bad), icon(bad)] }).ogImageUrl, null, bad);
    assert.equal(extract({ elements: [icon(bad)] }).faviconUrl, null, bad);
  }
  const got = extract({ elements: [meta("og:image", "javascript:x"), meta("og:image", "  "), meta("og:image", "/second.png"), meta("og:image", "/third.png"), icon("data:,"), icon("/good.ico"), icon("/later.ico")] });
  assert.deepEqual([got.ogImageUrl, got.faviconUrl], ["https://news.test/second.png", "https://news.test/good.ico"]);
  const long = `https://cdn.test/${"a".repeat(MAX_URL)}`;
  assert.equal(extract({ elements: [meta("og:image", long)] }).ogImageUrl, null);
});

test("several tags of one name: the first one with text counts", () => {
  const got = extract({ elements: [meta("description", "   "), meta("description", "First real"), meta("description", "Second"), meta("og:title", ""), meta("og:title", "OG first"), meta("og:title", "OG second")] });
  assert.deepEqual([got.description, got.ogTitle], ["First real", "OG first"]);
  // Name and property values match without regard to case, as HTML authors write them.
  assert.equal(extract({ elements: [meta("Description", "Upper")] }).description, "Upper");
  assert.equal(extract({ elements: [meta("OG:TITLE", "Upper OG", "property")] }).ogTitle, "Upper OG");
  // og:* in name= and description in property= are other tags.
  assert.equal(extract({ elements: [meta("og:title", "In name", "name")] }).ogTitle, null);
  assert.equal(extract({ elements: [meta("description", "In property", "property")] }).description, null);
});

test("empty or blank values are null, text is cleaned, and long values are cut to their limits", () => {
  const got = extract({ title: " \n\t ", elements: [meta("description", ""), meta("og:site_name", "\u3000"), meta("og:title", "  Breaking:\n\tnews\u0000 today  ")] });
  assert.deepEqual([got.title, got.description, got.ogSiteName, got.ogTitle], [null, null, null, "Breaking: news today"]);
  const huge = "長".repeat(MAX_RAW * 2);
  const cut = extract({ title: huge, elements: [meta("description", huge), meta("og:title", huge), meta("og:description", huge), meta("og:site_name", huge)] });
  const length = (value) => Array.from(value).length;
  assert.deepEqual([length(cut.title), length(cut.description), length(cut.ogTitle), length(cut.ogDescription), length(cut.ogSiteName)], [MAX_TITLE, MAX_DESCRIPTION, MAX_TITLE, MAX_DESCRIPTION, MAX_SITE_NAME]);
  // Never half a character, and a broken one becomes U+FFFD (the host could not read it otherwise).
  assert.equal(cleanText("😀".repeat(600), MAX_TITLE), "😀".repeat(MAX_TITLE));
  assert.equal(cleanText("a\ud800b", 10), "a\ufffdb");
});

test("the page reads at most a few candidates and characters, and only attributes", () => {
  const many = Array.from({ length: 50 }, (_, index) => meta("description", `d${index}`));
  const raw = readFrom({ title: "x".repeat(MAX_RAW * 3), elements: many });
  assert.equal(raw.description.length, MAX_CANDIDATES);
  assert.equal(raw.title.length, MAX_RAW);
  const source = readPageDom.toString();
  for (const forbidden of ["innerHTML", "outerHTML", "textContent", "innerText", "body", "fetch", "XMLHttpRequest", "Image(", "console"]) {
    assert.ok(!source.includes(forbidden), forbidden);
  }
});

test("the shared vectors agree with the host", () => {
  for (const [raw, limit, expected] of vectors.text) assert.equal(cleanText(raw, limit), expected, JSON.stringify(raw));
  for (const [value, expected] of vectors.url) assert.equal(cleanUrl(value, "https://base.test/"), expected, value);
});

test("an answer from another URL (pushState, redirect source) or of the wrong shape is no metadata", () => {
  assert.equal(extract({ href: "https://news.test/next" }, PAGE_URL), null);
  assert.equal(extract({ href: "http://news.test/article" }, "https://news.test/article"), null);
  for (const raw of [null, undefined, "x", [], {}, { href: PAGE_URL }]) assert.equal(shapePageMetadata(raw, PAGE_URL, AT), null);
  // Fields of the wrong type are only that field's null.
  assert.deepEqual(shapePageMetadata({ href: PAGE_URL, baseUri: PAGE_URL, title: 5, description: "x", ogImage: [1, "/ok.png"] }, PAGE_URL, AT), { ...nothing, capturedAt: AT, ogImageUrl: "https://news.test/ok.png" });
});

// --- capturePageMetadata: one call, in that document only --------------------------------------

const raw = (extra = {}) => ({ href: PAGE_URL, baseUri: PAGE_URL, title: "T", description: [], ogTitle: [], ogDescription: [], ogSiteName: [], ogImage: [], icon: [], ...extra });
const now = () => new Date(AT);

test("the page is read once, in exactly the loaded document, in the isolated world", async () => {
  const calls = [];
  const got = await capturePageMetadata({
    executeScript: async (options) => {
      calls.push(options);
      return [{ documentId: "doc-1", frameId: 0, result: raw() }];
    },
    tabId: 4,
    documentId: "doc-1",
    url: PAGE_URL,
    now,
  });
  assert.equal(got.title, "T");
  assert.equal(got.capturedAt, AT);
  assert.equal(calls.length, 1);
  assert.deepEqual(calls[0].target, { tabId: 4, documentIds: ["doc-1"] });
  assert.equal(calls[0].func, readPageDom);
  assert.deepEqual(calls[0].args, [MAX_CANDIDATES, MAX_RAW]);
  assert.equal(calls[0].world, "ISOLATED");
});

test("without a document id nothing is read; a failure, a timeout or a foreign answer is no metadata", async () => {
  let called = 0;
  const executeScript = async () => {
    called += 1;
    return [{ documentId: "doc-1", result: raw() }];
  };
  for (const documentId of [undefined, null, "", 5]) assert.equal(await capturePageMetadata({ executeScript, tabId: 1, documentId, url: PAGE_URL }), null);
  assert.equal(await capturePageMetadata({ executeScript, tabId: undefined, documentId: "doc-1", url: PAGE_URL }), null);
  assert.equal(called, 0);
  const answers = [
    () => Promise.reject(new Error(`No document with id doc-1 at ${PAGE_URL}`)),
    () => {
      throw new Error("sync");
    },
    async () => [],
    async () => [{ documentId: "doc-1", result: raw() }, { documentId: "doc-1", result: raw() }],
    async () => [{ documentId: "doc-2", result: raw() }],
    async () => [{ documentId: "doc-1", result: raw({ href: "https://news.test/after-pushstate" }) }],
    async () => [{ documentId: "doc-1", result: null }],
    async () => null,
  ];
  for (const answer of answers) assert.equal(await capturePageMetadata({ executeScript: answer, tabId: 1, documentId: "doc-1", url: PAGE_URL, now }), null, answer.toString());
  const started = Date.now();
  const late = await capturePageMetadata({ executeScript: () => new Promise((resolve) => setTimeout(() => resolve([{ documentId: "doc-1", result: raw() }]), 200)), tabId: 1, documentId: "doc-1", url: PAGE_URL, now, timeoutMs: 20 });
  assert.equal(late, null);
  assert.ok(Date.now() - started < 150, "the visit does not wait for a late page");
  // A failure after the timeout is swallowed, not an unhandled rejection.
  const lateFailure = await capturePageMetadata({ executeScript: () => new Promise((_, reject) => setTimeout(() => reject(new Error(PAGE_URL)), 30)), tabId: 1, documentId: "doc-1", url: PAGE_URL, now, timeoutMs: 5 });
  assert.equal(lateFailure, null);
  await settle(50);
});

// --- The service worker -----------------------------------------------------------------------

function fakeHost() {
  return {
    policy: clone(defaults),
    state: "ready",
    received: [],
    order: [],
    answer: null,
    respond(message) {
      if (message.type === "policy_evaluate_batch") {
        this.order.push("evaluate");
        if (this.state !== "ready") return { ok: false, type: "policy_evaluate_batch_result", requestId: message.requestId, error: "policy_invalid" };
        const names = { none: "allow", saveButNoMcp: "save_but_no_mcp", neverSave: "never_save" };
        return { ok: true, type: "policy_evaluate_batch_result", requestId: message.requestId, etag: "e1", revision: 1, items: message.items.map((item) => ({ id: item.id, decision: names[evaluatePolicy(this.policy, item.url)] })) };
      }
      if (message.type === "policy_status") {
        if (this.state !== "ready") return { ok: true, type: "policy_result", requestId: message.requestId, state: this.state };
        return { ok: true, type: "policy_result", requestId: message.requestId, state: "ready", revision: 1, etag: "e1", schemaVersion: 1, engineVersion: 1 };
      }
      this.order.push("deliver");
      this.received.push(message);
      return this.answer ? this.answer(message) : { ok: true, eventId: message.id };
    },
  };
}

async function startWorker({ host = fakeHost(), tabs = {}, data = new Map(), page = () => raw(), failWrites = () => false } = {}) {
  const listeners = {};
  const on = (name) => ({ addListener: (fn) => ((listeners[name] ??= []).push(fn)) });
  const scripts = [];
  const writes = [];
  globalThis.chrome = {
    runtime: {
      onStartup: on("startup"),
      onInstalled: on("installed"),
      onMessage: on("message"),
      sendMessage: async () => {},
      connectNative() {},
      lastError: undefined,
      sendNativeMessage(_name, message, reply) {
        reply(clone(host.respond(clone(message))));
      },
    },
    webNavigation: { onBeforeNavigate: on("before"), onErrorOccurred: on("failed"), onCompleted: on("navigation"), onCommitted: on("committed"), onCreatedNavigationTarget: on("createdTarget"), onHistoryStateUpdated: on("historyState") },
    contextMenus: { create() {}, onClicked: on("menuClick") },
    notifications: { create: async () => {} },
    action: { setBadgeText: async () => {}, setTitle: async () => {} },
    sidePanel: { setPanelBehavior: async () => {} },
    scripting: {
      async executeScript(options) {
        host.order.push("read");
        scripts.push(options);
        const result = await page(options);
        return [{ documentId: options.target.documentIds?.[0], frameId: 0, result }];
      },
    },
    tabs: {
      onRemoved: on("tabRemoved"), onReplaced: on("tabReplaced"),
      get: async (id) => {
        if (!(id in tabs)) throw new Error("No tab");
        return tabs[id];
      },
      query: async () => Object.values(tabs),
    },
    storage: {
      local: {
        async get(keys) {
          const out = {};
          for (const key of Array.isArray(keys) ? keys : [keys]) if (data.has(key)) out[key] = clone(data.get(key));
          return out;
        },
        async set(items) {
          const text = JSON.stringify(items);
          if (failWrites(text)) throw new Error("QUOTA_BYTES quota exceeded");
          writes.push(text);
          for (const [key, value] of Object.entries(items)) data.set(key, clone(value));
        },
      },
      session: { get: async () => ({}), set: async () => {}, remove: async () => {} },
    },
  };
  await import(`${pathToFileURL(path.join(extensionDir, "service-worker.js")).href}?metadata=${Math.random()}`);
  return {
    host,
    data,
    scripts,
    writes,
    listeners,
    pending: () => data.get(QUEUE_KEY) ?? [],
    async navigate(url, { tabId = 1, documentId = `doc-${url}`, withoutDocumentId = false, incognito = false } = {}) {
      tabs[tabId] = { id: tabId, windowId: 1, index: 0, url, title: "Tab title", pinned: false, incognito };
      await Promise.all(listeners.navigation.map((fn) => fn(withoutDocumentId ? { frameId: 0, tabId, url } : { frameId: 0, tabId, url, documentId })));
      await settle(30);
    },
  };
}

async function withConsoleWatch(run) {
  const seen = [];
  const original = {};
  for (const method of ["log", "info", "warn", "error", "debug"]) {
    original[method] = console[method];
    console[method] = (...args) => seen.push(args.join(" "));
  }
  try {
    await run();
  } finally {
    Object.assign(console, original);
  }
  return seen.join("\n");
}

test("an allowed visit is read only after the host allowed it, and its metadata travels with it", async () => {
  const worker = await startWorker({ page: () => raw({ title: "Doc title", ogTitle: ["OG"], icon: ["/i.png"] }) });
  await worker.navigate(PAGE_URL);
  assert.deepEqual(worker.host.order, ["evaluate", "read", "evaluate", "deliver"]);
  assert.equal(worker.scripts[0].target.documentIds[0], `doc-${PAGE_URL}`);
  const [sent] = worker.host.received;
  assert.equal(sent.type, "visit");
  assert.equal(sent.url, PAGE_URL);
  assert.equal(sent.title, "Tab title", "the tab title is unchanged");
  assert.deepEqual({ ...sent.page, capturedAt: undefined }, { ...nothing, capturedAt: undefined, title: "Doc title", ogTitle: "OG", faviconUrl: "https://news.test/i.png" });
  assert.match(sent.page.capturedAt, /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z$/);
  assert.deepEqual(worker.pending(), []);
});

test("never-save, an unknown policy and incognito tabs are not read at all", async () => {
  const blind = clone(defaults);
  blind.layers.neverSave.rules.push({ kind: "pattern", pattern: "blind.test" });
  const host = fakeHost();
  host.policy = blind;
  const worker = await startWorker({ host });
  const logged = await withConsoleWatch(async () => {
    await worker.navigate("https://blind.test/secret?CANARY=1");
    host.state = "invalid";
    await worker.navigate("https://unknown-policy.test/CANARY");
    host.state = "ready";
    await worker.navigate("https://incognito.test/CANARY", { incognito: true });
  });
  assert.deepEqual(worker.scripts, [], "no DOM was read");
  assert.deepEqual(worker.host.received, []);
  assert.ok(!worker.writes.some((write) => write.includes("CANARY")));
  assert.equal(logged, "");
});

test("a save-but-no-MCP page keeps its metadata locally like its URL", async () => {
  const url = "https://mail.google.com/mail/u/0/";
  const worker = await startWorker({ page: () => raw({ href: url, baseUri: url }) });
  await worker.navigate(url);
  assert.equal(evaluatePolicy(defaults, url), "saveButNoMcp");
  assert.equal(worker.scripts.length, 1);
  assert.equal(worker.host.received[0].page.title, "T");
});

test("a page that cannot be read, or moved on, still records its visit, without metadata", async () => {
  const pages = {
    closed: () => Promise.reject(new Error("No tab with id: 1")),
    moved: () => raw({ href: "https://news.test/after-pushstate" }),
    redirected: () => raw({ href: "https://news.test/start" }),
  };
  for (const [name, page] of Object.entries(pages)) {
    const worker = await startWorker({ page });
    const logged = await withConsoleWatch(() => worker.navigate(PAGE_URL));
    assert.equal(worker.host.received.length, 1, name);
    assert.equal(worker.host.received[0].url, PAGE_URL, name);
    assert.equal("page" in worker.host.received[0], false, name);
    assert.equal(logged, "", name);
  }
  // A navigation event without a document id (older Chrome) reads nothing.
  const worker = await startWorker();
  await worker.navigate(PAGE_URL, { withoutDocumentId: true });
  assert.deepEqual(worker.scripts, []);
  assert.equal("page" in worker.host.received[0], false);
});

test("quick navigations in one tab each get their own document's metadata or none", async () => {
  // The first document is gone when its read arrives; the second answers for itself.
  const worker = await startWorker({
    page: (options) => (options.target.documentIds[0] === "first" ? Promise.reject(new Error("No document")) : raw({ href: "https://news.test/second", title: "Second" })),
  });
  await Promise.all([worker.navigate("https://news.test/first", { documentId: "first" }), worker.navigate("https://news.test/second", { documentId: "second" })]);
  const byUrl = Object.fromEntries(worker.host.received.map((event) => [event.url, event.page?.title ?? null]));
  assert.deepEqual(byUrl, { "https://news.test/first": null, "https://news.test/second": "Second" });
});

test("metadata waits in the queue, survives a worker restart and is resent unchanged", async () => {
  const host = fakeHost();
  host.answer = () => ({ ok: false, error: "vault_busy" });
  const data = new Map();
  const first = await startWorker({ host, data, page: () => raw({ title: "Queued" }) });
  await first.navigate(PAGE_URL);
  assert.equal(first.pending().length, 1);
  assert.equal(first.pending()[0].page.title, "Queued");
  const attempted = clone(host.received[0]);
  // A new worker (restart) with the same storage delivers it, with the same id and metadata.
  host.answer = null;
  host.received = [];
  const second = await startWorker({ host, data });
  second.listeners.startup.forEach((fn) => fn());
  await settle(30);
  assert.deepEqual(host.received, [attempted]);
  assert.deepEqual(second.pending(), []);
});

test("if storage cannot take the metadata, the visit is still queued without it", async () => {
  const worker = await startWorker({ failWrites: (text) => text.includes('"page"'), page: () => raw({ title: "Too much" }) });
  await worker.navigate(PAGE_URL);
  assert.equal(worker.host.received.length, 1);
  assert.equal("page" in worker.host.received[0], false);
  assert.equal(worker.data.get("queueStats"), undefined, "nothing counted as dropped");
});

test("the metadata module is used by the service worker only and logs nothing", () => {
  const source = readFileSync(path.join(extensionDir, "page-metadata.js"), "utf8");
  assert.ok(!/console\./.test(source));
  assert.ok(!/fetch\(|XMLHttpRequest|new Image/.test(source));
  const users = readdirSync(extensionDir).filter((file) => file.endsWith(".js") && file !== "page-metadata.js" && readFileSync(path.join(extensionDir, file), "utf8").includes("page-metadata.js"));
  assert.deepEqual(users, ["service-worker.js"]);
  const manifest = JSON.parse(readFileSync(path.join(extensionDir, "manifest.json"), "utf8"));
  assert.equal(manifest.content_scripts, undefined, "no content script: one injection per allowed load");
  assert.deepEqual(manifest.permissions, ["webNavigation", "storage", "nativeMessaging", "tabs", "sidePanel", "contextMenus", "scripting", "notifications"], "no new permission");
});

test("events in the dead-letter list keep their metadata only as long as their URL is allowed", async () => {
  // The existing sweep judges by URL; metadata leaves with its event.
  const blind = clone(defaults);
  blind.layers.neverSave.rules.push({ kind: "pattern", pattern: "later-blind.test" });
  const host = fakeHost();
  host.policy = blind;
  const event = { type: "visit", id: "q1", url: "https://later-blind.test/x", title: "t", incognito: false, visitedAt: AT, page: { ...nothing, capturedAt: AT, title: "CANARY page" } };
  const data = new Map([[QUEUE_KEY, [event]], [DEAD_LETTER_KEY, [{ errorCode: "invalid_event", eventId: "d1", failedAt: AT, event: { ...event, id: "d1" } }]]]);
  const worker = await startWorker({ host, data });
  worker.listeners.startup.forEach((fn) => fn());
  await settle(30);
  assert.deepEqual(worker.pending(), []);
  assert.deepEqual(data.get(DEAD_LETTER_KEY), []);
  assert.ok(!JSON.stringify([...data.entries()]).includes("CANARY"));
  assert.deepEqual(host.received, []);
});
