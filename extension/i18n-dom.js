// Localizes the static text of an extension page (Issue #77). A classic script, loaded with a plain
// <script src="i18n-dom.js"></script> after the page's markup and before the page's module, so the
// page's own code always starts on a localized document. CSP-compatible: no inline code, no eval.
//
//   data-i18n=KEY             -> textContent (the element must be empty in the HTML)
//   data-i18n-aria-label=KEY  -> aria-label
//   data-i18n-title=KEY       -> title
//   data-i18n-placeholder=KEY -> placeholder
//
// Every message is looked up first; only when all of them exist is the document changed, so a missing
// message leaves the page as it was written instead of half translated. Text is never parsed as HTML.
(() => {
  "use strict";
  const ATTRIBUTES = [
    ["data-i18n-aria-label", "aria-label"],
    ["data-i18n-title", "title"],
    ["data-i18n-placeholder", "placeholder"],
  ];
  const message = (key) => {
    const text = chrome.i18n.getMessage(key);
    if (typeof text !== "string" || text === "") throw new Error(`Missing extension message: ${key}`);
    return text;
  };
  const lang = message("langTag");
  const changes = [];
  for (const element of document.querySelectorAll("[data-i18n], [data-i18n-aria-label], [data-i18n-title], [data-i18n-placeholder]")) {
    const textKey = element.getAttribute("data-i18n");
    if (textKey !== null) changes.push({ element, text: message(textKey) });
    for (const [source, attribute] of ATTRIBUTES) {
      const key = element.getAttribute(source);
      if (key !== null) changes.push({ element, attribute, text: message(key) });
    }
  }
  for (const { element, attribute, text } of changes) {
    if (attribute) element.setAttribute(attribute, text);
    else element.textContent = text;
  }
  document.documentElement.lang = lang;
})();
