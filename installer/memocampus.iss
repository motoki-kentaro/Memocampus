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
; Not done here on purpose (later slices): the Run key, starting or closing the tray, removing the
; Native Messaging registration at uninstall (3B-3), the Chrome extension itself.
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
  #define SetupAppId "{{2F6B1C57-6F0E-4D55-9B0D-0C3B8A5E7A41}"
  #define SetupDir "{localappdata}\Programs\MemocampusInstallerTest"
  #define SetupOutputDir "out-test"
  #define SetupOutputName "Memocampus-Setup-x64-INSTALLER-TEST"
  #define SetupName "Memocampus (installer test)"
#else
  #define NmhKeyPath "Software\Google\Chrome\NativeMessagingHosts\" + ProductHostName
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

[UninstallDelete]
; The one file this setup generates (not part of [Files]). Removing the Native Messaging registry value
; is slice 3B-3 and must check that it is still this installation's own.
Type: files; Name: "{app}\{#ProductHostName}.json"

[Code]
const
  HostName = '{#ProductHostName}';
  HostKeyPath = '{#NmhKeyPath}';
  BuiltInExtensionId = '{#ChromeExtensionId}';

#include "nmh.pas"

function ManifestFile: String;
begin
  Result := ExpandConstant('{app}\' + HostName + '.json');
end;

// Before anything is installed: a registration that is not ours stops the installation here, with the
// reason, instead of installing without a working Chrome connection and reporting success.
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Why: String;
begin
  Result := '';
  if not NmhIsValidExtensionId(BuiltInExtensionId) then
    Result := 'This installer was built with an invalid Chrome extension id.'
  else if not NmhCheckRegistration(HostKeyPath, ManifestFile, Why) then
    Result := Why;
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  Err: String;
begin
  if CurStep = ssPostInstall then
    if not NmhRegister(HostKeyPath, ManifestFile, HostName, ExpandConstant('{app}\memocampus-native-host.exe'), BuiltInExtensionId, Err) then
      RaiseException('Memocampus could not register its connection to Chrome (native messaging host ' + HostName +
        '). Nothing was left registered unless this message says ROLLBACK_FAILED.'#13#10#13#10 + Err);
end;
