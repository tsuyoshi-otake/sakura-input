#Requires -Version 5.1
<#
.SYNOPSIS
    Probe the saved Sakura Input AI key against the configured Responses endpoint.

.DESCRIPTION
    Reads Credential Manager target SakuraInput/AI/APIKey the same way the
    product does (UTF-8 blob via CredReadW). Endpoint and auth come from
    HKCU\SOFTWARE\SakuraInput\Preferences. The key is never printed, written
    to argv, or left in a temp file.

    Exit 0 only when both GET /models and POST /responses (gpt-5.6-luna)
    succeed. A 401 means the stored key is rejected; 403/404 usually means
    the account cannot use that model.

.EXAMPLE
    powershell -NoProfile -File scripts\test-ai-api-key.ps1
#>
[CmdletBinding()]
param(
    [string]$Endpoint,
    [ValidateSet('Bearer', 'ApiKey')]
    [string]$Auth
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$Target = 'SakuraInput/AI/APIKey'
$Model = 'gpt-5.6-luna'
$PrefPath = 'HKCU:\SOFTWARE\SakuraInput\Preferences'

function Write-Result {
    param([string]$Name, [string]$Value)
    [Console]::Out.WriteLine(("{0}`t{1}" -f $Name, $Value))
}

function Protect-SecretText {
    param([string]$Text)
    if ([string]::IsNullOrEmpty($Text)) { return '' }
    return ($Text -replace 'sk-[A-Za-z0-9_\-]{8,}', 'sk-REDACTED')
}

function Read-SavedApiKey {
    if (-not ('SakuraAiKeyProbe' -as [type])) {
        Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class SakuraAiKeyProbe {
  [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
  public struct CREDENTIAL {
    public uint Flags;
    public uint Type;
    public string TargetName;
    public string Comment;
    public System.Runtime.InteropServices.ComTypes.FILETIME LastWritten;
    public uint CredentialBlobSize;
    public IntPtr CredentialBlob;
    public uint Persist;
    public uint AttributeCount;
    public IntPtr Attributes;
    public string TargetAlias;
    public string UserName;
  }
  [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
  public static extern bool CredRead(string target, uint type, uint flags, out IntPtr cred);
  [DllImport("advapi32.dll", SetLastError = true)]
  public static extern void CredFree(IntPtr cred);
  public static byte[] ReadUtf8Blob(string target) {
    IntPtr raw;
    if (!CredRead(target, 1, 0, out raw) || raw == IntPtr.Zero) {
      return null;
    }
    try {
      var cred = (CREDENTIAL)Marshal.PtrToStructure(raw, typeof(CREDENTIAL));
      if (cred.CredentialBlob == IntPtr.Zero || cred.CredentialBlobSize == 0) {
        return new byte[0];
      }
      if (cred.CredentialBlobSize > 2048) {
        throw new InvalidOperationException("oversized");
      }
      byte[] bytes = new byte[cred.CredentialBlobSize];
      Marshal.Copy(cred.CredentialBlob, bytes, 0, (int)cred.CredentialBlobSize);
      return bytes;
    } finally {
      CredFree(raw);
    }
  }
}
"@
    }
    $bytes = [SakuraAiKeyProbe]::ReadUtf8Blob($Target)
    if ($null -eq $bytes) { return $null }
    $key = [Text.Encoding]::UTF8.GetString($bytes).Trim()
    [Array]::Clear($bytes, 0, $bytes.Length)
    return $key
}

function Read-AiPreferences {
    $provider = 0
    $authDword = 0
    $savedEndpoint = 'https://api.openai.com/v1'
    if (Test-Path $PrefPath) {
        $prefs = Get-ItemProperty -Path $PrefPath
        if ($null -ne $prefs.AiProvider) { $provider = [int]$prefs.AiProvider }
        if ($null -ne $prefs.AiAuth) { $authDword = [int]$prefs.AiAuth }
        if (-not [string]::IsNullOrWhiteSpace($prefs.AiEndpoint)) {
            $savedEndpoint = [string]$prefs.AiEndpoint
        }
    }
    $authName = switch ($authDword) {
        1 { 'ApiKey' }
        2 { 'None' }
        default { 'Bearer' }
    }
    [pscustomobject]@{
        Provider = $provider
        Auth     = $authName
        Endpoint = $savedEndpoint.Trim().TrimEnd('/')
    }
}

function Invoke-JsonProbe {
    param(
        [string]$Name,
        [string]$Method,
        [string]$Url,
        [string]$HeaderLine,
        [string]$JsonBody
    )
    $headerFile = Join-Path $env:TEMP ("sakura-ai-key-probe-{0}.hdr" -f [guid]::NewGuid().ToString('N'))
    $bodyFile = Join-Path $env:TEMP ("sakura-ai-key-probe-{0}.json" -f [guid]::NewGuid().ToString('N'))
    $outFile = Join-Path $env:TEMP ("sakura-ai-key-probe-{0}.out" -f [guid]::NewGuid().ToString('N'))
    $code = 0
    $errorType = ''
    $errorCode = ''
    try {
        [IO.File]::WriteAllText($headerFile, $HeaderLine)
        $curl = @(
            '-sS', '--http1.1', '-o', $outFile, '-w', '%{http_code}',
            '-X', $Method, $Url, '-H', ('@{0}' -f $headerFile)
        )
        if ($Method -eq 'POST') {
            [IO.File]::WriteAllText($bodyFile, $JsonBody, [Text.UTF8Encoding]::new($false))
            $curl += @('-H', 'Content-Type: application/json', '--data-binary', ('@{0}' -f $bodyFile))
        }
        $raw = & curl.exe @curl 2>&1
        if ($raw -is [System.Array]) { $raw = $raw -join '' }
        if ($raw -notmatch '^\d{3}$') {
            throw "curl failed: $(Protect-SecretText ([string]$raw))"
        }
        $code = [int]$raw
        if (Test-Path $outFile) {
            $text = Protect-SecretText ([IO.File]::ReadAllText($outFile))
            if ($text -match '"type"\s*:\s*"([^"]+)"') { $errorType = $Matches[1] }
            if ($text -match '"code"\s*:\s*"([^"]+)"') { $errorCode = $Matches[1] }
        }
    } finally {
        foreach ($path in @($headerFile, $bodyFile, $outFile)) {
            if (Test-Path $path) {
                $wipe = New-Object byte[] 4096
                [IO.File]::WriteAllBytes($path, $wipe)
                Remove-Item -LiteralPath $path -Force -ErrorAction SilentlyContinue
            }
        }
    }
    Write-Result ($Name + '.http') $code
    if ($errorType) { Write-Result ($Name + '.type') $errorType }
    if ($errorCode) { Write-Result ($Name + '.code') $errorCode }
    return @{ Http = $code; Type = $errorType; Code = $errorCode }
}

$prefs = Read-AiPreferences
if (-not $Endpoint) { $Endpoint = $prefs.Endpoint }
if (-not $Auth) { $Auth = $prefs.Auth }
$Endpoint = $Endpoint.Trim().TrimEnd('/')

Write-Result 'provider' $prefs.Provider
Write-Result 'endpoint' $Endpoint
Write-Result 'auth' $Auth
Write-Result 'model' $Model

if ($prefs.Provider -eq 5 -or $Auth -eq 'None') {
    Write-Result 'result' 'skip_chatgpt_codex'
    Write-Output 'ChatGPT Subscription uses Codex CLI login, not an API key.'
    exit 8
}

$key = Read-SavedApiKey
if ([string]::IsNullOrEmpty($key)) {
    Write-Result 'credential' 'missing'
    Write-Result 'result' 'fail'
    exit 2
}
if ($key.Length -gt 2048 -or $key.IndexOfAny([char[]]("`r", "`n", "`t")) -ge 0) {
    Write-Result 'credential' 'invalid'
    Write-Result 'result' 'fail'
    $key = $null
    exit 3
}

$prefix = if ($key.Length -ge 7) { $key.Substring(0, 7) } else { 'short' }
Write-Result 'credential' ('present len={0} prefix={1}' -f $key.Length, $prefix)

$header = if ($Auth -eq 'ApiKey') {
    'api-key: {0}' -f $key
} else {
    'Authorization: Bearer {0}' -f $key
}
$key = $null

$models = Invoke-JsonProbe -Name 'models' -Method 'GET' -Url ($Endpoint + '/models') -HeaderLine $header
$body = '{"model":"' + $Model + '","store":false,"instructions":"Return only the rewritten text.","input":[{"role":"user","content":[{"type":"input_text","text":"hi"}]}],"reasoning":{"effort":"low"}}'
$responses = Invoke-JsonProbe -Name 'responses' -Method 'POST' -Url ($Endpoint + '/responses') -HeaderLine $header -JsonBody $body
$header = $null
[GC]::Collect()

if ($models.Http -eq 200 -and $responses.Http -ge 200 -and $responses.Http -lt 300) {
    Write-Result 'result' 'ok'
    exit 0
}

if ($models.Http -eq 401 -or $responses.Http -eq 401) {
    Write-Result 'result' 'invalid_api_key'
    exit 4
}
if ($responses.Http -eq 403 -or $responses.Http -eq 404) {
    Write-Result 'result' ('model_or_permission http={0}' -f $responses.Http)
    exit 5
}
Write-Result 'result' ('http_error models={0} responses={1}' -f $models.Http, $responses.Http)
exit 6
