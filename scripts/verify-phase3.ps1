[CmdletBinding()]
param(
    [string]$Dictionary = (Join-Path $env:USERPROFILE 'tmp\sakura-input-dictionary-build\system.dic'),
    [string]$ReportDirectory = (Join-Path $PSScriptRoot '..\artifacts\phase3'),
    [string]$DogfoodRecord = (Join-Path $PSScriptRoot '..\artifacts\phase3\dogfood.json'),
    [ValidateRange(1, 1000000000)]
    [int]$FuzzIterations = 100000,
    [switch]$BuildDictionary,
    [switch]$EngineeringOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repository = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$dictionaryPath = [IO.Path]::GetFullPath($Dictionary)
$reportRoot = [IO.Path]::GetFullPath($ReportDirectory)
$dogfoodPath = [IO.Path]::GetFullPath($DogfoodRecord)
$phase2ReportRoot = Join-Path $reportRoot 'phase2-regression'
$phase2SummaryPath = Join-Path $phase2ReportRoot 'phase2-summary.json'
$summaryPath = Join-Path $reportRoot 'phase3-summary.json'
$processCheck = Join-Path $repository 'ci\check-process-clean.ps1'
$steps = [Collections.Generic.List[object]]::new()
$failure = $null
$started = [DateTime]::UtcNow

[IO.Directory]::CreateDirectory($reportRoot) | Out-Null

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

function Test-DogfoodRecord {
    param([Parameter(Mandatory)][string]$Path)

    $result = [ordered]@{
        path = $Path
        present = [IO.File]::Exists($Path)
        responsible_human = $null
        distinct_work_days = 0
        elapsed_days = 0
        pass_through_fallback_events = $null
        open_p0_p1 = $null
        engine_log_present = $false
        passed = $false
        reasons = [Collections.Generic.List[string]]::new()
    }
    if (-not $result.present) {
        $result.reasons.Add('dated dogfood record is missing')
        return $result
    }

    try {
        $record = Get-Content -LiteralPath $Path -Raw -Encoding utf8 | ConvertFrom-Json
        $result.responsible_human = [string]$record.responsible_human
        $result.pass_through_fallback_events = $record.pass_through_fallback_events
        $result.open_p0_p1 = $record.open_p0_p1
        if ($record.schema_version -ne 1 -or $record.phase -ne 3) {
            $result.reasons.Add('record schema/phase is not phase-3 schema version 1')
        }
        if ([string]::IsNullOrWhiteSpace($result.responsible_human)) {
            $result.reasons.Add('responsible_human is required')
        }
        if (-not [bool]$record.default_ime) {
            $result.reasons.Add('record does not attest that Sakura was the default IME')
        }

        $start = [DateTime]::ParseExact([string]$record.period_start, 'yyyy-MM-dd', [Globalization.CultureInfo]::InvariantCulture)
        $end = [DateTime]::ParseExact([string]$record.period_end, 'yyyy-MM-dd', [Globalization.CultureInfo]::InvariantCulture)
        $result.elapsed_days = [int](($end - $start).TotalDays + 1)
        if ($result.elapsed_days -lt 7) {
            $result.reasons.Add('dogfood period is shorter than seven elapsed days')
        }

        $validDates = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
        foreach ($day in @($record.work_days)) {
            $date = [DateTime]::ParseExact([string]$day.date, 'yyyy-MM-dd', [Globalization.CultureInfo]::InvariantCulture)
            if ($date -lt $start -or $date -gt $end) {
                $result.reasons.Add("work day $($day.date) is outside the declared period")
            }
            if (@($day.evidence).Count -eq 0 -or [string]::IsNullOrWhiteSpace([string]$day.evidence[0])) {
                $result.reasons.Add("work day $($day.date) has no direct artifact reference")
            }
            [void]$validDates.Add($date.ToString('yyyy-MM-dd'))
        }
        $result.distinct_work_days = $validDates.Count
        if ($result.distinct_work_days -lt 5) {
            $result.reasons.Add('fewer than five distinct work days have artifact evidence')
        }
        if ($null -eq $record.pass_through_fallback_events -or [int64]$record.pass_through_fallback_events -ne 0) {
            $result.reasons.Add('pass-through fallback count is missing or non-zero')
        }
        if ($null -eq $record.open_p0_p1 -or [int64]$record.open_p0_p1 -ne 0) {
            $result.reasons.Add('open P0/P1 count is missing or non-zero')
        }
        $engineLog = [string]$record.engine_log.path
        if (-not [string]::IsNullOrWhiteSpace($engineLog)) {
            $resolvedLog = if ([IO.Path]::IsPathRooted($engineLog)) {
                [IO.Path]::GetFullPath($engineLog)
            }
            else {
                [IO.Path]::GetFullPath((Join-Path (Split-Path -Parent $Path) $engineLog))
            }
            $result.engine_log_present = [IO.File]::Exists($resolvedLog)
        }
        if (-not $result.engine_log_present -or [string]::IsNullOrWhiteSpace([string]$record.engine_log.sha256)) {
            $result.reasons.Add('engine log artifact and SHA-256 are required')
        }
    }
    catch {
        $result.reasons.Add("record could not be graded: $($_.Exception.Message)")
    }
    $result.passed = $result.reasons.Count -eq 0
    return $result
}

$env:CARGO_HTTP_CHECK_REVOKE = 'false'
Push-Location $repository
try {
    $phase2Arguments = @(
        'proxy', 'pwsh', '-NoProfile', '-ExecutionPolicy', 'Bypass',
        '-File', (Join-Path $repository 'scripts\verify-phase2.ps1'),
        '-Dictionary', $dictionaryPath,
        '-ReportDirectory', $phase2ReportRoot,
        '-FuzzIterations', [string]$FuzzIterations
    )
    if ($BuildDictionary) {
        $phase2Arguments += '-BuildDictionary'
    }
    Invoke-Gate -Name 'Phase 2 quality, latency, footprint and UI regression' -Arguments $phase2Arguments
    Invoke-Gate -Name 'workspace formatting' -Arguments @(
        'cargo', 'fmt', '--all', '--', '--check'
    )
    Invoke-Gate -Name 'strict Phase 3 lint gate' -Arguments @(
        'cargo', 'clippy', '--locked', '-p', 'sakura-engine', '-p', 'sakura-core',
        '-p', 'sakura-proto', '-p', 'sakura-tsf', '-p', 'dictc', '--all-targets',
        '--', '-D', 'warnings'
    )
    Invoke-Gate -Name 'Phase 3 editing, replay, learning, context, user dictionary and upgrades' -CheckProcesses -Arguments @(
        'cargo', 'test', '--locked', '-p', 'sakura-engine', '-p', 'sakura-core',
        '-p', 'sakura-proto', '-p', 'sakura-tsf', '-p', 'dictc', '--all-targets'
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

    $phase2 = if ([IO.File]::Exists($phase2SummaryPath)) {
        Get-Content -LiteralPath $phase2SummaryPath -Raw -Encoding utf8 | ConvertFrom-Json
    }
    else { $null }
    $dogfood = Test-DogfoodRecord -Path $dogfoodPath
    $engineeringPassed = $null -eq $failure -and $null -ne $phase2 -and [bool]$phase2.passed
    $passed = $engineeringPassed -and [bool]$dogfood.passed
    $summary = [ordered]@{
        schema_version = 1
        phase = 3
        started_at_utc = $started.ToString('o')
        completed_at_utc = [DateTime]::UtcNow.ToString('o')
        dictionary = $dictionaryPath
        fuzz_iterations = $FuzzIterations
        steps = $steps
        phase2_regression = $phase2
        dogfood = $dogfood
        engineering_passed = $engineeringPassed
        failure = $failure
        passed = $passed
    }
    [IO.File]::WriteAllText(
        $summaryPath,
        (($summary | ConvertTo-Json -Depth 24) + [Environment]::NewLine),
        [Text.UTF8Encoding]::new($false)
    )
    Write-Host "Phase 3 report: $summaryPath"
}

if ($failure -or -not $engineeringPassed) {
    exit 1
}
if (-not $passed) {
    if ($EngineeringOnly) {
        Write-Warning 'Phase 3 engineering gates passed; the elapsed dogfood gate is still pending, so Phase 3 is not complete.'
        exit 0
    }
    exit 1
}
Write-Host 'Phase 3 gate passed.'
