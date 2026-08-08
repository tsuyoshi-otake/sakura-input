[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$CertificatePath,

    [Parameter(Mandatory)]
    [ValidateNotNullOrEmpty()]
    [string]$ExpectedSubject,

    [Parameter(Mandatory)]
    [ValidateNotNullOrEmpty()]
    [string[]]$Files,

    [string]$PasswordEnvironmentVariable = 'SAKURA_SIGNING_CERT_PASSWORD',

    [string]$TimestampUrl = 'https://timestamp.digicert.com'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Resolve-SignTool {
    $command = Get-Command signtool.exe -ErrorAction SilentlyContinue
    if ($null -ne $command) { return $command.Source }

    $kitsRoot = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
    if (-not [IO.Directory]::Exists($kitsRoot)) {
        throw 'Windows SDK SignTool was not found'
    }
    $options = [IO.EnumerationOptions]::new()
    $options.RecurseSubdirectories = $false
    $options.IgnoreInaccessible = $true
    $options.AttributesToSkip = [IO.FileAttributes]::ReparsePoint
    [string[]]$versions = [IO.Directory]::EnumerateDirectories($kitsRoot, '*', $options)
    [Array]::Sort($versions, [StringComparer]::OrdinalIgnoreCase)
    [Array]::Reverse($versions)
    foreach ($version in $versions) {
        $candidate = Join-Path $version 'x64\signtool.exe'
        if ([IO.File]::Exists($candidate)) { return $candidate }
    }
    throw 'Windows SDK SignTool x64 executable was not found'
}

function Invoke-SignTool {
    param([Parameter(Mandatory)][string[]]$Arguments)

    & $script:SignTool @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "SignTool failed with exit code $LASTEXITCODE"
    }
}

$certificateFile = [IO.Path]::GetFullPath($CertificatePath)
if (-not [IO.File]::Exists($certificateFile)) { throw "signing certificate is missing: $certificateFile" }
$passwordValue = [Environment]::GetEnvironmentVariable($PasswordEnvironmentVariable)
if ([string]::IsNullOrEmpty($passwordValue)) {
    throw "signing password environment variable $PasswordEnvironmentVariable is missing"
}
$timestamp = [Uri]$TimestampUrl
if (-not $timestamp.IsAbsoluteUri -or $timestamp.Scheme -ne 'https') {
    throw 'timestamp URL must be an absolute HTTPS URL'
}

$password = ConvertTo-SecureString $passwordValue -AsPlainText -Force
$probe = [Security.Cryptography.X509Certificates.X509Certificate2]::new(
    $certificateFile,
    $passwordValue,
    [Security.Cryptography.X509Certificates.X509KeyStorageFlags]::EphemeralKeySet
)
try {
    if (-not $probe.HasPrivateKey) { throw 'PFX contains no private key' }
    if ($probe.Subject -cne $ExpectedSubject) {
        throw "certificate subject '$($probe.Subject)' does not exactly match '$ExpectedSubject'"
    }
    $now = [DateTime]::UtcNow
    if ($probe.NotBefore.ToUniversalTime() -gt $now -or $probe.NotAfter.ToUniversalTime() -le $now) {
        throw 'signing certificate is not currently valid'
    }
    $thumbprint = $probe.Thumbprint
}
finally {
    $probe.Dispose()
}

$storePath = "Cert:\CurrentUser\My\$thumbprint"
if (Test-Path -LiteralPath $storePath) {
    throw "refusing to overwrite an existing CurrentUser certificate with thumbprint $thumbprint"
}

$resolvedFiles = [Collections.Generic.List[string]]::new()
foreach ($file in $Files) {
    $resolved = [IO.Path]::GetFullPath($file)
    if (-not [IO.File]::Exists($resolved)) { throw "release file is missing: $resolved" }
    if ($resolvedFiles.Contains($resolved)) { throw "release file was listed twice: $resolved" }
    $resolvedFiles.Add($resolved)
}

$SignTool = Resolve-SignTool
$imported = $null
try {
    $imported = Import-PfxCertificate -FilePath $certificateFile -CertStoreLocation Cert:\CurrentUser\My -Password $password -Exportable:$false
    if ($null -eq $imported -or $imported.Thumbprint -cne $thumbprint) {
        throw 'PFX import returned an unexpected certificate'
    }

    foreach ($file in $resolvedFiles) {
        Write-Host "==> signing $file"
        Invoke-SignTool @(
            'sign', '/sha1', $thumbprint, '/s', 'My', '/fd', 'SHA256',
            '/td', 'SHA256', '/tr', $TimestampUrl, '/v', $file
        )
        Invoke-SignTool @('verify', '/pa', '/all', '/v', $file)

        $signature = Get-AuthenticodeSignature -LiteralPath $file
        if ($signature.Status -ne [Management.Automation.SignatureStatus]::Valid) {
            throw "Authenticode status for $file is $($signature.Status): $($signature.StatusMessage)"
        }
        if ($null -eq $signature.SignerCertificate -or $signature.SignerCertificate.Subject -cne $ExpectedSubject) {
            throw "Authenticode signer for $file does not exactly match $ExpectedSubject"
        }
        if ($null -eq $signature.TimeStamperCertificate) {
            throw "Authenticode signature for $file has no RFC 3161 timestamp"
        }
    }
}
finally {
    if ($null -ne $imported -and (Test-Path -LiteralPath $storePath)) {
        Remove-Item -LiteralPath $storePath -Force
    }
}

Write-Host ("signed and verified {0} release file(s)" -f $resolvedFiles.Count)
