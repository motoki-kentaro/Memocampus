// Page UI text catalog. The current extension's locale is ja; keep new text out of UI logic.
import { formatFullDate } from "./format.js";
export const PAGE_TEXT = Object.freeze({
  tab: "Page", search: "保存したPageのURL・タイトルで検索", refresh: "更新", more: "さらに表示",
  bulk: "条件一致を一括削除", all: "すべてのPageを削除", open: "元のWebページを開く", openNote: "現在のWebページへ再訪問します。保存済みMHTMLの表示ではありません。",
  groupDelete: "URL全体を削除", eventDelete: "この保存履歴を削除", untitled: "（タイトルなし）",
  eventExport: "MHTMLを書き出す", exportNote: "この保存履歴のMHTML原本を、Windowsの保存ダイアログで選んだ場所へそのまま書き出します。",
  exporting: "保存ダイアログで保存先を選んでください。完了するまでPageの操作は待機します…", exportCancelled: "書き出しをキャンセルしました。ファイルは変更していません。",
  loading: "Pageを読み込んでいます…", empty: "一致する保存済みPageはありません。", initial: "Pageタブを開くと保存履歴を読み込みます。",
  scopeNote: "URLは完全一致でまとめます。検索はURL・タイトルの部分一致（英字は大文字小文字を区別せず、その他のUnicodeは区別）。検索中の履歴と一括削除は一致する保存イベントのみが対象です。保存履歴ごとにMHTML原本をファイルへ書き出せます。本文検索・MHTML表示は未対応です。",
  newItems: "新しい保存があります。更新してください。", confirmTitle: "Pageの削除確認", cancel: "キャンセル", confirm: "確認した対象を削除",
  irreversible: "削除すると元に戻せません。共有されるMHTML原本は他の履歴が参照している限り残ります。バックアップには残る場合があります。",
  deleting: "削除を確定しています…", deleted: "確認した保存履歴を削除しました。", retry: "再読込", close: "閉じる",
});
const ERRORS = Object.freeze({
  vault_locked: "Vaultがロックされています。設定画面で状態を確認してください。",
  key_unavailable: "Vaultの鍵を使用できません。", vault_not_configured: "Vaultが未選択です。設定画面で作成・選択してください。",
  vault_switched: "Vaultが変更されました。表示と削除確認を無効化しました。更新して確認し直してください。",
  page_targets_changed: "削除対象または共有参照が変更されました。削除していません。再確認してください。",
  page_list_changed: "保存履歴が変更されたため続きは取得できません。更新してください。",
  invalid_cursor: "一覧の続きが無効・期限切れです。更新してください。", invalid_review: "削除確認が無効・期限切れです。確認し直してください。",
  page_not_found: "対象の保存履歴は既に存在しません。更新してください。", timeout: "Hostからの応答がありません。更新して再確認してください。",
  host_unavailable: "Native Hostに接続できません。", unknown_type: "Native HostがPage管理に対応していません。Hostを更新してください。",
  vault_schema_mismatch: "このVaultのスキーマには対応していません。旧Vaultは変更しません。",
  page_delete_unknown: "削除状態未確定です。自動再削除はしません。一覧を再読込して確認してください。",
  page_export_changed: "対象の保存履歴が変更・削除されたため、書き出しを中止しました。ファイルは変更していません。更新して確認してください。",
  page_export_unknown: "書き出し状態未確定です。自動では再実行しません。保存先のファイルを確認し、必要ならもう一度書き出してください。",
  page_blob_corrupt: "保存済みMHTML原本の検証に失敗したため、書き出していません。ファイルは変更していません。",
  export_file_appeared: "保存先に同名のファイルが作られたため、上書きせずに中止しました。",
  export_target_busy: "上書き先のファイルを他のプログラムが使用中のため、変更せずに中止しました。",
  export_target_readonly: "上書き先のファイルは読み取り専用のため、変更せずに中止しました。",
  export_invalid_target: "この保存先・ファイル名には書き出せません。別の場所や名前を選んでください。",
  parent_missing: "保存先のフォルダーが見つかりません。ファイルは作成していません。",
  export_failed: "書き出しに失敗しました。保存先のファイルは変更していません。",
  export_verify_failed: "書き出したファイルを検証できなかったため取り消しました。保存先は書き出し前の状態です。",
  export_recovery_needed: "書き出しを完了できませんでした。置き換え前のファイルは、同じフォルダーに「.memocampus-old」で終わる名前で残っています。保存先を確認してください。",
  invalid_query: "検索文字列は200文字以内で入力してください。", page_response_too_large: "表示データが大きすぎるため取得できません。",
});
export const pageErrorText = (code) => Object.hasOwn(ERRORS, code) ? ERRORS[code] : "Pageの処理に失敗しました。変更を成功扱いにせず、更新して状態を確認してください。";
export const pageDate = (ms) => { try { return formatFullDate(new Date(ms).toISOString()); } catch { return "—"; } };
export const pageBytes = (n) => `${Number(n).toLocaleString("ja-JP")} bytes`;
export const historyLabel = (item) => `保存履歴 ${item.matchedCount.toLocaleString("ja-JP")}件 / 全${item.eventCount.toLocaleString("ja-JP")}件`;
export const summaryText = (n) => `読み込み済み ${n.toLocaleString("ja-JP")} URL`;
export const exportedText = (result) => result.previousFileLeft
  ? `「${result.fileName}」にMHTML原本を書き出しました。置き換え前のファイルは削除できず、同じフォルダーに「.memocampus-old」で終わる名前で残っています。`
  : `「${result.fileName}」にMHTML原本を書き出しました。`;
export function deletionText(preview) {
  const { target, counts, event } = preview;
  const scope = target.scope === "event" ? `個別履歴: ${pageDate(event.savedAt)}\n${event.title}\n${event.url}`
    : target.scope === "url" ? `URL全体: ${target.url}` : target.query === "" ? "全Pageの保存イベントが対象です（フィルターなし）。" : `検索条件: ${target.query}\n画面の読み込み範囲ではなく、DB上の一致全件が対象です。`;
  return `${scope}\n保存イベント ${counts.events.toLocaleString("ja-JP")}件・URL ${counts.urls.toLocaleString("ja-JP")}件\n原本削除 ${counts.blobsRemoved.toLocaleString("ja-JP")}件・共有原本保持 ${counts.blobsKept.toLocaleString("ja-JP")}件\n削除原本 ${pageBytes(counts.bytesRemoved)}\n${PAGE_TEXT.irreversible}`;
}
