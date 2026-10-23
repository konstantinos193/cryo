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
DefaultDirName={localappdata}\Programs\cryo
PrivilegesRequired=lowest
; Skip every wizard page we can — install is one Next and done.
DisableWelcomePage=yes
DisableDirPage=yes
DisableProgramGroupPage=yes
OutputDir=output
OutputBaseFilename=cryo-setup
SetupIconFile=..\assets\icon.ico
UninstallDisplayIcon={app}\cryo.exe
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
