// A stand-in for chrome.i18n (Issue #77) that reads the real `extension/_locales` catalogs and
// substitutes the way Chrome does. A message missing in the chosen locale falls back to the default
// locale ("en"), as Chrome does per message.
//
// The "$" handling is Chrome's own, shared with the static checks: see i18n-chrome-rules.js.
//
// Stricter than Chrome, to catch mistakes in tests: an unknown key is recorded in `missing` (Chrome
// just returns ""), and a call with a different number of substitutions than the message uses throws.
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { expandPlaceholders, substitute, substitutionCount } from "./i18n-chrome-rules.js";

const localesDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../extension/_locales");
export const DEFAULT_LOCALE = "en";
export const TEST_LOCALES = Object.freeze(["ja", "en"]);
const UI_LANGUAGE = { ja: "ja", en: "en-US" };

/** Messages of one catalog, by lowercased key (Chrome's keys are case-insensitive). */
export function loadMessages(locale, dir = localesDir) {
  const data = JSON.parse(readFileSync(path.join(dir, locale, "messages.json"), "utf8"));
  return new Map(Object.entries(data).map(([key, entry]) => [key.toLowerCase(), entry]));
}

export function createI18n({ locale = "ja", dir = localesDir } = {}) {
  const catalogs = new Map(TEST_LOCALES.map((name) => [name, loadMessages(name, dir)]));
  const state = { locale, missing: [] };
  const i18n = {
    missing: state.missing,
    get locale() { return state.locale; },
    setLocale(next) {
      if (!catalogs.has(next)) throw new Error(`no catalog for ${next}`);
      state.locale = next;
    },
    getUILanguage: () => UI_LANGUAGE[state.locale],
    getMessage(key, substitutions) {
      if (typeof key !== "string") throw new TypeError("getMessage needs a string key");
      const values = substitutions === undefined ? [] : Array.isArray(substitutions) ? substitutions : [substitutions];
      if (values.length > 9) throw new Error("Chrome allows at most 9 substitutions");
      if (!values.every((value) => typeof value === "string")) throw new TypeError(`substitutions for ${key} must be strings`);
      const entry = catalogs.get(state.locale).get(key.toLowerCase()) ?? catalogs.get(DEFAULT_LOCALE).get(key.toLowerCase());
      if (!entry) {
        state.missing.push(key);
        return "";
      }
      const expanded = expandPlaceholders(entry.message, entry.placeholders);
      if (expanded.error) throw new Error(`${key}: Chrome would not load this catalog: ${expanded.error}`);
      const needed = substitutionCount(expanded.text);
      if (values.length !== needed) throw new Error(`message ${key} takes ${needed} substitution(s), got ${values.length}`);
      return substitute(expanded.text, values).text;
    },
  };
  return i18n;
}

/** The one instance tests share (installed by i18n-setup.js); tests switch it with `withLocale`. */
export const chromeI18n = createI18n();

/** Runs `body` with the shared chrome.i18n in `locale`, restoring the previous locale afterwards. */
export async function withLocale(locale, body) {
  const previous = chromeI18n.locale;
  chromeI18n.setLocale(locale);
  try {
    return await body();
  } finally {
    chromeI18n.setLocale(previous);
  }
}
