#Requires -Version 5.1
<#
.SYNOPSIS
    Publish completed GitHub Actions step timings to the job summary.

.DESCRIPTION
    The script reads the current workflow attempt's jobs from the GitHub REST
    API and selects one job by exact display name. API or permission failures
    are diagnostic-only: they are written to the log and step summary, then
    the script exits successfully so the build result remains authoritative.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$JobName,

    [switch]$SelfTest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Add-SummaryText {
    param(
        [Parameter(Mandatory = $true)][string]$Text
    )

    $summaryPath = [string]$env:GITHUB_STEP_SUMMARY
    if ([string]::IsNullOrWhiteSpace($summaryPath)) {
        Write-Warning 'GITHUB_STEP_SUMMARY is not set; timing summary was not persisted.'
        return
    }
    try {
        [IO.File]::AppendAllText(
            $summaryPath,
            $Text + [Environment]::NewLine,
            [Text.UTF8Encoding]::new($false)
        )
    } catch {
        Write-Warning "Unable to write GITHUB_STEP_SUMMARY: $($_.Exception.Message)"
    }
}

function Select-ExactJob {
    param(
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][object[]]$Jobs,
        [Parameter(Mandatory = $true)][string]$Name
    )

    $matches = @($Jobs | Where-Object { [string]$_.name -ceq $Name })
    if ($matches.Count -ne 1) {
        throw "expected exactly one job named '$Name', found $($matches.Count)"
    }
    return $matches[0]
}

function ConvertTo-TimingSummary {
    param(
        [Parameter(Mandatory = $true)][object]$Job
    )

    $lines = [Collections.Generic.List[string]]::new()
    $null = $lines.Add("## Step timings ($([string]$Job.name))")
    $null = $lines.Add('')
    $null = $lines.Add('| # | Step | Conclusion | Seconds |')
    $null = $lines.Add('| ---: | --- | --- | ---: |')

    foreach ($step in @($Job.steps)) {
        # The current timing step is still running when the endpoint is read;
        # only completed steps are stable enough to report.
        if ([string]::IsNullOrWhiteSpace([string]$step.started_at) -or
            [string]::IsNullOrWhiteSpace([string]$step.completed_at)) {
            continue
        }
        if ([string]$step.name -in @('Set up job', 'Complete job')) {
            continue
        }

        try {
            $started = [DateTimeOffset]::Parse([string]$step.started_at)
            $completed = [DateTimeOffset]::Parse([string]$step.completed_at)
        } catch {
            Write-Warning "Skipping step '$([string]$step.name)' with invalid timing timestamps."
            continue
        }
        $seconds = ($completed - $started).TotalSeconds
        $name = ([string]$step.name).Replace('|', '\|').Replace("`r", ' ').Replace("`n", ' ')
        $conclusion = [string]$step.conclusion
        $null = $lines.Add(('| {0} | {1} | {2} | {3:N1} |' -f $step.number, $name, $conclusion, $seconds))
    }

    return ($lines -join [Environment]::NewLine)
}

function Invoke-TimingSummary {
    param(
        [Parameter(Mandatory = $true)][string]$Name
    )

    try {
        if ([string]::IsNullOrWhiteSpace([string]$env:GITHUB_TOKEN)) {
            throw 'GITHUB_TOKEN is not available'
        }
        $headers = @{
            Accept = 'application/vnd.github+json'
            Authorization = "Bearer $env:GITHUB_TOKEN"
            'X-GitHub-Api-Version' = '2022-11-28'
        }
        $uri = "$env:GITHUB_API_URL/repos/$env:GITHUB_REPOSITORY/actions/runs/$env:GITHUB_RUN_ID/attempts/$env:GITHUB_RUN_ATTEMPT/jobs?per_page=100"
        $response = Invoke-RestMethod -Method Get -Uri $uri -Headers $headers
        $job = Select-ExactJob -Jobs @($response.jobs) -Name $Name
        $text = ConvertTo-TimingSummary -Job $job
        Add-SummaryText -Text $text
        Write-Host $text
    } catch {
        $message = "Step timing summary unavailable: $($_.Exception.Message)"
        Write-Warning $message
        Add-SummaryText -Text ("## Step timings`n`n$message")
    }
}

function Invoke-SelfTest {
    $job = [pscustomobject]@{
        name = 'Timing self-test'
        steps = @(
            [pscustomobject]@{
                number = 1
                name = 'Set up job'
                conclusion = 'success'
                started_at = '2026-08-24T00:00:00Z'
                completed_at = '2026-08-24T00:00:01Z'
            }
            [pscustomobject]@{
                number = 2
                name = 'Run | test'
                conclusion = 'success'
                started_at = '2026-08-24T00:00:01Z'
                completed_at = '2026-08-24T00:00:03Z'
            }
            [pscustomobject]@{
                number = 3
                name = 'Still running'
                conclusion = $null
                started_at = '2026-08-24T00:00:03Z'
                completed_at = $null
            }
        )
    }

    $selected = Select-ExactJob -Jobs @($job, [pscustomobject]@{ name = 'Timing self-test-similar' }) -Name 'Timing self-test'
    if ($selected.name -cne 'Timing self-test') { throw 'exact job selection self-test failed' }
    $text = ConvertTo-TimingSummary -Job $job
    if (-not $text.Contains('Run \| test')) { throw 'pipe escaping self-test failed' }
    if ($text -match 'Still running') { throw 'in-progress step self-test failed' }
    if (-not $text.Contains('| 2 | Run \| test | success |')) {
        throw 'duration formatting self-test failed'
    }
    Write-Host 'step timing self-test passed.' -ForegroundColor Green
    return 0
}

if ($SelfTest) {
    exit (Invoke-SelfTest)
}

Invoke-TimingSummary -Name $JobName
exit 0
