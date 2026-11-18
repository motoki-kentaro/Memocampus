# History, Slice 2: full-page UI (calendar, domain search, paging, URL state)

状態：実装完了候補。**総合PASSではない**（独立レビューとPOのChrome実機確認は未実施）。commit / push / PRなし。
ブランチ `feature/41-history`（Slice 1 コミット `5509a26` の上）。Native Host・Schema・MCP・VisibleStoreは変更していない（Slice 1のAPIをそのまま使う）。
スコープ外：行内展開の遷移詳細（Slice 3）、削除、永続キャッシュ。

## 1. 構成と責務（`extension/`）
| ファイル | 責務 |
| --- | --- |
| `history.html` / `history.css` | 画面（左：カレンダー・ドメイン、右：選択日・件数・サイズ・一覧・ページャ）。インラインのscript・styleなし。幅720px以下は縦積み。明暗・forced-colors対応 |
| `history.js` | 起動のみ（`chromeNativeTransport` と `chrome.tabs` を `createHistoryApp` に渡す） |
| `history-state.js` | URLパラメータの解釈・補正・生成、ページ番号の列（純関数） |
| `history-client.js` | Host API（`host_capabilities`／`history_list`／`history_days`／`history_visit`）。`callHost`（requestId照合）を再利用し、応答を検証 |
| `history-view.js` | カレンダー、行、ページャの描画（`createElement`／`textContent`のみ） |
| `history-app.js` | 制御：状態、URL同期、2つの要求系列、Vault整合、イベント |
| `sidepanel.html/js` | 「History」ボタン（`chrome.tabs.create({url: chrome.runtime.getURL("history.html")})`、二重クリック防止） |

Manifestは変更なし（`tabs` 権限は既存。拡張内ページは追加の公開設定が不要）。新しい権限・通信方式・フレームワークなし。

## 2. URLパラメータ（正本はURL）
| Key | 意味 | 省略・不正 |
| --- | --- | --- |
| `date` | 選択日 `YYYY-MM-DD`（実在日。1970〜9998年） | 今日。`2026-02-30`・`2026-13-01`・`1969-12-31`・形式違いは補正 |
| `domain` | 入力文字列（256文字まで。超過は切って補正） | 空（全ドメイン） |
| `mode` | `exact`／`regex` | `exact` |
| `page` | 1始まりの整数（9桁まで）。0・負数・小数・文字列・長すぎる値は補正 | 1 |
| `size` | 25／50／100 | 50 |

- 書き出し：`date` は常に、他は既定と違うときだけ（`URLSearchParams`でエンコード。`(^|\.)github\.com$` も壊れない）。
- 初回：URLが正しければ書き換えない。不正値の補正は**`replaceState` 1回**。未来の日付は有効（空表示）。
- **`pushState`**：日付選択、ページ移動、ページサイズ、照合モード。**`replaceState`**：ドメイン入力中（1キーごとに履歴を増やさない）、補正、範囲外ページの最終ページへの補正。
  **何もしない**：カレンダーの月送り、タブ再表示時の再取得。**`popstate`**：URLから状態を読み直し、入力欄・カレンダーの月（選択日の月）を復元する。
- 日付選択・ドメイン・モード・サイズの変更は `page=1` に戻す。

## 3. カレンダー
- 常設、日曜始まり、`YYYY年M月`、前月／翌月、日付クリックで選択。曜日配置は `visit-calendar.js` の既存関数（`calendarWeeks`、`monthBounds`、`localDayBounds`＝`new Date(y, m, d)` と翌日の開始。24時間加算なし）。
- **表示月と選択日は別状態**：月送りは表示月だけを変え、URL・一覧・選択日に触れない。選択日が表示月の外なら選択の印は出さず、「選択中の日の月へ戻る」を出す。
- 今日は枠線、選択日は塗りつぶし（両方ありうる）。選択は `aria-pressed`、今日は `aria-current="date"`、`aria-label` は「11月17日（火）閲覧4件、今日、選択中」。
- 記録のある日は点（`aria-hidden`）＋太字＋ラベルの「閲覧N件」（色だけに頼らない）。`history_days` の実データ。現在のドメイン条件に一致する訪問だけが対象。
- キーボード：矢印（日・週）、Home／End、Enter。roving tabindex。マークの更新はその場で行い、フォーカスは失われない。
- 前月／翌月は選択日と今年+1年の範囲。

## 4. ドメイン検索
- 入力欄（placeholder「未入力ならすべてのドメイン」）と、完全一致／正規表現の切替。例を `domain-hint` に表示。ワイルドカードなし。
- 入力は約350msのdebounce（Enterで即時）。1キーごとに要求しない。古い要求の応答は破棄する。
- 正規表現の正否は**Hostが判定**（JSの `RegExp` では判定しない）。`invalid_regex`／`unsupported_regex`／`regex_limit`／`regex_too_long`／`invalid_domain` は入力欄の下に表示（`role="alert"`、`aria-invalid`）。
  そのとき**一覧とマークは空**（古い結果を新しい条件の結果に見せない。全件へのフォールバックなし）。

## 5. 一覧とページング
- 1訪問1行（同じURLも別行）、並びはHost（`visited_at DESC, event_id DESC`）、時刻はローカル `HH:MM:SS`、タイトル（空は「（タイトルなし）」）、ホスト名、URL（2行まで）。各行は `data-visit-id` を持つ（Slice 3用）。展開ボタンは**置いていない**。
- タイトルは、完全なhttp(s) URLなら `<a target="_blank" rel="noopener noreferrer">`。**切り詰められたURLはリンクにせず**、ボタンで `history_visit` から完全なURLを取得して新しいタブで開く。
  取得できない・http(s)でない場合は開かない（状態行に表示）。`javascript:`・`chrome:`・不正なURLは無効表示。
- ページャ：`« ‹ 1 … 6 7 [8] 9 10 … 20 › »`（全ページは並べない）、現在は `aria-current="page"`、不可能な移動は `disabled`。0件は表示しない。ページ移動後のフォーカスは現在ページのボタンへ。
- 範囲外ページ（`pages > 0` かつ要求ページ > pages）は最終ページへ補正（`replaceState`、補正は3回まで）。`pages = 0` は空状態（「この日の訪問履歴はありません」／検索中は「条件に一致する訪問はありません」）。

## 6. Host通信・Vault
- 起動時に `host_capabilities` で `history_v1` を確認。無い（または `unknown_type` の旧Host）なら「対応していません」と表示し、API呼び出しはしない。再読み込みボタンで再確認。
- 一覧（`history_list`）と月間マーク（`history_days`）は**別の最新性ゲート**（`createLatestGate`）。日付・ドメイン・モード・ページ・月・Vaultの変更後に届いた古い応答は採用しない。
- すべての成功応答の `vaultToken` を検証。最初に採用した値と異なる応答が来たら、表示中の一覧・マークを破棄して両方を読み直す（別Vaultを混在させない）。連続4回を超えると停止して再読み込みを促す。
- タブの `focus`／`visibilitychange`（表示）で再取得（非表示中は何もしない、1秒に1回まで）。常時ポーリングなし。
- エラー：未設定／ファイルなし／スキーマ不一致は「Vault設定を開く」付き、ロック中、接続失敗、タイムアウト、`response_too_large`（案内付き）、不正な応答（Hostの契約違反）。いずれも再読み込みボタン。**自動再試行はしない。**

## 7. Privacy
Web由来のタイトル・URL・ホスト名・入力は `textContent` と属性だけで描画（`innerHTML` 系の不使用をテストで検査）。History専用の保存・キャッシュはない。削除操作は置かない。
検索条件はURLに残り、Chromeの履歴やブックマークに残る可能性がある（PO採用の仕様）。追加の永続化・外部送信はしない。

## 8. テスト（2026-11-18）
- `tests/extension/history.test.js`（47件）：URL状態（既定・復元・不正日付／モード／page／size・エンコード・ページ番号）、初回表示、1訪問1行、補正の `replaceState`、カレンダー（曜日配置・28〜31日・うるう年・マーク・今日と選択日・月送り・古い月の応答・キーボード）、
  日付選択、debounce、Enter、モード、不正な正規表現、空状態、ページング（隣接・無効化・フォーカス・サイズ・範囲外補正）、戻る／進む、再読み込み、古い応答、Vault混在防止・再取得・暴走停止、能力確認・未対応・接続失敗・各Vaultエラー・サイズ超過・契約違反、
  URLを開く（切り詰め・危険なスキーム）、HTML注入、`history.html` と `history.css` の静的検査、Side Panelからの起動。
- `tests/extension/history-fixtures.js`：Slice 1の契約を守る疑似Host、`window`（実際の履歴スタック）、手動タイマー。
- `tests/extension/history-real.test.js`（opt-in：`MEMOCAMPUS_TEST_HOST`）：実Hostと合成Vaultで、一覧・マーク・完全一致・正規表現・Host拒否・範囲外補正を確認。
- JS全件・Schema 9の実Host＋MCP指定は完了報告に記載。Rustは未変更のため再実行していない。

## 9. 未実施・既知の制限
- Slice 2実装時の自動テストはfake DOMだった。その後POのChrome UI受入れは合格済み。Chrome＋実Hostでの全異常タイミングの有人検証は未実施であり、UI受入れと独立コードレビューを区別する。
- 月間集計は索引なしのため、100万件規模では月移動に約1〜1.4秒（Slice 1の実測）。
- 通知にはない：Vault切替のプッシュ検知（フォーカス復帰時の再取得で気づく）。
- 一度に数十万件ある1日にドメインを付けると、Host側でその日の全行を走査する（Slice 1の既知事項）。

## 10. Slice 3への引き継ぎ
- 行は `li.visit-row[data-visit-id]`、本文は `.visit-body`（タイトル・メタ）。展開領域を行に追加し、`history-client.js` の `historyVisit` を詳細（遷移元3状態・遷移先）に拡張する。
- `history_visit` は完全なURL・遷移・`pageMetadata` を返す（Slice 1）。巨大な値は `response_too_large`。
- 行内展開は暫定UI（PO実機確認後に変更の可能性）。`history-view.js` の `renderRows` にだけ手を入れる構成にしてある。

## 11. 独立レビュー結果と修正（2026-11-18）

**初回独立レビューの結果は FAIL（Medium 2件、Low 1件）だった。** この事実は残す。POによるChrome UI受入れは合格済み。以下の修正後、限定独立再レビューは **PASS WITH NOTES**（HISTORY-01／02／03解消確認）。その後のSlice 3＋UI最終レビューFAILとHISTORY-04／05／06の修正・再レビュー待ちは `history-slice-3.md` §12を参照。

| ID | 重要度 | 原因 | 修正 |
|---|---|---|---|
| FINDING-HISTORY-01 | Medium | ページ範囲補正の `syncInputs()` が、debounce待ちのドメイン入力を確定済みstateの値で上書きした。入力直後のサイズ変更も同様に、確定していない入力を落とした。 | 入力欄を「編集中の値」、`state.domain` を「確定済みの値」として分けた。`typingTimer` がある間は `syncInputs()` が入力欄を書き換えない（範囲補正・Hostエラーでは触らない）。日付選択・サイズ・モード・ページ移動は、先に `withDraft()` で編集中のドメインを確定してから適用する（確定時は `page=1`、ページ移動はクリックされたページを適用）。`popstate` はdebounceを取り消してURLから復元。月送りは入力を保持。入力確定は従来どおり `replaceState`。 |
| FINDING-HISTORY-02 | Medium | 完全URL取得（`openCut`）が、要求開始時の画面状態に結び付いていなかった。一覧が変わっても、`vaultToken === null` の間は照合が省略されて、旧行のURLが開いた。 | `openCut` に操作コンテキスト（訪問ID・一覧世代 `listEpoch`・要求開始時のVault token）を持たせた。一覧の読み込み・置換・エラー、状態確定、`popstate`、Vault不一致の検知で世代を進める。開く直前に、訪問ID一致・応答tokenと開始時tokenの一致・世代一致・現在のtokenが既知かつ開始時と一致・行が現在の一覧にある・URLがhttp(s)、をすべて確認し、満たさなければタブを開かず破棄する。token不明の間はそもそも開始しない。再試行はしない。 |
| FINDING-HISTORY-03 | Low | 旧Hostの `{ok:false, eventId:null, error:"unknown_type"}` は `requestId`・typeを持たないため、`callHost` が先に不正応答として扱った。 | `history-client.js` の `hostSupportsHistory` だけで、この**形状に完全一致**する応答（`ok:false`・`eventId:null`・`error:"unknown_type"`・他のキーなし）を未対応と判定する。`callHost` と共通契約は変更なし。他の欠落・不一致は従来どおり不正応答。 |

追加した回帰テスト（`tests/extension/history-review-fixes.test.js`、16件）：
- HISTORY-01：レビューの再現手順（`page=99` の旧応答を保留 → 入力 → debounce前に解放）、サイズ／日付／モード／ページ移動、月送り、`popstate`、Hostエラー、確定後の旧応答、`page=1`、履歴エントリが増えないこと。
- HISTORY-02：一覧変更、Vault切替（token不明の間に旧Aの応答を解放）、ドメイン・ページ・サイズ・モード・`popstate`・再取得、同じIDの再描画、訪問ID不一致・Vault token不一致・token欠落・不正スキーム、二重クリックで1回のみ開くこと。
- HISTORY-03：旧Host形状、正式な能力応答、`history_v1` なし、形状が少しでも違う応答、`requestId`／type不一致、`ok:true` で能力なし、`history_list` が旧形状を受理しないこと、旧Hostで履歴APIを呼ばず自動再試行もしないこと。
- 修正の一部を外すと6件が失敗することを確認した（修正前FAIL・修正後PASS）。
- テスト用に `history-fixtures.js` へ応答を書き換える `mutate` を追加（本番コードの通信契約は弱めていない）。

検証：JS通常全件 1069件（1054成功・15 skip・0失敗）。実Host＋MCP指定（debug Host、`mcp-server` のrelease MCP）で 1069件成功・0 skip。実Hostの結合テストは変更なしで通った。`git diff --check` は問題なし。Host・Schema 9・MCP・VisibleStoreは未変更。

未実施：Chrome＋実Hostでの全異常タイミング（遅延応答・Vault切替・入力中の操作）の有人検証。上記の限定独立再レビューは完了済みであり、POのUI受入れとは別の判定として扱う。
