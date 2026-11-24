// Issue #77: the Chrome i18n catalogs, manifest references and message usage of the extension.
import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import {
  catalogProblems, checkExtension, crossCatalogProblems, duplicateJsonKeys, extensionDir, hardCodedJapanese, htmlProblems, manifestProblems, usageProblems,
} from "./i18n-catalog.js";
import { expandPlaceholders, substitute } from "./i18n-chrome-rules.js";
import { createI18n } from "./i18n-mock.js";

// Files whose display text is fully in the catalogs. Each later slice adds the files it localizes.
const LOCALIZED_FILES = [
  "manifest.json", "i18n.js", "i18n-dom.js", "menu-item.js", "service-worker.js", "image-save.js", "page-save.js", "queue-text.js", "format.js", "search-client.js",
  // Slice 2: the Side Panel except the Vault section (sidepanel.html is checked in i18n-sidepanel.test.js).
  "sidepanel.js", "tabs.js", "delete-ui.js", "url-history.js", "visit-calendar.js", "item-dialog.js", "image-gallery.js", "image-client.js",
  "page-panel.js", "page-text.js", "page-client.js", "delete-client.js", "vault-items-client.js", "host-capabilities.js",
];

const entry = (message, placeholders) => ({ message, description: "test", ...(placeholders ? { placeholders } : {}) });
const catalogText = (messages, langTag = "en") => JSON.stringify({ langTag: entry(langTag), ...messages }, null, 2);
const problemsOf = (messages, locale = "en") => catalogProblems(catalogText(messages, locale), locale).problems;

test("the extension's catalogs, manifest and message usage pass every check", () => {
  const { problems, catalogs } = checkExtension();
  assert.deepEqual(problems, []);
  assert.ok(Object.keys(catalogs.ja).length > 100);
});

test("Japanese is the source: both catalogs have the same keys and placeholders", () => {
  const { catalogs } = checkExtension();
  assert.deepEqual(Object.keys(catalogs.en), Object.keys(catalogs.ja));
  assert.equal(catalogs.ja.langTag.message, "ja");
  assert.equal(catalogs.en.langTag.message, "en");
});

test("the catalogs are UTF-8 without a byte order mark, and formatted canonically", () => {
  for (const locale of ["ja", "en"]) {
    const bytes = readFileSync(path.join(extensionDir, "_locales", locale, "messages.json"));
    assert.notDeepEqual([...bytes.subarray(0, 3)], [0xef, 0xbb, 0xbf]);
    const text = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
    assert.equal(text, JSON.stringify(JSON.parse(text), null, 2) + "\n", `${locale}: two-space JSON with a final newline`);
  }
});

test("manifest uses default_locale en and a localized description", () => {
  const manifest = JSON.parse(readFileSync(path.join(extensionDir, "manifest.json"), "utf8"));
  assert.equal(manifest.default_locale, "en");
  assert.equal(manifest.description, "__MSG_extDescription__");
  assert.equal(manifest.name, "Memocampus", "the product name is not translated");
});

test("files that are localized hold no Japanese display text outside comments", () => {
  const problems = LOCALIZED_FILES.flatMap((file) => hardCodedJapanese(file, readFileSync(path.join(extensionDir, file), "utf8")));
  assert.deepEqual(problems, []);
});

// --- Negative controls: each rule really rejects what Chrome (or the project) would not accept -----

test("broken files are rejected: BOM, invalid JSON, duplicate keys, case-insensitive duplicates", () => {
  assert.match(catalogProblems("﻿" + catalogText({}), "en").problems.join("\n"), /byte order mark/);
  assert.match(catalogProblems("{ \"langTag\": ", "en").problems.join("\n"), /not valid JSON/);
  const duplicated = '{"langTag":{"message":"en","description":"d"},"a":{"message":"x","description":"d"},"a":{"message":"y","description":"d"}}';
  assert.deepEqual(duplicateJsonKeys(duplicated), ["a"]);
  assert.match(catalogProblems(duplicated, "en").problems.join("\n"), /"a" occurs twice/);
  assert.deepEqual(duplicateJsonKeys('{"a":{"b":1},"c":{"b":2},"d":[{"b":1},{"b":1}]}'), [], "same names in different objects are fine");
  assert.match(problemsOf({ menuSave: entry("x"), MenuSave: entry("y") }).join("\n"), /case-insensitive/);
});

test("keys, fields and descriptions follow Chrome's and the project's rules", () => {
  assert.match(problemsOf({ "bad-key": entry("x") }).join("\n"), /may use only/);
  assert.match(problemsOf({ "@@ui_locale": entry("x") }).join("\n"), /may use only/);
  assert.match(problemsOf({ a: { message: "x" } }).join("\n"), /description is required/);
  assert.match(problemsOf({ a: { message: "x", description: "costs [amount]$" } }).join("\n"), /description must not contain/);
  assert.match(problemsOf({ a: { message: "", description: "d" } }).join("\n"), /non-empty/);
  assert.match(problemsOf({ a: { message: "x", description: "d", extra: 1 } }).join("\n"), /unknown field/);
});

// What real Chrome for Testing 158 returned for these messages (getMessage with ["X"]); see the dogfood
// and the Slice 1 report. The shared rules must reproduce every row.
const CHROME_OBSERVED = [
  ["Price: $$100", null, "Price: $100"],
  ["github\\.com$$", null, "github\\.com$"],
  ["$$", null, "$"],
  ["a $$$ b", null, "a $$ b"],
  ["$$$$", null, "$$$"],
  ["a $$1 b", null, "a $1 b"],
  ["$$name$$", { name: { content: "$1" } }, "$1"],
  ["$$$count$", { count: { content: "$1" } }, "$$1"],
  ["$Count$ items", { count: { content: "$1" } }, "X items"],
  ["$1 items", null, "X items"],
  ["a $count$0 b", { count: { content: "$1" } }, "a X0 b"],
  ["x $count$y$ z", { count: { content: "$1" } }, "x Xyz"],
  ["cost $x here", null, "cost  here"],
  ["Hello $name", null, "Hello ame"],
  ["end $", null, "end "],
];

test("the shared $ rules reproduce what real Chrome returned", () => {
  for (const [message, placeholders, shown] of CHROME_OBSERVED) {
    const expanded = expandPlaceholders(message, placeholders);
    assert.equal(expanded.error, null, message);
    assert.equal(substitute(expanded.text, ["X"]).text, shown, message);
  }
  // Chrome refused to load the extension for these, with this message.
  assert.equal(expandPlaceholders("$$name$$").error, "Variable $name$ used but not defined.");
  assert.equal(expandPlaceholders("@$na@me$@", { na: { content: "$1" } }).error, "Variable $na@me$ used but not defined.");
});

test("Chrome's \"$$\" (a literal \"$\") is accepted, and getMessage shows one \"$\"", async () => {
  const valid = {
    price: entry("Price: $$100"),
    regex: entry("github\\.com$$"),
    spaced: entry("^abc$$ and $$ alone"),
    mixed: entry("$$ $count$", { count: { content: "$1" } }),
  };
  assert.deepEqual(problemsOf(valid), []);
  assert.deepEqual(problemsOf(valid, "ja").filter((problem) => !/Japanese|langTag/.test(problem)), []);
  // Through the mock, with catalogs on disk as Chrome would read them.
  const dir = mkdtempSync(path.join(tmpdir(), "memocampus-i18n-dollar-"));
  try {
    for (const locale of ["ja", "en"]) {
      mkdirSync(path.join(dir, locale));
      writeFileSync(path.join(dir, locale, "messages.json"), catalogText(valid, locale));
    }
    for (const locale of ["ja", "en"]) {
      const i18n = createI18n({ locale, dir });
      assert.equal(i18n.getMessage("price"), "Price: $100");
      assert.equal(i18n.getMessage("regex"), "github\\.com$");
      assert.equal(i18n.getMessage("spaced"), "^abc$ and $ alone");
      assert.equal(i18n.getMessage("mixed", ["5"]), "$ 5");
    }
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("\"$$name$$\" is not a literal \"$name$\" in Chrome: it still references the placeholder", () => {
  // Chrome refuses to load the extension when name is not defined...
  assert.match(problemsOf({ a: entry("$$name$$") }).join("\n"), /Chrome refuses to load the extension: Variable \$name\$ used but not defined/);
  // ...and shows "$1" (not the value, not "$name$") when it is. A literal "$name$" must be substituted.
  assert.match(problemsOf({ a: entry("$$name$$", { name: { content: "$1" } }) }).join("\n"), /runs into a placeholder/);
  assert.match(problemsOf({ a: entry("$$$count$", { count: { content: "$1" } }) }).join("\n"), /runs into a placeholder/);
});

test("a lone \"$\", an unclosed or malformed reference and a direct $n are rejected", () => {
  const cases = [
    ["costs $ 5", /Chrome drops "\$ "/],
    ["end $", /Chrome drops "\$"/],
    ["github\\.com$", /Chrome drops "\$"/],
    ["Hello $name", /Chrome drops "\$n"/],
    ["$co unt$ items", /Chrome drops "\$c"/],
    ["$na-me$ items", /Chrome drops "\$n"/],
    ["$count$ items", /Chrome refuses to load the extension: Variable \$count\$ used but not defined/],
    ["$1$ items", /Chrome refuses to load the extension: Variable \$1\$ used but not defined/],
    ["costs $5", /"\$5" is written directly \(project rule/],
    ["a $$$ b", /run of 3 "\$" shows 2 in Chrome \(project rule/],
    ["a $$$$ b", /run of 4 "\$" shows 3 in Chrome \(project rule/],
  ];
  for (const [message, pattern] of cases) assert.match(problemsOf({ a: entry(message) }).join("\n"), pattern, message);
});

test("placeholders must be defined, used, named validly and numbered $1…$n", () => {
  assert.match(problemsOf({ a: entry("$count$ items") }).join("\n"), /Variable \$count\$ used but not defined/);
  assert.match(problemsOf({ a: entry("none", { count: { content: "$1" } }) }).join("\n"), /never used/);
  assert.match(problemsOf({ a: entry("$x$ $y$", { x: { content: "$1" }, y: { content: "$3" } }) }).join("\n"), /\$1…\$2/);
  assert.match(problemsOf({ a: entry("$x$", { x: { content: "literal" } }) }).join("\n"), /one of \$1…\$9/);
  assert.match(problemsOf({ a: entry("$x$", { x: { content: "$1" }, X: { content: "$2" } }) }).join("\n"), /defined twice/);
  assert.deepEqual(problemsOf({ a: entry("$Count$ items", { count: { content: "$1" } }) }), [], "placeholder names are case-insensitive, as in Chrome");
});

test("English must not contain Japanese, and langTag must name the catalog", () => {
  assert.match(problemsOf({ a: entry("保存しました") }).join("\n"), /Japanese text/);
  assert.match(problemsOf({ a: entry("（x）") }).join("\n"), /Japanese text/, "full-width punctuation counts");
  assert.match(catalogProblems(catalogText({}, "ja"), "en").problems.join("\n"), /langTag must be "en"/);
});

test("the catalogs must agree on keys and placeholders", () => {
  const ja = { langTag: entry("ja"), a: entry("$n$件", { n: { content: "$1" } }), b: entry("x") };
  assert.deepEqual(crossCatalogProblems({ ja, en: { langTag: entry("en"), a: entry("$n$ items", { n: { content: "$1" } }), b: entry("x") } }), []);
  assert.match(crossCatalogProblems({ ja, en: { langTag: entry("en"), a: entry("$n$", { n: { content: "$1" } }) } }).join("\n"), /missing key b/);
  assert.match(crossCatalogProblems({ ja, en: { langTag: entry("en"), a: entry("$m$", { m: { content: "$1" } }), b: entry("x"), c: entry("y") } }).join("\n"), /other placeholders[\s\S]*not in the Japanese catalog|not in the Japanese catalog[\s\S]*other placeholders/);
});

test("manifest references must exist, take no $ and respect Chrome's length limits", () => {
  const catalogs = { ja: { d: entry("説明") }, en: { d: entry("Description") } };
  assert.deepEqual(manifestProblems({ default_locale: "en", description: "__MSG_d__" }, catalogs), []);
  assert.match(manifestProblems({ description: "__MSG_d__" }, catalogs).join("\n"), /default_locale/);
  assert.match(manifestProblems({ default_locale: "en", description: "__MSG_missing__" }, catalogs).join("\n"), /not in ja[\s\S]*not in en/);
  assert.match(manifestProblems({ default_locale: "en", description: "__MSG_d__" }, { ja: { d: entry("x".repeat(133)) }, en: catalogs.en }).join("\n"), /longer than 132/);
  assert.match(manifestProblems({ default_locale: "en", description: "__MSG_d__" }, { ja: { d: entry("$x$", { x: { content: "$1" } }) }, en: catalogs.en }).join("\n"), /must not contain "\$"/);
});

test("used but undefined and defined but unused messages are both reported", () => {
  const catalog = { langTag: entry("ja"), used: entry("x"), mapped: entry("x"), single: entry("x"), many: entry("x"), unused: entry("x") };
  const sources = [
    'const ERROR_KEYS = { a_code: "mapped" };\nt("used"); t("langTag"); t(pluralKey(n, { one: "single", other: "many" })); t("typo");',
    '<p data-i18n="missingInHtml"></p>',
  ];
  const problems = usageProblems(sources, catalog);
  assert.ok(problems.includes('message "typo" is used but not defined'));
  assert.ok(problems.includes('message "missingInHtml" is used but not defined'));
  assert.ok(problems.includes('message "unused" is never used'));
  assert.equal(problems.length, 3, problems.join("\n"));
});

test("elements with data-i18n must be empty, and Japanese outside comments is found", () => {
  assert.deepEqual(htmlProblems("a.html", '<button data-i18n="k"></button><span data-i18n-title="t">kept</span>'), []);
  assert.match(htmlProblems("a.html", '<p class="x" data-i18n="k">設定</p>').join("\n"), /must be empty/);
  assert.deepEqual(hardCodedJapanese("a.js", "// 日本語のコメント\n/** 説明 */\n * 続き\nconst a = 1;"), []);
  assert.match(hardCodedJapanese("a.js", 'const a = "保存";').join("\n"), /a\.js:1/);
  assert.match(hardCodedJapanese("a.js", 'save(); // 保存').join("\n"), /a\.js:1/, "a trailing comment is not exempt");
});
