#Requires -Version 5.1
<#
.SYNOPSIS
    Fail-closed static policy gate for the release workflow.

.DESCRIPTION
    The release workflow is a signing boundary. This gate keeps the build job
    free of release secrets, requires every third-party action to use a
    reviewed full commit SHA, and makes the artifact/provenance and signing
    cleanup ordering explicit. It intentionally uses a small line-oriented
    parser so the policy has no YAML module dependency on a runner.

.PARAMETER WorkflowPath
    Path to the workflow to inspect. Defaults to .github/workflows/release.yml.

.PARAMETER SelfTest
    Exercise the action-ref parser with both accepted and rejected fixtures.

.EXAMPLE
    pwsh ci/release-workflow-policy.ps1 -SelfTest
    pwsh ci/release-workflow-policy.ps1
#>
[CmdletBinding()]
param(
    [string]$WorkflowPath,
    [switch]$SelfTest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($WorkflowPath)) {
    $WorkflowPath = Join-Path $repoRoot '.github/workflows/release.yml'
}

# These are deliberately old enough to satisfy the seven-day quarantine. Each
# pin is checked against this reviewed allowlist instead of merely checking
# that it happens to be a 40-character string.
$ReviewedActionPins = [ordered]@{
    'actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683' = 'v4.2.2'
    'actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02' = 'v4.6.2'
    'actions/download-artifact@d3f86a106a0bac45b974a628896c90dbdf5c8093' = 'v4.3.0'
    'Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6' = 'v2.9.2'
}

function Get-ActionUseRecords {
    param([Parameter(Mandatory)][string]$Text)

    $lines = @($Text -split "`r?`n")
    $records = [Collections.Generic.List[object]]::new()
    foreach ($line in $lines) {
        if ($line -notmatch '^\s*(?:-\s+)?uses:\s*') { continue }
        if ($line -notmatch '^\s*(?:-\s+)?uses:\s*(?<action>[^@\s#]+)@(?<sha>[0-9a-fA-F]{40})(?:\s+(?<comment>#.*))?\s*$') {
            throw "mutable or malformed action reference: $line"
        }
        $action = [string]$Matches['action']
        $sha = [string]$Matches['sha']
        $comment = [string]$Matches['comment']
        if ($comment -notmatch '#\s*v\d+(?:\.\d+){1,2}\b') {
            throw "action reference has no version comment: $line"
        }
        $key = "$action@$($sha.ToLowerInvariant())"
        if (-not $ReviewedActionPins.Contains($key)) {
            throw "action reference is not in the reviewed pin allowlist: $key"
        }
        if ($comment -notmatch 'reviewed\s+\d{4}-\d{2}-\d{2}') {
            throw "action reference has no review date: $line"
        }
        if ($comment -notmatch 'upstream\s+\d{4}-\d{2}-\d{2}') {
            throw "action reference has no upstream commit date: $line"
        }
        if ($comment -notmatch 'quarantine\s+satisfied') {
            throw "action reference has no release-age assertion: $line"
        }
        $null = $records.Add([pscustomobject]@{
                action = $action
                sha = $sha.ToLowerInvariant()
                key = $key
                comment = $comment
            })
    }
    if ($records.Count -eq 0) { throw 'release workflow contains no action references' }
    return $records.ToArray()
}

function Get-JobBlock {
    param(
        [Parameter(Mandatory)][string]$Text,
        [Parameter(Mandatory)][string]$JobName
    )

    $escaped = [regex]::Escape($JobName)
    $match = [regex]::Match(
        $Text,
        "(?ms)^  ${escaped}:\s*(?<body>.*?)(?=^  [A-Za-z0-9_-]+:\s*$|\z)"
    )
    if (-not $match.Success) { throw "job '$JobName' is missing" }
    return $match.Groups['body'].Value
}

function Assert-Contains {
    param(
        [Parameter(Mandatory)][string]$Text,
        [Parameter(Mandatory)][string]$Needle,
        [Parameter(Mandatory)][string]$Description
    )
    if ($Text.IndexOf($Needle, [StringComparison]::Ordinal) -lt 0) {
        throw "$Description is missing: $Needle"
    }
}

function Assert-WorkflowPolicy {
    param(
        [Parameter(Mandatory)][string]$Text,
        [DateTime]$AsOfDate = ([DateTime]::UtcNow.Date)
    )

    if ($Text -notmatch '(?m)^name:\s*Release candidate\s*$') {
        throw 'workflow name is not the release workflow'
    }
    $uses = Get-ActionUseRecords -Text $Text
    foreach ($record in $uses) {
        $expectedVersion = $ReviewedActionPins[$record.key]
        if ($record.comment -notmatch [regex]::Escape($expectedVersion)) {
            throw "version comment for $($record.key) does not name $expectedVersion"
        }
        $dateMatch = [regex]::Match($record.comment, 'upstream\s+(?<date>\d{4}-\d{2}-\d{2})')
        $upstreamDate = [DateTime]::ParseExact(
            $dateMatch.Groups['date'].Value,
            'yyyy-MM-dd',
            [Globalization.CultureInfo]::InvariantCulture,
            [Globalization.DateTimeStyles]::AssumeUniversal
        ).Date
        if ($upstreamDate -gt $AsOfDate.AddDays(-7)) {
            throw "action $($record.key) is newer than the seven-day quarantine"
        }
    }

    $build = Get-JobBlock -Text $Text -JobName 'build-release'
    $sign = Get-JobBlock -Text $Text -JobName 'sign-and-package'

    if ($build -match '(?m)^\s+environment\s*:') {
        throw 'secretless build job must not select a protected environment'
    }
    if ($build -match '(?i)secrets\.') {
        throw 'secretless build job must not reference signing secrets'
    }
    Assert-Contains -Text $build -Needle 'build-artifact-digest' -Description 'build job digest output'
    Assert-Contains -Text $build -Needle 'build-provenance.json' -Description 'build job provenance'
    Assert-Contains -Text $build -Needle 'artifact-digest' -Description 'upload artifact digest output'
    Assert-Contains -Text $build -Needle 'actions/upload-artifact@' -Description 'secretless build artifact upload'
    Assert-Contains -Text $build -Needle 'git diff --name-only' -Description 'clean source checkout gate'

    Assert-Contains -Text $sign -Needle 'needs: build-release' -Description 'signing job dependency'
    Assert-Contains -Text $sign -Needle 'environment: release' -Description 'protected signing environment'
    Assert-Contains -Text $sign -Needle 'actions: read' -Description 'artifact digest read permission'
    Assert-Contains -Text $sign -Needle 'actions/download-artifact@' -Description 'build artifact download'
    Assert-Contains -Text $sign -Needle 'EXPECTED_BUILD_ARTIFACT_DIGEST' -Description 'expected artifact digest'
    Assert-Contains -Text $sign -Needle 'Invoke-RestMethod' -Description 'remote artifact digest readback'
    Assert-Contains -Text $sign -Needle '.digest' -Description 'remote artifact digest comparison'
    Assert-Contains -Text $sign -Needle 'build-provenance.json' -Description 'downloaded artifact provenance'
    Assert-Contains -Text $sign -Needle 'source_files' -Description 'verified source file provenance'
    Assert-Contains -Text $sign -Needle 'scripts/sign-release.ps1' -Description 'verified signing script'
    Assert-Contains -Text $sign -Needle 'installer/setup.iss' -Description 'verified installer manifest'
    Assert-Contains -Text $sign -Needle 'Get-FileHash' -Description 'provenance file hash verification'
    Assert-Contains -Text $sign -Needle 'source_commit' -Description 'source commit binding'
    Assert-Contains -Text $sign -Needle 'unsigned-owner-approved' -Description 'owner-approved unsigned metadata'
    Assert-Contains -Text $sign -Needle 'authenticode-signed' -Description 'signed metadata'
    Assert-Contains -Text $sign -Needle 'presentCount -ne $values.Count' -Description 'partial signing secret fail-closed check'

    $cleanupNeedle = 'name: Remove signing material before external artifact upload'
    $uploadNeedle = 'name: Upload release candidate'
    $cleanupIndex = $Text.IndexOf($cleanupNeedle, [StringComparison]::Ordinal)
    $uploadIndex = $Text.IndexOf($uploadNeedle, [StringComparison]::Ordinal)
    if ($cleanupIndex -lt 0 -or $uploadIndex -lt 0 -or $cleanupIndex -ge $uploadIndex) {
        throw 'PFX cleanup must precede the final external artifact upload'
    }
    $cleanup = $Text.Substring($cleanupIndex, $uploadIndex - $cleanupIndex)
    Assert-Contains -Text $cleanup -Needle 'if: always()' -Description 'always-run PFX cleanup'
    Assert-Contains -Text $cleanup -Needle '[IO.File]::Delete' -Description 'PFX deletion'
    Assert-Contains -Text $cleanup -Needle 'signing material remained after cleanup' -Description 'PFX cleanup readback'

    Write-Host ("release workflow policy passed: {0} reviewed action references" -f $uses.Count) -ForegroundColor Green
}

function Invoke-SelfTest {
    $good = @'
- uses: actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683 # v4.2.2 (reviewed 2026-08-24; upstream 2024-10-23; quarantine satisfied)
'@
    $records = Get-ActionUseRecords -Text $good
    if ($records.Count -ne 1 -or $records[0].key -ne 'actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683') {
        throw 'self-test failed to accept the reviewed full-SHA action reference'
    }

    $bad = '- uses: actions/checkout@v4'
    $rejected = $false
    try { Get-ActionUseRecords -Text $bad | Out-Null } catch { $rejected = $true }
    if (-not $rejected) { throw 'self-test failed to reject a mutable action reference' }

    $badComment = '- uses: actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683 # v4.2.2'
    $rejected = $false
    try { Get-ActionUseRecords -Text $badComment | Out-Null } catch { $rejected = $true }
    if (-not $rejected) { throw 'self-test failed to reject an unreviewed action comment' }

    Write-Host 'release workflow policy self-test passed.' -ForegroundColor Green
    return 0
}

if ($SelfTest) {
    exit (Invoke-SelfTest)
}

if (-not [IO.File]::Exists($WorkflowPath)) {
    throw "release workflow is missing: $WorkflowPath"
}
Assert-WorkflowPolicy -Text ([IO.File]::ReadAllText($WorkflowPath))
exit 0
