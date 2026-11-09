; Inno Setup script for Offline US Maps. Compile through build/build.ps1 so the version and the
; verified publish folder are supplied together.
;
; Per-user install: no administrator rights, files under %LOCALAPPDATA%\Programs\Offline US Maps,
; a Start entry under the app's name and an uninstaller in Settings > Apps. A later version
; installs over this one (same AppId). Maps, stations and addresses, saved places and settings
; (%LOCALAPPDATA%\Jupiter\OfflineMaps) are never touched by install, upgrade or uninstall.
;
; The Microsoft Edge WebView2 Runtime (which draws the map) is part of Windows 11; the offline
; runtime installer is bundled and run only if it is missing, so setup works without a
; connection.

#define AppName "Offline US Maps"
#define AppExe "OfflineMaps.exe"
#define AppUserModelId "Jupiter.OfflineMaps"
#ifndef AppVersion
  #error AppVersion must be supplied by build.ps1
#endif

[Setup]
AppId={{F91032A7-031E-4B0C-926B-ABE6EA3E12CB}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=Ian Rastall
DefaultDirName={autopf}\{#AppName}
DisableProgramGroupPage=yes
DisableDirPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Windows 11 24H2, as the app and Jupiter.UI require.
MinVersion=10.0.26100
; The app holds this mutex while it runs, so Setup asks for it to be closed first.
AppMutex=Jupiter.OfflineMaps
CloseApplications=no
RestartApplications=no
UninstallDisplayName={#AppName}
UninstallDisplayIcon={app}\{#AppExe}
SetupIconFile=..\src\OfflineMaps.App\Assets\AppIcon.ico
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
; Tell Explorer and Start to refresh icons after install and uninstall.
ChangesAssociations=yes
OutputDir=..\dist
OutputBaseFilename=OfflineUSMaps-{#AppVersion}-win-x64-setup

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[InstallDelete]
; A Jupiter folder in Start is never used; Start lists the app by name.
Type: files; Name: "{autoprograms}\Jupiter\{#AppName}.lnk"
Type: dirifempty; Name: "{autoprograms}\Jupiter"
; Shortcuts from before the rename to Offline US Maps (0.6.0). An upgrade keeps the install
; folder, the AppId and the user data; only the name changes.
Type: files; Name: "{autoprograms}\Offline Maps.lnk"
Type: files; Name: "{autodesktop}\Offline Maps.lnk"
; Map page files of an earlier version that no longer ship.
Type: filesandordirs; Name: "{app}\Web"

[Files]
Source: "..\dist\OfflineMaps\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs; Excludes: "*.pdb"
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "..\third_party\webview2\MicrosoftEdgeWebView2RuntimeInstallerX64.exe"; DestDir: "{tmp}"; Flags: deleteafterinstall; Check: NeedsWebView2

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"; WorkingDir: "{app}"; AppUserModelID: "{#AppUserModelId}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; WorkingDir: "{app}"; AppUserModelID: "{#AppUserModelId}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent

[Code]
function NeedsWebView2: Boolean;
var
  Version: string;
begin
  Result := not ((RegQueryStringValue(HKLM32, 'SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}', 'pv', Version)
    or RegQueryStringValue(HKCU, 'SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}', 'pv', Version))
    and (Version <> '') and (Version <> '0.0.0.0'));
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  ResultCode: Integer;
begin
  if (CurStep = ssPostInstall) and NeedsWebView2 then
  begin
    if not Exec(ExpandConstant('{tmp}\MicrosoftEdgeWebView2RuntimeInstallerX64.exe'), '/silent /install', '', SW_HIDE, ewWaitUntilTerminated, ResultCode) then
      RaiseException('The bundled WebView2 runtime could not be started.');
    if NeedsWebView2 then
      RaiseException('The WebView2 runtime could not be installed. Run setup again after resolving the runtime installation error.');
  end;
end;
