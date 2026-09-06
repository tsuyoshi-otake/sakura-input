//! The engine's pipe answered from inside a real AppContainer token.
//!
//! DESIGN 7 makes a specific, checkable claim: a sandboxed renderer — a
//! Chrome or Edge tab, any UWP app — can open the pipe and talk to the
//! engine, because the DACL names both AppContainer group SIDs, the access
//! mask handed to sandboxed callers withholds `FILE_CREATE_PIPE_INSTANCE`
//! (which shares its bit with `FILE_APPEND_DATA`, so asking generically gets
//! it back by accident), and the security descriptor carries a low
//! mandatory-label SACL so integrity, not just the DACL, lets the connection
//! through. `sakura-ipc`'s `security.rs` documents all three; nothing before
//! this file has ever exercised them together against a token Windows
//! actually built as an AppContainer. Every other test that talks to the
//! pipe — `pipe_round_trip.rs` included — does it from this process's own,
//! completely unsandboxed token, which cannot fail any of the three checks
//! above no matter how they are written.
//!
//! Getting this wrong is silent and total: if the DACL or the label were
//! subtly wrong, every Chrome or Edge user would lose the IME the moment
//! they typed into a page, and nothing in the rest of this suite would
//! notice, because nothing else launches a sandboxed client.
//!
//! # How the sandboxing is actually built
//!
//! There is no API that turns an already-running process into an
//! AppContainer. The token is assembled by the kernel at process-creation
//! time from a `SECURITY_CAPABILITIES` attached via
//! `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES`, so the only way to test
//! this honestly is to launch a *new* process that way — which is what
//! [`SandboxedChild::launch`] does, using a copy of this very test binary as
//! the thing it launches. The copy is invoked with `--exact <name>
//! --ignored --nocapture --test-threads=1` naming
//! [`the_probe_confirms_it_is_sandboxed_then_uses_the_pipe`], so it runs
//! only that one test and nothing else in this file. That probe does not
//! trust its own sandboxing implicitly — its first assertion is
//! `TokenIsAppContainer` on its own token, which is the thing this whole
//! arrangement exists to make true. It was checked to actually catch a
//! broken launch by temporarily deleting the
//! `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` attribute from
//! [`SandboxedChild::launch`] below: with it gone, this test fails on that
//! one assertion specifically, before ever reaching the pipe, rather than
//! passing for the wrong reason.
//!
//! # Why it is ignored by default
//!
//! Ordinary real-process tests use explicit owned private pipes and profiles,
//! so they can run concurrently. This is the sole exception: the sandboxed
//! child must independently derive the production well-known pipe name, so
//! this test owns that name only after it proves no installed engine owns it.
//! It remains ignored because it creates OS AppContainer state, copies this
//! binary, and launches a sandboxed child; it also deliberately fails when a
//! real installed engine owns the well-known pipe. So:
//!
//! ```text
//! cargo test -p sakura-engine --test appcontainer -- --exact \
//!     the_pipe_is_reachable_from_a_real_appcontainer_token --ignored --nocapture
//! ```
//!
//! run alone, on a machine where creating an AppContainer profile and
//! spawning a sandboxed child are both possible (an interactive session;
//! some locked-down CI or Windows Sandbox configurations disable
//! `CreateAppContainerProfile` outright).
//!
//! The `--exact the_pipe_is_reachable_from_a_real_appcontainer_token` is not
//! optional. This binary holds *two* `#[ignore]`d tests — the other is
//! [`the_probe_confirms_it_is_sandboxed_then_uses_the_pipe`], meant only to
//! be re-executed inside the sandbox this file builds — and plain `--ignored`
//! with no name filter runs every ignored test in the binary. Run that way,
//! libtest's own default parallelism starts the probe directly, unsandboxed,
//! at the same time as the real test below spawns a *second*, sandboxed copy
//! of it; the direct copy then correctly fails its own marker-env check (see
//! that test's doc comment), and the run as a whole is reported failed even
//! though the actual AppContainer round trip passed. That is not a flaky
//! test — it is `--ignored` doing exactly what it is documented to do, on a
//! binary this file deliberately keeps to one process for the self-re-exec
//! trick to work. Naming the one test that is meant to run this way avoids
//! it.

// `common` is shared verbatim across this directory's test binaries
// (`pipe_round_trip.rs`, `ipc_latency.rs`); each `mod common;` compiles its
// own copy, so an item this file has no reason to call — `named_key`, used
// by the other two for non-character keys like Enter — is legitimately
// dead code here rather than a sign anything is actually unused.
#[allow(dead_code)]
mod common;

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::IntoRawHandle;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use windows::core::{Error, HRESULT, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, SetHandleInformation, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS,
    ERROR_FILE_NOT_FOUND, ERROR_INSUFFICIENT_BUFFER, HANDLE, HANDLE_FLAG_INHERIT, WAIT_OBJECT_0,
    WIN32_ERROR,
};
use windows::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows::Win32::Security::{
    FreeSid, GetTokenInformation, TokenIsAppContainer, PSID, SECURITY_CAPABILITIES, TOKEN_QUERY,
};
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetCurrentProcess, GetExitCodeProcess,
    InitializeProcThreadAttributeList, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW,
    TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject, CREATE_UNICODE_ENVIRONMENT,
    EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, STARTF_USESTDHANDLES, STARTUPINFOEXW,
    STARTUPINFOW,
};

use common::{session_for, test_char_key, visible, Engine, PATIENT};
use sakura_ipc::{Client, Endpoint, ServerTrustPolicy};
use sakura_proto::{Request, Response, PROTOCOL_VERSION};

/// Namespaced so it cannot collide with anything a developer's own machine
/// might already have registered, and stable across runs so a crashed
/// previous run's profile is *reused* (via the `ERROR_ALREADY_EXISTS`
/// fallback below) rather than accumulating garbage in the AppContainer
/// store.
const APPCONTAINER_PROFILE_NAME: &str = "sakura-input-test-appcontainer";

/// Set in the child's environment by [`SandboxedChild::launch`]. Its
/// presence is what tells [`the_probe_confirms_it_is_sandboxed_then_uses_the_pipe`]
/// that it is running inside the arrangement this file builds, rather than
/// being run directly by a developer or by a plain `cargo test`.
const CHILD_MARKER_ENV: &str = "SAKURA_APPCONTAINER_CHILD";

/// Set alongside [`CHILD_MARKER_ENV`]: the pipe name the *parent* resolved,
/// still running under its own, unsandboxed token. The child resolves its
/// own copy of the same name and compares — see the module docs on why that
/// comparison exists before the connect attempt, not after it.
const PARENT_PIPE_NAME_ENV: &str = "SAKURA_APPCONTAINER_PARENT_PIPE_NAME";

/// Set alongside [`PARENT_PIPE_NAME_ENV`] by the parent that spawned the
/// well-known-pipe engine. The sandboxed child must bind its exact pipe handle
/// to this PID before it sends any protocol request.
const PARENT_ENGINE_PID_ENV: &str = "SAKURA_APPCONTAINER_PARENT_ENGINE_PID";

/// The exact image path of the engine child owned by the parent test. The
/// sandboxed probe uses this policy before Hello, exercising the same kernel
/// path/integrity binding as the production TSF client.
const PARENT_ENGINE_PATH_ENV: &str = "SAKURA_APPCONTAINER_PARENT_ENGINE_PATH";

/// Whether the child should independently derive the production Data name.
/// Private-pipe verification uses the explicit name while retaining the same
/// production Data descriptor and verified server-process checks.
const PARENT_PRODUCTION_PIPE_ENV: &str = "SAKURA_APPCONTAINER_PRODUCTION_PIPE";

/// The exact libtest name of the child-side probe. Passed to `--exact` so
/// the re-executed copy runs only that test. Kept next to the function it
/// names, in the same file, because nothing enforces the two staying in
/// sync except a human reading both at once.
const PROBE_TEST_NAME: &str = "the_probe_confirms_it_is_sandboxed_then_uses_the_pipe";

/// Generous, not tight: this waits on process creation, a fresh connect,
/// and a full handshake-plus-keystroke round trip, all from inside a
/// sandbox that may be doing this for the first time on the machine (first
/// access to a DLL, first page-in). A test that flakes on timing measures
/// the machine, not the DACL.
const CHILD_TIMEOUT: Duration = Duration::from_secs(30);

/// The whole point of this file: a real AppContainer token, built the only
/// way Windows allows, reaches the engine's pipe and completes a keystroke.
///
/// This is the sole intentional well-known-pipe owner. It first fails before
/// any protocol request if a user's engine already owns that pipe, then owns
/// and cleans up only the engine it spawned.
#[test]
#[ignore = "creates an AppContainer profile and spawns a sandboxed child \
            against the production well-known pipe, which must be unoccupied; \
            run alone, filtered to this test by name (see the module docs \
            for why the filter is required): cargo test -p sakura-engine \
            --test appcontainer -- --exact \
            the_pipe_is_reachable_from_a_real_appcontainer_token --ignored \
            --nocapture"]
fn the_pipe_is_reachable_from_a_real_appcontainer_token() {
    let mut engine = Engine::spawn_well_known_for_appcontainer();
    // Blocks until the engine is actually serving, so the child never races
    // a pipe that has not been created yet.
    let _ready = engine.client();

    run_sandboxed_probe(&mut engine, true);
}

/// Exercises the same AppContainer token against an explicitly owned private
/// Data pipe. This is safe to run while an installed engine owns the
/// well-known name and is the preferred evidence for verified process queries
/// in local/CI environments.
#[test]
#[ignore = "creates an AppContainer profile and spawns a sandboxed child against an owned private pipe; run alone with --exact and --ignored"]
fn verified_private_pipe_is_reachable_from_a_real_appcontainer_token() {
    let mut engine = Engine::spawn_isolated();
    let _ready = engine.client();
    run_sandboxed_probe(&mut engine, false);
}

fn run_sandboxed_probe(engine: &mut Engine, production_pipe: bool) {
    let mut child = SandboxedChild::launch(engine.child_pid(), engine.pipe_name(), production_pipe);
    let outcome = child.wait(CHILD_TIMEOUT);

    let stdout = read_log(&child.stdout_path);
    let stderr = read_log(&child.stderr_path);
    // Printed unconditionally — with `--nocapture` (which the module docs'
    // command always passes) this is the evidence a human reads to tell a
    // real ACCESS_DENIED apart from a naming-scheme mismatch, without
    // needing the test to fail first.
    println!("--- sandboxed child stdout ---\n{stdout}");
    println!("--- sandboxed child stderr ---\n{stderr}");

    let cleanup = engine
        .cleanup()
        .expect("owned well-known-pipe engine cleanup must succeed");
    assert!(
        cleanup.status.success(),
        "owned engine pid {} exited with {}",
        cleanup.pid,
        cleanup.status
    );

    match outcome {
        Some(0) => {}
        Some(code) => panic!(
            "the sandboxed child exited with code {code} (0 means the probe \
             passed)\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
        ),
        None => panic!(
            "the sandboxed child did not exit within {CHILD_TIMEOUT:?}\n\
             --- stdout so far ---\n{stdout}\n--- stderr so far ---\n{stderr}"
        ),
    }
}

/// The sandboxed side: re-executed as an AppContainer child by
/// [`SandboxedChild::launch`], never meant to run any other way.
///
/// # Why the self-check comes before the pipe
///
/// A test that only connects and converts a keystroke proves nothing about
/// sandboxing on its own: if the launch below silently produced an
/// ordinary, unsandboxed process — a wrong attribute, a constant that
/// stopped matching a `windows` crate signature, `UpdateProcThreadAttribute`
/// failing in some way this file's `.expect()` did not catch — the connect
/// would still succeed, the same way it succeeds for every other test in
/// this workspace, and "AppContainer" in this test's name would be a lie
/// nobody could see. So the very first thing this does, before touching the
/// pipe at all, is ask Windows about its own token, and refuses to go any
/// further if the answer says "not sandboxed". That check was verified
/// load-bearing by temporarily removing
/// `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` from `launch` and
/// confirming this test then fails on `TokenIsAppContainer` specifically,
/// rather than somewhere in the pipe handshake.
#[test]
#[ignore = "only meaningful launched by SandboxedChild::launch inside an \
            AppContainer; run directly, it proves your own desktop \
            session's token isn't sandboxed, which isn't a test of anything"]
fn the_probe_confirms_it_is_sandboxed_then_uses_the_pipe() {
    assert_eq!(
        env::var(CHILD_MARKER_ENV).as_deref(),
        Ok("1"),
        "this test only makes sense re-executed by \
         `the_pipe_is_reachable_from_a_real_appcontainer_token`; run it \
         directly and every assertion below is about your own desktop \
         session, not a sandbox"
    );

    assert!(
        current_process_is_app_container(),
        "TokenIsAppContainer is 0: this process is NOT actually sandboxed. \
         Either SandboxedChild::launch stopped building a real \
         AppContainer token (check PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES \
         is still attached before CreateProcessW), or this test ran outside \
         that arrangement despite the marker-env check above."
    );

    // Load-bearing diagnostics, not decoration: `pipe_name()` keys off the
    // token's logon-session SID (falling back to the user SID), and an
    // AppContainer token is derived from the parent's — in theory it keeps
    // the same logon session, so this should be a no-op comparison. If it
    // is not, a downstream connect failure is ERROR_FILE_NOT_FOUND (there
    // is genuinely no pipe by that name), which is a completely different
    // finding from ERROR_ACCESS_DENIED (there is a pipe by that name and
    // its security descriptor refused this token) and must never be
    // reported as the latter.
    let parent_pipe_name = env::var(PARENT_PIPE_NAME_ENV)
        .expect("SandboxedChild::launch always sets this alongside the marker");
    let resolved_production_name = match sakura_ipc::pipe_name() {
        Ok(name) => name,
        Err(fault) => panic!(
            "this AppContainer token could not even compute a pipe name \
             (sakura_ipc::pipe_name() failed: {fault:?}). The parent, \
             running under its own unsandboxed token, resolved \
             {parent_pipe_name:?}. Whatever token lookup pipe_name() \
             performs (TokenGroups/TokenUser via GetTokenInformation) is \
             failing differently under a sandboxed token — this is a \
             naming-scheme finding, not a DACL one."
        ),
    };
    let production_pipe = env::var(PARENT_PRODUCTION_PIPE_ENV).as_deref() == Ok("1");
    println!("parent selected pipe name: {parent_pipe_name}");
    println!("child production pipe name: {resolved_production_name}");
    if production_pipe {
        assert_eq!(
            resolved_production_name, parent_pipe_name,
            "the AppContainer child computed a DIFFERENT pipe name than the \
             parent did. This is a naming-scheme finding, not a security- \
             descriptor one: sakura_ipc::pipe_name() (security.rs) derives the \
             name from the token's logon-session SID, falling back to the user \
             SID, and that derivation is not producing the same answer under \
             an AppContainer token as it does under the parent's own token. A \
             connect failure downstream of this mismatch would surface as \
             ERROR_FILE_NOT_FOUND, not ERROR_ACCESS_DENIED, and must not be \
             reported as a DACL/mandatory-label bug."
        );
    }

    let expected_server_pid = child_contract_engine_pid();
    let expected_server_path = env::var(PARENT_ENGINE_PATH_ENV)
        .expect("SandboxedChild::launch always sets the engine image path");
    let policy = ServerTrustPolicy::Exact(expected_server_path.clone().into());
    println!(
        "sandbox classification of engine pid {expected_server_pid}: {:?}",
        sakura_ipc::classify_client_process(expected_server_pid)
    );

    let connected = if production_pipe {
        Client::connect_endpoint_verified(Endpoint::Data, &policy, PATIENT)
    } else {
        Client::connect_verified_to(&parent_pipe_name, &policy, PATIENT)
    };
    let mut client = match connected {
        Ok(client) => client,
        Err(sakura_ipc::Fault::Os(error)) if is_win32(&error, ERROR_ACCESS_DENIED) => panic!(
            "connect to {parent_pipe_name:?} failed with ERROR_ACCESS_DENIED \
             (5): this is the real finding this file exists to catch. The \
             pipe's DACL or its low-integrity mandatory-label SACL \
             (security.rs's `sddl`) is not actually granting this \
             AppContainer the access it documents — or CLIENT_ACCESS \
             (which this client used, via Client::connect) drifted from \
             what the server grants. Raw error: {error:?}"
        ),
        Err(sakura_ipc::Fault::Os(error)) if is_win32(&error, ERROR_FILE_NOT_FOUND) => panic!(
            "connect to {parent_pipe_name:?} failed with \
             ERROR_FILE_NOT_FOUND (2): despite the parent/child pipe names \
             matching above, no pipe by that name exists right now. This \
             is NOT a security-descriptor problem — check that the engine \
             `the_pipe_is_reachable_from_a_real_appcontainer_token` \
             started is still alive. Raw error: {error:?}"
        ),
        Err(sakura_ipc::Fault::UntrustedServer {
            process_id,
            rejection,
        }) => {
            eprintln!(
                "policy diagnostic: rejected_pid_matches_owned={}; {}",
                process_id == expected_server_pid,
                image_policy_evidence(
                    Path::new(&expected_server_path),
                    query_image_for_diagnostics(process_id),
                    &policy
                )
            );
            panic!(
                "verified connect rejected process {process_id}: {rejection}. \
                 No Hello was sent. A policy rejection identifies a failed policy \
                 decision, not by itself a different executable or an environment flake. \
                 The diagnostic is a later read-only query, not the original admission observation."
            )
        }
        Err(other) => panic!(
            "connect to {parent_pipe_name:?} failed with an unexpected \
             fault (neither ACCESS_DENIED nor FILE_NOT_FOUND): {other:?}"
        ),
    };

    let actual_server_pid = client.server_process_id().unwrap_or_else(|fault| {
        panic!(
            "could not identify the server on the sandbox child's exact pipe connection: {fault:?}; no protocol request was sent"
        )
    });
    assert_eq!(
        actual_server_pid, expected_server_pid,
        "refusing sandboxed protocol traffic: the exact pipe connection is served by pid {actual_server_pid}, not the parent-owned engine pid {expected_server_pid}; no protocol request was sent"
    );

    match client.call(
        &Request::Hello {
            client_version: PROTOCOL_VERSION,
        },
        PATIENT,
    ) {
        Ok(Response::Hello { server_version, .. }) => {
            assert_eq!(server_version, PROTOCOL_VERSION);
        }
        other => panic!("handshake from inside the AppContainer: expected Hello, got {other:?}"),
    }

    let session = session_for(&mut client, "appcontainer_probe.exe");

    match client.call(
        &Request::SendKey {
            session,
            key: test_char_key('a'),
        },
        PATIENT,
    ) {
        Ok(Response::Output(output)) => {
            assert_eq!(
                visible(output.preedit),
                "あ",
                "the sandboxed client's keystroke did not convert correctly"
            );
        }
        other => panic!("SendKey from inside the AppContainer: expected Output, got {other:?}"),
    }
}

/// `GetTokenInformation(..., TokenIsAppContainer, ...)` on this process's
/// own token. Nonzero means Windows built a real AppContainer token for it
/// — the only way that happens is `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES`
/// at the process's own creation; there is no API to self-elevate into one
/// afterward, which is exactly why this is trustworthy evidence rather than
/// something a process could fake.
fn current_process_is_app_container() -> bool {
    let mut token = HANDLE::default();
    // SAFETY: `GetCurrentProcess` is a pseudo-handle needing no release;
    // `token` is a valid out-parameter that receives a real handle on
    // success, closed below.
    unsafe {
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)
            .expect("a process can always query its own token");
    }

    let mut is_app_container: u32 = 0;
    let mut returned = 0u32;
    // SAFETY: `TokenIsAppContainer` is documented to fill exactly one
    // `DWORD`; `is_app_container` is sized for that, and `returned` is a
    // valid out-parameter.
    let result = unsafe {
        GetTokenInformation(
            token,
            TokenIsAppContainer,
            Some((&mut is_app_container as *mut u32).cast()),
            size_of::<u32>() as u32,
            &mut returned,
        )
    };
    // SAFETY: `token` was opened by this function, above, and is not used
    // again after this call.
    unsafe {
        let _ = CloseHandle(token);
    }
    result.expect("GetTokenInformation(TokenIsAppContainer) on our own token must succeed");
    is_app_container != 0
}

fn is_win32(error: &Error, code: WIN32_ERROR) -> bool {
    error.code() == HRESULT::from_win32(code.0)
}

fn child_contract_engine_pid() -> u32 {
    let value = env::var(PARENT_ENGINE_PID_ENV).unwrap_or_else(|error| {
        panic!(
            "SandboxedChild::launch must set {PARENT_ENGINE_PID_ENV}: {error}; no protocol request was sent"
        )
    });
    parse_owned_engine_pid(&value).unwrap_or_else(|detail| {
        panic!(
            "invalid {PARENT_ENGINE_PID_ENV} child launch contract: {detail}; no protocol request was sent"
        )
    })
}

fn parse_owned_engine_pid(value: &str) -> Result<u32, String> {
    let pid = value
        .parse::<u32>()
        .map_err(|_| format!("{value:?} is not an unsigned decimal PID"))?;
    if pid == 0 {
        return Err("PID 0 is not a child process".to_owned());
    }
    Ok(pid)
}

fn read_log(path: &Path) -> String {
    fs::read_to_string(path)
        .unwrap_or_else(|error| format!("<could not read {}: {error}>", path.display()))
}

/// Everything [`SandboxedChild::launch`] acquires, bundled so `Drop` cleans
/// all of it up regardless of how the caller's assertions on the child's
/// exit code turn out.
///
/// Each acquired resource is its own small guard type with its own `Drop`,
/// composed here rather than centralized in one hand-written cleanup
/// routine. That is what makes `launch` itself safe under `.expect()`:
/// every step before a failing one has already produced a local guard
/// variable, and a panic mid-`launch` unwinds through those exactly as if
/// `launch` had returned successfully and the caller's own code had
/// panicked. Field order matters here — Rust drops struct fields top to
/// bottom — and it is chosen deliberately: the child process is stopped and
/// its handles closed *first* (the one genuinely time-sensitive part, and
/// PLAN.md's Phase 1 "no orphaned processes" exit criterion is explicit
/// that a test must never leave one running), then the attribute list, then
/// the scratch directory the process might otherwise still be writing to,
/// then the SID, then the AppContainer profile registration.
struct SandboxedChild {
    process: ProcessGuard,
    _attrs: AttributeListGuard,
    _scratch: ScratchDirGuard,
    _sid: SidGuard,
    _profile: ProfileGuard,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
}

impl SandboxedChild {
    /// Builds a fresh AppContainer profile (or reuses one left behind by an
    /// interrupted previous run), copies this test binary into a scratch
    /// directory the AppContainer can read and execute, and launches the
    /// copy with `--exact PROBE_TEST_NAME --ignored --nocapture
    /// --test-threads=1` so it runs only
    /// [`the_probe_confirms_it_is_sandboxed_then_uses_the_pipe`].
    fn launch(
        owned_engine_pid: u32,
        parent_pipe_name: &str,
        production_pipe: bool,
    ) -> SandboxedChild {
        let profile_wide = to_wide_nul(APPCONTAINER_PROFILE_NAME);
        let display_wide = to_wide_nul("Sakura Input test AppContainer");
        let desc_wide = to_wide_nul(
            "Ephemeral profile used only by sakura-engine's appcontainer.rs test; safe to delete",
        );

        // SAFETY: all three buffers are NUL-terminated and outlive the
        // call. `None` capabilities means the AppContainer gets no special
        // rights beyond its own package SID — exactly the baseline
        // `security.rs`'s DACL promises every AppContainer, nothing more.
        let created = unsafe {
            CreateAppContainerProfile(
                PCWSTR(profile_wide.as_ptr()),
                PCWSTR(display_wide.as_ptr()),
                PCWSTR(desc_wide.as_ptr()),
                None,
            )
        };
        let sid = match created {
            Ok(sid) => sid,
            // A previous run of this test left the profile registered
            // (crashed before cleanup, or ran concurrently). Its SID is
            // deterministic from the name, so deriving it is exactly as
            // good as having just created it — no re-registration needed.
            Err(error) if error.code() == HRESULT::from_win32(ERROR_ALREADY_EXISTS.0) => {
                // SAFETY: as above.
                unsafe { DeriveAppContainerSidFromAppContainerName(PCWSTR(profile_wide.as_ptr())) }
                    .expect("the profile exists (ALREADY_EXISTS), so its SID must derive")
            }
            Err(error) => panic!("CreateAppContainerProfile: {error:?}"),
        };
        let sid_guard = SidGuard(sid);

        let scratch_dir = new_scratch_dir();
        let scratch_guard = ScratchDirGuard(scratch_dir.clone());

        let child_exe = scratch_dir.join("appcontainer_probe.exe");
        fs::copy(
            env::current_exe().expect("this process has a path"),
            &child_exe,
        )
        .expect("copy this test binary into the scratch directory");

        // Without these, CreateProcessW below fails with ERROR_ACCESS_DENIED
        // before the child ever runs: an AppContainer token has no access
        // to anything under the user's temp directory by default. DESIGN
        // 7's whole point is that the *pipe* is the one deliberate
        // exception to that; the exe and its directory are not, and have
        // to be granted explicitly, same as any other AppContainer host
        // would need to grant them.
        grant_appcontainer_access(&scratch_dir, true);
        grant_appcontainer_access(&child_exe, false);

        let stdout_path = scratch_dir.join("child-stdout.log");
        let stderr_path = scratch_dir.join("child-stderr.log");
        let stdout_handle = inheritable_log_handle(&stdout_path);
        let stderr_handle = inheritable_log_handle(&stderr_path);

        let mut security_capabilities = SECURITY_CAPABILITIES {
            AppContainerSid: sid,
            Capabilities: core::ptr::null_mut(),
            CapabilityCount: 0,
            Reserved: 0,
        };
        let (attr_buffer, attr_list) =
            init_security_capabilities_attribute(&mut security_capabilities);
        let attrs_guard = AttributeListGuard {
            _buffer: attr_buffer,
            list: attr_list,
        };

        let mut environment_block = build_environment_block(&[
            (PARENT_PIPE_NAME_ENV, parent_pipe_name.to_owned()),
            (PARENT_ENGINE_PID_ENV, owned_engine_pid.to_string()),
            (
                PARENT_ENGINE_PATH_ENV,
                env!("CARGO_BIN_EXE_sakura_engine").to_owned(),
            ),
            (
                PARENT_PRODUCTION_PIPE_ENV,
                if production_pipe { "1" } else { "0" }.to_owned(),
            ),
        ]);

        let command_line = format!(
            "\"{}\" --exact {PROBE_TEST_NAME} --ignored --nocapture --test-threads=1",
            child_exe.display(),
        );
        let mut command_line_wide = to_wide_nul(&command_line);
        let current_dir_wide = to_wide_nul(&scratch_dir.to_string_lossy());

        let startup_info = STARTUPINFOEXW {
            StartupInfo: STARTUPINFOW {
                cb: size_of::<STARTUPINFOEXW>() as u32,
                dwFlags: STARTF_USESTDHANDLES,
                hStdOutput: stdout_handle,
                hStdError: stderr_handle,
                ..Default::default()
            },
            lpAttributeList: attr_list,
        };
        let mut process_info = PROCESS_INFORMATION::default();

        // SAFETY: `command_line_wide` is a mutable, NUL-terminated buffer —
        // CreateProcessW is documented to potentially write into it, which
        // is why it is not a shared `&str`. `startup_info` carries the
        // attribute list built above: that one attribute is what makes
        // Windows assemble an AppContainer token for the child instead of
        // an ordinary one, with no token-duplication step needed on this
        // side. `environment_block` is double-NUL-terminated UTF-16,
        // matching `CREATE_UNICODE_ENVIRONMENT`. `process_info` is a valid
        // out-parameter. `bInheritHandles: true` is what makes the child
        // actually receive the two log-file handles referenced by
        // `startup_info` — both opened inheritable by
        // `inheritable_log_handle` below.
        let create_result = unsafe {
            CreateProcessW(
                PCWSTR::null(),
                Some(PWSTR(command_line_wide.as_mut_ptr())),
                None,
                None,
                true,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
                Some(environment_block.as_mut_ptr().cast()),
                PCWSTR(current_dir_wide.as_ptr()),
                &startup_info.StartupInfo,
                &mut process_info,
            )
        };

        // Whether or not the launch above succeeded, this process's copies
        // of the log handles serve no further purpose: on success they are
        // inherited by the child now, and on failure there is no child to
        // inherit them at all. Either way, holding them open would risk
        // this process racing the child's own writes when the handles are
        // eventually read back.
        // SAFETY: both came from `inheritable_log_handle`, above, and are
        // not used again in this process.
        unsafe {
            let _ = CloseHandle(stdout_handle);
            let _ = CloseHandle(stderr_handle);
        }

        create_result.unwrap_or_else(|error| {
            panic!(
                "CreateProcessW into the AppContainer failed: {error:?}\n\
                 (a common cause is the scratch directory or the exe copy \
                 missing the ALL APPLICATION PACKAGES grant that \
                 grant_appcontainer_access is supposed to have applied)"
            )
        });

        SandboxedChild {
            process: ProcessGuard {
                process: process_info.hProcess,
                thread: process_info.hThread,
                exited: false,
            },
            _attrs: attrs_guard,
            _scratch: scratch_guard,
            _sid: sid_guard,
            _profile: ProfileGuard,
            stdout_path,
            stderr_path,
        }
    }

    fn wait(&mut self, timeout: Duration) -> Option<u32> {
        self.process.wait(timeout)
    }
}

/// The live child process and thread handles from `CreateProcessW`.
///
/// `exited` records whether [`wait`](ProcessGuard::wait) ever observed the
/// child actually finish; when it did not (a timeout, or a panic in the
/// caller before `wait` ran at all), `Drop` terminates the child rather
/// than risk leaving a sandboxed process connected to the engine's pipe
/// after the test that spawned it has already reported its result.
struct ProcessGuard {
    process: HANDLE,
    thread: HANDLE,
    exited: bool,
}

impl ProcessGuard {
    fn wait(&mut self, timeout: Duration) -> Option<u32> {
        let millis = timeout.as_millis().min(u32::MAX as u128) as u32;
        // SAFETY: `process` is a valid, open process handle for the
        // lifetime of this call — owned by `self` and not closed until
        // `Drop`.
        let waited = unsafe { WaitForSingleObject(self.process, millis) };
        if waited != WAIT_OBJECT_0 {
            return None;
        }
        self.exited = true;
        let mut code = 0u32;
        // SAFETY: the handle just signalled, so GetExitCodeProcess is
        // documented to return the real exit code rather than
        // STILL_ACTIVE; `code` is a valid out-parameter.
        unsafe {
            GetExitCodeProcess(self.process, &mut code)
                .expect("a signalled process handle reports its exit code");
        }
        Some(code)
    }
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        if !self.exited {
            // SAFETY: `process` was returned by CreateProcessW in `launch`
            // and is still open; forcing it down is exactly what an
            // unexited child at this point requires (see the struct docs).
            unsafe {
                let _ = TerminateProcess(self.process, 1);
            }
        }
        // SAFETY: both handles were opened by CreateProcessW in `launch`
        // and are owned solely by this guard; closed exactly once, here.
        unsafe {
            let _ = CloseHandle(self.thread);
            let _ = CloseHandle(self.process);
        }
    }
}

/// The initialized process-thread attribute list backing `STARTUPINFOEXW`.
///
/// `_buffer` is the memory `list` points into; keeping them in one struct
/// is what guarantees the buffer outlives every use of the pointer,
/// including the `DeleteProcThreadAttributeList` call this issues on drop.
struct AttributeListGuard {
    _buffer: Vec<u8>,
    list: LPPROC_THREAD_ATTRIBUTE_LIST,
}

impl Drop for AttributeListGuard {
    fn drop(&mut self) {
        // SAFETY: `list` was initialized by `init_security_capabilities_attribute`
        // and points into `_buffer`, which is still alive at this point
        // (this method runs before Rust's field-drop glue frees it). Windows
        // makes no further use of the list once this call returns.
        unsafe {
            DeleteProcThreadAttributeList(self.list);
        }
    }
}

/// Frees the AppContainer SID with `FreeSid`, exactly as
/// `CreateAppContainerProfile`/`DeriveAppContainerSidFromAppContainerName`
/// document as the caller's responsibility.
struct SidGuard(PSID);

impl Drop for SidGuard {
    fn drop(&mut self) {
        if !self.0 .0.is_null() {
            // SAFETY: `self.0` came from CreateAppContainerProfile or
            // DeriveAppContainerSidFromAppContainerName in `launch`, both
            // of which document their result as freed with `FreeSid`.
            unsafe {
                FreeSid(self.0);
            }
        }
    }
}

/// Deletes the AppContainer profile this run created (or re-derived), so
/// repeated runs start from the same clean state instead of accumulating
/// registrations in Windows' AppContainer store.
struct ProfileGuard;

impl Drop for ProfileGuard {
    fn drop(&mut self) {
        let wide = to_wide_nul(APPCONTAINER_PROFILE_NAME);
        // SAFETY: `wide` is NUL-terminated and outlives the call. Deleting
        // a profile namespaced to this test and used nowhere else is safe
        // even if some other process is mid-use of it — the name is not
        // shared with anything real.
        unsafe {
            let _ = DeleteAppContainerProfile(PCWSTR(wide.as_ptr()));
        }
    }
}

/// Removes the scratch directory — the exe copy and the child's captured
/// stdout/stderr — on drop.
struct ScratchDirGuard(PathBuf);

impl Drop for ScratchDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Sizes, allocates, and initializes the process-thread attribute list that
/// carries `security_capabilities`, then attaches that one attribute to it.
/// Returns the backing buffer alongside the list handle into it — the
/// buffer must outlive every use of the list, which
/// [`AttributeListGuard`] is what guarantees.
fn init_security_capabilities_attribute(
    security_capabilities: &mut SECURITY_CAPABILITIES,
) -> (Vec<u8>, LPPROC_THREAD_ATTRIBUTE_LIST) {
    let mut size: usize = 0;
    // SAFETY: `None` for the buffer with attribute count 1 is the
    // documented way to size the list; the call is expected to fail with
    // ERROR_INSUFFICIENT_BUFFER, and `size` is a valid out-parameter either
    // way.
    let probe = unsafe { InitializeProcThreadAttributeList(None, 1, None, &mut size) };
    if let Err(error) = probe {
        if error.code() != HRESULT::from_win32(ERROR_INSUFFICIENT_BUFFER.0) {
            panic!("InitializeProcThreadAttributeList (sizing): {error:?}");
        }
    }

    let mut buffer = vec![0u8; size];
    let list = LPPROC_THREAD_ATTRIBUTE_LIST(buffer.as_mut_ptr().cast());
    // SAFETY: `buffer` is exactly the size the probe above reported, and is
    // returned alongside `list` so it outlives every later use of the
    // pointer into it.
    unsafe {
        InitializeProcThreadAttributeList(Some(list), 1, None, &mut size)
            .expect("InitializeProcThreadAttributeList (init)");
    }

    // SAFETY: `list` was just initialized above, and `security_capabilities`
    // outlives this call through the caller's stack frame across
    // CreateProcessW. This one attribute is what turns the child into an
    // AppContainer — see the module docs' summary of why no token
    // duplication step is needed.
    unsafe {
        UpdateProcThreadAttribute(
            list,
            0,
            PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
            Some((security_capabilities as *const SECURITY_CAPABILITIES).cast()),
            size_of::<SECURITY_CAPABILITIES>(),
            None,
            None,
        )
        .expect("UpdateProcThreadAttribute (security capabilities)");
    }

    (buffer, list)
}

/// Creates (or truncates) `path` and marks the resulting handle
/// inheritable, so a child launched with `bInheritHandles: true` can write
/// to it as its stdout/stderr even though the AppContainer itself has no
/// grant to open arbitrary files: the handle was opened by this process,
/// which does have access, and is merely handed down already-open. Ownership
/// of the handle transfers to the caller, who is responsible for closing it
/// (see `launch`, which closes both after `CreateProcessW`).
fn inheritable_log_handle(path: &Path) -> HANDLE {
    let file = fs::File::create(path).expect("create a log file in the scratch directory");
    let handle = HANDLE(file.into_raw_handle());
    // SAFETY: `handle` was just obtained from `File::into_raw_handle`,
    // which transfers ownership to us — `file` is gone and no longer
    // closes it — and this call only flips a flag on an otherwise
    // untouched, valid handle.
    unsafe {
        SetHandleInformation(handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAG_INHERIT)
            .expect("mark the log handle inheritable");
    }
    handle
}

/// The SID every AppContainer token carries — the same principal
/// `sakura-ipc`'s pipe DACL grants
/// (`sakura_ipc::security`'s `ALL_APPLICATION_PACKAGES`, private to that
/// crate). Written out again here as a literal rather than imported: it is
/// not part of `sakura_ipc`'s public API, and a well-known SID string is
/// exactly as stable as a crate-private constant would be.
const ALL_APPLICATION_PACKAGES_SID: &str = "S-1-15-2-1";

/// Grants the AppContainer principal read+execute on `path` via `icacls`.
///
/// Shelling out is simpler than building the ACE with
/// `SetNamedSecurityInfo` and just as correct for test setup that is not
/// shipped code. An AppContainer token has no access to anything under the
/// user's profile or temp directory by default — DESIGN 7's whole point is
/// that the pipe is the one deliberate exception — so without this grant
/// `CreateProcessW` in `launch` fails with `ERROR_ACCESS_DENIED` before the
/// child ever runs, which would look identical to (and be misdiagnosed as)
/// the pipe DACL problem this file exists to catch.
fn grant_appcontainer_access(path: &Path, inherit_to_children: bool) {
    let ace = if inherit_to_children {
        format!("*{ALL_APPLICATION_PACKAGES_SID}:(OI)(CI)(RX)")
    } else {
        format!("*{ALL_APPLICATION_PACKAGES_SID}:(RX)")
    };
    let output = Command::new("icacls")
        .arg(path)
        .arg("/grant")
        .arg(&ace)
        .output()
        .expect("icacls ships with every supported Windows (DESIGN 3.2)");
    if !output.status.success() {
        panic!(
            "icacls {} /grant {ace} failed (exit {:?}):\nstdout: {}\nstderr: {}",
            path.display(),
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

/// Test-only follow-up query. No raw paths or user input are logged, and this
/// result never participates in acceptance or authorizes a second connection.
fn query_image_for_diagnostics(process_id: u32) -> Result<PathBuf, String> {
    // SAFETY: read-only access to the PID reported by the rejected pipe handle.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id) }
        .map_err(|error| format!("open_failed({:?})", error.code()))?;
    let mut buffer = vec![0u16; 32_768];
    let mut length = buffer.len() as u32;
    // SAFETY: live process handle and writable bounded UTF-16 buffer.
    let result = unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        )
    }
    .map_err(|error| format!("image_failed({:?})", error.code()));
    // SAFETY: this helper owns exactly this handle, including on query failure.
    unsafe {
        let _ = CloseHandle(handle);
    }
    result?;
    Ok(PathBuf::from(OsString::from_wide(
        &buffer[..length as usize],
    )))
}

fn image_policy_evidence(
    expected: &Path,
    observed: Result<PathBuf, String>,
    policy: &ServerTrustPolicy,
) -> String {
    let shape = |path: &Path| {
        format!(
            "absolute={},rooted={},native_device={},nt_dos={},drive_prefix={},parent={},verbatim={},forward_slash={},engine_name={}",
            path.is_absolute(),
            path.has_root(),
            path.as_os_str().to_string_lossy().starts_with(r"\Device\"),
            path.as_os_str().to_string_lossy().starts_with(r"\??\"),
            matches!(path.components().next(), Some(std::path::Component::Prefix(prefix)) if matches!(prefix.kind(), std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_))),
            path.components()
                .any(|part| matches!(part, std::path::Component::ParentDir)),
            path.as_os_str().to_string_lossy().starts_with(r"\\?\"),
            path.as_os_str().to_string_lossy().contains('/'),
            path.file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case("sakura_engine.exe"))
        )
    };
    let Ok(observed) = observed else {
        return format!(
            "image_query={}; expected_shape=({})",
            observed.unwrap_err(),
            shape(expected)
        );
    };
    let expected_canonical = std::fs::canonicalize(expected);
    let observed_canonical = std::fs::canonicalize(&observed);
    let status = |result: &std::io::Result<PathBuf>| match result {
        Ok(_) => "ok".to_owned(),
        Err(error) => format!(
            "error(kind={:?},os={:?})",
            error.kind(),
            error.raw_os_error()
        ),
    };
    let equal = |left: &Path, right: &Path| {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    };
    let canonical_equal = match (&expected_canonical, &observed_canonical) {
        (Ok(left), Ok(right)) => Some(equal(left, right)),
        _ => None,
    };
    format!("image_query=ok; expected_shape=({}); observed_shape=({}); lexical_equal={}; expected_canonical={}; observed_canonical={}; canonical_equal={canonical_equal:?}; policy_recheck={}",
        shape(expected), shape(&observed), equal(expected, &observed), status(&expected_canonical), status(&observed_canonical), policy.matches_image_path(&observed))
}

#[test]
fn image_policy_diagnostics_explain_shape_without_emitting_paths() {
    let queried_self =
        query_image_for_diagnostics(std::process::id()).expect("query owned test process");
    assert_eq!(
        std::fs::canonicalize(queried_self).expect("canonical queried image"),
        std::fs::canonicalize(std::env::current_exe().expect("current image"))
            .expect("canonical current image")
    );
    let expected = PathBuf::from(r"C:\synthetic-private-label\sakura_engine.exe");
    let observed = PathBuf::from(r"\\?\C:\synthetic-private-label\sakura_engine.exe");
    let policy = ServerTrustPolicy::Exact(expected.clone());
    let evidence = image_policy_evidence(&expected, Ok(observed), &policy);
    assert!(!evidence.contains("synthetic-private-label"));
    assert!(!evidence.contains(r"C:\"));
    assert!(evidence.contains("verbatim=true"));
    assert!(evidence.contains("lexical_equal=false"));
    assert!(evidence.contains("expected_canonical="));
    let failed =
        image_policy_evidence(&expected, Err("open_failed(test_code)".to_owned()), &policy);
    assert!(failed.contains("image_query=open_failed(test_code)"));
    assert!(!failed.contains("synthetic-private-label"));
    for (path, marker) in [
        (
            r"\Device\HarddiskVolume7\synthetic-private-label\sakura_engine.exe",
            "native_device=true",
        ),
        (
            r"\??\C:\synthetic-private-label\sakura_engine.exe",
            "nt_dos=true",
        ),
        (
            r"C:synthetic-private-label\sakura_engine.exe",
            "drive_prefix=true",
        ),
    ] {
        let evidence = image_policy_evidence(&expected, Ok(PathBuf::from(path)), &policy);
        assert!(evidence.contains(marker));
        assert!(evidence.contains("absolute=false"));
        assert!(!evidence.contains("synthetic-private-label"));
        assert!(!evidence.contains("HarddiskVolume7"));
    }
}

/// A double-NUL-terminated `KEY=VALUE\0...\0\0` block: this process's own
/// environment, plus [`CHILD_MARKER_ENV`], plus `extra`. For
/// `CREATE_UNICODE_ENVIRONMENT`.
///
/// Built explicitly rather than via `std::env::set_var` on this process
/// (which would make the child inherit the ambient environment
/// automatically with `lpEnvironment: None`): mutating process-global
/// environment state is exactly the kind of thing that is safe today, in a
/// single-threaded test, and a data race the day this stops being the only
/// thing touching it.
fn build_environment_block(extra: &[(&str, String)]) -> Vec<u16> {
    let mut block = Vec::new();
    for (key, value) in env::vars_os() {
        let overridden = key == CHILD_MARKER_ENV
            || extra
                .iter()
                .any(|(extra_key, _)| OsStr::new(extra_key) == key.as_os_str());
        if overridden {
            continue;
        }
        push_env_entry(&mut block, &key, &value);
    }
    push_env_entry(&mut block, OsStr::new(CHILD_MARKER_ENV), OsStr::new("1"));
    for (key, value) in extra {
        push_env_entry(&mut block, OsStr::new(key), OsStr::new(value.as_str()));
    }
    block.push(0); // the block's own terminating empty string
    block
}

fn push_env_entry(block: &mut Vec<u16>, key: &OsStr, value: &OsStr) {
    block.extend(key.encode_wide());
    block.push(u16::from(b'='));
    block.extend(value.encode_wide());
    block.push(0);
}

/// A scratch directory under `%TEMP%`, unique per process and per instant —
/// never inside the repository, and never reused across runs even if two
/// happen in the same millisecond on the same machine.
fn new_scratch_dir() -> PathBuf {
    let unique = format!(
        "sakura-appcontainer-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the system clock is after 1970")
            .as_nanos(),
    );
    let dir = env::temp_dir().join(unique);
    fs::create_dir(&dir).expect("create the scratch directory");
    dir
}

fn to_wide_nul(s: &str) -> Vec<u16> {
    let mut wide: Vec<u16> = s.encode_utf16().collect();
    wide.push(0);
    wide
}

#[cfg(test)]
mod tests {
    use super::parse_owned_engine_pid;

    #[test]
    fn the_sandbox_pid_contract_accepts_only_a_nonzero_decimal_pid() {
        assert_eq!(parse_owned_engine_pid("12345"), Ok(12345));
        assert!(parse_owned_engine_pid("0").is_err());
        assert!(parse_owned_engine_pid("-1").is_err());
        assert!(parse_owned_engine_pid("12.5").is_err());
        assert!(parse_owned_engine_pid("pid").is_err());
    }
}
