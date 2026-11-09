; hushkey-setup-windows-x86_64.exe: the first download on Windows.
;
; The program is ~12 MB, mostly Vulkan shaders, which LZMA2 packs to
; ~3 MB. This setup only carries it compressed: it unpacks hushkey.exe to
; a temporary folder and runs `hushkey.exe --install`, so the program still
; installs itself (Start Menu, Settings > Apps entry with its own uninstall,
; autostart) exactly as when the plain .exe is opened. No uninstaller of its
; own, no admin rights.
;
; Built by .github/workflows/release.yml:
;   ISCC /DVersion=<version> /DFileVersion=<a.b.c.0> /DSource=<hushkey.exe>
;        /DIcon=<hushkey.ico> /O<out dir> setup.iss
; (the icon is the one build.rs makes for the program, in its OUT_DIR)

#ifndef Version
  #define Version "0.0.0"
#endif
; Windows' file version is four numbers: 0.11.3-rc.1 is 0.11.3.0.
#ifndef FileVersion
  #define FileVersion "0.0.0.0"
#endif
#ifndef Source
  #error Pass /DSource=<path to hushkey.exe>
#endif
#ifndef Icon
  #error Pass /DIcon=<path to hushkey.ico>
#endif

[Setup]
AppId=hushkey-setup
AppName=hushkey
AppVersion={#Version}
AppPublisher=hushkey
AppPublisherURL=https://hushkey.eu
VersionInfoVersion={#FileVersion}
OutputBaseFilename=hushkey-setup-windows-x86_64
SetupIconFile={#Icon}
Compression=lzma2/ultra64
SolidCompression=yes
LZMAUseSeparateProcess=yes
; The program installs itself and owns its Settings > Apps entry.
CreateAppDir=no
Uninstallable=no
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
; One click: nothing to choose, so no pages beyond progress.
DisableWelcomePage=yes
DisableReadyPage=yes
DisableFinishedPage=yes
DisableProgramGroupPage=yes
ShowLanguageDialog=no
WizardStyle=modern

[Files]
Source: "{#Source}"; DestDir: "{tmp}"; DestName: "hushkey.exe"; Flags: ignoreversion

[Code]
// --install copies the program into %LOCALAPPDATA%\Programs\hushkey and
// starts it; the temporary copy goes with {tmp} afterwards. Run from here
// rather than [Run], whose entries ignore exit codes: a failed install
// must fail the setup (silent and managed installs only see the code).
var
  InstallResult: Integer;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
  begin
    WizardForm.StatusLabel.Caption := 'Installing hushkey …';
    if not Exec(ExpandConstant('{tmp}\hushkey.exe'), '--install', '', SW_SHOWNORMAL,
                ewWaitUntilTerminated, InstallResult) then
      InstallResult := -1;
    if InstallResult <> 0 then
      SuppressibleMsgBox('hushkey could not be installed (code ' + IntToStr(InstallResult)
        + '). The log is in %LOCALAPPDATA%\hushkey-rs.', mbError, MB_OK, IDOK);
  end;
end;

function GetCustomSetupExitCode: Integer;
begin
  if InstallResult <> 0 then
    Result := 1
  else
    Result := 0;
end;
