[CmdletBinding()]
param(
    [string]$ArtifactDirectory = (Join-Path $PSScriptRoot '..\release-bundle'),

    [string]$Version = '1.0.0',

    [Parameter(Mandatory)]
    [ValidateNotNullOrEmpty()]
    [string]$ExpectedSubject,

    [string]$Notes = (Join-Path $PSScriptRoot '..\docs\release-notes-v1.0.0.md'),

    [switch]$Publish
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repositoryName = 'tsuyoshi-otake/sakura-input'
if ($Version -notmatch '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$') {
    throw 'version must be canonical major.minor.patch'
}
$tag = "v$Version"
if (-not $Publish) {
    throw 'publishing is an external mutation; pass -Publish only after the Phase 5 strict gate is green'
}

$bundle = [IO.Path]::GetFullPath($ArtifactDirectory)
$installer = Join-Path $bundle 'sakura_setup.exe'
$manifest = Join-Path $bundle 'release-manifest.txt'
$notesPath = [IO.Path]::GetFullPath($Notes)
foreach ($path in @($installer, $manifest, $notesPath)) {
    if (-not [IO.File]::Exists($path)) { throw "release input is missing: $path" }
}

if ($null -eq (Get-Command rtk -ErrorAction SilentlyContinue)) {
    throw 'rtk is required; all GitHub operations in this repository go through rtk gh'
}

function Invoke-RtkGh {
    param(
        [Parameter(Mandatory)][string[]]$Arguments,
        [switch]$AllowFailure
    )

    $output = @(& rtk gh @Arguments)
    $exitCode = $LASTEXITCODE
    if (-not $AllowFailure -and $exitCode -ne 0) {
        throw "rtk gh $($Arguments -join ' ') failed with exit code $exitCode"
    }
    return [pscustomobject]@{ ExitCode = $exitCode; Lines = $output }
}

function Get-Sha256 {
    param([Parameter(Mandatory)][string]$Path)
    $stream = [IO.File]::OpenRead($Path)
    try {
        $algorithm = [Security.Cryptography.SHA256]::Create()
        try { return [Convert]::ToHexString($algorithm.ComputeHash($stream)).ToLowerInvariant() }
        finally { $algorithm.Dispose() }
    }
    finally { $stream.Dispose() }
}

function Assert-Manifest {
    param(
        [Parameter(Mandatory)][string]$ManifestPath,
        [Parameter(Mandatory)][string]$InstallerPath
    )

    $text = [IO.File]::ReadAllText($ManifestPath, [Text.Encoding]::UTF8)
    $expected = @(
        'schema=1',
        "version=$Version",
        "installer_url=https://github.com/$repositoryName/releases/download/$tag/sakura_setup.exe",
        "sha256=$(Get-Sha256 $InstallerPath)",
        "size=$([IO.FileInfo]::new($InstallerPath).Length)"
    ) -join "`n"
    if ($text -cne "$expected`n") {
        throw 'release manifest is not the exact canonical description of the installer'
    }
}

Assert-Manifest -ManifestPath $manifest -InstallerPath $installer
& pwsh -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'verify-release-signatures.ps1') `
    -ExpectedSubject $ExpectedSubject -Files $installer
if ($LASTEXITCODE -ne 0) { throw 'local installer signature verification failed' }

& rtk git rev-parse --verify "refs/tags/$tag^{commit}" | Out-Null
if ($LASTEXITCODE -ne 0) { throw "local tag $tag is missing" }
Invoke-RtkGh @('auth', 'status') | Out-Null
$repository = Invoke-RtkGh @('repo', 'view', '--json', 'nameWithOwner')
$identity = ($repository.Lines -join "`n" | ConvertFrom-Json).nameWithOwner
if ($identity -cne $repositoryName) {
    throw "authenticated repository is '$identity', expected '$repositoryName'"
}

$existing = Invoke-RtkGh -Arguments @('release', 'view', $tag, '--repo', $repositoryName) -AllowFailure
if ($existing.ExitCode -eq 0) { throw "release $tag already exists" }
# Distinguish a genuine absent release from authentication/network failure.
Invoke-RtkGh @('api', "repos/$repositoryName") | Out-Null

$temporaryRoot = [IO.Path]::GetFullPath((Join-Path $env:USERPROFILE 'tmp'))
[IO.Directory]::CreateDirectory($temporaryRoot) | Out-Null
$readback = [IO.Path]::GetFullPath((Join-Path $temporaryRoot "sakura-input-release-readback-$PID"))
$expectedPrefix = $temporaryRoot.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
if (-not $readback.StartsWith($expectedPrefix, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'temporary release readback directory escaped ~/tmp'
}
[IO.Directory]::CreateDirectory($readback) | Out-Null

$watch = [Diagnostics.Stopwatch]::StartNew()
$draftCreated = $false
try {
    Invoke-RtkGh @(
        'release', 'create', $tag,
        $installer, $manifest,
        '--repo', $repositoryName,
        '--verify-tag',
        '--draft',
        '--title', "Sakura Input $Version",
        '--notes-file', $notesPath
    ) | Out-Null
    $draftCreated = $true

    Invoke-RtkGh @(
        'release', 'download', $tag,
        '--repo', $repositoryName,
        '--dir', $readback,
        '--pattern', 'sakura_setup.exe',
        '--pattern', 'release-manifest.txt'
    ) | Out-Null
    $downloadedInstaller = Join-Path $readback 'sakura_setup.exe'
    $downloadedManifest = Join-Path $readback 'release-manifest.txt'
    Assert-Manifest -ManifestPath $downloadedManifest -InstallerPath $downloadedInstaller
    if ((Get-Sha256 $installer) -cne (Get-Sha256 $downloadedInstaller)) {
        throw 'downloaded draft installer differs from the signed local artifact'
    }
    & pwsh -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'verify-release-signatures.ps1') `
        -ExpectedSubject $ExpectedSubject -Files $downloadedInstaller
    if ($LASTEXITCODE -ne 0) { throw 'downloaded draft installer signature verification failed' }

    $draft = Invoke-RtkGh @(
        'release', 'view', $tag, '--repo', $repositoryName,
        '--json', 'tagName,isDraft,isPrerelease,assets,url'
    )
    $draftRecord = $draft.Lines -join "`n" | ConvertFrom-Json
    if ($draftRecord.tagName -cne $tag -or -not $draftRecord.isDraft -or $draftRecord.isPrerelease) {
        throw 'GitHub draft release state is not the requested stable tag'
    }
    $assets = @($draftRecord.assets)
    if ($assets.Count -ne 2 -or @($assets.name | Sort-Object) -join ',' -cne 'release-manifest.txt,sakura_setup.exe') {
        throw 'draft release does not contain exactly the two updater assets'
    }

    Invoke-RtkGh @('release', 'edit', $tag, '--repo', $repositoryName, '--draft=false') | Out-Null
    $published = Invoke-RtkGh @(
        'release', 'view', $tag, '--repo', $repositoryName,
        '--json', 'tagName,isDraft,isPrerelease,assets,url,publishedAt'
    )
    $publishedRecord = $published.Lines -join "`n" | ConvertFrom-Json
    if ($publishedRecord.tagName -cne $tag -or $publishedRecord.isDraft -or $publishedRecord.isPrerelease -or [string]::IsNullOrWhiteSpace($publishedRecord.publishedAt)) {
        throw 'release readback did not reach the explicit published terminal state'
    }
    Write-Host "published and read back: $($publishedRecord.url)"
}
catch {
    if ($draftCreated) {
        Write-Warning "publication stopped; $tag remains a draft for inspection"
    }
    throw
}
finally {
    $watch.Stop()
    if ([IO.Directory]::Exists($readback)) {
        [IO.Directory]::Delete($readback, $true)
    }
    Write-Host ("publish workflow elapsed seconds: {0:N3}" -f $watch.Elapsed.TotalSeconds)
}
