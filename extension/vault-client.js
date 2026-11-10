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
  "このWindowsユーザーでは、毎回パスワードを入力せずに使えます。作成時に一度だけ表示する「ふっかつのじゅもん（Recovery Spell）」は、" +
  "PCの入れ替えやWindowsの再インストール後に開くときと、暗号化Vaultのバックアップから復元するときに必要です。";

/** Issue #51: the Recovery Spell (ふっかつのじゅもん), 256 random bits as 43 base64url characters. */
export const SPELL_NAME = "ふっかつのじゅもん";
export const SPELL_NAME_EN = "Recovery Spell";
export const SPELL_LENGTH = 43;
const SPELL_ALPHABET = /^[A-Za-z0-9_-]$/;
// The last of 43 characters carries 4 data bits and 2 unused bits, which must be zero.
const SPELL_LAST = "AEIMQUYcgkosw048";

export const SPELL_TITLE = `${SPELL_NAME}（${SPELL_NAME_EN}）`;

export const SPELL_LEAD = "あなたの記憶を取り戻すための、復活の呪文を記録してください。決して忘れないでくださいね。";

export const SPELL_CAUTION = "この呪文は一度しか表示されません。安全な場所に保管してください。失うとバックアップから復元できなくなる場合があります。";

export const SPELL_NOTE =
  "・別のPC、Windowsの再インストール後、別のWindowsユーザーでこのVaultを開くには、この呪文と「.mckey」ファイルが必要です。暗号化Vaultのバックアップ（形式B・D）から復元するときにも必要です。\n" +
  "・Memocampusは呪文を保存せず、再表示も再発行もできません。\n" +
  "・呪文を知っている人は、Vaultファイルと「.mckey」ファイル（またはバックアップ）があれば中身を読めます。誰にも伝えないでください。\n" +
  "・英字の大文字と小文字は区別されます。紙に書き写すときは、0（ゼロ）とO（オー）、1とlとI、-（ハイフン）と_（アンダースコア）の違いに注意してください。\n" +
  "・「コピー」を押すと、呪文がクリップボードに入ります。クリップボードやWindowsのクリップボード履歴に残る場合があるため、保管し終えたら消去してください。";

export const SPELL_CHECK_TITLE =
  `記録した${SPELL_NAME}を、${SPELL_LENGTH}文字すべて入力するか貼り付けてください。表示した呪文と完全に一致するまで、Vaultの作成は完了しません。\n` +
  "※この確認は、呪文を安全に保管したことの証明にはなりません。PCとは別の安全な場所に保管してください。";

export const RECOVERY_TITLE =
  "このWindowsユーザーには、この暗号化Vaultの鍵が保存されていません（別のPC・再インストール後・別のユーザーなど）。\n" +
  `作成時に記録した${SPELL_NAME}（${SPELL_LENGTH}文字）を入力すると、鍵をこのWindowsユーザー用に保存し直して開きます。\n` +
  "呪文はこのPCに保存しません。";

/** What the user typed or pasted, with surrounding whitespace removed (nothing else is changed). */
export function normalizeSpellInput(text) {
  return typeof text === "string" ? text.trim() : "";
}

/**
 * Null when `text` (after `normalizeSpellInput`) has the form of a Recovery Spell; otherwise what is
 * wrong. The host checks the same rules again and whether the spell opens the vault.
 */
export function checkSpell(text) {
  const chars = [...normalizeSpellInput(text)];
  if (chars.length !== SPELL_LENGTH) return `${SPELL_NAME}は${SPELL_LENGTH}文字です（${chars.length}文字でした）。`;
  const bad = chars.findIndex((c) => !SPELL_ALPHABET.test(c));
  if (bad >= 0) return `${bad + 1}文字目に使えない文字があります。使える文字は英字（大文字・小文字）、数字、「-」「_」だけです（空白や全角文字は使えません）。`;
  if (!SPELL_LAST.includes(chars[SPELL_LENGTH - 1])) return "最後の文字が正しくありません。記録した呪文と照らし合わせてください。";
  return null;
}

/** True only when `typed` is exactly the spell shown (surrounding whitespace aside). */
export function spellMatches(shown, typed) {
  return typeof shown === "string" && shown.length === SPELL_LENGTH && normalizeSpellInput(typed) === shown;
}

/** Message for a spell the host refused (`recovery_spell_invalid` details). Never echoes the input. */
export function describeSpellProblem(response) {
  if (response?.reason === "length") return `${SPELL_NAME}は${SPELL_LENGTH}文字です（${Number(response.count) || 0}文字でした）。`;
  if (response?.reason === "character") return `${Number(response.position) || "?"}文字目に使えない文字があります。使える文字は英字（大文字・小文字）、数字、「-」「_」だけです。`;
  if (response?.reason === "non_canonical") return "最後の文字が正しくありません。記録した呪文と照らし合わせてください。";
  return `${SPELL_NAME}の形式が正しくありません。`;
}

/** Issue #51 Slice C: backup formats. A plain vault's normal backup is A; see `backupFormat`. */
export function backupFormat(mode, protection) {
  const encrypted = mode === "encrypted";
  if (protection === "password") return encrypted ? "D" : "C";
  return encrypted ? "B" : "A";
}

export const BACKUP_FORMAT_NOTE = {
  A: "形式A: 通常のSQLiteファイルとして保存します（暗号化なし）。プライバシー設定のコピーは含みません。",
  B: "形式B: 暗号化したまま1つのファイルに保存します。復元にはふっかつのじゅもんが必要です（このPCの鍵は不要）。作成時に、お手元の呪文でこのVaultを復旧できることを確認します。プライバシー設定のコピーを暗号化して含みます。",
  C: "形式C: バックアップ全体をパスワードで暗号化します。復元にはこのパスワードが必要です。元のVault自体は暗号化されません。プライバシー設定のコピーを含みます。",
  D: "形式D: 暗号化Vaultを、さらにパスワードで保護します。復元にはパスワードとふっかつのじゅもんの両方が必要です。作成時に、お手元の呪文でこのVaultを復旧できることを確認します。プライバシー設定のコピーを含みます。",
};

export const PASSWORD_MIN_CHARS = 8;

export const PASSWORD_NOTE =
  `パスワードは${PASSWORD_MIN_CHARS}文字以上にしてください。パスワードはどこにも保存しません。忘れると、このバックアップは復元できません（Memocampusは再発行できません）。`;

export const BACKUP_SPELL_NOTE =
  "暗号化Vaultのバックアップは、復元にふっかつのじゅもんが必要です。記録した呪文（43文字）を入力してください。このVaultを復旧できる呪文でなければ、バックアップは作成しません。呪文は保存しません。";

/** Null when the two entries make a usable backup password, otherwise what is wrong. */
export function checkBackupPassword(password, confirmation) {
  if (typeof password !== "string" || [...password].length < PASSWORD_MIN_CHARS) return `パスワードは${PASSWORD_MIN_CHARS}文字以上で入力してください。`;
  if (password !== confirmation) return "確認用のパスワードが一致しません。";
  return null;
}

export const RESTORE_FORMAT_TEXT = {
  A: "形式A（平文Vaultの通常バックアップ）。暗号化されていません。プライバシー設定のコピーは含まれません。",
  B: "形式B（暗号化Vaultの通常バックアップ）。復元にはふっかつのじゅもんが必要です。",
  C: "形式C（パスワード保護）。復元にはバックアップのパスワードが必要です。",
  D: "形式D（暗号化Vault＋パスワード保護）。復元にはパスワードとふっかつのじゅもんの両方が必要です。",
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

/** Issue #51 Slice D: converting the vault shown between plain and encrypted. */
export const CONVERT_TEXT = {
  encrypt: {
    title: "このVaultを暗号化する",
    note:
      "現在のVaultの内容を、新しい暗号化Vaultへコピーします。元のVaultは削除されません。" +
      "コピーの後に、新しいVaultの「ふっかつのじゅもん」を一度だけ表示します。記録して全文を入力し直すまで、変換は完了しません。" +
      "変換したVaultを使い始めるかどうかは、その後で選べます。",
    pick: "保存先を選んで暗号化…",
  },
  decrypt: {
    title: "このVaultの暗号化を解除する",
    note:
      "新しい平文Vaultを作成します。閲覧履歴や保存画像などが暗号化されずにディスクへ保存されます。元の暗号化Vaultは削除されません。" +
      "平文Vaultには、ふっかつのじゅもんはありません（元の暗号化Vaultの呪文は、元のVaultにだけ使えます）。",
    pick: "保存先を選んで暗号化を解除…",
  },
};

/** Which conversion the vault shown offers: by its mode. */
export function convertDirection(mode) {
  if (mode === "plain") return "encrypt";
  if (mode === "encrypted") return "decrypt";
  return null;
}

/** For the spell step of a conversion: the spell belongs to the new vault only. */
export const CONVERT_SPELL_NOTE =
  "これは新しい暗号化Vaultのふっかつのじゅもんです。以前に記録した呪文（ほかのVaultの呪文）では、このVaultを復旧できません。";

/** A converted vault (start or confirm answer), for the user's decision. */
export function describeConverted(converted) {
  const encrypt = converted?.direction === "encrypt";
  const lines = [`変換しました（まだ使用していません）: ${converted?.path ?? ""}`];
  if (converted?.sourcePath) lines.push(`変換元（そのまま残ります）: ${converted.sourcePath}`);
  lines.push(encrypt ? "新しい暗号化Vault（鍵はこのWindowsユーザー用に保存しました）" : "新しい平文Vault（暗号化されていません）");
  const counts = converted?.counts;
  if (counts && Number.isSafeInteger(counts.visits)) {
    lines.push(`検証済み: 閲覧 ${counts.visits.toLocaleString("ja-JP")}件・スナップショット ${counts.snapshots.toLocaleString("ja-JP")}件・画像 ${counts.images.toLocaleString("ja-JP")}件（変換元と全件一致）`);
  }
  lines.push(
    "「使い始める」を押すと、その時点までに変換元へ記録された内容も取り込んでから、このVaultをActive Vaultにします。MCPへの公開は未承認から始まります。",
    "「切り替えずに保存」を選ぶと、変換した時点の内容のまま残します（後で「既存のVaultを開く」で開けます。その後の記録は変換元にだけ残ります）。",
  );
  return lines.join("\n");
}

/** A conversion recorded by the host but not finished in this page (`status.conversionPending`). */
export function describeConversionPending(pending) {
  const what = pending?.direction === "decrypt" ? "暗号化を解除した平文Vault" : "暗号化した新しいVault";
  if (pending?.phase === "converted") {
    return `変換済みで、まだ使用していない${what}があります:\n${pending.path}\n使い始めるか、切り替えずに保存するか、破棄してください。`;
  }
  if (pending?.phase === "verified" && pending.direction === "encrypt") {
    return `ふっかつのじゅもんの確認前に中断された変換があります:\n${pending.path}\n呪文は再表示できないため、破棄してから、もう一度変換してください。元のVaultは変更していません。`;
  }
  if (pending?.phase === "verified") {
    return `変換済みで、まだ使用していない${what}があります:\n${pending.path}\n使い始めるか、切り替えずに保存するか、破棄してください。`;
  }
  return `中断された変換があります${pending?.path ? `:\n${pending.path}` : ""}\n途中のファイルを破棄してから、もう一度変換してください。元のVaultは変更していません。`;
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
