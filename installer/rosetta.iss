; Inno Setup script for Rosetta.
;
; Driven by scripts\package.ps1, which stages the payload first and passes the
; version and paths in:
;
;   ISCC /DAppVersion=0.1.0 /DPayloadDir=..\dist\stage /DOutputDir=..\dist installer\rosetta.iss
;
; Per-user by design. The app writes nothing outside its own folder and
; %APPDATA%\Rosetta, so there is no reason to ask for elevation.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef PayloadDir
  #define PayloadDir "..\dist\stage"
#endif
#ifndef OutputDir
  #define OutputDir "..\dist"
#endif

#define AppName "Rosetta"
#define AppExe "rosetta-desktop.exe"
#define AppPublisher "wlwatkins"

[Setup]
; Never change this: it is how Windows recognises an upgrade of an existing
; install rather than a second copy.
AppId={{1265AF53-A850-455D-9E00-4DB0F4CC65A9}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher={#AppPublisher}
VersionInfoVersion={#AppVersion}

; asInvoker: a tray utility has no business prompting for admin.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
AllowNoIcons=yes

; Setup refuses to overwrite a running copy, so ask for it to be closed. The
; name matches SINGLE_INSTANCE_MUTEX in src/ui/app.rs.
AppMutex=RosettaDesktopSingleInstance
CloseApplications=yes
RestartApplications=no

OutputDir={#OutputDir}
OutputBaseFilename=Rosetta-Setup-{#AppVersion}
SetupIconFile=..\assets\rosetta.ico
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName} {#AppVersion}
LicenseFile=..\LICENSE

; The payload is ~490MB, most of it ONNX weights, which compress well.
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; GroupDescription: "Shortcuts:"; Flags: unchecked
Name: "startupicon"; Description: "Start {#AppName} when I sign in"; GroupDescription: "Shortcuts:"; Flags: unchecked

[Files]
Source: "{#PayloadDir}\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\models\*"; DestDir: "{app}\models"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "{#PayloadDir}\vendor\*"; DestDir: "{app}\vendor"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "{#PayloadDir}\README.txt"; DestDir: "{app}"; Flags: ignoreversion isreadme
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{group}\{#AppName} Settings"; Filename: "{app}\{#AppExe}"; Parameters: "settings"; Comment: "Change the shortcut, theme and recognition options"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon
Name: "{userstartup}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: startupicon

[Run]
Filename: "{app}\{#AppExe}"; Description: "Start {#AppName} now"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
; ONNX Runtime unpacks next to the binary on first use.
Type: filesandordirs; Name: "{app}\models"
Type: filesandordirs; Name: "{app}\vendor"
Type: dirifempty; Name: "{app}"

[Code]
// Settings live outside {app}, so they survive an uninstall unless asked for.
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  SettingsDir: String;
begin
  if CurUninstallStep = usPostUninstall then
  begin
    SettingsDir := ExpandConstant('{userappdata}\Rosetta');
    if DirExists(SettingsDir) then
    begin
      if MsgBox('Remove your Rosetta settings as well?' + #13#10 + #13#10 + SettingsDir,
                mbConfirmation, MB_YESNO) = IDYES then
        DelTree(SettingsDir, True, True, True);
    end;
  end;
end;
