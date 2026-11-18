// A stand-in for the Native Host's History API (history.rs) and for the page around the full-page History:
// `createHistoryHost` answers `host_capabilities`, `history_list`, `history_days` and `history_visit` from visits
// held in memory (per Vault), `createWin` is a `window` with a real session history (push, replace, back,
// forward), `createTimers` a clock the test advances by hand.
import { createDocument } from "./fake-dom.js";
import { createHistoryApp } from "../../extension/history-app.js";

export const tokenOf = (vault) => vault.charCodeAt(0).toString(16).padStart(2, "0").repeat(32);

/** A visit at a local time (the test's time zone), as the host stores it: a UTC instant. */
export const at = (year, month, day, hour = 12, minute = 0, second = 0) => new Date(year, month - 1, day, hour, minute, second).toISOString();
export const visit = (id, url, title, visitedAt) => ({ id, url, title, visitedAt });

const hostOf = (url) => { try { return new URL(url).hostname.toLowerCase(); } catch { return ""; } };

function matcher(domain, mode) {
  if (!domain || domain.trim() === "") return () => true;
  if (mode === "regex") {
    let pattern;
    try { pattern = new RegExp(domain, "i"); } catch { return null; }
    return (url) => hostOf(url) !== "" && pattern.test(hostOf(url));
  }
  const wanted = domain.trim().toLowerCase();
  return (url) => hostOf(url) === wanted;
}

const clip = (text, max) => (text.length > max ? [text.slice(0, max), true] : [text, false]);

export function createHistoryHost(options = {}) {
  const host = {
    vault: "A",
    visits: { A: [], B: [] },
    capabilities: ["navigation_v1", "vault_items_v1", "visit_calendar_v1", "item_delete_v1", "history_v1"],
    calls: [],
    /** `gates.history_days = async (message) => {…}` holds an answer until the test lets it go. */
    gates: {},
    /** Fail the next call of a type with an error code (or throw `unavailable`). */
    errors: {},
    fullUrls: {},
    /** `mutate.history_visit = (reply) => ({ ...reply, … })` rewrites the answer of a type (a broken or foreign one). */
    mutate: {},
    /** `relations[id] = { source: 'v1' | 'censored' | {raw}, targets: [ids], targetCount, navigation, pageMetadata, leak }` */
    relations: {},
    ...options,
  };
  host.of = (type) => host.calls.filter((call) => call.type === type);
  host.send = async (message) => {
    const reply = await answer(message);
    return host.mutate[message.type] ? host.mutate[message.type](reply, message) : reply;
  };
  const answer = async (message) => {
    host.calls.push(structuredClone(message));
    const vault = host.vault; // the Vault the host process resolves when it starts
    if (host.gates[message.type]) await host.gates[message.type](message);
    if (host.errors[message.type]) {
      const error = host.errors[message.type];
      delete host.errors[message.type];
      if (error === "unavailable") throw new Error("host gone");
      return { ok: false, type: `${message.type}_result`, requestId: message.requestId, error };
    }
    const reply = (fields) => ({ ok: true, type: `${message.type}_result`, requestId: message.requestId, ...fields });
    const fail = (error) => ({ ok: false, type: `${message.type}_result`, requestId: message.requestId, error });
    if (message.type === "host_capabilities") return host.capabilitiesFail ? fail("unknown_type") : reply({ capabilities: host.capabilities });
    const test = matcher(message.domain, message.mode);
    if (message.type === "history_list") {
      if (!test) return fail("invalid_regex");
      const matching = (host.visits[vault] ?? [])
        .filter((v) => v.visitedAt >= message.visitedFrom && v.visitedAt < message.visitedTo && test(v.url))
        .sort((a, b) => (a.visitedAt < b.visitedAt ? 1 : a.visitedAt > b.visitedAt ? -1 : a.id < b.id ? 1 : -1));
      const from = (message.page - 1) * message.size;
      const items = matching.slice(from, from + message.size).map((v) => {
        const [url, urlTruncated] = clip(v.url, 2048);
        const [title, titleTruncated] = clip(v.title, 512);
        return { id: v.id, visitedAt: v.visitedAt, url, title, host: hostOf(v.url), urlTruncated, titleTruncated };
      });
      return reply({ vaultToken: tokenOf(vault), items, total: matching.length, page: message.page, size: message.size, pages: Math.ceil(matching.length / message.size) });
    }
    if (message.type === "history_days") {
      if (!test) return fail("invalid_regex");
      const counts = message.bounds.slice(0, -1).map((start, i) => (host.visits[vault] ?? []).filter((v) => v.visitedAt >= start && v.visitedAt < message.bounds[i + 1] && test(v.url)).length);
      return reply({ vaultToken: tokenOf(vault), counts, total: counts.reduce((a, b) => a + b, 0) });
    }
    if (message.type === "history_visit") {
      const all = host.visits[vault] ?? [];
      const found = all.find((v) => v.id === message.id);
      if (!found) return fail("visit_not_found");
      const entry = host.relations[message.id] ?? {};
      const summary = (v) => { const [url, urlTruncated] = clip(v.url, 2048); const [title, titleTruncated] = clip(v.title, 512); return { id: v.id, visitedAt: v.visitedAt, url, title, host: hostOf(v.url), urlTruncated, titleTruncated }; };
      let source = { state: "unknown" };
      if (entry.source === "censored") source = { state: "censored", ...(entry.leak ?? {}) };
      else if (typeof entry.source === "string") {
        const from = all.find((v) => v.id === entry.source);
        source = from ? { state: "present", visit: summary(from) } : { state: "unknown" };
      } else if (entry.source) source = entry.source;
      const wanted = (entry.targets ?? []).map((id) => all.find((v) => v.id === id)).filter(Boolean);
      const limit = host.targetLimit ?? 200;
      const targets = wanted.slice(0, limit).map((v) => ({ ...summary(v), kind: "link", transitionType: "link", occurredAt: v.visitedAt }));
      return reply({
        vaultToken: tokenOf(vault),
        visit: { id: found.id, url: host.fullUrls[found.id] ?? found.url, title: found.title, visitedAt: found.visitedAt, host: hostOf(found.url) },
        navigation: entry.navigation === undefined ? (entry.source ? { kind: "navigation", transitionType: "link", clientRedirect: false, serverRedirect: false, forwardBack: false, fromAddressBar: false, newTab: false, occurredAt: found.visitedAt } : null) : entry.navigation,
        source,
        targets,
        targetCount: entry.targetCount ?? wanted.length,
        targetsTruncated: (entry.targetCount ?? wanted.length) > targets.length,
        pageMetadata: entry.pageMetadata ?? null,
      });
    }
    return fail("unknown_type");
  };
  return host;
}

/** A `window`: `location.search`, a session history with push/replace/back/forward, and `popstate`. */
export function createWin(search = "") {
  const listeners = {};
  const entries = [{ search }];
  let index = 0;
  const log = [];
  const emit = (type) => (listeners[type] ?? []).forEach((fn) => fn({ type }));
  const win = {
    location: { get search() { return entries[index].search; } },
    history: {
      pushState(_state, _title, url) { entries.splice(index + 1); entries.push({ search: url }); index++; log.push(["push", url]); },
      replaceState(_state, _title, url) { entries[index] = { search: url }; log.push(["replace", url]); },
    },
    addEventListener(type, fn) { (listeners[type] ??= []).push(fn); },
    back() { if (index > 0) { index--; emit("popstate"); } },
    forward() { if (index < entries.length - 1) { index++; emit("popstate"); } },
    focus() { emit("focus"); },
    log,
    entries,
    get index() { return index; },
  };
  return win;
}

export function createTimers() {
  let next = 1;
  const pending = new Map();
  return {
    setTimeout(fn, ms) { const id = next++; pending.set(id, { fn, ms }); return id; },
    clearTimeout(id) { pending.delete(id); },
    /** Runs every pending timer (a debounce that elapsed). */
    fire() { const due = [...pending.values()]; pending.clear(); due.forEach((t) => t.fn()); },
    get count() { return pending.size; },
  };
}

export const settle = (ms = 0) => new Promise((resolve) => setTimeout(resolve, ms));

/** A History page over a fake host: `{ app, $, host, win, timers, document, tabs, clock }`. */
export async function openHistory({ search = "", visits = {}, now = new Date(2026, 10, 17, 9, 30), host: hostOptions = {} } = {}) {
  const document = createDocument();
  const host = createHistoryHost({ visits: { A: [], B: [], ...visits }, ...hostOptions });
  const win = createWin(search);
  const timers = createTimers();
  const tabs = { opened: [], async create(options) { this.opened.push(options); } };
  const clock = { now, advance(ms) { clock.now = new Date(clock.now.getTime() + ms); } };
  const settings = { opened: 0 };
  const app = createHistoryApp({ document, win, send: (message) => host.send(message), tabs, openSettings: () => { settings.opened++; }, now: () => clock.now, timers });
  await app.start();
  await settle(10);
  return { app, $: (id) => document.querySelector(`#${id}`), host, win, timers, document, tabs, clock, settings };
}
