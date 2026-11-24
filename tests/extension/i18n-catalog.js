// Static checks of the extension's Chrome i18n catalogs (Issue #77), shared by every slice. A broken
// messages.json or manifest reference stops Chrome from loading the extension at all (and with it,
// automatic recording), so everything Chrome would reject is rejected here first, plus the project's
// own rules. Each function returns a list of problems; an empty list means the check passed.
import { readFileSync, readdirSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { expandPlaceholders, substitute } from "./i18n-chrome-rules.js";

export const extensionDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../extension");
export const LOCALES = Object.freeze(["ja", "en"]);
export const DEFAULT_LOCALE = "en";
const NAME = /^[A-Za-z0-9_]+$/;
const JAPANESE = /[　-ヿ㐀-䶿一-鿿豈-﫿＀-￯]/u;
// Chrome Web Store / chrome://extensions limits for manifest strings.
const MANIFEST_LIMITS = { name: 75, short_name: 12, description: 132 };

/** Object keys that occur twice in the same JSON object (JSON.parse would silently keep the last). */
export function duplicateJsonKeys(text) {
  const duplicates = [];
  const stack = [];
  let i = 0;
  const readString = () => {
    let out = "";
    i += 1;
    while (i < text.length && text[i] !== '"') {
      if (text[i] === "\\") { out += text.slice(i, i + 2); i += 2; } else out += text[i++];
    }
    i += 1;
    return JSON.parse(`"${out}"`);
  };
  while (i < text.length) {
    const c = text[i];
    if (c === "{") { stack.push({ keys: new Set(), expectKey: true }); i += 1; }
    else if (c === "[") { stack.push(null); i += 1; }
    else if (c === "}" || c === "]") { stack.pop(); i += 1; }
    else if (c === ",") { const top = stack.at(-1); if (top) top.expectKey = true; i += 1; }
    else if (c === '"') {
      const value = readString();
      const top = stack.at(-1);
      if (top?.expectKey) {
        if (top.keys.has(value)) duplicates.push(value);
        top.keys.add(value);
        top.expectKey = false;
      }
    } else i += 1;
  }
  return duplicates;
}

/** Problems of one messages.json text, by Chrome's rules and the project's stricter ones. */
export function catalogProblems(text, locale) {
  const problems = [];
  const at = (message) => problems.push(`${locale}: ${message}`);
  if (text.charCodeAt(0) === 0xfeff) at("starts with a byte order mark");
  let data;
  try {
    data = JSON.parse(text);
  } catch (error) {
    at(`is not valid JSON (${error.message})`);
    return { problems, data: null };
  }
  for (const key of duplicateJsonKeys(text)) at(`key "${key}" occurs twice in one object`);
  if (data === null || typeof data !== "object" || Array.isArray(data)) {
    at("is not a JSON object");
    return { problems, data: null };
  }
  const seen = new Map();
  for (const [key, entry] of Object.entries(data)) {
    if (!NAME.test(key)) at(`key "${key}" may use only A-Z, a-z, 0-9 and _`);
    const folded = key.toLowerCase();
    if (seen.has(folded)) at(`keys "${seen.get(folded)}" and "${key}" are the same to Chrome (case-insensitive)`);
    seen.set(folded, key);
    if (entry === null || typeof entry !== "object" || Array.isArray(entry)) { at(`${key}: entry is not an object`); continue; }
    for (const field of Object.keys(entry)) if (!["message", "description", "placeholders"].includes(field)) at(`${key}: unknown field "${field}"`);
    if (typeof entry.message !== "string" || entry.message === "") at(`${key}: message must be a non-empty string`);
    if (typeof entry.description !== "string" || entry.description.trim() === "") at(`${key}: description is required`);
    // Project rule: Chrome ignores descriptions; "$" is kept out so that they never read as messages.
    else if (entry.description.includes("$")) at(`${key}: description must not contain "$" (project rule)`);
    const placeholders = new Map();
    if (entry.placeholders !== undefined) {
      if (entry.placeholders === null || typeof entry.placeholders !== "object" || Array.isArray(entry.placeholders)) at(`${key}: placeholders must be an object`);
      else for (const [name, value] of Object.entries(entry.placeholders)) {
        if (!NAME.test(name)) at(`${key}: placeholder name "${name}" may use only A-Z, a-z, 0-9 and _`);
        if (placeholders.has(name.toLowerCase())) at(`${key}: placeholder "${name}" is defined twice (case-insensitive)`);
        if (value === null || typeof value !== "object" || Array.isArray(value)) { at(`${key}: placeholder "${name}" is not an object`); continue; }
        for (const field of Object.keys(value)) if (!["content", "example"].includes(field)) at(`${key}: placeholder "${name}" has unknown field "${field}"`);
        // Project rule: Chrome also allows fixed text as content; here a placeholder is always a substitution.
        if (typeof value.content !== "string" || !/^\$[1-9]$/.test(value.content)) at(`${key}: placeholder "${name}" content must be one of $1…$9 (project rule)`);
        if (value.example !== undefined && typeof value.example !== "string") at(`${key}: placeholder "${name}" example must be a string`);
        placeholders.set(name.toLowerCase(), value?.content);
      }
    }
    if (typeof entry.message !== "string") continue;
    // "$" is read the way Chrome reads it (the shared algorithms in i18n-chrome-rules.js, checked against real
    // Chrome): first $name$ references at load time, then $1…$9 and "$$" in getMessage.
    const expanded = expandPlaceholders(entry.message, placeholders);
    // Chrome: an undefined $name$ (also inside "$$name$$") makes Chrome refuse to load the extension.
    if (expanded.error) { at(`${key}: Chrome refuses to load the extension: ${expanded.error}`); continue; }
    const inPlaceholder = (index) => expanded.inserted.some(([start, end]) => index >= start && index < end);
    for (const token of substitute(expanded.text).tokens) {
      const shown = expanded.text.slice(Math.max(0, token.at - 8), token.at + 10);
      if (token.kind === "dropped") {
        // Chrome: a "$" that starts no reference, "$$" or $1…$9 is dropped (with the next character).
        at(`${key}: Chrome drops ${JSON.stringify(token.text)} in "…${shown}…"; write "$$" for a literal "$", or $name$ for a placeholder`);
      } else if (token.kind === "literal") {
        const run = Array.from({ length: token.run }, (_, offset) => token.at + offset);
        if (run.some(inPlaceholder)) {
          // Chrome: "$$" next to a placeholder merges with its "$n" ("$$count$" shows "$1", not "$" and the value).
          at(`${key}: a literal "$" runs into a placeholder in "…${shown}…"; Chrome would not substitute it — keep "$$" apart from $name$`);
        } else if (token.run !== 2) {
          // Project rule: Chrome turns a run of n "$" into n - 1; only the documented "$$" (one "$") is allowed.
          at(`${key}: a run of ${token.run} "$" shows ${token.run - 1} in Chrome (project rule: only "$$", for one literal "$")`);
        }
      } else if (!inPlaceholder(token.at)) {
        // Project rule: $1…$9 only through named placeholders, so that ja and en can be compared by name.
        at(`${key}: "$${token.index + 1}" is written directly (project rule: use a named $name$ placeholder)`);
      }
    }
    const used = new Set(expanded.references);
    // Project rule: every defined placeholder is used.
    for (const name of placeholders.keys()) if (!used.has(name)) at(`${key}: placeholder "${name}" is never used`);
    const contents = [...placeholders.values()].filter((content) => typeof content === "string").map((content) => Number(content.slice(1))).sort((a, b) => a - b);
    if (contents.some((n, index) => n !== index + 1)) at(`${key}: placeholder contents must be $1…$${contents.length}, each once`);
  }
  if (locale !== "ja" && locale !== "en") at("is not a supported locale");
  if (data.langTag?.message !== locale) at(`langTag must be "${locale}"`);
  if (locale !== "ja") {
    for (const [key, entry] of Object.entries(data)) if (typeof entry?.message === "string" && JAPANESE.test(entry.message)) at(`${key}: Japanese text in the ${locale} catalog`);
  }
  return { problems, data };
}

const placeholderShape = (entry) => JSON.stringify(Object.entries(entry?.placeholders ?? {}).map(([name, value]) => [name.toLowerCase(), value?.content]).sort());

/** Problems between catalogs: the same keys (Japanese is the source) and the same placeholders. */
export function crossCatalogProblems(catalogs) {
  const problems = [];
  const source = catalogs.ja ?? {};
  for (const [locale, data] of Object.entries(catalogs)) {
    if (locale === "ja" || !data) continue;
    const keys = new Set(Object.keys(data).map((key) => key.toLowerCase()));
    const sourceKeys = new Set(Object.keys(source).map((key) => key.toLowerCase()));
    for (const key of sourceKeys) if (!keys.has(key)) problems.push(`${locale}: missing key ${key}`);
    for (const key of keys) if (!sourceKeys.has(key)) problems.push(`${locale}: key ${key} is not in the Japanese catalog`);
    for (const [key, entry] of Object.entries(source)) {
      const other = Object.entries(data).find(([name]) => name.toLowerCase() === key.toLowerCase())?.[1];
      if (other && placeholderShape(other) !== placeholderShape(entry)) problems.push(`${locale}: ${key} has other placeholders than in Japanese`);
    }
  }
  return problems;
}

/** Problems of manifest.json's use of the catalogs. */
export function manifestProblems(manifest, catalogs) {
  const problems = [];
  if (manifest.default_locale !== DEFAULT_LOCALE) problems.push(`default_locale must be "${DEFAULT_LOCALE}"`);
  const text = JSON.stringify(manifest);
  for (const [, key] of text.matchAll(/__MSG_([A-Za-z0-9_@]+)__/g)) {
    for (const locale of LOCALES) {
      const entry = Object.entries(catalogs[locale] ?? {}).find(([name]) => name.toLowerCase() === key.toLowerCase())?.[1];
      if (!entry) problems.push(`manifest: __MSG_${key}__ is not in ${locale}`);
      // Chrome applies only the load-time step to manifest strings, so "$$" is shown as "$$" there
      // (checked in real Chrome). Project rule: no "$" at all in messages the manifest uses.
      else if (entry.message.includes("$")) problems.push(`manifest: ${key} in ${locale} must not contain "$" (project rule: Chrome does not unescape "$$" in the manifest)`);
    }
  }
  for (const [field, limit] of Object.entries(MANIFEST_LIMITS)) {
    if (typeof manifest[field] !== "string") continue;
    for (const locale of LOCALES) {
      const value = manifest[field].replace(/__MSG_([A-Za-z0-9_]+)__/g, (_, key) => Object.entries(catalogs[locale] ?? {}).find(([name]) => name.toLowerCase() === key.toLowerCase())?.[1]?.message ?? "");
      if ([...value].length > limit) problems.push(`manifest: ${field} in ${locale} is longer than ${limit} characters`);
    }
  }
  return problems;
}

function filesUnder(dir) {
  const out = [];
  for (const name of readdirSync(dir)) {
    const full = path.join(dir, name);
    if (statSync(full).isDirectory()) out.push(...filesUnder(full));
    else out.push(full);
  }
  return out;
}

/** The extension's own text files that may reference messages. */
export function sourceFiles(dir = extensionDir) {
  return filesUnder(dir).filter((file) => /\.(js|html|css|json)$/.test(file) && !file.includes(`${path.sep}_locales${path.sep}`));
}

/** Message keys the sources use: t("key"), *_KEYS maps, plural forms, data-i18n* and __MSG_key__. */
export function referencedKeys(texts) {
  const keys = new Set();
  for (const text of texts) {
    for (const [, key] of text.matchAll(/\bt\(\s*"([^"]*)"/g)) keys.add(key);
    for (const [, key] of text.matchAll(/\b(?:zero|one|two|few|many|other):\s*"([^"]*)"/g)) keys.add(key);
    for (const [, body] of text.matchAll(/const [A-Z_]+_KEYS = (?:Object\.freeze\()?\{([\s\S]*?)\}/g)) for (const [, key] of body.matchAll(/:\s*"([^"]*)"/g)) keys.add(key);
    for (const [, key] of text.matchAll(/data-i18n(?:-aria-label|-title|-placeholder)?="([^"]*)"/g)) keys.add(key);
    for (const [, key] of text.matchAll(/__MSG_([A-Za-z0-9_@]+)__/g)) keys.add(key);
  }
  return keys;
}

/** Problems between the sources and the catalog: undefined references and unused messages. */
export function usageProblems(texts, catalog) {
  const problems = [];
  const defined = new Set(Object.keys(catalog).map((key) => key.toLowerCase()));
  for (const key of referencedKeys(texts)) if (!defined.has(key.toLowerCase())) problems.push(`message "${key}" is used but not defined`);
  // A key counts as used where the sources name it as a whole string literal (or __MSG_key__).
  const literals = new Set();
  for (const text of texts) {
    for (const [, key] of text.matchAll(/["'`]([A-Za-z0-9_]+)["'`]/g)) literals.add(key.toLowerCase());
    for (const [, key] of text.matchAll(/__MSG_([A-Za-z0-9_]+)__/g)) literals.add(key.toLowerCase());
  }
  for (const key of Object.keys(catalog)) if (!literals.has(key.toLowerCase())) problems.push(`message "${key}" is never used`);
  return problems;
}

/** Elements carrying data-i18n must be empty in the HTML: the script replaces their whole text. */
export function htmlProblems(file, text) {
  const problems = [];
  for (const [, tag, inner] of text.matchAll(/<([a-zA-Z][\w-]*)\b[^>]*\sdata-i18n="[^"]*"[^>]*>([\s\S]*?)<\/\1>/g)) {
    if (inner.trim() !== "") problems.push(`${path.basename(file)}: <${tag} data-i18n> must be empty in the HTML, found "${inner.trim().slice(0, 40)}"`);
  }
  return problems;
}

/**
 * Lines with Japanese text outside comments, in files that are already fully localized. Japanese may
 * appear only on comment lines (starting with //, /*, * or <!--), so that no display text is left behind.
 */
export function hardCodedJapanese(file, text) {
  const problems = [];
  text.split("\n").forEach((line, index) => {
    if (!JAPANESE.test(line)) return;
    if (/^\s*(\/\/|\/\*|\*|<!--)/.test(line)) return;
    problems.push(`${path.basename(file)}:${index + 1}: ${line.trim().slice(0, 60)}`);
  });
  return problems;
}

/** All catalog checks for the extension as it is on disk. */
export function checkExtension(dir = extensionDir) {
  const problems = [];
  const localesDir = path.join(dir, "_locales");
  const folders = readdirSync(localesDir).sort();
  if (JSON.stringify(folders) !== JSON.stringify([...LOCALES].sort())) problems.push(`_locales must hold exactly ${LOCALES.join(", ")}, found ${folders.join(", ")}`);
  const catalogs = {};
  for (const locale of LOCALES) {
    const files = readdirSync(path.join(localesDir, locale));
    if (JSON.stringify(files) !== JSON.stringify(["messages.json"])) problems.push(`_locales/${locale} must hold only messages.json, found ${files.join(", ")}`);
    const bytes = readFileSync(path.join(localesDir, locale, "messages.json"));
    const text = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
    const result = catalogProblems(text, locale);
    problems.push(...result.problems);
    catalogs[locale] = result.data;
  }
  problems.push(...crossCatalogProblems(catalogs));
  const manifest = JSON.parse(readFileSync(path.join(dir, "manifest.json"), "utf8"));
  problems.push(...manifestProblems(manifest, catalogs));
  const sources = sourceFiles(dir);
  const texts = sources.map((file) => readFileSync(file, "utf8"));
  if (catalogs.ja) problems.push(...usageProblems(texts, catalogs.ja));
  sources.forEach((file, index) => { if (file.endsWith(".html")) problems.push(...htmlProblems(file, texts[index])); });
  return { problems, catalogs };
}
