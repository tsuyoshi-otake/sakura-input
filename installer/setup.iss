; Sakura Input installer (Inno Setup 6, x64compatible syntax).
;
; DESIGN.md 12 is authoritative for the install/uninstall/upgrade flow this
; script encodes; the comments below point at the specific subsection they
; implement rather than repeating it. Everything IME-specific -- the COM
; class, the TSF profile, the per-user input-list entry, the logon task --
; lives in sakura_regtool.exe (crates/sakura-regtool), built on the shared
; sakura_reg crate. This script's job is the commodity half: file copy with
; rollback, upgrade detection, ARP entry, in-use-file handling, and calling
; regtool in the order DESIGN 12.2 specifies (3.1's full-scratch rule
; deliberately excludes packaging tooling, which is why this file exists at
; all instead of a hand-rolled installer).
;
; Scope note: this product targets Windows 11 on x86_64 only. There is no
; x86 or ARM64/ARM64X build to package, so this script never touches the
; wow64 payload directory regtool's --wow64-dll default would otherwise
; look for -- see the --register line in [Run] for what that means in
; practice.

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

#ifndef AppBuildId
#define AppBuildId "dev"
#endif
#define AppProductVersion "1.0.0"
#ifndef AppVersionedDir
#define AppVersionedDir "{app}\versions\1.0.0-dev"
#endif
; Neural reranking is an explicit release-pipeline opt-in. Keeping the default
; off lets normal installer builds remain independent of the optional native
; worker and large model. A build that opts in must include the generated,
; hash-pinned payload manifest below; a missing or stale manifest is a compile
; failure, never a partial installer.
#ifndef IncludeNeuralReranker
#define IncludeNeuralReranker 0
#endif
#if IncludeNeuralReranker
#include "..\artifacts\release\neural\deberta-v2-tiny-japanese-char-wwm\manifest.iss"
#if NeuralPayloadCount != 8
#error Neural reranker installer manifest must describe exactly eight payload files
#endif
#endif

[Setup]
; This value is burned into every installed machine's upgrade detection
; (AppId is what Inno's "is this an upgrade of the same product" check
; keys on) and into the Add/Remove Programs registry key name. Regenerating
; it would make every existing install look like a different, unrelated
; product on the next release -- an "upgrade" that is actually a silent
; side-by-side install with two competing TSF registrations fighting over
; the same CLSID. Generated once, kept forever.
AppId={{61D379C9-27DE-45E4-93B9-5871CB71A0CF}
AppName=Sakura Input
; Kept in step with [workspace.package].version in the repository's
; Cargo.toml by hand for now; a release pipeline that reads Cargo.toml is
; out of scope for this v0 script.
AppVersion={#AppProductVersion}
AppPublisher=Sakura Input contributors
AppPublisherURL=https://github.com/tsuyoshi-otake/sakura-input
DefaultDirName={autopf}\Sakura Input
; A resident text service is not something a user launches by hand, so
; there is nothing worth putting in the Start Menu; [Icons] is omitted
; entirely rather than shipping a shortcut to nothing.
PrivilegesRequired=admin
; Inno 6.3's replacement for the older, narrower "x64" architecture
; identifier: it also matches x64 processes running under Windows-on-Arm
; emulation, which plain "x64" does not. ARM64 and x86 are out of scope for
; this product (see the scope note above), so x64compatible is the only
; value listed for either directive.
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Windows 11's first public build. Earlier Windows releases are not a
; supported configuration for this product -- not merely an untested one --
; so Setup refuses to run there instead of leaving a TSF registration on a
; machine this project has never targeted. This is one of two install-time
; preconditions DESIGN 3.2/12.2 name explicitly; the other is the AVX check
; in InitializeSetup below, and both exist for the same reason: an
; unsupported configuration should get a clear refusal before any file is
; copied, not a subtly broken IME afterward.
MinVersion=10.0.22000
OutputDir=out
OutputBaseFilename=sakura_setup
Compression=lzma2
SolidCompression=yes
UninstallDisplayIcon={app}\sakura_settings.exe
; The TSF DLL is installed into a unique version directory and the registry is
; switched to it after the copy. A host process may keep an older version
; loaded, but the installer never overwrites that mapped image and therefore
; does not need a reboot to activate the new registration.
RestartIfNeededByRun=no

[Files]
; Every runtime payload is copied below a release/build-specific directory.
; The active version is selected only after all files are present, by the
; explicit --dll registration command in [Run]. This is the lock-safe part of
; the upgrade protocol: a host may keep any older TSF image mapped indefinitely
; while the new image is copied and registered beside it.
; .cargo/config.toml pins the workspace to the x86_64-pc-windows-msvc
; target (DESIGN 3.2), so release artifacts land under that target's own
; subdirectory rather than the bare target\release\ a host-triple-less
; build would use.
Source: "..\target\x86_64-pc-windows-msvc\release\sakura_tsf.dll"; DestDir: "{#AppVersionedDir}"; Flags: ignoreversion

; The engine and renderer are launched from the active version directory by
; sakura_logon.exe. They are copied beside the DLL, so stopping the old
; engine is still useful for a clean hand-off but is not required to replace a
; loaded image. The root executables are stable bootstraps and are kept out of
; the version directory so tasks and shortcuts do not change on every update.
; The settings bootstrap dispatches to the versioned payload.
Source: "..\target\x86_64-pc-windows-msvc\release\sakura_engine.exe"; DestDir: "{#AppVersionedDir}"; Flags: ignoreversion
Source: "..\target\x86_64-pc-windows-msvc\release\sakura_renderer.exe"; DestDir: "{#AppVersionedDir}"; Flags: ignoreversion
Source: "..\target\x86_64-pc-windows-msvc\release\sakura_regtool.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\x86_64-pc-windows-msvc\release\sakura_logon.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\x86_64-pc-windows-msvc\release\sakura_settings.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\x86_64-pc-windows-msvc\release\sakura_settings_payload.exe"; DestDir: "{#AppVersionedDir}"; Flags: ignoreversion

; The engine maps exactly one shared read-only image from this subdirectory.
; Each version owns its dictionary, so an older engine can finish using its
; image while the new registration is already active.
Source: "..\artifacts\release\system.dic"; DestDir: "{#AppVersionedDir}\dict"; Flags: ignoreversion

; Licenses and the Japanese operator guide are payload, not release-page-only
; links: the notices must remain available offline beside the derived data
; they govern.
Source: "..\LICENSE"; DestDir: "{#AppVersionedDir}\docs"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{#AppVersionedDir}\docs"; DestName: "README-ja.md"; Flags: ignoreversion
Source: "..\docs\guide-ja.md"; DestDir: "{#AppVersionedDir}\docs"; Flags: ignoreversion
Source: "..\THIRD_PARTY_NOTICES.md"; DestDir: "{#AppVersionedDir}\licenses"; Flags: ignoreversion
Source: "..\THIRD_PARTY_LICENSES\mozc-dictionary.txt"; DestDir: "{#AppVersionedDir}\licenses"; Flags: ignoreversion
Source: "..\THIRD_PARTY_LICENSES\smile-chat-public-MIT.txt"; DestDir: "{#AppVersionedDir}\licenses"; Flags: ignoreversion

#if IncludeNeuralReranker
; The worker resolves onnxruntime.dll beside its executable. The model, tokenizer
; and JSON manifest live in the engine's fixed sibling directory; no root-level
; mutable payload or network download is involved. manifest.iss is compile-time
; evidence only and is deliberately not installed.
Source: "..\artifacts\release\sakura_neural_worker.exe"; DestDir: "{#AppVersionedDir}"; Flags: ignoreversion
Source: "..\artifacts\release\onnxruntime.dll"; DestDir: "{#AppVersionedDir}"; Flags: ignoreversion
Source: "..\artifacts\release\neural\deberta-v2-tiny-japanese-char-wwm\model.onnx"; DestDir: "{#AppVersionedDir}\neural\deberta-v2-tiny-japanese-char-wwm"; Flags: ignoreversion
Source: "..\artifacts\release\neural\deberta-v2-tiny-japanese-char-wwm\vocab.txt"; DestDir: "{#AppVersionedDir}\neural\deberta-v2-tiny-japanese-char-wwm"; Flags: ignoreversion
Source: "..\artifacts\release\neural\deberta-v2-tiny-japanese-char-wwm\manifest.json"; DestDir: "{#AppVersionedDir}\neural\deberta-v2-tiny-japanese-char-wwm"; Flags: ignoreversion
Source: "..\artifacts\release\licenses\onnxruntime-MIT.txt"; DestDir: "{#AppVersionedDir}\licenses"; Flags: ignoreversion
Source: "..\artifacts\release\licenses\onnxruntime-ThirdPartyNotices.txt"; DestDir: "{#AppVersionedDir}\licenses"; Flags: ignoreversion
Source: "..\artifacts\release\licenses\ku-nlp-deberta-v2-tiny-japanese-char-wwm.txt"; DestDir: "{#AppVersionedDir}\licenses"; Flags: ignoreversion
#endif

[Run]
; Machine-wide registration (DESIGN 12.1/12.2): COM class in both registry
; views, TSF category claims, and the ja-JP language profile, in that order
; (register_all in crates/sakura-reg/src/lib.rs). The versioned DLL path and
; --no-wow64 are passed explicitly rather than left to regtool's defaults:
; Auto probes
; for x86\sakura_tsf.dll beside the executable and silently registers
; without one if it is absent, reporting success either way -- the right
; behaviour for a product that *might* ship an x86 build and sometimes
; doesn't. This product never does: x86_64 is the only supported
; architecture, so relying on Auto here would launder a packaging omission
; into a quiet, indistinguishable success. Saying --no-wow64 makes "no
; WOW64 text service on this machine" a decision this script made on
; purpose rather than a fact regtool happened to discover; the practical
; effect either way is the same -- 32-bit host applications have no Sakura
; Input entry and fall back to MS-IME.
Filename: "{app}\sakura_regtool.exe"; Parameters: "--register --dll ""{#AppVersionedDir}\sakura_tsf.dll"" --no-wow64"; Flags: runhidden waituntilterminated; Check: RegisterActivePayload; StatusMsg: "Registering the Sakura Input text service..."

; The resident per-user logon task intentionally runs at LUA so it can talk to
; normal-integrity applications. Payload deletion needs administrator rights,
; so a separate hidden SYSTEM task retries it for every logon without showing a
; UAC prompt or elevating the IME engine itself.
Filename: "{app}\sakura_regtool.exe"; Parameters: "--install-cleanup-task"; Flags: runhidden waituntilterminated; Check: InstallCleanupTaskOrAbort; StatusMsg: "Installing payload cleanup maintenance..."

; Local crash dumps remain on the machine and are never uploaded. Every Sakura
; executable shares the per-user dump directory; WER enforces DumpCount=5 for
; each image and the engine's logon maintenance additionally prunes the shared
; directory. This command is machine-wide and therefore runs before dropping
; back to the original user's token below.
Filename: "{app}\sakura_regtool.exe"; Parameters: "--configure-diagnostics"; Flags: runhidden waituntilterminated; StatusMsg: "Configuring bounded local crash diagnostics..."

; Per-user: adds Sakura Input to this account's input list, ensures the stable
; logon task exists, and runs that same stable bootstrap once for the current
; desktop (user_profile::enable + launcher::register_if_missing +
; sakura_logon.exe). An existing task is preserved across updates; only a
; missing task is created. Starting the bootstrap here is required because an
; update stops the old engine before switching payloads, while a logon task does
; not run again until the next sign-in. This must land in the *signed-in* user's HKCU,
; never the elevated installer's -- under "run as different user",
; SCCM/Intune, or a SYSTEM deployment, the elevated process's HKCU is a
; different hive, and writing there would enable the IME for an account
; nobody is using while install still reports success (DESIGN 12.1).
; runasoriginaluser is Inno's flag for exactly this: it runs the command as
; the account that launched Setup, with whatever UAC elevation was granted
; stripped back off.
;
; It does not cover every deployment shape. A SYSTEM-driven install with no
; interactive user signed in at all has no "original user" to drop back to,
; and in that case regtool's own guard (require_signed_in_user in
; crates/sakura-regtool/src/interactive.rs) refuses rather than silently
; guessing whose HKCU to write into -- this line fails there, on purpose,
; instead of enabling the IME for the wrong account. The complete answer
; for that case is the logon-stub self-repair DESIGN 12.1 describes and
; PLAN.md schedules as an M4 (Phase 5) exit criterion: a task that
; re-applies per-user registration at every sign-in until it sticks. Phase
; 1's regtool already provides the primitives that stub will need --
; --enable-profile is safe to call repeatedly, and the launcher checks whether
; the stable task already exists before attempting a write. It then waits for
; sakura_logon.exe and propagates that helper's exit bitmask, so success means
; the task/profile repair and both process launches completed -- but
; sakura_logon.exe is now that stub: the task launches it first, it reapplies
; both the task definition and the input-list entry, then starts the engine and
; renderer. Every failed step is reflected in its exit bitmask and status file.
Filename: "{app}\sakura_regtool.exe"; Parameters: "--enable-profile"; Flags: runhidden waituntilterminated runasoriginaluser; StatusMsg: "Adding Sakura Input to your input methods..."

[UninstallRun]
; The safety-critical maintenance-task removal and TSF deregistration run from
; CurUninstallStepChanged(usUninstall), before this section and before file
; removal. They cannot be expressed as side-effecting Check functions here:
; Inno evaluates [UninstallRun] Check parameters while installing the uninstall
; log, which would remove live state during an ordinary upgrade. The event
; handler inspects both exit codes, compensates by restoring maintenance when
; deregistration fails, and calls Abort while that call is documented to stop
; Uninstall.

; Best effort, and deliberately not gated the way --unregister is above: by
; the time execution reaches here the profile is already withdrawn, so a
; stuck engine process is untidy -- it can keep a versioned payload locked
; after the uninstaller has removed registration -- but it is not the brick
; scenario. Failing the whole uninstall over a process that would not exit
; would trade a minor annoyance for a much larger one.
Filename: "{app}\sakura_regtool.exe"; Parameters: "--stop"; Flags: runhidden waituntilterminated; RunOnceId: "SakuraStop"

; Policy removal is best effort and intentionally does not delete dump files.
; Those may contain sensitive composition memory and remain subject to the
; user's explicit /PURGE=1 choice, like every other per-user data file.
Filename: "{app}\sakura_regtool.exe"; Parameters: "--remove-diagnostics"; Flags: runhidden waituntilterminated; RunOnceId: "SakuraRemoveDiagnostics"

[UninstallDelete]
; The active version is selected through registration, not through the
; directory name. Once registration has been removed, deleting every
; versioned payload is safe when no host process still has one mapped. Locked
; files are simply left behind for a later retry; no reboot rename is queued.
Type: filesandordirs; Name: "{app}\versions"
; Remove payloads from installs made before the side-by-side layout existed.
Type: files; Name: "{app}\sakura_tsf.dll"
Type: files; Name: "{app}\sakura_engine.exe"
Type: files; Name: "{app}\sakura_renderer.exe"
Type: filesandordirs; Name: "{app}\dict"
Type: filesandordirs; Name: "{app}\docs"
Type: filesandordirs; Name: "{app}\licenses"

[Code]

// DESIGN 3.2: the whole workspace is compiled with
// -C target-feature=+avx,+ssse3 (.cargo/config.toml). The 128-bit width
// scanner uses SSSE3 `pshufb`, so AVX + SSSE3 is a compatibility floor, not a
// branch. A CPU missing either does not get a graceful fallback: it gets an
// illegal-instruction fault the first time any process loads
// sakura_tsf.dll. That would present to the user as their applications
// crashing, with nothing pointing at the IME as the cause. AVX and SSSE3
// shipped before the Windows 11 hardware baseline and are present on every CPU
// Microsoft supports for Windows 11, so this check should never actually
// fire on a machine that also passes the MinVersion gate above -- it
// exists for the narrow gap between "Windows 11 is running" and "on this
// exact hardware", e.g. a VM whose hypervisor does not pass AVX through.
function IsProcessorFeaturePresent(Feature: Cardinal): Boolean;
  external 'IsProcessorFeaturePresent@kernel32.dll stdcall';

const
  PF_SSSE3_INSTRUCTIONS_AVAILABLE = 36;
  PF_AVX_INSTRUCTIONS_AVAILABLE = 39;
  UNINSTALL_TEARDOWN_NOT_STARTED = 0;
  UNINSTALL_TEARDOWN_RUNNING = 1;
  UNINSTALL_TEARDOWN_COMPLETE = 2;
  UNINSTALL_TEARDOWN_FAILED = 3;

var
  UninstallTeardownState: Integer;

#if IncludeNeuralReranker
function VerifyNeuralPayloadFile(const RelativePath, ExpectedSha256: String;
  const ExpectedBytes: Int64): Boolean;
var
  Path: String;
  ActualBytes: Int64;
begin
  Path := ExpandConstant('{#AppVersionedDir}\') + RelativePath;
  Result := FileExists(Path) and FileSize64(Path, ActualBytes) and
    (ActualBytes = ExpectedBytes) and
    (CompareText(GetSHA256OfFile(Path), ExpectedSha256) = 0);
  if not Result then
    Log('neural reranker payload verification failed: ' + RelativePath);
end;

// This runs after [Files] copied the new side-by-side generation but before
// registration switches Windows to it. The generated .iss constants are tied
// exactly to the JSON manifest by build-neural-reranker.ps1, so a missing,
// truncated, swapped, or stale worker/runtime/model/tokenizer/license payload
// leaves the existing text-service registration untouched.
function VerifyNeuralPayloadOrAbort(): Boolean;
begin
  Result :=
    VerifyNeuralPayloadFile('{#NeuralPayload0Path}', '{#NeuralPayload0Sha256}', {#NeuralPayload0Bytes}) and
    VerifyNeuralPayloadFile('{#NeuralPayload1Path}', '{#NeuralPayload1Sha256}', {#NeuralPayload1Bytes}) and
    VerifyNeuralPayloadFile('{#NeuralPayload2Path}', '{#NeuralPayload2Sha256}', {#NeuralPayload2Bytes}) and
    VerifyNeuralPayloadFile('{#NeuralPayload3Path}', '{#NeuralPayload3Sha256}', {#NeuralPayload3Bytes}) and
    VerifyNeuralPayloadFile('{#NeuralPayload4Path}', '{#NeuralPayload4Sha256}', {#NeuralPayload4Bytes}) and
    VerifyNeuralPayloadFile('{#NeuralPayload5Path}', '{#NeuralPayload5Sha256}', {#NeuralPayload5Bytes}) and
    VerifyNeuralPayloadFile('{#NeuralPayload6Path}', '{#NeuralPayload6Sha256}', {#NeuralPayload6Bytes}) and
    VerifyNeuralPayloadFile('{#NeuralPayload7Path}', '{#NeuralPayload7Sha256}', {#NeuralPayload7Bytes});
  if not Result then
  begin
    SuppressibleMsgBox(
      'Sakura Input could not verify the optional neural reranker payload. ' +
      'The existing text-service registration was left unchanged.',
      mbCriticalError, MB_OK, IDOK);
    Abort;
  end;
end;
#endif

// Runs before Setup shows its first wizard page, i.e. before anything in
// [Files] is touched. Deliberately checks the full AVX+SSSE3 compatibility
// floor, but not AVX2 or AVX-512: those concrete strategies are resolved at
// startup by sakura-core's CPU-dispatch code (DESIGN 3.2), precisely so that
// machines without them still work.
function InitializeSetup(): Boolean;
begin
  Result := IsProcessorFeaturePresent(PF_AVX_INSTRUCTIONS_AVAILABLE) and
            IsProcessorFeaturePresent(PF_SSSE3_INSTRUCTIONS_AVAILABLE);
  if not Result then
    MsgBox(
      'This CPU does not support the AVX + SSSE3 baseline required by ' +
      'Sakura Input (Intel Sandy Bridge / AMD Bulldozer, 2011, or later). ' +
      'Installing anyway would let the text service crash every application ' +
      'that loads it instead of failing here, cleanly, before anything is copied.',
      mbCriticalError, MB_OK);
end;

function RegToolPath(): String;
begin
  Result := ExpandConstant('{app}\sakura_regtool.exe');
end;

// The [Run] entry below is intentionally declarative for auditability, but
// Check owns the actual command so a registration failure aborts installation
// before a new version can be considered active. The DLL path is explicit:
// the root regtool is a stable bootstrap and must never infer an old root-level
// payload beside itself.
procedure CleanupObsoletePayload();
var
  ResultCode: Integer;
  Launched: Boolean;
begin
  // Use the same guarded implementation as the SYSTEM logon task. It resolves
  // the active generation from COM registration, requires that generation to
  // be a direct child of a non-reparse-point versions directory, ignores
  // unrecognized entries, and treats mapped DLLs as a retryable "kept" state.
  // Keeping this best effort is deliberate: registration has already switched
  // to the complete new payload, so aborting here could leave an external COM
  // registration change pointing at files that Inno rolls back.
  Launched := Exec(RegToolPath(), '--cleanup-payloads', ExpandConstant('{app}'),
    SW_HIDE, ewWaitUntilTerminated, ResultCode);
  if Launched and (ResultCode = 0) then
    Log('completed guarded Sakura Input payload cleanup')
  else
    Log('guarded payload cleanup was deferred; the SYSTEM logon task will retry');
end;

function RegisterActivePayload(): Boolean;
var
  ResultCode: Integer;
  Launched: Boolean;
  Parameters: String;
begin
#if IncludeNeuralReranker
  VerifyNeuralPayloadOrAbort();
#endif
  Parameters := '--register --dll "' +
    ExpandConstant('{#AppVersionedDir}\sakura_tsf.dll') + '" --no-wow64';
  Launched := Exec(RegToolPath(), Parameters, ExpandConstant('{app}'), SW_HIDE,
    ewWaitUntilTerminated, ResultCode);
  if (not Launched) or (ResultCode <> 0) then
  begin
    SuppressibleMsgBox(
      'Sakura Input could not activate the new text service version ' +
      ExpandConstant('{#AppProductVersion}') +
      '. Installation has stopped before the previous registration was ' +
      'discarded. Check the installer log and try again.',
      mbCriticalError, MB_OK, IDOK);
    Abort;
  end;
  CleanupObsoletePayload();
  // The command was already executed above; prevent [Run] from executing it
  // a second time after Check returns.
  Result := False;
end;

function InstallCleanupTaskOrAbort(): Boolean;
var
  ResultCode: Integer;
  Launched: Boolean;
begin
  Launched := Exec(RegToolPath(), '--install-cleanup-task',
    ExpandConstant('{app}'), SW_HIDE, ewWaitUntilTerminated, ResultCode);
  if (not Launched) or (ResultCode <> 0) then
  begin
    SuppressibleMsgBox(
      'Sakura Input could not install its automatic payload cleanup task. ' +
      'Installation has stopped so future updates do not accumulate locked ' +
      'payload generations without a retry path.',
      mbCriticalError, MB_OK, IDOK);
    Abort;
  end;
  Log('installed and read-back verified Sakura Input payload cleanup task');
  Result := False;
end;

function RestoreCleanupTask(): Boolean;
var
  ResultCode: Integer;
  Launched: Boolean;
begin
  Launched := Exec(RegToolPath(), '--install-cleanup-task',
    ExpandConstant('{app}'), SW_HIDE, ewWaitUntilTerminated, ResultCode);
  Result := Launched and (ResultCode = 0);
  if Result then
    Log('restored Sakura Input payload cleanup task after aborted uninstall')
  else
    Log('could not restore Sakura Input payload cleanup task after aborted uninstall');
end;

// Performs the irreversible uninstall boundary while every installed binary
// still exists. Each branch reaches an explicit terminal state: success permits
// normal file removal, and every failure restores the maintenance retry when
// possible before Abort prevents file removal.
procedure TeardownRegistrationOrAbort();
var
  ResultCode: Integer;
  Launched: Boolean;
  CleanupRestored: Boolean;
  FailureText: String;
begin
  if UninstallTeardownState = UNINSTALL_TEARDOWN_COMPLETE then
    exit;
  if UninstallTeardownState <> UNINSTALL_TEARDOWN_NOT_STARTED then
  begin
    UninstallTeardownState := UNINSTALL_TEARDOWN_FAILED;
    SuppressibleMsgBox(
      'Sakura Input detected an incomplete uninstall teardown. No program ' +
      'files were removed. Run Uninstall again from an elevated account.',
      mbCriticalError, MB_OK, IDOK);
    Abort;
  end;

  UninstallTeardownState := UNINSTALL_TEARDOWN_RUNNING;
  if not FileExists(RegToolPath()) then
  begin
    UninstallTeardownState := UNINSTALL_TEARDOWN_FAILED;
    SuppressibleMsgBox(
      'Sakura Input cannot safely unregister because sakura_regtool.exe is ' +
      'missing. No additional program files were removed. Repair or reinstall ' +
      'Sakura Input, then run Uninstall again.',
      mbCriticalError, MB_OK, IDOK);
    Abort;
  end;

  Launched := Exec(RegToolPath(), '--remove-cleanup-task',
    ExpandConstant('{app}'), SW_HIDE, ewWaitUntilTerminated, ResultCode);
  if (not Launched) or (ResultCode <> 0) then
  begin
    CleanupRestored := RestoreCleanupTask();
    UninstallTeardownState := UNINSTALL_TEARDOWN_FAILED;
    FailureText :=
      'Sakura Input could not remove its automatic payload cleanup task. ' +
      'Uninstallation stopped before text-service registration or program ' +
      'files were removed.';
    if not CleanupRestored then
      FailureText := FailureText +
        ' The maintenance task also could not be restored; reinstall Sakura ' +
        'Input before retrying Uninstall.';
    SuppressibleMsgBox(FailureText, mbCriticalError, MB_OK, IDOK);
    Abort;
  end;
  Log('removed Sakura Input payload cleanup task for uninstall');

  Launched := Exec(RegToolPath(), '--unregister', ExpandConstant('{app}'),
    SW_HIDE, ewWaitUntilTerminated, ResultCode);
  if (not Launched) or (ResultCode <> 0) then
  begin
    CleanupRestored := RestoreCleanupTask();
    UninstallTeardownState := UNINSTALL_TEARDOWN_FAILED;
    // SuppressibleMsgBox, not MsgBox: a silent uninstall (/SUPPRESSMSGBOXES,
    // DESIGN 12.4) must never block on a dialog nobody is there to click.
    // Under that flag this returns immediately with the default answer
    // instead of hanging the uninstaller forever.
    FailureText :=
      'Sakura Input could not remove its text service registration ' +
      '(sakura_regtool.exe --unregister failed). Uninstall has stopped ' +
      'here on purpose: deleting the program files now would leave ' +
      'Windows pointing at a text service whose files no longer exist, ' +
      'which breaks typing in every application until it is fixed by ' +
      'hand. Run sakura_regtool.exe --unregister from an elevated ' +
      'command prompt, then try Uninstall again.';
    if not CleanupRestored then
      FailureText := FailureText +
        ' The payload cleanup task could not be restored either; reinstall ' +
        'Sakura Input before retrying Uninstall.';
    SuppressibleMsgBox(FailureText, mbCriticalError, MB_OK, IDOK);
    // Stops the uninstall outright -- rolling back is not the point here
    // (nothing has been removed yet); refusing to go any further is.
    Abort;
  end;
  Log('removed Sakura Input text service registration for uninstall');
  UninstallTeardownState := UNINSTALL_TEARDOWN_COMPLETE;
end;

// Runs before any file in [Files] is copied, on both a fresh install and an
// upgrade. Stopping the old engine makes the process hand-off deterministic;
// it is not used to make a loaded TSF DLL overwriteable because the new DLL is
// in a different directory. Both scheduled tasks deliberately remain
// registered throughout an upgrade: their stable root action paths stay valid,
// and preserving them ensures a canceled or failed installer cannot remove
// either normal startup or the next-logon payload-cleanup retry. A fresh install
// has no previous regtool.exe to call yet, which is not an error.
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  ResultCode: Integer;
  ExistingRegTool: String;
begin
  Result := '';
  ExistingRegTool := RegToolPath();
  if FileExists(ExistingRegTool) then
  begin
    // A failure to stop is not fatal to the upgrade: the new version is
    // side-by-side and can be copied even while the old engine is alive.
    // Recorded in /LOG rather than acted on further; the old process will keep
    // using its already-selected version until it exits.
    Exec(ExistingRegTool, '--stop', ExpandConstant('{app}'), SW_HIDE,
      ewWaitUntilTerminated, ResultCode);
  end;
end;

// DESIGN 12.2: user data under %LOCALAPPDATA% is kept by default and
// removed only on explicit opt-in. /PURGE=1 is that opt-in for v0; a
// checkbox on the uninstall page is future work, since Inno's uninstall
// wizard does not host custom pages as readily as the install wizard does,
// and the command-line switch alone already satisfies 12.4's "opt-in, not
// automatic" requirement.
function ShouldPurgeUserData(): Boolean;
begin
  Result := CompareText(ExpandConstant('{param:PURGE|0}'), '1') = 0;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
    TeardownRegistrationOrAbort();

  // Runs after Setup's own file and registry removal (usPostUninstall),
  // not interleaved with [UninstallRun]: a purge failure here must never
  // be mistaken for the registration teardown above having failed, and
  // user data cleanup is independent of both steps.
  if (CurUninstallStep = usPostUninstall) and ShouldPurgeUserData() then
    DelTree(ExpandConstant('{localappdata}\SakuraInput'), True, True, True);
end;
