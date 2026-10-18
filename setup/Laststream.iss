; Installeur Laststream (Inno Setup 6).
; Source : dist\laststream-desktop, assemble a partir du build release (voir README-FR.txt).
; Script ecrit sans compilation : Inno Setup n'est pas installe sur la machine de build.

#define MyAppName "Laststream"
#define MyAppExeName "laststream.exe"
#define MyAppSourceDir SourcePath + "..\..\dist\laststream-desktop\"
#define MyAppVersion() GetVersionComponents(MyAppSourceDir + MyAppExeName, Local[0], Local[1], Local[2], Local[3]), \
  Str(Local[0]) + "." + Str(Local[1]) + "." + Str(Local[2])

#define MyAppPublisher "Laststream"
#define public Dependency_NoExampleSetup
#include "CodeDependencies.iss"

[Setup]
; NOTE: AppId identifie cette application. Ne pas le reutiliser pour une autre application.
AppId={{3F8A1C6E-7B2D-4E95-A0F4-6D2C9B1E7A58}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
; Installation par utilisateur : aucun droit administrateur requis.
PrivilegesRequired=lowest
DisableProgramGroupPage=yes
DisableReadyPage=yes
DisableDirPage=yes
OutputDir=..\..\dist
OutputBaseFilename={#MyAppName}Setup-v{#MyAppVersion}_x64
Compression=lzma
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
LanguageDetectionMethod=uilanguage
ShowLanguageDialog=auto
CloseApplications=yes
SetupMutex=LaststreamSetupMutex,Global\LaststreamSetupMutex
SetupIconFile={#SourcePath}..\images\stremio.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
#ifdef SIGN
SignTool=stremiosign
SignedUninstaller=yes
#endif

[Languages]
Name: "french"; MessagesFile: "compiler:Languages\French.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Creer une icone sur le Bureau"; GroupDescription: "Raccourcis :"; Flags: unchecked

[Files]
; Tout le dossier portable : coque, libmpv, runtimes, server.js, lastaddon\ (dont node.exe) et ui\.
Source: "{#MyAppSourceDir}*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "{#MyAppSourceDir}README-FR.txt"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "Lancer {#MyAppName}"; Flags: nowait postinstall skipifsilent

[Code]
function InitializeSetup: Boolean;
begin
  Dependency_AddWebView2;
  Result := True;
end;
