#Requires -Version 5.1
[CmdletBinding()]
param(
    [Parameter()]
    [string]$DllPath = '',

    [switch]$SelfTest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

[long]$MaximumDllBytes = 1048576

function Assert-DllSize {
    param(
        [Parameter(Mandatory)]
        [ValidateNotNullOrEmpty()]
        [string]$Path,

        [Parameter(Mandatory)]
        [ValidateRange(0, [long]::MaxValue)]
        [long]$MaximumBytes
    )

    $resolvedPath = [IO.Path]::GetFullPath($Path)
    if (-not [IO.File]::Exists($resolvedPath)) {
        throw "TSF DLL is missing: $resolvedPath"
    }

    $length = [IO.FileInfo]::new($resolvedPath).Length
    if ($length -gt $MaximumBytes) {
        throw "TSF DLL is $length bytes; maximum is $MaximumBytes bytes: $resolvedPath"
    }

    return [pscustomobject]@{
        Path = $resolvedPath
        Length = $length
        MaximumBytes = $MaximumBytes
    }
}

function Assert-Rejected {
    param(
        [Parameter(Mandatory)]
        [string]$Name,

        [Parameter(Mandatory)]
        [scriptblock]$Check,

        [Parameter(Mandatory)]
        [string]$ExpectedError
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

function New-SizedFile {
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [Parameter(Mandatory)]
        [long]$Length
    )

    $stream = [IO.File]::Open($Path, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try {
        $stream.SetLength($Length)
    }
    finally {
        $stream.Dispose()
    }
}

function Invoke-SelfTest {
    $temporaryRoot = [IO.Path]::GetFullPath((Join-Path ([Environment]::GetFolderPath('UserProfile')) 'tmp'))
    do {
        $testRoot = Join-Path $temporaryRoot ('sakura-input-dll-size-' + [guid]::NewGuid().ToString('N'))
    } while ([IO.Directory]::Exists($testRoot) -or [IO.File]::Exists($testRoot))

    [IO.Directory]::CreateDirectory($testRoot) | Out-Null
    try {
        $boundaryPath = Join-Path $testRoot 'boundary.dll'
        New-SizedFile -Path $boundaryPath -Length $MaximumDllBytes
        $boundary = Assert-DllSize -Path $boundaryPath -MaximumBytes $MaximumDllBytes
        if ($boundary.Length -ne $MaximumDllBytes) {
            throw "self-test boundary returned $($boundary.Length) bytes instead of $MaximumDllBytes"
        }

        $oversizePath = Join-Path $testRoot 'oversize.dll'
        New-SizedFile -Path $oversizePath -Length ($MaximumDllBytes + 1)
        Assert-Rejected -Name 'one byte over the limit' -ExpectedError "TSF DLL is $($MaximumDllBytes + 1) bytes; maximum is $MaximumDllBytes bytes:*" -Check {
            $null = Assert-DllSize -Path $oversizePath -MaximumBytes $MaximumDllBytes
        }

        $missingPath = Join-Path $testRoot 'missing.dll'
        Assert-Rejected -Name 'missing DLL' -ExpectedError 'TSF DLL is missing:*' -Check {
            $null = Assert-DllSize -Path $missingPath -MaximumBytes $MaximumDllBytes
        }
    }
    finally {
        if ([IO.Directory]::Exists($testRoot)) {
            foreach ($fixture in @('boundary.dll', 'oversize.dll')) {
                [IO.File]::Delete((Join-Path $testRoot $fixture))
            }
            [IO.Directory]::Delete($testRoot, $false)
        }
    }

    Write-Output 'PASS: TSF DLL size gate self-test (boundary, oversized, and missing)'
}

if ($SelfTest) {
    Invoke-SelfTest
    exit 0
}

if ([string]::IsNullOrWhiteSpace($DllPath)) {
    $repository = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
    $metadataJson = & cargo metadata --no-deps --format-version 1 --locked --manifest-path (Join-Path $repository 'Cargo.toml')
    if ($LASTEXITCODE -ne 0) { throw "cargo metadata failed with exit code $LASTEXITCODE" }
    $metadata = ($metadataJson -join "`n") | ConvertFrom-Json
    $buildTarget = $env:CARGO_BUILD_TARGET
    if ([string]::IsNullOrWhiteSpace($buildTarget)) {
        $configuration = [IO.File]::ReadAllText((Join-Path $repository '.cargo/config.toml'))
        $targetMatch = [regex]::Match($configuration, '(?m)^target\s*=\s*"([^"]+)"\s*$')
        if (-not $targetMatch.Success) { throw 'could not resolve the configured Cargo build target; supply -DllPath' }
        $buildTarget = $targetMatch.Groups[1].Value
    }
    $DllPath = Join-Path $metadata.target_directory "$buildTarget/release/sakura_tsf.dll"
}
$result = Assert-DllSize -Path $DllPath -MaximumBytes $MaximumDllBytes
Write-Output "PASS: TSF DLL is $($result.Length) bytes (maximum $($result.MaximumBytes)): $($result.Path)"
