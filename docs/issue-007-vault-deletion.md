# Issue #7 — URL / Image 削除と全Vault削除UIの統一

実装完了候補。**総合PASSではない**。POのChrome実機確認・独立レビューは未実施。
commit / push / PRなし。ブランチは `feature/7-vault-deletion`（`main` の `4f603a6` から作成）。

## 1. PO決定（Slice 0後）

1. **案A**：URLタブの既存行はURLグループのまま。展開すると、そのURLの個別閲覧イベント（`visits.event_id`）を表示する。
   イベント行の赤いゴミ箱（アイコンのみ）はそのイベント1件だけを削除する。URLグループの赤いゴミ箱＋「URL全件削除」は
   従来の `url_delete` と同じ範囲を削除する。
2. 条件一致の一括削除は**`visits` イベントのみ**。現在の検索・日付条件に一致するイベントが対象で、同じURLの条件不一致イベントと
   `tab_snapshots` は残す。条件なしでも対象は全 `visits`。
3. `tab_snapshots` は独立した記録種別。個別削除・一括削除には連動せず、URL全件削除だけが削除する。

## 2. 削除の単位

| 画面 | ボタン | 削除するもの |
| --- | --- | --- |
| URL・イベント行 | ゴミ箱のみ | `visits` 1行（そのイベントが持つ `page_metadata` 行と、そのイベント自身の `navigation_events` 行。他イベントから見た遷移元は `NULL`）|
| URL・グループ見出し | ゴミ箱＋「URL全件削除」 | そのURL文字列の全 `visits`・`tab_snapshots`・`page_metadata` と遷移記録（従来の `delete_url` と同じ）|
| URL・上部 | ゴミ箱＋「全閲覧履歴を削除」／「条件に一致する閲覧履歴を削除」 | 一致した `visits` だけ（スナップショット対象外）|
| Image・タイル右上 | ゴミ箱のみ | 画像1件＝同じバイト列（`blobId`+`sha256`）。その全保存履歴・原本・サムネイル・タグ付与と、他の画像に付いていないタグ |
| Image・上部 | ゴミ箱＋「全画像を削除」／「検索結果をすべて削除」 | 全画像、またはタグ絞り込みに一致する画像（ギャラリーの `image_tag_search` と同じ述語。新しい検索機能は追加していない）|
| Page・履歴行 | ゴミ箱のみ | 既存のまま（保存イベント1件）|
| Page・URL見出し | ゴミ箱＋「URL全件削除」 | 既存のまま |
| Page・上部 | ゴミ箱＋「全Pageを削除」／「検索結果をすべて削除」 | 既存のまま |

**条件一致の意味**：Hostは `search_pages` と同じ行テスト（`url` または `title` のLIKE、日付窓は同じ `visited_at` 範囲と有効性検査）を
`visits` の各行に適用する。件数は表示グループ数ではなくDB上の一致イベント数。テスト（`the_deletion_filter_and_the_url_search_mean_the_same_thing`）が、
11種の検索語（`%`、`_`、`\`、大小文字、日本語、前後空白を含む）×3種の日付窓で、削除対象のURL集合と `search_pages` が返すURL集合が一致することを確認する。

**`page_metadata` についての判断（Schema変更なし）**：`page_metadata` はURLごとに1行で、最新の読めた閲覧イベントを `event_id` で指す
（`ON DELETE CASCADE`）。そのイベントを個別削除すると、同じURLの他の閲覧が残っていてもこの1行は消える。付け替えると
「そのイベントのメタデータ」という意味が変わり、消した閲覧に由来するページ情報が残るため、既存のスキーマ意味に従って**一緒に削除**し、
確認画面に件数を出す。

## 3. 確認ID・対象集合・Vault境界（URL / Imageを含む）

Page削除（Issue #6）と同じ契約を、URL・Imageにも適用した（`native-host/src/item_delete.rs`）。

- **持続ポート**：`chrome.runtime.connectNative` の1ポート＝1 Hostプロセス。確認ID・領収情報はそのプロセスの中だけにあり、
  Host再起動・別ポートでは確認できない。Hostは再送しない。
- **メッセージ**：`item_state`、`visit_history`、`item_delete_preview`、`item_delete`、`item_delete_status`（capability `item_delete_v1`）。
- **確認ID**：UUID v4（予測不能）、TTL 120秒、1ポートに1件、一度使うと消える。別のIDでの確認は拒否するだけで、保留中の確認は消費しない。
  新しいpreviewは前のものを置き換える。
- **Vault結び付け**：全メッセージが `vaultToken`（ファイルの同一性）と `selection_generation` を持ち、Hostは毎回ConfigLockの下で
  新規に確認する。A→B→Aは同じファイルでも別の世代として拒否。Vault・鍵エラーでHost側の確認・領収情報を破棄。
- **対象集合の固定**：previewは1つの読み取りスナップショットで、件数と**対象行のフィンガープリント**（SHA-256。`event_id`・URL・タイトル・日時、
  画像はIDとハッシュ・保存履歴の全列・サムネイル・タグ付与。`data` 列は読まない）を作る。削除はIMMEDIATEトランザクション内で
  同じ計算をやり直し、**件数もフィンガープリントも一致した場合だけ**進む。件数が同じで別IDの集合に入れ替わった場合・行が編集された場合も `item_targets_changed` で
  変更ゼロ。
- **トランザクション**：visitsは一時ID集合→削除→残りゼロの検証。URL・画像は既存の `delete_url` / `delete_image`（原子的、結果検証つき）。
  失敗はすべてロールバック。共有BLOBの概念はImageにはない（1 `image_blobs` 行＝1画像）。参照整合（FK、`integrity_check`）をテストで確認。VACUUMなし。
- **応答喪失**：COMMIT後に応答が壊れる・来ない場合、同じ生存ポートの `item_delete_status` で証明できたときだけ成功。
  ポート切断・Host再起動は「削除状態未確定」で、自動再送しない。
- **旧メッセージの撤去**：弱い契約の `url_delete(_preview)` / `image_delete(_preview)` はHostから**削除**した（`unknown_type`）。
  削除の核関数（`delete_url`、`delete_image`）は共有。Issue #43の該当節は本書が置き換える。

UI側：`delete-client.js`（持続ポート。要求ID照合、閉じた接続の遅延エラーが後継接続を壊さない）、`delete-ui.js`（共通ボタン・確認ダイアログ）、
`url-history.js`（グループの展開・イベント行）。Vault変更・新しい検索・一覧の置き換えでダイアログと展開履歴を撤回し、
古い応答は世代番号で捨てる。

## 4. 共通削除UI

- **赤いゴミ箱**：`button.danger-button`（`--danger` はライト `#b3261e`／ダーク `#f2b8b5`、forced-colors対応、hover・focus-visible・disabled）。
  アイコンは `extension/assets/delete.svg` をCSSマスクで表示（既存の `settings.svg`／`cached.svg` と同じ方式。外部読み込み・スクリプトなし）。
- 個別削除はアイコンのみで、`aria-label` と `title` を持つ。範囲削除はアイコン＋範囲が分かるラベル。赤だけに依存しない。
- すべての削除に確認ダイアログ（`<dialog>`、背景inert）。**破壊的ボタンへ自動フォーカスしない**（初期フォーカスはキャンセル）。
  Escape／Tabの循環、キャンセル後のフォーカス復帰、削除中の二重実行防止。
- Image：ゴミ箱はタイルの**外側**（兄弟要素）に置き、ボタンの入れ子を避ける。押してもタイルの詳細や右クリック操作は動かない。
- 操作ドロップダウンの「Vaultから削除」は除去（URLタブ・画像の管理）。Policy操作は残した。

**SVG**：`assets/delete.svg`（PO配置）をそのまま `extension/assets/` にコピーした（改変なし）。`<svg>` に `<path>` 1つで、script・style・外部参照なし
（テストで確認）。Material Symbolsの「delete」としてPOから提供されたもの。**上流のSVGとの照合は行っていない**ため、
`THIRD_PARTY_NOTICES.md`（ルートと `extension/`）に、Apache-2.0・提供元・「上流との同一性は未確認」と記録した。

## 5. Secure Vault と VisibleStore

VisibleStoreは別コピーを持たず、同じVaultファイルをMCP要求ごとに読み、ポリシーも毎回評価する（Issue #43の記録どおり）。
したがって削除は次のMCP応答に反映され、孤立した公開レコードは残らない。実Host＋実MCPの結合テスト
（`tests/extension/vault-items.test.js`、opt-in）で、イベント個別削除・条件一致削除・画像削除の直後にMCP応答から消えることと、
残すべきものが残ることを確認した。暗号化Vault（Secure Vault）については、ロック中のHostテスト（`a_locked_secure_vault_refuses_reads_and_confirmations`）で、
ロック中の読み取り・確認が拒否され、解錠しても古い確認が復活しないことを確認した。暗号化Vaultでの実際の削除は、**自動テストでは実施していない**
（PO手順に含めた）。

## 6. Privacy Policy・MCP境界

Policy・公開承認・ETagには触れていない。削除はPrivacy Policyで拒否しない。`neverSave`／`saveButNoMcp` の記録・評価は変更なし。
Page原本・メタデータはVisibleStore／MCPに入らないまま（Page・URL・Imageの非干渉をテスト）。方式A・ADR-0000は変更なし。

## 7. 変更ファイル

新規：`native-host/src/item_delete.rs`、`native-host/src/item_delete/tests.rs`、`extension/delete-client.js`、`extension/delete-ui.js`、
`extension/url-history.js`、`extension/assets/delete.svg`、`tests/extension/{delete-client.test.js,delete-fixtures.js,vault-delete-ui.test.js}`、本書。
（`assets/delete.svg` はPO配置のまま。）

更新：`native-host/src/{main.rs,vault_items.rs,vault_items/tests.rs,read_token_tests.rs}`、`native-host/tests/{vault_items.rs,stdio_protocol.rs}`、
`extension/{sidepanel.html,sidepanel.css,sidepanel.js,image-gallery.js,item-dialog.js,vault-items-client.js,host-capabilities.js,page-panel.js,page-text.js,THIRD_PARTY_NOTICES.md}`、
`THIRD_PARTY_NOTICES.md`、`README.md`、`tests/extension/{fake-dom.js,navigation.test.js,page-panel.test.js,tabs.test.js,vault-items-ui.test.js,vault-items.test.js}`。

`Cargo.toml`／`Cargo.lock`、manifest、Schema（8のまま）、VisibleStore、MCP Server、Privacy Policyは変更していない。

## 8. 残存リスク・未解決事項

- 実Chrome・実Windowsでの確認は未実施（§9）。ダイアログ・フォーカス・狭い幅・ダークテーマの実見が必要。
- 展開した閲覧履歴・ダイアログは、**他のプロファイルやウィンドウでのVault変更を即時には検出しない**。Hostは次の操作（preview・削除）で必ず検証して拒否する。
  Page削除にある5秒のpollは付けていない。
- 条件一致の `visits` 削除と `tab_snapshots`：検索で見つかったURLでも、その条件に一致する閲覧がなければ確認前に「既に存在しません」となる（スナップショットだけが一致した場合）。
- イベント個別削除は、そのイベントが持つ `page_metadata` 行を一緒に消す（§2）。同じURLの他の閲覧のページ情報は失われる。
- 画像・URLの全件／一括削除は1トランザクションで、非常に大きなVaultでは書き込みロックを長く保持し得る。フィンガープリント計算はメタデータのO(N)走査（Page削除と同じ）。
- 削除後のVaultファイルは縮小されない（VACUUMなし）。バックアップには残る。確認画面に明記した。
- 「確認中…」の状態表示は、URLタブの状態行にだけ出る（Imageタブでは出ない）。
- `delete.svg` の上流との同一性は未確認（§4）。
- 暗号化Vaultでの実削除、Page保存・エクスポートとの組み合わせは、自動テストでは範囲外（Pageとの非干渉は一時Vaultのテストで確認）。

## 9. PO実機確認手順（全項目未実施）

隔離Chromeの準備は `docs/issue-006-slice-2.md` §8と同じ（別名のテストHost、temp APP_DIR、tempプロファイル。Hostは今回のReleaseビルド）。
**テスト用Vaultだけ**を使い、実Vaultには触れない。

1. 見た目：URL・Image・Pageの各タブの削除ボタンがすべて赤いゴミ箱で、個別はアイコンのみ、範囲削除は文言つき。ライト・ダーク・狭い幅で視認できる。
2. URLタブ：行の操作ドロップダウンに「Vaultから削除」がない。行ごとに「閲覧履歴を表示」「URL全件削除」がある。
3. 展開：履歴が新しい順に出る。同じURLを複数回開いたVaultで、閲覧イベントが日時つきで並ぶ。スナップショット件数の注記が出る。
4. イベント個別削除：1件のゴミ箱→確認ダイアログに日時・タイトル・URL・「同じURLの他の閲覧履歴とスナップショットは削除しません」。キャンセル／Escapeで無変更、フォーカスが戻る。
   確定後、そのイベントだけが消え、展開したまま残りが表示される。スナップショットだけ残るURLは一覧に残る。
5. 最後の閲覧：スナップショットなしのURLは、最後の閲覧を消すと一覧から消える。
6. URL全件削除：確認の件数（閲覧・スナップショット・ページ情報・遷移）が実際と一致し、確定後に行が消える。別クエリのURLは残る。
7. 範囲削除：条件なしで「全閲覧履歴を削除」。検索語で「条件に一致する閲覧履歴を削除」に変わる。確認に条件・件数（表示行数ではなくDB上のイベント数）・
   「スナップショットは対象外」が出る。カレンダーで日付を選ぶと、その日の閲覧だけが対象になる。同じURLの条件外の閲覧が残る。
8. Image：タイル右上のゴミ箱。押しても詳細が開かない。確認に原本サイズ・保存履歴数・サムネイル・タグ。確定後にタイルと（開いていれば）詳細が消え、他の画像のタグは残る。
   タグ絞り込み中は「検索結果をすべて削除」、なしは「全画像を削除」。
9. Page：履歴行・URL見出し・上部のゴミ箱が同じ見た目で、既存の確認・削除・エクスポートが従来どおり動く。
10. 境界：確認ダイアログを開いたまま設定でVaultをBへ切り替え→Aへ戻す。ダイアログが閉じ、何も削除されない。別ウィンドウで対象を変更してから確定すると「確認後に削除対象が変更されました」で無変更。
11. Host停止・ポート切断：確定の途中でHostを終了し、「削除状態を確認できません」（削除が完了している可能性がある旨）が操作元のタブに表示され、ダイアログが閉じても残り、再送されない。「何も削除していません」とは表示されない（再読み込みで状態を確認）。
12. MCP：Vaultを公開承認し、MCPクライアントで検索。削除したURL・画像が次の応答から出ない。
13. バックアップ・復元：削除前のバックアップには削除済みデータが残る。削除後のバックアップを復元して、一覧・削除が正常。
14. 暗号化Vault：ロック中に確認ダイアログを確定すると拒否され、解錠後も古い確認が使えない。
15. キーボードのみ：Tabで各ゴミ箱へ、Enterで確認、Escで閉じる。スクリーンリーダーでアイコンのみのボタンが読み上げられる。
16. 非回帰：URL・Imageの検索、保存、タグ、Policy操作（保存しない／MCPに見せない／公開）、Page保存。

終了時の後片付けは `docs/issue-006-slice-2.md` §9末尾と同じ。

## 10. 自動検証

2026-11-14、合成データ・一時Vaultのみ。

| 対象 | 結果 |
| --- | --- |
| JS（ReleaseのHost／MCPを指定、全opt-in） | 963成功、0 skip、失敗0 |
| JS（通常の `npm test`） | 963件中949成功、条件付き14 skip、失敗0 |
| Native Host `cargo test --locked` | 全成功（lib 146、bin 352、結合テスト各種。`item_delete` 19件、`vault_items` 結合5件を含む）|
| VisibleStore | 108成功、既存の計測2件はignored |
| MCP Server | 57成功（21＋15＋21）|
| 3 crateの fmt --check ／ clippy `-D warnings` | 成功 |
| Native HostのReleaseビルド | 成功 |
| `git diff --check` | 成功 |

- VisibleStore／MCP ServerのReleaseビルドは、ソースが変わっていないため今回は再実行していない（同日の前回ビルドを実Hostテストで使用）。
- 実Host・実MCPの結合テスト（`vault-items.test.js`、`vault-items-ui.test.js`）は成功。
- ログ（ignored）：`.tmp-7-native-host-tests.log`、`.tmp-7-js-real-tests.log`。

## 独立レビューの重点

- `item_delete.rs` のフィンガープリントと再検証、確認の単一消費、世代照合、`VISIT_MATCH` と `search_pages` の一致。
- 旧 `url_delete*` / `image_delete*` の撤去が他の経路に残っていないこと。
- `delete-client.js` の接続世代、`delete-ui.js` のダイアログ状態機械（削除中のキャンセル禁止、撤回、古い応答の破棄）。
- 未追跡の新規ソース・テストもレビュー対象。

---

# 追加Slice — CENSORED（削除済み遷移元の表現、Schema 9）

PO決定（2026-11-15）：案Aを採用し、Schema 9への更新を承認。実装完了候補。**総合PASSではない**（独立レビュー・PO実機確認は未実施）。
commit / push / PRなし。

## A. Slice 0で確認したデータモデル

- `visits.event_id`（TEXT主キー）は拡張機能の `crypto.randomUUID()`。`page_metadata`（URLごとに1行、`event_id` で最新の読めた閲覧を指す、
  `ON DELETE CASCADE`）と `navigation_events`（`target_visit_id` が主キーで `ON DELETE CASCADE`、`source_visit_id` が `ON DELETE SET NULL`）が従属する。
- **遷移記録 = `navigation_events` の1行**。遷移先の訪問（`target_visit_id`）ごとに1行で、遷移元は「直前に表示していた文書」の**申告**
  （クリックの証明ではない。Issue #41）。`tab_snapshots` は遷移の参照元ではなく、閲覧イベント削除では動かない。
- `source_visit_id = NULL` は元々「未取得・保存していない・ポリシーで除外」を意味する。FKの `SET NULL` で削除済みの遷移元もNULLになるため、
  **Schema 8ではNULLから「削除された」と「元から不明」を区別できなかった**。
- 現行UI・VisibleStore・MCPは `navigation_events` を読まない（既存テストで確認）。遷移ビューは存在しない。
- 削除対象のURL・タイトルが残りうる経路：`page_metadata`（CASCADEで削除）、`navigation_events`（URL列を持たない）、`tab_snapshots`
  （独立記録。同じURL文字列でも閲覧イベントの従属ではないため、イベント削除・条件一致削除では削除しない。URL全件削除のみ削除）。

## B. Schema 9

`navigation_events.source_censored INTEGER NOT NULL DEFAULT 0 CHECK (source_censored IN (0, 1))` と
`CHECK (source_censored = 0 OR source_visit_id IS NULL)` を追加（`schema.rs`、`SCHEMA_VERSION = 9`）。

- 新規Vault用。Schema 8以前は拒否し、移行は作らない（Hostの `vault_schema_mismatch`）。
- 既存の `ON DELETE SET NULL` / `CASCADE` は維持。
- マーカーはフラグ1つで、URL・タイトル・時刻・IDを持たない。**元のURLやタイトルを `[CENSORED]` に書き換えて保存することはしない。**
- `source_visit_id` に有効な参照を新しく設定する経路は存在しない（行は更新されない。Issue #41）。仮に設定すればCHECKが
  `source_censored = 1` との同居を拒否する（テスト済み）ので、矛盾状態は作れない。

## C. CENSOREDの意味論

| 状態 | 条件 | 表示 |
|---|---|---|
| Present | `source_visit_id` が有効、`source_censored = 0` | 通常表示 |
| Censored | `source_visit_id IS NULL` かつ `source_censored = 1`（明示的な削除が設定した場合だけ）| `[CENSORED]` |
| Unknown | `source_visit_id IS NULL` かつ `source_censored = 0` | 既存の不明表示 |
| （存在しない組み合わせ）| 参照あり＋censored | 何も表示しない（`None`／`null`）|

- 判定関数：Rust `navigation::source_state` / `SourceState::label` / `CENSORED_LABEL`、JS `navigation-text.js` の `navigationSource`。
  NULLだけでCensoredにはならない（テスト）。`[CENSORED]` は表示文字列のみで、保存しない。
- **遷移の真正性**：`A → B → C` でBを削除すると、`A→B` の行はB（遷移先）の行として一緒に削除され、`B→C` の行はCの行として残り
  `source_censored = 1` になる。残る情報は `[CENSORED] → C` だけで、`A → C` は作らない（テストで `a` を遷移元とする行が存在しないことを確認）。
  Aとの関係はBの行と共に失われ、復元しない。時系列から遷移を推測もしない。

## D. 影響を受ける記録の定義とプレビューの件数

Hostが同じDBスナップショットから計算する（`item_delete.rs`）。

- **n**：削除する `visits` の行数。
- **m**（`navigationSourcesCleared`）：遷移元が削除対象で、遷移先は削除対象でない `navigation_events` の行数。記録は残り、遷移元がCENSOREDになる。
- **k**（`navigations`）：遷移先が削除対象の `navigation_events` の行数。遷移記録そのものが削除される。
- 遷移元・遷移先がともに削除対象の行はkに数え、マーカーは付けない（行ごと消える）。
- `page_metadata` の件数は遷移記録数として使わない（別項目「ページ情報」）。

確認ダイアログ（0件の項目は省略）：

> n件の閲覧履歴を削除します。
> 遷移元が検閲済み（[CENSORED]）になる記録：m件（記録は残りますが、削除した閲覧は復元しません）
> 一緒に削除される遷移記録：k件

イベント個別・条件一致・URL全件削除のすべてで同じ表示。

## E. 実行と変更検知

- 削除トランザクション内で、遷移元が削除対象の残存行に `source_visit_id = NULL, source_censored = 1` を**同一文で**設定してから
  `visits` を削除する（CHECKを満たす順序。FKの `SET NULL` は残るが、すでにNULLなので作用しない）。設定した行数が `m` と一致しなければロールバック。
  `delete_url`（URL全件削除）も同じ。
- **フィンガープリントに影響遷移記録の集合を追加**：k・mの各行（対象ID、遷移元、`source_censored`、種別、遷移タイプ、時刻）を
  対象の閲覧行と一緒にSHA-256へ入れる。確認後に、閲覧集合が同じでも影響遷移記録が増減・入れ替わった場合・フラグが変わった場合は
  `item_targets_changed` で拒否（変更ゼロ、マーカーも付かない）。件数が同じで別IDの集合に入れ替わった場合も拒否（テスト済み）。
- 確認ID・TTL・単一消費・接続・Vault世代・応答喪失時の状態照会は既存#7のまま。

## F. UI

- **新しい遷移ビューは作っていない**。データ層（Schema・マーカー・判定関数）と確認ダイアログの件数表示までを実装し、`[CENSORED]` を
  実際に表示する経路ビューは**後続Issueへの切り出しを提案**する（`navigationSource` をそのまま使える）。
- URL検索結果・履歴展開に `[CENSORED]` の疑似行は出さない（テストでソースに文字列が現れないことを確認）。

## G. Privacy / MCP境界

- 削除された `visits` はURL検索に返らない（既存テスト）。従属する `page_metadata` はCASCADEで物理削除。マーカーにURL・タイトルはない
  （テストで、削除した閲覧のタイトル・URLがライブVaultのどの表の値にも残らないことを確認）。
- VisibleStore・MCPは `navigation_events` を読まず、マーカーを実在URLとして解釈する経路がない。Page・Imageの独立記録は消さない。
- `neverSave` / `saveButNoMcp`、公開承認、Vault切り替え安全性は変更なし。
- **アプリケーション上の削除であり、SQLiteファイル・WAL・外部バックアップからの物理的な復元不能性は保証しない。** 削除前のバックアップには
  元の記録（遷移元の閲覧を含む）が残る。

## H. 追加・変更したファイル

`native-host/src/{schema.rs,navigation.rs,item_delete.rs,vault_items.rs}`、`extension/{navigation-text.js,delete-ui.js}`、テスト
（`native-host/src/{item_delete,vault_items,navigation}/tests.rs`、`native-host/tests/{vault_items,stdio_protocol}.rs`、`manage/convert_tests.rs`、`images/tests.rs`、
`visible-store/src/tests.rs`、`mcp-server/tests/stdio_mcp.rs`、`tests/extension/{vault-delete-ui,navigation-text,delete-fixtures}`）。
位置指定の `INSERT INTO navigation_events VALUES (…)` は、列追加に伴い列名指定に直した。

## I. 追加テスト

Rust：`source_censored` のCHECK（0/1のみ、参照と同居不可、SET NULL単独ではマーカーなし）、明示削除と元から不明の区別、A→B→C、
遷移元・遷移先の同時削除、複数回の削除（マーカー保持、遷移先削除で行ごと消える）、URL全件削除のマーカー、影響遷移記録のみの変化・
同数別集合・フラグ変更の拒否、バックアップへのマーカー保持と整合性、削除した閲覧の痕跡がないこと、`source_state` の4状態。
JS：n・m・k表示、0件の省略、キャンセル初期フォーカス、影響集合の変化で拒否、`[CENSORED]` のUnknownとの区別、疑似URLが一覧に出ないこと。
既存のURL / Image / Page削除テストは再実行して成功。

## J. 未検証・残存リスク

- 実Chrome・実機での確認は未実施。暗号化Vaultでの実削除、バックアップの復元操作は自動テストの範囲外（バックアップの内容はテスト）。
- Schema 8の既存Vaultは開けない（移行なし、PO決定）。
- `[CENSORED]` を実際に見せる経路ビューはない。削除したAとの関係が失われる（`[CENSORED] → C` までしか残らない）のは仕様。
- 同じ遷移元から複数の遷移がある場合も各行が別々にCENSOREDとなる（件数mはそれを数える）。

## K. PO実機確認手順（追加）

1. 新規のテストVault（Schema 9）で、A→B→Cと開く（Chromeで実際にリンクをたどる）。拡張機能で記録されていること。
2. BをURLタブから個別削除する確認画面で、「1件の閲覧履歴を削除します」「遷移元が検閲済みになる記録：1件」「一緒に削除される遷移記録：1件」が表示される。
3. 確定後、SQLiteビューアで確認する（任意）。削除対象はBの閲覧イベントだけである。
   - Aの閲覧イベントは**残存**し、AのURL・タイトルも残る。
   - Bの閲覧イベントは物理削除される。Bを遷移先とする `navigation_events` の行（A→B）はCASCADEで削除される。
   - Cの行は残り、`source_visit_id = NULL` かつ `source_censored = 1`（表示上は `[CENSORED] → C`）になる。
   - A→Cという直接の遷移は新規に生成されない。
   - Bのタイトル・URLが、どの表の値にも残っていない。
4. 条件一致削除・URL全件削除でも件数表示が同じ形で、0件の項目が省略される。
5. 確認後に別ウィンドウで遷移を追加してから確定すると「確認後に削除対象が変更されました」で無変更。
6. 元から遷移元が不明の記録（直接URL入力）は、削除後も `source_censored` 0のまま。
7. 旧Schema 8のVaultを選ぶと「対応していないスキーマ」で拒否され、ファイルが変更されない。

## L. 追加Sliceの実行記録（2026-11-15、合成データ・一時Vaultのみ）

| 対象 | 結果 |
| --- | --- |
| JS（Schema 9のHost＋Release MCP指定、全opt-in） | 968成功、0 skip、失敗0 |
| JS（通常の `npm test`） | 968件中954成功、条件付き14 skip、失敗0 |
| Native Host `cargo test --locked --no-fail-fast` | 全成功（bin 357、`item_delete` 24件、結合テスト各種）|
| VisibleStore | 108成功（既存の計測2件はignored）|
| MCP Server | 57成功 |
| 3 crateの fmt --check ／ clippy `-D warnings` | 成功 |
| VisibleStore・MCP ServerのReleaseビルド | 成功（Schema 9で再ビルド）|
| Native HostのReleaseビルド | 成功（別の出力先 `--target-dir` で検証）|
| `git diff --check` | 成功 |

- **`native-host/target/release/memocampus-native-host.exe` は更新できていない。** 実行中の `memocampus-native-host.exe` プロセスがこのファイルを使用中のため、
  既定の出力先への上書きが「アクセスが拒否されました」で失敗する。これらのプロセスの所有者を確認できなかった（POのChromeが起動したHostの可能性がある）ため、
  停止していない。結合テストにはSchema 9でビルドしたデバッグ版Hostを使用した。実機確認の前に、POが当該プロセスを終了してから
  Releaseビルドをやり直す必要がある。
- 初回の全テスト実行で `concurrent_config_writers_never_leave_a_mixed_or_partial_file`（Vault設定の並行書き込み、本Sliceと無関係の既存テスト）が
  1回だけ失敗したが、単独・再実行では成功した（不安定な既存テストの可能性。原因は調査していない）。

## M. 独立レビュー指摘の修正（FINDING-DELETE-01 / 02、2026-11-16）

**総合PASSではない**（agyの差分再レビューとPOのChrome実機確認は未実施）。

### M1. FINDING-DELETE-01（Medium）：COMMIT後の切断で「何も削除していません」と表示される

根本原因：`delete-client.js` の `close()` が、保留中のDELETEを `host_unavailable` で却下したあと**同期的に**
`onInvalidated("host_unavailable")` を呼ぶ。`sidepanel.js` はこれでダイアログを撤回（セッション破棄）し、
「Native Hostに接続できません。何も削除していません。」を表示した。その後、`delete()` が返した `item_delete_unknown` は
`delete-ui.js` の `if (session !== s) return;` で捨てられ、画面に届かなかった。ダイアログの寿命と削除コマンドの寿命を同一視していた。

状態（`delete()` の終わり方は次のいずれか1つ）：

| 状態 | 条件 | コード | 表示 |
| --- | --- | --- | --- |
| NotSent | `postMessage` 前の失敗（接続不能・`postMessage` が例外） | `delete_not_sent` | 削除要求は送信されませんでした。何も削除していません。 |
| ConfirmedNotDeleted | Hostが `ok:false` で拒否（ロールバック済み） | Hostのコード | 各コードの既存文言 |
| ConfirmedDeleted | Hostのreceipt、または同じ生きたポートへの `item_delete_status` が `confirmed:true` | — | 削除しました |
| Unknown | `postMessage` 後に結果を証明できない（切断・タイムアウト・不正応答・状態照会不能） | `item_delete_unknown` | 削除状態を確認できません。削除が完了している可能性があります。Vaultの内容を再確認してください（自動では再実行しません）。 |

切断だけではNotSent/ConfirmedNotDeletedにしない。DELETEが送信済みのまま `close()` が呼ばれた場合、`onInvalidated` には
`item_delete_unknown` を渡す（「何も削除していません」を主張しない）。

通知の維持：`createDeleteDialog.open({ onUnknown })` を追加した。`confirmDelete()` の `catch` は、ダイアログ（セッション）が
撤回済みでも、`item_delete_unknown` なら `onUnknown` を呼ぶ（キャプチャ済みのセッション `s` から）。URLタブは
`#url-item-status`、Imageタブは `#gallery-status` に表示する。両者とも `createStatusNotice` を使い、未確定通知は
**後続の通常メッセージ・一覧の再読込・新しいプレビューの失敗で上書きされない**（通知の下に通常メッセージが並ぶ）。
別のVaultを表示している場合は「以前に開いていたVaultでの削除要求です」を付け、新Vaultの結果と取り違えない。
通知はパネルを閉じるまで残る（消去UIは作っていない）。自動再送・自動再接続はしない（`delete()` は `item_delete` を1回しか送らない）。

変更ファイル：`extension/delete-client.js`、`extension/delete-ui.js`、`extension/sidepanel.js`、`extension/image-gallery.js`、
`extension/url-history.js`。Hostの契約（確認ID・TTL・単一消費・世代検証・receipt）、Schema 9、CENSORED、n/m/kは変更していない。
副次的変更：Hostが `vault_switched` 等で明確に拒否した場合は、従来の「未確定」ではなく確定的な「削除していません」になる
（Hostは削除前に拒否するため）。

回帰テスト：`tests/extension/delete-client.test.js`（4件）、`tests/extension/vault-delete-ui.test.js`（6件）。
`delete-fixtures.js` に `commitThenDisconnect`（COMMIT→応答破棄→ポート切断）を追加した。
修正を一時的に外すと7件が失敗し、修正後は全件成功することを確認した。

### M2. FINDING-DELETE-02（Low）：実機確認手順の誤記

§K手順3が「AとBの行・URL・タイトルが残っていない」としていた。Aの閲覧イベントは削除対象外のため残存する。
正しい期待結果（Aは残存、Bは物理削除、A→B行はCASCADE、Cは `source_visit_id = NULL` かつ `source_censored = 1`、A→Cは生成されない）に修正した。
同ファイル内の他の手順に同種の誤記がないか確認した（§9 手順11の文言のみ、新しい通知文言に更新）。

## N. FINDING-DELETE-04の修正とPO判断（2026-11-16）

独立差分再レビューの結果（FINDING-DELETE-03/04/05）は**FAILのまま消していない**。以下はPOが新たに下した対応方針による、未対応指摘の扱いの変更である。
**総合PASSではない**（再レビュー・実機確認待ち）。

| 指摘 | 重要度 | PO判断 |
| --- | --- | --- |
| DELETE-03 | Medium | **既知リスクとして受容**（実装しない） |
| DELETE-04 | Medium | **今回修正** |
| DELETE-05 | Low | **後続の品質改善へ延期** |

### N1. 調査：Hostの正式な契約（`native-host/src/item_delete.rs`）
- `item_delete { reviewId }` の成功：`{ ok:true, vaultToken, generation, operationId, deleted }`。`operationId` は確認した `reviewId` そのもの（Hostが発行したUUID）、
  `deleted` は実際に削除した件数のオブジェクト（プレビューの `counts` と同じ形。値の一致は契約にない）。
- `item_delete_status { operationId }` の成功：`{ ok:true, vaultToken, generation, operationId, confirmed }`。`confirmed:true` のときだけ `deleted` が付く。
  `operationId` は要求の値のエコーである。同一ポート・TTL内のreceiptだけを証明する。
- `requestId` は1往復の相関（応答の取り違え防止）、`reviewId` は1回限りの確認、`operationId` はその確認で実行した削除の識別子。
- 失敗は `ok:false`＋コード（ロールバック済み）。

### N2. 根本原因
`delete-client.js` は `ok:true` とVaultトークンの形だけを確認し、`operationId`・`deleted` を見なかった。状態照会も `confirmed === true` だけで成功にした。
契約外の応答（欠落・別操作・型不正）が削除成功になり、URL成功表示とImageタイル除去まで進めた。

### N3. 修正
- `isReceipt(response, reviewId)`：`operationId === reviewId` かつ `deleted` が（null・配列でない）オブジェクト。`deleted` をプレビュー件数と照合する制約は**加えていない**。余分なフィールドは許す。
- `delete()` は受理するreceiptにこれを要求する。満たさない応答は「送信済みで証明できない」として同じ生きたポートへの状態照会に回し、
  状態照会も `confirmed === true` かつ `isReceipt` を満たす場合だけ成功にする。証明できなければ `item_delete_unknown`。DELETEは再送しない。
- 分類（NotSent／ConfirmedNotDeleted／ConfirmedDeleted／Unknown）は不変。変更は `extension/delete-client.js` のみ。Rust・Schema・Host契約は変更していない。

### N4. 回帰テスト
`delete-client.test.js`：不正receipt 7種（operationId欠落／型不正、deleted欠落／型不正／null／配列、別操作）×（状態照会で成功証明／証明不能でunknown）、
不正な状態照会5種（別operationId、欠落、deleted型不正、confirmedの型不正）、正式receipt・余分なフィールドの受理。
`vault-delete-ui.test.js`：Imageで別操作receiptかつ証明なし→タイルは残り「削除状態を確認できません」・DELETE1回、URLで欠落receiptかつ証明なし→成功表示なし、
不正receiptでも状態照会で証明できればタイル除去。フィクスチャに `deleteReplyShape` / `statusReplyShape` を追加。
検証を一時的に外すと20件が失敗し、戻すと全成功。JS全件1001件：通常 987成功・14 skip・0失敗、Schema 9の実Host＋MCP指定 1001成功・0 skip・0失敗。

### N5. 既知リスク・延期（PO判断）
- **DELETE-03（受容）**：Unknown通知は永続化せず、複数Unknownの操作別管理、Vault世代付き履歴、A→B→A後の再評価は実装しない。操作が重なると通知の誤帰属・消失があり得る。Host側のVault世代検証は維持。
- **DELETE-05（延期）**：`item_state` 送信後、DELETE送信前のタイムアウトがUnknown扱いになる。DELETE未送信でデータ破壊はなく、表示が保守的になるだけ。後続の品質改善Issue候補。

## O. 削除確認ダイアログの可読性（PO実機確認の指摘、2026-11-17）

指摘：確認ダイアログが連続した文章で、対象・件数・注意が区別できない。**UI表示のみの変更**で、削除API・削除対象・確認ID・DB処理・Schema 9・Hostは変更していない。
総合PASSではない（Chromeでの見た目は未確認。自動テストはfake DOMによる構造・文言・CSS規則の検証）。

### O1. 構成（URL・Image・Pageで共通）
`extension/delete-ui.js` の `deletionModel`（URL/Image）／`page-text.js` の `deletionModel`（Page）が表示モデル `{ target, effects, notes }` を作り、
共通の `renderDeletion` がDOM APIだけで描画する（`createElement`＋`textContent`。`innerHTML`系は使用禁止でテストが検査）。

1. **削除対象**：対象・日時・タイトル・URL（個別履歴）／条件など。日時・タイトル・URLは**別々の行**（`dl/dt/dd`）。
2. **削除による影響**：件数は項目名と同じ行（例：削除する閲覧履歴 4件／遷移元が検閲済み（[CENSORED]）になる遷移記録 m件／一緒に削除される遷移記録 k件）。m・kが0の行は省略（従来仕様）。
3. **注意事項**：削除しないもの（スナップショット、他の閲覧履歴、ポリシー等）。
4. **警告**：「削除すると元に戻せません。」を独立した枠（`role="note"`、`--danger`の枠線と `--error-bg/--error-fg`）で表示。
- 確認ボタンは「削除する」（処理中は「削除中…」）。赤＋既存の `assets/delete.svg`（`setDangerLabel`）で、URL・Image・Pageとも同じ。

### O2. 狭い幅・テーマ・操作
- 長いタイトル・URLは `overflow-wrap: anywhere; word-break: break-all; white-space: pre-wrap` でダイアログ内に折り返す。幅300px以下では項目名と値を縦積みにする。
- ライト／ダークは既存のトークン（`--danger`、`--error-bg`、`--error-fg`）で切り替わる。forced-colors（高コントラスト）では枠線と色をシステム色にする。
- 初期フォーカスはキャンセルのまま（確認ボタンに初期フォーカスしない）。Tabはキャンセル⇔削除する、Escapeでキャンセル。ボタンは狭い幅で折り返す。

### O3. テスト
`tests/extension/vault-delete-ui.test.js`（構成・行・警告・ボタン・フォーカス・CSS・`innerHTML`禁止の4件を追加）。件数と文言の期待値を新しい行形式に更新した
（`vault-delete-ui`、`vault-items-ui`、`page-panel`、`page-process`）。Page側の既存契約（確認・キャンセル・フォーカス・二重確定）は変更なし。
JS全件1005件：通常 991成功・14 skip・0失敗、Schema 9の実Host＋MCP指定 1005成功・0失敗。
