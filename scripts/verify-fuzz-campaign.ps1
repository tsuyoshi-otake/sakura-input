[CmdletBinding()]
param(
    [string]$StateDirectory = (Join-Path $PSScriptRoot '..\.codex\goal-loop\all-phases\phase5\fuzz'),

    [ValidateRange(0.001, 10000.0)]
    [double]$RequiredHours = 72.0,

    [ValidateRange(1, 64)]
    [int]$RequiredShards = 4,

    [string]$SummaryPath = '',

    [switch]$ProgressOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$root = [IO.Path]::GetFullPath($StateDirectory)
if ([string]::IsNullOrWhiteSpace($SummaryPath)) {
    $SummaryPath = Join-Path $root 'campaign-summary.json'
}
$summaryFile = [IO.Path]::GetFullPath($SummaryPath)
$requiredSeconds = $RequiredHours * 3600.0
$targets = @('ipc', 'dictionary', 'fsm')
$reasons = [Collections.Generic.List[string]]::new()
$states = @{}
$intervals = @{
    ipc = [Collections.Generic.List[object]]::new()
    dictionary = [Collections.Generic.List[object]]::new()
    fsm = [Collections.Generic.List[object]]::new()
}

function Get-Sha256 {
    param([Parameter(Mandatory)][string]$Path)

    $stream = [IO.File]::OpenRead($Path)
    try {
        $algorithm = [Security.Cryptography.SHA256]::Create()
        try { return [Convert]::ToHexString($algorithm.ComputeHash($stream)).ToLowerInvariant() }
        finally { $algorithm.Dispose() }
    }
    finally { $stream.Dispose() }
}

function Resolve-EvidencePath {
    param(
        [Parameter(Mandatory)][string]$StatePath,
        [Parameter(Mandatory)][string]$RelativePath
    )

    if ([string]::IsNullOrWhiteSpace($RelativePath) -or [IO.Path]::IsPathRooted($RelativePath)) {
        return $null
    }
    $stateDirectory = [IO.Path]::GetFullPath((Split-Path -Parent $StatePath))
    $candidate = [IO.Path]::GetFullPath((Join-Path $stateDirectory $RelativePath))
    $prefix = $stateDirectory.TrimEnd([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
    if (-not $candidate.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) {
        return $null
    }
    return $candidate
}

function Get-UnionSeconds {
    param([Parameter(Mandatory)][AllowEmptyCollection()][object[]]$Ranges)

    if ($Ranges.Count -eq 0) { return 0.0 }
    $ordered = @($Ranges | Sort-Object StartUtc, EndUtc)
    $start = $ordered[0].StartUtc
    $end = $ordered[0].EndUtc
    $seconds = 0.0
    foreach ($range in $ordered | Select-Object -Skip 1) {
        if ($range.StartUtc -le $end) {
            if ($range.EndUtc -gt $end) { $end = $range.EndUtc }
        }
        else {
            $seconds += ($end - $start).TotalSeconds
            $start = $range.StartUtc
            $end = $range.EndUtc
        }
    }
    return $seconds + ($end - $start).TotalSeconds
}

if (-not [IO.Directory]::Exists($root)) {
    $reasons.Add("campaign state directory is missing: $root")
    $stateFiles = @()
}
else {
    $options = [IO.EnumerationOptions]::new()
    $options.RecurseSubdirectories = $true
    $options.IgnoreInaccessible = $true
    $options.AttributesToSkip = [IO.FileAttributes]::ReparsePoint
    $stateFiles = @([IO.Directory]::EnumerateFiles($root, '*.state.json', $options))
}

foreach ($statePath in $stateFiles) {
    try {
        $state = [IO.File]::ReadAllText($statePath, [Text.Encoding]::UTF8) | ConvertFrom-Json
        $identity = "{0}:{1}" -f [string]$state.target, [int]$state.shard
        if ($states.ContainsKey($identity)) {
            $reasons.Add("duplicate state for $identity")
            continue
        }
        $states[$identity] = $statePath
        if ($state.schema_version -ne 1) { $reasons.Add("$identity has an unsupported schema") }
        if ($state.target -notin $targets -or [int]$state.shard -lt 0 -or [int]$state.shard -ge $RequiredShards) {
            $reasons.Add("$identity is outside the required campaign matrix")
            continue
        }
        if ($state.run_status -ne 'ready') { $reasons.Add("$identity is not at the explicit ready terminal state") }

        $seenSeeds = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
        foreach ($receipt in @($state.receipts)) {
            $label = "$identity seed $($receipt.seed)"
            if (-not $seenSeeds.Add([string]$receipt.seed)) { $reasons.Add("$label is duplicated") }
            if ($receipt.status -ne 'passed' -or $receipt.exit_code -ne 0) {
                $reasons.Add("$label recorded terminal status '$($receipt.status)' / exit '$($receipt.exit_code)'")
                continue
            }
            if ([uint64]$receipt.iterations -lt 1) { $reasons.Add("$label records no iterations") }

            try {
                $started = [DateTimeOffset]::Parse(
                    [string]$receipt.started_at_utc,
                    [Globalization.CultureInfo]::InvariantCulture,
                    [Globalization.DateTimeStyles]::RoundtripKind
                )
                $ended = [DateTimeOffset]::Parse(
                    [string]$receipt.ended_at_utc,
                    [Globalization.CultureInfo]::InvariantCulture,
                    [Globalization.DateTimeStyles]::RoundtripKind
                )
            }
            catch {
                $reasons.Add("$label has invalid UTC timestamps")
                continue
            }
            if ($ended -lt $started) { $reasons.Add("$label ends before it starts"); continue }
            if ($ended -gt [DateTimeOffset]::UtcNow.AddMinutes(5)) { $reasons.Add("$label ends in the future") }
            $wallSeconds = ($ended - $started).TotalSeconds
            $recordedSeconds = [double]$receipt.elapsed_seconds
            $tolerance = [Math]::Max(5.0, $wallSeconds * 0.05)
            if ([Math]::Abs($wallSeconds - $recordedSeconds) -gt $tolerance) {
                $reasons.Add("$label wall-clock and monotonic elapsed time disagree")
                continue
            }

            foreach ($stream in @('stdout', 'stderr')) {
                $relative = [string]$receipt.("${stream}_path")
                $expectedHash = [string]$receipt.("${stream}_sha256")
                $path = Resolve-EvidencePath -StatePath $statePath -RelativePath $relative
                if ($null -eq $path -or -not [IO.File]::Exists($path)) {
                    $reasons.Add("$label $stream evidence is missing or escapes its state directory")
                }
                elseif ($expectedHash -notmatch '^[0-9a-f]{64}$' -or (Get-Sha256 $path) -ne $expectedHash) {
                    $reasons.Add("$label $stream evidence hash does not match")
                }
            }

            $intervals[[string]$state.target].Add([pscustomobject]@{
                StartUtc = $started
                EndUtc = $ended
            })
        }
    }
    catch {
        $reasons.Add("state could not be graded ($statePath): $($_.Exception.Message)")
    }
}

foreach ($target in $targets) {
    foreach ($shard in 0..($RequiredShards - 1)) {
        $identity = "${target}:$shard"
        if (-not $states.ContainsKey($identity)) { $reasons.Add("required state $identity is missing") }
    }
}

$progress = [ordered]@{}
$allStarts = [Collections.Generic.List[DateTimeOffset]]::new()
$allEnds = [Collections.Generic.List[DateTimeOffset]]::new()
foreach ($target in $targets) {
    $ranges = @($intervals[$target])
    $unionSeconds = Get-UnionSeconds -Ranges $ranges
    foreach ($range in $ranges) {
        $allStarts.Add($range.StartUtc)
        $allEnds.Add($range.EndUtc)
    }
    $targetPassed = $unionSeconds -ge $requiredSeconds
    if (-not $targetPassed) {
        $reasons.Add(("{0} has {1:N3} verified non-overlapping hours; {2:N3} required" -f $target, ($unionSeconds / 3600.0), $RequiredHours))
    }
    $progress[$target] = [ordered]@{
        verified_union_hours = [Math]::Round($unionSeconds / 3600.0, 6)
        required_hours = $RequiredHours
        receipt_count = $ranges.Count
        passed = $targetPassed
    }
}

$campaignSpanHours = if ($allStarts.Count -eq 0) {
    0.0
}
else {
    (($allEnds | Sort-Object | Select-Object -Last 1) - ($allStarts | Sort-Object | Select-Object -First 1)).TotalHours
}
if ($campaignSpanHours -lt $RequiredHours) {
    $reasons.Add(("campaign elapsed span is {0:N3} hours; {1:N3} required" -f $campaignSpanHours, $RequiredHours))
}

$summary = [ordered]@{
    schema_version = 1
    generated_at_utc = [DateTime]::UtcNow.ToString('o')
    state_directory = $root
    required_hours_per_target = $RequiredHours
    required_shards = $RequiredShards
    campaign_elapsed_span_hours = [Math]::Round($campaignSpanHours, 6)
    progress = $progress
    reasons = @($reasons)
    passed = $reasons.Count -eq 0
}

[IO.Directory]::CreateDirectory((Split-Path -Parent $summaryFile)) | Out-Null
$temporary = "$summaryFile.$PID.tmp"
[IO.File]::WriteAllText($temporary, ($summary | ConvertTo-Json -Depth 10), [Text.UTF8Encoding]::new($false))
[IO.File]::Move($temporary, $summaryFile, $true)
$summary | ConvertTo-Json -Depth 10

if ($summary.passed -or $ProgressOnly) { exit 0 }
exit 1
