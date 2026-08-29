[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidateNotNullOrEmpty()][string]$Version,
    [Parameter(Mandatory)][ValidateNotNullOrEmpty()][string]$SourceCommit,
    [Parameter(Mandatory)][ValidateNotNullOrEmpty()][string]$ExpiresUnix,
    [Parameter(Mandatory)][ValidateNotNullOrEmpty()][string]$ReleaseSequence,
    [string]$TrustEpoch = '1',
    [string]$MinimumUpdaterVersion = '1.0.33',
    [ValidateSet('required', 'unsigned')][string]$Authenticode = 'unsigned',
    [string]$Tag,
    [string]$Installer = (Join-Path $PSScriptRoot '..\installer\out\sakura_setup.exe'),
    [string]$Output = (Join-Path $PSScriptRoot '..\installer\out\release-manifest-v2.txt'),
    [string]$ExpectedSubject = ''
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Assert-CanonicalVersion {
    param([Parameter(Mandatory)][string]$Value, [Parameter(Mandatory)][string]$Name)
    if ($Value -notmatch '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$') {
        throw "$Name must be canonical major.minor.patch"
    }
}

Assert-CanonicalVersion $Version 'version'
Assert-CanonicalVersion $MinimumUpdaterVersion 'minimum updater version'
if ([version]$MinimumUpdaterVersion -gt [version]$Version) { throw 'minimum updater version cannot be newer than the release version' }
if ([string]::IsNullOrWhiteSpace($Tag)) { $Tag = "v$Version" }
if ($Tag -cne "v$Version") { throw 'tag must be exactly v<version>' }
if ($SourceCommit -notmatch '^[0-9a-f]{40}$') { throw 'source commit must be 40 lowercase hexadecimal characters' }

[UInt64]$epoch = 0
if (-not [UInt64]::TryParse($TrustEpoch, [Globalization.NumberStyles]::None, [Globalization.CultureInfo]::InvariantCulture, [ref]$epoch) -or $epoch -eq 0) { throw 'trust epoch must be a positive decimal unsigned integer' }
[UInt64]$sequence = 0
if (-not [UInt64]::TryParse($ReleaseSequence, [Globalization.NumberStyles]::None, [Globalization.CultureInfo]::InvariantCulture, [ref]$sequence) -or $sequence -eq 0) { throw 'release sequence must be a positive decimal unsigned integer' }
[Int64]$expires = 0
if (-not [Int64]::TryParse($ExpiresUnix, [Globalization.NumberStyles]::None, [Globalization.CultureInfo]::InvariantCulture, [ref]$expires)) { throw 'expires_unix must be a decimal Unix timestamp' }
if ($expires -le [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()) { throw 'expires_unix must be strictly later than the current UTC time' }

$installerPath = [IO.Path]::GetFullPath($Installer)
$outputPath = [IO.Path]::GetFullPath($Output)
$item = [IO.FileInfo]::new($installerPath)
if (-not $item.Exists) { throw "installer is missing: $installerPath" }
if ($item.Name -cne 'sakura_setup.exe') { throw 'installer basename must be exactly sakura_setup.exe' }
if ($item.Length -lt 1 -or $item.Length -gt 200MB) { throw 'installer size is outside the updater bound' }
$stream = [IO.File]::OpenRead($installerPath)
try {
    $algorithm = [Security.Cryptography.SHA256]::Create()
    try { $sha256 = ([BitConverter]::ToString($algorithm.ComputeHash($stream))).Replace('-', '').ToLowerInvariant() }
    finally { $algorithm.Dispose() }
}
finally { $stream.Dispose() }

$auth = Get-AuthenticodeSignature -LiteralPath $installerPath
if ($Authenticode -ceq 'unsigned') {
    if ($auth.Status -ne [Management.Automation.SignatureStatus]::NotSigned) { throw "unsigned policy requires exact NotSigned status, got $($auth.Status)" }
} else {
    if ($auth.Status -ne [Management.Automation.SignatureStatus]::Valid) { throw "required policy requires Valid Authenticode status, got $($auth.Status)" }
    if ([string]::IsNullOrWhiteSpace($ExpectedSubject) -or $null -eq $auth.SignerCertificate -or $auth.SignerCertificate.Subject -cne $ExpectedSubject) { throw 'required policy requires the configured exact Authenticode subject' }
}

$url = "https://github.com/tsuyoshi-otake/sakura-input/releases/download/$Tag/sakura_setup.exe"
$manifest = (@(
    'schema=2', 'product=sakura-input', 'repository=tsuyoshi-otake/sakura-input',
    'channel=stable', 'platform=windows-x86_64', "trust_epoch=$epoch",
    "release_sequence=$sequence", "version=$Version", "tag=$Tag",
    "source_commit=$SourceCommit", 'asset_name=sakura_setup.exe', "installer_url=$url",
    "sha256=$sha256", "size=$($item.Length)", "authenticode=$Authenticode",
    "minimum_updater_version=$MinimumUpdaterVersion", "expires_unix=$expires"
) -join "`n") + "`n"
[IO.Directory]::CreateDirectory((Split-Path -Parent $outputPath)) | Out-Null
$temporary = "$outputPath.$PID.tmp"
try {
    [IO.File]::WriteAllText($temporary, $manifest, [Text.UTF8Encoding]::new($false))
    [IO.File]::Move($temporary, $outputPath, $true)
}
finally { if ([IO.File]::Exists($temporary)) { [IO.File]::Delete($temporary) } }
Write-Host "release manifest v2: $outputPath"
