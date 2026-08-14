[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$SourceModel,
    [Parameter(Mandatory)][string]$SourceResearchManifest,
    [Parameter(Mandatory)][string]$WorkerExe,
    [Parameter(Mandatory)][string]$RuntimeDll,
    [Parameter(Mandatory)][string]$RuntimeLicense,
    [Parameter(Mandatory)][string]$RuntimeNotices,
    [Parameter(Mandatory)][string]$ModelLicense,
    [Parameter(Mandatory)][string]$OutputDirectory
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$expectedResearchManifestSha256 = '07f1c54cbe361e117b547f47511de960977f1d0f754f051f44b9447a591d96b9'
$expectedModelSha256 = 'b3fe1e0aa7229edfd0760162d648f10328b0d75224a9cd49f2ba986b7db2ccbd'
$expectedModelBytes = 7466707L

function Resolve-RequiredFile {
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][string]$Name)
    $resolved = [IO.Path]::GetFullPath($Path)
    if (-not [IO.File]::Exists($resolved)) {
        throw "$Name is missing: $resolved"
    }
    return $resolved
}

function Get-Sha256 {
    param([Parameter(Mandatory)][string]$Path)
    $stream = [IO.File]::OpenRead($Path)
    try {
        $algorithm = [Security.Cryptography.SHA256]::Create()
        try {
            return ([BitConverter]::ToString($algorithm.ComputeHash($stream))).Replace('-', '').ToLowerInvariant()
        } finally {
            $algorithm.Dispose()
        }
    } finally {
        $stream.Dispose()
    }
}

$model = Resolve-RequiredFile $SourceModel 'Sakura model'
$researchManifestPath = Resolve-RequiredFile $SourceResearchManifest 'Sakura research manifest'
$worker = Resolve-RequiredFile $WorkerExe 'Sakura neural worker'
$runtime = Resolve-RequiredFile $RuntimeDll 'ONNX Runtime DLL'
$runtimeLicense = Resolve-RequiredFile $RuntimeLicense 'ONNX Runtime license'
$runtimeNotices = Resolve-RequiredFile $RuntimeNotices 'ONNX Runtime third-party notices'
$modelLicense = Resolve-RequiredFile $ModelLicense 'Sakura model license'
$output = [IO.Path]::GetFullPath($OutputDirectory)

if ((Get-Sha256 $researchManifestPath) -ne $expectedResearchManifestSha256) {
    throw 'Sakura research manifest hash does not match the reviewed prototype'
}
$research = [Text.Encoding]::UTF8.GetString([IO.File]::ReadAllBytes($researchManifestPath)) | ConvertFrom-Json
if ($research.schema_version -ne 1 -or
    $research.manifest_kind -ne 'sakura_rerank_tiny_model' -or
    $research.model_contract_version -ne 1 -or
    $research.model_name -ne 'Sakura-Rerank-Tiny-v1-research-prototype' -or
    $research.status -ne 'research_only_gate_a_failed' -or
    $research.data.gate_a_status -ne 'gate_a_failed' -or
    $research.data.final_holdout_used -ne $false -or
    $research.distribution_authorized -ne $false -or
    $research.license_status -ne 'not_selected_no_distribution_authorized' -or
    $research.exports.fp32.sha256 -ne $expectedModelSha256 -or
    $research.exports.fp32.bytes -ne $expectedModelBytes) {
    throw 'Sakura research manifest does not match the admitted research-only contract'
}
if ([IO.FileInfo]::new($model).Length -ne $expectedModelBytes -or (Get-Sha256 $model) -ne $expectedModelSha256) {
    throw 'Sakura model bytes do not match the admitted FP32 export'
}
if ([IO.Directory]::Exists($output) -or [IO.File]::Exists($output)) {
    throw "staging output already exists: $output"
}

$modelDirectory = Join-Path $output 'neural\sakura-rerank-tiny-v1'
$licenseDirectory = Join-Path $output 'licenses'
[IO.Directory]::CreateDirectory($modelDirectory) | Out-Null
[IO.Directory]::CreateDirectory($licenseDirectory) | Out-Null
[IO.File]::Copy($worker, (Join-Path $output 'sakura_neural_worker.exe'), $false)
[IO.File]::Copy($runtime, (Join-Path $output 'onnxruntime.dll'), $false)
[IO.File]::Copy($model, (Join-Path $modelDirectory 'model.onnx'), $false)
[IO.File]::Copy($runtimeLicense, (Join-Path $licenseDirectory 'onnxruntime-MIT.txt'), $false)
[IO.File]::Copy($runtimeNotices, (Join-Path $licenseDirectory 'onnxruntime-ThirdPartyNotices.txt'), $false)
[IO.File]::Copy($modelLicense, (Join-Path $licenseDirectory 'sakura-rerank-tiny-v1-MIT.txt'), $false)

$runtimeManifest = [ordered]@{
    schema_version = 1
    manifest_kind = 'sakura_rerank_runtime_model'
    status = 'release_experimental_gate_a_failed'
    model = [ordered]@{
        id = 'Sakura-Rerank-Tiny-v1-research-prototype'
        contract_version = 1
        format = 'onnx-fp32'
        opset = 18
    }
    runtime = [ordered]@{
        name = 'onnxruntime'
        version = '1.28.0'
    }
    research = [ordered]@{
        source_manifest_sha256 = $expectedResearchManifestSha256
        gate_a_status = 'gate_a_failed'
        final_holdout_used = $false
        artifact_distribution_authorized = $true
        license = 'MIT'
    }
    files = @([ordered]@{
        path = 'model.onnx'
        bytes = $expectedModelBytes
        sha256 = $expectedModelSha256
    })
    raw_text_in_manifest = $false
    raw_stable_ids_in_manifest = $false
}
$manifestJson = ($runtimeManifest | ConvertTo-Json -Depth 6 -Compress) + "`n"
[IO.File]::WriteAllText(
    (Join-Path $modelDirectory 'manifest.json'),
    $manifestJson,
    [Text.UTF8Encoding]::new($false)
)

$payloadFiles = @(
    'sakura_neural_worker.exe',
    'onnxruntime.dll',
    'neural\sakura-rerank-tiny-v1\model.onnx',
    'neural\sakura-rerank-tiny-v1\manifest.json',
    'licenses\onnxruntime-MIT.txt',
    'licenses\onnxruntime-ThirdPartyNotices.txt',
    'licenses\sakura-rerank-tiny-v1-MIT.txt'
)
$installerLines = [Collections.Generic.List[string]]::new()
$installerLines.Add('; Generated by scripts/stage-sakura-rerank.ps1. Do not edit.')
$installerLines.Add("#define NeuralPayloadCount $($payloadFiles.Count)")
for ($index = 0; $index -lt $payloadFiles.Count; $index++) {
    $relative = $payloadFiles[$index]
    $path = Join-Path $output $relative
    $item = [IO.FileInfo]::new($path)
    if (-not $item.Exists -or $item.Length -le 0) {
        throw "staged neural payload is missing or empty: $relative"
    }
    $installerLines.Add("#define NeuralPayload$($index)Path `"$relative`"")
    $installerLines.Add("#define NeuralPayload$($index)Bytes $($item.Length)")
    $installerLines.Add("#define NeuralPayload$($index)Sha256 `"$(Get-Sha256 $path)`"")
}
$installerLines.Add('#define NeuralModelLicense "MIT"')
$installerLines.Add('#define NeuralOnnxRuntimeVersion "1.28.0"')
[IO.File]::WriteAllText(
    (Join-Path $modelDirectory 'manifest.iss'),
    (($installerLines -join [Environment]::NewLine) + [Environment]::NewLine),
    [Text.UTF8Encoding]::new($false)
)

[ordered]@{
    status = 'staged-release-experimental'
    output = $output
    model_sha256 = $expectedModelSha256
    distribution_authorized = $true
    license = 'MIT'
    gate_a_status = 'gate_a_failed'
} | ConvertTo-Json -Compress
