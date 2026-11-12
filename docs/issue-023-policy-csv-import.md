# #23 CSV Slice 2 — Privacy Policy CSV Import

## 作業状態と変更範囲

- `git fetch origin`でExportマージ済みの最新`origin/main`（`fd94875`、PR #58）を確認。
- 開始時は`feature/23-policy-csv-export`、upstreamは同名originブランチ、作業ツリーはクリーン。そこから最新mainを基点に`feature/23-policy-csv-import`を作成した。
- commit / push / PRは行わない。実装後はPOレビュー待ち。

| ファイル | 変更 |
| --- | --- |
| `extension/policy-csv.js` | Exportは変更せず、上限定数・fatal UTF-8読み取り・CSV状態機械・追加候補構築を追加 |
| `extension/options-state.js` | 世代付き原子的バッチ追加API、Import用の検証応答バージョン照合オプション |
| `extension/options-page.js` | 明示ファイル選択、読み取り/検証/ETag/世代確認、ゾーン別状態表示 |
| `extension/options.html` | ゾーン別Importボタン・file input・説明・上限表示 |
| `tests/extension/policy-csv-import.test.js` | 構文・UTF-8・制限・Lossless・原子的APIの46テスト |
| `tests/extension/options-csv-import.test.js` | UI・失敗時無変更・非同期競合・明示適用の27テスト |
| `tests/dogfood/policy-csv-import-chrome.mjs` | 実Hostループバック＋隔離Chrome＋一時Secure Vaultによるdogfood |
| 本文書 | 設計・結果・制限・agy/POへの引き継ぎ |

依存追加、Chrome権限変更、Rust/Policy評価/MCP公開/暗号化/移行/Export仕様変更はない。

## パーサーと入力仕様

`readPolicyCsvFile`でファイルサイズを先に確認し、`arrayBuffer`を読み、実バイト数も再確認。
`TextDecoder("utf-8", { fatal: true, ignoreBOM: true })`で不正UTF-8を拒否し、先頭のBOMだけを除く。
`parsePolicyCsv`はstart/unquoted/quoted/afterQuoteの状態機械で1回走査する。行や列の単純splitではない。

- 生のヘッダーは正確な`kind,pattern`のみ。ヘッダーの引用符、空白、大小文字変更、旧形式は拒否する。
- 正式なkindは既存モデル/Rust wire形式と同じ`pattern` / `regex` / `domainWildcard` / `exact`。未知kindを変換しない。
- レコード区切りはLFのみまたはCRLFのみ。混在・裸のCRは拒否する。引用符内ではCR/LF/CRLFをそのまま保持し、物理行数を追跡する。
- CSV引用符と`""`、空フィールド、末尾改行の有無、Unicode/空白/重複/順序を保持する。
- 空ファイルは拒否。ヘッダーのみは追加0件で無変更、Host呼び出しなし。
- 余剰/不足列、不正引用符、NUL等のC0制御文字（TAB/CR/LF以外）とDEL/C1制御文字を拒否する。
- パーサーは文字列のPolicy上の意味を判定しない。たとえば空RegexはRust Hostが許容するため、原子的追加APIも独自に拒否しない。一方、空Pattern等はHost検証で拒否される。
- CSVエラーには物理行番号、Hostの追加ルールエラーにはレコード開始行番号を表示する。UTF-8/読み取り/ファイル全体の上限など、位置を特定できないものは1行目をファイルレベルの表示位置とする。
- ルール本文、ファイル名/パス、例外メッセージ/スタックをログやUIに流さない。セルをJSONや動的プロパティ名として解釈しない。

## リソース上限

全値は`CSV_IMPORT_LIMITS`に集約している。

| 対象 | 上限 | 根拠 |
| --- | ---: | --- |
| ファイル全体 | 1,048,576バイト | Host `policy_gate.rs`の`MAX_DRAFT_BYTES` / `policy_store.rs`の`MAX_BYTES`と同じ |
| CSVデータレコード | 1,000件 | Host `policy.rs`のゾーンごとの上限 |
| フィールド | 32,768 UTF-8バイト | HostのURL上限32,768 UTF-16単位に対し、より保守的なバイト上限。非Regexの任意長を許さないクライアント側リソース制限 |
| Regexフィールド | 4,096 UTF-8バイト | Host `MAX_REGEX_BYTES`と同じ |
| 追加後の対象ゾーン | 1,000件 | Hostのゾーン上限と同じ |
| 追加後の全Regex | 128件 | Host `MAX_REGEX_RULES`と同じ |
| 候補Draft JSON | 1,048,576 UTF-8バイト | Hostリクエスト上限と同じ。Hostはさらにpretty JSONでの保存上限も検証する |

フィールドを閉じる前にも文字数上限で成長を止め、閉じるときにUTF-8バイト数を確認する。
時間は入力長に線形、保持量は上限付きの入力・デコード文字列・最大1,000レコード・既存/候補Draft/JSONでO(入力＋Draftサイズ)。レコードごとのDraft書き込みや無制限な配列/ネスト展開は行わない。
大きすぎるファイルは読み取り前、件数超過は走査途中、候補全体の超過はHost送信前に中断する。

## Host検証と原子的Draft更新

1. ゾーン別ボタンでfile inputを開く。開始時のDraft参照・version・view参照・ETag・対象ゾーンをoperationへ固定する。
2. ファイル全体を読み取り、全件をパース。現在の世代を確認してからDraftスナップショットを取る。
3. `csvImportCandidate`が指定ゾーン末尾へ追加した**別の候補**を作る。既存ルール・他ゾーン・categories・enabled・revision等を保持する。
4. 既存`validateDraft`経由の正式なHost検証で候補全体を検証。応答のschema/engineが候補と一致することもImportでは確認する。
5. 既存`loadActivePolicy`でActiveを読み直し、開始時ETagと照合する。読んだActiveをページにadoptしたり、Draftを再作成したりしない。
6. operation/Draft/view参照・version・ETag・busy/loading/blockedを再確認する。
7. `appendRulesAtomically`がexpectedVersionをもう一度確認し、配列をコピーして1回の代入で追加。version/layoutは1回だけ進む。空バッチは進まない。

途中まで追加して戻す方式ではなく、失敗時にはロールバックすべき変更自体が存在しない。
重複はCSV内・既存Draftとの双方で無言除去しない。Rust Hostが重複を許容することを実Host dogfoodでも確認した。
一方、既存チェッカーの単独追加/編集にある重複抑止は変更していない。

## 非同期競合とETag

- Import中は編集、他のImport、検証、適用、再読み込みを停止する。イベントハンドラーのガードもあり、disabled属性だけに依存しない。
- 直接モデルを変更してUI無効化を迂回しても、file/validate/status/get各待機後にversion不一致で中止する。
- 同じ入力のchange二重発火と他ゾーンのchangeは受理しない。古いoperationの完了は新しいoperationの状態を消さない。
- 選択キャンセルはDraft/Active無変更・無エラー。チェッカーの編集中コンテキストも保持する。
- 他画面/プロセスがActiveを変更した場合、最終読み取りのETag不一致で中止し、既存のconflict状態を使って再読み込みを促す。ローカル変更は保持する。
- Activeが未設定・破損・不明・Host切断なら反映しない。初回案内のpreset Draft、旧v1、読み取り不能の画面ではImportを利用不可とする。勝手な初期化や移行はしない。
- 外部プロセスの変更とページ内メモリ更新を共通lockで原子化するAPIはないため、最終ETag読み取り後の外部変更までは固定できない。その場合もActiveはImportで書かれず、後の明示Applyは既存HostのETag CAS/再検証が拒否する。安全性を古い検証結果だけに依存させない。

## Privacy Gateの根拠

CSVにゾーン識別子はない。ユーザーがクリックしたゾーンだけをallowlistで扱い、ゾーン指定の余剰列/未知kindは拒否する。
Importは追加だけであり、既存Blindの削除/書換え、ゾーン移動、有効化/無効化、categories変更を行わない。
パースしたデータはローカルメモリに置き、候補検証にだけ既存Native Hostへ送る。ネットワーク送信・storage書き込み・自動記録キュー操作は追加していない。
Activeへの反映は既存の明示「適用」のみであり、`neverSave`優先、`saveButNoMcp`のMCP出口拒否、Secure VaultのPublication Gateを変えない。
PSL制限Wildcardの承認やMCP公開承認をImportから呼ぶことはない。

## 自動検証記録（2026-11-12）

- 新規CSV/モデル46件＋UI27件：計73成功、失敗なし。
- 最終`npm test`：877件中864成功、13 opt-in skip、失敗なし。
- 実Host/MCP指定の全JS：877成功、失敗/skipなし。
- `cargo build --locked`：成功。既存OpenSSL静的ライブラリのPDB欠落LNK4099警告、およびsandboxパスcanonicalize警告あり。
- Native Host `cargo test --locked --quiet`：成功（exit 0）。`cargo test -- --list`で対象585件を確認、ignore属性なし。テストは既存の合成・一時データ用fixtureを使用する。
- 変更/追加JS/MJSの6ファイルの`node --check`、`git diff --check`：成功。
- `extension/manifest.json`は差分なし。`policy-csv.js`は既存Export/ダウンロード部分をそのまま残し、Import機能を末尾に追加したことを確認。
- Rustソース、MCP、visible-store、Worker、依存/権限は未変更。Options Pageには専用lint/typecheck設定がないため、新規ツールは導入しない。

### 実Host + 隔離Chrome（UI fixtureではない）

```powershell
node tests/dogfood/policy-csv-import-chrome.mjs '<Chrome for Testingのchrome.exe>' '<native-host.exe>'
```

Chrome for Testing `157.0.8103.50`、一時APP_DIR、一時Secure Vault、一時Chromeプロファイル、一時拡張コピーを使用。
本番Native Messaging登録は変更せず、拡張コピーのtransportだけをloopback bridgeへ置換する。Hostは上記buildした実バイナリ。
CDPで**Importボタンが実際にfile chooserを開いたこと**を観測し、一時CSVをfile inputへ選択する。controller直接呼び出しやFileのUI fixtureではない。

最終成功成果物：`%TEMP%\memocampus-23-import-chrome-ortcqa\result.json` / `import-before-apply.png` / 合成CSV。

確認済み:

- CSV構文エラーでHost検証を呼ばず、Draft変更ゼロ。
- CSVとして正常でも実Rustが不正Regex/未承認Wildcardを拒否し、全件変更ゼロ・秘密本文非露出。
- BOM、全4型、Unicode/引用符/カンマ/埋込CRLF、前後空白、空Regex、既存/CSV内重複を文字列のまま保持。
- 対象ゾーンのみ末尾追加し、Importごとにversionが1回だけ増える。
- Import前後のActiveファイル/ETagは不変、既存Blindは保持。
- 明示Applyの前は新ルールがNative判定に使われず、Apply後にだけ新Blind/Private判定へ変わる。
- Secure VaultのpublicationはImport後もApply後も`pending`。MCP承認・PSL承認要求は送信しない。

正常系Wildcardには同梱PSLのテストで承認不要と確認した`*.example.com`を使用し、ネットワークへアクセスしない。
初期の合成`.test`/非registrable subdomainが拒否された検証試行は期待どおりのHost制限であり、評価エンジンを変更して通していない。

### UI fixtureとの区別

`options-csv-import.test.js`はfake DOM/Hostによる障害・競合注入。Hostの判定正当性の証明には使わない。
file read/Host validate/status/get待機中の直接モデル編集、並行Import/対象切替、ETag変更、Host切断、不正応答、バージョン不一致、キャンセル、適用/再読み込み競合を確認する。
これとは別に、上記Chromeでは実Hostで正当性と保存境界を確認する。

## agy独立レビューの重点

1. CSV状態遷移、物理行番号、混在改行・不正UTF-8・制御文字・末尾処理、全件取り消し。
2. 入力/レコード/フィールド/候補サイズの上限と、制限前に無制限な処理がないこと。
3. `appendRulesAtomically`の1回更新、expectedVersion、他ゾーン/既存prefix不変、空Regex/重複のHost準拠。
4. file/validate/Active rereadの各await後の参照/世代/ETag確認、古いcallbackが別operationへ干渉しないこと。
5. 初回preset/v1/破損PolicyではImport不可、明示Apply/CAS・PSL/MCP承認の既存経路が維持されていること。
6. Import例外やHostエラーにpattern/ファイル名/パス/スタックが出ないこと、CSVセルがプロパティや実行コードとして扱われないこと。
7. Exportが変更されていないこと、manifest/権限/Rust評価コードに差分がないこと。untrackedのテスト/本文書もレビュー対象に含める。

## PO有人Chrome確認手順・未確認

1. テスト専用Windowsユーザー等に隔離Chromeプロファイル・一時APP_DIR・一時Secure Vault・テスト専用Native Host登録を用意する。本番登録を上書きしない。自動dogfoodのloopback検証とインストール済みNative Messaging検証を混同しない。
2. 既存の明示操作でPolicyを開始し、Secure VaultのMCP公開承認は保留にする。
3. 両ゾーンに別の合成既存ルールを作り、ExportしたCSVをImportする。重複を含め対象ゾーンにだけ末尾追加されることを確認する。
4. ファイル選択をキャンセルして無変更・無エラーを確認。不正CSV/不正Regex/未承認Wildcardも試し、行番号と全件無変更を確認する。
5. Import前後のActive/ETag/Vaultが不変で、明示「適用」後だけPolicyが変わること、MCP公開は別承認のままであることを確認する。
6. Import中の編集/適用/再読み込み停止、Host切断、他画面からのPolicy変更時の中止と変更保持を確認する。

有人のOSファイル選択ダイアログ/キャンセル、キーボード・スクリーンリーダー、インストール済み本番Native Messaging登録、企業ブラウザ制約は未確認。
保守的フィールド上限のため、Host単体では受け付け得る長い非RegexもImportでは拒否する。LF/CRLF混在、引用したヘッダー、C1制御文字も受け付けない。
独立レビュー・有人確認は未実施のため、総合PASSとはしない。POレビュー待ちで停止し、commit / push / PRは作成しない。
