[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidateNotNullOrEmpty()][string]$Manifest,
    [Parameter(Mandatory)][ValidateNotNullOrEmpty()][string]$Signature,
    [string]$Keyring = (Join-Path $PSScriptRoot '..\data\update-signing\public-keys-v1.txt'),
    [switch]$VerifyInstaller,
    [string]$Installer
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Read-ExactUtf8 { param([string]$Path)
    $bytes = [IO.File]::ReadAllBytes([IO.Path]::GetFullPath($Path))
    if ($bytes.Length -lt 4 -or $bytes[0] -eq 0xef -or $bytes -contains 0) { throw 'input contains BOM, NUL, or is too short' }
    $text = [Text.UTF8Encoding]::new($false, $true).GetString($bytes)
    if ($text.Contains("`r") -or $text -notmatch "\A(?:[^\r\n=]+=[^\r\n]*\n)+\z") { throw 'input must be UTF-8 LF text with one terminal LF' }
    [pscustomobject]@{ Bytes = $bytes; Text = $text }
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
        [UInt64]$map.trust_epoch -eq 0 -or [UInt64]$map.release_sequence -eq 0) { throw 'manifest trust, sequence, or numeric encoding is invalid' }
    if ([version]$map.minimum_updater_version -gt [version]$map.version) { throw 'minimum updater version exceeds release version' }
    if ([Int64]$map.expires_unix -le [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()) { throw 'manifest has expired' }
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
            $record.trust_epoch -notmatch '^[1-9][0-9]*$' -or $record.not_before_sequence -notmatch '^[1-9][0-9]*$' -or $record.not_after_sequence -notmatch '^[1-9][0-9]*$' -or
            [UInt64]$record.not_after_sequence -lt [UInt64]$record.not_before_sequence) { throw "keyring record $i is malformed" }
        $keyDomain = [Text.Encoding]::UTF8.GetBytes('Sakura Input update key v1' + [char]0)
        $coords = New-Object byte[] 64
        [Convert]::FromHexString($record.x).CopyTo($coords, 0)
        [Convert]::FromHexString($record.y).CopyTo($coords, 32)
        $keyIdInput = New-Object byte[] ($keyDomain.Length + $coords.Length)
        [Array]::Copy($keyDomain, $keyIdInput, $keyDomain.Length)
        [Array]::Copy($coords, 0, $keyIdInput, $keyDomain.Length, $coords.Length)
        $hash = [Security.Cryptography.SHA256]::Create()
        try { $derived = ([BitConverter]::ToString($hash.ComputeHash($keyIdInput))).Replace('-', '').ToLowerInvariant() }
        finally { $hash.Dispose(); [Array]::Clear($keyIdInput, 0, $keyIdInput.Length); [Array]::Clear($coords, 0, $coords.Length) }
        if ($derived -cne $record.id) { throw "keyring record $i has an id inconsistent with its coordinates" }
        $records.Add([pscustomobject]$record)
    }
    if ($records.Count -eq 0) { throw 'pinned keyring is empty' }
    return $records
}

function Get-PublicEcdsa { param([object]$Record)
    $parameters = [Security.Cryptography.ECParameters]::new()
    $parameters.Curve = [Security.Cryptography.ECCurve+NamedCurves]::nistP256
    $point = [Security.Cryptography.ECPoint]::new()
    $point.X = [Convert]::FromHexString($Record.x)
    $point.Y = [Convert]::FromHexString($Record.y)
    $parameters.Q = $point
    return [Security.Cryptography.ECDsa]::Create($parameters)
}

$manifestData = Read-ExactUtf8 $Manifest
$map = Parse-Manifest $manifestData.Text
$envelopeData = Read-ExactUtf8 $Signature
$lines = $envelopeData.Text.Split([char]10)
if ($lines.Count -lt 6 -or $lines[0] -cne 'schema=1' -or $lines[1] -cne 'algorithm=ecdsa-p256-sha256-p1363' -or $lines[2] -notmatch '^manifest_sha256=[0-9a-f]{64}$' -or $lines[3] -notmatch '^signature_count=[1-3]$') { throw 'signature envelope header is invalid' }
$manifestHash = [Security.Cryptography.SHA256]::Create(); try { $actualManifestHash = ([BitConverter]::ToString($manifestHash.ComputeHash($manifestData.Bytes))).Replace('-','').ToLowerInvariant() } finally { $manifestHash.Dispose() }
[void]($lines[2] -match '^manifest_sha256=(.+)$'); if ($Matches[1] -cne $actualManifestHash) { throw 'signature envelope manifest digest does not match exact manifest bytes' }
[void]($lines[3] -match '^signature_count=([1-3])$'); [int]$count = $Matches[1]
if ($lines.Count -ne (5 + $count) -or $lines[4 + $count] -ne '') { throw 'signature envelope has extra or missing records' }
$keys = Read-Keyring $Keyring; $previous = '' ; $valid = 0
$domain = [Text.Encoding]::UTF8.GetBytes('Sakura Input update manifest v2' + [char]0 + 'ecdsa-p256-sha256-p1363' + [char]0)
$input = New-Object byte[] ($domain.Length + $manifestData.Bytes.Length); [Array]::Copy($domain,$input,$domain.Length); [Array]::Copy($manifestData.Bytes,0,$input,$domain.Length,$manifestData.Bytes.Length)
$sha = [Security.Cryptography.SHA256]::Create(); try { $digest = $sha.ComputeHash($input) } finally { $sha.Dispose(); [Array]::Clear($input,0,$input.Length) }
for($i=0;$i -lt $count;$i++) {
    if ($lines[4+$i] -notmatch "^signature\.$i=([0-9a-f]{64}):([0-9a-f]{128})$") { throw 'signature record is not canonical P-1363' }
    $id = $Matches[1]; $sig = [Convert]::FromHexString($Matches[2]); if ([string]::CompareOrdinal($id, $previous) -le 0) { throw 'signature key IDs are not strictly ascending' }; $previous = $id
    $record = @($keys | Where-Object { $_.id -ceq $id }); if ($record.Count -ne 1) { throw 'signature key ID is not pinned' }; $record = $record[0]
    if ($record.role -eq 'revoked' -or [UInt64]$map.trust_epoch -ne [UInt64]$record.trust_epoch -or [UInt64]$map.release_sequence -lt [UInt64]$record.not_before_sequence -or [UInt64]$map.release_sequence -gt [UInt64]$record.not_after_sequence) { throw 'signature key is outside its trust window' }
    $ecdsa = Get-PublicEcdsa $record
    try {
        if (-not $ecdsa.VerifyHash($digest,$sig,[Security.Cryptography.DSASignatureFormat]::IeeeP1363FixedFieldConcatenation)) {
            throw "signature record $i failed P-256 verification"
        }
        $valid++
    }
    finally { $ecdsa.Dispose(); [Array]::Clear($sig,0,$sig.Length) }
}
[Array]::Clear($digest,0,$digest.Length)
if ($valid -ne $count) { throw 'not every supplied signature verified with the pinned keyring' }
if ($VerifyInstaller) {
    if ([string]::IsNullOrWhiteSpace($Installer)) { throw 'installer path is required for installer verification' }
    $item = [IO.FileInfo]::new([IO.Path]::GetFullPath($Installer)); if (-not $item.Exists -or [UInt64]$item.Length -ne [UInt64]$map.size) { throw 'installer size does not match the signed manifest' }
    $stream = [IO.File]::OpenRead($item.FullName); try { $hash = [Security.Cryptography.SHA256]::Create(); try { $actual = ([BitConverter]::ToString($hash.ComputeHash($stream))).Replace('-','').ToLowerInvariant() } finally { $hash.Dispose() } } finally { $stream.Dispose() }
    if ($actual -cne $map.sha256) { throw 'installer hash does not match the signed manifest' }
    $auth = Get-AuthenticodeSignature -LiteralPath $item.FullName
    if ($map.authenticode -ceq 'unsigned' -and $auth.Status -ne [Management.Automation.SignatureStatus]::NotSigned) { throw 'unsigned manifest requires exact NotSigned installer status' }
    if ($map.authenticode -ceq 'required' -and $auth.Status -ne [Management.Automation.SignatureStatus]::Valid) { throw 'required manifest requires Valid Authenticode status' }
}
Write-Host "verified update manifest v2: $Manifest ($valid valid pinned signature)"
