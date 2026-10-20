; Installeur Laststream (Inno Setup 6).
; Source : dist\laststream-desktop (dossier portable assemble par le build release, voir README-FR.txt).
; Sortie : dist\installer\Laststream-Setup-1.0.3.exe
; Installation par utilisateur, sans droits administrateur. La desinstallation ne supprime jamais
; les donnees utilisateur (%APPDATA%\Laststream) : aucune entree [UninstallDelete] ne les vise.

#define MyAppName "Laststream"
#define MyAppVersion "1.0.4"
#define MyAppExeName "laststream.exe"
#define MyAppSourceDir SourcePath + "..\..\dist\laststream-desktop\"
#define MyAppPublisher "Laststream"
#define public Dependency_NoExampleSetup
#include "CodeDependencies.iss"

[Setup]
; NOTE: AppId identifie cette application. Ne pas le reutiliser pour une autre application.
AppId={{3F8A1C6E-7B2D-4E95-A0F4-6D2C9B1E7A58}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
DefaultDirName={localappdata}\Programs\{#MyAppName}
DefaultGroupName={#MyAppName}
; Installation par utilisateur : aucun droit administrateur requis.
PrivilegesRequired=lowest
DisableProgramGroupPage=yes
DisableReadyPage=yes
DisableDirPage=yes
OutputDir=..\..\dist\installer
OutputBaseFilename={#MyAppName}-Setup-{#MyAppVersion}
Compression=lzma2
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
Name: "desktopicon"; Description: "Créer une icône sur le Bureau"; GroupDescription: "Raccourcis :"; Flags: unchecked

[Files]
; Tout le dossier portable : coque, libmpv, runtimes, server.js, lastaddon\ (dont node.exe) et ui\.
; Le cache WebView2 (laststream.exe.WebView2\) est exclu : il est recree par l'application.
Source: "{#MyAppSourceDir}*"; DestDir: "{app}"; Excludes: "laststream.exe.WebView2,laststream.exe.WebView2\*"; Flags: ignoreversion recursesubdirs createallsubdirs
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
