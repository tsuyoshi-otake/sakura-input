[CmdletBinding()]
param(
    [string]$EvidenceDirectory = (Join-Path $PSScriptRoot '..\.codex\goal-loop\all-phases'),
    [string]$ReportPath = (Join-Path $PSScriptRoot '..\.codex\goal-loop\all-phases\all-phases-summary.json'),
    [TimeSpan]$MaximumEvidenceAge = ([TimeSpan]::FromHours(24)),
    [switch]$EngineeringOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repository = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$evidenceRoot = [IO.Path]::GetFullPath($EvidenceDirectory)
$summaryPath = [IO.Path]::GetFullPath($ReportPath)
$processCheck = Join-Path $repository 'ci\check-process-clean.ps1'
$started = [DateTimeOffset]::UtcNow
$localSteps = [Collections.Generic.List[object]]::new()
$localPassed = $true

if ($MaximumEvidenceAge -le [TimeSpan]::Zero) {
    throw 'MaximumEvidenceAge must be positive'
}
[IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($summaryPath)) | Out-Null

function Get-Sha256 {
    param([Parameter(Mandatory)][string]$Path)

    $stream = [IO.File]::OpenRead($Path)
    try {
        $algorithm = [Security.Cryptography.SHA256]::Create()
        try {
            return [Convert]::ToHexString($algorithm.ComputeHash($stream)).ToLowerInvariant()
        }
        finally { $algorithm.Dispose() }
    }
    finally { $stream.Dispose() }
}

function Confirm-ProcessClean {
    & rtk proxy pwsh -NoProfile -ExecutionPolicy Bypass -File $processCheck -RepositoryRoot $repository
    if ($LASTEXITCODE -eq 0) { return }

    Write-Warning 'A local gate left a repository-scoped process; terminating parents first.'
    & rtk proxy pwsh -NoProfile -ExecutionPolicy Bypass -File $processCheck -RepositoryRoot $repository -Terminate
    if ($LASTEXITCODE -ne 0) { throw 'test processes survived the bounded cleanup attempt' }
    & rtk proxy pwsh -NoProfile -ExecutionPolicy Bypass -File $processCheck -RepositoryRoot $repository
    if ($LASTEXITCODE -ne 0) { throw 'process re-list was not clean after cleanup' }
    throw 'a gate leaked a process; cleanup succeeded but the gate fails'
}

function Invoke-Gate {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][string[]]$Arguments
    )

    Write-Host "==> $Name"
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $exitCode = 1
    $errorText = $null
    $processesClean = $false
    try {
        & rtk @Arguments
        $exitCode = $LASTEXITCODE
    }
    catch {
        $errorText = $_.Exception.Message
    }
    finally {
        try {
            Confirm-ProcessClean
            $processesClean = $true
        }
        catch {
            $errorText = if ($null -eq $errorText) {
                $_.Exception.Message
            }
            else {
                "$errorText; $($_.Exception.Message)"
            }
        }
        $watch.Stop()
    }

    $passed = $exitCode -eq 0 -and $processesClean -and $null -eq $errorText
    $record = [ordered]@{
        name = $Name
        seconds = [Math]::Round($watch.Elapsed.TotalSeconds, 3)
        exit_code = $exitCode
        processes_clean = $processesClean
        passed = $passed
    }
    if ($null -ne $errorText) { $record.error = $errorText }
    $localSteps.Add($record)
    if (-not $passed) { $script:localPassed = $false }
}

function Invoke-RtkCapture {
    param([Parameter(Mandatory)][string[]]$Arguments)

    $output = @(& rtk @Arguments 2>&1)
    $exitCode = $LASTEXITCODE
    if ($exitCode -ne 0) {
        $detail = @($output | ForEach-Object { $_.ToString() }) -join "`n"
        throw "rtk $($Arguments -join ' ') exited $exitCode$(if ($detail) { ": $detail" })"
    }
    return @($output | ForEach-Object { $_.ToString() }) -join "`n"
}

function Get-NestedValue {
    param(
        [Parameter(Mandatory)]$Object,
        [Parameter(Mandatory)][string[]]$Path
    )

    $current = $Object
    foreach ($segment in $Path) {
        if ($null -eq $current) { return $null }
        $property = $current.PSObject.Properties[$segment]
        if ($null -eq $property) { return $null }
        $current = $property.Value
    }
    return $current
}

function Get-BooleanFlag {
    param(
        [Parameter(Mandatory)]$Object,
        [Parameter(Mandatory)][string[]]$Path
    )

    return [bool](Get-NestedValue -Object $Object -Path $Path)
}

function Add-NestedReasons {
    param(
        [Parameter(Mandatory)][AllowEmptyCollection()][Collections.Generic.List[string]]$Destination,
        [Parameter(Mandatory)]$Object,
        [Parameter(Mandatory)][string[]]$Path
    )

    $value = Get-NestedValue -Object $Object -Path $Path
    if ($null -eq $value) { return }
    foreach ($reason in @($value)) {
        if (-not [string]::IsNullOrWhiteSpace([string]$reason)) {
            $Destination.Add([string]$reason)
        }
    }
}

function Read-PhaseEvidence {
    param([Parameter(Mandatory)][ValidateRange(0, 5)][int]$Phase)

    $path = Join-Path $evidenceRoot "phase$Phase\phase$Phase-summary.json"
    $reasons = [Collections.Generic.List[string]]::new()
    $raw = $null
    $stamp = $null
    $hash = $null
    if (-not [IO.File]::Exists($path)) {
        $reasons.Add("Phase $Phase summary is missing")
    }
    else {
        try {
            $hash = Get-Sha256 $path
            $raw = [IO.File]::ReadAllText($path, [Text.Encoding]::UTF8) | ConvertFrom-Json
            if ([int](Get-NestedValue $raw @('schema_version')) -ne 1 -or
                [int](Get-NestedValue $raw @('phase')) -ne $Phase) {
                $reasons.Add("Phase $Phase summary schema or phase is invalid")
            }
            $timestampField = if ($Phase -in @(2, 3, 4)) { 'completed_at_utc' } else { 'generated_at_utc' }
            $timestampText = [string](Get-NestedValue $raw @($timestampField))
            if ([string]::IsNullOrWhiteSpace($timestampText)) {
                $reasons.Add("Phase $Phase summary has no $timestampField")
            }
            else {
                $stamp = [DateTimeOffset]::Parse(
                    $timestampText,
                    [Globalization.CultureInfo]::InvariantCulture,
                    [Globalization.DateTimeStyles]::RoundtripKind
                )
                $now = [DateTimeOffset]::UtcNow
                if ($stamp -gt $now.AddMinutes(5)) {
                    $reasons.Add("Phase $Phase summary timestamp is in the future")
                }
                elseif (($now - $stamp) -gt $MaximumEvidenceAge) {
                    $reasons.Add("Phase $Phase summary is older than $($MaximumEvidenceAge.TotalHours) hours")
                }
            }
        }
        catch {
            $reasons.Add("Phase $Phase summary could not be graded: $($_.Exception.Message)")
        }
    }

    return [pscustomobject]@{
        phase = $Phase
        path = $path
        sha256 = $hash
        timestamp = $stamp
        raw = $raw
        reasons = @($reasons)
        valid = $reasons.Count -eq 0
    }
}

function Grade-Phase {
    param([Parameter(Mandatory)]$Evidence)

    $phase = [int]$Evidence.phase
    $summary = $Evidence.raw
    $externalReasons = [Collections.Generic.List[string]]::new()
    $engineeringPassed = $false
    $strictPassed = $false

    if ($Evidence.valid) {
        switch ($phase) {
            0 {
                $engineeringPassed = Get-BooleanFlag $summary @('engineering', 'passed')
                $strictPassed = $engineeringPassed -and (Get-BooleanFlag $summary @('github', 'passed'))
                Add-NestedReasons $externalReasons $summary @('github', 'reasons')
            }
            1 {
                $engineeringPassed = Get-BooleanFlag $summary @('engineering', 'passed')
                $strictPassed = $engineeringPassed -and (Get-BooleanFlag $summary @('real_host_matrix', 'passed'))
                Add-NestedReasons $externalReasons $summary @('real_host_matrix', 'reasons')
            }
            2 {
                $engineeringPassed = Get-BooleanFlag $summary @('passed')
                $strictPassed = $engineeringPassed
            }
            3 {
                $engineeringPassed = Get-BooleanFlag $summary @('engineering_passed')
                $strictPassed = $engineeringPassed -and (Get-BooleanFlag $summary @('dogfood', 'passed'))
                Add-NestedReasons $externalReasons $summary @('dogfood', 'reasons')
            }
            4 {
                $engineeringPassed = Get-BooleanFlag $summary @('engineering_passed')
                $phase3Complete = Get-BooleanFlag $summary @('phase3_complete')
                $reconversionPassed = Get-BooleanFlag $summary @('reconversion_host_matrix', 'passed')
                $dogfoodPassed = Get-BooleanFlag $summary @('dogfood', 'passed')
                $strictPassed = $engineeringPassed -and $phase3Complete -and $reconversionPassed -and $dogfoodPassed
                if (-not $phase3Complete) { $externalReasons.Add('Phase 3 elapsed dogfood prerequisite is incomplete') }
                Add-NestedReasons $externalReasons $summary @('reconversion_host_matrix', 'reasons')
                Add-NestedReasons $externalReasons $summary @('dogfood', 'reasons')
            }
            5 {
                $engineeringPassed = Get-BooleanFlag $summary @('engineering', 'passed')
                $compatibilityPassed = Get-BooleanFlag $summary @('compatibility', 'passed')
                $stagedUpdatePassed = Get-BooleanFlag $summary @('staged_update', 'passed')
                $fuzzPassed = Get-BooleanFlag $summary @('fuzz', 'passed')
                $bundlePassed = Get-BooleanFlag $summary @('signed_release_bundle', 'passed')
                $publishedPassed = Get-BooleanFlag $summary @('published_release', 'passed')
                $strictPassed = $engineeringPassed -and $compatibilityPassed -and $stagedUpdatePassed -and
                    $fuzzPassed -and $bundlePassed -and $publishedPassed
                Add-NestedReasons $externalReasons $summary @('external_reasons')
            }
        }
    }

    return [ordered]@{
        phase = $phase
        evidence_path = $Evidence.path
        evidence_sha256 = $Evidence.sha256
        evidence_timestamp_utc = if ($null -eq $Evidence.timestamp) { $null } else { $Evidence.timestamp.ToString('O') }
        evidence_valid = [bool]$Evidence.valid
        evidence_reasons = @($Evidence.reasons)
        engineering_passed = $engineeringPassed
        strict_passed = $strictPassed
        external_reasons = @($externalReasons)
    }
}

function Get-SourceAudit {
    $reasons = [Collections.Generic.List[string]]::new()
    $hits = [Collections.Generic.List[object]]::new()
    $inventory = [Collections.Generic.List[object]]::new()
    $roots = [Collections.Generic.List[string]]::new()
    $crates = Join-Path $repository 'crates'
    foreach ($crate in [IO.Directory]::EnumerateDirectories($crates)) {
        $source = Join-Path $crate 'src'
        if ([IO.Directory]::Exists($source)) { $roots.Add($source) }
    }
    foreach ($relative in @('scripts', 'installer', 'ci', '.github\workflows')) {
        $path = Join-Path $repository $relative
        if ([IO.Directory]::Exists($path)) { $roots.Add($path) }
    }

    $enumeration = [IO.EnumerationOptions]::new()
    $enumeration.RecurseSubdirectories = $true
    $enumeration.IgnoreInaccessible = $true
    $enumeration.AttributesToSkip = [IO.FileAttributes]::ReparsePoint
    $extensions = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($extension in @('.rs', '.ps1', '.iss', '.yml', '.yaml')) { $null = $extensions.Add($extension) }
    $files = @(
        foreach ($root in $roots) {
            foreach ($path in [IO.Directory]::EnumerateFiles($root, '*', $enumeration)) {
                if ($extensions.Contains([IO.Path]::GetExtension($path))) { $path }
            }
        }
    ) | Sort-Object -Unique

    $regexOptions = [Text.RegularExpressions.RegexOptions]::CultureInvariant -bor
        [Text.RegularExpressions.RegexOptions]::IgnoreCase -bor
        [Text.RegularExpressions.RegexOptions]::Multiline
    $patterns = @(
        [ordered]@{ name = 'unfinished Rust macro'; regex = [regex]::new('\b(?:todo|unimplemented)!\s*\(', $regexOptions) },
        [ordered]@{ name = 'unfinished work marker'; regex = [regex]::new('(?:^|\s)(?:TODO|FIXME)(?=\s*:|\b)', $regexOptions) },
        [ordered]@{
            name = 'shipping not-implemented marker'
            regex = [regex]::new(('\b(?:not ' + 'implemented|stub ' + 'until)\b'), $regexOptions)
        },
        [ordered]@{ name = 'stub print entry point'; regex = [regex]::new('println!\([^\r\n]*(?:stub|placeholder)', $regexOptions) }
    )
    foreach ($file in $files) {
        $text = [IO.File]::ReadAllText($file, [Text.Encoding]::UTF8)
        foreach ($pattern in $patterns) {
            foreach ($match in $pattern.regex.Matches($text)) {
                $line = 1 + [regex]::Matches($text.Substring(0, $match.Index), "`n").Count
                $relative = [IO.Path]::GetRelativePath($repository, $file).Replace('\', '/')
                $hits.Add([ordered]@{ rule = $pattern.name; path = $relative; line = $line; text = $match.Value })
                $reasons.Add("$($pattern.name) at ${relative}:$line")
            }
        }
    }

    $inventorySpecs = @(
        [ordered]@{
            name = 'watchdog reconnect terminal and exponential backoff'
            path = 'crates\sakura-renderer\src\watch.rs'
            tokens = @('RETRY_CEILING', 'ProtocolRejected', 'retry_schedule', 'protocol_rejection_keeps_exponential_backoff_instead_of_resetting_it')
        },
        [ordered]@{
            name = 'TSF reconnect dedupe window'
            path = 'crates\sakura-tsf\src\engine.rs'
            tokens = @('RETRY_INTERVAL', 'blocked_until', 'fn drop_link')
        },
        [ordered]@{
            name = 'user-dictionary single watcher, dedupe and error backoff'
            path = 'crates\sakura-engine\src\user_dictionary.rs'
            tokens = @('MAX_ERROR_BACKOFF', 'recv_timeout', 'previous == Some(observed)', 'fn stop_and_join')
        },
        [ordered]@{
            name = 'prediction one-attempt generation terminal'
            path = 'crates\sakura-engine\src\dispatch.rs'
            tokens = @('attempted_for(session_id, generation)', 'PREDICTION_TIMEOUT', 'one bounded retry against current history')
        },
        [ordered]@{
            name = 'bounded engine accept pool and explicit connection outcomes'
            path = 'crates\sakura-engine\src\server.rs'
            tokens = @('MAX_INSTANCES', 'enum Outcome', 'Outcome::Shutdown')
        },
        [ordered]@{
            name = 'updater redirects, rate-limit terminal and installer terminals'
            path = 'crates\sakura-settings\src\updater.rs'
            tokens = @('MAX_REDIRECTS', 'WINHTTP_OPTION_CONNECT_RETRIES', 'no immediate retry was attempted', 'installer_success_restart_timeout_and_failure_are_distinct_terminals')
        },
        [ordered]@{
            name = 'shutdown nested waits remain within the caller deadline'
            path = 'crates\sakura-regtool\src\shutdown.rs'
            tokens = @('cap_connect_budget(remaining(deadline))', 'each_connect_wait_is_capped_by_the_callers_remaining_budget')
        },
        [ordered]@{
            name = 'logon bootstrap has observable terminal state for every branch'
            path = 'crates\sakura-logon\src\lib.rs'
            tokens = @('pub struct Outcome', 'every_failure_combination_has_a_unique_observable_terminal_code')
        },
        [ordered]@{
            name = 'renderer message-pump error is terminal'
            path = 'crates\sakura-renderer\src\main.rs'
            tokens = @('while GetMessageW(&mut message, None, 0, 0).0 > 0')
        },
        [ordered]@{
            name = 'settings message-pump error is terminal'
            path = 'crates\sakura-settings\src\ui.rs'
            tokens = @('while GetMessageW(&mut message, None, 0, 0).0 > 0')
        }
    )
    foreach ($spec in $inventorySpecs) {
        $path = Join-Path $repository $spec.path
        $missing = [Collections.Generic.List[string]]::new()
        if (-not [IO.File]::Exists($path)) {
            $missing.Add('file missing')
        }
        else {
            $text = [IO.File]::ReadAllText($path, [Text.Encoding]::UTF8)
            foreach ($token in $spec.tokens) {
                if ($text.IndexOf([string]$token, [StringComparison]::Ordinal) -lt 0) {
                    $missing.Add([string]$token)
                }
            }
        }
        $itemPassed = $missing.Count -eq 0
        if (-not $itemPassed) {
            $reasons.Add("stateful inventory '$($spec.name)' is incomplete: $(@($missing) -join ', ')")
        }
        $inventory.Add([ordered]@{
            name = $spec.name
            path = $spec.path.Replace('\', '/')
            passed = $itemPassed
            missing = @($missing)
        })
    }

    $memoryPath = Join-Path $repository '.claude\memory\rules.md'
    $memoryTokens = @(
        'WM_DPICHANGED',
        'Opening the engine pipe is not a successful protocol handshake',
        'A Cargo target directory is not the installed product layout',
        'Diagnostic tier names must come from the same canonical vocabulary'
    )
    $memoryMissing = [Collections.Generic.List[string]]::new()
    if (-not [IO.File]::Exists($memoryPath)) {
        $memoryMissing.Add('memory file missing')
    }
    else {
        $memory = [IO.File]::ReadAllText($memoryPath, [Text.Encoding]::UTF8)
        foreach ($token in $memoryTokens) {
            if ($memory.IndexOf($token, [StringComparison]::Ordinal) -lt 0) { $memoryMissing.Add($token) }
        }
    }
    if ($memoryMissing.Count -ne 0) {
        $reasons.Add("verified project memory is incomplete: $(@($memoryMissing) -join ', ')")
    }

    $workflowReasons = [Collections.Generic.List[string]]::new()
    if ($null -eq (Get-Command ConvertFrom-Yaml -ErrorAction SilentlyContinue)) {
        $workflowReasons.Add('ConvertFrom-Yaml is unavailable')
    }
    else {
        foreach ($relative in @(
            '.github\workflows\ci.yml',
            '.github\workflows\installer.yml',
            '.github\workflows\fuzz-campaign.yml',
            '.github\workflows\release.yml'
        )) {
            try {
                $null = [IO.File]::ReadAllText((Join-Path $repository $relative), [Text.Encoding]::UTF8) | ConvertFrom-Yaml
            }
            catch { $workflowReasons.Add("$relative is not valid YAML: $($_.Exception.Message)") }
        }
    }
    foreach ($reason in $workflowReasons) { $reasons.Add($reason) }

    return [ordered]@{
        files_scanned = $files.Count
        forbidden_hits = @($hits)
        stateful_inventory = @($inventory)
        memory_path = $memoryPath
        memory_missing = @($memoryMissing)
        workflow_reasons = @($workflowReasons)
        reasons = @($reasons)
        passed = $reasons.Count -eq 0
    }
}

function Get-ReleaseState {
    $result = [ordered]@{
        repository = $null
        visibility = $null
        local_head = $null
        main_sha = $null
        tag_sha = $null
        worktree_entry_count = $null
        worktree_entries = @()
        issues = @()
        workflows = [ordered]@{}
        reasons = [Collections.Generic.List[string]]::new()
        passed = $false
    }

    try {
        $status = Invoke-RtkCapture @('git', 'status', '--porcelain')
        $entries = @($status -split "`r?`n" | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
        $result.worktree_entry_count = $entries.Count
        $result.worktree_entries = @($entries | Select-Object -First 40)
        if ($entries.Count -ne 0) { $result.reasons.Add("worktree has $($entries.Count) changed or untracked entries") }
        $result.local_head = (Invoke-RtkCapture @('git', 'rev-parse', 'HEAD')).Trim()
    }
    catch { $result.reasons.Add("local Git state could not be graded: $($_.Exception.Message)") }

    try {
        $result.tag_sha = (Invoke-RtkCapture @('git', 'rev-parse', 'refs/tags/v1.0.0')).Trim()
    }
    catch { $result.reasons.Add('local v1.0.0 tag is missing or unreadable') }

    try {
        $repo = Invoke-RtkCapture @('gh', 'repo', 'view', '--json', 'nameWithOwner,visibility,defaultBranchRef') | ConvertFrom-Json
        $result.repository = [string]$repo.nameWithOwner
        $result.visibility = [string]$repo.visibility
        if ($result.repository -cne 'tsuyoshi-otake/sakura-input' -or $result.visibility -cne 'PRIVATE' -or
            [string]$repo.defaultBranchRef.name -cne 'main') {
            $result.reasons.Add('repository identity, private visibility, or default branch is invalid')
        }
        $result.main_sha = (Invoke-RtkCapture @(
            'gh', 'api', 'repos/tsuyoshi-otake/sakura-input/commits/main', '--jq', '.sha'
        )).Trim()
    }
    catch { $result.reasons.Add("repository/main readback failed: $($_.Exception.Message)") }

    if ($result.local_head -and $result.main_sha -and $result.local_head -cne $result.main_sha) {
        $result.reasons.Add('local HEAD is not the final main SHA')
    }
    if ($result.tag_sha -and $result.main_sha -and $result.tag_sha -cne $result.main_sha) {
        $result.reasons.Add('v1.0.0 does not point at the final main SHA')
    }

    try {
        $issues = @(Invoke-RtkCapture @(
            'gh', 'issue', 'list', '--state', 'all', '--limit', '100',
            '--json', 'number,title,state,url', '--repo', 'tsuyoshi-otake/sakura-input'
        ) | ConvertFrom-Json)
        $result.issues = @($issues | Where-Object { $_.number -in 1..6 } | Sort-Object number)
        foreach ($number in 1..6) {
            $matches = @($issues | Where-Object { $_.number -eq $number })
            if ($matches.Count -ne 1 -or [string]$matches[0].state -cne 'CLOSED') {
                $result.reasons.Add("tracking Issue #$number is not uniquely present and closed")
            }
        }
    }
    catch { $result.reasons.Add("tracking Issue readback failed: $($_.Exception.Message)") }

    if ($result.main_sha) {
        foreach ($workflow in @('ci.yml', 'installer.yml', 'release.yml')) {
            try {
                $runs = @(Invoke-RtkCapture @(
                    'gh', 'run', 'list', '--workflow', $workflow, '--branch', 'main', '--limit', '1',
                    '--json', 'databaseId,headSha,status,conclusion,url,workflowName',
                    '--repo', 'tsuyoshi-otake/sakura-input'
                ) | ConvertFrom-Json)
                if ($runs.Count -ne 1) { throw 'no unique latest main run' }
                $run = $runs[0]
                $result.workflows[$workflow] = $run
                if ([string]$run.headSha -cne $result.main_sha -or [string]$run.status -cne 'completed' -or
                    [string]$run.conclusion -cne 'success') {
                    $result.reasons.Add("$workflow is not successful at the final main SHA")
                }
            }
            catch { $result.reasons.Add("$workflow readback failed: $($_.Exception.Message)") }
        }
    }

    $result.passed = $result.reasons.Count -eq 0
    return $result
}

$initialProcessClean = $true
try { Confirm-ProcessClean }
catch {
    $initialProcessClean = $false
    $localPassed = $false
    $localSteps.Add([ordered]@{
        name = 'initial process state'
        seconds = 0
        exit_code = 1
        processes_clean = $false
        passed = $false
        error = $_.Exception.Message
    })
}

if ($initialProcessClean) {
    $previousRevocation = [Environment]::GetEnvironmentVariable('CARGO_HTTP_CHECK_REVOKE', 'Process')
    [Environment]::SetEnvironmentVariable('CARGO_HTTP_CHECK_REVOKE', 'false', 'Process')
    Push-Location $repository
    try {
        Invoke-Gate -Name 'workspace formatting' -Arguments @('cargo', 'fmt', '--all', '--', '--check')
        Invoke-Gate -Name 'strict workspace lint' -Arguments @('cargo', 'clippy', '--workspace', '--all-targets', '--', '-D', 'warnings')
        Invoke-Gate -Name 'workspace tests' -Arguments @('cargo', 'test', '--workspace')
        Invoke-Gate -Name 'locked release workspace build' -Arguments @('cargo', 'build', '--workspace', '--release', '--locked')
        Invoke-Gate -Name 'dependency-policy negative self-test' -Arguments @(
            'proxy', 'pwsh', '-NoProfile', '-ExecutionPolicy', 'Bypass',
            '-File', (Join-Path $repository 'ci\dep-policy.ps1'), '-SelfTest'
        )
        Invoke-Gate -Name 'dependency-policy enforcement' -Arguments @(
            'proxy', 'pwsh', '-NoProfile', '-ExecutionPolicy', 'Bypass',
            '-File', (Join-Path $repository 'ci\dep-policy.ps1')
        )
    }
    finally {
        Pop-Location
        [Environment]::SetEnvironmentVariable('CARGO_HTTP_CHECK_REVOKE', $previousRevocation, 'Process')
    }
}

$sourceAuditWatch = [Diagnostics.Stopwatch]::StartNew()
$sourceAudit = Get-SourceAudit
$sourceAuditWatch.Stop()
$localSteps.Add([ordered]@{
    name = 'shipping source, stateful flow, workflow and memory audit'
    seconds = [Math]::Round($sourceAuditWatch.Elapsed.TotalSeconds, 3)
    exit_code = if ($sourceAudit.passed) { 0 } else { 1 }
    processes_clean = $true
    passed = [bool]$sourceAudit.passed
})
if (-not $sourceAudit.passed) { $localPassed = $false }

$phaseEvidence = @(foreach ($phase in 0..5) { Read-PhaseEvidence $phase })
$phases = @(foreach ($evidence in $phaseEvidence) { Grade-Phase $evidence })
$releaseState = Get-ReleaseState

$engineeringPassed = $localPassed -and @($phases | Where-Object { -not $_.engineering_passed }).Count -eq 0
$phaseStrictPassed = @($phases | Where-Object { -not $_.strict_passed }).Count -eq 0
$strictPassed = $engineeringPassed -and $phaseStrictPassed -and $releaseState.passed
$criteria = [ordered]@{
    C1_phase0 = [bool]$phases[0].strict_passed
    C2_phase1 = [bool]$phases[1].strict_passed
    C3_phase2 = [bool]$phases[2].strict_passed
    C4_phase3 = [bool]$phases[3].strict_passed
    C5_phase4 = [bool]$phases[4].strict_passed
    C6_phase5 = [bool]$phases[5].strict_passed
    C7_cross_phase = [bool]$localPassed
    C8_release_state = [bool]$releaseState.passed
}

$blockers = [Collections.Generic.List[string]]::new()
foreach ($phase in $phases) {
    foreach ($reason in @($phase.evidence_reasons)) { $blockers.Add("Phase $($phase.phase): $reason") }
    foreach ($reason in @($phase.external_reasons)) { $blockers.Add("Phase $($phase.phase): $reason") }
}
foreach ($reason in @($sourceAudit.reasons)) { $blockers.Add("Cross-phase: $reason") }
foreach ($reason in @($releaseState.reasons)) { $blockers.Add("Release state: $reason") }

$summary = [ordered]@{
    schema_version = 1
    objective = 'Complete every phase in PLAN.md (Phase 0 through Phase 5)'
    generated_at_utc = [DateTimeOffset]::UtcNow.ToString('O')
    elapsed_seconds = [Math]::Round(([DateTimeOffset]::UtcNow - $started).TotalSeconds, 3)
    maximum_evidence_age_hours = $MaximumEvidenceAge.TotalHours
    local_verification = [ordered]@{
        steps = @($localSteps)
        source_audit = $sourceAudit
        passed = $localPassed
    }
    phases = $phases
    release_state = $releaseState
    criteria = $criteria
    engineering_passed = $engineeringPassed
    strict_passed = $strictPassed
    blockers = @($blockers | Select-Object -Unique)
    engineering_only = [bool]$EngineeringOnly
    passed = if ($EngineeringOnly) { $engineeringPassed } else { $strictPassed }
}

$temporary = "$summaryPath.$PID.tmp"
[IO.File]::WriteAllText(
    $temporary,
    (($summary | ConvertTo-Json -Depth 24) + [Environment]::NewLine),
    [Text.UTF8Encoding]::new($false)
)
[IO.File]::Move($temporary, $summaryPath, $true)
$summary | ConvertTo-Json -Depth 24
if ($summary.passed) { exit 0 }
exit 1
