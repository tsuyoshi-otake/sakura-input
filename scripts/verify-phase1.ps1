[CmdletBinding()]
param(
    [string]$ReportDirectory = (Join-Path $PSScriptRoot '..\artifacts\phase1'),

    [string]$HostMatrix = (Join-Path $PSScriptRoot '..\artifacts\phase1\host-matrix.json'),

    [switch]$EngineeringOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repository = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$reportRoot = [IO.Path]::GetFullPath($ReportDirectory)
$hostMatrixPath = [IO.Path]::GetFullPath($HostMatrix)
$summaryPath = Join-Path $reportRoot 'phase1-summary.json'
$latencyReportPath = Join-Path $reportRoot 'ipc-latency.json'
$dictionaryPath = Join-Path $repository 'artifacts\release\system.dic'
$processCheck = Join-Path $repository 'ci\check-process-clean.ps1'
$steps = [Collections.Generic.List[object]]::new()
$engineeringPassed = $true
$started = [DateTime]::UtcNow
[IO.Directory]::CreateDirectory($reportRoot) | Out-Null

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

function Confirm-ProcessClean {
    & rtk proxy pwsh -NoProfile -ExecutionPolicy Bypass -File $processCheck -RepositoryRoot $repository
    if ($LASTEXITCODE -eq 0) { return }
    Write-Warning 'A Phase 1 test left a repository-scoped process; terminating parents first.'
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
        $steps.Add([ordered]@{
            name = $Name
            seconds = [Math]::Round($watch.Elapsed.TotalSeconds, 3)
            exit_code = 0
            passed = $true
        })
    }
    catch {
        $steps.Add([ordered]@{
            name = $Name
            seconds = [Math]::Round($watch.Elapsed.TotalSeconds, 3)
            exit_code = 1
            passed = $false
            error = $_.Exception.Message
        })
        throw
    }
    finally { $watch.Stop() }
}

function Parse-Utc {
    param([Parameter(Mandatory)][string]$Value)

    return [DateTimeOffset]::Parse(
        $Value,
        [Globalization.CultureInfo]::InvariantCulture,
        [Globalization.DateTimeStyles]::RoundtripKind
    )
}

function Resolve-Evidence {
    param(
        [Parameter(Mandatory)][string]$RecordPath,
        [AllowEmptyString()][string]$RelativePath,
        [AllowEmptyString()][string]$ExpectedHash
    )

    if ([string]::IsNullOrWhiteSpace($RelativePath) -or [IO.Path]::IsPathRooted($RelativePath)) { return $false }
    $recordDirectory = [IO.Path]::GetFullPath((Split-Path -Parent $RecordPath))
    $candidate = [IO.Path]::GetFullPath((Join-Path $recordDirectory $RelativePath))
    $prefix = $recordDirectory.TrimEnd(
        [IO.Path]::DirectorySeparatorChar,
        [IO.Path]::AltDirectorySeparatorChar
    ) + [IO.Path]::DirectorySeparatorChar
    if (-not $candidate.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) { return $false }
    if (-not [IO.File]::Exists($candidate) -or $ExpectedHash -notmatch '^[0-9a-fA-F]{64}$') { return $false }
    return (Get-Sha256 $candidate) -ceq $ExpectedHash.ToLowerInvariant()
}

function Test-HostMatrix {
    $required = @(
        'notepad-typing', 'windows-terminal-typing', 'chrome-typing', 'width-policy',
        'engine-crash-recovery', 'focus-loss', 'elevated-host', 'install-uninstall'
    )
    $result = [ordered]@{
        path = $hostMatrixPath
        rows = [ordered]@{}
        reasons = [Collections.Generic.List[string]]::new()
        passed = $false
    }
    if (-not [IO.File]::Exists($hostMatrixPath)) {
        $result.reasons.Add('Phase 1 real-host matrix record is missing')
        return $result
    }

    try {
        $record = [IO.File]::ReadAllText($hostMatrixPath, [Text.Encoding]::UTF8) | ConvertFrom-Json
        if ($record.schema_version -ne 1 -or $record.phase -ne 1) {
            $result.reasons.Add('host matrix schema or phase is invalid')
        }
        if ([string]::IsNullOrWhiteSpace([string]$record.responsible_human)) {
            $result.reasons.Add('host matrix has no responsible human')
        }
        if ([string]::IsNullOrWhiteSpace([string]$record.host.machine) -or
            [string]::IsNullOrWhiteSpace([string]$record.host.windows_build)) {
            $result.reasons.Add('host matrix does not identify the machine and Windows build')
        }
        $startedAt = Parse-Utc ([string]$record.started_at_utc)
        $completedAt = Parse-Utc ([string]$record.completed_at_utc)
        if ($completedAt -lt $startedAt -or $completedAt -gt [DateTimeOffset]::UtcNow.AddMinutes(5)) {
            $result.reasons.Add('host matrix timestamps are impossible or in the future')
        }

        $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
        foreach ($row in @($record.rows)) {
            $id = [string]$row.id
            if ($id -notin $required) { $result.reasons.Add("unexpected host-matrix row '$id'"); continue }
            if (-not $seen.Add($id)) { $result.reasons.Add("duplicate host-matrix row '$id'"); continue }
            $evidencePassed = Resolve-Evidence -RecordPath $hostMatrixPath `
                -RelativePath ([string]$row.evidence.path) -ExpectedHash ([string]$row.evidence.sha256)
            $rowPassed = [string]$row.status -ceq 'pass' -and
                -not [string]::IsNullOrWhiteSpace([string]$row.observation) -and $evidencePassed
            $result.rows[$id] = [ordered]@{
                status = [string]$row.status
                evidence_verified = $evidencePassed
                passed = $rowPassed
            }
            if (-not $rowPassed) { $result.reasons.Add("host-matrix row '$id' is not directly evidenced as pass") }
        }
        foreach ($id in $required) {
            if (-not $seen.Contains($id)) { $result.reasons.Add("required host-matrix row '$id' is missing") }
        }
    }
    catch { $result.reasons.Add("host matrix could not be graded: $($_.Exception.Message)") }
    $result.passed = $result.reasons.Count -eq 0
    return $result
}

$oldLocalAppData = $env:LOCALAPPDATA
$oldLatencyReport = $env:SAKURA_IPC_LATENCY_REPORT
$oldPhase1Dictionary = $env:SAKURA_PHASE1_DICTIONARY
$runNonce = "{0}-{1}" -f $PID, [DateTime]::UtcNow.Ticks
$engineeringAppData = Join-Path $reportRoot "appdata-engineering-$runNonce"
$latencyAppData = Join-Path $reportRoot "appdata-latency-$runNonce"
$latencyEngineLog = Join-Path $latencyAppData 'SakuraInput\logs\engine.log'
[IO.Directory]::CreateDirectory($engineeringAppData) | Out-Null
[IO.Directory]::CreateDirectory($latencyAppData) | Out-Null

Push-Location $repository
try {
    $env:LOCALAPPDATA = $engineeringAppData
    if (-not [IO.File]::Exists($dictionaryPath)) { throw "release dictionary is missing: $dictionaryPath" }
    $env:SAKURA_PHASE1_DICTIONARY = $dictionaryPath
    & rtk proxy pwsh -NoProfile -ExecutionPolicy Bypass -File $processCheck -RepositoryRoot $repository
    if ($LASTEXITCODE -ne 0) { throw 'Phase 1 verifier requires a clean initial process state' }

    Invoke-Gate -Name 'workspace formatting' -Arguments @('cargo', 'fmt', '--all', '--', '--check')
    Invoke-Gate -Name 'strict workspace lint' -Arguments @('cargo', 'clippy', '--workspace', '--all-targets', '--', '-D', 'warnings')
    Invoke-Gate -Name 'workspace tests' -Arguments @('cargo', 'test', '--workspace') -CheckProcesses
    Invoke-Gate -Name 'debug workspace build for watchdog' -Arguments @('cargo', 'build', '--workspace', '--locked')
    Invoke-Gate -Name 'locked release workspace build' -Arguments @('cargo', 'build', '--workspace', '--release', '--locked')
    Invoke-Gate -Name 'core zero-allocation gate' -Arguments @('cargo', 'test', '-p', 'sakura-core', '--test', 'zero_alloc') -CheckProcesses
    Invoke-Gate -Name 'engine handoff zero-allocation gate' -Arguments @('cargo', 'test', '-p', 'sakura-engine', '--test', 'zero_alloc_dispatch') -CheckProcesses
    Invoke-Gate -Name 'named SIMD kernel agreement' -Arguments @('cargo', 'test', '-p', 'sakura-core', '--lib', '--', 'simd::', '--nocapture') -CheckProcesses

    $env:LOCALAPPDATA = $latencyAppData
    $env:SAKURA_IPC_LATENCY_REPORT = $latencyReportPath
    Invoke-Gate -Name 'real-pipe IPC p99 budget' -Arguments @(
        'cargo', 'test', '-p', 'sakura-engine', '--release', '--test', 'ipc_latency', '--',
        '--exact', 'a_keystroke_crosses_the_pipe_and_returns_inside_the_budget', '--ignored', '--nocapture'
    ) -CheckProcesses
    Invoke-Assertion -Name 'machine-readable IPC and ISA evidence' -Check {
        if (-not [IO.File]::Exists($latencyReportPath)) { throw 'IPC latency report was not written' }
        $latency = [IO.File]::ReadAllText($latencyReportPath) | ConvertFrom-Json
        if ($latency.schema_version -ne 1 -or $latency.samples -ne 5000 -or
            $latency.passed -ne $true -or [double]$latency.p99_us -ge 5000.0) {
            throw 'IPC latency report does not prove 5,000 samples with p99 below 5 ms'
        }
        if (-not [IO.File]::Exists($latencyEngineLog)) { throw 'engine startup log was not written' }
        $startupMatches = [regex]::Matches(
            [IO.File]::ReadAllText($latencyEngineLog),
            '(?m)^unix_ms=[0-9]+\tevent=startup\tcpu_tier=(avx|avx2|avx512bw)$'
        )
        if ($startupMatches.Count -ne 1) {
            throw "expected exactly one ISA startup record, found $($startupMatches.Count)"
        }
    }

    $env:LOCALAPPDATA = $engineeringAppData
    Invoke-Gate -Name 'real AppContainer pipe round trip' -Arguments @(
        'cargo', 'test', '-p', 'sakura-engine', '--test', 'appcontainer', '--',
        '--exact', 'the_pipe_is_reachable_from_a_real_appcontainer_token', '--ignored', '--nocapture'
    ) -CheckProcesses
    Invoke-Gate -Name 'real watchdog crash recovery' -Arguments @(
        'cargo', 'test', '-p', 'sakura-renderer', '--test', 'watchdog_recovery', '--',
        '--exact', 'a_killed_engine_comes_back_only_when_the_renderer_is_watching', '--ignored', '--nocapture'
    ) -CheckProcesses
    Invoke-Gate -Name 'warning-free installer package audit' -Arguments @(
        'proxy', 'pwsh', '-NoProfile', '-ExecutionPolicy', 'Bypass',
        '-File', (Join-Path $PSScriptRoot 'build-installer.ps1')
    ) -CheckProcesses
    Invoke-Assertion -Name 'release TSF DLL size budget' -Check {
        $dll = [IO.FileInfo]::new((Join-Path $repository 'target\x86_64-pc-windows-msvc\release\sakura_tsf.dll'))
        if (-not $dll.Exists) { throw 'release TSF DLL is missing' }
        if ($dll.Length -gt 1MB) { throw "release TSF DLL is $($dll.Length) bytes; 1 MiB maximum" }
    }
}
catch {
    $engineeringPassed = $false
    $steps.Add([ordered]@{
        name = 'engineering terminal'
        seconds = 0
        exit_code = 1
        passed = $false
        error = $_.Exception.Message
    })
}
finally {
    if ($null -eq $oldLocalAppData) { Remove-Item Env:LOCALAPPDATA -ErrorAction SilentlyContinue }
    else { $env:LOCALAPPDATA = $oldLocalAppData }
    if ($null -eq $oldLatencyReport) { Remove-Item Env:SAKURA_IPC_LATENCY_REPORT -ErrorAction SilentlyContinue }
    else { $env:SAKURA_IPC_LATENCY_REPORT = $oldLatencyReport }
    if ($null -eq $oldPhase1Dictionary) { Remove-Item Env:SAKURA_PHASE1_DICTIONARY -ErrorAction SilentlyContinue }
    else { $env:SAKURA_PHASE1_DICTIONARY = $oldPhase1Dictionary }
    Pop-Location
}

$hostMatrixResult = Test-HostMatrix
$strictPassed = $engineeringPassed -and $hostMatrixResult.passed
$summary = [ordered]@{
    schema_version = 1
    phase = 1
    generated_at_utc = [DateTime]::UtcNow.ToString('O')
    elapsed_seconds = [Math]::Round(([DateTime]::UtcNow - $started).TotalSeconds, 3)
    engineering = [ordered]@{
        passed = $engineeringPassed
        steps = @($steps)
        latency_report = $latencyReportPath
        engine_log = $latencyEngineLog
    }
    real_host_matrix = $hostMatrixResult
    engineering_only = [bool]$EngineeringOnly
    passed = if ($EngineeringOnly) { $engineeringPassed } else { $strictPassed }
}
$temporary = "$summaryPath.$PID.tmp"
[IO.File]::WriteAllText(
    $temporary,
    (($summary | ConvertTo-Json -Depth 12) + [Environment]::NewLine),
    [Text.UTF8Encoding]::new($false)
)
[IO.File]::Move($temporary, $summaryPath, $true)
$summary | ConvertTo-Json -Depth 12
if ($summary.passed) { exit 0 }
exit 1
