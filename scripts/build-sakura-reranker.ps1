[CmdletBinding()]
param(
    [string]$SourceModel = '',
    [string]$SourceResearchManifest = '',
    [string]$WorkerExe = '',
    [string]$OutputDirectory = '',
    [string]$WorkDirectory = ''
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
if ([string]::IsNullOrWhiteSpace($SourceModel)) {
    $SourceModel = Join-Path $repositoryRoot 'models\sakura-rerank-tiny-v1\model.onnx'
}
if ([string]::IsNullOrWhiteSpace($SourceResearchManifest)) {
    $SourceResearchManifest = Join-Path $repositoryRoot 'models\sakura-rerank-tiny-v1\research-manifest.json'
}
if ([string]::IsNullOrWhiteSpace($WorkerExe)) {
    $WorkerExe = Join-Path $repositoryRoot 'target\x86_64-pc-windows-msvc\release\sakura_neural_worker.exe'
}
if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
    $OutputDirectory = Join-Path $repositoryRoot 'artifacts\release\neural-payload'
}
if ([string]::IsNullOrWhiteSpace($WorkDirectory)) {
    if ([string]::IsNullOrWhiteSpace($env:USERPROFILE)) {
        throw 'USERPROFILE is required to place the task-specific ONNX Runtime cache under ~/tmp'
    }
    $WorkDirectory = Join-Path $env:USERPROFILE 'tmp\sakura-input-sakura-reranker'
}

$sourceModel = [IO.Path]::GetFullPath($SourceModel)
$sourceResearchManifest = [IO.Path]::GetFullPath($SourceResearchManifest)
$workerExe = [IO.Path]::GetFullPath($WorkerExe)
$outputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
$workDirectory = [IO.Path]::GetFullPath($WorkDirectory)
$stageScript = Join-Path $PSScriptRoot 'stage-sakura-rerank.ps1'

# ONNX Runtime comes from Microsoft's official NuGet package. The GitHub
# release zip used before (#484) stopped resolving in 2026-11; the DLL,
# LICENSE and ThirdPartyNotices.txt inside this package are byte-identical to
# the ones that zip carried, so the shipped payload does not change. Both the
# package and the extracted DLL are pinned.
$ortVersion = '1.28.0'
$ortPackageSha256 = '769d1d3ea8ab6cd69f737c9dd4d4462aa4ad0ccfa106eaf506efc40d7bead5db'
$ortPackageUrl = "https://api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime/$ortVersion/microsoft.ml.onnxruntime.$ortVersion.nupkg"
$ortRuntimeDllSha256 = '18370c375f07357fa5874344a9d9ac17e6b6fe1eb18b1dd209d79483b4470257'
$ortPackageEntries = [ordered]@{
    'runtimes/win-x64/native/onnxruntime.dll' = 'onnxruntime.dll'
    'LICENSE' = 'LICENSE'
    'ThirdPartyNotices.txt' = 'ThirdPartyNotices.txt'
}

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

function Require-File {
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][string]$Description)
    $item = [IO.FileInfo]::new($Path)
    if (-not $item.Exists -or $item.Length -le 0) {
        throw "$Description is missing or empty: $Path"
    }
    return $item.FullName
}

Require-File $sourceModel 'tracked Sakura FP32 model' | Out-Null
Require-File $sourceResearchManifest 'tracked Sakura research manifest' | Out-Null
Require-File $workerExe 'release Sakura neural worker' | Out-Null
Require-File $stageScript 'Sakura reranker staging script' | Out-Null
[IO.Directory]::CreateDirectory($workDirectory) | Out-Null

$package = Join-Path $workDirectory "microsoft.ml.onnxruntime.$ortVersion.nupkg"
if (-not [IO.File]::Exists($package) -or (Get-Sha256 $package) -cne $ortPackageSha256) {
    Add-Type -AssemblyName System.Net.Http
    $client = [Net.Http.HttpClient]::new()
    try {
        $bytes = $client.GetByteArrayAsync($ortPackageUrl).GetAwaiter().GetResult()
        [IO.File]::WriteAllBytes($package, $bytes)
    }
    finally {
        $client.Dispose()
    }
}
if ((Get-Sha256 $package) -cne $ortPackageSha256) {
    throw 'downloaded ONNX Runtime package does not match the pinned SHA-256'
}

$ortParent = Join-Path $workDirectory 'onnxruntime-nuget'
$ortRoot = Join-Path $ortParent "microsoft.ml.onnxruntime-$ortVersion"
$runtimeDll = Join-Path $ortRoot 'onnxruntime.dll'
if (-not [IO.File]::Exists($runtimeDll) -or (Get-Sha256 $runtimeDll) -cne $ortRuntimeDllSha256) {
    if ([IO.Directory]::Exists($ortParent)) {
        $resolvedParent = [IO.Path]::GetFullPath($ortParent)
        $resolvedWork = $workDirectory.TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
        if (-not $resolvedParent.StartsWith($resolvedWork, [StringComparison]::OrdinalIgnoreCase)) {
            throw "refusing to replace ONNX Runtime cache outside the task work directory: $resolvedParent"
        }
        [IO.Directory]::Delete($resolvedParent, $true)
    }
    [IO.Directory]::CreateDirectory($ortRoot) | Out-Null
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [IO.Compression.ZipFile]::OpenRead($package)
    try {
        foreach ($entryName in $ortPackageEntries.Keys) {
            $entry = $zip.GetEntry($entryName)
            if ($null -eq $entry) {
                throw "ONNX Runtime package lacks $entryName"
            }
            [IO.Compression.ZipFileExtensions]::ExtractToFile($entry, (Join-Path $ortRoot $ortPackageEntries[$entryName]), $false)
        }
    }
    finally {
        $zip.Dispose()
    }
}

$runtimeDll = Require-File $runtimeDll 'ONNX Runtime DLL'
if ((Get-Sha256 $runtimeDll) -cne $ortRuntimeDllSha256) {
    throw 'extracted ONNX Runtime DLL does not match the pinned SHA-256'
}
$runtimeLicense = Require-File (Join-Path $ortRoot 'LICENSE') 'ONNX Runtime license'
$runtimeNotices = Require-File (Join-Path $ortRoot 'ThirdPartyNotices.txt') 'ONNX Runtime third-party notices'
$modelLicense = Require-File (Join-Path $repositoryRoot 'LICENSE') 'Sakura model MIT license'

$outputParent = [IO.Path]::GetDirectoryName($outputDirectory)
[IO.Directory]::CreateDirectory($outputParent) | Out-Null
$stageDirectory = Join-Path $outputParent ('.sakura-reranker-stage-' + [guid]::NewGuid().ToString('N'))
$backupDirectory = Join-Path $outputParent ('.sakura-reranker-backup-' + [guid]::NewGuid().ToString('N'))
try {
    & $stageScript `
        -SourceModel $sourceModel `
        -SourceResearchManifest $sourceResearchManifest `
        -WorkerExe $workerExe `
        -RuntimeDll $runtimeDll `
        -RuntimeLicense $runtimeLicense `
        -RuntimeNotices $runtimeNotices `
        -ModelLicense $modelLicense `
        -OutputDirectory $stageDirectory

    if ([IO.Directory]::Exists($outputDirectory)) {
        [IO.Directory]::Move($outputDirectory, $backupDirectory)
    }
    try {
        [IO.Directory]::Move($stageDirectory, $outputDirectory)
    }
    catch {
        if ([IO.Directory]::Exists($backupDirectory) -and -not [IO.Directory]::Exists($outputDirectory)) {
            [IO.Directory]::Move($backupDirectory, $outputDirectory)
        }
        throw
    }
    if ([IO.Directory]::Exists($backupDirectory)) {
        [IO.Directory]::Delete($backupDirectory, $true)
    }
}
finally {
    if ([IO.Directory]::Exists($stageDirectory)) {
        [IO.Directory]::Delete($stageDirectory, $true)
    }
}

Write-Host "Sakura reranker release payload staged: $outputDirectory"
