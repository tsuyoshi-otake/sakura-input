#Requires -Version 7.0
<#
.SYNOPSIS
    Records the default per-package Cargo test baseline from observed suites.

.DESCRIPTION
    Discovers workspace packages and targets with locked `cargo metadata`, then
    runs `cargo test --locked -p <package>` through run-test-quiet.ps1. Cargo's
    normal output is retained only in a temporary capture used to associate
    each `Running` label with its terminal libtest summary. The output JSON is
    published only after every expected suite has a successful summary.

.PARAMETER Compare
    Regenerates the same command universe and compares stable package, target,
    feature, and count records with an existing baseline. Volatile timestamps,
    source commits, toolchain descriptions, and artifact paths are ignored.
#>
[CmdletBinding()]
param(
    [ValidateNotNullOrEmpty()]
    [string]$Output = (Join-Path $PSScriptRoot 'test-baseline.json'),
    [string]$Compare = '',
    [switch]$SelfTest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repository = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$quietRunner = Join-Path $PSScriptRoot 'run-test-quiet.ps1'
$utf8NoBom = [Text.UTF8Encoding]::new($false)

function Assert-Rejected {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][scriptblock]$Check,
        [Parameter(Mandatory)][string]$ExpectedError
    )

    try {
        & $Check
    }
    catch {
        if ($_.Exception.Message -notlike $ExpectedError) {
            throw "self-test '$Name' failed for the wrong reason: $($_.Exception.Message)"
        }
        return
    }
    throw "self-test '$Name' unexpectedly passed"
}

function Assert-CargoSucceeded {
    param(
        [Parameter(Mandatory)][string]$PackageName,
        [Parameter(Mandatory)][int]$ExitCode
    )

    if ($ExitCode -ne 0) {
        throw "cargo test failed for package '$PackageName' with exit code $ExitCode"
    }
}

function Get-DefaultFeatureSet {
    param([Parameter(Mandatory)]$Package)

    $enabled = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $pending = [Collections.Generic.Queue[string]]::new()
    $defaultProperty = $Package.features.PSObject.Properties['default']
    if ($null -ne $defaultProperty) {
        foreach ($feature in @($defaultProperty.Value)) {
            if (-not [string]::IsNullOrWhiteSpace([string]$feature)) {
                $pending.Enqueue([string]$feature)
            }
        }
    }

    while ($pending.Count -gt 0) {
        $feature = $pending.Dequeue()
        if ($feature.StartsWith('dep:', [StringComparison]::Ordinal)) { continue }
        if ($feature.Contains('?/')) { continue }
        $featureName = ($feature -split '[/?]', 2)[0]
        $definition = $Package.features.PSObject.Properties[$featureName]
        if ($null -eq $definition -or -not $enabled.Add($featureName)) { continue }
        foreach ($nested in @($definition.Value)) {
            if (-not [string]::IsNullOrWhiteSpace([string]$nested)) {
                $pending.Enqueue([string]$nested)
            }
        }
    }
    Write-Output -NoEnumerate $enabled
}

function Get-RequiredFeatures {
    param([Parameter(Mandatory)]$Target)

    $property = $Target.PSObject.Properties['required-features']
    if ($null -eq $property) { return @() }
    return @($property.Value | Where-Object { -not [string]::IsNullOrWhiteSpace([string]$_) } | ForEach-Object { [string]$_ })
}

function Get-ExpectedSuites {
    param(
        [Parameter(Mandatory)]$Package,
        [Parameter(Mandatory)]$Target
    )

    $enabled = Get-DefaultFeatureSet -Package $Package
    $required = @(Get-RequiredFeatures -Target $Target)
    $missing = @($required | Where-Object { -not $enabled.Contains($_) })
    if ($missing.Count -gt 0) { return @() }

    $suites = [Collections.Generic.List[string]]::new()
    if ([bool]$Target.test) {
        if (@($Target.kind) -contains 'test') { $suites.Add('integration') }
        else { $suites.Add('unit') }
    }
    if ([bool]$Target.doctest) { $suites.Add('doc') }
    return $suites.ToArray()
}

function Convert-ToRepositoryPath {
    param([Parameter(Mandatory)][string]$Path)

    $fullPath = [IO.Path]::GetFullPath($Path)
    $prefix = $repository.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
    if (-not $fullPath.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Cargo metadata target is outside the repository: $fullPath"
    }
    return $fullPath.Substring($prefix.Length).Replace('\', '/')
}

function Resolve-SourceTarget {
    param(
        [Parameter(Mandatory)]$Package,
        [Parameter(Mandatory)][string]$SourceLabel
    )

    $suffix = '/' + $SourceLabel.Replace('\', '/').TrimStart('/')
    $matches = @($Package.targets | Where-Object {
        ([string]$_.src_path).Replace('\', '/').EndsWith($suffix, [StringComparison]::OrdinalIgnoreCase)
    })
    if ($matches.Count -ne 1) {
        throw "package '$($Package.name)' suite source '$SourceLabel' matched $($matches.Count) metadata targets"
    }
    return $matches[0]
}

function Resolve-DocTarget {
    param(
        [Parameter(Mandatory)]$Package,
        [Parameter(Mandatory)][string]$Label
    )

    $normalizedLabel = $Label.Replace('-', '_')
    $matches = @($Package.targets | Where-Object {
        [bool]$_.doctest -and ([string]$_.name).Replace('-', '_') -ceq $normalizedLabel
    })
    if ($matches.Count -ne 1) {
        throw "package '$($Package.name)' doc suite '$Label' matched $($matches.Count) metadata targets"
    }
    return $matches[0]
}

function Convert-CargoTestOutput {
    param(
        [Parameter(Mandatory)]$Package,
        [Parameter(Mandatory)][AllowEmptyCollection()][AllowEmptyString()][string[]]$Lines
    )

    $results = [Collections.Generic.List[object]]::new()
    $current = $null
    $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)

    foreach ($rawLine in $Lines) {
        $line = ([string]$rawLine) -replace "`e\[[0-9;]*[A-Za-z]", ''
        if ($line -match '^\s*Running\s+(?:unittests\s+)?(?<source>.+?)\s+\(.+\)\s*$') {
            if ($null -ne $current) {
                throw "package '$($Package.name)' suite '$($current.label)' has no terminal summary"
            }
            $sourceLabel = [string]$Matches['source']
            $target = Resolve-SourceTarget -Package $Package -SourceLabel $sourceLabel
            $suite = if (@($target.kind) -contains 'test') { 'integration' } else { 'unit' }
            $current = [pscustomobject]@{
                target = $target
                suite = $suite
                label = "Running $sourceLabel"
                running = $null
            }
            continue
        }
        if ($line -match '^\s*Doc-tests\s+(?<name>\S+)\s*$') {
            if ($null -ne $current) {
                throw "package '$($Package.name)' suite '$($current.label)' has no terminal summary"
            }
            $target = Resolve-DocTarget -Package $Package -Label ([string]$Matches['name'])
            $current = [pscustomobject]@{
                target = $target
                suite = 'doc'
                label = "Doc-tests $([string]$Matches['name'])"
                running = $null
            }
            continue
        }
        if ($line -match '^\s*running\s+(?<count>[0-9]+)\s+tests?\s*$' -and $null -ne $current) {
            if ($null -ne $current.running) {
                throw "package '$($Package.name)' suite '$($current.label)' reported its running count twice"
            }
            $current.running = [long]$Matches['count']
            continue
        }
        if ($line -match '^\s*test result:\s*(?<status>ok|FAILED)\.\s+(?<passed>[0-9]+)\s+passed;\s+(?<failed>[0-9]+)\s+failed;\s+(?<ignored>[0-9]+)\s+ignored;\s+(?<measured>[0-9]+)\s+measured;\s+(?<filtered>[0-9]+)\s+filtered out;') {
            if ($null -eq $current) {
                throw "package '$($Package.name)' emitted a test summary without a Running label"
            }
            if ($null -eq $current.running) {
                throw "package '$($Package.name)' suite '$($current.label)' has no running count"
            }

            $passed = [long]$Matches['passed']
            $failed = [long]$Matches['failed']
            $ignored = [long]$Matches['ignored']
            $measured = [long]$Matches['measured']
            $filtered = [long]$Matches['filtered']
            $observed = $passed + $failed + $ignored + $measured
            if ($observed -ne [long]$current.running) {
                throw "package '$($Package.name)' suite '$($current.label)' running count $($current.running) does not match summary count $observed"
            }
            if ([string]$Matches['status'] -cne 'ok' -or $failed -ne 0) {
                throw "package '$($Package.name)' suite '$($current.label)' did not pass"
            }

            $sourcePath = Convert-ToRepositoryPath -Path ([string]$current.target.src_path)
            $key = "$sourcePath|$($current.suite)"
            if (-not $seen.Add($key)) {
                throw "package '$($Package.name)' emitted duplicate suite '$key'"
            }
            $results.Add([pscustomobject][ordered]@{
                target = [string]$current.target.name
                target_kind = @($current.target.kind | ForEach-Object { [string]$_ })
                suite = [string]$current.suite
                source = $sourcePath
                cargo_label = [string]$current.label
                discovered = [long]$current.running
                executed = $passed + $failed + $measured
                passed = $passed
                failed = $failed
                ignored = $ignored
                measured = $measured
                filtered_out = $filtered
            })
            $current = $null
        }
    }

    if ($null -ne $current) {
        throw "package '$($Package.name)' suite '$($current.label)' has no terminal summary"
    }
    if ($results.Count -eq 0) {
        throw "package '$($Package.name)' emitted no test suite summaries"
    }

    foreach ($target in @($Package.targets)) {
        $sourcePath = Convert-ToRepositoryPath -Path ([string]$target.src_path)
        foreach ($suite in @(Get-ExpectedSuites -Package $Package -Target $target)) {
            if (-not $seen.Contains("$sourcePath|$suite")) {
                throw "package '$($Package.name)' expected $suite suite for '$sourcePath', but Cargo emitted no result"
            }
        }
    }
    return $results.ToArray()
}

function Get-TargetScope {
    param([Parameter(Mandatory)]$Package)

    $enabled = Get-DefaultFeatureSet -Package $Package
    return @($Package.targets | ForEach-Object {
        $required = @(Get-RequiredFeatures -Target $_)
        $missing = @($required | Where-Object { -not $enabled.Contains($_) })
        $expected = @(Get-ExpectedSuites -Package $Package -Target $_)
        $reason = $null
        if ($missing.Count -gt 0) {
            $reason = 'required default features are not enabled: ' + ($missing -join ', ')
        }
        elseif ($expected.Count -eq 0) {
            $reason = 'Cargo metadata disables test and doctest for this target'
        }
        [pscustomobject][ordered]@{
            name = [string]$_.name
            kinds = @($_.kind | ForEach-Object { [string]$_ })
            source = Convert-ToRepositoryPath -Path ([string]$_.src_path)
            test = [bool]$_.test
            doctest = [bool]$_.doctest
            required_features = $required
            expected_suites = $expected
            exclusion_reason = $reason
        }
    })
}

function Get-BuildTarget {
    param([Parameter(Mandatory)][string]$RustcHost)

    if (-not [string]::IsNullOrWhiteSpace([string]$env:CARGO_BUILD_TARGET)) {
        return [pscustomobject][ordered]@{ triple = [string]$env:CARGO_BUILD_TARGET; source = 'CARGO_BUILD_TARGET' }
    }
    $configPath = Join-Path $repository '.cargo\config.toml'
    if ([IO.File]::Exists($configPath)) {
        $text = [IO.File]::ReadAllText($configPath)
        $build = [regex]::Match($text, '(?ms)^\s*\[build\]\s*(?<body>.*?)(?=^\s*\[|\z)')
        if ($build.Success -and $build.Groups['body'].Value -match '(?m)^\s*target\s*=\s*"(?<target>[^"]+)"\s*(?:#.*)?$') {
            return [pscustomobject][ordered]@{ triple = [string]$Matches['target']; source = '.cargo/config.toml [build].target' }
        }
    }
    return [pscustomobject][ordered]@{ triple = $RustcHost; source = 'rustc host default' }
}

function Get-ComparisonLines {
    param([Parameter(Mandatory)]$Document)

    if ([string]$Document.schema -cne 'sakura-test-baseline-v1') {
        throw "unsupported test baseline schema: $([string]$Document.schema)"
    }
    if ([string]$Document.invocation.command -cne 'cargo test --locked -p <workspace-package>' -or
        [string]$Document.invocation.target_selection -cne 'Cargo default package targets' -or
        [string]$Document.invocation.feature_selection -cne 'default features') {
        throw 'test baseline uses a different Cargo command universe'
    }

    $lines = [Collections.Generic.List[string]]::new()
    $lines.Add("I|$([string]$Document.invocation.command)|$([string]$Document.toolchain.build_target.triple)")
    $seenPackages = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($package in @($Document.packages)) {
        $packageName = [string]$package.package
        if ([string]::IsNullOrWhiteSpace($packageName) -or -not $seenPackages.Add($packageName)) {
            throw "test baseline contains an empty or duplicate package: '$packageName'"
        }
        $manifest = ([string]$package.manifest).Replace('\', '/')
        $defaultFeatures = @($package.features.default_enabled | ForEach-Object { [string]$_ } | Sort-Object)
        $lines.Add("P|$packageName|$manifest|$([string]$package.features.mode)|$($defaultFeatures -join ',')|$([bool]$package.features.all_features)|$([bool]$package.features.no_default_features)")

        foreach ($target in @($package.target_scope)) {
            $source = ([string]$target.source).Replace('\', '/')
            $kinds = @($target.kinds | ForEach-Object { [string]$_ } | Sort-Object)
            $required = @($target.required_features | ForEach-Object { [string]$_ } | Sort-Object)
            $expected = @($target.expected_suites | ForEach-Object { [string]$_ } | Sort-Object)
            $lines.Add("S|$packageName|$([string]$target.name)|$($kinds -join ',')|$source|$([bool]$target.test)|$([bool]$target.doctest)|$($required -join ',')|$($expected -join ',')|$([string]$target.exclusion_reason)")
        }
        foreach ($result in @($package.target_results)) {
            $source = ([string]$result.source).Replace('\', '/')
            $kinds = @($result.target_kind | ForEach-Object { [string]$_ } | Sort-Object)
            $counts = @('discovered', 'executed', 'passed', 'failed', 'ignored', 'measured', 'filtered_out')
            $values = @($counts | ForEach-Object {
                $property = $result.PSObject.Properties[$_]
                if ($null -eq $property) { throw "test baseline result is missing count '$_'" }
                [long]$property.Value
            })
            $lines.Add("R|$packageName|$([string]$result.target)|$($kinds -join ',')|$([string]$result.suite)|$source|$($values -join ',')")
        }
    }
    if ($seenPackages.Count -eq 0) { throw 'test baseline contains no packages' }
    return @($lines.ToArray() | Sort-Object)
}

function Assert-BaselineMatches {
    param(
        [Parameter(Mandatory)]$Expected,
        [Parameter(Mandatory)]$Actual
    )

    $expectedLines = @(Get-ComparisonLines -Document $Expected)
    $actualLines = @(Get-ComparisonLines -Document $Actual)
    if (($expectedLines -join "`n") -ceq ($actualLines -join "`n")) { return }

    $difference = @(Compare-Object -ReferenceObject $expectedLines -DifferenceObject $actualLines -CaseSensitive)
    $first = $difference | Select-Object -First 1
    if ($null -eq $first) { throw 'test baseline differs from the current result' }
    $meaning = if ([string]$first.SideIndicator -ceq '<=') { 'missing from current result' } else { 'new in current result' }
    throw "test baseline differs ($meaning): $([string]$first.InputObject)"
}

function Invoke-TextCommand {
    param(
        [Parameter(Mandatory)][string]$Executable,
        [Parameter(Mandatory)][string[]]$Arguments
    )

    $outputLines = @(& $Executable @Arguments)
    $exitCode = [int]$global:LASTEXITCODE
    if ($exitCode -ne 0) {
        throw "$Executable $($Arguments -join ' ') failed with exit code $exitCode"
    }
    return ($outputLines -join "`n").Trim()
}

function Invoke-SelfTest {
    $fixturePackage = [pscustomobject]@{
        name = 'fixture-package'
        features = [pscustomobject]@{ default = @() }
        targets = @(
            [pscustomobject]@{ name = 'fixture_lib'; kind = @('lib'); src_path = (Join-Path $repository 'fixture\src\lib.rs'); test = $true; doctest = $true; 'required-features' = @() }
            [pscustomobject]@{ name = 'fixture-bin'; kind = @('bin'); src_path = (Join-Path $repository 'fixture\src\main.rs'); test = $true; doctest = $false; 'required-features' = @() }
            [pscustomobject]@{ name = 'integration'; kind = @('test'); src_path = (Join-Path $repository 'fixture\tests\integration.rs'); test = $true; doctest = $false; 'required-features' = @() }
        )
    }
    $fixture = @(
        'Running unittests src\lib.rs (target\debug\deps\fixture_lib.exe)'
        'running 3 tests'
        'test result: ok. 2 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.01s'
        'Running unittests src\main.rs (target\debug\deps\fixture_bin.exe)'
        'running 0 tests'
        'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s'
        'Running tests\integration.rs (target\debug\deps\integration.exe)'
        'running 1 test'
        'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s'
        'Doc-tests fixture_lib'
        'running 2 tests'
        'test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s'
        ''
    )
    $parsed = @(Convert-CargoTestOutput -Package $fixturePackage -Lines $fixture)
    if ($parsed.Count -ne 4 -or ($parsed | Measure-Object -Property passed -Sum).Sum -ne 5 -or ($parsed | Measure-Object -Property ignored -Sum).Sum -ne 1) {
        throw 'self-test valid mixed-suite fixture produced incorrect counts'
    }
    if ((@($parsed.suite) -join ',') -cne 'unit,unit,integration,doc') {
        throw 'self-test did not preserve suite labels'
    }
    if ($parsed[0].discovered -ne 3 -or $parsed[0].executed -ne 2 -or $parsed[0].ignored -ne 1) {
        throw 'self-test did not distinguish discovered, executed, and ignored tests'
    }

    $binOnlyPackage = [pscustomobject]@{
        name = 'bin-only'
        features = [pscustomobject]@{ default = @() }
        targets = @(
            [pscustomobject]@{ name = 'bin-only'; kind = @('bin'); src_path = (Join-Path $repository 'bin-only\src\main.rs'); test = $true; doctest = $false; 'required-features' = @() }
        )
    }
    $binOnly = @(Convert-CargoTestOutput -Package $binOnlyPackage -Lines @(
        'Running unittests src\main.rs (target\debug\deps\bin_only.exe)'
        'running 1 test'
        'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s'
    ))
    if ($binOnly.Count -ne 1 -or $binOnly[0].suite -cne 'unit' -or $binOnly[0].target -cne 'bin-only') {
        throw 'self-test bin-only package was not recorded as a binary unit suite'
    }

    $disabledPackage = [pscustomobject]@{
        name = 'feature-disabled'
        features = [pscustomobject]@{ default = @(); optional = @() }
        targets = @(
            [pscustomobject]@{ name = 'disabled-bin'; kind = @('bin'); src_path = (Join-Path $repository 'feature-disabled\src\main.rs'); test = $true; doctest = $false; 'required-features' = @('optional') }
        )
    }
    $disabledScope = @(Get-TargetScope -Package $disabledPackage)
    if ($disabledScope.Count -ne 1 -or @($disabledScope[0].expected_suites).Count -ne 0 -or
        [string]$disabledScope[0].exclusion_reason -notlike 'required default features are not enabled:*') {
        throw 'self-test required-feature-disabled binary was not explicitly excluded'
    }

    Assert-Rejected -Name 'missing summary' -ExpectedError '*has no terminal summary*' -Check {
        $null = Convert-CargoTestOutput -Package $fixturePackage -Lines @('Running unittests src\lib.rs (target\fixture.exe)', 'running 3 tests')
    }
    Assert-Rejected -Name 'failed summary' -ExpectedError '*did not pass*' -Check {
        $failedFixture = @($fixture[0..1]) + @('test result: FAILED. 2 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s')
        $null = Convert-CargoTestOutput -Package $fixturePackage -Lines $failedFixture
    }
    Assert-Rejected -Name 'cargo failure' -ExpectedError "cargo test failed for package 'fixture-package' with exit code 23" -Check {
        Assert-CargoSucceeded -PackageName 'fixture-package' -ExitCode 23
    }
    $comparisonFixture = [pscustomobject]@{
        schema = 'sakura-test-baseline-v1'
        generated_utc = 'old'
        source_commit = 'old'
        toolchain = [pscustomobject]@{ build_target = [pscustomobject]@{ triple = 'fixture-target' } }
        invocation = [pscustomobject]@{
            command = 'cargo test --locked -p <workspace-package>'
            target_selection = 'Cargo default package targets'
            feature_selection = 'default features'
        }
        packages = @([pscustomobject]@{
            package = 'fixture-package'; manifest = 'fixture/Cargo.toml'
            features = [pscustomobject]@{ mode = 'default'; default_enabled = @(); all_features = $false; no_default_features = $false }
            target_scope = @(Get-TargetScope -Package $fixturePackage)
            target_results = $parsed
        })
    }
    $comparisonCopy = $comparisonFixture | ConvertTo-Json -Depth 20 | ConvertFrom-Json
    $comparisonCopy.generated_utc = 'new'
    $comparisonCopy.source_commit = 'new'
    Assert-BaselineMatches -Expected $comparisonFixture -Actual $comparisonCopy
    $comparisonCopy.packages[0].target_results[0].passed--
    Assert-Rejected -Name 'lost test count' -ExpectedError 'test baseline differs*' -Check {
        Assert-BaselineMatches -Expected $comparisonFixture -Actual $comparisonCopy
    }
    $comparisonCopy.packages[0].target_results[0].passed++
    $comparisonCopy.packages[0].target_results = @($comparisonCopy.packages[0].target_results | Select-Object -Skip 1)
    Assert-Rejected -Name 'lost target suite' -ExpectedError 'test baseline differs*' -Check {
        Assert-BaselineMatches -Expected $comparisonFixture -Actual $comparisonCopy
    }
    Write-Output 'PASS: test baseline parser self-test'
}

if ($SelfTest) {
    Invoke-SelfTest
    exit 0
}

if (-not [IO.File]::Exists($quietRunner)) {
    throw "quiet test runner is missing: $quietRunner"
}

Push-Location $repository
try {
    $metadataText = Invoke-TextCommand -Executable 'cargo' -Arguments @('metadata', '--locked', '--no-deps', '--format-version', '1')
    try { $metadata = $metadataText | ConvertFrom-Json }
    catch { throw "cargo metadata returned invalid JSON: $($_.Exception.Message)" }

    $memberIds = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($member in @($metadata.workspace_members)) { $null = $memberIds.Add([string]$member) }
    $packages = @($metadata.packages | Where-Object { $memberIds.Contains([string]$_.id) })
    if ($packages.Count -eq 0) { throw 'cargo metadata returned no workspace packages' }

    $sourceCommit = Invoke-TextCommand -Executable 'git' -Arguments @('rev-parse', '--verify', 'HEAD')
    if ($sourceCommit -notmatch '^[0-9a-fA-F]{40}$') { throw "git returned an invalid source commit: $sourceCommit" }
    $cargoVersion = Invoke-TextCommand -Executable 'cargo' -Arguments @('--version')
    $rustcVerbose = Invoke-TextCommand -Executable 'rustc' -Arguments @('--version', '--verbose')
    $hostMatch = [regex]::Match($rustcVerbose, '(?m)^host:\s*(?<host>\S+)\s*$')
    if (-not $hostMatch.Success) { throw 'rustc verbose version did not report its host triple' }
    $buildTarget = Get-BuildTarget -RustcHost ([string]$hostMatch.Groups['host'].Value)

    $userProfile = [Environment]::GetFolderPath([Environment+SpecialFolder]::UserProfile)
    $temporaryRoot = Join-Path $userProfile 'tmp'
    [IO.Directory]::CreateDirectory($temporaryRoot) | Out-Null
    $captureRoot = Join-Path $temporaryRoot ('sakura-input-test-baseline-' + [guid]::NewGuid().ToString('N'))
    [IO.Directory]::CreateDirectory($captureRoot) | Out-Null
    $ownedCaptureFiles = [Collections.Generic.List[string]]::new()
    try {
        $packageRecords = [Collections.Generic.List[object]]::new()
        foreach ($package in $packages) {
            $packageName = [string]$package.name
            $defaultFeatureSet = Get-DefaultFeatureSet -Package $package
            $defaultFeatures = @(foreach ($feature in $defaultFeatureSet) { [string]$feature }) | Sort-Object
            $capturePath = Join-Path $captureRoot ($packageName + '-' + [guid]::NewGuid().ToString('N') + '.log')
            $exitPath = Join-Path $captureRoot ($packageName + '-' + [guid]::NewGuid().ToString('N') + '.exit')
            $ownedCaptureFiles.Add($capturePath)
            $ownedCaptureFiles.Add($exitPath)
            $command = {
                $capturedLines = [Collections.Generic.List[string]]::new()
                & cargo test --locked -p $packageName 2>&1 | ForEach-Object {
                    $text = [string]$_
                    $null = $capturedLines.Add($text)
                    Write-Output $text
                }
                $cargoExitCode = [int]$global:LASTEXITCODE
                [IO.File]::WriteAllLines($capturePath, $capturedLines.ToArray(), $utf8NoBom)
                [IO.File]::WriteAllText(
                    $exitPath,
                    $cargoExitCode.ToString([Globalization.CultureInfo]::InvariantCulture),
                    $utf8NoBom
                )
                $global:LASTEXITCODE = $cargoExitCode
            }.GetNewClosure()

            & $quietRunner -Name "$packageName default tests" -Command $command
            if (-not [IO.File]::Exists($exitPath)) {
                throw "cargo test for package '$packageName' produced no exit status"
            }
            [int]$cargoExitCode = 0
            $exitText = [IO.File]::ReadAllText($exitPath, $utf8NoBom)
            if (-not [int]::TryParse($exitText, [ref]$cargoExitCode)) {
                throw "cargo test for package '$packageName' produced an invalid exit status: $exitText"
            }
            Assert-CargoSucceeded -PackageName $packageName -ExitCode $cargoExitCode
            $captured = @([IO.File]::ReadAllLines($capturePath, $utf8NoBom))
            $targetResults = @(Convert-CargoTestOutput -Package $package -Lines $captured)
            $packageRecords.Add([pscustomobject][ordered]@{
                package = $packageName
                manifest = Convert-ToRepositoryPath -Path ([string]$package.manifest_path)
                features = [pscustomobject][ordered]@{
                    mode = 'default'
                    default_enabled = @($defaultFeatures)
                    all_features = $false
                    no_default_features = $false
                }
                target_scope = @(Get-TargetScope -Package $package)
                target_results = $targetResults
                totals = [pscustomobject][ordered]@{
                    discovered = [long](($targetResults | Measure-Object -Property discovered -Sum).Sum)
                    executed = [long](($targetResults | Measure-Object -Property executed -Sum).Sum)
                    passed = [long](($targetResults | Measure-Object -Property passed -Sum).Sum)
                    failed = [long](($targetResults | Measure-Object -Property failed -Sum).Sum)
                    ignored = [long](($targetResults | Measure-Object -Property ignored -Sum).Sum)
                }
            })
        }

        $allResults = @($packageRecords | ForEach-Object { $_.target_results })
        $document = [pscustomobject][ordered]@{
            schema = 'sakura-test-baseline-v1'
            generated_utc = [DateTime]::UtcNow.ToString('o', [Globalization.CultureInfo]::InvariantCulture)
            source_commit = $sourceCommit.ToLowerInvariant()
            toolchain = [pscustomobject][ordered]@{
                cargo = $cargoVersion
                rustc_verbose = $rustcVerbose -split "`n"
                build_target = $buildTarget
            }
            invocation = [pscustomobject][ordered]@{
                command = 'cargo test --locked -p <workspace-package>'
                package_discovery = 'cargo metadata --locked --no-deps --format-version 1'
                target_selection = 'Cargo default package targets'
                feature_selection = 'default features'
            }
            packages = $packageRecords.ToArray()
            totals = [pscustomobject][ordered]@{
                packages = [long]$packageRecords.Count
                suites = [long]$allResults.Count
                discovered = [long](($allResults | Measure-Object -Property discovered -Sum).Sum)
                executed = [long](($allResults | Measure-Object -Property executed -Sum).Sum)
                passed = [long](($allResults | Measure-Object -Property passed -Sum).Sum)
                failed = [long](($allResults | Measure-Object -Property failed -Sum).Sum)
                ignored = [long](($allResults | Measure-Object -Property ignored -Sum).Sum)
            }
        }

        if (-not [string]::IsNullOrWhiteSpace($Compare)) {
            $comparePath = [IO.Path]::GetFullPath($Compare)
            if (-not [IO.File]::Exists($comparePath)) { throw "test baseline to compare is missing: $comparePath" }
            try { $expectedDocument = [IO.File]::ReadAllText($comparePath, $utf8NoBom) | ConvertFrom-Json }
            catch { throw "test baseline to compare is invalid JSON: $($_.Exception.Message)" }
            Assert-BaselineMatches -Expected $expectedDocument -Actual $document
            Write-Output "PASS: test baseline matches: $comparePath"
        }
        else {
            $outputPath = [IO.Path]::GetFullPath($Output)
            $outputDirectory = [IO.Path]::GetDirectoryName($outputPath)
            [IO.Directory]::CreateDirectory($outputDirectory) | Out-Null
            $temporaryOutput = Join-Path $outputDirectory ('.test-baseline-' + [guid]::NewGuid().ToString('N') + '.tmp')
            try {
                [IO.File]::WriteAllText($temporaryOutput, (($document | ConvertTo-Json -Depth 12) + [Environment]::NewLine), $utf8NoBom)
                if ([IO.File]::Exists($outputPath)) {
                    [IO.File]::Replace($temporaryOutput, $outputPath, $null)
                }
                else {
                    [IO.File]::Move($temporaryOutput, $outputPath)
                }
            }
            finally {
                if ([IO.File]::Exists($temporaryOutput)) { [IO.File]::Delete($temporaryOutput) }
            }
            Write-Output "PASS: test baseline recorded: $outputPath"
        }
    }
    finally {
        foreach ($ownedFile in $ownedCaptureFiles) {
            if ([IO.File]::Exists($ownedFile)) { [IO.File]::Delete($ownedFile) }
        }
        if ([IO.Directory]::Exists($captureRoot)) { [IO.Directory]::Delete($captureRoot, $false) }
    }
}
finally {
    Pop-Location
}
