[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidateNotNullOrEmpty()]
    [string]$ExpectedSubject,

    [Parameter(Mandatory)]
    [ValidateNotNullOrEmpty()]
    [string[]]$Files
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
foreach ($file in $Files) {
    $path = [IO.Path]::GetFullPath($file)
    if (-not $seen.Add($path)) { throw "release file was listed twice: $path" }
    if (-not [IO.File]::Exists($path)) { throw "release file is missing: $path" }
    $signature = Get-AuthenticodeSignature -LiteralPath $path
    if ($signature.Status -ne [Management.Automation.SignatureStatus]::Valid) {
        throw "Authenticode status for $path is $($signature.Status): $($signature.StatusMessage)"
    }
    if ($null -eq $signature.SignerCertificate -or $signature.SignerCertificate.Subject -cne $ExpectedSubject) {
        throw "Authenticode signer for $path does not exactly match $ExpectedSubject"
    }
    if ($null -eq $signature.TimeStamperCertificate) {
        throw "Authenticode signature for $path has no timestamp"
    }
}

Write-Host ("valid Authenticode signatures: {0}" -f $seen.Count)
