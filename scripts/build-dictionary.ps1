[CmdletBinding()]
param(
    [string]$MozcSource,
    [string]$GlossarySource,
    [string]$WordNetLmf,
    # Canonical Sakura system dictionary split into fourteen category files.
    # The source stays outside the repository and is included in the generated
    # image when explicitly supplied. Keep the old parameter as a compatibility
    # alias for existing local build commands.
    [Alias('SupplementLexiconDirectory')]
    [string]$SystemCategoryDirectory,
    [string]$OutputDirectory = (Join-Path $env:USERPROFILE 'tmp\sakura-input-dictionary-build'),
    [switch]$SkipDeterminismCheck,
    [switch]$UpdateCheckedInData
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$MozcRepository = 'google/mozc'
$MozcRevision = '3f235b4eb6fcff7d14ef5f0fb8ee56de7ee4c732'
$GlossaryRepository = 'systemexe-research-and-development/smile-chat'
$GlossaryRevision = 'b5cada441b41c207ab49bf2cd5f1d9c5614c5b92'
$WordNetRevision = 'v1.1'
$WordNetArtifactUrl = 'https://github.com/bond-lab/wnja/releases/download/v1.1/jpn_wn_lmf.xml.gz'
$WordNetArtifactBytes = 12415268L
$WordNetArtifactSha256 = '1ed18d08f6f311ebd05c15344b2ebb4ece6752cccfcfe6f9ecffafd7aa207aa0'

$RepositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
$CuratedTerms = Join-Path $RepositoryRoot 'data\curated-terms.tsv'
$CuratedPhrases = Join-Path $RepositoryRoot 'data\curated-phrases.tsv'
$CuratedGeneralDetails = Join-Path $RepositoryRoot 'data\curated-general-details.tsv'
$CuratedPhraseTargetEntries = Join-Path $RepositoryRoot 'data\curated-phrase-target-entries.tsv'
$CuratedGeneralTargetEntries = Join-Path $RepositoryRoot 'data\curated-general-target-entries.tsv'
$ConversionPriorities = Join-Path $RepositoryRoot 'data\conversion-priorities.tsv'
$LlmDetailTargetDirectory = Join-Path $RepositoryRoot 'data\llm-detail-targets\000010'
$LlmDetailReleaseDirectory = Join-Path $RepositoryRoot 'data\llm-details\releases\000010'
$ExpectedSystemCategoryFiles = @(
    '01-grammar-function.tsv',
    '02-inflectional.tsv',
    '03-general-lexicon.tsv',
    '04-fixed-expressions.tsv',
    '05-numeric-time-units.tsv',
    '06-person-names.tsv',
    '07-place-names.tsv',
    '08-organizations-products.tsv',
    '09-katakana-loanwords.tsv',
    '10-abbreviations-ascii.tsv',
    '11-it-engineering.tsv',
    '12-specialist-domains.tsv',
    '13-symbols-emoji.tsv',
    '14-orthography-variants.tsv'
)
$CategoryDictionaryFiles = @(
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
[IO.Directory]::CreateDirectory($OutputDirectory) | Out-Null

if (-not [IO.File]::Exists($CuratedTerms)) {
    throw "curated dictionary layer is missing: $CuratedTerms"
}
if (-not [IO.File]::Exists($ConversionPriorities)) {
    throw "conversion-priority dictionary layer is missing: $ConversionPriorities"
}
foreach ($path in @($CuratedPhrases, $CuratedGeneralDetails, $CuratedPhraseTargetEntries, $CuratedGeneralTargetEntries)) {
    if (-not [IO.File]::Exists($path)) {
        throw "curated dictionary layer is missing: $path"
    }
}
foreach ($path in @(
    (Join-Path $LlmDetailTargetDirectory 'manifest.json'),
    (Join-Path $LlmDetailReleaseDirectory 'manifest.json')
)) {
    if (-not [IO.File]::Exists($path)) {
        throw "reviewed LLM detail release contract is missing: $path"
    }
}

function Invoke-Rtk {
    param([Parameter(Mandatory)][string[]]$Arguments)

    $rtk = Get-Command rtk -ErrorAction SilentlyContinue
    if ($null -ne $rtk) {
        $output = @(& $rtk.Source @Arguments)
        $displayName = 'rtk'
    }
    else {
        $program = $Arguments[0]
        $programArguments = @($Arguments | Select-Object -Skip 1)
        $output = @(& $program @programArguments)
        $displayName = $program
    }
    if ($LASTEXITCODE -ne 0) {
        throw "$displayName $($Arguments -join ' ') failed with exit code $LASTEXITCODE"
    }
    foreach ($line in $output) {
        Write-Host $line
    }
}

function Get-RepositoryHead {
    param([Parameter(Mandatory)][string]$Path)

    $rtk = Get-Command rtk -ErrorAction SilentlyContinue
    if ($null -ne $rtk) {
        $output = @(& $rtk.Source git -C $Path rev-parse HEAD)
    }
    else {
        $output = @(& git -C $Path rev-parse HEAD)
    }
    if ($LASTEXITCODE -ne 0) {
        throw "cannot read Git revision for $Path"
    }
    return ($output[-1]).Trim()
}

function Resolve-PinnedSource {
    param(
        [string]$ProvidedPath,
        [Parameter(Mandatory)][string]$Repository,
        [Parameter(Mandatory)][string]$Revision,
        [Parameter(Mandatory)][string]$ManagedName,
        [Parameter(Mandatory)][string[]]$SparsePaths
    )

    if ($ProvidedPath) {
        $resolved = [IO.Path]::GetFullPath($ProvidedPath)
        if (-not [IO.Directory]::Exists($resolved)) {
            throw "source directory does not exist: $resolved"
        }
        $pinnedRevisionFile = Join-Path $resolved 'PINNED_REVISION'
        if ([IO.File]::Exists($pinnedRevisionFile)) {
            $pinned = ([IO.File]::ReadAllText($pinnedRevisionFile)).Trim()
            if ($pinned -cne $Revision) {
                throw "$resolved PINNED_REVISION '$pinned' does not match required revision $Revision"
            }
            return $resolved
        }
        if ((Get-RepositoryHead $resolved) -ne $Revision) {
            throw "$resolved is not checked out at pinned revision $Revision"
        }
        return $resolved
    }

    $sourceRoot = [IO.Path]::GetFullPath((Join-Path $env:USERPROFILE 'tmp\sakura-input-dictionary-sources'))
    [IO.Directory]::CreateDirectory($sourceRoot) | Out-Null
    $resolved = [IO.Path]::GetFullPath((Join-Path $sourceRoot $ManagedName))
    $expectedPrefix = $sourceRoot.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
    if (-not $resolved.StartsWith($expectedPrefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw "managed source escaped ~/tmp: $resolved"
    }

    if (-not [IO.Directory]::Exists($resolved)) {
        Invoke-Rtk -Arguments @('gh', 'repo', 'clone', $Repository, $resolved, '--', '--filter=blob:none', '--no-checkout')
        Invoke-Rtk -Arguments @('git', '-C', $resolved, 'sparse-checkout', 'init', '--cone')
        Invoke-Rtk -Arguments (@('git', '-C', $resolved, 'sparse-checkout', 'set') + $SparsePaths)
        Invoke-Rtk -Arguments @('git', '-C', $resolved, 'checkout', '--detach', $Revision)
    }
    else {
        # Re-assert the sparse profile so checkouts provisioned before a new
        # pinned path was added materialize it. Blobs come from the same
        # pinned revision, so build inputs stay deterministic.
        Invoke-Rtk -Arguments (@('git', '-C', $resolved, 'sparse-checkout', 'set') + $SparsePaths)
    }

    $rtk = Get-Command rtk -ErrorAction SilentlyContinue
    if ($null -ne $rtk) {
        $dirty = @(& $rtk.Source git -C $resolved status --porcelain)
    }
    else {
        $dirty = @(& git -C $resolved status --porcelain)
    }
    if ($LASTEXITCODE -ne 0) {
        throw "cannot inspect managed source $resolved"
    }
    if ($dirty.Count -ne 0) {
        throw "managed source has local changes: $resolved"
    }
    if ((Get-RepositoryHead $resolved) -ne $Revision) {
        throw "managed source is not at pinned revision $Revision; remove or repair $resolved"
    }
    return $resolved
}

function Get-Sha256 {
    param([Parameter(Mandatory)][string]$Path)

    $stream = [IO.File]::OpenRead($Path)
    try {
        $algorithm = [Security.Cryptography.SHA256]::Create()
        try {
            $bytes = $algorithm.ComputeHash($stream)
            return (([BitConverter]::ToString($bytes)).Replace('-', '')).ToLowerInvariant()
        }
        finally {
            $algorithm.Dispose()
        }
    }
    finally {
        $stream.Dispose()
    }
}

function Get-ArtifactRecord {
    param([Parameter(Mandatory)][string]$Path)

    $item = [IO.FileInfo]::new($Path)
    if (-not $item.Exists) {
        throw "expected artifact was not produced: $Path"
    }
    return [ordered]@{
        file = $item.Name
        bytes = $item.Length
        sha256 = Get-Sha256 $item.FullName
    }
}

function Resolve-WordNetLmf {
    param([AllowEmptyString()][string]$ProvidedPath)

    if ([string]::IsNullOrWhiteSpace($ProvidedPath)) {
        $cacheDirectory = Join-Path $env:USERPROFILE 'tmp\sakura-input-dictionary-sources\japanese-wordnet-v1.1'
        [IO.Directory]::CreateDirectory($cacheDirectory) | Out-Null
        $ProvidedPath = Join-Path $cacheDirectory 'jpn_wn_lmf.xml.gz'
        if (-not [IO.File]::Exists($ProvidedPath)) {
            Invoke-WebRequest -UseBasicParsing -Uri $WordNetArtifactUrl -OutFile $ProvidedPath
        }
    }
    $resolved = [IO.Path]::GetFullPath($ProvidedPath)
    $file = [IO.FileInfo]::new($resolved)
    if (-not $file.Exists) { throw "Japanese WordNet LMF archive is missing: $resolved" }
    if ($file.Length -ne $WordNetArtifactBytes) {
        throw "Japanese WordNet LMF archive size mismatch: expected $WordNetArtifactBytes, got $($file.Length)"
    }
    $actualHash = Get-Sha256 $resolved
    if ($actualHash -ne $WordNetArtifactSha256) {
        throw "Japanese WordNet LMF archive SHA-256 mismatch: expected $WordNetArtifactSha256, got $actualHash"
    }
    return $resolved
}

function Resolve-SystemCategoryDictionary {
    param([AllowEmptyString()][string]$Directory)

    if ([string]::IsNullOrWhiteSpace($Directory)) {
        return $null
    }

    $resolved = [IO.Path]::GetFullPath($Directory)
    if (-not [IO.Directory]::Exists($resolved)) {
        throw "system category dictionary directory does not exist: $resolved"
    }
    $manifestPath = Join-Path $resolved 'manifest.json'
    if (-not [IO.File]::Exists($manifestPath)) {
        throw "system category dictionary manifest is missing: $manifestPath"
    }
    try {
        $manifest = [IO.File]::ReadAllText($manifestPath, [Text.UTF8Encoding]::new($false)) |
            ConvertFrom-Json
    }
    catch {
        throw "cannot parse system category dictionary manifest ${manifestPath}: $($_.Exception.Message)"
    }

    if ($manifest.schema_version -ne 1) {
        throw "unsupported system category dictionary manifest schema: $($manifest.schema_version)"
    }
    if ($manifest.license_declaration -ne 'LicenseRef-ATOK36-LGPL') {
        throw 'system category dictionary manifest is not compatible with this build'
    }
    if ([string]$manifest.source_scope -notlike '*user dictionaries excluded*') {
        throw 'system category dictionary manifest does not prove that user dictionaries were excluded'
    }
    [long]$uniquePairs = 0
    if (-not [long]::TryParse([string]$manifest.unique_safely_mapped_pairs, [ref]$uniquePairs) -or $uniquePairs -lt 1200000) {
        throw "system category dictionary has fewer than 1,200,000 safely mapped pairs: $($manifest.unique_safely_mapped_pairs)"
    }

    $categories = @($manifest.categories)
    if ($categories.Count -ne $ExpectedSystemCategoryFiles.Count) {
        throw "system category dictionary manifest must contain exactly $($ExpectedSystemCategoryFiles.Count) categories, found $($categories.Count)"
    }
    $byFile = @{}
    foreach ($category in $categories) {
        $file = [string]$category.file
        if ([string]::IsNullOrWhiteSpace($file) -or $byFile.ContainsKey($file)) {
            throw "system category dictionary manifest has a missing or duplicate category file: $file"
        }
        $byFile[$file] = $category
    }

    $categoryPaths = [Collections.Generic.List[string]]::new()
    $directoryPrefix = if ($resolved.EndsWith([IO.Path]::DirectorySeparatorChar)) {
        $resolved
    }
    else {
        $resolved + [IO.Path]::DirectorySeparatorChar
    }
    for ($index = 0; $index -lt $ExpectedSystemCategoryFiles.Count; $index++) {
        $file = $ExpectedSystemCategoryFiles[$index]
        if (-not $byFile.ContainsKey($file)) {
            throw "system category dictionary manifest is missing category file: $file"
        }
        if ($byFile[$file].id -ne ($index + 1)) {
            throw "system category file $file does not match its required id"
        }
        $path = [IO.Path]::GetFullPath((Join-Path $resolved $file))
        if (-not $path.StartsWith($directoryPrefix, [StringComparison]::OrdinalIgnoreCase)) {
            throw "system category file escaped its declared dictionary directory: $path"
        }
        if (-not [IO.File]::Exists($path)) {
            throw "system category file is missing: $path"
        }
        $categoryPaths.Add($path)
    }

    return [pscustomobject]@{
        manifest = $manifestPath
        paths = $categoryPaths.ToArray()
    }
}

function Invoke-BuildPass {
    param(
        [Parameter(Mandatory)][AllowEmptyString()][string]$Suffix,
        [Parameter(Mandatory)][string]$MozcDictionaryDirectory,
        [Parameter(Mandatory)][string]$GlossaryDirectory,
        [Parameter(Mandatory)][string]$ConnectionPath,
        [Parameter(Mandatory)][string]$MozcPosPath,
        [Parameter(Mandatory)][string]$MozcSegmenterPath,
        [Parameter(Mandatory)][string]$CuratedTermsPath,
        [Parameter(Mandatory)][string]$CuratedPhrasesPath,
        [Parameter(Mandatory)][string]$CuratedGeneralDetailsPath,
        [Parameter(Mandatory)][string]$CuratedPhraseTargetEntriesPath,
        [Parameter(Mandatory)][string]$CuratedGeneralTargetEntriesPath,
        [Parameter(Mandatory)][string]$ConversionPrioritiesPath,
        [Parameter(Mandatory)][string]$WordNetLmfPath,
        [string[]]$SystemCategoryPaths = @()
    )

    $options = [IO.EnumerationOptions]::new()
    $options.RecurseSubdirectories = $false
    $options.IgnoreInaccessible = $false
    $options.AttributesToSkip = 0
    [string[]]$shards = [IO.Directory]::EnumerateFiles(
        $MozcDictionaryDirectory,
        'dictionary??.txt',
        $options
    )
    [Array]::Sort($shards, [StringComparer]::Ordinal)
    if ($shards.Count -ne 10) {
        throw "expected 10 Mozc dictionary shards, found $($shards.Count)"
    }

    $systemTsv = Join-Path $OutputDirectory "mozc-system$Suffix.tsv"
    $trimReport = Join-Path $OutputDirectory "mozc-trim$Suffix.report.json"
    $overlayTsv = Join-Path $OutputDirectory "it-terms$Suffix.tsv"
    $overlayReport = Join-Path $OutputDirectory "it-terms$Suffix.report.json"
    $inflectionTsv = Join-Path $OutputDirectory "inflection$Suffix.tsv"
    $inflectionReport = Join-Path $OutputDirectory "inflection$Suffix.report.json"
    $categoryDirectory = Join-Path $OutputDirectory "カテゴリ辞書$Suffix"
    $dictionary = Join-Path $OutputDirectory "system$Suffix.dic"
    $wordNetReport = Join-Path $OutputDirectory "wordnet$Suffix.report.json"
    $curatedDetailReport = Join-Path $OutputDirectory "curated-details$Suffix.report.json"
    $llmDetailReport = Join-Path $OutputDirectory "llm-details$Suffix.report.json"
    $detailCoverage = Join-Path $OutputDirectory "detail-coverage$Suffix.tsv"

    $mozcArguments = @('cargo', 'run', '--locked', '-p', 'dictc', '--bin', 'mozc-trim', '--')
    foreach ($shard in $shards) {
        $mozcArguments += @('--mozc-system', $shard)
    }
    $mozcArguments += @('--output', $systemTsv, '--report', $trimReport)
    Invoke-Rtk -Arguments $mozcArguments

    $inflectionArguments = @(
        'cargo', 'run', '--locked', '-p', 'dictc', '--bin', 'inflection-expand', '--',
        '--system', $systemTsv,
        '--mozc-id-def', $MozcPosPath,
        '--output', $inflectionTsv,
        '--report', $inflectionReport
    )
    Invoke-Rtk -Arguments $inflectionArguments

    $glossaryArguments = @(
        'cargo', 'run', '--locked', '-p', 'dictc', '--bin', 'glossary-import', '--',
        '--glossary-dir', $GlossaryDirectory,
        '--glossary-revision', $GlossaryRevision
    )
    foreach ($shard in $shards) {
        $glossaryArguments += @('--mozc-system', $shard)
    }
    $glossaryArguments += @('--output', $overlayTsv, '--report', $overlayReport)
    Invoke-Rtk -Arguments $glossaryArguments

    $categoryArguments = @(
        'cargo', 'run', '--locked', '-p', 'dictc', '--bin', 'category-split', '--',
        '--mozc-pos', $MozcPosPath,
        '--system', $systemTsv,
        '--overlay', $overlayTsv,
        '--overlay', $CuratedTermsPath,
        '--overlay', $inflectionTsv,
        '--overlay', $CuratedGeneralDetailsPath,
        '--overlay', $CuratedGeneralTargetEntriesPath,
        '--overlay', $ConversionPrioritiesPath,
        '--system-category', '4', $CuratedPhrasesPath,
        '--system-category', '4', $CuratedPhraseTargetEntriesPath,
        '--output-dir', $categoryDirectory
    )
    # PowerShell unwraps an empty array passed through a parameter, so the
    # no-category build can arrive here as `$null` even though the parameter
    # is declared as `string[]`. Normalize it before reading Count or indexing.
    $systemCategoryPathArray = @(
        foreach ($path in @($SystemCategoryPaths)) {
            if (-not [string]::IsNullOrWhiteSpace([string]$path)) {
                [string]$path
            }
        }
    )
    for ($index = 0; $index -lt $systemCategoryPathArray.Count; $index++) {
        $categoryArguments += @('--system-category', ($index + 1), $systemCategoryPathArray[$index])
    }
    Invoke-Rtk -Arguments $categoryArguments

    $categoryFiles = [Collections.Generic.List[string]]::new()
    foreach ($file in $CategoryDictionaryFiles) {
        $path = Join-Path $categoryDirectory $file
        if (-not [IO.File]::Exists($path)) {
            throw "category dictionary was not produced: $path"
        }
        $categoryFiles.Add($path)
    }

    $dictionaryArguments = @(
        'cargo', 'run', '--locked', '-p', 'dictc', '--bin', 'dictc', '--'
    )
    foreach ($category in $categoryFiles) {
        $dictionaryArguments += @('--category', $category)
    }
    $dictionaryArguments += @(
        '--mozc-connection', $ConnectionPath,
        '--mozc-id-def', $MozcPosPath,
        '--mozc-segmenter', $MozcSegmenterPath,
        '--glossary-dir', $GlossaryDirectory,
        '--wordnet-lmf', $WordNetLmfPath,
        '--wordnet-report', $wordNetReport,
        '--curated-detail-source', $CuratedPhrasesPath,
        '--curated-detail-source', $CuratedGeneralDetailsPath,
        '--curated-detail-report', $curatedDetailReport,
        '--detail-coverage-output', $detailCoverage,
        '--llm-detail-target-dir', $LlmDetailTargetDirectory,
        '--llm-detail-release-dir', $LlmDetailReleaseDirectory,
        '--llm-detail-report', $llmDetailReport,
        '--output', $dictionary
    )
    Invoke-Rtk -Arguments $dictionaryArguments

    return [ordered]@{
        system_tsv = $systemTsv
        trim_report = $trimReport
        overlay_tsv = $overlayTsv
        overlay_report = $overlayReport
        inflection_tsv = $inflectionTsv
        inflection_report = $inflectionReport
        category_directory = $categoryDirectory
        category_files = $categoryFiles.ToArray()
        dictionary = $dictionary
        wordnet_report = $wordNetReport
        curated_detail_report = $curatedDetailReport
        llm_detail_report = $llmDetailReport
        detail_coverage = $detailCoverage
    }
}

function Remove-BuildArtifact {
    param([Parameter(Mandatory)][string]$Path)

    $resolved = [IO.Path]::GetFullPath($Path)
    $expectedPrefix = $OutputDirectory.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
    if (-not $resolved.StartsWith($expectedPrefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw "refusing to remove artifact outside output directory: $resolved"
    }
    [IO.File]::Delete($resolved)
}

function Remove-BuildDirectory {
    param([Parameter(Mandatory)][string]$Path)

    $resolved = [IO.Path]::GetFullPath($Path)
    $expectedPrefix = $OutputDirectory.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
    if (-not $resolved.StartsWith($expectedPrefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw "refusing to remove directory outside output directory: $resolved"
    }
    if ([IO.Directory]::Exists($resolved)) {
        [IO.Directory]::Delete($resolved, $true)
    }
}

$stopwatch = [Diagnostics.Stopwatch]::StartNew()
$MozcSource = Resolve-PinnedSource -ProvidedPath $MozcSource -Repository $MozcRepository `
    -Revision $MozcRevision -ManagedName 'mozc' `
    -SparsePaths @('src/data/dictionary_oss', 'src/data/rules')
if ([string]::IsNullOrWhiteSpace($GlossarySource)) {
    # Prefer the MIT glossary snapshot vendored in-tree so CI and local builds
    # do not need a private-repository token for smile-chat.
    $vendoredGlossary = Join-Path $RepositoryRoot 'third_party\smile-chat-public'
    if ([IO.Directory]::Exists($vendoredGlossary)) {
        $GlossarySource = $vendoredGlossary
    }
}
$GlossarySource = Resolve-PinnedSource -ProvidedPath $GlossarySource -Repository $GlossaryRepository `
    -Revision $GlossaryRevision -ManagedName 'smile-chat' -SparsePaths @('frontend/public')
$SystemCategoryDictionary = Resolve-SystemCategoryDictionary -Directory $SystemCategoryDirectory
$WordNetLmfPath = Resolve-WordNetLmf -ProvidedPath $WordNetLmf
if ($UpdateCheckedInData -and $null -ne $SystemCategoryDictionary) {
    throw '-UpdateCheckedInData cannot be combined with -SystemCategoryDirectory; canonical category source files remain local only'
}
$SystemCategoryPaths = if ($null -eq $SystemCategoryDictionary) { @() } else { [string[]]$SystemCategoryDictionary.paths }

$mozcDictionaryDirectory = Join-Path $MozcSource 'src\data\dictionary_oss'
$glossaryDirectory = Join-Path $GlossarySource 'frontend\public\glossaries'
$connectionPath = Join-Path $mozcDictionaryDirectory 'connection_single_column.txt'
$mozcPosPath = Join-Path $mozcDictionaryDirectory 'id.def'
$mozcSegmenterPath = Join-Path $MozcSource 'src\data\rules\segmenter.def'
$requiredLicenseFiles = @(
    (Join-Path $MozcSource 'LICENSE'),
    (Join-Path $mozcDictionaryDirectory 'README.txt'),
    (Join-Path $GlossarySource 'frontend\public\LICENSE'),
    (Join-Path $RepositoryRoot 'THIRD_PARTY_LICENSES\japanese-wordnet-1.1-NICT.txt')
)
foreach ($path in $requiredLicenseFiles) {
    if (-not [IO.File]::Exists($path)) {
        throw "required upstream license file is missing: $path"
    }
}
if (-not [IO.File]::Exists($mozcPosPath)) {
    throw "Mozc POS taxonomy is missing: $mozcPosPath"
}
if (-not [IO.File]::Exists($mozcSegmenterPath)) {
    throw "Mozc segmenter rules are missing: $mozcSegmenterPath"
}

$env:CARGO_HTTP_CHECK_REVOKE = 'false'
Push-Location $RepositoryRoot
try {
    $primary = Invoke-BuildPass -Suffix '' -MozcDictionaryDirectory $mozcDictionaryDirectory `
        -GlossaryDirectory $glossaryDirectory -ConnectionPath $connectionPath `
        -MozcPosPath $mozcPosPath -MozcSegmenterPath $mozcSegmenterPath -CuratedTermsPath $CuratedTerms `
        -CuratedPhrasesPath $CuratedPhrases `
        -CuratedGeneralDetailsPath $CuratedGeneralDetails `
        -CuratedPhraseTargetEntriesPath $CuratedPhraseTargetEntries `
        -CuratedGeneralTargetEntriesPath $CuratedGeneralTargetEntries `
        -ConversionPrioritiesPath $ConversionPriorities -WordNetLmfPath $WordNetLmfPath -SystemCategoryPaths $SystemCategoryPaths

    $scalarArtifactNames = @('system_tsv', 'trim_report', 'overlay_tsv', 'overlay_report', 'inflection_tsv', 'inflection_report', 'dictionary', 'wordnet_report', 'curated_detail_report', 'llm_detail_report', 'detail_coverage')
    if (-not $SkipDeterminismCheck) {
        $repeat = Invoke-BuildPass -Suffix '.repeat' -MozcDictionaryDirectory $mozcDictionaryDirectory `
            -GlossaryDirectory $glossaryDirectory -ConnectionPath $connectionPath `
            -MozcPosPath $mozcPosPath -MozcSegmenterPath $mozcSegmenterPath -CuratedTermsPath $CuratedTerms `
            -CuratedPhrasesPath $CuratedPhrases `
            -CuratedGeneralDetailsPath $CuratedGeneralDetails `
            -CuratedPhraseTargetEntriesPath $CuratedPhraseTargetEntries `
            -CuratedGeneralTargetEntriesPath $CuratedGeneralTargetEntries `
            -ConversionPrioritiesPath $ConversionPriorities -WordNetLmfPath $WordNetLmfPath -SystemCategoryPaths $SystemCategoryPaths
        foreach ($name in $scalarArtifactNames) {
            $firstHash = Get-Sha256 $primary[$name]
            $secondHash = Get-Sha256 $repeat[$name]
            if ($firstHash -ne $secondHash) {
                throw "non-deterministic $name output: $firstHash != $secondHash"
            }
        }
        for ($index = 0; $index -lt $primary.category_files.Count; $index++) {
            $firstHash = Get-Sha256 $primary.category_files[$index]
            $secondHash = Get-Sha256 $repeat.category_files[$index]
            if ($firstHash -ne $secondHash) {
                throw "non-deterministic category dictionary $($CategoryDictionaryFiles[$index]): $firstHash != $secondHash"
            }
        }
        foreach ($name in $scalarArtifactNames) {
            Remove-BuildArtifact $repeat[$name]
        }
        Remove-BuildDirectory $repeat.category_directory
    }

    $wordNetImport = Get-Content -Raw -LiteralPath $primary.wordnet_report | ConvertFrom-Json
    $curatedDetailImport = Get-Content -Raw -LiteralPath $primary.curated_detail_report | ConvertFrom-Json
    $llmDetailImport = Get-Content -Raw -LiteralPath $primary.llm_detail_report | ConvertFrom-Json
    $report = [ordered]@{
        schema_version = 2
        mozc_revision = $MozcRevision
        glossary_revision = $GlossaryRevision
        sources = [ordered]@{
            japanese_wordnet = [ordered]@{
                id = 'japanese-wordnet'
                revision = $WordNetRevision
                artifact_url = $WordNetArtifactUrl
                archive_sha256 = $WordNetArtifactSha256
                archive_bytes = $WordNetArtifactBytes
                license_id = 'LicenseRef-Japanese-WordNet-1.1'
                license_file = 'THIRD_PARTY_LICENSES/japanese-wordnet-1.1-NICT.txt'
            }
        }
        details = [ordered]@{
            schema_version = 1
            # Backward-compatible primary source field. `sources` records all
            # detail provenance after the smile-chat + WordNet exact merge.
            source = 'japanese-wordnet'
            full_definition_max_bytes = $null
            count = [int64]$wordNetImport.details.merged_count +
                [int64]$curatedDetailImport.emitted_details +
                [int64]$llmDetailImport.report.emitted_details
            sources = @($wordNetImport.details.sources) + @(
                [ordered]@{
                    source = 'sakura-curated-entry-details'
                    input_records = [int64]$curatedDetailImport.input_records
                    emitted_details = [int64]$curatedDetailImport.emitted_details
                    suppressed_by_existing = [int64]$curatedDetailImport.suppressed_by_existing
                    inputs = @(
                        Get-ArtifactRecord $CuratedPhrases
                        Get-ArtifactRecord $CuratedGeneralDetails
                    )
                },
                [ordered]@{
                    source = 'sakura-llm-reviewed-details'
                    validated_unique_terms = [int64]$llmDetailImport.report.validated_unique_terms
                    emitted_details = [int64]$llmDetailImport.report.emitted_details
                    target_manifest = Get-ArtifactRecord (Join-Path $LlmDetailTargetDirectory 'manifest.json')
                    release_manifest = Get-ArtifactRecord (Join-Path $LlmDetailReleaseDirectory 'manifest.json')
                }
            )
        }
        wordnet_import = $wordNetImport
        curated_detail_import = $curatedDetailImport
        llm_detail_import = $llmDetailImport
        deterministic_repeat = -not $SkipDeterminismCheck
        inputs = [ordered]@{
            curated_terms = Get-ArtifactRecord $CuratedTerms
            curated_phrases = Get-ArtifactRecord $CuratedPhrases
            curated_general_details = Get-ArtifactRecord $CuratedGeneralDetails
            curated_phrase_target_entries = Get-ArtifactRecord $CuratedPhraseTargetEntries
            curated_general_target_entries = Get-ArtifactRecord $CuratedGeneralTargetEntries
            conversion_priorities = Get-ArtifactRecord $ConversionPriorities
            system_category_dictionary = if ($null -eq $SystemCategoryDictionary) {
                $null
            }
            else {
                [ordered]@{
                    manifest = Get-ArtifactRecord $SystemCategoryDictionary.manifest
                    categories = @(
                        foreach ($path in $SystemCategoryDictionary.paths) {
                            Get-ArtifactRecord $path
                        }
                    )
                }
            }
        }
        artifacts = [ordered]@{}
    }
    foreach ($name in $scalarArtifactNames) {
        $report.artifacts[$name] = Get-ArtifactRecord $primary[$name]
    }
    $report.artifacts.category_dictionaries = @(
        foreach ($path in $primary.category_files) {
            Get-ArtifactRecord $path
        }
    )
    $buildReport = Join-Path $OutputDirectory 'dictionary-build.report.json'
    [IO.File]::WriteAllText(
        $buildReport,
        (($report | ConvertTo-Json -Depth 7) + [Environment]::NewLine),
        [Text.UTF8Encoding]::new($false)
    )

    if ($UpdateCheckedInData) {
        [IO.File]::Copy($primary.overlay_tsv, (Join-Path $RepositoryRoot 'data\it-terms.tsv'), $true)
        [IO.File]::Copy($primary.overlay_report, (Join-Path $RepositoryRoot 'data\it-terms.report.json'), $true)
        [IO.File]::Copy($primary.trim_report, (Join-Path $RepositoryRoot 'data\mozc-trim.report.json'), $true)
        [IO.File]::Copy($buildReport, (Join-Path $RepositoryRoot 'data\dictionary-build.report.json'), $true)
    }
}
finally {
    Pop-Location
    $stopwatch.Stop()
}

Write-Host ("dictionary pipeline completed in {0:N2}s; output: {1}" -f $stopwatch.Elapsed.TotalSeconds, $OutputDirectory)
