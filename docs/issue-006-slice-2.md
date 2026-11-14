# Issue #6 Slice 2 — Pageタブ／履歴／削除の引き渡し

実装完了候補。**総合PASSではない**。POのChrome実機確認・独立レビューは未実施。
Slice 1レビューはPO報告のPASS WITH NOTESを前提とし、今回の独立レビューを代替しない。

## 1. Git／変更範囲

開始・終了ブランチは `feature/6-page-vault`。HEADは
`9fe97eb852292bd0ea8327b48252237a6b64efa0`。Slice 1の未コミット／未追跡ファイル、既存の
`.gitignore`変更、保留PoCを保全。commit / push / PRなし。実Vault・実Chromeプロファイル・
本番Native Messaging登録は使用／変更していない。

Slice 2で追加したファイル:

- `extension/page-client.js`：持続Native接続、request ID照合、Vault世代、削除状態照会。
- `extension/page-panel.js`：一覧・履歴・確認modal・競合破棄・新着／更新。
- `extension/page-text.js`：Page用の日本語表示文言。既存に共通翻訳カタログ／`_locales`はなく、
  既存のtextモジュールと日付formatter方式に合わせて集約した。新しい翻訳エンジンは作らない。
- `native-host/src/page_manage.rs`、`page_manage/tests.rs`：ローカル管理APIと検証。
- `tests/extension/page-fixtures.js`、`page-client.test.js`、`page-panel.test.js`、
  `page-process.test.js`：fixture／UI／通信／実Host・MCP。
- 本書。

Slice 2で更新したファイル:

- `extension/sidepanel.html`、`sidepanel.css`、`sidepanel.js`：4タブとPage接続。
- `extension/service-worker.js`：保存完了通知にselection generationを追加。
- `native-host/src/main.rs`：Page管理APIのdispatch／接続ごとのManager。
- `native-host/src/page_protocol.rs`：既存generation取得関数をcrate内再利用可能にしただけ。
- `native-host/src/manage/backup_restore_tests.rs`：復元後一覧・削除の検証追加。
- `scripts/measure-page-transfer.ps1`：FINDING-PAGE-01のハンドル保持・Refresh。
- `tests/extension/fake-dom.js`、`tabs.test.js`：Page fixtureと4タブ検証。

Slice 1のschema・保存・転送・バックアップ製品ロジックは再設計していない。Schema 8の
テーブル／制約／インデックスを**変更していない**。移行・互換レイヤーなし。

## 2. UI／検索の契約

- URL / Image / Page / 設定。既存のcreateTabsをそのまま4項目へ拡張。入力・スクロールは
  タブを切り替えても保持。Pageは初回選択時だけ遅延取得。
- グループキーは原本URLの完全一致（BINARY）。query、fragment、大文字小文字、Unicodeを
  変更しない。同一タイトルの別URLは別グループ。同じURLの異なるタイトルは同じグループ。
- グループの表示タイトル／日時はそのURLの最新保存イベント。保存日時DESC、イベントID DESC
  の安定順。履歴も同順。Dateは既存formatterで閲覧者のローカル時刻。
- 検索はURL・保存時タイトルの**リテラル部分一致**。trim・正規化なし。SQLite LIKEの
  ASCII英字は大小文字非区別、その他のUnicodeは区別。`!`、`%`、`_`はエスケープしてバインド。
- 検索でグループが見つかる条件は「そのURLのいずれかの保存イベントが一致」。グループは
  最新イベントを見出しにし、一致件数／全件数を表示。展開履歴とフィルター削除は
  **一致した保存イベントだけ**。URL全体削除は検索に関係なく全履歴で、確認件数も全件。
- 元URLはHTTP(S)検証後、ユーザーのクリックだけでChromeタブへ渡す。渡す文字列は原本のまま。
  現在のWebへの再訪問であり、魚拓表示でないことを明示。
- すべてtextContent／DOM API。MHTML読み出し・HTML挿入・renderer・本文検索・exportなし。
- modalは標準dialog.showModalで背景をinert化。キャンセルに初期フォーカス、Tab循環／Escape、
  キャンセル後の起点復帰。削除中は入力・確認・キャンセル・重複削除を無効化。
- 読込中・空・通信／Vaultエラーを分離。削除ACK前に行を消さない。
- 新着はタブの装飾dotとaccessible nameで通知。表示先頭、履歴閉、一覧へフォーカスなし、
  確認／削除中でない場合だけ自動更新。それ以外はユーザーが更新する。新着で展開やフォーカスを奪わない。

## 3. ローカルNative API

Page管理は **chrome.runtime.connectNativeの1ポート＝1 Hostプロセス**を使用。
既存のsendNativeMessageによる一回ごとの接続へ置き換えてはいけない。

共通: `requestId`。応答: `type: <command>_result, requestId, ok`。
成功応答には `vaultToken, generation`。失敗は固定errorコードのみで生のSQL／URL／stackを出さない。

| コマンド | 入力・用途 |
| --- | --- |
| `page_state` | 初回のVault識別／鍵・schema状態確認と5秒周期の再検証 |
| `page_list` | binding、query、cursor?、limit?。URLグループ単位 |
| `page_history` | binding、完全URL、query、cursor?、limit?。保存イベント単位 |
| `page_delete_preview` | binding、target。確認情報とreviewIdを発行 |
| `page_delete` | binding、reviewIdのみ。targetは再指定させない |
| `page_delete_status` | binding、operationId。**同じ接続で**保持したCOMMIT済みreceiptの照会 |

`target`は `{scope:'event',id}` / `{scope:'url',url}` / `{scope:'filter',query}`。
event ID・blob IDは出力で10進文字列にし、JavaScriptの整数丸めを回避。入力event IDは正の
canonical十進文字列、またはJS安全整数範囲の正整数のみ。save ID・SHA、URL、保存時タイトル、
capture/Host保存時刻、原本byte length、URL内全件／一致件数をメタデータから取得。
**page_blobs.dataは一覧・確認・fingerprintのSELECTに含めない。**

取得上限: default 20、max 50、query max 200 Unicode文字（UI入力上限は200 UTF-16単位）、
URL max 32768 UTF-8 bytes。応答組立予算700000 bytes、1行で超過しても巨大応答を返さず拒否。

### 継続取得

カーソルはHost発行UUIDに対する接続内の状態。query、取得種別、URL、直前の安定sort key、
メタデータ集合fingerprint、発行時刻に固定。TTL 5分、max 128。偽造／別query・URL・種別／
期限切れ／別ポート・Host再起動は拒否。上限で古い状態を破棄した場合も更新が必要。

更新・削除・同数置換・rowid再利用による取りこぼしを避けるため、カーソルを使う前に集合の
fingerprintを再検証。集合が変わったら`page_list_changed`で停止し、先頭から更新する。
新着を古いcursorへ混ぜない。グループと履歴のcursorは相互利用できない。

Schemaを増やさない代わりに、fingerprintは全Pageメタデータのストリーミング走査。
shared-reference集計は一度のGROUP BYで行い、1イベントごとの全参照走査を避ける。
大きなVaultでは走査のCPU／I/Oと一時集計メモリが増える。全原本のロードではないがO(N)であり、
大規模最適化／新index／更新revision列の導入は今回行っていない。

## 4. 確認と削除トランザクション

previewは一つの読取snapshotで次を確定する:

- イベント／URL件数、削除される原本数・bytes、共有により保持する原本数。
- 対象イベントのID、save ID、完全メタデータ、原本ID／SHA／サイズ、参照件数のfingerprint。
- 接続・Vault token・selection generation・targetに固定したreviewId。TTL 120秒。確認は1件だけ保持。

実行は既存ConfigLock → fresh Session ticket/key/schema検証 → write_forのIMMEDIATE transaction。
そこで対象集合・参照件数を再計算し、一致しない場合は**変更ゼロ**で`page_targets_changed`。
件数が同じ置換でも拒否する。reviewIdは一度だけ消費され、再送して範囲を広げない。

検証後、その同じトランザクションでTEMP ID集合を構成し、保存イベントを削除してから
その操作が参照していたBLOBのうち参照ゼロのものだけをDELETEする。無関係なorphan BLOB、
URL／Imageは削除しない。FK RESTRICT維持、削除件数・残存件数を照合。失敗はすべてrollback。
TEMP集合はmain schemaの変更ではない。Secure VaultのMEMORY temp_store契約を維持する。
VACUUMは自動実行しない。バックアップは削除しない。

成功はCOMMIT後のみ。応答喪失時、同じ生存ポートでreceiptが確認できた場合だけ成功。
切断・再起動・確認不能は「削除状態未確定」。再接続して旧deleteを再送しない。
UIは更新して状態を確認する。Host再起動をまたぐ永続削除receiptはSchema 8に追加していない。

## 5. Vault／Privacy Gate

- すべての管理要求はConfigLockの下でfreshなVault接続・鍵・schemaを確認し、全操作で
  vaultTokenとselection generationを照合。A→B→A、同じパスでの再作成、再選択でも旧確認は使えない。
- Vault／鍵エラーが観測されたらHost側のcursor・review・receiptも破棄。ロック後の解除で
  古い確認を復活させない。エラーをUIだけで防ぐ構成ではない。
- UIは操作世代で古い応答を破棄。同一プロファイルのVault変更通知／設定状態変更では即クリア。
  他のプロファイル／外部プロセスの変更は5秒pollでも検出し、確認を閉じる。pollは確認中も実施。
  外部変更を画面へ無遅延pushする保証はないが、Hostは次の操作時に必ず検証する。
- Native Host停止時は表示・確認を無効化。ポート再作成では旧cursor／reviewを再利用しない。
- neverSave、saveButNoMcpを一覧・削除の拒否理由にしない。Policyの変更・評価エンジン新設なし。
- Page管理はVisibleStore/MCPを通さない。MCP tool追加／公開権限追加なし。方式A・ADR-0000維持。

## 6. 自動検証

- JS全体: **925成功、0 skip**（Release Host/MCPを指定して全opt-inを実行）。
  通常のnpm testも実行: 925件中 **911成功、条件付き14 skip、失敗0**。
- Native Host: **615成功、0 ignored**。VisibleStore: **116成功、既存measurement 2 ignored**。
  MCP server: **57成功、0 ignored**。
- 3 crateのfmt --check、clippy --all-targets -- -D warnings、Release build成功。git diff --check成功。
- 新規Host管理11テスト: 同一／最終BLOB参照、完全URL、DB全件bulk、置換／metadata／共有参照変更、
  rollback、1万URL、安定keyset／独立history、literal検索、期限／改ざんcursor・review、A→B→A、
  鍵ロック、Host再起動、巨大event ID、最大メタデータ応答予算。
- 平文・Secure Vaultのバックアップ復元後に一覧→削除し、復元元・URL・Imageの保持を比較。
- JS fixtureはDOMとNative契約のUI検証。実Host/MCPの追加テストは、合成Blob→実SQLite保存、
  実Page一覧→UI確認→共有BLOB保持／最終回収、公開承認後のMCP非混入まで実行。
  **どちらも実Chromeキャプチャ・modal表示・レイアウト・ユーザー操作の証拠ではない。**
- OpenSSLの既存LNK4099（PDB欠落）warningとGit ignore参照の権限warningは残る。Rust lint失敗ではない。

主ログ（ローカルのignoredファイル）:
`.tmp-6-s2-{native-host,visible-store,mcp-server}-{tests,clippy,release}.log`、
`.tmp-6-s2-js-real-tests.log`、`.tmp-6-s2-page-process.log`、`.tmp-6-s2-memory-ps51.log`、
`.tmp-6-s2-memory-ps7.log`。合成データのみ。

## 7. FINDING-PAGE-01

WaitForExit後のRefreshだけではPowerShell 5.1で成功テストを失敗扱いすることを再現した。
Start-Process直後にHandleを保持し、WaitForExit後のRefreshも実施する最小修正。
測定ループや終了コードの検証は除去／緩和していない。

- Windows PowerShell **5.1.26100.9836**: 成功、終了コード0。128MiB合成転送 9.358秒、peak 153.82MiB。
- PowerShell **7.6.10**: 成功、終了コード0。8.669秒、peak 145.57MiB。

同時に他のテストも走っており、速度比較ではない。Chrome／Native JSON framingのメモリは含まない。

## 8. POの隔離Chrome準備（未実施）

既存 `tests/dogfood/onboarding-chrome.mjs` はsendNativeMessageだけの一回接続bridgeであり、
**変更せずにPage管理へ使ってはいけない**。Pageのcursor/reviewには持続接続が必要。
以下はPOが人手で行う別名テストHost方式。本番の`com.memocampus.vault`登録を変更しない。
Codexはこの準備／Chrome実機操作／レジストリ追加を実行していない。
登録形式・Windowsの別名キー・持続ポートの仕様は
[Chrome公式Native Messaging仕様](https://developer.chrome.com/docs/extensions/develop/concepts/native-messaging)
を参照した。インストール済みChromeでの動作を確認した意味ではない。

1. freshな `%TEMP%\memocampus-page-po-<GUID>` 配下に `app`, `profile`, `extension` を作り、
   本リポジトリのextensionを**コピー**する。PoCや実Vaultはコピーしない。
2. テストHost名を `com.memocampus.page_test_<32桁小文字hex>` とする。コピーしたextensionの
   JavaScript内の文字列`com.memocampus.vault`だけをこの別名に置換する。製品ソースは変更しない。
3. 新しいChromeプロセスを、そのtemp appを`MEMOCAMPUS_APP_DIR`として継承させ、
   `MEMOCAMPUS_DB`を未設定、`--user-data-dir=<temp profile> --no-first-run --no-default-browser-check`
   で起動する。環境変数はその起動用PowerShell内だけで設定し、OSの永続設定は変更しない。
   既存Chromeの窓や実プロファイルを再利用しない。
4. テストChromeのchrome://extensionsで開発者モード→コピーした拡張を「パッケージ化されていない
   拡張機能を読み込む」。一部Chromeではload-extension起動フラグが使えないため手動で行う。
5. コピー拡張のIDを確認し、temp内に次のNative manifestを作る（例の値は実際の別名・IDへ置換）。

   ```json
   {
     "name": "com.memocampus.page_test_<hex>",
     "description": "Isolated Page PO test only",
     "path": "C:\\works\\Memocampus\\native-host\\target\\release\\memocampus-native-host.exe",
     "type": "stdio",
     "allowed_origins": ["chrome-extension://<COPY_EXTENSION_ID>/"]
   }
   ```

6. POが許可する場合のみ、**新しい別名のキーだけ**
   `HKCU\Software\Google\Chrome\NativeMessagingHosts\com.memocampus.page_test_<hex>` を作り、
   既定値をtemp manifestの絶対パスにする。存在するキーは上書きしない。本番名のキーには触れない。
   開発拡張・テスト登録が組織ポリシーで禁止ならここで止め、PO判断で持続loopback bridgeを別途準備する。
7. 設定を更新してtemp appのパスを確認し、新しいSchema 8テストVault A/Bを明示作成する。
   WindowsのNative Hostが実際にtemp APP_DIRを継承したことを確認できない場合は**保存操作へ進まない**。
   この登録方式／環境継承／インストール経路自体は今回未検証。

## 9. PO実機チェックリスト（全項目未実施）

1. 4タブ、矢印／Home／Endキー、tabindex／ARIA、明暗テーマと狭い幅を確認。
2. localhostの合成HTTPページを右クリック保存。COMMIT後にPageへ表示されること。
3. 同URLを複数回保存し、1URLグループ／複数履歴になること（MHTML metadata差による別SHAは許容）。
4. query／fragment／大文字小文字だけ異なるURL、同タイトルの別URLを別グループとして確認。
5. 履歴を展開、続き取得、閉じて再展開。スクロール／フォーカス保持、保存日時とサイズを確認。
6. 元URLを開き、現在のWebへの再訪問でありMHTML表示でないこと。HTML風タイトルは文字列表示のみ。
7. 個別削除をキャンセル／Escape後にフォーカス復帰。再確認→確定し他の履歴／共有原本が残ること。
8. URL全体削除で全履歴件数を確認。別URLの共有原本はその保存イベントが残る限り保持。
9. URL／タイトル検索、ASCII大小文字、日本語、`% _ !`、前後空白でliteral検索を確認。
   初回表示件数を超える一致を用意し、bulkが読み込み済みページだけでなくDB全一致を削除すること。
10. フィルターなしでは「すべて」の警告と全件数を確認。確認後、別窓で一致履歴を増減して
    古いreviewIdが拒否されること（同件数置換、共有参照変更も含む）。
11. 一覧・履歴・確認取得を遅延させてVault A→B→A。旧表示／確認が消え、旧応答で上書きされないこと。
12. 一時キーの利用不可／復旧、Host停止／再起動。ロック・通信エラー表示、旧確認失効。
    COMMIT応答だけ喪失させた場合、receiptが証明できなければ「削除状態未確定」、自動再送なし。
13. URL／Imageの入力・スクロール・検索・保存・削除・Privacy Gateの非回帰を確認。
    PageのneverSave／saveButNoMcp保存は閲覧できても、MCP公開承認には結び付かないこと。
14. Page保存中のreload／SPA遷移／タブ消失をSlice 1同様確認。失敗時に新着成功通知を出さないこと。
15. temp平文／Secure Vaultをバックアップ・復元し、Page一覧・履歴・件数・削除が一致すること。

終了時はテストChromeだけを閉じる。作った**別名の一意なキーだけ**をパス・manifest内容と照合して
削除し、本番登録は保持。temp記録は独立レビュー用に保持してよい。広いレジストリ削除／
Chrome終了一括操作／実データの削除は行わない。

## 10. 独立レビューの重点／残存制約

- main dispatchから永続ポート、cursor／reviewの寿命、Host再起動後のunknown。
- ConfigLock→fresh key/schema→write lockの順序、全世代照合、UI epochと後続refresh中の遅延破棄。
- 対象／参照fingerprintの完全性、TEMP ID集合、原子的rollback、RESTRICT、rowid再利用。
- metadata-only SQL、出力budget、巨大ID、literal LIKE、条件付き履歴とURL全体削除の違い。
- 方式A／ADR-0000、既存Policy、URL／Image／MCP経路が変更されていないこと。
- 未追跡の新規ソース・テストもレビュー対象。git diffだけでSlice 1／2全体は見えない。
- 他プロファイルの画面クリアはpoll遅延あり。全metadata走査と蓄積したDOMの実機性能、Chromeの
  native modal／フォーカス／Side Panel描画は未確認。MHTML再現性・raw export・全文検索は対象外。
- 接続を失うとcursor／review／削除receiptは復元できない。自動再送を優先せず再確認する契約。

以上をPO実機確認と独立レビューへ引き渡し、commit / push / PRをせず停止する。
