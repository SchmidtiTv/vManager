//! vManager — library half of the `vmanager` command line application.
//!
//! The binaries (`src/main.rs`, and `src/bin/vman.rs` under the shorter name)
//! are deliberately thin: both call [`main`], which parses arguments, calls
//! [`run`] and turns the result into an exit code. Keeping the logic in
//! a library crate means integration tests and future front-ends can drive the
//! same code without going through a process boundary.

pub mod archive;
pub mod cli;
pub mod commands;
pub mod config;
pub mod db;
pub mod error;
pub mod link;
pub mod logging;
pub mod menu;
pub mod pinfile;
pub mod shim;
pub mod sweep;
pub mod version;

pub use cli::Cli;
pub use error::{Error, Result};

use std::process::ExitCode;

use clap::Parser;
use cli::Command;
use commands::Context;

/// The whole process for both binaries, `vmanager` and `vman`: parse, set up
/// logging, run, and turn the result into an exit code.
pub fn main() -> ExitCode {
    // clap prints its own diagnostics and exits with 2 on a usage error.
    let cli = Cli::parse();

    logging::init(&cli.globals);

    match run(cli) {
        Ok(()) => ExitCode::from(error::exit::SUCCESS),
        // `vmanager list | head -1` closes stdout early; that is the
        // pipeline working as intended, not a failure worth reporting.
        Err(Error::Io(err)) if err.kind() == std::io::ErrorKind::BrokenPipe => {
            ExitCode::from(error::exit::SUCCESS)
        }
        Err(err) => err.report(),
    }
}

/// Execute a parsed command line.
pub fn run(cli: Cli) -> Result<()> {
    let Cli { globals, command } = cli;
    let ctx = Context::new(globals);

    match &command {
        Command::Run(args) => commands::run::run(args, &ctx),
        Command::List(args) => commands::list::run(args, &ctx),
        Command::Folder(args) => commands::folder::run(args, &ctx),
        Command::Use(args) => commands::r#use::run(args, &ctx),
        Command::Schedule(args) => commands::schedule::run(args, &ctx),
        Command::Shim(args) => commands::shim::run(args, &ctx),
        Command::Hook(args) => commands::hook::run(args, &ctx),
        Command::Pin(args) => commands::pin::run(args, &ctx),
        Command::Completions(args) => commands::completions::run(args, &ctx),
    }
}
