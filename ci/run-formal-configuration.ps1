[CmdletBinding()]
param(
    [string]$Configuration,
    [string]$JarPath,
    [string]$ManifestPath,
    [string]$OutputDirectory,
    [ValidateRange(1, 1140)][int]$TimeoutSeconds = 1020,
    [ValidateRange(1, 16)][int]$Workers = 2,
    [switch]$SelfTest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$repoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
if (-not $ManifestPath) { $ManifestPath = Join-Path $PSScriptRoot 'formal-configurations.json' }

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
            exit_code = $process.ExitCode
            timed_out = $timedOut
            elapsed_ms = $watch.Elapsed.TotalMilliseconds
            pid = $process.Id
            process_exited = $process.HasExited
            stdout = $outTask.Result
            stderr = $errTask.Result
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

function Get-TlcOutcome($Result, $Entry) {
    if ($Result.timed_out) { return 'TIMEOUT' }
    if (-not $Result.process_exited) { return 'TOOL_ERROR' }

    if ($Entry.expect -ceq 'complete') {
        if ($Result.exit_code -eq 0 -and
            $Result.stdout.Contains('Model checking completed. No error has been found.') -and
            $Result.stdout -match '(?m)^([0-9,]+) states generated, ([0-9,]+) distinct states found, 0 states left on queue\.' -and
            [long]$Matches[1].Replace(',', '') -gt 0 -and
            [long]$Matches[2].Replace(',', '') -gt 0 -and
            $Result.stdout -notmatch '(?m)^Error:' -and
            $Result.stderr -notmatch '(?m)^Error:') {
            return 'PASS'
        }
        return 'INCOMPLETE'
    }

    if ($Entry.expect -ceq 'counterexample') {
        if ($Entry.expected_kind -cne 'invariant') { return 'MANIFEST_ERROR' }
        $violation = 'Error: Invariant ' + $Entry.expected_check + ' is violated.'
        $invariantErrors = @([regex]::Matches($Result.stdout, '(?m)^Error: Invariant ([A-Za-z][A-Za-z0-9_]*) is violated\.\r?$'))
        $unexpectedErrors = @($Result.stdout -split "`r?`n" | Where-Object {
            $_.StartsWith('Error:') -and $_ -cne $violation -and
            $_ -cne 'Error: The behavior up to this point is:'
        })
        if ($Result.exit_code -eq 12 -and
            $Result.stdout.Contains($violation) -and
            $invariantErrors.Count -eq 1 -and
            $unexpectedErrors.Count -eq 0 -and
            $invariantErrors[0].Groups[1].Value -ceq $Entry.expected_check -and
            $Result.stdout.Contains('The behavior up to this point is:') -and
            $Result.stdout.Contains('State 1:') -and
            $Result.stderr -notmatch '(?m)^Error:' -and
            -not $Result.stdout.Contains('Model checking completed. No error has been found.')) {
            return 'PASS'
        }
        return 'WRONG_COUNTEREXAMPLE'
    }

    return 'MANIFEST_ERROR'
}

function Assert-Inventory($Manifest) {
    if ($Manifest.schema -ne 1) { throw 'Unsupported formal configuration manifest schema' }
    if ($Manifest.tlc.version -cne '1.7.4' -or
        $Manifest.tlc.url -cne 'https://github.com/tlaplus/tlaplus/releases/download/v1.7.4/tla2tools.jar' -or
        $Manifest.tlc.sha256 -cne '936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88') {
        throw 'Formal configuration manifest does not use the reviewed TLC pin'
    }

    $trackedPaths = @(& git -C $repoRoot ls-files -- 'verification/tla/*.cfg')
    if ($LASTEXITCODE -ne 0) { throw 'Cannot enumerate tracked TLA+ configurations' }
    $actual = @($trackedPaths | ForEach-Object { [IO.Path]::GetFileName($_) } | Sort-Object)
    $declared = @($Manifest.configurations | ForEach-Object config | Sort-Object)
    if ($declared.Count -ne $Manifest.configurations.Count -or
        (Compare-Object -CaseSensitive $actual $declared)) {
        throw 'formal-configurations.json must classify every tracked verification/tla configuration exactly once'
    }

    $names = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($entry in $Manifest.configurations) {
        if (-not $names.Add($entry.config)) { throw "Duplicate configuration: $($entry.config)" }
        if ([IO.Path]::GetFileName($entry.config) -cne $entry.config -or $entry.config -notmatch '^[A-Za-z0-9.-]+\.cfg$') {
            throw "Unsafe configuration name: $($entry.config)"
        }
        if ($entry.model -notmatch '^[A-Za-z][A-Za-z0-9]*$' -or
            -not $entry.config.StartsWith($entry.model + '-', [StringComparison]::Ordinal)) {
            throw "Configuration/model mismatch: $($entry.config)"
        }
        if (-not [IO.File]::Exists((Join-Path $repoRoot "verification/tla/$($entry.model).tla"))) {
            throw "Missing model for $($entry.config)"
        }
        if ($entry.expect -ceq 'counterexample') {
            if ($entry.expected_kind -cne 'invariant' -or -not $entry.expected_check) {
                throw "Counterexample lacks an exact invariant: $($entry.config)"
            }
            $configText = [IO.File]::ReadAllText((Join-Path $repoRoot "verification/tla/$($entry.config)"))
            if ($configText -notmatch "(?m)^\s+$([regex]::Escape($entry.expected_check))\s*$") {
                throw "Expected invariant is not declared by $($entry.config): $($entry.expected_check)"
            }
        }
        elseif ($entry.expect -cne 'complete') {
            throw "Unknown expected outcome for $($entry.config): $($entry.expect)"
        }
    }
}

if (-not [IO.File]::Exists($ManifestPath)) { throw "Manifest does not exist: $ManifestPath" }
$manifest = Get-Content -LiteralPath $ManifestPath -Raw | ConvertFrom-Json
Assert-Inventory $manifest

if ($SelfTest) {
    $complete = "Model checking completed. No error has been found.`n12 states generated, 9 distinct states found, 0 states left on queue."
    $trace = "Error: Invariant NeverInserts is violated.`nThe behavior up to this point is:`nState 1:"
    $positive = [pscustomobject]@{ expect = 'complete' }
    $negative = [pscustomobject]@{ expect = 'counterexample'; expected_kind = 'invariant'; expected_check = 'NeverInserts' }
    $cases = @(
        @(@{ exit_code = 0; timed_out = $false; process_exited = $true; stdout = $complete; stderr = '' }, $positive, 'PASS'),
        @(@{ exit_code = 0; timed_out = $false; process_exited = $true; stdout = ''; stderr = '' }, $positive, 'INCOMPLETE'),
        @(@{ exit_code = 0; timed_out = $false; process_exited = $true; stdout = $complete.Replace('0 states left', '1 states left'); stderr = '' }, $positive, 'INCOMPLETE'),
        @(@{ exit_code = 12; timed_out = $false; process_exited = $true; stdout = $trace; stderr = '' }, $negative, 'PASS'),
        @(@{ exit_code = 12; timed_out = $false; process_exited = $true; stdout = $trace.Replace("`n", "`r`n"); stderr = '' }, $negative, 'PASS'),
        @(@{ exit_code = 12; timed_out = $false; process_exited = $true; stdout = $trace.Replace('NeverInserts', 'OtherInvariant'); stderr = '' }, $negative, 'WRONG_COUNTEREXAMPLE'),
        @(@{ exit_code = 12; timed_out = $false; process_exited = $true; stdout = ($trace + "`nError: Unexpected tool failure"); stderr = '' }, $negative, 'WRONG_COUNTEREXAMPLE'),
        @(@{ exit_code = 12; timed_out = $true; process_exited = $true; stdout = $trace; stderr = '' }, $negative, 'TIMEOUT'),
        @(@{ exit_code = 0; timed_out = $false; process_exited = $false; stdout = $complete; stderr = '' }, $positive, 'TOOL_ERROR')
    )
    foreach ($case in $cases) {
        $actual = Get-TlcOutcome $case[0] $case[1]
        if ($actual -cne $case[2]) { throw "Classification regression: wanted $($case[2]), got $actual" }
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
    Write-Host "PASS: formal inventory ($($manifest.configurations.Count)), classifications ($($cases.Count)), and child lifecycle"
    return
}

if (-not $Configuration) { throw 'Configuration is required' }
$entries = @($manifest.configurations | Where-Object { $_.config -ceq $Configuration })
if ($entries.Count -ne 1) { throw "Unknown or duplicate configuration: $Configuration" }
$entry = $entries[0]
if (-not $JarPath -or -not [IO.File]::Exists($JarPath)) { throw 'An existing TLA+ tools JarPath is required' }
$jar = [IO.Path]::GetFullPath($JarPath)
$jarHash = (Get-FileHash -LiteralPath $jar -Algorithm SHA256).Hash.ToLowerInvariant()
if ($jarHash -cne $manifest.tlc.sha256) { throw 'TLC jar hash differs from the reviewed pin; no run performed' }

if (-not $OutputDirectory) {
    $OutputDirectory = Join-Path $repoRoot ".codex/formal-verification/$([IO.Path]::GetFileNameWithoutExtension($Configuration))"
}
$output = [IO.Path]::GetFullPath($OutputDirectory)
[void][IO.Directory]::CreateDirectory($output)
$stdoutPath = Join-Path $output 'stdout.log'
$stderrPath = Join-Path $output 'stderr.log'
$resultPath = Join-Path $output 'result.json'
$java = (Get-Command java.exe -ErrorAction Stop).Source
$modelPath = Join-Path $repoRoot "verification/tla/$($entry.model).tla"
$configPath = Join-Path $repoRoot "verification/tla/$($entry.config)"
$arguments = @(
    '-Xmx2g', '-XX:+UseParallelGC', '-cp', $jar, 'tlc2.TLC', '-config', $configPath,
    '-workers', "$Workers", '-coverage', '1', '-fp', '0', '-seed', '20260913',
    '-metadir', (Join-Path $output 'states'), $modelPath
)

$result = Invoke-BoundedProcess $java $arguments ($TimeoutSeconds * 1000)
[IO.File]::WriteAllText($stdoutPath, $result.stdout)
[IO.File]::WriteAllText($stderrPath, $result.stderr)
$outcome = Get-TlcOutcome $result $entry
$receipt = [ordered]@{
    schema = 1
    configuration = $entry.config
    model = $entry.model
    expected = $entry.expect
    expected_kind = if ($entry.expect -ceq 'counterexample') { $entry.expected_kind } else { $null }
    expected_check = if ($entry.expect -ceq 'counterexample') { $entry.expected_check } else { $null }
    outcome = $outcome
    exit_code = $result.exit_code
    timed_out = $result.timed_out
    process_exited = $result.process_exited
    elapsed_ms = $result.elapsed_ms
    jar_sha256 = $jarHash
    model_sha256 = (Get-FileHash -LiteralPath $modelPath -Algorithm SHA256).Hash.ToLowerInvariant()
    configuration_sha256 = (Get-FileHash -LiteralPath $configPath -Algorithm SHA256).Hash.ToLowerInvariant()
    stdout_sha256 = (Get-FileHash -LiteralPath $stdoutPath -Algorithm SHA256).Hash.ToLowerInvariant()
    stderr_sha256 = (Get-FileHash -LiteralPath $stderrPath -Algorithm SHA256).Hash.ToLowerInvariant()
}
[IO.File]::WriteAllText($resultPath, ($receipt | ConvertTo-Json -Depth 5) + "`n")
if ($outcome -cne 'PASS') {
    Write-Host $result.stdout
    Write-Host $result.stderr
    throw "TLC $Configuration failed classification: $outcome (exit $($result.exit_code))"
}
Write-Host "PASS: TLC $Configuration ($($entry.expect), $([math]::Round($result.elapsed_ms)) ms)"
