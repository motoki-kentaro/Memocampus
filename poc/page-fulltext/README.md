# PageVault 全文検索 PoC — Slice 1（MHTML 本文・メタデータ抽出）/ Slice 2（SQLite FTS5 検索）

**製品コードではありません。** Memocampus の Page（コード上の正式名称。PageVault）に保存される
Chrome 製 MHTML から、検索用の本文テキストとメタデータを安全に取り出せるかを検証する独立 CLI です。
製品 crate・拡張・Vault・Native Host には一切リンクせず、何も変更しません。結果は
[`reports/slice-1-results.md`](reports/slice-1-results.md)（抽出）と
[`reports/slice-2-results.md`](reports/slice-2-results.md)（検索索引）。

**Slice 2** は、抽出結果を PoC 専用の SQLite データベース（FTS5 trigram）に索引し、「検索文字列が本文または
タイトルにリテラルな部分文字列として存在するか」を判定します。既存 Vault には接続しません。仕様の要点は
[検索（Slice 2）](#検索slice-2) の節、詳細はレポート XIV。

**現行の抽出方式は html5ever の tokenizer による逐次抽出です（DOM ツリーは作りません）。** Slice 1 当初の
DOM 方式（scraper）は廃止しました。経緯と比較はレポート XIII。全文検索の目的は「検索語がページ本文に
あるか」の判定であり、ブラウザの表示やレンダリングの再現ではありません。

## 使い方

```powershell
cd poc\page-fulltext
cargo build --release
# 既定：要約（サイズ・件数・文字コード・警告コード・トークン数・時間・メモリ）だけを表示。本文・メタデータは出さない
.\target\release\page-fulltext.exe --input C:\path\page.mhtml
# 本文を含む JSON は、存在しない出力先を明示したときだけ作る（既存ファイルは上書きしない）
.\target\release\page-fulltext.exe --input C:\path\page.mhtml --output C:\path\new-result.json
cargo test --release
```

終了コード：0 成功 / 1 入力拒否（stderr に `status=error code=<code>`。ページ由来の文字列は含まない）/
2 使い方の誤り / 3 出力先の問題（既存・書き込み失敗）。失敗時に出力ファイルは作られません。

入力は利用者が指定したローカルファイル 1 個だけです。URL 取得・標準入力・外部通信・JavaScript 実行はありません。
拡張子では判定せず、MIME 構造（最上位が `multipart/related`）で判定します。

## 構成

| ファイル | 役割 |
|---|---|
| `src/mime.rs` | mail-parser で解析し、厳格検証してルート HTML パートを選ぶ。ヘッダ由来文字列の上限・制御文字検査 |
| `src/charset.rs` | BOM → MIME charset → `<meta>` プレスキャン → 既定 UTF-8 の順で文字コードを決め、64 KiB ずつ UTF-8 に変換（復号後サイズを数えて上限で拒否） |
| `src/html.rs` | html5ever tokenizer の TokenSink。内容モデルの切り替え、開いている要素の名前スタック（上限あり）、本文の区切りと除外 |
| `src/metadata.rs` | title / description / keywords / og:title / og:description / article:tag をトークンから収集 |
| `src/limits.rs` | 資源制限とエラーコード |
| `src/lib.rs` | 全体の流れと JSON 出力契約（`memocampus.page-fulltext.poc/2`） |
| `src/main.rs` | CLI、ヒープ使用量の計測（計測専用アロケータ）、Windows のピークワーキングセット |
| `tests/integration.rs` | 合成 MHTML と Chrome 製 fixture によるテスト（Slice 1 当初からの 31 件） |
| `tests/tokenizer.rs` | tokenizer 方式で追加したテスト（22 件） |
| `tests/end_tags.rs` | 独立レビュー指摘（HIGH-1 / HIGH-2 / MEDIUM-1）と同種の状態管理の回帰テスト（12 件） |
| `src/index.rs` | Slice 2：SQLite スキーマ、登録・更新・削除・再構築・整合性検査、検索（FTS5 候補＋リテラル照合 / 全件走査） |
| `src/quality.rs` | Slice 2：品質ゲート（置換文字・UTF-8 の誤読・除去された制御文字の計測と判定） |
| `src/bin/page_index.rs` | Slice 2 の CLI（`page-index`）。登録・検索・削除・再構築・検査・品質計測・ベンチマーク |
| `tests/search.rs` | Slice 2 のテスト（32 件＋実 MHTML 受入試験 1 件。受入試験は既定では実行しない） |
| `tests/open_safety.rs` | Slice 2 独立レビュー指摘 HIGH-1 / MEDIUM-2 の回帰テスト（11 件：拒否するファイル、読み取り専用） |
| `tests/cli_guard.rs` | Slice 2 独立レビュー指摘 MEDIUM-1 / MEDIUM-2 の回帰テスト（7 件：Git 作業ツリー、CLI の副作用） |
| `tests/journal.rs` | rollback journal の検証（4 件：異常終了・復旧・原子性。子プロセスを異常終了させる） |
| `tests/fixtures/chrome/` | **合成ページ**を Chrome に保存させた MHTML（実サイトではない） |
| `tools/pages/` | 上記の元になった合成 HTML（`sjis-news.html` は CP932 バイト列。元テキストは `.utf8.html`） |
| `tools/chrome-capture.mjs` | 合成ページを 127.0.0.1 で配信し、headless Chrome に MHTML を作らせるスクリプト |
| `tools/make-synthetic.mjs` | 性能計測用の大きな合成 MHTML を作るスクリプト（出力は Git 管理外の場所へ） |

## ライブラリ選定

| crate | 版 | ライセンス | 用途 | 判断 |
|---|---|---|---|---|
| mail-parser | 1.0.1 | Apache-2.0 OR MIT | MIME 構造 | 採用（下記の条件付き） |
| encoding_rs | 0.8.44 | (Apache-2.0 OR MIT) AND BSD-3-Clause | 文字コード（`Decoder` で逐次変換） | 採用 |
| html5ever | 0.40.1 | MIT OR Apache-2.0 | `html5ever::tokenizer` のみ（`Tokenizer` / `TokenSink` / `BufferQueue`）。tree builder は使わない | 採用 |
| serde / serde_json | 1.x | MIT OR Apache-2.0 | JSON 出力 | 採用 |
| rusqlite（`bundled`） | =0.32.1 | MIT | Slice 2 の SQLite（libsqlite3-sys 0.30.1 同梱の SQLite 3.46.0、`SQLITE_ENABLE_FTS5` 付きでビルド）。製品 crate と同じ版 | 採用（Slice 2） |

Slice 2 で増えた crate（rusqlite / libsqlite3-sys / hashlink / hashbrown / ahash / fallible-iterator /
fallible-streaming-iterator / bitflags / smallvec、ビルド時の cc / pkg-config / vcpkg / shlex など）も
MIT / Apache-2.0 系で、MPL-2.0 は 0 個。SQLite 本体はパブリックドメイン。`Cargo.lock` は 62 パッケージ。

scraper・ego-tree と、scraper 経由だった MPL-2.0 の 4 crate（selectors / cssparser / cssparser-macros /
dtoa-short）は削除した。実行時の推移的依存は 36 crate で、すべて MIT / Apache-2.0 / Unlicense 系
（encoding_rs は BSD-3-Clause、unicode-ident は Unicode-3.0 を含む）。MPL-2.0 は 0 個。`Cargo.lock` は 45 パッケージ。

### mail-parser を採用した理由と、その使い方

確認した事実（ソースと `tests/integration.rs` の `mail_parser_alone_accepts_what_the_poc_refuses`）：

- `multipart/related` を含む multipart を解析し、パートごとの Content-Type（パラメータ `start` / `type` /
  `boundary` / `charset`）、`Content-ID`（山括弧なし）、`Content-Location` を取得できる。
- 入力を借用して索引だけを作る（全パートのコピーを作らない）。`decoded_len()` は base64 / quoted-printable を
  **復号せずに**復号後サイズを返すので、16 MiB 上限を確保前に判定できる。
- `max_parts` / `max_depth` / `max_encoded_nesting` の上限を持ち、到達すると `LIMIT_REACHED` を付ける。
- **寛容**である：閉じ区切りの欠落、不正な base64（アルファベット外を読み飛ばす）、不正な QP を、
  `PartFlags` / `DecodeProblems` で**報告するだけ**で解析は成功する。宣言された boundary が本文に無い場合は、
  フラグすら付けずに最上位を 1 個の葉として扱う。
- charset 未指定の `text/html` を、mail-parser 自身の `text()` は UTF-8 として扱う。Chrome は HTML パートに
  MIME charset を書かず、Shift_JIS ページは Shift_JIS のバイトのまま保存するため、そのままでは文字化けする。

したがって、**mail-parser の「解析できた」を MHTML の妥当性とは見なさない**。`src/mime.rs` が次を行う：
フラグ（`LIMIT_REACHED` / `UNTERMINATED` / `MISSING_DELIMITER` / `FALLBACK_DELIMITER` / `NO_BLANK_LINE`）をエラー化、
宣言は multipart なのに展開されなかったパートをエラー化、boundary の RFC 2046 形式検査、`message/*` パートの拒否、
**全**葉パートの Content-Transfer-Encoding の値と内容（base64 / QP）を**復号・確保なしで**検査、ルート HTML の
`decoded_checked` の問題をエラー化。文字コード変換は mail-parser に任せず `charset.rs` で行う。

### HTML（tokenizer 方式）

html5ever の tokenizer は WHATWG HTML の字句解析器（文字参照の解決、属性、コメント、CDATA を含む）。
ただし **script / style などの内容モデルの切り替えは tree builder の役目**で、tokenizer 単体では行われない。
本 PoC は TokenSink から `TokenSinkResult::RawData` / `Plaintext` を返して切り替える（下記）。正規表現や
文字列分割による HTML 解析はしない（例外は `charset.rs` の `<meta>` プレスキャンで、これも HTML 標準のプレスキャンを
簡略化したバイト走査）。JavaScript は実行しない。CSS は解釈しない。

## 抽出仕様（要約）

- **ルート選択**：`start` パラメータがあればその Content-ID のパート（無い・重複はエラー）。無ければ先頭パート
  （RFC 2387 の既定。Chrome は `start` を書かず、主文書を先頭に置き、`Content-Location` を
  `Snapshot-Content-Location` と同じにする）。ルートが `text/html` でなければ**拒否**し、他の HTML を黙って採らない。
  iframe（Chrome は `cid:frame-…@mhtml.blink` の別パートにする）は数えるだけで抽出しない（警告 `secondary_html_parts_not_extracted`）。
- **ヘッダ由来文字列**：`Content-Location` / `Snapshot-Content-Location` は 8 KiB、MIME `charset` は 64 バイトまで。
  超過、制御文字を含む値、UTF-8 でないバイトを含む値は**捨てて警告**（`*_too_large_dropped` /
  `*_control_characters_dropped` / `*_invalid_utf8_dropped` / `mime_charset_*_ignored`）。UTF-8 の検査は
  2 つの location では生のヘッダバイトで、charset では解析後の値の U+FFFD で行う。charset を捨てた場合は
  `<meta>` で判定する。
- **文字コード**：BOM → MIME `charset` → `<meta charset>` / `http-equiv` の先頭 1024 バイト → 既定 UTF-8。
  未知のラベルは警告して次へ。`replacement` に対応するラベルは拒否。`<meta>` の UTF-16 は UTF-8 とみなす（HTML 標準）。
  不正バイト列は U+FFFD にして警告（拒否しない）。宣言と実データの不一致は**修復しない**（文字化けした本文で
  警告付き成功。索引時の扱いは Slice 2）。
- **逐次処理**：変換元 64 KiB ごとに UTF-8 へ変換し、`BufferQueue` に入れて tokenizer を進める。復号済み文書全体は
  保持しない。チャンク境界は結果に影響しない（1 バイト単位でも同一になることをテストで確認）。
- **内容モデル**（HTML 文脈のみ）：`script` → script data、`style` / `xmp` / `iframe` / `noembed` / `noframes` /
  `noscript` → RAWTEXT、`title` / `textarea` → RCDATA、`plaintext` → PLAINTEXT。これらの中の `<` や疑似タグは
  タグにも本文にもならない（`textarea` と `xmp` の中身は画面に出るので本文に入れる）。SVG / MathML の中では切り替えない
  （`<svg><script/>` で後続を失わない）。`<![CDATA[` は SVG / MathML の中だけで認識する。
- **本文から除外**：script / style / noscript / template / svg（全体）/ canvas / iframe / noembed / noframes / frameset /
  head と title / コメント / doctype。`hidden` 属性（`until-found` を除く）とインライン style の `display:none`。
  **`aria-hidden="true"` とインライン `visibility:hidden` だけでは除外しない**（画面に出る文字列の取りこぼしを避ける）。
  MathML の文字は残す。迷う場合は取りこぼさない側に倒す。
- **入れ子の補正**（tree builder の近似）：開いている要素の名前スタック（最大 1024）を持つ。void 要素は積まない。
  終了タグはスコープ内の同名要素まで閉じ、無ければ無視（ブラウザと同じ）。スコープの境界は HTML 標準の要素と、
  SVG / MathML の統合点（`foreignObject` / `desc` / `title`、`mi` / `mo` / `mn` / `ms` / `mtext` / `annotation-xml`）
  だけで、他の SVG / MathML 要素は境界にならない。表の終了タグは表スコープ（統合点は境界にならない）。
  `</template>` はスコープを見ずに最も近い template を閉じる（HTML 標準どおり）。`</h1>`〜`</h6>` はどの見出しも閉じる。
  **ブラウザとの意図的な違い**：開いている SVG / MathML 要素の終了タグは、統合点の中に HTML 要素が残っていても
  閉じる（`<svg><foreignObject><p>x</foreignObject></svg>` の後続本文を SVG 内に取り残さない）。ただし間に開いた
  template がある場合は閉じない。`<p>` を閉じる開始タグ、`<li>` / `<dt>` /
  `<dd>` / `<option>` / 表の行・セルの暗黙の終了、head の暗黙の終了（head に置けない開始タグ・空白以外の文字）、
  SVG / MathML からの脱出（`<p>` `<div>` `<b>` など、と `</p>` `</br>`。最も近い HTML 要素か統合点まで）を扱う。
  スタック上限を超えた要素は追跡しない
  （除外範囲が早く終わる＝文字が増える方向にしか崩れない。警告 `open_element_limit_reached`）。
  スコープ探索は要素名ごとの位置索引で行い、スタックを毎回たどらない（大量の不一致終了タグでも計算量が増えない）。
- **区切り**：ブロック要素・`<br>`・`<hr>`・カスタム要素（名前に `-`）は改行、表のセルは空白、インライン要素は連結
  （`<p>東京</p><p>都内</p>` は 2 行、`<span>東</span><span>京都</span>` は `東京都`）。空白類（NBSP・U+3000 を含む）は
  1 個の空白に畳み、空行は除く。`pre` / `listing` / `textarea` / `xmp` / `plaintext` の改行は保つ。改行以外の制御文字は除去。
- **メタデータ**：`<title>` は HTML 文脈の最初のもの（svg 内の title は除外）。`<meta>` は `name` と `property` の
  **両方**からキーを読む（`name="description" property="og:description"` は両方に入る）。対象外のキーの `<meta>` は
  `content` を見ずに無視（巨大でも拒否しない）。単一値は最初の非空値（重複は警告）。`keywords` は `,` `、` `，` `;`
  で分割して trim、空要素除去・重複除去。`article:tag` は全値を文書順・重複除去。欠落は `null`。すべて未信頼文字列。

## 資源制限

| 対象 | 上限 | 判定の時点 |
|---|---|---|
| MHTML 入力 | 128 MiB | 読み込み**前**（ファイルサイズ）と読み込み中（上限＋1 バイトまでしか読まない） |
| HTML パート（転送復号後） | 16 MiB | 復号**前**（`decoded_len`）と復号後 |
| HTML の UTF-8 変換後 | 32 MiB | 変換しながら累計で |
| 抽出本文 | 8 MiB（UTF-8） | 1 文字追加するごと |
| メタデータ 1 項目 | 64 KiB | 整形しながら（`article:tag` は合計） |
| Content-Location / Snapshot-Content-Location | 8 KiB | MIME 解析時 |
| MIME charset | 64 バイト | MIME 解析時 |
| 開いている要素の追跡 | 1024 | 要素を積むとき（超過分は追跡しない） |
| tokenizer への供給単位 | 64 KiB（変換元バイト） | 逐次 |
| MIME パート数 / 入れ子 | 1024 / 16 | mail-parser の上限（到達はエラー） |

**保証できないもの**（詳細はレポート XIII）：入力全体はメモリに読み込む（最大 128 MiB）。転送復号後の HTML も
1 個保持する（最大 16 MiB）。tokenizer は 1 個のコメント・属性値・タグをまとめて保持するため、巨大なコメントや属性値は
その大きさ分のバッファになる（上限は HTML サイズ）。プロセス全体のメモリ上限・CPU 時間上限を強制する仕組みは無い。

## 検索（Slice 2）

```powershell
# DB は Git 作業ツリーの外に置く（中に置こうとすると拒否。抽出本文を含むため MHTML 原本と同等に扱う）
# DB を新規作成するのは add だけ（--db に何も無いとき）。search / stats は読み取り専用で開く
.\target\release\page-index.exe add    --db C:\path\outside\index.sqlite3 --key page-1 --input C:\path\page.mhtml
.\target\release\page-index.exe search --db C:\path\outside\index.sqlite3 --query 𩸽          # HIT したページ ID とキーだけ
.\target\release\page-index.exe search --db ... --query 𩸽 --show-titles                       # タイトルと URL も表示
.\target\release\page-index.exe delete|rebuild|check|stats --db ... [--key page-1]
.\target\release\page-index.exe quality --input C:\path\page.mhtml   # 品質ゲートの数値だけ（本文は出さない）
# 実 MHTML 受入試験（既定では実行しない）
$env:PAGE_FULLTEXT_REAL_HOKKE = 'C:\path\hokke.mhtml'; cargo test --release --test search -- --ignored --nocapture
```

- **一致規則**：索引したタイトルまたは本文に、検索文字列が**リテラルな部分文字列**として含まれれば HIT。
  Unicode スカラー値の列としての一致で、大文字小文字・NFC/NFD・全角半角は区別する（変換しない）。
  ワイルドカードや検索構文はない。空文字列は `empty_query` で拒否、4,096 バイト超は `query_too_large`。
- **最終判定は常に Rust の `str::contains`**（保存したテキストに対して）。FTS5 は候補の絞り込みにだけ使う。
- **1〜2 文字**：全ページを走査。**3 文字以上**：FTS5 trigram（`case_sensitive 1`）の `GLOB`（`*` `?` `[` は
  文字クラスでエスケープ）で候補を出し、候補をリテラル照合。索引の整合性が未確認（検査で不一致）の場合と、
  検索文字列に制御文字（改行を含む）がある場合は全件走査に切り替える。
- **利用者の入力は常にバインド引数**。SQL 文にも FTS5 MATCH 構文にも埋め込まない。
- **スキーマ**：`pages`（本文・タイトル・状態・品質 JSON）、外部コンテンツ型 FTS5 `pages_fts`（トリガーで同期）、
  `attempts`（登録試行の記録。失敗時も残る）、`meta`。登録・更新・削除・再構築はそれぞれ 1 トランザクション。
- **品質ゲート**：抽出失敗では既存の索引を変えない。文字化け（置換文字・UTF-8 の誤読・除去された制御文字の
  割合）と判定した本文は索引せず、既存ページがあればそれを残す。しきい値は暫定（レポート XIV-6）。
  索引済みの本文があるページは、再抽出の本文が空・文字化け・失敗のいずれでも上書きしない（`kept_previous`、
  試行は記録する）。新規ページで本文が空ならタイトルだけ登録する（XIV-13）。
- **開いてよいファイル**：このPoCが作った DB だけ。ヘッダーの `application_id`、スキーマ版、スキーマ全体
  （表・列・トリガー・FTS5 設定）、`meta` 行を、まず SQLite を使わずにヘッダーを読み、次に `immutable=1` の
  読み取り専用接続で確かめ、通ってから本来の接続を開いて再確認する。空ファイル・非 SQLite・他アプリの DB は
  一切変更せずに拒否する。FTS5 は `detail=none` が既定（PO 決定）。
- **ジャーナル**：常に SQLite 標準の rollback journal（`journal_mode=DELETE`、`synchronous=FULL`）。ジャーナル
  モードは設定も変更もせず、WAL 形式の DB は拒否する（PO 決定、XIV-14）。異常終了で残った `-journal` は、
  次に書き込み用に開いたとき（`add`・`delete`・`rebuild`・`check`）に SQLite が巻き戻す。それまでは
  `search`・`stats` は `needs_recovery` で拒否する（読み取り専用接続では巻き戻せないため）。
- **Git 作業ツリーの判定**：DB パス（存在しない部分は存在する親）の実パスを解決し（シンボリックリンク・
  ジャンクション）、そのパス自身を含むすべての祖先に `.git` が無いことを確かめる。判定できなければ拒否。
  `bench --dir` も同じ（上書き指定なし、既存ファイルは削除せず拒否）。

## 機密情報の扱い

- 既定ではページ由来の文字列（本文・メタデータ・URL）を標準出力・標準エラーに出さない（テストで確認）。
- 警告・エラーはコードのみ。
- `--output` は存在しないファイルにだけ作成（`create_new`）。失敗時は作りかけを消す。
- 実ページの MHTML や抽出結果を Git に入れないため、`real/` と `out/` は `.gitignore` 済み。

## 実 Chrome 製 MHTML の検証手順（PO 向け）

外部サイトへの自動アクセスはしていない。実ページでの検証は次の手順で PO が行う（1 件実施済み。レポート XII / XIII）：

1. 検証専用の Chrome プロファイル（または VM）で、対象ページを Memocampus の右クリック保存で保存する。
2. Side Panel の Page タブから「MHTMLを書き出す」で `poc\page-fulltext\real\` に書き出す（Git 管理外）。
3. `page-fulltext.exe --input real\<file>.mhtml` を実行し、**要約行だけ**を記録する。
4. 品質確認が必要なら `--output out\<file>.json` に書き出し、手元で目視する。本文はレポートに転記しない。
