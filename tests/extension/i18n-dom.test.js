// Issue #77: i18n-dom.js, the classic script that localizes a page's static text before the page's
// module runs. Run here as the browser would run it: as a plain script against a document and chrome.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import vm from "node:vm";
import { extensionDir } from "./fake-dom.js";
import { createI18n } from "./i18n-mock.js";

const script = readFileSync(path.join(extensionDir, "i18n-dom.js"), "utf8");

/** Elements with attributes and text only; innerHTML and friends are traps. */
function element(attributes = {}, text = "") {
  const attrs = new Map(Object.entries(attributes));
  const el = {
    textContent: text,
    getAttribute: (name) => (attrs.has(name) ? attrs.get(name) : null),
    setAttribute: (name, value) => attrs.set(name, String(value)),
    attrs,
  };
  for (const name of ["innerHTML", "outerHTML"]) Object.defineProperty(el, name, { set() { throw new Error(`${name} used`); }, get() { throw new Error(`${name} read`); } });
  el.insertAdjacentHTML = () => { throw new Error("insertAdjacentHTML used"); };
  return el;
}

function run(elements, locale = "ja", i18n = createI18n({ locale })) {
  const root = { lang: "ja" };
  const selectors = [];
  const document = {
    documentElement: root,
    querySelectorAll(selector) {
      selectors.push(selector);
      return elements;
    },
  };
  const events = [];
  const context = vm.createContext({ document, chrome: { i18n }, events });
  vm.runInContext(script, context, { filename: "i18n-dom.js" });
  return { root, selectors };
}

test("text and the three attributes are filled from the catalog, and <html lang> follows it", () => {
  const button = element({ "data-i18n": "menuSaveImage", "data-i18n-title": "menuSavePage", "data-i18n-aria-label": "imageSavedTitle" });
  const input = element({ "data-i18n-placeholder": "queueErrorGeneric" });
  const { root, selectors } = run([button, input], "en");
  assert.equal(button.textContent, "Save to Memocampus");
  assert.equal(button.attrs.get("title"), "Save page to Memocampus");
  assert.equal(button.attrs.get("aria-label"), "Saved to Memocampus");
  assert.equal(input.attrs.get("placeholder"), "Recording failed.");
  assert.equal(input.textContent, "", "an attribute-only element keeps its text");
  assert.equal(root.lang, "en");
  assert.deepEqual(selectors, ["[data-i18n], [data-i18n-aria-label], [data-i18n-title], [data-i18n-placeholder]"]);
  const ja = element({ "data-i18n": "menuSaveImage" });
  assert.equal(run([ja], "ja").root.lang, "ja");
  assert.equal(ja.textContent, "Memocampusへ保存");
});

test("message text is set as text, never parsed as HTML", () => {
  const i18n = createI18n({ locale: "en" });
  const original = i18n.getMessage;
  i18n.getMessage = (key, ...rest) => (key === "menuSaveImage" ? "<img src=x onerror=alert(1)>" : original(key, ...rest));
  const target = element({ "data-i18n": "menuSaveImage", "data-i18n-title": "menuSaveImage" });
  run([target], "en", i18n);
  assert.equal(target.textContent, "<img src=x onerror=alert(1)>");
  assert.equal(target.attrs.get("title"), "<img src=x onerror=alert(1)>");
});

test("a missing message throws and leaves the whole page untouched", () => {
  const first = element({ "data-i18n": "menuSaveImage" }, "");
  const broken = element({ "data-i18n-title": "noSuchMessage" });
  assert.throws(() => run([first, broken], "en"), /Missing extension message: noSuchMessage/);
  assert.equal(first.textContent, "", "nothing was changed before the failure was found");
  assert.equal(broken.attrs.get("title"), undefined);
  const root = { lang: "ja" };
  assert.throws(() => vm.runInContext(script, vm.createContext({ document: { documentElement: root, querySelectorAll: () => [] }, chrome: { i18n: { getMessage: () => "" } } })));
  assert.equal(root.lang, "ja");
});

test("the script runs synchronously and is CSP-safe: no modules, eval, timers, network or HTML", () => {
  const target = element({ "data-i18n": "menuSavePage" });
  run([target], "en");
  assert.equal(target.textContent, "Save page to Memocampus", "done when the script returns, before any later script");
  const code = script.split("\n").filter((line) => !/^\s*\/\//.test(line)).join("\n");
  assert.doesNotMatch(code, /\b(import|export|eval|Function|setTimeout|setInterval|requestAnimationFrame|fetch|innerHTML|outerHTML|insertAdjacentHTML|document\.write|addEventListener)\b/);
});
