[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidateNotNullOrEmpty()]
    [string]$Name,

    [Parameter(Mandatory)]
    [ValidateNotNull()]
    [scriptblock]$Command
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$userProfile = [Environment]::GetFolderPath([Environment+SpecialFolder]::UserProfile)
$testOutputRoot = Join-Path $userProfile 'tmp\sakura-input-test-output'
[void][System.IO.Directory]::CreateDirectory($testOutputRoot)
$logPath = Join-Path $testOutputRoot ("{0}.log" -f [guid]::NewGuid().ToString('N'))

$exitCode = 0
$failureRecord = $null

try {
    # A previous native command must not make a successful PowerShell-only
    # probe look like it failed. Scriptblocks run in a child scope, so use
    # the global automatic variable that their native commands update rather
    # than creating a script-local shadow that would remain zero.
    $global:LASTEXITCODE = 0

    try {
        & $Command *> $logPath
        $exitCode = [int]$global:LASTEXITCODE
    }
    catch {
        $failureRecord = $_
        $exitCode = if ($global:LASTEXITCODE -ne 0) { [int]$global:LASTEXITCODE } else { 1 }
        ($_ | Out-String) | Out-File -LiteralPath $logPath -Append -Encoding utf8
    }

    if ($null -eq $failureRecord -and $exitCode -eq 0) {
        Write-Output "PASS: $Name"
        return
    }

    if ([System.IO.File]::Exists($logPath)) {
        Get-Content -LiteralPath $logPath
    }

    if ($null -ne $failureRecord) {
        throw "FAIL: $Name (PowerShell exception; exit code $exitCode)"
    }

    throw "FAIL: $Name (exit code $exitCode)"
}
finally {
    if ([System.IO.File]::Exists($logPath)) {
        [System.IO.File]::Delete($logPath)
    }
}
