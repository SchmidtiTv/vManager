//! vManager — library half of the `vmanager` command line application.
//!
//! The binary in `src/main.rs` is deliberately thin: it parses arguments,
//! calls [`run`] and turns the result into an exit code. Keeping the logic in
//! a library crate means integration tests and future front-ends can drive the
//! same code without going through a process boundary.

pub mod archive;
pub mod cli;
pub mod commands;
pub mod config;
pub mod db;
pub mod error;
pub mod logging;
pub mod sweep;
pub mod version;

pub use cli::Cli;
pub use error::{Error, Result};

use cli::Command;
use commands::Context;

/// Execute a parsed command line.
pub fn run(cli: Cli) -> Result<()> {
    let Cli { globals, command } = cli;
    let ctx = Context::new(globals);

    match &command {
        Command::Run(args) => commands::run::run(args, &ctx),
        Command::List(args) => commands::list::run(args, &ctx),
        Command::Folder(args) => commands::folder::run(args, &ctx),
        Command::Schedule(args) => commands::schedule::run(args, &ctx),
        Command::Completions(args) => commands::completions::run(args, &ctx),
    }
}
