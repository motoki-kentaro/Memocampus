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
  vault_encryption_unsupported: "この環境では暗号化Vaultを開けません。ファイルは変更していません",
  key_unavailable: "このWindowsユーザーでは暗号化Vaultの鍵を取得できません。ファイルは変更していません",
  vault_locked: "暗号化Vaultがロックされています。ファイルは変更していません",
  vault_decrypt_failed: "暗号化Vaultを復号できません。ファイルは変更していません",
};

/** Issue #51 Slice B: encrypted vaults. */
export const ENCRYPTION_NOTE =
  "暗号化すると、Vaultファイルが盗まれたりコピーされたりしても、鍵がなければ中身を読めません。" +
  "このWindowsユーザーでは、毎回パスワードを入力せずに使えます。作成時に表示する12単語のリカバリーフレーズは、" +
  "PCの入れ替えやWindowsの再インストール後に開くために必要です。";

export const PHRASE_TITLE =
  "リカバリーフレーズ（12単語）を紙などに順番どおり書き留めてください。\n" +
  "このフレーズは今回だけ表示します。あとから表示し直すことはできません。";

export const PHRASE_NOTE =
  "・別のPC、Windowsの再インストール後、別のWindowsユーザーでこのVaultを開くには、このフレーズと「.mckey」ファイルが必要です。\n" +
  "・フレーズをなくすと、このPCのWindowsユーザーが使えなくなったときにVaultを開けなくなります。Memocampusは再発行できません。\n" +
  "・フレーズを知っている人は、Vaultファイルと「.mckey」ファイルがあれば中身を読めます。誰にも伝えず、写真やクラウドのメモに残さないでください。\n" +
  "・この画面にはコピー機能はありません（クリップボードに残さないため）。";

export const PHRASE_CHECK_TITLE = "書き留めたフレーズから、次の番号の単語を入力してください。正しく入力されるまでVaultは作成されません。";

export const RECOVERY_TITLE =
  "このWindowsユーザーには、この暗号化Vaultの鍵が保存されていません（別のPC・再インストール後・別のユーザーなど）。\n" +
  "作成時に書き留めた12単語のリカバリーフレーズを入力すると、鍵をこのWindowsユーザー用に保存し直して開きます。\n" +
  "フレーズはこのPCに保存しません。";

/** Message for a phrase the host refused (`recovery_phrase_invalid` details). */
export function describePhraseProblem(response) {
  if (response?.reason === "word_count") return `12単語を入力してください（${Number(response.count) || 0}単語でした）。`;
  if (response?.reason === "unknown_word") return `${Number(response.position) || "?"}番目の単語が単語リストにありません。綴りを確認してください。`;
  if (response?.reason === "checksum") return "単語の綴りまたは順番が違います。書き留めたフレーズと照らし合わせてください。";
  return "リカバリーフレーズの形式が正しくありません。";
}

/** `count` distinct 1-based positions out of 12, in ascending order, from `random` (a CSPRNG fill). */
export function pickPositions(count = 3, random = (array) => crypto.getRandomValues(array)) {
  const chosen = new Set();
  const buffer = new Uint32Array(1);
  while (chosen.size < count) {
    random(buffer);
    // Rejection sampling keeps every position equally likely.
    if (buffer[0] >= 4294967292) continue;
    chosen.add((buffer[0] % 12) + 1);
  }
  return [...chosen].sort((a, b) => a - b);
}

/** True when every answer matches the word at its position (case and surrounding space ignored). */
export function checkAnswers(words, answers) {
  return answers.length > 0 && answers.every(({ position, word }) => typeof word === "string" && words[position - 1] === word.trim().toLowerCase());
}

/** Issue #51 Slice C: backup formats. A plain vault's normal backup is A; see `backupFormat`. */
export function backupFormat(mode, protection) {
  const encrypted = mode === "encrypted";
  if (protection === "password") return encrypted ? "D" : "C";
  return encrypted ? "B" : "A";
}

export const BACKUP_FORMAT_NOTE = {
  A: "形式A: 通常のSQLiteファイルとして保存します（暗号化なし）。プライバシー設定のコピーは含みません。",
  B: "形式B: 暗号化したまま1つのファイルに保存します。復元には12単語のリカバリーフレーズが必要です（このPCの鍵は不要）。作成時に、お手元のフレーズでこのVaultを復旧できることを確認します。プライバシー設定のコピーを暗号化して含みます。",
  C: "形式C: バックアップ全体をパスワードで暗号化します。復元にはこのパスワードが必要です。元のVault自体は暗号化されません。プライバシー設定のコピーを含みます。",
  D: "形式D: 暗号化Vaultを、さらにパスワードで保護します。復元にはパスワードと12単語のリカバリーフレーズの両方が必要です。作成時に、お手元のフレーズでこのVaultを復旧できることを確認します。プライバシー設定のコピーを含みます。",
};

export const PASSWORD_MIN_CHARS = 8;

export const PASSWORD_NOTE =
  `パスワードは${PASSWORD_MIN_CHARS}文字以上にしてください。パスワードはどこにも保存しません。忘れると、このバックアップは復元できません（Memocampusは再発行できません）。`;

export const BACKUP_PHRASE_NOTE =
  "暗号化Vaultのバックアップは、復元にリカバリーフレーズが必要です。書き留めたフレーズを入力してください。このVaultを復旧できるフレーズでなければ、バックアップは作成しません。フレーズは保存しません。";

/** Null when `text` has the shape of a 12-word phrase (the host checks the words and the vault). */
export function checkBackupPhrase(text) {
  const words = typeof text === "string" ? text.trim().split(/\s+/).filter(Boolean) : [];
  return words.length === 12 ? null : `リカバリーフレーズを12単語で入力してください（${words.length}単語でした）。`;
}

/** Null when the two entries make a usable backup password, otherwise what is wrong. */
export function checkBackupPassword(password, confirmation) {
  if (typeof password !== "string" || [...password].length < PASSWORD_MIN_CHARS) return `パスワードは${PASSWORD_MIN_CHARS}文字以上で入力してください。`;
  if (password !== confirmation) return "確認用のパスワードが一致しません。";
  return null;
}

export const RESTORE_FORMAT_TEXT = {
  A: "形式A（平文Vaultの通常バックアップ）。暗号化されていません。プライバシー設定のコピーは含まれません。",
  B: "形式B（暗号化Vaultの通常バックアップ）。復元には12単語のリカバリーフレーズが必要です。",
  C: "形式C（パスワード保護）。復元にはバックアップのパスワードが必要です。",
  D: "形式D（暗号化Vault＋パスワード保護）。復元にはパスワードと12単語のリカバリーフレーズの両方が必要です。",
};

export const RESTORE_NOTE =
  "復元は常に新しいVaultとして作成します。既存のファイルやVaultは上書きしません。" +
  "現在のVaultは、復元したVaultを「使い始める」まで変わりません。復元したVaultのMCPへの公開は、未承認から始まります。";

/** What the user is asked before a restore (`vault_restore_inspect` answer). */
export function describeRestoreSource(info) {
  const lines = [`復元するバックアップ: ${info.backupPath}`, RESTORE_FORMAT_TEXT[info.format] || "形式を確認できません。"];
  if (info.policyCopy === true) lines.push("プライバシー設定のコピーが含まれています（復元後に内容を確認できます。自動では適用しません）。");
  lines.push(RESTORE_NOTE);
  return lines.join("\n");
}

/** The restored vault, for the user's confirmation before it becomes active. */
export function describeRestored(restored) {
  const counts = restored?.counts;
  const lines = [`復元しました（まだ使用していません）: ${restored?.path ?? ""}`];
  lines.push(restored?.mode === "encrypted" ? "暗号化Vault（このWindowsユーザーで鍵を保存しました）" : "平文のVault");
  if (counts && Number.isSafeInteger(counts.visits)) {
    lines.push(`閲覧 ${counts.visits.toLocaleString("ja-JP")}件・スナップショット ${counts.snapshots.toLocaleString("ja-JP")}件・画像 ${counts.images.toLocaleString("ja-JP")}件`);
  }
  lines.push("「使い始める」を押すと、このVaultをActive Vaultにします。MCPへの公開は未承認のままです。");
  return lines.join("\n");
}

/** The policy copy a restore found: never applied, only shown. */
export function describePolicyCopy(copy) {
  switch (copy?.state) {
    case "valid":
      return `このバックアップには、プライバシー設定のコピー（改訂${Number(copy.revision) || "?"}）が含まれています。現在のプライバシー設定には自動で適用しません。内容を確認し、必要なら「プライバシー設定」で同じ設定にしてください。`;
    case "invalid":
      return "プライバシー設定のコピーを読み取れません（破損または未対応の版）。適用はしません。";
    case "absent":
      return "このバックアップには、プライバシー設定のコピーは含まれていません。";
    case "not_supported":
      return "形式Aのバックアップには、プライバシー設定のコピーは含まれません。";
    default:
      return "";
  }
}

/** A restore recorded by the host but not activated (`status.restorePending`). */
export function describeRestorePending(pending) {
  if (pending?.phase === "restored" || pending?.phase === "verified") {
    return `復元済みで、まだ使用していないVaultがあります:\n${pending.path}\n使い始めるか、破棄してください。`;
  }
  return `中断された復元があります${pending?.path ? `:\n${pending.path}` : ""}\n途中のファイルを破棄してから、もう一度復元してください。`;
}

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
