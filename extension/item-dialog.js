// The confirmation of one item action (Issue #43): a policy change for one URL, or deleting one URL or
// one image. Nothing is done when an action is chosen: the host previews it first, the dialog shows
// what the host said, and only an explicit confirmation sends the change, bound to what was previewed
// (the policy's ETag, the vault's token). Nothing is shown as done before the host confirmed it.
//
// One dialog serves the URL tab and the Image tab. Every request belongs to one opening of it
// (`session`) and one step of that opening; an answer for an earlier opening or step is dropped, so a
// late answer can never fill the dialog of another item, another vault or another choice.

import { SearchError, VAULT_UNAVAILABLE_CODES } from "./search-client.js";
import { applyPolicyItem, deleteImage, deleteUrl, previewImageDelete, previewPolicyItem, previewUrlDelete } from "./vault-items-client.js";

export const DECISION_LABEL = { allow: "公開", save_but_no_mcp: "MCPサーバに見せない", never_save: "保存しない" };
const DECISION_TEXT = {
  allow: "公開（記録し、MCPサーバにも見せる）",
  save_but_no_mcp: "MCPサーバに見せない（Vaultには記録する）",
  never_save: "保存しない（自動記録せず、MCPサーバにも見せない）",
};
export const ACTION_LABEL = {
  never_save: "保存しないグループへ追加",
  save_but_no_mcp: "MCPサーバに見せないグループへ追加",
  delete: "Vaultから削除",
  publish: "公開",
};
const CONFIRM_LABEL = {
  never_save: "保存しないグループへ追加する",
  save_but_no_mcp: "MCPサーバに見せないグループへ追加する",
  publish: "公開する（ルールを外す）",
};
/** How an action is said when it would leave the state as it is (`sameState`). */
export const SAME_STATE = { never_save: "既に保存しない", save_but_no_mcp: "既にMCPサーバに見せない", publish: "既に公開" };

/**
 * Whether `action` would leave a URL as the host judges it now (`decision`): a rule that cannot make it
 * stricter, or publishing a URL already public. Only the host's decision is compared; whether the
 * change can be made at all is the preview's answer.
 */
export function sameState(action, decision) {
  if (action === "never_save") return decision === "never_save";
  if (action === "save_but_no_mcp") return decision === "save_but_no_mcp" || decision === "never_save";
  if (action === "publish") return decision === "allow";
  return false;
}

const LAYER_NAME = { neverSave: "保存しないグループ", saveButNoMcp: "MCPサーバに見せないグループ" };
const KIND_LABEL = { pattern: "Pattern", regex: "Regex", domainWildcard: "Domain Wildcard", exact: "Exact URL" };
const REASON_TEXT = {
  publish_blocked: "このURLは、ほかのルールまたは基本保護によっても非公開になっています。ここで外せるのはこのURLだけのルール（Exact URL）で、ほかのURLにも効くルールは変更しません。変更するにはPrivacy設定を開いてください。",
  layer_disabled: "このグループは現在無効です。ルールを追加しても効果がないため、追加しません。Privacy設定でグループを有効にできます。",
  policy_limit: "ルールの数が上限に達しているため追加できません（1グループ1,000件）。Privacy設定でルールを整理してください。",
  policy_migration_required: "プライバシー設定が旧形式（v1）のため、ここからは変更できません。",
  unsupported_url: "このURLのホスト（IPv6アドレスなど）はルールで指定できません。",
  invalid_url: "このURLはルールの対象にできません（http・https以外、または判定できない形式）。",
};
/** Reasons the settings page can resolve. */
const SETTINGS_REASONS = new Set(["publish_blocked", "layer_disabled", "policy_limit", "policy_migration_required"]);

const errorText = (error, fallback) => (error instanceof SearchError ? error.message : fallback);
const isVaultSwitched = (error) => error instanceof SearchError && error.code === "vault_switched";

/**
 * `elements`: the dialog's parts (see sidepanel.html, `#item-dialog`). `openPrivacySettings` opens the
 * settings page; `onVaultUnavailable` points at the vault settings.
 */
export function createItemDialog({ send, openPrivacySettings, onVaultUnavailable = () => {}, document: doc = document }) {
  const $ = (id) => doc.querySelector(`#${id}`);
  const dialog = $("item-dialog");
  const title = $("item-dialog-title");
  const target = $("item-dialog-target");
  const urlChoice = $("item-dialog-urls");
  const urlList = $("item-dialog-url-list");
  const scopeChoice = $("item-dialog-scope");
  const scopeUrl = $("item-scope-url");
  const scopeHost = $("item-scope-host");
  const body = $("item-dialog-body");
  const ack = $("item-dialog-ack");
  const ackBox = $("item-dialog-ack-box");
  const ackText = $("item-dialog-ack-text");
  const errorLine = $("item-dialog-error");
  const confirmButton = $("item-dialog-confirm");
  const repreviewButton = $("item-dialog-repreview");
  const settingsButton = $("item-dialog-settings");
  const cancelButton = $("item-dialog-cancel");

  let session = null;

  function paragraph(text, className = "") {
    const p = doc.createElement("p");
    p.textContent = text;
    if (className) p.className = className;
    return p;
  }

  function list(lines) {
    const ul = doc.createElement("ul");
    ul.className = "item-dialog-list";
    for (const text of lines) {
      const li = doc.createElement("li");
      li.textContent = text;
      ul.append(li);
    }
    return ul;
  }

  /** Enables what the current state allows; nothing while a request runs. */
  function update() {
    const s = session;
    if (!s) return;
    const busy = s.busy;
    scopeUrl.disabled = busy || s.kind !== "policy" || s.action === "publish" || !s.url;
    scopeHost.disabled = scopeUrl.disabled;
    for (const input of s.urlInputs ?? []) input.disabled = busy || input.dataset.unavailable === "true";
    ackBox.disabled = busy;
    cancelButton.disabled = busy;
    repreviewButton.disabled = busy;
    const ready = s.ready && !busy && (!s.needsAck || ackBox.checked);
    confirmButton.disabled = !ready;
  }

  function setError(text) {
    errorLine.textContent = text;
    errorLine.hidden = text === "";
  }

  function open(next) {
    close({ restoreFocus: false, notify: true });
    session = { ...next, step: 0, busy: false, ready: false, needsAck: false };
    title.textContent = next.title;
    target.textContent = next.targetText ?? "";
    target.hidden = !next.targetText;
    urlList.replaceChildren();
    urlChoice.hidden = true;
    scopeChoice.hidden = true;
    scopeUrl.checked = true;
    scopeHost.checked = false;
    ack.hidden = true;
    ackBox.checked = false;
    body.replaceChildren();
    setError("");
    repreviewButton.hidden = true;
    settingsButton.hidden = true;
    confirmButton.textContent = next.confirmLabel;
    confirmButton.className = next.destructive ? "danger" : "";
    dialog.hidden = false;
    update();
    dialog.focus();
    return session;
  }

  /** Closes the dialog; `notify` tells the opener (which resets its drop-down). */
  function close({ restoreFocus = true, notify = true } = {}) {
    const s = session;
    if (!s) return;
    session = null;
    const hadFocus = dialog.contains(doc.activeElement);
    dialog.hidden = true;
    body.replaceChildren();
    urlList.replaceChildren();
    if (notify) s.onClose?.();
    if (restoreFocus && hadFocus) s.opener?.focus?.();
  }

  /** Starts a step of `s`; its answer counts only while it is still the latest step of the open dialog. */
  function step(s) {
    s.step += 1;
    const mine = s.step;
    return () => session === s && s.step === mine;
  }

  // --- Policy changes ---------------------------------------------------------------------------

  /**
   * `targets`: `[{ url, label, decision?, unavailable? }]`. One target is the URL itself; several (an
   * image) must be chosen from, none is chosen for the person. `note`: shown with every preview.
   */
  function openPolicy({ owner, action, targets, targetText, note = "", opener, onApplied, onClose }) {
    const s = open({ kind: "policy", owner, action, title: ACTION_LABEL[action], targetText, confirmLabel: CONFIRM_LABEL[action], opener, onApplied, onClose, note, url: null, scope: "url", preview: null });
    if (targets.length === 1 && !targets[0].unavailable) {
      s.url = targets[0].url;
      if (!targetText) {
        target.textContent = `対象URL: ${s.url}`;
        target.hidden = false;
      }
      void previewPolicy(s);
    } else {
      renderTargets(s, targets);
      body.replaceChildren(paragraph("ルールを適用するURLを選んでください。画像そのものではなく、選んだURLにルールが適用されます。"));
      if (note) body.append(paragraph(note, "item-dialog-note"));
    }
    scopeChoice.hidden = action === "publish";
    update();
  }

  function renderTargets(s, targets) {
    urlChoice.hidden = false;
    const rows = targets.map((choice, index) => {
      const row = doc.createElement("label");
      row.className = "item-url-option";
      const input = doc.createElement("input");
      input.type = "radio";
      input.name = "item-url";
      input.className = "item-url";
      input.value = choice.url;
      input.id = `item-url-${index}`;
      input.dataset.unavailable = String(Boolean(choice.unavailable));
      input.disabled = Boolean(choice.unavailable);
      const text = doc.createElement("span");
      const state = choice.decision ? `（現在: ${DECISION_LABEL[choice.decision]}）` : "（現在の状態: 不明）";
      text.textContent = `${choice.label}${state}${choice.unavailable ? ` — ${choice.unavailable}` : ""}: ${choice.url}`;
      row.append(input, text);
      return row;
    });
    urlList.replaceChildren(...rows);
    s.targets = targets;
    s.urlInputs = rows.map((row) => row.children[0]);
  }

  async function previewPolicy(s) {
    const current = step(s);
    s.ready = false;
    s.preview = null;
    s.needsAck = false;
    ack.hidden = true;
    ackBox.checked = false;
    repreviewButton.hidden = true;
    settingsButton.hidden = true;
    setError("");
    body.replaceChildren(paragraph("Native Hostで確認しています…"));
    update();
    try {
      const preview = await previewPolicyItem({ url: s.url, action: s.action, scope: s.scope }, { send });
      if (!current()) return;
      s.preview = preview;
      renderPolicyPreview(s, preview);
    } catch (error) {
      if (!current()) return;
      body.replaceChildren();
      setError(errorText(error, "確認できませんでした。"));
      repreviewButton.hidden = false;
      if (error instanceof SearchError && VAULT_UNAVAILABLE_CODES.has(error.code)) onVaultUnavailable();
    }
    update();
  }

  function ruleLine(change) {
    const verb = change.op === "add" ? "追加" : "削除";
    return `${verb}: ${LAYER_NAME[change.layer]}・${KIND_LABEL[change.rule.kind]}「${change.rule.pattern}」`;
  }

  function blockerLine(blocker) {
    if (blocker.builtin) return `${LAYER_NAME[blocker.layer]}: 基本保護（組み込みのカテゴリまたはURL内の認証情報）`;
    return `${LAYER_NAME[blocker.layer]}: ${blocker.ruleIndex + 1}番目のルール（${KIND_LABEL[blocker.kind]}）`;
  }

  function renderPolicyPreview(s, preview) {
    const parts = [];
    parts.push(paragraph(`対象URL: ${preview.url}`));
    if (s.action !== "publish") {
      parts.push(paragraph(preview.scope === "host" ? "適用範囲: 同じホスト全体（このURL以外にも適用されます）" : "適用範囲: このURLのみ（Exact URL）", preview.scope === "host" ? "item-dialog-warning" : ""));
    }
    parts.push(paragraph(`現在の状態: ${DECISION_TEXT[preview.current]}`));
    parts.push(paragraph(`実行後の状態: ${DECISION_TEXT[preview.expected]}`));
    if (preview.changes.length > 0) {
      parts.push(paragraph("変更されるルール:"));
      parts.push(list(preview.changes.map(ruleLine)));
    }
    if (s.note) parts.push(paragraph(s.note, "item-dialog-note"));
    if (s.action !== "publish") parts.push(paragraph("ルールの追加は、Vaultに保存済みのデータを削除しません。", "item-dialog-note"));
    let ready = preview.executable;
    if (!preview.executable) {
      parts.push(paragraph(REASON_TEXT[preview.reason] ?? errorText(new SearchError(preview.reason), "この操作は実行できません。"), "item-dialog-warning"));
      if (preview.blockers.length > 0) {
        parts.push(paragraph("非公開にしているもの:"));
        parts.push(list(preview.blockers.map(blockerLine)));
      }
      settingsButton.hidden = !SETTINGS_REASONS.has(preview.reason);
    } else if (preview.changes.length === 0) {
      ready = false;
      parts.push(paragraph(s.action === "publish" ? "このURLは既に公開されています。変更はありません。" : "同じルールが既にあります。変更はありません。"));
    } else if (preview.expected === preview.current && s.action !== "publish") {
      parts.push(paragraph("現在の状態は変わりませんが、このルールが追加されます（ほかのルールを外しても、このルールは残ります）。"));
    }
    if (preview.affectsOtherUrls && ready) {
      s.needsAck = true;
      ack.hidden = false;
      ackText.textContent = "このホストのすべてのURL（ほかのページも含む）に適用されることを確認しました";
    }
    s.ready = ready;
    body.replaceChildren(...parts);
  }

  async function applyPolicy(s) {
    const preview = s.preview;
    if (!preview || !s.ready || s.busy || (s.needsAck && !ackBox.checked)) return;
    const current = step(s);
    s.busy = true;
    setError("");
    update();
    try {
      const result = await applyPolicyItem({ url: preview.url, action: preview.action, scope: preview.scope, etag: preview.etag }, { send });
      if (!current()) return;
      s.busy = false;
      close({ restoreFocus: true });
      s.onApplied?.({ ...result, url: preview.url, action: preview.action, scope: preview.scope });
    } catch (error) {
      if (!current()) return;
      s.busy = false;
      // What is stored is unknown or unchanged: never shown as done, and never sent again as it was.
      s.ready = false;
      setError(error instanceof SearchError && error.code === "policy_conflict" ? `${error.message}「もう一度確認」で最新の設定に対して確認し直せます。` : errorText(error, "変更できませんでした。"));
      repreviewButton.hidden = false;
      update();
    }
  }

  // --- Deletions --------------------------------------------------------------------------------

  /** Deletes one URL of the URL tab's results; `vaultToken`: the vault the results came from. */
  function openUrlDelete({ owner, url, label, vaultToken, opener, onDeleted, onVaultMismatch, onClose }) {
    const s = open({ kind: "url_delete", owner, title: "Vaultから削除", targetText: `対象: ${label}\n${url}`, confirmLabel: "削除する（取り消せません）", destructive: true, opener, onDeleted, onVaultMismatch, onClose, url, vaultToken });
    void previewUrl(s);
  }

  async function previewUrl(s) {
    const current = step(s);
    s.ready = false;
    repreviewButton.hidden = true;
    setError("");
    body.replaceChildren(paragraph("削除される記録を数えています…"));
    update();
    try {
      const preview = await previewUrlDelete(s.url, { send });
      if (!current()) return;
      if (s.vaultToken && preview.vaultToken !== s.vaultToken) return vaultMoved(s);
      s.preview = preview;
      const c = preview.counts;
      const parts = [paragraph("このURLの記録をVaultから完全に削除します。取り消せません。", "item-dialog-warning")];
      parts.push(
        list([
          `閲覧記録: ${c.visits}件`,
          `タブのスナップショット: ${c.tabSnapshots}件`,
          `ページ情報（タイトル・説明など）: ${c.pageMetadata}件`,
          `このURLへの移動の記録: ${c.navigations}件`,
          ...(c.navigationSourcesCleared > 0 ? [`このURLから別のページへ移動した記録 ${c.navigationSourcesCleared}件は残り、移動元が「不明」になります`] : []),
        ]),
      );
      parts.push(paragraph(c.imageSavesKept > 0 ? `このURLから保存した画像の保存履歴 ${c.imageSavesKept}件は削除されません（Imageタブに残ります）。` : "保存済みの画像は削除されません。"));
      parts.push(paragraph("作成済みのバックアップファイルには、この記録が残ります。プライバシー設定は変わらないため、このURLを再び開くと記録されることがあります（記録しないようにするには「保存しないグループへ追加」）。", "item-dialog-note"));
      const nothing = c.visits + c.tabSnapshots + c.pageMetadata === 0;
      if (nothing) parts.push(paragraph("このURLの記録は既にありません。"));
      s.ready = !nothing;
      body.replaceChildren(...parts);
    } catch (error) {
      if (!current()) return;
      failPreview(s, error, "削除内容を確認できませんでした。");
    }
    update();
  }

  /** Deletes the image `item` of the gallery; `vaultToken`: the vault the gallery shows. */
  function openImageDelete({ owner, item, label, vaultToken, opener, onDeleted, onVaultMismatch, onClose }) {
    const s = open({ kind: "image_delete", owner, title: "Vaultから削除", targetText: `対象: ${label}`, confirmLabel: "削除する（取り消せません）", destructive: true, opener, onDeleted, onVaultMismatch, onClose, item, vaultToken });
    void previewImage(s);
  }

  async function previewImage(s) {
    const current = step(s);
    s.ready = false;
    repreviewButton.hidden = true;
    setError("");
    body.replaceChildren(paragraph("削除される記録を数えています…"));
    update();
    try {
      const preview = await previewImageDelete(s.item, { send });
      if (!current()) return;
      if (preview.vaultToken !== s.vaultToken) return vaultMoved(s);
      s.preview = preview;
      const c = preview.counts;
      const kb = Math.max(1, Math.round(preview.byteLength / 1024));
      body.replaceChildren(
        paragraph("この画像をVaultから完全に削除します。取り消せません。", "item-dialog-warning"),
        list([
          `画像データ（元のファイル、${kb} KB）`,
          `保存履歴: ${c.saves}件（すべて）`,
          `サムネイル: ${c.thumbnails}件`,
          `タグの付与: ${c.tagLinks}件${c.tagsRemoved > 0 ? `（ほかの画像に付いていないタグ ${c.tagsRemoved}件はタグ一覧からも消えます）` : ""}`,
        ]),
        paragraph("ほかの画像、閲覧記録（URL）は削除されません。作成済みのバックアップファイルには、この画像が残ります。", "item-dialog-note"),
      );
      s.ready = true;
    } catch (error) {
      if (!current()) return;
      failPreview(s, error, "削除内容を確認できませんでした。");
    }
    update();
  }

  function failPreview(s, error, fallback) {
    if (isVaultSwitched(error)) return vaultMoved(s);
    body.replaceChildren();
    if (imageGone(s, error)) return;
    setError(errorText(error, fallback));
    repreviewButton.hidden = false;
    if (error instanceof SearchError && VAULT_UNAVAILABLE_CODES.has(error.code)) onVaultUnavailable();
  }

  /** An image the vault no longer has (deleted, or another vault is active): nothing to retry here. */
  function imageGone(s, error) {
    if (s.kind !== "image_delete" || !(error instanceof SearchError) || error.code !== "blob_missing") return false;
    setError("この画像がVaultに見つかりません（既に削除されたか、Vaultが切り替えられた可能性があります）。画像一覧を更新してください。");
    return true;
  }

  /** The vault is no longer the one the list came from: nothing is deleted, the list starts again. */
  function vaultMoved(s) {
    close({ restoreFocus: false });
    s.onVaultMismatch?.();
  }

  async function runDelete(s) {
    if (!s.preview || !s.ready || s.busy) return;
    const current = step(s);
    s.busy = true;
    setError("");
    update();
    try {
      const deleted = s.kind === "url_delete" ? await deleteUrl(s.url, s.preview.vaultToken, { send }) : await deleteImage(s.item, s.preview.vaultToken, { send });
      if (!current()) return;
      s.busy = false;
      close({ restoreFocus: false });
      s.onDeleted?.(deleted);
    } catch (error) {
      if (!current()) return;
      s.busy = false;
      if (isVaultSwitched(error)) return vaultMoved(s);
      // Not deleted, or not known to be: kept as it is, and only a new preview can confirm again.
      s.ready = false;
      if (imageGone(s, error)) return update();
      setError(errorText(error, "削除できませんでした。"));
      repreviewButton.hidden = false;
      update();
    }
  }

  // --- Events -----------------------------------------------------------------------------------

  confirmButton.addEventListener("click", () => {
    const s = session;
    if (!s) return;
    if (s.kind === "policy") void applyPolicy(s);
    else void runDelete(s);
  });
  repreviewButton.addEventListener("click", () => {
    const s = session;
    if (!s || s.busy) return;
    if (s.kind === "policy") void previewPolicy(s);
    else if (s.kind === "url_delete") void previewUrl(s);
    else void previewImage(s);
  });
  cancelButton.addEventListener("click", () => {
    if (session && !session.busy) close();
  });
  settingsButton.addEventListener("click", () => openPrivacySettings());
  ackBox.addEventListener("change", () => update());
  for (const radio of [scopeUrl, scopeHost]) {
    radio.addEventListener("change", () => {
      const s = session;
      if (!s || s.kind !== "policy" || s.busy || !s.url) return;
      s.scope = scopeHost.checked ? "host" : "url";
      void previewPolicy(s);
    });
  }
  urlList.addEventListener("change", (event) => {
    const input = event.target.closest?.("input.item-url");
    const s = session;
    if (!input || !s || s.kind !== "policy" || s.busy || input.disabled) return;
    s.url = input.value;
    s.scope = "url";
    scopeUrl.checked = true;
    scopeHost.checked = false;
    void previewPolicy(s);
  });
  dialog.addEventListener("keydown", (event) => {
    if (event.key !== "Escape" || !session || session.busy) return;
    event.preventDefault();
    close();
  });

  return {
    openPolicy,
    openUrlDelete,
    openImageDelete,
    /** Closes the dialog if it belongs to `owner` (a list or a selection that is gone). */
    cancel(owner) {
      if (session && (owner === undefined || session.owner === owner)) close({ restoreFocus: false });
    },
    get owner() {
      return session?.owner ?? null;
    },
    get busy() {
      return Boolean(session?.busy);
    },
  };
}
