# Issue #41 — History Slice 3: 訪問詳細・遷移元／遷移先の行内展開

状態：POによるChrome UIドッグフードは合格済み。最終独立レビュー（Codex）は **FAIL（Medium 2件、Low 1件）**。HISTORY-04／05／06の修正とセルフ検証は完了し、**修正後の独立差分再レビュー待ち**（総合PASSとは判定しない）。Host API・Schema 9・Privacy Policy・MCP・VisibleStore・Manifestは変更なし。

## 1. UI構成
- 各訪問行（`li.visit-row[data-visit-id]`、1訪問1行は不変）に「詳細を表示／詳細を閉じる」ボタン（`.visit-toggle`、`aria-expanded`、`aria-controls`＝行の詳細領域）。タイトルのリンクとは別ボタン。初期はすべて折りたたみ。
- 詳細領域は行の直下（`section.visit-detail`、`id="visit-detail-<id>"`）。DOM順・視覚順・通常のTab順を **遷移元／訪問情報／遷移先／取得したページ情報**（あるときだけ）に統一。訪問情報は日時・完全タイトル・完全URL・記録された遷移の種類を表示する。CSS `order` や正の `tabindex` による補正は使わない。
- 複数行を同時に開ける。各行が自分の取得世代を持つ。永続キャッシュなし（閉じると中身を捨て、開き直すと再取得）。
- URLは折り返し、遷移先リストは領域内でスクロール（`max-height`）。狭い幅でも横スクロールなし。明暗・forced-colorsは既存トークンのまま。

## 2. 表示モデル（`history-client.js` の `historyVisit`）
`{ id, url, vaultToken, visit{id,url,title,visitedAt,host}, navigation|null, source, targets[], targetCount, targetsTruncated, pageMetadata|null }`。
実コード（`native-host/src/history.rs` の `run_visit`）を正とした。Slice 1の文書と差異なし。
- `source.state` は `present`（遷移元の訪問を同梱）／`censored`／`unknown` の3つだけ。それ以外は `invalid_response`（別の状態に解釈しない）。`censored`・`unknown` は状態以外を**保持しない**（Hostが余分な値を付けても捨てる）。
- `targets` は Host の順（古い順）。各項目に `kind`・`transitionType`・`occurredAt`（文字列のみ）。IDを保持（将来の内部ナビゲーション用。今回はジャンプなし・URLパラメータ追加なし）。
- `targetCount` は総数、`targetsTruncated` は切り詰め。`targetCount >= targets.length` でなければ不正。

## 3. 遷移元の3状態
| state | 表示 |
|---|---|
| present | 遷移元のタイトル・URL・ホスト・訪問日時（URLが切り詰められていればテキストのみでリンクにしない） |
| censored | `[CENSORED]` と「遷移元の閲覧履歴は削除されています」。URL・タイトル・IDは出さない |
| unknown | 「遷移元：不明」＋「記録されていないか、確定できませんでした」。直接アクセスとは言わない。リンクなし |
削除済みの遷移元はHost側で `unknown`（Hostが `source_censored` を立てた場合のみ `censored`）。UIはそれを推測・復元しない。

## 4. 遷移先
- 0件：「記録された遷移先はありません」（どこにも移動しなかったとは言わない）。
- 件数は受信値：`遷移先 254件（先頭200件を表示）`（`targetsTruncated` のとき。表示件数は受信した `targets.length`。200固定ではない）。
- 近い時刻・似たURLからの補完はしない。

## 5. pageMetadata
`page_metadata.event_id` がこの訪問そのものの場合だけHostが返す。ある場合のみ「取得したページ情報」カードを出し、「この訪問に紐づく取得（取得日時）。訪問時点の内容とは限りません」と明記。画像URL・ファビコンURLは**テキスト表示のみ**（読み込まない）。`tab_snapshots` は使わない。

## 6. 非同期の世代・Vault照合（`history-app.js`）
開くたびに文脈 `{id, ui, epoch(listEpoch), token(vaultToken)}` を作る（同じIDでも開くごとに別）。応答は次をすべて満たすときだけ描画する：応答の訪問ID＝要求ID／応答のVault token＝開始時token／現在のtokenが既知で開始時と一致／一覧世代が同じ／行が現在の一覧にあり同じ行要素／その文脈がまだ開いている（`expanded` が同じ文脈）。満たさなければ**何も描かず破棄**（エラー表示にもしない）。Vault不一致の応答が来て文脈が現役なら、`accept()` で再読み込み（Vault切替の検知）。
無効化：日付・ドメイン・モード・ページ・サイズ・`popstate`・一覧の再取得／置換・一覧エラー・Vault切替で世代が進み、新しい一覧の描画で `expanded` も空になる（同じIDが再描画されても旧詳細は再利用しない）。詳細を閉じる、または読み込み中に再クリック（＝閉じる）でその文脈は消える。
**画面を再表示したときの再取得（`focus`）**：詳細が開いている間は一覧を読み直さない（読み直すと全部閉じるため）。月間マークは読み直すので、Vault切替の検知は残る。

## 7. エラー
`visit_not_found`（削除された可能性）、`response_too_large`、不正応答等は詳細領域内（`role="alert"`）で表示し、一覧を維持する。`[CENSORED]`・不明とは別表示。行の「再読み込み」は現在の世代・tokenで1回だけ再取得（自動再試行なし）。URLを自動で開かない。

HISTORY-04修正後、現在有効な要求のVault利用不能エラーは、token・一覧世代・一覧／月間の要求ゲート・保留中のURL取得／詳細取得・展開内容・一覧・月間マーカーを失効させ、画面全体に理由と再読み込み操作を出す。対象は `vault_locked`、`vault_not_configured`、`vault_schema_mismatch` に加え、Host `VaultError::code` で確認した `vault_missing`、`vault_invalid`、`config_invalid`、`app_dir_unavailable`、`vault_switched`、`key_unavailable`、`vault_decrypt_failed`、`vault_mode_mismatch`、`vault_encryption_unsupported`、`vault_legacy_unsupported`。

適用先は一覧・月間・行内詳細・完全URL取得（能力確認にも防御的に適用。現行Hostの能力確認はVault非依存）。処理前に要求ゲート／行のコンテキスト／能力確認の実行番号を照合し、古い要求のエラーは破棄する。失効自身は再取得しない。利用可能になればfocus復帰または既存の再読み込みで新しい応答を取得する。

`timeout`／`host_unavailable` はVault切替確定とはしない。月間・詳細・完全URL取得で現在有効な通信障害を受けた場合、保留中の詳細とURLオープンは取り消すが、既知tokenや取得済みの詳細は保持する。取り消された詳細は行内エラーから明示再試行できる。`vault_busy`／`db_error` 等も利用不能状態と混同しない。

## 8. URL安全性・DOM
遷移元・遷移先・訪問のURLは `http(s)` だけリンク（新規タブ・`noopener noreferrer`）。それ以外・切り詰め済みはテキストのみ。すべて `createElement`／`textContent`／属性で描画（`innerHTML` 不使用をテストで検査）。詳細からの書き込み・削除APIなし。

## 9. テスト（2026-11-18）
- `tests/extension/history-detail.test.js`（20件）：開閉・再展開・複数行、3状態（`censored` に識別情報を出さない・不明を直接アクセスとしない）、不正な `source.state`、遷移先0件／複数／上限（2・3で確認）、`pageMetadata` あり／なし、HTML注入・不正スキーム、再表示で閉じない、競合（別日・ドメイン・モード・ページ・サイズ・Back/Forward・同じID再描画・Vault切替でtoken不明・別訪問／別Vaultの応答・閉じて即再展開で逆順解放・複数行の逆順解放・読み込み中の再クリック）、エラー各種と再試行・閉じた後の再試行、クライアントの契約検査、CSS／`innerHTML` 静的検査。
- `tests/extension/history-detail-real.test.js`（実Host・合成Vault・opt-in）：present／`[CENSORED]`／unknown／複数遷移先／削除された遷移元が復元されない。
- 既存の `history-fixtures.js` に `relations`・`targetLimit` を追加。Slice 2のテストは1件だけ更新（展開ボタンが「まだない」→「閉じた状態である」）。
- 結果は完了報告に記載。Rustは未変更のため再実行していない。

## 10. 受入れと未確認事項
- POのChrome UIドッグフードは合格済み。今回の修正後にPOの有人確認を再実施したわけではない。
- Slice 2のHISTORY-01／02／03は限定独立再レビュー **PASS WITH NOTES**。今回も既存回帰テストを実行した。
- 最終独立レビューのFAIL記録と今回の修正は§12に残す。今回のセルフ検証は独立レビューの代わりではない。
- ChromeでのNative Messaging切替・遅延応答競合は、今回は疑似Host／合成Vaultのテストで検証した。有人Chrome＋実Hostでの全異常タイミング、スクリーンリーダーでの確認は未実施。
- 展開中は同一Vaultでの削除等が表示へ即時反映されないことがある。Vaultの即時プッシュ通知もない。これらは既知の表示鮮度制約であり、Vault利用不能の明示エラーを無視してよいという意味ではない。

## 11. 次の改善候補
遷移先・遷移元から該当訪問へのジャンプ（URL仕様の変更が必要）、遷移先が上限を超えるときの続き取得（Host拡張が必要）、行全体のクリック展開、展開状態の保持（一覧再読み込みをまたぐ）、Host側ターゲットのページング。

## 12. 最終独立レビュー指摘修正（2026-11-18）

最終独立レビューはCodexによる **FAIL（Medium 2件、Low 1件）** だった。過去の判定は消さない。

| ID | 原因 | 修正と検証 |
|---|---|---|
| HISTORY-04 / Medium | 展開中のfocusは月間要求だけを送り、そのVault状態エラーが旧token／詳細文脈を失効させなかった | 最新性チェック後に共通失効処理を適用。list／days／detail／full URL／capabilities、3指定エラーと正式な同等エラー、旧エラー、回復、連続失効、通信障害を回帰検証 |
| HISTORY-05 / Medium | `.calendar-months` の `display:grid` が `hidden` を上書き。日表示中のクリックが `pickerYear=null` を使用した | `[hidden] { display:none }` の専用規則と、monthsビュー・整数年・範囲・月番号の入口検証を追加。null／NaN／不正型・範囲外は変化なし。実Chromeのcomputed styleも検証 |
| HISTORY-06 / Low | CSS orderだけで視覚順を変更し、DOM／Tab順は現在→遷移元だった | DOM自体を遷移元→現在→遷移先→ページ情報へ変更し、orderを削除。カードを役割で識別するテストと、実ChromeのTab入力で確認 |

追加：`tests/extension/history-final-fixes.test.js` **32件**。既存詳細テストからVaultエラーの「行内エラーのまま一覧保持」という旧期待を分離し、新しい全体失効テストへ移した（skipや期待値緩和で隠していない）。

セルフ検証結果：
- History関連：**117成功・2 skip・0失敗**（基準85成功＋32件）。
- 通常JS：**1,108成功・16 skip・0失敗**（基準1,076成功＋32件）。
- 実Host＋MCP：**1,124成功・0 skip・0失敗**（基準1,092成功＋32件）。最新の既存debug Hostと `mcp-server/target/release/memocampus-mcp.exe` を指定し、一時APP_DIR・合成Schema 9 Vaultを使用。Releaseの上書きなし。
- 過去レビューの再現用プローブも、今回は修正担当のセルフ検証として再実行した：Slice 3／境界32項目、HISTORY-01／02／03の46項目がすべて成功。前回失敗した3エラーの観測は `tokenStillA:false / oldDetailVisible:false / rowCount:0`。新たな独立レビューとは扱わない。
- `node scripts/verify-history-review-fixes.mjs`：隔離headless Chrome 157.0.8103.39で **8項目成功**。実HTML／CSS／JS＋疑似Host、初期hidden/computed display/可視性、誤クリック拒否、年月／Escape、実Tab順、明暗の黄色／赤・720/1280pxの横スクロールなし、forced-colors、reduced-motionを確認。一時プロファイルは終了後に削除。実ユーザー環境・Native Messaging登録は使わない。
- 修正途中に旧「ロック時は設定ボタンなし」のテストが1件失敗した。設定ボタンの既存契約を保持するよう実装を修正し、再実行と全件検証が成功した。失敗をskipしていない。
- `git diff --check` 実行。Host／Schema／MCP／VisibleStore／Policy／Manifestは未変更。

再現コマンド：`node --test tests/extension/history-final-fixes.test.js`、`node --test "tests/extension/history*.test.js"`、`npm test`、`node scripts/verify-history-review-fixes.mjs [Chrome実行ファイル]`。

**修正後の独立差分再レビューは未実施。セルフ検証を独立PASSとせず、再レビュー待ちで停止する。commit／push／PRは行わない。**
