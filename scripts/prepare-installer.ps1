param([string]$Destination = (Join-Path $PSScriptRoot '../.work/tools/inno'))
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'release-common.ps1')
$compiler = Join-Path $Destination 'ISCC.exe'
if (Test-Path $compiler) { Write-Output ([IO.Path]::GetFullPath($compiler)); return }
$version = '6.7.1'
$expected = '4d11e8050b6185e0d49bd9e8cc661a7a59f44959a621d31d11033124c4e8a7b0'
New-Item -ItemType Directory -Force $Destination | Out-Null
$Destination = (Resolve-Path $Destination).Path
$installer = Join-Path $Destination "innosetup-$version.exe"
& curl.exe --fail --location --retry 3 --output $installer "https://github.com/jrsoftware/issrc/releases/download/is-6_7_1/innosetup-$version.exe"
if ($LASTEXITCODE -ne 0) { throw 'Inno Setup compiler download failed.' }
if ((Get-ReleaseSha256 $installer) -ne $expected) {
    Remove-Item $installer
    throw 'Inno Setup compiler checksum mismatch.'
}
$process = Start-Process -FilePath $installer -ArgumentList @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/CURRENTUSER', "/DIR=`"$Destination`"", '/NOICONS', '/TASKS=') -PassThru -Wait
if ($process.ExitCode -ne 0 -or -not (Test-Path $compiler)) { throw 'Inno Setup compiler preparation failed.' }
Remove-Item $installer
Write-Output ([IO.Path]::GetFullPath($compiler))
