[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidateNotNullOrEmpty()][string]$Manifest,
    [Parameter(Mandatory)][ValidateNotNullOrEmpty()][string]$ProtectedPrivateKey,
    [Parameter(Mandatory)][ValidateNotNullOrEmpty()][string]$KeyId,
    [string]$Keyring = (Join-Path $PSScriptRoot '..\data\update-signing\public-keys-v1.txt'),
    [string]$Output = (Join-Path $PSScriptRoot '..\installer\out\release-manifest-v2.sig')
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$script:Entropy = [Text.Encoding]::UTF8.GetBytes("Sakura Input offline update signing key v1`0")
if (-not ('System.Security.Cryptography.ProtectedData' -as [type])) { Add-Type -AssemblyName System.Security.Cryptography.ProtectedData }

function Read-ExactUtf8 { param([string]$Path)
    $bytes = [IO.File]::ReadAllBytes([IO.Path]::GetFullPath($Path))
    if ($bytes.Length -lt 4 -or $bytes[0] -eq 0xef -or $bytes -contains 0) { throw 'manifest contains BOM, NUL, or is too short' }
    $text = [Text.UTF8Encoding]::new($false, $true).GetString($bytes)
    if ($text -notmatch "\A(?:[^\r\n=]+=[^\r\n]*\n)+\z" -or $text.Contains("`r")) { throw 'manifest must be UTF-8 LF text with one terminal LF' }
    return [pscustomobject]@{ Bytes = $bytes; Text = $text }
}

function Parse-Manifest { param([string]$Text)
    $names = @('schema','product','repository','channel','platform','trust_epoch','release_sequence','version','tag','source_commit','asset_name','installer_url','sha256','size','authenticode','minimum_updater_version','expires_unix')
    $lines = $Text.Split([char]10)
    if ($lines.Count -ne 18 -or $lines[17] -ne '') { throw 'manifest must contain exactly 17 fields and one terminal LF' }
    $map = [ordered]@{}
    for ($i=0; $i -lt 17; $i++) {
        $parts = $lines[$i].Split('=', 2)
        if ($parts.Count -ne 2 -or $parts[0] -cne $names[$i] -or $parts[1].Length -eq 0 -or $parts[1].Trim() -cne $parts[1]) { throw 'manifest is not canonical' }
        $map[$parts[0]] = $parts[1]
    }
    if ($map.schema -cne '2' -or $map.product -cne 'sakura-input' -or $map.repository -cne 'tsuyoshi-otake/sakura-input' -or $map.channel -cne 'stable' -or $map.platform -cne 'windows-x86_64' -or $map.asset_name -cne 'sakura_setup.exe') { throw 'manifest identity is not allow-listed' }
    if ($map.version -notmatch '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$' -or $map.tag -cne "v$($map.version)") { throw 'manifest version or tag is invalid' }
    if ($map.source_commit -notmatch '^[0-9a-f]{40}$' -or $map.sha256 -notmatch '^[0-9a-f]{64}$' -or $map.size -notmatch '^[1-9][0-9]*$' -or [UInt64]$map.size -gt 200MB -or $map.authenticode -cnotin @('required','unsigned')) { throw 'manifest field encoding is invalid' }
    if ($map.installer_url -cne "https://github.com/tsuyoshi-otake/sakura-input/releases/download/$($map.tag)/sakura_setup.exe") { throw 'manifest installer URL is not canonical' }
    if ($map.trust_epoch -notmatch '^[1-9][0-9]*$' -or $map.release_sequence -notmatch '^[1-9][0-9]*$' -or
        $map.minimum_updater_version -notmatch '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$' -or
        $map.expires_unix -notmatch '^[0-9]+$' -or
        [UInt64]$map.trust_epoch -eq 0 -or [UInt64]$map.release_sequence -eq 0 -or
        [version]$map.minimum_updater_version -gt [version]$map.version -or
        [Int64]$map.expires_unix -le [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()) { throw 'manifest trust, sequence, minimum version, or expiry is invalid' }
    return $map
}

function Read-Keyring { param([string]$Path)
    $bytes = [IO.File]::ReadAllBytes([IO.Path]::GetFullPath($Path))
    if ($bytes.Length -lt 4 -or $bytes[0] -eq 0xef -or $bytes -contains 0) { throw 'keyring contains BOM, NUL, or is too short' }
    $text = [Text.UTF8Encoding]::new($false, $true).GetString($bytes)
    if ($text.Contains("`r") -or $text -notmatch "\A(?:[^\r\n=]+=[^\r\n]*\n)+\z") { throw 'keyring must be UTF-8 LF text with one terminal LF' }
    $lines = $text.Split([char]10)
    if ($lines.Count -lt 3 -or $lines[0] -cne 'schema=1' -or $lines[1] -notmatch '^key_count=([1-9][0-9]*)$') { throw 'keyring header is not canonical' }
    [int]$count = $Matches[1]
    if ($count -gt 32 -or $lines.Count -ne (3 + (7 * $count)) -or $lines[$lines.Count - 1] -cne '') { throw 'keyring record count or terminal LF is invalid' }
    $records = [Collections.Generic.List[object]]::new()
    for ($i = 0; $i -lt $count; $i++) {
        $record = [ordered]@{}
        foreach ($name in @('id','role','x','y','trust_epoch','not_before_sequence','not_after_sequence')) {
            $line = $lines[2 + (7 * $i) + $record.Count]
            $expected = "key.$i.$name="
            if (-not $line.StartsWith($expected, [StringComparison]::Ordinal) -or $line.Length -eq $expected.Length) { throw "keyring record $i has an unexpected field order" }
            $record[$name] = $line.Substring($expected.Length)
        }
        if ($record.role -cnotin @('active','standby','retired','revoked') -or $record.id -cnotmatch '^[0-9a-f]{64}$' -or $record.x -cnotmatch '^[0-9a-f]{64}$' -or $record.y -cnotmatch '^[0-9a-f]{64}$' -or
            $record.trust_epoch -cnotmatch '^[1-9][0-9]*$' -or $record.not_before_sequence -cnotmatch '^[1-9][0-9]*$' -or $record.not_after_sequence -notmatch '^[1-9][0-9]*$' -or
            [UInt64]$record.not_after_sequence -lt [UInt64]$record.not_before_sequence) { throw "keyring record $i is malformed" }
        $keyDomain = [Text.Encoding]::UTF8.GetBytes('Sakura Input update key v1' + [char]0)
        $coords = New-Object byte[] 64; [Convert]::FromHexString($record.x).CopyTo($coords, 0); [Convert]::FromHexString($record.y).CopyTo($coords, 32)
        $keyIdInput = New-Object byte[] ($keyDomain.Length + $coords.Length); [Array]::Copy($keyDomain, $keyIdInput, $keyDomain.Length); [Array]::Copy($coords, 0, $keyIdInput, $keyDomain.Length, $coords.Length)
        $hash = [Security.Cryptography.SHA256]::Create(); try { $derived = ([BitConverter]::ToString($hash.ComputeHash($keyIdInput))).Replace('-', '').ToLowerInvariant() } finally { $hash.Dispose(); [Array]::Clear($keyIdInput, 0, $keyIdInput.Length); [Array]::Clear($coords, 0, $coords.Length) }
        if ($derived -cne $record.id) { throw "keyring record $i has an id inconsistent with its coordinates" }
        $records.Add([pscustomobject]$record)
    }
    return $records.ToArray()
}

$raw = [IO.File]::ReadAllBytes([IO.Path]::GetFullPath($ProtectedPrivateKey))
$plain = $null; $ecdsa = $null
try {
    $plain = [Security.Cryptography.ProtectedData]::Unprotect($raw, $script:Entropy, [Security.Cryptography.DataProtectionScope]::CurrentUser)
    $ecdsa = [Security.Cryptography.ECDsa]::Create()
    $bytesRead = 0
    $ecdsa.ImportPkcs8PrivateKey($plain, [ref]$bytesRead) | Out-Null
    if ($bytesRead -ne $plain.Length) { throw 'protected private key contains trailing bytes' }
    $manifestData = Read-ExactUtf8 $Manifest
    $map = Parse-Manifest $manifestData.Text
    $record = @((Read-Keyring $Keyring) | Where-Object { $_.id -ceq $KeyId.ToLowerInvariant() })
    if ($record.Count -ne 1) { throw 'requested key id is absent from the pinned keyring' }
    $record = $record[0]
    if ($record.role -notin @('active', 'standby')) { throw 'requested signing key is not active or standby' }
    if ($map.trust_epoch -cne $record.trust_epoch -or [UInt64]$map.release_sequence -lt [UInt64]$record.not_before_sequence -or [UInt64]$map.release_sequence -gt [UInt64]$record.not_after_sequence) { throw 'manifest sequence is outside the pinned key window' }
    $parameters = $ecdsa.ExportParameters($false)
    if ($parameters.Curve.Oid.Value -ne [Security.Cryptography.ECCurve+NamedCurves]::nistP256.Oid.Value -or
        $null -eq $parameters.Q.X -or $null -eq $parameters.Q.Y -or $parameters.Q.X.Length -ne 32 -or $parameters.Q.Y.Length -ne 32) { throw 'private key is not a P-256 key' }
    $coords = New-Object byte[] 64; [Array]::Copy($parameters.Q.X, 0, $coords, 0, 32); [Array]::Copy($parameters.Q.Y, 0, $coords, 32, 32)
    $keyDomain = [Text.Encoding]::UTF8.GetBytes('Sakura Input update key v1' + [char]0)
    $keyIdInput = New-Object byte[] ($keyDomain.Length + $coords.Length)
    [Array]::Copy($keyDomain, $keyIdInput, $keyDomain.Length); [Array]::Copy($coords, 0, $keyIdInput, $keyDomain.Length, $coords.Length)
    $hash = [Security.Cryptography.SHA256]::Create(); try { $derived = ([BitConverter]::ToString($hash.ComputeHash($keyIdInput))).Replace('-','').ToLowerInvariant() } finally { $hash.Dispose(); [Array]::Clear($keyIdInput,0,$keyIdInput.Length); [Array]::Clear($coords,0,$coords.Length) }
    if ($derived -cne $record.id) { throw 'private key public coordinates do not match the committed key id' }
    $canonical = [Text.Encoding]::UTF8.GetBytes($manifestData.Text)
    $domain = [Text.Encoding]::UTF8.GetBytes('Sakura Input update manifest v2' + [char]0 + 'ecdsa-p256-sha256-p1363' + [char]0)
    $digestInput = New-Object byte[] ($domain.Length + $canonical.Length); [Array]::Copy($domain, $digestInput, $domain.Length); [Array]::Copy($canonical, 0, $digestInput, $domain.Length, $canonical.Length)
    $hash = [Security.Cryptography.SHA256]::Create(); try { $digest = $hash.ComputeHash($digestInput) } finally { $hash.Dispose(); [Array]::Clear($digestInput,0,$digestInput.Length); [Array]::Clear($canonical,0,$canonical.Length) }
    $sig = $ecdsa.SignHash($digest, [Security.Cryptography.DSASignatureFormat]::IeeeP1363FixedFieldConcatenation); [Array]::Clear($digest,0,$digest.Length)
    if ($sig.Length -ne 64) { throw 'ECDSA provider did not return P-1363 r||s' }
    $manifestHash = [Security.Cryptography.SHA256]::Create(); try { $mhash = ([BitConverter]::ToString($manifestHash.ComputeHash([Text.Encoding]::UTF8.GetBytes($manifestData.Text)))).Replace('-','').ToLowerInvariant() } finally { $manifestHash.Dispose() }
    $envelope = "schema=1`nalgorithm=ecdsa-p256-sha256-p1363`nmanifest_sha256=$mhash`nsignature_count=1`nsignature.0=$($record.id):$(([BitConverter]::ToString($sig)).Replace('-','').ToLowerInvariant())`n"
    [Array]::Clear($sig,0,$sig.Length)
    $out = [IO.Path]::GetFullPath($Output); [IO.Directory]::CreateDirectory((Split-Path -Parent $out)) | Out-Null; $tmp = "$out.$PID.tmp"
    try { [IO.File]::WriteAllText($tmp, $envelope, [Text.UTF8Encoding]::new($false)); [IO.File]::Move($tmp,$out,$true) } finally { if([IO.File]::Exists($tmp)){[IO.File]::Delete($tmp)} }
    Write-Host "signed update manifest: $out"
}
finally {
    if ($null -ne $plain) { [Array]::Clear($plain,0,$plain.Length) }; [Array]::Clear($raw,0,$raw.Length)
    if ($null -ne $ecdsa) { $ecdsa.Dispose() }
}
