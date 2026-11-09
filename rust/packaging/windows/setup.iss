; hushkey-setup-windows-x86_64.exe: the first download on Windows.
;
; The program is ~58 MB, nearly all of it Vulkan shaders, which LZMA2 packs
; to ~6 MB. This setup only carries it compressed: it unpacks hushkey.exe to
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

[Run]
; --install copies the program into %LOCALAPPDATA%\Programs\hushkey and
; starts it; the temporary copy is removed with {tmp} afterwards.
Filename: "{tmp}\hushkey.exe"; Parameters: "--install"; StatusMsg: "Installing hushkey …"; Flags: waituntilterminated
