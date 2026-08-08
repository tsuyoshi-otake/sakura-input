[CmdletBinding()]
param(
    [string]$Dictionary = (Join-Path $env:USERPROFILE 'tmp\sakura-input-dictionary-build\system.dic'),
    [string]$ReportDirectory = (Join-Path $PSScriptRoot '..\artifacts\phase2'),
    [ValidateRange(1, 1000000000)]
    [int]$FuzzIterations = 100000,
    [switch]$BuildDictionary
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repository = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$dictionaryPath = [IO.Path]::GetFullPath($Dictionary)
$reportRoot = [IO.Path]::GetFullPath($ReportDirectory)
$processCheck = Join-Path $repository 'ci\check-process-clean.ps1'
$qualityReport = Join-Path $reportRoot 'quality.json'
$engineReport = Join-Path $reportRoot 'engine-resources.json'
$rendererReport = Join-Path $reportRoot 'renderer-resources.json'
$candidateReport = Join-Path $reportRoot 'candidate-uia.json'
$summaryPath = Join-Path $reportRoot 'phase2-summary.json'
$latencyReading = 'きょうかいぎでせっていへんこうのけっかをくわしくせつめいする'
$steps = [Collections.Generic.List[object]]::new()
$failure = $null
$started = [DateTime]::UtcNow

[IO.Directory]::CreateDirectory($reportRoot) | Out-Null

function Get-Sha256 {
    param([Parameter(Mandatory)][string]$Path)

    $stream = [IO.File]::OpenRead($Path)
    try {
        $algorithm = [Security.Cryptography.SHA256]::Create()
        try {
            return ([Convert]::ToHexString($algorithm.ComputeHash($stream))).ToLowerInvariant()
        }
        finally {
            $algorithm.Dispose()
        }
    }
    finally {
        $stream.Dispose()
    }
}

function Confirm-ProcessClean {
    $output = @(& rtk proxy pwsh -NoProfile -ExecutionPolicy Bypass -File $processCheck -RepositoryRoot $repository)
    $exitCode = $LASTEXITCODE
    foreach ($line in $output) {
        Write-Host $line
    }
    if ($exitCode -eq 0) {
        return
    }

    Write-Warning 'A test left a Sakura or repository runner process; terminating parents first.'
    & rtk proxy pwsh -NoProfile -ExecutionPolicy Bypass -File $processCheck -RepositoryRoot $repository -Terminate
    if ($LASTEXITCODE -ne 0) {
        throw 'test processes survived the bounded cleanup attempt'
    }
    & rtk proxy pwsh -NoProfile -ExecutionPolicy Bypass -File $processCheck -RepositoryRoot $repository
    if ($LASTEXITCODE -ne 0) {
        throw 'process re-list was not clean after cleanup'
    }
    throw 'the preceding test leaked a process; cleanup succeeded but the gate fails'
}

function Invoke-Gate {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][string[]]$Arguments,
        [switch]$CheckProcesses
    )

    Write-Host "==> $Name"
    $watch = [Diagnostics.Stopwatch]::StartNew()
    & rtk @Arguments
    $exitCode = $LASTEXITCODE
    $watch.Stop()
    if ($CheckProcesses) {
        Confirm-ProcessClean
    }
    $steps.Add([ordered]@{
        name = $Name
        seconds = [Math]::Round($watch.Elapsed.TotalSeconds, 3)
        exit_code = $exitCode
        passed = $exitCode -eq 0
    })
    if ($exitCode -ne 0) {
        throw "$Name failed with exit code $exitCode"
    }
}

function Assert-DictionaryProvenance {
    if (-not [IO.File]::Exists($dictionaryPath)) {
        throw "dictionary does not exist: $dictionaryPath (use -BuildDictionary)"
    }
    $checkedInPath = Join-Path $repository 'data\dictionary-build.report.json'
    $checkedIn = Get-Content -LiteralPath $checkedInPath -Raw -Encoding utf8 | ConvertFrom-Json
    if (-not $checkedIn.deterministic_repeat) {
        throw 'checked-in dictionary report does not prove a deterministic repeat build'
    }
    $actual = [IO.FileInfo]::new($dictionaryPath)
    $actualHash = Get-Sha256 $dictionaryPath
    if ([int64]$checkedIn.artifacts.dictionary.bytes -ne $actual.Length -or
        [string]$checkedIn.artifacts.dictionary.sha256 -ne $actualHash) {
        throw "dictionary does not match data/dictionary-build.report.json: $actualHash ($($actual.Length) bytes)"
    }
}

$env:CARGO_HTTP_CHECK_REVOKE = 'false'
Push-Location $repository
try {
    if ($BuildDictionary) {
        Invoke-Gate -Name 'deterministic pinned dictionary build' -Arguments @(
            'proxy', 'pwsh', '-NoProfile', '-ExecutionPolicy', 'Bypass',
            '-File', (Join-Path $repository 'scripts\build-dictionary.ps1'),
            '-OutputDirectory', (Split-Path -Parent $dictionaryPath)
        )
    }
    Assert-DictionaryProvenance

    Invoke-Gate -Name 'protocol, core, engine and dictionary compiler tests' -CheckProcesses -Arguments @(
        'cargo', 'test', '--locked', '-p', 'sakura-proto', '-p', 'sakura-core',
        '-p', 'sakura-engine', '-p', 'dictc', '--all-targets'
    )
    Invoke-Gate -Name 'candidate renderer and UI-less TSF contracts' -CheckProcesses -Arguments @(
        'cargo', 'test', '--locked', '-p', 'sakura-renderer', '-p', 'sakura-tsf'
    )
    Invoke-Gate -Name 'engine conversion handoff allocation gate' -CheckProcesses -Arguments @(
        'cargo', 'test', '--locked', '-p', 'sakura-engine', '--test', 'zero_alloc_dispatch'
    )
    Invoke-Gate -Name 'real pipe conversion round trip' -CheckProcesses -Arguments @(
        'cargo', 'test', '--locked', '-p', 'sakura-engine', '--test', 'pipe_round_trip'
    )

    $env:SAKURA_FUZZ_ITERS = [string]$FuzzIterations
    $env:SAKURA_FUZZ_SHARD = '0'
    Invoke-Gate -Name 'hostile dictionary image campaign' -CheckProcesses -Arguments @(
        'cargo', 'test', '--locked', '-p', 'dictc', '--release', '--test',
        'dictionary_robustness', '--', '--ignored', '--nocapture'
    )

    Invoke-Gate -Name 'held-out quality and conversion latency' -CheckProcesses -Arguments @(
        'cargo', 'run', '--locked', '--release', '-p', 'dictc', '--bin', 'corpus-eval', '--',
        '--dictionary', $dictionaryPath,
        '--corpus', (Join-Path $repository 'corpus\held-out.tsv'),
        '--baseline', (Join-Path $repository 'corpus\mozc-baseline.tsv'),
        '--report', $qualityReport,
        '--latency-reading', $latencyReading
    )

    $env:SAKURA_PHASE2_DICTIONARY = $dictionaryPath
    $env:SAKURA_RESOURCE_REPORT = $engineReport
    Invoke-Gate -Name 'real engine image and private-working-set budgets' -CheckProcesses -Arguments @(
        'cargo', 'test', '--locked', '-p', 'sakura-engine', '--release', '--test',
        'resource_budget', '--', '--ignored', '--nocapture'
    )

    Invoke-Gate -Name 'release engine and renderer build' -Arguments @(
        'cargo', 'build', '--locked', '-p', 'sakura-engine', '-p', 'sakura-renderer', '--release'
    )
    $env:SAKURA_RENDERER_RESOURCE_REPORT = $rendererReport
    Invoke-Gate -Name 'real candidate renderer private-working-set budget' -CheckProcesses -Arguments @(
        'cargo', 'test', '--locked', '-p', 'sakura-renderer', '--release', '--test',
        'resource_budget', '--', '--ignored', '--nocapture'
    )
    $env:SAKURA_CANDIDATE_UIA_REPORT = $candidateReport
    Invoke-Gate -Name 'real candidate paging, digit selection, caret placement and UIA' -CheckProcesses -Arguments @(
        'cargo', 'test', '--locked', '-p', 'sakura-renderer', '--release', '--test',
        'candidate_uia', '--', '--ignored', '--nocapture'
    )
}
catch {
    $failure = $_.Exception.Message
    Write-Error $failure
}
finally {
    Pop-Location
    try {
        Confirm-ProcessClean
    }
    catch {
        if ($null -eq $failure) {
            $failure = $_.Exception.Message
        }
    }

    $quality = if ([IO.File]::Exists($qualityReport)) {
        Get-Content -LiteralPath $qualityReport -Raw -Encoding utf8 | ConvertFrom-Json
    }
    else { $null }
    $engine = if ([IO.File]::Exists($engineReport)) {
        Get-Content -LiteralPath $engineReport -Raw -Encoding utf8 | ConvertFrom-Json
    }
    else { $null }
    $renderer = if ([IO.File]::Exists($rendererReport)) {
        Get-Content -LiteralPath $rendererReport -Raw -Encoding utf8 | ConvertFrom-Json
    }
    else { $null }
    $candidates = if ([IO.File]::Exists($candidateReport)) {
        Get-Content -LiteralPath $candidateReport -Raw -Encoding utf8 | ConvertFrom-Json
    }
    else { $null }
    $passed = $null -eq $failure -and
        $null -ne $quality -and $quality.passed -and
        $null -ne $engine -and $engine.passed -and
        $null -ne $renderer -and $renderer.passed -and
        $null -ne $candidates -and $candidates.passed
    $dictionaryRecord = if ([IO.File]::Exists($dictionaryPath)) {
        [ordered]@{
            path = $dictionaryPath
            bytes = [IO.FileInfo]::new($dictionaryPath).Length
            sha256 = Get-Sha256 $dictionaryPath
        }
    }
    else { $null }
    $summary = [ordered]@{
        schema_version = 1
        phase = 2
        started_at_utc = $started.ToString('o')
        completed_at_utc = [DateTime]::UtcNow.ToString('o')
        fuzz_iterations = $FuzzIterations
        dictionary = $dictionaryRecord
        steps = $steps
        quality = $quality
        engine_resources = $engine
        renderer_resources = $renderer
        candidate_uia = $candidates
        failure = $failure
        passed = $passed
    }
    [IO.File]::WriteAllText(
        $summaryPath,
        (($summary | ConvertTo-Json -Depth 20) + [Environment]::NewLine),
        [Text.UTF8Encoding]::new($false)
    )
    Write-Host "Phase 2 report: $summaryPath"
}

if ($failure -or -not $passed) {
    exit 1
}
Write-Host 'Phase 2 automated gate passed.'
