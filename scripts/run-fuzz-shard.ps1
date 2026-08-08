[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidateSet('ipc', 'dictionary', 'fsm')]
    [string]$Target,

    [Parameter(Mandatory)]
    [ValidateRange(0, 63)]
    [int]$Shard,

    [string]$StateDirectory = (Join-Path $PSScriptRoot '..\.codex\goal-loop\all-phases\phase5\fuzz'),

    [ValidateRange(0.01, 330.0)]
    [double]$RunMinutes = 300.0,

    [ValidateRange(1, 3600)]
    [int]$SliceTimeoutSeconds = 600,

    [ValidateRange(1, 1000000000)]
    [uint64]$IterationsPerSlice = 2000000,

    [ValidateRange(0, 1000000)]
    [int]$MaxSlices = 0
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repository = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$stateRoot = [IO.Path]::GetFullPath($StateDirectory)
$logRoot = Join-Path $stateRoot 'logs'
$statePath = Join-Path $stateRoot ("{0}-shard-{1}.state.json" -f $Target, $Shard)
$processCheck = Join-Path $repository 'ci\check-process-clean.ps1'
[IO.Directory]::CreateDirectory($stateRoot) | Out-Null
[IO.Directory]::CreateDirectory($logRoot) | Out-Null

$targetArguments = @{
    ipc = @(
        'test', '-p', 'sakura-proto', '--release', '--test', 'robustness',
        'sharded_protocol_campaign', '--', '--exact', '--ignored', '--nocapture'
    )
    dictionary = @(
        'test', '-p', 'dictc', '--release', '--test', 'dictionary_robustness',
        'sharded_hostile_dictionary_campaign', '--', '--exact', '--ignored', '--nocapture'
    )
    fsm = @(
        'test', '-p', 'sakura-core', '--release', '--test', 'fsm_robustness',
        'sharded_fsm_campaign', '--', '--exact', '--ignored', '--nocapture'
    )
}

function Write-StateAtomic {
    param([Parameter(Mandatory)][object]$State)

    $temporary = "$statePath.$PID.tmp"
    $json = $State | ConvertTo-Json -Depth 12
    [IO.File]::WriteAllText($temporary, $json, [Text.UTF8Encoding]::new($false))
    [IO.File]::Move($temporary, $statePath, $true)
}

function Get-Sha256 {
    param([Parameter(Mandatory)][string]$Path)

    $stream = [IO.File]::OpenRead($Path)
    try {
        $algorithm = [Security.Cryptography.SHA256]::Create()
        try {
            return [Convert]::ToHexString($algorithm.ComputeHash($stream)).ToLowerInvariant()
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
    $lines = @(& pwsh -NoProfile -ExecutionPolicy Bypass -File $processCheck -RepositoryRoot $repository)
    $exitCode = $LASTEXITCODE
    foreach ($line in $lines) { Write-Host $line }
    if ($exitCode -eq 0) {
        return $true
    }

    Write-Warning 'A fuzz slice left a repository-scoped process; terminating parents first.'
    & pwsh -NoProfile -ExecutionPolicy Bypass -File $processCheck -RepositoryRoot $repository -Terminate
    if ($LASTEXITCODE -ne 0) {
        return $false
    }
    & pwsh -NoProfile -ExecutionPolicy Bypass -File $processCheck -RepositoryRoot $repository
    return $LASTEXITCODE -eq 0
}

function New-ProcessStartInfo {
    param(
        [Parameter(Mandatory)][string]$StdoutPath,
        [Parameter(Mandatory)][string]$StderrPath,
        [Parameter(Mandatory)][uint64]$Seed
    )

    $rtk = Get-Command rtk -ErrorAction SilentlyContinue
    $info = [Diagnostics.ProcessStartInfo]::new()
    if ($null -ne $rtk) {
        $info.FileName = $rtk.Source
        $info.ArgumentList.Add('cargo')
    }
    else {
        $cargo = Get-Command cargo -ErrorAction Stop
        $info.FileName = $cargo.Source
    }
    foreach ($argument in $targetArguments[$Target]) {
        $info.ArgumentList.Add($argument)
    }
    $info.WorkingDirectory = $repository
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $info.Environment['SAKURA_FUZZ_ITERS'] = [string]$IterationsPerSlice
    $info.Environment['SAKURA_FUZZ_SHARD'] = [string]$Shard
    $info.Environment['SAKURA_FUZZ_SEED'] = [string]$Seed
    $info.Environment['CARGO_TERM_COLOR'] = 'never'
    return $info
}

function Invoke-Slice {
    param(
        [Parameter(Mandatory)][uint64]$Seed,
        [Parameter(Mandatory)][int]$TimeoutSeconds
    )

    $stamp = [DateTime]::UtcNow.ToString('yyyyMMddTHHmmssfffZ')
    $baseName = "{0}-shard-{1}-seed-{2}-{3}" -f $Target, $Shard, $Seed, $stamp
    $stdoutPath = Join-Path $logRoot "$baseName.stdout.log"
    $stderrPath = Join-Path $logRoot "$baseName.stderr.log"
    $started = [DateTime]::UtcNow
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $status = 'start_failed'
    $exitCode = $null
    $detail = $null
    $process = $null
    $stdoutTask = $null
    $stderrTask = $null

    try {
        $info = New-ProcessStartInfo -StdoutPath $stdoutPath -StderrPath $stderrPath -Seed $Seed
        $process = [Diagnostics.Process]::new()
        $process.StartInfo = $info
        if (-not $process.Start()) {
            throw 'Process.Start returned false'
        }
        $stdoutTask = $process.StandardOutput.ReadToEndAsync()
        $stderrTask = $process.StandardError.ReadToEndAsync()

        if ($process.WaitForExit($TimeoutSeconds * 1000)) {
            $process.WaitForExit()
            $exitCode = $process.ExitCode
            $status = if ($exitCode -eq 0) { 'passed' } else { 'failed' }
        }
        else {
            $status = 'timed_out'
            $detail = "test process exceeded the bounded ${TimeoutSeconds}s slice"
            try {
                $process.Kill($true)
                $process.WaitForExit(10000) | Out-Null
            }
            catch {
                $detail = "$detail; process-tree kill failed: $($_.Exception.Message)"
            }
        }
    }
    catch {
        $detail = $_.Exception.Message
    }
    finally {
        $watch.Stop()
        $stdout = if ($null -ne $stdoutTask) {
            try { $stdoutTask.GetAwaiter().GetResult() } catch { "stdout read failed: $($_.Exception.Message)" }
        }
        else { '' }
        $stderr = if ($null -ne $stderrTask) {
            try { $stderrTask.GetAwaiter().GetResult() } catch { "stderr read failed: $($_.Exception.Message)" }
        }
        else { '' }
        [IO.File]::WriteAllText($stdoutPath, $stdout, [Text.UTF8Encoding]::new($false))
        [IO.File]::WriteAllText($stderrPath, $stderr, [Text.UTF8Encoding]::new($false))
        if ($null -ne $process) { $process.Dispose() }
    }

    if (-not (Confirm-ProcessClean)) {
        $status = 'process_leak'
        $detail = if ($null -eq $detail) {
            'process cleanup or proof-of-cleanliness failed'
        }
        else {
            "$detail; process cleanup or proof-of-cleanliness failed"
        }
    }

    $ended = [DateTime]::UtcNow
    return [ordered]@{
        target = $Target
        shard = $Shard
        seed = [string]$Seed
        iterations = [string]$IterationsPerSlice
        started_at_utc = $started.ToString('o')
        ended_at_utc = $ended.ToString('o')
        elapsed_seconds = [Math]::Round($watch.Elapsed.TotalSeconds, 6)
        timeout_seconds = $TimeoutSeconds
        status = $status
        exit_code = $exitCode
        detail = $detail
        stdout_path = [IO.Path]::GetRelativePath($stateRoot, $stdoutPath)
        stdout_sha256 = Get-Sha256 $stdoutPath
        stderr_path = [IO.Path]::GetRelativePath($stateRoot, $stderrPath)
        stderr_sha256 = Get-Sha256 $stderrPath
    }
}

if ([IO.File]::Exists($statePath)) {
    $state = [IO.File]::ReadAllText($statePath, [Text.Encoding]::UTF8) | ConvertFrom-Json -AsHashtable
    if ($state.schema_version -ne 1 -or $state.target -ne $Target -or [int]$state.shard -ne $Shard) {
        throw "state identity mismatch in $statePath"
    }
}
else {
    $state = [ordered]@{
        schema_version = 1
        target = $Target
        shard = $Shard
        created_at_utc = [DateTime]::UtcNow.ToString('o')
        updated_at_utc = [DateTime]::UtcNow.ToString('o')
        run_status = 'ready'
        active_slice = $null
        last_error = $null
        receipts = @()
    }
}

$receipts = [Collections.Generic.List[object]]::new()
foreach ($receipt in @($state.receipts)) { $receipts.Add($receipt) }
$state.receipts = $receipts

if ($state.run_status -eq 'running') {
    $interrupted = [ordered]@{
        target = $Target
        shard = $Shard
        seed = if ($null -ne $state.active_slice) { [string]$state.active_slice.seed } else { '' }
        iterations = '0'
        started_at_utc = if ($null -ne $state.active_slice) { [string]$state.active_slice.started_at_utc } else { [string]$state.updated_at_utc }
        ended_at_utc = [DateTime]::UtcNow.ToString('o')
        elapsed_seconds = 0.0
        timeout_seconds = 0
        status = 'interrupted'
        exit_code = $null
        detail = 'the previous runner ended without recording a terminal state'
        stdout_path = ''
        stdout_sha256 = ''
        stderr_path = ''
        stderr_sha256 = ''
    }
    $receipts.Add($interrupted)
}

$state.run_status = 'running'
$state.active_slice = $null
$state.last_error = $null
$state.updated_at_utc = [DateTime]::UtcNow.ToString('o')
Write-StateAtomic $state

$deadline = [DateTime]::UtcNow.AddMinutes($RunMinutes)
$completedThisRun = 0
$terminalStatus = 'failed'
$terminalError = $null

try {
    while ([DateTime]::UtcNow -lt $deadline -and ($MaxSlices -eq 0 -or $completedThisRun -lt $MaxSlices)) {
        $remainingSeconds = [Math]::Floor(($deadline - [DateTime]::UtcNow).TotalSeconds)
        if ($remainingSeconds -lt 1) { break }
        $timeout = [Math]::Min($SliceTimeoutSeconds, [int]$remainingSeconds)
        $seed = [uint64]($receipts.Count + 1)
        $state.active_slice = [ordered]@{
            seed = [string]$seed
            started_at_utc = [DateTime]::UtcNow.ToString('o')
            timeout_seconds = $timeout
        }
        $state.updated_at_utc = [DateTime]::UtcNow.ToString('o')
        Write-StateAtomic $state

        Write-Host ("==> {0} shard {1}, seed {2}, timeout {3}s" -f $Target, $Shard, $seed, $timeout)
        $receipt = Invoke-Slice -Seed $seed -TimeoutSeconds $timeout
        $receipts.Add($receipt)
        $state.active_slice = $null
        $state.updated_at_utc = [DateTime]::UtcNow.ToString('o')
        Write-StateAtomic $state
        $completedThisRun++

        if ($receipt.status -ne 'passed') {
            throw "fuzz slice reached terminal status '$($receipt.status)'"
        }
    }
    $terminalStatus = 'ready'
}
catch {
    $terminalError = $_.Exception.Message
}
finally {
    $state.run_status = $terminalStatus
    $state.active_slice = $null
    $state.last_error = $terminalError
    $state.updated_at_utc = [DateTime]::UtcNow.ToString('o')
    Write-StateAtomic $state
}

if ($terminalStatus -ne 'ready') {
    Write-Error $terminalError
    exit 1
}

Write-Host ("ready: {0} shard {1}; {2} slice(s) completed this run; state {3}" -f $Target, $Shard, $completedThisRun, $statePath)
exit 0
