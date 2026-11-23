// Native Messaging host registration for the Memocampus installer (Issue #67, Slice 3B-2).
// Included into the [Code] section of memocampus.iss (and of nmh-selftest.iss, which exercises exactly
// this code against a private registry key).
//
// What it owns: ONE registry key, whose path the caller passes (in the product that is
// HKCU\Software\Google\Chrome\NativeMessagingHosts\io.github.memocampus.vault), the manifest file the
// caller names (inside the install folder), and that file's temporary and backup copies next to it.
// It reads no other key, writes no other key, and never touches HKLM. It has no function that deletes a
// key with its subkeys.
//
// The rules, in short:
//   - A registration whose default value is this installation's own manifest path may be rewritten.
//   - A key that is absent may be created.
//   - Anything else (another path, no default value, a value of another type) is not ours: refuse.
//   - A step that fails is undone with the exact previous values; the undo re-checks that the value is
//     still ours and never removes what another program wrote meanwhile. If the undo itself fails, the
//     error says so (ROLLBACK_FAILED) instead of reporting success.

type
  TNmhSnapshot = record
    KeyExisted: Boolean;
    HadValue: Boolean;
    Value: String;
    ManifestExisted: Boolean;
    ManifestRaw: AnsiString;
  end;

var
  // Test hooks. Always 0 / False in the product: only nmh-selftest.iss sets them.
  //   1 fail after the temporary manifest is written   2 fail after the manifest is in place
  //   3 fail after the registry value is written       4 treat the registry read-back as a mismatch
  //   5 deleting the registration's default value fails (NmhUnregister)
  NmhFailAt: Integer;
  NmhFailRollback: Boolean;
  // Set by the installer only around the registration that runs before the files are copied (the host
  // executable is not in place yet then). Always False otherwise: the default requires the executable.
  NmhExeMayBeMissing: Boolean;

// A text of the active language (installer\i18n\*.isl, [CustomMessages]), with %1..%9 filled in and %n as
// a line break. Only texts shown to the user come from here; the diagnostic details (Err, Report) and the
// identifiers in them (ROLLBACK_FAILED, KEPT, FAILED) are the same in every language. Issue #75.
function MsgText(const Name: String; const Args: array of String): String;
begin
  Result := FmtMessage(CustomMessage(Name), Args);
end;

function NmhIsValidExtensionId(const Id: String): Boolean;
var
  I, C: Integer;
begin
  // Chrome extension ids: 32 characters, a to p, lower case.
  Result := Length(Id) = 32;
  if Result then
    for I := 1 to 32 do begin
      C := Ord(Id[I]);
      if (C < 97) or (C > 112) then begin
        Result := False;
        Break;
      end;
    end;
end;

function NmhHex4(Value: Integer): String;
var
  Digits: String;
begin
  Digits := '0123456789abcdef';
  Result := Copy(Digits, ((Value div 4096) mod 16) + 1, 1) + Copy(Digits, ((Value div 256) mod 16) + 1, 1) +
    Copy(Digits, ((Value div 16) mod 16) + 1, 1) + Copy(Digits, (Value mod 16) + 1, 1);
end;

// A JSON string literal. Backslash, quote and control characters are escaped; everything else
// (including non-ASCII) is kept and written out as UTF-8 by the file writer.
function NmhJsonString(const S: String): String;
var
  I, C: Integer;
begin
  Result := '"';
  for I := 1 to Length(S) do begin
    C := Ord(S[I]);
    if C = 34 then
      Result := Result + '\"'
    else if C = 92 then
      Result := Result + '\\'
    else if C < 32 then
      Result := Result + '\u' + NmhHex4(C)
    else
      Result := Result + S[I];
  end;
  Result := Result + '"';
end;

// An absolute local path to an .exe, without characters that cannot be in a Windows path.
function NmhIsSafeExePath(const Path: String): Boolean;
var
  I, C: Integer;
  Drive: Integer;
begin
  Result := (Length(Path) >= 7) and (Copy(Path, 2, 2) = ':\') and (Lowercase(Copy(Path, Length(Path) - 3, 4)) = '.exe');
  if not Result then Exit;
  Drive := Ord(Lowercase(Copy(Path, 1, 1))[1]);
  if (Drive < 97) or (Drive > 122) then begin
    Result := False;
    Exit;
  end;
  for I := 1 to Length(Path) do begin
    C := Ord(Path[I]);
    if (C < 32) or (C = 34) or (C = 42) or (C = 60) or (C = 62) or (C = 63) or (C = 124) then begin
      Result := False;
      Break;
    end;
  end;
end;

// The manifest, one entry per line. Written as UTF-8 without a byte order mark.
function NmhManifestLines(const HostName, ExePath, ExtensionId: String): TArrayOfString;
begin
  SetArrayLength(Result, 9);
  Result[0] := '{';
  Result[1] := '  "name": ' + NmhJsonString(HostName) + ',';
  Result[2] := '  "description": "Memocampus Native Messaging Host",';
  Result[3] := '  "path": ' + NmhJsonString(ExePath) + ',';
  Result[4] := '  "type": "stdio",';
  Result[5] := '  "allowed_origins": [';
  Result[6] := '    ' + NmhJsonString('chrome-extension://' + ExtensionId + '/');
  Result[7] := '  ]';
  Result[8] := '}';
end;

// What the file must contain: the lines, each ended by CR LF.
function NmhManifestText(const Lines: TArrayOfString): String;
var
  I: Integer;
begin
  Result := '';
  for I := 0 to GetArrayLength(Lines) - 1 do
    Result := Result + Lines[I] + #13#10;
end;

// The bytes of a file (empty when it cannot be read).
function NmhRawOf(const Path: String): AnsiString;
begin
  if not FileExists(Path) or not LoadStringFromFile(Path, Result) then Result := '';
end;

// UTF-8 bytes to text (what a manifest file holds, as Chrome reads it). A malformed sequence becomes
// U+FFFD, which never equals what was written.
function NmhUtf8Decode(const Raw: AnsiString): String;
var
  I, N, B, C, Extra, K: Integer;
  Bad: Boolean;
begin
  Result := '';
  I := 1;
  N := Length(Raw);
  while I <= N do begin
    B := Ord(Raw[I]);
    Bad := False;
    Extra := 0;
    C := 0;
    if B < 128 then
      C := B
    else if (B and 224) = 192 then begin
      C := B and 31;
      Extra := 1;
    end else if (B and 240) = 224 then begin
      C := B and 15;
      Extra := 2;
    end else if (B and 248) = 240 then begin
      C := B and 7;
      Extra := 3;
    end else
      Bad := True;
    if I + Extra > N then Bad := True;
    if not Bad then
      for K := 1 to Extra do begin
        B := Ord(Raw[I + K]);
        if (B and 192) <> 128 then Bad := True
        else C := (C * 64) + (B and 63);
      end;
    if Bad then begin
      Result := Result + Chr(65533);
      Extra := 0;
    end else if C >= 65536 then begin
      C := C - 65536;
      Result := Result + Chr(55296 + (C div 1024)) + Chr(56320 + (C mod 1024));
    end else
      Result := Result + Chr(C);
    I := I + 1 + Extra;
  end;
end;

// Paths compare without regard to case or slash direction.
function NmhSamePath(const A, B: String): Boolean;
var
  X, Y: String;
begin
  X := A;
  Y := B;
  StringChangeEx(X, '/', '\', True);
  StringChangeEx(Y, '/', '\', True);
  Result := (Length(X) > 0) and (CompareText(X, Y) = 0);
end;

// True when the key is absent, or its default value is exactly this installation's manifest path.
// Used before anything is changed, and again inside NmhRegister.
function NmhCheckRegistration(const KeyPath, ManifestPath: String; var Why: String): Boolean;
var
  Current: String;
begin
  Why := '';
  if not RegKeyExists(HKCU, KeyPath) then begin
    Result := True;
    Exit;
  end;
  if RegQueryStringValue(HKCU, KeyPath, '', Current) and NmhSamePath(Current, ManifestPath) then begin
    Result := True;
    Exit;
  end;
  Result := False;
  // Not ours: refused (Setup will not replace or change it), with the reason in the active language.
  if RegQueryStringValue(HKCU, KeyPath, '', Current) then
    Why := MsgText('McNmhForeignRegistration', [Current])
  else
    Why := MsgText('McNmhUnknownKey', [KeyPath]);
end;

// Whether the registration is this installation's own (for a later, ownership-checked removal).
function NmhOwnsRegistration(const KeyPath, ManifestPath: String): Boolean;
var
  Current: String;
begin
  Result := RegKeyExists(HKCU, KeyPath) and RegQueryStringValue(HKCU, KeyPath, '', Current) and
    NmhSamePath(Current, ManifestPath);
end;

function NmhTake(const KeyPath, ManifestPath: String): TNmhSnapshot;
begin
  Result.KeyExisted := RegKeyExists(HKCU, KeyPath);
  Result.HadValue := Result.KeyExisted and RegQueryStringValue(HKCU, KeyPath, '', Result.Value);
  if not Result.HadValue then Result.Value := '';
  Result.ManifestExisted := FileExists(ManifestPath);
  if Result.ManifestExisted then
    Result.ManifestRaw := NmhRawOf(ManifestPath)
  else
    Result.ManifestRaw := '';
end;

procedure NmhRemoveScratch(const ManifestPath: String);
begin
  if FileExists(ManifestPath + '.tmp') then DeleteFile(ManifestPath + '.tmp');
  if FileExists(ManifestPath + '.previous') then DeleteFile(ManifestPath + '.previous');
end;

// Puts the registry and the manifest back as NmhTake saw them. Only what is still ours is touched.
function NmhRollback(const KeyPath, ManifestPath: String; const Snap: TNmhSnapshot; var Err: String): Boolean;
var
  Current: String;
  Has: Boolean;
begin
  Result := False;
  Err := '';
  try
    if NmhFailRollback then RaiseException('rollback failure injected');
    // Registry: re-check ownership first.
    Has := RegKeyExists(HKCU, KeyPath) and RegQueryStringValue(HKCU, KeyPath, '', Current);
    if Has and not NmhSamePath(Current, ManifestPath) and not (Snap.HadValue and NmhSamePath(Current, Snap.Value)) then begin
      Err := 'the registration was changed by another program during setup and was left as it is';
      Exit;
    end;
    if Snap.HadValue then begin
      if not (Has and (Current = Snap.Value)) then
        if not RegWriteStringValue(HKCU, KeyPath, '', Snap.Value) then begin
          Err := 'the previous registry value could not be restored';
          Exit;
        end;
    end else if not Snap.KeyExisted then begin
      if Has then
        if not RegDeleteValue(HKCU, KeyPath, '') then begin
          Err := 'the registry value created by setup could not be removed';
          Exit;
        end;
      if RegKeyExists(HKCU, KeyPath) then
        if not RegDeleteKeyIfEmpty(HKCU, KeyPath) then begin
          Err := 'the registry key created by setup could not be removed';
          Exit;
        end;
    end;
    // Manifest.
    if Snap.ManifestExisted then begin
      if FileExists(ManifestPath + '.previous') then
        if not FileCopy(ManifestPath + '.previous', ManifestPath, False) then begin
          Err := 'the previous manifest could not be restored';
          Exit;
        end;
      if not FileExists(ManifestPath) or (NmhRawOf(ManifestPath) <> Snap.ManifestRaw) then begin
        Err := 'the previous manifest is not back in place';
        Exit;
      end;
    end else if FileExists(ManifestPath) then
      if not DeleteFile(ManifestPath) then begin
        Err := 'the manifest created by setup could not be removed';
        Exit;
      end;
    NmhRemoveScratch(ManifestPath);
    Result := True;
  except
    Err := GetExceptionMessage;
  end;
end;

// Writes the manifest and registers it. True only when both were written and read back correctly.
// On failure the previous state is restored; Err says what failed, and ends with ROLLBACK_FAILED: ...
// when that restore did not complete (the state is then not known to be consistent).
function NmhRegister(const KeyPath, ManifestPath, HostName, ExePath, ExtensionId: String; var Err: String): Boolean;
var
  Snap: TNmhSnapshot;
  Lines: TArrayOfString;
  Why, RollbackErr, Written: String;
begin
  Result := False;
  Err := '';
  if not NmhIsValidExtensionId(ExtensionId) then begin
    Err := 'the extension id is not valid';
    Exit;
  end;
  if not NmhIsSafeExePath(ExePath) then begin
    Err := 'the host path is not an absolute local .exe path';
    Exit;
  end;
  if not NmhExeMayBeMissing and not FileExists(ExePath) then begin
    Err := 'the host executable is not in the install folder';
    Exit;
  end;
  if not NmhCheckRegistration(KeyPath, ManifestPath, Why) then begin
    Err := Why;
    Exit;
  end;
  Snap := NmhTake(KeyPath, ManifestPath);
  Lines := NmhManifestLines(HostName, ExePath, ExtensionId);
  Written := NmhManifestText(Lines);
  try
    NmhRemoveScratch(ManifestPath);
    if Snap.ManifestExisted then
      if not FileCopy(ManifestPath, ManifestPath + '.previous', False) then
        RaiseException('the existing manifest could not be backed up');
    if not SaveStringsToUTF8FileWithoutBOM(ManifestPath + '.tmp', Lines, False) then
      RaiseException('the manifest could not be written');
    if NmhUtf8Decode(NmhRawOf(ManifestPath + '.tmp')) <> Written then
      RaiseException('the manifest read back differently from what was written');
    if NmhFailAt = 1 then RaiseException('failure injected after the temporary manifest');
    if not FileCopy(ManifestPath + '.tmp', ManifestPath, False) then
      RaiseException('the manifest could not be put in place');
    if NmhUtf8Decode(NmhRawOf(ManifestPath)) <> Written then
      RaiseException('the manifest in place differs from what was written');
    if NmhFailAt = 2 then RaiseException('failure injected after the manifest');
    if not RegWriteStringValue(HKCU, KeyPath, '', ManifestPath) then
      RaiseException('the registry value could not be written');
    if NmhFailAt = 3 then RaiseException('failure injected after the registry value');
    if (NmhFailAt = 4) or not NmhOwnsRegistration(KeyPath, ManifestPath) then
      RaiseException('the registry value read back differently from what was written');
    NmhRemoveScratch(ManifestPath);
    Result := True;
  except
    Err := GetExceptionMessage;
    if not NmhRollback(KeyPath, ManifestPath, Snap, RollbackErr) then
      Err := Err + ' ROLLBACK_FAILED: ' + RollbackErr;
  end;
end;

// ---- Removal at uninstall (Slice 3B-3) ----------------------------------------------------------------
// Result codes of the removal functions: 0 removed, 1 kept (not ours, or nothing to remove), 2 failed
// and still ours (the caller must not go on as if it were gone). Report gets one line about it.

// Removes this installation's own registration: only a default value that is a string equal to this
// installation's manifest path, checked again immediately before the delete. The registry has no
// compare-and-delete, so a change by another program in that instant is not excluded. Only the default
// value is deleted; the key itself only when it is then empty (RegDeleteKeyIfEmpty never removes a key
// that still has other values or subkeys). No key is ever deleted with its subkeys.
function NmhUnregister(const KeyPath, ManifestPath: String; var Report: String): Integer;
var
  Current: String;
begin
  Result := 1;
  Report := 'native messaging registration: nothing to remove (key absent)';
  if not RegKeyExists(HKCU, KeyPath) then Exit;
  if not RegQueryStringValue(HKCU, KeyPath, '', Current) then begin
    Report := 'native messaging registration: KEPT (the key has no string default value, so it is not ours)';
    Exit;
  end;
  if not NmhSamePath(Current, ManifestPath) then begin
    Report := 'native messaging registration: KEPT (it belongs to another program: ' + Current + ')';
    Exit;
  end;
  // Immediately before the delete: still ours?
  if not NmhOwnsRegistration(KeyPath, ManifestPath) then begin
    Report := 'native messaging registration: KEPT (it changed just before removal)';
    Exit;
  end;
  if (NmhFailAt = 5) or not RegDeleteValue(HKCU, KeyPath, '') then begin
    Result := 2;
    Report := 'native messaging registration: FAILED to remove our default value (still registered)';
    Exit;
  end;
  Result := 0;
  Report := 'native messaging registration: removed our default value';
  if RegKeyExists(HKCU, KeyPath) then
    if RegDeleteKeyIfEmpty(HKCU, KeyPath) then
      Report := Report + ' and the empty key'
    else
      Report := Report + '; the key was kept (it still holds other values or subkeys)';
end;

// Removes the manifest file only when its bytes are exactly what this installer writes (built again
// from the same constants), so a file someone else put or changed there is never treated as ours just
// because it sits in the install folder. Call it only when the registration is no longer ours.
function NmhRemoveOwnManifest(const ManifestPath, HostName, ExePath, ExtensionId: String; var Report: String): Integer;
var
  Expected: String;
begin
  Result := 1;
  if not FileExists(ManifestPath) then begin
    Report := 'manifest: nothing to remove (file absent)';
    Exit;
  end;
  Expected := NmhManifestText(NmhManifestLines(HostName, ExePath, ExtensionId));
  if NmhUtf8Decode(NmhRawOf(ManifestPath)) <> Expected then begin
    Report := 'manifest: KEPT (its content is not what this installer wrote: ' + ManifestPath + ')';
    Exit;
  end;
  if not DeleteFile(ManifestPath) then begin
    Result := 2;
    Report := 'manifest: FAILED to delete our manifest ' + ManifestPath;
    Exit;
  end;
  Result := 0;
  Report := 'manifest: removed our manifest';
end;
