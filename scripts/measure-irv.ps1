#Requires -Version 7
<#
.SYNOPSIS
    Measures IRV (Issue Reading Volume) for the benchmark suite in
    verification/irv/benchmarks.json and writes a machine-readable snapshot.

.DESCRIPTION
    Physical IRV = every file an agent must load for the benchmark's Issue
    shape, per category (production, tests, contracts, docs, config,
    verification), in LOC / bytes / file count. Inline `#[cfg(test)]` regions
    inside production files are reported separately so test relocation is
    visible without being mistaken for a semantic reduction.

    Semantic IRV = the ranges an agent must actually understand. It is
    recorded from the benchmark's agent-verified ranges plus an estimate, and
    is never derived from file size alone (plan §1a: moving tests to a
    sibling file lowers physical IRV only).

    -Compare <baseline.json> re-measures and applies the regression policy:
      * any benchmark whose physical LOC grows by more than
        thresholds.warn_physical_increase_ratio  -> WARN
      * a critical benchmark growing by more than
        thresholds.fail_critical_increase_ratio  -> FAIL (exit 1)
      * unconditional docs exceeding unconditional_docs.budget_bytes -> FAIL
        (with -DocsBudgetMode Warn: WARN, and only when the byte count also
        grew versus the baseline; used until CLAUDE.md / rules.md are trimmed)
      * a non-glob file listed in a benchmark that does not exist -> FAIL
        (a moved file must be re-pointed in benchmarks.json, never silently 0)

    -SelfTest builds a temporary benchmark set and checks that measurement,
    inline-test detection, WARN and FAIL paths behave.

.EXAMPLE
    pwsh ./scripts/measure-irv.ps1 -Out verification/irv/baseline.json
    pwsh ./scripts/measure-irv.ps1 -Compare verification/irv/baseline.json
#>
[CmdletBinding()]
param(
    [string]$Benchmarks = 'verification/irv/benchmarks.json',
    [string]$Out = '',
    [string]$Compare = '',
    [switch]$SelfTest,
    [ValidateSet('Fail', 'Warn')]
    [string]$DocsBudgetMode = 'Fail',
    [string]$RepositoryRoot = ''
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Resolve-Root {
    param([string]$Root)
    if ($Root) { return (Resolve-Path $Root).Path }
    return (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
}

function Expand-Entry {
    param([string]$Root, [string]$Entry)
    $full = Join-Path $Root $Entry
    if ($Entry -match '[\*\?]') {
        $dir = Split-Path $full -Parent
        $leaf = Split-Path $full -Leaf
        if (-not (Test-Path $dir)) { return @() }
        return @(Get-ChildItem -Path $dir -Filter $leaf -File | ForEach-Object { $_.FullName })
    }
    if (Test-Path $full -PathType Leaf) { return @($full) }
    $script:MissingEntries += $Entry
    return @()
}
$script:MissingEntries = @()

function Measure-File {
    param([string]$Path)
    $bytes = (Get-Item $Path).Length
    $text = [System.IO.File]::ReadAllText($Path)
    # wc -l semantics: count newline characters; a final unterminated line counts once.
    $lines = ([regex]::Matches($text, "`n")).Count
    if ($text.Length -gt 0 -and -not $text.EndsWith("`n")) { $lines++ }
    $inlineTest = 0
    if ($Path -match '\.rs$') {
        $index = 0
        foreach ($line in ($text -split "`n")) {
            $index++
            if ($line -match '^\s*#\[cfg\(test\)\]') { $inlineTest = $lines - $index + 1; break }
        }
    }
    [pscustomobject]@{ path = $Path; loc = $lines; bytes = $bytes; inline_test_loc = $inlineTest }
}

function Measure-Category {
    param([string]$Root, [object[]]$Entries)
    $files = @()
    foreach ($entry in $Entries) { $files += Expand-Entry -Root $Root -Entry $entry }
    $files = @($files | Sort-Object -Unique)
    $measured = @($files | ForEach-Object { Measure-File $_ })
    $loc = 0; $bytes = 0; $inline = 0
    foreach ($m in $measured) { $loc += $m.loc; $bytes += $m.bytes; $inline += $m.inline_test_loc }
    [pscustomobject]@{
        files = @($measured | ForEach-Object { $_.path.Substring($Root.Length).TrimStart('\', '/') -replace '\\', '/' })
        file_count = $measured.Count
        loc = $loc
        bytes = $bytes
        inline_test_loc = $inline
    }
}

function Measure-Semantic {
    param([string]$Root, [object]$Semantic)
    $rangeLoc = 0
    foreach ($r in @($Semantic.ranges)) { $rangeLoc += ($r.end - $r.start + 1) }
    [pscustomobject]@{
        method = $Semantic.method
        verified_range_loc = $rangeLoc
        estimated_loc = $Semantic.estimated_loc
        target_loc_after = $Semantic.target_loc_after
    }
}

function Measure-Suite {
    param([string]$Root, [object]$Suite)
    $categories = 'production', 'tests', 'contracts', 'docs', 'config', 'verification'
    $unconditional = Measure-Category -Root $Root -Entries @($Suite.unconditional_docs.files)
    $results = @()
    foreach ($b in $Suite.benchmarks) {
        $per = [ordered]@{}
        $totalLoc = 0; $totalBytes = 0; $totalFiles = 0; $inline = 0
        foreach ($c in $categories) {
            $entries = @()
            if ($b.PSObject.Properties[$c]) { $entries = @($b.$c) }
            $m = Measure-Category -Root $Root -Entries $entries
            $per[$c] = $m
            $totalLoc += $m.loc; $totalBytes += $m.bytes; $totalFiles += $m.file_count; $inline += $m.inline_test_loc
        }
        $results += [pscustomobject]@{
            id = $b.id
            issue_shape = $b.issue_shape
            critical = [bool]$b.critical
            entry_point = $b.entry_point
            physical = [pscustomobject]@{
                loc = $totalLoc
                bytes = $totalBytes
                file_count = $totalFiles
                inline_test_loc = $inline
                loc_excluding_inline_tests = $totalLoc - $inline
                with_unconditional_docs_bytes = $totalBytes + $unconditional.bytes
                categories = $per
            }
            semantic = Measure-Semantic -Root $Root -Semantic $b.semantic
        }
    }
    [pscustomobject]@{
        measured_at = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
        commit = (& git -C $Root rev-parse --short HEAD 2>$null)
        unconditional_docs = [pscustomobject]@{
            files = $unconditional.files
            bytes = $unconditional.bytes
            budget_bytes = $Suite.unconditional_docs.budget_bytes
        }
        thresholds = $Suite.thresholds
        benchmarks = $results
    }
}

function Compare-Snapshot {
    param([object]$Baseline, [object]$Current, [string]$DocsMode = 'Fail', [string[]]$Missing = @())
    $warn = $Current.thresholds.warn_physical_increase_ratio
    $fail = $Current.thresholds.fail_critical_increase_ratio
    $status = 'PASS'
    $lines = @()
    foreach ($m in $Missing) {
        $status = 'FAIL'
        $lines += "FAIL benchmark entry not found: $m (re-point benchmarks.json after moving files)"
    }
    $docBytes = $Current.unconditional_docs.bytes
    $docBudget = $Current.unconditional_docs.budget_bytes
    if ($docBytes -gt $docBudget) {
        if ($DocsMode -eq 'Warn') {
            $baseDocs = if ($Baseline.unconditional_docs) { [double]$Baseline.unconditional_docs.bytes } else { 0 }
            if ($docBytes -gt $baseDocs) {
                if ($status -eq 'PASS') { $status = 'WARN' }
                $lines += "WARN unconditional docs grew $baseDocs -> $docBytes B (over budget $docBudget B)"
            } else {
                $lines += "note unconditional docs $docBytes B over budget $docBudget B (not grown; Warn mode)"
            }
        } else {
            $status = 'FAIL'
            $lines += "FAIL unconditional docs $docBytes B > budget $docBudget B"
        }
    }
    foreach ($cur in $Current.benchmarks) {
        $base = $Baseline.benchmarks | Where-Object { $_.id -eq $cur.id } | Select-Object -First 1
        if (-not $base) { $lines += "WARN $($cur.id) has no baseline"; if ($status -eq 'PASS') { $status = 'WARN' }; continue }
        $b = [double]$base.physical.loc; $c = [double]$cur.physical.loc
        $ratio = if ($b -eq 0) { 0 } else { ($c - $b) / $b }
        $pct = [math]::Round($ratio * 100, 1)
        if ($cur.critical -and $ratio -gt $fail) {
            $status = 'FAIL'; $lines += "FAIL $($cur.id) physical LOC $b -> $c (+$pct%) exceeds critical +$([int]($fail*100))%"
        } elseif ($ratio -gt $warn) {
            if ($status -eq 'PASS') { $status = 'WARN' }
            $lines += "WARN $($cur.id) physical LOC $b -> $c (+$pct%) exceeds +$([int]($warn*100))%"
        } else {
            $lines += "ok   $($cur.id) physical LOC $b -> $c ($pct%)"
        }
    }
    [pscustomobject]@{ status = $status; lines = $lines }
}

function Invoke-SelfTest {
    $tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("irv-selftest-" + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path (Join-Path $tmp 'src') | Out-Null
    try {
        $prod = "fn a() {}`nfn b() {}`n#[cfg(test)]`nmod tests {`n}`n"
        [System.IO.File]::WriteAllText((Join-Path $tmp 'src/a.rs'), $prod)
        [System.IO.File]::WriteAllText((Join-Path $tmp 'CLAUDE.md'), ('x' * 100))
        $suite = [pscustomobject]@{
            unconditional_docs = [pscustomobject]@{ files = @('CLAUDE.md'); budget_bytes = 50 }
            thresholds = [pscustomobject]@{ warn_physical_increase_ratio = 0.10; fail_critical_increase_ratio = 0.25 }
            benchmarks = @([pscustomobject]@{
                id = 'T'; issue_shape = 't'; critical = $true; entry_point = 'src/a.rs'
                production = @('src/*.rs'); tests = @(); contracts = @(); docs = @(); config = @(); verification = @()
                semantic = [pscustomobject]@{ method = 'x'; ranges = @([pscustomobject]@{ file = 'src/a.rs'; start = 1; end = 2 }); estimated_loc = 2; target_loc_after = 2 }
            })
        }
        $snap = Measure-Suite -Root $tmp -Suite $suite
        $t = $snap.benchmarks[0]
        if ($t.physical.loc -ne 5) { throw "selftest: expected 5 loc, got $($t.physical.loc)" }
        if ($t.physical.inline_test_loc -ne 3) { throw "selftest: expected 3 inline test loc, got $($t.physical.inline_test_loc)" }
        if ($t.semantic.verified_range_loc -ne 2) { throw "selftest: expected 2 semantic loc" }
        # budget breach -> FAIL
        $cmp = Compare-Snapshot -Baseline $snap -Current $snap
        if ($cmp.status -ne 'FAIL') { throw "selftest: docs over budget must FAIL, got $($cmp.status)" }
        # within budget, +20% on critical -> WARN ; +40% -> FAIL
        $snap.unconditional_docs.budget_bytes = 1000
        $grown = Measure-Suite -Root $tmp -Suite $suite
        $grown.unconditional_docs.budget_bytes = 1000
        $grown.benchmarks[0].physical.loc = 6
        if ((Compare-Snapshot -Baseline $snap -Current $grown).status -ne 'WARN') { throw 'selftest: +20% must WARN' }
        $grown.benchmarks[0].physical.loc = 7
        if ((Compare-Snapshot -Baseline $snap -Current $grown).status -ne 'FAIL') { throw 'selftest: +40% critical must FAIL' }
        # Warn mode: over budget but not grown -> PASS with note; grown -> WARN
        $snap.unconditional_docs.budget_bytes = 1
        $same = Measure-Suite -Root $tmp -Suite $suite
        $same.unconditional_docs.budget_bytes = 1
        if ((Compare-Snapshot -Baseline $snap -Current $same -DocsMode Warn).status -ne 'PASS') { throw 'selftest: Warn mode, not grown must PASS' }
        $same.unconditional_docs.bytes = $same.unconditional_docs.bytes + 1
        if ((Compare-Snapshot -Baseline $snap -Current $same -DocsMode Warn).status -ne 'WARN') { throw 'selftest: Warn mode, grown must WARN' }
        # missing entry -> FAIL
        if ((Compare-Snapshot -Baseline $snap -Current $same -DocsMode Warn -Missing @('src/gone.rs')).status -ne 'FAIL') { throw 'selftest: missing entry must FAIL' }
        $grown.benchmarks[0].physical.loc = 5
        if ((Compare-Snapshot -Baseline $snap -Current $grown).status -ne 'PASS') { throw 'selftest: unchanged must PASS' }
        Write-Host 'PASS: measure-irv self-test'
    } finally {
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    }
}

if ($SelfTest) { Invoke-SelfTest; exit 0 }

$root = Resolve-Root $RepositoryRoot
$suite = Get-Content (Join-Path $root $Benchmarks) -Raw | ConvertFrom-Json
$snapshot = Measure-Suite -Root $root -Suite $suite

if ($Out) {
    $target = Join-Path $root $Out
    New-Item -ItemType Directory -Force -Path (Split-Path $target -Parent) | Out-Null
    $snapshot | ConvertTo-Json -Depth 8 | Set-Content -Path $target -Encoding utf8NoBOM
    Write-Host "wrote $Out"
}

foreach ($b in $snapshot.benchmarks) {
    '{0,-24} physical {1,7} LOC ({2,5} inline-test) {3,9} B {4,3} files | semantic est {5,5} LOC' -f `
        $b.id, $b.physical.loc, $b.physical.inline_test_loc, $b.physical.bytes, $b.physical.file_count, $b.semantic.estimated_loc
}
'{0,-24} unconditional docs {1} B (budget {2} B)' -f 'UNCONDITIONAL', $snapshot.unconditional_docs.bytes, $snapshot.unconditional_docs.budget_bytes

if ($Compare) {
    $baseline = Get-Content (Join-Path $root $Compare) -Raw | ConvertFrom-Json
    $result = Compare-Snapshot -Baseline $baseline -Current $snapshot -DocsMode $DocsBudgetMode -Missing $script:MissingEntries
    $result.lines | ForEach-Object { Write-Host $_ }
    Write-Host "IRV regression check: $($result.status)"
    if ($result.status -eq 'FAIL') { exit 1 }
}
exit 0
