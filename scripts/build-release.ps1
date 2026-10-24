param(
    [ValidateSet('all', 'no-model', 'with-model')][string]$Variant = 'all',
    [string]$Tag = '',
    [string]$ModelDirectory = $env:QWEN_MODEL_DIR,
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../artifacts/release'),
    [switch]$SkipBuild
)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'release-common.ps1')
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Push-Location $root
try {
    $package = Get-Content package.json -Raw | ConvertFrom-Json
    $tauri = Get-Content src-tauri/tauri.conf.json -Raw | ConvertFrom-Json
    $cargoText = Get-Content src-tauri/Cargo.toml -Raw
    $cargoVersion = [regex]::Match($cargoText, '(?m)^version\s*=\s*"([^"]+)"').Groups[1].Value
    $version = $package.version
    if ($version -notmatch '^\d+\.\d+\.\d+$' -or $tauri.version -ne $version -or $cargoVersion -ne $version) { throw 'package.json, tauri.conf.json and Cargo.toml must have the same stable version.' }
    if ($Tag -and $Tag -cne "v$version") { throw "Release tag must match source version v$version." }
    if (-not $Tag) { $Tag = "v$version" }
    if ($env:OS -ne 'Windows_NT') { throw 'Windows x64 is required to package this application.' }
    New-Item -ItemType Directory -Force $OutputDirectory | Out-Null
    $output = (Resolve-Path $OutputDirectory).Path
    # Reject a mixed release directory rather than publishing stale assets.
    $allowed = "^QwenDesk-$([regex]::Escape($version))-x64-(no-model|with-model)-setup(?:-\d+)?\.(exe|bin)$|^(SHA256SUMS|RELEASE-NOTES\.md|models\.lock\.json)$"
    foreach ($file in Get-ChildItem $output -Force) {
        if ($file.PSIsContainer -or $file.Name -notmatch $allowed) { throw 'Output directory contains unrelated files. Use a separate empty release directory.' }
    }
    if (Test-Path (Join-Path $output 'SHA256SUMS')) { Remove-Item (Join-Path $output 'SHA256SUMS') }
    & (Join-Path $PSScriptRoot 'prepare-runtime.ps1')
    if (-not $SkipBuild) {
        if ($Variant -eq 'with-model') { & npm.cmd run tauri -- build --no-bundle -- --locked }
        else { & npm.cmd run tauri -- build --bundles nsis -- --locked }
        if ($LASTEXITCODE -ne 0) { throw 'Tauri release build failed.' }
    }
    $app = Join-Path $root 'src-tauri/target/release/qwen-desk.exe'
    if (-not (Test-Path $app)) { throw 'Release application executable is missing.' }
    if ((Get-Item $app).VersionInfo.ProductVersion -notlike "$version*") { throw 'Release executable version does not match the source version.' }
    if ($Variant -in @('all', 'no-model')) {
        $installer = Join-Path $root "src-tauri/target/release/bundle/nsis/Qwen Desk_${version}_x64-setup.exe"
        if (-not (Test-Path $installer)) { throw 'NSIS installer is missing; rebuild without -SkipBuild.' }
        Copy-Item $installer (Join-Path $output "QwenDesk-$version-x64-no-model-setup.exe") -Force
    }
    if ($Variant -in @('all', 'with-model')) {
        & (Join-Path $PSScriptRoot 'prepare-models.ps1') -ModelDirectory $ModelDirectory
        $models = Join-Path $root '.work/models'
        $compiler = & (Join-Path $PSScriptRoot 'prepare-installer.ps1')
        Get-ChildItem $output -Filter "QwenDesk-$version-x64-with-model-setup*" | Remove-Item
        & $compiler "/DAppVersion=$version" "/DProjectDir=$root" "/DModelsDir=$models" "/DOutputDir=$output" (Join-Path $PSScriptRoot 'installer-with-model.iss')
        if ($LASTEXITCODE -ne 0) { throw 'Offline model installer compilation failed.' }
        $parts = @(Get-ChildItem $output -Filter "QwenDesk-$version-x64-with-model-setup-*.bin")
        if ($parts.Count -lt 1) { throw 'Offline installer data files are missing.' }
        foreach ($part in $parts) { if ($part.Length -gt 1GB) { throw 'Offline installer part exceeds 1 GiB.' } }
    }
    Copy-Item (Join-Path $root 'models.lock.json') $output -Force
    $notes = @"
# Qwen Desk $Tag

Windows 10/11 x64 desktop application with bundled CPU/Vulkan inference runtimes.

- **Chinese / English:** choose the interface language in Application Settings. Changes apply immediately; save to remember the language across launches, including tray menus.
- **Content scrolling:** the right scrollbar starts below the title bar, keeping minimize, maximize and close controls outside the scrolling area.
- **Hugging Face token settings:** enter and save a read-only token in Application Settings. Tokens are stored separately in the current user's application data directory, take priority over legacy YAML files and are never returned to the interface or logs. No restart is needed.
- **Startup automation:** enable "Load models and start API on launch" in Application Settings > Startup & Window, then save. On the next launch the API starts listening and existing generation/embedding models load automatically. Downloads are never started automatically; the option is off by default.
- **Without models:** run QwenDesk-$version-x64-no-model-setup.exe; download models in the application or import existing GGUF files.
- **With models:** download QwenDesk-$version-x64-with-model-setup.exe and EVERY matching numbered .bin file into the SAME folder, then run the .exe. Includes Qwen3-4B-Instruct-2507 Q4_K_M and Qwen3-Embedding-0.6B Q8_0 (Apache-2.0). No model download or Hugging Face token is needed.
- Verify downloads against SHA256SUMS. models.lock.json records pinned upstream revisions and weight checksums.
- Requires Microsoft Visual C++ 2015-2022 x64 Runtime and Edge WebView2. Install these before offline setup. GPU acceleration requires a Vulkan driver; CPU fallback is included.
- Installers are unsigned. Both install per user and retain personal models/settings on uninstall. Uninstall an existing variant before switching between the NSIS no-model installer and the Inno with-model installer.

Source, build instructions and CI/release details are in the repository README and docs/RELEASING.md.
"@
    [IO.File]::WriteAllText((Join-Path $output 'RELEASE-NOTES.md'), $notes, (New-Object Text.UTF8Encoding($false)))
    $lines = foreach ($file in Get-ChildItem $output -File | Sort-Object Name) {
        if ($file.Length -ge 2GB) { throw "Release asset exceeds GitHub's 2 GiB limit: $($file.Name)" }
        '{0}  {1}' -f (Get-ReleaseSha256 $file.FullName), $file.Name
    }
    [IO.File]::WriteAllText((Join-Path $output 'SHA256SUMS'), (($lines -join "`n") + "`n"), (New-Object Text.UTF8Encoding($false)))
    Write-Output "Release artifacts ready: $output"
    Get-ChildItem $output -File | Select-Object Name, Length
} finally { Pop-Location }
