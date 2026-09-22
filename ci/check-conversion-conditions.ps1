[CmdletBinding()]
param(
    [string]$LogPath,
    [string]$BranchCoveragePath,
    [string]$OutputPath,
    [switch]$SelfTest
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Measure-Decision {
    param([string[]]$Rows)
    $cases = @{}
    foreach ($row in $Rows) {
        if ($row -notmatch '^([01]+) ([01])$') { throw "Invalid evidence: $row" }
        if ($cases.ContainsKey($Matches[1])) { throw 'Duplicate condition vector' }
        $cases[$Matches[1]] = [int]$Matches[2]
    }
    if ($cases.Count -eq 0) { throw 'No evidence' }
    $width = (@($cases.Keys)[0]).Length
    if ($width -gt 8 -or @($cases.Keys | Where-Object Length -ne $width).Count) { throw 'Invalid condition width' }
    if ($cases.Count -ne [math]::Pow(2, $width)) { throw 'MCC has missing combinations' }
    $pairs = @()
    for ($condition = 0; $condition -lt $width; $condition++) {
        $witness = $null
        foreach ($vector in @($cases.Keys | Sort-Object)) {
            $other = $vector.ToCharArray()
            $other[$condition] = if ($other[$condition] -eq '0') { '1' } else { '0' }
            $other = -join $other
            if ($cases[$vector] -ne $cases[$other]) {
                $witness = "$vector/$other"
                break
            }
        }
        if ($null -eq $witness) { throw "MC/DC has no independent pair for condition $condition" }
        $pairs += $witness
    }
    [pscustomobject]@{ Combinations=$cases.Count; Conditions=$width; Pairs=$pairs }
}

function Merge-AtomicBranches {
    param([object[]]$Branches)
    # A library may be instrumented in several test binaries. LLVM exports one
    # entry per instantiation; combine only identical source ranges, not operands.
    $locations = @{}
    foreach ($branch in $Branches) {
        if ($branch.Count -ne 9 -or $branch[8] -ne 4) { throw 'Unexpected LLVM branch record' }
        $key = $branch[0..3] -join ':'
        if (-not $locations.ContainsKey($key)) {
            $locations[$key] = [pscustomobject]@{ TrueCount=[long]0; FalseCount=[long]0 }
        }
        $locations[$key].TrueCount += [long]$branch[4]
        $locations[$key].FalseCount += [long]$branch[5]
    }
    $locations.Values
}

if ($SelfTest) {
    $and = Measure-Decision @('00 0','01 0','10 0','11 1')
    if ($and.Combinations -ne 4 -or ($and.Pairs -join ',') -cne '01/11,10/11') { throw 'AND witnesses incorrect' }
    $or = Measure-Decision @('00 0','01 1','10 1','11 1')
    if (($or.Pairs -join ',') -cne '00/10,00/01') { throw 'OR witnesses incorrect' }
    foreach ($invalid in @(
        @('00 0','01 1','10 1'),
        @('00 0','01 0','10 1','11 1'),
        @('00 0','01 1','10 1','11 1','11 1')
    )) {
        $rejected = $false
        try { $null = Measure-Decision $invalid } catch { $rejected = $true }
        if (-not $rejected) { throw 'Missing coverage was accepted' }
    }
    $merged = @(Merge-AtomicBranches @(
        @(1,2,1,5,2,0,0,0,4), @(1,2,1,5,0,3,0,0,4), @(1,9,1,12,0,0,0,0,4)
    ))
    if ($merged.Count -ne 2 -or @($merged | Where-Object { $_.TrueCount -eq 2 -and $_.FalseCount -eq 3 }).Count -ne 1) {
        throw 'Branch instantiations were not merged by source range'
    }
    'PASS: condition-coverage checker self-test'
    exit 0
}
if (-not $LogPath -or -not $BranchCoveragePath -or -not $OutputPath) {
    throw 'LogPath, BranchCoveragePath and OutputPath are required'
}
$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$catalog = @(
    @('core.single_kanji_guard', 'crates/sakura-core/src/conversion/synthesis/single_kanji.rs', 'if self.candidates.len() >= wanted || !dictionary.has_single_kanji() {', 1, 'full, table absent'),
    @('projection.tail_overflow', 'crates/sakura-engine/src/candidate_projection.rs', 'if allow_tail_overflow && projection.visible_count > 0 {', 1, 'tail allowed, prefix exists'),
    @('history.planned_mismatch', 'crates/sakura-engine/src/input_history.rs', 'if backup.as_ref().is_some_and(|image| image.hash != old)', 2, 'backup mismatch, replacement mismatch'),
    @('history.legacy_mismatch', 'crates/sakura-engine/src/input_history.rs', 'if backup', 6, 'backup differs, replacement differs'),
    @('history.canonical_generation', 'crates/sakura-engine/src/input_history.rs', 'Some(image) if image.hash == old || image.hash == new => {}', 1, 'matches old, matches new'),
    @('history.plan_shape', 'crates/sakura-engine/src/input_history.rs', 'if plan.len() != 72 || &plan[..8] != b"SKCP0001" {', 1, 'wrong length, wrong magic')
)
$log = [IO.File]::ReadAllText([IO.Path]::GetFullPath($LogPath))
if ($log -match 'test result: FAILED' -or $log -notmatch 'test result: ok\.') { throw 'Evidence is not a passing test log' }
$observations = @{}
foreach ($match in [regex]::Matches($log, 'decision-evidence ([a-z_.]+) ([01]+) ([01])')) {
    $id = $match.Groups[1].Value
    if (-not $observations.ContainsKey($id)) { $observations[$id] = @() }
    $observations[$id] += "$($match.Groups[2].Value) $($match.Groups[3].Value)"
}
if ($observations.Count -ne $catalog.Count) { throw 'Unexpected/missing decision evidence' }
$coverage = [IO.File]::ReadAllText([IO.Path]::GetFullPath($BranchCoveragePath)) | ConvertFrom-Json
$lines = [Collections.Generic.List[string]]::new()
$lines.Add('# Conversion correction condition evidence')
$lines.Add('')
$lines.Add('Scope: six explicitly listed production decisions. MCC and unique-cause MC/DC are witnessed by real-operation fixtures, with both sides of every corresponding atomic branch confirmed by LLVM branch counters. This is not compiler-generated MC/DC or whole-workspace MC/DC coverage.')
$lines.Add('')
$lines.Add('| Decision (condition order) | MCC | MC/DC | Independent pairs | Atomic branch outcomes | LF-normalized source SHA-256 |')
$lines.Add('|---|---:|---:|---|---:|---|')
foreach ($item in $catalog) {
    $id, $relative, $anchor, $span, $description = $item
    if (-not $observations.ContainsKey($id)) { throw "Missing $id" }
    $result = Measure-Decision $observations[$id]
    $path = Join-Path $root $relative
    $source = [IO.File]::ReadAllLines($path)
    $positions = @(for ($i=0; $i -lt $source.Length; $i++) { if ($source[$i].Trim() -ceq $anchor) { $i+1 } })
    if ($positions.Count -ne 1) { throw "Ambiguous or stale source anchor: $id" }
    $start = $positions[0]
    $file = @($coverage.data[0].files | Where-Object { [IO.Path]::GetFullPath($_.filename) -eq $path })
    if ($file.Count -ne 1) { throw "Missing instrumented file: $relative" }
    $branches = @(Merge-AtomicBranches @($file[0].branches | Where-Object { $_[0] -ge $start -and $_[0] -lt ($start + [int]$span) }))
    if ($branches.Count -ne $result.Conditions -or @($branches | Where-Object { $_.TrueCount -eq 0 -or $_.FalseCount -eq 0 }).Count) {
        throw "Atomic production branches are missing a polarity: $id"
    }
    $normalized = [IO.File]::ReadAllText($path).Replace("`r`n", "`n")
    $hash = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes($normalized))).ToLowerInvariant()
    $lines.Add("| $id ($description) | $($result.Combinations)/$($result.Combinations) | $($result.Conditions)/$($result.Conditions) | $($result.Pairs -join ', ') | $($branches.Count*2)/$($branches.Count*2) | $hash |")
}
$lines.Add('')
$lines.Add('The fixtures assert concrete candidate contents, overflow behavior, exact recovery errors, and byte-preservation/cleanup outcomes before emitting each vector. Short-circuited operands have fixture-established values; LLVM counters separately prove each operand was actually evaluated both true and false in the instrumented suite. No skipped operand is counted as an executed LLVM branch.')
$lines.Add('')
$lines.Add('For OR decisions the pairs are 00/10 and 00/01; for AND decisions they are 01/11 and 10/11. Each pair changes exactly one condition and changes the observed decision outcome. The checker rejects missing combinations, duplicate vectors, or a condition with no such pair.')
$lines.Add('')
$lines.Add('Multiple instrumented test binaries can report the same atomic source range. Their counters are summed by that exact range; distinct operands remain distinct. The checker also requires both outcomes for each operand, rather than accepting a test executable exit code as coverage.')
[IO.File]::WriteAllLines([IO.Path]::GetFullPath($OutputPath), $lines)
'PASS: 24/24 combinations, 12/12 independently effective conditions, 24/24 atomic branch outcomes (six decisions)'
