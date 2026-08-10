//! The engine process.
//!
//! Windowless: it is started by a per-user logon task and lives until the
//! session ends (DESIGN 4.3), so a console subsystem binary would flash a
//! window at every logon. `--verbose` attaches to the console of whatever
//! started it, which is how it stays diagnosable when run by hand without
//! costing every user a black rectangle at logon.

#![cfg(windows)]
#![windows_subsystem = "windows"]

use sakura_engine::event_log::{prune_default_dumps, EngineEvent, EventLog, WidthScanStrategy};
use sakura_engine::server::Server;
use std::time::Instant;

const TEST_PIPE_PREFIX: &str = r"\\.\pipe\SakuraInputEngineTest-";

/// Clean exit: a client asked the engine to stop.
const EXIT_OK: i32 = 0;
/// The engine could not start.
const EXIT_FAILED: i32 = 1;
/// Another engine already owns this session's pipe. Not an error — the
/// logon task and a manual launch can race, and the loser should be quiet
/// about it rather than leave a failure in the task scheduler's history.
const EXIT_ALREADY_RUNNING: i32 = 2;
/// This machine is below the instruction-set baseline the binary was built
/// for (DESIGN 3.2).
const EXIT_UNSUPPORTED_CPU: i32 = 3;

fn main() {
    let options = match startup_options(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("sakura-engine: {error}");
            std::process::exit(EXIT_FAILED);
        }
    };
    let started = Instant::now();
    let event_log = EventLog::open_default().ok();
    match prune_default_dumps() {
        Ok(report) => record(
            event_log.as_ref(),
            EngineEvent::DumpsPruned {
                removed: report.removed,
                failures: report.failures,
            },
        ),
        Err(error) => record(
            event_log.as_ref(),
            EngineEvent::DumpPruneFailed {
                os_error: error.raw_os_error().unwrap_or(0),
            },
        ),
    }
    let verbose = options.verbose;
    if verbose {
        attach_parent_console();
    }

    // Before the pipe, before the dictionary, before anything can be waiting
    // on the answer: which vector kernel this machine gets, decided once
    // (DESIGN 3.2). Naming the concrete kernel in the log is what makes a later
    // "why is it slower here?" a question with an answer.
    match sakura_core::simd::startup() {
        Ok(kernel_set) => {
            let kernel = kernel_set.width_scan().metadata();
            record(
                event_log.as_ref(),
                EngineEvent::Startup {
                    width_scan: WidthScanStrategy::from_name(kernel.name),
                },
            );
            if verbose {
                eprintln!("sakura-engine: vector kernel: {}", kernel.name);
            }
        }
        Err(error) => {
            record(event_log.as_ref(), EngineEvent::UnsupportedCpu);
            // The installer refuses to install on such a machine, so
            // reaching this means the files were copied here by hand.
            if verbose {
                eprintln!("sakura-engine: {error}");
            }
            std::process::exit(EXIT_UNSUPPORTED_CPU);
        }
    }

    std::process::exit(
        match run(
            verbose,
            started,
            event_log.as_ref(),
            options.test_pipe.as_deref(),
        ) {
            Ok(()) => {
                record(event_log.as_ref(), EngineEvent::Stopped);
                EXIT_OK
            }
            Err(error) if is_name_taken(&error) => {
                record(event_log.as_ref(), EngineEvent::AlreadyRunning);
                if verbose {
                    eprintln!("sakura-engine: another engine already has this session's pipe");
                }
                EXIT_ALREADY_RUNNING
            }
            Err(error) => {
                record(
                    event_log.as_ref(),
                    EngineEvent::StartupFailed {
                        hresult: error.code().0,
                    },
                );
                if verbose {
                    eprintln!("sakura-engine: {error}");
                }
                EXIT_FAILED
            }
        },
    );
}

fn run(
    verbose: bool,
    started: Instant,
    event_log: Option<&EventLog>,
    test_pipe: Option<&str>,
) -> windows::core::Result<()> {
    use std::sync::Arc;

    use windows::Win32::Foundation::E_FAIL;

    let dictionary_path = sakura_engine::dictionary::default_path()
        .map_err(|error| windows::core::Error::new(E_FAIL, format!("dictionary path: {error}")))?;
    let conversion = sakura_engine::dictionary::open(&dictionary_path)
        .map_err(|error| windows::core::Error::new(E_FAIL, error.to_string()))?;
    let user_dictionary_path = sakura_engine::user_dictionary::default_path().ok();
    let user_dictionary_watcher = user_dictionary_path.as_ref().and_then(|path| {
        match sakura_engine::user_dictionary::UserDictionaryWatcher::start(
            path.clone(),
            Arc::clone(&conversion),
        ) {
            Ok(watcher) => {
                if verbose {
                    if let Some(error) = watcher.last_error() {
                        eprintln!(
                            "sakura-engine: user dictionary update rejected; keeping the last valid snapshot: {error}"
                        );
                    }
                }
                Some(watcher)
            }
            Err(error) => {
                if verbose {
                    eprintln!("sakura-engine: user dictionary watcher unavailable: {error}");
                }
                None
            }
        }
    });
    let learning = match sakura_engine::learning::default_path()
        .and_then(|path| sakura_engine::learning::LearningService::open(&path))
    {
        Ok(service) => Arc::new(service),
        Err(error) => {
            if verbose {
                eprintln!(
                    "sakura-engine: learning store unavailable; using volatile learning: {error}"
                );
            }
            Arc::new(sakura_engine::learning::LearningService::memory())
        }
    };
    let learning_path = learning.path();
    let learning_maintenance =
        match sakura_engine::learning::LearningMaintenance::start(Arc::clone(&learning)) {
            Ok(maintenance) => Some(maintenance),
            Err(error) => {
                if verbose {
                    eprintln!("sakura-engine: learning maintenance unavailable: {error}");
                }
                None
            }
        };
    let (preferences, profiles, config_path) = match sakura_engine::configuration::default_path() {
        Ok(path) => match sakura_engine::configuration::load(&path) {
            Ok(loaded) => (loaded.preferences, loaded.profiles, Some(path)),
            Err(error) => {
                if verbose {
                    eprintln!("sakura-engine: configuration unavailable; using defaults: {error}");
                }
                let preferences = sakura_core::Preferences::default();
                (
                    preferences,
                    sakura_core::default_app_profiles(preferences),
                    Some(path),
                )
            }
        },
        Err(error) => {
            if verbose {
                eprintln!("sakura-engine: configuration path unavailable; using defaults: {error}");
            }
            let preferences = sakura_core::Preferences::default();
            (
                preferences,
                sakura_core::default_app_profiles(preferences),
                None,
            )
        }
    };
    let input_history = if preferences.developer_mode {
        match sakura_engine::input_history::default_path()
            .and_then(|path| sakura_engine::input_history::InputHistoryService::open(&path))
        {
            Ok(history) => Some(history),
            Err(error) => {
                if verbose {
                    eprintln!(
                        "sakura-engine: developer input history unavailable; continuing without it: {error}"
                    );
                }
                None
            }
        }
    } else {
        None
    };
    let prediction_requested =
        preferences.prediction_enabled || profiles.iter().any(|profile| profile.prediction_enabled);
    let profiles = Arc::<[sakura_core::AppProfile]>::from(profiles);
    let prediction_runtime = if prediction_requested {
        match sakura_engine::prediction::PredictionRuntime::start_with_learning(
            Arc::clone(&conversion),
            Arc::clone(&learning),
        ) {
            Ok(runtime) => Some(runtime),
            Err(error) => {
                if verbose {
                    eprintln!(
                        "sakura-engine: prediction worker unavailable; suggest disabled: {error}"
                    );
                }
                None
            }
        }
    } else {
        None
    };
    let long_conversion_runtime =
        match sakura_engine::long_conversion::LongConversionRuntime::discover(Arc::clone(
            &conversion,
        )) {
            Ok(runtime) => runtime,
            Err(error) => {
                if verbose {
                    eprintln!(
                        "sakura-engine: long-conversion reranker unavailable; using local ranking: {error}"
                    );
                }
                None
            }
        };
    let server = match (prediction_runtime.as_ref(), input_history.as_ref()) {
        (Some(runtime), Some(history)) => {
            Server::with_runtime_configuration_and_profiles_and_history(
                verbose,
                conversion,
                learning,
                runtime.service(),
                preferences,
                Arc::clone(&profiles),
                Arc::clone(history),
            )?
        }
        (Some(runtime), None) => Server::with_runtime_configuration_and_profiles(
            verbose,
            conversion,
            learning,
            runtime.service(),
            preferences,
            Arc::clone(&profiles),
        )?,
        (None, Some(history)) => Server::with_configuration_and_profiles_and_history(
            verbose,
            conversion,
            learning,
            preferences,
            Arc::clone(&profiles),
            Arc::clone(history),
        )?,
        (None, None) => Server::with_configuration_and_profiles(
            verbose,
            conversion,
            learning,
            preferences,
            Arc::clone(&profiles),
        )?,
    };
    let server = match long_conversion_runtime.as_ref() {
        Some(runtime) => server.with_long_conversion(runtime.service()),
        None => server,
    };
    let server = match test_pipe {
        Some(pipe_name) => server.with_explicit_test_pipe(pipe_name.to_owned()),
        None => server,
    };
    if verbose {
        eprintln!("sakura-engine: dictionary: {}", dictionary_path.display());
        if let Some(path) = learning_path {
            eprintln!("sakura-engine: learning: {}", path.display());
        }
        if let Some(path) = config_path {
            eprintln!("sakura-engine: configuration: {}", path.display());
        }
        if let Some(history) = input_history.as_ref() {
            if let Some(path) = history.path() {
                eprintln!("sakura-engine: developer input history: {}", path.display());
            }
        } else if preferences.developer_mode {
            eprintln!("sakura-engine: developer input history: unavailable");
        }
        if let Some(watcher) = user_dictionary_watcher.as_ref() {
            eprintln!(
                "sakura-engine: user dictionary: {}",
                watcher.path().display()
            );
        }
        eprintln!("sakura-engine: listening on {}", server.pipe_name());
    }
    record(
        event_log,
        EngineEvent::Ready {
            elapsed_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        },
    );
    let result = server.run();
    drop(long_conversion_runtime);
    // The watcher owns a polling thread; dropping it here signals and joins
    // that thread before the engine process reaches its terminal state.
    drop(user_dictionary_watcher);
    if let Some(maintenance) = learning_maintenance {
        let _ = maintenance.stop();
    }
    if let Some(runtime) = prediction_runtime {
        let _ = runtime.stop();
    }
    if let Some(history) = input_history {
        let _ = history.stop();
    }
    result
}

#[derive(Debug, Default, PartialEq, Eq)]
struct StartupOptions {
    verbose: bool,
    test_pipe: Option<String>,
}

fn startup_options(arguments: impl IntoIterator<Item = String>) -> Result<StartupOptions, String> {
    let mut options = StartupOptions::default();
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--verbose" => options.verbose = true,
            "--test-pipe" => {
                let value = arguments
                    .next()
                    .ok_or_else(|| "--test-pipe requires a named-pipe path".to_owned())?;
                if options
                    .test_pipe
                    .replace(validate_test_pipe(value)?)
                    .is_some()
                {
                    return Err("--test-pipe may be supplied only once".to_owned());
                }
            }
            _ => {}
        }
    }
    Ok(options)
}

/// Restricts the integration-test override to the private namespace generated
/// by `tests/common`. This is deliberately an argument, never an environment
/// variable, so production pipe discovery remains ambient-state free.
fn validate_test_pipe(value: String) -> Result<String, String> {
    let suffix = value
        .strip_prefix(TEST_PIPE_PREFIX)
        .ok_or_else(|| format!("--test-pipe must start with {TEST_PIPE_PREFIX:?}"))?;
    if suffix.is_empty()
        || suffix.len() > 160
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(
            "--test-pipe must have a non-empty ASCII alphanumeric, '-' or '_' suffix".to_owned(),
        );
    }
    Ok(value)
}

fn record(log: Option<&EventLog>, event: EngineEvent) {
    if let Some(log) = log {
        let _ = log.record(event);
    }
}

/// What `FILE_FLAG_FIRST_PIPE_INSTANCE` reports when the name is already
/// taken. It denies access rather than saying "in use", because from the
/// object manager's point of view we asked to own a name somebody else
/// already owns.
fn is_name_taken(error: &windows::core::Error) -> bool {
    use windows::core::HRESULT;
    use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_PIPE_BUSY};
    let code = error.code();
    code == HRESULT::from_win32(ERROR_ACCESS_DENIED.0)
        || code == HRESULT::from_win32(ERROR_PIPE_BUSY.0)
}

/// Borrows the launching terminal's console so `--verbose` output has
/// somewhere to go. Failure is expected and ignored: started from a logon
/// task there is no parent console, which is exactly the case this binary
/// is built windowless for.
fn attach_parent_console() {
    use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    // SAFETY: no arguments to get wrong, and the only failure mode —
    // having no parent console, or already having one — is reported as an
    // error rather than as undefined behaviour.
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pipe_option_accepts_only_the_private_test_namespace() {
        let pipe = format!("{TEST_PIPE_PREFIX}worker-abc_123");
        assert_eq!(validate_test_pipe(pipe.clone()), Ok(pipe));
        assert!(validate_test_pipe(r"\\.\pipe\SakuraInput".to_owned()).is_err());
        assert!(validate_test_pipe(format!("{TEST_PIPE_PREFIX}has/slash")).is_err());
        assert!(validate_test_pipe(TEST_PIPE_PREFIX.to_string()).is_err());
    }

    #[test]
    fn production_startup_options_do_not_change_pipe_discovery() {
        assert_eq!(
            startup_options(["--verbose".to_owned()]),
            Ok(StartupOptions {
                verbose: true,
                test_pipe: None,
            })
        );
        assert_eq!(
            startup_options(["--test-pipe".to_owned(), format!("{TEST_PIPE_PREFIX}one")]),
            Ok(StartupOptions {
                verbose: false,
                test_pipe: Some(format!("{TEST_PIPE_PREFIX}one")),
            })
        );
    }

    #[test]
    fn test_pipe_option_requires_one_valid_unique_value() {
        assert!(startup_options(["--test-pipe".to_owned()]).is_err());
        assert!(startup_options([
            "--test-pipe".to_owned(),
            format!("{TEST_PIPE_PREFIX}one"),
            "--test-pipe".to_owned(),
            format!("{TEST_PIPE_PREFIX}two"),
        ])
        .is_err());
    }
}
