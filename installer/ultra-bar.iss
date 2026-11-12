; Version and payload are supplied by scripts/package.ps1. Stable identity across upgrades.
#ifndef AppVersion
  #error AppVersion is required
#endif
#ifdef IsolationTest
  #define ProductName "Ultra Bar Installer Test"
  #define ProductId "{{B90D83EE-2C79-459E-8D36-96033E55D8DC}"
  #define AssociationArgs " --installer-test"
#else
  #define ProductName "Ultra Bar"
  #define ProductId "{{DB47F301-83D2-48A2-A3F7-47299DC2F93E}"
  #define AssociationArgs ""
#endif
[Setup]
AppId={#ProductId}
AppName={#ProductName}
AppVersion={#AppVersion}
AppPublisher=Emssion
AppPublisherURL=https://github.com/adoin/ultra-bar
DefaultDirName={localappdata}\Programs\{#ProductName}
DefaultGroupName={#ProductName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
OutputDir={#OutputPath}
OutputBaseFilename={#OutputName}
SetupIconFile=..\assets\ultra-bar.ico
UninstallDisplayIcon={app}\ultra-bar.exe
VersionInfoVersion={#AppVersion}
VersionInfoProductName={#ProductName}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ChangesAssociations=yes
CloseApplications=no
RestartApplications=no
UsePreviousAppDir=yes
UsePreviousTasks=yes
DisableDirPage=no
Uninstallable=yes
SetupLogging=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "chinesesimplified"; MessagesFile: "languages\ChineseSimplified.isl"
[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
[Dirs]
Name: "{app}\plugins"
[Files]
Source: "{#Payload}\ultra-bar.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Payload}\vcruntime140.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Payload}\build-info.json"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Payload}\THIRD-PARTY-NOTICES.txt"; DestDir: "{app}"; Flags: ignoreversion
[Icons]
Name: "{group}\{#ProductName}"; Filename: "{app}\ultra-bar.exe"; WorkingDir: "{app}"; AppUserModelID: "com.emssion.ultra-bar"
Name: "{autodesktop}\{#ProductName}"; Filename: "{app}\ultra-bar.exe"; WorkingDir: "{app}"; Tasks: desktopicon; AppUserModelID: "com.emssion.ultra-bar"
[Run]
Filename: "{app}\ultra-bar.exe"; Description: "{cm:LaunchProgram,{#ProductName}}"; Flags: nowait postinstall skipifsilent unchecked

[Code]
function StopInstalledApp: Boolean;
var Exe: String; Code: Integer;
begin
  Exe := ExpandConstant('{app}\ultra-bar.exe');
  Result := not FileExists(Exe);
  if not Result then
    Result := Exec(Exe, '--request-exit', ExpandConstant('{app}'), SW_HIDE, ewWaitUntilTerminated, Code) and (Code = 0);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := '';
  if not StopInstalledApp then
    Result := 'Please save and close Ultra Bar editors, dialogs or plugin installations, then retry. / 请保存并关闭 Ultra Bar 编辑器、对话框或插件安装，然后重试。';
end;

procedure CurStepChanged(CurStep: TSetupStep);
var Code: Integer;
begin
  if CurStep = ssPostInstall then begin
    if not Exec(ExpandConstant('{app}\ultra-bar.exe'), '--register-plugin-association{#AssociationArgs}', ExpandConstant('{app}'), SW_HIDE, ewWaitUntilTerminated, Code) or (Code <> 0) then
      RaiseException('Unable to register plugin packages. / 无法注册插件包文件关联。');
    if not Exec(ExpandConstant('{app}\ultra-bar.exe'), '--sync-startup{#AssociationArgs}', ExpandConstant('{app}'), SW_HIDE, ewWaitUntilTerminated, Code) or (Code <> 0) then
      RaiseException('Unable to configure logon startup. / 无法设置开机启动。');
  end;
end;

function InitializeUninstall: Boolean;
var Code: Integer;
begin
  Result := StopInstalledApp;
  if not Result then begin
    if not UninstallSilent then MsgBox('Please save and close Ultra Bar editors or dialogs, then retry. / 请保存并关闭 Ultra Bar 编辑器或对话框，然后重试。', mbError, MB_OK);
    exit;
  end;
  if FileExists(ExpandConstant('{app}\ultra-bar.exe')) then
    if not Exec(ExpandConstant('{app}\ultra-bar.exe'), '--unregister-startup{#AssociationArgs}', ExpandConstant('{app}'), SW_HIDE, ewWaitUntilTerminated, Code) or (Code <> 0) then
      Log('Unable to remove owned startup entry.');
  if FileExists(ExpandConstant('{app}\ultra-bar.exe')) then
    if not Exec(ExpandConstant('{app}\ultra-bar.exe'), '--unregister-plugin-association{#AssociationArgs}', ExpandConstant('{app}'), SW_HIDE, ewWaitUntilTerminated, Code) or (Code <> 0) then
      Log('File association belongs to another installation or could not be removed; leaving it untouched.');
end;
