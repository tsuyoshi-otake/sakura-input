[CmdletBinding()]
param(
    [string]$Dictionary = (Join-Path $PSScriptRoot '..\artifacts\release\system.dic'),
    [string]$ReportDirectory = (Join-Path $PSScriptRoot '..\artifacts\phase5'),
    [string]$CompatibilityRecord = (Join-Path $PSScriptRoot '..\artifacts\phase5\compat-matrix.json'),
    [string]$StagedUpdateRecord = (Join-Path $PSScriptRoot '..\artifacts\phase5\staged-update.json'),
    [string]$FuzzStateDirectory = (Join-Path $PSScriptRoot '..\.codex\goal-loop\all-phases\phase5\fuzz'),
    [string]$ReleaseBundle = (Join-Path $PSScriptRoot '..\release-bundle'),
    [string]$ExpectedSigningSubject = '',
    [switch]$EngineeringOnly,
    [switch]$SkipEngineeringTests
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repository = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$dictionaryPath = [IO.Path]::GetFullPath($Dictionary)
$reportRoot = [IO.Path]::GetFullPath($ReportDirectory)
$compatibilityPath = [IO.Path]::GetFullPath($CompatibilityRecord)
$stagedUpdatePath = [IO.Path]::GetFullPath($StagedUpdateRecord)
$fuzzRoot = [IO.Path]::GetFullPath($FuzzStateDirectory)
$bundleRoot = [IO.Path]::GetFullPath($ReleaseBundle)
$summaryPath = Join-Path $reportRoot 'phase5-summary.json'
$processCheck = Join-Path $repository 'ci\check-process-clean.ps1'
$steps = [Collections.Generic.List[object]]::new()
$externalReasons = [Collections.Generic.List[string]]::new()
$started = [DateTime]::UtcNow
$engineeringPassed = $true
[IO.Directory]::CreateDirectory($reportRoot) | Out-Null

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

function Confirm-ProcessClean {
    & rtk proxy pwsh -NoProfile -ExecutionPolicy Bypass -File $processCheck -RepositoryRoot $repository
    if ($LASTEXITCODE -eq 0) { return }
    Write-Warning 'A test left a repository-scoped process; terminating parents first.'
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

function Resolve-Evidence {
    param(
        [Parameter(Mandatory)][string]$RecordPath,
        [AllowEmptyString()][string]$RelativePath,
        [AllowEmptyString()][string]$ExpectedHash
    )

    if ([string]::IsNullOrWhiteSpace($RelativePath) -or [IO.Path]::IsPathRooted($RelativePath)) { return $false }
    $recordDirectory = [IO.Path]::GetFullPath((Split-Path -Parent $RecordPath))
    $candidate = [IO.Path]::GetFullPath((Join-Path $recordDirectory $RelativePath))
    $prefix = $recordDirectory.TrimEnd([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
    if (-not $candidate.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) { return $false }
    if (-not [IO.File]::Exists($candidate) -or $ExpectedHash -notmatch '^[0-9a-fA-F]{64}$') { return $false }
    return (Get-Sha256 $candidate) -ceq $ExpectedHash.ToLowerInvariant()
}

function Parse-Utc {
    param([Parameter(Mandatory)][string]$Value)
    return [DateTimeOffset]::Parse(
        $Value,
        [Globalization.CultureInfo]::InvariantCulture,
        [Globalization.DateTimeStyles]::RoundtripKind
    )
}

function Test-CompatibilityMatrix {
    $required = @(
        'word-horizontal', 'word-vertical', 'excel', 'electron-vscode',
        'electron-secondary', 'game', 'rdp', 'uwp', 'windows-terminal',
        'conhost', 'touch-keyboard', 'mixed-dpi'
    )
    $result = [ordered]@{ path = $compatibilityPath; rows = [ordered]@{}; reasons = [Collections.Generic.List[string]]::new(); passed = $false }
    if (-not [IO.File]::Exists($compatibilityPath)) {
        $result.reasons.Add('compatibility matrix record is missing')
        return $result
    }
    try {
        $record = [IO.File]::ReadAllText($compatibilityPath, [Text.Encoding]::UTF8) | ConvertFrom-Json
        if ($record.schema_version -ne 1 -or $record.phase -ne 5) { $result.reasons.Add('compatibility record schema/phase is invalid') }
        if ([string]::IsNullOrWhiteSpace([string]$record.responsible_human)) { $result.reasons.Add('compatibility record has no responsible human') }
        $startedAt = Parse-Utc ([string]$record.started_at_utc)
        $completedAt = Parse-Utc ([string]$record.completed_at_utc)
        if ($completedAt -lt $startedAt -or $completedAt -gt [DateTimeOffset]::UtcNow.AddMinutes(5)) {
            $result.reasons.Add('compatibility record timestamps are impossible or in the future')
        }
        $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
        foreach ($row in @($record.rows)) {
            $id = [string]$row.id
            if ($id -notin $required) { $result.reasons.Add("unexpected compatibility row '$id'"); continue }
            if (-not $seen.Add($id)) { $result.reasons.Add("duplicate compatibility row '$id'"); continue }
            $status = [string]$row.status
            $workaround = [string]$row.workaround
            $evidence = Resolve-Evidence -RecordPath $compatibilityPath -RelativePath ([string]$row.evidence.path) -ExpectedHash ([string]$row.evidence.sha256)
            $rowPassed = $status -in @('green', 'workaround') -and
                ($status -ne 'workaround' -or -not [string]::IsNullOrWhiteSpace($workaround)) -and
                [int]$row.open_p0_p1 -eq 0 -and
                -not [string]::IsNullOrWhiteSpace([string]$row.host) -and
                -not [string]::IsNullOrWhiteSpace([string]$row.host_version) -and
                -not [string]::IsNullOrWhiteSpace([string]$row.windows_build) -and $evidence
            if (-not $rowPassed) { $result.reasons.Add("compatibility row '$id' is incomplete, red, or lacks hashed evidence") }
            $result.rows[$id] = [ordered]@{ status = $status; evidence_verified = $evidence; passed = $rowPassed }
        }
        foreach ($id in $required) {
            if (-not $seen.Contains($id)) { $result.reasons.Add("required compatibility row '$id' is missing") }
        }
    }
    catch { $result.reasons.Add("compatibility record could not be graded: $($_.Exception.Message)") }
    $result.passed = $result.reasons.Count -eq 0
    return $result
}

function Test-StagedUpdate {
    $result = [ordered]@{ path = $stagedUpdatePath; reasons = [Collections.Generic.List[string]]::new(); passed = $false }
    if (-not [IO.File]::Exists($stagedUpdatePath)) {
        $result.reasons.Add('staged update record is missing')
        return $result
    }
    try {
        $record = [IO.File]::ReadAllText($stagedUpdatePath, [Text.Encoding]::UTF8) | ConvertFrom-Json
        if ($record.schema_version -ne 1 -or $record.phase -ne 5) { $result.reasons.Add('staged update schema/phase is invalid') }
        if ([string]::IsNullOrWhiteSpace([string]$record.responsible_human) -or [string]::IsNullOrWhiteSpace([string]$record.machine)) {
            $result.reasons.Add('staged update needs a responsible human and machine label')
        }
        $observed = Parse-Utc ([string]$record.observed_at_utc)
        if ($observed -gt [DateTimeOffset]::UtcNow.AddMinutes(5)) { $result.reasons.Add('staged update observation is in the future') }
        if ([string]$record.target_version -cne '1.0.0' -or [string]$record.source_version -ceq [string]$record.target_version) {
            $result.reasons.Add('staged update must move an older distinct version to 1.0.0')
        }
        foreach ($field in @('source_engine_sha256', 'target_engine_sha256', 'source_dictionary_sha256', 'target_dictionary_sha256')) {
            if ([string]$record.$field -notmatch '^[0-9a-f]{64}$') { $result.reasons.Add("$field is not a lowercase SHA-256") }
        }
        if ([string]$record.source_engine_sha256 -ceq [string]$record.target_engine_sha256 -or
            [string]$record.source_dictionary_sha256 -ceq [string]$record.target_dictionary_sha256) {
            $result.reasons.Add('engine and dictionary evidence must prove a real replacement')
        }
        $exitCode = [int]$record.installer_exit_code
        if ($exitCode -ne 0) { $result.reasons.Add('normal side-by-side update must finish with installer exit code 0') }
        if (-not [bool]$record.engine_copied_before_activation -or -not [bool]$record.dictionary_copied_before_activation -or
            -not [bool]$record.old_dll_new_engine_round_trip -or -not [bool]$record.typing_before_update -or
            -not [bool]$record.typing_after_activation -or -not [bool]$record.typing_after_new_host_restart) {
            $result.reasons.Add('staged update did not prove copy-before-activation and pre/post activation typing state')
        }
        if ([string]$record.dll_terminal -cne 'registered_side_by_side' -or [string]$record.active_dll_version -cne '1.0.0') {
            $result.reasons.Add('DLL side-by-side activation evidence is missing or has the wrong terminal')
        }
        if (-not (Resolve-Evidence -RecordPath $stagedUpdatePath -RelativePath ([string]$record.evidence.path) -ExpectedHash ([string]$record.evidence.sha256))) {
            $result.reasons.Add('staged update hashed evidence is missing or invalid')
        }
    }
    catch { $result.reasons.Add("staged update record could not be graded: $($_.Exception.Message)") }
    $result.passed = $result.reasons.Count -eq 0
    return $result
}

function Test-ReleaseBundle {
    $result = [ordered]@{ path = $bundleRoot; signatures_verified = $false; manifest_verified = $false; package_report_verified = $false; reasons = [Collections.Generic.List[string]]::new(); passed = $false }
    if ([string]::IsNullOrWhiteSpace($ExpectedSigningSubject)) {
        $result.reasons.Add('expected Authenticode subject is required for a release gate')
        return $result
    }
    $files = @(
        (Join-Path $bundleRoot 'sakura_setup.exe'),
        (Join-Path $bundleRoot 'payload\sakura_tsf.dll'),
        (Join-Path $bundleRoot 'payload\sakura_engine.exe'),
        (Join-Path $bundleRoot 'payload\sakura_renderer.exe'),
        (Join-Path $bundleRoot 'payload\sakura_regtool.exe'),
        (Join-Path $bundleRoot 'payload\sakura_logon.exe'),
        (Join-Path $bundleRoot 'payload\sakura_settings.exe'),
        (Join-Path $bundleRoot 'payload\sakura_settings_payload.exe')
    )
    foreach ($file in $files) {
        if (-not [IO.File]::Exists($file)) { $result.reasons.Add("signed release file is missing: $file") }
    }
    if ($result.reasons.Count -eq 0) {
        & rtk proxy pwsh -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'verify-release-signatures.ps1') -ExpectedSubject $ExpectedSigningSubject -Files $files
        $result.signatures_verified = $LASTEXITCODE -eq 0
        if (-not $result.signatures_verified) { $result.reasons.Add('one or more Authenticode signatures failed verification') }
    }
    $installer = Join-Path $bundleRoot 'sakura_setup.exe'
    $manifest = Join-Path $bundleRoot 'release-manifest.txt'
    if ([IO.File]::Exists($installer) -and [IO.File]::Exists($manifest)) {
        $expected = @(
            'schema=1',
            'version=1.0.0',
            'installer_url=https://github.com/tsuyoshi-otake/sakura-input/releases/download/v1.0.0/sakura_setup.exe',
            "sha256=$(Get-Sha256 $installer)",
            "size=$([IO.FileInfo]::new($installer).Length)"
        ) -join "`n"
        $result.manifest_verified = [IO.File]::ReadAllText($manifest, [Text.Encoding]::UTF8) -ceq "$expected`n"
        if (-not $result.manifest_verified) { $result.reasons.Add('release manifest does not exactly describe the signed installer') }
    }
    else { $result.reasons.Add('release manifest or installer is missing') }

    $packageReportPath = Join-Path $bundleRoot 'installer-build.report.json'
    $dictionaryReportPath = Join-Path $bundleRoot 'dictionary-build.report.json'
    if (-not [IO.File]::Exists($packageReportPath) -or -not [IO.File]::Exists($dictionaryReportPath)) {
        $result.reasons.Add('installer or dictionary provenance report is missing from the release bundle')
    }
    else {
        try {
            $packageReport = [IO.File]::ReadAllText($packageReportPath, [Text.Encoding]::UTF8) | ConvertFrom-Json
            $dictionaryReport = [IO.File]::ReadAllText($dictionaryReportPath, [Text.Encoding]::UTF8) | ConvertFrom-Json
            if ($packageReport.schema_version -ne 1 -or $packageReport.version -cne '1.0.0' -or
                $packageReport.compiler.warnings -ne 0 -or @($packageReport.payloads).Count -ne 14 -or
                [string]::IsNullOrWhiteSpace([string]$packageReport.build_id)) {
                throw 'installer build report schema, version, warning count, or payload count is invalid'
            }
            if ($dictionaryReport.schema_version -ne 1 -or $dictionaryReport.deterministic_repeat -ne $true -or
                $packageReport.dictionary_provenance_sha256 -cne (Get-Sha256 $dictionaryReportPath)) {
                throw 'dictionary provenance report is invalid or not linked from the installer build report'
            }

            $binaryNames = @(
                'sakura_tsf.dll', 'sakura_engine.exe', 'sakura_renderer.exe',
                'sakura_regtool.exe', 'sakura_logon.exe', 'sakura_settings.exe',
                'sakura_settings_payload.exe'
            )
            foreach ($name in $binaryNames) {
                $sourcePath = "target/x86_64-pc-windows-msvc/release/$name"
                $records = @($packageReport.payloads | Where-Object { $_.path -ceq $sourcePath })
                $bundledPath = Join-Path $bundleRoot "payload\$name"
                if ($records.Count -ne 1 -or $records[0].sha256 -cne (Get-Sha256 $bundledPath) -or
                    [long]$records[0].bytes -ne [IO.FileInfo]::new($bundledPath).Length) {
                    throw "bundled payload $name does not match the installer build input"
                }
            }
            $dictionaryRecords = @($packageReport.payloads | Where-Object { $_.path -ceq 'artifacts/release/system.dic' })
            if ($dictionaryRecords.Count -ne 1 -or
                $dictionaryRecords[0].sha256 -cne $dictionaryReport.artifacts.dictionary.sha256 -or
                [long]$dictionaryRecords[0].bytes -ne [long]$dictionaryReport.artifacts.dictionary.bytes) {
                throw 'installer dictionary payload does not match the reproducible dictionary artifact'
            }
            if ($packageReport.installer.sha256 -notmatch '^[0-9a-f]{64}$' -or [long]$packageReport.installer.bytes -le 0) {
                throw 'installer build report has no valid pre-signing installer record'
            }
            $result.package_report_verified = $true
        }
        catch { $result.reasons.Add("release package provenance failed verification: $($_.Exception.Message)") }
    }
    $result.passed = $result.reasons.Count -eq 0
    return $result
}

function Test-PublishedRelease {
    $result = [ordered]@{
        tag = 'v1.0.0'
        url = $null
        download_directory = $null
        downloaded_installer_sha256 = $null
        manifest_verified = $false
        signature_verified = $false
        local_candidate_match = $false
        reasons = [Collections.Generic.List[string]]::new()
        passed = $false
    }
    if ($null -eq (Get-Command rtk -ErrorAction SilentlyContinue)) {
        $result.reasons.Add('rtk is unavailable for GitHub release readback')
        return $result
    }
    try {
        $json = @(& rtk gh release view v1.0.0 --repo tsuyoshi-otake/sakura-input --json tagName,isDraft,isPrerelease,assets,url,publishedAt)
        if ($LASTEXITCODE -ne 0) { throw "rtk gh release view exited $LASTEXITCODE" }
        $release = $json -join "`n" | ConvertFrom-Json
        $result.url = [string]$release.url
        if ($release.tagName -cne 'v1.0.0' -or $release.isDraft -or $release.isPrerelease -or [string]::IsNullOrWhiteSpace([string]$release.publishedAt)) {
            $result.reasons.Add('GitHub release is not the published stable v1.0.0 terminal state')
        }
        $assets = @($release.assets)
        if ($assets.Count -ne 2 -or @($assets.name | Sort-Object) -join ',' -cne 'release-manifest.txt,sakura_setup.exe') {
            $result.reasons.Add('GitHub release does not contain exactly the updater assets')
        }

        if ($result.reasons.Count -eq 0) {
            if ([string]::IsNullOrWhiteSpace($ExpectedSigningSubject)) {
                $result.reasons.Add('expected Authenticode subject is required to verify the downloaded release')
            }
            else {
                $downloadRoot = Join-Path $reportRoot "published-v1.0.0-$PID"
                [IO.Directory]::CreateDirectory($downloadRoot) | Out-Null
                $result.download_directory = $downloadRoot
                & rtk gh release download v1.0.0 `
                    --repo tsuyoshi-otake/sakura-input `
                    --dir $downloadRoot
                if ($LASTEXITCODE -ne 0) { throw "rtk gh release download exited $LASTEXITCODE" }

                $installer = Join-Path $downloadRoot 'sakura_setup.exe'
                $manifest = Join-Path $downloadRoot 'release-manifest.txt'
                if (-not [IO.File]::Exists($installer) -or -not [IO.File]::Exists($manifest)) {
                    $result.reasons.Add('downloaded release is missing the installer or manifest')
                }
                else {
                    $result.downloaded_installer_sha256 = Get-Sha256 $installer
                    $expectedManifest = @(
                        'schema=1',
                        'version=1.0.0',
                        'installer_url=https://github.com/tsuyoshi-otake/sakura-input/releases/download/v1.0.0/sakura_setup.exe',
                        "sha256=$($result.downloaded_installer_sha256)",
                        "size=$([IO.FileInfo]::new($installer).Length)"
                    ) -join "`n"
                    $result.manifest_verified = [IO.File]::ReadAllText($manifest, [Text.Encoding]::UTF8) -ceq "$expectedManifest`n"
                    if (-not $result.manifest_verified) {
                        $result.reasons.Add('downloaded manifest does not exactly describe the downloaded installer')
                    }

                    $localInstaller = Join-Path $bundleRoot 'sakura_setup.exe'
                    $result.local_candidate_match = [IO.File]::Exists($localInstaller) -and
                        (Get-Sha256 $localInstaller) -ceq $result.downloaded_installer_sha256
                    if (-not $result.local_candidate_match) {
                        $result.reasons.Add('downloaded installer is not byte-identical to the locally verified release candidate')
                    }

                    & rtk proxy pwsh -NoProfile -ExecutionPolicy Bypass `
                        -File (Join-Path $PSScriptRoot 'verify-release-signatures.ps1') `
                        -ExpectedSubject $ExpectedSigningSubject `
                        -Files $installer
                    $result.signature_verified = $LASTEXITCODE -eq 0
                    if (-not $result.signature_verified) {
                        $result.reasons.Add('downloaded installer Authenticode verification failed')
                    }
                }
            }
        }
    }
    catch { $result.reasons.Add("published release could not be read back: $($_.Exception.Message)") }
    $result.passed = $result.reasons.Count -eq 0
    return $result
}

if (-not $SkipEngineeringTests) {
    Push-Location $repository
    try {
        Invoke-Gate -Name 'format' -Arguments @('cargo', 'fmt', '--all', '--', '--check')
        Invoke-Gate -Name 'strict clippy' -Arguments @('cargo', 'clippy', '--workspace', '--all-targets', '--', '-D', 'warnings')
        Invoke-Gate -Name 'workspace tests' -Arguments @('cargo', 'test', '--workspace') -CheckProcesses
        Invoke-Gate -Name 'release build' -Arguments @('cargo', 'build', '--workspace', '--release', '--locked')
        Invoke-Gate -Name 'installer package audit' -Arguments @(
            'proxy', 'pwsh', '-NoProfile', '-ExecutionPolicy', 'Bypass',
            '-File', (Join-Path $PSScriptRoot 'build-installer.ps1')
        ) -CheckProcesses
        if (-not [IO.File]::Exists($dictionaryPath)) { throw "release UIA dictionary is missing: $dictionaryPath" }
        $oldDictionary = $env:SAKURA_PHASE2_DICTIONARY
        $oldReport = $env:SAKURA_CANDIDATE_UIA_REPORT
        try {
            $env:SAKURA_PHASE2_DICTIONARY = $dictionaryPath
            $env:SAKURA_CANDIDATE_UIA_REPORT = Join-Path $reportRoot 'candidate-uia.json'
            Invoke-Gate -Name 'live candidate UIA and placement' -Arguments @(
                'cargo', 'test', '-p', 'sakura-renderer', '--test', 'candidate_uia', '--release',
                '--', '--exact', 'popup_follows_caret_pages_selects_by_digit_and_exposes_uia', '--ignored', '--nocapture'
            ) -CheckProcesses
        }
        finally {
            if ($null -eq $oldDictionary) { Remove-Item Env:SAKURA_PHASE2_DICTIONARY -ErrorAction SilentlyContinue } else { $env:SAKURA_PHASE2_DICTIONARY = $oldDictionary }
            if ($null -eq $oldReport) { Remove-Item Env:SAKURA_CANDIDATE_UIA_REPORT -ErrorAction SilentlyContinue } else { $env:SAKURA_CANDIDATE_UIA_REPORT = $oldReport }
        }
    }
    catch {
        $engineeringPassed = $false
        $steps.Add([ordered]@{ name = 'engineering terminal'; seconds = 0; exit_code = 1; passed = $false; error = $_.Exception.Message })
    }
    finally { Pop-Location }
}

$compatibility = Test-CompatibilityMatrix
$stagedUpdate = Test-StagedUpdate
$fuzzSummary = Join-Path $reportRoot 'fuzz-campaign-summary.json'
& rtk proxy pwsh -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'verify-fuzz-campaign.ps1') `
    -StateDirectory $fuzzRoot -RequiredHours 72 -RequiredShards 4 -SummaryPath $fuzzSummary
$fuzzPassed = $LASTEXITCODE -eq 0
if (-not $fuzzPassed) { $externalReasons.Add('72-hour IPC/dictionary/FSM campaign is incomplete or has a non-success terminal') }
$releaseBundleResult = Test-ReleaseBundle
$publishedRelease = Test-PublishedRelease
foreach ($reason in $compatibility.reasons) { $externalReasons.Add([string]$reason) }
foreach ($reason in $stagedUpdate.reasons) { $externalReasons.Add([string]$reason) }
foreach ($reason in $releaseBundleResult.reasons) { $externalReasons.Add([string]$reason) }
foreach ($reason in $publishedRelease.reasons) { $externalReasons.Add([string]$reason) }

$externalPassed = $externalReasons.Count -eq 0 -and $fuzzPassed
$strictPassed = $engineeringPassed -and $externalPassed
$summary = [ordered]@{
    schema_version = 1
    phase = 5
    generated_at_utc = [DateTime]::UtcNow.ToString('o')
    elapsed_seconds = [Math]::Round(([DateTime]::UtcNow - $started).TotalSeconds, 3)
    engineering = [ordered]@{ passed = $engineeringPassed; steps = @($steps) }
    compatibility = $compatibility
    staged_update = $stagedUpdate
    fuzz = [ordered]@{ passed = $fuzzPassed; summary = $fuzzSummary }
    signed_release_bundle = $releaseBundleResult
    published_release = $publishedRelease
    external_reasons = @($externalReasons)
    engineering_only = [bool]$EngineeringOnly
    passed = if ($EngineeringOnly) { $engineeringPassed } else { $strictPassed }
}
$temporary = "$summaryPath.$PID.tmp"
[IO.File]::WriteAllText($temporary, ($summary | ConvertTo-Json -Depth 14), [Text.UTF8Encoding]::new($false))
[IO.File]::Move($temporary, $summaryPath, $true)
$summary | ConvertTo-Json -Depth 14
if ($summary.passed) { exit 0 }
exit 1
