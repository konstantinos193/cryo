; cryo installer — per-user, no admin. Version comes from CI:
;   ISCC.exe /DAppVersion=x.y.z installer\cryo.iss
#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

[Setup]
; Stable identity — upgrades detect and replace installs under this id.
AppId={{9B3E5A7C-4D2F-4E1A-B8C6-3F7A9E2D5C1B}
AppName=cryo
AppVersion={#AppVersion}
AppVerName=cryo {#AppVersion}
AppPublisher=konstantinos193
AppPublisherURL=https://github.com/konstantinos193/cryo
AppSupportURL=https://github.com/konstantinos193/cryo/issues
AppCopyright=MIT License
; Version info on cryo-setup.exe itself — unsigned files with no
; VERSIONINFO get a harder SmartScreen look.
VersionInfoCompany=konstantinos193
VersionInfoDescription=cryo browser installer
VersionInfoProductName=cryo
VersionInfoVersion={#AppVersion}
UninstallDisplayName=cryo
DefaultDirName={localappdata}\Programs\cryo
PrivilegesRequired=lowest
; Minimal wizard: destination dir picker, tasks, ready — no welcome
; or program-group pages. Installs default to %LOCALAPPDATA%\Programs
; but any user-writable path works (other drives included).
DisableWelcomePage=yes
DisableProgramGroupPage=yes
OutputDir=output
OutputBaseFilename=cryo-setup
SetupIconFile=..\assets\icon.ico
UninstallDisplayIcon={app}\cryo.exe
; Wizard art — regenerate with installer\gen-images.ps1. Each slot takes
; a comma list of DPI variants; Inno picks the closest to the scaling.
WizardImageFile=images\wizard.bmp,images\wizard@150.bmp,images\wizard@200.bmp
WizardSmallImageFile=images\wizard-small.bmp,images\wizard-small@150.bmp,images\wizard-small@200.bmp
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
; Silent updates: close a running cryo via Restart Manager and let the
; [Run] entry bring it back up on the new build.
CloseApplications=yes
RestartApplications=yes

[Files]
Source: "..\target\release\cryo.exe"; DestDir: "{app}"; Flags: ignoreversion

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Additional shortcuts:"; Flags: unchecked

[Icons]
Name: "{autoprograms}\cryo"; Filename: "{app}\cryo.exe"
Name: "{autodesktop}\cryo"; Filename: "{app}\cryo.exe"; Tasks: desktopicon

[Run]
; Relaunch after every install — silent update or first run alike.
Filename: "{app}\cryo.exe"; Flags: nowait runascurrentuser
