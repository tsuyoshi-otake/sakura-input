//! Argument parsing, by hand.
//!
//! There is no argument-parsing crate here for the same reason there is no
//! MeCab: shipping binaries depend on the `windows` family and nothing else
//! (DESIGN 3.1). The surface is a small set of verbs and options, so the cost of
//! that rule is this file.
//!
//! One property is worth stating because it is load-bearing rather than
//! stylistic: an unrecognized argument is an error, never a no-op. This
//! tool is driven by an installer's `[Run]` and `[UninstallRun]` lines,
//! where a typo that silently degrades to "did nothing, exit 0" would
//! leave a machine registered halfway and report success.

use std::path::PathBuf;
use std::time::Duration;

/// How to find the 32-bit DLL that lets 32-bit host applications load the
/// text service on a 64-bit machine.
#[derive(Debug, PartialEq, Eq)]
pub enum Wow64 {
    /// Look in the conventional place next to the payload; if it is not
    /// there, say so and continue. A 32-bit-only or ARM64-only install is
    /// legitimately without one.
    Auto,
    /// A path the caller named. Missing is then an error, not a shrug.
    At(PathBuf),
    /// Deliberately none.
    Skip,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    /// Machine-wide: COM class, TSF categories, language profile.
    Register {
        dll: Option<PathBuf>,
        wow64: Wow64,
        enabled_by_default: bool,
    },
    /// Machine-wide removal, ordered to fail safe (DESIGN 12.1).
    Unregister,
    /// Machine-wide: install the SYSTEM logon task that retries old-payload cleanup.
    InstallCleanupTask,
    /// Machine-wide removal of the SYSTEM payload-cleanup task.
    RemoveCleanupTask,
    /// Internal machine-wide maintenance action run by the cleanup task.
    CleanupPayloads,
    /// Per-user: input list entry plus the logon task.
    EnableProfile {
        logon_stub: Option<PathBuf>,
    },
    /// Per-user removal used by the installer before replacing an old task.
    RemoveLogonTask,
    /// Per-user removal.
    DisableProfile,
    /// Install bounded WER LocalDumps policy for Sakura executables.
    ConfigureDiagnostics,
    /// Remove the WER policy but retain existing dumps.
    RemoveDiagnostics,
    /// Ask a running engine to exit, and wait until it has.
    Stop {
        budget: Duration,
    },
    Help,
}

pub const USAGE: &str = "\
sakura_regtool — registration helper for the Sakura Input text service

USAGE:
    sakura_regtool <command> [options]

MACHINE-WIDE (requires administrator):
    --register              Register the COM class, the TSF categories and
                            the Japanese language profile.
        --dll <path>        The text service DLL matching this process's
                            bitness. Default: sakura_tsf.dll beside this
                            executable.
        --wow64-dll <path>  The x86 build, so 32-bit host applications can
                            load the text service on a 64-bit machine.
                            Default: x86\\sakura_tsf.dll beside this
                            executable, if present.
        --no-wow64          Register without one. 32-bit applications will
                            have no IME.
        --default           Also make Sakura Input the default input method
                            for Japanese.

    --unregister            Remove the profile, the categories and the
                            class, in that order.

    --install-cleanup-task  Install the elevated machine-wide logon task that
                            removes inactive versioned payloads.
    --remove-cleanup-task   Remove that maintenance task.

    --cleanup-payloads      Remove inactive payload generations now. This is
                            normally invoked by the maintenance task.

    --configure-diagnostics Configure local WER minidumps for Sakura
                            processes (DumpCount=5; never uploaded).
    --remove-diagnostics    Remove that WER policy but keep existing dumps.

PER-USER (must run as the signed-in user, not as an elevated installer):
    --enable-profile        Add the text service to this user's input list
                            and register the logon task that starts the
                            engine.
        --logon-stub <path> Default: sakura_logon.exe beside this executable.

    --remove-logon-task     Remove only this user's logon task. The installer
                            uses this during upgrades so an old task with a
                            stale ACL cannot block re-registration.

    --disable-profile       Remove both again.

ANY USER:
    --stop                  Ask a running engine to exit, and wait for it.
        --timeout <ms>      How long to wait. Default: 5000.

    --help                  This text.

EXIT CODES:
    0  success
    1  the command failed
    2  the command line was not understood
";

/// Parses the argument list (without `argv[0]`).
///
/// The error is the message to show the user, not a code to translate.
pub fn parse<I>(args: I) -> Result<Command, String>
where
    I: IntoIterator<Item = String>,
{
    let mut args = args.into_iter();
    let mut verb: Option<String> = None;
    let mut seen: Vec<&'static str> = Vec::new();
    let mut dll = None;
    let mut wow64_dll = None;
    let mut no_wow64 = false;
    let mut enabled_by_default = false;
    let mut logon_stub = None;
    let mut timeout_ms = 5_000u64;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--register"
            | "--unregister"
            | "--install-cleanup-task"
            | "--remove-cleanup-task"
            | "--cleanup-payloads"
            | "--configure-diagnostics"
            | "--remove-diagnostics"
            | "--enable-profile"
            | "--remove-logon-task"
            | "--disable-profile"
            | "--stop"
            | "--help"
            | "-h" => {
                if let Some(first) = &verb {
                    return Err(format!(
                        "{first} and {arg} are separate commands; run one at a time"
                    ));
                }
                verb = Some(arg);
            }
            "--dll" => {
                seen.push("--dll");
                dll = Some(value(&mut args, &arg)?.into());
            }
            "--wow64-dll" => {
                seen.push("--wow64-dll");
                wow64_dll = Some(value(&mut args, &arg)?.into());
            }
            "--no-wow64" => {
                seen.push("--no-wow64");
                no_wow64 = true;
            }
            "--default" => {
                seen.push("--default");
                enabled_by_default = true;
            }
            "--logon-stub" => {
                seen.push("--logon-stub");
                logon_stub = Some(value(&mut args, &arg)?.into());
            }
            "--timeout" => {
                seen.push("--timeout");
                let raw = value(&mut args, &arg)?;
                timeout_ms = raw
                    .parse()
                    .map_err(|_| format!("--timeout wants milliseconds, got {raw:?}"))?;
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }

    let verb = verb.ok_or_else(|| "no command given".to_string())?;
    reject_options_that_do_not_apply(&verb, &seen)?;
    let command = match verb.as_str() {
        "--help" | "-h" => Command::Help,
        "--register" => {
            let wow64 = match (no_wow64, wow64_dll) {
                (true, Some(_)) => {
                    return Err("--no-wow64 and --wow64-dll contradict each other".into())
                }
                (true, None) => Wow64::Skip,
                (false, Some(path)) => Wow64::At(path),
                (false, None) => Wow64::Auto,
            };
            Command::Register {
                dll,
                wow64,
                enabled_by_default,
            }
        }
        "--unregister" => Command::Unregister,
        "--install-cleanup-task" => Command::InstallCleanupTask,
        "--remove-cleanup-task" => Command::RemoveCleanupTask,
        "--cleanup-payloads" => Command::CleanupPayloads,
        "--configure-diagnostics" => Command::ConfigureDiagnostics,
        "--remove-diagnostics" => Command::RemoveDiagnostics,
        "--enable-profile" => Command::EnableProfile { logon_stub },
        "--remove-logon-task" => Command::RemoveLogonTask,
        "--disable-profile" => Command::DisableProfile,
        "--stop" => Command::Stop {
            budget: Duration::from_millis(timeout_ms),
        },
        _ => unreachable!("verb was matched above"),
    };

    Ok(command)
}

/// An option that belongs to a different verb is a mistake worth naming.
/// `--register --engine foo.exe` looks like it configured something; it did
/// not, and finding that out from an installer log is expensive.
fn reject_options_that_do_not_apply(verb: &str, seen: &[&str]) -> Result<(), String> {
    let allowed: &[&str] = match verb {
        "--register" => &["--dll", "--wow64-dll", "--no-wow64", "--default"],
        "--enable-profile" => &["--logon-stub"],
        "--stop" => &["--timeout"],
        _ => &[],
    };
    match seen.iter().find(|option| !allowed.contains(option)) {
        Some(option) => Err(format!("{option} does not apply to {verb}")),
        None => Ok(()),
    }
}

fn value<I>(args: &mut I, flag: &str) -> Result<String, String>
where
    I: Iterator<Item = String>,
{
    args.next()
        .filter(|v| !v.starts_with("--"))
        .ok_or_else(|| format!("{flag} needs a value"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(args: &[&str]) -> Result<Command, String> {
        parse(args.iter().map(|s| (*s).to_string()))
    }

    #[test]
    fn register_defaults_to_finding_its_own_payload() {
        assert_eq!(
            parse_args(&["--register"]),
            Ok(Command::Register {
                dll: None,
                wow64: Wow64::Auto,
                enabled_by_default: false,
            })
        );
    }

    #[test]
    fn the_two_ways_of_saying_no_wow64_dll_are_distinguished() {
        let explicit = parse_args(&["--register", "--wow64-dll", "x86\\sakura_tsf.dll"]);
        assert!(matches!(
            explicit,
            Ok(Command::Register {
                wow64: Wow64::At(_),
                ..
            })
        ));
        assert!(matches!(
            parse_args(&["--register", "--no-wow64"]),
            Ok(Command::Register {
                wow64: Wow64::Skip,
                ..
            })
        ));
    }

    #[test]
    fn contradicting_yourself_about_wow64_is_refused() {
        assert!(parse_args(&["--register", "--no-wow64", "--wow64-dll", "a.dll"]).is_err());
    }

    /// The installer's uninstall step halts on a nonzero exit code, so a
    /// typo must produce one rather than a successful no-op.
    #[test]
    fn an_unknown_argument_is_an_error_not_a_shrug() {
        assert!(parse_args(&["--unregsiter"]).is_err());
        assert!(parse_args(&[]).is_err());
    }

    #[test]
    fn two_commands_at_once_are_refused() {
        assert!(parse_args(&["--register", "--unregister"]).is_err());
    }

    /// `--dll --default` would otherwise swallow the next flag as a path
    /// and register a DLL named "--default".
    #[test]
    fn a_flag_is_never_mistaken_for_a_missing_value() {
        assert!(parse_args(&["--register", "--dll", "--default"]).is_err());
        assert!(parse_args(&["--register", "--dll"]).is_err());
    }

    #[test]
    fn an_option_belonging_to_another_verb_is_refused() {
        // Silently ignoring this would report success for a --register
        // that never saw the engine path the caller thought it passed.
        assert_eq!(
            parse_args(&["--register", "--logon-stub", "sakura_logon.exe"]),
            Err("--logon-stub does not apply to --register".into())
        );
        assert!(parse_args(&["--stop", "--dll", "sakura_tsf.dll"]).is_err());
    }

    #[test]
    fn the_stop_budget_is_configurable_and_validated() {
        assert_eq!(
            parse_args(&["--stop", "--timeout", "250"]),
            Ok(Command::Stop {
                budget: Duration::from_millis(250)
            })
        );
        assert!(parse_args(&["--stop", "--timeout", "soon"]).is_err());
    }

    #[test]
    fn diagnostic_policy_commands_are_unambiguous_and_take_no_options() {
        assert_eq!(
            parse_args(&["--configure-diagnostics"]),
            Ok(Command::ConfigureDiagnostics)
        );
        assert_eq!(
            parse_args(&["--remove-diagnostics"]),
            Ok(Command::RemoveDiagnostics)
        );
        assert!(parse_args(&["--configure-diagnostics", "--timeout", "1"]).is_err());
    }
}
