; Self-test of installer\nmh.pas, the Native Messaging registration code of the installer.
; NOT an installer: it installs nothing, writes no [Files], creates no uninstall record, and exits from
; InitializeSetup. It is compiled and run by installer\run-selftest.ps1, never distributed.
;
; Safety: every registry access of the code under test goes to a private key that the caller names with
; /SelfTestKey=Software\MemocampusInstallerTest\<id>, and only to a key under that prefix (anything
; else, in particular a Chrome key, is refused before the first test). Files go to the folder named by
; /SelfTestDir. Results go to the file named by /SelfTestLog.

[Setup]
AppName=Memocampus NMH self-test
AppVersion=0
CreateAppDir=no
Uninstallable=no
PrivilegesRequired=lowest
DisableWelcomePage=yes
DisableDirPage=yes
DisableProgramGroupPage=yes
DisableReadyPage=yes
DisableFinishedPage=yes
OutputBaseFilename=nmh-selftest
Compression=none

[Code]
#include "nmh.pas"

const
  Prefix = 'Software\MemocampusInstallerTest\';
  Host = 'io.github.memocampus.vault';
  Id = 'abcdefghijklmnopabcdefghijklmnop';
  Id2 = 'ponmlkjihgfedcbaponmlkjihgfedcba';

var
  Log: TArrayOfString;
  Failures: Integer;
  Root, Dir: String;

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

function KeyOf(const Name: String): String;
begin
  Result := Root + '\NativeMessagingHosts\' + Name;
end;

function ValueOf(const Key: String): String;
begin
  if not RegQueryStringValue(HKCU, Key, '', Result) then Result := '<none>';
end;

procedure Reset;
begin
  NmhFailAt := 0;
  NmhFailRollback := False;
  if RegKeyExists(HKCU, KeyOf(Host)) then begin
    RegDeleteValue(HKCU, KeyOf(Host), '');
    RegDeleteKeyIfEmpty(HKCU, KeyOf(Host));
  end;
  DeleteFile(Dir + '\' + Host + '.json');
  DeleteFile(Dir + '\' + Host + '.json.tmp');
  DeleteFile(Dir + '\' + Host + '.json.previous');
end;

function ScratchLeft(const M: String): Boolean;
begin
  Result := FileExists(M + '.tmp') or FileExists(M + '.previous');
end;

procedure TestIds;
begin
  Check('id valid', NmhIsValidExtensionId(Id));
  Check('id valid (all letters)', NmhIsValidExtensionId('abcdefghijklmnopabcdefghijklmnop'));
  Check('id empty rejected', not NmhIsValidExtensionId(''));
  Check('id 31 rejected', not NmhIsValidExtensionId('abcdefghijklmnopabcdefghijklmno'));
  Check('id 33 rejected', not NmhIsValidExtensionId('abcdefghijklmnopabcdefghijklmnopa'));
  Check('id upper case rejected', not NmhIsValidExtensionId('Abcdefghijklmnopabcdefghijklmnop'));
  Check('id letter q rejected', not NmhIsValidExtensionId('abcdefghijklmnopabcdefghijklmnoq'));
  Check('id digit rejected', not NmhIsValidExtensionId('abcdefghijklmnopabcdefghijklmno1'));
  Check('id wildcard rejected', not NmhIsValidExtensionId('*bcdefghijklmnopabcdefghijklmnop'));
  Check('id with slash rejected', not NmhIsValidExtensionId('abcdefghijklmnopabcdefghijklmn/p'));
end;

procedure TestPathsAndJson;
var
  Lines: TArrayOfString;
begin
  Check('path ok', NmhIsSafeExePath('C:\Users\a\AppData\Local\Programs\Memocampus\memocampus-native-host.exe'));
  Check('path relative rejected', not NmhIsSafeExePath('memocampus-native-host.exe'));
  Check('path UNC rejected', not NmhIsSafeExePath('\\server\share\x.exe'));
  Check('path not exe rejected', not NmhIsSafeExePath('C:\x\y.dll'));
  Check('path quote rejected', not NmhIsSafeExePath('C:\x"y\z.exe'));
  Check('path wildcard rejected', not NmhIsSafeExePath('C:\x*\z.exe'));
  Check('json backslash and quote', NmhJsonString('C:\a b\"q"') = '"C:\\a b\\\"q\""');
  Check('json control char', NmhJsonString('a' + #9 + 'b') = '"a\u0009b"');
  Check('json hex', NmhHex4(31) = '001f');
  Lines := NmhManifestLines(Host, 'C:\Users\a\x.exe', Id);
  Check('manifest text', NmhManifestText(Lines) = '{'#13#10 + '  "name": "io.github.memocampus.vault",'#13#10 +
    '  "description": "Memocampus Native Messaging Host",'#13#10 + '  "path": "C:\\Users\\a\\x.exe",'#13#10 +
    '  "type": "stdio",'#13#10 + '  "allowed_origins": ['#13#10 +
    '    "chrome-extension://abcdefghijklmnopabcdefghijklmnop/"'#13#10 + '  ]'#13#10 + '}'#13#10);
  Check('utf8 decode of ascii', NmhUtf8Decode('abc') = 'abc');
  Check('same path ignores case and slashes', NmhSamePath('C:\A\b.json', 'c:/a/B.JSON'));
  Check('different paths differ', not NmhSamePath('C:\A\b.json', 'C:\A\c.json'));
  Check('empty path never matches', not NmhSamePath('', ''));
end;

procedure TestFresh;
var
  M, Err: String;
begin
  Reset;
  M := Dir + '\' + Host + '.json';
  Check('fresh: allowed', NmhCheckRegistration(KeyOf(Host), M, Err));
  Check('fresh: registered', NmhRegister(KeyOf(Host), M, Host, Dir + '\memocampus-native-host.exe', Id, Err));
  Check('fresh: value is the manifest path', ValueOf(KeyOf(Host)) = M);
  Check('fresh: manifest exists', FileExists(M));
  Check('fresh: owns', NmhOwnsRegistration(KeyOf(Host), M));
  Check('fresh: no scratch left', not ScratchLeft(M));
  Check('fresh: manifest is the expected text', NmhUtf8Decode(NmhRawOf(M)) =
    NmhManifestText(NmhManifestLines(Host, Dir + '\memocampus-native-host.exe', Id)));
  Check('fresh: no byte order mark', (Length(NmhRawOf(M)) > 3) and (Ord(NmhRawOf(M)[1]) = 123));
  Check('fresh: other host untouched', ValueOf(KeyOf('other.host')) = 'C:\other\host.json');
end;

procedure TestUpdate;
var
  M, Err: String;
begin
  Reset;
  M := Dir + '\' + Host + '.json';
  NmhRegister(KeyOf(Host), M, Host, Dir + '\memocampus-native-host.exe', Id, Err);
  Check('update: re-registered with another id', NmhRegister(KeyOf(Host), M, Host, Dir + '\memocampus-native-host.exe', Id2, Err));
  Check('update: new id in manifest', NmhUtf8Decode(NmhRawOf(M)) =
    NmhManifestText(NmhManifestLines(Host, Dir + '\memocampus-native-host.exe', Id2)));
  Check('update: value unchanged', ValueOf(KeyOf(Host)) = M);
  Check('update: no scratch left', not ScratchLeft(M));
end;

procedure TestConflict;
var
  M, Err, Before: String;
begin
  Reset;
  M := Dir + '\' + Host + '.json';
  RegWriteStringValue(HKCU, KeyOf(Host), '', 'C:\somewhere\else.json');
  Check('conflict: refused by the check', not NmhCheckRegistration(KeyOf(Host), M, Err));
  Check('conflict: reason names the other path', Pos('C:\somewhere\else.json', Err) > 0);
  Check('conflict: register refuses', not NmhRegister(KeyOf(Host), M, Host, Dir + '\memocampus-native-host.exe', Id, Err));
  Check('conflict: value unchanged', ValueOf(KeyOf(Host)) = 'C:\somewhere\else.json');
  Check('conflict: no manifest written', not FileExists(M));
  Check('conflict: not owned', not NmhOwnsRegistration(KeyOf(Host), M));
  // A key with no default value is not identifiable either.
  Reset;
  RegWriteStringValue(HKCU, KeyOf(Host), 'Other', 'x');
  Before := ValueOf(KeyOf(Host));
  Check('empty key: refused', not NmhCheckRegistration(KeyOf(Host), M, Err));
  Check('empty key: register refuses', not NmhRegister(KeyOf(Host), M, Host, Dir + '\memocampus-native-host.exe', Id, Err));
  Check('empty key: untouched', (ValueOf(KeyOf(Host)) = Before) and RegValueExists(HKCU, KeyOf(Host), 'Other'));
  RegDeleteValue(HKCU, KeyOf(Host), 'Other');
  // Inputs that must be refused before anything is written.
  Reset;
  Check('bad id: refused', not NmhRegister(KeyOf(Host), M, Host, Dir + '\memocampus-native-host.exe', 'BAD', Err));
  Check('relative exe: refused', not NmhRegister(KeyOf(Host), M, Host, 'memocampus-native-host.exe', Id, Err));
  Check('missing exe: refused', not NmhRegister(KeyOf(Host), M, Host, Dir + '\nothere.exe', Id, Err));
  Check('refused inputs leave nothing', not RegKeyExists(HKCU, KeyOf(Host)) and not FileExists(M));
end;

procedure TestFaultsFresh;
var
  M, Err: String;
  At: Integer;
begin
  M := Dir + '\' + Host + '.json';
  for At := 1 to 4 do begin
    Reset;
    NmhFailAt := At;
    Check('fresh fault ' + IntToStr(At) + ': fails', not NmhRegister(KeyOf(Host), M, Host, Dir + '\memocampus-native-host.exe', Id, Err));
    Check('fresh fault ' + IntToStr(At) + ': key gone', not RegKeyExists(HKCU, KeyOf(Host)));
    Check('fresh fault ' + IntToStr(At) + ': manifest gone', not FileExists(M));
    Check('fresh fault ' + IntToStr(At) + ': no scratch', not ScratchLeft(M));
    Check('fresh fault ' + IntToStr(At) + ': clean error', Pos('ROLLBACK_FAILED', Err) = 0);
    Check('fresh fault ' + IntToStr(At) + ': other host untouched', ValueOf(KeyOf('other.host')) = 'C:\other\host.json');
  end;
  NmhFailAt := 0;
end;

procedure TestFaultsUpdate;
var
  M, Err, Hash: String;
  At: Integer;
begin
  M := Dir + '\' + Host + '.json';
  for At := 1 to 4 do begin
    Reset;
    NmhRegister(KeyOf(Host), M, Host, Dir + '\memocampus-native-host.exe', Id, Err);
    Hash := GetSHA256OfFile(M);
    NmhFailAt := At;
    Check('update fault ' + IntToStr(At) + ': fails', not NmhRegister(KeyOf(Host), M, Host, Dir + '\memocampus-native-host.exe', Id2, Err));
    NmhFailAt := 0;
    Check('update fault ' + IntToStr(At) + ': previous manifest back', GetSHA256OfFile(M) = Hash);
    Check('update fault ' + IntToStr(At) + ': previous value kept', ValueOf(KeyOf(Host)) = M);
    Check('update fault ' + IntToStr(At) + ': no scratch', not ScratchLeft(M));
    Check('update fault ' + IntToStr(At) + ': clean error', Pos('ROLLBACK_FAILED', Err) = 0);
  end;
end;

procedure TestRollbackFailureIsReported;
var
  M, Err: String;
begin
  Reset;
  M := Dir + '\' + Host + '.json';
  NmhFailAt := 3;
  NmhFailRollback := True;
  Check('rollback failure: register fails', not NmhRegister(KeyOf(Host), M, Host, Dir + '\memocampus-native-host.exe', Id, Err));
  Check('rollback failure: reported, not hidden', Pos('ROLLBACK_FAILED', Err) > 0);
  NmhFailAt := 0;
  NmhFailRollback := False;
end;

procedure TestRollbackRespectsOwnership;
var
  M, Err: String;
  Snap: TNmhSnapshot;
begin
  Reset;
  M := Dir + '\' + Host + '.json';
  // Setup created the key (snapshot: absent) ...
  Snap := NmhTake(KeyOf(Host), M);
  // ... but another program has since written its own registration there.
  RegWriteStringValue(HKCU, KeyOf(Host), '', 'C:\other\program.json');
  Check('foreign change: rollback refuses to remove it', not NmhRollback(KeyOf(Host), M, Snap, Err));
  Check('foreign change: value kept', ValueOf(KeyOf(Host)) = 'C:\other\program.json');
  Check('foreign change: reason given', Length(Err) > 0);
  Reset;
end;

procedure TestLeaveManifestsForTheJsonParser;
var
  Err, Exe, M, Sub: String;
begin
  // Two manifests are left behind for the caller's JSON parser (ConvertFrom-Json), which compares
  // the parsed path and origin with the "manifest" lines below: a plain path, and a path with
  // Japanese characters and a space (the file must be UTF-8 for Chrome to read the path back).
  Reset;
  M := Dir + '\' + Host + '.json';
  Exe := Dir + '\memocampus-native-host.exe';
  Check('left: plain registered', NmhRegister(KeyOf(Host), M, Host, Exe, Id, Err));
  Say('manifest ' + M + '|' + Exe + '|' + Id);
  Sub := Dir + '\' + Chr($65E5) + Chr($672C) + Chr($8A9E) + ' ' + Chr($00E9) + 'x';
  ForceDirectories(Sub);
  SaveStringToFile(Sub + '\memocampus-native-host.exe', 'x', False);
  M := Sub + '\' + Host + '.json';
  Exe := Sub + '\memocampus-native-host.exe';
  Check('left: unicode registered', NmhRegister(KeyOf('unicode.test'), M, Host, Exe, Id2, Err));
  Check('left: unicode value is the path', ValueOf(KeyOf('unicode.test')) = M);
  Say('manifest ' + M + '|' + Exe + '|' + Id2);
end;

function InitializeSetup: Boolean;
var
  LogFile: String;
begin
  Result := False;
  Root := ExpandConstant('{param:SelfTestKey}');
  Dir := ExpandConstant('{param:SelfTestDir}');
  LogFile := ExpandConstant('{param:SelfTestLog}');
  if (LogFile = '') or (Dir = '') or (Copy(Root, 1, Length(Prefix)) <> Prefix) or (Pos('Chrome', Root) > 0) or
     (Pos('..', Root) > 0) or (Length(Root) <= Length(Prefix)) then begin
    if LogFile <> '' then SaveStringToFile(LogFile, 'REFUSED unsafe or missing parameters'#13#10, False);
    Exit;
  end;
  Failures := 0;
  Say('root ' + Root);
  ForceDirectories(Dir);
  SaveStringToFile(Dir + '\memocampus-native-host.exe', 'x', False);
  RegWriteStringValue(HKCU, KeyOf('other.host'), '', 'C:\other\host.json');
  try
    TestIds;
    TestPathsAndJson;
    TestFresh;
    TestUpdate;
    TestConflict;
    TestFaultsFresh;
    TestFaultsUpdate;
    TestRollbackFailureIsReported;
    TestRollbackRespectsOwnership;
    TestLeaveManifestsForTheJsonParser;
  except
    Check('no exception', False);
    Say('EXCEPTION ' + GetExceptionMessage);
  end;
  Say('failures ' + IntToStr(Failures));
  SaveStringsToUTF8FileWithoutBOM(ExpandConstant('{param:SelfTestLog}'), Log, False);
end;
