[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$JarPath,

    [ValidateRange(5, 300)]
    [int]$TimeoutSeconds = 60
)

$ErrorActionPreference = 'Stop'
$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$modelDir = Join-Path $repoRoot 'verification\tla'
$jar = [System.IO.Path]::GetFullPath($JarPath)
if (-not [System.IO.File]::Exists($jar)) {
    throw "TLA+ tools jar not found: $jar"
}

$outputRoot = [System.IO.Path]::GetFullPath(
    (Join-Path $env:TEMP 'sakura-input-tlc-engine-recovery')
)
[System.IO.Directory]::CreateDirectory($outputRoot) | Out-Null
$configs = @(
    'EngineRecovery-small.cfg',
    'EngineRecovery-concurrent.cfg',
    'EngineRecovery-reordered.cfg',
    'EngineRecovery-boundary.cfg'
)

foreach ($configName in $configs) {
    $slug = [System.IO.Path]::GetFileNameWithoutExtension($configName)
    $runDir = [System.IO.Path]::GetFullPath((Join-Path $outputRoot $slug))
    $outputPrefix = $outputRoot.TrimEnd([System.IO.Path]::DirectorySeparatorChar) +
        [System.IO.Path]::DirectorySeparatorChar
    if (-not $runDir.StartsWith($outputPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to replace TLC directory outside the output root: $runDir"
    }
    if ([System.IO.Directory]::Exists($runDir)) {
        [System.IO.Directory]::Delete($runDir, $true)
    }
    [System.IO.Directory]::CreateDirectory($runDir) | Out-Null

    $stdout = Join-Path $runDir 'stdout.log'
    $stderr = Join-Path $runDir 'stderr.log'
    $arguments = @(
        '-cp', $jar,
        'tlc2.TLC',
        '-config', (Join-Path $modelDir $configName),
        '-workers', '1',
        '-coverage', '1',
        '-fp', '0',
        '-seed', '20260814',
        '-metadir', (Join-Path $runDir 'states'),
        (Join-Path $modelDir 'EngineRecovery.tla')
    )

    $startInfo = New-Object System.Diagnostics.ProcessStartInfo
    $startInfo.FileName = 'java.exe'
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $startInfo.Arguments = (($arguments | ForEach-Object {
        '"{0}"' -f $_.Replace('"', '\"')
    }) -join ' ')

    $process = New-Object System.Diagnostics.Process
    $process.StartInfo = $startInfo
    try {
        if (-not $process.Start()) {
            throw "Failed to start TLC for $configName"
        }
        $stdoutTask = $process.StandardOutput.ReadToEndAsync()
        $stderrTask = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
            Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
            $process.WaitForExit()
            throw "TLC timed out after $TimeoutSeconds seconds for $configName"
        }
        $process.WaitForExit()
        $outputText = $stdoutTask.Result
        $errorText = $stderrTask.Result
        [System.IO.File]::WriteAllText($stdout, $outputText)
        [System.IO.File]::WriteAllText($stderr, $errorText)
        $exitCode = $process.ExitCode
        if ($exitCode -ne 0) {
            throw "TLC failed for $configName (exit $exitCode)`n$outputText`n$errorText"
        }
    }
    finally {
        if (-not $process.HasExited) {
            Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
            $process.WaitForExit()
        }
        $process.Dispose()
    }

    $summary = [System.IO.File]::ReadLines($stdout) |
        Where-Object {
            $_ -match 'Model checking completed|states generated|distinct states|depth|No error has been found|Finished in'
        }
    Write-Output "[$configName]"
    $summary | Write-Output
}

$jarPattern = [regex]::Escape($jar)
$survivors = @(Get-CimInstance Win32_Process | Where-Object {
    $_.Name -eq 'java.exe' -and $_.CommandLine -match $jarPattern
})
if ($survivors.Count -ne 0) {
    throw "TLC left $($survivors.Count) Java process(es) running"
}
