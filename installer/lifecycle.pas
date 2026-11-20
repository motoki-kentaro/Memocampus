// Process lifecycle and ownership helpers for the Memocampus installer (Issue #67, Slice 3B-3).
// Included into [Code] after nmh.pas, by memocampus.iss and by lifecycle-selftest.iss.
//
// What it does, and what it must never do:
//   - LcCloseTrays asks a running product tray to quit, by posting WM_CLOSE to its window, and waits for
//     the process to end, for a bounded time. Only a window whose OWNING PROCESS is verified to run
//     exactly {app}\memocampus-tray.exe, in this session, is ever messaged. Nothing is terminated:
//     no TerminateProcess, no taskkill, no kill by name. A tray that does not end in time is reported
//     and the caller stops.
//   - LcLockedFiles finds installed executables that something else (an AI client's memocampus-mcp.exe,
//     Chrome's native host) still holds open, without changing them. Those processes are never touched.
//   - LcRemoveRunValue removes the tray's own HKCU Run value, only when it starts exactly this
//     installation's tray. StartupApproved is never read or written here.

const
  LcTrayClass = 'MemocampusTrayHost';
  LcRunValueName = 'MemocampusTray';
  LcWmClose = 16;
  LcProcessQueryLimitedInformation = $1000;
  LcSynchronize = $100000;
  LcStillActive = 259;
  LcExitQuitIncomplete = 7;
  // The tray's quit may wait up to 30 s for its operations thread (CONTROLLER_WAIT in tray/src/quit.rs)
  // and then stops the server it owns; 45 s leaves 15 s for that and for process exit. Finite on purpose.
  LcTrayWaitMs = 45000;

function EnumWindows(lpEnumFunc: NativeInt; lParam: LPARAM): BOOL;
external 'EnumWindows@user32.dll stdcall';
function GetClassNameW(hWnd: HWND; lpClassName: String; nMaxCount: Integer): Integer;
external 'GetClassNameW@user32.dll stdcall';
function GetWindowThreadProcessId(hWnd: HWND; var lpdwProcessId: DWORD): DWORD;
external 'GetWindowThreadProcessId@user32.dll stdcall';
function IsWindow(hWnd: HWND): BOOL;
external 'IsWindow@user32.dll stdcall';
function PostMessageW(hWnd: HWND; Msg: UINT; wParam: WPARAM; lParam: LPARAM): BOOL;
external 'PostMessageW@user32.dll stdcall';
function OpenProcess(dwDesiredAccess: DWORD; bInheritHandle: BOOL; dwProcessId: DWORD): NativeInt;
external 'OpenProcess@kernel32.dll stdcall';
function CloseHandle(hObject: NativeInt): BOOL;
external 'CloseHandle@kernel32.dll stdcall';
function WaitForSingleObject(hHandle: NativeInt; dwMilliseconds: DWORD): DWORD;
external 'WaitForSingleObject@kernel32.dll stdcall';
function GetExitCodeProcess(hProcess: NativeInt; var lpExitCode: DWORD): BOOL;
external 'GetExitCodeProcess@kernel32.dll stdcall';
function QueryFullProcessImageNameW(hProcess: NativeInt; dwFlags: DWORD; lpExeName: String; var lpdwSize: DWORD): BOOL;
external 'QueryFullProcessImageNameW@kernel32.dll stdcall';
function ProcessIdToSessionId(dwProcessId: DWORD; var pSessionId: DWORD): BOOL;
external 'ProcessIdToSessionId@kernel32.dll stdcall';
function GetCurrentProcessId: DWORD;
external 'GetCurrentProcessId@kernel32.dll stdcall';
function CreateFileW(lpFileName: String; dwDesiredAccess: DWORD; dwShareMode: DWORD; lpSecurityAttributes: NativeInt; dwCreationDisposition: DWORD; dwFlagsAndAttributes: DWORD; hTemplateFile: NativeInt): NativeInt;
external 'CreateFileW@kernel32.dll stdcall';
function GetLastError: DWORD;
external 'GetLastError@kernel32.dll stdcall';

type
  TLcResult = record
    Found: Integer;       // windows of the tray class seen
    Verified: Integer;    // ... owned by {app}\memocampus-tray.exe in this session
    LeftAlone: Integer;   // ... owned by anything else (a development tray, another copy): not touched
    Closed: Integer;      // verified trays that ended within the wait
    TimedOut: Integer;    // verified trays still running when the wait ran out
    Unclosable: Integer;  // verified trays the close request could not be posted to
    Incomplete: Integer;  // verified trays that ended with the "quit incomplete" exit code (7)
    Report: String;
  end;

var
  LcWindows: array of HWND;
  LcWindowCount: Integer;
  // Test hook, always 0 in the product (only lifecycle-selftest.iss sets it): 2 = deleting the Run value fails.
  // (The registration step's failure is injected with NmhFailAt = 5, see nmh.pas.)
  LcFailAt: Integer;

function LcEnumProc(hWnd: HWND; lParam: LPARAM): BOOL;
var
  Name: String;
  Len: Integer;
begin
  SetLength(Name, 256);
  Len := GetClassNameW(hWnd, Name, 256);
  if Len > 0 then begin
    SetLength(Name, Len);
    if Name = LcTrayClass then begin
      SetArrayLength(LcWindows, LcWindowCount + 1);
      LcWindows[LcWindowCount] := hWnd;
      LcWindowCount := LcWindowCount + 1;
    end;
  end;
  Result := True;
end;

procedure LcListTrayWindows;
begin
  LcWindowCount := 0;
  SetArrayLength(LcWindows, 0);
  EnumWindows(CreateCallback(@LcEnumProc), 0);
end;

function LcImagePath(Proc: NativeInt): String;
var
  Buf: String;
  Size: DWORD;
begin
  Result := '';
  SetLength(Buf, 1024);
  Size := 1024;
  if QueryFullProcessImageNameW(Proc, 0, Buf, Size) then begin
    SetLength(Buf, Size);
    Result := Buf;
  end;
end;

// True only when the window belongs to a process that runs exactly TrayExe, in this session. On True,
// Proc is an open handle to that very process (the caller closes it): the handle pins the process, so
// the id cannot be reused by another program while it is held, and the wait later is on this process.
function LcVerifyOwner(Wnd: HWND; const TrayExe: String; var Proc: NativeInt; var Pid: DWORD; var Why: String): Boolean;
var
  Pid2, Mine, Theirs: DWORD;
  Path: String;
  Alive: Boolean;
begin
  Result := False;
  Proc := 0;
  Pid := 0;
  Why := '';
  GetWindowThreadProcessId(Wnd, Pid);
  if Pid = 0 then begin
    Why := 'the window has no owner';
    Exit;
  end;
  Proc := OpenProcess(LcProcessQueryLimitedInformation or LcSynchronize, False, Pid);
  if Proc = 0 then begin
    Why := 'the owning process cannot be opened (pid ' + IntToStr(Pid) + ')';
    Exit;
  end;
  Path := LcImagePath(Proc);
  if not NmhSamePath(Path, TrayExe) then begin
    Why := 'runs another executable (pid ' + IntToStr(Pid) + '): ' + Path;
    CloseHandle(Proc);
    Proc := 0;
    Exit;
  end;
  Mine := 0;
  Theirs := 1;
  if ProcessIdToSessionId(GetCurrentProcessId, Mine) then
    if not ProcessIdToSessionId(Pid, Theirs) then Theirs := Mine + 1;
  if Mine <> Theirs then begin
    Why := 'another session (pid ' + IntToStr(Pid) + ')';
    CloseHandle(Proc);
    Proc := 0;
    Exit;
  end;
  // The window must still belong to the process whose handle we hold (a window can end, and a handle
  // value can be reused, between the first query and the open).
  Pid2 := 0;
  Alive := IsWindow(Wnd);
  if Alive then GetWindowThreadProcessId(Wnd, Pid2);
  if not Alive or (Pid2 <> Pid) then begin
    Why := 'the window changed owner or ended (pid ' + IntToStr(Pid) + ')';
    CloseHandle(Proc);
    Proc := 0;
    Exit;
  end;
  Result := True;
end;

// Lists, without changing anything, what LcCloseTrays would act on.
function LcFindTrays(const TrayExe: String): TLcResult;
var
  I: Integer;
  Proc: NativeInt;
  Pid: DWORD;
  Why: String;
begin
  Result.Found := 0; Result.Verified := 0; Result.LeftAlone := 0; Result.Closed := 0;
  Result.TimedOut := 0; Result.Unclosable := 0; Result.Incomplete := 0; Result.Report := '';
  LcListTrayWindows;
  Result.Found := LcWindowCount;
  for I := 0 to LcWindowCount - 1 do
    if LcVerifyOwner(LcWindows[I], TrayExe, Proc, Pid, Why) then begin
      Result.Verified := Result.Verified + 1;
      Result.Report := Result.Report + 'tray ' + IntToStr(Pid) + ': ours'#13#10;
      CloseHandle(Proc);
    end else begin
      Result.LeftAlone := Result.LeftAlone + 1;
      Result.Report := Result.Report + 'tray window left alone: ' + Why + #13#10;
    end;
end;

// Asks every verified product tray to quit (WM_CLOSE: the same orderly quit as the menu's Quit) and
// waits at most WaitMs for each process to end. Never terminates anything.
function LcCloseTrays(const TrayExe: String; WaitMs: Integer): TLcResult;
var
  I, N, Pending, Spin, Spins: Integer;
  Procs: array of NativeInt;
  Pids: array of DWORD;
  Wnds: array of HWND;
  Done: array of Boolean;
  Posted: array of Boolean;
  Proc: NativeInt;
  Pid, Code: DWORD;
  Why: String;
  Alive: Boolean;
begin
  Result.Found := 0; Result.Verified := 0; Result.LeftAlone := 0; Result.Closed := 0;
  Result.TimedOut := 0; Result.Unclosable := 0; Result.Incomplete := 0; Result.Report := '';
  LcListTrayWindows;
  Result.Found := LcWindowCount;
  N := 0;
  for I := 0 to LcWindowCount - 1 do
    if LcVerifyOwner(LcWindows[I], TrayExe, Proc, Pid, Why) then begin
      SetArrayLength(Procs, N + 1);
      SetArrayLength(Pids, N + 1);
      SetArrayLength(Wnds, N + 1);
      SetArrayLength(Done, N + 1);
      SetArrayLength(Posted, N + 1);
      Procs[N] := Proc;
      Pids[N] := Pid;
      Wnds[N] := LcWindows[I];
      Done[N] := False;
      Posted[N] := False;
      N := N + 1;
    end else begin
      Result.LeftAlone := Result.LeftAlone + 1;
      Result.Report := Result.Report + 'tray window left alone: ' + Why + #13#10;
    end;
  Result.Verified := N;
  try
    // The request. Verified again right before it is sent.
    for I := 0 to N - 1 do begin
      if WaitForSingleObject(Procs[I], 0) = 0 then
        Done[I] := True
      else begin
        Alive := IsWindow(Wnds[I]);
        if Alive then Alive := PostMessageW(Wnds[I], LcWmClose, 0, 0);
        if Alive then begin
          Posted[I] := True;
          Result.Report := Result.Report + 'tray ' + IntToStr(Pids[I]) + ': quit requested'#13#10;
        end else begin
          Result.Unclosable := Result.Unclosable + 1;
          Result.Report := Result.Report + 'tray ' + IntToStr(Pids[I]) + ': the quit request could not be posted'#13#10;
          Done[I] := True;
        end;
      end;
    end;
    // The wait: bounded, in 100 ms slices (the installer window does not repaint meanwhile; Pascal Script
    // has no message pump, and a normal tray quit takes a second or two).
    Spins := (WaitMs div 100) + 1;
    for Spin := 1 to Spins do begin
      Pending := 0;
      for I := 0 to N - 1 do
        if not Done[I] then begin
          if WaitForSingleObject(Procs[I], 0) = 0 then Done[I] := True else Pending := Pending + 1;
        end;
      if Pending = 0 then Break;
      Sleep(100);
    end;
    for I := 0 to N - 1 do
      if WaitForSingleObject(Procs[I], 0) = 0 then begin
        Code := 0;
        GetExitCodeProcess(Procs[I], Code);
        if Code = LcExitQuitIncomplete then begin
          Result.Incomplete := Result.Incomplete + 1;
          Result.Report := Result.Report + 'tray ' + IntToStr(Pids[I]) + ': ended, but its quit was incomplete (exit 7)'#13#10;
        end else
          Result.Report := Result.Report + 'tray ' + IntToStr(Pids[I]) + ': ended (exit ' + IntToStr(Code) + ')'#13#10;
        Result.Closed := Result.Closed + 1;
      end else if Posted[I] then begin
        Result.TimedOut := Result.TimedOut + 1;
        Result.Report := Result.Report + 'tray ' + IntToStr(Pids[I]) + ': still running after ' + IntToStr(WaitMs) + ' ms'#13#10;
      end;
  finally
    for I := 0 to N - 1 do CloseHandle(Procs[I]);
  end;
end;

// Installed executables that another process still holds open, found by opening each for writing with
// no sharing (OPEN_EXISTING: nothing is created, truncated or changed). Retries cover the moments
// right after a tray quit, while the server it owned is still ending. Returns the names, or ''.
function LcLockedFiles(const AppDir: String; Tries: Integer): String;
var
  I, T: Integer;
  Names: array of String;
  H: NativeInt;
  Path: String;
  Locked: Boolean;
begin
  Result := '';
  SetArrayLength(Names, 3);
  Names[0] := 'memocampus-tray.exe';
  Names[1] := 'memocampus-mcp.exe';
  Names[2] := 'memocampus-native-host.exe';
  for I := 0 to 2 do begin
    Path := AppDir + '\' + Names[I];
    if not FileExists(Path) then Continue;
    Locked := True;
    for T := 1 to Tries do begin
      H := CreateFileW(Path, $40000000, 0, 0, 3, $80, 0);
      if H <> -1 then begin
        CloseHandle(H);
        Locked := False;
        Break;
      end;
      Sleep(250);
    end;
    if Locked then Result := Result + '  ' + Names[I] + #13#10;
  end;
end;

// ---- HKCU Run value, owned by the tray --------------------------------------------------------------------

// The Run value is this installation's own only when it is a string that, with at most one pair of
// surrounding quotes removed, names exactly TrayExe (the tray writes the quoted path and no arguments).
// Anything else (another path, arguments, an odd format, another type) is not ours.
function LcRunValueIsOurs(const RunKeyPath, ValueName, TrayExe: String; var Why: String): Boolean;
var
  V: String;
begin
  Result := False;
  Why := '';
  if not RegValueExists(HKCU, RunKeyPath, ValueName) then begin
    Why := 'absent';
    Exit;
  end;
  if not RegQueryStringValue(HKCU, RunKeyPath, ValueName, V) then begin
    Why := 'not a string value';
    Exit;
  end;
  V := Trim(V);
  if (Length(V) >= 2) and (V[1] = '"') and (V[Length(V)] = '"') then V := Copy(V, 2, Length(V) - 2);
  if Pos('"', V) > 0 then begin
    Why := 'unexpected quoting: ' + V;
    Exit;
  end;
  if Copy(V, 1, 4) = '\\?\' then V := Copy(V, 5, Length(V));
  if not NmhSamePath(V, TrayExe) then begin
    Why := 'starts another program: ' + V;
    Exit;
  end;
  Result := True;
end;

// Removes the Run value only if it is ours, checked again immediately before the delete.
// 0 removed, 1 kept (absent or not ours), 2 failed and still ours.
function LcRemoveRunValue(const RunKeyPath, ValueName, TrayExe: String; var Report: String): Integer;
var
  Why: String;
begin
  Result := 1;
  if not LcRunValueIsOurs(RunKeyPath, ValueName, TrayExe, Why) then begin
    if Why = 'absent' then
      Report := 'logon start (Run value): nothing to remove'
    else
      Report := 'logon start (Run value): KEPT (' + Why + ')';
    Exit;
  end;
  if not LcRunValueIsOurs(RunKeyPath, ValueName, TrayExe, Why) then begin
    Report := 'logon start (Run value): KEPT (it changed just before removal)';
    Exit;
  end;
  if (LcFailAt = 2) or not RegDeleteValue(HKCU, RunKeyPath, ValueName) then begin
    Result := 2;
    Report := 'logon start (Run value): FAILED to remove our value (it still starts the tray)';
    Exit;
  end;
  Result := 0;
  Report := 'logon start (Run value): removed our value';
end;

// ---- The uninstall cleanup, in the order that keeps a failed run retryable ---------------------------------
// 1 our Native Messaging registration, 2 our Run value, 3 our own manifest (only when the registration
// is no longer ours). Every step is idempotent and checks ownership itself. False means a step failed
// while its target is still ours: the caller must stop the uninstall (nothing else was removed yet), and
// running it again repeats the steps that remain. Report lists what was removed and what was KEPT.
function LcUninstallCleanup(const NmhKeyPath, ManifestPath, HostName, ExePath, ExtensionId, RunKeyPath, TrayExe: String; var Report: String): Boolean;
var
  Line: String;
  Code: Integer;
begin
  Result := False;
  Report := '';
  Code := NmhUnregister(NmhKeyPath, ManifestPath, Line);
  Report := Report + Line + #13#10;
  if Code = 2 then Exit;
  Code := LcRemoveRunValue(RunKeyPath, LcRunValueName, TrayExe, Line);
  Report := Report + Line + #13#10;
  if Code = 2 then Exit;
  if NmhOwnsRegistration(NmhKeyPath, ManifestPath) then
    Report := Report + 'manifest: KEPT (the registration still points at it)'#13#10
  else begin
    Code := NmhRemoveOwnManifest(ManifestPath, HostName, ExePath, ExtensionId, Line);
    Report := Report + Line + #13#10;
    // A manifest that cannot be deleted is reported, but the uninstall can go on: nothing points at it.
  end;
  Result := True;
end;
