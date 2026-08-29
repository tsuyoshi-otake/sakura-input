[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$temp = Join-Path ([IO.Path]::GetTempPath()) "sakura-update-signing-v2-$PID"
$entropy = [Text.Encoding]::UTF8.GetBytes("Sakura Input offline update signing key v1`0")
$ecdsa = $null
function Assert-Rejected {
    param([Parameter(Mandatory)][string]$ManifestPath, [Parameter(Mandatory)][string]$SignaturePath, [Parameter(Mandatory)][string]$KeyringPath)
    & pwsh -NoProfile -ExecutionPolicy Bypass -File (Join-Path $root 'scripts/verify-update-manifest.ps1') `
        -Manifest $ManifestPath -Signature $SignaturePath -Keyring $KeyringPath 2>&1 | Out-Null
    if ($LASTEXITCODE -eq 0) { throw "negative fixture unexpectedly passed: $ManifestPath / $SignaturePath / $KeyringPath" }
}
try {
    [IO.Directory]::CreateDirectory($temp) | Out-Null
    $manifest = Join-Path $temp 'manifest.txt'
    $signature = Join-Path $temp 'manifest.sig'
    $protected = Join-Path $temp 'private.dpapi'
    $keyring = Join-Path $temp 'keyring.txt'
    $productionManifest = Join-Path $root 'verification/fixtures/update-signing-v2/manifest-positive.txt'
    $productionSignature = Join-Path $root 'verification/fixtures/update-signing-v2/signature-positive.txt'
    $productionKeyring = Join-Path $root 'data/update-signing/public-keys-v1.txt'
    & pwsh -NoProfile -ExecutionPolicy Bypass -File (Join-Path $root 'scripts/verify-update-manifest.ps1') `
        -Manifest $productionManifest -Signature $productionSignature -Keyring $productionKeyring
    if ($LASTEXITCODE -ne 0) { throw 'production positive fixture verification failed' }
    $uppercaseManifest = Join-Path $temp 'manifest-uppercase-policy.txt'
    [IO.File]::WriteAllText($uppercaseManifest, [IO.File]::ReadAllText($productionManifest, [Text.Encoding]::UTF8).Replace("authenticode=unsigned`n", "authenticode=UNSIGNED`n"), [Text.UTF8Encoding]::new($false))
    Assert-Rejected $uppercaseManifest $productionSignature $productionKeyring
    $malformedKeyring = Join-Path $temp 'keyring-malformed.txt'
    [IO.File]::WriteAllText($malformedKeyring, [IO.File]::ReadAllText($productionKeyring, [Text.Encoding]::UTF8).Replace('key.0.role=active', 'key.0.role=ACTIVE'), [Text.UTF8Encoding]::new($false))
    Assert-Rejected $productionManifest $productionSignature $malformedKeyring
    $unknownSignature = Join-Path $temp 'signature-unknown.txt'
    [IO.File]::WriteAllText($unknownSignature, [IO.File]::ReadAllText($productionSignature, [Text.Encoding]::UTF8).Replace('178bc99d4699cde4b78c0169655d3b165140a62173812867a8a66b1a608b6c47', ('f' * 64)), [Text.UTF8Encoding]::new($false))
    Assert-Rejected $productionManifest $unknownSignature $productionKeyring
    $unsortedSignature = Join-Path $temp 'signature-unsorted.txt'
    $positiveSignatureText = [IO.File]::ReadAllText($productionSignature, [Text.Encoding]::UTF8)
    $signatureHex = ([regex]::Match($positiveSignatureText, 'signature\.0=[0-9a-f]{64}:([0-9a-f]{128})')).Groups[1].Value
    $standbyId = '44e075680f1155c911119d9e039858a828757e37b901c2afe49fde3c4a0af92f'
    $activeId = '178bc99d4699cde4b78c0169655d3b165140a62173812867a8a66b1a608b6c47'
    $unsortedText = "schema=1`nalgorithm=ecdsa-p256-sha256-p1363`nmanifest_sha256=b90f4862b54c5643fac5f0188d2dbd0fae79feb7975f18620fdd731b33978340`nsignature_count=2`nsignature.0=$standbyId`:$signatureHex`nsignature.1=$activeId`:$signatureHex`n"
    [IO.File]::WriteAllText($unsortedSignature, $unsortedText, [Text.UTF8Encoding]::new($false))
    Assert-Rejected $productionManifest $unsortedSignature $productionKeyring
    $partlyInvalidSignature = Join-Path $temp 'signature-partly-invalid.txt'
    $partlyInvalidText = "schema=1`nalgorithm=ecdsa-p256-sha256-p1363`nmanifest_sha256=b90f4862b54c5643fac5f0188d2dbd0fae79feb7975f18620fdd731b33978340`nsignature_count=2`nsignature.0=$activeId`:$signatureHex`nsignature.1=$standbyId`:$signatureHex`n"
    [IO.File]::WriteAllText($partlyInvalidSignature, $partlyInvalidText, [Text.UTF8Encoding]::new($false))
    Assert-Rejected $productionManifest $partlyInvalidSignature $productionKeyring
    [IO.File]::Copy((Join-Path $root 'verification/fixtures/update-signing-v2/manifest-positive.txt'), $manifest, $true)

    $ecdsa = [Security.Cryptography.ECDsa]::Create([Security.Cryptography.ECCurve+NamedCurves]::nistP256)
    $parameters = $ecdsa.ExportParameters($true)
    $coords = New-Object byte[] 64
    [Array]::Copy($parameters.Q.X, 0, $coords, 0, 32)
    [Array]::Copy($parameters.Q.Y, 0, $coords, 32, 32)
    $keyDomain = [Text.Encoding]::UTF8.GetBytes('Sakura Input update key v1' + [char]0)
    $keyIdInput = New-Object byte[] ($keyDomain.Length + $coords.Length)
    [Array]::Copy($keyDomain, $keyIdInput, $keyDomain.Length)
    [Array]::Copy($coords, 0, $keyIdInput, $keyDomain.Length, $coords.Length)
    $hash = [Security.Cryptography.SHA256]::Create()
    try { $keyId = ([BitConverter]::ToString($hash.ComputeHash($keyIdInput))).Replace('-', '').ToLowerInvariant() }
    finally { $hash.Dispose() }
    $x = ([BitConverter]::ToString($parameters.Q.X)).Replace('-', '').ToLowerInvariant()
    $y = ([BitConverter]::ToString($parameters.Q.Y)).Replace('-', '').ToLowerInvariant()
    $keyringText = @(
        'schema=1', 'key_count=1', "key.0.id=$keyId", 'key.0.role=active', "key.0.x=$x", "key.0.y=$y",
        'key.0.trust_epoch=1', 'key.0.not_before_sequence=1', 'key.0.not_after_sequence=18446744073709551615'
    ) -join "`n"
    [IO.File]::WriteAllText($keyring, "$keyringText`n", [Text.UTF8Encoding]::new($false))
    $private = $ecdsa.ExportPkcs8PrivateKey()
    try {
        $ciphertext = [Security.Cryptography.ProtectedData]::Protect($private, $entropy, [Security.Cryptography.DataProtectionScope]::CurrentUser)
        [IO.File]::WriteAllBytes($protected, $ciphertext)
        [Array]::Clear($ciphertext, 0, $ciphertext.Length)
    }
    finally { [Array]::Clear($private, 0, $private.Length) }
    $ecdsa.Dispose(); $ecdsa = $null

    & pwsh -NoProfile -ExecutionPolicy Bypass -File (Join-Path $root 'scripts/sign-update-manifest.ps1') `
        -Manifest $manifest -ProtectedPrivateKey $protected -KeyId $keyId -Keyring $keyring -Output $signature
    if ($LASTEXITCODE -ne 0) { throw 'ephemeral signer test failed' }
    & pwsh -NoProfile -ExecutionPolicy Bypass -File (Join-Path $root 'scripts/verify-update-manifest.ps1') `
        -Manifest $manifest -Signature $signature -Keyring $keyring
    if ($LASTEXITCODE -ne 0) { throw 'ephemeral verifier test failed' }

    $tampered = Join-Path $temp 'manifest-tampered.txt'
    $tamperedText = [IO.File]::ReadAllText($manifest, [Text.Encoding]::UTF8).Replace("size=56`n", "size=57`n")
    [IO.File]::WriteAllText($tampered, $tamperedText, [Text.UTF8Encoding]::new($false))
    & pwsh -NoProfile -ExecutionPolicy Bypass -File (Join-Path $root 'scripts/verify-update-manifest.ps1') `
        -Manifest $tampered -Signature $signature -Keyring $keyring 2>&1 | Out-Null
    if ($LASTEXITCODE -eq 0) { throw 'tampered manifest unexpectedly passed ephemeral verification' }
    Write-Host 'update-signing v2 ephemeral sign/verify/tamper tests passed.' -ForegroundColor Green
}
finally {
    if ($null -ne $ecdsa) { $ecdsa.Dispose() }
    [Array]::Clear($entropy, 0, $entropy.Length)
    if ([IO.Directory]::Exists($temp)) { [IO.Directory]::Delete($temp, $true) }
}
