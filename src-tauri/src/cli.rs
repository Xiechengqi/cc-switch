//! Lightweight CLI entry points for headless / container environments.
//!
//! `cc-switch version` and `cc-switch help` exit before the Tauri runtime starts,
//! so CI and Docker health checks can read build metadata without launching the GUI.

use crate::build_info;

/// Handle `version` / `help` (and common flag aliases). Returns `true` when the
/// process should exit immediately with status 0.
pub fn try_handle(args: &[String]) -> bool {
    match parse_command(args) {
        CliCommand::None => false,
        CliCommand::Version => {
            print_version();
            true
        }
        CliCommand::Help => {
            print_help();
            true
        }
    }
}

enum CliCommand {
    None,
    Version,
    Help,
}

fn parse_command(args: &[String]) -> CliCommand {
    for arg in args.iter().skip(1) {
        match arg.as_str() {
            "version" | "--version" | "-V" => return CliCommand::Version,
            "help" | "--help" | "-h" => return CliCommand::Help,
            "--no-desktop" => {}
            other if other.starts_with('-') => {}
            _ => return CliCommand::None,
        }
    }
    CliCommand::None
}

pub fn print_version() {
    println!("{}", version_text());
}

pub fn version_text() -> String {
    format!(
        "cc-switch {}\n\
         package: {}\n\
         commit: {}\n\
         built: {}\n\
         channel: {}",
        build_info::DISPLAY_VERSION,
        build_info::PACKAGE_VERSION,
        build_info::BUILD_SHA,
        build_info::BUILD_TIME,
        build_info::BUILD_CHANNEL,
    )
}

pub fn print_help() {
    println!("{}", help_text());
}

pub fn help_text() -> String {
    format!(
        "cc-switch {} — multi-app AI provider switcher and local API proxy\n\
         \n\
         Usage:\n\
           cc-switch [OPTIONS]           Start the desktop application (default)\n\
           cc-switch version             Print build version information\n\
           cc-switch help                Print this help message\n\
         \n\
         Options:\n\
           --no-desktop                  Run without desktop UI (headless / container)\n\
           -h, --help                    Print help (alias for `help`)\n\
           -V, --version                 Print version (alias for `version`)",
        build_info::DISPLAY_VERSION,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(parts: &[&str]) -> Vec<String> {
        std::iter::once("cc-switch")
            .chain(parts.iter().copied())
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn version_subcommand_is_handled() {
        assert!(matches!(
            parse_command(&args(&["version"])),
            CliCommand::Version
        ));
        assert!(matches!(
            parse_command(&args(&["--version"])),
            CliCommand::Version
        ));
        assert!(matches!(parse_command(&args(&["-V"])), CliCommand::Version));
    }

    #[test]
    fn help_subcommand_is_handled() {
        assert!(matches!(parse_command(&args(&["help"])), CliCommand::Help));
        assert!(matches!(
            parse_command(&args(&["--help"])),
            CliCommand::Help
        ));
        assert!(matches!(parse_command(&args(&["-h"])), CliCommand::Help));
    }

    #[test]
    fn no_args_starts_app() {
        assert!(matches!(parse_command(&args(&[])), CliCommand::None));
    }

    #[test]
    fn no_desktop_flag_does_not_block_startup() {
        assert!(matches!(
            parse_command(&args(&["--no-desktop"])),
            CliCommand::None
        ));
    }

    #[test]
    fn version_before_no_desktop_is_handled() {
        assert!(matches!(
            parse_command(&args(&["version", "--no-desktop"])),
            CliCommand::Version
        ));
    }

    #[test]
    fn version_text_includes_build_sha() {
        assert!(version_text().contains(build_info::BUILD_SHA));
        assert!(version_text().contains(build_info::DISPLAY_VERSION));
    }
}
