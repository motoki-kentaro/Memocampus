// Display text for the extension (Issue #77). Messages come only from Chrome's own i18n
// (`_locales/<lang>/messages.json`, `default_locale` "en"): Chrome picks the catalog from its UI
// language and falls back to English, per message. Nothing here decides what is saved or shown to
// MCP; a missing message throws instead of turning into an empty or untranslated string.

/** The Intl locale for each catalog. Any English UI (en-GB, en-AU, …) uses en-US formats. */
const INTL_LOCALES = Object.freeze({ ja: "ja-JP", en: "en-US" });
const MAX_SUBSTITUTIONS = 9;

export class MissingMessageError extends Error {
  constructor(key) {
    super(`Missing extension message: ${key}`);
    this.name = "MissingMessageError";
    this.key = key;
  }
}

/**
 * The message `key`, with `substitutions` (strings or numbers) filling its placeholders in order.
 * Substituted values are inserted as they are: Chrome does not re-read them for placeholders, and
 * the text is only ever used as text (textContent, attributes, notifications), never as HTML.
 */
export function t(key, ...substitutions) {
  const i18n = globalThis.chrome?.i18n;
  if (typeof i18n?.getMessage !== "function") throw new MissingMessageError(key);
  if (substitutions.length > MAX_SUBSTITUTIONS) throw new RangeError(`Too many substitutions for ${key}`);
  const values = substitutions.map((value) => {
    if (typeof value !== "string" && typeof value !== "number") throw new TypeError(`Substitution for ${key} must be a string or a number`);
    return String(value);
  });
  const text = values.length > 0 ? i18n.getMessage(key, values) : i18n.getMessage(key);
  if (typeof text !== "string" || text === "") throw new MissingMessageError(key);
  return text;
}

/** The language of the catalog Chrome actually used ("ja" or "en"), read from the catalog itself. */
export function uiLanguage() {
  const tag = t("langTag");
  return Object.hasOwn(INTL_LOCALES, tag) ? tag : "en";
}

/** The locale for Intl formatting of the language in use. */
export const intlLocale = () => INTL_LOCALES[uiLanguage()];

export const formatNumber = (value, options = {}) => new Intl.NumberFormat(intlLocale(), options).format(value);

export const formatDateTime = (date, options) => new Intl.DateTimeFormat(intlLocale(), options).format(date);

/**
 * The message key for `count` among plural `forms` ({ one, other, … }, Intl.PluralRules
 * categories), falling back to `other`. Japanese has only `other`.
 */
export function pluralKey(count, forms) {
  const category = new Intl.PluralRules(intlLocale()).select(count);
  return Object.hasOwn(forms, category) ? forms[category] : forms.other;
}
