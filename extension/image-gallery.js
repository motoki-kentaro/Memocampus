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
// - Tags (Issue #44): the details of a selected image show its tags and let the user add one (typed
//   or picked from the tags in use) or remove one. A tag filter replaces `image_list` by
//   `image_tag_search` as the source of the same grid; everything else (thumbnails, paging, copy,
//   export, details) is shared. Tag writes carry the vault token of the list they were made from,
//   so the host refuses them in any other vault. The filter, the picker and the details belong to
//   the vault: a vault change drops all of them. Tag names are shown as the host normalized them.

import { SearchError, VAULT_UNAVAILABLE_CODES } from "./search-client.js";
import {
  THUMBNAIL_BATCH,
  changeImageTag,
  describeSourceUrl,
  fetchThumbnails,
  getImageTags,
  listImages,
  listSources,
  listTags,
  searchImagesByTag,
  setMcpPublic,
  shortMime,
} from "./image-client.js";
import { formatFullDate } from "./format.js";
import { copyImage, exportImage, rebuildThumbnail } from "./image-original.js";

const LOAD_MARGIN = "400px";
const KEEP_MARGIN = "2500px";
const BATCH_DELAY_MS = 30;
const MENU_MARGIN = 4;
const EMPTY_TEXT = "保存された画像はまだありません。ページ上の画像を右クリックして「Memocampusへ保存」を選ぶと、ここに表示されます。";
/** Messages for tag requests where the general one would point the wrong way. */
const TAG_MESSAGES = {
  blob_missing: "この画像がVaultに見つかりません。一覧を更新してください。",
};

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
  const filterButton = $("#gallery-tag-filter");
  const filterChip = $("#gallery-filter");
  const filterName = $("#gallery-filter-name");
  const filterClear = $("#gallery-filter-clear");
  const tagsList = $("#gallery-tags");
  const tagsEmpty = $("#gallery-tags-empty");
  const tagInput = $("#gallery-tag-input");
  const tagAdd = $("#gallery-tag-add");
  const tagChoose = $("#gallery-tag-choose");
  const tagsMessage = $("#gallery-tags-message");
  const picker = $("#gallery-tag-picker");
  const pickerTitle = $("#gallery-tag-picker-title");
  const pickerForm = $("#gallery-tag-picker-form");
  const pickerInput = $("#gallery-tag-picker-input");
  const pickerApply = $("#gallery-tag-picker-apply");
  const pickerMessage = $("#gallery-tag-picker-message");
  const pickerList = $("#gallery-tag-picker-list");
  const pickerMore = $("#gallery-tag-picker-more");
  const pickerClose = $("#gallery-tag-picker-close");

  let generation = 0;
  let tiles = new Map(); // blobId -> { item, li, button, img, status, url }
  let next = null;
  let started = false;
  let stale = false;
  let loadingPage = false;
  let vaultToken = null;
  let queue = new Set();
  let batchTimer = null;
  let selected = null; // { blobId, item, next, tags: string[] | null, busy }
  let filter = null; // { tag }: the grid shows the images with this tag (normalized once the host answered)
  let pickerState = null; // { mode: "filter" | "add", next, loading, opener }
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
    const focusLost = [grid, details, menu, picker].some((element) => element.contains(document.activeElement));
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
    tagsList.replaceChildren();
    setTagsMessage("");
    closePicker({ restoreFocus: false });
    errorBox.hidden = true;
    empty.hidden = true;
    moreButton.hidden = true;
    summary.textContent = "";
    updateTagControls();
    if (focusLost) scroller.focus();
  }

  /** The vault changed under the list (another window): its filter means nothing in the new one. */
  function vaultMismatch() {
    setFilter(null);
    return reload();
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
      const page = filter ? await searchImagesByTag(filter.tag, next, { send }) : await listImages(next, { send });
      if (token !== generation) return false;
      if (vaultToken && page.vaultToken !== vaultToken) {
        // The vault changed between pages (e.g. from another window): start over in the new one.
        return vaultMismatch();
      }
      vaultToken = page.vaultToken;
      if (filter && page.tag !== filter.tag) {
        filter = { tag: page.tag }; // the host's normalized name, used for the next pages too
        renderFilter();
      }
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
      empty.textContent = emptyText();
      empty.hidden = tiles.size !== 0;
      // A fresh first page is the newest state; anything announced before is now shown.
      if (firstPage) setNew(false);
      return true;
    } catch (error) {
      if (token !== generation) return false;
      if (filter && error instanceof SearchError && error.code === "invalid_tag") {
        // A typed filter the host does not accept as a tag: back to all images, and say why.
        loadingPage = false;
        setFilter(null);
        const loaded = reload();
        showStatus(error.message, true);
        return loaded;
      }
      started = tiles.size > 0;
      showError(error);
      return false;
    } finally {
      if (token === generation) {
        loadingPage = false;
        moreButton.hidden = !next;
        updateSummary();
        updateTagControls();
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
        vaultMismatch();
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

  function publicationToggle(source) {
    const label = document.createElement("label");
    label.className = "source-publication";
    const box = document.createElement("input");
    box.type = "checkbox";
    box.className = "source-public";
    // An incognito row is never public, whatever value is stored, so it shows and offers no choice.
    box.checked = source.mcpPublic && !source.incognito;
    box.disabled = source.incognito;
    box.dataset.savedImageId = String(source.savedImageId);
    const text = document.createElement("span");
    text.textContent = source.incognito ? "シークレット保存のためMCP非公開" : "MCPへの公開を許可（プライバシー設定が常に優先。MCPは未実装）";
    const failure = document.createElement("span");
    failure.className = "source-publication-error";
    label.append(box, text, failure);
    return label;
  }

  async function changePublication(box) {
    if (box.disabled) return;
    const wanted = box.checked;
    const failure = box.closest("label.source-publication").querySelector(".source-publication-error");
    failure.textContent = "";
    box.disabled = true;
    try {
      await setMcpPublic(Number(box.dataset.savedImageId), wanted, vaultToken, { send });
    } catch (error) {
      // What is stored is unchanged or unknown; never show a choice that was not confirmed.
      box.checked = !wanted;
      failure.textContent = error instanceof SearchError ? error.message : "公開設定を変更できませんでした。";
    } finally {
      box.disabled = false;
    }
  }

  async function loadSources(state) {
    const token = generation;
    sourcesMore.disabled = true;
    try {
      const page = await listSources(state.blobId, state.next, { send });
      if (token !== generation || selected !== state) return;
      if (page.vaultToken !== vaultToken) {
        vaultMismatch();
        return;
      }
      const fragment = document.createDocumentFragment();
      for (const source of page.items) {
        const li = document.createElement("li");
        const when = document.createElement("time");
        when.textContent = formatFullDate(new Date(source.savedAt).toISOString());
        li.append(when);
        if (source.incognito) {
          const origin = document.createElement("span");
          origin.className = "source-origin";
          origin.textContent = "シークレット";
          li.append(origin);
        }
        li.append(sourceLine("画像", source.imageUrl), sourceLine("ページ", source.pageUrl));
        if (source.frameUrl) li.append(sourceLine("フレーム", source.frameUrl));
        li.append(publicationToggle(source));
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
    selected = { blobId, item, next: null, tags: null, busy: false };
    detailsMeta.textContent = `${shortMime(item.mimeType)}（${item.mimeType}）・${item.width}×${item.height}・${Math.max(1, Math.round(item.byteLength / 1024))} KB・保存${item.saveCount}回・最終保存 ${formatFullDate(new Date(item.lastSavedAt).toISOString())}`;
    sourcesList.replaceChildren();
    sourcesMore.hidden = true;
    tagsList.replaceChildren();
    tagsEmpty.hidden = true;
    tagInput.value = "";
    setTagsMessage("タグを読み込んでいます…");
    if (pickerState?.mode === "add") closePicker({ restoreFocus: false });
    details.hidden = false;
    updateTagControls();
    // Only now, never when the history arrives later: by then the user may be elsewhere.
    if (keyboard) details.focus();
    void loadTags(selected);
    void loadSources(selected);
  }

  function closeDetails() {
    const hadFocus = details.contains(document.activeElement);
    const blobId = selected?.blobId;
    details.hidden = true;
    selected = null;
    if (pickerState?.mode === "add") closePicker({ restoreFocus: false });
    updateTagControls();
    if (hadFocus && blobId !== undefined) focusTile(blobId);
  }

  // --- Tags of the selected image ------------------------------------------------------------

  function setTagsMessage(text, isError = false) {
    tagsMessage.textContent = text;
    tagsMessage.className = isError ? "gallery-tags-message is-error" : "gallery-tags-message";
  }

  function tagErrorText(error, fallback) {
    if (!(error instanceof SearchError)) return fallback;
    return TAG_MESSAGES[error.code] ?? error.message;
  }

  /** Tag controls work only for a loaded image of a known vault, one request at a time. */
  function updateTagControls() {
    filterButton.disabled = !vaultToken;
    const usable = Boolean(selected && vaultToken && selected.tags && !selected.busy);
    tagInput.disabled = !usable;
    tagAdd.disabled = !usable;
    tagChoose.disabled = !usable;
    for (const li of tagsList.children) {
      const remove = li.querySelector("button.tag-chip-remove");
      if (remove) remove.disabled = !usable;
    }
  }

  function renderTags(state) {
    const fragment = document.createDocumentFragment();
    for (const name of state.tags) {
      const li = document.createElement("li");
      li.className = "tag-chip";
      const text = document.createElement("span");
      text.className = "tag-chip-name";
      text.textContent = name;
      const remove = document.createElement("button");
      remove.type = "button";
      remove.className = "tag-chip-remove";
      remove.dataset.tag = name;
      remove.textContent = "×";
      remove.setAttribute("aria-label", `タグ「${name}」を外す`);
      remove.title = `タグ「${name}」を外す`;
      li.append(text, remove);
      fragment.append(li);
    }
    tagsList.replaceChildren(fragment);
    tagsEmpty.hidden = state.tags.length !== 0;
    updateTagControls();
  }

  async function loadTags(state) {
    const token = generation;
    try {
      const result = await getImageTags(state.blobId, { send });
      if (token !== generation || selected !== state) return;
      if (result.vaultToken !== vaultToken) {
        vaultMismatch();
        return;
      }
      state.tags = result.tags;
      setTagsMessage("");
      renderTags(state);
    } catch (error) {
      if (token !== generation || selected !== state) return;
      setTagsMessage(tagErrorText(error, "タグを読み込めませんでした。"), true);
      if (error instanceof SearchError && VAULT_UNAVAILABLE_CODES.has(error.code)) onVaultUnavailable();
    }
  }

  /**
   * Adds or removes one tag of the selected image. Nothing is shown as done before the host
   * confirmed it; on failure the shown tags stay as they were. One request at a time per image.
   */
  async function changeTag(kind, raw) {
    const state = selected;
    if (!state || state.busy || !state.tags || !vaultToken) return;
    if (typeof raw !== "string" || raw.trim() === "") {
      setTagsMessage("タグを入力してください。", true);
      return;
    }
    const token = generation;
    state.busy = true;
    updateTagControls();
    setTagsMessage(kind === "add" ? "タグを追加しています…" : "タグを外しています…");
    try {
      const result = await changeImageTag(kind, state.item, raw, vaultToken, { send });
      if (token !== generation || selected !== state) return;
      state.tags = result.tags;
      renderTags(state);
      if (kind === "add") {
        tagInput.value = "";
        setTagsMessage(result.changed ? `タグ「${result.tag}」を付けました。` : `タグ「${result.tag}」は既に付いています。`);
      } else {
        setTagsMessage(result.changed ? `タグ「${result.tag}」を外しました。` : `タグ「${result.tag}」は付いていません。`);
      }
      followFilter(state);
    } catch (error) {
      if (token !== generation || selected !== state) return;
      if (error instanceof SearchError && error.code === "vault_switched") {
        // The list belongs to a vault that is no longer active; the host wrote nothing anywhere.
        vaultMismatch();
        showStatus(error.message, true);
        return;
      }
      setTagsMessage(tagErrorText(error, "タグを変更できませんでした。"), true);
      if (error instanceof SearchError && VAULT_UNAVAILABLE_CODES.has(error.code)) onVaultUnavailable();
    } finally {
      if (selected === state) {
        state.busy = false;
        updateTagControls();
      }
    }
  }

  /** Under a tag filter, the grid follows the confirmed tags of the edited image. */
  function followFilter(state) {
    if (!filter) return;
    const has = state.tags.includes(filter.tag);
    if (!has && tiles.has(state.blobId)) removeTile(state.blobId);
    else if (has && !tiles.has(state.blobId)) insertTile(state.item);
  }

  function emptyText() {
    return filter ? `タグ「${filter.tag}」の付いた画像はありません。` : EMPTY_TEXT;
  }

  function removeTile(blobId) {
    const tile = tiles.get(blobId);
    if (!tile) return;
    loadObserver.unobserve?.(tile.li);
    keepObserver.unobserve?.(tile.li);
    if (tile.url) URL.revokeObjectURL(tile.url);
    tiles.delete(blobId);
    queue.delete(blobId);
    rebuildQueue = rebuildQueue.filter((id) => id !== blobId);
    if (menuFor === blobId) closeMenu({ restoreFocus: false });
    tile.li.remove();
    empty.hidden = tiles.size !== 0 || Boolean(next);
    updateSummary();
  }

  /**
   * Puts an image back at its place in the list order. It was shown before, so it is never older
   * than the next page's cursor: if no loaded tile is older, it belongs at the end (the next page
   * starts after the cursor and would never bring it).
   */
  function insertTile(item) {
    const newer = (a, b) => a.lastSavedAt > b.lastSavedAt || (a.lastSavedAt === b.lastSavedAt && a.blobId > b.blobId);
    // In grid order: tiles put back earlier are no longer in insertion order.
    const before = [...grid.children].map((li) => tiles.get(Number(li.dataset.blobId))).find((tile) => tile && newer(item, tile.item));
    const tile = renderTile(item);
    tiles.set(item.blobId, tile);
    if (before) grid.insertBefore(tile.li, before.li);
    else grid.append(tile.li);
    loadObserver.observe(tile.li);
    keepObserver.observe(tile.li);
    empty.hidden = true;
    updateSummary();
  }

  // --- Tag filter and the picker of tags in use ------------------------------------------------

  function renderFilter() {
    filterChip.hidden = !filter;
    filterName.textContent = filter ? `タグ: ${filter.tag}` : "";
  }

  /** Sets the filter (null: all images); the caller reloads the list. */
  function setFilter(tag) {
    filter = tag === null ? null : { tag };
    renderFilter();
  }

  /** Shows the images with `tag` (null: all images again), from the top of a new list. */
  function applyFilter(tag) {
    closePicker({ restoreFocus: false });
    setFilter(tag);
    scroller.scrollTop = 0;
    void reload();
  }

  function openPicker(mode, opener) {
    if (!vaultToken || (mode === "add" && (!selected?.tags || selected.busy))) return;
    pickerState = { mode, next: null, loading: false, opener };
    pickerTitle.textContent = mode === "filter" ? "タグで絞り込み" : "既存のタグから選ぶ";
    pickerForm.hidden = mode !== "filter";
    pickerInput.value = "";
    pickerList.replaceChildren();
    pickerMore.hidden = true;
    pickerMessage.textContent = "タグを読み込んでいます…";
    picker.hidden = false;
    if (mode === "filter") pickerInput.focus();
    else picker.focus();
    void loadTagPage(pickerState);
  }

  function closePicker({ restoreFocus }) {
    picker.hidden = true;
    if (!pickerState) return;
    const opener = pickerState.opener;
    pickerState = null;
    if (restoreFocus && opener && !opener.disabled) opener.focus();
  }

  /** One page of the tags in use (at most 200); more only when the user asks for them. */
  async function loadTagPage(state) {
    if (state.loading) return;
    state.loading = true;
    pickerMore.disabled = true;
    const token = generation;
    try {
      const page = await listTags(state.next, { send });
      if (token !== generation || pickerState !== state) return;
      if (page.vaultToken !== vaultToken) {
        vaultMismatch();
        return;
      }
      const fragment = document.createDocumentFragment();
      for (const tag of page.items) {
        const li = document.createElement("li");
        const option = document.createElement("button");
        option.type = "button";
        option.className = "tag-option";
        option.dataset.tag = tag.name;
        const name = document.createElement("span");
        name.textContent = tag.name;
        const count = document.createElement("span");
        count.className = "tag-option-count";
        count.textContent = `${tag.imageCount}`;
        option.append(name, count);
        const owned = state.mode === "add" && Boolean(selected?.tags?.includes(tag.name));
        option.setAttribute("aria-disabled", String(owned));
        option.setAttribute("aria-label", owned ? `${tag.name}（付与済み）` : `${tag.name}、${tag.imageCount}件`);
        li.append(option);
        fragment.append(li);
      }
      pickerList.append(fragment);
      state.next = page.next;
      pickerMore.hidden = !page.next;
      pickerMessage.textContent = pickerList.children.length === 0 ? "登録済みのタグはまだありません。" : "";
    } catch (error) {
      if (token !== generation || pickerState !== state) return;
      pickerMessage.textContent = tagErrorText(error, "タグの一覧を読み込めませんでした。");
      if (error instanceof SearchError && VAULT_UNAVAILABLE_CODES.has(error.code)) onVaultUnavailable();
    } finally {
      if (pickerState === state) {
        state.loading = false;
        pickerMore.disabled = false;
      }
    }
  }

  function choose(name) {
    const state = pickerState;
    if (!state) return;
    if (state.mode === "filter") {
      applyFilter(name);
      return;
    }
    closePicker({ restoreFocus: true });
    void changeTag("add", name);
  }

  function applyTyped() {
    if (pickerInput.value.trim() === "") {
      pickerMessage.textContent = "タグを入力してください。";
      return;
    }
    applyFilter(pickerInput.value);
  }

  filterButton.addEventListener("click", () => openPicker("filter", filterButton));
  filterClear.addEventListener("click", () => {
    if (!filter) return;
    applyFilter(null);
    filterButton.focus();
  });
  tagChoose.addEventListener("click", () => openPicker("add", tagChoose));
  tagAdd.addEventListener("click", () => void changeTag("add", tagInput.value));
  tagInput.addEventListener("keydown", (event) => {
    if (event.key !== "Enter" || event.isComposing) return;
    event.preventDefault();
    void changeTag("add", tagInput.value);
  });
  tagsList.addEventListener("click", (event) => {
    const remove = event.target.closest?.("button.tag-chip-remove");
    if (remove && !remove.disabled) void changeTag("remove", remove.dataset.tag);
  });
  pickerList.addEventListener("click", (event) => {
    const option = event.target.closest?.("button.tag-option");
    if (option && option.getAttribute("aria-disabled") !== "true") choose(option.dataset.tag);
  });
  pickerApply.addEventListener("click", applyTyped);
  pickerInput.addEventListener("keydown", (event) => {
    if (event.key !== "Enter" || event.isComposing) return;
    event.preventDefault();
    applyTyped();
  });
  pickerMore.addEventListener("click", () => pickerState && void loadTagPage(pickerState));
  pickerClose.addEventListener("click", () => closePicker({ restoreFocus: true }));
  picker.addEventListener("keydown", (event) => {
    if (event.key !== "Escape") return;
    event.preventDefault();
    closePicker({ restoreFocus: true });
  });

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
  sourcesList.addEventListener("change", (event) => {
    const box = event.target.closest?.("input.source-public");
    if (box) void changePublication(box);
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
      setFilter(null); // the filter, picker and details belong to the old vault
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
      if (scroller.scrollTop === 0 && details.hidden && picker.hidden && !refreshing) void refresh();
    },
    get size() {
      return tiles.size;
    },
  };
}
