[CmdletBinding()]
param(
    [string]$RepositoryRoot = '',
    [switch]$Terminate,
    [switch]$SelfTest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if ([string]::IsNullOrWhiteSpace($RepositoryRoot)) {
    $RepositoryRoot = Join-Path $PSScriptRoot '..'
}
$repository = [IO.Path]::GetFullPath($RepositoryRoot)
$targetPrefix = [IO.Path]::Combine($repository, 'target') + [IO.Path]::DirectorySeparatorChar
$sakuraNames = @(
    'sakura_engine.exe',
    'sakura_renderer.exe',
    'sakura_regtool.exe',
    'sakura_logon.exe',
    'sakura_settings.exe',
    'sakura_settings_payload.exe'
)
$runnerNames = @('cargo.exe', 'rustc.exe', 'clippy-driver.exe')

function Test-ScopedProcess {
    param([Parameter(Mandatory)]$Process)

    $name = [string]$Process.Name
    $path = [string]$Process.ExecutablePath
    $commandLine = [string]$Process.CommandLine
    $isSakura = $sakuraNames -contains $name
    $isTarget = $path.Length -gt 0 -and
        $path.StartsWith($targetPrefix, [StringComparison]::OrdinalIgnoreCase)
    $mentionsRepository = $commandLine.Contains(
        $repository,
        [StringComparison]::OrdinalIgnoreCase
    ) -or $commandLine.Contains(
        $targetPrefix,
        [StringComparison]::OrdinalIgnoreCase
    )
    $isRepositorySakura = $isSakura -and ($isTarget -or $mentionsRepository)
    $isRepositoryRunner = $runnerNames -contains $name -and $mentionsRepository
    return $isRepositorySakura -or $isTarget -or $isRepositoryRunner
}

function Invoke-SelfTest {
    param([switch]$Quiet)

    $targetExecutable = Join-Path $targetPrefix 'debug\sakura_engine.exe'
    $cases = @(
        @{
            Label = 'installed Sakura runtime is user state, not a test leak'
            Expected = $false
            Process = [pscustomobject]@{
                Name = 'sakura_engine.exe'
                ExecutablePath = 'C:\Program Files\Sakura Input\versions\1.0.0\sakura_engine.exe'
                CommandLine = '"C:\Program Files\Sakura Input\versions\1.0.0\sakura_engine.exe"'
            }
        },
        @{
            Label = 'repository target Sakura executable is scoped'
            Expected = $true
            Process = [pscustomobject]@{
                Name = 'sakura_engine.exe'
                ExecutablePath = $targetExecutable
                CommandLine = "`"$targetExecutable`""
            }
        },
        @{
            Label = 'every executable below target is scoped'
            Expected = $true
            Process = [pscustomobject]@{
                Name = 'integration-test.exe'
                ExecutablePath = (Join-Path $targetPrefix 'debug\deps\integration-test.exe')
                CommandLine = ''
            }
        },
        @{
            Label = 'repository cargo runner is scoped'
            Expected = $true
            Process = [pscustomobject]@{
                Name = 'cargo.exe'
                ExecutablePath = 'C:\Rust\bin\cargo.exe'
                CommandLine = "cargo test --manifest-path `"$repository\Cargo.toml`""
            }
        },
        @{
            Label = 'unrelated external process is ignored'
            Expected = $false
            Process = [pscustomobject]@{
                Name = 'notepad.exe'
                ExecutablePath = 'C:\Windows\System32\notepad.exe'
                CommandLine = 'notepad.exe'
            }
        }
    )

    foreach ($case in $cases) {
        $actual = Test-ScopedProcess -Process $case.Process
        if ($actual -ne $case.Expected) {
            throw "self-test failed: $($case.Label) (expected $($case.Expected), got $actual)"
        }
    }
    if (-not $Quiet) {
        Write-Host "self-test: $($cases.Count) process-scope cases passed"
    }
}

if ($SelfTest) {
    Invoke-SelfTest
    exit 0
}
Invoke-SelfTest -Quiet

function Get-ScopedRunner {
    @(
        Get-CimInstance Win32_Process | Where-Object {
            Test-ScopedProcess -Process $_
        }
    )
}

$processes = @(Get-ScopedRunner)
if ($Terminate -and $processes.Count -gt 0) {
    $deadline = [DateTime]::UtcNow.AddSeconds(5)
    do {
        $byId = @{}
        foreach ($process in $processes) {
            $byId[[int]$process.ProcessId] = $process
        }
        $roots = @(
            $processes | Where-Object {
                -not $byId.ContainsKey([int]$_.ParentProcessId)
            }
        )
        if ($roots.Count -eq 0) {
            $roots = @($processes | Select-Object -First 1)
        }
        foreach ($process in $roots) {
            Stop-Process -Id ([int]$process.ProcessId) -Force -ErrorAction SilentlyContinue
        }
        Start-Sleep -Milliseconds 50
        $processes = @(Get-ScopedRunner)
    } while ($processes.Count -gt 0 -and [DateTime]::UtcNow -lt $deadline)
}

if ($processes.Count -eq 0) {
    Write-Host 'clean: no repository-scoped Sakura or test runner remains'
    exit 0
}

$processes |
    Select-Object ProcessId, ParentProcessId, Name, ExecutablePath, CommandLine |
    ConvertTo-Json -Compress
exit 1
