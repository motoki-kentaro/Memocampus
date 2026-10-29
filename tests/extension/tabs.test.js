import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, existsSync } from "node:fs";
import path from "node:path";
import { createTabs, nextTabIndex } from "../../extension/tabs.js";
import { createDocument, extensionDir } from "./fake-dom.js";

const repoRoot = path.resolve(extensionDir, "..");
const read = (...parts) => readFileSync(path.join(...parts));

function setup() {
  const document = createDocument();
  const entries = ["url", "image", "settings"].map((id) => ({
    id,
    tab: document.createElement("button"),
    panel: document.createElement("section"),
  }));
  const changes = [];
  const tabs = createTabs(entries, { onChange: (id, previous) => changes.push([id, previous]) });
  tabs.select("url");
  return { document, entries, tabs, changes };
}

test("arrow keys wrap, Home and End jump, other keys are ignored", () => {
  assert.equal(nextTabIndex("ArrowRight", 2, 3), 0);
  assert.equal(nextTabIndex("ArrowLeft", 0, 3), 2);
  assert.equal(nextTabIndex("Home", 2, 3), 0);
  assert.equal(nextTabIndex("End", 0, 3), 2);
  assert.equal(nextTabIndex("Enter", 0, 3), null);
});

test("exactly one tab is selected, focusable and showing its panel", () => {
  const { entries, tabs, changes } = setup();
  tabs.select("image");
  assert.deepEqual(entries.map((e) => e.tab.getAttribute("aria-selected")), ["false", "true", "false"]);
  assert.deepEqual(entries.map((e) => e.tab.tabIndex), [-1, 0, -1]);
  assert.deepEqual(entries.map((e) => e.panel.hidden), [true, false, true]);
  tabs.select("image"); // re-selecting is not a change
  assert.deepEqual(changes, [["url", null], ["image", "url"]]);
  assert.equal(tabs.current, "image");
});

test("keyboard navigation moves selection and focus", () => {
  const { document, entries, tabs } = setup();
  const press = (index, key) => entries[index].tab.dispatch("keydown", { key });
  assert.equal(press(0, "ArrowRight").defaultPrevented, true);
  assert.equal(tabs.current, "image");
  assert.equal(document.activeElement, entries[1].tab);
  press(1, "End");
  assert.equal(tabs.current, "settings");
  press(2, "ArrowRight");
  assert.equal(tabs.current, "url");
  assert.equal(press(0, "a").defaultPrevented, false);
  entries[2].tab.dispatch("click");
  assert.equal(tabs.current, "settings");
});

test("scroll offsets survive switching, also when the browser resets a hidden scroller", () => {
  const { entries, tabs } = setup();
  entries[0].panel.scrollTop = 420;
  tabs.select("settings");
  entries[0].panel.scrollTop = 0; // a display:none scroller may report or keep 0
  entries[2].panel.scrollTop = 80;
  assert.equal(tabs.getScroll("url"), 420);
  tabs.select("url");
  assert.equal(entries[0].panel.scrollTop, 420);
  tabs.select("settings");
  assert.equal(entries[2].panel.scrollTop, 80);
});

test("setScroll on a hidden tab applies when it is shown", () => {
  const { entries, tabs } = setup();
  entries[0].panel.scrollTop = 300;
  tabs.select("settings");
  tabs.setScroll("url", 0);
  assert.equal(tabs.getScroll("url"), 0);
  tabs.select("url");
  assert.equal(entries[0].panel.scrollTop, 0);
});

test("sidepanel.html wires three tabs to their panels", () => {
  const html = read(extensionDir, "sidepanel.html").toString("utf8");
  const tabs = [...html.matchAll(/<button id="(tab-[a-z]+)"[^>]*role="tab"[^>]*aria-controls="([a-z-]+)"/g)].map((m) => [m[1], m[2]]);
  assert.deepEqual(tabs, [["tab-url", "panel-url"], ["tab-image", "panel-image"], ["tab-settings", "panel-settings"]]);
  for (const [tab, panel] of tabs) {
    assert.match(html, new RegExp(`<section id="${panel}"[^>]*role="tabpanel"[^>]*aria-labelledby="${tab}"`), panel);
  }
  assert.match(html, /role="tablist"/);
  assert.match(html, /id="tab-settings"[^>]*aria-label="設定"/, "icon-only tab has an accessible name");
  assert.match(html, /<span id="settings-attention"[^>]*aria-hidden="true"/, "the attention dot is decorative");
  assert.doesNotMatch(html, /⚙/, "no Unicode gear substitute");
});

test("the Image tab is a placeholder only", () => {
  const html = read(extensionDir, "sidepanel.html").toString("utf8");
  const panel = /<section id="panel-image"[\s\S]*?<\/section>/.exec(html)[0];
  assert.match(panel, /画像ライブラリ/);
  assert.match(panel, /今後実装予定/);
  assert.doesNotMatch(panel, /<(button|input|img|a|canvas|form)\b/);
  const manifest = JSON.parse(read(extensionDir, "manifest.json").toString("utf8"));
  assert.ok(!manifest.permissions.includes("contextMenus"), "no context menu permission");
});

test("the settings icon is an unmodified copy of assets/settings.svg without active content", () => {
  const original = read(repoRoot, "assets", "settings.svg");
  const packaged = read(extensionDir, "assets", "settings.svg");
  assert.ok(original.equals(packaged), "extension/assets/settings.svg must stay byte-identical to assets/settings.svg");
  const svg = packaged.toString("utf8");
  assert.doesNotMatch(svg, /<script|<foreignObject|\bon\w+=|href=|url\(/i);
  const css = read(extensionDir, "sidepanel.css").toString("utf8");
  for (const reference of css.matchAll(/url\("([^"]+)"\)/g)) {
    assert.ok(existsSync(path.join(extensionDir, reference[1])), `${reference[1]} is packaged`);
  }
  assert.match(css, /mask: url\("assets\/settings\.svg"\)/);
});
