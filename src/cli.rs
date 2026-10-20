//! Command line surface: the parser, the global flags and the subcommand list.
//!
//! Everything a user can type is declared here; the actual work lives in
//! [`crate::commands`]. Adding a subcommand means adding a variant to
//! [`Command`] and a module under `commands/`.

use std::io::IsTerminal;
use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

/// Long help text shown by `vmanager --help`.
const AFTER_HELP: &str = "\
Getting started:
  vmanager folder add C:\\Tools\\Blender   Watch a tool's version folders
  vmanager list                           See what each folder holds
  vmanager run --dry-run                  See what would be archived
  vmanager run                            Archive all but the newest versions
  vmanager use C:\\Tools\\Blender 4.3      Point `current` at a version
  vmanager shim regenerate                 Rebuild a folder's shim\\ scripts
  vmanager hook set nodejs node setup.ps1  Run a script before `node` launches
  vmanager pin                            Pick tool versions for this directory
  vmanager schedule install               Run it daily at 03:00
  vmanager completions --help             Complete commands as you type them

Configuration (optional; every key has a default):
  Windows           %APPDATA%\\vmanager\\config.toml
  Linux             ~/.config/vmanager/config.toml
  macOS             ~/Library/Application Support/vmanager/config.toml

  keep_versions, compression_level, compression_threads, data_dir, database

Environment:
  VMANAGER_CONFIG   Path to the config file (overridden by --config)
  NO_COLOR          Disable colored output
  RUST_LOG          Fine-grained log filter, e.g. `vmanager=debug`

Exit codes:
  0  success               3  configuration error    6  archive error
  1  failure               4  not found            130  interrupted
  2  invalid arguments     5  database error";

#[derive(Debug, Parser)]
#[command(
    name = "vmanager",
    version,
    about = "vManager command line interface",
    propagate_version = true,
    after_help = AFTER_HELP,
    // Print help instead of a bare usage error when invoked with no arguments.
    arg_required_else_help = true
)]
pub struct Cli {
    #[command(flatten)]
    pub globals: GlobalArgs,

    #[command(subcommand)]
    pub command: Command,
}

/// Flags accepted by every subcommand.
#[derive(Debug, clap::Args)]
pub struct GlobalArgs {
    /// Increase logging verbosity (repeat for more: -v, -vv, -vvv)
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Silence all output except errors
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    pub quiet: bool,

    /// Path to the configuration file
    #[arg(
        short,
        long,
        global = true,
        value_name = "FILE",
        env = "VMANAGER_CONFIG"
    )]
    pub config: Option<PathBuf>,

    /// When to use colored output
    #[arg(long, global = true, value_name = "WHEN", default_value_t = ColorChoice::Auto)]
    pub color: ColorChoice,
}

impl GlobalArgs {
    /// Log level implied by `--verbose` / `--quiet`.
    pub fn log_level(&self) -> tracing::Level {
        if self.quiet {
            return tracing::Level::ERROR;
        }
        match self.verbose {
            0 => tracing::Level::WARN,
            1 => tracing::Level::INFO,
            2 => tracing::Level::DEBUG,
            _ => tracing::Level::TRACE,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum ColorChoice {
    /// Colorize when stderr is a terminal
    Auto,
    /// Always colorize
    Always,
    /// Never colorize
    Never,
}

impl ColorChoice {
    /// Resolve the choice against the environment and the actual stream.
    ///
    /// `NO_COLOR` wins over `auto` but not over an explicit `--color always`,
    /// which matches the convention at <https://no-color.org>.
    pub fn should_colorize(self) -> bool {
        match self {
            ColorChoice::Always => true,
            ColorChoice::Never => false,
            ColorChoice::Auto => {
                std::env::var_os("NO_COLOR").is_none() && std::io::stderr().is_terminal()
            }
        }
    }
}

impl std::fmt::Display for ColorChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            ColorChoice::Auto => "auto",
            ColorChoice::Always => "always",
            ColorChoice::Never => "never",
        };
        f.write_str(name)
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Sweep every registered folder: archive versions beyond the newest few
    Run(crate::commands::run::Args),

    /// Show every version of every registered folder, unpacked and archived
    List(crate::commands::list::Args),

    /// Register the folders that `vmanager run` sweeps
    Folder(crate::commands::folder::Args),

    /// Point a folder's `current` link at one of its versions
    Use(crate::commands::r#use::Args),

    /// Install or remove the scheduled daily run
    Schedule(crate::commands::schedule::Args),

    /// Inspect or rebuild a folder's generated command shims
    Shim(crate::commands::shim::Args),

    /// Configure the pre-hook a shim runs before its command
    Hook(crate::commands::hook::Args),

    /// Pin tool versions for the current directory in `.vmanagerversions`
    Pin(crate::commands::pin::Args),

    /// Print the shell completion script for the given shell
    Completions(crate::commands::completions::Args),
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        // Catches conflicting flags, duplicate short options and bad defaults.
        Cli::command().debug_assert();
    }

    #[test]
    fn verbosity_maps_to_log_levels() {
        let parse = |args: &[&str]| {
            Cli::try_parse_from(args)
                .expect("valid arguments")
                .globals
                .log_level()
        };

        assert_eq!(parse(&["vmanager", "list"]), tracing::Level::WARN);
        assert_eq!(parse(&["vmanager", "-v", "list"]), tracing::Level::INFO);
        assert_eq!(parse(&["vmanager", "-vvv", "list"]), tracing::Level::TRACE);
        assert_eq!(
            parse(&["vmanager", "--quiet", "list"]),
            tracing::Level::ERROR
        );
    }

    #[test]
    fn verbose_and_quiet_conflict() {
        assert!(Cli::try_parse_from(["vmanager", "-v", "-q", "list"]).is_err());
    }

    #[test]
    fn globals_are_accepted_after_the_subcommand() {
        let cli = Cli::try_parse_from(["vmanager", "list", "--color", "never"])
            .expect("globals should propagate to subcommands");
        assert_eq!(cli.globals.color, ColorChoice::Never);
    }

    #[test]
    fn explicit_color_choice_ignores_terminal_detection() {
        assert!(ColorChoice::Always.should_colorize());
        assert!(!ColorChoice::Never.should_colorize());
    }
}
