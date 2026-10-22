; Offline model installer. Data files are split below GitHub's per-asset limit.
#ifndef AppVersion
  #error AppVersion must be passed by build-release.ps1
#endif
[Setup]
AppId={{67D9D347-7A31-4B51-A632-7A7CCE5F8F37}
AppName=Qwen Desk
AppVersion={#AppVersion}
AppPublisher=Qwen Desk contributors
DefaultDirName={localappdata}\Qwen Desk
DefaultGroupName=Qwen Desk
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutputDir}
OutputBaseFilename=QwenDesk-{#AppVersion}-x64-with-model-setup
SetupIconFile={#ProjectDir}\src-tauri\icons\icon.ico
UninstallDisplayIcon={app}\qwen-desk.exe
LicenseFile={#ProjectDir}\LICENSE
InfoBeforeFile={#ProjectDir}\docs\MODEL-NOTICES.md
Compression=lzma2/fast
SolidCompression=no
DiskSpanning=yes
DiskSliceSize=1073741824
SlicesPerDisk=1
WizardStyle=modern
DisableProgramGroupPage=yes
CloseApplications=yes
CloseApplicationsFilter=qwen-desk.exe
RestartApplications=no
SetupLogging=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Files]
Source: "{#ProjectDir}\src-tauri\target\release\qwen-desk.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#ProjectDir}\src-tauri\runtime\*"; DestDir: "{app}\runtime"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "{#ProjectDir}\src-tauri\licenses\*"; DestDir: "{app}\licenses"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "{#ProjectDir}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#ProjectDir}\THIRD_PARTY_NOTICES.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#ModelsDir}\models.lock.json"; DestDir: "{app}\model-notices"; Flags: ignoreversion
Source: "{#ModelsDir}\MODEL-NOTICES.md"; DestDir: "{app}\model-notices"; Flags: ignoreversion
Source: "{#ModelsDir}\LICENSE-APACHE-2.0.txt"; DestDir: "{app}\model-notices"; Flags: ignoreversion
Source: "{#ModelsDir}\qwen3-generation\MODEL-CARD.md"; DestDir: "{app}\model-notices\qwen3-generation"; Flags: ignoreversion
Source: "{#ModelsDir}\qwen3-embedding\MODEL-CARD.md"; DestDir: "{app}\model-notices\qwen3-embedding"; Flags: ignoreversion
; Preserve existing user weights and retain weights after uninstall.
Source: "{#ModelsDir}\qwen3-generation\Qwen3-4B-Instruct-2507-Q4_K_M.gguf.download.json"; DestDir: "{localappdata}\com.qwendesk.desktop\models\qwen3-generation"; Flags: onlyifdoesntexist uninsneveruninstall; Check: NeedsGenerationModel
Source: "{#ModelsDir}\qwen3-embedding\Qwen3-Embedding-0.6B-Q8_0.gguf.download.json"; DestDir: "{localappdata}\com.qwendesk.desktop\models\qwen3-embedding"; Flags: onlyifdoesntexist uninsneveruninstall; Check: NeedsEmbeddingModel
Source: "{#ModelsDir}\qwen3-generation\Qwen3-4B-Instruct-2507-Q4_K_M.gguf"; DestDir: "{localappdata}\com.qwendesk.desktop\models\qwen3-generation"; Flags: onlyifdoesntexist uninsneveruninstall nocompression
Source: "{#ModelsDir}\qwen3-embedding\Qwen3-Embedding-0.6B-Q8_0.gguf"; DestDir: "{localappdata}\com.qwendesk.desktop\models\qwen3-embedding"; Flags: onlyifdoesntexist uninsneveruninstall nocompression

[Icons]
Name: "{group}\Qwen Desk"; Filename: "{app}\qwen-desk.exe"
Name: "{autodesktop}\Qwen Desk"; Filename: "{app}\qwen-desk.exe"; Tasks: desktopicon

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; Flags: unchecked

[Run]
Filename: "{app}\qwen-desk.exe"; Description: "Launch Qwen Desk"; Flags: nowait postinstall skipifsilent

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueName: "Qwen Desk"; Flags: dontcreatekey uninsdeletevalue

[Code]
function NeedsGenerationModel: Boolean;
begin
  Result := not FileExists(ExpandConstant('{localappdata}\com.qwendesk.desktop\models\qwen3-generation\Qwen3-4B-Instruct-2507-Q4_K_M.gguf'));
end;

function NeedsEmbeddingModel: Boolean;
begin
  Result := not FileExists(ExpandConstant('{localappdata}\com.qwendesk.desktop\models\qwen3-embedding\Qwen3-Embedding-0.6B-Q8_0.gguf'));
end;
