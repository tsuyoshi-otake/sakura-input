[CmdletBinding()]
param(
    [string]$ReportDirectory = (Join-Path $PSScriptRoot '..\artifacts\phase0'),

    [switch]$EngineeringOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repository = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$reportRoot = [IO.Path]::GetFullPath($ReportDirectory)
$summaryPath = Join-Path $reportRoot 'phase0-summary.json'
$processCheck = Join-Path $repository 'ci\check-process-clean.ps1'
$steps = [Collections.Generic.List[object]]::new()
$engineeringPassed = $true
$started = [DateTime]::UtcNow
[IO.Directory]::CreateDirectory($reportRoot) | Out-Null

function Confirm-ProcessClean {
    & rtk proxy pwsh -NoProfile -ExecutionPolicy Bypass -File $processCheck -RepositoryRoot $repository
    if ($LASTEXITCODE -eq 0) { return }
    Write-Warning 'A Phase 0 test left a repository-scoped process; terminating parents first.'
    & rtk proxy pwsh -NoProfile -ExecutionPolicy Bypass -File $processCheck -RepositoryRoot $repository -Terminate
    if ($LASTEXITCODE -ne 0) { throw 'test processes survived the bounded cleanup attempt' }
    & rtk proxy pwsh -NoProfile -ExecutionPolicy Bypass -File $processCheck -RepositoryRoot $repository
    if ($LASTEXITCODE -ne 0) { throw 'process re-list was not clean after cleanup' }
    throw 'the preceding test leaked a process; cleanup succeeded but the gate fails'
}

function Invoke-Gate {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][string[]]$Arguments,
        [switch]$CheckProcesses
    )

    Write-Host "==> $Name"
    $watch = [Diagnostics.Stopwatch]::StartNew()
    & rtk @Arguments
    $exitCode = $LASTEXITCODE
    $watch.Stop()
    if ($CheckProcesses) { Confirm-ProcessClean }
    $steps.Add([ordered]@{
        name = $Name
        seconds = [Math]::Round($watch.Elapsed.TotalSeconds, 3)
        exit_code = $exitCode
        passed = $exitCode -eq 0
    })
    if ($exitCode -ne 0) { throw "$Name failed with exit code $exitCode" }
}

function Invoke-Assertion {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][scriptblock]$Check
    )

    Write-Host "==> $Name"
    $watch = [Diagnostics.Stopwatch]::StartNew()
    try {
        & $Check
        $steps.Add([ordered]@{
            name = $Name
            seconds = [Math]::Round($watch.Elapsed.TotalSeconds, 3)
            exit_code = 0
            passed = $true
        })
    }
    catch {
        $steps.Add([ordered]@{
            name = $Name
            seconds = [Math]::Round($watch.Elapsed.TotalSeconds, 3)
            exit_code = 1
            passed = $false
            error = $_.Exception.Message
        })
        throw
    }
    finally { $watch.Stop() }
}

function Invoke-RtkCapture {
    param([Parameter(Mandatory)][string[]]$Arguments)

    $output = @(& rtk @Arguments)
    if ($LASTEXITCODE -ne 0) {
        throw "rtk $($Arguments -join ' ') failed with exit code $LASTEXITCODE"
    }
    return $output -join "`n"
}

function Test-GitHubState {
    $result = [ordered]@{
        repository = $null
        visibility = $null
        main_sha = $null
        issues = @()
        workflows = [ordered]@{}
        reasons = [Collections.Generic.List[string]]::new()
        passed = $false
    }
    if ($null -eq (Get-Command rtk -ErrorAction SilentlyContinue)) {
        $result.reasons.Add('rtk is unavailable for required GitHub CLI readback')
        return $result
    }

    try {
        $repo = Invoke-RtkCapture @('gh', 'repo', 'view', '--json', 'nameWithOwner,visibility,defaultBranchRef') | ConvertFrom-Json
        $result.repository = [string]$repo.nameWithOwner
        $result.visibility = [string]$repo.visibility
        if ($result.repository -cne 'tsuyoshi-otake/sakura-input') {
            $result.reasons.Add("GitHub repository identity is '$($result.repository)'")
        }
        if ($result.visibility -cne 'PRIVATE') {
            $result.reasons.Add("repository visibility is '$($result.visibility)', not PRIVATE")
        }
        if ([string]$repo.defaultBranchRef.name -cne 'main') {
            $result.reasons.Add('GitHub default branch is not main')
        }
    }
    catch { $result.reasons.Add("repository readback failed: $($_.Exception.Message)") }

    try {
        $issues = @(Invoke-RtkCapture @(
            'gh', 'issue', 'list', '--state', 'all', '--limit', '100',
            '--json', 'number,title,state,url', '--repo', 'tsuyoshi-otake/sakura-input'
        ) | ConvertFrom-Json)
        $result.issues = @($issues | Sort-Object number)
        foreach ($number in 1..6) {
            if (@($issues | Where-Object { $_.number -eq $number }).Count -ne 1) {
                $result.reasons.Add("tracking Issue #$number is missing or duplicated")
            }
        }
    }
    catch { $result.reasons.Add("tracking Issue readback failed: $($_.Exception.Message)") }

    try {
        $mainSha = (Invoke-RtkCapture @(
            'gh', 'api', 'repos/tsuyoshi-otake/sakura-input/commits/main', '--jq', '.sha'
        )).Trim()
        if ($mainSha -notmatch '^[0-9a-f]{40}$') { throw "main SHA is malformed: '$mainSha'" }
        $result.main_sha = $mainSha
        foreach ($workflow in @('ci.yml', 'installer.yml')) {
            $runs = @(Invoke-RtkCapture @(
                'gh', 'run', 'list', '--workflow', $workflow, '--branch', 'main', '--limit', '1',
                '--json', 'databaseId,headSha,status,conclusion,url,workflowName',
                '--repo', 'tsuyoshi-otake/sakura-input'
            ) | ConvertFrom-Json)
            if ($runs.Count -ne 1) {
                $result.reasons.Add("$workflow has no unique latest main run")
                continue
            }
            $run = $runs[0]
            $result.workflows[$workflow] = $run
            if ([string]$run.headSha -cne $mainSha -or [string]$run.status -cne 'completed' -or
                [string]$run.conclusion -cne 'success') {
                $result.reasons.Add(
                    "$workflow latest main run is head=$($run.headSha) status=$($run.status) conclusion=$($run.conclusion); expected successful $mainSha"
                )
            }
        }
    }
    catch { $result.reasons.Add("main/workflow readback failed: $($_.Exception.Message)") }

    $result.passed = $result.reasons.Count -eq 0
    return $result
}

Push-Location $repository
try {
    & rtk proxy pwsh -NoProfile -ExecutionPolicy Bypass -File $processCheck -RepositoryRoot $repository
    if ($LASTEXITCODE -ne 0) { throw 'Phase 0 verifier requires a clean initial process state' }
    Invoke-Gate -Name 'workspace formatting' -Arguments @('cargo', 'fmt', '--all', '--', '--check')
    Invoke-Gate -Name 'strict workspace lint' -Arguments @('cargo', 'clippy', '--workspace', '--all-targets', '--', '-D', 'warnings')
    Invoke-Gate -Name 'workspace tests' -Arguments @('cargo', 'test', '--workspace') -CheckProcesses
    Invoke-Gate -Name 'locked release workspace build' -Arguments @('cargo', 'build', '--workspace', '--release', '--locked')
    Invoke-Gate -Name 'dependency-policy negative self-test' -Arguments @(
        'proxy', 'pwsh', '-NoProfile', '-ExecutionPolicy', 'Bypass',
        '-File', (Join-Path $repository 'ci\dep-policy.ps1'), '-SelfTest'
    ) -CheckProcesses
    Invoke-Gate -Name 'dependency-policy enforcement' -Arguments @(
        'proxy', 'pwsh', '-NoProfile', '-ExecutionPolicy', 'Bypass',
        '-File', (Join-Path $repository 'ci\dep-policy.ps1')
    ) -CheckProcesses
    Invoke-Assertion -Name 'workflow YAML syntax' -Check {
        if ($null -eq (Get-Command ConvertFrom-Yaml -ErrorAction SilentlyContinue)) {
            throw 'ConvertFrom-Yaml is unavailable'
        }
        foreach ($relative in @(
            '.github\workflows\ci.yml',
            '.github\workflows\installer.yml',
            '.github\workflows\fuzz-campaign.yml',
            '.github\workflows\release.yml'
        )) {
            $path = Join-Path $repository $relative
            if (-not [IO.File]::Exists($path)) { throw "workflow is missing: $relative" }
            $null = [IO.File]::ReadAllText($path) | ConvertFrom-Yaml
        }
    }
}
catch {
    $engineeringPassed = $false
    $steps.Add([ordered]@{
        name = 'engineering terminal'
        seconds = 0
        exit_code = 1
        passed = $false
        error = $_.Exception.Message
    })
}
finally { Pop-Location }

$githubState = Test-GitHubState
$strictPassed = $engineeringPassed -and $githubState.passed
$summary = [ordered]@{
    schema_version = 1
    phase = 0
    generated_at_utc = [DateTime]::UtcNow.ToString('O')
    elapsed_seconds = [Math]::Round(([DateTime]::UtcNow - $started).TotalSeconds, 3)
    engineering = [ordered]@{ passed = $engineeringPassed; steps = @($steps) }
    github = $githubState
    engineering_only = [bool]$EngineeringOnly
    passed = if ($EngineeringOnly) { $engineeringPassed } else { $strictPassed }
}
$temporary = "$summaryPath.$PID.tmp"
[IO.File]::WriteAllText(
    $temporary,
    (($summary | ConvertTo-Json -Depth 12) + [Environment]::NewLine),
    [Text.UTF8Encoding]::new($false)
)
[IO.File]::Move($temporary, $summaryPath, $true)
$summary | ConvertTo-Json -Depth 12
if ($summary.passed) { exit 0 }
exit 1
