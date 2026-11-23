; Self-test of installer\nmh.pas and installer\lifecycle.pas (Slice 3B-3). NOT an installer: it installs nothing,
; makes no uninstall record, and exits from InitializeSetup. Compiled and run by
; installer\run-lifecycle-test.ps1, never distributed.
;
; Parameters: /Mode=registry|find|close|locks  /SelfTestLog=<file>
;   registry  also /SelfTestKey=Software\MemocampusInstallerTest\<id> /SelfTestDir=<folder>: ownership logic
;             against private registry keys (the Run value and the native messaging key are under the root)
;   find      also /AppDir=<folder>: lists which running tray windows belong to <folder>\memocampus-tray.exe
;             (changes nothing)
;   close     also /AppDir /WaitMs=<n>: asks those trays to quit and waits (the real routine)
;   locks     also /AppDir /Tries=<n>: reports installed executables that are held open

[Setup]
AppName=Memocampus lifecycle self-test
AppVersion=0
CreateAppDir=no
Uninstallable=no
PrivilegesRequired=lowest
DisableWelcomePage=yes
DisableDirPage=yes
DisableProgramGroupPage=yes
DisableReadyPage=yes
DisableFinishedPage=yes
OutputBaseFilename=lifecycle-selftest
Compression=none
; The installer's own languages and texts (Issue #75; nmh.pas takes its texts from them). The runner picks
; one with /LANG=; never a dialog.
ShowLanguageDialog=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl,i18n\en.isl"
Name: "japanese"; MessagesFile: "compiler:Languages\Japanese.isl,i18n\ja.isl"

[Code]
#include "nmh.pas"
#include "lifecycle.pas"

const
  Prefix = 'Software\MemocampusInstallerTest\';
  Host = 'io.github.memocampus.vault';
  Id = 'abcdefghijklmnopabcdefghijklmnop';

var
  Log: TArrayOfString;
  Failures: Integer;
  Root, Dir, TrayPath: String;

procedure Say(const Line: String);
var
  N: Integer;
begin
  N := GetArrayLength(Log);
  SetArrayLength(Log, N + 1);
  Log[N] := Line;
end;

procedure Check(const Name: String; Ok: Boolean);
begin
  if Ok then
    Say('PASS ' + Name)
  else begin
    Say('FAIL ' + Name);
    Failures := Failures + 1;
  end;
end;

function NmhKey: String;
begin
  Result := Root + '\NativeMessagingHosts\' + Host;
end;

function RunKey: String;
begin
  Result := Root + '\Run';
end;

function ManifestPath: String;
begin
  Result := Dir + '\' + Host + '.json';
end;

function ExePath: String;
begin
  Result := Dir + '\memocampus-native-host.exe';
end;

function Dflt(const Key: String): String;
begin
  if not RegQueryStringValue(HKCU, Key, '', Result) then Result := '<none>';
end;

procedure ResetAll;
begin
  RegDeleteKeyIncludingSubkeys(HKCU, Root + '\NativeMessagingHosts');
  RegDeleteKeyIncludingSubkeys(HKCU, Root + '\Run');
  DeleteFile(ManifestPath);
  DeleteFile(ManifestPath + '.tmp');
  DeleteFile(ManifestPath + '.previous');
  LcFailAt := 0;
  NmhFailAt := 0;
  NmhFailRollback := False;
end;

procedure WriteOwnManifest;
var
  Err: String;
begin
  // The installer's own writer, so the content is exactly what a real install leaves.
  NmhRegister(NmhKey, ManifestPath, Host, ExePath, Id, Err);
end;

// ---- Native messaging removal ------------------------------------------------------------------------------
procedure TestNmhUnregister;
var
  R: String;
begin
  ResetAll;
  Check('nm: absent key is kept/nothing', NmhUnregister(NmhKey, ManifestPath, R) = 1);
  // Ours, alone in the key: value and key removed.
  WriteOwnManifest;
  Check('nm own: removed', NmhUnregister(NmhKey, ManifestPath, R) = 0);
  Check('nm own: key gone too', not RegKeyExists(HKCU, NmhKey));
  Check('nm own: report says removed', Pos('removed', R) > 0);
  // Ours, with another named value: the key stays.
  ResetAll;
  WriteOwnManifest;
  RegWriteStringValue(HKCU, NmhKey, 'Keep', 'x');
  Check('nm own+value: removed', NmhUnregister(NmhKey, ManifestPath, R) = 0);
  Check('nm own+value: other value kept', Dflt(NmhKey) = '<none>');
  Check('nm own+value: key kept', RegKeyExists(HKCU, NmhKey) and RegValueExists(HKCU, NmhKey, 'Keep'));
  Check('nm own+value: report says key kept', Pos('key was kept', R) > 0);
  // Ours, with a subkey: the key stays with the subkey (never a recursive delete).
  ResetAll;
  WriteOwnManifest;
  RegWriteStringValue(HKCU, NmhKey + '\sub', 'v', 'y');
  Check('nm own+subkey: removed', NmhUnregister(NmhKey, ManifestPath, R) = 0);
  Check('nm own+subkey: subkey kept', RegKeyExists(HKCU, NmhKey + '\sub') and RegValueExists(HKCU, NmhKey + '\sub', 'v'));
  // Another program's value.
  ResetAll;
  RegWriteStringValue(HKCU, NmhKey, '', 'C:\other\program.json');
  Check('nm foreign: kept', NmhUnregister(NmhKey, ManifestPath, R) = 1);
  Check('nm foreign: value unchanged', Dflt(NmhKey) = 'C:\other\program.json');
  Check('nm foreign: report says KEPT and names it', (Pos('KEPT', R) > 0) and (Pos('C:\other\program.json', R) > 0));
  // The same path in another spelling is still ours.
  ResetAll;
  RegWriteStringValue(HKCU, NmhKey, '', Uppercase(ManifestPath));
  Check('nm own, spelled in capitals: removed', NmhUnregister(NmhKey, ManifestPath, R) = 0);
  // No default value.
  ResetAll;
  RegWriteStringValue(HKCU, NmhKey, 'Other', 'x');
  Check('nm no default: kept', NmhUnregister(NmhKey, ManifestPath, R) = 1);
  Check('nm no default: untouched', RegValueExists(HKCU, NmhKey, 'Other'));
  // A default value that is not a string.
  ResetAll;
  RegWriteDWordValue(HKCU, NmhKey, '', 1);
  Check('nm dword default: kept', NmhUnregister(NmhKey, ManifestPath, R) = 1);
  Check('nm dword default: untouched', RegKeyExists(HKCU, NmhKey));
  // Another host's key is not touched by anything above.
  ResetAll;
  RegWriteStringValue(HKCU, Root + '\NativeMessagingHosts\other.host', '', 'C:\other\host.json');
  WriteOwnManifest;
  NmhUnregister(NmhKey, ManifestPath, R);
  Check('nm: other host untouched', Dflt(Root + '\NativeMessagingHosts\other.host') = 'C:\other\host.json');
end;

// ---- Manifest removal ----------------------------------------------------------------------------------------
procedure TestManifestRemoval;
var
  R: String;
begin
  ResetAll;
  Check('manifest absent', NmhRemoveOwnManifest(ManifestPath, Host, ExePath, Id, R) = 1);
  WriteOwnManifest;
  NmhUnregister(NmhKey, ManifestPath, R);
  Check('manifest own content: removed', NmhRemoveOwnManifest(ManifestPath, Host, ExePath, Id, R) = 0);
  Check('manifest own content: file gone', not FileExists(ManifestPath));
  ResetAll;
  SaveStringToFile(ManifestPath, '{"name":"someone else"}', False);
  Check('manifest foreign content: kept', NmhRemoveOwnManifest(ManifestPath, Host, ExePath, Id, R) = 1);
  Check('manifest foreign content: file stays', FileExists(ManifestPath));
  Check('manifest foreign content: reported KEPT', Pos('KEPT', R) > 0);
  ResetAll;
  WriteOwnManifest;
  Check('manifest of another extension id: kept', NmhRemoveOwnManifest(ManifestPath, Host, ExePath, 'ponmlkjihgfedcbaponmlkjihgfedcba', R) = 1);
  Check('manifest of another extension id: stays', FileExists(ManifestPath));
end;

// ---- Run value ownership ----------------------------------------------------------------------------------------
procedure TestRun;
var
  R, Why: String;
  Tray: String;
begin
  Tray := Dir + '\memocampus-tray.exe';
  ResetAll;
  Check('run absent: nothing', LcRemoveRunValue(RunKey, LcRunValueName, Tray, R) = 1);
  Check('run absent: reported', Pos('nothing', R) > 0);
  RegWriteStringValue(HKCU, RunKey, LcRunValueName, '"' + Tray + '"');
  Check('run ours (quoted): recognized', LcRunValueIsOurs(RunKey, LcRunValueName, Tray, Why));
  Check('run ours (quoted): removed', LcRemoveRunValue(RunKey, LcRunValueName, Tray, R) = 0);
  Check('run ours (quoted): gone', not RegValueExists(HKCU, RunKey, LcRunValueName));
  RegWriteStringValue(HKCU, RunKey, LcRunValueName, Tray);
  Check('run ours (unquoted): removed', LcRemoveRunValue(RunKey, LcRunValueName, Tray, R) = 0);
  RegWriteStringValue(HKCU, RunKey, LcRunValueName, '"' + Uppercase(Tray) + '"');
  Check('run ours (capitals): removed', LcRemoveRunValue(RunKey, LcRunValueName, Tray, R) = 0);
  RegWriteStringValue(HKCU, RunKey, LcRunValueName, '"' + Tray + '"   ');
  Check('run ours (trailing blanks): removed', LcRemoveRunValue(RunKey, LcRunValueName, Tray, R) = 0);
  // Not ours: every one of these must stay.
  RegWriteStringValue(HKCU, RunKey, LcRunValueName, '"C:\works\Memocampus\tray\target\release\memocampus-tray.exe"');
  Check('run development exe: kept', LcRemoveRunValue(RunKey, LcRunValueName, Tray, R) = 1);
  Check('run development exe: still there', RegValueExists(HKCU, RunKey, LcRunValueName));
  Check('run development exe: reported KEPT', Pos('KEPT', R) > 0);
  RegWriteStringValue(HKCU, RunKey, LcRunValueName, '"' + Dir + 'x\memocampus-tray.exe"');
  Check('run same name, other folder: kept', LcRemoveRunValue(RunKey, LcRunValueName, Tray, R) = 1);
  RegWriteStringValue(HKCU, RunKey, LcRunValueName, '"' + Tray + '" --headless');
  Check('run with arguments: kept', LcRemoveRunValue(RunKey, LcRunValueName, Tray, R) = 1);
  RegWriteStringValue(HKCU, RunKey, LcRunValueName, '"' + Tray + '" "x"');
  Check('run odd quoting: kept', LcRemoveRunValue(RunKey, LcRunValueName, Tray, R) = 1);
  RegWriteStringValue(HKCU, RunKey, LcRunValueName, 'C:\Windows\notepad.exe');
  Check('run another program: kept', LcRemoveRunValue(RunKey, LcRunValueName, Tray, R) = 1);
  RegWriteStringValue(HKCU, RunKey, LcRunValueName, '');
  Check('run empty value: kept', LcRemoveRunValue(RunKey, LcRunValueName, Tray, R) = 1);
  RegWriteDWordValue(HKCU, RunKey, LcRunValueName, 1);
  Check('run not a string: kept', LcRemoveRunValue(RunKey, LcRunValueName, Tray, R) = 1);
  Check('run not a string: still there', RegValueExists(HKCU, RunKey, LcRunValueName));
  RegDeleteValue(HKCU, RunKey, LcRunValueName);
  // A value with another NAME that starts our tray is not ours to remove (the name decides first).
  RegWriteStringValue(HKCU, RunKey, 'SomethingElse', '"' + Tray + '"');
  Check('run other name: untouched', LcRemoveRunValue(RunKey, LcRunValueName, Tray, R) = 1);
  Check('run other name: still there', RegValueExists(HKCU, RunKey, 'SomethingElse'));
  // The deletion only touches that one value.
  RegWriteStringValue(HKCU, RunKey, LcRunValueName, '"' + Tray + '"');
  RegWriteStringValue(HKCU, RunKey, 'Neighbour', 'C:\n\n.exe');
  LcRemoveRunValue(RunKey, LcRunValueName, Tray, R);
  Check('run: neighbour untouched', Dflt(RunKey) = '<none>');
  Check('run: neighbour value kept', RegValueExists(HKCU, RunKey, 'Neighbour') and RegValueExists(HKCU, RunKey, 'SomethingElse'));
  // StartupApproved is never part of any of it.
  Check('startupapproved: not referenced by the routines', not RegKeyExists(HKCU, Root + '\StartupApproved'));
end;

// ---- The whole cleanup, its order, failures and the retry --------------------------------------------------------------
procedure TestCleanup;
var
  R: String;
  Tray: String;
  Ok: Boolean;
begin
  Tray := Dir + '\memocampus-tray.exe';
  // Everything ours.
  ResetAll;
  WriteOwnManifest;
  RegWriteStringValue(HKCU, RunKey, LcRunValueName, '"' + Tray + '"');
  Ok := LcUninstallCleanup(NmhKey, ManifestPath, Host, ExePath, Id, RunKey, Tray, R);
  Check('cleanup all ours: ok', Ok);
  Check('cleanup all ours: registration gone', not RegKeyExists(HKCU, NmhKey));
  Check('cleanup all ours: run value gone', not RegValueExists(HKCU, RunKey, LcRunValueName));
  Check('cleanup all ours: manifest gone', not FileExists(ManifestPath));
  // Registration changed to another program: kept, and our own manifest is still removed.
  ResetAll;
  WriteOwnManifest;
  RegWriteStringValue(HKCU, NmhKey, '', 'C:\other\program.json');
  RegWriteStringValue(HKCU, RunKey, LcRunValueName, '"C:\dev\memocampus-tray.exe"');
  Ok := LcUninstallCleanup(NmhKey, ManifestPath, Host, ExePath, Id, RunKey, Tray, R);
  Check('cleanup foreign: ok (nothing of ours failed)', Ok);
  Check('cleanup foreign: registration kept', Dflt(NmhKey) = 'C:\other\program.json');
  Check('cleanup foreign: run value kept', RegValueExists(HKCU, RunKey, LcRunValueName));
  Check('cleanup foreign: our manifest content removed', not FileExists(ManifestPath));
  Check('cleanup foreign: report lists what was kept', (Pos('KEPT', R) > 0));
  // Registration changed to another program, and the manifest file replaced by theirs: it stays.
  ResetAll;
  RegWriteStringValue(HKCU, NmhKey, '', ManifestPath);
  SaveStringToFile(ManifestPath, '{"name":"someone else"}', False);
  // (the registration names this path, so it counts as ours; the manifest is not removed while registered)
  Ok := LcUninstallCleanup(NmhKey, ManifestPath, Host, ExePath, Id, RunKey, Tray, R);
  Check('cleanup own registration, foreign manifest bytes: ok', Ok);
  Check('cleanup own registration, foreign manifest bytes: file stays', FileExists(ManifestPath));
  // A failure in the middle: stops, keeps what is still ours, and a second run finishes.
  ResetAll;
  WriteOwnManifest;
  RegWriteStringValue(HKCU, RunKey, LcRunValueName, '"' + Tray + '"');
  LcFailAt := 2;
  Ok := LcUninstallCleanup(NmhKey, ManifestPath, Host, ExePath, Id, RunKey, Tray, R);
  Check('failure in the run step: cleanup reports failure', not Ok);
  Check('failure in the run step: the value is still there', RegValueExists(HKCU, RunKey, LcRunValueName));
  Check('failure in the run step: the manifest is still there', FileExists(ManifestPath));
  Check('failure in the run step: report says FAILED', Pos('FAILED', R) > 0);
  LcFailAt := 0;
  Ok := LcUninstallCleanup(NmhKey, ManifestPath, Host, ExePath, Id, RunKey, Tray, R);
  Check('retry: finishes', Ok);
  Check('retry: run value gone', not RegValueExists(HKCU, RunKey, LcRunValueName));
  Check('retry: manifest gone', not FileExists(ManifestPath));
  Check('retry: registration gone', not RegKeyExists(HKCU, NmhKey));
  // A failure in the first step: nothing after it was done.
  ResetAll;
  WriteOwnManifest;
  RegWriteStringValue(HKCU, RunKey, LcRunValueName, '"' + Tray + '"');
  NmhFailAt := 5;
  Ok := LcUninstallCleanup(NmhKey, ManifestPath, Host, ExePath, Id, RunKey, Tray, R);
  Check('failure in the registration step: reports failure', not Ok);
  Check('failure in the registration step: registration still there', NmhOwnsRegistration(NmhKey, ManifestPath));
  Check('failure in the registration step: run value untouched', RegValueExists(HKCU, RunKey, LcRunValueName));
  Check('failure in the registration step: manifest untouched', FileExists(ManifestPath));
  Check('failure in the registration step: report says FAILED', Pos('FAILED', R) > 0);
  NmhFailAt := 0;
  Check('retry after registration failure: finishes', LcUninstallCleanup(NmhKey, ManifestPath, Host, ExePath, Id, RunKey, Tray, R));
  // Running the cleanup again on an already clean state is harmless.
  Check('cleanup twice: second run ok', LcUninstallCleanup(NmhKey, ManifestPath, Host, ExePath, Id, RunKey, Tray, R));
  ResetAll;
end;

procedure RunRegistryMode;
begin
  Say('language ' + ActiveLanguage);
  ForceDirectories(Dir);
  SaveStringToFile(ExePath, 'x', False);
  ResetAll;
  TestNmhUnregister;
  TestManifestRemoval;
  TestRun;
  TestCleanup;
  ResetAll;
end;

function InitializeSetup: Boolean;
var
  Mode, LogFile, AppDir, Locked: String;
  R: TLcResult;
begin
  Result := False;
  Mode := ExpandConstant('{param:Mode}');
  LogFile := ExpandConstant('{param:SelfTestLog}');
  if LogFile = '' then Exit;
  Failures := 0;
  try
    if Mode = 'registry' then begin
      Root := ExpandConstant('{param:SelfTestKey}');
      Dir := ExpandConstant('{param:SelfTestDir}');
      if (Dir = '') or (Copy(Root, 1, Length(Prefix)) <> Prefix) or (Pos('Chrome', Root) > 0) or (Pos('..', Root) > 0) or
         (Length(Root) <= Length(Prefix)) then begin
        Say('REFUSED unsafe or missing parameters');
        Failures := Failures + 1;
      end else
        RunRegistryMode;
    end else begin
      AppDir := ExpandConstant('{param:AppDir}');
      if AppDir = '' then begin
        Say('REFUSED no AppDir');
        Failures := Failures + 1;
      end else if Mode = 'find' then begin
        R := LcFindTrays(AppDir + '\memocampus-tray.exe');
        Say('found ' + IntToStr(R.Found) + ' verified ' + IntToStr(R.Verified) + ' leftalone ' + IntToStr(R.LeftAlone));
        Say(R.Report);
      end else if Mode = 'close' then begin
        R := LcCloseTrays(AppDir + '\memocampus-tray.exe', StrToIntDef(ExpandConstant('{param:WaitMs}'), 3000));
        Say('found ' + IntToStr(R.Found) + ' verified ' + IntToStr(R.Verified) + ' leftalone ' + IntToStr(R.LeftAlone) +
          ' closed ' + IntToStr(R.Closed) + ' timedout ' + IntToStr(R.TimedOut) + ' unclosable ' + IntToStr(R.Unclosable) +
          ' incomplete ' + IntToStr(R.Incomplete));
        Say(R.Report);
      end else if Mode = 'locks' then begin
        Locked := LcLockedFiles(AppDir, StrToIntDef(ExpandConstant('{param:Tries}'), 2));
        Say('locked [' + Locked + ']');
      end else begin
        Say('REFUSED unknown mode');
        Failures := Failures + 1;
      end;
    end;
  except
    Say('EXCEPTION ' + GetExceptionMessage);
    Failures := Failures + 1;
  end;
  Say('failures ' + IntToStr(Failures));
  SaveStringsToUTF8FileWithoutBOM(LogFile, Log, False);
end;
