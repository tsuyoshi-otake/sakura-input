[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$Version,

    [string]$Installer = (Join-Path $PSScriptRoot '..\installer\out\sakura_setup.exe'),

    [string]$Output = (Join-Path $PSScriptRoot '..\installer\out\release-manifest.txt')
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if ($Version -notmatch '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$') {
    throw 'version must be canonical major.minor.patch'
}
$installerPath = [IO.Path]::GetFullPath($Installer)
$outputPath = [IO.Path]::GetFullPath($Output)
$item = [IO.FileInfo]::new($installerPath)
if (-not $item.Exists) { throw "installer is missing: $installerPath" }
if ($item.Length -lt 1 -or $item.Length -gt 200MB) { throw 'installer size is outside the updater bound' }

$stream = [IO.File]::OpenRead($installerPath)
try {
    $algorithm = [Security.Cryptography.SHA256]::Create()
    try { $sha256 = [Convert]::ToHexString($algorithm.ComputeHash($stream)).ToLowerInvariant() }
    finally { $algorithm.Dispose() }
}
finally { $stream.Dispose() }

$url = "https://github.com/tsuyoshi-otake/sakura-input/releases/download/v$Version/sakura_setup.exe"
$manifest = "schema=1`nversion=$Version`ninstaller_url=$url`nsha256=$sha256`nsize=$($item.Length)`n"
[IO.Directory]::CreateDirectory((Split-Path -Parent $outputPath)) | Out-Null
$temporary = "$outputPath.$PID.tmp"
[IO.File]::WriteAllText($temporary, $manifest, [Text.UTF8Encoding]::new($false))
[IO.File]::Move($temporary, $outputPath, $true)
Write-Host "release manifest: $outputPath"
