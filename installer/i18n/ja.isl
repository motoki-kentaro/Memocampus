; Memocampus installer messages: Japanese (Issue #75). Read after compiler:Languages\Japanese.isl.
; The same keys and the same %1..%9 arguments as en.isl (installer\verify-static.ps1 checks it).
; Identifiers are not translated: ROLLBACK_FAILED, host names, paths, registry names, ids.
; The details passed in as an argument (Err, Report) stay in English (they are also the setup log lines).
; The tray's own menu is Japanese ("終了", "Windowsログオン時に起動"); the texts below name it as it is.
; UTF-8 without a BOM. No "#" lines: the compiler does not preprocess .isl files.

[Messages]
; 「完了」はWindows側の作業だけを指す。拡張機能の追加、Vaultの選択、記録の開始は利用者がChromeで行う別の手順。
FinishedLabel=[name] のプログラムと、Chrome との接続をこのコンピューターにインストールしました。%n%nセットアップが行うのはここまでです。Chrome 拡張機能は、Chrome Web Store からご自身で Chrome に追加してください。Vault の選択と記録の開始は、拡張機能の画面であなたが承認したときにだけ行われます。
FinishedLabelNoIcons=[name] のプログラムと、Chrome との接続をこのコンピューターにインストールしました。%n%nセットアップが行うのはここまでです。Chrome 拡張機能は、Chrome Web Store からご自身で Chrome に追加してください。Vault の選択と記録の開始は、拡張機能の画面であなたが承認したときにだけ行われます。

[CustomMessages]
McLaunchTray=Memocampus を今すぐ起動する（初回の起動で Windows ログオン時の起動も有効になります。トレイのメニュー「Windowsログオン時に起動」で変更できます）
McOpenStorePage=Chrome Web Store で Memocampus を開く（拡張機能はご自身で Chrome に追加してください）

McInvalidExtensionId=このインストーラーは、無効な Chrome 拡張機能 ID でビルドされています。
McTrayStillRunning=Memocampus が実行中のまま、時間内に終了しませんでした。何も変更していません。%n%n通知領域の Memocampus アイコンを右クリックして「終了」を選び、しばらく待ってから、もう一度実行してください。
McFilesInUse=次の Memocampus のファイルを、ほかのプログラムが使用しています:%n%n%1%nセットアップはほかのプログラムを終了しません。Memocampus を使う AI クライアント（MCP クライアントなど）と、Memocampus 拡張機能を使っている Chrome のウィンドウを閉じてから、もう一度実行してください。何も変更していません。

McNmhForeignRegistration=このユーザーの Chrome には、別の場所を指す Memocampus ホストの登録がすでにあります:%n%1%n%nセットアップはこの登録を置き換えません。この登録を削除するか、登録元の別の Memocampus をアンインストールしてから、もう一度セットアップを実行してください。
McNmhUnknownKey=Chrome に、セットアップが作成したものではない、識別できない Memocampus ホストのレジストリ キーがあります（%1）。%n%nセットアップはこのキーを変更しません。このキーを削除してから、もう一度セットアップを実行してください。
McRegisterFailed=Memocampus と Chrome の接続（ネイティブ メッセージング ホスト %1）を登録できませんでした。セットアップを中止します。このメッセージに ROLLBACK_FAILED が含まれていなければ、何もインストールされていません。%n%n詳細:%n%2
McRollbackIncomplete=セットアップが完了せず、Chrome への登録を元の状態に完全には戻せませんでした。もう一度セットアップを実行すると修復されます。%n%n詳細:%n%1

McUninstallStopped=Memocampus の登録の一部を削除できなかったため、ファイルを削除する前にアンインストールを中止しました。もう一度アンインストールを実行してください。%n%n詳細:%n%1
McUninstallKept=Memocampus をアンインストールしました。Memocampus のものではない項目は残してあります:%n%n%1
