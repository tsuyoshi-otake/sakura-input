[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$JarPath,

    [ValidateRange(5, 300)]
    [int]$TimeoutSeconds = 60,

    [ValidateRange(1, 8)]
    [int]$Workers = 1,

    [string[]]$Configs = @(
        'DualTsfCandidateBoard-legacy-bug.cfg',
        'DualTsfCandidateBoard-shipped-1.0.15.cfg',
        'DualTsfCandidateBoard-reach-hide.cfg',
        'DualTsfCandidateBoard-end-guard-only.cfg',
        'DualTsfCandidateBoard-proposed-fix.cfg'
    )
)

$ErrorActionPreference = 'Stop'
$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$modelDir = Join-Path $repoRoot 'verification\tla'
$jar = [System.IO.Path]::GetFullPath($JarPath)
if (-not [System.IO.File]::Exists($jar)) {
    throw "TLA+ tools jar not found: $jar"
}

$outputRoot = Join-Path $repoRoot 'verification\dual-tsf-candidate-board\tlc'
[System.IO.Directory]::CreateDirectory($outputRoot) | Out-Null
$Configs = @($Configs | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim() } | Where-Object { $_ })

foreach ($configName in $Configs) {
    $slug = [System.IO.Path]::GetFileNameWithoutExtension($configName)
    $runDir = Join-Path $outputRoot $slug
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
        '-workers', "$Workers",
        '-coverage', '1',
        '-fp', '0',
        '-seed', '20260818',
        '-metadir', (Join-Path $runDir 'states'),
        (Join-Path $modelDir 'DualTsfCandidateBoard.tla')
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
    if (-not $process.Start()) {
        throw "Failed to start TLC for $configName"
    }
    $stdoutTask = $process.StandardOutput.ReadToEndAsync()
    $stderrTask = $process.StandardError.ReadToEndAsync()
    if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
        Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
        $process.WaitForExit()
        [System.IO.File]::WriteAllText($stdout, $stdoutTask.Result)
        [System.IO.File]::WriteAllText($stderr, $stderrTask.Result)
        throw "TLC timed out after $TimeoutSeconds seconds for $configName"
    }
    $process.WaitForExit()
    [System.IO.File]::WriteAllText($stdout, $stdoutTask.Result)
    [System.IO.File]::WriteAllText($stderr, $stderrTask.Result)
    $passSlugs = @(
        'DualTsfCandidateBoard-end-guard-only',
        'DualTsfCandidateBoard-proposed-fix',
        'DualTsfCandidateBoard-fix'
    )
    $expected = if ($passSlugs -contains $slug) { 0 } else { 12 }
    if ($process.ExitCode -ne $expected) {
        throw "TLC $configName exit $($process.ExitCode), expected $expected"
    }
    Write-Host "TLC $configName exit $($process.ExitCode) (expected $expected)"
}

Write-Host "TLC logs written under $outputRoot"
