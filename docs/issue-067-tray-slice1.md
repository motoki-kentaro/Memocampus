# Issue #67 Slice 1：Windows Tray 基盤・管理IPC・サイレント起動

ブランチ：`feature/67-tray-foundation`（未コミット）。受入判定は PO が行う。

## 1. 範囲

実装したもの（実際の Tray アイコン・メニュー、ログオン自動起動登録、設定ダイアログ、インストーラーは **含まない**。Slice 2 以降）：

| 領域 | 内容 |
|---|---|
| `tray/`（新規 crate `memocampus-tray`） | Supervisor、状態機械、設定ファイル、単一起動、Windows 起動処理（サイレント・Job Object・管理パイプ）、検証用の最小ホスト exe |
| `mcp-server/` | 管理IPC（匿名パイプ）の受信と、既存 graceful shutdown への接続。**この変更だけが製品コードの変更** |
| 検証 | Tray 単体 36、Tray 実プロセス 10、MCP 実プロセス（管理IPC・Supervisor 連携）14 を追加 |

変更していないもの：VisibleStore、Native Host、Chrome 拡張、Vault スキーマ、Privacy Gate、MCP のツール・プロトコル・認証、`config.json`／`policy.json`、stdio の動作。

## 2. spike：IPC 方式の選定

### 2.1 検証して確認できたこと（実機・実プロセス）

1. **GUI プロセスから、制御用ハンドルだけを子へ継承できる**。`CreateProcessW` に `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` を渡し、子が継承するハンドルを「管理パイプ読取端／stderr パイプ書込端／NUL」の3つに限定した。Tray 側の端（管理パイプ書込端、stderr 読取端）は継承不可にしてある。
2. **コンソールを一切持たない**。`DETACHED_PROCESS` で起動した子は、スタブでも実 MCP でも `console_window=false`、`console_processes=0`（実 MCP は自身の診断ログ `startup_context` に記録）。Tray 自身は release ビルドの PE サブシステムが `2`（WINDOWS_GUI）。MCP は `3`（CUI）なので、フラグなしで GUI から起動するとコンソールが開く構成であり、`DETACHED_PROCESS` が効いていることになる。
3. **Job Object への所属を生成時に原子的に行える**。`PROC_THREAD_ATTRIBUTE_JOB_LIST` で子を匿名 Job（`KILL_ON_JOB_CLOSE`）へ生成と同時に入れる。「生成後・所属前」の窓が存在しないので、サスペンド起動は不要。Tray を `TerminateProcess` で落としても子が残らない（§6）。
4. **管理パイプの EOF を親子双方で検知できる**。子は Tray 死亡で `management_closed` として停止する。親は stderr パイプの EOF と `WaitForSingleObject` で子の終了を検知する。
5. **停止要求は既存の graceful shutdown に接続できる**。`http::serve` の停止待ちに管理パイプの受信を加え、Ctrl+C／Ctrl+Break／コンソール終了と同じ経路（待受停止 → rmcp キャンセル → 5 秒の接続待ち → 終了コード 0）を通る。ログの `http_shutdown.reason` で区別できる。
6. **stdio と衝突しない**。管理パイプは stdin/stdout とは別のハンドル番号で渡す。子の stdin/stdout は `NUL`、stderr は Tray が読むパイプ。

### 2.2 方式の比較と選定理由

| 方式 | 結果 | 理由 |
|---|---|---|
| **匿名パイプ（採用）** | 成立 | 書込端を持つのは Tray のみ。他プロセスは名前で到達できない。子との対応は「そのハンドルを継承した子」に一意。EOF が親の死を表す |
| 名前付きイベント | 不採用 | 名前を知る同一ユーザーの任意プロセスが `SetEvent` できる。名前の衝突・推測の問題があり、「第三者による停止要求を防ぐ」に反する |
| 名前付きパイプ | 不採用 | ACL と接続認可の実装が増える。クライアント検証が必要で、匿名パイプで足りる |
| stdin 経由 | 不採用 | stdio 経路と混同しうる。HTTP モードでも stdin を管理路に使うと既存の意味が変わる |
| Ctrl+Break を送る | 不採用 | 送信側が子のコンソールに接続している必要がある。GUI の Tray にはコンソールがなく、`AttachConsole` を介する方法は脆く、`DETACHED_PROCESS` と両立しない |

「Tray から起動されたか」を環境変数の有無で判断しない。子は **コマンドラインで明示された `--management-pipe-handle <n>`** を、使う前に検証する（§3.2）。

## 3. MCP Server 側の変更（`mcp-server/src/mgmt.rs`、`main.rs`、`http.rs`）

### 3.1 振る舞い

- 起動引数 `--management-pipe-handle <n>` がある場合に限り、そのハンドルを監視する。通常起動では何も変わらない（停止は従来どおり Ctrl+C／Ctrl+Break／コンソール終了）。
- **HTTP 専用**。stdio（`--transport stdio` または未指定）では引数エラー（終了コード 2、`--management-pipe-handle needs --transport streamable-http`）。無視はしない。
- プロトコルは1バイト：`S` = 停止要求。
  - `S` を受信 → 停止（`management_stop`）。
  - 書込端の全閉鎖（EOF、`ERROR_BROKEN_PIPE`）→ 停止（`management_closed`）。Tray が死んだ場合に管理者のいない常駐サーバを残さないため。
  - `S` 以外の1バイト、または読取エラー → 停止（`management_invalid`）。正規の書き手は Tray だけなので、不正な内容は Fail-Closed で止める。
- 停止の実体は既存の `http::serve` の graceful shutdown を再利用する（重複実装なし）。`stop_requested` の `select!` に管理チャネルの待ちを加えただけで、チャネルがなければ永久に解決しない future なので従来の `select!` と同じ。
- MCP の公開ツールに停止ツールはなく、HTTP の停止エンドポイントもない。`tools/list` は従来の5ツールのまま、`/stop`・`/shutdown` などは 404、`memocampus_stop` などの呼出しはエラー（テストで確認）。

### 3.2 起動時の検証と、漏洩防止

- 値は10進の正準形のみ（`0` 始まり・符号・16進・空は拒否）。
- 次のどれかなら起動を拒否（終了コード 2、`cannot start: management_not_a_pipe`、待受は作られない）：標準ハンドル（stdin/stdout/stderr）と同じ値、パイプではない、読取アクセスがない（`PeekNamedPipe` で確認。書込端はここで失敗する）。
- ハンドル値は、stderr・診断ログ・エラーメッセージに出さない（`argc` のみ記録される既存仕様を維持、エラー文はこの値を含まない）。
- ハンドルの権限モデル：読取端だけが子に渡る。書込端は Tray だけが保持し、継承不可。他プロセスが停止要求を書くには、書込端を複製する権限（`PROCESS_DUP_HANDLE` 等）が要り、それは同一ユーザーが当該プロセスを強制終了できる権限と同等で、新たな攻撃面ではない。

## 4. Job Object と起動（`tray/src/winproc.rs`）

- `CreateProcessW` 1回で、`DETACHED_PROCESS`（コンソールなし）、ハンドルリスト（3つのみ継承）、ジョブリスト（匿名 Job）を指定する。
- 子の stdin/stdout は `NUL`、stderr は Tray が読むパイプ（行ごとに `Diag` へ分類し、原文は保持しない）。
- Job は名前なし・子1つのみ。`KILL_ON_JOB_CLOSE` により Tray のハンドルが閉じる（正常終了・異常終了・強制終了すべて）と OS が子を終了させる。
- 通常停止は管理パイプで要求し、`stop_timeout`（8 秒）以内に終了しなければ、**Tray が保持するその子のプロセスハンドルだけ** を `TerminateProcess` する。プロセス名・PID による検索や `taskkill` は一切使わない。
- 生成の同時実行を `Mutex` で直列化（継承可能ハンドルが別スレッドの子へ漏れる窓を排除）。

## 5. Supervisor（`tray/src/supervisor.rs`、`state.rs`、`classify.rs`）

状態：`Stopped`／`Starting`／`Running`／`Stopping`／`Error(原因)`。UI は `Command`（Start／Stop／Restart／Quit）を送り、`Snapshot` を読む。Supervisor は専用スレッドで動き、UI をブロックしない。

- `Running` へ遷移するのは、子の stderr に `memocampus-mcp: listening on http://127.0.0.1:<port>/mcp` が **出てから** だけ。10 秒で出なければ `Error(StartTimeout)` にして子を止める。
- **Vault 状態は別軸**（`VaultStatus`）。`Running` でも `Unknown` のまま。確実に分かるのは起動時の拒否（`vault refused: <code>`）だけで、その場合 `Error(VaultRefused(code))` と `Refused(code)`。Tray は Vault を読まず、公開可否を推測しない。
- 起動拒否（終了コード 2：Vault 未承認／未設定／非 Active、ポート使用中、引数・token 不備）は **再試行しない**。ポート使用中は別ポートへ切り替えず、保持者に触れない。
- 予期しない終了（終了コード 2 以外、または Running 後の終了）は異常終了。`RestartPolicy`：5 分以内に 3 回まで、待機 1 秒→2 秒→4 秒。超えたら `Error(CrashLoop)` で停止。ユーザーの Start／Restart でカウンタはリセット。
- 子の終了コードが不明（`WaitForSingleObject` 失敗）なら、稼働とは見なさず終了扱い。
- ログ・状態に載せるのは固定コードのみ。stderr の原文・パス・URL は保持しない。

## 6. 設定（`tray/src/settings.rs`）と単一起動（`tray/src/instance.rs`）

`%LOCALAPPDATA%\Memocampus\tray-settings.json`（`MEMOCAMPUS_APP_DIR` に従う）。

```json
{ "version": 1, "autostart": true, "server_autostart": true, "listen_port": 8765,
  "allow_hosts": [], "vault_mode": "follow_active", "pinned_vault_path": null }
```

- **ファイルが存在しないときだけ** 既定値を使う。読めない・不正・未知フィールド・未知バージョン・過大（64 KiB 超）はエラーで、推測値でサーバを起動しない。
- 保存は原子的（一時ファイル → `sync_all` → `MoveFileExW`）。未知バージョンや不正なファイルがあるときは **上書きを拒否** する。
- `config.json`／`policy.json` には触れない（テストで確認）。Vault を作らない。
- `pinned` は `--vault` を渡すだけ。MCP 側の既存仕様により、固定した Vault は「それが Active かつ公開承認済みである間」しか読めない。実サーバで、Active ではないファイルを固定すると `vault_not_active` で拒否されることを確認した。Tray に Active Vault の選択権限は移していない。
- `--allow-host` の形式検証は MCP に一本化。Tray は件数（8 以下）、重複、コマンドライン注入になる文字だけを確認する。
- 既定ポートは MCP 既定と同じ `8765`。Cloudflare Tunnel が使う `8080` は `listen_port` で変更する。
- 単一起動：名前付き Mutex `Local\Memocampus.Tray.<app dir のハッシュ>`。同じ app dir は2つ目が終了コード 3 で終了し、最初のものに干渉しない。別の app dir は同時に起動できる。`Local\` なので Windows セッションごと（同一ユーザーの別セッションの Tray は別物で、ポート競合は `port_in_use` として現れる）。

## 7. 検証ホスト（`tray/src/main.rs`）

`memocampus-tray.exe` は GUI サブシステムの最小ホスト。アイコンもメニューもなく、ログファイルにのみ出力する。

```text
memocampus-tray.exe [--run-seconds <n>] [--log <file>] [--no-autostart]
                    [--mcp-exe <path>] [--mcp-arg <arg>]...
```

`--mcp-exe`／`--mcp-arg` はテスト用で、通常起動では MCP 実行ファイルは Tray の隣の `memocampus-mcp.exe`。終了コード：0 完了、2 引数不正、3 同じ app dir の Tray が起動中、4 app dir を決められない。

**互換性の注意**：管理IPC を持たない従来の `memocampus-mcp.exe`（現在動いているサービスなど）に対し、この Tray が起動すると、引数エラー（終了コード 2）で `Error(StartRefused)` になる。新しい `memocampus-mcp.exe` をビルドして Tray の隣に置く必要がある。

## 8. テスト結果

| 区分 | 件数 | 内容 |
|---|---|---|
| Tray 単体 | 36 | 設定（欠落／不正／未知バージョン／原子保存／既存ファイル不変）、stderr 分類、コマンドライン引用、再起動ポリシー、状態遷移（スクリプト化した子で Running 条件・拒否・ポート・クラッシュ・タイムアウト・設定エラー・Restart・Drop）、単一起動名 |
| Tray 実プロセス | 10 | 実プロセスでの起動／管理パイプ停止、コンソールなし、Restart、クラッシュ／拒否の検知、停止無視の強制終了と **無関係プロセス不変**、起動しないサーバ、MCP なし、Tray 強制終了での孤児防止、**Job 単独で終了する検証**（EOF・停止要求に従わない子）、graceful 終了と単一起動、不正設定 |
| MCP 実プロセス（追加） | 14 | 停止要求／EOF／不正バイト、不正ハンドルの拒否、引数エラー、停止ツール・エンドポイント無し、フラグ無しの従来動作、Supervisor が実サーバを起動・停止、未承認／未設定 Vault、固定 Vault の Active 制約、ポート使用中、他者のサーバを停止しない、Tray プロセスを強制終了しても実サーバが残らない |
| MCP 既存（回帰） | unit 30（うち今回の追加4）、http 17、stdio 26 | すべて合格。`tools/list`、Privacy Gate、認証、stdio は無変更 |

```powershell
cd tray; cargo test --locked          # 単体＋実プロセス（examples/stub_mcp を同時にビルドする）
cd mcp-server; cargo test --locked    # 既存＋管理IPC（tray を dev-dependency として使う）
```

CI（`.github/workflows/ci.yml`）に `tray/` の fmt／test／clippy／release build を追加した。

## 9. Windows 実機での確認

### 9.1 実施済み（この開発機、機械確認）

隔離 app dir とスタブサーバで、release の `memocampus-tray.exe` を `Start-Process` で起動し、起動 → 稼働 → 停止の間に：Tray のメインウィンドウなし、子のメインウィンドウなし、子の子プロセスなし、新しい `conhost`／`OpenConsole` の生成なし、子は `console_window=false`・`console_processes=0`。

### 9.2 **未実施：目視確認（PO にお願いする項目）**

このスライスの完了条件「コンソールが表示されない」は、自動テストだけでは PASS にしない。次を目視で確認する（隔離 app dir を使う）。

1. `cargo build --release`（`mcp-server/` と `tray/`）。`memocampus-mcp.exe` を `memocampus-tray.exe` と同じフォルダへ置く。
2. 新しい PowerShell で `$env:MEMOCAMPUS_APP_DIR` を一時フォルダにし、`memocampus-tray.exe --run-seconds 60 --log <file>` を `Start-Process` する。
3. **起動時**：コンソール／黒いウィンドウが一瞬でも出ないこと。
4. **再起動時・異常終了時・停止時**：Tray のログ（`state=`）で遷移を確認しながら、同様に出ないこと（スタブでクラッシュさせるには `--mcp-arg --stub --mcp-arg crash`。スタブは `tray/target/debug/examples/stub_mcp.exe`）。
5. タスクマネージャーで、Tray を「タスクの終了」した直後に `memocampus-mcp.exe`（または `stub_mcp.exe`）が残っていないこと。

注意：ユーザー環境で動いている既存の `memocampus-mcp.exe` は止めない（本作業は実ユーザー Vault・既存サービスに触れていない）。

## 10. 残リスクと未検証

- **目視確認が未実施**（§9.2）。
- 実 MCP を使った **Tray 強制終了の検証は、EOF による graceful 停止と Job の強制終了のどちらで終了したか区別していない**（結果は「残らない」）。Job 単独の効果は、EOF にも停止要求にも従わないスタブで別に確認した。
- 実 Vault での運用（ユーザー Vault、暗号化 Vault）、実 Chrome／Native Host との連動は、本作業では行っていない（合成データのみ）。
- 起動しない場合の再試行方針（Vault を後から承認したときの自動再試行など）は未実装。現状は Error のままで、ユーザーの Start／Restart が必要（Slice 2 の UI と合わせて検討）。
- Windows のセッション間（同一ユーザーの複数セッション）の Tray 同時起動は、設計上は許容（`Local\`）だが実機検証していない。
- 管理IPC は Windows 専用。他 OS は未対応（スコープ外）。
- `tray-icon`（Slice 2）は未導入のため、`tray-icon` の実機挙動は未確認のまま。
