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
  invalid_bounds: "日付の指定が不正です。",
  unknown_type: "Native Hostがこの操作に対応していません。Hostを更新してください。",
  app_dir_unavailable: "%LOCALAPPDATA% を特定できないため、Vaultの設定を読み書きできません。",
  vault_not_configured: "Vaultが未設定です。Vault設定から新規作成するか、既存のVaultを開いてください。",
  vault_missing: "Vaultファイルが見つかりません。移動・削除されていないか確認し、Vaultを開き直してください。",
  vault_invalid: "このファイルはMemocampus Vaultとして利用できません。",
  vault_schema_mismatch: "このVaultは現在の形式に対応していません。自動変換は行いません（このVaultは変更していません）。",
  config_invalid: "設定ファイル(config.json)を読み取れません。Vaultを作成し直すか開き直すと設定が書き直されます。",
  vault_switched: "Vaultが切り替えられました。もう一度お試しください。",
  vault_mode_mismatch: "Vaultファイルが設定上の形式（暗号化／平文）と一致しません。ファイルは変更していません。",
  vault_encryption_unsupported: "この環境では暗号化Vaultを開けません。ファイルは変更していません。",
  key_unavailable: "このWindowsユーザーでは暗号化Vaultの鍵を取得できません。ファイルは変更していません。",
  // Encrypted vaults (Issue #51 Slice B)
  vault_backup_encrypted_unsupported: "暗号化Vaultのバックアップは、まだ作成できません（今後のバージョンで対応します）。平文のコピーは作成しません。",
  // Backups and restore (Issue #51 Slice C)
  backup_password_required: "パスワードを入力してください。",
  backup_password_too_short: "パスワードは8文字以上にしてください。",
  backup_password_too_long: "パスワードが長すぎます。",
  backup_password_rejected: "パスワードが違います。何も作成していません。",
  backup_path_required: "復元するバックアップファイルを選んでください。",
  backup_unsupported_format: "このファイルはMemocampusのバックアップではありません。",
  backup_unsupported_version: "このバックアップは未対応の版です。Memocampusを更新してください。",
  backup_malformed: "バックアップファイルの形式が不正です（改変または破損）。何も作成していません。",
  backup_incomplete: "バックアップファイルが途中で切れているか、余分なデータが付いています。何も作成していません。",
  backup_tampered: "バックアップの内容が改変または破損しています。復元途中のファイルは削除しました。",
  backup_damaged: "バックアップの内容または復旧情報が破損しています。復元途中のファイルは削除しました。",
  backup_schema_mismatch: "このバックアップのVaultの形式には対応していません。",
  backup_vault_invalid: "このファイルはMemocampus Vaultのバックアップとして利用できません。",
  vault_restore_pending: "使用開始前の復元済みVault（または中断された復元）があります。使い始めるか破棄してから、もう一度お試しください。",
  vault_restore_not_pending: "使用開始を待っている復元済みVaultはありません。",
  vault_restore_incomplete: "復元が中断されています。破棄してから、もう一度復元してください。",
  vault_restore_damaged: "復元の記録または復元先のファイルが壊れているか、変更されています。破棄してください。",
  restore_failed: "復元に失敗しました。復元途中のファイルは削除しました。",
  disk_full: "保存先の空き容量が不足しています。作成途中のファイルは削除しました。",
  backup_wal_unsupported: "このVaultはWALモードのため、パスワード保護バックアップを作成できません。通常のバックアップを使ってください。何も作成していません。",
  recovery_key_mismatch: "復旧情報ファイル（.mckey）が、このVaultの鍵と一致しません。バックアップは作成していません。",
  invalid_request: "要求が不正です。",
  vault_creation_pending: "作成途中の暗号化Vaultがあります。破棄してから、もう一度作成してください。",
  // Issue #51 Slice D: conversion between plain and encrypted.
  vault_convert_pending: "変換途中（または使用開始前）のVaultがあります。使い始めるか、保存して終えるか、破棄してから、もう一度お試しください。",
  vault_convert_not_pending: "使用開始を待っている変換済みVaultはありません。",
  vault_convert_incomplete: "変換が中断されています。破棄してから、もう一度変換してください。元のVaultは変更していません。",
  vault_convert_spell_unconfirmed: "新しいVaultのふっかつのじゅもんを確認するまで、使い始めることはできません。",
  vault_convert_invalid_state: "この操作は、今の変換の段階では行えません。表示を更新してください。",
  vault_convert_damaged: "変換の記録または変換先のファイルが壊れているか、変更されています。破棄してください。元のVaultは変更していません。",
  vault_convert_source_changed: "変換元のVaultが、変換を始めたときのファイルではなくなりました。Active Vaultは変更していません。変換したVaultは、切り替えずに保存するか破棄してください。",
  vault_convert_same_as_source: "変換先に、変換元のVaultそのものが選ばれました。別の新しいファイル名を選んでください。",
  vault_convert_mode_mismatch: "表示中のVaultの形式（暗号化／平文）が変わりました。表示を更新してから、もう一度お試しください。",
  vault_convert_plaintext_unacknowledged: "暗号化を解除すると、閲覧履歴や画像が暗号化されずに保存されます。確認のチェックを入れてから実行してください。",
  vault_convert_verify_failed: "変換したVaultが変換元と一致しませんでした。変換途中のファイルは削除しました。元のVaultは変更していません。",
  vault_convert_failed: "変換に失敗しました。変換途中のファイルは削除しました。元のVaultは変更していません。",
  vault_work_file: "作成・復元・変換の途中のファイルは、Vaultとして開けません。",
  vault_creation_not_pending: "作成途中の暗号化Vaultが見つかりません。最初から作成し直してください。",
  vault_creation_damaged: "作成途中の暗号化Vaultのファイルが壊れているか、見つかりません。破棄して作成し直してください。",
  recovery_spell_invalid: "ふっかつのじゅもんの形式が正しくありません。",
  recovery_spell_rejected: "ふっかつのじゅもんが、このVaultのものと一致しません。Vaultも鍵も変更していません。",
  recovery_metadata_unsupported:
    "このVaultの復旧情報ファイル（.mckey）は、以前の12単語のリカバリーフレーズ方式のもので、この版では使えません。このWindowsユーザーで開けるVaultはそのまま使えますが、別の環境での復旧や形式B・Dのバックアップはできません。",
  backup_legacy_recovery_phrase:
    "このバックアップは、以前の12単語のリカバリーフレーズ方式で作られたもの（形式BまたはD）で、この版では復元できません。何も変更していません。",
  recovery_metadata_missing: "このVaultの復旧情報ファイル（.mckey）が見つかりません。Vaultと同じフォルダに置いてください。",
  recovery_metadata_damaged: "このVaultの復旧情報ファイル（.mckey）が壊れているか、改変されています。",
  recovery_metadata_mismatch: "復旧情報ファイル（.mckey）が、このVaultファイルのものではありません。",
  vault_not_encrypted: "このVaultは暗号化されていません。「既存のVaultを開く」で開いてください。",
  key_protector_unavailable: "この環境では鍵を保護できない（Windows DPAPIが使えない）ため、暗号化Vaultを作成できません。",
  key_protector_failed: "Windowsで鍵を保護できませんでした。何も作成していません。",
  random_unavailable: "安全な乱数を取得できませんでした。何も作成していません。",
  vault_locked: "暗号化Vaultがロックされています。ファイルは変更していません。",
  vault_decrypt_failed: "暗号化Vaultを復号できません。ファイルは変更していません。",
  invalid_vault_token: "対象のVaultを確認できませんでした。Vault設定を開き直してください。",
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
  // Privacy policy
  policy_not_initialized: "プライバシー設定が初期化されていないため、自動記録を停止しています。",
  policy_invalid: "プライバシー設定を読み取れない（欠落・破損・未対応の版）ため、自動記録を停止しています。設定は上書きしていません。",
  policy_busy: "プライバシー設定が使用中のため、自動記録を一時停止しています。",
  policy_unavailable: "プライバシー設定にアクセスできないため、自動記録を停止しています。",
  policy_unsupported: "プライバシー設定に、このバージョンでは使えないルール（先読み・後読み・後方参照を使う正規表現など）が含まれているため、自動記録を停止しています。設定は上書きしていません。",
  invalid_policy_batch: "Native Hostと拡張機能のバージョンが合わないため、自動記録を停止しています。",
  policy_batch_too_large: "Native Hostと拡張機能のバージョンが合わないため、自動記録を停止しています。",
  policy_already_initialized: "プライバシー設定は初期化済みです。既存の設定は上書きしません。",
  // Image saving
  incognito_unknown: "保存元のウィンドウの種類を確認できないため、保存しませんでした。",
  invalid_incognito: "保存元のウィンドウの種類を確認できないため、保存しませんでした。",
  incognito_fixed_at_begin: "転送手順が不正です。もう一度保存してください。",
  unsupported_url: "この画像のURL形式には対応していません。",
  network_error: "画像を取得できませんでした（ネットワークエラー）。",
  http_error: "画像の取得に失敗しました（HTTPエラー）。",
  page_access_denied: "このページからは画像を取得できません。",
  too_large: "画像が大きすぎます（上限 32 MiB）。",
  empty_image: "画像データが空です。",
  svg_not_supported: "SVG画像は保存できません。",
  unsupported_format: "対応していない画像形式です（PNG・JPEG・GIF・WebP・AVIF・BMPに対応）。",
  decode_failed: "画像を読み込めませんでした。壊れているか、対応していない形式です。",
  transfer_interrupted: "Native Hostとの接続が途中で切れました。保存されていません。",
  host_outdated: "Native Hostが古いため画像を保存できません。Hostを更新してください。",
  save_failed: "画像の保存に失敗しました。",
  saved_image_not_found: "この保存履歴が見つかりません。一覧を更新してください。",
  incognito_not_publishable: "シークレット保存のためMCP非公開です。",
  invalid_saved_image_id: "保存履歴の指定が不正です。",
  invalid_mcp_public: "公開設定の指定が不正です。",
  hash_mismatch: "転送中に画像データが変化しました。保存されていません。",
  length_mismatch: "画像データの長さが一致しません。保存されていません。",
  mime_mismatch: "画像の形式が申告と一致しません。",
  dimension_mismatch: "画像の寸法が一致しません。",
  invalid_dimensions: "画像の寸法が不正です。",
  truncated_image: "画像データが途中で切れています。",
  invalid_thumbnail: "サムネイルが不正です。",
  save_id_conflict: "保存IDが重複しています。もう一度保存してください。",
  blob_missing: "保存済みの画像が見つかりません。もう一度保存してください。",
  invalid_hash: "画像のハッシュ値が不正です。",
  invalid_save_id: "保存IDが不正です。",
  unsupported_protocol: "Native Hostと拡張機能のバージョンが合いません。",
  no_upload: "転送が中断されました。もう一度保存してください。",
  upload_in_progress: "別の転送が進行中です。もう一度保存してください。",
  save_id_mismatch: "転送の対応関係が崩れました。もう一度保存してください。",
  unexpected_chunk: "転送手順が不正です。もう一度保存してください。",
  duplicate_chunk: "転送データが重複しました。もう一度保存してください。",
  chunk_out_of_order: "転送データの順序が乱れました。もう一度保存してください。",
  invalid_chunk_size: "転送データのサイズが不正です。もう一度保存してください。",
  invalid_base64: "転送データが壊れています。もう一度保存してください。",
  upload_too_large: "画像データが申告より大きいため中止しました。",
  incomplete_upload: "画像データが揃わないまま確定しようとしました。保存されていません。",
  // Image tags
  invalid_tag: "タグ名が不正です。1〜64文字で、制御文字や見えない文字は使えません。",
  too_many_tags: "1枚の画像に付けられるタグは64個までです。",
  // Managing one item (Issue #43)
  policy_conflict: "プライバシー設定がほかの画面で変更されました。内容を確認し直してください。",
  publish_blocked: "ほかのルールまたは基本保護によって非公開になっているため、ここからは公開できません。Privacy設定で確認してください。",
  policy_limit: "ルールの数が上限に達しているため追加できません（1グループ1,000件）。Privacy設定で整理してください。",
  layer_disabled: "このグループは無効になっているため、ルールを追加しても効果がありません。Privacy設定でグループを有効にしてください。",
  invalid_exact_url: "URLの形式が不正なため、Exact URLルールにできません。",
  policy_migration_required: "プライバシー設定が旧形式（v1）のため、この操作はできません。",
  unsupported_url: "このURLのホストはルールで指定できません。",
  invalid_action: "要求が不正です。",
  invalid_scope: "要求が不正です。",
  invalid_etag: "要求が不正です。もう一度確認してください。",
  // Copy, export and thumbnail rebuilding
  invalid_vault_token: "要求が不正です。一覧を更新してください。",
  invalid_blob_id: "画像の指定が不正です。",
  read_incomplete: "画像データを最後まで読み出せませんでした。",
  read_hash_mismatch: "読み出した画像データが記録と一致しません（SHA-256）。",
  clipboard_unavailable: "このブラウザではクリップボードに画像をコピーできません。",
  clipboard_denied: "クリップボードへのコピーが許可されませんでした。サイドパネルをクリックしてから、もう一度お試しください。",
  clipboard_failed: "クリップボードにコピーできませんでした。",
  copy_too_large: "画像の画素数が大きすぎるため、コピーできません。",
  copy_convert_failed: "画像をPNGに変換できなかったため、コピーできません。",
  export_failed: "ファイルを書き出せませんでした。既存のファイルは変更されていません。",
  export_file_appeared: "書き出し中に同名のファイルが作られたため、上書きせずに中止しました。",
  export_invalid_target: "この場所には書き出せません。",
};

/** Japanese message for a host or client error code. */
export function describeErrorCode(code) {
  return ERROR_MESSAGES[code] || "操作に失敗しました。";
}

/** Codes meaning "there is no usable vault" — the UI should send the user to vault settings. */
export const VAULT_UNAVAILABLE_CODES = new Set(["vault_not_configured", "vault_missing", "vault_invalid", "vault_schema_mismatch", "config_invalid", "app_dir_unavailable"]);

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

/** A vault token as the host writes it (SHA-256, lowercase hex), or null. */
const validVaultToken = (value) => (typeof value === "string" && /^[0-9a-f]{64}$/.test(value) ? value : null);

/**
 * Sends one search request. Resolves to { items, received, hasMore, vaultToken }, where `received` is the raw
 * row count used to advance the offset. `visitedFrom` and `visitedTo` (Issue #5) narrow it to the visits of a
 * local day: both or neither are sent, and only to a host that names `visit_calendar_v1` (an older host would
 * ignore them and answer the unfiltered list).
 */
export async function searchPages({ query, offset = 0, limit = PAGE_SIZE, visitedFrom = null, visitedTo = null }, options) {
  const message = { type: "search_pages", query, limit, offset };
  if (visitedFrom !== null || visitedTo !== null) {
    if (typeof visitedFrom !== "string" || typeof visitedTo !== "string") throw new SearchError("invalid_bounds");
    Object.assign(message, { visitedFrom, visitedTo });
  }
  const response = await callHost(message, { ...options, resultType: "search_pages_result" });
  if (!Array.isArray(response.items)) throw new SearchError("invalid_response");
  // The vault the page came from (Issue #43); a host from before it sends none.
  const vaultToken = validVaultToken(response.vaultToken);
  return {
    items: response.items.map(normalizeItem).filter(Boolean),
    received: response.items.length,
    hasMore: response.hasMore === true,
    vaultToken,
  };
}

/**
 * Visits per local day of a month (Issue #5): `bounds` are the starts of the days and of the day after the
 * last (29 to 32 instants, see visit-calendar.js). Resolves to { counts, vaultToken }: one non-negative
 * integer per day, and the vault they were counted in. Anything else is `invalid_response`.
 */
export async function visitDays(bounds, options) {
  const response = await callHost({ type: "visit_days", bounds }, { ...options, resultType: "visit_days_result" });
  const counts = response.counts;
  const vaultToken = validVaultToken(response.vaultToken);
  if (!Array.isArray(counts) || counts.length !== bounds.length - 1 || !counts.every((count) => Number.isSafeInteger(count) && count >= 0) || vaultToken === null) {
    throw new SearchError("invalid_response");
  }
  return { counts: [...counts], vaultToken };
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
