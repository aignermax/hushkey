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
DefaultDirName={localappdata}\Programs\whisper-ptt
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
Source: "..\..\update_helper.py"; DestDir: "{app}"
Source: "..\..\recorder.py"; DestDir: "{app}"
Source: "..\..\transcribe.py"; DestDir: "{app}"
Source: "..\..\whisper_cpp.py"; DestDir: "{app}"
Source: "..\..\setup_vulkan.py"; DestDir: "{app}"
Source: "..\..\acceleration.py"; DestDir: "{app}"
Source: "..\..\setup_acceleration.py"; DestDir: "{app}"
Source: "..\..\native\*"; DestDir: "{app}\native"; Flags: recursesubdirs createallsubdirs
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
procedure CurStepChanged(CurStep: TSetupStep);
var
  ResultCode: Integer;
begin
  if CurStep = ssPostInstall then
  begin
    WizardForm.StatusLabel.Caption := 'Setting up hushkey and automatic acceleration ...';
    // install.ps1 owns Python discovery and the SHA256-verified bootstrap.
    if not Exec('powershell.exe',
                '-NoProfile -ExecutionPolicy Bypass -File "' + ExpandConstant('{app}') + '\install.ps1"',
                ExpandConstant('{app}'), SW_HIDE, ewWaitUntilTerminated, ResultCode)
       or (ResultCode <> 0) then
      RaiseException('hushkey setup failed. Check the installation log and retry.');
  end;
end;
