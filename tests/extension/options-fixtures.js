// Shared stand-ins for the Privacy settings page tests (#23 Slice 2 and 3): synthetic policies, a fake
// host with the real host's compare-and-swap, and helpers that drive the page through its controls.
// The fake host's answers are scripted: the page must show what the host says and judge nothing.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { createDocument, extensionDir, settle } from "./fake-dom.js";
import { startOptionsPage } from "../../extension/options-page.js";
import { CATEGORY_KEYS } from "../../extension/options-state.js";

export const html = readFileSync(path.join(extensionDir, "options.html"), "utf8");
/** The text of an element as options.html writes it (the fake DOM does not read the page). */
export const htmlText = (id) => new RegExp(`id="${id}"[^>]*>([^<]*)<`).exec(html)?.[1] ?? "";

export const allOn = () => Object.fromEntries(CATEGORY_KEYS.map((key) => [key, true]));
export const pattern = (text) => ({ kind: "pattern", pattern: text });
export const regex = (text) => ({ kind: "regex", pattern: text });
export function v2(rules = {}, extra = {}) {
  return {
    schemaVersion: 2,
    engineVersion: 2,
    revision: 7,
    categories: allOn(),
    layers: {
      neverSave: { enabled: true, rules: rules.neverSave ?? [] },
      saveButNoMcp: { enabled: true, rules: rules.saveButNoMcp ?? [] },
    },
    ...extra,
  };
}
export const PSL = { source: "publicsuffix/list", sourceCommit: "8437e5262f13305f94087abe699b4fe2be1d1de8", snapshotDate: "2026-10-22" };
export const RULES = { neverSave: [pattern("bank.example"), regex("^https://a\\.example/")], saveButNoMcp: [pattern("wiki.example/private")] };

/**
 * A host with a stored policy. `policy_apply` replaces it only for the stored ETag and revision, as
 * the real one does. `policy_test_rule` says "match" unless told otherwise. `answers[type]` overrides
 * a reply; `hold(type)` keeps the next reply of a type until released. Every request is recorded.
 */
export function fakeHost({ policy = v2(RULES), etag = "etag-7" } = {}) {
  const host = { requests: [], policy: structuredClone(policy), etag, answers: {}, holds: {}, writes: 0 };
  // Scripted PSL answers: `restricted[pattern] = { domain, reason }`; `approved[domain] = approvedAt`;
  // `inUse` the domains the active policy needs. A pattern not listed is valid syntax and registrable.
  host.restricted = {};
  host.approved = {};
  host.inUse = new Set();
  host.approvalWrites = 0;
  host.hold = (type) => {
    let release;
    const held = new Promise((resolve) => { release = resolve; });
    (host.holds[type] ??= []).push(held);
    return () => release();
  };
  host.ofType = (type) => host.requests.filter((message) => message.type === type);
  /** The types sent, but the read-only listing of approved domains the page asks for on every reading. */
  host.types = () => host.allTypes().filter((type) => type !== "policy_domain_approvals");
  host.allTypes = () => host.requests.map((message) => message.type);
  host.send = async (message) => {
    host.requests.push(structuredClone(message));
    const held = host.holds[message.type]?.shift();
    if (held) await held;
    const base = { type: `${message.type === "policy_status" ? "policy" : message.type}_result`, requestId: message.requestId };
    if (host.answers[message.type]) {
      const answer = await host.answers[message.type](message, host);
      return answer === undefined ? undefined : { ...base, ...answer };
    }
    const p = host.policy;
    const version = { etag: host.etag, revision: p.revision, schemaVersion: p.schemaVersion, engineVersion: p.engineVersion };
    switch (message.type) {
      case "policy_status":
        return { ok: true, ...base, state: "ready", ...version };
      case "policy_get":
        return { ok: true, ...base, state: "ready", ...version, policy: structuredClone(p) };
      case "policy_validate":
        return { ...base, ...validateAnswer(message.policy) };
      case "policy_test_rule": {
        const restriction = host.restricted[message.rule?.pattern];
        if (message.rule?.kind === "domainWildcard" && restriction && !(restriction.domain in host.approved)) {
          return { ok: false, ...base, error: "domain_wildcard_not_approved" };
        }
        return { ok: true, ...base, matched: true };
      }
      case "policy_domain_check": {
        const restriction = host.restricted[message.rule?.pattern];
        const fields = restriction
          ? { status: "restricted", domain: restriction.domain, reason: restriction.reason, approved: restriction.domain in host.approved }
          : { status: "valid", domain: String(message.rule?.pattern).slice(2).toLowerCase() };
        return { ok: true, ...base, ...fields, psl: PSL };
      }
      case "policy_domain_approve": {
        const restriction = host.restricted[message.rule?.pattern];
        if (!restriction) return { ok: false, ...base, error: "domain_not_restricted" };
        if (restriction.domain !== message.domain) return { ok: false, ...base, error: "domain_mismatch" };
        host.approved[message.domain] ??= "2026-11-04T00:00:00.000Z";
        host.approvalWrites += 1;
        return { ok: true, ...base, domain: message.domain, approvedAt: host.approved[message.domain] };
      }
      case "policy_domain_approvals":
        return {
          ok: true,
          ...base,
          approvals: Object.entries(host.approved).map(([domain, approvedAt]) => ({ domain, approvedAt, inUse: host.inUse.has(domain) })),
          psl: PSL,
        };
      case "policy_domain_revoke": {
        if (host.inUse.has(message.domain)) return { ok: false, ...base, error: "domain_in_use" };
        const revoked = message.domain in host.approved;
        delete host.approved[message.domain];
        host.approvalWrites += 1;
        return { ok: true, ...base, domain: message.domain, revoked };
      }
      case "policy_test_url":
        return { ok: true, ...base, decision: "allow" };
      case "policy_apply": {
        if (message.expectedEtag !== host.etag) return { ok: false, ...base, error: "policy_conflict" };
        const checked = validateAnswer(message.policy);
        if (!checked.ok) return { ...base, ...checked };
        if (message.policy.revision !== p.revision) return { ok: false, ...base, error: "policy_conflict" };
        host.commit(message.policy);
        return { ok: true, ...base, state: "ready", etag: host.etag, revision: host.policy.revision, schemaVersion: 2, engineVersion: 2 };
      }
      default:
        return { ok: false, ...base, error: "unknown_type" };
    }
  };
  host.commit = (policy) => {
    host.policy = { ...structuredClone(policy), revision: host.policy.revision + 1 };
    host.etag = `etag-${host.policy.revision}`;
    host.writes += 1;
  };
  return host;
}

/** The test's stand-in for the host's checks: a rule whose text starts with "bad" is invalid. */
export function validateAnswer(policy) {
  const errors = [];
  for (const layer of ["neverSave", "saveButNoMcp"]) {
    policy.layers[layer].rules.forEach((rule, ruleIndex) => {
      if (rule.pattern.startsWith("bad")) errors.push({ layer, ruleIndex, code: rule.kind === "regex" ? "invalid_regex" : "invalid_pattern" });
    });
  }
  return errors.length ? { ok: false, error: "policy_invalid", errors } : { ok: true, schemaVersion: 2, engineVersion: 2 };
}

export function fakeWindow() {
  const listeners = new Map();
  return {
    addEventListener: (type, listener) => listeners.set(type, [...(listeners.get(type) ?? []), listener]),
    dispatch(type) {
      const event = { type, defaultPrevented: false, returnValue: undefined, preventDefault() { this.defaultPrevented = true; } };
      for (const listener of listeners.get(type) ?? []) listener(event);
      return event;
    },
  };
}

export async function openPage(hostOptions = {}) {
  const host = hostOptions.send ? hostOptions : fakeHost(hostOptions);
  const document = createDocument();
  const window = fakeWindow();
  const page = startOptionsPage({ document, window, send: host.send });
  await page.ready;
  const $ = (id) => document.querySelector(`#${id}`);
  const rowsOf = (layer) => $(`${layer}-rules`).children;
  const controls = (layer, index) => {
    const row = rowsOf(layer)[index];
    const [kind, text, actions] = row.children;
    const [edit, remove] = actions.children[0].children;
    return { row, kind: kind.textContent, text: text.children[0], error: text.children[1], edit, remove };
  };
  const ruleTexts = (layer) => rowsOf(layer).map((row) => row.children[1].children[0].textContent);
  return { page, host, document, window, $, rowsOf, controls, ruleTexts };
}

export function type(input, value) {
  input.value = value;
  input.dispatch("input");
}
export function check(input, checked) {
  input.checked = checked;
  input.dispatch("change");
}
export const click = (element) => element.dispatch("click");
/** Lets every pending reply of the fake host arrive. */
export const flush = () => settle(0);

/** Chooses the checker's kind as a browser does: one radio of the group checked, then "change". */
export function chooseKind($, kind) {
  for (const other of ["pattern", "regex", "domainWildcard"]) $(`checker-kind-${other}`).checked = other === kind;
  $(`checker-kind-${kind}`).dispatch("change");
}
/** Fills the checker and asks the host; the reply has arrived when this resolves. */
export async function checkRule($, rule, url = "https://x.example/page") {
  chooseKind($, rule.kind);
  type($("checker-rule"), rule.pattern);
  type($("checker-url"), url);
  click($("checker-run"));
  await flush();
}
/** Adds `rule` to a group through the checker (the fake host says "match" unless told otherwise). */
export async function addRule({ $ }, layer, rule, url) {
  await checkRule($, rule, url);
  click($(`checker-add-${layer}`));
}
/** Changes a rule of a group through the checker's edit mode. */
export async function editRule({ $, controls }, layer, index, rule, url) {
  click(controls(layer, index).edit);
  await checkRule($, rule, url);
  click($("checker-apply-edit"));
}

/** Runs `body` with the console and every storage the page could use turned into traps. */
export async function guarded(body) {
  const logged = [];
  const methods = ["log", "info", "warn", "error", "debug", "trace"];
  const saved = methods.map((method) => console[method]);
  for (const method of methods) console[method] = (...args) => logged.push(args);
  const savedStorage = Object.getOwnPropertyDescriptor(globalThis, "localStorage");
  Object.defineProperty(globalThis, "localStorage", { configurable: true, get() { throw new Error("localStorage used"); } });
  globalThis.chrome = { storage: new Proxy({}, { get() { throw new Error("chrome.storage used"); } }) };
  try {
    await body();
  } finally {
    methods.forEach((method, index) => { console[method] = saved[index]; });
    if (savedStorage) Object.defineProperty(globalThis, "localStorage", savedStorage);
    else delete globalThis.localStorage;
    delete globalThis.chrome;
  }
  assert.deepEqual(logged, [], "nothing is logged");
}
