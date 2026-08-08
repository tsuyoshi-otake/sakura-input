use std::path::{Path, PathBuf};

use sakura_core::{AppProfile, Preset, SuggestAccept, UserDictionaryEntry, UserPartOfSpeech};
use sakura_proto::Mode;
use sakura_settings::user_dictionary::{self, ImportMode};
use sakura_settings::{
    configuration::ConfigurationDocument, diagnostics, formats, input_history, learning, paths,
    updater,
};

pub const USAGE: &str = "\
Usage: sakura_settings <command>\n\
\n\
  config show\n\
  config set keymap <ms-ime|atok>\n\
  config set prediction <on|off>\n\
  config set suggest <tab|shift-enter|disabled>\n\
  config set developer-mode <on|off>\n\
  profile list\n\
  profile set <process.exe> <mode> <on|off> <suggest>\n\
  profile delete <process.exe>\n\
  dictionary list\n\
  dictionary add <reading> <surface> <pos> [comment]\n\
  dictionary update <old-reading> <old-surface> <reading> <surface> <pos> [comment]\n\
  dictionary delete <reading> <surface>\n\
  dictionary import <file> <auto|sakura|ms-ime|atok|mozc> <merge|replace>\n\
  dictionary export <file> <sakura|ms-ime|atok|mozc>\n\
  learning show\n\
  learning export <file>\n\
  learning clear\n\
  history show\n\
  history export <file>\n\
  history clear\n\
  history stats\n\
  diagnostics show [text|tsv]\n\
  diagnostics clear\n\
  update status\n\
  update enable\n\
  update disable\n\
  update apply\n\
  help\n\
\n\
Modes: direct, hiragana, katakana, half-katakana, full-alnum, half-alnum\n\
Parts of speech: noun, proper-noun, personal-name, family-name, first-name,\n\
  organization, place, sa-noun, adjectival-noun, number, alphabet, symbol,\n\
  adverb, prenoun-adjectival, conjunction, interjection, prefix,\n\
  counter-suffix, generic-suffix, person-name-suffix, place-name-suffix\n";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Help,
    ConfigShow,
    ConfigSet {
        setting: ConfigSetting,
        value: String,
    },
    ProfileList,
    ProfileSet {
        process_name: String,
        default_mode: Mode,
        prediction_enabled: bool,
        suggest_accept: SuggestAccept,
    },
    ProfileDelete {
        process_name: String,
    },
    DictionaryList,
    DictionaryAdd {
        entry: UserDictionaryEntry,
    },
    DictionaryUpdate {
        old_reading: String,
        old_surface: String,
        replacement: UserDictionaryEntry,
    },
    DictionaryDelete {
        reading: String,
        surface: String,
    },
    DictionaryImport {
        path: PathBuf,
        format: Option<formats::DictionaryFormat>,
        mode: ImportMode,
    },
    DictionaryExport {
        path: PathBuf,
        format: formats::DictionaryFormat,
    },
    LearningShow,
    LearningExport {
        path: PathBuf,
    },
    LearningClear,
    HistoryShow,
    HistoryExport {
        path: PathBuf,
    },
    HistoryClear,
    HistoryStats,
    DiagnosticsShow {
        tsv: bool,
    },
    DiagnosticsClear,
    UpdateStatus,
    UpdateEnable,
    UpdateDisable,
    UpdateApply,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigSetting {
    Keymap,
    Prediction,
    Suggest,
    DeveloperMode,
}

pub fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Command, String> {
    let args: Vec<String> = arguments.into_iter().collect();
    let strings: Vec<&str> = args.iter().map(String::as_str).collect();
    match strings.as_slice() {
        ["help" | "--help" | "-h"] => Ok(Command::Help),
        ["config", "show"] => Ok(Command::ConfigShow),
        ["config", "set", setting, value] => Ok(Command::ConfigSet {
            setting: match *setting {
                "keymap" => ConfigSetting::Keymap,
                "prediction" => ConfigSetting::Prediction,
                "suggest" => ConfigSetting::Suggest,
                "developer-mode" => ConfigSetting::DeveloperMode,
                _ => return Err(format!("unknown configuration setting {setting:?}")),
            },
            value: (*value).to_owned(),
        }),
        ["profile", "list"] => Ok(Command::ProfileList),
        ["profile", "set", process_name, mode, prediction, suggest] => Ok(Command::ProfileSet {
            process_name: (*process_name).to_owned(),
            default_mode: parse_mode(mode)?,
            prediction_enabled: parse_switch(prediction)?,
            suggest_accept: parse_suggest(suggest)?,
        }),
        ["profile", "delete", process_name] => Ok(Command::ProfileDelete {
            process_name: (*process_name).to_owned(),
        }),
        ["dictionary", "list"] => Ok(Command::DictionaryList),
        ["dictionary", "add", reading, surface, pos] => Ok(Command::DictionaryAdd {
            entry: dictionary_entry(reading, surface, pos, "")?,
        }),
        ["dictionary", "add", reading, surface, pos, comment] => Ok(Command::DictionaryAdd {
            entry: dictionary_entry(reading, surface, pos, comment)?,
        }),
        ["dictionary", "update", old_reading, old_surface, reading, surface, pos] => {
            Ok(Command::DictionaryUpdate {
                old_reading: (*old_reading).to_owned(),
                old_surface: (*old_surface).to_owned(),
                replacement: dictionary_entry(reading, surface, pos, "")?,
            })
        }
        ["dictionary", "update", old_reading, old_surface, reading, surface, pos, comment] => {
            Ok(Command::DictionaryUpdate {
                old_reading: (*old_reading).to_owned(),
                old_surface: (*old_surface).to_owned(),
                replacement: dictionary_entry(reading, surface, pos, comment)?,
            })
        }
        ["dictionary", "delete", reading, surface] => Ok(Command::DictionaryDelete {
            reading: (*reading).to_owned(),
            surface: (*surface).to_owned(),
        }),
        ["dictionary", "import", path, format, mode] => Ok(Command::DictionaryImport {
            path: PathBuf::from(path),
            format: parse_import_format(format)?,
            mode: match *mode {
                "merge" => ImportMode::Merge,
                "replace" => ImportMode::Replace,
                _ => return Err(format!("unknown dictionary import mode {mode:?}")),
            },
        }),
        ["dictionary", "export", path, format] => Ok(Command::DictionaryExport {
            path: PathBuf::from(path),
            format: parse_export_format(format)?,
        }),
        ["learning", "show"] => Ok(Command::LearningShow),
        ["learning", "export", path] => Ok(Command::LearningExport {
            path: PathBuf::from(path),
        }),
        ["learning", "clear"] => Ok(Command::LearningClear),
        ["history", "show"] => Ok(Command::HistoryShow),
        ["history", "export", path] => Ok(Command::HistoryExport {
            path: PathBuf::from(path),
        }),
        ["history", "clear"] => Ok(Command::HistoryClear),
        ["history", "stats"] => Ok(Command::HistoryStats),
        ["diagnostics", "show"] | ["diagnostics", "show", "text"] => {
            Ok(Command::DiagnosticsShow { tsv: false })
        }
        ["diagnostics", "show", "tsv"] => Ok(Command::DiagnosticsShow { tsv: true }),
        ["diagnostics", "clear"] => Ok(Command::DiagnosticsClear),
        ["update", "status"] => Ok(Command::UpdateStatus),
        ["update", "enable"] => Ok(Command::UpdateEnable),
        ["update", "disable"] => Ok(Command::UpdateDisable),
        ["update", "apply"] => Ok(Command::UpdateApply),
        [] => Err("no command supplied".to_owned()),
        _ => Err(format!("unrecognized command: {}", strings.join(" "))),
    }
}

/// Re-anchors an import/export file operand on the caller's directory.
///
/// The install-root `sakura_settings.exe` starts the versioned payload with
/// its working directory set to the payload's own folder under Program Files.
/// Without this, `learning export report.tsv` resolved inside that read-only
/// folder and failed with a bare access-denied error that named no path.
/// Absolute operands and a missing base are left exactly as they were.
pub fn resolve_file_operands(command: Command, caller_directory: Option<&Path>) -> Command {
    let Some(base) = caller_directory else {
        return command;
    };
    let anchor = |path: PathBuf| {
        if path.is_absolute() {
            path
        } else {
            base.join(path)
        }
    };
    match command {
        Command::DictionaryImport { path, format, mode } => Command::DictionaryImport {
            path: anchor(path),
            format,
            mode,
        },
        Command::DictionaryExport { path, format } => Command::DictionaryExport {
            path: anchor(path),
            format,
        },
        Command::LearningExport { path } => Command::LearningExport { path: anchor(path) },
        Command::HistoryExport { path } => Command::HistoryExport { path: anchor(path) },
        other => other,
    }
}

pub fn run(command: Command) -> Result<(), String> {
    match command {
        Command::Help => print!("{USAGE}"),
        Command::ConfigShow => {
            let document = ConfigurationDocument::load(&paths::configuration().map_err(display)?)
                .map_err(display)?;
            print_configuration(&document);
        }
        Command::ConfigSet { setting, value } => {
            let path = paths::configuration().map_err(display)?;
            let mut document = ConfigurationDocument::load(&path).map_err(display)?;
            let mut developer_mode = None;
            match setting {
                ConfigSetting::Keymap => {
                    document.preferences.keymap_preset = Preset::from_name(&value)
                        .ok_or_else(|| format!("unknown keymap preset {value:?}"))?;
                }
                ConfigSetting::Prediction => {
                    document.preferences.prediction_enabled = parse_switch(&value)?;
                }
                ConfigSetting::Suggest => {
                    document.preferences.suggest_accept = parse_suggest(&value)?;
                }
                ConfigSetting::DeveloperMode => {
                    let enabled = parse_switch(&value)?;
                    document.preferences.developer_mode = enabled;
                    developer_mode = Some(enabled);
                }
            }
            document.save(&path).map_err(display)?;
            println!("saved {}", path.display());
            if let Some(enabled) = developer_mode {
                match paths::input_history()
                    .map_err(display)
                    .and_then(|history_path| input_history::stats(&history_path).map_err(display))
                {
                    Ok(stats) => println!(
                        "developer-history\t{}",
                        developer_history_terminal(enabled, stats)
                    ),
                    Err(error) => {
                        println!("developer-history\tstatus-unavailable-restart-required");
                        eprintln!("could not verify the running history service: {error}");
                    }
                }
            }
        }
        Command::ProfileList => {
            let document = ConfigurationDocument::load(&paths::configuration().map_err(display)?)
                .map_err(display)?;
            println!("process\tdefault-mode\tprediction\tsuggest");
            for profile in document.profiles {
                println!(
                    "{}\t{}\t{}\t{}",
                    profile.process_name,
                    mode_name(profile.default_mode),
                    switch_name(profile.prediction_enabled),
                    profile.suggest_accept.name()
                );
            }
        }
        Command::ProfileSet {
            process_name,
            default_mode,
            prediction_enabled,
            suggest_accept,
        } => {
            let path = paths::configuration().map_err(display)?;
            let mut document = ConfigurationDocument::load(&path).map_err(display)?;
            let normalizer = document
                .profiles
                .iter()
                .find(|profile| profile.matches(&process_name))
                .map_or(document.preferences.normalizer, |profile| {
                    profile.normalizer
                });
            document
                .upsert_profile(AppProfile {
                    process_name,
                    default_mode,
                    normalizer,
                    prediction_enabled,
                    suggest_accept,
                })
                .map_err(display)?;
            document.save(&path).map_err(display)?;
            println!("saved {}", path.display());
        }
        Command::ProfileDelete { process_name } => {
            let path = paths::configuration().map_err(display)?;
            let mut document = ConfigurationDocument::load(&path).map_err(display)?;
            document.remove_profile(&process_name).map_err(display)?;
            document.save(&path).map_err(display)?;
            println!("removed profile {process_name}");
        }
        Command::DictionaryList => {
            let dictionary = user_dictionary::load(&paths::user_dictionary().map_err(display)?)
                .map_err(display)?;
            print!("{}", dictionary.to_tsv());
        }
        Command::DictionaryAdd { entry } => {
            let path = paths::user_dictionary().map_err(display)?;
            let dictionary = user_dictionary::add(&path, entry).map_err(display)?;
            println!("saved {} entries to {}", dictionary.len(), path.display());
        }
        Command::DictionaryUpdate {
            old_reading,
            old_surface,
            replacement,
        } => {
            let path = paths::user_dictionary().map_err(display)?;
            let dictionary =
                user_dictionary::update(&path, &old_reading, &old_surface, replacement)
                    .map_err(display)?;
            println!("saved {} entries to {}", dictionary.len(), path.display());
        }
        Command::DictionaryDelete { reading, surface } => {
            let path = paths::user_dictionary().map_err(display)?;
            let dictionary = user_dictionary::delete(&path, &reading, &surface).map_err(display)?;
            println!("saved {} entries to {}", dictionary.len(), path.display());
        }
        Command::DictionaryImport { path, format, mode } => {
            let bytes = std::fs::read(&path).map_err(display)?;
            let destination = paths::user_dictionary().map_err(display)?;
            let report =
                user_dictionary::import(&destination, &bytes, format, mode).map_err(display)?;
            println!(
                "imported {} {} entries; {} total in {}",
                report.imported,
                report.format.name(),
                report.total,
                destination.display()
            );
        }
        Command::DictionaryExport { path, format } => {
            let source = paths::user_dictionary().map_err(display)?;
            let count = user_dictionary::export(&source, &path, format).map_err(display)?;
            println!("exported {count} entries to {}", path.display());
        }
        Command::LearningShow => {
            let snapshot = learning::view(&paths::learning().map_err(display)?).map_err(display)?;
            print!("{}", snapshot.to_tsv());
            if snapshot.ignored_tail_bytes > 0 {
                eprintln!(
                    "warning: ignored {} unverified trailing bytes",
                    snapshot.ignored_tail_bytes
                );
            }
        }
        Command::LearningExport { path } => {
            let count =
                learning::export(&paths::learning().map_err(display)?, &path).map_err(display)?;
            println!("exported {count} learning records to {}", path.display());
        }
        Command::LearningClear => {
            let route = learning::clear(&paths::learning().map_err(display)?).map_err(display)?;
            match route {
                learning::ClearRoute::LiveEngine => println!("cleared learning through the engine"),
                learning::ClearRoute::Offline { cleared_records } => {
                    println!(
                        "cleared {cleared_records} learning records while the engine was offline"
                    )
                }
            }
        }
        Command::HistoryShow => {
            let snapshot =
                input_history::view(&paths::input_history().map_err(display)?).map_err(display)?;
            print!("{}", snapshot.to_tsv());
            if snapshot.ignored_tail_bytes > 0 {
                eprintln!(
                    "warning: ignored {} unverified trailing bytes",
                    snapshot.ignored_tail_bytes
                );
            }
        }
        Command::HistoryExport { path } => {
            let source = paths::input_history().map_err(display)?;
            let count = input_history::export(&source, &path).map_err(display)?;
            println!(
                "exported {count} input-history records to {}",
                path.display()
            );
        }
        Command::HistoryClear => {
            let route =
                input_history::clear(&paths::input_history().map_err(display)?).map_err(display)?;
            match route {
                input_history::ClearRoute::LiveEngine => {
                    println!("cleared input history through the engine")
                }
                input_history::ClearRoute::Offline { cleared_records } => {
                    println!(
                        "cleared {cleared_records} input-history records while the engine was offline"
                    )
                }
            }
        }
        Command::HistoryStats => {
            let stats =
                input_history::stats(&paths::input_history().map_err(display)?).map_err(display)?;
            println!("live-engine\t{}", if stats.live { "yes" } else { "no" });
            println!(
                "history-service-active\t{}",
                if stats.active { "yes" } else { "no" }
            );
            println!("dropped-events\t{}", stats.dropped_events);
            println!("persistence-failures\t{}", stats.persistence_failures);
            println!(
                "excluded-unclassified-events\t{}",
                stats.excluded_unclassified_events
            );
            println!(
                "excluded-sensitive-events\t{}",
                stats.excluded_sensitive_events
            );
            println!(
                "excluded-test-only-events\t{}",
                stats.excluded_test_only_events
            );
        }
        Command::DiagnosticsShow { tsv } => {
            let data = diagnostics::load(&paths::timeout_diagnostics().map_err(display)?)
                .map_err(display)?;
            if tsv {
                print!("{}", diagnostics::render_tsv(&data));
            } else {
                print!("{}", diagnostics::render_text(&data));
            }
        }
        Command::DiagnosticsClear => {
            let path = paths::timeout_diagnostics().map_err(display)?;
            diagnostics::clear(&path).map_err(display)?;
            println!("cleared IPC timeout diagnostics");
        }
        Command::UpdateStatus => {
            let preferences_path = paths::update_preferences().map_err(display)?;
            let preferences =
                updater::UpdatePreferences::load(&preferences_path).map_err(display)?;
            println!(
                "automatic-updates\t{}",
                if preferences.enabled {
                    "enabled"
                } else {
                    "disabled"
                }
            );
            println!("current-version\t{}", updater::current_version());
            let outcome = updater::check_real(preferences.enabled);
            match outcome {
                updater::UpdateCheckOutcome::Failed(failure) => return Err(failure.to_string()),
                outcome => println!("{}", outcome.describe()),
            }
        }
        Command::UpdateEnable | Command::UpdateDisable => {
            let enabled = matches!(command, Command::UpdateEnable);
            let path = paths::update_preferences().map_err(display)?;
            updater::UpdatePreferences { enabled }
                .save(&path)
                .map_err(display)?;
            println!(
                "automatic updates {} in {}",
                if enabled { "enabled" } else { "disabled" },
                path.display()
            );
        }
        Command::UpdateApply => {
            let preferences =
                updater::UpdatePreferences::load(&paths::update_preferences().map_err(display)?)
                    .map_err(display)?;
            let paths = updater::UpdatePaths {
                installer: paths::update_installer().map_err(display)?,
                log: paths::update_log().map_err(display)?,
            };
            let outcome = updater::apply_real(preferences.enabled, &paths);
            let description = outcome.describe();
            if outcome.is_failure() {
                return Err(description);
            }
            println!("{description}");
        }
    }
    Ok(())
}

fn print_configuration(document: &ConfigurationDocument) {
    println!("format-version\t{}", document.source_version);
    println!("keymap\t{}", document.preferences.keymap_preset.name());
    println!(
        "prediction\t{}",
        switch_name(document.preferences.prediction_enabled)
    );
    println!("suggest\t{}", document.preferences.suggest_accept.name());
    println!(
        "developer-mode\t{}",
        switch_name(document.preferences.developer_mode)
    );
    println!("profiles\t{}", document.profiles.len());
}

fn dictionary_entry(
    reading: &str,
    surface: &str,
    pos: &str,
    comment: &str,
) -> Result<UserDictionaryEntry, String> {
    Ok(UserDictionaryEntry {
        reading: reading.to_owned(),
        surface: surface.to_owned(),
        part_of_speech: UserPartOfSpeech::from_name(pos)
            .ok_or_else(|| format!("unknown Sakura part of speech {pos:?}"))?,
        comment: comment.to_owned(),
    })
}

fn parse_import_format(value: &str) -> Result<Option<formats::DictionaryFormat>, String> {
    if value == "auto" {
        Ok(None)
    } else {
        parse_export_format(value).map(Some)
    }
}

fn parse_export_format(value: &str) -> Result<formats::DictionaryFormat, String> {
    formats::DictionaryFormat::from_name(value)
        .ok_or_else(|| format!("unknown dictionary format {value:?}"))
}

fn parse_switch(value: &str) -> Result<bool, String> {
    match value {
        "on" | "true" | "yes" | "1" => Ok(true),
        "off" | "false" | "no" | "0" => Ok(false),
        _ => Err(format!("expected on or off, got {value:?}")),
    }
}

fn parse_suggest(value: &str) -> Result<SuggestAccept, String> {
    SuggestAccept::from_name(value).ok_or_else(|| format!("unknown suggest binding {value:?}"))
}

fn parse_mode(value: &str) -> Result<Mode, String> {
    match value {
        "direct" => Ok(Mode::Direct),
        "hiragana" => Ok(Mode::Hiragana),
        "katakana" => Ok(Mode::Katakana),
        "half-katakana" => Ok(Mode::HalfKatakana),
        "full-alnum" => Ok(Mode::FullAlnum),
        "half-alnum" => Ok(Mode::HalfAlnum),
        _ => Err(format!("unknown input mode {value:?}")),
    }
}

pub(crate) const fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Direct => "direct",
        Mode::Hiragana => "hiragana",
        Mode::Katakana => "katakana",
        Mode::HalfKatakana => "half-katakana",
        Mode::FullAlnum => "full-alnum",
        Mode::HalfAlnum => "half-alnum",
    }
}

fn switch_name(enabled: bool) -> &'static str {
    if enabled {
        "on"
    } else {
        "off"
    }
}

fn developer_history_terminal(enabled: bool, stats: input_history::HistoryStats) -> &'static str {
    match (enabled, stats.live, stats.active) {
        (true, true, true) => "active",
        (true, true, false) => "restart-required-to-enable",
        (true, false, _) => "will-enable-at-next-engine-start",
        (false, _, true) => "restart-required-to-disable",
        (false, _, false) => "inactive",
    }
}

fn display(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_words(words: &[&str]) -> Result<Command, String> {
        parse(words.iter().map(|word| (*word).to_owned()))
    }

    #[test]
    fn parses_every_mutating_command_without_touching_user_state() {
        assert!(matches!(
            parse_words(&["config", "set", "prediction", "off"]),
            Ok(Command::ConfigSet {
                setting: ConfigSetting::Prediction,
                ..
            })
        ));
        assert!(matches!(
            parse_words(&["config", "set", "developer-mode", "on"]),
            Ok(Command::ConfigSet {
                setting: ConfigSetting::DeveloperMode,
                value,
            }) if value == "on"
        ));
        assert!(matches!(
            parse_words(&[
                "profile",
                "set",
                "code.exe",
                "half-alnum",
                "off",
                "disabled"
            ]),
            Ok(Command::ProfileSet { .. })
        ));
        assert!(matches!(
            parse_words(&["dictionary", "import", "words.txt", "auto", "merge"]),
            Ok(Command::DictionaryImport { format: None, .. })
        ));
        assert!(matches!(
            parse_words(&["learning", "clear"]),
            Ok(Command::LearningClear)
        ));
        assert!(matches!(
            parse_words(&["history", "export", "history.tsv"]),
            Ok(Command::HistoryExport { path }) if path == PathBuf::from("history.tsv")
        ));
        assert!(matches!(
            parse_words(&["history", "stats"]),
            Ok(Command::HistoryStats)
        ));
        assert!(matches!(
            parse_words(&["diagnostics", "clear"]),
            Ok(Command::DiagnosticsClear)
        ));
        assert!(matches!(
            parse_words(&["update", "enable"]),
            Ok(Command::UpdateEnable)
        ));
        assert!(matches!(
            parse_words(&["update", "apply"]),
            Ok(Command::UpdateApply)
        ));
    }

    #[test]
    fn a_relative_file_operand_resolves_where_the_user_ran_the_command() {
        let base = PathBuf::from(r"C:\Users\someone\reports");

        // Without this the operand resolved inside the payload's own folder
        // under Program Files and failed with a bare access-denied error.
        let resolved = resolve_file_operands(
            Command::LearningExport {
                path: PathBuf::from("learning.tsv"),
            },
            Some(&base),
        );
        assert!(matches!(
            resolved,
            Command::LearningExport { path } if path == base.join("learning.tsv")
        ));

        // An absolute operand already names its destination.
        let absolute = PathBuf::from(r"D:\archive\history.tsv");
        let resolved = resolve_file_operands(
            Command::HistoryExport {
                path: absolute.clone(),
            },
            Some(&base),
        );
        assert!(matches!(
            resolved,
            Command::HistoryExport { path } if path == absolute
        ));

        // Running the payload directly leaves the caller's own working
        // directory in charge.
        let resolved = resolve_file_operands(
            Command::HistoryExport {
                path: PathBuf::from("history.tsv"),
            },
            None,
        );
        assert!(matches!(
            resolved,
            Command::HistoryExport { path } if path == PathBuf::from("history.tsv")
        ));

        // Import reads through the same operand and needs the same anchor.
        let resolved = resolve_file_operands(
            Command::DictionaryImport {
                path: PathBuf::from("words.txt"),
                format: None,
                mode: ImportMode::Merge,
            },
            Some(&base),
        );
        assert!(matches!(
            resolved,
            Command::DictionaryImport { path, .. } if path == base.join("words.txt")
        ));
    }

    #[test]
    fn developer_history_terminal_state_never_claims_a_saved_setting_is_already_active() {
        use input_history::HistoryStats;

        let stats = |live, active| HistoryStats {
            active,
            live,
            dropped_events: 0,
            persistence_failures: 0,
            excluded_unclassified_events: 0,
            excluded_sensitive_events: 0,
            excluded_test_only_events: 0,
        };

        assert_eq!(
            developer_history_terminal(true, stats(true, true)),
            "active"
        );
        assert_eq!(
            developer_history_terminal(true, stats(true, false)),
            "restart-required-to-enable"
        );
        assert_eq!(
            developer_history_terminal(true, stats(false, false)),
            "will-enable-at-next-engine-start"
        );
        assert_eq!(
            developer_history_terminal(false, stats(true, true)),
            "restart-required-to-disable"
        );
        assert_eq!(
            developer_history_terminal(false, stats(true, false)),
            "inactive"
        );
    }

    #[test]
    fn rejects_ambiguous_or_lossy_arguments() {
        assert!(parse_words(&["dictionary", "export", "out.txt", "auto"]).is_err());
        assert!(parse_words(&["profile", "set", "x.exe", "magic", "on", "tab"]).is_err());
        assert!(parse_words(&["dictionary", "add", "かな", "仮名", "verb"]).is_err());
        assert!(parse_words(&["learning", "clear", "now"]).is_err());
        assert!(parse_words(&["update", "enable", "now"]).is_err());
    }
}
