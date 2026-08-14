[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$SourceModel,
    [Parameter(Mandatory)][string]$SourceResearchManifest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$expectedModelSha256 = 'b3fe1e0aa7229edfd0760162d648f10328b0d75224a9cd49f2ba986b7db2ccbd'
$expectedModelBytes = 7466707L
$expectedResearchManifestSha256 = '07f1c54cbe361e117b547f47511de960977f1d0f754f051f44b9447a591d96b9'
$expectedResearchManifestBytes = 2866L

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

function Resolve-ExpectedFile {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][long]$Bytes,
        [Parameter(Mandatory)][string]$Sha256,
        [Parameter(Mandatory)][string]$Description
    )
    $resolved = [IO.Path]::GetFullPath($Path)
    $item = [IO.FileInfo]::new($resolved)
    if (-not $item.Exists -or $item.Length -ne $Bytes -or (Get-Sha256 $resolved) -cne $Sha256) {
        throw "$Description does not match its reviewed bytes: $resolved"
    }
    return $resolved
}

$model = Resolve-ExpectedFile $SourceModel $expectedModelBytes $expectedModelSha256 'Sakura FP32 model'
$researchManifest = Resolve-ExpectedFile $SourceResearchManifest $expectedResearchManifestBytes $expectedResearchManifestSha256 'Sakura research manifest'
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$destination = Join-Path $repositoryRoot 'models\sakura-rerank-tiny-v1'
$modelDestination = Join-Path $destination 'model.onnx'
$manifestDestination = Join-Path $destination 'research-manifest.json'

if ([IO.File]::Exists($modelDestination) -or [IO.File]::Exists($manifestDestination)) {
    throw "release model destination already exists; refusing to overwrite reviewed bytes: $destination"
}
[IO.Directory]::CreateDirectory($destination) | Out-Null
[IO.File]::Copy($model, $modelDestination, $false)
[IO.File]::Copy($researchManifest, $manifestDestination, $false)

[ordered]@{
    status = 'imported'
    model = [ordered]@{ path = $modelDestination; bytes = $expectedModelBytes; sha256 = $expectedModelSha256 }
    research_manifest = [ordered]@{ path = $manifestDestination; bytes = $expectedResearchManifestBytes; sha256 = $expectedResearchManifestSha256 }
} | ConvertTo-Json -Depth 3 -Compress
