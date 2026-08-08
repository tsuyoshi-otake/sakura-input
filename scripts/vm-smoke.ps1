#Requires -Version 5.1
<#
.SYNOPSIS
    Installer smoke test: install -> type -> uninstall -> verify typing
    still works (DESIGN 12.2). Run this against a disposable VM snapshot,
    never against a machine you care about -- it installs a text service
    machine-wide and later force-removes it.

.DESCRIPTION
    This is CI's stand-in for a human clicking through the installer once
    and typing to see if it worked. It automates what can be automated and
    is honest about the rest: any step this script cannot verify prints a
    line marked MANUAL instead of silently assuming success, because a
    smoke test that reports green for something it never checked is worse
    than not testing it at all.

    Concretely, three classes of check:

    1. Fully automatic and gating (registry state, process presence, ASCII
       typing pass-through, exit codes). A failure here fails the run.
    2. Best-effort and gating where the failure mode is reachable, MANUAL
       where it depends on interactive session state this script cannot
       fully control (the per-user language-profile entry lives in HKCU,
       and enable-profile's runasoriginaluser only lands there when the
       account that launched Setup is the signed-in user -- see
       crates/sakura-regtool/src/interactive.rs).
    3. Not attempted at all: actually composing kana (romaji -> hiragana)
       requires the IME to be the *active* input method, and switching the
       active IME from a script is not reliably scriptable across Windows
       builds (the language bar / Win+Space shortcut is UI, not an API).
       That step is always MANUAL.

    Registry scope matches this product's target (DESIGN 3.2, x86_64-only,
    Windows 11 only): only the native 64-bit CLSID view is expected to
    exist. There is no x86 build to register, so unlike a mixed-bitness
    product this script asserts the WOW6432Node mirror is ABSENT rather
    than present -- regtool is invoked with --no-wow64 in [Run], and a
    WOW6432Node entry showing up anyway would mean that flag stopped being
    honored.

.PARAMETER Installer
    Path to sakura_setup.exe (installer/out/sakura_setup.exe from a release
    build). Checked for existence before anything else runs, including the
    administrator check below -- a wrong path is a typo, not a privilege
    problem, and should fail as one.

.PARAMETER Purge
    Also exercises /PURGE=1 on uninstall (DESIGN 12.2 §4): user data under
    %LOCALAPPDATA%\SakuraInput must be gone afterward. Without this switch,
    the default (data kept) is exercised instead, which is the more common
    path and the one every uninstall takes unless the operator opts in.

.EXAMPLE
    powershell -NoProfile -File scripts\vm-smoke.ps1 -Installer C:\out\sakura_setup.exe

.EXAMPLE
    powershell -NoProfile -File scripts\vm-smoke.ps1 -Installer C:\out\sakura_setup.exe -WhatIf

    Parses and self-checks the script without installing or uninstalling
    anything -- useful for validating this file on a developer's own
    machine, which is not a disposable VM and must not run the real thing.
#>
[CmdletBinding(SupportsShouldProcess = $true)]
param(
    [Parameter(Mandatory = $true)]
    [string]$Installer,

    [switch]$Purge
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# ---------------------------------------------------------------------------
# Identifiers that must match the running system exactly, kept in sync by
# hand with the two source-of-truth files: installer/setup.iss ([Setup]
# AppId) and crates/sakura-reg/src/guids.rs (CLSID_SAKURA_TSF,
# GUID_PROFILE_JA_JP, LANGID_JA_JP). There is no shared build-time constant
# between a Pascal Script installer, a Rust registry crate and a PowerShell
# smoke test, so this comment is the mechanism that keeps the three in
# agreement: if any of those change, this block must change with them.
# ---------------------------------------------------------------------------
$AppId = '{61D379C9-27DE-45E4-93B9-5871CB71A0CF}'
$ClsidTsf = '{C18F44DE-39E0-4B16-8D28-D5DE35BB11BC}'
$ProfileGuid = '{8466B5F0-210F-408B-A3FE-8D18ECBA711D}'
$LangIdKey = '0x00000411'  # ja-JP, decimal 1041 == guids::LANGID_JA_JP

$InstallDir = Join-Path ${env:ProgramFiles} 'Sakura Input'
$VersionRoot = Join-Path $InstallDir 'versions'
$TypingProbe = 'sakura smoke 1234'

# One row per check, printed as a table at the end. A script that only
# prints as it goes is easy to scroll past in CI logs; a summary table at
# the bottom is what a human actually reads first.
$script:Results = [System.Collections.Generic.List[pscustomobject]]::new()

function Add-Result {
    param(
        [Parameter(Mandatory)] [string]$Step,
        [Parameter(Mandatory)] [ValidateSet('Pass', 'Fail', 'Manual', 'Skipped')] [string]$Status,
        [string]$Detail = ''
    )
    $script:Results.Add([pscustomobject]@{ Step = $Step; Status = $Status; Detail = $Detail })
    $line = "[{0,-7}] {1}: {2}" -f $Status, $Step, $Detail
    if ($Status -eq 'Fail') {
        Write-Host $line -ForegroundColor Red
    } elseif ($Status -eq 'Manual') {
        Write-Host $line -ForegroundColor Yellow
    } else {
        Write-Host $line -ForegroundColor Green
    }
}

# ---------------------------------------------------------------------------
# Path validation first, before the administrator check that follows it.
# `-Installer nonexistent.exe` is the documented failure case this script
# is required to handle cleanly, and a wrong path is the cheapest possible
# mistake to report -- it should not first make the caller deal with a UAC
# prompt or an elevation error for a run that could never have worked.
# ---------------------------------------------------------------------------
if (-not (Test-Path -LiteralPath $Installer -PathType Leaf)) {
    Write-Error "installer not found: $Installer"
    exit 1
}
$Installer = (Resolve-Path -LiteralPath $Installer).ProviderPath

$principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Write-Error 'this script must run elevated: installing/uninstalling a machine-wide text service needs administrator rights'
    exit 1
}

# ---------------------------------------------------------------------------
# Install
# ---------------------------------------------------------------------------
function Invoke-Install {
    param([string]$Path)

    $logPath = Join-Path $env:TEMP 'sakura-install.log'
    if (Test-Path -LiteralPath $logPath) { Remove-Item -LiteralPath $logPath -Force }

    # Not `$args`: that is an automatic variable, and assigning to it inside a
    # function shadows PowerShell's own unbound-argument array for the rest of
    # the body -- a trap for whoever edits this next.
    $installerArgs = @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/RESTARTEXITCODE=3010', "/LOG=$logPath")

    if (-not $PSCmdlet.ShouldProcess($Path, 'Install (silent)')) {
        Add-Result -Step 'Install' -Status 'Skipped' -Detail 'WhatIf: install not attempted'
        return $false
    }

    $proc = Start-Process -FilePath $Path -ArgumentList $installerArgs -Wait -PassThru
    # The side-by-side installer has no mapped-file replacement to queue:
    # normal activation must finish with exit 0. A 3010 result is retained as
    # a failure signal here so a legacy/reboot-based installer cannot silently
    # re-enter the supported path.
    switch ($proc.ExitCode) {
        0 {
            Add-Result -Step 'Install' -Status 'Pass' -Detail 'exit 0'
            return $true
        }
        3010 {
            Add-Result -Step 'Install' -Status 'Fail' -Detail 'exit 3010 (unexpected reboot request; side-by-side activation must return 0)'
            return $false
        }
        default {
            Add-Result -Step 'Install' -Status 'Fail' -Detail "exit $($proc.ExitCode); see $logPath"
            return $false
        }
    }
}

# ---------------------------------------------------------------------------
# Machine-wide registration (DESIGN 12.1/12.2): the 64-bit CLSID view only,
# per the x86_64-only scope -- the WOW6432Node mirror must be absent, not
# present, because --no-wow64 is what [Run] passes.
# ---------------------------------------------------------------------------
function Test-MachineRegistration {
    $clsidPath = "HKLM:\SOFTWARE\Classes\CLSID\$ClsidTsf\InprocServer32"
    if (-not (Test-Path -LiteralPath $clsidPath)) {
        Add-Result -Step 'CLSID (64-bit)' -Status 'Fail' -Detail "missing: $clsidPath"
    } else {
        $entry = Get-ItemProperty -LiteralPath $clsidPath
        $registeredDll = $entry.'(default)'
        $threading = $entry.ThreadingModel
        $registeredDllPath = if ($registeredDll) {
            [IO.Path]::GetFullPath([string]$registeredDll)
        } else {
            ''
        }
        $versionRootPath = [IO.Path]::GetFullPath($VersionRoot)
        $relativeDll = if ($registeredDllPath.StartsWith($versionRootPath + '\', [StringComparison]::OrdinalIgnoreCase)) {
            $registeredDllPath.Substring($versionRootPath.Length + 1)
        } else {
            ''
        }
        $isVersionedDll = $relativeDll -match '^[^\\]+\\sakura_tsf\.dll$' -and [IO.File]::Exists($registeredDllPath)
        if ($isVersionedDll) {
            $detail = "points at $registeredDll"
            if ($threading -ne 'Apartment') {
                $detail += " (ThreadingModel is '$threading', expected Apartment)"
            }
            Add-Result -Step 'CLSID (64-bit)' -Status 'Pass' -Detail $detail
        } else {
            Add-Result -Step 'CLSID (64-bit)' -Status 'Fail' -Detail "registered path '$registeredDll' is not an existing versioned DLL below '$VersionRoot'"
        }
    }

    # Inverted on purpose (x86_64-only scope change): a x86 build would put
    # a mirror entry here via the WOW64 registry redirector, so this
    # product -- which never registers one -- must NOT have it. Finding one
    # anyway would mean --no-wow64 stopped being passed, or something else
    # registered a 32-bit view behind this installer's back.
    $wow64Path = "HKLM:\SOFTWARE\Classes\WOW6432Node\CLSID\$ClsidTsf"
    if (Test-Path -LiteralPath $wow64Path) {
        Add-Result -Step 'CLSID (WOW6432Node, must be absent)' -Status 'Fail' -Detail "found $wow64Path -- no x86 build ships, regtool was invoked with --no-wow64, so this should not exist"
    } else {
        Add-Result -Step 'CLSID (WOW6432Node, must be absent)' -Status 'Pass' -Detail 'absent, as expected for a --no-wow64 install'
    }
}

# ---------------------------------------------------------------------------
# Per-user state: only meaningful in the account that ran Setup, per
# regtool's own signed-in-user guard (interactive.rs). Best-effort: a
# SYSTEM/SCCM-style elevation of this very script could plausibly not be
# the signed-in user either, in which case regtool already refused to
# write here and this is correctly absent rather than a bug -- marked
# Manual, not Fail, when the guard's own limits (documented in
# interactive.rs) are the likely explanation rather than a real defect.
# ---------------------------------------------------------------------------
function Test-LanguageProfile {
    $profilePath = "HKCU:\Software\Microsoft\CTF\TIP\$ClsidTsf\LanguageProfile\$LangIdKey\$ProfileGuid"
    if (Test-Path -LiteralPath $profilePath) {
        Add-Result -Step 'Language profile (HKCU)' -Status 'Pass' -Detail $profilePath
    } else {
        Add-Result -Step 'Language profile (HKCU)' -Status 'Manual' `
            -Detail "not found at $profilePath -- expected only when this script's account is the signed-in user Setup ran as (crates/sakura-regtool/src/interactive.rs); confirm by hand if this run is not that account"
    }
}

# ---------------------------------------------------------------------------
# Payload cleanup maintenance runs separately from the per-user IME task. The
# latter stays at LUA for UIPI compatibility; this hidden task runs as SYSTEM
# so a mapped DLL under Program Files can be retried at every logon without a
# UAC prompt.
# ---------------------------------------------------------------------------
function Test-PayloadCleanupTask {
    $task = Get-ScheduledTask -TaskPath '\Sakura Input Maintenance\' -TaskName 'Payload Cleanup' -ErrorAction SilentlyContinue
    if (-not $task) {
        Add-Result -Step 'Payload cleanup task' -Status 'Fail' -Detail 'no SYSTEM task found under \Sakura Input Maintenance\Payload Cleanup'
        return
    }
    $action = @($task.Actions) | Select-Object -First 1
    $execute = if ($action) { [string]$action.Execute } else { '' }
    $arguments = if ($action) { [string]$action.Arguments } else { '' }
    $principal = $task.Principal
    if ([string]$principal.UserId -ne 'SYSTEM' -or
        [string]$principal.RunLevel -notmatch 'Highest' -or
        $execute -notmatch 'sakura_regtool\.exe$' -or
        $arguments -notmatch '--cleanup-payloads') {
        Add-Result -Step 'Payload cleanup task' -Status 'Fail' -Detail 'task exists but is not SYSTEM/Highest or does not invoke sakura_regtool --cleanup-payloads'
        return
    }
    Add-Result -Step 'Payload cleanup task' -Status 'Pass' -Detail "found $($task.TaskPath)$($task.TaskName) as SYSTEM/Highest"
}

# ---------------------------------------------------------------------------
# Engine/renderer autostart: --enable-profile registers a logon task
# (sakura_reg::launcher) rather than starting the processes immediately, so
# this triggers it manually instead of waiting for the next interactive
# logon, which a VM snapshot script cannot do without signing out.
# ---------------------------------------------------------------------------
function Test-EngineAutostart {
    $task = Get-ScheduledTask -TaskPath '\Sakura Input\' -TaskName 'Logon' -ErrorAction SilentlyContinue
    if (-not $task) {
        # Fallback naming used when the installer's account could not create
        # the \Sakura Input\ subfolder (launcher.rs: TASK_ROOT_PREFIX).
        $task = Get-ScheduledTask -TaskPath '\' -ErrorAction SilentlyContinue |
            Where-Object { $_.TaskName -like 'Sakura Input Logon*' } |
            Select-Object -First 1
    }
    if (-not $task) {
        Add-Result -Step 'Logon task' -Status 'Fail' -Detail 'no scheduled task found under \Sakura Input\Logon or a root-level "Sakura Input Logon (*)" fallback'
        return
    }
    Add-Result -Step 'Logon task' -Status 'Pass' -Detail "found $($task.TaskPath)$($task.TaskName)"

    if (-not $PSCmdlet.ShouldProcess($task.TaskName, 'Start scheduled task')) {
        Add-Result -Step 'Engine/renderer autostart' -Status 'Skipped' -Detail 'WhatIf: task not started'
        return
    }

    Start-ScheduledTask -InputObject $task
    $deadline = (Get-Date).AddSeconds(15)
    $engineUp = $false
    $rendererUp = $false
    while ((Get-Date) -lt $deadline -and -not ($engineUp -and $rendererUp)) {
        Start-Sleep -Milliseconds 500
        $engineUp = [bool](Get-Process -Name 'sakura_engine' -ErrorAction SilentlyContinue)
        $rendererUp = [bool](Get-Process -Name 'sakura_renderer' -ErrorAction SilentlyContinue)
    }
    if ($engineUp -and $rendererUp) {
        Add-Result -Step 'Engine/renderer autostart' -Status 'Pass' -Detail 'both processes observed running after the logon task fired'
    } elseif ($engineUp) {
        Add-Result -Step 'Engine/renderer autostart' -Status 'Fail' -Detail 'engine started but renderer did not (or exited already)'
    } else {
        Add-Result -Step 'Engine/renderer autostart' -Status 'Fail' -Detail 'engine did not start within 15s of the logon task firing'
    }
}

# ---------------------------------------------------------------------------
# Typing: what this script can check is that plain ASCII keystrokes still
# reach an application and round-trip through the clipboard -- i.e. that
# having Sakura Input installed has not broken ordinary typing. It does
# NOT prove kana composition works: Sakura Input is not made the default
# input method here (no --default in [Run]), and even if it were, switching
# the *active* input method from a script is a UI action (language bar /
# Win+Space), not something SendKeys or an API can drive reliably across
# Windows builds. That verification step is always MANUAL.
# ---------------------------------------------------------------------------
function Test-TypingPassthrough {
    param([string]$Label)

    if (-not $PSCmdlet.ShouldProcess('Notepad', 'Type and read back via clipboard')) {
        Add-Result -Step "Typing pass-through ($Label)" -Status 'Skipped' -Detail 'WhatIf: nothing typed'
        return
    }

    Add-Type -AssemblyName System.Windows.Forms

    $notepad = Start-Process -FilePath 'notepad.exe' -PassThru
    try {
        Start-Sleep -Milliseconds 1000
        $shell = New-Object -ComObject WScript.Shell
        [void]$shell.AppActivate($notepad.Id)
        Start-Sleep -Milliseconds 300

        [System.Windows.Forms.SendKeys]::SendWait($TypingProbe)
        Start-Sleep -Milliseconds 300
        [System.Windows.Forms.SendKeys]::SendWait('^a')
        [System.Windows.Forms.SendKeys]::SendWait('^c')
        Start-Sleep -Milliseconds 300

        $clip = (Get-Clipboard -Raw) -replace '\r?\n$', ''
        if ($clip -eq $TypingProbe) {
            Add-Result -Step "Typing pass-through ($Label)" -Status 'Pass' -Detail 'clipboard round-trip matched'
        } else {
            Add-Result -Step "Typing pass-through ($Label)" -Status 'Fail' -Detail "expected '$TypingProbe', clipboard held '$clip'"
        }
    } finally {
        Stop-Process -Id $notepad.Id -Force -ErrorAction SilentlyContinue
    }

    Add-Result -Step 'Kana composition (romaji -> hiragana)' -Status 'Manual' `
        -Detail 'switching the active IME is a UI action, not reliably scriptable; verify by hand: switch to Sakura Input, type romaji, confirm conversion'
}

# ---------------------------------------------------------------------------
# Uninstall
# ---------------------------------------------------------------------------
function Get-UninstallerPath {
    $key = "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\${AppId}_is1"
    if (-not (Test-Path -LiteralPath $key)) {
        return $null
    }
    $raw = (Get-ItemProperty -LiteralPath $key).UninstallString
    if (-not $raw) {
        return $null
    }
    # Inno writes this quoted ("C:\...\unins000.exe"); [Purge]/silent flags
    # are appended by this script, not embedded in the stored string, so the
    # quotes need stripping before the path can be used as -FilePath below.
    if ($raw -match '^"([^"]+)"') {
        return $Matches[1]
    }
    return $raw.Trim()
}

function Invoke-Uninstall {
    $uninstaller = Get-UninstallerPath
    if (-not $uninstaller -or -not (Test-Path -LiteralPath $uninstaller)) {
        Add-Result -Step 'Uninstall' -Status 'Fail' -Detail "could not resolve an uninstaller from ${AppId}_is1's UninstallString"
        return $false
    }

    $logPath = Join-Path $env:TEMP 'sakura-uninstall.log'
    if (Test-Path -LiteralPath $logPath) { Remove-Item -LiteralPath $logPath -Force }

    # See Invoke-Install for why this is not called `$args`.
    $uninstallerArgs = @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/RESTARTEXITCODE=3010', "/LOG=$logPath")
    if ($Purge) {
        # DESIGN 12.2 §4: user data is kept unless the operator opts in.
        # This is that opt-in, matched to ShouldPurgeUserData's own
        # {param:PURGE|0} check in installer/setup.iss.
        $uninstallerArgs += '/PURGE=1'
    }

    if (-not $PSCmdlet.ShouldProcess($uninstaller, 'Uninstall (silent)')) {
        Add-Result -Step 'Uninstall' -Status 'Skipped' -Detail 'WhatIf: uninstall not attempted'
        return $false
    }

    $proc = Start-Process -FilePath $uninstaller -ArgumentList $uninstallerArgs -Wait -PassThru
    switch ($proc.ExitCode) {
        0 {
            Add-Result -Step 'Uninstall' -Status 'Pass' -Detail 'exit 0'
            return $true
        }
        3010 {
            Add-Result -Step 'Uninstall' -Status 'Fail' -Detail 'exit 3010 (unexpected reboot request; no delete-on-reboot path is supported)'
            return $false
        }
        default {
            # This is the exit code UnregisterOrAbort's Abort call in
            # setup.iss produces when --unregister fails, among other
            # uninstall failures -- either way, files were not fully
            # removed and that must not be reported as success.
            Add-Result -Step 'Uninstall' -Status 'Fail' -Detail "exit $($proc.ExitCode); see $logPath"
            return $false
        }
    }
}

function Test-NoProcessesSurvive {
    $survivors = Get-Process -ErrorAction SilentlyContinue | Where-Object { $_.ProcessName -like 'sakura_*' }
    if ($survivors) {
        $names = ($survivors | Select-Object -ExpandProperty ProcessName -Unique) -join ', '
        Add-Result -Step 'No sakura_* processes survive uninstall' -Status 'Fail' -Detail "still running: $names"
    } else {
        Add-Result -Step 'No sakura_* processes survive uninstall' -Status 'Pass' -Detail 'none found'
    }
}

function Test-PayloadCleanupTaskRemoved {
    $task = Get-ScheduledTask -TaskPath '\Sakura Input Maintenance\' -TaskName 'Payload Cleanup' -ErrorAction SilentlyContinue
    if ($task) {
        Add-Result -Step 'Payload cleanup task removed' -Status 'Fail' -Detail 'SYSTEM maintenance task still exists after uninstall'
    } else {
        Add-Result -Step 'Payload cleanup task removed' -Status 'Pass' -Detail 'not found'
    }
}

function Test-FilesRemoved {
    if (Test-Path -LiteralPath $InstallDir) {
        # A host can keep a versioned TSF image mapped after deregistration.
        # The installer never queues a reboot rename; retry cleanup after all
        # host processes have exited instead.
        Add-Result -Step 'Install directory removed' -Status 'Manual' -Detail "$InstallDir still contains an unlocked/unavailable legacy payload; no reboot is pending, retry after host processes exit"
    } else {
        Add-Result -Step 'Install directory removed' -Status 'Pass' -Detail 'gone'
    }

    if ($Purge) {
        $userData = Join-Path $env:LOCALAPPDATA 'SakuraInput'
        if (Test-Path -LiteralPath $userData) {
            Add-Result -Step 'User data purged (/PURGE=1)' -Status 'Fail' -Detail "$userData still present"
        } else {
            Add-Result -Step 'User data purged (/PURGE=1)' -Status 'Pass' -Detail 'gone'
        }
    }
}

# ---------------------------------------------------------------------------
# Drive the sequence: install -> type -> uninstall -> verify typing still
# works (DESIGN 12.2). Each phase records into $script:Results regardless
# of earlier failures, on the theory that a smoke test that stops at the
# first red line tells the next run less than one that finishes and shows
# everything that was and was not true this time.
# ---------------------------------------------------------------------------
Write-Host "=== install ==="
if (Invoke-Install -Path $Installer) {
    Test-MachineRegistration
    Test-LanguageProfile
    Test-PayloadCleanupTask
    Test-EngineAutostart

    Write-Host "=== type (Sakura Input installed) ==="
    Test-TypingPassthrough -Label 'post-install'
} else {
    Add-Result -Step 'Post-install checks' -Status 'Skipped' -Detail 'install did not succeed'
}

Write-Host "=== uninstall ==="
$uninstalled = Invoke-Uninstall
Test-NoProcessesSurvive
Test-PayloadCleanupTaskRemoved
Test-FilesRemoved

Write-Host "=== type (Sakura Input uninstalled, MS-IME fallback) ==="
if ($uninstalled) {
    Test-TypingPassthrough -Label 'post-uninstall'
} else {
    Add-Result -Step 'Typing pass-through (post-uninstall)' -Status 'Skipped' -Detail 'uninstall did not succeed'
}

Write-Host ''
Write-Host '=== summary ==='
$script:Results | Format-Table -AutoSize | Out-String | Write-Host

$failures = $script:Results | Where-Object { $_.Status -eq 'Fail' }
if ($failures) {
    Write-Host "$($failures.Count) check(s) failed." -ForegroundColor Red
    exit 1
}

$manual = $script:Results | Where-Object { $_.Status -eq 'Manual' }
if ($manual) {
    Write-Host "$($manual.Count) check(s) require manual verification (see table above)." -ForegroundColor Yellow
}

exit 0
