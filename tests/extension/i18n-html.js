// An extension page's HTML as a person sees it in one language (Issue #77): every data-i18n* is replaced by its
// message from the real catalog, the way i18n-dom.js does in Chrome, and the data-i18n* attribute itself is dropped.
// Tests that read the page's text (labels, notes, accessible names) read this, not the raw file: the raw file holds
// keys only. A key missing from the catalog throws.
import { readFileSync } from "node:fs";
import path from "node:path";
import { createI18n } from "./i18n-mock.js";
import { extensionDir } from "./i18n-catalog.js";

export function localizeHtml(text, locale = "ja") {
  const i18n = createI18n({ locale });
  const message = (key) => {
    const value = i18n.getMessage(key);
    if (typeof value !== "string" || value === "") throw new Error(`missing message ${key}`);
    if (/[<>"]/.test(value)) throw new Error(`message ${key} cannot be put into HTML text as is`);
    return value;
  };
  return text
    .replace(/ data-i18n-(aria-label|title|placeholder)="([^"]*)"/g, (_, attribute, key) => ` ${attribute}="${message(key)}"`)
    .replace(/(<([a-zA-Z][\w-]*)\b[^>]*?) data-i18n="([^"]*)"([^>]*>)<\/\2>/g, (_, open, tag, key, rest) => `${open}${rest}${message(key)}</${tag}>`);
}

/** An extension page (by file name) localized in `locale`. */
export const localizedPage = (file, locale = "ja") => localizeHtml(readFileSync(path.join(extensionDir, file), "utf8"), locale);
