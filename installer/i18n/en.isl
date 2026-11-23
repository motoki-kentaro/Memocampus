; Memocampus installer messages: English (Issue #75). Read after compiler:Default.isl.
; Every key here must also be in ja.isl, with the same %1..%9 arguments (installer\verify-static.ps1).
; Identifiers stay as they are in every language: ROLLBACK_FAILED, host names, paths, registry names, ids.
; The details passed in as an argument (Err, Report) are diagnostic lines that are also written to the
; setup log; they stay in English on purpose (KEPT / FAILED / ROLLBACK_FAILED are read by the code).
; The tray's menu is Japanese only; an English text names its item as it is shown, with an English gloss:
; the item 終了, quoted and followed by its gloss in parentheses. That is the only Japanese allowed here
; (installer\verify-static.ps1).
; UTF-8 without a BOM. No "#" lines: the compiler does not preprocess .isl files.

[Messages]
; What "finished" means here: the Windows side only. The Chrome extension, choosing a vault and starting
; to remember are separate steps the user takes in Chrome; Setup never reports them as done.
FinishedLabel=Setup has installed the [name] programs and its connection to Chrome on this computer.%n%nThat is all Setup does. The Chrome extension is added by you in Chrome, and choosing a vault and starting to remember happen in the extension, only when you approve them there.
FinishedLabelNoIcons=Setup has installed the [name] programs and its connection to Chrome on this computer.%n%nThat is all Setup does. The Chrome extension is added by you in Chrome, and choosing a vault and starting to remember happen in the extension, only when you approve them there.

[CustomMessages]
; The last page ([Run] descriptions).
McLaunchTray=Start Memocampus now (its first start also turns on starting at logon; you can change that in the tray menu)
McOpenStorePage=Open the Memocampus page in the Chrome Web Store (you add the extension yourself, in Chrome)

; Before anything is installed (PrepareToInstall) and before an uninstall (InitializeUninstall).
McInvalidExtensionId=This installer was built with an invalid Chrome extension id.
McTrayStillRunning=Memocampus is still running and did not quit in time. Nothing was changed.%n%nQuit Memocampus from its tray icon: right-click the icon and select "終了" (Quit). Wait a moment, and run this again.
; %1 the file names, one per line.
McFilesInUse=These Memocampus files are still in use by another program:%n%n%1%nSetup does not close other programs. Close AI clients that use Memocampus (for example an MCP client) and Chrome windows that use the Memocampus extension, then run this again. Nothing was changed.

; Chrome Native Messaging registration (nmh.pas). %1 the path the existing registration names.
McNmhForeignRegistration=Chrome already has a Memocampus host registration for this user that points somewhere else:%n%1%n%nSetup will not replace it. Remove that registration (or uninstall the other Memocampus it belongs to), then run Setup again.
; %1 the registry key.
McNmhUnknownKey=Chrome already has a registry key for the Memocampus host (%1) that Setup did not create and cannot identify.%n%nSetup will not change it. Remove that key, then run Setup again.
; %1 the host name, %2 the error (it ends with ROLLBACK_FAILED: ... when the restore failed too).
McRegisterFailed=Memocampus could not register its connection to Chrome (native messaging host %1). Setup stops and nothing was installed unless this message says ROLLBACK_FAILED.%n%n%2
; %1 what could not be restored.
McRollbackIncomplete=Setup did not finish and could not fully restore the Chrome registration: %1 Running Setup again repairs it.

; Uninstall. %1 the report, one line per entry.
McUninstallStopped=Memocampus could not remove one of its registrations, so the uninstall stopped before removing any file. Run the uninstall again.%n%n%1
McUninstallKept=Memocampus was uninstalled. Some entries were kept because they are not Memocampus's own:%n%n%1
