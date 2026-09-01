[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$runner = Join-Path $PSScriptRoot 'run-test-quiet.ps1'

function Invoke-CapturedRunner {
    param(
        [Parameter(Mandatory)]
        [string]$Name,

        [Parameter(Mandatory)]
        [scriptblock]$Command
    )

    $output = [System.Collections.Generic.List[string]]::new()
    $caught = $null

    try {
        & $runner -Name $Name -Command $Command *>&1 |
            ForEach-Object { [void]$output.Add([string]$_) }
    }
    catch {
        $caught = $_
        [void]$output.Add([string]$_)
    }

    [pscustomobject]@{
        Output = $output.ToArray()
        Error = $caught
    }
}

$success = Invoke-CapturedRunner -Name 'success probe' -Command {
    & $env:ComSpec /d /s /c 'echo hidden-success-stdout & echo hidden-success-stderr 1>&2 & exit /b 0'
}

if ($null -ne $success.Error) {
    throw "success probe unexpectedly failed: $($success.Error)"
}
if ($success.Output.Count -ne 1 -or $success.Output[0] -cne 'PASS: success probe') {
    throw "success output was not exactly one PASS line: $($success.Output -join ' | ')"
}
if (($success.Output -join "`n") -match 'hidden-success') {
    throw 'successful command output leaked into the quiet result'
}

$failure = Invoke-CapturedRunner -Name 'failure probe' -Command {
    & $env:ComSpec /d /s /c 'echo visible-failure-stdout & echo visible-failure-stderr 1>&2 & exit /b 23'
}

if ($null -eq $failure.Error) {
    throw 'failure probe unexpectedly succeeded'
}

$failureText = $failure.Output -join "`n"
foreach ($required in @('visible-failure-stdout', 'visible-failure-stderr', 'exit code 23')) {
    if (-not $failureText.Contains($required)) {
        throw "failure output did not contain '$required': $failureText"
    }
}
if ($failureText.Contains('PASS: failure probe')) {
    throw 'failure output incorrectly contained a PASS line'
}

$workflowRoot = Join-Path (Split-Path $PSScriptRoot -Parent) '.github\workflows'
foreach ($workflow in [System.IO.Directory]::EnumerateFiles($workflowRoot, '*.yml')) {
    $lineNumber = 0
    foreach ($line in [System.IO.File]::ReadLines($workflow)) {
        $lineNumber++
        if ($line.Trim() -match '^(?:run:\s*)?cargo\s+test(?:\s|$)') {
            throw "raw cargo test bypasses the quiet runner at ${workflow}:$lineNumber"
        }
    }
}

# The expected failure probe leaves its native exit code in PowerShell's
# process-wide automatic variable. A successful self-test must not leak that
# probe result to CI or to a caller that chains the next gate.
$global:LASTEXITCODE = 0
Write-Output 'PASS: quiet test runner self-test'
