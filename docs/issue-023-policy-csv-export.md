# #23 CSV Slice 1: Privacy Policy CSV Export

## 範囲と既存構造

`origin/main` の `2fc13f8` から `feature/23-policy-csv-export` を作成。初回作業開始時の作業ツリーはクリーン。
追加修正は同じブランチの未コミット差分・untrackedファイルを保全して継続。PO決定に従い`kind,pattern`へ変更する。
CSV Exportのみ。Import・Policy評価・適用・永続化・Vault・MCP公開・権限・バックエンドAPIは変更しない。

- `options.html` / `options-page.js`: 既存のHTML Options Pageとイベント制御。追加依存なし。
- `options-state.js`: HostからのActiveをコピーしてfreezeし、`createDraft` は別コピーをページ内メモリで管理する。`draft.layer(key)` もコピーを返す。
- 通常の追加・編集はURLチェッカーでHostの一致確認後にDraftへ反映する。チェッカーに入力しただけの文字列はDraftではない。
- Draftの全体検証は`policy_validate`、適用は`policy_apply`と既存のETag/lock/原子的更新。初回開始は既存の`policy_start`。CSVはこれらを呼ばない。
- UIテストはNode組み込みtest + 既存fake DOM/Host。CSVも同じ基盤を使用する。

## 変更ファイル

| ファイル | 役割 |
| --- | --- |
| `extension/policy-csv.js` | 指定ゾーンのコピーから純粋なCSV生成、Blob + anchorダウンロード |
| `extension/options-page.js` | ゾーン別Exportイベント、状態/失敗表示。既存検証・適用ロジックは変更しない |
| `extension/options.html` | ゾーン別ボタン、Draft範囲・数式・プライバシー警告 |
| `tests/extension/options-fixtures.js` | ダウンロード処理を注入できる任意のテスト引数を追加 |
| `tests/extension/policy-csv.test.js` | 既存38テストを新形式へ更新し、内部データ不正・UI拒否の3テストを追加（計41テスト） |
| `tests/dogfood/policy-csv-chrome.mjs` | 隔離Chrome上の実ダウンロード検証（opt-in） |
| 本文書 | 設計・制限・検証・レビュー引き継ぎ |

## データとダウンロードの設計

`exportLayer(key)` → `draftZoneCsv(draft, key, { schemaVersion })` → `rulesToCsv(rules, { schemaVersion })` → `downloadCsv(...)`。
指定ゾーンの`rules`と、既にページが保持している形式の版（v1/v2）だけを使う。Activeの再取得も、他ゾーン・categories・enabled・承認ドメイン・ETag・Vault情報の出力もない。
Draftの`version`/`dirty`を変更せず、検証結果・処理中の検証・適用権限にも触れない。
全4型の不正なpattern文字列も構文検証せず出力する。正規化・trim・並べ替え・重複排除・数式対策の文字追加はしない。

- ヘッダー`kind,pattern`。既存の正式な識別子`pattern` / `regex` / `domainWildcard` / `exact`を型ごとにそのまま出力する。
- v1の読み取り専用文字列ルールは、その既存の意味どおり`pattern`として出力する。schemaVersion=1を明示した場合だけ文字列形式を認め、v2内の文字列ルールは不正データとして拒否する。Policy移行はしない。
- レコードはCRLF、最終レコードにもCRLFを付ける。空ゾーンは`kind,pattern\r\n`だけ。
- カンマ/引用符/CR/LFを含むフィールドは引用符で囲み、引用符を二重化する。フィールド内改行は変更しない。
- Blobは`text/csv;charset=utf-8`、BOMなし、既定のtransparent endings。ファイル名はゾーンに対して固定。
- 一時anchorをbodyに追加してclick後に除去。Blob URLは同じtask内で失効させず60秒後に解放する。ページ終了時もブラウザが解放する。click等の同期例外では即座に解放する。
- 例外はルール本文を含み得るので表示/ログに流さず、静的な日本語メッセージを表示してDraftを保つ。
- 標準anchor APIではダウンロード要求後のキャンセル/ディスク障害は検知できない。「保存成功」とは表示せず、保存完了をブラウザで確認するよう案内する。

## 全4型の識別子と不正データの扱い（PO決定反映済み）

識別子は`extension/options-state.js`の`RULE_KINDS`と照合した。Rust側も`native-host/src/policy.rs`の`TypedRule`に`serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)`が指定されており、同じ4型であることを確認した。
CSVモジュールはHost管理APIを持つ`options-state.js`に依存させず、正式なwire識別子を固定値で列挙する。テストでは編集モデルの`RULE_KINDS`全件を使い、識別子の不一致を検出する。既存のprivacy-ingress依存境界テストは緩和しない。

Domain Wildcard / Exact URLを理由とした拒否は撤廃。型・patternの対応、文字列、順序、重複を保持する。
未知のkind（大文字小文字が違う別名も含む）は`csv_unsupported_kind`として、ルールやkind本文を露出しない日本語の理由を表示する。
rulesが配列でない、ルールがobjectでない、kind/patternが欠落または文字列でない、余分なフィールド、v1/v2の形式不一致等は`csv_invalid_data`として拒否する。Policyの構文検証とは別の、CSVへ欠落や変換なく表現できる内部構造の確認に限る。
いずれも指定ゾーン全体のダウンロードを開始せず、部分出力・黙った変換・欠落を起こさない。他ゾーンは独立して出力できる。

## セキュリティ上の注意

CSV生成・ダウンロードはネットワーク、Host、storageを使わない。閲覧履歴、画像、鍵、Recovery Spell、MCP認証情報は入力にも含めない。
ただしルール自体にプライベートな文字列が含まれるため、保存先・共有先に注意が必要。
`= + - @`等の文字列をそのまま保持する以上、Excel等でのFormula Injectionリスクは残る。UIで明示し、テキストエディタでの確認を勧める。
CSVのエスケープはCSV構文の保証であり、表計算ソフトの数式実行を無害化するものではない。

## kind形式の追加修正検証記録（2026-11-11）

- CSVテスト: `node --test tests/extension/policy-csv.test.js` → 41成功、失敗/skipなし。
- `npm test`: 804件中791成功、13 opt-in skip、失敗なし。
- `MEMOCAMPUS_TEST_HOST` / `MEMOCAMPUS_TEST_MCP`に既存debugバイナリを指定した`node --test --test-concurrency=1 "tests/**/*.test.js"`: **804成功、失敗/skipなし**。実Host/MCP連携も一時APP_DIR・合成Policy/Vaultのみを使用し、実ユーザーデータには触れていない。
- JS/MJS構文チェック（今回変更した4ファイル）・`git diff --check`: 成功。
- 既存privacy-ingressテストがCSVからHost管理モジュールへの依存を検出したため、CSVモジュールを依存なしに修正し、テストを緩和せず全件再実行して成功した。
- 初回の旧形式での検証履歴（今回のkind形式の結果と区別する）: CSV38成功、`npm test`788成功/13skip、実Host/MCP指定801成功/skipなし、3 crate Rust fmt成功。Rustソース/依存は追加修正でも未変更で、`cargo test`/`cargo clippy`は未再実行。
- 今回のOptions Page（素のJavaScript）にはlint/typecheck専用スクリプト・設定がなく、新規ツールは追加していない。別の`worker/tsconfig.json`はこの変更の対象外。
- Chrome for Testing `157.0.8103.50`、最終コードの隔離プロファイルで以下すべて成功:
  - 合成Policyによる不正Draft/検証エラー、未適用変更、チェッカー未反映入力の除外。
  - Blind/Privateの個別ファイル、混入なし、UTF-8/BOMなし/CRLF/引用符/Unicode/空白/空文字/数式/順序/重複の実バイト比較。
  - 両ゾーンそれぞれに全4型を含め、`kind,pattern`および型とpatternの組み合わせを実バイト比較。未知kind/内部データ不正は理由付きエラーでダウンロードなし。
  - Export前後のDraft/Active/ETag/版/dirty/Native呼び出し履歴が不変。
  - `URL.createObjectURL`例外のUI表示・本文非露出・Draft保持・再試行成功、全件削除後のヘッダーのみ出力。
  - 一時APP_DIR/Vaultディレクトリは空のまま。実Host・実データには接続しないテスト専用transport。
  - 成果物: `%TEMP%\memocampus-23-csv-chrome-KUXzMI\result.json`、`options-csv.png`、`blind` / `private` / `retry-after-failure` / `empty`配下のCSV、拒否時の空の`refused`ディレクトリ。旧`memocampus-23-csv-chrome-X38Dui`は旧形式の成果物なので混同しない。

## Chromeでの再現・実機確認手順

自動再現（Chrome for Testing使用。実ユーザープロファイルは指定しない）:

```powershell
node tests/dogfood/policy-csv-chrome.mjs '<Chrome for Testingのchrome.exe絶対パス>'
```

スクリプトは一時プロファイル/拡張コピー/APP_DIR/Vaultディレクトリを毎回新規作成し、プロセスを終了する。Registry/実Native Host登録を変更しない。
テスト用transportは読み取り/チェッカー/検証の応答をスクリプト化しており、本番Hostの正当性を証明するものではない。

POによる有人確認（未実施）:

1. テスト専用Windowsユーザー等で、隔離Chromeプロファイル・一時APP_DIR・一時Vault・テスト専用Native Host登録を用意する。既存の実ユーザー登録を上書きしない。
2. 拡張を読み込みOptions Pageへ。全4型の合成Policyを用意し、両ゾーンへ別の合成ルールを置く。
3. ルールを追加/編集して未適用のまま両Exportをクリック。ファイル名と内容、ゾーン分離をテキストエディタで確認する。Excelでは開かない。
4. 空ゾーン、不正Draft（現UIの通常編集では未検証文字列はチェッカー止まり）、重複・埋込改行等は上の自動スクリプトの合成fixtureでも確認する。
5. Export前後のDraft、Active、ETag、保存済みPolicy、Vaultが変わらないことを確認する。ダウンロード保存先選択・キャンセル・ブラウザ側ブロック時の表示も確認する。
6. 全4型のkindとpatternの対応を確認する。未知の型・不正な内部データは自動スクリプトのテスト注入で拒否され、正常な別ゾーンは出力できることも確認する。

## agyへの引き継ぎ・未確認

- 対象はこのブランチの未コミット差分とuntrackedファイル。追加修正でもこれらを保全。commit/push/PRは行わず独立レビュー待ちで停止する。
- CSVからHost/Policy評価・適用・Vault・MCP公開へ到達する経路が追加されていないこと、他ゾーンの混入、例外本文の漏出、Blob URLの寿命を重点確認する。
- 旧`regex,pattern`形式・正式な既存型を理由とする出力拒否が残っていないことを確認する。未知kind/不正内部データの拒否は維持する。Import、CSV適用、Policy評価変更などへこのSliceを拡張しない。
- Chromeランタイム検証はheadlessの実ダウンロードであり、有人の保存ダイアログ/キャンセル、インストール済みNative Messaging登録、企業ダウンロードポリシー、実Hostと接続した有人UIは未確認。
- CSV保存の成功/失敗の最終結果はブラウザに委ねる制限がある。後続の書き込み失敗をアプリの成功扱いにしていないことを確認する。
- 独立レビューおよびPO確認前の総合PASSとはしない。
