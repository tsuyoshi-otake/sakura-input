[CmdletBinding()]
param(
    [string]$Dictionary = (Join-Path $env:USERPROFILE 'tmp\sakura-input-dictionary-build\system.dic'),
    [string]$ReportDirectory = (Join-Path $PSScriptRoot '..\artifacts\phase4'),
    [string]$DogfoodRecord = (Join-Path $PSScriptRoot '..\artifacts\phase4\dogfood.json'),
    [string]$ReconversionRecord = (Join-Path $PSScriptRoot '..\artifacts\phase4\reconversion-host-matrix.json'),
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
$reconversionPath = [IO.Path]::GetFullPath($ReconversionRecord)
$phase3ReportRoot = Join-Path $reportRoot 'phase3-regression'
$phase3SummaryPath = Join-Path $phase3ReportRoot 'phase3-summary.json'
$qualityReportPath = Join-Path $reportRoot 'quality.json'
$predictionReportPath = Join-Path $reportRoot 'prediction-latency.json'
$formatReportPath = Join-Path $reportRoot 'format-roundtrip.json'
$summaryPath = Join-Path $reportRoot 'phase4-summary.json'
$processCheck = Join-Path $repository 'ci\check-process-clean.ps1'
$steps = [Collections.Generic.List[object]]::new()
$failure = $null
$started = [DateTime]::UtcNow
$qualityStrictPassed = $false

[IO.Directory]::CreateDirectory($reportRoot) | Out-Null

function Confirm-ProcessClean {
    $output = @(& rtk proxy pwsh -NoProfile -ExecutionPolicy Bypass -File $processCheck -RepositoryRoot $repository)
    $exitCode = $LASTEXITCODE
    foreach ($line in $output) { Write-Host $line }
    if ($exitCode -eq 0) { return }

    Write-Warning 'A test left a Sakura or repository runner process; terminating parents first.'
    & rtk proxy pwsh -NoProfile -ExecutionPolicy Bypass -File $processCheck -RepositoryRoot $repository -Terminate
    if ($LASTEXITCODE -ne 0) { throw 'test processes survived the bounded cleanup attempt' }
    & rtk proxy pwsh -NoProfile -ExecutionPolicy Bypass -File $processCheck -RepositoryRoot $repository
    if ($LASTEXITCODE -ne 0) { throw 'process re-list was not clean after cleanup' }
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
    if ($CheckProcesses) { Confirm-ProcessClean }
    $steps.Add([ordered]@{
        name = $Name
        seconds = [Math]::Round($watch.Elapsed.TotalSeconds, 3)
        exit_code = $exitCode
        passed = $exitCode -eq 0
    })
    if ($exitCode -ne 0) { throw "$Name failed with exit code $exitCode" }
}

function Invoke-Assertion {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][scriptblock]$Check
    )

    Write-Host "==> $Name"
    $watch = [Diagnostics.Stopwatch]::StartNew()
    try {
        & $Check
        $watch.Stop()
        $steps.Add([ordered]@{
            name = $Name
            seconds = [Math]::Round($watch.Elapsed.TotalSeconds, 3)
            exit_code = 0
            passed = $true
        })
    }
    catch {
        $watch.Stop()
        $steps.Add([ordered]@{
            name = $Name
            seconds = [Math]::Round($watch.Elapsed.TotalSeconds, 3)
            exit_code = 1
            passed = $false
        })
        throw
    }
}

function Get-Sha256 {
    param([Parameter(Mandatory)][string]$Path)

    $stream = [IO.File]::OpenRead($Path)
    try {
        $algorithm = [Security.Cryptography.SHA256]::Create()
        try { return ([Convert]::ToHexString($algorithm.ComputeHash($stream))).ToLowerInvariant() }
        finally { $algorithm.Dispose() }
    }
    finally { $stream.Dispose() }
}

function Resolve-RecordArtifact {
    param(
        [Parameter(Mandatory)][string]$RecordPath,
        [Parameter(Mandatory)][string]$ArtifactPath
    )

    if ([string]::IsNullOrWhiteSpace($ArtifactPath)) { return $null }
    if ([IO.Path]::IsPathRooted($ArtifactPath)) { return [IO.Path]::GetFullPath($ArtifactPath) }
    return [IO.Path]::GetFullPath((Join-Path (Split-Path -Parent $RecordPath) $ArtifactPath))
}

function Test-HashedArtifact {
    param(
        [Parameter(Mandatory)][string]$RecordPath,
        [AllowEmptyString()][string]$ArtifactPath,
        [AllowEmptyString()][string]$ExpectedSha256
    )

    $resolved = Resolve-RecordArtifact -RecordPath $RecordPath -ArtifactPath $ArtifactPath
    if ($null -eq $resolved -or -not [IO.File]::Exists($resolved)) { return $false }
    if ($ExpectedSha256 -notmatch '^[0-9a-fA-F]{64}$') { return $false }
    return (Get-Sha256 $resolved) -eq $ExpectedSha256.ToLowerInvariant()
}

function Test-ReconversionRecord {
    param([Parameter(Mandatory)][string]$Path)

    $result = [ordered]@{
        path = $Path
        present = [IO.File]::Exists($Path)
        responsible_human = $null
        hosts = [ordered]@{}
        passed = $false
        reasons = [Collections.Generic.List[string]]::new()
    }
    if (-not $result.present) {
        $result.reasons.Add('Word/Notepad reconversion record is missing')
        return $result
    }
    try {
        $record = [IO.File]::ReadAllText($Path, [Text.Encoding]::UTF8) | ConvertFrom-Json
        $result.responsible_human = [string]$record.responsible_human
        if ($record.schema_version -ne 1 -or $record.phase -ne 4) {
            $result.reasons.Add('record schema/phase is not phase-4 schema version 1')
        }
        if ([string]::IsNullOrWhiteSpace($result.responsible_human)) {
            $result.reasons.Add('responsible_human is required')
        }
        $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
        foreach ($row in @($record.rows)) {
            $host = [string]$row.host
            if ($host -notin @('Word', 'Notepad')) {
                $result.reasons.Add("unsupported or missing reconversion host '$host'")
                continue
            }
            if (-not $seen.Add($host)) { $result.reasons.Add("duplicate reconversion host '$host'") }
            $observed = [DateTime]::Parse([string]$row.observed_at_utc, [Globalization.CultureInfo]::InvariantCulture, [Globalization.DateTimeStyles]::AssumeUniversal)
            if ($observed.ToUniversalTime() -gt [DateTime]::UtcNow.AddMinutes(5)) {
                $result.reasons.Add("$host observation is in the future")
            }
            $artifactPassed = Test-HashedArtifact -RecordPath $Path -ArtifactPath ([string]$row.evidence.path) -ExpectedSha256 ([string]$row.evidence.sha256)
            $rowPassed = [bool]$row.committed_text_reentered_conversion -and
                [bool]$row.candidate_ui_visible -and [bool]$row.commit_succeeded -and $artifactPassed
            if (-not $rowPassed) { $result.reasons.Add("$host reconversion row or hashed evidence is incomplete") }
            $result.hosts[$host] = [ordered]@{
                observed_at_utc = $observed.ToUniversalTime().ToString('o')
                evidence_verified = $artifactPassed
                passed = $rowPassed
            }
        }
        foreach ($required in @('Word', 'Notepad')) {
            if (-not $seen.Contains($required)) { $result.reasons.Add("missing $required reconversion row") }
        }
    }
    catch { $result.reasons.Add("record could not be graded: $($_.Exception.Message)") }
    $result.passed = $result.reasons.Count -eq 0
    return $result
}

function Test-DogfoodRecord {
    param([Parameter(Mandatory)][string]$Path)

    $result = [ordered]@{
        path = $Path
        present = [IO.File]::Exists($Path)
        responsible_human = $null
        elapsed_days = 0
        distinct_work_days = 0
        comparator_count = 0
        preference_statement = $null
        open_p0_p1 = $null
        steady_state_ipc_timeouts = $null
        diagnostics_verified = $false
        passed = $false
        reasons = [Collections.Generic.List[string]]::new()
    }
    if (-not $result.present) {
        $result.reasons.Add('dated two-week comparison dogfood record is missing')
        return $result
    }
    try {
        $record = [IO.File]::ReadAllText($Path, [Text.Encoding]::UTF8) | ConvertFrom-Json
        $result.responsible_human = [string]$record.responsible_human
        $result.preference_statement = [string]$record.preference_statement
        $result.open_p0_p1 = $record.open_p0_p1
        $result.steady_state_ipc_timeouts = $record.diagnostics.steady_state_ipc_timeouts
        if ($record.schema_version -ne 1 -or $record.phase -ne 4) {
            $result.reasons.Add('record schema/phase is not phase-4 schema version 1')
        }
        if ([string]::IsNullOrWhiteSpace($result.responsible_human)) {
            $result.reasons.Add('responsible_human is required')
        }
        if (-not [bool]$record.sakura_used_for_primary_work) {
            $result.reasons.Add('record does not attest Sakura use for primary work')
        }
        $imes = @($record.comparison_imes | ForEach-Object { [string]$_ } | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
        $hasSakura = $imes | Where-Object { $_ -eq 'Sakura Input' }
        $comparators = @($imes | Where-Object { $_ -ne 'Sakura Input' } | Select-Object -Unique)
        $result.comparator_count = $comparators.Count
        if (-not $hasSakura -or $comparators.Count -lt 1) {
            $result.reasons.Add('comparison_imes must name Sakura Input and at least one comparator')
        }
        $start = [DateTime]::ParseExact([string]$record.period_start, 'yyyy-MM-dd', [Globalization.CultureInfo]::InvariantCulture)
        $end = [DateTime]::ParseExact([string]$record.period_end, 'yyyy-MM-dd', [Globalization.CultureInfo]::InvariantCulture)
        $result.elapsed_days = [int](($end - $start).TotalDays + 1)
        if ($result.elapsed_days -lt 14) { $result.reasons.Add('dogfood period is shorter than fourteen elapsed days') }
        if ($end.Date -gt [DateTime]::UtcNow.Date) { $result.reasons.Add('dogfood period ends in the future') }
        $validDates = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
        foreach ($day in @($record.work_days)) {
            $date = [DateTime]::ParseExact([string]$day.date, 'yyyy-MM-dd', [Globalization.CultureInfo]::InvariantCulture)
            if ($date -lt $start -or $date -gt $end) { $result.reasons.Add("work day $($day.date) is outside the declared period") }
            if (@($day.evidence).Count -eq 0 -or [string]::IsNullOrWhiteSpace([string]$day.evidence[0])) {
                $result.reasons.Add("work day $($day.date) has no direct artifact reference")
            }
            if ($imes -notcontains [string]$day.ime) { $result.reasons.Add("work day $($day.date) names an undeclared IME") }
            [void]$validDates.Add($date.ToString('yyyy-MM-dd'))
        }
        $result.distinct_work_days = $validDates.Count
        if ($result.distinct_work_days -lt 10) { $result.reasons.Add('fewer than ten distinct work days have comparison evidence') }
        if ([string]::IsNullOrWhiteSpace($result.preference_statement)) { $result.reasons.Add('preference_statement is required') }
        if ($null -eq $record.open_p0_p1 -or [int64]$record.open_p0_p1 -ne 0) { $result.reasons.Add('open P0/P1 count is missing or non-zero') }
        if ($null -eq $record.diagnostics.steady_state_ipc_timeouts -or [int64]$record.diagnostics.steady_state_ipc_timeouts -ne 0) {
            $result.reasons.Add('steady-state IPC timeout count is missing or non-zero')
        }
        $result.diagnostics_verified = Test-HashedArtifact -RecordPath $Path -ArtifactPath ([string]$record.diagnostics.path) -ExpectedSha256 ([string]$record.diagnostics.sha256)
        if (-not $result.diagnostics_verified) { $result.reasons.Add('diagnostics artifact/hash is missing or does not match') }
    }
    catch { $result.reasons.Add("record could not be graded: $($_.Exception.Message)") }
    $result.passed = $result.reasons.Count -eq 0
    return $result
}

function Write-FormatReport {
    $fixtures = [Collections.Generic.List[object]]::new()
    foreach ($item in @(
        @{ format = 'ms-ime'; path = 'crates\sakura-settings\tests\fixtures\ms-ime.txt' },
        @{ format = 'atok'; path = 'crates\sakura-settings\tests\fixtures\atok.txt' },
        @{ format = 'mozc'; path = 'crates\sakura-settings\tests\fixtures\mozc.txt' }
    )) {
        $path = Join-Path $repository $item.path
        if (-not [IO.File]::Exists($path)) { throw "format fixture is missing: $path" }
        $fixtures.Add([ordered]@{
            format = $item.format
            path = $path
            sha256 = Get-Sha256 $path
        })
    }
    $report = [ordered]@{
        schema_version = 1
        phase = 4
        fixtures = $fixtures
        lossless_import = $true
        same_format_export_roundtrip = $true
        cross_format_export_roundtrip = $true
        windows_exports_utf16le = $true
        all_supported_pos_roundtrip = $true
        learning_export_clear = $true
        passed = $true
    }
    [IO.File]::WriteAllText(
        $formatReportPath,
        (($report | ConvertTo-Json -Depth 12) + [Environment]::NewLine),
        [Text.UTF8Encoding]::new($false)
    )
}

$env:CARGO_HTTP_CHECK_REVOKE = 'false'
Push-Location $repository
try {
    $phase3Arguments = @(
        'proxy', 'pwsh', '-NoProfile', '-ExecutionPolicy', 'Bypass',
        '-File', (Join-Path $repository 'scripts\verify-phase3.ps1'),
        '-Dictionary', $dictionaryPath,
        '-ReportDirectory', $phase3ReportRoot,
        '-FuzzIterations', [string]$FuzzIterations,
        '-EngineeringOnly'
    )
    if ($BuildDictionary) { $phase3Arguments += '-BuildDictionary' }
    Invoke-Gate -Name 'Phase 3 engineering regression' -Arguments $phase3Arguments
    Invoke-Gate -Name 'workspace formatting' -Arguments @('cargo', 'fmt', '--all', '--', '--check')
    Invoke-Gate -Name 'strict Phase 4 lint gate' -Arguments @(
        'cargo', 'clippy', '--locked', '--workspace', '--all-targets', '--', '-D', 'warnings'
    )
    Invoke-Gate -Name 'Phase 4 prediction, reconversion, settings, profiles and diagnostics tests' -CheckProcesses -Arguments @(
        'cargo', 'test', '--locked', '-p', 'sakura-engine', '-p', 'sakura-core',
        '-p', 'sakura-proto', '-p', 'sakura-tsf', '-p', 'sakura-renderer',
        '-p', 'sakura-settings', '-p', 'dictc', '--all-targets'
    )
    Invoke-Gate -Name 'file-backed MS-IME, ATOK and Mozc fixture round-trips' -CheckProcesses -Arguments @(
        'cargo', 'test', '--locked', '-p', 'sakura-settings', '--test', 'format_fixtures'
    )
    Invoke-Gate -Name 'learning view, export and clear terminal paths' -CheckProcesses -Arguments @(
        'cargo', 'test', '--locked', '-p', 'sakura-settings', 'learning::tests::'
    )
    Write-FormatReport
    Invoke-Gate -Name 'held-out quality and conversion latency' -CheckProcesses -Arguments @(
        'cargo', 'run', '--locked', '--release', '-p', 'dictc', '--bin', 'corpus-eval', '--',
        '--dictionary', $dictionaryPath,
        '--corpus', (Join-Path $repository 'corpus\held-out.tsv'),
        '--baseline', (Join-Path $repository 'corpus\mozc-baseline.tsv'),
        '--report', $qualityReportPath,
        '--latency-reading', 'きょうかいぎでせっていへんこうのけっかをくわしくせつめいする'
    )
    Invoke-Assertion -Name 'Phase 4 IT accuracy ratchet and Mozc parity' -Check {
        $quality = [IO.File]::ReadAllText($qualityReportPath, [Text.Encoding]::UTF8) | ConvertFrom-Json
        if ([int64]$quality.it.sakura_correct * 100 -lt [int64]$quality.it.total * 95) {
            throw 'IT held-out accuracy is below 95 percent'
        }
        if ([int64]$quality.it.sakura_correct -lt [int64]$quality.it.mozc_correct) {
            throw 'technical IT corpus accuracy is below the frozen Mozc baseline'
        }
        $script:qualityStrictPassed = $true
    }
    Invoke-Gate -Name 'prediction worker p99 latency' -CheckProcesses -Arguments @(
        'cargo', 'run', '--locked', '--release', '-p', 'sakura-engine', '--bin', 'prediction_eval', '--',
        '--dictionary', $dictionaryPath,
        '--report', $predictionReportPath
    )
    Invoke-Gate -Name 'learned choice over commit cache and domain prior' -CheckProcesses -Arguments @(
        'cargo', 'test', '--locked', '-p', 'sakura-engine',
        'explicit_learning_beats_conflicting_commit_cache_and_domain_coherence'
    )
}
catch {
    $failure = $_.Exception.Message
    Write-Error $failure
}
finally {
    Pop-Location
    try { Confirm-ProcessClean }
    catch { if ($null -eq $failure) { $failure = $_.Exception.Message } }

    $phase3 = if ([IO.File]::Exists($phase3SummaryPath)) {
        [IO.File]::ReadAllText($phase3SummaryPath, [Text.Encoding]::UTF8) | ConvertFrom-Json
    } else { $null }
    $quality = if ([IO.File]::Exists($qualityReportPath)) {
        [IO.File]::ReadAllText($qualityReportPath, [Text.Encoding]::UTF8) | ConvertFrom-Json
    } else { $null }
    $prediction = if ([IO.File]::Exists($predictionReportPath)) {
        [IO.File]::ReadAllText($predictionReportPath, [Text.Encoding]::UTF8) | ConvertFrom-Json
    } else { $null }
    $formats = if ([IO.File]::Exists($formatReportPath)) {
        [IO.File]::ReadAllText($formatReportPath, [Text.Encoding]::UTF8) | ConvertFrom-Json
    } else { $null }
    $reconversion = Test-ReconversionRecord -Path $reconversionPath
    $dogfood = Test-DogfoodRecord -Path $dogfoodPath
    $engineeringPassed = $null -eq $failure -and $null -ne $phase3 -and
        [bool]$phase3.engineering_passed -and $qualityStrictPassed -and
        $null -ne $prediction -and [bool]$prediction.passed -and
        $null -ne $formats -and [bool]$formats.passed
    $phase3Complete = $null -ne $phase3 -and [bool]$phase3.passed
    $passed = $engineeringPassed -and $phase3Complete -and
        [bool]$reconversion.passed -and [bool]$dogfood.passed
    $summary = [ordered]@{
        schema_version = 1
        phase = 4
        started_at_utc = $started.ToString('o')
        completed_at_utc = [DateTime]::UtcNow.ToString('o')
        dictionary = $dictionaryPath
        fuzz_iterations = $FuzzIterations
        steps = $steps
        phase3_regression = $phase3
        quality = $quality
        prediction_latency = $prediction
        format_roundtrip = $formats
        reconversion_host_matrix = $reconversion
        dogfood = $dogfood
        phase3_complete = $phase3Complete
        engineering_passed = $engineeringPassed
        failure = $failure
        passed = $passed
    }
    [IO.File]::WriteAllText(
        $summaryPath,
        (($summary | ConvertTo-Json -Depth 32) + [Environment]::NewLine),
        [Text.UTF8Encoding]::new($false)
    )
    Write-Host "Phase 4 report: $summaryPath"
}

if ($failure -or -not $engineeringPassed) { exit 1 }
if (-not $passed) {
    if ($EngineeringOnly) {
        Write-Warning 'Phase 4 engineering gates passed; prerequisite dogfood, Word/Notepad reconversion, or two-week comparison evidence is still pending, so Phase 4 is not complete.'
        exit 0
    }
    exit 1
}
Write-Host 'Phase 4 gate passed.'
