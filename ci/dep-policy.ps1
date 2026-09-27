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
    'serde'         = 'offline dictc LLM-detail release-gate and ime-eval Judge JSON parser'
    'serde_derive'  = 'derive-only schema implementation for offline dictc / ime-eval JSON'
    'serde_json'    = 'offline dictc LLM-detail JSONL parser and ime-eval Judge/corpus JSON'
    'itoa'          = 'serde_json integer formatting implementation detail for offline dictc / ime-eval'
    'memchr'        = 'serde_json parser implementation detail for offline dictc / ime-eval'
    'ryu'           = 'serde_json float formatting implementation detail for offline dictc / ime-eval'
    'sha2'          = 'offline dictc LLM-detail identity and ime-eval Judge/corpus SHA-256'
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

# Pad cryptographic implementation dependencies stay in the isolated worker.
# `zeroize` also clears secret transport buffers in the wire crate and renderer;
# it does not bring the KDF or cipher implementation into either process.
$PadWorkerRuntime = [ordered]@{
    'argon2'         = 'experimental Pad password KDF; sakura-pad-worker only'
    'aes-gcm'        = 'experimental Pad authenticated encryption; sakura-pad-worker only'
    'aes'            = 'AES implementation for isolated Pad worker'
    'aead'           = 'authenticated-encryption API for isolated Pad worker'
    'cipher'         = 'cipher traits for isolated Pad worker'
    'ctr'            = 'AES-GCM counter mode for isolated Pad worker'
    'ghash'          = 'AES-GCM authentication for isolated Pad worker'
    'polyval'        = 'GHASH field implementation for isolated Pad worker'
    'universal-hash' = 'POLYVAL hash interface for isolated Pad worker'
    'opaque-debug'   = 'cryptographic trait implementation detail for isolated Pad worker'
    'inout'          = 'cipher buffer API for isolated Pad worker'
    'subtle'         = 'constant-time operations for isolated Pad worker'
    'blake2'         = 'Argon2 hash primitive for isolated Pad worker'
    'base64ct'       = 'Argon2 encoding dependency; isolated Pad worker only'
    'password-hash'  = 'Argon2 alloc-feature dependency; isolated Pad worker only'
    'rand_core'      = 'password-hash salt source dependency; isolated Pad worker only'
    'zeroize'        = 'secret-buffer clearing for isolated Pad worker'
}

$PadWorkerPackage = 'sakura-pad-worker'
$PadSecretBufferConsumers = @($PadWorkerPackage, 'sakura-pad-session-proto', 'sakura-renderer')

# These tools produce build artifacts but are not shipping runtime binaries.
# A dependency admitted for dictc must not therefore become available to an IME
# runtime transitively. Check the resolved graph, not just direct manifests.
$RuntimeCrates = @(
    'sakura-core', 'sakura-proto', 'sakura-ipc', 'sakura-reg', 'sakura-user-prefs',
    'sakura-install-maintenance', 'sakura-tsf',
    'sakura-engine', 'sakura-renderer', 'sakura-pad-session-proto',
    'sakura-regtool', 'sakura-logon', 'sakura-settings'
)
# Tools that stay nested Cargo workspaces keep their own lockfile, which the
# root lock never sees (R11). candidate-snapshot stays nested on purpose: the
# directory is copied into historical release worktrees so its relative
# sakura-core path resolves to that release's core. Its lock is audited against
# the same rule, with its own package names counted as workspace crates.
$NestedToolWorkspaces = @(
    'tools/candidate-snapshot'
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
        if ($PadWorkerRuntime.Contains($name)) { continue }
        $offenders.Add($name)
    }
    return , $offenders.ToArray()
}

function Get-PadWorkerDependencyLeak {
    param(
        [Parameter(Mandatory)][string]$Consumer,
        [Parameter(Mandatory)][AllowEmptyCollection()][string[]]$PackageName
    )

    if ($Consumer -eq $PadWorkerPackage) { return , @() }
    return , @($PackageName | Where-Object {
            $PadWorkerRuntime.Contains($_) -and
            ($_ -ne 'zeroize' -or $PadSecretBufferConsumers -notcontains $Consumer)
        } | Sort-Object -Unique)
}

function Invoke-SelfTest {
    $workspace = @('sakura-core', 'sakura-tsf')
    $failures = New-Object System.Collections.Generic.List[string]

    $allowed = @(
        'sakura-core', 'sakura-tsf',
        'windows', 'windows-core', 'windows_x86_64_msvc', 'windows-implement',
        'proc-macro2', 'quote', 'syn', 'unicode-ident',
        'ort', 'serde', 'serde_json', 'sha2',
        'argon2', 'aes-gcm', 'aes', 'aead', 'cipher', 'ctr', 'ghash',
        'polyval', 'universal-hash', 'opaque-debug', 'inout', 'subtle',
        'blake2', 'base64ct', 'password-hash', 'rand_core', 'zeroize'
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

    $padCryptoFixture = @('argon2', 'aes-gcm', 'aes', 'aead', 'cipher', 'zeroize')
    if ((Get-PadWorkerDependencyLeak -Consumer $PadWorkerPackage -PackageName $padCryptoFixture).Count -ne 0) {
        $failures.Add('Pad worker was rejected from its isolated crypto dependency set')
    }
    $padCryptoLeaks = Get-PadWorkerDependencyLeak -Consumer 'sakura-ai-worker' -PackageName $padCryptoFixture
    if ($padCryptoLeaks.Count -ne $padCryptoFixture.Count) {
        $failures.Add('Pad crypto isolation did not reject the synthetic AI-worker dependency set')
    }
    foreach ($consumer in @('sakura-pad-session-proto', 'sakura-renderer')) {
        $leaks = Get-PadWorkerDependencyLeak -Consumer $consumer -PackageName $padCryptoFixture
        if ($leaks.Count -ne $padCryptoFixture.Count - 1 -or $leaks -contains 'zeroize') {
            $failures.Add("Pad secret buffer consumer '$consumer' was not restricted to zeroize")
        }
    }

    # R11: every nested tool workspace must still be readable, or the audit of
    # its lock would silently stop. A stale entry fails here, not as a pass.
    foreach ($tool in $NestedToolWorkspaces) {
        $toolRoot = Join-Path $repoRoot $tool
        try {
            $names = Get-WorkspaceCrateName -Manifest (Join-Path $toolRoot 'Cargo.toml')
            if ($names.Count -eq 0) { $failures.Add("nested tool '$tool' declares no package") }
            $locked = Get-LockedPackageName -Lock (Join-Path $toolRoot 'Cargo.lock')
            $leaks = Get-PadWorkerDependencyLeak -Consumer $tool -PackageName $locked
            if ($leaks.Count -ne 0) {
                $failures.Add("Pad crypto leaked into nested workspace '$tool': $($leaks -join ', ')")
            }
        } catch {
            $failures.Add("nested tool '$tool' cannot be audited: $_")
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
$offenders = New-Object System.Collections.Generic.List[string]
foreach ($name in (Get-DisallowedPackage -PackageName $packages -WorkspaceCrate $workspaceCrates)) {
    $offenders.Add($name)
}

Write-Host ("Checked {0} locked packages against the full-scratch rule (DESIGN.md 3.1)." -f (
        $packages | Sort-Object -Unique).Count)

foreach ($tool in $NestedToolWorkspaces) {
    $toolRoot = Join-Path $repoRoot $tool
    $toolCrates = @($workspaceCrates) + @(
        Get-WorkspaceCrateName -Manifest (Join-Path $toolRoot 'Cargo.toml'))
    $toolPackages = Get-LockedPackageName -Lock (Join-Path $toolRoot 'Cargo.lock')
    foreach ($name in (Get-DisallowedPackage -PackageName $toolPackages -WorkspaceCrate $toolCrates)) {
        $offenders.Add("$name (in $tool/Cargo.lock)")
    }
    foreach ($name in (Get-PadWorkerDependencyLeak -Consumer $tool -PackageName $toolPackages)) {
        $offenders.Add("$name (Pad-only dependency in $tool/Cargo.lock)")
    }
    Write-Host ("Checked {0} locked packages in nested tool workspace {1}." -f (
            $toolPackages | Sort-Object -Unique).Count, $tool)
}

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
    # dictc-core there) but cannot enter the shipping runtime binary.
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

# Keep the experimental Pad crypto closure out of all other workspace crates,
# including AI/neural workers and offline tools. Restricting the check to the
# dedicated package name makes adding a new direct or transitive dependency a
# deliberate policy change rather than a broad allowlist expansion.
foreach ($crate in ($workspaceCrates | Where-Object { $_ -ne $PadWorkerPackage })) {
    $tree = & cargo tree --locked -p $crate --edges normal --prefix none 2>$null
    if ($LASTEXITCODE -ne 0) {
        throw "could not inspect resolved dependency graph for Pad crypto isolation crate '$crate'"
    }
    $treePackages = @($tree | ForEach-Object {
        if ($_ -match '^([^ ]+) v') { $Matches[1] }
    })
    $leaks = Get-PadWorkerDependencyLeak -Consumer $crate -PackageName $treePackages
    if ($leaks.Count -ne 0) {
        throw "Pad-only dependency '$($leaks -join ', ')' leaked into '$crate'; only '$PadWorkerPackage' may use them"
    }
}

Write-Host 'No disallowed dependencies.' -ForegroundColor Green
exit 0
