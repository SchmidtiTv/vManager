//! `vmanager completions` — the shell completion script, on stdout.
//!
//! The script is generated from the very same clap definition the binary
//! parses with, so it cannot drift: a new subcommand or flag is completable the
//! moment it is added, without anyone remembering to update a checked-in
//! script.
//!
//! Nothing here reads the config file or the database. That is deliberate —
//! completions have to work on a machine where vManager has not been set up
//! yet, which is exactly when a shell profile first runs this command.

use std::io::Write;

use clap::CommandFactory;
use clap_complete::Shell;

use crate::cli::Cli;
use crate::commands::Context;
use crate::error::{Error, Result};

/// Where each shell wants the script, shown by `vmanager completions --help`.
const AFTER_HELP: &str = "\
Installing:
  PowerShell  vmanager completions powershell >> $PROFILE
  Bash        vmanager completions bash > ~/.local/share/bash-completion/completions/vmanager
  Zsh         vmanager completions zsh > ~/.zfunc/_vmanager     (~/.zfunc on $fpath)
  Fish        vmanager completions fish > ~/.config/fish/completions/vmanager.fish
  Elvish      vmanager completions elvish >> ~/.config/elvish/rc.elv

Start a new shell afterwards; the script is read once, at startup.

Written to stdout, so it can be piped somewhere else or eval'd directly:
  eval \"$(vmanager completions bash)\"

Rerun this after upgrading vmanager — the script describes the commands of the
version that produced it.";

#[derive(Debug, clap::Args)]
#[command(after_help = AFTER_HELP)]
pub struct Args {
    /// Shell to generate for; guessed from the environment when omitted
    #[arg(value_name = "SHELL")]
    shell: Option<Shell>,
}

pub fn run(args: &Args, _ctx: &Context) -> Result<()> {
    let shell = match args.shell {
        Some(shell) => shell,
        // `$SHELL` is not set under PowerShell, and is the *login* shell rather
        // than the current one elsewhere, so a wrong guess is possible — name
        // the shell explicitly and it is never consulted.
        None => Shell::from_env().ok_or_else(|| {
            Error::Other(anyhow::anyhow!(
                "could not tell which shell this is; name it, e.g. \
                 `vmanager completions powershell`"
            ))
        })?,
    };

    let script = generate(shell);

    // Buffered rather than written through clap_complete, which panics on a
    // write error. `vmanager completions bash | head` should end quietly, the
    // way every other command does.
    std::io::stdout().lock().write_all(&script)?;
    Ok(())
}

/// The completion script for `shell`.
fn generate(shell: Shell) -> Vec<u8> {
    let mut command = Cli::command();
    let name = command.get_name().to_string();

    let mut script = Vec::new();
    clap_complete::generate(shell, &mut command, name, &mut script);
    script
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every shell clap_complete knows about, so a new one cannot be added
    /// upstream and silently go untested here.
    fn every_shell() -> Vec<Shell> {
        use clap::ValueEnum;
        Shell::value_variants().to_vec()
    }

    #[test]
    fn every_shell_produces_a_script() {
        for shell in every_shell() {
            let script = String::from_utf8(generate(shell)).expect("scripts are UTF-8");
            assert!(
                script.contains("vmanager"),
                "the {shell} script should name the binary"
            );
        }
    }

    #[test]
    fn the_script_knows_the_subcommands() {
        let script = String::from_utf8(generate(Shell::Bash)).unwrap();
        for subcommand in ["run", "list", "folder", "schedule", "shim", "hook"] {
            assert!(
                script.contains(subcommand),
                "`{subcommand}` should be completable"
            );
        }
    }

    #[test]
    fn the_script_knows_the_global_flags() {
        let script = String::from_utf8(generate(Shell::Zsh)).unwrap();
        assert!(script.contains("--config"));
        assert!(script.contains("--color"));
    }
}
