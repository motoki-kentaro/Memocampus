// The URL tab's rows are URL groups (Issue #7, PO decision A). A group expands to the URL's visits, one row each
// (`visits.event_id`), with an icon-only delete for that one visit; the group's own button deletes the whole URL
// as `url_delete` did. Tab snapshots are not visits: they are counted, never listed or deleted one by one.
import { formatFullDate } from "./format.js";
import { DELETE_TEXT, createTrashButton } from "./delete-ui.js";
import { formatNumber, t } from "./i18n.js";

const safeDate = (iso) => { try { return formatFullDate(iso); } catch { return iso; } };
const n = (value) => formatNumber(Number(value));

/**
 * `context()`: the list as shown ({ query, visited: {visitedFrom, visitedTo} | null, vaultToken }). A group lists the
 * visits that match it, so a deletion of "this visit" is always a visit the person was shown.
 */
export function createUrlHistory({ client, dialog, context, onVisitDeleted, onUrlDeleted, onVaultMismatch, onFailure, onUnknown }) {
  let epoch = 0, supported = false, counter = 0;
  const rows = new Set();

  function setSupported(value) {
    supported = value === true;
    for (const row of rows) row.tools.hidden = !supported;
  }

  /** Every expanded history belongs to a list that is gone (a new search, another Vault). */
  function invalidate() {
    epoch++;
    dialog.cancel();
    rows.clear();
  }

  function decorate(li, item) {
    counter += 1;
    const label = item.title || item.url;
    const row = { li, url: item.url, cursor: null, loaded: false, busy: false, open: false, buttons: [] };
    const tools = document.createElement("div");
    tools.className = "visit-tools";
    tools.hidden = !supported;
    const toggle = document.createElement("button");
    toggle.type = "button";
    toggle.className = "secondary";
    toggle.textContent = t("urlHistoryShow");
    toggle.setAttribute("aria-expanded", "false");
    toggle.id = `visit-toggle-${counter}`;
    const bodyId = `visit-history-${counter}`;
    toggle.setAttribute("aria-controls", bodyId);
    const urlDelete = createTrashButton({ label: t("urlHistoryDeleteUrlLabel", label), text: DELETE_TEXT.urlAll, onClick: () => deleteUrl(row) });
    urlDelete.title = t("urlHistoryDeleteUrlTitle", DELETE_TEXT.urlAll);
    tools.append(toggle, urlDelete);
    const body = document.createElement("div");
    body.className = "visit-history";
    body.id = bodyId;
    body.hidden = true;
    const note = document.createElement("p");
    note.className = "visit-history-note";
    note.setAttribute("role", "status");
    const list = document.createElement("ol");
    list.className = "visit-events";
    list.setAttribute("aria-label", t("urlHistoryListLabel"));
    const more = document.createElement("button");
    more.type = "button";
    more.className = "secondary";
    more.textContent = t("urlHistoryMore");
    more.hidden = true;
    body.append(note, list, more);
    Object.assign(row, { tools, toggle, body, note, list, more, urlDelete });
    toggle.addEventListener("click", () => setOpen(row, !row.open));
    more.addEventListener("click", () => void load(row, true));
    li.append(tools, body);
    rows.add(row);
    return row;
  }

  function setOpen(row, open) {
    row.open = open;
    row.toggle.setAttribute("aria-expanded", String(open));
    row.toggle.textContent = t(open ? "urlHistoryHide" : "urlHistoryShow");
    row.body.hidden = !open;
    if (open && !row.loaded) void load(row, false);
  }

  function appendEvents(row, items) {
    for (const item of items) {
      const event = document.createElement("li");
      event.className = "visit-event";
      const text = document.createElement("div");
      text.className = "visit-event-text";
      const time = document.createElement("time");
      time.dateTime = item.visitedAt;
      time.textContent = safeDate(item.visitedAt);
      const title = document.createElement("span");
      title.className = "visit-event-title";
      title.textContent = item.title || t("urlHistoryUntitled");
      text.append(time, title);
      const trash = createTrashButton({ label: t("urlHistoryDeleteVisitLabel", safeDate(item.visitedAt)), onClick: () => deleteVisit(row, item, trash) });
      trash.title = t("urlHistoryDeleteVisitTitle", DELETE_TEXT.trashEvent, safeDate(item.visitedAt));
      event.append(text, trash);
      row.list.append(event);
    }
  }

  async function load(row, append) {
    if (row.busy || !rows.has(row)) return;
    row.busy = true;
    row.more.disabled = true;
    const version = epoch, shown = context();
    try {
      const page = await client.history(row.url, { query: shown.query, visitedFrom: shown.visited?.visitedFrom ?? null, visitedTo: shown.visited?.visitedTo ?? null }, append ? row.cursor : null);
      if (version !== epoch || !rows.has(row)) return;
      if (shown.vaultToken && page.vaultToken !== shown.vaultToken) { onVaultMismatch?.(); return; }
      if (!append) row.list.replaceChildren();
      appendEvents(row, page.items);
      row.cursor = page.next;
      row.loaded = true;
      row.more.hidden = page.next === null;
      const c = page.counts;
      const scope = c.matched !== c.total ? t("urlHistoryNoteScope", n(c.total)) : "";
      row.note.textContent = t("urlHistoryNote", t("deleteCount", n(c.matched)), scope, t("deleteCount", n(c.snapshots)), DELETE_TEXT.urlAll);
    } catch (error) {
      if (version === epoch && rows.has(row)) {
        if (error.code === "vault_switched") onVaultMismatch?.();
        else onFailure?.(error.code);
      }
    } finally {
      if (version === epoch) { row.busy = false; row.more.disabled = false; }
    }
  }

  const reload = (row) => { row.loaded = false; row.cursor = null; if (row.open) void load(row, false); };

  function deleteVisit(row, item, opener) {
    void dialog.open({
      target: { scope: "visit", id: item.id },
      vaultToken: context().vaultToken,
      opener,
      onDeleted: (deleted) => onVisitDeleted?.(row.url, deleted),
      onVaultMismatch,
      onStale: () => reload(row),
      onError: onFailure,
      onUnknown,
    });
  }

  function deleteUrl(row) {
    void dialog.open({
      target: { scope: "url", url: row.url },
      vaultToken: context().vaultToken,
      opener: row.urlDelete,
      onDeleted: (deleted) => onUrlDeleted?.(row.li, deleted),
      onVaultMismatch,
      onStale: () => onVaultMismatch?.(),
      onError: onFailure,
      onUnknown,
    });
  }

  /** After a list reload: open this URL's history again (the group of a deleted visit). */
  function reopen(url) {
    for (const row of rows) {
      if (row.url !== url) continue;
      setOpen(row, true);
      row.toggle.focus?.();
      return true;
    }
    return false;
  }

  return { decorate, invalidate, setSupported, reopen, get rows() { return rows; } };
}
