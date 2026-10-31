// Display text for queue results. "Queued" and "stored in SQLite" are always reported separately.

export function describeRecordResult(result) {
  if (!result?.ok) return `記録に失敗しました。\n${result?.error || "保存に失敗しました。"}`;
  if (result.queued === 0) return "記録できる通常ウィンドウのタブ（http/https）がありません。";
  if (result.stored === result.queued) return `${result.stored}件のタブをSQLiteに保存しました。タブは閉じていません。`;
  const lines = [`${result.queued}件のタブを送信キューに登録しました。タブは閉じていません。`, `SQLiteに保存済み: ${result.stored}件`];
  if (result.pending > 0) {
    lines.push(`未保存（送信待ち・自動で再送）: ${result.pending}件`);
    if (result.reason) lines.push(`理由: ${result.reason}`);
  }
  if (result.deadLettered > 0) lines.push(`記録できず隔離: ${result.deadLettered}件`);
  return lines.join("\n");
}

/** The policy state for the side panel, or "" when automatic recording is running. */
export function describePolicyStatus(status) {
  if (status?.ok !== true) return "プライバシー設定の状態を確認できないため、自動記録が止まっている可能性があります。";
  return status.state === "ready" ? "" : status.message || "自動記録を停止しています。";
}

/** One line for the side panel, or "" when nothing needs attention. */
export function describeQueueStatus(status) {
  if (!status?.ok) return "";
  const parts = [];
  if (status.pending > 0) parts.push(`送信待ち ${status.pending}件${status.blockedReason ? `（${status.blockedReason}）` : ""}`);
  if (status.deadLetters > 0) parts.push(`記録できず隔離したイベント ${status.deadLetters}件`);
  if (status.dropped > 0) parts.push(`キューに入れられず破棄したイベント ${status.dropped}件`);
  return parts.join("\n");
}
