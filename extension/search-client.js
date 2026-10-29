// Native Messaging client. Transport is injected so this module stays testable.

export const HOST_NAME = "com.memocampus.vault";
export const PAGE_SIZE = 50;
export const MAX_QUERY_CHARS = 200;
export const SEARCH_TIMEOUT_MS = 10_000;

const ERROR_MESSAGES = {
  host_unavailable: "Native Hostに接続できません。インストールと登録を確認してください。",
  timeout: "Native Hostから応答がありません。Hostが古いか、停止している可能性があります。",
  invalid_response: "Native Hostから不正な応答を受け取りました。",
  db_error: "データベースの操作に失敗しました。",
  query_too_long: `検索語は${MAX_QUERY_CHARS}文字以内で入力してください。`,
  invalid_query: "検索条件が不正です。",
  invalid_limit: "検索条件が不正です。",
  invalid_offset: "これ以上読み込めません。",
  invalid_request_id: "要求が不正です。",
  unknown_type: "Native Hostがこの操作に対応していません。Hostを更新してください。",
  app_dir_unavailable: "%LOCALAPPDATA% を特定できないため、Vaultの設定を読み書きできません。",
  vault_not_configured: "Vaultが未設定です。Vault設定から新規作成するか、既存のVaultを開いてください。",
  vault_missing: "Vaultファイルが見つかりません。移動・削除されていないか確認し、Vaultを開き直してください。",
  vault_invalid: "このファイルはMemocampus Vaultとして利用できません。",
  config_invalid: "設定ファイル(config.json)を読み取れません。Vaultを作成し直すか開き直すと設定が書き直されます。",
  vault_switched: "Vaultが切り替えられました。もう一度お試しください。",
  vault_busy: "Vaultが使用中です。しばらくしてからもう一度お試しください。",
  management_disabled: "開発用DB(MEMOCAMPUS_DB)が指定されているため、Vault管理は無効です。",
  file_exists: "同名のファイルが既にあります。上書きはしません。別の名前を選ぶか「既存のVaultを開く」を使ってください。",
  sidecar_exists: "保存先に同名の -journal / -wal / -shm ファイルが残っているため、安全のため作成を中止しました。",
  parent_missing: "保存先のフォルダが存在しません。",
  invalid_path: "パスが不正です。ドライブ名から始まる絶対パスを指定してください。",
  cancelled: "キャンセルしました。",
  backup_failed: "バックアップに失敗しました。バックアップは作成されていません。",
  location_rejected: "この場所はActive Vaultとして使用できません。",
  location_needs_approval: "この場所をActive Vaultにするには確認が必要です。",
  io_error: "ファイルの読み書きに失敗しました。",
  invalid_ack: "Native Hostの応答が不正です。Hostを更新してください。",
  invalid_url: "記録できないURLです。",
  invalid_event: "イベントの形式が不正です。",
  invalid_message: "メッセージの形式が不正です。",
  queue_full: "送信待ちキューが満杯です。",
  storage_write_failed: "拡張機能のストレージへの保存に失敗しました。",
  dead_letter_full: "隔離領域が満杯のため、送信を一時停止しています。",
  dead_letter_write_failed: "隔離領域への保存に失敗したため、送信を一時停止しています。",
};

/** Japanese message for a host or client error code. */
export function describeErrorCode(code) {
  return ERROR_MESSAGES[code] || "操作に失敗しました。";
}

/** Codes meaning "there is no usable vault" — the UI should send the user to vault settings. */
export const VAULT_UNAVAILABLE_CODES = new Set(["vault_not_configured", "vault_missing", "vault_invalid", "config_invalid", "app_dir_unavailable"]);

export class SearchError extends Error {
  constructor(code, detail = "", response = null) {
    super(describeErrorCode(code));
    this.name = "SearchError";
    this.code = code;
    this.detail = detail;
    this.response = response;
  }
}

/** Wraps chrome.runtime.sendNativeMessage; each call starts a short-lived host process. */
export function chromeNativeTransport(runtime) {
  return (message) =>
    new Promise((resolve, reject) => {
      runtime.sendNativeMessage(HOST_NAME, message, (response) => {
        const lastError = runtime.lastError;
        if (lastError) return reject(new SearchError("host_unavailable", lastError.message || ""));
        resolve(response);
      });
    });
}

function withTimeout(promise, timeoutMs) {
  let timer;
  const timeout = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new SearchError("timeout")), timeoutMs);
  });
  return Promise.race([promise, timeout]).finally(() => clearTimeout(timer));
}

/**
 * Sends one request carrying a fresh requestId and validates the tagged response. Resolves to
 * the response on `ok: true`; rejects with SearchError (keeping the response) otherwise.
 */
export async function callHost(message, { send, resultType, timeoutMs = SEARCH_TIMEOUT_MS, createId = () => crypto.randomUUID() }) {
  const requestId = createId();
  const response = await withTimeout(
    Promise.resolve().then(() => send({ ...message, requestId })),
    timeoutMs,
  );
  if (!response || response.type !== resultType || response.requestId !== requestId) {
    throw new SearchError("invalid_response");
  }
  if (response.ok !== true) {
    throw new SearchError(typeof response.error === "string" ? response.error : "invalid_response", "", response);
  }
  return response;
}

function isWebUrl(value) {
  if (typeof value !== "string") return false;
  try {
    const { protocol } = new URL(value);
    return protocol === "http:" || protocol === "https:";
  } catch {
    return false;
  }
}

const optionalString = (value) => (typeof value === "string" ? value : null);

function normalizeItem(item) {
  if (!item || !isWebUrl(item.url) || typeof item.lastActivityAt !== "string") return null;
  return {
    url: item.url,
    title: typeof item.title === "string" ? item.title : "",
    lastVisitedAt: optionalString(item.lastVisitedAt),
    lastCapturedAt: optionalString(item.lastCapturedAt),
    lastActivityAt: item.lastActivityAt,
    source: ["visit", "snapshot", "both"].includes(item.source) ? item.source : "visit",
  };
}

/**
 * Sends one search request. Resolves to { items, received, hasMore }, where `received` is the raw
 * row count used to advance the offset.
 */
export async function searchPages({ query, offset = 0, limit = PAGE_SIZE }, options) {
  const response = await callHost({ type: "search_pages", query, limit, offset }, { ...options, resultType: "search_pages_result" });
  if (!Array.isArray(response.items)) throw new SearchError("invalid_response");
  return {
    items: response.items.map(normalizeItem).filter(Boolean),
    received: response.items.length,
    hasMore: response.hasMore === true,
  };
}

/** Hands out tokens so that only the most recently started search may update the UI. */
export function createLatestGate() {
  let current = 0;
  return {
    next: () => ++current,
    isCurrent: (token) => token === current,
  };
}

export { isWebUrl };
