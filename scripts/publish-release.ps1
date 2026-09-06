[CmdletBinding()]
param(
    [string]$ArtifactDirectory = (Join-Path $PSScriptRoot '..\release-candidate'),
    [Parameter(Mandatory)][ValidateNotNullOrEmpty()][string]$Version,
    [Parameter(Mandatory)][ValidateNotNullOrEmpty()][string]$ProtectedPrivateKey,
    [Parameter(Mandatory)][ValidateNotNullOrEmpty()][string]$KeyId,
    [string]$Notes,
    [switch]$Publish
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$repository = 'tsuyoshi-otake/sakura-input'
if ($Version -notmatch '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$') { throw 'version must be canonical major.minor.patch' }
if ([string]::IsNullOrWhiteSpace($Notes)) { $Notes = Join-Path $PSScriptRoot "..\docs\release-notes-v$Version.md" }
$tag = "v$Version"
if (-not $Publish) { throw 'publishing is an external mutation; pass -Publish only after all v2 gates are green' }
if ($null -eq (Get-Command gh -ErrorAction SilentlyContinue)) { throw 'gh is required for GitHub operations' }

$root = [IO.Path]::GetFullPath($ArtifactDirectory)
$installer = Join-Path $root 'sakura_setup.exe'
$manifest = Join-Path $root 'release-manifest-v2.txt'
foreach ($path in @($installer,$manifest,$Notes,$ProtectedPrivateKey)) { if (-not [IO.File]::Exists([IO.Path]::GetFullPath($path))) { throw "release input is missing: $path" } }
$candidateNames = @([IO.Directory]::EnumerateFiles($root, '*', [IO.SearchOption]::TopDirectoryOnly) | ForEach-Object { [IO.Path]::GetFileName($_) } | Sort-Object)
if ((@($candidateNames) -join ',') -cne 'release-manifest-v2.txt,sakura_setup.exe') {
    throw 'release candidate must contain exactly installer and canonical v2 manifest before local signing'
}
$manifestBytes = [IO.File]::ReadAllBytes($manifest)
if ($manifestBytes.Length -lt 4 -or $manifestBytes[0] -eq 0xef -or $manifestBytes -contains 0) { throw 'candidate manifest has BOM, NUL, or invalid length' }
$manifestText = [Text.UTF8Encoding]::new($false, $true).GetString($manifestBytes)
$manifestLines = $manifestText.Split([char]10)
$fieldNames = @('schema','product','repository','channel','platform','trust_epoch','release_sequence','version','tag','source_commit','asset_name','installer_url','sha256','size','authenticode','minimum_updater_version','expires_unix')
if ($manifestLines.Count -ne 18 -or $manifestLines[17] -cne '' -or @(0..16 | Where-Object { $manifestLines[$_] -notmatch "^$($fieldNames[$_])=[^\r\n=]+$" }).Count -ne 0) { throw 'candidate manifest is not the exact v2 field order' }
$sourceCommit = ([regex]::Match($manifestLines[9], '^source_commit=([0-9a-f]{40})$')).Groups[1].Value
$manifestTag = ([regex]::Match($manifestLines[8], '^tag=(v[0-9]+\.[0-9]+\.[0-9]+)$')).Groups[1].Value
if ([string]::IsNullOrWhiteSpace($sourceCommit) -or $manifestTag -cne $tag) { throw 'candidate manifest tag/source commit is invalid' }
$verify = Join-Path $PSScriptRoot 'verify-update-manifest.ps1'
$sign = Join-Path $PSScriptRoot 'sign-update-manifest.ps1'

function Invoke-Gh { param([Parameter(Mandatory)][string[]]$Arguments,[switch]$AllowFailure)
    $lines = @(& gh @Arguments); $code = $LASTEXITCODE
    if (-not $AllowFailure -and $code -ne 0) { throw "gh $($Arguments -join ' ') failed with exit code $code" }
    [pscustomobject]@{ ExitCode=$code; Lines=$lines }
}

# Verify the candidate's GitHub artifact provenance before creating a public
# release. The application signature is deliberately created locally below.
$remoteCommitRecord = Invoke-Gh @('api',"repos/$repository/commits/$tag")
$remoteCommit = (($remoteCommitRecord.Lines -join "`n") | ConvertFrom-Json).sha
if ([string]$remoteCommit -cne $sourceCommit) { throw "remote tag commit does not match manifest source_commit: $remoteCommit vs $sourceCommit" }
Invoke-Gh @('attestation','verify',$installer,'--repo',$repository,'--signer-workflow','tsuyoshi-otake/sakura-input/.github/workflows/release.yml','--source-digest',$sourceCommit,'--signer-digest',$sourceCommit) | Out-Null
Invoke-Gh @('attestation','verify',$manifest,'--repo',$repository,'--signer-workflow','tsuyoshi-otake/sakura-input/.github/workflows/release.yml','--source-digest',$sourceCommit,'--signer-digest',$sourceCommit) | Out-Null

& pwsh -NoProfile -ExecutionPolicy Bypass -File $sign -Manifest $manifest -ProtectedPrivateKey $ProtectedPrivateKey -KeyId $KeyId -Output (Join-Path $root 'release-manifest-v2.sig')
if ($LASTEXITCODE -ne 0) { throw 'local update-manifest signing failed' }
$signature = Join-Path $root 'release-manifest-v2.sig'
& pwsh -NoProfile -ExecutionPolicy Bypass -File $verify -Manifest $manifest -Signature $signature -Keyring (Join-Path $PSScriptRoot '..\data\update-signing\public-keys-v1.txt') -VerifyInstaller -Installer $installer
if ($LASTEXITCODE -ne 0) { throw 'local update-manifest verification failed' }

$tempRoot = [IO.Path]::GetFullPath((Join-Path ([Environment]::GetFolderPath('UserProfile')) 'tmp'))
[IO.Directory]::CreateDirectory($tempRoot) | Out-Null
$readback = [IO.Path]::GetFullPath((Join-Path $tempRoot "sakura-input-release-readback-$PID-$([Guid]::NewGuid().ToString('N'))"))
if (-not $readback.StartsWith($tempRoot.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) { throw 'release readback must remain under the owned temporary root' }
[IO.Directory]::CreateDirectory($readback) | Out-Null
$draftCreated = $false
try {
    Invoke-Gh @('release','create',$tag,$installer,$manifest,$signature,'--repo',$repository,'--verify-tag','--draft','--title',"Sakura Input $Version",'--notes-file',[IO.Path]::GetFullPath($Notes)) | Out-Null
    $draftCreated = $true
    Invoke-Gh @('release','download',$tag,'--repo',$repository,'--dir',$readback,'--pattern','sakura_setup.exe','--pattern','release-manifest-v2.txt','--pattern','release-manifest-v2.sig') | Out-Null
    $ri = Join-Path $readback 'sakura_setup.exe'; $rm = Join-Path $readback 'release-manifest-v2.txt'; $rs = Join-Path $readback 'release-manifest-v2.sig'
    & pwsh -NoProfile -ExecutionPolicy Bypass -File $verify -Manifest $rm -Signature $rs -Keyring (Join-Path $PSScriptRoot '..\data\update-signing\public-keys-v1.txt') -VerifyInstaller -Installer $ri
    if ($LASTEXITCODE -ne 0) { throw 'downloaded draft failed v2 verification' }
    $draft = Invoke-Gh @('release','view',$tag,'--repo',$repository,'--json','tagName,isDraft,isPrerelease,assets,url')
    $record = ($draft.Lines -join "`n") | ConvertFrom-Json
    if ($record.tagName -cne $tag -or -not $record.isDraft -or $record.isPrerelease) { throw 'draft release state is not the requested stable tag' }
    $assets = @($record.assets)
    if ($assets.Count -ne 3 -or (@($assets.name | Sort-Object) -join ',') -cne 'release-manifest-v2.sig,release-manifest-v2.txt,sakura_setup.exe') { throw 'draft does not contain exactly the three v2 updater assets' }
    Invoke-Gh @('release','edit',$tag,'--repo',$repository,'--draft=false') | Out-Null
    $published = Invoke-Gh @('release','view',$tag,'--repo',$repository,'--json','tagName,isDraft,isPrerelease,publishedAt,assets,url')
    $final = ($published.Lines -join "`n") | ConvertFrom-Json
    if ($final.tagName -cne $tag -or $final.isDraft -or $final.isPrerelease -or [string]::IsNullOrWhiteSpace($final.publishedAt)) { throw 'published release did not reach an explicit stable terminal state' }
    $finalAssets = @($final.assets)
    if ($finalAssets.Count -ne 3 -or (@($finalAssets.name | Sort-Object) -join ',') -cne 'release-manifest-v2.sig,release-manifest-v2.txt,sakura_setup.exe') { throw 'published release asset set changed after draft verification' }
    Write-Host "published and read back: $($final.url)"
}
catch {
    if ($draftCreated) { Write-Warning "publication stopped; $tag remains a draft for inspection" }
    throw
}
finally {
    if ([IO.Directory]::Exists($readback)) {
        $resolvedReadback = [IO.Path]::GetFullPath($readback)
        if ($resolvedReadback -cne $readback -or -not $resolvedReadback.StartsWith($tempRoot.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) { throw 'refusing cleanup outside the owned release readback directory' }
        if (([IO.File]::GetAttributes($resolvedReadback) -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw 'refusing recursive cleanup of a reparse-point readback directory' }
        [IO.Directory]::Delete($resolvedReadback,$true)
    }
}
