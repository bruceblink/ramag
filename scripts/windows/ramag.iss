#define MyAppName "Ramag"
#define MyAppPublisher "axemc"
#define MyAppURL GetEnv("RAMAG_PACKAGE_URL")
#define MyAppExeName "ramag.exe"
#define MyAppId "com.axemc.ramag"
#define RepoRoot AddBackslash(SourcePath) + "..\..\"
#define SourceExe GetEnv("RAMAG_PACKAGE_EXE")
#define AppVersion GetEnv("RAMAG_PACKAGE_VERSION")
#define AppVersionInfo GetEnv("RAMAG_PACKAGE_VERSION_INFO")

#if SourceExe == ""
  #error RAMAG_PACKAGE_EXE is required
#endif
#if AppVersion == ""
  #error RAMAG_PACKAGE_VERSION is required
#endif
#if AppVersionInfo == ""
  #error RAMAG_PACKAGE_VERSION_INFO is required
#endif
#if MyAppURL == ""
  #error RAMAG_PACKAGE_URL is required
#endif

[Setup]
AppId={#MyAppId}
AppName={#MyAppName}
AppVersion={#AppVersion}
AppVerName={#MyAppName} {#AppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}/issues
AppUpdatesURL={#MyAppURL}/releases
DefaultDirName={localappdata}\Programs\{#MyAppName}
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
AppMutex=Local\RamagSingleInstanceMutex
CloseApplications=yes
RestartApplications=no
LicenseFile={#RepoRoot}LICENSE
SetupIconFile={#RepoRoot}scripts\icons\ramag.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
UninstallDisplayName={#MyAppName}
VersionInfoVersion={#AppVersionInfo}
VersionInfoCompany={#MyAppPublisher}
VersionInfoDescription={#MyAppName} installer
VersionInfoProductName={#MyAppName}
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
SetupLogging=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "chinesesimplified"; MessagesFile: "{#RepoRoot}scripts\windows\languages\ChineseSimplified.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; DestName: "{#MyAppExeName}"; Flags: ignoreversion
Source: "{#RepoRoot}LICENSE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#StringChange(MyAppName, '&', '&&')}}"; Flags: nowait postinstall skipifsilent
