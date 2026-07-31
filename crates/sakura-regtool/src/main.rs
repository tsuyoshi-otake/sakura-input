//! `sakura_regtool.exe` — the installer's hands.
//!
//! Everything this tool knows how to do lives in `sakura_reg` and
//! `sakura_ipc`; what is here is the command line, the ordering, and the
//! refusals. The DLL's own `DllRegisterServer` calls the same library
//! functions, so a machine registered by `regsvr32` during development and
//! one registered by the installer end up identical (DESIGN 12.1).
//!
//! Two orderings in this file are safety properties rather than taste:
//!
//! * On the way in, the thing that lets Windows *try to activate* the text
//!   service goes last; on the way out it goes first. Anything else can
//!   leave a live profile pointing at a class or a binary that is not
//!   there, which is the state where the user has no working keyboard.
//! * Per-user work is refused unless it would land on the signed-in user
//!   (see [`interactive`]). Every API involved would happily succeed
//!   against the wrong hive.
//!
//! Exit codes matter here more than in most programs: the uninstaller
//! halts on a nonzero exit from `--unregister` instead of proceeding to
//! delete files (DESIGN 12.2), so this must not report success for work it
//! did not do.

#![cfg(windows)]

mod cli;
mod interactive;
mod paths;
mod shutdown;

use std::path::{Path, PathBuf};
use std::time::Duration;

use cli::{Command, Wow64};
use sakura_reg::{launcher, user_profile, ComApartment};
use windows::core::{Error, HRESULT};
use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, E_ACCESSDENIED};

const EXIT_OK: i32 = 0;
const EXIT_FAILED: i32 = 1;
const EXIT_USAGE: i32 = 2;

fn main() {
    let command = match cli::parse(std::env::args().skip(1)) {
        Ok(command) => command,
        Err(message) => {
            eprintln!("sakura_regtool: {message}");
            eprintln!();
            eprint!("{}", cli::USAGE);
            std::process::exit(EXIT_USAGE);
        }
    };

    std::process::exit(match run(command) {
        Ok(()) => EXIT_OK,
        Err(message) => {
            eprintln!("sakura_regtool: {message}");
            EXIT_FAILED
        }
    });
}

fn run(command: Command) -> Result<(), String> {
    match command {
        Command::Help => {
            print!("{}", cli::USAGE);
            Ok(())
        }
        Command::Register {
            dll,
            wow64,
            enabled_by_default,
        } => register(dll, wow64, enabled_by_default),
        Command::Unregister => unregister(),
        Command::EnableProfile { engine, renderer } => enable_profile(engine, renderer),
        Command::DisableProfile => disable_profile(),
        Command::Stop { budget } => stop(budget),
    }
}

fn register(dll: Option<PathBuf>, wow64: Wow64, enabled_by_default: bool) -> Result<(), String> {
    let native = paths::required(dll, paths::TSF_DLL, "text service DLL")?;
    let wow64 = match wow64 {
        Wow64::Skip => None,
        Wow64::At(path) => Some(paths::required(
            Some(path),
            paths::TSF_DLL,
            "x86 text service DLL",
        )?),
        Wow64::Auto => paths::optional(
            None,
            &format!("{}\\{}", paths::WOW64_DIR, paths::TSF_DLL),
            "x86 text service DLL",
        )?,
    };
    if wow64.is_none() {
        eprintln!(
            "note: registering without an x86 build; 32-bit applications on this \
             machine will have no IME"
        );
    }

    let _com = ComApartment::new().map_err(|error| explain("COM initialization", &error))?;
    sakura_reg::register_all(&native, wow64.as_deref(), enabled_by_default)
        .map_err(|error| explain("registration", &error))?;

    println!("registered {}", native.display());
    if let Some(path) = &wow64 {
        println!("registered {} for 32-bit hosts", path.display());
    }
    if enabled_by_default {
        println!("Sakura Input is now the default input method for Japanese");
    }
    Ok(())
}

fn unregister() -> Result<(), String> {
    let _com = ComApartment::new().map_err(|error| explain("COM initialization", &error))?;
    // `unregister_all` runs every step even after one fails and reports the
    // first error, because stopping halfway is what leaves a machine with
    // an IME that neither loads nor uninstalls.
    sakura_reg::unregister_all().map_err(|error| explain("deregistration", &error))?;
    println!("removed the machine-wide registration");
    Ok(())
}

fn enable_profile(engine: Option<PathBuf>, renderer: Option<PathBuf>) -> Result<(), String> {
    let account = interactive::require_signed_in_user().map_err(|why| why.to_string())?;

    let engine = paths::required(engine, paths::ENGINE_EXE, "engine executable")?;
    let renderer = paths::optional(renderer, paths::RENDERER_EXE, "renderer executable")?;
    let mut programs: Vec<&Path> = vec![engine.as_path()];
    match &renderer {
        Some(path) => programs.push(path.as_path()),
        None => eprintln!(
            "note: no renderer alongside the engine; the mode indicator and \
             candidate window will not start at logon"
        ),
    }

    let _com = ComApartment::new().map_err(|error| explain("COM initialization", &error))?;

    // The launcher first. If this order were reversed and the task failed,
    // the user would have Sakura Input in their input list with nothing
    // behind it — an IME that swallows keystrokes. This way round, a
    // failure leaves a task that starts an engine nobody talks to.
    launcher::register(&programs).map_err(|error| explain("logon task registration", &error))?;
    user_profile::enable().map_err(|error| explain("adding to the input list", &error))?;

    println!("enabled Sakura Input for {account}");
    Ok(())
}

fn disable_profile() -> Result<(), String> {
    let account = interactive::require_signed_in_user().map_err(|why| why.to_string())?;
    let _com = ComApartment::new().map_err(|error| explain("COM initialization", &error))?;

    // Mirror image of enabling: the input list entry goes first, so no
    // moment exists where Windows can activate a text service whose engine
    // is no longer scheduled to run. Both steps run even if the first
    // fails — a half-removed per-user state is the thing worth avoiding.
    let removed_from_list = user_profile::disable();
    let removed_task = launcher::unregister();

    match removed_from_list.and(removed_task) {
        Ok(()) => {
            println!("disabled Sakura Input for {account}");
            Ok(())
        }
        Err(error) => Err(explain("per-user removal", &error)),
    }
}

fn stop(budget: Duration) -> Result<(), String> {
    match shutdown::stop(budget)? {
        shutdown::Outcome::NotRunning => println!("no engine was running"),
        shutdown::Outcome::Stopped => println!("the engine stopped"),
    }
    Ok(())
}

/// Turns a Win32/COM failure into something a person reading an installer
/// log can act on. Access denied is singled out because it is both the
/// most common failure here and the one whose default message ("Access is
/// denied.") does not say what to do about it.
fn explain(action: &str, error: &Error) -> String {
    let message = format!("{action} failed: {error}");
    let denied = error.code() == E_ACCESSDENIED
        || error.code() == HRESULT::from_win32(ERROR_ACCESS_DENIED.0);
    if denied {
        format!(
            "{message}\n\
             hint: machine-wide registration needs an elevated (administrator) \
             command prompt"
        )
    } else {
        message
    }
}
