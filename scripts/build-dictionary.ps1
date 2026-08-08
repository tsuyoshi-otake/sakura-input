[CmdletBinding()]
param(
    [string]$MozcSource,
    [string]$GlossarySource,
    [string]$OutputDirectory = (Join-Path $env:USERPROFILE 'tmp\sakura-input-dictionary-build'),
    [switch]$SkipDeterminismCheck,
    [switch]$UpdateCheckedInData
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$MozcRepository = 'google/mozc'
$MozcRevision = '3f235b4eb6fcff7d14ef5f0fb8ee56de7ee4c732'
$GlossaryRepository = 'systemexe-research-and-development/smile-chat'
$GlossaryRevision = 'b5cada441b41c207ab49bf2cd5f1d9c5614c5b92'

$RepositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
$CuratedTerms = Join-Path $RepositoryRoot 'data\curated-terms.tsv'
[IO.Directory]::CreateDirectory($OutputDirectory) | Out-Null

if (-not [IO.File]::Exists($CuratedTerms)) {
    throw "curated dictionary layer is missing: $CuratedTerms"
}

function Invoke-Rtk {
    param([Parameter(Mandatory)][string[]]$Arguments)

    $rtk = Get-Command rtk -ErrorAction SilentlyContinue
    if ($null -ne $rtk) {
        $output = @(& $rtk.Source @Arguments)
        $displayName = 'rtk'
    }
    else {
        $program = $Arguments[0]
        $programArguments = @($Arguments | Select-Object -Skip 1)
        $output = @(& $program @programArguments)
        $displayName = $program
    }
    if ($LASTEXITCODE -ne 0) {
        throw "$displayName $($Arguments -join ' ') failed with exit code $LASTEXITCODE"
    }
    foreach ($line in $output) {
        Write-Host $line
    }
}

function Get-RepositoryHead {
    param([Parameter(Mandatory)][string]$Path)

    $rtk = Get-Command rtk -ErrorAction SilentlyContinue
    if ($null -ne $rtk) {
        $output = @(& $rtk.Source git -C $Path rev-parse HEAD)
    }
    else {
        $output = @(& git -C $Path rev-parse HEAD)
    }
    if ($LASTEXITCODE -ne 0) {
        throw "cannot read Git revision for $Path"
    }
    return ($output[-1]).Trim()
}

function Resolve-PinnedSource {
    param(
        [string]$ProvidedPath,
        [Parameter(Mandatory)][string]$Repository,
        [Parameter(Mandatory)][string]$Revision,
        [Parameter(Mandatory)][string]$ManagedName,
        [Parameter(Mandatory)][string[]]$SparsePaths
    )

    if ($ProvidedPath) {
        $resolved = [IO.Path]::GetFullPath($ProvidedPath)
        if (-not [IO.Directory]::Exists($resolved)) {
            throw "source directory does not exist: $resolved"
        }
        if ((Get-RepositoryHead $resolved) -ne $Revision) {
            throw "$resolved is not checked out at pinned revision $Revision"
        }
        return $resolved
    }

    $sourceRoot = [IO.Path]::GetFullPath((Join-Path $env:USERPROFILE 'tmp\sakura-input-dictionary-sources'))
    [IO.Directory]::CreateDirectory($sourceRoot) | Out-Null
    $resolved = [IO.Path]::GetFullPath((Join-Path $sourceRoot $ManagedName))
    $expectedPrefix = $sourceRoot.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
    if (-not $resolved.StartsWith($expectedPrefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw "managed source escaped ~/tmp: $resolved"
    }

    if (-not [IO.Directory]::Exists($resolved)) {
        Invoke-Rtk -Arguments @('gh', 'repo', 'clone', $Repository, $resolved, '--', '--filter=blob:none', '--no-checkout')
        Invoke-Rtk -Arguments @('git', '-C', $resolved, 'sparse-checkout', 'init', '--cone')
        Invoke-Rtk -Arguments (@('git', '-C', $resolved, 'sparse-checkout', 'set') + $SparsePaths)
        Invoke-Rtk -Arguments @('git', '-C', $resolved, 'checkout', '--detach', $Revision)
    }

    $rtk = Get-Command rtk -ErrorAction SilentlyContinue
    if ($null -ne $rtk) {
        $dirty = @(& $rtk.Source git -C $resolved status --porcelain)
    }
    else {
        $dirty = @(& git -C $resolved status --porcelain)
    }
    if ($LASTEXITCODE -ne 0) {
        throw "cannot inspect managed source $resolved"
    }
    if ($dirty.Count -ne 0) {
        throw "managed source has local changes: $resolved"
    }
    if ((Get-RepositoryHead $resolved) -ne $Revision) {
        throw "managed source is not at pinned revision $Revision; remove or repair $resolved"
    }
    return $resolved
}

function Get-Sha256 {
    param([Parameter(Mandatory)][string]$Path)

    $stream = [IO.File]::OpenRead($Path)
    try {
        $algorithm = [Security.Cryptography.SHA256]::Create()
        try {
            $bytes = $algorithm.ComputeHash($stream)
            return ([Convert]::ToHexString($bytes)).ToLowerInvariant()
        }
        finally {
            $algorithm.Dispose()
        }
    }
    finally {
        $stream.Dispose()
    }
}

function Get-ArtifactRecord {
    param([Parameter(Mandatory)][string]$Path)

    $item = [IO.FileInfo]::new($Path)
    if (-not $item.Exists) {
        throw "expected artifact was not produced: $Path"
    }
    return [ordered]@{
        file = $item.Name
        bytes = $item.Length
        sha256 = Get-Sha256 $item.FullName
    }
}

function Invoke-BuildPass {
    param(
        [Parameter(Mandatory)][AllowEmptyString()][string]$Suffix,
        [Parameter(Mandatory)][string]$MozcDictionaryDirectory,
        [Parameter(Mandatory)][string]$GlossaryDirectory,
        [Parameter(Mandatory)][string]$ConnectionPath,
        [Parameter(Mandatory)][string]$CuratedTermsPath
    )

    $options = [IO.EnumerationOptions]::new()
    $options.RecurseSubdirectories = $false
    $options.IgnoreInaccessible = $false
    $options.AttributesToSkip = 0
    [string[]]$shards = [IO.Directory]::EnumerateFiles(
        $MozcDictionaryDirectory,
        'dictionary??.txt',
        $options
    )
    [Array]::Sort($shards, [StringComparer]::Ordinal)
    if ($shards.Count -ne 10) {
        throw "expected 10 Mozc dictionary shards, found $($shards.Count)"
    }

    $systemTsv = Join-Path $OutputDirectory "mozc-system$Suffix.tsv"
    $trimReport = Join-Path $OutputDirectory "mozc-trim$Suffix.report.json"
    $overlayTsv = Join-Path $OutputDirectory "it-terms$Suffix.tsv"
    $overlayReport = Join-Path $OutputDirectory "it-terms$Suffix.report.json"
    $dictionary = Join-Path $OutputDirectory "system$Suffix.dic"

    $mozcArguments = @('cargo', 'run', '--locked', '-p', 'dictc', '--bin', 'mozc-trim', '--')
    foreach ($shard in $shards) {
        $mozcArguments += @('--mozc-system', $shard)
    }
    $mozcArguments += @('--output', $systemTsv, '--report', $trimReport)
    Invoke-Rtk -Arguments $mozcArguments

    $glossaryArguments = @(
        'cargo', 'run', '--locked', '-p', 'dictc', '--bin', 'glossary-import', '--',
        '--glossary-dir', $GlossaryDirectory,
        '--glossary-revision', $GlossaryRevision
    )
    foreach ($shard in $shards) {
        $glossaryArguments += @('--mozc-system', $shard)
    }
    $glossaryArguments += @('--output', $overlayTsv, '--report', $overlayReport)
    Invoke-Rtk -Arguments $glossaryArguments

    Invoke-Rtk -Arguments @(
        'cargo', 'run', '--locked', '-p', 'dictc', '--bin', 'dictc', '--',
        '--system', $systemTsv,
        '--overlay', $overlayTsv,
        '--overlay', $CuratedTermsPath,
        '--mozc-connection', $ConnectionPath,
        '--output', $dictionary
    )

    return [ordered]@{
        system_tsv = $systemTsv
        trim_report = $trimReport
        overlay_tsv = $overlayTsv
        overlay_report = $overlayReport
        dictionary = $dictionary
    }
}

function Remove-BuildArtifact {
    param([Parameter(Mandatory)][string]$Path)

    $resolved = [IO.Path]::GetFullPath($Path)
    $expectedPrefix = $OutputDirectory.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
    if (-not $resolved.StartsWith($expectedPrefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw "refusing to remove artifact outside output directory: $resolved"
    }
    [IO.File]::Delete($resolved)
}

$stopwatch = [Diagnostics.Stopwatch]::StartNew()
$MozcSource = Resolve-PinnedSource -ProvidedPath $MozcSource -Repository $MozcRepository `
    -Revision $MozcRevision -ManagedName 'mozc' -SparsePaths @('LICENSE', 'src/data/dictionary_oss')
$GlossarySource = Resolve-PinnedSource -ProvidedPath $GlossarySource -Repository $GlossaryRepository `
    -Revision $GlossaryRevision -ManagedName 'smile-chat' -SparsePaths @('frontend/public')

$mozcDictionaryDirectory = Join-Path $MozcSource 'src\data\dictionary_oss'
$glossaryDirectory = Join-Path $GlossarySource 'frontend\public\glossaries'
$connectionPath = Join-Path $mozcDictionaryDirectory 'connection_single_column.txt'
$requiredLicenseFiles = @(
    (Join-Path $MozcSource 'LICENSE'),
    (Join-Path $mozcDictionaryDirectory 'README.txt'),
    (Join-Path $GlossarySource 'frontend\public\LICENSE')
)
foreach ($path in $requiredLicenseFiles) {
    if (-not [IO.File]::Exists($path)) {
        throw "required upstream license file is missing: $path"
    }
}

$env:CARGO_HTTP_CHECK_REVOKE = 'false'
Push-Location $RepositoryRoot
try {
    $primary = Invoke-BuildPass -Suffix '' -MozcDictionaryDirectory $mozcDictionaryDirectory `
        -GlossaryDirectory $glossaryDirectory -ConnectionPath $connectionPath `
        -CuratedTermsPath $CuratedTerms

    if (-not $SkipDeterminismCheck) {
        $repeat = Invoke-BuildPass -Suffix '.repeat' -MozcDictionaryDirectory $mozcDictionaryDirectory `
            -GlossaryDirectory $glossaryDirectory -ConnectionPath $connectionPath `
            -CuratedTermsPath $CuratedTerms
        foreach ($name in $primary.Keys) {
            $firstHash = Get-Sha256 $primary[$name]
            $secondHash = Get-Sha256 $repeat[$name]
            if ($firstHash -ne $secondHash) {
                throw "non-deterministic $name output: $firstHash != $secondHash"
            }
        }
        foreach ($path in $repeat.Values) {
            Remove-BuildArtifact $path
        }
    }

    $report = [ordered]@{
        schema_version = 1
        mozc_revision = $MozcRevision
        glossary_revision = $GlossaryRevision
        deterministic_repeat = -not $SkipDeterminismCheck
        inputs = [ordered]@{
            curated_terms = Get-ArtifactRecord $CuratedTerms
        }
        artifacts = [ordered]@{}
    }
    foreach ($name in $primary.Keys) {
        $report.artifacts[$name] = Get-ArtifactRecord $primary[$name]
    }
    $buildReport = Join-Path $OutputDirectory 'dictionary-build.report.json'
    [IO.File]::WriteAllText(
        $buildReport,
        (($report | ConvertTo-Json -Depth 5) + [Environment]::NewLine),
        [Text.UTF8Encoding]::new($false)
    )

    if ($UpdateCheckedInData) {
        [IO.File]::Copy($primary.overlay_tsv, (Join-Path $RepositoryRoot 'data\it-terms.tsv'), $true)
        [IO.File]::Copy($primary.overlay_report, (Join-Path $RepositoryRoot 'data\it-terms.report.json'), $true)
        [IO.File]::Copy($primary.trim_report, (Join-Path $RepositoryRoot 'data\mozc-trim.report.json'), $true)
        [IO.File]::Copy($buildReport, (Join-Path $RepositoryRoot 'data\dictionary-build.report.json'), $true)
    }
}
finally {
    Pop-Location
    $stopwatch.Stop()
}

Write-Host ("dictionary pipeline completed in {0:N2}s; output: {1}" -f $stopwatch.Elapsed.TotalSeconds, $OutputDirectory)
