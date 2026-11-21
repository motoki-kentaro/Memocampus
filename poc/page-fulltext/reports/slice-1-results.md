# PageVault 全文検索 PoC Slice 1 — 結果報告

- 日付：2026-11-21
- ブランチ：`poc/page-vault-fulltext-search`（作業開始時の未コミット変更は未追跡の `assets/icon_new.png` のみ。触れていない）
- 対象：`poc/page-fulltext/` 配下のみ。製品コード・既存ワークスペース・Vault は無変更。commit / push / PR なし。
- 環境：Windows 11（10.0.26320）、Intel Core i7-13700F、RAM 31.7 GB、rustc 1.100.0、Chrome 158.0.8127.36

## I. 総合判定

**判定保留（PO と独立レビューの判定待ち）。実装者による PASS 確定はしない。**
初回報告時の NOT VERIFIED は、実ページ受入試験（XII）を 1 件実施したことで更新した。
実ページは 1 件だけで、指示書が想定した 5 種類は揃っていない。

| 項目 | 結果 |
|---|---|
| 独立 Rust CLI のビルド | PASS（release、clippy 警告 0、rustfmt 済み） |
| 自動テスト | PASS（31 件成功・0 失敗・0 スキップ） |
| 合成ページを Chrome に保存させた MHTML（6 件）からの抽出 | PASS（ただし下記 VIII の制約付き） |
| 実ページ MHTML（PO 提供、1 件：日本語 Wikipedia のページ） | 抽出成功。所見と制限は XII（判定は PO / 独立レビュー） |
| 実ページ MHTML（日本語技術記事・英語技術記事・日本語ニュース・複雑 HTML・JS 多用） | **NOT VERIFIED**（上の 1 件以外は未入手） |
| `chrome.pageCapture.saveAsMHTML` 製であること | PO 申告による。ファイルからは判別できない（XII-1） |

## II. 採用ライブラリ

| crate | 版 | ライセンス | 採用理由 |
|---|---|---|---|
| mail-parser | 1.0.1 | Apache-2.0 OR MIT | multipart/related・Content-ID / Content-Location・各種パラメータを取得できる。入力を借用して索引のみ作る。`decoded_len()` で**復号前に**復号後サイズが分かる。パート数・深さの上限を持つ。問題をフラグで報告する（寛容だが、報告を厳格層でエラー化できる） |
| encoding_rs | 0.8.44 | (Apache-2.0 OR MIT) AND BSD-3-Clause | WHATWG Encoding Standard のラベル解決と復号（Shift_JIS / EUC-JP / ISO-2022-JP / windows-1252 など） |
| scraper | 0.29.0 | ISC | html5ever（HTML5 準拠パーサ）で DOM 相当の木を作る。`default-features = false` |
| ego-tree | 0.11.1 | ISC | scraper の木の型。反復（非再帰）走査に使う |
| serde / serde_json | 1.0.229 / 1.0.154 | MIT OR Apache-2.0 | JSON 出力 |

実行時の推移的依存 55 crate（`Cargo.lock` で固定）。ライセンス内訳：MIT OR Apache-2.0 系 36、MIT 10、ISC 2、
MPL-2.0 4（`selectors`、`cssparser`、`cssparser-macros`、`dtoa-short`。scraper の CSS セレクタ機能由来で、PoC は使わない）、
BSD-3-Clause を含むもの 1（encoding_rs）、Unicode-3.0 を含むもの 1、Unlicense OR MIT 1。
ネットワークを使う依存・HTML 外部サービス・JavaScript エンジンは無い。

不採用・保留：自作 MIME パーサ（境界・折り返し・パラメータを一から書く検証コストが大きい）。
html5ever を直接使う構成（MPL-2.0 依存を避けられるが、木の実装を自前で持つ必要がある。XI の論点）。

## III. アーキテクチャ

```text
--input <file>
  │ read_input: 通常ファイルか / サイズ ≤128MiB を読む前に判定 / 上限+1 バイトまでしか読まない
  ▼
bytes (Vec<u8>, 入力全体)
  │ mime::select_root
  │   mail-parser: MessageParser::new().max_parts(1024).max_depth(16).max_encoded_nesting(0).parse
  │   厳格層: 最上位 multipart/related・boundary 形式・全パートのフラグ・message/* 拒否・
  │           全葉パートの CTE 値と base64/QP 内容（復号なし）・HTML パート有無
  │   ルート選択: start → Content-ID / なければ先頭パート / text/html でなければ拒否
  │   decoded_len ≤16MiB を確認してから decoded_checked（問題があればエラー）
  ▼
root HTML bytes（転送復号済み、文字コード未変換）
  │ charset::decode: BOM → MIME charset → <meta> プレスキャン(1024B) → 既定 UTF-8、encoding_rs で変換
  ▼
UTF-8 String
  │ scraper::Html::parse_document（html5ever）
  ▼
DOM ──metadata::extract──▶ title / description / keywords / og:title / og:description / article:tag
    └─html::body_text────▶ <body> 可視テキスト（反復走査、8MiB 上限）
  ▼
Extraction（JSON 契約 memocampus.page-fulltext.poc/1）
  既定: 要約行のみ表示 / --output 指定時のみ create_new で JSON 作成
```

## IV. MIME 解析の仕様

**Chrome の実出力で確認した構造**（`tests/fixtures/chrome/*.mhtml`、Chrome 158、CDP `Page.captureSnapshot`）：

- 先頭ヘッダ：`From: <Saved by Blink>`、`Snapshot-Content-Location: <ページURL>`、`Subject:`（タイトルの encoded-word）、
  `Date:`、`MIME-Version: 1.0`、`Content-Type: multipart/related; type="text/html"; boundary="----MultipartBoundary--…----"`。
  **`start` パラメータは無い。**
- 第 1 パートが主文書：`Content-Type: text/html`（**charset 無し**）、`Content-ID: <frame-…@mhtml.blink>`、
  `Content-Transfer-Encoding: quoted-printable`、`Content-Location: <ページURL>`。
- Chrome はシリアライズ時に `<meta http-equiv="Content-Type" content="text/html; charset=…">` を挿入する。
  UTF-8 ページは `UTF-8`、Shift_JIS ページは `Shift_JIS` のまま、**本文も元の文字コードのバイト列**で保存される。
- `<script>` 要素はシリアライズ時に除去される。JS が作った DOM（`innerHTML`・`document.write` の結果）は残る。
  JS が付けたインライン `style="display: none"` も属性として残る。
- iframe は別の `text/html` パート（`Content-ID: <frame-…@mhtml.blink>`）になり、本文側は `src="cid:frame-…"` で参照。
  srcdoc フレームのパートには `Content-Location` が無い。
- 画像は `base64`、CSS は `quoted-printable`。すべて ASCII（8bit パートは観測されなかった）。

**ルート選択規則**：(1) `start` があれば一致する Content-ID の直下パート。一致なし→`start_not_found`、
複数→`start_ambiguous`。(2) 無ければ先頭の直下パート（RFC 2387 の既定。Chrome の挙動と一致）。
ルートが `text/html` でなければ `root_not_html`（他の HTML を黙って採らない）。HTML パートが 1 つも無ければ `no_html_part`。
ルートの `Content-Location` と `Snapshot-Content-Location` が異なる・ルートに Content-Location が無い・
同じ Content-Location の HTML パートが複数ある、は警告。副文書（iframe）は数えるだけで抽出しない。

**検証（ライブラリの解析成功 ≠ 妥当）**：

| 不正 | 扱い | mail-parser 単体の挙動（テストで確認） |
|---|---|---|
| boundary パラメータ欠落・RFC 2046 形式外（71 文字以上など） | `invalid_boundary` | — |
| 宣言した boundary が本文に無い | `malformed_multipart` | **フラグ無し**で最上位を 1 個の葉として返す |
| 閉じ区切り欠落 | `unterminated_multipart` | `UNTERMINATED` を付けて成功 |
| 行途中の区切り・ヘッダ後の空行欠落 | `malformed_multipart` | `FALLBACK_DELIMITER` / `NO_BLANK_LINE` / `MISSING_DELIMITER` |
| パート数超過（>1024）・入れ子超過（>16） | `too_many_parts` / `nesting_too_deep` | `LIMIT_REACHED` を付けて途中まで |
| `message/*` パート | `unexpected_nested_message` | — |
| 未知の Content-Transfer-Encoding | `invalid_transfer_encoding` | `UNKNOWN_TRANSFER_ENCODING` |
| 不正 base64（アルファベット外・長さ・パディング） | `invalid_base64`（**全**葉パートを復号せず検査） | アルファベット外を読み飛ばして復号 |
| 不正 quoted-printable（`=ZZ` など） | `invalid_quoted_printable`（同上） | 書かれたまま残す |
| 7bit 宣言に 8bit バイト | 警告 `seven_bit_part_has_8bit_bytes` | — |
| MIME でない（素の HTML 等） | `not_mime` / `not_multipart_related` | `None` / 単一パート |

**文字コード**：BOM → MIME `charset` → `<meta charset>` / `<meta http-equiv=Content-Type content=…charset=…>`
（先頭 1024 バイト、コメントは飛ばす）→ 既定 UTF-8（警告 `encoding_not_declared_utf8_assumed`）。
未知ラベルは警告（`unknown_mime_charset_label_ignored` / `unknown_meta_charset_label_ignored`）して次の手段へ。
`replacement` に解決されるラベル（例：`iso-2022-kr`）は `unsupported_charset`。`<meta>` の UTF-16 は UTF-8、
`x-user-defined` は windows-1252（HTML 標準）。不正バイト列は U+FFFD にして `malformed_bytes_replaced_with_u_fffd`。
出力 JSON に `encoding.name` / `source`（`bom` / `mime_charset` / `meta_prescan` / `default`）/ `mime_charset` / `malformed_sequences_replaced` を残す。

## V. HTML 抽出仕様

- 対象：`<body>` 配下のテキストノード。`<body>` が無ければ空（警告 `no_body_element`）。
- 除外（部分木ごと）：script / style / noscript / template / svg / canvas / head / title / iframe / frame /
  frameset / object / embed / noembed / noframes。コメントはテキストノードではないので含まれない。
- 静的に判定できる非表示：`hidden` 属性、`aria-hidden="true"`（大文字小文字無視）、インライン `style` の
  `display: none` / `visibility: hidden|collapse`（`!important`・空白・大文字小文字を許容）。外部 CSS・クラス指定・
  JS による変化は判定しない。
- 区切り：ブロック要素（p / div / h1–h6 / li / tr / table / section / article など）と `<br>` / `<hr>` は改行。
  `td` / `th` は空白。インライン要素は連結（`<b>bold</b><i>italic</i>` → `bolditalic`、描画と同じ）。
  `<p>東京都</p><p>江東区</p>` → `東京都\n江東区`。
- 空白：Unicode の空白（NBSP・U+3000 を含む）の連続を 1 個の ASCII 空白に。`pre` 内の改行は保持。
  行頭・行末の空白と空行は出さない。改行以外の制御文字（NUL を含む）は除去し、件数を `dropped_control_characters` に。
- 走査は ego-tree の `traverse()` による反復（10 万段の入れ子 `<div>` でスタック溢れしないことをテスト済み）。
- 記事本文の推定（Readability 相当）はしない。nav / header / footer / aside の文字も含まれる（例：「ホーム | 概要」「フッター」）。
- `img` の `alt`、`input` の値、`title` 属性などの属性テキストは含めない。

メタデータは README「抽出仕様」を参照。要点：欠落は `null`、`keywords` は `,` `、` `，` `;` で分割、
`article:tag` は複数値（文書順・重複除去）、単一値の重複は最初を採用して警告、空値は `null` にして警告、
svg 内の `<title>` は文書タイトルにしない。すべて未信頼文字列。

## VI. 資源制限

| 対象 | 上限 | 実装 | テスト |
|---|---|---|---|
| MHTML 入力 | 128 MiB | 読み込み前のサイズ判定＋読み込み量の制限 | 128 MiB+1 のファイルで `input_too_large`（読まずに）／135 MB の合成 MHTML で同じ |
| HTML 復号後 | 16 MiB | `decoded_len()` で**復号前**に判定、復号後も再確認 | 16 MiB+1 バイトの base64 HTML で `html_too_large` |
| 抽出本文 | 8 MiB | 1 文字ごとに判定して即中止 | 約 9 MiB の本文で `text_too_large`、境界値も確認 |
| MIME パート | 1024 | mail-parser の上限（到達＝エラー） | 1101 パートで `too_many_parts`、小さい上限で境界値 |
| MIME 入れ子 | 16 | mail-parser の上限（到達＝エラー） | 3 段は成功、20 段で `nesting_too_deep` |
| メタデータ 1 項目 | 64 KiB | 項目ごと（`article:tag` は合計） | 64 KiB は成功、+1 バイトで `metadata_too_large` |

**保証できる上限**：入力バイト数、HTML 復号後バイト数、本文バイト数、パート数、入れ子、メタデータ長。
HTML 復号の前に確保が起きない（base64 / QP で `decoded_len` が使える場合）。

**保証できない上限（実態）**：
- 入力全体をメモリに読み込む（最大 128 MiB）。mail-parser は借用で索引を作るので追加はわずか
  （125 MiB 入力でヒープのピーク 140 MB）。
- **DOM のメモリは HTML サイズで抑えられない**。html5ever の木はノードごとに確保するため、マークアップ中心の
  16.5 MB の HTML で**ヒープのピーク約 399 MB（HTML の約 24 倍）**、ワーキングセット 385 MB。
  ノード数の上限、またはプロセス全体のメモリ上限を強制する仕組みは無い。
- 計測は計測専用アロケータ（ヒープのピーク）と `K32GetProcessMemoryInfo`（ピークワーキングセット）による。制限ではない。

## VII. テスト結果

実行コマンド（`poc/page-fulltext/` で）：

```text
cargo build --release                        → 成功
cargo fmt --check                            → 差分なし
cargo clippy --release --all-targets         → 警告 0
cargo test --release                         → 31 passed / 0 failed / 0 ignored（integration）、
                                               lib・bin の単体テスト 0 件（すべて tests/integration.rs）
```

| 分類 | テスト | 件数 |
|---|---|---|
| A. MIME | 正常 multipart/related と非 HTML（画像・CSS・JS パート）の非混入、base64 / QP / 8bit / binary のルート、QP ソフト改行、`start` による選択と未発見・重複、先頭が非 HTML の拒否、HTML なし、boundary 欠落・長すぎ・不一致・閉じ欠落、不正 base64（ルート・画像）・不正 QP・未知 CTE、パート数上限（境界値）、入れ子上限、`message/rfc822`・非 MIME | 11 |
| B. HTML 本文 | 日本語・英語・混在・エンティティ・絵文字・段落区切り・空白改行・リスト・表・インライン連結／script・style・noscript・template・svg・canvas・コメント・hidden・aria-hidden・display:none・visibility:hidden・iframe 除外／メタデータと本文の分離 | 3 |
| C. メタデータ | 全項目・大文字小文字・keywords 分割・article:tag 複数と重複・空値・svg 内 title／欠落は null／Shift_JIS（MIME）・Shift_JIS（meta）・EUC-JP（http-equiv＋QP）・MIME 優先・BOM 優先／未知ラベル・replacement 拒否・meta の UTF-16 | 4 |
| D. セキュリティ | 128 MiB 超を読まずに拒否・ディレクトリ拒否、16 MiB 超 HTML、8 MiB 超本文、64 KiB 超メタデータ、NUL 等の制御文字と不正 UTF-8、エラーコードに本文を含まない、**変異入力 2000 件で panic なし**、10 万段の入れ子 | 8 |
| mail-parser 単体の寛容性 | 閉じ欠落・不正 base64・境界不一致・Shift_JIS 誤変換を、ライブラリ単体は成功として返すこと | 1 |
| CLI | 既定で本文・メタデータ・URL を出さない、`--output` 指定時だけ JSON、既存ファイルを上書きしない（終了コード 3・内容不変）、失敗時に出力ファイルを作らない | 2 |
| Chrome 製 fixture | ja-article（除外カナリア 12 種、メタデータ）、sjis-news（Shift_JIS・機種依存文字・半角カナ）、complex（iframe 2 個を抽出しない）、js-heavy（JS 生成 DOM を含み、JS で隠した要素を除外） | 2 |

所要時間の大半（約 29 秒）は 16 MiB 級の入力を作る D 系テスト。

## VIII. 実 Chrome MHTML 検証

**実在サイトの MHTML：NOT VERIFIED。** 指示書の想定対象（日本語技術記事・英語技術記事・日本語ニュース・
複雑な HTML・JS 多用ページ）の実ページは取得していない（外部サイトへの無断アクセスをしないため）。

**代わりに実施したこと（実ページ検証の代替ではない）**：合成 HTML を `127.0.0.1` で配信し、headless Chrome 158
（使い捨てプロファイル、バックグラウンド通信無効）に DevTools Protocol `Page.captureSnapshot({format:"mhtml"})` で
保存させた（`tools/chrome-capture.mjs`）。これは Chrome 自身の MHTML シリアライザの出力だが、Memocampus が使う
`chrome.pageCapture.saveAsMHTML` そのものではない。両者が同一形式かは未確認。

| fixture（合成ページ） | 入力 | パート（葉 / HTML） | HTML 復号後 | 文字コード（判定元） | 本文 | 結果・品質 |
|---|---|---|---|---|---|---|
| ja-article | 5,102 B | 4 / 1 | 2,155 B | UTF-8（meta） | 251 文字 | 成功。段落区切り・表・リスト・絵文字・エンティティ正常。除外カナリア 12 種すべて非出現。nav「ホーム \| 概要」と footer が混入（仕様どおり） |
| en-article | 2,451 B | 3 / 1 | 1,133 B | UTF-8（meta） | 463 文字 | 成功。NBSP は空白に、QP ソフト改行で語が割れない |
| complex | 3,679 B | 5 / 3 | 975 B | UTF-8（meta） | 67 文字 | 成功。入れ子表・details・br 正常。iframe 2 個（通常・srcdoc）は抽出せず警告 |
| js-heavy | 1,481 B | 1 / 1 | 482 B | UTF-8（meta） | 97 文字 | 成功。JS 生成の見出し・リスト・document.write 段落を抽出。初期文言「読み込み中…」は DOM に残らず非出現。JS で display:none にした要素を除外 |
| sjis-news | 1,584 B | 1 / 1 | 436 B | **Shift_JIS（meta）** | 65 文字 | 成功。①②③・半角カナも正しく変換。MIME に charset が無いことを確認 |
| large | 1,823,917 B | 1 / 1 | 1,055,079 B | UTF-8（meta） | 742,893 文字 | 成功 |

問題点・未確認：実ページ特有の構造（巨大な DOM、多数の iframe、shadow DOM の扱い、lazy-load 画像、
Web フォント、CSS による非表示の多用、文字コード宣言の欠落・誤り、Chrome がプレスキャン範囲外に meta を置くケース）は未検証。
README「実 Chrome 製 MHTML の検証手順」に、Memocampus の保存→書き出し機能を使う PO 向け手順を記載した。

## IX. 性能

条件：release ビルド、各 3 回実行、表は中央値。時間は CLI 内の段階別（μs）と読み込み込みの合計。単発の環境依存値であり性能保証ではない。

| 入力（区分） | 種別 | サイズ | パート | HTML 復号後 | 本文 | MIME | 文字コード | HTML 解析 | 抽出 | 合計（読込含む） | ヒープのピーク | ワーキングセットのピーク |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 小 ja-article | Chrome 製（合成ページ） | 5.1 KB | 5 | 2.2 KB | 435 B | 0.17 ms | 0.03 ms | 0.33 ms | 0.05 ms | 0.85 ms | 34 KB | 5.5 MB |
| 中 large | Chrome 製（合成ページ） | 1.8 MB | 2 | 1.06 MB | 0.98 MB | 4.4 ms | 0.45 ms | 6.8 ms | 5.4 ms | 18.7 ms | 11.0 MB | 15.6 MB |
| 大 HTML 偏重 | **合成** | 19.6 MB | 2 | 16.5 MB | 2.1 MB | 49.6 ms | 4.9 ms | 901 ms | 251 ms | 1,300 ms | **399 MB** | 385 MB |
| 大 リソース偏重 | **合成** | 104.9 MB | 26 | 0.97 MB | 0.90 MB | 357 ms | 0.4 ms | 7.7 ms | 4.8 ms | 420 ms | 114 MB | 118 MB |
| 上限付近 | **合成** | 130.8 MB | 32 | 0.97 MB | 0.90 MB | 467 ms | 0.5 ms | 7.2 ms | 6.3 ms | 548 ms | 140 MB | 144 MB |
| 上限超過 | **合成** | 135.1 MB | — | — | — | — | — | — | — | 即時に `input_too_large` | — | — |

所見：リソース中心の大きな MHTML では、時間の大半は MIME 段（mail-parser の走査と、全 base64 パートの内容検査）で、
メモリは入力サイズ＋α。HTML の量が多い場合は DOM 構築が支配的で、メモリは HTML サイズの約 24 倍に達した。

## X. 既知の制限

- 主文書 1 個のみ。iframe の本文は抽出しない（個数のみ記録）。
- 可視性は静的判定のみ。外部 CSS・class・`<style>` 内のルール・JS による非表示は判定しない。
  逆に `<details>` の閉じた中身など、画面上は隠れていても検索対象になる文字は含む。
- 記事本文の推定なし。ナビゲーション・フッター・サイドバーのノイズを含む。
- 属性テキスト（`alt`、`title`、`aria-label`、`value`）は含めない。
- `<meta>` プレスキャンは HTML 標準の簡略版（先頭 1024 バイト、コメントのみ考慮）。
- base64 / QP の内容検査は厳格で、壊れた画像パートが 1 個あるだけで全体を拒否する（意図的。実データでの妥当性は未検証）。
- Subject（encoded-word）からのタイトルは使わない（`<title>` のみ）。
- 入力全体をメモリに読む。DOM のメモリは上限なし（VI）。
- `pageCapture.saveAsMHTML` の出力形式は未確認（VIII）。
- 抽出は 1 ファイル 1 プロセスの CLI。Native Host 内での実行やタイムアウトは未検討。

## XI. Slice 2 への申し送り

**検索インデックスに渡せるデータ**（`--output` の JSON、スキーマ `memocampus.page-fulltext.poc/1`）：

```text
metadata.title / description / og_title / og_description : string | null
metadata.keywords   : string[] | null（null = タグ自体が無い）
metadata.article_tags : string[]
content.body_text   : string（行区切り \n、行内は単一空白、制御文字なし、≤8 MiB）
content.character_count / utf8_bytes
source.selected_part / html_part_count / snapshot_location
diagnostics.encoding / warnings（コードのみ）
```

**未解決の論点（事実ベース）**：

1. 実ページ（とくに `pageCapture.saveAsMHTML` 製）での抽出品質と失敗率。厳格検証で拒否される実データがどれだけあるか。
2. DOM メモリが HTML サイズの約 24 倍になり得ること。製品に入れるなら、HTML 上限を下げる・ノード数上限・
   別プロセス化・ストリーミング抽出（DOM を作らない tokenizer 方式）のどれで抑えるか。
3. 抽出の単位：Page の原本は `page_blobs`（SHA-256 で重複排除）にあり、保存イベント（`saved_pages`）と 1:N。
   本文は原本単位、URL・タイトルはイベント単位。
4. iframe の本文を検索対象にするか。
5. nav / footer などのノイズを検索で許容するか。
6. 日本語の分かち書き：本文は空白区切りではないので、FTS5 の既定トークナイザでは語が切れない。
   `trigram` トークナイザ（3 文字未満の検索語に弱い）、ICU 等の比較が必要。
7. scraper 経由の MPL-2.0 依存（selectors / cssparser 等）を製品に入れるか。html5ever を直接使えば避けられる。
8. 既存の Page 仕様（Privacy Policy を経由しない手動保存、MCP 非公開、平文 Vault では平文保存）と、
   抽出した本文・索引の扱いの関係（Phase 0 調査の XIII を参照）。

## XII. 実ページ受入試験（PO 提供 MHTML）

- 実施日：2026-11-21。PoC のコードは変更していない。初回報告時と同じ release ビルドの CLI を使った。
- 対象：「実ページ1」の 1 件だけ（PO 提供。日本語 Wikipedia のポータル型ページ）。
- 実ファイルの扱い：
  - リポジトリ外の PO 指定パスから、読み取りだけを行った。
  - Git 管理下へのコピーはしていない。
  - 抽出 JSON・ブラウザ表示テキスト・破損コピーは、すべてセッションの scratchpad（Git 管理外）に置いた。
- レポートの記載範囲：本文・URL・ファイル名・ページ固有の文字列は転載しない。構造・件数・割合だけを記す。

### XII-1. 入力と MIME 構造

| 項目 | 実ページ1 |
|---|---|
| 入力サイズ | 897,090 B |
| 作成経路 | PO 申告：Memocampus の `chrome.pageCapture.saveAsMHTML`。ファイル上の `From: <Saved by Blink>` は CDP 経路と共通なので、ファイルからは区別できない |
| トップレベル | `multipart/related`。パラメータは `type` と `boundary` のみで、**`start` は無い**。閉じ区切りあり |
| パート | 38 個（葉 37）。HTML 1、画像 25（webp 17・svg 6・png 2）、CSS 11（うち 8 個は `cid:` の Content-Location） |
| 先頭パート | `text/html`。quoted-printable。**MIME charset なし**。Content-ID あり。Content-Location は `Snapshot-Content-Location` と一致 |
| 文字コード宣言 | Chrome が挿入した `<meta http-equiv="Content-Type" content="text/html; charset=UTF-8">` が HTML の先頭付近にある。元ページ側の `<meta charset>` は残っていない |
| `<script>` | 0 個（Chrome が除去。ブラウザで開いた DOM でも 0） |

合成ページで観察した Chrome の書式（VIII）と、実ページの書式はすべての項目で一致した。

### XII-2. 抽出結果（CLI 要約行より）

| 項目 | 値 |
|---|---|
| status | success（3 回とも同じ結果、終了コード 0） |
| root 選択 | part 1、規則 `FirstPart` |
| HTML 復号後 | 169,794 B |
| 文字コード | UTF-8（判定元 `MetaPrescan`）、不正バイトの置換なし、U+FFFD 0 個 |
| 本文 | 4,780 文字 / 11,881 B（UTF-8） |
| メタデータ | title あり、og:title あり（両者は同一）。description・keywords・og:description なし。article:tag 0 |
| 除外 / 非表示サブツリー | 1 / 13 |
| 警告 | なし |

### XII-3. 性能・メモリ（3 回の中央値）

| MIME | 文字コード | HTML 解析 | 抽出 | 合計（読込含む） | ヒープのピーク | ワーキングセットのピーク |
|---|---|---|---|---|---|---|
| 1.34 ms | 0.07 ms | 2.66 ms | 0.29 ms | 4.84 ms | 2.45 MB | 7.9 MB |

- ヒープのピークには、入力全体（0.90 MB）と復号後 HTML（0.17 MB）が含まれる。
- それを差し引いた DOM・本文相当は約 1.4 MB で、HTML サイズの約 8 倍。
- 合成の HTML 偏重ケース（約 24 倍）より小さい。ただし、実ページ 1 件で DOM 膨張の上限は評価できない。

### XII-4. ブラウザ表示との一致度

**方法：**
- 同じ MHTML を headless Chrome 158 で開き、`document.body.innerText` を「ブラウザ表示本文」とした。
- Chrome は使い捨てプロファイルで起動し、`--host-resolver-rules=MAP * ~NOTFOUND` と到達不能プロキシで外部通信を遮断した。
- サブリソースの要求 40 件はすべて MHTML 内から解決され、失敗は 0 件。外部への名前解決は遮断されていた。
- 比較には文字 4-gram の多重集合を使った。

| 指標 | 空白を含めて比較 | 空白を除いて比較 |
|---|---|---|
| 表示本文の 4-gram のうち、抽出本文にあるもの（再現率） | 94.7 % | **99.1 %** |
| 抽出本文の 4-gram のうち、表示本文にあるもの（適合率） | 69.3 % | **74.8 %** |
| 文字数（抽出 / 表示） | 4,780 / 3,522 | 4,439 / 3,351 |

**文字化け：** なし。U+FFFD は 0 個で、日本語・多言語の文字（キリル・ヘブライ・デーヴァナーガリー・タイ文字等）を正しく復号した。

**本文欠落：**
- 表示本文 112 行のうち、空白を除くと抽出本文に見つからないのは 2 行（計 12 文字）だけだった。
- 2 行とも記事本文ではなく UI のボタン・ラベル。
  - 1 行は `aria-hidden="true"` の要素内のラベル。PoC は `aria-hidden` を非表示として除外するが、ブラウザでは見えている。
  - もう 1 行も同種のボタン文言。
- **記事・ポータル本文の欠落は見つからなかった。**
- 空白を含めた再現率が下がる主な原因は区切り方の違い。横並びのリンク列を、ブラウザは区切りなしで連結し、PoC は空白や改行で区切る。検索上はむしろ有利と考えられる。

**不要な文章の混入（抽出のみにある 84 行・894 文字。抽出本文の約 19 %）：** いずれも、ブラウザ表示テキストに現れない UI 要素や付随要素だった。主な原因は、サイトの外部 CSS による非表示や折りたたみ。

| 種類 | 内容 |
|---|---|
| サイドバー | 折りたたまれたメインメニューとツールメニューの項目（約 30 行） |
| 言語切替 | 折りたたまれた他言語版の一覧（約 40 行） |
| 表示設定 | 表示設定パネルの説明文（3 行） |
| 印刷用・フッター要素 | サイト副題、`printfooter` の「取得元」行、フッターの最終更新行（3 行）。いずれも表示テキストには現れなかった。最終更新行が表示されなかった理由は特定していない |
| 画像キャプション | `<figure>` 内のキャプション 1 行（表示テキストには現れなかった） |
| ボタン | メニューの固定・非表示を切り替えるボタンの文言（3 行） |

これは既知の制限（X：外部 CSS・class による非表示は判定しない、ノイズを含む）が実データで現れたもの。
記事中心のページより、ナビゲーションの多いポータル型ページで比率が高くなると考えられる。

### XII-5. 追加確認事項への回答

1. **MIME root 選択：** 正しい。`start` が無いため先頭パートを選んだ。そのパートは唯一の HTML で、Content-Location はスナップショットの URL と一致した（警告なし）。
2. **MIME charset なしでの復号：** 実ページで成功した。MIME charset は無く、Chrome が挿入した meta から UTF-8 を判定した。追加で、実ファイルの scratchpad コピーを改変して確認した（いずれも元ファイルは無変更）。
   - meta を削除：既定の UTF-8 に落ち、警告 `encoding_not_declared_utf8_assumed` が出て、本文は完全に同一。
   - meta を Shift_JIS に書き換え：**status は success のまま文字化けした本文を返した**（U+FFFD 613 個、表示本文との 4-gram 一致 6 %）。警告 `malformed_bytes_replaced_with_u_fffd` と `malformed_replaced=true` は出る。
   - したがって、誤った宣言は拒否ではなく警告付きの成功になる。製品で索引に入れる際の扱い（警告時は索引しない等）は、Slice 2 で決める必要がある。
   - 実際の Chrome 出力では、meta は Chrome 自身が正しい値で挿入するため、この事態は起きにくいと考えられる（実ページ 1 件での観察）。
3. **HTML パートが複数ある場合：** 実ページ1は HTML パートが 1 個で、iframe を含まない。**実データでは未検証。** 合成 fixture `complex`（iframe 2 個）の結果（主文書のみ抽出し、警告を出す）だけが根拠。
4. **一部リソースに問題がある場合の拒否：** 実ファイルの scratchpad コピーを 1 箇所ずつ壊して確認した。
   - webp 画像パートの base64 に不正文字を 1 個入れた → `invalid_base64`（終了コード 1）
   - CSS パートに不正な QP エスケープを入れた → `invalid_quoted_printable`
   - 閉じ区切りを削除した → `unterminated_multipart`
   - いずれも HTML 本文は無傷だが、**ページ全体が拒否される**ことを実データで確認した（X の制限どおり）。
   - 実ページ1そのものは全パートが妥当で、拒否されなかった。実保存データでの拒否率は、件数が増えるまで評価できない。
5. **DOM のメモリ膨張：** 実ページ1では約 8 倍（XII-3）で、問題ない値。大きな実ページは未入手のため、約 24 倍（合成）の評価は変わらない。
6. **ブラウザ表示との一致度：** XII-4 のとおり。表示本文はほぼすべて含む（空白を除いて 99.1 %）。一方で、CSS で隠れた UI 文言が約 19 % 混入する（空白を除いた適合率 74.8 %）。

### XII-6. この試験で残る制限・未検証事項

- 実ページは 1 件だけ（日本語、ポータル型、UTF-8）。英語記事・日本語ニュース・Shift_JIS などのレガシー文字コード・iframe 多用・巨大 DOM の実ページは未検証。
- `pageCapture.saveAsMHTML` 製であることは PO 申告で、ファイルからは検証できない。
- CSS・class による非表示を判定しないため、ナビゲーション文言が混入する（実ページで約 19 %）。
- `aria-hidden="true"` を非表示とみなす規則は、ブラウザ表示と食い違う場合がある（実ページで 12 文字の欠落）。
- 誤った charset 宣言は、拒否ではなく警告付きの成功（文字化け本文）になる。
- 1 個のリソースパートの破損で、ページ全体が拒否される。
- 比較の基準は headless Chrome の `innerText`（ウィンドウ幅 1366 px）で、利用者が実際に見た画面と完全には一致しない可能性がある。

PoC のコード・テスト・README は変更していない。追記したのは本レポートの I（判定表）と XII のみ。

## XIII. Tokenizer方式への改修・DOM方式との比較

- 実施日：2026-11-21。
- 範囲：`poc/page-fulltext/` 配下のみ。製品コード・Vault・Privacy Policy・MCP は無変更。commit / push / PR なし。
- **I〜XII は改修前（DOM 方式）の記録として残し、書き換えていない。** I〜XII の「現行実装」の記述は DOM 方式を指す。
- 判定：**この章でも PASS は確定しない。** PO と独立レビューの判定を待つ。
- 計測について：**計測値は、メモリ・CPU 使用量が製品レベルで安全であることの証明ではない。**

### XIII-1. 改修理由

独立レビューの指摘：

- DOM 構築のメモリが大きい。合成の 16.5 MB HTML で、ヒープのピークが約 399 MB だった（IX）。
- DOM 構築の CPU 時間とメモリに、厳密な上限がない。
- CSS の可視性を再現していないので、DOM を作っても表示テキストとは一致しない（XII-4）。
- `aria-hidden` による除外で、画面に出ている文字列が欠ける（XII-4）。

方針：全文検索の目的は「検索語がページ本文にあるか」の判定。親子構造やレンダリングの再現は要らないので、
DOM を作らず、HTML5 tokenizer で逐次的に文字列を取り出す方式に変えた（PO 指示）。

### XIII-2. 採用 API・依存関係

**採用 API：** `html5ever` 0.40.1 の `html5ever::tokenizer` のみ（MIT OR Apache-2.0）。tree builder（`html5ever::tree_builder`）は使わない。

| 使用する型・関数 | 用途 |
|---|---|
| `Tokenizer::new(sink, TokenizerOpts)` | tokenizer の生成 |
| `Tokenizer::feed(&BufferQueue)`、`Tokenizer::end()` | 逐次供給と終了処理 |
| `BufferQueue::push_back(StrTendril)` | チャンクの投入 |
| `TokenSink::process_token` | トークンの受け取り。戻り値 `TokenSinkResult::RawData(RawKind)` / `Plaintext` で内容モデルを切り替える |
| `TokenSink::adjusted_current_node_present_but_not_in_html_namespace` | CDATA を SVG / MathML の中だけで認識させる |

**ソースで確認した事実：**

- tokenizer 単体は、script / style の中でも data 状態のままになる。内容モデルの切り替えは tree builder の役目である。
- 「適切な終了タグ」の判定に使う直前の開始タグ名は、tokenizer 自身が記録している（`last_start_tag_name`）。
- `end()` は、利用者の `BufferQueue` に先読みのため残った文字を処理しない（新しい空のキューで EOF 処理をする）。

**依存関係（`cargo tree -e normal` で確認）：**

- `scraper`・`ego-tree` を削除し、`html5ever = "0.40"` を直接の依存にした。
- MPL-2.0 の 4 crate（selectors / cssparser / cssparser-macros / dtoa-short）は依存から消えた。
- 実行時の推移的依存は 55 crate から 36 crate に減った。ライセンスはすべて MIT / Apache-2.0 / Unlicense のいずれか、または選択式で、encoding_rs は BSD-3-Clause、unicode-ident は Unicode-3.0 を含む。MPL-2.0 は 0 個。
- `Cargo.lock` は 64 パッケージから 45 パッケージに減った。

### XIII-3. 新しい抽出アルゴリズム

```text
MHTML ──(変更なし) mail-parser + 厳格検証──▶ ルート HTML（転送復号済み、≤16 MiB、1 個保持）
  ──BOM / MIME charset / <meta> プレスキャン──▶ 文字コード決定（変換はまだしない）
  ──┬─▶ 変換元 64 KiB を encoding_rs Decoder で UTF-8 に（累計 ≤32 MiB）
    ├─▶ StrTendril にして BufferQueue へ、Tokenizer::feed
    ├─▶ TokenSink：開始タグ／終了タグ → 状態更新・区切り、文字 → 本文 or title、meta → メタデータ
    └─ 次の 64 KiB（復号済み文書の全体は保持しない）
  ──▶ Tokenizer::end ──▶ 本文（≤8 MiB）＋メタデータ
```

- 本文の区切りと正規化は、文字を追加するときに同時に行う。中間文字列は作らない。
- 本文バッファには、整形済みの文字だけを入れる。

### XIII-4. MIME 処理との接続

- `mime::select_root` は変更していない（厳格検証、ルート選択、`decoded_len` による事前のサイズ判定）。
- 変えたのは返り値の扱いだけで、ヘッダ由来の文字列に上限と制御文字の検査を加えた（MEDIUM-4、XIII-6）。
- 文字コードの決定（`charset::detect`）と変換（`charset::decode_streaming`）を分けた。
  - 変換は tokenizer への供給と交互に行う。
  - 宣言と実データの不一致は修復しない（PO 判断）。
  - `malformed_sequences_replaced` と各警告は従来どおり出す。

### XIII-5. 状態管理の仕様

| 状態 | 保持するもの | 遷移 |
|---|---|---|
| 内容モデル | `raw`：Title / Text（textarea・xmp・plaintext）/ Dropped（script 等） | HTML 文脈の開始タグで `RawData` / `Plaintext` を返して設定し、次の終了タグ（tokenizer が保証する対応タグ）で解除する。`<script/>` も HTML では生テキスト扱い |
| 開いている要素 | 名前・名前空間（HTML / SVG / MathML）・除外フラグ・pre フラグのスタック（最大 1024）と、名前ごとの位置索引 | void 要素は積まない。終了タグは、スコープ内に同名要素があればそこまで閉じ、無ければ無視する |
| 除外 | `excluded_open`：除外フラグ付き要素の数 | template / canvas / svg / frameset、`hidden`（`until-found` を除く）、インライン `display:none` |
| head | `in_head` | `<head>` で入る。`</head>`、`<body>`、head に置けない開始タグ、空白以外の文字で出る |
| foreign | スタック最上位の名前空間 | SVG / MathML 内では内容モデルを切り替えない。統合点（foreignObject・desc・title・mi など）の中は HTML 規則。脱出タグ（`<p>` `<div>` `<b>` 等）で foreign 要素を閉じる |
| title | 最初の HTML 文脈の title だけを収集し、空白を畳みながら上限を検査 | EOF で閉じていなくても確定する |

**暗黙の終了（tree builder の近似）：**

- `<p>` を閉じる開始タグ（ボタンスコープ）。
- `<li>`（リストスコープ）、`<dt>` / `<dd>`。
- `<option>` / `<optgroup>`。
- `<tr>` / `<td>` / `<th>` / `<tbody>` 等（表スコープ）。
- 対応する開始タグのない `</p>` は区切り、`</br>` は `<br>` として扱う。

**実装していないもの：**

- adoption agency（`<b><i></b></i>` の再構成）。
- table の foster parenting（表の中のセル外の文字を、表の前へ移す処理）。
- select の内部規則。
- frameset の詳細。
- これらは除外範囲と区切りにだけ影響し、文字が消える方向には働かない。ただし、表のセル外の文字の順序は、ブラウザと異なる場合がある。

**状態破綻の復旧方針：**

- 不明な終了タグは無視する。
- スタックが上限（1024）に達したら、それ以上の要素は追跡せず警告 `open_element_limit_reached` を出す。除外範囲が早く終わる方向にしか崩れないので、文字は増えることはあっても減らない。
- スコープ探索は位置索引による定数回の参照で行い、スタックをたどる繰り返しはない。
- tokenizer 側は入力を消費して必ず前進する。変換ループには、出力領域不足で前進しない場合の防護を入れた。
- ループの上限は入力サイズで決まる。無制限に伸びるバッファは XIII-6 の「保証できないもの」に挙げた 1 種類だけ。

**除外する・しないの変更点：**

- `aria-hidden="true"` とインラインの `visibility:hidden` は除外しない（取りこぼし回避）。
- `hidden="until-found"` は除外しない（ブラウザのページ内検索で表示される）。
- MathML の文字は残す。

### XIII-6. 追加した資源制限と、メモリ・CPU の内訳

| 対象 | 上限 | 備考 |
|---|---|---|
| MHTML 入力 | 128 MiB（変更なし） | 全体をメモリに保持する |
| 転送復号後 HTML | 16 MiB（変更なし） | 1 個保持する |
| **UTF-8 変換後 HTML** | **32 MiB（新規）** | 変換しながら累計で判定。`decoded_html_too_large` |
| UTF-8 変換バッファ | 64 KiB × 3 + 16 B | 1 個を再利用。tokenizer へは同じ大きさの StrTendril を 1 個ずつ渡す |
| tokenizer の内部バッファ | **上限なし（HTML サイズで間接的に制限）** | 1 個のコメント・属性値・タグ・doctype をまとめて保持する。巨大なコメントは丸ごとバッファになる（下の計測） |
| 開いている要素 | 1024（新規） | 名前とフラグのみ |
| 本文出力 | 8 MiB（変更なし） | `String` の伸長で容量は最大で約 2 倍になりうる |
| メタデータ | 1 項目 64 KiB（変更なし） | 整形しながら判定するので、空白だらけの巨大 title でもバッファは伸びない |
| `Content-Location` / `Snapshot-Content-Location` | **8 KiB（新規）** | 超過・制御文字は値を捨てて警告 |
| MIME charset | **64 バイト（新規）** | 超過・制御文字は無視して警告し、`<meta>` で判定 |
| JSON 出力 | 上限なし（本文に比例） | `--output` 指定時だけ。本文（≤8 MiB）のエスケープ込みの複製と抽出結果を同時に保持する |
| CPU 時間 | **上限なし** | タイムアウト機構はない。計測では、16 MB 級の病的入力で 1 秒未満だった |

### XIII-7. テスト結果

- `cargo test --release`：**53 件成功・0 失敗・0 スキップ。**
  - 既存の `tests/integration.rs`：31 件。
  - 新規の `tests/tokenizer.rs`：22 件。
- clippy（`--all-targets`）の警告は 0、rustfmt の差分なし。

**既存テストのうち、仕様変更で期待値を変えたもの（2 件）：**

- `b_excluded_and_hidden_content`
  - `aria-hidden="true"` とインライン `visibility:hidden` の文字を、除外から**含める**に変えた。
  - `hidden_subtrees` は「4 以上」から「ちょうど 2」に変えた（`hidden` 属性とインライン `display:none` の 2 個）。
- `chrome_ja_article`
  - `ARIA-HIDDEN-CANARY` と `VISIBILITY-HIDDEN-CANARY` は、本文に**含まれる**ことを確認する形に変えた。
  - 他の除外カナリアは、従来どおり含まれないことを確認している。
- 変更理由はテスト内にコメントで残した。他の 29 件は、期待値を変えずにそのまま成功した。

**新規テストと、指示書の追加テスト項目の対応：**

| 指示書の項目 | テスト |
|---|---|
| 日本語・英語・混在 | `t_japanese_english_and_mixed` |
| script / style 内の疑似タグ | `t_script_and_style_content_never_becomes_text_or_tags`（`a<b`、`document.write("<p>…")`、`"</scr"+"ipt>"`、エスケープされた `<!-- <script> -->`、`<script/>`）、`t_rcdata_and_rawtext_that_are_visible` |
| HTML エンティティ | `t_entities_in_text_and_attributes`（名前付き・数値・セミコロンなし・属性値の中） |
| 不正な入れ子 | `t_misnested_and_implicitly_closed_markup`（p と div の交差、b と i の交差、閉じない td / tr、迷子の終了タグ、暗黙に閉じる hidden 要素） |
| 深いネスト | `t_deep_nesting_and_open_element_limit`（20 万段、上限超過時は文字を残す） |
| 大量タグ | `t_many_tags`（30 万タグ） |
| title / meta | `t_rcdata_…`、`t_foreign_content_rules`（svg 内の title）、`m_*` |
| SVG・template 等の除外 | `t_excluded_elements`、`t_comments_head_and_doctype_are_not_text` |
| aria-hidden 付きの可視テキスト | `t_aria_hidden_and_until_found_text_is_kept` |
| ブロック境界 / インライン連結 | `t_block_boundaries_and_inline_joining`（`東京` / `都内` は 2 行、`東` + `京都` は `東京都`） |
| チャンク分割入力 | `t_chunked_input_gives_identical_output`（1・2・3・5・7・64・4096 バイト単位で、Chrome fixture 4 件と合成 1 件の本文・メタデータが同一） |
| 対象外の巨大 meta を無視（MEDIUM-1） | `m_huge_non_target_meta_is_ignored`（200 KiB の viewport / og:image / キーなし） |
| name / property 併記（MEDIUM-5） | `m_name_and_property_on_one_tag` |
| metadata 上限 | `m_metadata_limits`、既存の `d_metadata_limit` |
| ヘッダ由来の巨大文字列・制御文字（MEDIUM-4） | `m_header_values_are_bounded_and_checked` |
| 正常 Shift_JIS / UTF-8 / 宣言誤り | `c_shift_jis_utf8_and_wrong_declaration`、既存の `c_charsets_declared_in_several_ways` |
| 不正 charset | `c_invalid_charset_label_warns`、既存の `c_unknown_or_unusable_charset_labels` |
| 復号後 UTF-8 サイズ超過 | `c_decoded_size_limit`（実値 32 MiB と小さい上限） |
| 置換文字警告の維持 | 既存の `d_control_characters_and_invalid_utf8`、`c_shift_jis_…` |
| 入力 128 MiB / HTML 16 MiB / 本文 8 MiB 超 | 既存の `d_input_size_limit_…`、`d_decoded_html_limit_real_value`、`d_text_limit_real_value` |
| panic しない | 既存の `d_mutated_inputs_never_panic`（変異入力 2,000 件） |
| 既定で非出力 / 既存ファイル保護 | 既存の `cli_default_output_has_no_page_text`、`cli_output_is_written_only_…` |
| DOM を作らない | `n_extractor_is_the_tokenizer`。scraper は依存から削除済みで、tree builder を使うコードはない |

### XIII-8. DOM 方式との比較

**条件：**
- 同じ PC・同じ日・同じ release ビルド条件で計測した。
- DOM 方式は、改修前のバイナリを scratchpad に退避したもの（SHA-256 `02A2BB94…E46326`）。
- 各入力を 3 回実行した中央値。DOM 方式は 1 回 120 秒で打ち切った。
- 「HTML 段」の中身は方式で異なる。DOM 方式は文字コード変換・DOM 構築・抽出の合計、tokenizer 方式は変換・tokenize・抽出の合計。
- 時間は単発の環境依存値。

| 入力 | 種別 | 方式 | 本文文字数 | HTML 段 | 合計（読込含む） | ヒープのピーク | ワーキングセットのピーク |
|---|---|---|---|---|---|---|---|
| 小 ja-article | Chrome 製（合成ページ） | DOM | 251 | 0.34 ms | 0.71 ms | 0.03 MB | 5.1 MB |
| | | tokenizer | 295 ※1 | 0.27 ms | 0.68 ms | 0.20 MB | 5.2 MB |
| 中 large（1.8 MB） | Chrome 製（合成ページ） | DOM | 742,893 | 10.6 ms | 15.9 ms | 10.5 MB | 14.8 MB |
| | | tokenizer | 742,893 | 13.8 ms | 18.9 ms | **4.7 MB** | 9.4 MB |
| 実ページ1（0.9 MB） | 実 MHTML | DOM | 4,780 | 5.6 ms | 8.6 ms | 2.34 MB | 7.7 MB |
| | | tokenizer | 4,840 ※1 | 5.0 ms | 7.7 ms | **1.41 MB** | 6.8 MB |
| HTML 偏重（HTML 16.5 MB） | 合成 | DOM | 1,544,158 | 1,081 ms | 1,217 ms | 380.6 MB | 367 MB |
| | | tokenizer | 1,544,158 | 1,132 ms | 1,190 ms | **41.7 MB** | 43.8 MB |
| リソース偏重 105 MB | 合成 | DOM | 700,889 | 7.5 ms | 267 ms | 108.5 MB | 112 MB |
| | | tokenizer | 700,889 | 8.6 ms | 268 ms | 102.9 MB | 108 MB |
| 上限付近 131 MB | 合成 | DOM | 700,889 | 7.5 ms | 331 ms | 133.1 MB | 138 MB |
| | | tokenizer | 700,889 | 8.7 ms | 331 ms | 127.5 MB | 132 MB |
| 上限超過 135 MB | 合成 | 両方 | — | — | 即時に `input_too_large`（約 6 ms） | — | — |
| 深い入れ子 140 万段（16.0 MB） | 合成・新規 | DOM | — | **120 秒で打ち切り**（打ち切らない予備実行でも CPU 600 秒超で未完了） | — | — | — |
| | | tokenizer | 4 | 773 ms | 808 ms | 30.8 MB | 35.3 MB |
| 不一致の終了タグ 350 万個（14.6 MB） | 合成・新規 | DOM | 2 | **28,458 ms** | 28,478 ms | 54.6 MB | 59.3 MB |
| | | tokenizer | 2 | 707 ms | 738 ms | 28.1 MB | 32.6 MB |
| 15 MiB のコメント 1 個 | 合成・新規 | DOM | 3 | 327 ms | 363 ms | 77.2 MB | 82.6 MB |
| | | tokenizer | 3 | 364 ms | 399 ms | 47.5 MB | 51.9 MB |
| 15 MiB の属性値 1 個 | 合成・新規 | DOM | 3 | 20 ms | 54 ms | 77.2 MB | 80.6 MB |
| | | tokenizer | 3 | 23 ms | 54 ms | 47.5 MB | 51.9 MB |

※1 `aria-hidden` / `visibility:hidden` の文字を含めるようにした分（XIII-5）。

**所見（悪化した点も含む）：**

- **メモリ：** HTML の多い入力で大きく減った（HTML 16.5 MB で 381 MB → 42 MB、約 9 分の 1）。
  - tokenizer 方式のピークは、おおむね「入力全体＋転送復号後 HTML＋本文＋α」で、HTML サイズの約 24 倍に膨らむことはなくなった。
  - 巨大なコメント 1 個のケースでは、「入力 16 MB＋HTML 15 MB＋コメントのバッファ 15 MB」で約 47 MB になった。1 個のトークンの大きさには上限がない。
  - リソース中心の入力は、入力そのものが支配的なので、ほとんど変わらない。
  - 小さい入力（5 KB）では、変換バッファ（約 192 KiB）の分だけ増えた（0.03 MB → 0.20 MB）。
- **CPU：** **通常の入力では改善していない。**
  - HTML 偏重 16.5 MB で 1.08 秒 → 1.13 秒（約 5 % 悪化）。1.8 MB の中規模で 10.6 ms → 13.8 ms（約 30 % 悪化）。
  - 主因は html5ever tokenizer 自体の処理時間と考えられる（DOM 方式でも、構文解析の前段は同じ tokenizer）。ただし、**プロファイルは取っていないので未確認**。
  - 実ページ1では、わずかに速かった（5.6 ms → 5.0 ms）。
- **病的入力：** DOM 方式は木の構築で時間が爆発した。
  - 深い入れ子：120 秒超（予備実行では CPU 600 秒超で未完了）。
  - 不一致の終了タグ：28 秒。
  - tokenizer 方式は、どちらも 1 秒未満だった。改修途中で見つけた「不一致の終了タグごとにスタックをたどる」実装は、位置索引に直してから計測した。
- **入力上限：** 両方式とも、読み込み前に `input_too_large` で拒否した（差なし）。

**抽出品質の一致率（空白を除いた文字 4-gram の多重集合で比較）：**

| 入力 | 本文（tokenizer / DOM） | DOM の本文のうち tokenizer にあるもの | tokenizer の本文のうち DOM にあるもの | 行単位の差 | メタデータ・文字コード・警告 |
|---|---|---|---|---|---|
| en-article / complex / js-heavy / sjis-news / large | 同一 | 100 % | 100 % | なし（完全一致） | 一致 |
| ja-article | 295 / 251 文字 | 98.6 % ※2 | 82.3 % | tokenizer のみ 2 行（aria-hidden・visibility のカナリア）、DOM のみ 0 行 | 一致 |
| 実ページ1 | 4,840 / 4,780 文字 | 99.7 % ※2 | 98.7 % | tokenizer のみ 9 行（aria-hidden の UI ラベル）、DOM のみ 0 行 | 一致 |

※2 100 % にならないのは、追加された行が、DOM 方式では隣り合っていた文字列の間に入り、その境界をまたぐ 4-gram が変わるため。行単位では、DOM 方式の行はすべて tokenizer 方式にもある（欠落 0）。

**重点検証項目の結果：**

| 項目 | 結果 |
|---|---|
| 本文の欠落 | fixture 6 件と実ページ1で、DOM 方式に比べた欠落は 0 行 |
| HTML タグの混入 | テストで確認。`<` を含むのは、textarea / xmp / エンティティ由来の文字だけ |
| JavaScript の混入 | script 内の疑似タグ・`document.write`・エスケープを含むテストで 0 |
| 隣接しない文章の誤連結 | ブロック境界のテストと、fixture の改行位置が DOM 方式と同一であることで確認 |

### XIII-9. 実 MHTML 検証結果（実ページ1、tokenizer 方式）

対象と方法は XII と同じで、ブラウザ表示テキストも XII-4 で取得したものを使った。本文・URL は転載しない。

| 指標 | DOM 方式（XII） | tokenizer 方式 |
|---|---|---|
| 状態 | success、警告なし | success、警告なし |
| 本文 | 4,780 文字 | 4,840 文字 |
| 表示本文の 4-gram のうち抽出本文にあるもの（空白を除く） | 99.13 % | 99.10 % |
| 抽出本文の 4-gram のうち表示本文にあるもの（空白を除く） | 74.8 % | 74.0 % |
| 空白を除いて表示本文の行が見つからないもの | 2 行・12 文字 | **1 行・5 文字**。表示本文での 1 行が、抽出側では別の UI 文言を挟んで 2 行に分かれたもので、文字自体は両方とも抽出本文にある |
| 文字化け（U+FFFD） | 0 | 0 |
| トークン | — | 開始タグ 1,480、終了タグ 1,317、文字トークン 2,283、コメント 20、構文エラー 0、無視した終了タグ 0、暗黙の終了 0、最大の深さ 18 |
| 破損コピー（XII-5 の 4 種） | `invalid_base64` / `unterminated_multipart` / `invalid_quoted_printable` / charset 誤りは文字化けした本文で警告付き成功 | 同じ結果 |

- `aria-hidden` による欠落は解消した（XII-4 で欠けていた UI ラベルを回収した）。
- 代わりに、画面に出ない UI 文言が 60 文字増え、適合率が 0.8 ポイント下がった。
- 再現率の 0.03 ポイント低下は、上の ※2 と同じ理由による。
- 実ページは引き続き 1 件だけ。

### XIII-10. 残る問題

1. **CPU 時間は短くならなかった。** 通常入力では DOM 方式と同等か、やや遅い（XIII-8）。原因は未確認（プロファイル未実施）。
2. **1 個のトークンの大きさに上限がない。** tokenizer はコメント・属性値・タグを丸ごと保持するので、上限は HTML サイズ（≤16 MiB、UTF-8 で ≤32 MiB）による間接的なものだけ。html5ever の公開 API では制限できない。
3. 入力全体（≤128 MiB）と転送復号後 HTML（≤16 MiB）は、従来どおりメモリに保持する。逐次化したのは、文字コード変換から先だけ。
4. tree builder の近似には限界がある（XIII-5 の「実装していないもの」）。除外範囲の終わり方と表の中の文字の順序が、ブラウザと異なる場合がある。文字が消える方向の崩れは、テストした範囲では見つかっていない。
5. CSS による非表示は判定しない。ナビゲーションなどのノイズが残り（実ページで適合率 74.0 %）、`aria-hidden` を含めた分だけ増えた。
6. `Tokenizer::end()` は、利用者キューに先読みのため残った文字を処理しない。文書末尾が `<!-` のような不完全なマークアップで終わる場合、その数文字が落ちうる。本文の途中には影響しない（チャンク分割テストで確認）。
7. ルビ（`<rt>`）の読みが、親文字の間に入る（`漢かん字`）。DOM 方式と同じ挙動で、`漢字` で検索できない可能性がある。Slice 2 の論点。
8. iframe 本文の未抽出、1 個のリソースの破損でページ全体を拒否すること、charset 宣言の誤りが警告付き成功になることは、XII から変わらない。
9. `noscript` は、スクリプト有効時のブラウザと同じく除外している（Chrome の保存時の DOM と同じ前提）。
10. 実ページでの検証は 1 件だけ。

### XIII-11. 製品統合前に必要な対策

1. **プロセス隔離と OS レベルの上限：**
   - 抽出は、Native Host 本体とは別のワーカープロセスで行う。
   - Windows の Job Object でメモリ上限（例：入力上限＋100 MiB 程度）と CPU 時間・壁時計時間の上限をかける。
   - 超過時はプロセスごと終了し、「抽出失敗（索引なし）」として記録する。
   - PoC の資源制限は論理上限であり、OS による強制ではない。
2. **1 トークンの大きさへの対策：** 次のいずれかを、計測で選ぶ。
   - 上記 1 のメモリ上限で吸収する。
   - HTML の上限を下げる。
   - コメントや属性値を保持しない字句解析器に替える（html5ever へのパッチ、または別実装）。
3. **CPU の調査：**
   - プロファイルを取る（tokenizer 本体とシンク処理の内訳）。
   - 索引付けは、バックグラウンドの低優先度で行う。
   - ページ当たりのタイムアウトを決める。
4. **ファジング：** `cargo-fuzz` 等で、MIME 層と tokenizer シンクを継続的に検査する。PoC は決定的な変異 2,000 件だけ。
5. **入力の逐次化の検討：** 保存原本（`page_blobs`）を全体読み込みせずに処理できるか。mail-parser は全体のバイト列を必要とする。
6. **索引に入れる条件：** `malformed_sequences_replaced`、`decoded_html_too_large` 等の警告・エラーのときに索引へ入れるかどうか（Slice 2）。
7. **依存のライセンス：** 再確認の結果、MPL-2.0 は 0 個。製品の依存方針に照らした最終確認は、統合時に行う。
8. **実ページでの検証：** 英語記事・日本語ニュース・Shift_JIS・iframe・大きな DOM を、PO の手順で増やす。

### XIII-12. 独立レビュー指摘（HIGH-1 / HIGH-2 / MEDIUM-1）の修正と再検証

独立レビューで FAIL とされた 3 件を修正した。同じ種類の状態管理の誤りを探して見つかったものも、あわせて直した。終了タグの扱いについては、この節が XIII-5 の「開いている要素」行と「暗黙の終了」の記述に優先する（XIII-1〜11 の本文は書き換えていない）。

**指摘ごとの原因と修正：**

| 指摘 | 原因 | 修正 |
|---|---|---|
| HIGH-1：SVG / MathML の統合点で、終了タグが効かない | 旧実装は、SVG / MathML 要素を閉じる処理を「スタックの最上位が SVG / MathML 要素のとき」に限っていた。`<svg><foreignObject><p>cap</foreignObject>` では最上位が HTML の `p` なので、この処理に入らなかった。HTML 側の処理では、SVG / MathML 要素がすべてスコープ境界だったので無視された。その結果 `svg`（除外）が閉じず、後続の本文がすべて除外された。`<div hidden><math><mtext><p>` では、`</mtext>` と `</math>` が無視され、さらに `mtext` が境界になって `</div>` も無視された | 開いている SVG / MathML 要素と同名の終了タグは、その要素より上に残っている HTML 要素ごと閉じる。ただし、その要素より上に `template` が開いている場合は閉じない（template の中から外の要素を閉じない）。同名の HTML 要素の方が近い場合は、HTML 側の規則を使う |
| HIGH-2：`</template>` が、中で閉じていない表に妨げられる | `</template>` にも通常のスコープ判定を使っていた。スコープ境界の `table` / `td` 等が template より上に開いていると、無視された | `</template>` はスコープを見ず、最も近い template まで閉じる（HTML 標準の挙動）。template 以外の終了タグにとって、template は従来どおりスコープ境界（中から外の要素を閉じない） |
| MEDIUM-1：`Snapshot-Content-Location` の非 UTF-8 バイト | UTF-8 でない値は、警告なしに `None` になっていた | 値を捨て、警告 `snapshot_location_invalid_utf8_dropped` を記録する。長さ・制御文字の検査とルート選択は変更していない |

**意図的にブラウザと異なる点（HIGH-1）：**

- ブラウザ（HTML 標準）は、`<svg><foreignObject><p>x</foreignObject></svg>` の 2 つの終了タグを無視する。`p` が special 要素なので、そこで探索が止まるため。
- このため、ブラウザでは後続の本文も SVG の中に入る。foreignObject に幅・高さがなければ、画面に表示されないこともある。
- 本 PoC では、SVG を丸ごと除外する方針なので、ブラウザと同じにすると後続本文が丸ごと失われる。そこで、明示された終了タグを優先して閉じる（「迷う場合は取りこぼさない側」）。
- 代償として、統合点の中で閉じていない `hidden` 要素がこのとき一緒に閉じる。例：`<svg><foreignObject><div hidden>x</foreignObject></svg>y`。
  - `y` は、ブラウザでは非表示だが、本 PoC では抽出される。
  - 除外が解けるのは `</svg>` まで閉じたときだけで、`</foreignObject>` だけでは SVG の除外が続く。

**同じ種類の誤りとして探し、あわせて直したもの：**

| 箇所 | 旧挙動（後続本文が除外されたままになる例） | 修正 |
|---|---|---|
| HTML の終了タグとスコープ境界 | すべての SVG / MathML 要素を境界にしていた。`<div hidden><svg><g></div>本文` で `</div>` が無視された | 境界は統合点だけにした（HTML 標準どおり）。`g` 等は、閉じる HTML 要素と一緒に閉じる |
| 表の終了タグ | 統合点も境界にしていた。`<td><svg><foreignObject><p>…</table>本文` で `</table>` が無視された | 表スコープは html / table / template だけにした（HTML 標準どおり） |
| SVG / MathML の中の `</p>` / `</br>` | SVG を閉じなかった。`<svg><g></p>本文` が除外された | 最も近い HTML 要素か統合点まで、SVG / MathML 要素を閉じる（HTML 標準どおり） |
| 脱出タグ（`<p>` 等）による SVG の閉じ方 | 最も近い HTML 要素まで閉じていた。統合点を越えて外側の `svg` まで閉じ、その中身が本文に出る（除外の破れ）ことがあった | 最も近い HTML 要素か統合点で止める（HTML 標準どおり） |
| 見出しの終了タグ | 同名の見出ししか閉じなかった。`<h1 hidden>…</h2>本文` が除外された | `</h1>`〜`</h6>` は、開いているどの見出しでも閉じる（HTML 標準どおり） |
| `</dd>` / `</dt>` | `dl` も境界にしていた。`<dd hidden><dl><dt>…</dd>本文` が除外された | 通常のスコープにした（`dl` は開始タグでの暗黙の終了でだけ境界にする） |
| 非 UTF-8 の `Content-Location` | mail-parser が U+FFFD に置き換えた値を、そのまま出力していた | 生のヘッダバイトを検査し、値を捨てて `content_location_invalid_utf8_dropped` を出す |
| 非 UTF-8 の MIME `charset` | U+FFFD 入りの値を診断情報に出し、未知ラベルとして扱っていた | 値を無視して `mime_charset_invalid_utf8_ignored` を出す。判定は `<meta>` に任せる |

統合点の位置は、別のスタック（最大 1024 個）で持つ。どの終了タグの処理も、名前ごとの位置索引を定数回引くだけで済み、スタックをたどる処理は増えていない。

**新規の回帰テスト（`tests/end_tags.rs`、12 件）：**

| テスト | 内容 |
|---|---|
| `h1_review_counterexamples` | レビューの反例 3 件。`IMPORTANT_BODY` だけが抽出され、`cap` や `x` は出ないこと |
| `h1_every_integration_point_and_deeper_html` | 統合点 9 種それぞれで、深い HTML（`div` / `ul` / `li` / `b`、表）が開いたままでも閉じること。大文字の終了タグ、入れ子の SVG |
| `h1_exclusions_are_not_ended_early` | SVG を閉じても外側の `hidden` は続くこと。統合点の中の `</div>` は外の要素を閉じないこと。template 内の `</foreignObject>` は外の SVG を閉じないこと。閉じない SVG は以降を隠すこと |
| `h1_same_kind_html_end_tags_close_through_svg` | 上の表で直した同種の誤り（`g` の通過、`</p>` / `</br>`、表スコープ、見出し、`</dd>`） |
| `h1_normal_svg_and_math_unchanged` | 正常な SVG の除外、MathML の文字を残すこと、脱出タグ、脱出が統合点で止まること |
| `h2_review_counterexample` | レビューの反例。`IMPORTANT_BODY` が出て、`cell` は出ないこと |
| `h2_template_end_tag_cases` | template 内で閉じていない caption / tbody・th / li・p / SVG / MathML / object・applet・marquee / select・option / hidden を `</template>` で閉じること。入れ子の template、template 内の外側要素の終了タグを無視すること、不正な終了タグ、迷子の `</template>` |
| `e_stack_limit_with_integration_points` | 上限 1024 を超えた位置の SVG は追跡せず、文字を残すこと。上限ちょうどの SVG は閉じること。最大の深さが 1024 以下であること |
| `e_many_stray_end_tags_against_a_full_stack` | 深さ 1,000 の上で、今回扱いを変えた種類の不一致終了タグ 220 万個（`</h1>` `</foreignobject>` `</desc>` `</dd>` `</p>` `</br>` `</mtext>` `</svg>` `</td>` 等）。release で 0.87 秒（12 件全体） |
| `m1_non_utf8_snapshot_location_is_dropped_with_a_warning` | 値を捨てて専用警告を出すこと。ルートと本文、`Content-Location` は変わらないこと |
| `m1_same_check_for_content_location_and_mime_charset` | 同じ検査を `Content-Location` と MIME `charset` にも適用すること。charset を無視したときは `<meta>` で判定し、ルートも同じであること |
| `m1_valid_utf8_and_other_checks_unchanged` | 正常な非 ASCII UTF-8 の location は残ること。非 UTF-8 と制御文字の両方を含む場合は、UTF-8 の警告 1 つにまとまること |

- 除外対象のカナリア（`cap` / `cell` / `SECRET`）が本文に出ないことを、各ケースで共通に検査している。
- 修正前のコードに対しては、新規テストを実行していない（修正前のバイナリを残していなかった）。
  - 反例 3 件については、レビューで再現済み。
  - 上の表の旧挙動は、旧実装のコードを追って導いたもの。

**テスト・ビルド結果：**

- `cargo test --release`：**65 件成功・0 失敗・0 スキップ。**
  - `integration.rs` 31 件と `tokenizer.rs` 22 件は、期待値を変更せずに成功した。
  - `end_tags.rs` 12 件は新規。
- `cargo build --release` は成功。clippy（`--all-targets`）の警告は 0、`cargo fmt --check` の差分なし。

**出力の回帰確認（修正前の tokenizer 方式の JSON と比較）：**

- Chrome fixture 6 件（`large` を含む）と実ページ1で、本文・メタデータ・文字コード・警告がすべて完全に一致した。
- 実ページ1は 4,840 文字で、変わらなかった。

**性能（tokenizer 方式のみ。3 回の中央値）：**

| 入力 | 修正前（XIII-8） | 修正後 |
|---|---|---|
| 中 large（1.8 MB） | HTML 段 13.8 ms / ヒープ 4.7 MB | 13.1〜14.5 ms / 4.7 MB |
| 実ページ1 | 5.0 ms / 1.41 MB | 5.0 ms / 1.41 MB |
| HTML 偏重 16.5 MB | 1,132 ms / 41.7 MB | 1,092〜1,334 ms（4 回の測定）/ 41.7 MB |
| 不一致の終了タグ 350 万個 | 707 ms / 28.1 MB | 563 ms / 28.1 MB |
| 深い入れ子 140 万段 | 773 ms / 30.8 MB | 471 ms / 30.8 MB |
| 15 MiB のコメント 1 個 | 364 ms / 47.5 MB | 192 ms / 47.5 MB |

- ヒープのピークは、すべて同じ値だった。
- 時間は、測定ごとのばらつきが ±10 % 以上ある。
  - HTML 偏重の入力で一時 1,250 ms 前後が続いたため、終了タグごとの文字列の確保をやめた（tokenizer がタグ名を小文字化済みのため不要）。
  - 直後に同じ条件で DOM 方式と並べると、DOM 方式 1,112 ms に対し tokenizer 方式 1,092 ms だった。
  - 修正による悪化があるかどうかは、このばらつきの中では判別できない。病的入力が速くなった分も、同じ理由で改善とは断定しない。
- XIII-6 の資源上限（スタック 1024 を含む）は変更していない。

**残る制限：**

- XIII-10 から変わらない。
- 追加：統合点の中で閉じていない `hidden` / `display:none` 要素は、SVG / MathML 要素の終了タグで閉じる。この場合、ブラウザでは非表示の文字が本文に入る（上の「意図的にブラウザと異なる点」）。
- tree builder の近似であることに変わりはない。今回の修正は、終了タグについて、HTML 標準の規則と本 PoC の方針（取りこぼさない側）を照らし合わせて見直したもの。adoption agency・foster parenting・select の規則は、引き続き実装していない。

**判定：**

- 指摘 3 件の修正と再検証まで行った。判定は自己確定せず、PO と独立レビューの再判定を待つ。
- commit・push・PR は行っていない。
