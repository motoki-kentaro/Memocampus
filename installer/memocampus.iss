; Memocampus installer, Issue #67 Slice 3B-1: the three executables and a Start menu entry.
;
; Build (the version is required, there is no default):
;   ISCC.exe /DAppVersion=0.1.0 installer\memocampus.iss
; Needs installer\stage\ filled by installer\stage.ps1 (exactly the three executables).
;
; Not done here on purpose (later slices): Native Messaging registration, the Run key, starting the
; tray, closing a running tray, the Chrome extension. There is no [Registry], [Run], [Dirs],
; [InstallDelete], [UninstallDelete] or [Code] section: the files below and Inno Setup's own
; uninstall record (under HKCU, because the install is per user) are all this installer owns.
; Written for Inno Setup 6.4 or later (ArchitecturesAllowed=x64compatible needs 6.3 or later).

#ifndef AppVersion
  #error AppVersion is required: ISCC.exe /DAppVersion=<x.y.z> installer\memocampus.iss
#endif

[Setup]
; Fixed for good. Never change it: it is how a later setup recognizes this installation.
AppId={{63D78D26-D4BF-49ED-B29D-FBFFF1EAF56A}
AppName=Memocampus
AppVersion={#AppVersion}
AppPublisher=Memocampus
VersionInfoVersion={#AppVersion}

; Current user only: no administrator rights, nothing under HKLM or Program Files.
PrivilegesRequired=lowest
DefaultDirName={localappdata}\Programs\Memocampus
; The folder is fixed: no page to change it, and an earlier choice is not remembered.
DisableDirPage=yes
UsePreviousAppDir=no
DefaultGroupName=Memocampus
DisableProgramGroupPage=yes

; Windows on x64 only (Arm64 with x64 emulation counts as compatible); a 64-bit installation.
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible

OutputDir=out
OutputBaseFilename=Memocampus-Setup-x64
Compression=lzma2
SolidCompression=yes
UninstallDisplayName=Memocampus
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
