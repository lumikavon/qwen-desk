#requires -Version 5.1
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet('github', 'gitlab')]
    [string]$Provider,
    [Parameter(Mandatory = $true)]
    [string]$Tag,
    [string]$Directory = 'artifacts/release',
    [string]$Repository = $env:GH_REPO
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
. (Join-Path $PSScriptRoot 'release-common.ps1')

function Invoke-Git {
    param([string[]]$Arguments)
    $output = & git -C (Join-Path $PSScriptRoot '..') @Arguments 2>$null
    if ($LASTEXITCODE -ne 0) { throw 'Git validation failed. Fetch the requested tag and check out its commit.' }
    return ($output -join "`n").Trim()
}

function Invoke-Gh {
    param([string[]]$Arguments)
    $output = & gh @Arguments 2>$null
    if ($LASTEXITCODE -ne 0) { throw 'GitHub CLI request failed. Check authentication, repository access, and network availability.' }
    return ($output -join "`n")
}

function Get-GhJson {
    param([string]$Endpoint)
    return (Invoke-Gh -Arguments @('api', $Endpoint) | ConvertFrom-Json)
}

function Get-GhPages {
    param([string]$Endpoint)
    $pages = Invoke-Gh -Arguments @('api', '--paginate', '--slurp', $Endpoint) | ConvertFrom-Json
    foreach ($page in $pages) { foreach ($item in $page) { $item } }
}

function Get-PropertyValue {
    param($Object, [string]$Name)
    $property = $Object.PSObject.Properties[$Name]
    if ($null -ne $property) { return $property.Value }
    return $null
}

function Assert-GhTag {
    param([string]$RepoApi, [string]$EncodedTag, [string]$Commit)
    $reference = Get-GhJson -Endpoint "$RepoApi/git/ref/tags/$EncodedTag"
    $remoteObject = $reference.object
    for ($depth = 0; $remoteObject.type -eq 'tag' -and $depth -lt 10; $depth++) {
        $annotation = Get-GhJson -Endpoint "$RepoApi/git/tags/$($remoteObject.sha)"
        $remoteObject = $annotation.object
    }
    if ($remoteObject.type -ne 'commit' -or $remoteObject.sha -cne $Commit) {
        throw 'GitHub tag does not point to the current checkout commit.'
    }
}

function Assert-GhDraft {
    param([string]$Endpoint)
    $release = Get-GhJson -Endpoint $Endpoint
    if (-not $release.draft -or $release.tag_name -cne $Tag) {
        throw 'Refusing to change a published release or a release for another tag.'
    }
    return $release
}

function Invoke-GitLab {
    param(
        [string]$Method,
        [string]$Uri,
        $Body = $null,
        [string]$File = '',
        [switch]$AllowNotFound
    )
    # Keep credentials in headers, never command arguments, URLs, or log output.
    $request = @{
        Uri = $Uri
        Method = $Method
        Headers = @{ 'JOB-TOKEN' = $env:CI_JOB_TOKEN }
        ErrorAction = 'Stop'
        MaximumRedirection = 0
    }
    if ($File) {
        $request.InFile = $File
        $request.ContentType = 'application/octet-stream'
        $request.TimeoutSec = 3600
    } elseif ($null -ne $Body) {
        $request.Body = [Text.Encoding]::UTF8.GetBytes(($Body | ConvertTo-Json -Depth 12 -Compress))
        $request.ContentType = 'application/json; charset=utf-8'
    }
    try { return Invoke-RestMethod @request }
    catch {
        $status = 0
        $response = Get-PropertyValue -Object $_.Exception -Name 'Response'
        if ($null -ne $response) { $status = [int]$response.StatusCode }
        if ($AllowNotFound -and $status -eq 404) { return $null }
        throw "GitLab API request failed (HTTP $status). Check job-token permissions, registry settings, and connectivity."
    }
}

function Get-GitLabPages {
    param([string]$Uri)
    $separator = '?'
    if ($Uri.Contains('?')) { $separator = '&' }
    for ($page = 1; ; $page++) {
        $response = Invoke-GitLab -Method Get -Uri "${Uri}${separator}per_page=100&page=$page"
        $items = @($response | ForEach-Object { $_ })
        foreach ($item in $items) { $item }
        if ($items.Count -lt 100) { break }
    }
}

function Get-GitLabPackageFiles {
    param([string]$ProjectApi, [string]$EncodedTag)
    $packages = @(Get-GitLabPages -Uri "$ProjectApi/packages?package_type=generic&package_name=qwen-desk&package_version=$EncodedTag")
    foreach ($package in $packages) {
        if ($package.name -ceq 'qwen-desk' -and $package.version -ceq $Tag -and $package.package_type -ceq 'generic') {
            Get-GitLabPages -Uri "$ProjectApi/packages/$($package.id)/package_files"
        }
    }
}

function Assert-GitLabFiles {
    param([object[]]$RemoteFiles, [switch]$RequireAll)
    $seen = New-Object 'System.Collections.Generic.HashSet[string]' ([StringComparer]::Ordinal)
    foreach ($remote in $RemoteFiles) {
        $name = [string]$remote.file_name
        if (-not $inventory.ContainsKey($name) -or -not $seen.Add($name)) {
            throw "Unexpected or duplicate file in versioned GitLab package: $name"
        }
        $local = $inventory[$name]
        if ([long]$remote.size -ne $local.Size -or $remote.file_sha256 -ine $local.Sha256) {
            throw "Versioned GitLab package already contains different content: $name. Use a new version."
        }
    }
    if ($RequireAll -and $seen.Count -ne $inventory.Count) { throw 'GitLab package inventory is incomplete; release was not created.' }
    return ,$seen
}

if ($Tag -cnotmatch '^v(?<Version>[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?)$') {
    throw 'Tag must be a version such as v1.0.0.'
}
$version = $Matches.Version
$releaseDirectory = Get-Item -LiteralPath $Directory
if (-not $releaseDirectory.PSIsContainer -or ($releaseDirectory.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
    throw 'Release directory must be a regular directory.'
}
$files = @(Get-ChildItem -LiteralPath $releaseDirectory.FullName -Force)
$required = @(
    "QwenDesk-$version-x64-no-model-setup.exe",
    "QwenDesk-$version-x64-with-model-setup.exe",
    'models.lock.json', 'SHA256SUMS', 'RELEASE-NOTES.md'
)
$allowed = New-Object 'System.Collections.Generic.HashSet[string]' ([StringComparer]::Ordinal)
foreach ($name in $required) { [void]$allowed.Add($name) }
$partPattern = '^QwenDesk-' + [regex]::Escape($version) + '-x64-with-model-setup-([1-9][0-9]*)\.bin$'
$parts = @()
$inventory = New-Object 'System.Collections.Generic.Dictionary[string,object]' ([StringComparer]::Ordinal)
foreach ($file in $files) {
    $name = $file.Name
    if ($file.PSIsContainer -or ($file.Attributes -band [IO.FileAttributes]::ReparsePoint) -or $name -cnotmatch '^[A-Za-z0-9][A-Za-z0-9._-]*$') {
        throw "Unsafe release entry: $name"
    }
    if ($name -cmatch $partPattern) {
        $parts += [int]$Matches[1]
        if ($file.Length -gt 1GB) { throw "Inno data part exceeds 1 GiB: $name" }
    } elseif (-not $allowed.Contains($name)) { throw "Unexpected release file: $name" }
    if ($file.Length -le 0) { throw "Empty release file: $name" }
    if ($Provider -eq 'github' -and $file.Length -ge 2GB) { throw "GitHub assets must be smaller than 2 GiB: $name" }
    $inventory.Add($name, @{
        Path = $file.FullName
        Size = [long]$file.Length
        Sha256 = Get-ReleaseSha256 $file.FullName
    })
}
foreach ($name in $required) {
    if (-not $inventory.ContainsKey($name)) { throw "Missing release file: $name" }
}
$parts = @($parts | Sort-Object)
if ($parts.Count -eq 0) { throw 'Missing diskspanned Inno data files.' }
for ($index = 0; $index -lt $parts.Count; $index++) {
    if ($parts[$index] -ne $index + 1) { throw 'Inno data part numbers must be contiguous, starting at 1.' }
}
$checksummed = New-Object 'System.Collections.Generic.HashSet[string]' ([StringComparer]::OrdinalIgnoreCase)
foreach ($line in [IO.File]::ReadAllLines($inventory['SHA256SUMS'].Path)) {
    if ([string]::IsNullOrWhiteSpace($line)) { continue }
    if ($line -cnotmatch '^([a-fA-F0-9]{64})\s+\*?([A-Za-z0-9][A-Za-z0-9._-]*)$') { throw 'Malformed or unsafe SHA256SUMS entry.' }
    $hash = $Matches[1]
    $name = $Matches[2]
    if ($name -ceq 'SHA256SUMS' -or -not $inventory.ContainsKey($name) -or -not $checksummed.Add($name)) {
        throw "Unexpected, missing, or duplicate checksum target: $name"
    }
    if ($inventory[$name].Sha256 -ine $hash) { throw "SHA256 mismatch: $name" }
}
if ($checksummed.Count -ne $inventory.Count - 1) { throw 'SHA256SUMS must cover every release file except itself.' }

$headCommit = Invoke-Git -Arguments @('rev-parse', '--verify', 'HEAD^{commit}')
$tagCommit = Invoke-Git -Arguments @('rev-parse', '--verify', "refs/tags/${Tag}^{commit}")
if ($tagCommit -cne $headCommit) { throw 'Current checkout does not match the requested tag.' }
& git -C (Join-Path $PSScriptRoot '..') diff --quiet HEAD --
if ($LASTEXITCODE -ne 0) { throw 'Tracked source changes are present. Publish only from a clean tagged checkout.' }
$encodedTag = [Uri]::EscapeDataString($Tag)

if ($Provider -eq 'github') {
    if ($Repository -cnotmatch '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$') { throw 'Set -Repository or GH_REPO to owner/repository.' }
    if (-not (Get-Command gh -ErrorAction SilentlyContinue)) { throw 'GitHub CLI (gh) is required.' }
    $repoApi = "repos/$Repository"
    Assert-GhTag -RepoApi $repoApi -EncodedTag $encodedTag -Commit $headCommit
    $releases = @(Get-GhPages -Endpoint "$repoApi/releases?per_page=100")
    $existing = @($releases | Where-Object { $_.tag_name -ceq $Tag })
    if ($existing.Count -gt 1) { throw 'Multiple releases unexpectedly use the same tag.' }
    if ($existing.Count -eq 1) {
        $release = $existing[0]
        if (-not $release.draft) { throw 'This GitHub release is already published. Published releases are never replaced.' }
    } else {
        [void](Invoke-Gh -Arguments @('release', 'create', $Tag, '--repo', $Repository, '--draft', '--verify-tag', '--target', $headCommit, '--title', $Tag, '--notes-file', $inventory['RELEASE-NOTES.md'].Path))
        # List releases to include drafts; do not depend on the published-tag endpoint.
        $created = @(Get-GhPages -Endpoint "$repoApi/releases?per_page=100" | Where-Object { $_.tag_name -ceq $Tag })
        if ($created.Count -ne 1) { throw 'Could not identify the new GitHub draft release.' }
        $release = $created[0]
    }
    $releaseApi = "$repoApi/releases/$($release.id)"
    [void](Assert-GhDraft -Endpoint $releaseApi)
    $remoteAssets = @(Get-GhPages -Endpoint "$releaseApi/assets?per_page=100")
    foreach ($asset in $remoteAssets) {
        if (-not $inventory.ContainsKey([string]$asset.name)) { throw "Draft has an unexpected asset: $($asset.name). Remove it before retrying." }
    }
    # --clobber is confined to a verified draft; a partial upload can be retried safely.
    foreach ($name in ($inventory.Keys | Sort-Object)) {
        [void](Assert-GhDraft -Endpoint $releaseApi)
        Write-Output "Uploading GitHub asset: $name"
        [void](Invoke-Gh -Arguments @('release', 'upload', $Tag, $inventory[$name].Path, '--repo', $Repository, '--clobber'))
    }
    $remoteAssets = @(Get-GhPages -Endpoint "$releaseApi/assets?per_page=100")
    if ($remoteAssets.Count -ne $inventory.Count) { throw 'GitHub asset count differs from the verified release directory.' }
    $seen = New-Object 'System.Collections.Generic.HashSet[string]' ([StringComparer]::Ordinal)
    foreach ($asset in $remoteAssets) {
        $name = [string]$asset.name
        if (-not $inventory.ContainsKey($name) -or -not $seen.Add($name)) { throw 'GitHub returned unexpected or duplicate assets.' }
        if ([long]$asset.size -ne $inventory[$name].Size -or $asset.state -ne 'uploaded') { throw "GitHub asset is incomplete: $name" }
        $digest = Get-PropertyValue -Object $asset -Name 'digest'
        if ($digest -and $digest -ine "sha256:$($inventory[$name].Sha256)") { throw "GitHub asset digest differs: $name" }
    }
    # Refresh notes on a retry, then publish only the fully verified draft.
    [void](Assert-GhDraft -Endpoint $releaseApi)
    Assert-GhTag -RepoApi $repoApi -EncodedTag $encodedTag -Commit $headCommit
    [void](Invoke-Gh -Arguments @('api', '--method', 'PATCH', $releaseApi, '-F', "body=@$($inventory['RELEASE-NOTES.md'].Path)"))
    [void](Invoke-Gh -Arguments @('api', '--method', 'PATCH', $releaseApi, '-F', 'draft=false'))
    $published = Get-GhJson -Endpoint $releaseApi
    if ($published.draft) { throw 'GitHub release is still a draft.' }
    Write-Output "Published $($published.html_url)"
} else {
    foreach ($variable in @('CI_API_V4_URL', 'CI_PROJECT_ID', 'CI_JOB_TOKEN')) {
        if (-not [Environment]::GetEnvironmentVariable($variable)) { throw "GitLab requires environment variable $variable." }
    }
    $apiUri = [Uri]$env:CI_API_V4_URL
    if (-not $apiUri.IsAbsoluteUri -or $apiUri.Scheme -ne 'https' -or $apiUri.UserInfo -or $apiUri.Query -or $apiUri.Fragment) {
        throw 'CI_API_V4_URL must be an HTTPS API URL without credentials, query, or fragment.'
    }
    $projectApi = $env:CI_API_V4_URL.TrimEnd('/') + '/projects/' + [Uri]::EscapeDataString($env:CI_PROJECT_ID)
    $remoteTag = Invoke-GitLab -Method Get -Uri "$projectApi/repository/tags/$encodedTag"
    if ($remoteTag.commit.id -cne $headCommit -or ($env:CI_COMMIT_SHA -and $env:CI_COMMIT_SHA -cne $headCommit)) {
        throw 'GitLab tag or pipeline commit does not match the current checkout.'
    }
    if ($env:CI_COMMIT_TAG -and $env:CI_COMMIT_TAG -cne $Tag) { throw 'GitLab pipeline tag differs from the requested tag.' }
    $releaseApi = "$projectApi/releases/$encodedTag"
    $existing = Invoke-GitLab -Method Get -Uri $releaseApi -AllowNotFound
    if ($null -ne $existing) { throw 'This GitLab release already exists. Existing releases are never replaced.' }
    $remoteFiles = @(Get-GitLabPackageFiles -ProjectApi $projectApi -EncodedTag $encodedTag)
    $present = Assert-GitLabFiles -RemoteFiles $remoteFiles
    $links = @()
    foreach ($name in ($inventory.Keys | Sort-Object)) {
        $url = "$projectApi/packages/generic/qwen-desk/$encodedTag/$([Uri]::EscapeDataString($name))"
        if (-not $present.Contains($name)) {
            Write-Output "Uploading GitLab package file: $name"
            [void](Invoke-GitLab -Method Put -Uri $url -File $inventory[$name].Path)
        }
        $links += @{ name = $name; url = $url; link_type = 'package' }
    }
    $remoteFiles = @(Get-GitLabPackageFiles -ProjectApi $projectApi -EncodedTag $encodedTag)
    [void](Assert-GitLabFiles -RemoteFiles $remoteFiles -RequireAll)
    $remoteTag = Invoke-GitLab -Method Get -Uri "$projectApi/repository/tags/$encodedTag"
    if ($remoteTag.commit.id -cne $headCommit) { throw 'GitLab tag moved during upload; release was not created.' }
    $body = @{
        name = $Tag
        tag_name = $Tag
        description = [IO.File]::ReadAllText($inventory['RELEASE-NOTES.md'].Path)
        assets = @{ links = $links }
    }
    $published = Invoke-GitLab -Method Post -Uri "$projectApi/releases" -Body $body
    Write-Output "Published GitLab release $($published.tag_name) with $($links.Count) persistent package links."
}
