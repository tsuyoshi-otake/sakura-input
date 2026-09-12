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

function Get-RustLexicalView {
    param([Parameter(Mandatory)][string]$Text)

    # Preserve character positions and newlines, but blank tokens that cannot
    # contain Rust attributes or item delimiters. The scan is bounded O(N).
    $view = $Text.ToCharArray()
    $length = $view.Length
    $index = 0
    $blockDepth = 0
    while ($index -lt $length) {
        if ($blockDepth -gt 0) {
            if ($index + 1 -lt $length -and $Text[$index] -eq '/' -and $Text[$index + 1] -eq '*') {
                if ($view[$index] -notin "`r", "`n") { $view[$index] = ' ' }
                if ($view[$index + 1] -notin "`r", "`n") { $view[$index + 1] = ' ' }
                $blockDepth++
                $index += 2
                continue
            }
            if ($index + 1 -lt $length -and $Text[$index] -eq '*' -and $Text[$index + 1] -eq '/') {
                if ($view[$index] -notin "`r", "`n") { $view[$index] = ' ' }
                if ($view[$index + 1] -notin "`r", "`n") { $view[$index + 1] = ' ' }
                $blockDepth--
                $index += 2
                continue
            }
            if ($view[$index] -notin "`r", "`n") { $view[$index] = ' ' }
            $index++
            continue
        }

        if ($index + 1 -lt $length -and $Text[$index] -eq '/' -and $Text[$index + 1] -eq '/') {
            while ($index -lt $length -and $Text[$index] -notin "`r", "`n") {
                $view[$index] = ' '
                $index++
            }
            continue
        }
        if ($index + 1 -lt $length -and $Text[$index] -eq '/' -and $Text[$index + 1] -eq '*') {
            $view[$index] = ' '
            $view[$index + 1] = ' '
            $blockDepth = 1
            $index += 2
            continue
        }

        # Raw strings: r"...", r#"..."#, br##"..."##. Starting at r
        # also handles the r within a byte-raw prefix.
        if ($Text[$index] -eq 'r') {
            $probe = $index + 1
            $hashes = 0
            while ($probe -lt $length -and $Text[$probe] -eq '#') { $hashes++; $probe++ }
            if ($probe -lt $length -and $Text[$probe] -eq '"') {
                $cursor = $index
                $end = -1
                $probe++
                while ($probe -lt $length) {
                    if ($Text[$probe] -eq '"') {
                        $terminatorMatches = $true
                        for ($hash = 0; $hash -lt $hashes; $hash++) {
                            if ($probe + 1 + $hash -ge $length -or $Text[$probe + 1 + $hash] -ne '#') {
                                $terminatorMatches = $false
                                break
                            }
                        }
                        if ($terminatorMatches) { $end = $probe + $hashes; break }
                    }
                    $probe++
                }
                if ($end -lt 0) { throw "unterminated Rust raw string at character $index" }
                while ($cursor -le $end) {
                    if ($view[$cursor] -notin "`r", "`n") { $view[$cursor] = ' ' }
                    $cursor++
                }
                $index = $end + 1
                continue
            }
        }

        if ($Text[$index] -eq '"') {
            $cursor = $index
            $index++
            $escaped = $false
            $closed = $false
            while ($index -lt $length) {
                $character = $Text[$index]
                if (-not $escaped -and $character -eq '"') { $closed = $true; $index++; break }
                if ($character -in "`r", "`n") { $escaped = $false }
                elseif (-not $escaped -and $character -eq '\') { $escaped = $true }
                else { $escaped = $false }
                $index++
            }
            if (-not $closed) { throw "unterminated Rust string at character $cursor" }
            while ($cursor -lt $index) {
                if ($view[$cursor] -notin "`r", "`n") { $view[$cursor] = ' ' }
                $cursor++
            }
            continue
        }

        if ($Text[$index] -eq "'") {
            $end = -1
            if ($index + 1 -lt $length -and $Text[$index + 1] -eq '\') {
                $escape = $index + 2
                if ($escape -ge $length -or $Text[$escape] -in "`r", "`n") {
                    throw "unterminated Rust character escape at character $index"
                }
                if ($Text[$escape] -eq 'u') {
                    $probe = $escape + 1
                    if ($probe -ge $length -or $Text[$probe] -ne '{') {
                        throw "malformed Rust Unicode character escape at character $index"
                    }
                    while ($probe -lt $length -and $Text[$probe] -ne '}') { $probe++ }
                    if ($probe -ge $length) { throw "unterminated Rust Unicode character escape at character $index" }
                    $closing = $probe + 1
                }
                elseif ($Text[$escape] -eq 'x') {
                    $closing = $escape + 3
                }
                else {
                    # Single-character escapes include quote and backslash.
                    $closing = $escape + 1
                }
                if ($closing -lt $length -and $Text[$closing] -eq "'") { $end = $closing }
            }
            elseif ($index + 2 -lt $length -and $Text[$index + 2] -eq "'") {
                $end = $index + 2
            }
            elseif ($index + 3 -lt $length -and
                [char]::IsHighSurrogate($Text[$index + 1]) -and
                [char]::IsLowSurrogate($Text[$index + 2]) -and
                $Text[$index + 3] -eq "'") {
                $end = $index + 3
            }
            if ($end -ge 0) {
                for ($cursor = $index; $cursor -le $end; $cursor++) { $view[$cursor] = ' ' }
                $index = $end + 1
                continue
            }
            if ($index + 1 -lt $length -and $Text[$index + 1] -eq '\') {
                throw "unterminated Rust character literal at character $index"
            }
        }
        $index++
    }
    if ($blockDepth -ne 0) { throw 'unterminated Rust block comment' }
    -join $view
}

function Get-RustAttributeGroupStart {
    param([string]$Lexical, [int]$AttributeStart)
    $cursor = $AttributeStart
    while ($true) {
        $probe = $cursor - 1
        while ($probe -ge 0 -and [char]::IsWhiteSpace($Lexical[$probe])) { $probe-- }
        if ($probe -lt 0 -or $Lexical[$probe] -ne ']') { break }
        $depth = 1
        $probe--
        while ($probe -ge 0 -and $depth -gt 0) {
            if ($Lexical[$probe] -eq ']') { $depth++ }
            elseif ($Lexical[$probe] -eq '[') { $depth-- }
            $probe--
        }
        if ($depth -ne 0) { throw 'unterminated Rust attribute before cfg(test)' }
        while ($probe -ge 0 -and [char]::IsWhiteSpace($Lexical[$probe])) { $probe-- }
        if ($probe -lt 0 -or $Lexical[$probe] -ne '#') { break }
        $cursor = $probe
    }
    $cursor
}

function Get-RustCfgItemEnd {
    param([string]$Lexical, [int]$AfterAttribute)

    $length = $Lexical.Length
    $cursor = $AfterAttribute
    # Skip whitespace and any attributes following cfg(test).
    while ($true) {
        while ($cursor -lt $length -and [char]::IsWhiteSpace($Lexical[$cursor])) { $cursor++ }
        if ($cursor -ge $length) { throw 'cfg(test) has no following Rust item' }
        if ($Lexical[$cursor] -ne '#') { break }
        $attributeBracket = $cursor + 1
        while ($attributeBracket -lt $length -and [char]::IsWhiteSpace($Lexical[$attributeBracket])) { $attributeBracket++ }
        if ($attributeBracket -ge $length -or $Lexical[$attributeBracket] -ne '[') { break }
        $depth = 1
        $cursor = $attributeBracket + 1
        while ($cursor -lt $length -and $depth -gt 0) {
            if ($Lexical[$cursor] -eq '[') { $depth++ }
            elseif ($Lexical[$cursor] -eq ']') { $depth-- }
            $cursor++
        }
        if ($depth -ne 0) { throw 'unterminated Rust attribute after cfg(test)' }
    }
    if ($cursor -ge $length) { throw 'cfg(test) has no following Rust item' }
    $itemStart = $cursor
    $prefixLength = [Math]::Min(240, $length - $itemStart)
    $itemPrefix = $Lexical.Substring($itemStart, $prefixLength)
    $visibility = '(?:pub(?:\s*\([^)]*\))?\s+)?'
    $isFunction = $itemPrefix -match "^\s*$visibility(?:(?:const|async|unsafe|default)\s+)*(?:extern\s+(?:`"[^`"]*`"\s+)?)?fn\b"
    $requiresSemicolon = -not $isFunction -and $itemPrefix -match "^\s*$visibility(?:static\b|type\b|use\b|const\s+)"
    $braceTerminated = $isFunction -or $itemPrefix -match "^\s*$visibility(?:impl\b|mod\b|struct\b|enum\b|union\b|trait\b|macro_rules\s*!|if\b|for\b|while\b|loop\b|match\b)"
    $allowComma = -not $requiresSemicolon -and -not $braceTerminated
    # Angle depth is needed for function const generics and comma-terminated
    # fields/variants. Do not treat comparison operators in cfg(test) control
    # statements as generic delimiters.
    $trackAngle = $isFunction -or $allowComma

    $paren = 0
    $bracket = 0
    $angle = 0
    $brace = 0
    $headerBrace = 0
    $firstTopBrace = -1
    while ($cursor -lt $length) {
        $character = $Lexical[$cursor]
        switch ($character) {
            '(' { $paren++ }
            ')' { if ($paren -eq 0) { throw 'unbalanced parenthesis in cfg(test) item' }; $paren-- }
            '[' { $bracket++ }
            ']' { if ($bracket -eq 0) { throw 'unbalanced bracket in cfg(test) item' }; $bracket-- }
            '<' { if ($trackAngle -and $paren -eq 0 -and $bracket -eq 0 -and $brace -eq 0 -and $headerBrace -eq 0) { $angle++ } }
            '>' { if ($trackAngle -and $paren -eq 0 -and $bracket -eq 0 -and $brace -eq 0 -and $headerBrace -eq 0 -and $angle -gt 0) { $angle-- } }
            '{' {
                if ($paren -eq 0 -and $bracket -eq 0) {
                    if ($headerBrace -gt 0) { $headerBrace++ }
                    elseif ($isFunction -and $angle -gt 0) { $headerBrace = 1 }
                    else {
                        if ($brace -eq 0) { $firstTopBrace = $cursor }
                        $brace++
                    }
                }
            }
            '}' {
                if ($paren -eq 0 -and $bracket -eq 0 -and $headerBrace -gt 0) {
                    $headerBrace--
                }
                elseif ($paren -eq 0 -and $bracket -eq 0 -and $brace -gt 0) {
                    $brace--
                    if ($brace -eq 0 -and -not $requiresSemicolon) {
                        return [pscustomobject]@{ End = $cursor; Terminator = 'brace'; ItemStart = $itemStart }
                    }
                }
            }
            ';' {
                if ($paren -eq 0 -and $bracket -eq 0 -and $angle -eq 0 -and $brace -eq 0 -and $headerBrace -eq 0) {
                    return [pscustomobject]@{ End = $cursor; Terminator = 'semicolon'; ItemStart = $itemStart }
                }
            }
            ',' {
                if ($allowComma -and $paren -eq 0 -and $bracket -eq 0 -and $angle -eq 0 -and $brace -eq 0 -and $headerBrace -eq 0) {
                    return [pscustomobject]@{ End = $cursor; Terminator = 'comma'; ItemStart = $itemStart }
                }
            }
        }
        $cursor++
    }
    if ($firstTopBrace -ge 0 -or $paren -ne 0 -or $bracket -ne 0 -or $angle -ne 0 -or $brace -ne 0 -or $headerBrace -ne 0) {
        throw 'unterminated cfg(test) Rust item'
    }
    throw 'cfg(test) Rust item has no brace or semicolon terminator'
}

function Measure-RustInlineTestLoc {
    param([Parameter(Mandatory)][string]$Text)
    if ($Text.Length -eq 0) { return 0 }
    $lexical = Get-RustLexicalView $Text
    $lineStarts = [Collections.Generic.List[int]]::new()
    $lineStarts.Add(0)
    for ($index = 0; $index -lt $Text.Length; $index++) {
        if ($Text[$index] -eq "`n" -and $index + 1 -lt $Text.Length) { $lineStarts.Add($index + 1) }
    }
    $counted = [Collections.Generic.HashSet[int]]::new()
    $coveredThrough = -1
    $lineCursor = 0
    $pattern = [regex]'#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]'
    foreach ($match in $pattern.Matches($lexical)) {
        if ($match.Index -le $coveredThrough) { continue }
        $item = Get-RustCfgItemEnd $lexical ($match.Index + $match.Length)
        $itemPrefix = $lexical.Substring($item.ItemStart, [Math]::Min(160, $item.End - $item.ItemStart + 1))
        # An out-of-line module declaration has no inline test body. Its
        # cfg/path attributes must not consume production LOC.
        if ($item.Terminator -eq 'semicolon' -and $itemPrefix -match '^\s*(?:pub(?:\s*\([^)]*\))?\s+)?mod\s+[A-Za-z_][A-Za-z0-9_]*\s*;') {
            $coveredThrough = $item.End
            continue
        }
        $start = Get-RustAttributeGroupStart $lexical $match.Index
        while ($lineCursor + 1 -lt $lineStarts.Count -and $lineStarts[$lineCursor + 1] -le $start) { $lineCursor++ }
        $startLine = $lineCursor
        while ($lineCursor + 1 -lt $lineStarts.Count -and $lineStarts[$lineCursor + 1] -le $item.End) { $lineCursor++ }
        $endLine = $lineCursor
        for ($line = $startLine; $line -le $endLine; $line++) { [void]$counted.Add($line) }
        $coveredThrough = $item.End
    }
    $counted.Count
}

function Measure-File {
    param([string]$Path)
    $bytes = (Get-Item $Path).Length
    $text = [System.IO.File]::ReadAllText($Path)
    # wc -l semantics: count newline characters; a final unterminated line counts once.
    $lines = ([regex]::Matches($text, "`n")).Count
    if ($text.Length -gt 0 -and -not $text.EndsWith("`n")) { $lines++ }
    $inlineTest = 0
    if ($Path -match '\.rs$') {
        $inlineTest = Measure-RustInlineTestLoc $text
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
    $fixtureRoot = Join-Path $PSScriptRoot 'fixtures/irv-inline'
    $fixtureManifest = Get-Content -LiteralPath (Join-Path $fixtureRoot 'expected.json') -Raw | ConvertFrom-Json
    foreach ($case in $fixtureManifest.cases) {
        $fixturePath = Join-Path $fixtureRoot ([string]$case.file)
        $fixtureText = [IO.File]::ReadAllText($fixturePath)
        if ($case.PSObject.Properties['expect_error'] -and [bool]$case.expect_error) {
            $failedAsExpected = $false
            try { [void](Measure-RustInlineTestLoc $fixtureText) } catch { $failedAsExpected = $true }
            if (-not $failedAsExpected) { throw "selftest: malformed fixture must fail: $($case.file)" }
            continue
        }
        $actualInline = Measure-RustInlineTestLoc $fixtureText
        if ($actualInline -ne [int]$case.inline_test_loc) {
            throw "selftest: $($case.file) expected $($case.inline_test_loc) inline LOC, got $actualInline"
        }
    }

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
