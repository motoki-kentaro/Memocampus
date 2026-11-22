# PageVault 全文検索 PoC — Slice 2 検証記録（SQLite FTS5 検索索引）

Slice 1 の記録（`slice-1-results.md`）は変更していない。この文書の章番号は XIV から始める。

**判定：実装担当者は総合判定を確定しない。PO と独立レビューの判定を待つ。**
commit・push・PR は行っていない。

## XIV-1. 概要

- Slice 1 の抽出エンジン（html5ever tokenizer 方式）の結果を、PoC 専用の SQLite データベースに索引する。
- 検索の契約：索引したタイトルまたは本文に、検索文字列が**リテラルな部分文字列**として存在すれば HIT。
- **最終判定は常に Rust の `str::contains`**（保存したテキストに対して）で行う。FTS5 は候補の絞り込みにだけ使い、FTS5 の結果だけで HIT を決めることはない。
- 1〜2 文字は全件走査。3 文字以上は FTS5 trigram で候補を出してから照合する。安全でない場合は全件走査に切り替える。
- 抽出結果は品質ゲートを通してから登録する。抽出失敗や文字化けで、登録済みの本文を失わない。
- 既存 Vault・製品コード・Privacy Policy・MCP・Chrome 拡張・Native Host は変更していない。

**変更ファイル（すべて `poc/page-fulltext/` 配下、コミット `2ec7432` からの差分）：**

| ファイル | 内容 |
|---|---|
| `Cargo.toml` / `Cargo.lock` | `rusqlite = "=0.32.1"`（`bundled`）と、2 個目のバイナリ `page-index` を追加 |
| `src/lib.rs` | `index` / `quality` モジュールの公開（2 行） |
| `src/index.rs`（新規） | スキーマ・登録・更新・削除・再構築・整合性検査・検索 |
| `src/quality.rs`（新規） | 品質ゲート |
| `src/bin/page_index.rs`（新規） | CLI（登録・検索・削除・再構築・検査・統計・品質計測・ベンチマーク） |
| `tests/search.rs`（新規） | Slice 2 のテスト 31 件＋実 MHTML 受入試験 1 件（既定では実行しない） |
| `README.md` | Slice 2 の使い方・仕様の要約・依存の追加 |
| `reports/slice-2-results.md`（新規） | この文書 |

Slice 1 のソース（`mime.rs` / `charset.rs` / `html.rs` / `metadata.rs` / `limits.rs` / `main.rs`）は変更していない。

## XIV-2. 環境

- Windows 11 Home、Intel Core i7-13700F、メモリ 32 GB、rustc 1.100.0、release ビルド。
- SQLite 3.46.0（libsqlite3-sys 0.30.1 同梱、`SQLITE_ENABLE_FTS5` 付き）。
- 製品 crate は同じ libsqlite3-sys の SQLCipher 版（SQLite 3.45.3）を使う。そのビルドスクリプトにも `-DSQLITE_ENABLE_FTS5` があることは確認した。ただし、SQLCipher 版での動作試験はしていない（XIV-12）。

## XIV-3. SQLite スキーマ

```sql
CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT;
  -- schema_version=1, tokenizer, detail, fts_state（ok / inconsistent）
CREATE TABLE pages(
  page_id INTEGER PRIMARY KEY,          -- PoC 内で一意。再登録・更新でも変わらない
  page_key TEXT NOT NULL UNIQUE,        -- 呼び出し側のページ識別子（製品では Page の ID を想定）
  url TEXT,                             -- Snapshot-Content-Location（無ければ Content-Location）
  title TEXT,                           -- 索引したタイトル（品質ゲートで落とした場合は NULL）
  body TEXT NOT NULL,                   -- 索引した本文（落とした場合は ''）
  status TEXT NOT NULL,                 -- XIV-6 の状態コード
  quality TEXT NOT NULL,                -- 品質情報 JSON（コードと数値のみ。本文は含まない）
  registered_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
) STRICT;
CREATE TABLE attempts(                  -- 登録試行の記録。失敗・保留も残る
  attempt_id INTEGER PRIMARY KEY, page_key TEXT NOT NULL, at INTEGER NOT NULL,
  outcome TEXT NOT NULL,                -- inserted / updated / kept_previous / not_registered
  status TEXT NOT NULL, error_code TEXT, quality TEXT
) STRICT;
CREATE INDEX attempts_by_key ON attempts(page_key, attempt_id);
CREATE VIRTUAL TABLE pages_fts USING fts5(
  title, body, content = 'pages', content_rowid = 'page_id',
  tokenize = 'trigram case_sensitive 1', detail = 'full'
);
-- 外部コンテンツ型の標準的な同期トリガー（pages の INSERT / DELETE / UPDATE OF title, body）
CREATE TRIGGER pages_ai AFTER INSERT ON pages BEGIN
  INSERT INTO pages_fts(rowid, title, body) VALUES (new.page_id, new.title, new.body); END;
CREATE TRIGGER pages_ad AFTER DELETE ON pages BEGIN
  INSERT INTO pages_fts(pages_fts, rowid, title, body) VALUES ('delete', old.page_id, old.title, old.body); END;
CREATE TRIGGER pages_au AFTER UPDATE OF title, body ON pages BEGIN
  INSERT INTO pages_fts(pages_fts, rowid, title, body) VALUES ('delete', old.page_id, old.title, old.body);
  INSERT INTO pages_fts(rowid, title, body) VALUES (new.page_id, new.title, new.body); END;
```

**外部コンテンツ型を選んだ理由：** 本文を 1 回だけ保存し、FTS5 は索引だけを持つ。最終照合は `pages` の本文で行う。

**接続設定：**

| 設定 | 値 | 理由 |
|---|---|---|
| `journal_mode` | WAL | 書き込み中も検索できる |
| `synchronous` | NORMAL | 計測値はこの設定。停電時に直近のコミットを失う可能性はあるが、ファイルの整合性は保たれる。FULL も選べる |
| `secure_delete` | ON | 削除したページの本文をファイル上に残さない（空き領域をゼロで上書き） |
| `trusted_schema` | OFF | — |

**開く前の検査：**

- 空のデータベースか、この PoC が作ったもの（`meta.schema_version = 1`）だけを使う。
- テーブルがあるのに `meta` がない SQLite ファイル（たとえば Vault）は、**何も書き込まずに拒否する**。テストでは、ファイルのバイト列が変わらず、WAL ファイルも作られないことを確認した。
- `journal_mode` の変更もファイルに書き込まれるため、この検査の後に行う。
- CLI は、Git 作業ツリーの中にデータベースを作ることを拒否する（`--allow-db-in-git` で解除）。ルートの `.gitignore` も `*.sqlite3` / `*.db` を除外している。

## XIV-4. FTS5 の構成と、候補絞り込み方式の比較

**確認した FTS5 trigram の仕様（SQLite 3.46.0）：**

- 3 文字（Unicode スカラー値）ごとのトークンを作る。空白・改行も文字として扱う。
- `case_sensitive 1` で大文字小文字を折りたたまない。既定の `0` では折りたたむので、候補は増えるが漏れない。どちらでも最終照合は Rust で行う。
- `MATCH`・`LIKE`・`GLOB` の 3 通りで照会できる。3 文字未満の部分は索引を使えない。
- 整合性検査は `INSERT INTO pages_fts(pages_fts, rank) VALUES('integrity-check', 1)`。`rank = 1` で、外部コンテンツ表との一致まで検査する。

**比較した 3 方式：**

| 方式 | 照会 | 利用者の入力の扱い |
|---|---|---|
| **GLOB（採用）** | `title GLOB ?1` と `body GLOB ?1` の UNION | `*検索文字列*` をバインド引数で渡す。`*` `?` `[` は `[*]` `[?]` `[[]` の文字クラスにエスケープ |
| LIKE | `LIKE ?1 ESCAPE '\'` | `%` `_` `\` をエスケープ。SQLite の LIKE は ASCII の大文字小文字を区別しない（候補は増える） |
| MATCH（フレーズ） | `pages_fts MATCH ?1` | 検索文字列を `"` で囲み、中の `"` を二重にした FTS5 文字列。**指示書が避けるよう求める「MATCH 構文への埋め込み」に当たる** |

**差分試験（`f_fts5_candidates_never_miss_compared_with_full_scan`）：**

- 方法：
  - 大文字小文字・NFC/NFD・全角半角・合成用絵文字・ZWJ・`İ` / `ß` / `ﬁ` / `Σσς`・改行・空白・SQL 記号・FTS5 記号・GLOB 記号を混ぜた乱数文書を 150 件作る。
  - 3 文字以上の検索文字列を 2,200 個前後使う。半分は保存文書の部分文字列（ほぼ HIT）、半分は乱数（ほぼ MISS）。
  - 各方式の「候補＋リテラル照合」の結果を、全件走査の結果と比べる。

| detail | 検索数 | GLOB | LIKE | MATCH（フレーズ） |
|---|---|---|---|---|
| full | 2,236 | **0 件不一致** | 0 件不一致 | 0 件不一致 |
| none | 2,198 | **0 件不一致** | 0 件不一致 | **1,732 件不一致（候補漏れ）** |

**採用理由：**

- GLOB は、`detail` の設定によらず漏れがなかった。
- GLOB は大文字小文字を区別するので、`case_sensitive 1` と一致し、余計な候補が出ない。
- MATCH 構文を使わずに済む。
- MATCH は `detail=none` で大量に候補を漏らした。原因（位置情報なしでのフレーズの扱い）は推測で、未調査。
- LIKE も漏れはなかったが、ASCII の大文字小文字を区別しないため候補が増える。索引が使われるかどうかの速度比較はしていない。

**全件走査に切り替える条件（3 文字以上でも）：**

| 条件 | fallback コード | 理由 |
|---|---|---|
| `meta.fts_state` が `ok` でない（整合性検査で不一致） | `index_not_verified` | 索引が本文と食い違っていると候補が漏れる。XIV-8 の試験で、実際に漏れることを確認した |
| 検索文字列に制御文字（改行・タブ・NUL 等）を含む | `control_character` | 予防的な措置。差分試験では、改行を含む検索でも GLOB の漏れはなかった |

1〜2 文字の検索は、すべて全件走査（`short_query`）。`SearchMode::Verify` は、FTS5 経路と全件走査の両方を実行し、食い違えばエラー `fts_missed_pages` を返す。

- テストでは、すべての検索を自動・全件走査・Verify の 3 モードで実行し、結果が一致することを確認した。
- ベンチマークでは、自動モードと全件走査の HIT 数が一致することを確認した。

## XIV-5. Unicode の一致仕様

- 一致の単位は Unicode スカラー値（コードポイント）。比較は UTF-8 文字列に対する `str::contains`。
  - UTF-8 は自己同期的なので、妥当な UTF-8 同士のバイト列一致は、コードポイント列の一致と同じ。
  - バイト単位や UTF-16 単位で文字を数える処理はない。
- 3 文字判定は `chars().count()`。`𩸽`（U+29E3D、UTF-8 で 4 バイト、UTF-16 でサロゲートペア）は 1 文字。`𩸽を焼` は 3 文字として FTS5 経路に入る（テストで確認）。
- **変換しないもの：** 大文字小文字、NFC/NFD、全角半角。それぞれ別の文字列として扱う（テストで確認）。
- **コードポイント単位の帰結（PO 確認事項）：**
  - `👍` は `👍🏽`（肌色修飾付き）の部分文字列として HIT する。
  - `か` は NFD の `か゛`（U+304B U+3099）に HIT するが、NFC の `が`（U+304C）には HIT しない。
  - 書記素クラスタ単位の一致にはしていない。
- **空白・改行：** 照合対象は Slice 1 が整形した本文。ブロック境界は `\n`、その他の空白は 1 個の半角空白に畳まれている。
  - 本文が `東京\n都内` のとき、`東京 都内` や `東京都内` は MISS。
  - 検索文字列の空白は正規化しない（指示書の「完全一致」に従った）。PO 確認事項として XIV-12 に挙げる。
- 空文字列は `empty_query` で拒否する（すべてのページに含まれるとは扱わない）。4,096 バイトを超える検索文字列は `query_too_large`。

## XIV-6. 品質ゲート（暫定設計）

**計測値（本文・タイトルそれぞれ。コードと数値だけを品質 JSON に保存）：**

| 指標 | 意味 |
|---|---|
| 置換文字（U+FFFD）の数・千分率 | 復号時に不正だったバイト |
| UTF-8 誤読の文字数・千分率 | 非 ASCII 文字のうち、「ページの復号に使った文字コードで符号化し直すと、正しい UTF-8 になる」連続部分に属する文字。UTF-8 を windows-1252・Shift_JIS 等で読んだ文字化けは U+FFFD を含まないので、これで測る |
| 除去された制御文字の数・千分率 | Slice 1 が本文から取り除いた制御文字。UTF-8 を ISO-8859-x 等で読むと、C1 制御文字が大量に出る |

- **UTF-8 誤読の判定は、ページを UTF-8 以外で復号したときだけ行う。** UTF-8 のページの不正バイトは U+FFFD として現れる。
- 各連続部分（非 ASCII 文字の連なり）の 3/4 以上のバイトが正しい UTF-8 なら、誤読とみなす。
- 1 バイトずつ判定する方式を最初に試したが、正常な日本語でも EUC-JP で 11‰、GBK で 11‰ を誤検出したため、連続部分単位に改めた。

**判定（暫定値。`GateConfig::default()`）：**

| 判定 | 条件 |
|---|---|
| garbled（文字化け：索引しない） | 置換文字 4 個以上かつ 10‰ 以上、**または** 非 ASCII 16 文字以上でかつ UTF-8 誤読 300‰ 以上、**または** 除去した制御文字 8 個以上かつ 50‰ 以上 |
| suspect（索引し、数値を保持） | 上記未満だが、いずれかが 1 以上 |
| clean | すべて 0 |
| empty | 文字数 0 |

タイトルは短く、単独では判定しにくい。そこで、本文が garbled のときは、非 ASCII 文字を含むタイトルも garbled とする。ASCII だけのタイトルは、どの ASCII 互換文字コードでも同じに読めるので残す。

**しきい値の根拠（実データと変異コピーの計測）：**

| 入力 | 文字コード | 置換文字 | UTF-8 誤読 | 除去した制御文字 | 判定 |
|---|---|---|---|---|---|
| 実ページ（𩸽、ホッケ） | UTF-8 | 0 | —（UTF-8） | 0 | clean |
| 実ページ（XII で使ったもの） | UTF-8 | 0 | — | 0 | clean |
| Chrome fixture 6 件（Shift_JIS 1 件を含む） | UTF-8 / Shift_JIS | 0 | 0‰ | 0 | clean |
| 実ページ 2 件を windows-1252 と宣言 | windows-1252 | 0 | 494‰ / 626‰ | 102‰ / 83‰ | garbled |
| 同、Shift_JIS と宣言 | Shift_JIS | 81‰ / 81‰ | 814‰ / 808‰ | 13‰ / 10‰ | garbled |
| 同、EUC-JP と宣言 | EUC-JP | 651‰ / 650‰ | 55‰ / 54‰ | 0 | garbled |
| 同、GBK と宣言 | GBK | 42‰ / 51‰ | 927‰ / 918‰ | 0 | garbled |
| 同、KOI8-R と宣言 | KOI8-R | 0 | 995‰ / 998‰ | 0 | garbled |
| 同、ISO-8859-5 と宣言 | ISO-8859-5 | 0 | 38‰ / 47‰ | 371‰ / 360‰ | garbled（制御文字の規則） |
| Shift_JIS fixture を UTF-8 / EUC-JP と宣言 | — | 663‰ / 642‰ | — / 0 | 0 | garbled |
| Shift_JIS fixture を windows-1252 と宣言 | windows-1252 | 0 | 66‰ | 130‰ | garbled（制御文字の規則） |

**誤検出の確認：** 正常な本文（実ページ 2 件、`large`、`ja-article`、`sjis-news`、`complex`）を、11 種の文字コード（windows-1252 / 1251、ISO-8859-5、KOI8-R、Shift_JIS、EUC-JP、ISO-2022-JP、GBK、gb18030、Big5、EUC-KR）で復号した**と仮定**して UTF-8 誤読率を測った。

- 最大は、大きい本文で 17‰（EUC-KR）、短い本文（50〜92 文字）で 40‰。
- しきい値 300‰ との間に十分な差がある。
- 正常なページで、制御文字が除去された例は 0 件。

**状態コード：**

| status | 本文 | タイトル |
|---|---|---|
| `ok` | 索引（clean、抽出警告なし） | 索引または無し |
| `ok_with_warnings` | 索引（suspect、または抽出警告あり。警告は品質 JSON に保持） | 索引 |
| `title_only_empty_body` | 空 | 索引 |
| `title_only_garbled_body` | 文字化けのため索引しない | 索引（ASCII のみ等） |
| `nothing_indexed_empty` / `nothing_indexed_garbled` | 索引しない | 索引しない |

**抽出失敗の分類**（`attempts.status`）：

| 分類 | 対象のエラー |
|---|---|
| `input_too_large` | 入力サイズ上限 |
| `content_too_large` | HTML・復号後・本文・メタデータの上限 |
| `mhtml_parse_failed` | MIME 構造・転送符号化・ルート選択の失敗 |
| `charset_unusable` | 使えない文字コード |
| `input_unreadable` | 読み込み失敗 |
| `internal_error` | 内部エラー |

**限界（暫定である理由）：**

- しきい値の根拠は、実ページ 2 件と、その変異コピーだけ。
- 本物の legacy 文字コードのページは、合成 fixture 1 件（Shift_JIS）しかない。
- legacy → legacy の文字化け（Shift_JIS のページを windows-1252 で読む等）は、UTF-8 誤読の指標では捉えられない。今回は制御文字の規則で捉えたが、C1 制御文字が出ない組み合わせ（Shift_JIS ↔ EUC-JP 等）は、置換文字が出ない限り見逃す。
- 短い本文が 1 個の連続部分しかなく、C1 制御文字が除去された場合は、UTF-8 誤読率が 0 になりうる。テストの合成例では制御文字の規則で捉えた（2 つの規則が補い合う）。
- 製品仕様として確定する前に、実データでの再計測が必要（XIV-12）。

## XIV-7. 登録・更新・削除・再構築の契約

| 操作 | 契約 |
|---|---|
| 登録・更新 `put_page` / `register` | 1 トランザクション（`BEGIN IMMEDIATE`）。`pages` の upsert（`ON CONFLICT(page_key) DO UPDATE`）、トリガーによる FTS5 の更新、`attempts` への記録をまとめて行う。同じ `page_key` は同じ `page_id` を保つ（重複しない） |
| 抽出失敗の登録 | `pages` と FTS5 は変更しない。`attempts` に `kept_previous`（既存あり）か `not_registered`（既存なし）と、エラーコード・分類を記録する |
| 文字化け、または索引できるものが無い再抽出 | 既存ページがあれば変更しない（`kept_previous`）。無ければ、通過したもの（タイトル等）だけで登録する |
| 本文が空の再抽出 | 既存ページを「タイトルのみ」に**更新する**（抽出としては成功のため）。PO 確認事項（XIV-12） |
| 削除 `delete` | 1 トランザクション。トリガーで FTS5 からも消える。`secure_delete` で、ファイル上の本文も消す。`attempts` の履歴は残る |
| 再構築 `rebuild` | 1 トランザクションで `INSERT INTO pages_fts(pages_fts) VALUES('rebuild')` を実行し、その後に整合性検査を行う |
| 整合性検査 `check` | FTS5 の `integrity-check`（`rank = 1`）と `PRAGMA integrity_check`。不一致なら `fts_state = inconsistent` とし、以後の検索はすべて全件走査になる（再構築で `ok` に戻る） |
| クローズ・再オープン | 状態はすべて DB ファイルに保存される（再オープン後の検索結果が同一であることをテストで確認） |
| FTS5 が無い環境 | 開くときに、一時表で FTS5 trigram を作って試す。失敗したら `fts5_unavailable`（SQLite のメッセージ付き）で停止する。黙って機能を落とすことはしない |

**途中失敗：** 次の 3 か所に故障を注入する試験で確認した。

- ページ書き込みの直後。
- コミットの直前。
- 再構築の途中。

どの場合も、既存の本文・タイトル・試行記録・FTS5 が元のままで、整合性検査に通ることを確認した。

## XIV-8. テスト

`cargo test --release`：**96 件成功・0 失敗、無視 1 件**（実 MHTML 受入試験は環境変数を指定したときだけ実行する）。

- Slice 1 の 65 件（`integration.rs` 31、`tokenizer.rs` 22、`end_tags.rs` 12）は、変更せずにすべて成功した。
- clippy（`--all-targets`）の警告は 0、`cargo fmt --check` の差分なし。

**Slice 2 のテスト（`tests/search.rs`、31 件）と、指示書の項目の対応：**

| 指示書の項目 | テスト（主な確認内容） |
|---|---|
| 日本語 1 文字 / 2 文字 / 3 文字以上 | `s_japanese_one_two_and_more_characters`、`s_hokke_table_from_the_specification`（仕様の表 6 件） |
| `𩸽` 単独 / `𩸽𩸽` / `𩸽` を含む 3 文字以上 | `s_hokke_table_…`、`s_hokke_repeated_and_in_longer_queries`（`𩸽 𩸽` は `𩸽𩸽` に HIT しない） |
| ASCII と日本語の混在 | `s_ascii_and_japanese_mixed` |
| 英字の大文字小文字 | `s_case_is_significant`（Ä / Σ を含む） |
| NFC / NFD | `s_nfc_and_nfd_are_different` |
| 全角半角 | `s_fullwidth_and_halfwidth_are_different` |
| 絵文字 | `s_emoji`（肌色修飾・ZWJ 家族・国旗・🦀🦀🦀） |
| SQL 特殊文字 | `s_sql_special_characters_are_plain_text`（`%` `_` `'` `"` `\` `;--`、`'; DROP TABLE pages; --` 等。表が残ることも確認） |
| FTS5 特殊文字 | `s_fts5_and_glob_special_characters_are_plain_text`（`*` `?` `[` `(` `)` `-` `"` `:` `^` `NEAR(…)` `AND OR NOT`。`x*y` が `xzy` に、`a?c` が `abc` に HIT しない） |
| 空文字列 / 長い検索文字列 | `s_empty_long_and_oversized_queries`（600 文字・1,200 文字で HIT、上限超過は拒否） |
| 改行と空白 | `s_newlines_and_spaces` |
| タイトルと本文 | `s_title_and_body_are_separate_fields`（境界をまたいで一致しない。結果に本文を含まない）、`s_single_page_hit_or_miss` |
| 登録・更新・削除 | `r_register_update_delete` |
| 再登録時の重複防止 | `r_reregistration_does_not_duplicate` |
| DB 再オープン | `r_close_and_reopen`（スキーマ版の不一致は拒否） |
| 他の DB を使わない | `r_other_databases_are_refused_and_not_modified` |
| FTS5 再構築 | `r_rebuild` |
| 破損・不一致の検出 | `r_index_damage_is_detected_and_searches_fall_back` |
| 途中失敗時のロールバック | `r_failure_inside_a_transaction_rolls_back` |
| FTS5 機能の欠如 | `r_missing_fts5_feature_is_an_explicit_error` |
| 不正なキー・テキスト | `r_invalid_keys_and_text_are_refused` |
| FTS5 候補漏れ時のフォールバック | `r_index_damage_…`、`f_fts5_candidates_never_miss_compared_with_full_scan`（XIV-4） |
| 解析失敗時の既存索引維持 | `g_failed_or_garbled_re_extraction_keeps_the_indexed_page`、`g_real_size_limit_failure_is_classified` |
| 文字化け検出・品質情報保持 | `g_garbled_text_is_not_indexed_and_quality_is_kept`、`g_genuine_legacy_encoded_page_is_clean` |
| 本文空・警告付き・正常 | `g_empty_body_keeps_the_title`、`g_clean_pages_and_pages_with_warnings` |
| 抽出から検索まで（QP のソフト改行が `𩸽` の 4 バイトの途中に入る合成 MHTML） | `g_end_to_end_hokke_through_quoted_printable` |

**破損試験の結果：**

1. FTS5 からページ 1 件の索引だけを消した（本文は残す）。
2. 整合性検査の前は、FTS5 経路がそのページを取りこぼした。`Verify` モードは `fts_missed_pages(1)` で検出した。
3. `check` で不一致を検出した後は、自動で全件走査に切り替わり、HIT した。
4. `rebuild` で FTS5 経路に戻った。

**つまり、整合性検査を実行するまでの間は、索引の破損による取りこぼしが起こりうる**（XIV-11）。

## XIV-9. 実 MHTML 受入試験

**対象：** PO 提供の `ホッケ - Wikipedia.mhtml`（Chrome の「ページを保存」、約 832 KiB）。Git 管理外の場所のまま使い、本文・URL は転載しない。

**ファイルの検査結果：**

- ルート HTML パートは quoted-printable で、`<meta charset>` は UTF-8。MIME パートは 46 個。
- `𩸽` は QP で `=F0=A9=B8=BD` として 2 回現れる。うち 1 回は、4 バイトの途中に QP のソフト改行（`=\r\n`）が入っていた。
- Slice 1 の抽出結果：success、警告なし、本文 5,618 文字。`𩸽` は本文に 2 回、タイトル（15 文字）には 0 回。2 回とも抽出結果で確認した。

**試験：**

- `PAGE_FULLTEXT_REAL_HOKKE=<path> cargo test --release --test search -- --ignored --nocapture` で実行した。
- 合成ページ 20 件と同じ DB に入れて行った。DB は `target/` 配下の一時ファイルで、試験の最後に削除した。

| # | 受入条件 | 結果 |
|---|---|---|
| 1 | Slice 1 で正常に抽出できる | 成功（警告なし）。品質ゲートの判定は `ok`（本文・タイトルとも索引） |
| 2 | 抽出本文またはタイトルに `𩸽` が存在する | 本文に 2 回（抽出結果を数えて確認） |
| 3 | 1 文字検索 `𩸽` で HIT | HIT（全件走査）。合成ページは HIT しない |
| 4 | 実在する 3 文字以上で HIT | 抽出本文から機械的に切り出した文字列で、すべて HIT。`𩸽` の前後 3 文字と 8 文字の 6 通りは FTS5 経路、タイトル先頭 5 文字はタイトルで HIT。**網羅確認：** 本文の 3 文字おきの位置から 1・2・3・4・6・10 文字を切り出した 11,231 個がすべて HIT（うち 5,822 個は FTS5 経路、`Verify` で全件走査と一致） |
| 5 | 存在しない文字列で MISS | `𩸽𩸽`、`鮭𩸽鮭` 等の 4 個（抽出結果に無いことを確認してから使用）がすべて MISS |
| 6 | 再オープン後も同じ結果 | 同じ。整合性検査も成功 |
| 7 | 削除で検索結果から消える | 消えた。整合性検査も成功 |
| 8 | 再登録で重複しない | `updated`、page_id は同じ、ページ数は変わらず 21 |
| 9 | 本文・タイトルの更新が反映される | 本文を合成文に差し替えると、旧本文の 8 文字が MISS、新しい文が HIT。元の抽出結果で再登録すると `𩸽` が再び HIT |
| 10 | 検索結果に全文を出さない | 結果 JSON は 259 バイト（ページ ID・キー・URL・タイトル・HIT 箇所のみ）。本文の断片が含まれないことを確認 |

受入試験の判定は、PO と独立レビューに委ねる。

## XIV-10. 性能測定

**条件：**

- XIV-2 の PC、release ビルド、`synchronous = NORMAL`、WAL。
- 合成ページ：約 45 語の語彙（日本語・英語・絵文字・記号）からの乱数文。平均約 3,000 文字。50 件に 1 件は `𩸽を焼く` を含み、各ページは固有の目印（`頁000123番`）を含む。
- 1 ページ 1 トランザクションで登録した。検索は各 7 回の中央値。
- 実ページより語彙が少なく、trigram の重複が多い。索引サイズと共通語の検索時間は、実データと異なりうる。
- 時間は単発の環境依存値で、性能目標の合否判定には使っていない。

**登録・保守：**

| detail | ページ数 | 平均文字数 | DB サイズ | 登録（1 件、中央値） | 登録（全件、生成込み） | 更新（中央値） | 削除（中央値） | 整合性検査 | 再構築＋検査 |
|---|---|---|---|---|---|---|---|---|---|
| full | 100 | 3,070 | 2.6 MiB | 1.42 ms | 0.2 s | 2.05 ms | 1.32 ms | 0.07 s | 0.1 s |
| full | 1,000 | 2,984 | 20.1 MiB | 1.15 ms | 2.9 s | 1.74 ms | 1.09 ms | 0.59 s | 1.9 s |
| full | 10,000 | 3,008 | 188.8 MiB | 1.19 ms | 50.8 s | 1.83 ms | 0.87 ms | 5.52 s | 21.2 s |
| none | 100 | 3,070 | 1.5 MiB | 1.27 ms | 0.2 s | 2.25 ms | 2.09 ms | 0.09 s | 0.1 s |
| none | 1,000 | 2,984 | 10.3 MiB | 1.08 ms | 2.3 s | 1.53 ms | 0.92 ms | 0.54 s | 1.2 s |
| none | 10,000 | 3,008 | 89.8 MiB | 1.02 ms | 23.3 s | 1.53 ms | 0.81 ms | 5.37 s | 12.1 s |

- 10,000 件の全件登録は、中央値の積（約 12 s）より長かった（50.8 s / 23.3 s）。一部の登録が長く、外れ値がある（WAL のチェックポイントや FTS5 のセグメント併合と推測。内訳は未計測）。
- 10,000 件（本文約 3,000 万文字）で、DB は detail=full が 188.8 MiB、detail=none が 89.8 MiB。本文のバイト数は計測していないので、本文に対する倍率は出していない。

**検索（detail=full。各欄は「照合したページ数 / HIT 数 / 自動モード / 全件走査」）：**

| クエリ | 文字数 | 方式 | 100 件 | 1,000 件 | 10,000 件 |
|---|---|---|---|---|---|
| 1 文字・頻出（`の`） | 1 | 全件走査 | 100 / 100 / 1.99 ms / 1.96 ms | 1000 / 1000 / 27.0 ms / 27.5 ms | 10000 / 10000 / 237 ms / 239 ms |
| 1 文字・稀（`𩸽`） | 1 | 全件走査 | 100 / 2 / 1.72 ms / 1.72 ms | 1000 / 20 / 26.8 ms / 26.3 ms | 10000 / 200 / 231 ms / 241 ms |
| 2 文字・頻出（`東京`） | 2 | 全件走査 | 100 / 100 / 1.68 ms / 1.70 ms | 1000 / 1000 / 23.9 ms / 21.6 ms | 10000 / 10000 / 243 ms / 238 ms |
| 2 文字・不在 | 2 | 全件走査 | 100 / 0 / 1.68 ms / 1.68 ms | 1000 / 0 / 21.4 ms / 21.8 ms | 10000 / 0 / 237 ms / 230 ms |
| 4 文字・頻出（`について`） | 4 | FTS5 候補 | 100 / 100 / 3.22 ms / 1.77 ms | 1000 / 1000 / 38.8 ms / 22.9 ms | 10000 / 10000 / **405 ms** / 232 ms |
| `𩸽を焼` | 3 | FTS5 候補 | 2 / 2 / 0.23 ms / 1.91 ms | 20 / 20 / 1.20 ms / 21.6 ms | 200 / 200 / 9.79 ms / 241 ms |
| 固有の目印（8 文字） | 8 | FTS5 候補 | 1 / 1 / 0.27 ms / 1.80 ms | 1 / 1 / 0.97 ms / 22.1 ms | 1 / 1 / 2.26 ms / 229 ms |
| 不在・11 文字 | 11 | FTS5 候補 | 0 / 0 / 0.13 ms / 2.02 ms | 0 / 0 / 0.41 ms / 24.1 ms | 0 / 0 / 0.56 ms / 262 ms |
| 不在・3 文字 | 3 | FTS5 候補 | 0 / 0 / 0.12 ms / 1.84 ms | 0 / 0 / 0.41 ms / 22.0 ms | 0 / 0 / 0.58 ms / 235 ms |

detail=none でも傾向は同じだった。絞り込める検索は 10,000 件で 0.4〜11 ms、頻出 4 文字は 510 ms、全件走査は 221〜241 ms。

**所見：**

- **全件走査はページ数に比例する。** 約 3,000 文字のページで 1,000 件あたり約 23 ms、10,000 件（約 3,000 万文字）で約 230〜240 ms。1〜2 文字の検索は、この時間がそのままかかる。
- **3 文字以上で候補が少ない検索は、FTS5 で 1〜2 桁速い**（10,000 件で 0.4〜11 ms）。
- **ほぼ全ページに現れる 3 文字以上の検索は、FTS5 経路の方が遅い**（10,000 件で 405 ms、全件走査は 232 ms）。候補の照会に加えて、候補全件の本文を照合するため。製品では、候補数の見積もりで方式を選ぶ等の工夫が必要（XIV-12）。
- 登録・更新・削除は 1 件あたり 1〜2 ms（中央値）。整合性検査と再構築は、10,000 件で数秒〜20 秒程度かかる。

## XIV-11. セキュリティ・資源上の制限

- **SQL インジェクション：** SQL 文は定数だけで組み立てる。利用者の入力・ページ由来の文字列は、すべてバインド引数で渡す。FTS5 MATCH 構文は使わない（MATCH は比較試験のためだけに実装し、既定では使わない）。
- **出力：** CLI の既定出力は、コード・数値・ページ ID・キーだけ。タイトルと URL は `--show-titles` を指定したときだけ表示する。本文は出力しない。品質 JSON・試行記録にも本文は入れない。
- **DB の機密性：** PoC の DB には抽出本文が**平文で**入る（SQLCipher ではない）。MHTML 原本と同等に扱う必要がある。
  - CLI は Git 作業ツリー内への作成を拒否する。
  - 計測用 DB は合成ページだけで、scratchpad に置いた。
  - 受入試験の DB は `target/` 配下に作り、試験の最後に削除した。
- **既存 DB の保護：** この PoC が作ったもの以外の SQLite ファイルは、書き込まずに拒否する。
- **外部通信・スクリプト実行：** なし。
- **資源：**
  - 1〜2 文字の検索はページ数に比例する（上限なし）。
  - 検索結果の件数に上限はない（全ページ HIT もありうる）。
  - 検索文字列は 4,096 バイトまで。
  - 索引込みの DB は、合成 10,000 件（約 3,000 万文字）で 188.8 MiB（detail=full）。
  - 検索のタイムアウトや取り消しは未実装。
- **索引の破損：** 整合性検査を実行するまでは検出できない。その間は、FTS5 経路の検索がページを取りこぼしうる（XIV-8）。

## XIV-12. 既知の制限と、PO 判断待ち・持ち越し事項

**PO 判断待ち：**

1. **空白・改行の扱い：** Slice 1 の整形後の本文（ブロック境界は `\n`、空白は 1 個に畳む）に対する完全一致にしている。そのため、`東京 都内` は `東京\n都内` に HIT しない。検索文字列の空白を正規化するか（例：空白類を 1 個の空白に畳み、空白と改行を同一視する）。
2. **コードポイント単位の一致：** `👍` が `👍🏽` に、`か` が NFD の `か゛` に HIT する。書記素単位にするか。
3. **品質ゲートのしきい値**（XIV-6）：暫定値。実データ（特に本物の legacy 文字コードのページ）で再計測してから決める。
4. **本文が空の再抽出：** 既存の本文を「タイトルのみ」に更新している。文字化けと同じく既存を残すか。
5. **detail=full / none：** 正しさは同等（GLOB 経路、XIV-4）。DB サイズは約半分になるが、頻出語の検索はやや遅い。
6. **頻出語の 3 文字以上検索：** FTS5 経路が全件走査より遅い。候補数の上限や、方式の自動選択を入れるか。
7. **整合性検査の頻度：** 起動時、異常終了後、定期など、いつ実行するか（10,000 件で約 5.5 秒）。

**製品統合時への持ち越し：**

- SQLCipher 版 libsqlite3-sys での FTS5 trigram の動作確認（ビルドフラグはあるが未試験）。暗号化 DB での索引サイズ・速度。
- Vault の Page と索引の対応（`page_key`）、保存・削除と索引更新の順序と原子性（別 DB にするか、同じ DB にするか）。
- 索引作成をバックグラウンドで行うこと、検索の取り消し・タイムアウト、結果件数の上限・ページング。
- 検索結果のタイトル・URL は信頼できない外部入力なので、UI 表示時にエスケープする。
- 抽出処理の資源上限（Slice 1 XIII-11：別プロセス・Job Object）と合わせた設計。
- 品質ゲートのしきい値の確定、legacy → legacy の文字化けへの対策。
- iframe 本文・ルビ（Slice 1 から持ち越し）。

**既知の制限（PoC として）：**

- 実 MHTML は 1 件（𩸽）で試験した。品質ゲートの根拠の実ページは、XII のものを含めて 2 件。
- 合成ページによる性能測定は、語彙が少ない。
- 検索の同時実行（複数の接続）は試験していない。

## XIV-13. 独立レビュー指摘の修正（HIGH-1 / HIGH-2 / MEDIUM-1 / MEDIUM-2）

XIV-1〜XIV-12 は修正前の記録としてそのまま残し、修正と再検証の結果をここに追記する。検索方式（FTS5 trigram ＋ `GLOB` 候補 ＋ `str::contains` の最終判定）は PO 承認済みのため変更していない。

### 13-1. 変更したファイル（すべて `poc/page-fulltext/` 配下）

| ファイル | 変更 |
|---|---|
| `src/index.rs` | 開き方の分離と検証（HIGH-1 / MEDIUM-2）、`register` の保持条件（HIGH-2）、既定を `detail=none` に、スキーマ版 2 |
| `src/bin/page_index.rs` | Git 作業ツリー判定の作り直し（MEDIUM-1）、コマンドごとの開き方（MEDIUM-2）、`bench` は既存ファイルを削除せず拒否 |
| `tests/search.rs` | HIGH-2 の回帰テスト 1 件を追加。既存テストの `Index::open` を新 API に置換。受入試験の DB を Git 作業ツリーの外に移し、読み取り専用での再オープンを追加 |
| `tests/open_safety.rs`（新規） | HIGH-1 / MEDIUM-2 の回帰テスト 11 件 |
| `tests/cli_guard.rs`（新規） | MEDIUM-1 / MEDIUM-2 の回帰テスト 7 件（CLI を実行） |
| `README.md`、本レポート | 追記 |

### 13-2. HIGH-1：PoC 以外の SQLite DB を変更する問題

**原因：** `meta` 表に `schema_version = '1'` があるだけで PoC の DB と見なし、その直後に `PRAGMA journal_mode = WAL`（ファイルヘッダーへの書き込み）を実行していた。識別が 1 行の値だけで、検証より前に書き込みがあった。

**修正：** 既存ファイルは次の順でしか開かない（`Index::open_existing` / `Index::open_readonly`）。

1. **SQLite を使わずに**先頭 100 バイトを読み、SQLite のマジックと `application_id`（PoC 固有値 `0x4D435046`）を確かめる。他アプリの DB・空ファイル・非 SQLite はここで拒否され、SQLite はファイルを開きもしない。
2. `immutable=1` の読み取り専用接続（ロックを取らず、`-wal`/`-shm`/journal を作らない）で開き、次を確かめる。
   - `PRAGMA application_id`
   - `meta.schema_version`（現在は `2`。違えば `schema_mismatch`）
   - スキーマ全体：`sqlite_schema` の全行（type・name・tbl_name・SQL 文）を、同じ `detail` でメモリ内に作った基準スキーマと完全一致で比較する。表・列・索引・トリガー・FTS5 の `tokenize`/`detail`/外部コンテンツ設定・FTS5 の影の表がすべて対象。
   - FTS5 の設定表 `pages_fts_config` の行も基準と比較。
   - `meta` の行がちょうど `app`・`detail`・`fts_state`・`schema_version`・`tokenizer` の 5 行で、`app = 'memocampus.page-fulltext.poc'`、`tokenizer` が固定値、`fts_state` が `ok` か `inconsistent`。
3. すべて通ったときだけ本来の接続を開く（書き込み用は `READ_WRITE` で `CREATE` なし、読み取り用は `READ_ONLY`）。
4. その接続でも 2. と同じ検証をもう一度行い、通ってから初めて書き込む（`journal_mode` の変更を含む）。

`application_id` は識別の手がかりとして使い、それだけで受け入れることはない（2. のスキーマ・meta の検証と組み合わせる）。

新規作成は `Index::create` だけが行う。パスに何も無いことを `create_new` で排他的に確かめてからファイルを作り、1 つのトランザクションの中で「空であること」を確認したうえで `application_id`・スキーマ・`meta` を書く。この時点では rollback journal なので、ヘッダーとスキーマは最初から本体ファイルにある（1. と 2. が WAL の内容に依存しない）。失敗したら、自分が作ったファイルだけを消す。`Index::open_or_create` は「何も無ければ作成、あれば `open_existing`」で、空ファイルや他の DB を初期化することはない。

スキーマ版を 1 → 2 に上げた（`application_id` と `meta.app` を追加）。版 1 の DB は拒否されるので作り直す（移行はしない。指示書のとおり）。

**回帰テスト（`tests/open_safety.rs`）：** 拒否されるべきファイルについて、`open_existing`・`open_readonly`・`open_or_create`・`create` のすべてが拒否し、本体・`-wal`・`-shm`・`-journal` の内容（有無を含む）が前後で同じであることをバイト単位で比較する。

| 対象 | 結果 |
|---|---|
| 一般の SQLite DB（rollback journal / WAL） | `not_a_page_index`、変更なし |
| `meta.schema_version = '2'` だけを持つ偽装 DB | 同上 |
| PoC の `application_id` だけを持つ DB（rollback journal / WAL） | 同上 |
| `application_id` と正しい `meta` 行だけを持ち、表が無い DB | 同上 |
| 本物の索引から `attempts`・`pages`・`pages_fts`・トリガー 1 個を消したもの | 同上 |
| 列の追加、表の追加 | 同上 |
| FTS5 の tokenizer を `trigram`（`case_sensitive` なし）に変えたもの | 同上 |
| `meta.detail` と FTS5 の `detail` が食い違うもの | 同上 |
| `meta` の `app` 変更・行の追加・欠落・`fts_state` 不正値、FTS5 設定表の行の追加 | 同上 |
| `application_id` を別の値にしたもの | 同上 |
| スキーマ版 `1`・`3`・空文字 | `schema_mismatch`、変更なし |
| 空ファイル、非 SQLite（4 KiB の `x`）、マジックと ID だけ正しい壊れたファイル | `not_a_page_index`、変更なし（空ファイルは 0 バイトのまま） |
| ディレクトリ | 拒否 |
| 存在しないパス | `open_existing`・`open_readonly` は `not_found`。ファイルは作られない |
| 正常な PoC DB（`detail=none` / `full`） | 開ける。検索・検査・登録が動く。既存パスへの `create` は `already_exists` で、ファイルは不変 |

実 Vault を使った試験はしていない（禁止事項）。

### 13-3. HIGH-2：本文が空の再抽出による既存データの消失

**原因：** 既存ページを残す条件が「本文が文字化け、または索引できるものが何も無い」だけだった。そのため、タイトルが正常で本文が空の再抽出は「タイトルだけのページ」として `put_page` に進み、既存の本文と FTS5 の行を空で上書きしていた。

**修正（PO 方針）：** 既存ページの判定には、行があるかではなく `pages.body` が空でないか（本文はゲートを通ったときだけ保存されるので、索引された本文があるかと同じ）を使う。

| 既存の状態 | 新しい抽出 | 動作 |
|---|---|---|
| 索引済みの本文あり | 本文がゲートを通った | 更新（`updated`） |
| 索引済みの本文あり | 本文が空・文字化け（タイトルの状態に関係なく） | **既存を保持**（`kept_previous`）。タイトル・本文・FTS5 の行は変えない |
| 本文なし（タイトルのみ） | 本文が文字化け、または何も索引できない | 既存を保持 |
| 本文なし（タイトルのみ） | タイトルのみ・本文あり | 更新 |
| なし | タイトルのみ | 従来どおりタイトルだけ登録（`inserted`、`title_only_empty_body`） |
| どれでも | 抽出失敗 | 既存を保持（なければ `not_registered`） |

保持したときも、`attempts` に今回の結果（`kept_previous` と、`title_only_empty_body` などの状態・品質 JSON）を記録する。`Registration.page_id` は、保持した既存ページの ID を返すようにした（以前は `None`）。明示的な削除（`delete`）は従来どおり。

**回帰テスト（`tests/search.rs` の `g_empty_body_re_extraction_keeps_the_indexed_body`）：**

1. 正常な本文のページを登録。
2. 同じキーで、タイトルあり・本文空の抽出を再登録すると `kept_previous` になり、同じ page_id が返る。
3. 本文の 3 か所から取った 5 文字が、FTS5 経路・全件走査・Verify のすべてで引き続き HIT する。
4. 元のタイトルは HIT し、新しいタイトルは HIT しない。保存されているタイトル・本文は元の抽出と完全一致。FTS5 と SQLite の整合性検査はどちらも OK。
5. 本文が文字化けし ASCII タイトルが正常な再抽出も `kept_previous`。`attempts` は `inserted/ok`、`kept_previous/title_only_empty_body`、`kept_previous/title_only_garbled_body` の順に記録される。正常な本文での再登録は `updated`。
6. 新規ページのタイトルのみ登録は成功し、タイトルで HIT する。タイトルのみのページは、タイトルのみの再抽出でも本文ありの再抽出でも更新できる。
7. 削除すると検索結果から消え、最後は 0 ページで整合性 OK。

### 13-4. MEDIUM-1：Git 作業ツリーへの DB 作成ガード

**原因：** 祖先の検査が `ancestors().skip(1)` で、指定パス自身を見ていなかった（`bench --dir` にリポジトリのルートを渡すと通った）。また、パスを字面のまま検査し、シンボリックリンクやジャンクションの先を見ていなかった。判定できない場合は「Git 外」として扱っていた。

**修正（`git_guard`）：**

- パスを絶対パスにする。そのパス自身を含めて、存在する最も深い祖先を探し、`canonicalize` で実パスに解決する（シンボリックリンク・ジャンクション）。存在しない残りの部分は、その後ろに付け直す。
- 字面の絶対パスと実パスの両方について、パス自身を含むすべての祖先に `.git`（ディレクトリでもファイルでもよい。worktree・submodule は `.git` がファイル）が無いこと、およびパスが `.git` ディレクトリの中でないことを確かめる。
- 絶対パスにできない、存在する祖先が無い、実パスに解決できない、`.git` の有無を調べられない、のいずれでも拒否する（fail closed）。
- `--db` を取る全コマンド（`add`・`search`・`stats`・`delete`・`rebuild`・`check`）と `bench --dir` に適用する。`bench --dir` は作成前と作成後の 2 回確かめる。`--allow-db-in-git` は `--db` だけに効き、`bench` には上書き指定が無い。
- `bench` は既存の DB ファイル（`-wal`・`-shm`・`-journal` を含む）を削除して作り直していたが、拒否するように改めた（既存の生成物を消さない）。

**回帰テスト（`tests/cli_guard.rs`、CLI を実行）：** 試験用の「リポジトリ」は、システムの一時ディレクトリ（このリポジトリの外）に `.git` を置いたディレクトリ。

| 場合 | 結果 |
|---|---|
| リポジトリ直下、既存のサブディレクトリ、未存在の 2 階層下、`.git` ディレクトリの中 | 拒否、ファイルもディレクトリも作られない |
| リポジトリ内からの相対パス（`relative.sqlite3`、`../up.sqlite3`） | 拒否 |
| `.git` がファイルの worktree | 拒否 |
| このリポジトリ自身の `target/`（Git 無視対象だが作業ツリー内） | 拒否 |
| Windows ジャンクション（リポジトリのサブディレクトリを指す。字面のパスには `.git` の祖先が無い） | 拒否。ジャンクション下の未存在ディレクトリ、`bench --dir` も拒否 |
| シンボリックリンク（ディレクトリ・ファイル） | **この環境では未実行**。シンボリックリンクの作成に特権が要り（os error 1314）、テストは理由を表示して何もせずに成功する。同じ実パス解決をジャンクションで確認した |
| `search`・`stats`・`delete`・`rebuild`・`check` | 拒否 |
| `bench --dir` にリポジトリのルート・サブディレクトリ・その下の未存在ディレクトリ | 拒否、ディレクトリは作られない |
| リポジトリ外（絶対パス・相対パス） | 作成でき、`search`・`stats` も動く |
| リポジトリ外の `bench --dir`（20 ページ） | 動く。同じディレクトリでの 2 回目は「既存」で拒否し、既存の DB は変わらない |
| `--allow-db-in-git` | 従来どおり許可 |

### 13-5. MEDIUM-2：読み取り系 CLI の副作用

**原因：** すべてのコマンドが同じ `Index::open` を使っていた。存在しないパスには DB が作られ、空ファイルは初期化され、`search`・`stats` でも `journal_mode = WAL` を設定していた。

**修正：** 開き方を API で分けた。

| API | 使うコマンド | 作成 | 書き込み |
|---|---|---|---|
| `Index::open_readonly` | `search`、`stats` | しない（無ければ `not_found`） | しない（`SQLITE_OPEN_READ_ONLY`。書き込み系メソッドは SQLite に渡す前に `read_only` エラー） |
| `Index::open_existing` | `delete`、`rebuild`、`check` | しない | する（検証後に WAL） |
| `Index::open_or_create` | `add` | 何も無いときだけ | する |
| `Index::create` | `bench` | 必ず新規（既存なら `already_exists`） | する |

`check` は `meta.fts_state` を更新するため、書き込み系に分類した。読み取り専用で開いた索引は、`fts_state` が `ok` でなければ従来どおり全件走査に切り替える（テストで確認）。

### 13-6. SQLite の読み取り専用接続に関する確認結果

SQLite 3.46.0（rusqlite 0.32.1 bundled）、Windows 11、NTFS で、合成 DB を使って次を実測した。

| 状況 | 結果 |
|---|---|
| WAL モードの DB を通常の `READ_ONLY` 接続で開く（`-wal`・`-shm` が無い状態） | 読める。最初の読み取りで `-wal`（0 バイト）と `-shm`（32,768 バイト）が**作られ、閉じても残る**。DB 本体は不変 |
| 同上で、DB ファイルに読み取り専用属性を付けた場合 | 同じ（ディレクトリが書き込み可能なため、付随ファイルは作られる） |
| `immutable=1` の読み取り専用接続 | 読める。付随ファイルを何も作らない。ただし WAL の内容とロックを無視するので、他の接続が書き込み中だと古い内容や壊れた途中の状態を読みうる |
| 書き込み接続が開いている間の `READ_ONLY` 接続 | 書き込み側の `-wal`・`-shm` を使い、コミット済みの内容を読める |
| rollback journal モードの DB を `READ_ONLY` 接続で開く | 付随ファイルを作らない |
| `READ_ONLY`、または `READ_WRITE`（`CREATE` なし）で存在しないパス | エラー。ファイルは作られない |
| `READ_ONLY` 接続での TEMP 表の作成 | できる（ただし FTS5 の有無の確認は、別のメモリ内接続で行うようにした） |

**採った方式：**

- 検証用の接続（13-2 の 2.）は `immutable=1` にした。付随ファイルを作らず、検証前のファイルに何も残さない。古い内容を読みうるのは、本来の接続での再検証（4.）で補う。
- 書き込みセッションは従来どおり WAL で動き、`Index` を閉じるときに `journal_mode = DELETE` に戻す。これで WAL がチェックポイントされて `-wal` が消え、ヘッダーの 18〜19 バイト目が 1/1（rollback journal）になる。閉じている DB を後から `READ_ONLY` で開いても、付随ファイルは作られない。

**実測（テストで確認）：**

- 書き込みセッション中は `-wal` がある。閉じた後は `-wal`・`-shm`・`-journal` が無く、ヘッダーは 1/1（`m_writable_session_leaves_no_side_files`）。
- `open_readonly` で `search`（FTS5 経路・Verify を含む）と `stats` を行ったとき、開いている間も閉じた後も、本体・`-wal`・`-shm`・`-journal` の内容と有無がバイト単位で不変（`m_read_only_search_and_stats_change_nothing`）。
- CLI の `search`・`stats` でも同じく不変（`c_read_only_commands_create_nothing`）。
- 実 MHTML の DB でも同じく不変（受入試験 6）。
- DB ファイルに読み取り専用属性を付けても、`open_readonly` で検索でき、ファイルは不変。`open_existing` はエラーになり、ファイルは不変（`m_read_only_file_attribute`）。

**残る制約：** `journal_mode = DELETE` への切り替えは、他の接続が DB を開いていると失敗する（失敗は無視し、DB は WAL のまま残る）。書き込み中に異常終了した場合も WAL のまま残る。そのような DB を後から `READ_ONLY` で開くと、上の表のとおり `-wal`・`-shm` が作られる。ただし、作られるのは検証済みの PoC DB の付随ファイルだけで、DB 本体は変わらない。

### 13-7. テスト結果

| 対象 | 件数 |
|---|---|
| Slice 1（`integration` 31・`tokenizer` 22・`end_tags` 12） | 65 件成功（変更なし） |
| Slice 2 `search`（既存 31 ＋ HIGH-2 の 1） | 32 件成功、受入試験 1 件は既定で無視 |
| Slice 2 `open_safety`（新規） | 11 件成功 |
| Slice 2 `cli_guard`（新規） | 7 件成功（うちシンボリックリンクの 1 件は、この環境では中身を実行せずに成功。13-4） |
| **合計** | **115 件成功・0 失敗・無視 1 件** |

- `cargo test --release`、`cargo clippy --release --all-targets -- -D warnings`（警告 0）、`cargo fmt --check`、`cargo build --release` はすべて成功した。
- 既存テストの修正は、新 API への置換と、外部 DB の拒否エラーのコードが `schema_mismatch` から `not_a_page_index` に変わったことへの合わせ込みだけ。

### 13-8. 実 MHTML 受入試験（再実行）

PO 提供の 𩸽 ページ（XIV-9 と同じファイル）で、受入試験 1〜10 がすべて XIV-9 と同じ結果になった。

- 抽出は本文 5,618 文字、警告なし。𩸽 は本文に 2 回、タイトルに 0 回。
- `𩸽` は全件走査で HIT。𩸽 を含む 3 文字以上は FTS5 経路で HIT。
- 掃引した 11,231 個の部分文字列がすべて HIT し、うち 5,822 個は FTS5 経路だった。
- 存在しない 4 語は MISS。
- 再登録で重複なし。更新が反映され、削除も反映される。結果 JSON は 259 バイトで本文を含まない。

今回、受入試験 6 に「読み取り専用で開き直して検索し、ファイル群が不変」を加えた。また、受入試験の DB はシステムの一時ディレクトリ（Git 作業ツリーの外）に作り、試験の最後に削除する（削除されたことも確認する）。

性能の確認：1,000 ページ・`detail=none`・5 回中央値で、XIV-10 と同水準だった。

- 1〜2 文字（全件走査）：約 22 ms
- 候補が少ない 3 文字以上：0.2〜1.2 ms
- 頻出 4 文字：51 ms（全件走査は 22 ms）
- 登録：1.1 ms／件
- DB：10.3 MiB

### 13-9. 残存する制限とリスク

- **検証から書き込みまでの間の差し替え：** 本来の接続を開いた後、最初の書き込みの前に同じ接続で再検証している。ただし、その後に別プロセスがファイルを置き換える場合は対象外（Windows では開いているファイルの置き換えは通常失敗する）。
- **`immutable=1` での検証と書き込み中の DB：** 別のプロセスが書き込み中の正当な DB を、検証段階で拒否することがありうる。拒否する方向（fail closed）にだけ起きる。
- **完全な偽装：** `application_id`・スキーマ・`meta` まで PoC と完全に同じファイルは、PoC の DB として受け入れる。構造上、PoC の DB と区別できないためで、他アプリの DB がこの条件を満たすことは想定しにくい。
- **Git の判定範囲：**
  - `.git` の有無だけで判定するので、`GIT_DIR`・`GIT_WORK_TREE` 環境変数で作業ツリーを外に置く構成と、bare リポジトリは検出しない。
  - 判定から作成までの間にパスの途中がジャンクションへ差し替えられる競合は防げない。
  - ガードは CLI のもので、ライブラリ API（`Index::create` 等）自体は Git を検査しない。テストは合成テキストの DB を `target/tmp` に作る。
- **シンボリックリンク：** この環境では作成できず、試験していない（ジャンクションで代替確認）。
- **テストの生成物：** `cli_guard` は、合成テキストだけの試験用ディレクトリをシステムの一時ディレクトリ（`page-index-guard-<pid>`）に残す。
- **既存 DB：** 版 1 の PoC DB は開けない（作り直し）。

### 13-10. PO 判断待ち事項（更新）

- XIV-12 の 4（本文が空の再抽出）は、PO 方針どおりに実装した（13-3）。XIV-12 の 5 は、`detail=none` を既定にした（`full` は比較・テスト用に残す）。
- XIV-12 の 1・2・3・6・7 は今回の対象外のため、変更していない。
- 新たに判断が必要な点：
  1. 書き込みセッションを WAL で行い、閉じるときに rollback journal へ戻す方式でよいか（常に rollback journal にする案もある。13-6）。
  2. Git 判定の範囲（13-9）を PoC として十分とするか。
  3. 読み取り専用コマンドにも Git ガードをかけている（`--allow-db-in-git` で解除）が、これを維持するか。

## XIV-14. ジャーナルモードを常時 rollback journal に統一（PO 決定）

XIV-13-6 の「書き込みセッションは WAL、閉じるときに rollback journal へ戻す」方式を取りやめた。PoC 専用 DB は常に SQLite 標準の rollback journal で動く。XIV-13 のその他の修正（`application_id` とスキーマの検証、書き込み接続前の読み取り専用検証、既存の正常な本文の保持、Git 作業ツリーのガード、読み取り専用コマンド）と、検索方式・`detail=none`・品質ゲートは変更していない。

### 14-1. 変更内容

- **削除したもの：**
  - `PRAGMA journal_mode = WAL` と `PRAGMA synchronous = NORMAL` を設定する処理（`writable_session`）。
  - 閉じるときに `journal_mode = DELETE` へ戻す処理（`Drop`）。
  - `Options.synchronous_full`。
  - ベンチマークの `wal_checkpoint`。
  - ジャーナルモードを設定・変更する処理は、コードに一つも残っていない。
- **既定値で動かす：** `journal_mode = DELETE`、`synchronous = FULL`。どちらも SQLite の既定値で、テストで確認した。
- **WAL 形式の DB を拒否する：** ヘッダーの 18〜19 バイト目が 1/1（rollback journal 形式）でなければ、SQLite で開く前に拒否する。検証時には `PRAGMA journal_mode` を引数なしで読み（変更しない）、`delete` であることも確かめる。
- **書き込み禁止のファイル：** `open_existing` は書き込み禁止のファイルを `read_only` で拒否する。SQLite は、書き込み禁止のファイルをエラーなしで読み取り専用として開くため。以前は WAL への切り替えの失敗で、結果的に拒否されていた。
- **ホットジャーナルが残った DB の読み取り専用オープン：** 読み取り専用接続では巻き戻しできないので、新しいエラー `needs_recovery` で拒否する（SQLite の `SQLITE_READONLY_ROLLBACK`）。書き込み用に開く（CLI では `check` など）と、SQLite が標準の手順で巻き戻す。

### 14-2. journal / WAL / SHM の挙動（実測）

| 状況 | 本体 | `-journal` | `-wal` / `-shm` |
|---|---|---|---|
| 書き込み用に開いている間、トランザクションの外（登録・更新・削除・検査・再構築の各コミット後） | 更新される | 無い（コミットで削除） | 作られない |
| 正常終了（閉じた後） | ヘッダー 1/1 | 無い | 無い |
| 読み取り専用での検索・統計表示（開いている間・閉じた後） | バイト単位で不変 | 無い | 無い |
| トランザクションの途中で異常終了（子プロセスの `abort`） | 未コミットのページが書き出された状態（キャッシュあふれ） | **残る（ホットジャーナル）** | 無い |
| その DB を読み取り専用で開く | 不変 | 不変 | 無い（`needs_recovery` で拒否） |
| その DB を書き込み用に開く | **異常終了前とバイト単位で一致**するまで巻き戻される | 削除される | 無い |
| コミット直後に異常終了 | コミットした内容を保持 | 無い | 無い |
| 他アプリの DB にホットジャーナルが残っている場合 | 不変（ヘッダー検査で拒否し、SQLite は開かない） | 不変（巻き戻さない） | 無い |

### 14-3. 原子性と障害時の復旧（`tests/journal.rs`、4 件）

- **`j_crash_inside_a_transaction_is_rolled_back_by_sqlite`：**
  - 20 ページを確定した DB を用意する。子プロセスが 1 つのトランザクションの中で 200 ページの挿入と 1 ページの削除を行い、コミットせずに `abort` する。ページキャッシュを 2 ページにして、未確定のページが本体に書き出されるようにした。
  - 本体が変化し、ホットジャーナルが残ったことを確認した。
  - 読み取り専用での再オープンは `needs_recovery` になり、ファイルは不変。
  - 書き込み用の再オープンで巻き戻され、本体は異常終了前と**バイト単位で一致**し、`-journal` は消えた。20 ページは FTS5 経路と全件走査の両方で見つかり、未確定の本文は見つからない。FTS5 と SQLite の整合性検査は OK。その後の登録・検索・読み取り専用オープンも正常。
- **`j_crash_after_a_commit_keeps_the_committed_page`：** コミット直後に `abort` しても、付随ファイルは残らない。コミットしたページは検索でき、読み取り専用オープンでもファイルは不変。
- **`j_hot_journal_of_another_database_is_not_touched`：** ホットジャーナルが残った一般の SQLite DB は、3 つの開き方すべてで `not_a_page_index` になり、本体もジャーナルも不変（PoC が他の DB を巻き戻すことはない）。
- **`j_failed_transactions_on_a_file_keep_the_committed_data`：**
  - ファイル上の DB で、障害注入（ページ書き込み直後、コミット直前、再構築中）によりトランザクションを途中で失敗させた。
  - 失敗のたびに `-journal` は残らない。開き直すと確定済みの 20 ページと `attempts` は元のまま、途中の本文は見つからない。整合性も OK。

### 14-4. テスト結果

| 対象 | 件数 |
|---|---|
| Slice 1（`integration` 31・`tokenizer` 22・`end_tags` 12） | 65 件成功（変更なし） |
| Slice 2 `search` | 32 件成功 |
| Slice 2 `open_safety`（WAL 形式の拒否を追加。WAL 前提だった検査を rollback journal 用に書き換え） | 11 件成功 |
| Slice 2 `cli_guard` | 7 件成功（シンボリックリンクの 1 件はこの環境では未実行。13-4） |
| Slice 2 `journal`（新規） | 4 件成功 |
| **合計** | **119 件成功・0 失敗・無視 2 件**（実 MHTML 受入試験と、異常終了用の子プロセスとして呼ぶ `crash_child`） |

- `cargo clippy --release --all-targets -- -D warnings`（警告 0）、`cargo fmt --check`、`cargo build --release` はすべて成功した。
- 実 MHTML（𩸽 ページ）の受入試験 1〜10 は、XIV-9・XIV-13-8 と同じ結果ですべて成功した。
  - 掃引した 11,231 個の部分文字列がすべて HIT し、うち 5,822 個は FTS5 経路だった。
  - 読み取り専用での再オープンで、ファイル群は不変だった。
  - 結果 JSON は 259 バイト。
  - 試験用 DB は Git 作業ツリーの外に作り、試験後に削除した。

### 14-5. 性能への影響（`detail=none`、7 回中央値、合成ページ）

| 項目 | 100 件 | 1,000 件 | 10,000 件 | 参考：WAL ＋ `synchronous=NORMAL`（XIV-10、10,000 件） |
|---|---|---|---|---|
| 登録（1 件、中央値） | 15.0 ms | 8.9 ms | 7.9 ms | 1〜2 ms |
| 登録（全件の合計） | 1.6 秒 | 13.5 秒 | 126 秒 | — |
| 更新／削除（中央値） | 15.6 / 14.8 ms | 29.1 / 17.6 ms | 7.1 / 6.2 ms | 1〜2 ms |
| 整合性検査 | 0.07 秒 | 0.56 秒 | 5.3 秒 | 約 5.5 秒 |
| 再構築＋検査 | 0.15 秒 | 1.2 秒 | 12.0 秒 | 約 12 秒 |
| DB サイズ | 1.5 MiB | 10.3 MiB | 89.8 MiB | 89.8 MiB |
| 1〜2 文字（全件走査） | 1.8 ms | 22 ms | 235〜250 ms | 約 230 ms |
| 候補が少ない 3 文字以上 | 0.4〜0.6 ms | 0.5〜1.4 ms | 0.7〜12 ms | 0.4〜11 ms |
| 頻出 4 文字（FTS5 経路／全件走査） | 3.5 / 1.9 ms | 50 / 22 ms | 542 / 256 ms | 510 / 232 ms |

- 検索・検査・再構築・DB サイズは、WAL のときと同水準だった。
- 書き込みは、コミットごとに journal と本体を `fsync` するため、1 件あたり数 ms〜十数 ms に増えた（WAL のときの数倍）。
- ばらつきはディスクの状態に左右される。この環境では、100 件のときの方が 1 件あたりが遅かった。
- 大量の一括登録を 1 トランザクションにまとめる API は、今回は追加していない（14-7）。

### 14-6. 既知の制限

- **異常終了後の読み取り専用操作：** 異常終了でホットジャーナルが残った DB は、書き込み用に一度開く（CLI では `check` など）まで、`search`・`stats` が `needs_recovery` で失敗する。
- **異常終了後の検証の順序：** 書き込み用に開いたとき、SQLite の巻き戻しは、その接続での再検証（XIV-13-2 の手順 4）の最初の読み取りで行われる。これは、ヘッダー検査と `immutable=1` での検証（手順 1〜2）に通った後に限られる。`immutable=1` での検証は、ジャーナルを無視して、異常終了時の本体をそのまま読む。この試験ではスキーマのページは壊れておらず通過したが、本体がスキーマのページまで中途半端に書き換わった状態では、検証で拒否されて自動では復旧しない（拒否する方向で、ファイルは変えない）。
- **同時実行：** rollback journal では、書き込みトランザクションの間（コミット中）は読み取りが待たされる（`SQLITE_BUSY`）。PoC では待ち時間（busy timeout）を設定しておらず、同時に実行する試験もしていない。
- **電源断：** `synchronous=FULL` を前提とする。電源断そのものは試験していない（試験はプロセスの異常終了だけ）。
- XIV-13-9 の制限（Git 判定の範囲、シンボリックリンクの未試験、完全な偽装など）は、そのまま残る。

### 14-7. 最終再レビューへの引き継ぎ事項

1. ジャーナルモードを設定・変更する処理が残っていないか（`src/index.rs` に `journal_mode =` が無いこと）。WAL 形式の DB を拒否する検査が、ヘッダー（SQLite を使わない）と接続上の検証の両方にあるか。
2. `needs_recovery` の扱い。異常終了後に、ユーザーが書き込み用に開いて復旧するという運用でよいか。
3. 書き込み性能の低下（14-5）を PoC として許容するか。製品統合時の一括登録・トランザクション設計への持ち越しとするか。
4. XIV-13-10 の 2・3（Git 判定の範囲、読み取り専用コマンドへの Git ガード）は未決のまま。XIV-13-10 の 1 は、本節で決着した。
