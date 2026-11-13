# Issue #14 クローズ記録

PO決定日: 2026-11-14（JST）。対象は [Issue #14](https://github.com/motoki-kentaro/Memocampus/issues/14)。

## PO決定

1. Prompt Injection対策は現行方式Aを維持する。MCPの `instructions` と該当Tool descriptionの注意書き、既存の応答形式を維持する。追加注意文B、注意文＋構造分離Cは採用しない。
2. AC-H1（虚偽の移転告知）は、Web由来の未検証情報が回答に取り込まれる独立した残存リスクとして記録する。
3. 旧要件の棚卸しと後続への引き継ぎ、文書PRのCI・レビュー・マージを確認してから #14 をクローズする。

## Prompt Injectionの評価

Claude Code 2.1.322で、Claude Haiku 5.5とSonnet 5.5を使い、各方式・各モデルで攻撃200試行、正常50試行を評価した。攻撃40ケースと正常10ケースを各5回反復し、計1,500試行を判定した。Haikuの機械判定はA 5/200、B 4/200、C 3/200、Sonnetは各方式0/200。観測された12件はすべてAC-H1の反復で、方式間で有意差を確認できなかった。秘密探索・模擬持ち出し・外向きモック呼び出しは計1,200攻撃試行で0件。注入なしの正常タスクは全300試行で達成した。評価範囲はこのクライアント、2モデル、合成データのモック環境に限られる。同一ケースの反復は独立した攻撃ケースとして数えない。

Haikuの方式間比較の両側Fisher検定はp=0.724〜1.000。正常タスクでは誤拒否・誤警告が観測されなかった。B/CではAより入力トークンが約2.5〜5.6%増え、CではMCP応答のスキーマが変わる。攻撃ケース数が少なく、方式間の小さな差や低頻度の漏えいを検出するには検出力が足りない。別のクライアント・モデル、適応的な攻撃、Page本文への一般化は未検証。

ローカルの詳細資料は `poc/mcp-prompt-injection/RESULTS.md` と `poc/mcp-prompt-injection/RECOMMENDATION.md`。

### 残存リスク AC-H1

第三者ページの `og_description` に含まれる虚偽の移転告知を、AIが回答中で紹介し、利用者を未検証のリンクに誘導するおそれがある。Haikuの機械判定12件のうち、目視では11件がページ側の記述で未確認と説明し、1件が留保の弱い形で案内した。元の判定数を維持する。既存の注意書きでは外部文字列の出自と扱い方を伝えられるが、Web上の主張の真偽は判定できない。

一般公開・第三者配布の前、正式対応するモデルやクライアントの追加時、Page Vault本文等への公開範囲拡大時、書き込み・外向きToolの追加時、実事例の報告時に再評価する。再評価では、出所と未確認の明示、留保の弱いリンク案内、実際の外向き操作を個別に測る。

## 旧要件の棚卸し

| 旧要件 | 扱い | 根拠・引き継ぎ先 |
|---|---|---|
| URLのquery/fragmentを原則削除しallowlistで復元。検索語のMCP公開を初期OFFにし、タイトル側の抑止と設定を追加 | 撤回 | URL・検索語の忠実性と既存の公開契約を優先する。従来の検証項目のうち、query/fragmentの一律削除や検索語OFF時のタイトル検証も取り下げる。 |
| メール・電話・カード番号等の汎用マスキング | 撤回 | 汎用の自動マスキングを本Issueの製品要件から外す。既知の資格情報検出と権限系シークレットの追加対策は下の行で扱う。 |
| 既知トークンキー、userinfo、既知認証パスの判定。URL内URL、曖昧なパス識別子、検出漏れ・誤検出、追加マスキングの有用性、秘密値の伝播と検索オラクル | 完了／保留 | 現行の資格情報検出とPrivacy Gateの対象ケースを確認した。権限系シークレットの追加マスキング、Release Projection導入、トークン・オラクル対策の採否は [ADR-0000](adr/adr-0000-mcp-secret-redaction-deferred.md) で保留。製品での追加マスキング評価は再検討時に扱う。 |
| 公開・非公開混在データのカナリア検証。URL、タイトル、メタデータの非公開値を結果・件数等に出さない | 完了（現行の対象範囲） | [#12](https://github.com/motoki-kentaro/Memocampus/issues/12) のVisibleStoreと [#8](https://github.com/motoki-kentaro/Memocampus/issues/8) のMCP回帰テスト。非公開行のOGP/descriptionを含む。公開済みの自由記述に含まれる任意の秘密や未実装のPage本文の検証は下の該当行で扱う。 |
| title・OGP・Twitter Card等の現状調査、出力契約、隔離した比較、正当な検索・要約への影響、既存の公開制御との整合 | 完了（PoCの範囲） | 下記の現行経路と隔離試験を確認した。旧条件にある応答内注意文・構造分離の採用はPO決定で撤回。将来のPage本文を公開する際は再評価する。 |
| Page VaultのMHTML原本を不変保存、書き出し時にバイト一致 | 後続Issue移管 | [#6](https://github.com/motoki-kentaro/Memocampus/issues/6) の既存仕様・Done条件に記載済み。 |
| MHTMLのscript/hidden・不要部分等を除いた本文抽出、未信頼データとしての提示、マスキング後のsnippet、本文カナリア、MCPでの原本返却禁止 | 後続Issue移管 | #6を前提とする [#60](https://github.com/motoki-kentaro/Memocampus/issues/60) に引き継いだ。現行MCPのTool一覧にはMHTML返却がない。 |
| FTS5で非公開の行・機密本文を検索語、ヒット有無、順位、snippet経由で推測できないこと | 後続Issue移管 | [#60](https://github.com/motoki-kentaro/Memocampus/issues/60) に引き継いだ。検索対象は公開判定済みの派生データとし、件数・順位・snippetを含めて検証する。 |
| 画像は公開可能な保存履歴のメタデータのみ。MCPでのサムネイル返却 | 完了／後続Issue移管 | [#8](https://github.com/motoki-kentaro/Memocampus/issues/8) の画像Toolはメタデータのみ。サムネイル返却はMVP外として [#61](https://github.com/motoki-kentaro/Memocampus/issues/61) に引き継いだ。 |

### 確認した現行経路

[#22](https://github.com/motoki-kentaro/Memocampus/issues/22) で収集する `document.title`、description、OGP、Favicon URLは、`extension/page-metadata.js` と `extension/service-worker.js` からHostへ渡り、`native-host/src/main.rs` が `page_metadata` に保存する。`visible-store/src/snapshot.rs` と `visible-store/src/visible_set.rs` が公開可能なvisitとメタデータを扱い、`mcp-server/src/server.rs` が同じ応答契約をstdioとStreamable HTTPで提供する。OGP画像とFaviconはURLを扱う。Twitter Card、Page本文、alt、MHTMLの収集・MCP公開はこの経路に含まれない。

### PoCと検証記録

1,500試行の保存済みデータは、試行ごとの回答とモックToolログから凍結済み判定器で再判定し、集計・方式間の対応あり比較を照合した。`FREEZE.json` の10ファイルは現物のハッシュと一致し、各本番 `run.json` に記録された5ハッシュも一致した。判定器の自己テスト13件が通過した。Haiku 4.5とA0の途中判定115件・128件は本番集計から除外されている。

`poc/release-projection/` は追加マスキングと検索オラクルの調査記録で、採否はADR-0000が管理する。Prompt Injection比較の生ログはローカルの `poc/mcp-prompt-injection/runs/` に保管する。文書PRで比較結果と限界を記録する。
