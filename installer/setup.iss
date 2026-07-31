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
AppVersion=0.1.0
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
UninstallDisplayIcon={app}\sakura_renderer.exe
; Governs whether an *interactive* run offers to restart immediately once a
; pending file rename (see restartreplace below) is queued. Left at its
; default (yes) deliberately: turning it off would mean the DLL swap
; silently waits for whenever the machine next reboots on its own -- which
; could be days -- during which the new engine/renderer negotiate with the
; still-loaded old DLL across a gap far larger than DESIGN 12.3
; anticipates. Silent installs are unaffected either way; they always
; honor /NORESTART plus /RESTARTEXITCODE=3010 instead (12.4), which is how
; scripts -- including scripts/vm-smoke.ps1 -- tell a normal
; reboot-pending completion apart from an actual failure.
RestartIfNeededByRun=yes

[Files]
; The text service DLL is mapped into every host process that has ever
; focused a text field for as long as that process keeps running (DESIGN
; 12.3), so it can be loaded -- and therefore locked -- at the exact moment
; Setup wants to overwrite it. restartreplace queues the swap with
; MoveFileEx instead of failing the copy outright, and the reboot that
; performs it is the *normal* completion of an install or upgrade, not a
; fallback path. uninsrestartdelete gives uninstall the same treatment:
; without it, uninstalling on a machine where any host process still has
; the old DLL open would leave a stray file the uninstaller silently
; failed to remove.
; .cargo/config.toml pins the workspace to the x86_64-pc-windows-msvc
; target (DESIGN 3.2), so release artifacts land under that target's own
; subdirectory rather than the bare target\release\ a host-triple-less
; build would use.
Source: "..\target\x86_64-pc-windows-msvc\release\sakura_tsf.dll"; DestDir: "{app}"; Flags: restartreplace uninsrestartdelete

; The four executables are not memory-mapped into anyone else's process, so
; there is nothing to queue for them: PrepareToInstall (below) stops the
; engine before Setup reaches this section, which is what makes a plain
; overwrite of their .exe files safe (DESIGN 12.3, "the engine is stopped,
; so nothing maps them"). ignoreversion is stated explicitly rather than
; relied on as a default: none of these binaries carries Win32
; version-resource metadata (the dependency policy in DESIGN 3.1 excludes
; the crates that would embed one), so Inno's normal version comparison has
; nothing to compare, and this line says so instead of depending on
; unstated default behaviour.
Source: "..\target\x86_64-pc-windows-msvc\release\sakura_engine.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\x86_64-pc-windows-msvc\release\sakura_renderer.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\x86_64-pc-windows-msvc\release\sakura_regtool.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\x86_64-pc-windows-msvc\release\sakura_settings.exe"; DestDir: "{app}"; Flags: ignoreversion

; No dict\system.dic yet: the shared dictionary image is Phase 2 work
; (DESIGN 6.1/6.4). Nothing in [Run] or sakura_regtool references one
; either; adding a line for it here before dictc exists would just fail
; the build.

[Run]
; Machine-wide registration (DESIGN 12.1/12.2): COM class in both registry
; views, TSF category claims, and the ja-JP language profile, in that order
; (register_all in crates/sakura-reg/src/lib.rs). --no-wow64 is passed
; explicitly rather than left to regtool's Wow64::Auto default: Auto probes
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
Filename: "{app}\sakura_regtool.exe"; Parameters: "--register --no-wow64"; Flags: runhidden waituntilterminated; StatusMsg: "Registering the Sakura Input text service..."

; Per-user: adds Sakura Input to this account's input list and registers
; the logon task that starts the engine (user_profile::enable +
; launcher::register). This must land in the *signed-in* user's HKCU,
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
; --enable-profile is safe to call repeatedly, and
; sakura_reg::launcher::is_registered exists for it to check first -- but
; the stub itself, and wiring it into this installer, is later work, not
; v0.
Filename: "{app}\sakura_regtool.exe"; Parameters: "--enable-profile"; Flags: runhidden waituntilterminated runasoriginaluser; StatusMsg: "Adding Sakura Input to your input methods..."

[UninstallRun]
; Ordering here is a safety property, not a preference (DESIGN 12.2): the
; language profile -- the thing that lets Windows *try to activate* this
; text service -- has to be gone before the DLL it points at is deleted
; from disk, or a host process that activates it in the gap between those
; two events finds a class with nowhere to load from. Inno already runs
; every [UninstallRun] entry before any file is removed, which is the
; ordering this needs; nothing here changes that guarantee.
;
; The command that actually executes lives in Check, not in the
; declarative Filename/Parameters below -- those exist so a reader of this
; file sees the real ordering and the real command at a glance without
; having to read Pascal. Inno's Run/UninstallRun mechanism has no native
; way to stop the surrounding uninstall because an entry's exit code was
; nonzero: by default a failing entry is just a line in the log, and file
; removal proceeds regardless. UnregisterOrAbort (see [Code]) runs the
; command itself, inspects the real exit code, and calls Abort on failure
; -- which does stop the uninstall before anything is deleted. Continuing
; past a failed deregistration here is precisely the brick scenario DESIGN
; 12.2 names: a live profile left pointing at a DLL that file removal is
; about to erase. Check always returns False afterward so Setup does not
; then also launch Filename/Parameters itself and run --unregister a
; second, redundant time.
Filename: "{app}\sakura_regtool.exe"; Parameters: "--unregister"; Flags: runhidden waituntilterminated; Check: UnregisterOrAbort

; Best effort, and deliberately not gated the way --unregister is above: by
; the time execution reaches here the profile is already withdrawn, so a
; stuck engine process is untidy -- it keeps its files locked until
; uninsrestartdelete cleans them up at the next reboot -- but it is not the
; brick scenario. Failing the whole uninstall over a process that would not
; exit would trade a minor annoyance for a much larger one.
Filename: "{app}\sakura_regtool.exe"; Parameters: "--stop"; Flags: runhidden waituntilterminated

[Code]

// DESIGN 3.2: the whole workspace is compiled with -C target-feature=+avx
// (.cargo/config.toml), so 128- and 256-bit vector code in sakura-core's
// width normalizer needs no run-time guard. That is a floor, not a branch
// -- which means a CPU without AVX does not get a graceful fallback, it
// gets an illegal-instruction fault the first time any process loads
// sakura_tsf.dll. That would present to the user as their applications
// crashing, with nothing pointing at the IME as the cause. AVX shipped in
// Sandy Bridge (2011) / Bulldozer (2011) and is present on every CPU
// Microsoft supports for Windows 11, so this check should never actually
// fire on a machine that also passes the MinVersion gate above -- it
// exists for the narrow gap between "Windows 11 is running" and "on this
// exact hardware", e.g. a VM whose hypervisor does not pass AVX through.
function IsProcessorFeaturePresent(Feature: Cardinal): Boolean;
  external 'IsProcessorFeaturePresent@kernel32.dll stdcall';

const
  PF_AVX_INSTRUCTIONS_AVAILABLE = 39;

// Runs before Setup shows its first wizard page, i.e. before anything in
// [Files] is touched. Deliberately checks only AVX, not AVX2 or AVX-512:
// those tiers are resolved at run time by sakura-core's CPU-dispatch code
// (DESIGN 3.2) precisely so that machines without them still work, and
// gating the installer on either would refuse perfectly capable hardware
// to save the engine one already-cheap indirect call.
function InitializeSetup(): Boolean;
begin
  Result := IsProcessorFeaturePresent(PF_AVX_INSTRUCTIONS_AVAILABLE);
  if not Result then
    MsgBox(
      'This CPU does not support AVX, which Sakura Input requires ' +
      '(Intel Sandy Bridge / AMD Bulldozer, 2011, or later). Installing ' +
      'anyway would let the text service crash every application that ' +
      'loads it instead of failing here, cleanly, before anything is ' +
      'copied.',
      mbCriticalError, MB_OK);
end;

function RegToolPath(): String;
begin
  Result := ExpandConstant('{app}\sakura_regtool.exe');
end;

// The real work behind the --unregister line in [UninstallRun]; see the
// comment there for why this lives in a Check function instead of an
// ordinary Run entry. Returns False unconditionally on the success path
// too, so Setup never launches the entry's own Filename/Parameters
// afterward and runs the command twice.
function UnregisterOrAbort(): Boolean;
var
  ResultCode: Integer;
  Launched: Boolean;
begin
  Launched := Exec(RegToolPath(), '--unregister', ExpandConstant('{app}'),
    SW_HIDE, ewWaitUntilTerminated, ResultCode);
  if (not Launched) or (ResultCode <> 0) then
  begin
    // SuppressibleMsgBox, not MsgBox: a silent uninstall (/SUPPRESSMSGBOXES,
    // DESIGN 12.4) must never block on a dialog nobody is there to click.
    // Under that flag this returns immediately with the default answer
    // instead of hanging the uninstaller forever.
    SuppressibleMsgBox(
      'Sakura Input could not remove its text service registration ' +
      '(sakura_regtool.exe --unregister failed). Uninstall has stopped ' +
      'here on purpose: deleting the program files now would leave ' +
      'Windows pointing at a text service whose files no longer exist, ' +
      'which breaks typing in every application until it is fixed by ' +
      'hand. Run sakura_regtool.exe --unregister from an elevated ' +
      'command prompt, then try Uninstall again.',
      mbCriticalError, MB_OK, IDOK);
    // Stops the uninstall outright -- rolling back is not the point here
    // (nothing has been removed yet); refusing to go any further is.
    Abort;
  end;
  Result := False;
end;

// Runs before any file in [Files] is copied, on both a fresh install and
// an upgrade. On an upgrade the previous engine and renderer are still
// running; stopping them here is what makes the plain (non-restartreplace)
// overwrite of their .exe files safe, and it is why the DLL swap that
// follows can rely on nothing new mapping the old file while it happens
// (DESIGN 12.3, "the engine is stopped, so nothing maps them"). A fresh
// install has no previous regtool.exe to call yet, which is not an error
// -- there is simply nothing to stop.
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  ResultCode: Integer;
  ExistingRegTool: String;
begin
  Result := '';
  ExistingRegTool := RegToolPath();
  if FileExists(ExistingRegTool) then
  begin
    // A failure to stop is not fatal to the upgrade: restartreplace still
    // queues the DLL swap for the next reboot regardless of whether the
    // engine exited cleanly, and refusing to proceed here would turn one
    // stuck process into a stuck installer. Recorded in /LOG rather than
    // acted on further.
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
  // Runs after Setup's own file and registry removal (usPostUninstall),
  // not interleaved with [UninstallRun]: a purge failure here must never
  // be mistaken for the registration teardown above having failed, and
  // user data cleanup is independent of both steps.
  if (CurUninstallStep = usPostUninstall) and ShouldPurgeUserData() then
    DelTree(ExpandConstant('{localappdata}\SakuraInput'), True, True, True);
end;
