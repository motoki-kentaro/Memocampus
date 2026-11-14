# Issue #6 Slice 3 — MHTML原本エクスポート／最終統合の引き渡し

実装完了候補。**総合PASSではない**。POのChrome／Windows実機確認と独立レビューは未実施。
Slice 1・2の引き渡しは `issue-006-slice-1.md`、`issue-006-slice-2.md`。本書はSlice 3の差分と、
Slice 1〜3を通した確認手順を記録する。

## 1. Git／変更範囲

開始・終了ブランチは `feature/6-page-vault`、HEADは `9fe97eb`（`origin/main` と同じ）。
Slice 1・2の未コミット／未追跡ファイル、既存の `.gitignore` 変更、保留PoC（`poc/`）を保全。
commit / push / PRなし。実Vault・実Chromeプロファイル・本番Native Messaging登録は使用／変更していない。
Schema Version 8のまま（テーブル・制約・インデックスの変更なし、移行・互換レイヤーなし）。

Slice 3で追加したファイル:

- `native-host/src/page_manage/export.rs`：書き出しの手順（対象固定→ダイアログ→再検証→ストリーミング→確定）と初期ファイル名。
- `native-host/src/page_manage/export/file.rs`：保存先のハンドル固定、一時ファイル、ハンドル経由のrename／削除、読み戻し検証。
- `native-host/src/page_manage/export/tests.rs`：18テスト（一時Vault・一時フォルダーのみ）。
- `scripts/measure-page-export.ps1`：128MiB書き出しのピークメモリ／時間の計測。
- 本書。

Slice 3で更新したファイル:

- `native-host/src/dialog.rs`：`Dialogs::choose_page_export_file`（既存のrfd保存ダイアログ実装を再利用）。
- `native-host/src/main.rs`：`page_export` のdispatch、ポート生存確認 `port_open`。
- `native-host/src/page_manage.rs`：Vault照合（`bind`）と応答整形（`reply`）を関数に切り出して書き出しと共用。動作は不変。`Event` に `PartialEq`。
- `native-host/src/page_protocol.rs`：`generation_for(env)` を追加（既存 `generation` はそれを呼ぶ）。
- `native-host/Cargo.toml`：既存依存 `windows-sys` に機能 `Win32_System_Pipes` を追加。**新しいcrateの追加なし**、`Cargo.lock` 変更なし。
- `extension/page-client.js`：`export()`、タイマーなし要求、閉じた接続の遅延エラーが後継接続を壊さない修正（§8）。
- `extension/page-panel.js`、`page-text.js`：各保存履歴の「MHTMLを書き出す」、待機中の操作無効化、通知文言。
- `tests/extension/page-fixtures.js`、`page-client.test.js`、`page-panel.test.js`、`page-process.test.js`：fixtureとテスト追加。

`manifest.json` は変更していない（Chrome権限の追加なし）。HTML／CSSの変更なし。

## 2. 書き出しの契約

- 対象は**1つの保存イベント**。要求はイベントID・save ID・SHA-256の3つを指定し、Hostは保存済みの行と
  すべて一致する場合だけ書き出す。rowidの再利用、別Vaultの同じID、原本の差し替えでは書き出さない。
- 書き出すのはVault内の原本BLOBそのもの。Chromeでの再取得・HTML変換・改行／エンコーディング変更なし。
  同じ原本を共有する別イベントの有無に影響されない。
- 保存先とファイル名はHostが表示する**Windows標準の保存ダイアログ**で選ぶ。種別は「MHTML Web Archive (*.mhtml)」、
  開始フォルダーはドキュメント。拡張子なしの名前には `.mhtml` が付く（ダイアログの既定拡張子）。
- **上書き確認はOSダイアログだけ**（`FOS_OVERWRITEPROMPT`。rfdは保存ダイアログの既定オプションを変更しない）。
  Memocampus独自の確認モーダルはない。キャンセルは正常終了で、ファイルもVaultも変更しない。
- Chromeへ返すのは結果だけ（ファイル名、長さ、SHA-256）。フォルダーのパスも原本のバイトも返さない。
- 書き出しはVaultに対して読み取り専用。保存イベント・BLOB・公開状態を追加／変更／削除しない。
  VisibleStore／MCPへの経路は追加していない。Privacy Policy（neverSave／saveButNoMcp）は書き出しを妨げない。

### 初期ファイル名

保存時タイトルだけから作る（URLは使わない）。

- `< > : " / \ | ? *` は `_` に置換。制御文字・改行・空白の連続は空白1つ。
- 双方向制御・ゼロ幅などの不可視文字（U+202E等）は除去。
- 先頭・末尾のドットと空白を除去。末尾の `.mhtml` / `.mht` は除去してから付け直す（重複なし）。
- `.mhtml` を除いて最大120 UTF-16単位（文字境界で切る）。
- Windows予約名（`CON`、`NUL`、`COM1`、`LPT9`、`COM¹`、拡張子付きを含む）は先頭に `_`。
- 空になった場合、またはタイトルがURLに見える場合（`://` を含む、または保存URLからスキームを除いた文字列と一致。
  タイトルのないページはURLがタイトルになる）は固定名 `memocampus-page.mhtml`。

これは提案にすぎず、最終的な名前はユーザーがダイアログで決める。ダイアログが返した名前も、予約名・末尾ドット／空白・
禁止文字・255単位超は拒否する（実パスは `\\?\` 形式でWindowsが正規化しないため）。

## 3. 手順とVault境界

1. ConfigLockの下でVault・鍵・Schemaを新規に確認し、`vaultToken` と `selection_generation` を要求と照合。対象イベントを読む。
2. **ロック・接続・トランザクションをすべて手放してから**ダイアログを開く（表示中に保持するものはない）。
3. ダイアログが戻った直後に保存先をハンドルで固定（§4）。要求元ポートが閉じていれば何も書かずに終了。
4. 再びConfigLockを取り、Vault・鍵・Schema・`selection_generation` を再確認（A→B→Aは別の世代なので拒否）。
5. 1つの読み取りトランザクション内で、対象イベントが手順1と完全に同じであることを確認。
6. 保存先フォルダーに一時ファイルを排他作成し、incremental BLOB I/Oで1MiBずつ書く。長さとSHA-256を照合。
7. 最終パスを変える直前に `selection_generation` とイベントをもう一度確認。
8. ハンドル経由で最終名へ確定し、最終パスのファイルを読み戻して長さとSHA-256を検証。
9. その後で成功を応答。

手順4〜8の間はConfigLockを保持するので、Memocampusの他プロセスはVault選択を変更できない。ロックを無視して
`config.json` を書き換えられた場合も手順7で検出する。読み取りトランザクションを保持するので、書き出し中の削除は
割り込めない（削除側が待つ）。ダイアログ表示中に対象イベントが削除／置換された場合は手順5で中止する。

エラーは固定コードのみ。`vault_switched`、`vault_locked` などVault系のエラーでは、Slice 2と同じくHost側の
cursor／確認状態も破棄する。

## 4. 上書き・TOCTOU・原子的確定（Windows）

保存ダイアログはハンドルを返さないため、**戻った直後に**次のハンドルを取り、以後はパスではなくハンドルに対して操作する。

| 対象 | 開き方 | 効果 |
| --- | --- | --- |
| 保存先フォルダー | `FILE_SHARE_DELETE` なし。実パス（リンク／ジャンクション解決後）で以後を指定 | フォルダーと全祖先はrename・削除・ジャンクション差し替え不可 |
| 既存の同名ファイル | `DELETE` アクセス、`FILE_SHARE_READ` のみ、リンクは辿らない | 他プロセスは変更・削除・rename・置換できない |
| 一時ファイル | `CREATE_NEW`、`.memocampus-<UUID>.tmp`、書き込み中は削除予約 | 既存名を辿らない。Hostが強制終了しても平文が残らない |

確定の順序:

- **新規**（戻った時点で名前が空いていた）：一時ファイルをハンドル経由で最終名へrename。置換は**しない**ので、
  その間に同名ファイルが作られていれば `export_file_appeared` で中止（作られたファイルは変更しない）。
- **上書き**（戻った時点で同名ファイルがあった＝OSが上書きを確認した）：固定した旧ファイルをハンドル経由で
  `<名前>.<12桁>.memocampus-old` へ退避 → 一時ファイルを最終名へrename（置換なし）→ 読み戻し検証 → 旧ファイルを
  ハンドル経由で削除。途中で失敗したら旧ファイルを最終名へ戻す。戻せない場合（空いた瞬間に別のファイルが置かれた等）は
  `export_recovery_needed` とし、旧ファイルは `.memocampus-old` のまま残す。**どの経路でもパス指定の上書きはしない。**
- 使用中のファイルは `export_target_busy`、読み取り専用は `export_target_readonly`、フォルダーやシンボリックリンクは
  `export_invalid_target` で、いずれも変更しない。
- 検証に失敗した場合は書き出したファイルを取り除いて元の状態へ戻し、`export_verify_failed`（成功通知なし）。

### 保証できない範囲（OS標準ダイアログの契約による）

1. **OSの上書き確認から、Hostが最初のハンドルを取るまでの間**。ダイアログは確認した対象の識別情報を返さないので、
   この間にファイルが作成・差し替えされた場合、Hostは「確認済みの既存ファイル」と区別できず置換する。逆にこの間に
   削除された場合は新規作成になる。ハンドル取得はダイアログ復帰後の最初の処理で、窓は最小だがゼロではない。
   これを閉じるには、ダイアログが開いている間に対象を固定する独自のCOM実装（`IFileDialogEvents::OnFileOk`）が必要で、
   rfdでは提供されない。採用するかはPO判断（今回は実装していない）。
2. **上書き時、旧ファイル退避と新ファイルrenameの間は最終名が存在しない**（ごく短時間）。この間にHostやOSが停止すると、
   旧ファイルは `.memocampus-old`、新ファイルは一時名で残り得る。内容は失われないが自動復旧はしない。
   「最終名が常に存在する」1操作の置換（`ReplaceFileW`／`MOVEFILE_REPLACE_EXISTING`）はパス指定であり、
   固定したファイルと置換対象の同一性を保証できないため採用していない。
3. 置換後のファイルは新しいファイルで、旧ファイルのACL・属性・代替データストリームは引き継がない。
4. ネットワーク共有、FAT/exFAT、クラウド同期フォルダーでのハンドル操作は未検証。失敗時は変更せずに `export_failed` となる想定。
5. OS停止・電源断では、削除予約中の一時ファイルが残る場合がある（プロセス終了では残らない）。

## 5. Native API

Slice 2と同じ持続ポート（1ポート＝1 Hostプロセス）で送る。

```text
page_export {requestId, vaultToken, generation, id, saveId, sha256}
  -> page_export_result {ok:true, id, cancelled:true, vaultToken, generation}
  -> page_export_result {ok:true, id, exported:true, fileName, byteLength, sha256, previousFileLeft, vaultToken, generation}
  -> page_export_result {ok:false, error}
```

`previousFileLeft:true` は、新ファイルは検証済みで確定したが、置換前のファイルを削除できず `.memocampus-old` として残っている状態。

追加したエラーコード: `page_export_changed`、`page_blob_corrupt`、`export_file_appeared`、`export_target_busy`、
`export_target_readonly`、`export_invalid_target`、`parent_missing`、`export_failed`、`export_verify_failed`、
`export_recovery_needed`。クライアント側のみ: `page_export_unknown`。

接続:

- ダイアログ表示中、Hostは他の要求に応答しない（既存の画像書き出しと同じ）。UIはPageの操作を無効化し、5秒pollも止める。
- 書き出し要求にはタイマーを付けない（ダイアログは何分でも開いていられる）。終了条件は応答かポート切断のみ。
- 応答前にポートが切れた場合、UIは「書き出し状態未確定」と表示し、**再送しない**。
- ダイアログ表示中にポートが閉じた（パネルを閉じた、UIでVaultを変更した）場合、Hostはダイアログ復帰後に
  stdinの状態を確認し、何も書かずに終了する。ダイアログ自体はユーザーが閉じるまで画面に残る。
- Hostは書き出しの状態を保持しない。Host再起動後に何かが再開されることはない。

## 6. UI

- 各保存履歴の行に「MHTMLを書き出す」。削除ボタンの前に置き、URLグループの「元のWebページを開く」とは別の行・別の文言。
  URLグループ単位の書き出しはない。
- 実行中は書き出し・削除・更新・続き取得・検索入力を無効化し、二重実行しない。ステータスに保存ダイアログの案内を表示。
- キャンセルはステータス表示のみ（エラー表示にしない）。成功はファイル名付きのステータス、失敗・未確定はエラー欄。
- 完了後、押したボタンへフォーカスを戻す（ユーザーが別の場所へ移動していなければ）。
- ファイル名・タイトル・URLはすべて `textContent`。MHTMLのプレビュー・レンダリング・スクリプト実行はない。
- 文言は既存方式どおり `page-text.js` に集約。

## 7. 大容量

- 原本は1MiBのバッファ1つで読み書きする（バッファはzeroize）。128MiB全体をメモリへ載せない。Native Messagingで原本を返さない。
- 計測（合成データ、平文Vault、Releaseテストハーネス、PowerShell 7.6.10、`scripts/measure-page-export.ps1`）:
  128MiBの書き出し（ストリーミング＋sync＋確定＋読み戻し検証）**0.38秒**、プロセスのピークワーキングセット **24.07MiB**
  （128MiBの原本投入もチャンク単位で行うテストなので、この値が書き出し時の上限）。他の負荷と同時の1回計測で、速度の保証値ではない。
- 0バイトの原本はSchemaのCHECKと保存処理が拒否するため存在しない。最小は1バイト。

## 8. Slice 2コードへの変更

- `page_manage.rs` の `answer` から、Vault照合と応答整形を `bind` / `reply` に切り出した。処理内容は同じ。
- `page-client.js`：`close()` された時点で保留中だった要求が後から `vault_switched` で失敗したとき、その後に作られた
  新しい接続まで閉じていた。書き出し要求は長時間保留されるため顕在化する。要求開始時の接続世代と一致する場合だけ
  閉じるようにした（Vault変更中に書き出しダイアログが開いていても、新しいVaultの一覧が消えない）。

## 9. 自動検証

実行結果は末尾の「実行記録」を参照。追加したテストの対象:

- Host（18件）：原本バイト完全一致（CRLF・NUL・0xFF・BOM、チャンク境界）、長さ／SHA、同一URLの複数スナップショットと
  共有BLOBからの選択、キャンセル／上書き拒否、上書き承認（書き出し中に他者が変更・削除・rename・フォルダーrenameできないこと）、
  ダイアログ後に現れたファイル、退避後に名前を奪われた場合、各段階の障害注入（一時ファイル作成・書き込み途中・確定直前・
  rename・検証・旧ファイル削除）と後始末、不正な保存先、ジャンクション差し替え・シンボリックリンク・ハードリンク、
  A→B→A、ロック保持と世代再確認、ダイアログ中の削除／rowid再利用／新規追加、書き出し中の削除の排他、破損BLOB、
  ポート切断、Host再起動、Secure Vaultの鍵ロック、128MiB、初期ファイル名（トラバーサル・予約名・制御文字・長大・URL風）。
- JS（8件）：要求の内容と1回だけの送信、タイマーなし、応答喪失＝未確定で再送なし、別イベント／別原本の応答を成功にしない、
  エラーコード、A→B→A、行ごとの対象指定、待機中の無効化と二重実行防止、キャンセル／失敗／未確定の表示、
  Vault変更で旧応答を破棄、ファイル名の安全な描画、権限追加なし。
- 実Host（既存の実プロセステストに追加）：`page_export` のdispatchと、ダイアログに到達しない拒否経路
  （SHA違い・save ID違い・存在しないID・不正ID）。

**自動テストでは実際のWindowsダイアログを操作していない。** ダイアログの答えはテスト用の差し替えで注入し、ファイルI/Oは
実物を使っている。OSの上書き確認の表示、ダイアログの前面表示、実Chromeでの操作は§10でPOが確認する。

## 10. PO実機確認手順（全項目未実施）

隔離Chromeの準備は `issue-006-slice-2.md` §8と同じ（別名のテストHost、temp APP_DIR、tempプロファイル）。
Hostは今回のReleaseビルドを使う。保存先には空のtempフォルダーを使う。

書き出し:

1. Pageタブで保存履歴を開く（同じURLを2回以上保存しておく）。各行に「MHTMLを書き出す」があること。
2. 古い方の履歴で「MHTMLを書き出す」を押す。Windowsの保存ダイアログが前面に出ること（Chromeの背後に出た場合は記録）。
3. 種別が「MHTML Web Archive (*.mhtml)」、初期ファイル名が保存時タイトル由来であること。タイトルに `/ : ?` 等を
   含むページ、タイトルのないページ（`memocampus-page.mhtml` になる）も確認。
4. ダイアログ表示中、Pageタブの他の操作が無効で、URL／Imageタブは操作できること。
5. 保存する。ステータスにファイル名が出て、保存先にファイルがあること。フォルダーに `.memocampus-*.tmp` や
   `.memocampus-old` が残っていないこと。
6. **原本との一致**：PowerShellで `(Get-FileHash <ファイル> -Algorithm SHA256).Hash.ToLower()` を取り、
   履歴の行のサイズ（bytes）とファイルサイズが一致すること。同じ履歴をもう一度別名で書き出し、2つのハッシュが一致すること。
   新しい方の履歴を書き出し、（内容が違えば）ハッシュが異なること＝選んだ履歴が書き出されていること。
7. **表示の確認（別項目）**：書き出したファイルをChromeで開き、内容を確認する。表示が崩れる・開けない場合でも、
   手順6が一致していれば原本破損とは判断しない（MHTMLの表示はChromeの仕様とサイト構造に依存する）。
8. 同じ名前でもう一度書き出す。**OSの**上書き確認が出ること（Memocampusの確認画面は出ない）。
9. 「いいえ」を選びダイアログをキャンセルする。既存ファイルの更新日時・ハッシュが変わらないこと。ステータスはキャンセル表示。
10. もう一度同じ名前で書き出し、「はい」で上書きする。ファイルが原本に置き換わり、余分なファイルが残らないこと。
11. 上書き先をメモ帳以外のプログラム（例：`$f=[IO.File]::Open(<ファイル>,'Open','Read','Read')`）で開いたまま上書きを試み、
    「使用中」のエラーになりファイルが変わらないこと。読み取り専用属性でも同様。
12. 書き込めないフォルダー（例：`C:\Windows`）を選び、エラーになること。

境界:

13. 書き出した履歴を削除し、同じURLの他の履歴と、書き出し済みファイルが残ること。
14. ダイアログを開いたまま設定でVaultをBへ切り替え、Aへ戻し、ダイアログで保存する。ファイルが作られないこと
    （既存ファイルを選んだ場合は変更されないこと）。Pageタブに古い成功表示が出ないこと。
15. Secure Vaultで、ダイアログを開いたままVaultをロックし、保存する。ファイルが作られないこと。
16. ダイアログを開いたままSide Panelを閉じ、ダイアログで保存する。ファイルが作られないこと。
17. ダイアログを開いたまま別ウィンドウ（同じVault）で対象履歴を削除し、保存する。中止されること。
18. 大きなページ（数十MiB以上）を保存して書き出し、手順6の一致を確認。可能なら128MiB近いページ。

統合・非回帰:

19. Slice 1・2のチェックリスト（保存、複数回保存、グループ展開、個別／URL／フィルター削除、共有原本の保持と回収）。
20. URL／Imageタブの検索・保存・削除・画像の書き出し、Privacy Gateの非回帰。
21. neverSave／saveButNoMcpのサイトで保存したPageも書き出せること。書き出しても公開承認やMCPの検索結果に現れないこと。
22. 平文／Secure Vaultをバックアップ・復元し、復元後のVaultでPageの一覧・書き出し（ハッシュ一致）・削除ができること。

終了時の後片付けは `issue-006-slice-2.md` §9末尾と同じ。

## 11. 残存リスク・未解決事項

- §4「保証できない範囲」の5項目。特に1（OS確認からハンドル取得まで）は標準ダイアログを使う限り残る。
- 実ダイアログ・実Chromeでの動作は未確認。Hostは背景プロセスなので、ダイアログがChromeの背後に出る可能性がある
  （既存の画像書き出しと同じ仕組みで、最前面の隠しウィンドウを親にしている）。
- ダイアログが開いている間、そのポートのPage操作は待たされる。ダイアログを放置するとPageタブは待機表示のままになる
  （ダイアログを閉じれば戻る。Side Panelを開き直すと新しい接続になる）。
- 要求元ポートが閉じた後もダイアログは画面に残る（閉じても何も書かれない）。
- 書き出したファイルは平文のMHTMLで、Memocampusの管理外になる（ユーザーの明示操作の結果）。認証後の内容や
  スクリプトを含み得る。Secure Vaultの暗号化は及ばない。
- Windows専用。Windows以外のビルドでは書き出しを拒否する（弱い方式へのフォールバックはしない）。
- Slice 2の残存制約（全メタデータ走査のO(N)、他プロファイル変更のpoll遅延、Host再起動をまたぐ削除receiptなし）は不変。
- MHTMLのExtension内表示、本文抽出・全文検索、Page情報のMCP公開は対象外のまま。

## 12. 独立レビューの重点

- `export/file.rs` の共有モードとアクセス権、ハンドル経由rename（`FILE_RENAME_INFO`、置換なし）、削除予約の解除と再設定、
  失敗時の復旧順序、`unsafe` 3箇所。
- 手順4〜8のロック順序（ConfigLock → 接続 → 読み取りトランザクション）と、ダイアログ表示中に何も保持していないこと。
- 要求の3点照合（ID・save ID・SHA）と、ダイアログ前後の `Event` 完全一致。
- 初期ファイル名とダイアログ戻り値の検証。
- `page-client.js` の接続世代の修正がSlice 2の無効化動作を変えていないこと。
- テスト専用フック（`#[cfg(test)]` の `HOOK`）が製品ビルドに含まれないこと。
- 未追跡の新規ソース・テストもレビュー対象。`git diff` だけではSlice 1〜3全体は見えない。

以上をPO実機確認と独立レビューへ引き渡し、commit / push / PRをせず停止する。

## 実行記録（2026-11-14、合成データ・一時Vaultのみ）

| 対象 | 結果 |
| --- | --- |
| JS（ReleaseのHost／MCPを指定、全opt-in） | 933成功、0 skip、失敗0 |
| JS（通常の `npm test`） | 933件中919成功、条件付き14 skip、失敗0 |
| Native Host `cargo test --locked` | 633成功、0 ignored（Slice 2時点615＋書き出し18） |
| VisibleStore `cargo test --locked` | 116成功、既存measurement 2 ignored |
| MCP server `cargo test --locked` | 57成功、0 ignored |
| 3 crateの `cargo fmt --check` | 成功 |
| 3 crateの `cargo clippy --locked --all-targets -- -D warnings` | 成功 |
| 3 crateの `cargo build --release --locked` | 成功 |
| `git diff --check` | 成功（未追跡の新規ファイルも行末空白なし） |

- VisibleStore／MCPのReleaseビルドはOpenSSLの再ビルドを伴い、ネイティブPerl（Strawberry）の指定が必要だった
  （Git BashのMSYS版Perlでは失敗する。`docs/secure-vault/slice-b.md` の既知事項）。
- OpenSSLの既存LNK4099（PDB欠落）warningは残る。Rust lintの失敗ではない。
- シンボリックリンクのテストケースは、作成権限がない環境ではその部分だけスキップしてログに出す（ジャンクションとハードリンクは常に実行）。
- ログ（ignored）：`.tmp-6-s3-{native-host,visible-store,mcp-server}-tests.log`、`.tmp-6-s3-js-real-tests.log`、
  `.tmp-6-s3-export-std{out,err}.log`。
