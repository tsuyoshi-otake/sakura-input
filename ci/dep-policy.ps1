#Requires -Version 5.1
<#
.SYNOPSIS
    Enforces the full-scratch dependency rule (DESIGN.md 3.1).

.DESCRIPTION
    Sakura Input links no third-party code. The only crates permitted in the
    dependency graph are:

      * the workspace's own crates;
      * the Windows binding family (`windows`, `windows-core`, the per-target
        `windows_*_msvc` crates, ...), which is the platform rather than a
        library;
      * a closed list of proc-macro crates that run at build time and
        contribute no bytes to any shipped artifact.

    Anything else fails the build. The point is to catch the accident — a
    `serde` or a `once_cell` arriving as somebody's transitive dependency —
    before it is load-bearing and expensive to remove.

.PARAMETER SelfTest
    Runs the classifier against synthetic inputs instead of Cargo.lock, proving
    it both accepts what it should and rejects what it should. Used by CI so the
    gate itself cannot silently degrade into a no-op.

.EXAMPLE
    pwsh ci/dep-policy.ps1
    pwsh ci/dep-policy.ps1 -SelfTest
#>
[CmdletBinding()]
param(
    [string]$LockFile,
    [string]$ManifestFile,
    [switch]$SelfTest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
if (-not $LockFile) { $LockFile = Join-Path $repoRoot 'Cargo.lock' }
if (-not $ManifestFile) { $ManifestFile = Join-Path $repoRoot 'Cargo.toml' }

# Compile-time-only crates. Each entry needs a reason, because "it is only a
# build dependency" is exactly the argument that would erode this rule.
$BuildTimeOnly = [ordered]@{
    'proc-macro2'   = 'token plumbing for windows-implement / windows-interface'
    'quote'         = 'token plumbing for windows-implement / windows-interface'
    'syn'           = 'token plumbing for windows-implement / windows-interface'
    'unicode-ident' = 'identifier validation used by syn'
}

# `windows`, `windows-core`, `windows_x86_64_msvc`, ... — one family, one rule.
$WindowsFamilyPattern = '^windows([-_].+)?$'

function Get-WorkspaceCrateName {
    <#
        Reads the crate names declared by the workspace members, so the gate
        does not have to be edited every time a crate is added.
    #>
    param([Parameter(Mandatory)][string]$Manifest)

    $text = Get-Content -Raw -LiteralPath $Manifest
    $membersMatch = [regex]::Match($text, '(?ms)^\s*members\s*=\s*\[(.*?)\]')
    if (-not $membersMatch.Success) {
        throw "No [workspace] members list found in $Manifest"
    }

    $root = Split-Path -Parent $Manifest
    $names = New-Object System.Collections.Generic.List[string]
    foreach ($m in [regex]::Matches($membersMatch.Groups[1].Value, '"([^"]+)"')) {
        $memberManifest = Join-Path $root (Join-Path $m.Groups[1].Value 'Cargo.toml')
        if (-not (Test-Path -LiteralPath $memberManifest)) {
            throw "Workspace member '$($m.Groups[1].Value)' has no Cargo.toml"
        }
        $nameMatch = [regex]::Match(
            (Get-Content -Raw -LiteralPath $memberManifest),
            '(?m)^\s*name\s*=\s*"([^"]+)"')
        if (-not $nameMatch.Success) {
            throw "Workspace member '$($m.Groups[1].Value)' declares no package name"
        }
        $names.Add($nameMatch.Groups[1].Value)
    }
    return , $names.ToArray()
}

function Get-LockedPackageName {
    param([Parameter(Mandatory)][string]$Lock)

    $names = New-Object System.Collections.Generic.List[string]
    foreach ($m in [regex]::Matches(
            (Get-Content -Raw -LiteralPath $Lock),
            '(?m)^name\s*=\s*"([^"]+)"')) {
        $names.Add($m.Groups[1].Value)
    }
    if ($names.Count -eq 0) {
        throw "No packages parsed from $Lock — the gate would pass vacuously"
    }
    return , $names.ToArray()
}

function Get-DisallowedPackage {
    <#
        The whole policy, in one testable function: returns the names that
        violate it, in the order they were given.
    #>
    param(
        [Parameter(Mandatory)][AllowEmptyCollection()][string[]]$PackageName,
        [Parameter(Mandatory)][AllowEmptyCollection()][string[]]$WorkspaceCrate
    )

    $offenders = New-Object System.Collections.Generic.List[string]
    foreach ($name in ($PackageName | Sort-Object -Unique)) {
        if ($WorkspaceCrate -contains $name) { continue }
        if ($name -match $WindowsFamilyPattern) { continue }
        if ($BuildTimeOnly.Contains($name)) { continue }
        $offenders.Add($name)
    }
    return , $offenders.ToArray()
}

function Invoke-SelfTest {
    $workspace = @('sakura-core', 'sakura-tsf')
    $failures = New-Object System.Collections.Generic.List[string]

    $allowed = @(
        'sakura-core', 'sakura-tsf',
        'windows', 'windows-core', 'windows_x86_64_msvc', 'windows-implement',
        'proc-macro2', 'quote', 'syn', 'unicode-ident'
    )
    $flagged = Get-DisallowedPackage -PackageName $allowed -WorkspaceCrate $workspace
    if ($flagged.Count -ne 0) {
        $failures.Add("permitted crates were rejected: $($flagged -join ', ')")
    }

    # The case that matters: a plausible third-party crate must not slip past.
    $forbidden = @('serde', 'once_cell', 'winapi', 'window-shopping')
    $flagged = Get-DisallowedPackage `
        -PackageName ($allowed + $forbidden) -WorkspaceCrate $workspace
    foreach ($name in $forbidden) {
        if ($flagged -notcontains $name) {
            $failures.Add("forbidden crate '$name' was not rejected")
        }
    }

    if ($failures.Count -gt 0) {
        Write-Host 'dep-policy self-test FAILED:' -ForegroundColor Red
        $failures | ForEach-Object { Write-Host "  - $_" -ForegroundColor Red }
        return 1
    }
    Write-Host 'dep-policy self-test passed.' -ForegroundColor Green
    return 0
}

if ($SelfTest) {
    exit (Invoke-SelfTest)
}

$workspaceCrates = Get-WorkspaceCrateName -Manifest $ManifestFile
$packages = Get-LockedPackageName -Lock $LockFile
$offenders = Get-DisallowedPackage -PackageName $packages -WorkspaceCrate $workspaceCrates

Write-Host ("Checked {0} locked packages against the full-scratch rule (DESIGN.md 3.1)." -f (
        $packages | Sort-Object -Unique).Count)

if ($offenders.Count -gt 0) {
    Write-Host ''
    Write-Host 'Disallowed dependencies found:' -ForegroundColor Red
    $offenders | ForEach-Object { Write-Host "  - $_" -ForegroundColor Red }
    Write-Host ''
    Write-Host 'Sakura Input links no third-party code (DESIGN.md 3.1). Remove the'
    Write-Host 'dependency, or -- if it is genuinely build-time-only and unavoidable --'
    Write-Host 'add it to $BuildTimeOnly in this script together with a written reason'
    Write-Host 'and amend DESIGN.md 3.1 in the same commit.'
    exit 1
}

Write-Host 'No disallowed dependencies.' -ForegroundColor Green
exit 0
