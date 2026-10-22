param(
    [string]$Destination = (Join-Path $PSScriptRoot '../.work/models'),
    [string]$ModelDirectory = $env:QWEN_MODEL_DIR
)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'release-common.ps1')
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$lock = Get-Content (Join-Path $root 'models.lock.json') -Raw | ConvertFrom-Json
New-Item -ItemType Directory -Force $Destination | Out-Null
$Destination = (Resolve-Path $Destination).Path
Add-Type -AssemblyName System.Net.Http
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
$client = New-Object Net.Http.HttpClient
$client.Timeout = [TimeSpan]::FromHours(6)
$client.DefaultRequestHeaders.UserAgent.ParseAdd('QwenDesk-Release/1.0')
if ($env:HF_TOKEN) { $client.DefaultRequestHeaders.Authorization = New-Object Net.Http.Headers.AuthenticationHeaderValue('Bearer', $env:HF_TOKEN) }
function Save-RemoteFile([string]$Url, [string]$Path) {
    $partial = "$Path.part"
    for ($attempt = 1; $attempt -le 3; $attempt++) {
        $response = $null; $inputStream = $null; $outputStream = $null
        try {
            $response = $client.GetAsync($Url, [Net.Http.HttpCompletionOption]::ResponseHeadersRead).GetAwaiter().GetResult()
            if (-not $response.IsSuccessStatusCode) { throw "Download returned HTTP $([int]$response.StatusCode)." }
            $inputStream = $response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
            $outputStream = [IO.File]::Create($partial)
            $inputStream.CopyTo($outputStream, 1048576)
            $outputStream.Dispose(); $outputStream = $null
            Move-Item $partial $Path -Force
            return
        } catch {
            if ($attempt -eq 3) { throw 'Pinned model or model-card download failed after three attempts. Check network access and HF_TOKEN permissions.' }
        } finally {
            if ($outputStream) { $outputStream.Dispose() }
            if ($inputStream) { $inputStream.Dispose() }
            if ($response) { $response.Dispose() }
        }
    }
}
function Test-Model([string]$Path, $Model) {
    return (Test-Path $Path -PathType Leaf) -and ((Get-Item $Path).Length -eq $Model.size) -and ((Get-ReleaseSha256 $Path) -eq $Model.sha256)
}
try {
    foreach ($model in $lock.models) {
        if ($model.id -notmatch '^[a-z0-9-]+$' -or $model.filename -notmatch '^[A-Za-z0-9_.-]+\.gguf$' -or $model.repo -notmatch '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$' -or $model.commit -notmatch '^[a-f0-9]{40}$' -or $model.sha256 -notmatch '^[a-f0-9]{64}$') { throw 'Invalid pinned model descriptor.' }
        $folder = Join-Path $Destination $model.id
        New-Item -ItemType Directory -Force $folder | Out-Null
        $target = Join-Path $folder $model.filename
        if (-not (Test-Model $target $model)) {
            if ($ModelDirectory) {
                $source = Join-Path (Join-Path $ModelDirectory $model.id) $model.filename
                if (-not (Test-Model $source $model)) { throw "Local model is missing or fails its pinned size/SHA-256 check: $($model.id)" }
                if ([IO.Path]::GetFullPath($source) -ne [IO.Path]::GetFullPath($target)) { Copy-Item $source $target -Force }
            } else {
                Write-Output "Downloading pinned model: $($model.id)"
                Save-RemoteFile "https://huggingface.co/$($model.repo)/resolve/$($model.commit)/$($model.filename)?download=true" $target
            }
            if (-not (Test-Model $target $model)) { Remove-Item $target -Force -ErrorAction SilentlyContinue; throw "Model checksum mismatch: $($model.id)" }
        }
        # Download provenance at the same immutable revision; no credentials are copied.
        Save-RemoteFile "https://huggingface.co/$($model.repo)/resolve/$($model.commit)/README.md" (Join-Path $folder 'MODEL-CARD.md')
        $card = [IO.File]::ReadAllText((Join-Path $folder 'MODEL-CARD.md'))
        if ($model.license -ne 'Apache-2.0' -or $card -notmatch '(?mi)^license:\s*apache-2\.0\s*$') { throw 'Bundled model license differs from the approved Apache-2.0 descriptor.' }
        $record = [ordered]@{ version = 1; id = $model.id; repo = $model.repo; filename = $model.filename; commit = $model.commit; sha256 = $model.sha256; size = $model.size }
        [IO.File]::WriteAllText("$target.download.json", ($record | ConvertTo-Json -Compress), (New-Object Text.UTF8Encoding($false)))
        Write-Output "Verified model: $($model.id) ($($model.size) bytes)"
    }
    Copy-Item (Join-Path $root 'models.lock.json') $Destination -Force
    Copy-Item (Join-Path $root 'docs/MODEL-NOTICES.md') $Destination -Force
    Copy-Item (Join-Path $root 'docs/MODEL-LICENSE-APACHE-2.0.txt') (Join-Path $Destination 'LICENSE-APACHE-2.0.txt') -Force
} finally { $client.Dispose() }
