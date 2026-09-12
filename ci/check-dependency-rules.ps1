[CmdletBinding()]
param(
    [switch]$Advisory,
    [string[]]$Enforce = @(),
    [switch]$SelfTest,
    [switch]$ForceNoRg
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$repoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$allRules = @((1..9 | ForEach-Object { "R$_" }) + @('R12', 'R13'))
$requested = @($Enforce | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim().ToUpperInvariant() } | Where-Object { $_ })
$unknown = @($requested | Where-Object { $allRules -cnotcontains $_ } | Sort-Object -Unique)
if ($unknown.Count) { throw "Unknown dependency rule(s): $($unknown -join ', ')" }
$enforced = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
foreach ($rule in $requested) { [void]$enforced.Add($rule) }
$results = [Collections.Generic.List[object]]::new()

function Resolve-NativeCommand([string[]]$Names, [switch]$Required) {
    foreach ($name in $Names) {
        $command = Get-Command -Name $name -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($null -ne $command) {
            $path = if (-not [string]::IsNullOrWhiteSpace([string]$command.Source)) {
                [string]$command.Source
            }
            else {
                [string]$command.Path
            }
            if (-not [string]::IsNullOrWhiteSpace($path)) { return $path }
        }
    }
    if ($Required) { throw "Required native command was not found: $($Names -join ', ')" }
    return $null
}

$rgExecutable = if ($ForceNoRg) { $null } else { Resolve-NativeCommand @('rg.exe', 'rg') }

function Get-RustFiles([string]$Root, [switch]$IncludeTests) {
    if (-not [IO.Directory]::Exists($Root)) { return @() }
    $options = [IO.EnumerationOptions]::new()
    $options.RecurseSubdirectories = $true
    $options.IgnoreInaccessible = $false
    $options.AttributesToSkip = [IO.FileAttributes]::ReparsePoint
    $files = @([IO.Directory]::EnumerateFiles($Root, '*.rs', $options))
    if ($IncludeTests) { return $files }
    return @($files | Where-Object {
        $relative = [IO.Path]::GetRelativePath($Root, $_)
        $relative -notmatch '(^|[\\/])tests([\\/]|$)'
    })
}

function Invoke-Captured([string]$Executable, [string[]]$Arguments, [int]$TimeoutMilliseconds = 60000) {
    $start = [Diagnostics.ProcessStartInfo]::new($Executable)
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    foreach ($argument in $Arguments) { $start.ArgumentList.Add($argument) }
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $start
    $started = $false
    try {
        $started = $process.Start()
        if (-not $started) { throw "Failed to start $Executable" }
        $outTask = $process.StandardOutput.ReadToEndAsync()
        $errTask = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit($TimeoutMilliseconds)) {
            $process.Kill($true)
            if (-not $process.WaitForExit(10000)) { throw "Owned process survived timeout: $Executable" }
            throw "Tool timed out: $Executable"
        }
        if (-not [Threading.Tasks.Task]::WaitAll([Threading.Tasks.Task[]]@($outTask, $errTask), 10000)) {
            throw "Tool output did not close: $Executable"
        }
        return [pscustomobject]@{ ExitCode = $process.ExitCode; Stdout = $outTask.Result; Stderr = $errTask.Result }
    }
    finally {
        if ($started -and -not $process.HasExited) {
            $process.Kill($true)
            if (-not $process.WaitForExit(10000)) { throw "Owned process survived cleanup: $Executable" }
        }
        $process.Dispose()
    }
}

function Add-Result([string]$Rule, [string]$State, [string]$Summary, [string[]]$Details = @()) {
    $results.Add([pscustomobject]@{ Rule = $Rule; State = $State; Summary = $Summary; Details = @($Details) })
}

function Find-InFiles([string[]]$Files, [string]$Pattern) {
    $sourceHits = [Collections.Generic.List[string]]::new()
    foreach ($file in $Files) {
        $lineNumber = 0
        foreach ($line in [IO.File]::ReadAllLines($file)) {
            $lineNumber++
            if ($line -cmatch $Pattern) {
                $relative = [IO.Path]::GetRelativePath($repoRoot, $file)
                $sourceHits.Add("${relative}:${lineNumber}:$($line.Trim())")
            }
        }
    }
    return @($sourceHits)
}

function Invoke-SourceScan([string]$Rule, [string]$Path, [string]$Pattern) {
    if (-not [IO.File]::Exists($Path) -and -not [IO.Directory]::Exists($Path)) {
        throw "Scan path does not exist for ${Rule}: $Path"
    }
    if ($null -ne $rgExecutable) {
        $run = Invoke-Captured $rgExecutable @('--no-config', '--no-ignore', '--hidden', '--with-filename', '--no-heading', '--color', 'never', '-n', '--glob', '*.rs', $Pattern, $Path)
        if ($run.ExitCode -eq 1) { return @() }
        if ($run.ExitCode -eq 0) {
            if ([string]::IsNullOrEmpty($run.Stdout)) { throw "rg returned success without matches for $Rule" }
            return @($run.Stdout.TrimEnd() -split "`r?`n")
        }
        throw "rg failed for $Rule (exit $($run.ExitCode)): $($run.Stderr.Trim())"
    }

    $files = if ([IO.File]::Exists($Path)) {
        if ([IO.Path]::GetExtension($Path) -cne '.rs') { @() } else { @($Path) }
    }
    else {
        @(Get-RustFiles $Path -IncludeTests)
    }
    return @(Find-InFiles $files $Pattern)
}

function Invoke-SourceRule([string]$Rule, [string]$RelativePath, [string]$Pattern, [string]$Boundary) {
    $path = Join-Path $repoRoot $RelativePath
    if (-not [IO.File]::Exists($path) -and -not [IO.Directory]::Exists($path)) {
        Add-Result $Rule 'PENDING' "$Boundary pending: future path does not exist" @($RelativePath)
        return
    }
    $hits = @(Invoke-SourceScan $Rule $path $Pattern)
    if (-not $hits.Count) {
        Add-Result $Rule 'PASS' $Boundary
    }
    else {
        Add-Result $Rule 'VIOLATION' $Boundary $hits
    }
}

function Get-R5Audit([string]$RendererRoot) {
    $findings = [Collections.Generic.List[string]]::new()
    $pending = [Collections.Generic.List[string]]::new()
    $scopes = @(
        [pscustomobject]@{
            Name = 'indicator -> candidate'
            Current = Join-Path $RendererRoot 'indicator.rs'
            Future = Join-Path $RendererRoot 'indicator'
            Pattern = '(?:\bcrate::candidate\b|\buse\s+crate::\{[^}\r\n]*\bcandidate\b)'
        },
        [pscustomobject]@{
            Name = 'candidate -> accessibility/watch'
            Current = Join-Path $RendererRoot 'candidate.rs'
            Future = Join-Path $RendererRoot 'candidate'
            Pattern = '(?:\bcrate::(?:accessibility|watch)\b|\buse\s+crate::\{[^}\r\n]*\b(?:accessibility|watch)\b)'
        }
    )
    foreach ($scope in $scopes) {
        $paths = @(@($scope.Current, $scope.Future) | Where-Object { [IO.File]::Exists($_) -or [IO.Directory]::Exists($_) })
        if (-not $paths.Count) {
            $pending.Add("$($scope.Name) scan pending: current and future paths do not exist")
            continue
        }
        if (-not [IO.Directory]::Exists($scope.Future)) {
            $pending.Add("$($scope.Name) future module pending: $([IO.Path]::GetRelativePath($repoRoot, $scope.Future))")
        }
        foreach ($path in $paths) {
            foreach ($hit in @(Invoke-SourceScan 'R5' $path $scope.Pattern)) { $findings.Add("$($scope.Name): $hit") }
        }
    }
    return [pscustomobject]@{ Findings = @($findings); Pending = @($pending) }
}

function Get-DirectNormalDependency($Metadata, [string]$Package, [string]$Dependency) {
    $packages = @($Metadata.packages | Where-Object { $_.name -ceq $Package })
    if ($packages.Count -ne 1) { throw "cargo metadata did not identify exactly one $Package package" }
    return @($packages[0].dependencies | Where-Object { $_.name -ceq $Dependency -and $null -eq $_.kind })
}

function Check-DirectBoundary($Metadata, [string]$Rule, [string]$Package, [string]$Dependency, [string]$SourcePath, [string]$ImportPattern, [string]$Summary) {
    $details = [Collections.Generic.List[string]]::new()
    $edges = @(Get-DirectNormalDependency $Metadata $Package $Dependency)
    foreach ($edge in $edges) {
        $target = if ($null -eq $edge.target) { 'all targets' } else { "target $($edge.target)" }
        $optional = if ($edge.optional) { ', optional' } else { '' }
        $details.Add("Cargo.toml direct normal dependency: $Package -> $Dependency ($target$optional)")
    }

    $path = Join-Path $repoRoot $SourcePath
    if (-not [IO.Directory]::Exists($path)) { throw "Source boundary does not exist: $SourcePath" }
    foreach ($line in @(Invoke-SourceScan $Rule $path $ImportPattern)) { $details.Add($line) }
    Add-Result $Rule $(if ($details.Count) { 'VIOLATION' } else { 'PASS' }) $Summary @($details)
}

function Get-R13Findings($Metadata, [string]$SourceRoot) {
    $findings = [Collections.Generic.List[string]]::new()
    $packages = @($Metadata.packages | Where-Object { $_.name -ceq 'sakura-store' })
    if ($packages.Count -ne 1) {
        $findings.Add("expected one sakura-store package, found $($packages.Count)")
    }
    else {
        $allowed = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
        [void]$allowed.Add('sakura-values')
        [void]$allowed.Add('windows')
        $normal = @($packages[0].dependencies | Where-Object { $null -eq $_.kind })
        foreach ($dependency in $normal) {
            if (-not $allowed.Contains([string]$dependency.name)) {
                $findings.Add("sakura-store has forbidden normal dependency: $($dependency.name)")
            }
        }
        if (@($normal | Where-Object { $_.name -ceq 'sakura-values' }).Count -ne 1) {
            $findings.Add('sakura-store must have exactly one normal dependency on sakura-values')
        }
    }

    if (-not [IO.Directory]::Exists($SourceRoot)) {
        $findings.Add('crates/sakura-store/src is missing')
    }
    else {
        # Durable records legitimately contain a `session` value. Reject only
        # runtime ownership/dependency paths; never scan for that raw token.
        $pattern = '(?:\bstd::thread\b|\bstd::sync::mpsc\b|\bstd::sync::\{[^}\r\n]*\bmpsc\b|\bstd::\{[^}\r\n]*\bthread\b|\bmpsc::|\bsakura_engine\b|\bsakura_proto\b|\bsakura_ipc\b|\bcrate::session(?:::|\b)|::session::)'
        foreach ($hit in @(Invoke-SourceScan 'R13' $SourceRoot $pattern)) { $findings.Add($hit) }
    }
    return @($findings)
}

function Get-RerankOwnershipFindings([string[]]$Files, [string]$OwnerRoot) {
    $findings = [Collections.Generic.List[string]]::new()
    $ownerPrefix = [IO.Path]::GetFullPath($OwnerRoot).TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
    $requestPattern = '(?i)0x0*524e_?4b53\b'
    $responsePattern = '(?i)0x0*534e_?4b53\b'
    $rerankFiles = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($file in $Files) {
        $text = [IO.File]::ReadAllText($file)
        $relative = [IO.Path]::GetRelativePath($repoRoot, $file).Replace('\', '/')
        if ($text -match $requestPattern -or $text -match $responsePattern -or
            $relative -ceq 'crates/sakura-engine/src/long_conversion.rs' -or
            $relative -ceq 'crates/sakura-neural-worker/src/protocol.rs' -or
            [IO.Path]::GetFullPath($file).StartsWith($ownerPrefix, [StringComparison]::OrdinalIgnoreCase)) {
            [void]$rerankFiles.Add($file)
        }
    }

    $concepts = [ordered]@{
        request_magic = $requestPattern
        response_magic = $responsePattern
        version = '(?m)\bconst\s+(?:PROTOCOL_VERSION|VERSION)\s*:\s*u16\s*=\s*1\s*;'
        frame_bound = '(?m)\bconst\s+(?:MAX_FRAME|MAXIMUM_FRAME_BYTES)\s*:\s*usize\s*=\s*32\s*\*\s*1024\s*;'
        candidate_bound = '(?m)\bconst\s+(?:MAX_CANDIDATES|MAXIMUM_MODEL_CANDIDATES)\s*:\s*usize\s*=\s*6\s*;'
        candidate_bytes_bound = '(?m)(?:\bconst\s+MAX_CANDIDATE_BYTES\s*:\s*usize\s*=\s*)?3\s*\*\s*1024\b'
    }
    foreach ($concept in $concepts.GetEnumerator()) {
        $locations = [Collections.Generic.List[string]]::new()
        $occurrences = 0
        foreach ($file in $rerankFiles) {
            $text = [IO.File]::ReadAllText($file)
            $found = @([regex]::Matches($text, $concept.Value))
            if ($found.Count) {
                $relative = [IO.Path]::GetRelativePath($repoRoot, $file)
                $locations.Add("$relative ($($found.Count))")
                $occurrences += $found.Count
            }
        }
        if ($occurrences -ne 1) {
            $findings.Add("$($concept.Key) must have one definition/literal; found ${occurrences}: $($locations -join ', ')")
        }
        else {
            $ownerFile = @($rerankFiles | Where-Object { [regex]::IsMatch([IO.File]::ReadAllText($_), $concept.Value) })[0]
            if (-not [IO.Path]::GetFullPath($ownerFile).StartsWith($ownerPrefix, [StringComparison]::OrdinalIgnoreCase)) {
                $findings.Add("$($concept.Key) is owned outside sakura-rerank-proto: $([IO.Path]::GetRelativePath($repoRoot, $ownerFile))")
            }
        }
    }
    return @($findings)
}

function Get-R9Findings([string[]]$Files, [string]$OracleRoot = (Join-Path $repoRoot 'crates/sakura-oracles/src')) {
    $findings = [Collections.Generic.List[string]]::new()
    $oraclePrefix = [IO.Path]::GetFullPath($OracleRoot).TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
    $declarationPattern = '(?ms)#\[cfg\s*\(\s*test\s*\)\]\s*(?:#\[path\s*=\s*"([^"]+)"\]\s*)?mod\s+([A-Za-z0-9_]+)\s*;'
    foreach ($file in $Files) {
        $text = [IO.File]::ReadAllText($file)
        $leaf = [IO.Path]::GetFileName($file)
        $relative = [IO.Path]::GetRelativePath($repoRoot, $file)
        if ($text -match '(?m)^\s*#!\[cfg\s*\(\s*test\s*\)\]\s*$' -and
            $leaf -notmatch '_tests\.rs$' -and $leaf -cne 'testing.rs') {
            $findings.Add("$relative is a file-level cfg(test) module outside *_tests.rs/testing.rs")
        }
        if ($leaf -match '_oracle\.rs$' -and -not [IO.Path]::GetFullPath($file).StartsWith($oraclePrefix, [StringComparison]::OrdinalIgnoreCase)) {
            $findings.Add("$relative is an oracle under src; migration to sakura-oracles is pending")
        }
        foreach ($match in [regex]::Matches($text, $declarationPattern)) {
            $target = if ($match.Groups[1].Success) { [IO.Path]::GetFileName($match.Groups[1].Value) } else { "$($match.Groups[2].Value).rs" }
            if ($target -notmatch '_tests\.rs$' -and $target -cne 'testing.rs') {
                $findings.Add("$relative declares test-only file $target outside *_tests.rs/testing.rs")
            }
        }
    }
    return @($findings | Sort-Object -Unique)
}

function Get-OracleDependencyFindings($Metadata) {
    foreach ($package in $Metadata.packages) {
        foreach ($dependency in $package.dependencies) {
            if ($dependency.name -ceq 'sakura-oracles' -and $dependency.kind -cne 'dev') {
                "$($package.name) must consume sakura-oracles only as a dev-dependency"
            }
        }
    }
}

if ($SelfTest) {
    $tmpRoot = [IO.Path]::GetFullPath((Join-Path $env:USERPROFILE 'tmp'))
    [void][IO.Directory]::CreateDirectory($tmpRoot)
    $fixtureRoot = [IO.Path]::GetFullPath((Join-Path $tmpRoot ("dependency-rules-selftest-" + [Guid]::NewGuid().ToString('N'))))
    $tmpPrefix = $tmpRoot.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
    if (-not $fixtureRoot.StartsWith($tmpPrefix, [StringComparison]::OrdinalIgnoreCase)) { throw 'Unsafe self-test fixture path' }
    [void][IO.Directory]::CreateDirectory($fixtureRoot)
    $fixtureFailure = $null
    try {
        $allowed = Join-Path $fixtureRoot 'feature_tests.rs'
        $testing = Join-Path $fixtureRoot 'testing.rs'
        $bad = Join-Path $fixtureRoot 'oracle.rs'
        $owner = Join-Path $fixtureRoot 'sakura-rerank-proto/src'
        [void][IO.Directory]::CreateDirectory($owner)
        [IO.File]::WriteAllText($allowed, "#![cfg(test)]`n")
        [IO.File]::WriteAllText($testing, "#![cfg(test)]`n")
        [IO.File]::WriteAllText($bad, "#![cfg(test)]`n")
        if (@(Get-R9Findings @($allowed, $testing)).Count -ne 0) { throw 'R9 allowed fixture failed' }
        if (@(Get-R9Findings @($bad)).Count -ne 1) { throw 'R9 negative fixture failed' }
        $oracleOwner = Join-Path $fixtureRoot 'sakura-oracles/src'
        [void][IO.Directory]::CreateDirectory($oracleOwner)
        $ownedOracle = Join-Path $oracleOwner 'reference_oracle.rs'
        [IO.File]::WriteAllText($ownedOracle, '// independent reference algorithm')
        if (@(Get-R9Findings @($ownedOracle) $oracleOwner).Count -ne 0) { throw 'R9 oracle owner fixture failed' }
        if (@(Get-R9Findings @($ownedOracle) ($oracleOwner + '-other')).Count -ne 1) { throw 'R9 oracle sibling exclusion failed' }
        foreach ($kind in @('dev', 'build', $null)) {
            $oracleMetadata = [pscustomobject]@{ packages = @([pscustomobject]@{
                name = 'consumer'; dependencies = @([pscustomobject]@{ name = 'sakura-oracles'; kind = $kind })
            }) }
            $expectedCount = if ($kind -ceq 'dev') { 0 } else { 1 }
            if (@(Get-OracleDependencyFindings $oracleMetadata).Count -ne $expectedCount) { throw 'R9 oracle dependency fixture failed' }
        }

        $ownerFile = Join-Path $owner 'lib.rs'
        $uniqueText = @'
const REQUEST_MAGIC: u32 = 0x524e_4b53;
const RESPONSE_MAGIC: u32 = 0x534e_4b53;
const PROTOCOL_VERSION: u16 = 1;
const MAX_FRAME: usize = 32 * 1024;
const MAX_CANDIDATES: usize = 6;
const MAX_CANDIDATE_BYTES: usize = 3 * 1024;
'@
        [IO.File]::WriteAllText($ownerFile, $uniqueText)
        if (@(Get-RerankOwnershipFindings @($ownerFile) (Join-Path $fixtureRoot 'sakura-rerank-proto')).Count -ne 0) {
            throw 'R8 unique-owner fixture failed'
        }
        $duplicate = Join-Path $fixtureRoot 'duplicate.rs'
        [IO.File]::WriteAllText($duplicate, 'const REQUEST_MAGIC: u32 = 0x524e_4b53;')
        if (@(Get-RerankOwnershipFindings @($ownerFile, $duplicate) (Join-Path $fixtureRoot 'sakura-rerank-proto')).Count -eq 0) {
            throw 'R8 duplicate fixture failed'
        }

        $fakeMetadata = [pscustomobject]@{ packages = @(
            [pscustomobject]@{ name = 'source'; dependencies = @(
                [pscustomobject]@{ name = 'forbidden'; kind = $null; optional = $true; target = 'cfg(windows)' },
                [pscustomobject]@{ name = 'dev-only'; kind = 'dev'; optional = $false; target = $null }
            ) }
        ) }
        if (@(Get-DirectNormalDependency $fakeMetadata 'source' 'forbidden').Count -ne 1 -or
            @(Get-DirectNormalDependency $fakeMetadata 'source' 'dev-only').Count -ne 0) {
            throw 'R1/R2 metadata edge fixture failed'
        }
        if (@(Find-InFiles @($ownerFile) 'REQUEST_MAGIC').Count -ne 1 -or
            @(Find-InFiles @($ownerFile) 'DOES_NOT_EXIST').Count -ne 0) {
            throw 'Source scan fixture failed'
        }

        $fallbackRoot = Join-Path $fixtureRoot 'fallback-boundary'
        [void][IO.Directory]::CreateDirectory($fallbackRoot)
        $fallbackFile = Join-Path $fallbackRoot 'lib.rs'
        [IO.File]::WriteAllText($fallbackFile, "use sakura_proto::Message;`nuse SAKURA_PROTO::Message;`n")
        $savedRgExecutable = $rgExecutable
        try {
            $rgExecutable = $null
            $fallbackHits = @(Invoke-SourceScan 'self-test fallback' $fallbackRoot '\bsakura_proto\b')
            $fallbackState = if ($fallbackHits.Count) { 'VIOLATION' } else { 'PASS' }
            if ($fallbackState -cne 'VIOLATION' -or $fallbackHits.Count -ne 1 -or
                [string]$fallbackHits[0] -notmatch 'lib\.rs:1:use sakura_proto::Message;') {
                throw 'Missing-rg fallback did not reject the known source violation'
            }
        }
        finally {
            $rgExecutable = $savedRgExecutable
        }
        $r13Fixture = Join-Path $fixtureRoot 'store/src'
        [void][IO.Directory]::CreateDirectory($r13Fixture)
        $r13File = Join-Path $r13Fixture 'format.rs'
        [IO.File]::WriteAllText($r13File, "pub struct Record { pub session: u64 }`n")
        $r13Metadata = [pscustomobject]@{ packages = @(
            [pscustomobject]@{ name = 'sakura-store'; dependencies = @(
                [pscustomobject]@{ name = 'sakura-values'; kind = $null },
                [pscustomobject]@{ name = 'windows'; kind = $null },
                [pscustomobject]@{ name = 'dev-helper'; kind = 'dev' }
            ) }
        ) }
        if (@(Get-R13Findings $r13Metadata $r13Fixture).Count -ne 0) {
            throw 'R13 allowed dependency/session-field fixture failed'
        }
        [IO.File]::WriteAllText($r13File, "pub struct Record { pub session: u64 }`nuse std::thread;`n")
        $r13Metadata.packages[0].dependencies += [pscustomobject]@{ name = 'sakura-engine'; kind = $null }
        if (@(Get-R13Findings $r13Metadata $r13Fixture).Count -ne 2) {
            throw 'R13 runtime path/dependency negative fixture failed'
        }
        $r12Fixture = Join-Path $fixtureRoot 'values'
        [void][IO.Directory]::CreateDirectory($r12Fixture)
        $r12Bad = Join-Path $r12Fixture 'lib.rs'
        [IO.File]::WriteAllText($r12Bad, "pub fn encode() {}`n")
        $r12Pattern = '(?:\bsakura_proto\b|\bwire::|\bReader\b|\bSink\b|\bfn\s+encode\b|\bfn\s+decode\b)'
        $savedRgExecutable = $rgExecutable
        try {
            $rgExecutable = $null
            if (@(Invoke-SourceScan 'R12 fixture' $r12Fixture $r12Pattern).Count -ne 1) {
                throw 'R12 missing-rg fixture did not reject fn encode'
            }
        }
        finally {
            $rgExecutable = $savedRgExecutable
        }

        $renderer = Join-Path $fixtureRoot 'renderer'
        $indicator = Join-Path $renderer 'indicator'
        $candidate = Join-Path $renderer 'candidate'
        [void][IO.Directory]::CreateDirectory($indicator)
        [void][IO.Directory]::CreateDirectory($candidate)
        $indicatorFile = Join-Path $indicator 'mod.rs'
        $candidateFile = Join-Path $candidate 'mod.rs'
        [IO.File]::WriteAllText($indicatorFile, 'use crate::events::CandidateEvent;')
        [IO.File]::WriteAllText($candidateFile, 'use crate::events::CandidateEvent;')
        $r5Positive = Get-R5Audit $renderer
        if ($r5Positive.Findings.Count -ne 0 -or $r5Positive.Pending.Count -ne 0) { throw 'R5 positive fixture failed' }
        [IO.File]::WriteAllText($indicatorFile, 'use crate::candidate::Window;')
        [IO.File]::WriteAllText($candidateFile, 'use crate::{accessibility, watch};')
        $r5Negative = Get-R5Audit $renderer
        if ($r5Negative.Findings.Count -ne 2) { throw 'R5 negative fixture failed' }

        Write-Host 'PASS: dependency rule fixtures cover metadata edges, rg-free source rejection, R5/R8 ownership, R9 placement, R12 values isolation, and R13 store isolation'
    }
    catch {
        $fixtureFailure = $_
        throw
    }
    finally {
        if ([IO.Directory]::Exists($fixtureRoot)) {
            try { [IO.Directory]::Delete($fixtureRoot, $true) }
            catch {
                if ($null -ne $fixtureFailure) {
                    Write-Warning "Self-test cleanup also failed for ${fixtureRoot}: $($_.Exception.Message)"
                }
                else { throw }
            }
        }
    }
    return
}

$cargo = Resolve-NativeCommand @('cargo.exe', 'cargo') -Required
$metadataRun = Invoke-Captured $cargo @('metadata', '--no-deps', '--format-version', '1', '--locked')
if ($metadataRun.ExitCode -ne 0) { throw "cargo metadata failed (exit $($metadataRun.ExitCode)): $($metadataRun.Stderr.Trim())" }
try { $metadata = $metadataRun.Stdout | ConvertFrom-Json } catch { throw "cargo metadata returned invalid JSON: $($_.Exception.Message)" }

Check-DirectBoundary $metadata 'R1' 'sakura-core' 'sakura-proto' 'crates/sakura-core/src' '\bsakura_proto\b' 'core must not know proto'
Check-DirectBoundary $metadata 'R2' 'sakura-settings' 'sakura-engine' 'crates/sakura-settings/src' '\bsakura_engine\b' 'settings must not know engine'
Invoke-SourceRule 'R3' 'crates/sakura-tsf/src/session' '(?:\buse\s+windows\b|\bwindows::)' 'TSF session must not know Win32/COM'
Invoke-SourceRule 'R4' 'crates/sakura-engine/src/state' '\buse\s+crate::(?:keys|commit|render|ipc|request|services|runtime)\b' 'engine state must not know upper layers'
$r5 = Get-R5Audit (Join-Path $repoRoot 'crates/sakura-renderer/src')
$r5Details = @($r5.Findings) + @($r5.Pending)
$r5State = if ($r5.Findings.Count) { 'VIOLATION' } elseif ($r5.Pending.Count) { 'PENDING' } else { 'PASS' }
Add-Result 'R5' $r5State 'renderer boundary transitional regex scan (AST module-edge enforcement pending)' $r5Details
Invoke-SourceRule 'R6' 'crates/sakura-renderer/src/pad/rail.rs' '\bcrate::pad::' 'pad rail must not know pad implementation'
Invoke-SourceRule 'R7' 'crates/sakura-settings/src/ui/presentation.rs' '\buse\s+(?:super|crate)::' 'settings UI presentation must be a leaf'

$allRust = Get-RustFiles (Join-Path $repoRoot 'crates') -IncludeTests
$productionRust = @($allRust | Where-Object { $_ -match '[\\/]src[\\/]' })
$allowedWireFixture = [IO.Path]::GetFullPath((Join-Path $repoRoot 'crates/sakura-neural-worker/tests/real_model_e2e.rs'))
$r8Files = @($allRust | Where-Object { [IO.Path]::GetFullPath($_) -cne $allowedWireFixture })
$ownerRoot = Join-Path $repoRoot 'crates/sakura-rerank-proto'
$r8 = [Collections.Generic.List[string]]::new()
if (-not [IO.Directory]::Exists($ownerRoot)) { $r8.Add('future owner crate crates/sakura-rerank-proto is pending') }
foreach ($finding in @(Get-RerankOwnershipFindings $r8Files $ownerRoot)) { $r8.Add($finding) }
foreach ($consumer in @('sakura-engine', 'sakura-neural-worker')) {
    if (@(Get-DirectNormalDependency $metadata $consumer 'sakura-rerank-proto').Count -ne 1) {
        $r8.Add("$consumer does not yet have its required normal direct dependency on sakura-rerank-proto")
    }
}
Add-Result 'R8' $(if ($r8.Count) { 'VIOLATION' } else { 'PASS' }) 'reranker protocol must have one owner' @($r8)

$r9 = @(Get-R9Findings $productionRust) + @(Get-OracleDependencyFindings $metadata)
Add-Result 'R9' $(if ($r9.Count) { 'VIOLATION' } else { 'PASS' }) 'test-only source placement' $r9

$r12 = [Collections.Generic.List[string]]::new()
$valuesRoot = Join-Path $repoRoot 'crates/sakura-values/src'
if (-not [IO.Directory]::Exists($valuesRoot)) {
    $r12.Add('crates/sakura-values/src is missing')
}
else {
    $r12Pattern = '(?:\bsakura_proto\b|\bwire::|\bReader\b|\bSink\b|\bfn\s+encode\b|\bfn\s+decode\b)'
    foreach ($hit in @(Invoke-SourceScan 'R12' $valuesRoot $r12Pattern)) { $r12.Add($hit) }
}
$valuesPackage = @($metadata.packages | Where-Object { $_.name -ceq 'sakura-values' })
if ($valuesPackage.Count -ne 1) {
    $r12.Add("expected one sakura-values package, found $($valuesPackage.Count)")
}
else {
    $normalDependencies = @($valuesPackage[0].dependencies | Where-Object { $null -eq $_.kind })
    if ($normalDependencies.Count -ne 0) {
        $r12.Add("sakura-values has normal dependencies: $($normalDependencies.name -join ', ')")
    }
}
Add-Result 'R12' $(if ($r12.Count) { 'VIOLATION' } else { 'PASS' }) 'values must not know wire and must remain a dependency leaf' @($r12)

$r13 = @(Get-R13Findings $metadata (Join-Path $repoRoot 'crates/sakura-store/src'))
Add-Result 'R13' $(if ($r13.Count) { 'VIOLATION' } else { 'PASS' }) 'store must own data/codec without runtime dependencies' $r13

if ($results.Count -ne $allRules.Count) {
    throw "Dependency audit produced $($results.Count) results for $($allRules.Count) rules"
}

$failures = 0
foreach ($result in $results) {
    $isEnforced = $enforced.Contains($result.Rule)
    $label = if ($result.State -ceq 'PASS') { 'PASS' } elseif ($isEnforced) { 'FAIL' } elseif ($result.State -ceq 'PENDING') { 'PENDING' } else { 'WARN' }
    if ($label -ceq 'FAIL') { $failures++ }
    Write-Host "${label}: $($result.Rule) $($result.Summary)"
    foreach ($detail in $result.Details) { Write-Host "  $detail" }
}
if ($failures) { throw "$failures enforced dependency rule(s) failed" }
$advisoryRules = @($allRules | Where-Object { -not $enforced.Contains($_) }) -join ','
Write-Host "PASS: dependency-rule audit completed; enforced=$($requested -join ','); advisory=$advisoryRules"
