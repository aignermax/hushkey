; hushkey Windows installer — per-user, no admin rights needed.
; Compiled by the release workflow: HUSHKEY_VERSION comes from the tag.
#ifndef MyAppVersion
  #define MyAppVersion GetEnv("HUSHKEY_VERSION")
  #if MyAppVersion == ""
    #define MyAppVersion "0.0.0-dev"
  #endif
#endif

[Setup]
AppId={{7F3A9C2E-4B6D-4E1A-9C5F-2D8E6A1B3F47}
AppName=hushkey
AppVersion={#MyAppVersion}
AppVerName=hushkey {#MyAppVersion}
AppPublisher=aignermax
AppPublisherURL=https://github.com/aignermax/hushkey
DefaultDirName={localappdata}\Programs\hushkey
PrivilegesRequired=lowest
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
; stop tray + daemon, drop the autostart entry, before files are removed
Filename: "powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\uninstall.ps1"""; Flags: runhidden waituntildone

[Code]
function HavePython(): Boolean;
var
  ResultCode: Integer;
begin
  Result := Exec('cmd.exe', '/c python --version >nul 2>&1', '', SW_HIDE,
                 ewWaitUntilTerminated, ResultCode) and (ResultCode = 0);
  if not Result then
    Result := Exec('cmd.exe', '/c py -3 --version >nul 2>&1', '', SW_HIDE,
                   ewWaitUntilTerminated, ResultCode) and (ResultCode = 0);
  if not Result then
    // just bootstrapped by us, PATH of this process is stale
    Result := FileExists(ExpandConstant(
                '{localappdata}\Programs\Python\Python312\python.exe'));
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
  // fallback: official per-user installer from python.org
  Installer := ExpandConstant('{tmp}\python-3.12.10-amd64.exe');
  if Exec('cmd.exe', '/c curl -fsSL -o "' + Installer + '" https://www.python.org/ftp/python/3.12.10/python-3.12.10-amd64.exe',
          '', SW_HIDE, ewWaitUntilTerminated, ResultCode) and (ResultCode = 0) then
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
      if not Exec('powershell.exe',
                  '-NoProfile -ExecutionPolicy Bypass -File "' + ExpandConstant('{app}') + '\install.ps1"',
                  ExpandConstant('{app}'), SW_HIDE, ewWaitUntilTerminated, ResultCode)
         or (ResultCode <> 0) then
        MsgBox('hushkey setup failed. Run install.ps1 in ' + ExpandConstant('{app}')
               + ' to see the error.', mbError, MB_OK);
    end;
  end;
end;
