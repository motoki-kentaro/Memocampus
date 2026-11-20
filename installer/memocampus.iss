; Memocampus installer, Issue #67: the three executables, a Start menu entry (Slice 3B-1) and the
; per-user Native Messaging host registration for Chrome (Slice 3B-2).
;
; Build (the version and the Chrome Web Store extension id are both required, there are no defaults):
;   ISCC.exe /DAppVersion=0.1.0 /DChromeExtensionId=<32 letters a-p> installer\memocampus.iss
; Needs installer\stage\ filled by installer\stage.ps1 (exactly the three executables).
;
; Isolated test build (never distributed; see installer\README.md): add /DInstallerTestMode. It uses
; another AppId, another install folder, another output name and a private registry key instead of
; Chrome's, so it cannot touch a real registration.
;
; Slice 3B-3 adds the lifecycle: before an update or an uninstall a running PRODUCT tray (verified by
; its executable path) is asked to quit and awaited for a bounded time; at uninstall only this
; installation's own Native Messaging registration, Run value and manifest are removed. Nothing is ever
; killed, nothing in the data folder is touched, StartupApproved is never read or written, and the
; installer never starts the tray (that is slice 3B-4).
; Not done here on purpose: the Chrome extension itself.
; Written for Inno Setup 7.1.1.

#ifndef AppVersion
  #error AppVersion is required: ISCC.exe /DAppVersion=<x.y.z> /DChromeExtensionId=<id> installer\memocampus.iss
#endif

; The Chrome Web Store extension id: exactly 32 characters, each a to p (lower case). A dummy is never
; supplied here; without a valid id the compile stops.
#ifndef ChromeExtensionId
  #error ChromeExtensionId is required: ISCC.exe /DChromeExtensionId=<32 letters a-p> ...
#endif
#if Len(ChromeExtensionId) != 32
  #error ChromeExtensionId must be exactly 32 characters (the Chrome Web Store extension id)
#endif
#ifdef StoreListing
  #ifdef InstallerTestMode
    #error StoreListing is for the product installer only; an installer test build never shows a Web Store link
  #endif
#endif
#define ExtIdPos 1
#sub CheckExtIdChar
  #if Pos(Copy(ChromeExtensionId, ExtIdPos, 1), "abcdefghijklmnop") == 0
    #error ChromeExtensionId may only contain the lower case letters a to p
  #endif
#endsub
#for {ExtIdPos = 1; ExtIdPos <= 32; ExtIdPos++} CheckExtIdChar

#define ProductHostName "io.github.memocampus.vault"

#ifdef InstallerTestMode
  #define NmhKeyPath "Software\MemocampusInstallerTest\NativeMessagingHosts\" + ProductHostName
  #define RunKeyPath "Software\MemocampusInstallerTest\Run"
  #define SetupAppId "{{2F6B1C57-6F0E-4D55-9B0D-0C3B8A5E7A41}"
  #define SetupDir "{localappdata}\Programs\MemocampusInstallerTest"
  #define SetupOutputDir "out-test"
  #define SetupOutputName "Memocampus-Setup-x64-INSTALLER-TEST"
  #define SetupName "Memocampus (installer test)"
#else
  #define NmhKeyPath "Software\Google\Chrome\NativeMessagingHosts\" + ProductHostName
  #define RunKeyPath "Software\Microsoft\Windows\CurrentVersion\Run"
  #define SetupAppId "{{63D78D26-D4BF-49ED-B29D-FBFFF1EAF56A}"
  #define SetupDir "{localappdata}\Programs\Memocampus"
  #define SetupOutputDir "out"
  #define SetupOutputName "Memocampus-Setup-x64"
  #define SetupName "Memocampus"
#endif

[Setup]
; Fixed for good. Never change it: it is how a later setup recognizes this installation.
AppId={#SetupAppId}
AppName={#SetupName}
AppVersion={#AppVersion}
AppPublisher=Memocampus
VersionInfoVersion={#AppVersion}

; Current user only: no administrator rights, nothing under HKLM or Program Files.
PrivilegesRequired=lowest
DefaultDirName={#SetupDir}
; The folder is fixed: no page to change it, and an earlier choice is not remembered.
DisableDirPage=yes
UsePreviousAppDir=no
DefaultGroupName=Memocampus
DisableProgramGroupPage=yes

; Windows on x64 only (Arm64 with x64 emulation counts as compatible); a 64-bit installation.
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible

OutputDir={#SetupOutputDir}
OutputBaseFilename={#SetupOutputName}
Compression=lzma2
SolidCompression=yes
UninstallDisplayName={#SetupName}
UninstallDisplayIcon={app}\memocampus-tray.exe

; A running tray is neither asked to close nor ended by this slice (that is slice 3B-3). If a
; file is in use, Setup's standard "file in use" prompt appears instead.
CloseApplications=no
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Files]
; Exactly these three. A missing source file stops the compile; nothing is generated.
Source: "stage\memocampus-native-host.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "stage\memocampus-mcp.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "stage\memocampus-tray.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
; Start menu > Memocampus > Memocampus. No parameters, no working directory, no environment.
Name: "{group}\Memocampus"; Filename: "{app}\memocampus-tray.exe"

[Messages]
; What "finished" means here: the Windows side only. The Chrome extension, choosing a vault and starting
; to remember are separate steps the user takes in Chrome; Setup never reports them as done.
FinishedLabel=Setup has installed the [name] programs and its connection to Chrome on this computer.%n%nThat is all Setup does. The Chrome extension is added by you in Chrome, and choosing a vault and starting to remember happen in the extension, only when you approve them there.
FinishedLabelNoIcons=Setup has installed the [name] programs and its connection to Chrome on this computer.%n%nThat is all Setup does. The Chrome extension is added by you in Chrome, and choosing a vault and starting to remember happen in the extension, only when you approve them there.

#ifndef InstallerTestMode
[Run]
; Offered on the last page, never run unless the user ticks it (unchecked), never in a silent install
; (skipifsilent), never when the installation failed (postinstall runs only after success). No
; arguments, no environment, the user's own rights (runasoriginaluser is the postinstall default).
; Starting the tray is not starting to remember: the tray's first start only creates its own settings
; (including start at logon, changeable in its menu); the vault, the policy and the MCP publication
; are not touched. A second start is refused by the tray itself (single instance), so ticking it with
; a tray already running does nothing.
Filename: "{app}\memocampus-tray.exe"; Description: "Start Memocampus now (its first start also turns on starting at logon; you can change that in the tray menu)"; Flags: nowait postinstall skipifsilent unchecked
#ifdef StoreListing
; Opens the Chrome Web Store page in the default browser, only when ticked. Nothing is installed in
; Chrome by Setup. The address is built from the same extension id the manifest trusts; a build
; without /DStoreListing (every test build) has no such entry.
Filename: "https://chromewebstore.google.com/detail/{#ChromeExtensionId}"; Description: "Open the Memocampus page in the Chrome Web Store (you add the extension yourself, in Chrome)"; Flags: shellexec postinstall skipifsilent unchecked
#endif
#endif

[Code]
const
  HostName = '{#ProductHostName}';
  HostKeyPath = '{#NmhKeyPath}';
  RunKeyPath = '{#RunKeyPath}';
  BuiltInExtensionId = '{#ChromeExtensionId}';

#include "nmh.pas"
#include "lifecycle.pas"

var
  PreSnap: TNmhSnapshot;
  Registered, InstallCompleted, DirCreated: Boolean;

function ManifestFile: String;
begin
  Result := ExpandConstant('{app}\' + HostName + '.json');
end;

function TrayExe: String;
begin
  Result := ExpandConstant('{app}\memocampus-tray.exe');
end;

// Stops here, before anything is changed, when a product tray does not end or something else still holds
// an installed executable. Returns '' when the way is clear.
function Quiesce: String;
var
  R: TLcResult;
  Locked: String;
begin
  Result := '';
  R := LcCloseTrays(TrayExe, LcTrayWaitMs);
  Log('lifecycle: trays seen ' + IntToStr(R.Found) + ', ours ' + IntToStr(R.Verified) + ', left alone ' + IntToStr(R.LeftAlone) +
    ', ended ' + IntToStr(R.Closed) + ', still running ' + IntToStr(R.TimedOut) + ', not asked ' + IntToStr(R.Unclosable));
  Log(R.Report);
  if (R.TimedOut > 0) or (R.Unclosable > 0) then begin
    Result := 'Memocampus is still running and did not quit in time. Nothing was changed.'#13#10#13#10 +
      'Quit Memocampus from its tray icon (right-click, Quit), wait a moment, and run this again.';
    Exit;
  end;
  Locked := LcLockedFiles(ExpandConstant('{app}'), 20);
  if Locked <> '' then
    Result := 'These Memocampus files are still in use by another program:'#13#10#13#10 + Locked + #13#10 +
      'Setup does not close other programs. Close AI clients that use Memocampus (for example an MCP ' +
      'client) and Chrome windows that use the Memocampus extension, then run this again. Nothing was changed.';
end;

// Before anything is installed: a registration that is not ours, a tray that will not quit and files
// held by other programs each stop the installation here, with the reason.
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Why: String;
begin
  Result := '';
  if not NmhIsValidExtensionId(BuiltInExtensionId) then
    Result := 'This installer was built with an invalid Chrome extension id.'
  else if not NmhCheckRegistration(HostKeyPath, ManifestFile, Why) then
    Result := Why
  else
    Result := Quiesce;
end;

// The registration is made BEFORE the files are copied: Abort is honoured in ssInstall (and not in
// ssPostInstall), so a failure here ends Setup with nothing installed, and a failure of the file copy
// afterwards is undone in DeinitializeSetup.
procedure CurStepChanged(CurStep: TSetupStep);
var
  Err, App: String;
begin
  if CurStep = ssInstall then begin
    App := ExpandConstant('{app}');
    if not DirExists(App) then
      if ForceDirectories(App) then DirCreated := True;
    PreSnap := NmhTake(HostKeyPath, ManifestFile);
    NmhExeMayBeMissing := True;
    Registered := NmhRegister(HostKeyPath, ManifestFile, HostName, App + '\memocampus-native-host.exe', BuiltInExtensionId, Err);
    NmhExeMayBeMissing := False;
    if not Registered then begin
      if DirCreated then RemoveDir(App);
      SuppressibleMsgBox('Memocampus could not register its connection to Chrome (native messaging host ' + HostName +
        '). Setup stops and nothing was installed unless this message says ROLLBACK_FAILED.'#13#10#13#10 + Err, mbCriticalError, MB_OK, IDOK);
      Abort;
    end;
  end else if CurStep = ssPostInstall then
    InstallCompleted := True;
end;

// Setup ended without completing after the registration was made (the file copy failed or was
// cancelled): put the registration and the manifest back as they were.
procedure DeinitializeSetup;
var
  Err: String;
begin
  if Registered and not InstallCompleted then begin
    if NmhRollback(HostKeyPath, ManifestFile, PreSnap, Err) then
      Log('setup incomplete: the native messaging registration was restored')
    else
      SuppressibleMsgBox('Setup did not finish and could not fully restore the Chrome registration: ' + Err + ' ' +
        'Running Setup again repairs it.', mbError, MB_OK, IDOK);
    if DirCreated then RemoveDir(ExpandConstant('{app}'));
  end;
end;

// Uninstall, step 1: a product tray must quit and nothing may hold the files, before anything is removed.
function InitializeUninstall: Boolean;
var
  Why: String;
begin
  Why := Quiesce;
  Result := Why = '';
  if not Result then
    SuppressibleMsgBox(Why, mbError, MB_OK, IDOK);
end;

// Uninstall, step 2: only what this installation registered, in a retryable order; the files follow.
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Report: String;
begin
  if CurUninstallStep = usUninstall then begin
    if not LcUninstallCleanup(HostKeyPath, ManifestFile, HostName, ExpandConstant('{app}\memocampus-native-host.exe'),
        BuiltInExtensionId, RunKeyPath, TrayExe, Report) then begin
      Log(Report);
      SuppressibleMsgBox('Memocampus could not remove one of its registrations, so the uninstall stopped before ' +
        'removing any file. Run the uninstall again.'#13#10#13#10 + Report, mbError, MB_OK, IDOK);
      Abort;
    end;
    Log(Report);
    if (Pos('KEPT', Report) > 0) or (Pos('FAILED', Report) > 0) then
      SuppressibleMsgBox('Memocampus was uninstalled. Some entries were kept because they are not Memocampus''s own:'#13#10#13#10 +
        Report, mbInformation, MB_OK, IDOK);
  end;
end;
