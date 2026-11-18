// HISTORY-04/05/06 regression checks. Synthetic in-memory Vaults only; held replies are captured before release.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { extensionDir } from "./fake-dom.js";
import { at, openHistory, settle, tokenOf, visit } from "./history-fixtures.js";
import { isPickerSelectionValid } from "../../extension/history-app.js";

const walk = (node) => [node, ...node.children.flatMap(walk)];
const text = (node) => [node.textContent, ...node.children.map(text)].join(" ");
const classes = (node, name) => walk(node).filter((el) => el.className.split(" ").includes(name));
const rows = (p) => p.$("history-list").children;
const ui = (p, id = "a") => rows(p).find((row) => row.dataset.visitId === id)?.ui;
const LONG = `https://a.synthetic.test/${"x".repeat(2500)}`;
const A = [visit("a", LONG, "A page", at(2026, 11, 17)), visit("a2", "https://source.synthetic.test/", "A source", at(2026, 11, 17, 11))];
const B = [visit("b", "https://b.synthetic.test/", "B page", at(2026, 11, 17))];
const page = () => openHistory({ search: "?date=2026-11-17", visits: { A, B }, host: { relations: { a: { source: "a2", pageMetadata: { ogTitle: "OG", capturedAt: at(2026, 11, 17) } } } } });
const cut = (p) => classes(ui(p).row, "visit-link")[0];
const focus = async (p) => { p.clock.advance(1500); p.win.focus(); await settle(10); };

function holdReplies(p, type, transform = (reply) => reply) {
  const original = p.host.send;
  const held = [];
  let enabled = true;
  p.host.send = async (message) => {
    const reply = await original(message);
    if (message.type !== type || !enabled) return reply;
    return new Promise((resolve) => held.push({ message, release: () => resolve(transform(reply, message)) }));
  };
  return { held, stop: () => { enabled = false; }, release: () => held.forEach((entry) => entry.release()) };
}
const errorReply = (code) => (_reply, message) => ({ ok: false, type: `${message.type}_result`, requestId: message.requestId, error: code });
const assertRevoked = (p) => {
  assert.equal(p.app.vaultToken, null);
  assert.equal(rows(p).length, 0);
  assert.equal(p.$("history-list").hidden, true);
  assert.equal(classes(p.$("cal-table"), "has-visits").length, 0);
  assert.equal(p.$("history-retry").hidden, false);
};

for (const [code, message] of [["vault_locked", /ロック/], ["vault_not_configured", /未設定/], ["vault_schema_mismatch", /現在の形式に対応/]]) {
  test(`HISTORY-04: focus ${code} revokes A, its pending detail and full URL; Retry reads B`, async () => {
    const p = await page();
    const old = ui(p);
    const replies = holdReplies(p, "history_visit");
    old.toggle.dispatch("click");
    cut(p).dispatch("click");
    await settle(5);
    assert.equal(replies.held.length, 2);
    p.host.vault = "B";
    p.host.errors.history_days = code;
    await focus(p);
    assertRevoked(p);
    assert.match(p.$("history-state-text").textContent, message);
    assert.equal(old.region.children.length, 0, "even the old detached region is emptied");
    const sent = p.host.of("history_visit").length;
    old.toggle.dispatch("click");
    classes(old.row, "visit-link")[0].dispatch("click");
    replies.release();
    await settle(10);
    assertRevoked(p);
    assert.equal(p.host.of("history_visit").length, sent);
    assert.equal(p.tabs.opened.length, 0);
    const calls = p.host.calls.length;
    await settle(10);
    assert.equal(p.host.calls.length, calls, "revocation itself does not retry");
    replies.stop();
    p.$("history-retry").dispatch("click");
    await settle(15);
    assert.equal(p.app.vaultToken, tokenOf("B"));
    assert.deepEqual(rows(p).map((row) => row.dataset.visitId), ["b"]);
  });
}

for (const code of ["vault_missing", "vault_invalid", "config_invalid", "app_dir_unavailable", "vault_switched", "key_unavailable", "vault_decrypt_failed", "vault_mode_mismatch", "vault_encryption_unsupported", "vault_legacy_unsupported"]) {
  test(`HISTORY-04: formal VaultError ${code} revokes current data`, async () => {
    const p = await page();
    ui(p).toggle.dispatch("click");
    await settle(5);
    p.host.errors.history_days = code;
    await focus(p);
    assertRevoked(p);
  });
}

for (const route of ["history_list", "detail", "full-url", "host_capabilities"]) {
  test(`HISTORY-04: current ${route} Vault error revokes data and focus/Retry recovers`, async () => {
    const p = await page();
    if (route === "detail" || route === "full-url") {
      p.host.errors.history_visit = "vault_locked";
      if (route === "detail") ui(p).toggle.dispatch("click"); else cut(p).dispatch("click");
    } else if (route === "history_list") {
      p.host.errors.history_list = "vault_locked";
      await focus(p);
    } else {
      p.host.errors.host_capabilities = "vault_locked";
      p.$("history-retry").dispatch("click");
    }
    await settle(10);
    assertRevoked(p);
    assert.match(p.$("history-state-text").textContent, /ロック/);
    if (route === "host_capabilities") p.$("history-retry").dispatch("click"); else await focus(p);
    await settle(10);
    assert.equal(p.app.vaultToken, tokenOf("A"));
    assert.equal(rows(p).length, 2);
    assert.equal(classes(p.$("history-list"), "detail-card").length, 0, "no old detail resurrection");
  });
}

for (const route of ["history_list", "history_days", "detail", "full-url", "host_capabilities"]) {
  test(`HISTORY-04: delayed obsolete ${route} Vault error cannot erase healthy B`, async () => {
    const p = await page();
    const type = route === "detail" || route === "full-url" ? "history_visit" : route;
    const replies = holdReplies(p, type, errorReply("vault_locked"));
    if (route === "history_list") { p.$("size-select").value = "25"; p.$("size-select").dispatch("change"); }
    if (route === "history_days") p.$("cal-prev").dispatch("click");
    if (route === "detail") ui(p).toggle.dispatch("click");
    if (route === "full-url") cut(p).dispatch("click");
    if (route === "host_capabilities") p.$("history-retry").dispatch("click");
    await settle(5);
    assert.equal(replies.held.length, 1);
    replies.stop();
    p.host.vault = "B";
    p.$("history-retry").dispatch("click");
    await settle(15);
    assert.equal(p.app.vaultToken, tokenOf("B"));
    replies.release();
    await settle(10);
    assert.equal(p.app.vaultToken, tokenOf("B"));
    assert.deepEqual(rows(p).map((row) => row.dataset.visitId), ["b"]);
  });
}

test("HISTORY-04: normal A to B focus switch closes expanded A details", async () => {
  const p = await page();
  ui(p).toggle.dispatch("click");
  await settle(5);
  p.host.vault = "B";
  await focus(p);
  assert.equal(p.app.vaultToken, tokenOf("B"));
  assert.deepEqual(rows(p).map((row) => row.dataset.visitId), ["b"]);
  assert.equal(classes(p.$("history-list"), "detail-card").length, 0);
});

test("HISTORY-04: consecutive failures do not retry, and later focus recovers", async () => {
  const p = await page();
  for (let i = 0; i < 2; i++) {
    p.host.errors.history_days = "vault_not_configured";
    await focus(p);
    assertRevoked(p);
    const calls = p.host.calls.length;
    await settle(10);
    assert.equal(p.host.calls.length, calls);
  }
  await focus(p);
  assert.equal(p.app.vaultToken, tokenOf("A"));
});

test("HISTORY-04: transport failure keeps identity but cancels pending detail and URL replies", async () => {
  const p = await page();
  const old = ui(p);
  const replies = holdReplies(p, "history_visit");
  old.toggle.dispatch("click");
  cut(p).dispatch("click");
  await settle(5);
  p.host.errors.history_days = "unavailable";
  await focus(p);
  assert.equal(p.app.vaultToken, tokenOf("A"), "a disconnect is not declared to be a Vault switch");
  assert.equal(rows(p).length, 2);
  assert.match(text(old.region), /Native Host/);
  replies.release();
  await settle(10);
  assert.equal(classes(old.region, "detail-card").length, 0);
  assert.equal(p.tabs.opened.length, 0);
  replies.stop();
  classes(old.region, "link-button")[0].dispatch("click");
  await settle(10);
  assert.match(text(old.region), /A page/);
});

test("HISTORY-04: completed details survive a transient marks failure", async () => {
  const p = await page();
  ui(p).toggle.dispatch("click");
  await settle(5);
  p.host.errors.history_days = "unavailable";
  await focus(p);
  assert.match(text(ui(p).region), /A page/);
  assert.equal(p.app.vaultToken, tokenOf("A"));
});

test("HISTORY-04: timeout is not a Vault switch and cancels a pending detail", async () => {
  const p = await page();
  const replies = holdReplies(p, "history_visit");
  ui(p).toggle.dispatch("click");
  await settle(5);
  p.host.errors.history_days = "timeout";
  await focus(p);
  replies.release();
  await settle(5);
  assert.equal(p.app.vaultToken, tokenOf("A"));
  assert.match(text(ui(p).region), /応答がありません/);
  assert.equal(classes(ui(p).region, "detail-card").length, 0);
});

test("HISTORY-04: cancelled full-URL reply cannot later adopt a foreign token", async () => {
  const p = await page();
  const replies = holdReplies(p, "history_visit", (reply) => ({ ...reply, vaultToken: tokenOf("B") }));
  ui(p).toggle.dispatch("click"); // keep focus from replacing the list
  await settle(5);
  cut(p).dispatch("click");
  await settle(5);
  p.host.errors.history_days = "unavailable";
  await focus(p);
  replies.release();
  await settle(5);
  assert.equal(p.app.vaultToken, tokenOf("A"));
  assert.equal(rows(p).length, 2);
  assert.equal(p.tabs.opened.length, 0);
});

test("HISTORY-05: invalid picker state/year/month is rejected without coercion", () => {
  for (const year of [null, NaN, undefined, "2026", 2026.5, 1969, 2028]) assert.equal(isPickerSelectionValid("months", year, 0, 2027), false, String(year));
  for (const month of [-1, 12, NaN, null, "0", 0.5]) assert.equal(isPickerSelectionValid("months", 2026, month, 2027), false, String(month));
  assert.equal(isPickerSelectionValid("days", 2026, 0, 2027), false);
  assert.equal(isPickerSelectionValid("months", 2026, 11, 2027), true);
});

test("HISTORY-05: accidental month handler in days view changes neither state nor requests", async () => {
  const p = await page();
  const shown = p.app.shownMonth, state = p.app.state, search = p.win.location.search, calls = p.host.calls.length;
  p.$("cal-months").children[0].dispatch("click"); // pickerYear is still null
  assert.deepEqual(p.app.shownMonth, shown);
  assert.deepEqual(p.app.state, state);
  assert.equal(p.win.location.search, search);
  assert.equal(p.host.calls.length, calls);
  assert.match(p.$("cal-title").textContent, /2026/);
});

test("HISTORY-05: rapid picker operations only fetch the chosen month, not another list", async () => {
  const p = await page();
  const lists = p.host.of("history_list").length, days = p.host.of("history_days").length, search = p.win.location.search;
  p.$("cal-title").dispatch("click");
  for (let i = 0; i < 6; i++) { p.$("cal-prev").dispatch("click"); p.$("cal-next").dispatch("click"); }
  assert.equal(p.host.of("history_days").length, days);
  p.$("cal-months").children[1].dispatch("click");
  await settle(10);
  assert.deepEqual(p.app.shownMonth, { year: 2026, monthIndex: 1 });
  assert.equal(p.host.of("history_days").length, days + 1);
  assert.equal(p.host.of("history_list").length, lists);
  assert.equal(p.win.location.search, search);
  assert.equal(p.app.state.date.day, 17);
  p.$("cal-months").children[0].dispatch("click"); // pickerYear is valid, but days view still forbids a click
  assert.equal(p.app.shownMonth.monthIndex, 1);
});

test("HISTORY-05/06: hidden CSS and natural pipeline DOM order, without positive tabindex", async () => {
  const css = readFileSync(path.join(extensionDir, "history.css"), "utf8");
  assert.match(css, /\.calendar-months\[hidden\]\s*\{\s*display:\s*none\s*;/);
  assert.doesNotMatch(css, /\.detail-card-(?:source|visit|targets|meta)[^{]*\{[^}]*\border\s*:/);
  const p = await page();
  ui(p).toggle.dispatch("click");
  await settle(5);
  const region = ui(p).region;
  assert.deepEqual(region.children.map((card) => card.className), ["detail-card detail-card-source", "detail-card detail-card-visit", "detail-card detail-card-targets", "detail-card detail-card-meta"]);
  const anchors = walk(region).filter((node) => node.tagName === "A");
  assert.deepEqual(anchors.slice(0, 2).map((node) => node.getAttribute("href")), [A[1].url, LONG]);
  assert.ok(walk(region).every((node) => !(Number(node.getAttribute("tabindex")) > 0)));
  assert.equal(ui(p).toggle.getAttribute("aria-controls"), region.getAttribute("id"));
  assert.equal(ui(p).toggle.getAttribute("aria-expanded"), "true");
  ui(p).toggle.dispatch("click");
  assert.equal(region.children.length, 0);
  assert.equal(ui(p).toggle.getAttribute("aria-expanded"), "false");
});
