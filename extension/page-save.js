import { captureAndUploadPage, PageSaveError } from "./page-upload.js";

export const SAVE_PAGE_MENU_ID = "memocampus-save-page";
export function registerPageMenu(contextMenus, runtime) {
  contextMenus.create({ id: SAVE_PAGE_MENU_ID, title: "Memocampusにページを保存", contexts: ["page", "frame", "link", "selection", "image", "video", "audio"] }, () => { void runtime?.lastError; });
}
const web = (raw) => { try { return ["http:", "https:"].includes(new URL(raw).protocol); } catch { return false; } };

/** Observes synchronously from click until COMMIT; documentId plus navigation generation catches
 * reloads and same-document A→B→A. No state or MHTML is persisted across worker restarts. */
export function createPageSaver(deps) {
  let active = null;
  const invalidate = (tabId, code = "navigation_changed") => {
    if (active?.tabId === tabId) { active.generation += 1; active.controller.abort(new PageSaveError(code)); }
  };
  const navigation = (details) => { if (details.frameId === 0) invalidate(details.tabId); };
  for (const event of [deps.webNavigation?.onBeforeNavigate, deps.webNavigation?.onCommitted, deps.webNavigation?.onHistoryStateUpdated, deps.webNavigation?.onReferenceFragmentUpdated, deps.webNavigation?.onErrorOccurred]) event?.addListener(navigation);
  deps.tabs?.onRemoved?.addListener((id) => invalidate(id));
  deps.tabs?.onReplaced?.addListener((added, removed) => { invalidate(removed); invalidate(added); });
  deps.tabs?.onUpdated?.addListener((id, change) => { if (change.status === "loading" || change.url !== undefined) invalidate(id); });
  return {
    cancel() { if (active) invalidate(active.tabId, "vault_switched"); },
    async save(_info, tab) {
      if (active) return { ok: false, code: "page_upload_busy" };
      if (!Number.isInteger(tab?.id) || tab.id < 0 || !web(tab.url)) return { ok: false, code: "unsupported_url" };
      if (tab.incognito !== false) return { ok: false, code: "incognito_not_allowed" };
      if (tab.status === "loading") return { ok: false, code: "navigation_changed" };
      // Never select activeTab/linkUrl/frameUrl. Freeze primitives before the first await.
      const operation = { tabId: tab.id, url: tab.url, title: tab.title ?? "", generation: 0, controller: new AbortController() };
      active = operation;
      const check = () => { if (operation.generation !== 0 || operation.controller.signal.aborted) throw new PageSaveError(operation.controller.signal.reason?.code ?? "navigation_changed"); };
      try {
        const initial = await deps.webNavigation.getFrame({ tabId: operation.tabId, frameId: 0 });
        check();
        if (!initial?.documentId || initial.url !== operation.url || initial.errorOccurred || initial.documentLifecycle !== "active") throw new PageSaveError("navigation_changed");
        const verify = async () => {
          check();
          const current = await deps.tabs.get(operation.tabId);
          const frame = await deps.webNavigation.getFrame({ tabId: operation.tabId, frameId: 0 });
          check();
          if (current?.incognito !== false || current.url !== operation.url || current.status === "loading" || frame?.documentId !== initial.documentId || frame.url !== operation.url || frame.errorOccurred || frame.documentLifecycle !== "active") throw new PageSaveError("navigation_changed");
        };
        const saved = await captureAndUploadPage({
          saveId: deps.randomUUID(),
          metadata: { url: operation.url, title: operation.title, capturedAt: deps.now(), incognito: false },
          capture: () => deps.capture({ tabId: operation.tabId }), verify,
          connect: deps.connect, signal: operation.controller.signal, timeoutMs: deps.timeoutMs,
        });
        return { ok: true, saved };
      } catch (error) { return { ok: false, code: error instanceof PageSaveError ? error.code : "capture_failed" }; }
      finally { if (active === operation) active = null; }
    },
  };
}

export function describePageSaveResult(result) {
  if (result.ok) return { title: "Memocampusにページを保存しました", message: "MHTML原本と保存履歴をVaultへ保存しました。MCPには公開しません。" };
  const reasons = {
    unsupported_url: "HTTP(S)ページのみ保存できます。リンク先ではなくクリック元のタブを保存します。",
    incognito_not_allowed: "シークレットモードのページ保存には対応していません。",
    navigation_changed: "保存中にページが変わったため中止しました。読み込み完了後に再操作してください。",
    vault_switched: "Vaultが変更されたため保存を中止しました。",
    invalid_page_size: "MHTMLが空、または上限128MiBを超えています。",
    page_upload_busy: "別のページを保存中です。完了後に再操作してください。",
    vault_not_configured: "先に設定画面で新しいVaultを作成または選択してください。",
    vault_schema_mismatch: "このVaultのスキーマは非対応です。Schema 8のVaultを明示的に新規作成してください。旧Vaultは変更しません。",
    save_state_unknown: "保存状態未確定です。自動再保存は行いません。重複を避けるため状態を確認してください。",
    timeout: "保存処理がタイムアウトしました。",
    transfer_interrupted: "Native Hostとの接続が切れたため保存できませんでした。",
    capture_failed: "ChromeによるMHTML取得に失敗しました。取得できないページやフレーム、ページ消失などの技術的制約が考えられます。",
    vault_locked: "Vaultがロックされているため保存できません。設定画面で状態を確認してください。",
    key_unavailable: "Vaultの鍵を使用できないため保存できません。",
    vault_reselect_required: "Vaultを設定画面で明示的に選び直してから保存してください。",
    save_id_conflict: "保存操作IDが既存の異なる保存記録と一致したため中止しました。",
    db_error: "Vaultへの保存を確定できませんでした。自動再保存は行いません。",
  };
  return { title: "ページを保存できませんでした", message: Object.hasOwn(reasons, result.code) ? reasons[result.code] : "ページ取得またはVaultへの保存に失敗しました。Chromeの技術的制約で取得できないページもあります。" };
}
