#Requires -Version 5.1
<#
.SYNOPSIS
    Ensure that the workflow uses the reviewed cargo-audit version.

.DESCRIPTION
    The release workflow must not rebuild cargo-audit on every tag. The
    Swatinem/rust-cache step shares ~/.cargo/bin between the main CI and the
    release build, but a cache miss must remain recoverable. This script first
    validates the restored executable and installs the exact reviewed version
    only when it is absent or wrong.
#>
[CmdletBinding()]
param(
    [switch]$SelfTest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# cargo-audit 0.22.2 was published on 2026-06-05, satisfying the repository's
# seven-day release-age quarantine as of the current workflow review. The
# binary's actual version output is `cargo-audit-audit 0.22.2`.
$expectedVersion = '0.22.2'
$expectedCommand = 'cargo-audit-audit'
$expectedLine = "$expectedCommand $expectedVersion"

function Parse-CargoAuditVersion {
    param(
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$Text,
        [Parameter(Mandatory = $true)][int]$ExitCode
    )

    $versionMatches = [regex]::Matches(
        $Text,
        '(?m)^\s*cargo-audit-audit\s+(?<version>\d+\.\d+\.\d+)\s*$'
    )
    if ($ExitCode -ne 0 -or $versionMatches.Count -ne 1) {
        return [pscustomobject]@{
            ExitCode = $ExitCode
            Version = $null
            Output = $Text.Trim()
        }
    }
    return [pscustomobject]@{
        ExitCode = $ExitCode
        Version = $versionMatches[0].Groups['version'].Value
        Output = $Text.Trim()
    }
}

function Get-CargoAuditVersion {
    try {
        $output = @(& cargo audit --version 2>&1)
        $exitCode = $LASTEXITCODE
        $text = ($output | ForEach-Object { [string]$_ }) -join [Environment]::NewLine
        return (Parse-CargoAuditVersion -Text $text -ExitCode $exitCode)
    } catch {
        return [pscustomobject]@{
            ExitCode = 1
            Version = $null
            Output = $_.Exception.Message
        }
    }
}

function Test-CargoAuditNeedsInstall {
    param(
        [Parameter(Mandatory = $true)][object]$Probe
    )
    return ([string]$Probe.Version -ne $expectedVersion)
}

function Assert-CargoAuditVersion {
    param(
        [Parameter(Mandatory = $true)][object]$Probe
    )
    if ([string]$Probe.Version -ne $expectedVersion) {
        throw "cargo-audit version verification failed: expected $expectedLine, got '$($Probe.Output)'"
    }
}

function Invoke-SelfTest {
    $hit = Parse-CargoAuditVersion -Text 'cargo-audit-audit 0.22.2' -ExitCode 0
    if ($hit.Version -ne '0.22.2') { throw 'cache-hit version self-test failed' }
    if (Test-CargoAuditNeedsInstall -Probe $hit) { throw 'cache-hit requested an unnecessary install' }

    $missing = Parse-CargoAuditVersion -Text '' -ExitCode 1
    if ($null -ne $missing.Version) { throw 'cache-miss self-test failed' }
    if (-not (Test-CargoAuditNeedsInstall -Probe $missing)) { throw 'cache-miss did not request install' }

    $mismatch = Parse-CargoAuditVersion -Text 'cargo-audit-audit 0.22.1' -ExitCode 0
    if ($mismatch.Version -ne '0.22.1') { throw 'version-mismatch self-test failed' }
    if (-not (Test-CargoAuditNeedsInstall -Probe $mismatch)) { throw 'version-mismatch did not request install' }

    $malformed = Parse-CargoAuditVersion -Text 'cargo-audit 0.22.2' -ExitCode 0
    if ($null -ne $malformed.Version) { throw 'malformed output self-test failed' }
    $rejected = $false
    try { Assert-CargoAuditVersion -Probe $malformed } catch { $rejected = $true }
    if (-not $rejected) { throw 'post-install malformed output was accepted' }

    Write-Host 'cargo-audit version parser self-test passed.' -ForegroundColor Green
    return 0
}

if ($SelfTest) {
    exit (Invoke-SelfTest)
}

$current = Get-CargoAuditVersion
if (Test-CargoAuditNeedsInstall -Probe $current) {
    if ($current.Output) {
        Write-Host "cargo-audit cache miss or version mismatch: $($current.Output)"
    } else {
        Write-Host 'cargo-audit cache miss or executable is unavailable.'
    }
    Write-Host "Installing $expectedLine from the locked crates.io dependency graph."
    $installOutput = @(& cargo install cargo-audit --version $expectedVersion --locked --force 2>&1)
    $installExitCode = $LASTEXITCODE
    $installOutput | ForEach-Object { Write-Host ([string]$_) }
    if ($installExitCode -ne 0) {
        throw "cargo-audit $expectedVersion installation failed with exit code $installExitCode"
    }
    $current = Get-CargoAuditVersion
}

Assert-CargoAuditVersion -Probe $current

Write-Host "cargo-audit version verified: $expectedLine"
