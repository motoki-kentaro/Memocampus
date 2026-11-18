# History, Slice 1: Host read API (Schema 9 kept)

状態：実装完了候補。**総合PASSではない**（独立レビュー・PO実機確認は未実施）。ブランチ変更・commit・push・PRなし。
今回のスコープ外：フルページUI、カレンダーDOM、URLパラメータ同期、Side Panel導線（Slice 2以降）。

## 1. 決定と理由
PO決定：**Schema 9を維持し、インデックスを追加しない。** `schema.rs`・`SCHEMA_VERSION`（9）・既存テーブル・索引は未変更
（`git diff` に `schema.rs` なし。Schemaの全テスト通過）。Schema 10にすると旧Vaultが開けなくなる（移行しない方針）ため。
`visits` には `visited_at` の索引がなく、ドメイン列もない。したがって日付範囲の走査（全表走査）で候補を取り、ホスト名はRust側で照合する。
永続コピー・新テーブル・キャッシュは作らない。読み取りは書き込まない（テストで `total_changes` 不変を確認）。

## 2. 契約（`native-host/src/history.rs`、能力 `history_v1`）
共通：`requestId` 必須（なければ `invalid_request_id`）。成功は `{ok:true, type, requestId, vaultToken, …}`、失敗は `{ok:false, type, requestId, error}`。
`type` は `history_list_result` / `history_days_result` / `history_visit_result`。要求の検査（日付・ドメイン・正規表現・page・size・id）は**Vaultを開く前**に行う。
Vaultの1回の解決（`Session::read_with_token`）で、トークンとデータは同じVaultのもの。複数SQLは1つの読み取りトランザクション（同一スナップショット）で実行する。

### `history_list`
要求：`{visitedFrom, visitedTo, domain?, mode?, page?, size?}`。応答：`{items, total, page, size, pages}`。
- `visitedFrom <= visited_at < visitedTo`。`visit_days` と同じ検証（厳密な `toISOString()` 形式、増加、32日以内）に加え、**26時間以内**（ローカル1日は23〜25時間）。違反は `invalid_bounds`。
- `size` は 25 / 50（既定）/ 100、`page` は1始まりの整数（既定1）。違反は `invalid_size` / `invalid_page`。範囲外のページは**エラーではなく空の `items`**（`total`・`pages` は正確）。
- `total`＝条件に合う訪問数、`pages = ceil(total/size)`（0件なら0）。並び：`visited_at DESC, event_id DESC`。1訪問＝1件（同一URLも別行）。`tab_snapshots` は含めない。
- item：`{id, visitedAt, url, title, host, urlTruncated, titleTruncated}`。URLは2048文字、タイトルは512文字まで（文字境界で切る）。
- 応答サイズ：項目のシリアライズ後の実バイト数（JSONエスケープ込み）の合計が700,000を超えたら、件数を黙って減らさず `response_too_large`（`total`・`pages` の契約を守るため）。

### `history_days`
要求：`{bounds[29..32], domain?, mode?}`（`visit_days` と同じ検証）。応答：`{counts[], total}`。`counts[i]` は `[bounds[i], bounds[i+1])` の、同じ条件の訪問数。
**同じ条件・同じ境界なら `history_list.total` と一致する**（同じ有効日時判定・同じホスト抽出・同じ正規表現。性質テストで検証）。

### `history_visit`
要求：`{id}`（`navigation::is_visit_id`）。応答：`{visit{id,visitedAt,url,title,host}, navigation, source, targets[], targetCount, targetsTruncated, pageMetadata}`。
- `visit` のURL・タイトルは**全文**。全体が予算を超える場合は切らずに `response_too_large`（チャンク転送は未実装）。
- `navigation`：その訪問の `navigation_events` 行（`kind, transitionType, clientRedirect, serverRedirect, forwardBack, fromAddressBar, newTab, occurredAt`）。行がなければ `null`。
- `source.state`：`present`（`visit` に遷移元の訪問を同梱）／`censored`（`source_censored = 1`。それ以外は何も分からず、URL・タイトル・IDは返さない）／`unknown`
  （行がない、またはNULLでマーカーなし、または確定できない）。**NULLだけでは `censored` にしない。`unknown` を「直接アクセス」とは言わない。**
- `targets`：この訪問を遷移元とする `navigation_events` すべて（古い順、最大200件。`targetCount` は総数、`targetsTruncated` で切り詰めを示す）。URL・タイトルは一覧と同じ上限。
- `pageMetadata`：`page_metadata.event_id` がこの訪問そのものの場合のみ。URL単位の最新情報を別の訪問に付けない。`tab_snapshots` は混ぜない（訪問と確実に結べない）。
- 存在しない／削除済み／有効な日時でない訪問は `visit_not_found`。不正なidは `invalid_id`。

### エラーコード
`invalid_request_id`, `unknown_type`, `invalid_bounds`, `invalid_page`, `invalid_size`, `invalid_mode`, `invalid_domain`, `invalid_regex`, `unsupported_regex`, `regex_limit`,
`regex_too_long`, `invalid_id`, `visit_not_found`, `response_too_large`, `db_error`、および既存のVaultコード（`vault_not_configured`, `vault_locked`, `vault_schema_mismatch` など）。SQLiteの文言は出さない。

## 3. 規則
- **有効な訪問日時**：`visit_days::VALID_VISITED_AT`（厳密なUTC瞬間）の行だけが一覧・集計・詳細の対象。辞書順だけでは混入させない。
- **ドメイン**（対象はホスト名のみ。URL全体・パス・クエリには照合しない）：空白は全ホスト。ホスト名は `url` クレートの `host_str()` を小文字化し末尾ドットを除いたもの（`policy.rs` と同じ。IDNはpunycode）。
  ホスト名のないURLは空でないフィルターに合致しない。
  - `exact`（既定）：ホスト名全体の一致、大文字小文字無視、入力の前後空白・末尾ドットは除く、IDN入力はpunycodeにする。スキーム・パス・クエリ・ポート・ユーザー情報・空白を含む入力は `invalid_domain`。サブドメインは含まれない。
  - `regex`：Rustの `regex`（線形時間、先読み・後方参照なし）、ホスト名への部分一致（全体一致は `^…$`）、大文字小文字無視。パターンは512バイトまで（超過は `regex_too_long`）、コンパイル後1MiB・DFA 256KiB（超過は `regex_limit`）、
    構文エラーは `invalid_regex`、先読み・後方参照は `unsupported_regex`。**エラーを0件や全件表示にしない。** IDNはpunycodeのホスト名に対して照合される。
  - 絞り込みはページングの前。行は逐次処理し、保持するのは1ページ分と件数だけ。
- ワイルドカードモードはない。`mode` が `exact` / `regex` 以外なら `invalid_mode`。Policyの許可・禁止設定には触れない（Historyは自分のVaultの全記録を読む、URLタブと同じ）。
- Privacy：NeverSaveの新規保存なし、MCP/VisibleStoreへの影響なし、削除済みの遷移元のURL・タイトル・IDは返らない（テスト）。Web由来の文字列は表示側（Slice 2）で安全なDOM APIだけで表示する。

## 4. 検証（2026-11-18、合成データ・一時Vaultのみ）
- `native-host`：`history` の単体22件（`src/history/tests.rs`）、実Host stdio 2件（`tests/history_api.rs`）。内容：空・同一URL複数訪問・同時刻・境界（含む／含まない）・不正日時・ページング（25/50/100、先頭／中間／末尾／範囲外／極端値）・
  不正なpage/size/範囲・DST日（23/24/25時間）・完全一致（大小文字・サブドメイン・類似ドメイン・IDN・不正入力）・正規表現（部分／アンカー／エラー5種／512バイト境界）・ホストなし・
  月間と一覧の一致（東京・ニューヨーク・UTC、28/29/30/31日、うるう年、境界の前後1ms、不正日時、各フィルター）・削除済み訪問の除外・文字境界の切り詰め・JSONエスケープによる予算超過（`\u0001`×100件＝約1.5MB→`response_too_large`、25件ずつなら収まる）・
  10MBのURL・予算超過の詳細・遷移3状態・複数の遷移先（255件）・存在しないID・削除済み遷移元の痕跡なし・`page_metadata` と `tab_snapshots` の非混入・読み取りが書き込まないこと・requestId／vaultToken・Vaultを開く前の拒否。
- 既存の期待値を更新：`host_capabilities` に `history_v1`（`tests/stdio_protocol.rs`、`tests/extension/navigation.test.js`）。他の既存テストは変更・skip・緩和していない。

### 性能（`cargo test --release … measure_history_reads -- --ignored`、`MEMOCAMPUS_HISTORY_ROWS`）
Windows 11、Releaseビルド、Host関数の直接呼び出し（Hostプロセス起動は別に約11ms）、各5回の中央値。合成データ：3年分に分散し、3分の1が測定月に入る。
測定日＝測定月の15日目。測定月は1日目から30日分のUTC+9の日境界。

| 条件 | 10万件（測定日1,092件／測定月33,334件） | 100万件（測定日11,117件／測定月333,334件） |
| --- | --- | --- |
| 1 一覧＋総件数、ドメインなし（1ページ目／3ページ目） | 27 ms / 27 ms | 305 ms / 286 ms |
| 2 一覧＋総件数、完全一致 | 28 ms | 309 ms |
| 2b 一覧＋総件数、正規表現 | 32 ms | 319 ms |
| 0 月間集計、ドメインなし | 99 ms | 1,017 ms |
| 3 月間集計、完全一致 | 143 ms | 1,380 ms |
| 3b 月間集計、正規表現 | 142 ms | 1,416 ms |
| 詳細（ID指定、主キー） | <1 ms | <1 ms |

所見：索引がないため、いずれも日付範囲の走査が支配的（行数にほぼ比例）。正規表現は完全一致とほぼ同じ（ホスト抽出のコストが主）。
100万件・1か月に33万件の想定ではカレンダーの月移動が約1〜1.4秒。10万件規模では0.1秒台。**この結果を理由にSchema・索引は変更していない。** 改善案（PO判断）：
(a) `visits(visited_at, event_id)` の索引（Schema 10。旧Vaultは開けなくなる）、(b) 月間集計の拡張側キャッシュ／先読み（Slice 2）、(c) 現状維持（想定規模では実用範囲）。

## 5. 未解決・次Sliceへ
- 詳細が1MBに迫る巨大なURL／タイトルは `response_too_large`（チャンク転送は未実装）。一覧は切り詰め＋フラグ。
- 巨大な1日（数十万件）でのドメイン絞り込みはその日の全行を走査する。
- Vault切替・外部削除の検知は、応答の `vaultToken` とフォーカス時の再読み込み（Slice 2）に委ねる。
- 実機確認（Chrome）・独立レビューは未実施。既存の不安定テスト（`concurrent_config_writers…`）は今回再発しなかった。CPU負荷中のJS全件実行で、
  無関係な2件の時間依存テスト（疑似ホストの `settle` 待ち）が一度ずつ失敗し、単独・再実行では成功した（History変更とは無関係。原因は負荷下のタイミング）。
