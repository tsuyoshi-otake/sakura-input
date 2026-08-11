[CmdletBinding()]
param(
    [string]$Manifest,
    [string]$SourceDirectory,
    [switch]$ManifestOnly,
    [switch]$SelfTest
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if ([string]::IsNullOrWhiteSpace($Manifest)) {
    $Manifest = Join-Path $PSScriptRoot '..\corpus\context-prediction\source-manifest.json'
}

function Get-ExactProperties {
    param(
        [Parameter(Mandatory)]$Object,
        [Parameter(Mandatory)][string[]]$Names,
        [Parameter(Mandatory)][string]$Label
    )

    $actual = @($Object.PSObject.Properties.Name | Sort-Object)
    $expected = @($Names | Sort-Object)
    if (($actual -join "`n") -cne ($expected -join "`n")) {
        throw "$Label has unknown or missing properties: expected $($expected -join ', '); got $($actual -join ', ')"
    }
}

function Get-FileDigest {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][ValidateSet('sha1', 'sha256')][string]$Algorithm
    )

    $hasher = if ($Algorithm -ceq 'sha1') {
        [Security.Cryptography.SHA1]::Create()
    } else {
        [Security.Cryptography.SHA256]::Create()
    }
    $stream = [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try {
        $digest = $hasher.ComputeHash($stream)
        ([BitConverter]::ToString($digest)).Replace('-', '').ToLowerInvariant()
    } finally {
        $stream.Dispose()
        $hasher.Dispose()
    }
}

function Read-SourceManifest {
    param([Parameter(Mandatory)][string]$Path)

    $resolved = [IO.Path]::GetFullPath($Path)
    if (-not [IO.File]::Exists($resolved)) {
        throw "source manifest does not exist: $resolved"
    }
    $manifest = [IO.File]::ReadAllText($resolved, [Text.UTF8Encoding]::new($false)) | ConvertFrom-Json
    Get-ExactProperties $manifest @(
        'schema_version', 'source_id', 'database', 'language', 'snapshot', 'source_page',
        'usage_boundary', 'license_review_status', 'license_reference', 'files'
    ) 'source manifest'
    if ([int]$manifest.schema_version -ne 1 -or
        [string]$manifest.database -cne 'jawiki' -or
        [string]$manifest.language -cne 'ja' -or
        [string]$manifest.snapshot -notmatch '^\d{8}$' -or
        [string]$manifest.usage_boundary -cne 'offline-research-only-not-shipped' -or
        [string]$manifest.license_review_status -cne 'required-before-dataset-or-model-distribution') {
        throw 'source manifest identity or usage boundary is invalid'
    }
    if (@($manifest.files).Count -ne 3) {
        throw 'source manifest must contain exactly articles, index, and official-checksums files'
    }

    $roles = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($file in $manifest.files) {
        Get-ExactProperties $file @('role', 'name', 'url', 'bytes', 'hash_algorithm', 'hash') "source file $($file.role)"
        if (-not $roles.Add([string]$file.role) -or
            [string]$file.role -notin @('articles', 'index', 'official-checksums')) {
            throw "invalid or duplicate source role: $($file.role)"
        }
        if ([string]::IsNullOrWhiteSpace([string]$file.name) -or
            [IO.Path]::GetFileName([string]$file.name) -cne [string]$file.name) {
            throw "source file name must be one plain file name: $($file.name)"
        }
        if ([long]$file.bytes -le 0) {
            throw "source file size must be positive: $($file.name)"
        }
        $algorithm = [string]$file.hash_algorithm
        $hash = [string]$file.hash
        $expectedLength = if ($algorithm -ceq 'sha1') { 40 } elseif ($algorithm -ceq 'sha256') { 64 } else { 0 }
        if ($expectedLength -eq 0 -or $hash -cnotmatch "^[0-9a-f]{$expectedLength}$") {
            throw "source file hash is invalid: $($file.name)"
        }
        $expectedPrefix = "https://dumps.wikimedia.org/jawiki/$($manifest.snapshot)/"
        if (-not ([string]$file.url).StartsWith($expectedPrefix, [StringComparison]::Ordinal)) {
            throw "source URL is outside the pinned Wikimedia snapshot: $($file.url)"
        }
    }
    $manifest
}

function Test-SourceFiles {
    param(
        [Parameter(Mandatory)]$ParsedManifest,
        [Parameter(Mandatory)][string]$Directory
    )

    $root = [IO.Path]::GetFullPath($Directory).TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
    if (-not [IO.Directory]::Exists($root)) {
        throw "source directory does not exist: $root"
    }
    foreach ($file in $ParsedManifest.files) {
        $path = [IO.Path]::GetFullPath((Join-Path $root ([string]$file.name)))
        if (-not $path.StartsWith($root, [StringComparison]::OrdinalIgnoreCase)) {
            throw "source path escaped its directory: $path"
        }
        $item = [IO.FileInfo]::new($path)
        if (-not $item.Exists) {
            throw "source file does not exist: $path"
        }
        if ($item.Length -ne [long]$file.bytes) {
            throw "source file size mismatch for $($file.name): expected $($file.bytes), got $($item.Length)"
        }
        $algorithm = [string]$file.hash_algorithm
        $actual = Get-FileDigest $path $algorithm
        if ($actual -cne [string]$file.hash) {
            throw "source file $($algorithm.ToUpperInvariant()) mismatch for $($file.name): expected $($file.hash), got $actual"
        }
    }
}

function Invoke-SelfTest {
    $testRoot = Join-Path ([IO.Path]::GetTempPath()) ("sakura-context-source-test-" + [Guid]::NewGuid().ToString('N'))
    $resolvedRoot = [IO.Path]::GetFullPath($testRoot)
    [IO.Directory]::CreateDirectory($resolvedRoot) | Out-Null
    try {
        $files = @(
            [pscustomobject]@{ role = 'articles'; name = 'articles.bin'; bytes = [byte[]](1, 2, 3); algorithm = 'sha1' },
            [pscustomobject]@{ role = 'index'; name = 'index.bin'; bytes = [byte[]](4, 5); algorithm = 'sha1' },
            [pscustomobject]@{ role = 'official-checksums'; name = 'checksums.txt'; bytes = [Text.Encoding]::UTF8.GetBytes('checksums'); algorithm = 'sha256' }
        )
        $records = foreach ($file in $files) {
            $path = Join-Path $resolvedRoot $file.name
            [IO.File]::WriteAllBytes($path, $file.bytes)
            $hash = Get-FileDigest $path $file.algorithm
            [ordered]@{
                role = $file.role
                name = $file.name
                url = "https://dumps.wikimedia.org/jawiki/20260801/$($file.name)"
                bytes = $file.bytes.Length
                hash_algorithm = $file.algorithm
                hash = $hash
            }
        }
        $testManifest = [ordered]@{
            schema_version = 1
            source_id = 'wikimedia-jawiki-self-test'
            database = 'jawiki'
            language = 'ja'
            snapshot = '20260801'
            source_page = 'https://dumps.wikimedia.org/jawiki/20260801/'
            usage_boundary = 'offline-research-only-not-shipped'
            license_review_status = 'required-before-dataset-or-model-distribution'
            license_reference = 'https://dumps.wikimedia.org/legal.html'
            files = @($records)
        }
        $manifestPath = Join-Path $resolvedRoot 'manifest.json'
        [IO.File]::WriteAllText($manifestPath, ($testManifest | ConvertTo-Json -Depth 5), [Text.UTF8Encoding]::new($false))
        $parsed = Read-SourceManifest $manifestPath
        Test-SourceFiles $parsed $resolvedRoot

        [IO.File]::WriteAllBytes((Join-Path $resolvedRoot 'index.bin'), [byte[]](9, 9))
        try {
            Test-SourceFiles $parsed $resolvedRoot
            throw 'self-test failed: tampered file was accepted'
        } catch {
            if ($_.Exception.Message -ceq 'self-test failed: tampered file was accepted') { throw }
        }
        Write-Output 'context prediction source verifier self-test passed.'
    } finally {
        if ([IO.Directory]::Exists($resolvedRoot) -and
            $resolvedRoot.StartsWith([IO.Path]::GetTempPath(), [StringComparison]::OrdinalIgnoreCase)) {
            [IO.Directory]::Delete($resolvedRoot, $true)
        }
    }
}

if ($SelfTest) {
    Invoke-SelfTest
    return
}
$parsedManifest = Read-SourceManifest $Manifest
if ($ManifestOnly) {
    Write-Output "validated pinned context-prediction source manifest for $($parsedManifest.source_id)."
    return
}
if ([string]::IsNullOrWhiteSpace($SourceDirectory)) {
    throw '-SourceDirectory is required unless -SelfTest or -ManifestOnly is used'
}
Test-SourceFiles $parsedManifest $SourceDirectory
Write-Output "verified $(@($parsedManifest.files).Count) pinned context-prediction source files for $($parsedManifest.source_id)."
