[CmdletBinding()]
param(
    [string]$IsccPath,

    [string]$ReportPath = '',

    [string]$DictionaryReportPath = '',

    [switch]$IncludeNeuralReranker
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$setupPath = Join-Path $repositoryRoot 'installer\setup.iss'
$installerPath = Join-Path $repositoryRoot 'installer\out\sakura_setup.exe'
$dictionaryReportPath = if ([string]::IsNullOrWhiteSpace($DictionaryReportPath)) {
    Join-Path $repositoryRoot 'artifacts\release\dictionary-build.report.json'
} else {
    [IO.Path]::GetFullPath($DictionaryReportPath)
}
$neuralBuildScript = Join-Path $repositoryRoot 'scripts\build-neural-reranker.ps1'
$dictionarySourceLockPath = Join-Path $repositoryRoot 'data\SOURCES.lock'
$thirdPartyNoticesPath = Join-Path $repositoryRoot 'THIRD_PARTY_NOTICES.md'
$japaneseWordNetLicensePath = Join-Path $repositoryRoot 'THIRD_PARTY_LICENSES\japanese-wordnet-1.1-NICT.txt'
$japaneseWordNetLicenseRelativePath = 'THIRD_PARTY_LICENSES/japanese-wordnet-1.1-NICT.txt'
$japaneseWordNetSourceLockRelativePath = 'data/SOURCES.lock'
$japaneseWordNetArtifactUrl = 'https://github.com/bond-lab/wnja/releases/download/v1.1/jpn_wn_lmf.xml.gz'
$japaneseWordNetArchiveSha256 = '1ed18d08f6f311ebd05c15344b2ebb4ece6752cccfcfe6f9ecffafd7aa207aa0'
$japaneseWordNetArchiveBytes = 12415268L
$japaneseWordNetRevision = 'v1.1'
$japaneseWordNetLicenseId = 'LicenseRef-Japanese-WordNet-1.1'
$canonicalCategoryDictionaryFiles = @(
    '01_文法・機能語.tsv',
    '02_活用語.tsv',
    '03_一般語.tsv',
    '04_慣用句・定型表現.tsv',
    '05_数値・日付・単位.tsv',
    '06_人名.tsv',
    '07_地名.tsv',
    '08_組織名・製品名.tsv',
    '09_外来語・カタカナ語.tsv',
    '10_略語・英数字.tsv',
    '11_IT・技術用語.tsv',
    '12_専門用語.tsv',
    '13_記号・絵文字.tsv',
    '14_表記ゆれ.tsv'
)
$neuralPayloadPaths = @(
    'artifacts\release\sakura_neural_worker.exe',
    'artifacts\release\onnxruntime.dll',
    'artifacts\release\neural\deberta-v2-tiny-japanese-char-wwm\model.onnx',
    'artifacts\release\neural\deberta-v2-tiny-japanese-char-wwm\vocab.txt',
    'artifacts\release\neural\deberta-v2-tiny-japanese-char-wwm\manifest.json',
    'artifacts\release\licenses\onnxruntime-MIT.txt',
    'artifacts\release\licenses\onnxruntime-ThirdPartyNotices.txt',
    'artifacts\release\licenses\ku-nlp-deberta-v2-tiny-japanese-char-wwm.txt'
)
if ([string]::IsNullOrWhiteSpace($ReportPath)) {
    $ReportPath = Join-Path $repositoryRoot 'installer\out\installer-build.report.json'
}
$ReportPath = [IO.Path]::GetFullPath($ReportPath)

function Get-Sha256 {
    param([Parameter(Mandatory)][string]$Path)

    $stream = [IO.File]::OpenRead($Path)
    try {
        $algorithm = [Security.Cryptography.SHA256]::Create()
        try {
            return ([BitConverter]::ToString($algorithm.ComputeHash($stream))).Replace('-', '').ToLowerInvariant()
        }
        finally {
            $algorithm.Dispose()
        }
    }
    finally {
        $stream.Dispose()
    }
}

function Get-TextSha256 {
    param([Parameter(Mandatory)][string]$Text)

    $algorithm = [Security.Cryptography.SHA256]::Create()
    try {
        return ([BitConverter]::ToString(
            $algorithm.ComputeHash([Text.Encoding]::UTF8.GetBytes($Text))
        )).Replace('-', '').ToLowerInvariant()
    }
    finally {
        $algorithm.Dispose()
    }
}

function Resolve-Iscc {
    if (-not [string]::IsNullOrWhiteSpace($IsccPath)) {
        $resolved = [IO.Path]::GetFullPath($IsccPath)
        if (-not [IO.File]::Exists($resolved)) { throw "ISCC is missing: $resolved" }
        return $resolved
    }

    $command = Get-Command ISCC.exe -ErrorAction SilentlyContinue
    if ($null -ne $command) { return $command.Source }

    $candidates = [Collections.Generic.List[string]]::new()
    if (-not [string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
        $candidates.Add((Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 6\ISCC.exe'))
    }
    if (-not [string]::IsNullOrWhiteSpace($env:USERPROFILE)) {
        $candidates.Add((Join-Path $env:USERPROFILE 'AppData\Local\Programs\Inno Setup 6\ISCC.exe'))
    }
    if (-not [string]::IsNullOrWhiteSpace(${env:ProgramFiles(x86)})) {
        $candidates.Add((Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'))
    }
    if (-not [string]::IsNullOrWhiteSpace($env:ProgramFiles)) {
        $candidates.Add((Join-Path $env:ProgramFiles 'Inno Setup 6\ISCC.exe'))
    }
    foreach ($candidate in $candidates) {
        if ([IO.File]::Exists($candidate)) { return $candidate }
    }
    throw 'Inno Setup 6 compiler (ISCC.exe) was not found'
}

function Get-CanonicalVersion {
    $cargoText = [IO.File]::ReadAllText((Join-Path $repositoryRoot 'Cargo.toml'))
    $workspaceMatch = [regex]::Match(
        $cargoText,
        '(?ms)^\[workspace\.package\]\s*(?<body>.*?)(?=^\[|\z)'
    )
    if (-not $workspaceMatch.Success) { throw 'Cargo.toml has no [workspace.package] section' }
    $cargoVersionMatch = [regex]::Match(
        $workspaceMatch.Groups['body'].Value,
        '(?m)^version\s*=\s*"(?<version>[^"]+)"\s*$'
    )
    if (-not $cargoVersionMatch.Success) { throw 'workspace package version is missing' }
    $version = $cargoVersionMatch.Groups['version'].Value
    if ($version -notmatch '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$') {
        throw "workspace package version is not canonical: $version"
    }

    $setupText = [IO.File]::ReadAllText($setupPath)
    $setupMatches = [regex]::Matches(
        $setupText,
        '(?m)^#define AppProductVersion "(?<version>[^"]+)"$'
    )
    if ($setupMatches.Count -ne 1) { throw 'setup.iss must contain exactly one AppProductVersion' }
    $setupVersion = $setupMatches[0].Groups['version'].Value.Trim()
    if ($setupVersion -cne $version) {
        throw "installer version $setupVersion does not match workspace version $version"
    }
    return $version
}

function Get-ArtifactRecord {
    param([Parameter(Mandatory)][string]$Path)

    $item = [IO.FileInfo]::new($Path)
    if (-not $item.Exists) { throw "release payload is missing: $Path" }
    $repositoryPrefix = $repositoryRoot.TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
    if (-not $item.FullName.StartsWith($repositoryPrefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw "release payload is outside the repository: $($item.FullName)"
    }
    return [ordered]@{
        path = $item.FullName.Substring($repositoryPrefix.Length).Replace('\', '/')
        bytes = $item.Length
        sha256 = Get-Sha256 $item.FullName
    }
}

function Require-TextContains {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$Expected,
        [Parameter(Mandatory)][string]$Description
    )

    if (-not [IO.File]::Exists($Path)) { throw "$Description is missing: $Path" }
    $text = [IO.File]::ReadAllText($Path, [Text.Encoding]::UTF8)
    if ($text.IndexOf($Expected, [StringComparison]::Ordinal) -lt 0) {
        throw "$Description does not contain the required pinned value: $Expected"
    }
}

function Get-RequiredJsonProperty {
    param(
        [Parameter(Mandatory)][object]$Object,
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][string]$Description
    )

    if ($null -eq $Object) { throw "$Description is missing" }
    $property = $Object.PSObject.Properties[$Name]
    if ($null -eq $property) { throw "$Description is missing" }
    return $property.Value
}

$version = Get-CanonicalVersion
$iscc = Resolve-Iscc
$payloadPaths = @(
    'target\x86_64-pc-windows-msvc\release\sakura_tsf.dll',
    'target\x86_64-pc-windows-msvc\release\sakura_engine.exe',
    'target\x86_64-pc-windows-msvc\release\sakura_renderer.exe',
    'target\x86_64-pc-windows-msvc\release\sakura_regtool.exe',
    'target\x86_64-pc-windows-msvc\release\sakura_logon.exe',
    'target\x86_64-pc-windows-msvc\release\sakura_settings.exe',
    'target\x86_64-pc-windows-msvc\release\sakura_settings_payload.exe',
    'artifacts\release\system.dic',
    'LICENSE',
    'README.md',
    'docs\guide-ja.md',
    'THIRD_PARTY_NOTICES.md',
    'THIRD_PARTY_LICENSES\mozc-dictionary.txt',
    'THIRD_PARTY_LICENSES\smile-chat-public-MIT.txt'
)
if ($IncludeNeuralReranker) {
    if (-not [IO.File]::Exists($neuralBuildScript)) {
        throw "neural reranker payload validator is missing: $neuralBuildScript"
    }
    try {
        & $neuralBuildScript -OutputDirectory (Join-Path $repositoryRoot 'artifacts\release') -ValidateOnly
    }
    catch {
        throw "neural reranker payload validation failed; refusing declared neural installer build: $($_.Exception.Message)"
    }
    $payloadPaths += $neuralPayloadPaths
}
if (-not [IO.File]::Exists($dictionaryReportPath)) {
    throw "dictionary provenance report is missing: $dictionaryReportPath"
}
$dictionaryReportRecord = Get-ArtifactRecord $dictionaryReportPath
$dictionaryReport = [IO.File]::ReadAllText($dictionaryReportPath) | ConvertFrom-Json
if ($dictionaryReport.schema_version -notin @(1, 2) -or $dictionaryReport.deterministic_repeat -ne $true) {
    throw 'dictionary provenance does not prove a deterministic repeat build'
}
$dictionaryArtifacts = Get-RequiredJsonProperty $dictionaryReport 'artifacts' 'dictionary artifacts'
$categoryArtifacts = @(
    Get-RequiredJsonProperty $dictionaryArtifacts 'category_dictionaries' 'canonical category dictionaries'
)
$reportedCategoryFiles = @(
    foreach ($category in $categoryArtifacts) {
        [string](Get-RequiredJsonProperty $category 'file' 'canonical category dictionary file')
    }
)
$missingCategoryFiles = @($canonicalCategoryDictionaryFiles | Where-Object { $_ -notin $reportedCategoryFiles })
$unexpectedCategoryFiles = @($reportedCategoryFiles | Where-Object { $_ -notin $canonicalCategoryDictionaryFiles })
if ($categoryArtifacts.Count -ne $canonicalCategoryDictionaryFiles.Count -or
    @($reportedCategoryFiles | Select-Object -Unique).Count -ne $canonicalCategoryDictionaryFiles.Count -or
    $missingCategoryFiles.Count -ne 0 -or $unexpectedCategoryFiles.Count -ne 0) {
    throw 'dictionary provenance does not prove that the canonical fourteen Sakura system categories were included'
}
$includesJapaneseWordNet = $false
if ($dictionaryReport.schema_version -eq 2) {
    $details = Get-RequiredJsonProperty $dictionaryReport 'details' 'dictionary detail provenance'
    $detailsSchemaVersion = Get-RequiredJsonProperty $details 'schema_version' 'dictionary detail schema version'
    $detailsSource = Get-RequiredJsonProperty $details 'source' 'dictionary detail source'
    $fullDefinitionMaxBytes = Get-RequiredJsonProperty $details 'full_definition_max_bytes' 'dictionary detail full-definition limit'
    $detailsCount = Get-RequiredJsonProperty $details 'count' 'dictionary detail count'
    if ($detailsSchemaVersion -ne 1 -or $detailsSource -cne 'japanese-wordnet' -or
        $null -ne $fullDefinitionMaxBytes -or $null -eq $detailsCount -or [long]$detailsCount -lt 0) {
        throw 'dictionary detail provenance is missing or invalid'
    }
    $sources = Get-RequiredJsonProperty $dictionaryReport 'sources' 'dictionary source provenance'
    $source = Get-RequiredJsonProperty $sources 'japanese_wordnet' 'Japanese WordNet source provenance'
    $sourceId = Get-RequiredJsonProperty $source 'id' 'Japanese WordNet source id'
    $sourceRevision = Get-RequiredJsonProperty $source 'revision' 'Japanese WordNet source revision'
    $sourceArtifactUrl = Get-RequiredJsonProperty $source 'artifact_url' 'Japanese WordNet source artifact URL'
    $sourceArchiveSha256 = Get-RequiredJsonProperty $source 'archive_sha256' 'Japanese WordNet source archive SHA-256'
    $sourceArchiveBytes = Get-RequiredJsonProperty $source 'archive_bytes' 'Japanese WordNet source archive bytes'
    $sourceLicenseId = Get-RequiredJsonProperty $source 'license_id' 'Japanese WordNet source license id'
    $sourceLicenseFile = Get-RequiredJsonProperty $source 'license_file' 'Japanese WordNet source license file'
    if ($sourceId -cne 'japanese-wordnet' -or $sourceRevision -cne $japaneseWordNetRevision -or
        $sourceArtifactUrl -cne $japaneseWordNetArtifactUrl -or
        $sourceArchiveSha256 -cne $japaneseWordNetArchiveSha256 -or
        $null -eq $sourceArchiveBytes -or [long]$sourceArchiveBytes -ne $japaneseWordNetArchiveBytes -or
        $sourceLicenseId -cne $japaneseWordNetLicenseId -or
        $sourceLicenseFile -cne $japaneseWordNetLicenseRelativePath) {
        throw 'Japanese WordNet provenance is missing, unpinned, or inconsistent'
    }
    $import = Get-RequiredJsonProperty $dictionaryReport 'wordnet_import' 'Japanese WordNet import accounting'
    $importSchemaVersion = Get-RequiredJsonProperty $import 'schema_version' 'Japanese WordNet import schema version'
    $importDetailCount = Get-RequiredJsonProperty $import 'detail_count' 'Japanese WordNet import detail count'
    $unresolved = Get-RequiredJsonProperty $import 'unresolved' 'Japanese WordNet unresolved accounting'
    $surfaceAmbiguous = Get-RequiredJsonProperty $unresolved 'surface_ambiguous' 'Japanese WordNet surface ambiguity count'
    $senseAmbiguous = Get-RequiredJsonProperty $unresolved 'sense_ambiguous' 'Japanese WordNet sense ambiguity count'
    $missingDefinition = Get-RequiredJsonProperty $unresolved 'missing_definition' 'Japanese WordNet missing definition count'
    $relationAmbiguous = Get-RequiredJsonProperty $unresolved 'relation_ambiguous' 'Japanese WordNet relation ambiguity count'
    $relationUnsupported = Get-RequiredJsonProperty $unresolved 'relation_unsupported' 'Japanese WordNet unsupported relation count'
    $relationTruncated = Get-RequiredJsonProperty $unresolved 'relation_truncated' 'Japanese WordNet truncated relation count'
    $mergedDetails = Get-RequiredJsonProperty $import 'details' 'merged dictionary detail accounting'
    $mergedDetailCount = Get-RequiredJsonProperty $mergedDetails 'merged_count' 'merged dictionary detail count'
    $detailSources = Get-RequiredJsonProperty $mergedDetails 'sources' 'dictionary detail source accounting'
    $wordNetDetailSource = Get-RequiredJsonProperty $detailSources 'japanese-wordnet' 'Japanese WordNet detail source accounting'
    $smileChatDetailSource = Get-RequiredJsonProperty $detailSources 'smile-chat' 'smile-chat detail source accounting'
    $wordNetSourceCount = Get-RequiredJsonProperty $wordNetDetailSource 'detail_count' 'Japanese WordNet source detail count'
    $smileChatSourceCount = Get-RequiredJsonProperty $smileChatDetailSource 'detail_count' 'smile-chat source detail count'
    $curatedImport = Get-RequiredJsonProperty $dictionaryReport 'curated_detail_import' 'curated detail import accounting'
    $curatedSchemaVersion = Get-RequiredJsonProperty $curatedImport 'schema_version' 'curated detail schema version'
    $curatedInputRecords = Get-RequiredJsonProperty $curatedImport 'input_records' 'curated detail input count'
    $curatedEmittedDetails = Get-RequiredJsonProperty $curatedImport 'emitted_details' 'curated detail emitted count'
    $curatedSuppressed = Get-RequiredJsonProperty $curatedImport 'suppressed_by_existing' 'curated detail suppression count'
    $llmImport = Get-RequiredJsonProperty $dictionaryReport 'llm_detail_import' 'LLM detail import accounting'
    $llmImportReport = Get-RequiredJsonProperty $llmImport 'report' 'LLM detail import report'
    $llmEmittedDetails = Get-RequiredJsonProperty $llmImportReport 'emitted_details' 'LLM detail emitted count'
    $expectedDetailCount = [long]$mergedDetailCount + [long]$curatedEmittedDetails + [long]$llmEmittedDetails
    if ($importSchemaVersion -ne 2 -or $null -eq $importDetailCount -or
        [long]$importDetailCount -ne [long]$wordNetSourceCount -or
        $null -eq $mergedDetailCount -or $curatedSchemaVersion -cne 'sakura.curated-detail-import.v1' -or
        [long]$curatedInputRecords -ne ([long]$curatedEmittedDetails + [long]$curatedSuppressed) -or
        $expectedDetailCount -ne [long]$detailsCount -or
        $null -eq $smileChatSourceCount -or [long]$smileChatSourceCount -lt 0 -or
        $null -eq $surfaceAmbiguous -or [long]$surfaceAmbiguous -lt 0 -or
        $null -eq $senseAmbiguous -or [long]$senseAmbiguous -lt 0 -or
        $null -eq $missingDefinition -or [long]$missingDefinition -lt 0 -or
        $null -eq $relationAmbiguous -or [long]$relationAmbiguous -lt 0 -or
        $null -eq $relationUnsupported -or [long]$relationUnsupported -lt 0 -or
        $null -eq $relationTruncated -or [long]$relationTruncated -lt 0) {
        throw 'Japanese WordNet import accounting is missing or invalid'
    }
    Require-TextContains -Path $dictionarySourceLockPath -Expected '[japanese_wordnet]' -Description 'Japanese WordNet source lock'
    Require-TextContains -Path $dictionarySourceLockPath -Expected $japaneseWordNetArtifactUrl -Description 'Japanese WordNet source lock'
    Require-TextContains -Path $dictionarySourceLockPath -Expected $japaneseWordNetArchiveSha256 -Description 'Japanese WordNet source lock'
    Require-TextContains -Path $dictionarySourceLockPath -Expected $japaneseWordNetLicenseRelativePath -Description 'Japanese WordNet source lock'
    Require-TextContains -Path $thirdPartyNoticesPath -Expected $japaneseWordNetLicenseRelativePath -Description 'Japanese WordNet third-party notice'
    if (-not [IO.File]::Exists($japaneseWordNetLicensePath)) {
        throw "Japanese WordNet license file is missing: $japaneseWordNetLicensePath"
    }
    $payloadPaths += @(
        $japaneseWordNetLicenseRelativePath.Replace('/', '\'),
        $japaneseWordNetSourceLockRelativePath.Replace('/', '\')
    )
    $includesJapaneseWordNet = $true
}

$payloads = [Collections.Generic.List[object]]::new()
foreach ($relativePath in $payloadPaths) {
    $payloads.Add((Get-ArtifactRecord (Join-Path $repositoryRoot $relativePath)))
}
$dictionaryRecord = $payloads | Where-Object { $_.path -ceq 'artifacts/release/system.dic' }
if ($null -eq $dictionaryReport.artifacts.dictionary -or
    $dictionaryReport.artifacts.dictionary.sha256 -cne $dictionaryRecord.sha256 -or
    [long]$dictionaryReport.artifacts.dictionary.bytes -ne [long]$dictionaryRecord.bytes) {
    throw 'packaged dictionary does not match its deterministic build report'
}

$fingerprintLines = [Collections.Generic.List[string]]::new()
foreach ($payload in $payloads) {
    $fingerprintLines.Add("$($payload.path)|$($payload.bytes)|$($payload.sha256)")
}
$fingerprintLines.Add("dictionary-provenance|$($dictionaryReportRecord.bytes)|$($dictionaryReportRecord.sha256)")
$buildIdInput = $version + "`n" + ($fingerprintLines -join "`n") + "`n"
$buildId = (Get-TextSha256 -Text $buildIdInput).Substring(0, 16)

$buildStarted = [DateTime]::UtcNow
$startInfo = [Diagnostics.ProcessStartInfo]::new()
$startInfo.FileName = $iscc
$isccArguments = [Collections.Generic.List[string]]::new()
$isccArguments.Add("/dAppBuildId=$buildId")
$isccArguments.Add("/dAppVersionedDir={app}\versions\$version-$buildId")
if ($IncludeNeuralReranker) {
    $isccArguments.Add('/dIncludeNeuralReranker=1')
}
if ($includesJapaneseWordNet) {
    $isccArguments.Add('/dIncludeJapaneseWordNet=1')
}
$isccArguments.Add(('"' + $setupPath.Replace('"', '\"') + '"'))
# ProcessStartInfo.ArgumentList is unavailable in Windows PowerShell's .NET
# Framework. All values above are internally generated and the only path is
# quoted, so the compatible Arguments form preserves exact ISCC semantics.
$startInfo.Arguments = $isccArguments -join ' '
$startInfo.WorkingDirectory = Join-Path $repositoryRoot 'installer'
$startInfo.UseShellExecute = $false
$startInfo.CreateNoWindow = $true
$startInfo.RedirectStandardOutput = $true
$startInfo.RedirectStandardError = $true
$process = [Diagnostics.Process]::new()
$process.StartInfo = $startInfo
try {
    if (-not $process.Start()) { throw 'ISCC did not start' }
    $stdoutTask = $process.StandardOutput.ReadToEndAsync()
    $stderrTask = $process.StandardError.ReadToEndAsync()
    $process.WaitForExit()
    $stdout = $stdoutTask.GetAwaiter().GetResult()
    $stderr = $stderrTask.GetAwaiter().GetResult()
    $exitCode = $process.ExitCode
}
finally {
    $process.Dispose()
}
$compilerOutput = ($stdout + $stderr).Replace("`r`n", "`n")
Write-Host $compilerOutput.TrimEnd()
if ($exitCode -ne 0) { throw "ISCC failed with exit code $exitCode" }
if ($compilerOutput -match '(?m)^Warning:') { throw 'ISCC emitted a warning; installer build fails closed' }
if ($compilerOutput -notmatch '(?m)^Successful compile') {
    throw 'ISCC returned success without its explicit successful terminal message'
}
$compilerVersionMatches = [regex]::Matches(
    $compilerOutput,
    'Compiler engine version:\s*(?:Inno Setup\s+)?(?<version>[0-9]+(?:\.[0-9]+){1,3})'
)
if ($compilerVersionMatches.Count -ne 1) {
    throw "ISCC reported $($compilerVersionMatches.Count) recognizable compiler-version lines; exactly one is required"
}
$compilerVersion = [version]$compilerVersionMatches[0].Groups['version'].Value
if ($compilerVersion -lt [version]'6.3.0') {
    throw "Inno Setup $compilerVersion is too old for x64compatible packaging"
}

$installer = [IO.FileInfo]::new($installerPath)
if (-not $installer.Exists -or $installer.Length -le 0) { throw 'installer output is missing or empty' }
if ($installer.LastWriteTimeUtc -lt $buildStarted.AddSeconds(-2)) {
    throw 'installer output was not recreated by this build'
}
$fileVersion = [Diagnostics.FileVersionInfo]::GetVersionInfo($installer.FullName)
if ($fileVersion.ProductVersion -notlike "$version*") {
    throw "installer product version '$($fileVersion.ProductVersion)' does not match $version"
}

$report = [ordered]@{
    schema_version = 1
    completed_utc = [DateTime]::UtcNow.ToString('O')
    version = $version
    build_id = $buildId
    compiler = [ordered]@{
        path = $iscc
        version = $compilerVersion.ToString()
        output_sha256 = Get-TextSha256 $compilerOutput
        warnings = 0
    }
    # Retain the scalar for older release-bundle verification while the complete
    # record is the build-id input and exposes the exact artifact in this report.
    dictionary_provenance_sha256 = $dictionaryReportRecord.sha256
    dictionary_provenance = $dictionaryReportRecord
    dictionary_details = [ordered]@{
        included = $includesJapaneseWordNet
        source = if ($includesJapaneseWordNet) { 'japanese-wordnet' } else { $null }
        provenance = if ($includesJapaneseWordNet) {
            [ordered]@{
                source_lock = Get-ArtifactRecord $dictionarySourceLockPath
                notice = Get-ArtifactRecord $thirdPartyNoticesPath
                license = Get-ArtifactRecord $japaneseWordNetLicensePath
            }
        } else {
            $null
        }
    }
    neural_reranker = [ordered]@{
        included = [bool]$IncludeNeuralReranker
        manifest = if ($IncludeNeuralReranker) {
            Get-ArtifactRecord (Join-Path $repositoryRoot 'artifacts\release\neural\deberta-v2-tiny-japanese-char-wwm\manifest.json')
        } else {
            $null
        }
    }
    payloads = @($payloads)
    installer = Get-ArtifactRecord $installer.FullName
}
$reportDirectory = [IO.Path]::GetDirectoryName($ReportPath)
[IO.Directory]::CreateDirectory($reportDirectory) | Out-Null
$temporaryReport = Join-Path $reportDirectory ('.installer-build.' + [guid]::NewGuid().ToString('N') + '.tmp')
$backupReport = Join-Path $reportDirectory ('.installer-build.' + [guid]::NewGuid().ToString('N') + '.bak')
try {
    [IO.File]::WriteAllText(
        $temporaryReport,
        (($report | ConvertTo-Json -Depth 6) + [Environment]::NewLine),
        [Text.UTF8Encoding]::new($false)
    )
    if ([IO.File]::Exists($ReportPath)) {
        [IO.File]::Replace($temporaryReport, $ReportPath, $backupReport)
    }
    else {
        [IO.File]::Move($temporaryReport, $ReportPath)
    }
}
finally {
    if ([IO.File]::Exists($temporaryReport)) { [IO.File]::Delete($temporaryReport) }
    if ([IO.File]::Exists($backupReport)) { [IO.File]::Delete($backupReport) }
}

Write-Host "installer built and audited: $installerPath"
Write-Host "audit report: $ReportPath"
