// The one deletion UI of the URL, Image and Page tabs (Issue #7): a red button with assets/delete.svg and
// one confirmation dialog for URL and Image deletion. Every text here is set with textContent.
import { formatFullDate } from "./format.js";
import { CENSORED_LABEL } from "./navigation-text.js";

export const DELETE_TEXT = Object.freeze({
  trashEvent: "この閲覧履歴を削除", trashImage: "この画像を削除", trashPageEvent: "この保存履歴を削除",
  urlAll: "URL全件削除", visitsFiltered: "条件に一致する閲覧履歴を削除", visitsAll: "全閲覧履歴を削除",
  imagesFiltered: "検索結果をすべて削除", imagesAll: "全画像を削除",
  title: "削除の確認", cancel: "キャンセル", close: "閉じる", confirm: "削除する", deleting: "削除中…", previewing: "削除対象を確認しています…",
});

const ERRORS = Object.freeze({
  item_not_found: "削除対象は既に存在しません。削除していません。",
  item_targets_changed: "確認後に削除対象が変更されました。何も削除していません。もう一度確認してください。",
  invalid_review: "削除確認が無効・期限切れです。何も削除していません。もう一度確認してください。",
  vault_switched: "Vaultが変更されました。何も削除していません。一覧を読み込み直してください。",
  vault_locked: "Vaultがロックされています。設定画面で状態を確認してください。",
  key_unavailable: "Vaultの鍵を使用できません。", vault_not_configured: "Vaultが未選択です。",
  vault_schema_mismatch: "このVaultのスキーマには対応していません。",
  // The request was sent and nothing proves its outcome: never "nothing was deleted", never resent by itself.
  item_delete_unknown: "削除状態を確認できません。削除要求の結果を確認できませんでした。削除が完了している可能性があります。Vaultの内容を再確認してください（自動では再実行しません）。",
  delete_not_sent: "削除要求は送信されませんでした。何も削除していません。",
  timeout: "Hostからの応答がありません。削除状態は確認できていません。一覧を読み込み直してください。",
  host_unavailable: "Native Hostに接続できません。何も削除していません。",
  unknown_type: "Native Hostがこの削除に対応していません。Hostを更新してください。",
  invalid_tag: "タグを指定できません。何も削除していません。",
  item_response_too_large: "表示データが大きすぎるため取得できません。",
});
export const deleteErrorText = (code) => Object.hasOwn(ERRORS, code) ? ERRORS[code] : "削除に失敗しました。成功扱いにせず、一覧を読み込み直して状態を確認してください。";
/** Errors after which the list shown is no longer what the host has. */
/**
 * A status line with a place for the one message that must never be overwritten: a delete that was sent and
 * whose outcome is unknown. Ordinary messages (`show`) replace each other; the notice stays above them until
 * the person clears it (`clear`) or the page is closed. `vault()` is the token of the Vault the tab shows now:
 * a notice of another Vault says so, and is never taken for a result in this one.
 */
export function createStatusNotice({ paint, vault = () => null }) {
  let current = { text: "", isError: false }, notice = null;
  const render = () => {
    const lines = [];
    if (notice) lines.push(notice.vaultToken !== null && vault() !== null && notice.vaultToken !== vault() ? `${deleteErrorText(UNKNOWN_CODE)}（以前に開いていたVaultでの削除要求です）` : deleteErrorText(UNKNOWN_CODE));
    if (current.text) lines.push(current.text);
    paint(lines.join("\n"), notice !== null || current.isError, notice !== null);
  };
  return {
    show(text, isError = false) { current = { text, isError }; render(); },
    unknown(vaultToken = null) { notice = { vaultToken }; render(); },
    clear() { notice = null; render(); },
    get unknownShown() { return notice !== null; },
  };
}

/** The one outcome that must outlive the dialog and any later message: the delete may have happened. */
export const UNKNOWN_CODE = "item_delete_unknown";
export const STALE_CODES = new Set(["item_not_found", "item_targets_changed", "invalid_review", "item_delete_unknown", "timeout", "vault_switched"]);

/**
 * A red delete button. `text` makes it a labelled "range" button; without it the button is the icon only,
 * and `label` is its accessible name and tooltip. The icon is assets/delete.svg through CSS (a mask).
 */
export function createTrashButton({ label, text = null, onClick = null, document: doc = globalThis.document }) {
  const button = doc.createElement("button");
  button.type = "button";
  button.className = text === null ? "danger-button icon-only" : "danger-button";
  button.setAttribute("aria-label", label);
  button.title = label;
  setDangerLabel(button, text);
  if (onClick) button.addEventListener("click", onClick);
  return button;
}

/** (Re)sets the visible label of a danger button, keeping its icon; null: icon only. */
export function setDangerLabel(button, text) {
  let icon = button.querySelector?.(".danger-icon") ?? null;
  if (!icon) {
    icon = button.ownerDocument.createElement("span");
    icon.className = "danger-icon";
    icon.setAttribute("aria-hidden", "true");
    button.children.length ? button.insertBefore(icon, button.children[0]) : button.append(icon);
  }
  let label = button.querySelector?.(".danger-label") ?? null;
  if (text === null) { label?.remove(); return; }
  if (!label) { label = button.ownerDocument.createElement("span"); label.className = "danger-label"; button.append(label); }
  label.textContent = text;
  if (button.className.includes("icon-only")) button.className = button.className.replace(" icon-only", "");
}

const n = (value) => Number(value ?? 0).toLocaleString("ja-JP");
const count = (value) => `${n(value)}件`;
const bytes = (value) => `${n(value)} bytes`;
const safeDate = (iso) => { try { return formatFullDate(iso); } catch { return iso; } };

// The sections of every deletion confirmation (URL, Image and Page): what is deleted, what it affects, what
// is kept, and one warning that nothing comes back. Facts are rows (a name and a value) so that a count is
// always next to its name; a value that comes from the web (title, URL, search text) is only ever text.
export const REVIEW_TEXT = Object.freeze({ target: "削除対象", effects: "削除による影響", notes: "注意事項", warning: "削除すると元に戻せません。", warningDetail: "ゴミ箱はありません。バックアップには残る場合があります。Vaultファイルは小さくなりません。" });

/**
 * The three counts of a visit deletion, kept apart: the visits (n), the navigation records that stay with their
 * source deleted (m, shown as [CENSORED] by a later route view) and the ones that go with a deleted visit (k).
 * A zero of m or k is left out.
 */
function navigationRows(counts) {
  const rows = [{ label: "削除する閲覧履歴", value: count(counts.visits) }];
  if (counts.navigationSourcesCleared) rows.push({ label: `遷移元が検閲済み（${CENSORED_LABEL}）になる遷移記録`, value: count(counts.navigationSourcesCleared) });
  if (counts.navigations) rows.push({ label: "一緒に削除される遷移記録", value: count(counts.navigations) });
  return rows;
}
const navigationNote = (counts) => counts.navigationSourcesCleared ? ["検閲済みの遷移記録は残りますが、削除した閲覧は復元しません。"] : [];

/** What the confirmation shows, from the host's preview: { target, effects, notes } (rows `{label, value, long}`). `condition`: the list's filter as text. */
export function deletionModel(preview, condition = "") {
  const { counts, target, detail } = preview;
  if (target.scope === "visit") {
    return {
      target: [{ label: "対象", value: "閲覧履歴1件" }, { label: "日時", value: safeDate(detail?.visitedAt ?? "") }, { label: "タイトル", value: detail?.title || "（なし）", long: true }, { label: "URL", value: detail?.url ?? "", long: true }],
      effects: [...navigationRows(counts), ...(counts.pageMetadata ? [{ label: "このイベントが持つページ情報", value: count(counts.pageMetadata) }] : [])],
      notes: ["同じURLの他の閲覧履歴とスナップショットは削除しません。", ...navigationNote(counts)],
    };
  }
  if (target.scope === "visits") {
    return {
      target: [{ label: "対象", value: "閲覧履歴" }, condition ? { label: "条件", value: condition, long: true } : { label: "条件", value: "なし（すべての閲覧履歴）" }, { label: "対象のURL", value: count(counts.urls) }],
      effects: [...navigationRows(counts), ...(counts.pageMetadata ? [{ label: "一緒に削除するページ情報", value: count(counts.pageMetadata) }] : [])],
      notes: ["件数は表示中のURL数ではなく、DB上の一致した閲覧履歴の件数です。", `スナップショットは対象外です（これらのURLのスナップショット ${count(counts.snapshotsKept)}は残ります）。`, "条件に一致しない同じURLの閲覧履歴も残ります。", ...navigationNote(counts)],
    };
  }
  if (target.scope === "url") {
    return {
      target: [{ label: "対象", value: "URL全件" }, { label: "URL", value: target.url, long: true }],
      effects: [...navigationRows(counts), { label: "削除するスナップショット", value: count(counts.tabSnapshots) }, { label: "削除するページ情報", value: count(counts.pageMetadata) }],
      notes: [`このURLを含む保存画像の保存履歴 ${count(counts.imageSavesKept)}は削除しません。`, "ポリシーは変更しません。ポリシーが許す限り、次の閲覧は再び記録されます。", ...navigationNote(counts)],
    };
  }
  const images = [
    { label: "削除する画像", value: `${count(counts.images)}（原本 ${bytes(counts.bytes)}）` }, { label: "保存履歴", value: count(counts.saves) }, { label: "サムネイル", value: count(counts.thumbnails) }, { label: "タグの付与", value: count(counts.tagLinks) },
    ...(counts.tagsRemoved ? [{ label: "他に使われず消えるタグ", value: count(counts.tagsRemoved) }] : []),
  ];
  return {
    target: target.scope === "images"
      ? [{ label: "対象", value: "画像" }, target.tag ? { label: "条件", value: `タグ「${target.tag}」の付いた画像`, long: true } : { label: "条件", value: "なし（すべての画像）" }]
      : [{ label: "対象", value: "画像1件" }, { label: "内容", value: condition || "画像1件", long: true }],
    effects: images,
    notes: ["閲覧履歴・URL・他の画像は削除しません。"],
  };
}

/**
 * Renders a model into `container` with DOM calls only (every value is set as text; nothing is parsed as markup):
 * three headed sections and a separate warning. Long values wrap inside the dialog.
 */
export function renderDeletion(container, { target = [], effects = [], notes = [] }) {
  const doc = container.ownerDocument;
  const make = (tag, className, text) => { const element = doc.createElement(tag); if (className) element.className = className; if (text !== undefined) element.textContent = text; return element; };
  const list = (rows) => {
    const dl = make("dl", "delete-facts");
    for (const row of rows) {
      const group = make("div", "delete-fact");
      group.append(make("dt", "", row.label), make("dd", row.long ? "delete-long" : "", row.value));
      dl.append(group);
    }
    return dl;
  };
  const section = (title, body, className = "") => { const element = make("section", `delete-section ${className}`.trim()); element.append(make("h3", "", title), body); return element; };
  const root = make("div", "delete-review");
  if (target.length) root.append(section(REVIEW_TEXT.target, list(target), "delete-target"));
  if (effects.length) root.append(section(REVIEW_TEXT.effects, list(effects), "delete-effects"));
  if (notes.length) {
    const ul = make("ul", "delete-notes");
    for (const note of notes) ul.append(make("li", "", note));
    root.append(section(REVIEW_TEXT.notes, ul));
  }
  const warning = make("p", "delete-warning");
  warning.setAttribute("role", "note");
  warning.append(make("strong", "", REVIEW_TEXT.warning), make("span", "", ` ${REVIEW_TEXT.warningDetail}`));
  root.append(warning);
  container.replaceChildren(root);
}

/** The confirm button: short, red, with the trash icon, like every other delete button. */
export function setConfirmLabel(button, text) { setDangerLabel(button, text); }

/**
 * The dialog. `open` asks the host for a preview and shows it; only the confirm button deletes, once,
 * the exact set that was previewed. `cancel()` (a new search, another Vault) withdraws the dialog and
 * drops every answer still on its way.
 */
export function createDeleteDialog({ client }) {
  const $ = (id) => document.querySelector(`#${id}`);
  const dialog = $("item-delete-dialog"), title = $("item-delete-title"), description = $("item-delete-description"), error = $("item-delete-error"), cancel = $("item-delete-cancel"), confirm = $("item-delete-confirm");
  title.textContent = DELETE_TEXT.title; cancel.textContent = DELETE_TEXT.cancel; setConfirmLabel(confirm, DELETE_TEXT.confirm);
  let session = null, seq = 0, onStatus = () => {};
  const busy = () => session !== null;
  function show() { dialog.hidden = false; dialog.showModal?.(); }
  function hide() { if (dialog.open) dialog.close?.(); dialog.hidden = true; }
  function finish(restore) {
    const s = session; session = null; hide();
    error.hidden = true; confirm.disabled = true; setConfirmLabel(confirm, DELETE_TEXT.confirm); cancel.textContent = DELETE_TEXT.cancel; cancel.disabled = false;
    if (restore && s?.opener) s.opener.focus?.();
    return s;
  }
  function fail(s, code) {
    error.textContent = deleteErrorText(code); error.hidden = false;
    confirm.disabled = true; cancel.disabled = false; cancel.textContent = DELETE_TEXT.close; cancel.focus?.();
    s.failed = code;
  }
  /**
   * `target`: what the host previews. `vaultToken`: the Vault the list came from; a preview of another one
   * is refused (`onVaultMismatch`). `condition`: the list's filter as text. `onDeleted(result, preview)` after the host's COMMIT.
   * `onStale(code)`: the list no longer matches the host. Resolves when the dialog is open or the attempt ended.
   */
  async function open({ target, vaultToken = null, condition = "", opener = null, onDeleted, onVaultMismatch, onStale, onError, onUnknown }) {
    if (busy()) return;
    const s = session = { id: ++seq, opener, target, preview: null, deleting: false, failed: null };
    onStatus(DELETE_TEXT.previewing);
    let preview;
    try { preview = await client.preview(target); }
    catch (err) {
      if (session !== s) return;
      session = null; onStatus("");
      if (err.code === "vault_switched") onVaultMismatch?.();
      else { onError?.(err.code); if (STALE_CODES.has(err.code)) onStale?.(err.code); }
      opener?.focus?.();
      return;
    }
    if (session !== s) return;
    onStatus("");
    if (vaultToken !== null && preview.vaultToken !== vaultToken) { session = null; onVaultMismatch?.(); opener?.focus?.(); return; }
    s.preview = preview;
    renderDeletion(description, deletionModel(preview, condition));
    error.hidden = true; confirm.disabled = false; cancel.disabled = false;
    show(); cancel.focus?.();
    s.onDeleted = onDeleted; s.onStale = onStale; s.onUnknown = onUnknown ?? onError;
  }
  async function confirmDelete() {
    const s = session;
    if (!s || !s.preview || s.deleting || s.failed) return;
    s.deleting = true; confirm.disabled = true; cancel.disabled = true; setConfirmLabel(confirm, DELETE_TEXT.deleting); dialog.setAttribute("aria-busy", "true");
    let result;
    try { result = await client.delete(s.preview.reviewId); }
    catch (err) {
      // The dialog's life and the delete's life differ: a connection that ends withdraws the dialog, but a
      // DELETE already sent keeps its outcome. Unknown always reaches the tab that asked for it.
      if (err.code === UNKNOWN_CODE) s.onUnknown?.(UNKNOWN_CODE, { vaultToken: s.preview.vaultToken });
      if (session !== s) return;
      dialog.setAttribute("aria-busy", "false"); setConfirmLabel(confirm, DELETE_TEXT.confirm); s.deleting = false;
      fail(s, err.code);
      return;
    }
    if (session !== s) return;
    dialog.setAttribute("aria-busy", "false");
    const preview = s.preview, deleted = s.onDeleted;
    finish(false);
    deleted?.(result.deleted ?? preview.counts, preview);
  }
  function dismiss() {
    const s = session;
    if (!s || s.deleting) return;
    const code = s.failed;
    finish(true);
    if (code && STALE_CODES.has(code)) s.onStale?.(code);
  }
  cancel.addEventListener("click", dismiss);
  confirm.addEventListener("click", () => void confirmDelete());
  dialog.addEventListener("keydown", (event) => {
    if (event.key === "Escape") { event.preventDefault(); dismiss(); }
    if (event.key === "Tab") { event.preventDefault(); const order = [cancel, confirm].filter((b) => !b.disabled); if (order.length) order[(order.indexOf(document.activeElement) + 1) % order.length].focus(); }
  });
  dialog.addEventListener("cancel", (event) => { event.preventDefault(); dismiss(); });
  return {
    open,
    /** Withdraws the dialog without a callback: the list it was about is gone or belongs to another Vault. */
    cancel() { if (!session) return; seq++; finish(false); onStatus(""); },
    get busy() { return busy(); },
    onStatus(handler) { onStatus = handler; },
  };
}
