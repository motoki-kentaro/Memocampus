import { captureAndUploadPage, PageSaveError } from "./page-upload.js";
import { t } from "./i18n.js";
import { registerMenuItem } from "./menu-item.js";

export const SAVE_PAGE_MENU_ID = "memocampus-save-page";
/** Creates the menu item, or only updates its title to the UI language if it already exists. */
export function registerPageMenu(contextMenus, runtime) {
  return registerMenuItem(contextMenus, runtime, { id: SAVE_PAGE_MENU_ID, title: t("menuSavePage"), contexts: ["page", "frame", "link", "selection", "image", "video", "audio"] });
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

// The message key for each page-save failure code; the codes themselves are never translated.
const PAGE_ERROR_KEYS = Object.freeze({
  unsupported_url: "pageSaveErrorUnsupportedUrl",
  incognito_not_allowed: "pageSaveErrorIncognitoNotAllowed",
  navigation_changed: "pageSaveErrorNavigationChanged",
  vault_switched: "pageSaveErrorVaultSwitched",
  invalid_page_size: "pageSaveErrorInvalidPageSize",
  page_upload_busy: "pageSaveErrorPageUploadBusy",
  vault_not_configured: "pageSaveErrorVaultNotConfigured",
  vault_schema_mismatch: "pageSaveErrorVaultSchemaMismatch",
  save_state_unknown: "pageSaveErrorSaveStateUnknown",
  timeout: "pageSaveErrorTimeout",
  transfer_interrupted: "pageSaveErrorTransferInterrupted",
  capture_failed: "pageSaveErrorCaptureFailed",
  vault_locked: "pageSaveErrorVaultLocked",
  key_unavailable: "pageSaveErrorKeyUnavailable",
  vault_reselect_required: "pageSaveErrorVaultReselectRequired",
  save_id_conflict: "pageSaveErrorSaveIdConflict",
  db_error: "pageSaveErrorDbError",
});

export function describePageSaveResult(result) {
  if (result.ok) return { title: t("pageSavedTitle"), message: t("pageSavedMessage") };
  const key = typeof result.code === "string" && Object.hasOwn(PAGE_ERROR_KEYS, result.code) ? PAGE_ERROR_KEYS[result.code] : "pageSaveErrorGeneric";
  return { title: t("pageSaveFailedTitle"), message: t(key) };
}
