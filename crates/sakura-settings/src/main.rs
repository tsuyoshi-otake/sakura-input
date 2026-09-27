//! Sakura Input's native settings control panel and scriptable administration CLI.

#![cfg(windows)]

use std::path::PathBuf;

mod cli;
mod ui;

const EXIT_FAILED: i32 = 1;
const EXIT_USAGE: i32 = 2;

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if let Some(destination) = ui_destination(&arguments) {
        if let Err(error) = ui::run(destination) {
            ui::show_fatal_error(&format!(
                "Sakura Input settings could not start.\n\n{error}"
            ));
            std::process::exit(EXIT_FAILED);
        }
        return;
    }

    let command = match cli::parse(arguments) {
        Ok(command) => command,
        Err(message) => {
            eprintln!("sakura_settings: {message}\n");
            eprint!("{}", cli::USAGE);
            std::process::exit(EXIT_USAGE);
        }
    };
    // The install-root bootstrap runs this payload from its own directory
    // under Program Files, so a relative file operand has to be re-anchored on
    // the directory the user actually typed the command in.
    let caller_directory =
        std::env::var_os(sakura_settings::CALLER_DIRECTORY_VARIABLE).map(PathBuf::from);
    let command = cli::resolve_file_operands(command, caller_directory.as_deref());
    if let Err(message) = cli::run(command) {
        eprintln!("sakura_settings: {message}");
        std::process::exit(EXIT_FAILED);
    }
}

fn ui_destination(arguments: &[String]) -> Option<ui::Destination> {
    match arguments {
        [] => Some(ui::Destination::Default),
        [argument] if argument == "--pad" => Some(ui::Destination::Pad),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_exact_pad_option_changes_gui_destination() {
        assert_eq!(ui_destination(&[]), Some(ui::Destination::Default));
        assert_eq!(
            ui_destination(&["--pad".into()]),
            Some(ui::Destination::Pad)
        );
        for arguments in [
            vec!["--pad".into(), "extra".into()],
            vec!["--PAD".into()],
            vec!["config".into(), "show".into()],
        ] {
            assert_eq!(ui_destination(&arguments), None);
        }
    }
}
