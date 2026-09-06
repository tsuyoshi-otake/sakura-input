[CmdletBinding()]
param(
    [string]$JarPath,
    [ValidateRange(1, 900)][int]$TimeoutSeconds = 180,
    [ValidateRange(1, 16)][int]$Workers = 2,
    [string[]]$Configs = @(
        'SpaceKeyDispatch-small.cfg', 'SpaceKeyDispatch-unfenced.cfg',
        'SpaceKeyDispatch-boundary.cfg', 'SpaceKeyDispatch-actors1.cfg',
        'SpaceKeyDispatch-actors3.cfg', 'SpaceKeyDispatch-reach-dual.cfg',
        'SpaceKeyDispatch-reach-convert.cfg', 'SpaceKeyDispatch-reach-predict.cfg',
        'SpaceKeyDispatch-reach-insert.cfg'
    ),
    [string]$OutputRoot,
    [switch]$SelfTest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$repoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$expected = [ordered]@{
    'SpaceKeyDispatch-small.cfg' = ''
    'SpaceKeyDispatch-unfenced.cfg' = ''
    'SpaceKeyDispatch-boundary.cfg' = ''
    'SpaceKeyDispatch-actors1.cfg' = ''
    'SpaceKeyDispatch-actors3.cfg' = ''
    'SpaceKeyDispatch-reach-dual.cfg' = 'NeverDualEffect'
    'SpaceKeyDispatch-reach-convert.cfg' = 'NeverConverts'
    'SpaceKeyDispatch-reach-predict.cfg' = 'NeverConvertedFromPredicting'
    'SpaceKeyDispatch-reach-insert.cfg' = 'NeverInserts'
}

function Get-TextHash([string]$Path) {
    # Only newline encoding is normalized across Git checkouts.
    $bytes = [Text.Encoding]::UTF8.GetBytes([IO.File]::ReadAllText($Path).Replace("`r`n", "`n"))
    return [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($bytes)).ToLowerInvariant()
}

function Invoke-BoundedProcess([string]$Executable, [string[]]$Arguments, [int]$Milliseconds) {
    $start = [Diagnostics.ProcessStartInfo]::new($Executable)
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    foreach ($argument in $Arguments) { $start.ArgumentList.Add($argument) }
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $start
    $started = $false
    $watch = [Diagnostics.Stopwatch]::StartNew()
    try {
        $started = $process.Start()
        if (-not $started) { throw 'Process failed to start' }
        $outTask = $process.StandardOutput.ReadToEndAsync()
        $errTask = $process.StandardError.ReadToEndAsync()
        $timedOut = -not $process.WaitForExit($Milliseconds)
        if ($timedOut) { $process.Kill($true) }
        if (-not $process.WaitForExit(10000)) { throw 'Owned process did not exit after termination' }
        if (-not [Threading.Tasks.Task]::WaitAll([Threading.Tasks.Task[]]@($outTask, $errTask), 10000)) {
            throw 'Owned process output did not close'
        }
        return [ordered]@{
            exit_code = $process.ExitCode; timed_out = $timedOut
            elapsed_ms = $watch.Elapsed.TotalMilliseconds; pid = $process.Id
            stdout = $outTask.Result; stderr = $errTask.Result; process_exited = $process.HasExited
        }
    }
    finally {
        if ($started -and -not $process.HasExited) {
            $process.Kill($true)
            if (-not $process.WaitForExit(10000)) { throw 'Owned process survived cleanup' }
        }
        $process.Dispose()
    }
}

function Get-TlcOutcome($Result, [string]$ExpectedInvariant) {
    if ($Result.timed_out) { return 'INCONCLUSIVE' }
    if ($ExpectedInvariant) {
        $violation = 'Error: Invariant ' + $ExpectedInvariant + ' is violated.'
        if ($Result.exit_code -eq 12 -and $Result.stdout.Contains($violation) -and
            $Result.stdout.Contains('The behavior up to this point is:') -and
            $Result.stdout.Contains('State 1:')) { return 'PASS' }
    }
    elseif ($Result.exit_code -eq 0 -and
        $Result.stdout.Contains('Model checking completed. No error has been found.') -and
        $Result.stdout -match '(?m)^([0-9,]+) states generated, ([0-9,]+) distinct states found, 0 states left on queue\.' -and
        [long]$Matches[1].Replace(',', '') -gt 0 -and [long]$Matches[2].Replace(',', '') -gt 0 -and
        $Result.stdout -notmatch '(?m)^Error:') { return 'PASS' }
    return 'FAIL'
}

if ($SelfTest) {
    $complete = "Model checking completed. No error has been found.`n12 states generated, 9 distinct states found, 0 states left on queue."
    $trace = "Error: Invariant NeverInserts is violated.`nThe behavior up to this point is:`nState 1:"
    $cases = @(
        @(0, $false, $complete, '', 'PASS'),
        @(1, $false, $complete, '', 'FAIL'),
        @(0, $false, '', '', 'FAIL'),
        @(0, $false, ($complete.Replace('12 states', '0 states').Replace('9 distinct', '0 distinct')), '', 'FAIL'),
        @(0, $false, ($complete.Replace('0 states left', '1 states left')), '', 'FAIL'),
        @(0, $true, $complete, '', 'INCONCLUSIVE'),
        @(12, $false, $trace, 'NeverInserts', 'PASS'),
        @(12, $false, $trace, 'NeverConverts', 'FAIL'),
        @(0, $false, $trace, 'NeverInserts', 'FAIL'),
        @(12, $false, ($trace.Replace('State 1:', '')), 'NeverInserts', 'FAIL'),
        @(12, $true, $trace, 'NeverInserts', 'INCONCLUSIVE'),
        @(12, $false, $trace, '', 'FAIL')
    )
    foreach ($case in $cases) {
        $actual = Get-TlcOutcome @{ exit_code = $case[0]; timed_out = $case[1]; stdout = $case[2] } $case[3]
        if ($actual -cne $case[4]) { throw "Outcome regression: wanted $($case[4]), got $actual" }
    }
    $pwsh = (Get-Process -Id $PID).Path
    $normal = Invoke-BoundedProcess $pwsh @('-NoProfile', '-Command', "Write-Output 'owned-normal'; exit 23") 10000
    if ($normal.exit_code -ne 23 -or $normal.timed_out -or -not $normal.stdout.Contains('owned-normal')) {
        throw 'Child exit/output lost'
    }
    $timeout = Invoke-BoundedProcess $pwsh @('-NoProfile', '-Command', 'Start-Sleep -Seconds 30') 200
    if (-not $timeout.timed_out -or -not $timeout.process_exited) { throw 'Timeout did not terminate owned child' }
    foreach ($child in @($normal, $timeout)) {
        if (Get-Process -Id $child.pid -ErrorAction SilentlyContinue) { throw 'Owned child remains' }
    }
    Write-Host "PASS: TLC outcome controls ($($cases.Count)) and two owned process lifecycle probes"
    return
}

if (-not $JarPath -or -not [IO.File]::Exists($JarPath)) { throw 'An existing TLA+ tools JarPath is required' }
$jar = [IO.Path]::GetFullPath($JarPath)
$jarHash = (Get-FileHash -LiteralPath $jar -Algorithm SHA256).Hash.ToLowerInvariant()
if ($jarHash -ne '936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88') {
    throw 'TLC jar hash differs from the reviewed pin; no run performed'
}
$Configs = @($Configs | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim() })
if ($Configs.Count -eq 0) { throw 'No TLC configurations requested' }
$seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
foreach ($config in $Configs) {
    if ($expected.Keys -cnotcontains $config -or -not $seen.Add($config)) { throw "Unknown or duplicate TLC configuration: $config" }
}
if (-not $OutputRoot) { $OutputRoot = Join-Path $repoRoot 'verification/space-key-dispatch/tlc' }
$output = [IO.Path]::GetFullPath($OutputRoot)
[void][IO.Directory]::CreateDirectory($output)
$runId = [Guid]::NewGuid().ToString('N')
$runRoot = Join-Path $output $runId
if ([IO.Directory]::Exists($runRoot) -or [IO.File]::Exists($runRoot)) { throw 'Run identity collision' }
[void][IO.Directory]::CreateDirectory($runRoot)

$inputPaths = @('scripts/verify-space-key-dispatch-tlc.ps1', 'verification/tla/SpaceKeyDispatch.tla') +
    @($Configs | ForEach-Object { "verification/tla/$_" })
$inputs = [ordered]@{}
$snapshotDir = Join-Path $runRoot 'inputs'
[void][IO.Directory]::CreateDirectory($snapshotDir)
$jarSnapshot = Join-Path $snapshotDir 'tla2tools.jar'
[IO.File]::Copy($jar, $jarSnapshot, $false)
if ((Get-FileHash -LiteralPath $jarSnapshot -Algorithm SHA256).Hash.ToLowerInvariant() -ne $jarHash) {
    throw 'TLC jar changed while being snapshotted; no run performed'
}
foreach ($path in $inputPaths) {
    $snapshot = Join-Path $snapshotDir ([IO.Path]::GetFileName($path))
    [IO.File]::WriteAllBytes($snapshot, [IO.File]::ReadAllBytes((Join-Path $repoRoot $path)))
    $inputs[$path] = Get-TextHash $snapshot
}
# TLC consumes the recorded model/config snapshots, not files that a later
# editor action can change during the run. The runner snapshot is provenance.
$revision = & git -C $repoRoot rev-parse HEAD
if ($LASTEXITCODE -ne 0) { throw 'Cannot resolve source revision' }
$java = (Get-Command java.exe -ErrorAction Stop).Source
$javaVersion = Invoke-BoundedProcess $java @('-version') 10000
if ($javaVersion.exit_code -ne 0 -or $javaVersion.timed_out) { throw 'Cannot determine Java runtime' }
$manifest = [ordered]@{
    schema = 1; domain = 'space-key-dispatch-tlc'; run_id = $runId
    started_utc = [DateTime]::UtcNow.ToString('o'); source_revision = $revision.Trim()
    source_identity = 'revision plus normalized working-tree input hashes; revision alone is not the evaluated tree'
    scope = 'TLA model/configuration only; no Rust, COM, physical routing or REQ-SPACE-09 teardown-credit proof'
    hash_encoding = 'UTF-8 text with CRLF normalized to LF; jar/logs use raw SHA-256'
    inputs = $inputs; jar_sha256 = $jarHash; java_version = ($javaVersion.stdout + $javaVersion.stderr).Trim()
    workers = $Workers; seed = '20260816'; timeout_seconds = $TimeoutSeconds
    status = 'NOT_RUN'; results = @()
}
$manifestPath = Join-Path $runRoot 'results.json'
function Save-Manifest { [IO.File]::WriteAllText($manifestPath, ($manifest | ConvertTo-Json -Depth 12) + "`n") }
foreach ($config in $Configs) {
    $manifest.results += [ordered]@{ config = $config; expected_invariant = $expected[$config]; status = 'NOT_RUN' }
}
Save-Manifest
foreach ($entry in $manifest.results) {
    $runDir = Join-Path $runRoot ([IO.Path]::GetFileNameWithoutExtension($entry.config))
    [void][IO.Directory]::CreateDirectory($runDir)
    $arguments = @('-cp', $jarSnapshot, 'tlc2.TLC', '-config', (Join-Path $snapshotDir $entry.config),
        '-workers', "$Workers", '-coverage', '1', '-fp', '0', '-seed', $manifest.seed,
        '-metadir', (Join-Path $runDir 'states'), (Join-Path $snapshotDir 'SpaceKeyDispatch.tla'))
    try {
        $result = Invoke-BoundedProcess $java $arguments ($TimeoutSeconds * 1000)
        foreach ($stream in @('stdout', 'stderr')) {
            $log = Join-Path $runDir "$stream.log"
            [IO.File]::WriteAllText($log, $result[$stream])
            $entry["${stream}_sha256"] = (Get-FileHash -LiteralPath $log -Algorithm SHA256).Hash.ToLowerInvariant()
        }
        foreach ($field in @('exit_code', 'timed_out', 'elapsed_ms', 'pid', 'process_exited')) { $entry[$field] = $result[$field] }
        if ($result.stdout -match '(?m)^([0-9,]+) states generated, ([0-9,]+) distinct states found, ([0-9,]+) states left on queue\.') {
            $entry.states_generated = [long]$Matches[1].Replace(',', '')
            $entry.distinct_states = [long]$Matches[2].Replace(',', '')
            $entry.states_left = [long]$Matches[3].Replace(',', '')
        }
        if ($result.stdout -match '(?m)^TLC2 Version ([^\r\n]+)') { $entry.tlc_version = $Matches[1] }
        $entry.status = Get-TlcOutcome $result $entry.expected_invariant
        Write-Host "$($entry.status): TLC $($entry.config), exit $($entry.exit_code), elapsed $([math]::Round($entry.elapsed_ms)) ms"
        if ($entry.status -ne 'PASS') { Write-Host ($result.stdout + $result.stderr) }
    }
    catch {
        $entry.status = 'FAIL'
        $entry.error = $_.Exception.Message
        Write-Host "FAIL: TLC $($entry.config): $($entry.error)"
    }
    Save-Manifest
}
$manifest.status = if (@($manifest.results | Where-Object { $_.status -eq 'FAIL' }).Count) { 'FAIL' }
    elseif (@($manifest.results | Where-Object { $_.status -eq 'INCONCLUSIVE' }).Count) { 'INCONCLUSIVE' }
    elseif (@($manifest.results | Where-Object { $_.status -ne 'PASS' }).Count) { 'NOT_RUN' }
    else { 'PASS' }
foreach ($path in $inputPaths) {
    if ((Get-TextHash (Join-Path $repoRoot $path)) -ne $inputs[$path]) { $manifest.status = 'STALE' }
}
$manifest.finished_utc = [DateTime]::UtcNow.ToString('o')
Save-Manifest
Write-Host "TLC evidence: $manifestPath"
if ($manifest.status -ne 'PASS') { throw "TLC campaign status: $($manifest.status)" }
