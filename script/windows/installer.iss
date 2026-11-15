; The Glasscode installer for Windows (Inno Setup 6.3 or later).
;
; script\bundle-windows.ps1 builds the programs and compiles this file with the defines AppVersion,
; AppBuild, StageDir (the folder target\release\Glasscode) and OutputDir.
;
; It installs for the current user, without administrator rights, into
; %LOCALAPPDATA%\Programs\Glasscode, with a Start menu entry and an entry in "Installed apps".
; Glasscode keeps its data in %USERPROFILE%\.glasscode, which is not part of the installation: an
; upgrade keeps it and an uninstall leaves it.
;
; The installer is unsigned unless the bundle script has a certificate (SIGN_CERT); Windows
; SmartScreen warns before an unsigned installer runs. The uninstaller is never signed.

#ifndef AppVersion
  #error Compile this file with script\bundle-windows.ps1, which defines AppVersion, AppBuild, StageDir and OutputDir.
#endif
#if Ver < EncodeVer(6, 3, 0)
  #error Inno Setup 6.3 or later is needed.
#endif

[Setup]
; Upgrades and the entry in "Installed apps" find the installation by this id: never change it.
AppId={{6F41BEC8-88F7-46B2-AFDC-1EB89617F216}
AppName=Glasscode
AppVersion={#AppVersion}
AppVerName=Glasscode {#AppVersion}
AppPublisher=The Glasscode authors
AppPublisherURL=https://github.com/glasscode-ide/glasscode
AppSupportURL=https://github.com/glasscode-ide/glasscode/issues
AppCopyright=Copyright (C) 2026 The Glasscode authors
VersionInfoVersion={#AppVersion}
VersionInfoProductTextVersion={#AppVersion} ({#AppBuild})
VersionInfoDescription=Glasscode Setup
; For the current user only: {autopf} is %LOCALAPPDATA%\Programs and {autoprograms} is the
; user's Start menu.
PrivilegesRequired=lowest
DefaultDirName={autopf}\Glasscode
DisableDirPage=yes
DisableProgramGroupPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
OutputDir={#OutputDir}
OutputBaseFilename=Glasscode-{#AppVersion}-setup
SetupIconFile=..\..\crates\glasscode\resources\Glasscode.ico
UninstallDisplayIcon={app}\Glasscode.exe
UninstallDisplayName=Glasscode
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
; An upgrade asks to close a running Glasscode: only the processes that use the files of {app},
; never the agent browser or a program that a session started.
CloseApplications=yes
RestartApplications=no

[Files]
; All builds have the same file version, so each file is replaced without a version check.
Source: "{#StageDir}\Glasscode.exe"; DestDir: "{app}"; Flags: ignoreversion
; The app looks for the console program next to itself (glasscode_core::cli::console_program).
Source: "{#StageDir}\glasscode-cli.exe"; DestDir: "{app}"; Flags: ignoreversion
; The Linux builds of the server daemon, when the bundle script had them.
Source: "{#StageDir}\glasscoded\*"; DestDir: "{app}\glasscoded"; Flags: ignoreversion skipifsourcedoesntexist

[Icons]
Name: "{autoprograms}\Glasscode"; Filename: "{app}\Glasscode.exe"; Comment: "Claude Code and Codex agents side by side"

[Run]
Filename: "{app}\Glasscode.exe"; Description: "{cm:LaunchProgram,Glasscode}"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
; The programs that an update replaced (glasscode_core::update::windows_app).
Type: files; Name: "{app}\Glasscode.old.exe"
Type: files; Name: "{app}\glasscode-cli.old.exe"
