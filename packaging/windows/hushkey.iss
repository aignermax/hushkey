; hushkey Windows installer — per-user, no admin rights needed.
; Compiled by the release workflow: HUSHKEY_VERSION comes from the tag.
#ifndef MyAppVersion
  #define MyAppVersion GetEnv("HUSHKEY_VERSION")
  #if MyAppVersion == ""
    #define MyAppVersion "0.0.0-dev"
  #endif
#endif
; SHA-256 of https://www.python.org/ftp/python/3.12.10/python-3.12.10-amd64.exe
#define PythonSha256 "67b5635e80ea51072b87941312d00ec8927c4db9ba18938f7ad2d27b328b95fb"

[Setup]
AppId={{7F3A9C2E-4B6D-4E1A-9C5F-2D8E6A1B3F47}
AppName=hushkey
AppVersion={#MyAppVersion}
AppVerName=hushkey {#MyAppVersion}
AppPublisher=aignermax
AppPublisherURL=https://github.com/aignermax/hushkey
DefaultDirName={localappdata}\Programs\hushkey
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
OutputDir=dist
OutputBaseFilename=hushkey-setup-{#MyAppVersion}
SetupIconFile=logo.ico
UninstallDisplayIcon={app}\logo.ico
Compression=lzma2
WizardStyle=modern
; the app is a background daemon — nothing to launch from the wizard
DisableProgramGroupPage=yes

[Files]
Source: "..\..\dictate.py"; DestDir: "{app}"
Source: "..\..\tray.py"; DestDir: "{app}"
Source: "..\..\recorder.py"; DestDir: "{app}"
Source: "..\..\transcribe.py"; DestDir: "{app}"
Source: "..\..\install.ps1"; DestDir: "{app}"
Source: "..\..\uninstall.ps1"; DestDir: "{app}"
Source: "..\..\requirements.txt"; DestDir: "{app}"
Source: "..\..\requirements-gpu.txt"; DestDir: "{app}"
Source: "..\..\README.md"; DestDir: "{app}"
Source: "..\..\assets\logo.png"; DestDir: "{app}\assets"
Source: "logo.ico"; DestDir: "{app}"

[UninstallRun]
; stop tray + daemon, drop the autostart entry AND the venv (-Purge),
; before Inno removes the payload files
Filename: "powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\uninstall.ps1"" -Purge"; Flags: runhidden waituntilterminated

[Code]
function HavePython(): Boolean;
var
  ResultCode: Integer;
begin
  // version-aware: an ancient python on PATH must not suppress the bootstrap
  Result := Exec('cmd.exe',
    '/c python -c "import sys; sys.exit(0 if sys.version_info >= (3, 10) else 1)" >nul 2>&1',
    '', SW_HIDE, ewWaitUntilTerminated, ResultCode) and (ResultCode = 0);
  if not Result then
    Result := Exec('cmd.exe',
      '/c py -3 -c "import sys; sys.exit(0 if sys.version_info >= (3, 10) else 1)" >nul 2>&1',
      '', SW_HIDE, ewWaitUntilTerminated, ResultCode) and (ResultCode = 0);
  if not Result then
    // just bootstrapped by us — PATH of this process is stale
    Result := FileExists(ExpandConstant(
                '{localappdata}\Programs\Python\Python312\python.exe'));
end;

function VerifySha256(const FileName, Expected: string): Boolean;
var
  OutFile, Clean, S, C: string;
  Output: AnsiString;
  ResultCode, I: Integer;
begin
  OutFile := ExpandConstant('{tmp}\pyhash.txt');
  Exec('cmd.exe', '/c certutil -hashfile "' + FileName + '" SHA256 > "' + OutFile + '"',
       '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  if not LoadStringFromFile(OutFile, Output) then
  begin
    Result := False;
    exit;
  end;
  S := string(Output);
  Clean := '';
  for I := 1 to Length(S) do
  begin
    C := Lowercase(Copy(S, I, 1));
    if Pos(C, '0123456789abcdef') > 0 then
      Clean := Clean + C;
  end;
  Result := Pos(Lowercase(Expected), Clean) > 0;
end;

procedure InstallPython();
var
  ResultCode: Integer;
  Installer: string;
begin
  // winget ships with Windows 11 / current Windows 10 (App Installer)
  if Exec('cmd.exe', '/c winget install -e --id Python.Python.3.12 --scope user --silent --accept-source-agreements --accept-package-agreements >nul 2>&1',
          '', SW_HIDE, ewWaitUntilTerminated, ResultCode) and (ResultCode = 0) then
    exit;
  // fallback: official per-user installer from python.org, checksum-pinned
  Installer := ExpandConstant('{tmp}\python-3.12.10-amd64.exe');
  if not (Exec('cmd.exe', '/c curl -fsSL -o "' + Installer + '" https://www.python.org/ftp/python/3.12.10/python-3.12.10-amd64.exe',
          '', SW_HIDE, ewWaitUntilTerminated, ResultCode) and (ResultCode = 0)) then
    exit;
  if not VerifySha256(Installer, '{#PythonSha256}') then
  begin
    DeleteFile(Installer);
    MsgBox('The downloaded Python installer failed its checksum — ' +
           'aborting the automatic Python install.', mbError, MB_OK);
    exit;
  end;
  Exec(Installer, '/quiet InstallAllUsers=0 PrependPath=1 Include_test=0',
       '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  ResultCode: Integer;
begin
  if CurStep = ssPostInstall then
  begin
    if not HavePython() then
    begin
      WizardForm.StatusLabel.Caption := 'Installing Python 3.12 ...';
      InstallPython();
    end;
    if not HavePython() then
      MsgBox('Python 3.12 could not be installed automatically.' + #13#10 +
             'Install it from https://www.python.org/downloads/ and then run install.ps1 in '
             + ExpandConstant('{app}'), mbError, MB_OK)
    else
    begin
      WizardForm.StatusLabel.Caption := 'Setting up hushkey (venv + dependencies + autostart) ...';
      // visible console on purpose: pip can take minutes, a hidden window
      // looks like a frozen installer
      if not Exec('powershell.exe',
                  '-NoProfile -ExecutionPolicy Bypass -File "' + ExpandConstant('{app}') + '\install.ps1"',
                  ExpandConstant('{app}'), SW_SHOWNORMAL, ewWaitUntilTerminated, ResultCode)
         or (ResultCode <> 0) then
        MsgBox('hushkey setup failed. Run install.ps1 in ' + ExpandConstant('{app}')
               + ' to see the error.', mbError, MB_OK);
    end;
  end;
end;
