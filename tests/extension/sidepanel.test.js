// Runs the real side panel (sidepanel.js, vault-panel.js, tabs.js) against a fake DOM and host.
import { test } from "node:test";
import assert from "node:assert/strict";
import { loadSidePanel, pages, settle } from "./fake-dom.js";

const titles = ($) => $("#results").children.map((li) => li.children[0].children[0].textContent);
const searches = (host) => host.requests.filter((message) => message.type === "search_pages");

async function type($, text) {
  const input = $("#query");
  input.value = text;
  input.dispatch("input", { isComposing: false });
  await settle(260); // debounce is 200 ms
}

test("switching tabs keeps the query, results and scroll position without searching again", async () => {
  const { $, host } = await loadSidePanel({ vaults: { A: pages("A", 120) }, active: "A" });
  await settle(30);
  assert.equal($("#panel-url").hidden, false);
  assert.equal($("#results").children.length, 50);

  await type($, "page 1");
  const shown = titles($);
  assert.equal(shown.length, 31); // page 1, 10-19, 100-119
  $("#url-scroll").scrollTop = 345;
  const before = searches(host).length;

  $("#tab-image").dispatch("click");
  assert.equal($("#panel-url").hidden, true);
  assert.equal($("#panel-image").hidden, false);
  $("#url-scroll").scrollTop = 0; // hidden scroller loses its offset
  $("#tab-settings").dispatch("click");
  await settle(20);
  assert.equal($("#panel-settings").hidden, false);
  $("#tab-url").dispatch("click");

  assert.equal($("#url-scroll").scrollTop, 345);
  assert.equal($("#query").value, "page 1");
  assert.deepEqual(titles($), shown);
  assert.equal(searches(host).length, before, "returning to the URL tab does not re-run the search");
});

test("result clicks still open tabs from the URL tab", async () => {
  const { $, host } = await loadSidePanel({ vaults: { A: pages("A", 3) }, active: "A" });
  await settle(30);
  const link = $("#results").children[0].children[0];
  // The fake DOM does not bubble; the side panel delegates clicks to #results.
  const event = $("#results").dispatch("click", { target: link });
  assert.equal(event.defaultPrevented, true);
  await settle(0);
  assert.deepEqual(host.openedTabs, [{ url: "https://a0.test/", active: true }]);
});

test("without a vault the panel opens on the settings tab and the URL tab points there", async () => {
  const { $ } = await loadSidePanel({ vaults: {}, active: null });
  await settle(30);
  assert.equal($("#tab-settings").getAttribute("aria-selected"), "true");
  assert.equal($("#panel-settings").hidden, false);
  assert.equal($("#panel-url").hidden, true);
  assert.equal($("#settings-attention").hidden, false);
  assert.match($("#tab-settings").getAttribute("aria-label"), /Vaultの確認が必要/);
  assert.equal($("#tab-settings").title, $("#tab-settings").getAttribute("aria-label"), "tooltip matches the accessible name");
  assert.equal(document.activeElement, $("#tab-settings"), "focus follows the startup redirect");
  assert.match($("#vault-state").textContent, /未設定/);

  $("#tab-url").dispatch("click");
  assert.equal($("#error").hidden, false);
  assert.equal($("#open-vault-settings").hidden, false);
  $("#open-vault-settings").dispatch("click");
  assert.equal($("#tab-settings").getAttribute("aria-selected"), "true");
  assert.equal(document.activeElement, $("#tab-settings"));
});

test("a vault switch clears old results at once and late answers from the old vault are ignored", async () => {
  const { $, host } = await loadSidePanel({ vaults: { A: pages("A", 80), B: pages("B", 5) }, active: "A" });
  await settle(30);
  assert.ok(titles($).every((title) => title.startsWith("A ")));
  $("#url-scroll").scrollTop = 500;

  // A slow search against vault A is still running when the user opens vault B.
  host.searchDelay.A = 150;
  host.searchDelay.B = 60;
  await type($, "page");
  $("#tab-settings").dispatch("click");
  $("#vault-path-input").value = "B";
  document.querySelector('[data-manual="vault_open"]').dispatch("click");
  await settle(20);
  assert.equal(host.active, "B");
  assert.equal($("#results").children.length, 0, "vault A results are gone before B answers");
  assert.equal($("#settings-attention").hidden, true);

  await settle(250); // B answers, then the stale A answer arrives
  assert.deepEqual(titles($), ["B page 0", "B page 1", "B page 2", "B page 3", "B page 4"]);
  $("#tab-url").dispatch("click");
  assert.equal($("#url-scroll").scrollTop, 0, "scroll position from vault A is not reused");
  assert.match($("#vault-message").textContent, /Vaultを開きました/);
});

test("the settings tab label and tooltip return to plain once a vault is opened", async () => {
  const { $ } = await loadSidePanel({ vaults: { B: pages("B", 1) }, active: null });
  await settle(30);
  assert.equal($("#tab-settings").title, "設定（Vaultの確認が必要です）");
  $("#vault-path-input").value = "B";
  document.querySelector('[data-manual="vault_open"]').dispatch("click");
  await settle(30);
  assert.equal($("#settings-attention").hidden, true);
  assert.equal($("#tab-settings").getAttribute("aria-label"), "設定");
  assert.equal($("#tab-settings").title, "設定");
});

test("a late vault status never takes focus or the tab away from a user who already started", async () => {
  const { $ } = await loadSidePanel({ vaults: {}, active: null, statusDelay: 80 });
  await settle(10);
  assert.equal(document.activeElement, $("#query"), "search box has focus at start");
  document.dispatch("keydown"); // the user starts typing before the status arrives
  await settle(120);
  assert.equal($("#tab-url").getAttribute("aria-selected"), "true", "no redirect after user input");
  assert.equal($("#panel-url").hidden, false);
  assert.equal(document.activeElement, $("#query"), "focus is not stolen");
  assert.equal($("#settings-attention").hidden, false, "the settings tab still signals the missing vault");
  assert.equal($("#tab-settings").title, "設定（Vaultの確認が必要です）");
});

test("a late vault status still redirects with focus when the user has not acted", async () => {
  const { $ } = await loadSidePanel({ vaults: {}, active: null, statusDelay: 80 });
  await settle(120);
  assert.equal($("#tab-settings").getAttribute("aria-selected"), "true");
  assert.equal(document.activeElement, $("#tab-settings"));
});
