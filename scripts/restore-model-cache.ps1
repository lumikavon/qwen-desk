param([string]$CacheDirectory = (Join-Path (Get-Location) '.work/models'))
$ErrorActionPreference = 'Stop'
$root = (Get-Location).Path
. (Join-Path $root 'scripts/release-common.ps1')
$lockPath = Join-Path $root 'models.lock.json'
$lock = [IO.File]::ReadAllText($lockPath) | ConvertFrom-Json
function Test-CachedModels {
    foreach ($model in $lock.models) {
        $path = Join-Path (Join-Path $CacheDirectory $model.id) $model.filename
        if (-not (Test-Path $path -PathType Leaf)) { return $false }
        if ((Get-Item $path).Length -ne $model.size -or (Get-ReleaseSha256 $path) -cne $model.sha256) { return $false }
    }
    return $true
}
if (Test-CachedModels) { Write-Output 'Pinned model cache verified.'; return }
# The seed is an already published, checksum-pinned installer of this repository.
# Execute it only on disposable GitHub-hosted Windows runners.
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_OS -ne 'Windows' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted') {
    Write-Output 'No verified cache. Use the normal pinned model download on this host.'
    return
}
$seedManifestHash = 'feca9c5ecc7087112a6aa113a18194395e8b01f0188822bf51f08120ad45eef7'
$seedModelLockHash = 'bbaf811e0a72ed40a6b45012aa06ddd21b04e483e4f00084dcc51eda45659852'
if ((Get-ReleaseSha256 $lockPath) -cne $seedModelLockHash) {
    Write-Output 'Model pins differ from the seed release; use the normal pinned download.'
    return
}
if ($env:GH_REPO -ne 'lumikavon/qwen-desk') { Write-Output 'Use the normal pinned download outside the original repository.'; return }
$seedDirectory = Join-Path $root '.work/model-seed'
$seedApp = Join-Path $root '.work/model-seed-app'
New-Item -ItemType Directory -Force $seedDirectory | Out-Null
Write-Output 'Downloading the pinned v1.0.0 release manifest.'
& gh release download v1.0.0 --repo $env:GH_REPO --pattern SHA256SUMS --dir $seedDirectory --clobber
if ($LASTEXITCODE -ne 0) { throw 'Cannot download the model seed manifest.' }
$manifest = Join-Path $seedDirectory 'SHA256SUMS'
if ((Get-ReleaseSha256 $manifest) -cne $seedManifestHash) { throw 'The model seed manifest differs from its pinned digest.' }
$required = @('QwenDesk-1.0.0-x64-with-model-setup.exe', 'QwenDesk-1.0.0-x64-with-model-setup-1.bin', 'QwenDesk-1.0.0-x64-with-model-setup-2.bin', 'QwenDesk-1.0.0-x64-with-model-setup-3.bin')
$hashes = @{}
foreach ($line in [IO.File]::ReadAllLines($manifest)) {
    if ($line -cmatch '^([a-f0-9]{64})  ([A-Za-z0-9._-]+)$') { $hashes[$Matches[2]] = $Matches[1] }
}
foreach ($name in $required) { if (-not $hashes.ContainsKey($name)) { throw 'The pinned seed manifest has an incomplete inventory.' } }
Write-Output 'Downloading model data from the existing GitHub release.'
& gh release download v1.0.0 --repo $env:GH_REPO --pattern 'QwenDesk-1.0.0-x64-with-model-setup*' --dir $seedDirectory --clobber
if ($LASTEXITCODE -ne 0) { throw 'Cannot download the model seed installer and data.' }
foreach ($name in $required) {
    if ((Get-ReleaseSha256 (Join-Path $seedDirectory $name)) -cne $hashes[$name]) { throw "Model seed checksum mismatch: $name" }
}
$existingSources = @{}
foreach ($model in $lock.models) {
    $source = Join-Path (Join-Path (Join-Path $env:LOCALAPPDATA 'com.qwendesk.desktop/models') $model.id) $model.filename
    $existingSources[$source] = Test-Path $source -PathType Leaf
}
Write-Output 'Extracting checksum-verified model data on the disposable runner.'
$installer = Start-Process -FilePath (Join-Path $seedDirectory $required[0]) -ArgumentList @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/NOICONS', "/DIR=`"$seedApp`"") -PassThru -Wait
if ($installer.ExitCode -ne 0) { throw 'The pinned seed installer could not extract its models.' }
foreach ($model in $lock.models) {
    if ($model.id -notmatch '^[a-z0-9-]+$' -or $model.filename -notmatch '^[A-Za-z0-9_.-]+\.gguf$') { throw 'Unsafe model pin.' }
    $folder = Join-Path $CacheDirectory $model.id
    New-Item -ItemType Directory -Force $folder | Out-Null
    $source = Join-Path (Join-Path (Join-Path $env:LOCALAPPDATA 'com.qwendesk.desktop/models') $model.id) $model.filename
    Copy-Item $source (Join-Path $folder $model.filename) -Force
}
if (-not (Test-CachedModels)) { throw 'Extracted model weights do not match the current pinned SHA-256 and size.' }
foreach ($source in $existingSources.Keys) {
    if (-not $existingSources[$source]) { Remove-Item $source -Force }
}
# Free the seed media before creating the new installer. Retain the verified cache.
Remove-Item $seedDirectory -Recurse -Force
Remove-Item $seedApp -Recurse -Force
Write-Output 'Pinned model cache restored from verified GitHub release data.'
