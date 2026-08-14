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
    an unreviewed parser or runtime crate arriving as a transitive dependency —
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
    'quick-xml'     = 'offline dictc build-tool streaming parser for pinned Japanese WordNet LMF'
    'flate2'        = 'offline dictc build-tool gzip reader for pinned Japanese WordNet LMF'
    'crc32fast'     = 'flate2 checksum implementation detail for offline WordNet archive validation'
    'miniz_oxide'   = 'flate2 pure-Rust DEFLATE implementation detail for offline WordNet archive'
    'adler2'        = 'miniz_oxide checksum implementation detail'
    'simd-adler32'  = 'miniz_oxide checksum implementation detail'
    'serde'         = 'offline dictc LLM-detail release-gate strict schema parser'
    'serde_derive'  = 'derive-only schema implementation for offline dictc LLM-detail release gate'
    'serde_json'    = 'offline dictc LLM-detail release-gate JSONL parser'
    'itoa'          = 'serde_json integer formatting implementation detail for offline dictc release gate'
    'memchr'        = 'serde_json parser implementation detail for offline dictc release gate'
    'ryu'           = 'serde_json float formatting implementation detail for offline dictc release gate'
    'sha2'          = 'offline dictc LLM-detail target and input SHA-256 release gate'
    'digest'        = 'sha2 implementation detail for offline dictc release gate'
    'block-buffer'  = 'sha2 implementation detail for offline dictc release gate'
    'crypto-common' = 'sha2 implementation detail for offline dictc release gate'
    'generic-array' = 'sha2 implementation detail for offline dictc release gate'
    'typenum'       = 'sha2 implementation detail for offline dictc release gate'
    'version_check' = 'sha2 build-time configuration for offline dictc release gate'
    'cpufeatures'   = 'sha2 CPU dispatch implementation detail for offline dictc release gate'
    'unicode-normalization' = 'offline dictc NFC normalization for LLM-detail target identity'
    'tinyvec'       = 'unicode-normalization implementation detail for offline dictc release gate'
    'tinyvec_macros' = 'tinyvec implementation detail for offline dictc release gate'
}

# `windows`, `windows-core`, `windows_x86_64_msvc`, ... — one family, one rule.
$WindowsFamilyPattern = '^windows([-_].+)?$'

# The isolated sakura_neural_worker dynamically loads the installer-provided
# ONNX Runtime DLL. These bindings never enter the TSF DLL or engine graph.
$IsolatedWorkerRuntime = [ordered]@{
    'ort'                   = 'isolated neural worker binding; load-dynamic only, no bundled runtime'
    'ort-sys'               = 'FFI declarations for the isolated worker; dynamic loading only'
    'autocfg'               = 'ort numeric build-time configuration'
    'cfg-if'                = 'ort platform configuration'
    'libloading'            = 'ort LoadLibrary implementation for worker sibling DLL'
    'matrixmultiply'        = 'ndarray arithmetic required by ort public API'
    'ndarray'               = 'ort tensor API'
    'num-complex'           = 'ndarray numeric support'
    'num-integer'           = 'ndarray numeric support'
    'num-traits'            = 'ndarray numeric support'
    'once_cell'             = 'ort process-local runtime initialization'
    'pin-project-lite'      = 'ort tracing dependency'
    'portable-atomic'       = 'ort runtime initialization'
    'portable-atomic-util'  = 'ort runtime initialization'
    'rawpointer'            = 'ndarray implementation detail'
    'smallvec'              = 'ort tensor shape storage'
    'tracing'               = 'ort diagnostic API'
    'tracing-core'          = 'ort diagnostic API'
    'sha2'                  = 'isolated worker manifest SHA-256 verification'
    'digest'                = 'sha2 implementation detail'
    'block-buffer'          = 'sha2 implementation detail'
    'crypto-common'         = 'sha2 implementation detail'
    'generic-array'         = 'sha2 implementation detail'
    'typenum'               = 'sha2 implementation detail'
    'version_check'         = 'sha2 build-time configuration'
    'cpufeatures'           = 'sha2 CPU dispatch'
    'libc'                  = 'transitive platform support for isolated worker'
    'serde'                 = 'isolated worker strict model-manifest deserialization'
    'serde_derive'          = 'derive-only manifest schema implementation'
    'serde_json'            = 'isolated worker strict JSON manifest parser'
    'itoa'                  = 'serde_json integer formatting implementation detail'
    'memchr'                = 'serde_json parser implementation detail'
    'ryu'                   = 'serde_json float formatting implementation detail'
}

# These tools produce build artifacts but are not shipping runtime binaries.
# A dependency admitted for dictc must not therefore become available to an IME
# runtime transitively. Check the resolved graph, not just direct manifests.
$RuntimeCrates = @(
    'sakura-core', 'sakura-proto', 'sakura-ipc', 'sakura-reg', 'sakura-tsf',
    'sakura-engine', 'sakura-renderer', 'sakura-regtool', 'sakura-logon', 'sakura-settings'
)
$OfflineDetailParserCrates = @(
    'serde', 'serde_derive', 'serde_json', 'itoa', 'memchr', 'ryu', 'sha2', 'digest',
    'block-buffer', 'crypto-common', 'generic-array', 'typenum', 'version_check',
    'cpufeatures', 'unicode-normalization', 'tinyvec', 'tinyvec_macros'
)

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
        if ($IsolatedWorkerRuntime.Contains($name)) { continue }
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
        'proc-macro2', 'quote', 'syn', 'unicode-ident',
        'ort', 'serde', 'serde_json', 'sha2'
    )
    $flagged = Get-DisallowedPackage -PackageName $allowed -WorkspaceCrate $workspace
    if ($flagged.Count -ne 0) {
        $failures.Add("permitted crates were rejected: $($flagged -join ', ')")
    }

    # The case that matters: a plausible third-party crate must not slip past.
    $forbidden = @('toml', 'regex', 'winapi', 'window-shopping')
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

foreach ($crate in $RuntimeCrates) {
    # Dev-dependencies compile test fixtures (the engine intentionally uses
    # dictc there) but cannot enter the shipping runtime binary.
    $tree = & cargo tree --locked -p $crate --edges normal --prefix none 2>$null
    if ($LASTEXITCODE -ne 0) {
        throw "could not inspect resolved dependency graph for runtime crate '$crate'"
    }
    foreach ($dependency in $OfflineDetailParserCrates) {
        if ($tree | Select-String -Quiet -Pattern ("^$([regex]::Escape($dependency)) v")) {
            throw "offline dictc LLM-detail dependency '$dependency' leaked into runtime crate '$crate'"
        }
    }
}

Write-Host 'No disallowed dependencies.' -ForegroundColor Green
exit 0
