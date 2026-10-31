// Pure evaluator: no storage, Chrome API, network, or recording side effects.
import builtins from "./policy-builtins.js";
const MAX_REVISION = Number.MAX_SAFE_INTEGER;
const keys = (value, expected) => value !== null && typeof value === "object" && !Array.isArray(value)
  && Object.keys(value).sort().join(",") === [...expected].sort().join(",");
const categories = ["authentication", "credentials", "government", "webmail", "aiChats"];
const asciiLower = value => value.replace(/[A-Z]/g, c => c.toLowerCase());

// Only the parser's own rejection becomes invalid_url; any other exception stays visible.
function rejectingAs(ErrorType, parse) {
  try {
    return parse();
  } catch (error) {
    if (error instanceof ErrorType) throw new Error("invalid_url");
    throw error;
  }
}
const decodeStrict = text => rejectingAs(URIError, () => decodeURIComponent(text));
const parseUrl = text => rejectingAs(TypeError, () => new URL(text));

function pathOf(path) {
  // Decode once; ambiguous/double encodings and controls fail closed.
  const decoded = decodeStrict(path);
  if (/[\x00-\x20\x7f%\\]/.test(decoded)) throw new Error("invalid_url");
  const parts = [];
  for (const part of decoded.split("/").slice(1)) {
    if (part === ".") continue;
    if (part === "..") parts.pop(); else parts.push(part);
  }
  return "/" + parts.join("/");
}

export function normalizeUrl(input) {
  if (typeof input !== "string" || input.length > 32768 || /[\x00-\x20\x7f\\]/.test(input)) throw new Error("invalid_url");
  let url = parseUrl(input);
  if (url.protocol === "blob:") url = parseUrl(input.slice(5));
  if (!["http:", "https:"].includes(url.protocol) || !url.hostname) throw new Error("invalid_url");
  const host = url.hostname.toLowerCase().replace(/\.+$/, "");
  if (!host) throw new Error("invalid_url");
  // Strict escape validation also covers query/fragment (URLSearchParams is otherwise forgiving).
  for (const text of [url.search, url.hash]) {
    const decoded = decodeStrict(text);
    if (/[\x00-\x1f\x7f%]/.test(decoded)) throw new Error("invalid_url");
  }
  return { host, port: url.port || null, path: pathOf(url.pathname),
    userinfo: !!(url.username || url.password), query: url.search.slice(1), fragment: url.hash.slice(1) };
}

export function parseRule(rule) {
  if (typeof rule !== "string" || rule.length > 2048 || /[\x00-\x20\x7f\\?#@:]/.test(rule)) throw new Error("invalid_policy");
  const wildcard = rule.startsWith("*.");
  const body = wildcard ? rule.slice(2) : rule;
  const slash = body.indexOf("/");
  const rawHost = slash < 0 ? body : body.slice(0, slash);
  const rawPath = slash < 0 ? null : body.slice(slash);
  if (!rawHost || rawHost.includes("*") || (rawPath !== null && (!rawPath.endsWith("/*") || rawPath.slice(0, -1).includes("*")))) throw new Error("invalid_policy");
  const normalized = normalizeUrl("https://" + rawHost + "/");
  if (normalized.userinfo || normalized.port || normalized.path !== "/") throw new Error("invalid_policy");
  const path = rawPath === null ? null : pathOf(rawPath.slice(0, -1));
  return { host: normalized.host, wildcard, path };
}

export function validatePolicy(policy) {
  if (!keys(policy, ["schemaVersion", "engineVersion", "revision", "categories", "layers"])
    || policy.schemaVersion !== 1 || policy.engineVersion !== 1
    || !Number.isSafeInteger(policy.revision) || policy.revision < 1 || policy.revision > MAX_REVISION
    || !keys(policy.categories, categories) || categories.some(key => typeof policy.categories[key] !== "boolean")
    || !keys(policy.layers, ["neverSave", "saveButNoMcp"])) throw new Error("invalid_policy");
  for (const layer of Object.values(policy.layers)) {
    if (!keys(layer, ["enabled", "rules"]) || typeof layer.enabled !== "boolean"
      || !Array.isArray(layer.rules) || layer.rules.length > 1000) throw new Error("invalid_policy");
    layer.rules.forEach(parseRule);
  }
  return policy;
}

function matches(rule, url) {
  const parsed = parseRule(rule);
  return (url.host === parsed.host || (parsed.wildcard && url.host.endsWith("." + parsed.host)))
    && (parsed.path === null || url.path.startsWith(parsed.path));
}
const pathMatches = (path, prefix) => path === prefix || path.startsWith(prefix + "/");
function credentials(url) {
  const fragment = url.fragment.includes("?") ? url.fragment.slice(url.fragment.indexOf("?") + 1) : url.fragment;
  const names = [url.query, url.fragment, fragment].flatMap(text => [...new URLSearchParams(text).keys()].map(asciiLower));
  return url.userinfo || names.some(key => builtins.credentialKeys.includes(key))
    || (names.includes("code") && (names.includes("state") || builtins.callbackPaths.some(path => pathMatches(url.path, path))));
}

/** neverSave governs automatic recording only, not an explicit manual image save. */
export function evaluatePolicy(policy, input) {
  try {
    validatePolicy(policy);
    const url = normalizeUrl(input);
    const a = policy.layers.neverSave;
    const b = policy.layers.saveButNoMcp;
    const c = policy.categories;
    if (a.enabled && (a.rules.some(rule => matches(rule, url))
      || (c.authentication && (builtins.authentication.some(rule => matches(rule, url)) || builtins.authPaths.some(path => pathMatches(url.path, path))))
      || (c.credentials && credentials(url)))) return "neverSave";
    if (b.enabled && (b.rules.some(rule => matches(rule, url))
      || ["government", "webmail", "aiChats"].some(key => c[key] && builtins[key].some(rule => matches(rule, url))))) return "saveButNoMcp";
    return "none";
  } catch { return "neverSave"; }
}
