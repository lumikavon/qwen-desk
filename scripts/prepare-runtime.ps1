param([string]$Destination = (Join-Path $PSScriptRoot '../src-tauri/runtime'))
$ErrorActionPreference = 'Stop'
$release = 'b11796'
$packages = @(
    @{ Backend = 'cpu'; Sha256 = 'd3ae3e9f395175b5a1cf8e9a82bbc92d21c1f44201dd0321f0196f8a4d2d0b69' },
    @{ Backend = 'vulkan'; Sha256 = 'b7035131edd2dcfc4a123a9deb16d109af655ef7c5e0d283edd7323808cb78b7' }
)
New-Item -ItemType Directory -Force $Destination | Out-Null
foreach ($package in $packages) {
    $backend = $package.Backend
    $target = Join-Path $Destination $backend
    $stamp = Join-Path $target "$release.verified"
    if ((Test-Path $stamp) -and (Test-Path (Join-Path $target 'llama-server.exe'))) {
        Write-Output "$backend runtime is ready ($release)"
        continue
    }
    $zip = Join-Path ([IO.Path]::GetTempPath()) "qwen-desk-$release-$backend.zip"
    $url = "https://github.com/ggml-org/llama.cpp/releases/download/$release/llama-$release-bin-win-$backend-x64.zip"
    if (-not (Test-Path $zip)) {
        & curl.exe --fail --location --retry 3 --output $zip $url
        if ($LASTEXITCODE -ne 0) { throw 'Runtime download failed.' }
    }
    $stream = [IO.File]::OpenRead($zip)
    try { $sha = [Security.Cryptography.SHA256]::Create(); $digest = [BitConverter]::ToString($sha.ComputeHash($stream)).Replace('-', '').ToLowerInvariant() }
    finally { $stream.Dispose(); if ($sha) { $sha.Dispose() } }
    if ($digest -ne $package.Sha256) { Remove-Item $zip; throw 'Runtime checksum mismatch; run again to download a clean archive.' }
    if (Test-Path $target) { Remove-Item $target -Recurse -Force }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [IO.Compression.ZipFile]::ExtractToDirectory($zip, $target)
    $server = Get-ChildItem $target -Filter llama-server.exe -Recurse | Select-Object -First 1
    if (-not $server) { throw 'Runtime archive has no llama-server.exe.' }
    if ($server.DirectoryName -ne (Resolve-Path $target).Path) {
        Get-ChildItem $server.DirectoryName | Copy-Item -Destination $target -Recurse -Force
    }
    # Only ship inference server, its required DLLs, and upstream license texts.
    Get-ChildItem $target -File -Filter '*.exe' | Where-Object { $_.Name -ne 'llama-server.exe' } | Remove-Item
    Set-Content -Path $stamp -Value $package.Sha256 -Encoding ascii
    Remove-Item $zip
    Write-Output "$backend runtime verified ($release)"
}
