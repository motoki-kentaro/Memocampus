# Issue #24 初回オンボーディング

対象: [Issue #24](https://github.com/motoki-kentaro/Memocampus/issues/24)。
実装基点は `origin/main` の `82a2c1001dbd3377eafd001054a0bf8e418264b5`。
ブランチは `feature/24-first-run-onboarding`。commit / push / PR は行わない。

## 利用者の操作と状態

サイドパネルは未初期化の場合だけ「初回案内を開く」を表示し、Options Pageへ誘導する。
Privacy設定を直接開いても同じ状態判定になる。案内を開くだけでは記録を開始しない。

| Hostの状態 | 表示・操作 | 永続化 |
|---|---|---|
| `uninitialized`（Policyと初期化マーカーの両方が存在しない） | 日本語の初回案内、Hostの `policy_preset(default)` を編集可能なDraftとして表示 | プリセット表示・編集・検証ではPolicyを書かない |
| 未初期化・Vault未選択または使用不可 | 既存のVault設定へのリンクと「Vaultを再確認」。開始不可 | Vaultを暗黙に作らない |
| 未初期化・使用可能なVaultあり | 既存Vaultを再利用。「記憶を開始する」で検証・状態確認・開始 | Hostのみが `policy_start` を通して書く |
| `ready`（v1/v2） | 通常のPrivacy設定。初回案内もプリセット再投入もなし | 明示的な通常編集以外では既存Policyを維持 |
| `invalid` / `unsupported` | 従来の停止・エラー表示。初回案内・開始操作を出さない | 復旧・初期化・上書きなし |
| Host切断・使用中・応答不正 | 状態未確定として停止。読取操作で再確認 | 開始を自動再送しない |

案内には自動記録するURL・タイトル・訪問日時・ページメタデータ、ローカルVault保存、
シークレットの自動記録除外、Blind Zone / Private Zoneの違い、既存履歴を自動削除しないこと、
プリセットの対象例と網羅性の限界、MCP/AI公開の別承認と接続先の取扱いを説明する。

初期候補の値はJS側の定数から作らずHostから取得する。#23の `createDraft`、ルール表、
URLチェッカー、カテゴリ・レイヤー編集、`validateDraft` を再利用する。
エディタ内部の比較基準としてプリセットを保持するが、画面の状態は未初期化のままとし、
ActiveのRevision/Versionを表示しない。下書きはページのメモリだけに置く。
Vault作成・選択は `sidepanel.html#settings` の既存Vaultパネルを別タブで開いて行う。

## agy向け: 開始・キュー配送・MCP公開の境界

| 境界 | 実装の追跡先 | 契約 |
|---|---|---|
| 開始の明示操作 | `extension/options-page.js`: `startRecording`, `checkStart`; `extension/onboarding-state.js`: `loadPreset`, `startDraft` | 最初のawait前に操作をロック。Draft全体をHost検証し現在状態も確認後、一度だけ `policy_start`。どの開始応答でも状態を再読込。別画面で開始済みならそのPolicyを表示し、Draftを上書き適用しない |
| Hostの開始確定 | `native-host/src/policy_gate.rs`: `start`, `initialize_store`; `native-host/src/policy_store.rs`: `start_with_then`, `initialize_inner` | 使用可能なVault、未初期化、Draft検証をHost側で確認。既存の排他lockの下で最終的に一度だけ書く。既存Policy/マーカーがあれば拒否 |
| 永続キューに入る前 | `extension/navigation-start.js`; `service-worker.js`: `onBeforeNavigate`, `onCommitted`, `onCompleted`; `policy-gate.js`: `evaluate`; Host `judge_batch` | `onBeforeNavigate` の開始時刻をメモリに保持し、commit時に `documentId` と結合、completion時にその文書の証拠だけを使用。`occurredAt` はナビゲーション開始時刻。開始境界以下または不明なら拒否。ページメタデータ取得、タイトルを含むイベント作成、キュー書込は許可後だけ |
| 配送時 | `native-host/src/main.rs`: `record_event`; `policy_gate.rs`: `decide_visit` | `recordingOccurredAt`（ナビゲーション開始時刻）を再検査。初回開始マーカーのあるPolicyでは欠落/不正時刻を `visitedAt` で補わず、開始境界以下と同様に `dropped: policy` ACKで破棄する。旧Policyにはこの追加条件を課さず、`visitedAt` の意味・並び順も変えない |
| 再起動・開始競合 | `policy_store.rs`: `recording_start`, `load_unlocked`, `current_etag` | 新しい開始マーカーに発生時刻の境界を永続保持。Workerのキャッシュや画面の存続に依存しない。通常Policy編集でも境界を維持 |
| MCP公開 | `policy_gate.rs`: `initialize_store` → `vault::withdraw_publication`; 既存の `vault_publication_confirm` / VisibleStore | 開始前に公開承認を撤回して `pending`。開始UIは公開承認を送らない。記録開始後も別途の明示承認が必要 |

新しい `policy.initialized` の形式は `Memocampus recording start v2\n<Unix milliseconds>\n`。
まず時刻未記載のマーカーをflushし、Policyを原子的に永続化した後、同じ排他lockの下で
開始境界を書いてflushする。途中停止したマーカーは `invalid` で、初回未設定や旧形式へ
フォールバックしない。同一ミリ秒のイベントも安全側で拒否する。`onBeforeNavigate` の
開始時刻が取得・対応付けできない場合も新規開始環境では拒否する。commitやcompletionの
時刻で補わない。未来のChrome時刻は開始イベントの受付時刻以下に抑える。
時刻の後退時は境界を越えるまで拒否するため、不確かな期間をバックフィルしない。

従来の `Memocampus policy initialization v1\n` マーカーは変更せず読める。
既存環境には開始時刻を後付けせず、Host障害・Vault不在時のPolicy判定、キュー保持・
再送、ACK消失時のidempotencyを維持する。新しいマーカーを作った環境では、旧版の
Host/MCPはマーカーを読めずfail-closedになる。拡張・Host・MCPはこの実装を含む版で揃える。

## 旧初期化ルートの棚卸し

| 従来の経路 | 現在 |
|---|---|
| サイドパネルの「プライバシー設定を初期化」 | 「初回案内を開く」へ変更。`openOptionsPage()` だけを呼ぶ。内部DOM idは互換のため維持 |
| Workerメッセージ `policy-initialize` | `policy_start_required` で拒否。Host呼出しなし |
| JS `createPolicyGate.initialize()` | 削除 |
| Native Messaging `policy_initialize` | 通常ビルドでは常に `policy_start_required`。Vaultの有無やPolicyの状態を問わず書かない |
| Rust内部の旧初期化API | 既存ユーザーのテストfixture用。Hostの旧ハンドラが成功するのは `cfg(test)` のユニットテストだけ。通常ビルドに成功ルートなし |
| 既存の実プロセステスト | 一時APP_DIRに旧Policy/マーカーを用意するfixtureへ変更。新規開始は `policy_start` を直接検証 |

CSV、破損Policyの復旧UI、旧暗号化Vaultの移行は対象外。

## 検証記録（2026-11-11）

実ユーザーのVault・Policy・ChromeプロファイルおよびNative Messaging登録は使用・変更していない。
自動テストは各自の一時APP_DIR/Vault、Chrome検証は専用の一時プロファイルを使用した。

| 検証 | 結果・追跡先 |
|---|---|
| 初回表示、プリセットだけ、Vaultなし、不正Draft、明示開始 | `tests/extension/onboarding.test.js`; `native-host/tests/stdio_protocol.rs` の `start_initializes...` |
| 二重押下・複数画面・切断・開始後の応答消失 | `onboarding.test.js`; `onboarding-process.test.js`; `stdio_protocol.rs` の `simultaneous_first_start_processes_have_one_winner` |
| 開始直前・遅延・Worker再起動・不明発生時刻 | `privacy-ingress.test.js`; `stdio_protocol.rs` の `first_start_fences...`; `policy_store.rs` の `an_interrupted_start_marker...` |
| 既存Policy保持・破損/消失の停止 | 同上、既存のPrivacy ingress/PolicyStore回帰テスト。旧ユーザーfixtureを含む |
| MCP公開承認の独立 | `onboarding-process.test.js`: 新規開始後は実MCPが `vault_publication_pending` で拒否。別途承認後は開始後の行だけ取得。`secure_vault_b0.rs` の公開撤回/再承認テストも成功 |
| Rust Native Host | `cargo test --locked --quiet --no-fail-fast`: 全成功（146 library + 293 binary unit + 149 integration = 588件、開始時刻修正後） |
| Rust VisibleStore | `cargo test --locked --quiet`: 115件成功、既存の2件ignored |
| Rust MCP | `cargo test --locked --quiet`: 57件成功 |
| 品質確認 | Native Host `cargo fmt --all -- --check`, `cargo clippy --locked --all-targets -- -D warnings`, `git diff --check` 成功 |
| JS全体（実Host/MCP有効） | `MEMOCAMPUS_TEST_HOST` / `MEMOCAMPUS_TEST_MCP` をdebugバイナリに設定し `node --test --test-concurrency=1 "tests/**/*.test.js"`: **763件成功、失敗0、skip0** |
| Chrome runtime | **PASS**。二重押下・応答消失・別画面の確認を通じ開始送信は1回。Worker再起動後もPolicyを保持 |

上記のテストは新規開始の結合テストと、既存Policyを持つ利用者の回帰テストを両方含む。

DPAPIを使う既存テストはサンドボックス内では `ProtectorFailed` となったため、
同じ隔離データで通常Windows権限で再実行して成功した。OpenSSLの既存LNK4099
（PDBなし）警告は出るが、テスト失敗や新しい依存追加はない。
全JSを多数並列で走らせた際は、既存DOMテストの短い固定待機が負荷で不足した。
コードを変更して隠さず、同じ実Host/MCPバイナリを使う直列実行で全件確認した。

### Chrome runtime dogfood

再実行: `node tests/dogfood/onboarding-chrome.mjs <Chrome-for-Testing.exe> <native-host.exe>`。
通常版Chromeが `--load-extension` を受け付けなかったため、公式Chrome for Testingを
一時ディレクトリに取得した。システムへのインストールや既存Chrome設定の変更はしていない。

ハーネスは拡張の一時コピーだけで通信をループバックのテスト用ブリッジへ差し替え、
一時APP_DIRで実Hostプロセスを起動する。プロダクトの拡張にはブリッジコードを含めない。
隔離Chrome上で日本語画面、Vault未選択、使用可能Vaultの再利用、開始前のstorage/DB不保存、
二重押下、開始応答消失、別画面の開始済み判定、開始後のDB保存、Worker停止・再起動を
確認した。再起動はWorkerのグローバルメモリ消去でも確認している。

スクリーンショットを目視し、案内・カテゴリ・ルール編集・開始ボタンの表示を確認した。
最新の証跡は一時フォルダ `memocampus-24-chrome-gokQLv/onboarding.png` に保持。

実機未確認: 通常のNative Messaging登録経由のChrome結合、WindowsのVaultファイル選択
ダイアログ、headfulでのキーボード/スクリーンリーダー操作、Chromeプロセス全体の再起動を
跨ぐ手動dogfood、独立した第三者レビュー。自動テストや通信ブリッジによる検証を
これらの完了とは扱わない。

## 追補: 開始前に始まり、開始後にcommit/completionするナビゲーションの修正

当初は `onCompleted.timeStamp` を開始境界と比較していた。そのため、開始前にHTTP
リクエストを送り、開始後にヘッダー・本文を受け取るナビゲーションが保存され得た。
当初の「完了イベントが開始と競合する」テストだけでは、このリクエスト開始との競合を
検出できていなかった。

現在は `navigation-start.js` がメインフレームの `onBeforeNavigate` を同期的に観測する。
commit時に文書へ結合し、completion時は同じ `documentId` の開始情報だけを取り出す。
一時的な対応付けはメモリのみで、URL/タイトルをログやsession/local storageへ書かない。
重複・provisional redirectでは古い開始時刻を残し、別ナビゲーションの新しい時刻へ
付け替えない。対応不明、情報の上限超過、Worker再起動、タブ置換では `null` を送り、
初回開始境界のあるPolicyではHostも拒否する。古いPolicyでは従来の判定・配送・再試行を
維持する。Chromeのイベント順序とdocumentIdについては
[公式webNavigation API](https://developer.chrome.com/docs/extensions/reference/api/webNavigation) を参照。

`tests/extension/navigation-start.test.js` は開始前/後、Worker再起動の位置、欠落、文書の
対応付け、重複、redirect、error、subframe、上限、タブ削除を検証する。
`stdio_protocol.rs` の `first_start_requires_navigation_start_evidence_but_legacy_delivery_keeps_its_contract`
は、開始証拠欠落/不正かつ訪問日時が将来でも新規開始環境は拒否し、旧Policyでは同じ
イベントを従来どおり保存することを実プロセスで確認する。

Chrome回帰テストは `tests/dogfood/onboarding-chrome.mjs` に追加した。専用プロファイル、
一時APP_DIR、一時Vaultのみを使用し、次の順序を明示的な同期点で制御する。

1. メインフレームのHTTPリクエストを受信し、レスポンスのヘッダーも送らず停止する。
2. `onBeforeNavigate` を観測し、まだ `onCommitted` / `onCompleted` がないことを確認する。
3. UIで「記憶を開始する」を押し、Hostによる開始確定を読み取る。
4. 停止中のレスポンスを解放し、commit/completionの両方が開始後に起きたことを確認する。
5. completionハンドラ、キューの排他チェーン、テスト観測の送信完了を待つ。対象URLが
   永続キュー、dead-letter、Vaultにないことと、local storageの全書込み履歴にも一度も
   含まれないことを確認する。テスト観測コードは拡張の一時コピーだけへ注入する。
6. 開始後に別の対照URLを新しく開き、実HostによるVault保存を確認する。
7. もう1つの開始前リクエストを、Worker停止・再起動後に解放する。開始証拠は `null` で
   拒否され、再起動後に新しく始めた対照ナビゲーションは記録されることを確認する。

`--completion-time-regression` は検出感度を確かめるnegative controlで、一時拡張コピーの
境界判定だけを旧完了時刻ロジックへ戻す。これは実際に
`pre-start navigation reached durable storage after release` で失敗した（期待する失敗）。
修正コードでは同じ回帰シナリオが成功した。通常のNative Messaging登録経由を検証して
いない点は、この追補でも同じ。

最終Chrome結果は **PASS**。最新の隔離データ・イベント時刻の証跡:

- `%TEMP%/memocampus-24-chrome-gokQLv/paused-before-start.json`
- `%TEMP%/memocampus-24-chrome-gokQLv/paused-start-lost-on-restart.json`

前者は `occurredAt <= startedAt`、後者は `occurredAt: null` で拒否され、両方で
`absentFromStorageAndVault: true` を確認した。対照URLがstorage観測とVaultの両方に
存在することも確認し、観測フックが動作していないために不保存と誤判定する経路を除いた。
