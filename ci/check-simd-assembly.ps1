[CmdletBinding()]
param(
    [string]$TargetDirectory = (Join-Path $PSScriptRoot '..\target\simd-assembly-gate'),
    [switch]$SelfTest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repository = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$targetRoot = [IO.Path]::GetFullPath($TargetDirectory)
$oldTargetDirectory = $env:CARGO_TARGET_DIR

function Get-OnlyAssembly {
    param([Parameter(Mandatory)][string]$DependenciesDirectory)

    if (-not [IO.Directory]::Exists($DependenciesDirectory)) {
        throw "assembly dependency directory is missing: $DependenciesDirectory"
    }

    [string[]]$paths = @([IO.Directory]::EnumerateFiles(
        $DependenciesDirectory,
        'sakura_core-*.s',
        [IO.SearchOption]::TopDirectoryOnly
    ))
    if ($paths.Count -ne 1) {
        throw "expected exactly one sakura-core assembly artifact under $DependenciesDirectory, found $($paths.Count)"
    }
    return $paths[0]
}

function Get-FunctionBody {
    param(
        [Parameter(Mandatory)][string]$Assembly,
        [Parameter(Mandatory)][string]$Name
    )

    $escapedName = [regex]::Escape($Name)
    $matches = [regex]::Matches(
        $Assembly,
        "(?m)^(?<symbol>_ZN[^`r`n]*$escapedName\d+h[0-9A-Za-z]+E):`r?`n"
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

function Assert-ThresholdTailWrapper {
    param(
        [Parameter(Mandatory)]$Function,
        [Parameter(Mandatory)][int]$Threshold
    )

    # The compiler currently emits these source-level wrappers as exactly one
    # immediate move and one direct tail jump. Check that complete semantic
    # shape instead of coupling the audit to an incidental register choice.
    [string[]]$instructions = @(
        foreach ($line in ($Function.body -split "`r?`n")) {
            $trimmed = $line.Trim()
            if ($trimmed.Length -eq 0 -or $trimmed.EndsWith(':') -or $trimmed.StartsWith('.')) {
                continue
            }
            $trimmed
        }
    )
    if ($instructions.Count -ne 2) {
        throw "$($Function.name) must be a two-instruction threshold tail wrapper, found $($instructions.Count) instructions"
    }

    $movePattern = '^mov(?:l|q)?\s+\$' + $Threshold + ',\s*%[a-z][a-z0-9]*$'
    if ($instructions[0] -notmatch $movePattern) {
        throw "$($Function.name) is missing the fixed $Threshold-byte takeover threshold in its immediate move"
    }
    if ($instructions[1] -notmatch '^jmpq?\s+.*scan_avx512_bw_vl_hybrid') {
        throw "$($Function.name) is missing the shared AVX-512BW+VL body direct tail call"
    }
}

function Test-SimdAssembly {
    param([Parameter(Mandatory)][string]$Assembly)

    $scalar = Get-FunctionBody $Assembly 'scan_scalar'
    Assert-NotContains $scalar.name $scalar.body '%(?:xmm|ymm|zmm|k)[0-9]+\b' 'vector or mask registers'

    $avxFloor = Get-FunctionBody $Assembly 'scan_avx_ssse3_128'
    Assert-Contains $avxFloor.name $avxFloor.body '%xmm[0-9]+\b' 'an XMM vector body'
    Assert-Contains $avxFloor.name $avxFloor.body '\bvpshufb\b' 'the SSSE3 nibble-LUT shuffle'
    Assert-NotContains $avxFloor.name $avxFloor.body '%(?:ymm|zmm|k)[0-9]+\b' 'AVX2 or AVX-512 registers'

    $avx2 = Get-FunctionBody $Assembly 'scan_avx2'
    Assert-Contains $avx2.name $avx2.body '%ymm[0-9]+\b' 'the AVX2 YMM body'
    Assert-Contains $avx2.name $avx2.body '%xmm[0-9]+\b' 'the in-function XMM tail'
    Assert-NotContains $avx2.name $avx2.body '%(?:zmm|k)[0-9]+\b' 'AVX-512 registers or mask registers'

    foreach ($candidate in @(
        [pscustomobject]@{ name = 'scan_avx512_bw_vl_from_64'; threshold = 64 },
        [pscustomobject]@{ name = 'scan_avx512_bw_vl_from_128'; threshold = 128 },
        [pscustomobject]@{ name = 'scan_avx512_bw_vl_from_256'; threshold = 256 }
    )) {
        $wrapper = Get-FunctionBody $Assembly $candidate.name
        Assert-ThresholdTailWrapper $wrapper $candidate.threshold
    }

    $avx512Shared = Get-FunctionBody $Assembly 'scan_avx512_bw_vl_hybrid'
    Assert-Contains $avx512Shared.name $avx512Shared.body '%zmm[0-9]+\b' 'the 512-bit ZMM body'
    Assert-Contains $avx512Shared.name $avx512Shared.body '%ymm[0-9]+\b' 'the VL 256-bit tail body'
    Assert-Contains $avx512Shared.name $avx512Shared.body '%k[0-9]+\b' 'an AVX-512 mask register'
    Assert-Contains $avx512Shared.name $avx512Shared.body '\bvptestnmb\b' 'the byte mask test'

    $entry = Get-FunctionBody $Assembly 'passthrough_len'
    Assert-Contains $entry.name $entry.body 'ACTIVE_WIDTH_SCAN' 'the one resolved strategy-pointer load'
    Assert-Contains $entry.name $entry.body '(?m)^\s*(?:rex64\s+)?jmpq\s+\*%[a-z0-9]+' 'the indirect selected-kernel call'
    Assert-NotContains $entry.name $entry.body '\bcpuid\b' 'a hot-path CPUID probe'

    return [pscustomobject]@{
        scalar = $scalar.symbol
        avxFloor = $avxFloor.symbol
        avx2 = $avx2.symbol
        avx512Shared = $avx512Shared.symbol
    }
}

function Get-SelfTestAssembly {
    return @'
	.def	_ZN11sakura_core4simd11scan_scalar17hfixtureE;
_ZN11sakura_core4simd11scan_scalar17hfixtureE:
	retq
	.def	_ZN11sakura_core4simd18scan_avx_ssse3_12817hfixtureE;
_ZN11sakura_core4simd18scan_avx_ssse3_12817hfixtureE:
	vpshufb	%xmm0, %xmm1, %xmm2
	retq
	.def	_ZN11sakura_core4simd9scan_avx217hfixtureE;
_ZN11sakura_core4simd9scan_avx217hfixtureE:
	vmovdqu	%ymm0, %ymm1
	vmovdqu	%xmm0, %xmm1
	retq
	.def	_ZN11sakura_core4simd25scan_avx512_bw_vl_from_6417hfixtureE;
_ZN11sakura_core4simd25scan_avx512_bw_vl_from_6417hfixtureE:
	movl	$64, %r10d
	jmp	_ZN11sakura_core4simd24scan_avx512_bw_vl_hybrid17hfixtureE
	.def	_ZN11sakura_core4simd26scan_avx512_bw_vl_from_12817hfixtureE;
_ZN11sakura_core4simd26scan_avx512_bw_vl_from_12817hfixtureE:
	movl	$128, %r11d
	jmp	_ZN11sakura_core4simd24scan_avx512_bw_vl_hybrid17hfixtureE
	.def	_ZN11sakura_core4simd26scan_avx512_bw_vl_from_25617hfixtureE;
_ZN11sakura_core4simd26scan_avx512_bw_vl_from_25617hfixtureE:
	movl	$256, %eax
	jmp	_ZN11sakura_core4simd24scan_avx512_bw_vl_hybrid17hfixtureE
	.def	_ZN11sakura_core4simd24scan_avx512_bw_vl_hybrid17hfixtureE;
_ZN11sakura_core4simd24scan_avx512_bw_vl_hybrid17hfixtureE:
	vmovdqu64	%zmm0, %zmm1
	vmovdqu	%ymm0, %ymm1
	vptestnmb	%zmm0, %zmm1, %k1
	retq
	.def	_ZN11sakura_core4simd15passthrough_len17hfixtureE;
_ZN11sakura_core4simd15passthrough_len17hfixtureE:
	movq	ACTIVE_WIDTH_SCAN(%rip), %rax
	jmpq	*%rax
'@
}

function Assert-MutantRejected {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][string]$Assembly,
        [Parameter(Mandatory)][string]$ExpectedError
    )

    try {
        $null = Test-SimdAssembly $Assembly
    }
    catch {
        if ($_.Exception.Message -notlike $ExpectedError) {
            throw "self-test mutant '$Name' failed for the wrong reason: $($_.Exception.Message)"
        }
        Write-Host "self-test killed mutant: $Name"
        return
    }
    throw "self-test mutant '$Name' unexpectedly passed"
}

function Invoke-SelfTest {
    $fixture = Get-SelfTestAssembly
    $null = Test-SimdAssembly $fixture

    Assert-MutantRejected 'required instruction removed' `
        ($fixture.Replace('vpshufb', 'vpand')) `
        '*missing the SSSE3 nibble-LUT shuffle*'
    Assert-MutantRejected 'forbidden vector register added' `
        ($fixture.Replace("`tretq`n`t.def`t_ZN11sakura_core4simd18scan_avx_ssse3_128", "`tvmovdqu`t%xmm0, %xmm1`n`tretq`n`t.def`t_ZN11sakura_core4simd18scan_avx_ssse3_128")) `
        '*unexpectedly contains vector or mask registers*'
    Assert-MutantRejected 'hot-path CPUID added' `
        ($fixture.Replace("`tmovq`tACTIVE_WIDTH_SCAN", "`tcpuid`n`tmovq`tACTIVE_WIDTH_SCAN")) `
        '*unexpectedly contains a hot-path CPUID probe*'
    Assert-MutantRejected 'required symbol removed' `
        ($fixture.Replace('scan_avx2', 'scan_avx2_missing')) `
        '*expected exactly one emitted symbol for scan_avx2, found 0*'
    Assert-MutantRejected 'required symbol duplicated' `
        ($fixture + "`n`t.def`t_ZN11sakura_core4simd9scan_avx217hduplicateE;`n_ZN11sakura_core4simd9scan_avx217hduplicateE:`n`tretq`n") `
        '*expected exactly one emitted symbol for scan_avx2, found 2*'

    Write-Host 'SIMD assembly matcher self-test passed: 5/5 mutants rejected'
}

if ($SelfTest) {
    Invoke-SelfTest
    exit 0
}

try {
    [IO.Directory]::CreateDirectory($targetRoot) | Out-Null
    do {
        $targetDirectory = Join-Path $targetRoot ('run-' + [guid]::NewGuid().ToString('N'))
    } while ([IO.Directory]::Exists($targetDirectory))
    [IO.Directory]::CreateDirectory($targetDirectory) | Out-Null

    # A new child target directory is used for every invocation. The audit can
    # therefore never select an assembly artifact left by an earlier compiler.
    $env:CARGO_TARGET_DIR = $targetDirectory
    Push-Location $repository
    try {
        # Disable fat LTO only for this audit artifact. That keeps each
        # target-feature function materialized under a stable symbol, while the
        # shipping release build keeps its normal LTO settings. The audit-only
        # feature is scoped to this command and never enters a shipping build.
        & cargo rustc --locked --target x86_64-pc-windows-msvc -p sakura-core --release --lib --features simd-assembly-audit --config 'profile.release.lto=false' -- --emit=asm
        if ($LASTEXITCODE -ne 0) { throw "cargo rustc failed with exit code $LASTEXITCODE" }
    }
    finally {
        Pop-Location
    }

    $deps = Join-Path $targetDirectory 'x86_64-pc-windows-msvc\release\deps'
    $assemblyPath = Get-OnlyAssembly $deps
    $assembly = [IO.File]::ReadAllText($assemblyPath, [Text.Encoding]::UTF8)
    $result = Test-SimdAssembly $assembly

    Write-Host "SIMD assembly gate passed: $assemblyPath"
    Write-Host "scalar=$($result.scalar)"
    Write-Host "avx-floor=$($result.avxFloor)"
    Write-Host "avx2=$($result.avx2)"
    Write-Host "avx512-shared=$($result.avx512Shared)"
}
finally {
    if ($null -eq $oldTargetDirectory) { Remove-Item Env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue }
    else { $env:CARGO_TARGET_DIR = $oldTargetDirectory }
}
