[CmdletBinding()]
param(
    [string]$TargetDirectory = (Join-Path $PSScriptRoot '..\target\simd-assembly-gate')
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repository = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$targetDirectory = [IO.Path]::GetFullPath($TargetDirectory)
$oldTargetDirectory = $env:CARGO_TARGET_DIR

function Get-NewestAssembly {
    param([Parameter(Mandatory)][string]$DependenciesDirectory)

    if (-not [IO.Directory]::Exists($DependenciesDirectory)) {
        throw "assembly dependency directory is missing: $DependenciesDirectory"
    }

    $newest = $null
    foreach ($path in [IO.Directory]::EnumerateFiles(
        $DependenciesDirectory,
        'sakura_core-*.s',
        [IO.SearchOption]::TopDirectoryOnly
    )) {
        $candidate = [IO.FileInfo]::new($path)
        if ($null -eq $newest -or $candidate.LastWriteTimeUtc -gt $newest.LastWriteTimeUtc) {
            $newest = $candidate
        }
    }
    if ($null -eq $newest) {
        throw "no sakura-core assembly artifact was emitted under $DependenciesDirectory"
    }
    return $newest.FullName
}

function Get-FunctionBody {
    param(
        [Parameter(Mandatory)][string]$Assembly,
        [Parameter(Mandatory)][string]$Name
    )

    $escapedName = [regex]::Escape($Name)
    $matches = [regex]::Matches(
        $Assembly,
        "(?m)^(?<symbol>_ZN[^`r`n]*$escapedName[^`r`n]*):`r?`n"
    )
    if ($matches.Count -ne 1) {
        throw "expected exactly one emitted symbol for $Name, found $($matches.Count)"
    }

    $match = $matches[0]
    $definitionPattern = [regex]::new('(?m)^\s*\.def\s+')
    $nextDefinition = $definitionPattern.Match($Assembly, $match.Index + $match.Length)
    $end = if ($nextDefinition.Success) { $nextDefinition.Index } else { $Assembly.Length }
    return [pscustomobject]@{
        name = $Name
        symbol = $match.Groups['symbol'].Value
        body = $Assembly.Substring($match.Index, $end - $match.Index)
    }
}

function Assert-Contains {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][string]$Body,
        [Parameter(Mandatory)][string]$Pattern,
        [Parameter(Mandatory)][string]$Expectation
    )

    if (-not [regex]::IsMatch($Body, $Pattern, [Text.RegularExpressions.RegexOptions]::IgnoreCase)) {
        throw "$Name is missing $Expectation (/$Pattern/)"
    }
}

function Assert-NotContains {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][string]$Body,
        [Parameter(Mandatory)][string]$Pattern,
        [Parameter(Mandatory)][string]$Expectation
    )

    if ([regex]::IsMatch($Body, $Pattern, [Text.RegularExpressions.RegexOptions]::IgnoreCase)) {
        throw "$Name unexpectedly contains $Expectation (/$Pattern/)"
    }
}

try {
    $env:CARGO_TARGET_DIR = $targetDirectory
    Push-Location $repository
    try {
        # Disable fat LTO only for this audit artifact. That keeps each
        # target-feature function materialized under a stable symbol, while the
        # shipping release build keeps its normal LTO settings.
        # This feature only retains bench-only AVX-512 function pointers in
        # this isolated artifact. It is not enabled for a shipping build.
        & rtk cargo rustc -p sakura-core --release --lib --features simd-assembly-audit --config 'profile.release.lto=false' -- --emit=asm
        if ($LASTEXITCODE -ne 0) { throw "cargo rustc failed with exit code $LASTEXITCODE" }
    }
    finally {
        Pop-Location
    }

    $deps = Join-Path $targetDirectory 'x86_64-pc-windows-msvc\release\deps'
    $assemblyPath = Get-NewestAssembly $deps
    $assembly = [IO.File]::ReadAllText($assemblyPath, [Text.Encoding]::UTF8)

    $scalar = Get-FunctionBody $assembly 'scan_scalar'
    Assert-NotContains $scalar.name $scalar.body '%(?:xmm|ymm|zmm|k)[0-9]+\b' 'vector or mask registers'

    $avxFloor = Get-FunctionBody $assembly 'scan_avx_ssse3_128'
    Assert-Contains $avxFloor.name $avxFloor.body '%xmm[0-9]+\b' 'an XMM vector body'
    Assert-Contains $avxFloor.name $avxFloor.body '\bvpshufb\b' 'the SSSE3 nibble-LUT shuffle'
    Assert-NotContains $avxFloor.name $avxFloor.body '%(?:ymm|zmm|k)[0-9]+\b' 'AVX2 or AVX-512 registers'

    $avx2 = Get-FunctionBody $assembly 'scan_avx2'
    Assert-Contains $avx2.name $avx2.body '%ymm[0-9]+\b' 'the AVX2 YMM body'
    Assert-Contains $avx2.name $avx2.body '%xmm[0-9]+\b' 'the in-function XMM tail'
    Assert-NotContains $avx2.name $avx2.body '%(?:zmm|k)[0-9]+\b' 'AVX-512 registers or mask registers'

    # The three threshold wrappers are deliberately tiny: each carries a fixed
    # ZMM takeover boundary into the one shared AVX-512BW+VL body. Verify both
    # sides of that contract: a concrete immediate threshold and a tail call in
    # every wrapper, then the ZMM/VL/mask instruction shape in the shared body.
    foreach ($candidate in @(
        [pscustomobject]@{ name = 'scan_avx512_bw_vl_from_64'; threshold = 64 },
        [pscustomobject]@{ name = 'scan_avx512_bw_vl_from_128'; threshold = 128 },
        [pscustomobject]@{ name = 'scan_avx512_bw_vl_from_256'; threshold = 256 }
    )) {
        $wrapper = Get-FunctionBody $assembly $candidate.name
        $thresholdPattern = '\bmovl\s+\$' + $candidate.threshold + ',\s*%r9d\b'
        Assert-Contains $wrapper.name $wrapper.body $thresholdPattern "the fixed $($candidate.threshold)-byte takeover threshold"
        Assert-Contains $wrapper.name $wrapper.body '\bjmp\s+.*scan_avx512_bw_vl_hybrid' 'the shared AVX-512BW+VL body tail call'
    }
    $avx512Shared = Get-FunctionBody $assembly 'scan_avx512_bw_vl_hybrid'
    Assert-Contains $avx512Shared.name $avx512Shared.body '%zmm[0-9]+\b' 'the 512-bit ZMM body'
    Assert-Contains $avx512Shared.name $avx512Shared.body '%ymm[0-9]+\b' 'the VL 256-bit tail body'
    Assert-Contains $avx512Shared.name $avx512Shared.body '%k[0-9]+\b' 'an AVX-512 mask register'
    Assert-Contains $avx512Shared.name $avx512Shared.body '\bvptestnmb\b' 'the byte mask test'

    $entry = Get-FunctionBody $assembly 'passthrough_len'
    Assert-Contains $entry.name $entry.body 'ACTIVE_WIDTH_SCAN' 'the one resolved strategy-pointer load'
    Assert-Contains $entry.name $entry.body '(?m)^\s*(?:rex64\s+)?jmpq\s+\*%[a-z0-9]+' 'the indirect selected-kernel call'
    Assert-NotContains $entry.name $entry.body '\bcpuid\b' 'a hot-path CPUID probe'

    Write-Host "SIMD assembly gate passed: $assemblyPath"
    Write-Host "scalar=$($scalar.symbol)"
    Write-Host "avx-floor=$($avxFloor.symbol)"
    Write-Host "avx2=$($avx2.symbol)"
    Write-Host "avx512-shared=$($avx512Shared.symbol)"
}
finally {
    if ($null -eq $oldTargetDirectory) { Remove-Item Env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue }
    else { $env:CARGO_TARGET_DIR = $oldTargetDirectory }
}
