; Per-user installer: no admin prompt, no system-wide state.
; Built by .github/workflows/release.yml; MyAppVersion is passed in with /D.

#ifndef MyAppVersion
  #define MyAppVersion "0.0.0"
#endif

#define MyAppName "Claude Code Usage Monitor"
#define MyAppPublisher "Hassan Dufer"
#define MyAppURL "https://github.com/hadufer/Claude-Code-Usage-Monitor"
#define MyAppExeName "claude-code-usage-monitor.exe"

[Setup]
AppId={{7F3A6E52-4C21-4C7E-9E4B-5B1D2C9A8E31}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
; Without this, Add/Remove Programs shows "<name> version <x>" and repeats the
; version it already lists in its own column.
AppVerName={#MyAppName}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}/issues
AppUpdatesURL={#MyAppURL}/releases
VersionInfoVersion={#MyAppVersion}

; Installing under the user's profile is what keeps this admin-free, and it is
; also the only location the app's own updater can write to.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
DefaultDirName={localappdata}\Programs\ClaudeCodeUsageMonitor
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
DisableDirPage=auto

; The app already guards against a second instance with this named mutex, so
; reuse it to have Setup ask the user to close a running copy instead of failing
; to replace a locked file.
AppMutex=Global\ClaudeCodeUsageMonitor
CloseApplications=yes
RestartApplications=no

OutputBaseFilename=claude-code-usage-monitor-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
UninstallDisplayIcon={app}\{#MyAppExeName}
LicenseFile=..\LICENSE

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "french"; MessagesFile: "compiler:Languages\French.isl"
Name: "german"; MessagesFile: "compiler:Languages\German.isl"
Name: "spanish"; MessagesFile: "compiler:Languages\Spanish.isl"
Name: "dutch"; MessagesFile: "compiler:Languages\Dutch.isl"
Name: "japanese"; MessagesFile: "compiler:Languages\Japanese.isl"
Name: "russian"; MessagesFile: "compiler:Languages\Russian.isl"
Name: "brazilianportuguese"; MessagesFile: "compiler:Languages\BrazilianPortuguese.isl"

[Tasks]
; Deliberately no "start with Windows" task: the app writes that registry value
; itself from its tray menu, and a second writer would fight it.
Name: "addtopath"; Description: "Add to PATH so 'claude-code-usage-monitor' works in a terminal"; GroupDescription: "Optional:"

[Files]
Source: "..\target\release\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"
Name: "{group}\{cm:UninstallProgram,{#MyAppName}}"; Filename: "{uninstallexe}"

[Registry]
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; \
  ValueData: "{olddata};{app}"; Tasks: addtopath; \
  Check: NeedsPathEntry(ExpandConstant('{app}'))

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#MyAppName}}"; \
  Flags: nowait postinstall skipifsilent

[Code]
{ Only append to PATH when it is not already there, so repeat installs and
  upgrades do not grow the variable every time. }
function NeedsPathEntry(Dir: string): Boolean;
var
  Existing: string;
begin
  if not RegQueryStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', Existing) then
  begin
    Result := True;
    exit;
  end;
  Result := Pos(Lowercase(Dir), Lowercase(Existing)) = 0;
end;
