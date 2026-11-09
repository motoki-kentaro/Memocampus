// Vault management requests (`vault_*`) and their display text.

import { SEARCH_TIMEOUT_MS, callHost } from "./search-client.js";

/** Operations that may show a Windows file dialog wait for the user, so they get a long timeout. */
export const DIALOG_TIMEOUT_MS = 10 * 60_000;

export function vaultRequest(type, params, options) {
  const interactive = type !== "vault_status";
  return callHost(
    { type, ...params },
    { timeoutMs: interactive ? DIALOG_TIMEOUT_MS : SEARCH_TIMEOUT_MS, ...options, resultType: "vault_result" },
  );
}

export const STATE_TEXT = {
  ready: "接続中",
  not_configured: "Vaultが未設定です",
  missing: "Vaultファイルが見つかりません",
  invalid: "Vaultの形式が不正です",
  vault_schema_mismatch: "このVaultは現在の形式に対応していません。自動変換は行いません",
  config_invalid: "設定ファイルを読み取れません",
  app_dir_unavailable: "設定フォルダを特定できません",
  // Issue #51: an encrypted or misdescribed vault is reported as such, never as invalid.
  vault_mode_mismatch: "Vaultファイルが設定上の形式（暗号化／平文）と一致しません。ファイルは変更していません",
  vault_encryption_unsupported: "このVaultは暗号化されていますが、このバージョンは暗号化Vaultを開けません。ファイルは変更していません",
  key_unavailable: "暗号化Vaultの鍵を取得できません。ファイルは変更していません",
  vault_locked: "暗号化Vaultがロックされています。ファイルは変更していません",
  vault_decrypt_failed: "暗号化Vaultを復号できません。ファイルは変更していません",
};

/** MCP access to the vault shown (Issue #51): `status.publication` from a host that knows it. */
export const PUBLICATION_TEXT = {
  confirmed: "MCPへの公開: 承認済み",
  pending: "MCPへの公開: 未承認",
};

export const PUBLICATION_NOTE = {
  confirmed: "MCPクライアントは、プライバシー設定を通過した記録と、MCP公開にした画像だけを読めます。設定の変更は次の読み取りから反映されます。",
  pending: "MCPクライアントには、このVaultの内容を一切返しません。新規作成・切り替え・置き換えたVaultは、承認するまでこの状態です。",
  unusable: "Vaultを開けないため、承認できません。",
};

export const PUBLICATION_REVIEW =
  "このVaultを、MCPクライアント（Claudeなど）から読めるようにします。\n" +
  "読めるのは、現在のプライバシー設定（保存しないグループ・MCPサーバに見せないグループ・基本保護）を通過した記録と、MCP公開にした画像だけです。\n" +
  "承認の前に、下の「プライバシー設定」で内容を確認してください。承認は、このVaultファイルにだけ適用されます。";

export function describePublication(status) {
  const text = PUBLICATION_TEXT[status?.publication];
  if (!text) return "";
  const revision = status.publicationPolicyRevision;
  return status.publication === "confirmed" && Number.isSafeInteger(revision) ? `${text}（プライバシー設定 改訂${revision}の時点で承認）` : text;
}

export const REASON_TEXT = {
  network_path: "ネットワーク共有(UNC)上です",
  network_drive: "ネットワークドライブ上です",
  optical_drive: "光学ドライブです",
  ram_disk: "RAMディスクです（再起動で消えます）",
  unknown_drive: "ドライブの種類を判定できません",
  executable_directory: "Native Hostの実行ファイルと同じフォルダです",
  removable_drive: "取り外し可能なドライブです",
  non_local_filesystem: "NTFS/ReFS以外のファイルシステムです",
  volume_info_unavailable: "ボリューム情報を取得できません",
  google_drive: "Google Driveのドライブ上です",
  sync_folder: "同期フォルダ(OneDrive等)の中です",
  cloud_files: "クラウド同期ファイルの属性があります",
};

export const VERDICT_TEXT = {
  no_issue_detected: "既知の問題は検出されませんでした",
  needs_approval: "注意が必要な場所です",
  rejected: "Active Vaultには使用できない場所です",
};

export const LOCATION_NOTE =
  "未登録の同期ツール（Syncthing等）は判定できません。Active Vaultは同期・ネットワーク外のローカルディスクに置いてください。";

export function describeReasons(reasons) {
  return Array.isArray(reasons) ? reasons.map((reason) => REASON_TEXT[reason] || reason) : [];
}

/** Text for the one-line summary at the top of the vault settings. */
export function describeStatus(status) {
  if (!status) return "";
  if (status.overridden) return `開発用DB(MEMOCAMPUS_DB)を使用中（${STATE_TEXT[status.state] || status.state}）`;
  return STATE_TEXT[status.state] || "Vaultの状態を確認できません";
}

/** True when the host answered and no usable vault is active, so the settings tab needs a visit. */
export function vaultNeedsAttention(status) {
  return Boolean(status) && status.state !== "ready";
}

export function formatCounts(counts) {
  if (!counts) return "";
  return `閲覧 ${counts.visits.toLocaleString("ja-JP")}件・スナップショット ${counts.snapshots.toLocaleString("ja-JP")}件`;
}
