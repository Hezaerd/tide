[Setup]
AppId={{EC1761A5-E5FA-4E24-9216-E147D070C7D3}
AppName=Tide
AppVersion={#Version}
AppPublisher=Hezaerd
AppPublisherURL=https://github.com/Hezaerd/tide
DefaultDirName={autopf}\Tide
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
UninstallDisplayIcon={app}\tide.exe
SetupIconFile=..\assets\tide.ico
WizardStyle=modern
DisableProgramGroupPage=yes
OutputDir=..\dist
OutputBaseFilename=Tide-{#Version}-setup

[Files]
Source: "..\target\release\tide.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Tide"; Filename: "{app}\tide.exe"

[Tasks]
Name: "autostart"; Description: "Start Tide when I sign in"

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "Tide"; ValueData: """{app}\tide.exe"""; Tasks: autostart
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run"; ValueType: none; ValueName: "Tide"; Flags: deletevalue; Tasks: autostart

[Run]
Filename: "{app}\tide.exe"; Description: "Launch Tide"; Flags: postinstall nowait skipifsilent

[UninstallRun]
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM tide.exe"; Flags: runhidden; RunOnceId: "StopTide"

[Code]
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  ResultCode: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM tide.exe', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  Result := '';
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
  begin
    RegDeleteValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Run', 'Tide');
    RegDeleteValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run', 'Tide');
  end;
end;
