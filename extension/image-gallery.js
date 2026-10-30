// Image tab: a grid of saved images (one tile per distinct original), newest first.
//
// - The list carries metadata only; thumbnails are requested for tiles near the viewport, in
//   batches, at most once while shown, and their object URLs are revoked when the tile is far away.
// - Every request belongs to a generation; a vault change or reload starts a new one and drops all
//   tiles, object URLs and late answers of the old one. A reply from another vault (switched in
//   another window) restarts the list instead of mixing vaults.
// - Images without a thumbnail stay in the grid with a type label instead of a picture. When such a
//   tile comes near the viewport, its thumbnail is rebuilt from the original once (one at a time)
//   and added to the vault; if that fails the label stays.
// - Right-click (or the context-menu key) on a tile offers copying and exporting the original.
// - Focus: the history panel takes focus only when opened from the keyboard, the menu whenever it
//   opens; both give it back to their tile when closed, and async results never move it.

import { SearchError, VAULT_UNAVAILABLE_CODES } from "./search-client.js";
import { THUMBNAIL_BATCH, describeSourceUrl, fetchThumbnails, listImages, listSources, shortMime } from "./image-client.js";
import { formatFullDate } from "./format.js";
import { copyImage, exportImage, rebuildThumbnail } from "./image-original.js";

const LOAD_MARGIN = "400px";
const KEEP_MARGIN = "2500px";
const BATCH_DELAY_MS = 30;
const MENU_MARGIN = 4;

const $ = (selector) => document.querySelector(selector);

/** Browser facilities for copying and thumbnails, looked up when used. */
const browserMedia = () => ({
  clipboard: globalThis.navigator?.clipboard,
  ClipboardItem: globalThis.ClipboardItem,
  createImageBitmap: globalThis.createImageBitmap && ((...args) => globalThis.createImageBitmap(...args)),
  OffscreenCanvas: globalThis.OffscreenCanvas,
});

export function createImageGallery({ send, isActive, openUrl, openSettings, onVaultUnavailable, media = browserMedia }) {
  const scroller = $("#panel-image");
  const grid = $("#gallery-grid");
  const summary = $("#gallery-summary");
  const refreshButton = $("#gallery-refresh");
  const newDot = $("#gallery-new-dot");
  const errorBox = $("#gallery-error");
  const errorMessage = $("#gallery-error-message");
  const retryButton = $("#gallery-retry");
  const settingsButton = $("#gallery-open-settings");
  const empty = $("#gallery-empty");
  const moreButton = $("#gallery-more");
  const details = $("#gallery-details");
  const detailsMeta = $("#gallery-details-meta");
  const sourcesList = $("#gallery-sources");
  const sourcesMore = $("#gallery-sources-more");
  const detailsClose = $("#gallery-details-close");
  const statusLine = $("#gallery-status");
  const menu = $("#gallery-menu");
  const menuItems = [$("#gallery-menu-copy"), $("#gallery-menu-export")];

  let generation = 0;
  let tiles = new Map(); // blobId -> { item, li, button, img, status, url }
  let next = null;
  let started = false;
  let stale = false;
  let loadingPage = false;
  let vaultToken = null;
  let queue = new Set();
  let batchTimer = null;
  let selected = null; // { blobId, next, generation }
  let hasNew = false;
  let refreshing = false;
  let rebuildQueue = [];
  let rebuildTried = new Set(); // per generation: each missing thumbnail is rebuilt at most once
  let rebuilding = false;
  let menuFor = null; // blobId the open menu acts on
  let busy = false; // a copy or export is running

  const loadObserver = new IntersectionObserver(onNearViewport, { root: scroller, rootMargin: LOAD_MARGIN });
  const keepObserver = new IntersectionObserver(onFarAway, { root: scroller, rootMargin: KEEP_MARGIN });
  new IntersectionObserver((entries) => {
    if (entries.some((entry) => entry.isIntersecting)) void loadPage();
  }, { root: scroller, rootMargin: LOAD_MARGIN }).observe(moreButton);

  /** The refresh button tells whether newer images exist, by its name and tooltip, not only by the dot. */
  function setNew(value) {
    hasNew = value;
    newDot.hidden = !value;
    const label = value ? "新しい画像があります" : "画像一覧を更新";
    refreshButton.setAttribute("aria-label", label);
    refreshButton.title = label;
  }

  function updateSummary() {
    summary.textContent = tiles.size === 0 ? "" : `${tiles.size}${next ? "件以上" : "件"}`;
  }

  function showError(error) {
    const vault = error instanceof SearchError && VAULT_UNAVAILABLE_CODES.has(error.code);
    errorMessage.textContent = error instanceof SearchError ? error.message : "画像を読み込めませんでした。";
    settingsButton.hidden = !vault;
    errorBox.hidden = false;
    // Like the URL tab: point at the settings tab without pulling the user away.
    if (vault) onVaultUnavailable();
  }

  /** Drops everything of the current generation: tiles, object URLs, pending work, details. */
  function reset() {
    // Focus inside what is removed would fall to the document; keep it in the Image tab instead.
    const focusLost = [grid, details, menu].some((element) => element.contains(document.activeElement));
    generation += 1;
    for (const tile of tiles.values()) {
      loadObserver.unobserve?.(tile.li);
      keepObserver.unobserve?.(tile.li);
      if (tile.url) URL.revokeObjectURL(tile.url);
    }
    tiles = new Map();
    queue = new Set();
    rebuildQueue = [];
    rebuildTried = new Set();
    menu.hidden = true;
    menuFor = null;
    clearTimeout(batchTimer);
    grid.replaceChildren();
    next = null;
    vaultToken = null;
    loadingPage = false;
    started = false;
    selected = null;
    details.hidden = true;
    errorBox.hidden = true;
    empty.hidden = true;
    moreButton.hidden = true;
    summary.textContent = "";
    if (focusLost) scroller.focus();
  }

  function showStatus(text, isError = false) {
    statusLine.textContent = text;
    statusLine.className = isError ? "gallery-status is-error" : "gallery-status";
  }

  /** Gives focus back to a tile, or to the Image tab itself when that tile is gone. */
  function focusTile(blobId) {
    const tile = tiles.get(blobId);
    if (tile) tile.button.focus();
    else scroller.focus();
  }

  function tileLabel(item) {
    const saved = formatFullDate(new Date(item.lastSavedAt).toISOString());
    return `${shortMime(item.mimeType)} ${item.width}×${item.height}、最終保存 ${saved}、保存${item.saveCount}回`;
  }

  function renderTile(item) {
    const li = document.createElement("li");
    li.className = "gallery-item";
    li.dataset.blobId = String(item.blobId);
    const button = document.createElement("button");
    button.type = "button";
    button.className = "tile";
    button.dataset.blobId = String(item.blobId);
    button.setAttribute("aria-label", tileLabel(item));
    button.title = tileLabel(item);
    const placeholder = document.createElement("span");
    placeholder.className = "tile-placeholder";
    placeholder.textContent = shortMime(item.mimeType);
    button.append(placeholder);
    if (item.saveCount > 1) {
      const badge = document.createElement("span");
      badge.className = "tile-count";
      badge.textContent = `×${item.saveCount}`;
      button.append(badge);
    }
    li.append(button);
    return { item, li, button, placeholder, img: null, status: item.hasThumbnail ? "idle" : "missing", url: null };
  }

  /** Loads the next page; resolves true when it was applied to the current list. */
  async function loadPage() {
    if (loadingPage || (started && !next)) return false;
    const token = generation;
    const firstPage = !started;
    loadingPage = true;
    started = true;
    errorBox.hidden = true;
    try {
      const page = await listImages(next, { send });
      if (token !== generation) return false;
      if (vaultToken && page.vaultToken !== vaultToken) {
        // The vault changed between pages (e.g. from another window): start over in the new one.
        return reload();
      }
      vaultToken = page.vaultToken;
      const fragment = document.createDocumentFragment();
      for (const item of page.items) {
        if (tiles.has(item.blobId)) continue;
        const tile = renderTile(item);
        tiles.set(item.blobId, tile);
        fragment.append(tile.li);
        loadObserver.observe(tile.li);
        keepObserver.observe(tile.li);
      }
      grid.append(fragment);
      next = page.next;
      empty.hidden = tiles.size !== 0;
      // A fresh first page is the newest state; anything announced before is now shown.
      if (firstPage) setNew(false);
      return true;
    } catch (error) {
      if (token !== generation) return false;
      started = tiles.size > 0;
      showError(error);
      return false;
    } finally {
      if (token === generation) {
        loadingPage = false;
        moreButton.hidden = !next;
        updateSummary();
      }
    }
  }

  function tileOf(entry) {
    return tiles.get(Number(entry.target.dataset.blobId));
  }

  function onNearViewport(entries) {
    for (const entry of entries) {
      const tile = tileOf(entry);
      if (entry.isIntersecting && tile?.status === "idle") queue.add(tile.item.blobId);
      if (entry.isIntersecting && tile?.status === "missing") requestRebuild(tile);
    }
    if (queue.size > 0) {
      clearTimeout(batchTimer);
      batchTimer = setTimeout(() => void flushThumbnails(), BATCH_DELAY_MS);
    }
  }

  /** Frees the decoded picture of tiles that scrolled far away; they reload when they come back. */
  function onFarAway(entries) {
    for (const entry of entries) {
      const tile = tileOf(entry);
      if (!entry.isIntersecting && tile?.status === "missing" && rebuildQueue.includes(tile.item.blobId)) {
        // Not needed any more; it is rebuilt if it comes back into view.
        rebuildQueue = rebuildQueue.filter((id) => id !== tile.item.blobId);
        rebuildTried.delete(tile.item.blobId);
      }
      if (entry.isIntersecting || tile?.status !== "loaded") continue;
      URL.revokeObjectURL(tile.url);
      tile.url = null;
      tile.img.removeAttribute("src");
      tile.status = "idle";
      queue.delete(tile.item.blobId);
    }
  }

  async function flushThumbnails() {
    const ids = [...queue].slice(0, THUMBNAIL_BATCH);
    if (ids.length === 0) return;
    for (const id of ids) {
      queue.delete(id);
      tiles.get(id).status = "pending";
    }
    const token = generation;
    try {
      const result = await fetchThumbnails(ids, { send });
      if (token !== generation) return; // answer for a vault or list that is gone
      if (result.vaultToken !== vaultToken) {
        reload();
        return;
      }
      for (const id of ids) {
        const tile = tiles.get(id);
        if (!tile || tile.status !== "pending") continue;
        if (result.deferred.includes(id) || !result.thumbnails.has(id)) {
          tile.status = "idle";
          queue.add(id);
          continue;
        }
        const thumbnail = result.thumbnails.get(id);
        if (!thumbnail) {
          tile.status = "missing";
          requestRebuild(tile);
          continue;
        }
        showThumbnail(tile, thumbnail);
      }
    } catch {
      if (token !== generation) return;
      for (const id of ids) {
        const tile = tiles.get(id);
        if (tile?.status === "pending") tile.status = "idle"; // retried when it comes into view again
      }
      return;
    }
    if (queue.size > 0) batchTimer = setTimeout(() => void flushThumbnails(), BATCH_DELAY_MS);
  }

  function showThumbnail(tile, thumbnail) {
    tile.url = URL.createObjectURL(thumbnail.blob);
    if (!tile.img) {
      tile.img = document.createElement("img");
      tile.img.alt = "";
      tile.img.decoding = "async";
      tile.button.append(tile.img);
    }
    tile.img.src = tile.url;
    tile.button.className = "tile has-image";
    tile.status = "loaded";
  }

  // --- Rebuilding missing thumbnails --------------------------------------------------------

  function requestRebuild(tile) {
    const id = tile.item.blobId;
    if (rebuildTried.has(id)) return;
    rebuildTried.add(id);
    rebuildQueue.push(id);
    void drainRebuilds();
  }

  /** One rebuild at a time; each reads a whole original. Results of an old generation are dropped. */
  async function drainRebuilds() {
    if (rebuilding) return;
    rebuilding = true;
    try {
      while (rebuildQueue.length > 0) {
        const id = rebuildQueue.shift();
        const token = generation;
        const tile = tiles.get(id);
        if (!tile || tile.status !== "missing" || !vaultToken) continue;
        tile.status = "rebuilding";
        let thumbnail = null;
        try {
          thumbnail = await rebuildThumbnail(tile.item, vaultToken, { send, isCurrent: () => token === generation, ...media() });
        } catch {
          thumbnail = null;
        }
        if (token !== generation || tiles.get(id) !== tile) continue;
        if (thumbnail) showThumbnail(tile, thumbnail);
        else tile.status = "missing"; // the type label stays; not retried in this list
      }
    } finally {
      rebuilding = false;
    }
  }

  // --- Copy and export ----------------------------------------------------------------------

  /** Opens the menu at the pointer, or below the tile when opened from the keyboard. */
  function openMenu(blobId, point) {
    const tile = tiles.get(blobId);
    if (!tile) return;
    menuFor = blobId;
    for (const item of menuItems) item.setAttribute("aria-disabled", String(busy));
    menu.hidden = false;
    let anchor = point;
    if (!anchor) {
      const rect = tile.button.getBoundingClientRect?.() ?? { left: 0, bottom: 0 };
      anchor = { x: rect.left, y: rect.bottom };
    }
    const width = menu.offsetWidth ?? 0;
    const height = menu.offsetHeight ?? 0;
    const viewWidth = globalThis.innerWidth ?? width;
    const viewHeight = globalThis.innerHeight ?? height;
    // Kept inside the panel: shifted left at the right edge, opened upwards at the bottom.
    const left = Math.max(MENU_MARGIN, Math.min(anchor.x, viewWidth - width - MENU_MARGIN));
    const top = anchor.y + height + MENU_MARGIN > viewHeight ? Math.max(MENU_MARGIN, anchor.y - height) : anchor.y;
    menu.style.left = `${left}px`;
    menu.style.top = `${top}px`;
    menuItems[0].focus();
  }

  function closeMenu({ restoreFocus }) {
    if (menu.hidden) return;
    const blobId = menuFor;
    menu.hidden = true;
    menuFor = null;
    if (restoreFocus) focusTile(blobId);
  }

  /** Runs synchronously inside the menu click, so the clipboard write starts within it. */
  function runAction(kind, blobId) {
    const tile = tiles.get(blobId);
    if (!tile || busy || !vaultToken) return;
    const token = generation;
    busy = true;
    let task;
    if (kind === "copy") {
      showStatus("画像をコピーしています…");
      task = copyImage(tile.item, vaultToken, { send, isCurrent: () => token === generation, ...media() }).then(() => "画像をクリップボードにコピーしました。");
    } else {
      showStatus("書き出し先を選んでください…");
      task = exportImage(tile.item, vaultToken, { send }).then((result) =>
        result.cancelled ? "書き出しをキャンセルしました。" : `「${result.fileName}」として書き出しました。`,
      );
    }
    void task
      .then(
        (text) => showStatus(text),
        (error) => showStatus(error instanceof SearchError ? error.message : "操作に失敗しました。", true),
      )
      .finally(() => {
        busy = false;
      });
  }

  grid.addEventListener("contextmenu", (event) => {
    const button = event.target.closest?.("button.tile");
    if (!button || !grid.contains(button)) return;
    event.preventDefault();
    // A mouse right-click carries button 2; the context-menu key and Shift+F10 do not.
    openMenu(Number(button.dataset.blobId), event.button === 2 ? { x: event.clientX, y: event.clientY } : null);
  });
  for (const [index, item] of menuItems.entries()) {
    item.addEventListener("click", () => {
      if (item.getAttribute("aria-disabled") === "true") return;
      const blobId = menuFor;
      closeMenu({ restoreFocus: true });
      runAction(index === 0 ? "copy" : "export", blobId);
    });
    item.addEventListener("keydown", (event) => {
      const last = menuItems.length - 1;
      const moves = { ArrowDown: index === last ? 0 : index + 1, ArrowUp: index === 0 ? last : index - 1, Home: 0, End: last };
      if (event.key in moves) {
        event.preventDefault();
        menuItems[moves[event.key]].focus();
      } else if (event.key === "Escape" || event.key === "Tab") {
        event.preventDefault();
        closeMenu({ restoreFocus: true });
      }
    });
  }
  // A click elsewhere, scrolling or leaving the panel closes the menu without moving focus.
  document.addEventListener("pointerdown", (event) => {
    if (menu.hidden || menu.contains(event.target) || menuItems.some((item) => item.contains(event.target))) return;
    closeMenu({ restoreFocus: false });
  }, true);
  scroller.addEventListener("scroll", () => closeMenu({ restoreFocus: false }));
  globalThis.addEventListener?.("blur", () => closeMenu({ restoreFocus: false }));
  globalThis.addEventListener?.("resize", () => closeMenu({ restoreFocus: false }));

  // --- Details (save history) ----------------------------------------------------------------

  function sourceLine(label, url) {
    const line = document.createElement("span");
    line.className = "source-line";
    const name = document.createElement("span");
    name.className = "source-label";
    name.textContent = `${label}: `;
    const { text, href } = describeSourceUrl(url);
    let value;
    if (href) {
      value = document.createElement("a");
      value.href = href;
      value.className = "source-link";
    } else {
      value = document.createElement("span");
    }
    value.textContent = text;
    line.append(name, value);
    return line;
  }

  async function loadSources(state) {
    const token = generation;
    sourcesMore.disabled = true;
    try {
      const page = await listSources(state.blobId, state.next, { send });
      if (token !== generation || selected !== state) return;
      if (page.vaultToken !== vaultToken) {
        reload();
        return;
      }
      const fragment = document.createDocumentFragment();
      for (const source of page.items) {
        const li = document.createElement("li");
        const when = document.createElement("time");
        when.textContent = formatFullDate(new Date(source.savedAt).toISOString());
        li.append(when, sourceLine("画像", source.imageUrl), sourceLine("ページ", source.pageUrl));
        if (source.frameUrl) li.append(sourceLine("フレーム", source.frameUrl));
        fragment.append(li);
      }
      sourcesList.append(fragment);
      state.next = page.next;
      sourcesMore.hidden = !page.next;
    } catch (error) {
      if (token !== generation || selected !== state) return;
      const li = document.createElement("li");
      li.textContent = error instanceof SearchError ? error.message : "保存履歴を読み込めませんでした。";
      sourcesList.append(li);
    } finally {
      sourcesMore.disabled = false;
    }
  }

  function select(blobId, { keyboard = false } = {}) {
    const tile = tiles.get(blobId);
    if (!tile) return;
    const { item } = tile;
    selected = { blobId, next: null };
    detailsMeta.textContent = `${shortMime(item.mimeType)}（${item.mimeType}）・${item.width}×${item.height}・${Math.max(1, Math.round(item.byteLength / 1024))} KB・保存${item.saveCount}回・最終保存 ${formatFullDate(new Date(item.lastSavedAt).toISOString())}`;
    sourcesList.replaceChildren();
    sourcesMore.hidden = true;
    details.hidden = false;
    // Only now, never when the history arrives later: by then the user may be elsewhere.
    if (keyboard) details.focus();
    void loadSources(selected);
  }

  function closeDetails() {
    const hadFocus = details.contains(document.activeElement);
    const blobId = selected?.blobId;
    details.hidden = true;
    selected = null;
    if (hadFocus && blobId !== undefined) focusTile(blobId);
  }

  grid.addEventListener("click", (event) => {
    const button = event.target.closest?.("button.tile");
    // A click from Enter or Space has detail 0; a mouse click counts its clicks.
    if (button && grid.contains(button)) select(Number(button.dataset.blobId), { keyboard: event.detail === 0 });
  });
  sourcesList.addEventListener("click", (event) => {
    const link = event.target.closest?.("a.source-link");
    if (!link) return;
    event.preventDefault();
    openUrl(link.href);
  });
  sourcesMore.addEventListener("click", () => selected && void loadSources(selected));
  detailsClose.addEventListener("click", () => closeDetails());
  details.addEventListener("keydown", (event) => {
    if (event.key !== "Escape") return;
    event.preventDefault();
    closeDetails();
  });
  moreButton.addEventListener("click", () => void loadPage());
  retryButton.addEventListener("click", () => void loadPage());
  settingsButton.addEventListener("click", () => openSettings());
  refreshButton.addEventListener("click", () => {
    if (refreshing) return;
    scroller.scrollTop = 0;
    void refresh();
  });

  function reload() {
    reset();
    stale = false;
    return loadPage();
  }

  /** Reloads from the top; ignores further clicks until done. A failure keeps the "new" state. */
  async function refresh() {
    refreshing = true;
    refreshButton.disabled = true;
    refreshButton.setAttribute("aria-busy", "true");
    try {
      await reload();
    } finally {
      refreshing = false;
      refreshButton.disabled = false;
      refreshButton.removeAttribute("aria-busy");
    }
  }

  return {
    /** Called when the Image tab is shown: loads lazily the first time or after a change. */
    activate() {
      if (!started || stale) reload();
    },
    /** The active vault changed: never show the old vault's images, even for a moment. */
    vaultChanged() {
      setNew(false); // "new" referred to the previous vault
      reset();
      stale = true;
      if (isActive()) reload();
    },
    /** A new image was saved: refresh only if that does not disturb the user. */
    imageSaved() {
      if (!started) return;
      setNew(true);
      if (!isActive()) {
        stale = true;
        return;
      }
      if (scroller.scrollTop === 0 && details.hidden && !refreshing) void refresh();
    },
    get size() {
      return tiles.size;
    },
  };
}
