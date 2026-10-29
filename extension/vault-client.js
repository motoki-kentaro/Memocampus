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
  config_invalid: "設定ファイルを読み取れません",
  app_dir_unavailable: "設定フォルダを特定できません",
};

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
