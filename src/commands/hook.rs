//! `vmanager hook` — pre-hooks run by a command shim before its real exe.
//!
//! A hook is keyed by `(folder, command)`, not by command name alone: the
//! same command name (`node`) can exist under two different registered
//! folders with two different hooks. Setting or clearing a hook here does
//! not require the shim to exist yet — see `shim.rs`'s [`crate::shim::Repatch`]
//! — but when it *does* already exist, the change is applied to it right
//! away rather than waiting for the next `use`/`run`/`shim regenerate`.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::archive;
use crate::commands::Context;
use crate::error::{Error, Result};
use crate::shim::{self, Repatch};

#[derive(Debug, clap::Args)]
pub struct Args {
    #[command(subcommand)]
    command: HookCommand,
}

#[derive(Debug, clap::Subcommand)]
enum HookCommand {
    /// Configure the script a shim runs before its command
    Set {
        /// A folder shown by `vmanager list`
        #[arg(value_name = "FOLDER")]
        folder: PathBuf,

        /// The shim command name, e.g. `node`
        #[arg(value_name = "COMMAND")]
        command: String,

        /// PowerShell script to run first; a non-zero exit aborts the command
        #[arg(value_name = "SCRIPT")]
        script: PathBuf,
    },

    /// Remove the hook configured for a command
    Remove {
        /// A folder shown by `vmanager list`
        #[arg(value_name = "FOLDER")]
        folder: PathBuf,

        /// The shim command name, e.g. `node`
        #[arg(value_name = "COMMAND")]
        command: String,
    },

    /// Show configured hooks
    List {
        /// Show hooks for only this folder [default: every folder with one]
        #[arg(long, value_name = "FOLDER")]
        folder: Option<PathBuf>,
    },
}

pub fn run(args: &Args, ctx: &Context) -> Result<()> {
    match &args.command {
        HookCommand::Set {
            folder,
            command,
            script,
        } => set(ctx, folder, command, script),
        HookCommand::Remove { folder, command } => remove(ctx, folder, command),
        HookCommand::List { folder } => list(ctx, folder.as_deref()),
    }
}

fn set(ctx: &Context, folder: &Path, command: &str, script: &Path) -> Result<()> {
    let folder = archive::canonical_dir(folder)?;
    let script = canonical_file(script)?;

    let db = ctx.open_database()?;
    db.set_hook(&folder, command, &script)?;

    let repatch = shim::repatch_hook(&folder, command, Some(&script))?;

    if ctx.is_quiet() {
        return Ok(());
    }
    let mut out = std::io::stdout().lock();
    writeln!(out, "set hook for {command} in {}", folder.display())?;
    writeln!(out, "  {}", script.display())?;
    report_repatch(&mut out, &repatch)?;
    Ok(())
}

fn remove(ctx: &Context, folder: &Path, command: &str) -> Result<()> {
    let folder = archive::canonical_dir(folder)?;
    let db = ctx.open_database()?;
    db.remove_hook(&folder, command)?;

    let repatch = shim::repatch_hook(&folder, command, None)?;

    if ctx.is_quiet() {
        return Ok(());
    }
    let mut out = std::io::stdout().lock();
    writeln!(out, "removed hook for {command} in {}", folder.display())?;
    report_repatch(&mut out, &repatch)?;
    Ok(())
}

fn report_repatch(out: &mut impl Write, repatch: &Repatch) -> Result<()> {
    match repatch {
        Repatch::Updated { path } => writeln!(out, "  updated {}", path.display())?,
        Repatch::NotYetCreated => writeln!(
            out,
            "  no shim yet; takes effect once `vmanager use`, `vmanager run`, \
             or `vmanager shim regenerate` creates one"
        )?,
        Repatch::Foreign => writeln!(
            out,
            "  a shim file already exists there but was not written by vmanager; left it alone"
        )?,
    }
    Ok(())
}

fn list(ctx: &Context, folder: Option<&Path>) -> Result<()> {
    let db = ctx.open_database()?;
    let hooks = match folder {
        Some(folder) => db.hooks_for_folder(&archive::canonical_dir(folder)?)?,
        None => db.hooks()?,
    };

    if ctx.is_quiet() {
        return Ok(());
    }

    let mut out = std::io::stdout().lock();
    if hooks.is_empty() {
        writeln!(out, "no hooks configured")?;
        return Ok(());
    }

    for hook in &hooks {
        writeln!(
            out,
            "{}  {}  {}",
            hook.folder_path.display(),
            hook.command,
            hook.script_path.display()
        )?;
    }
    Ok(())
}

/// Resolve `path` and confirm it is a file that exists — a hook that
/// silently points nowhere is a loud problem later, at the worst possible
/// time (mid-launch, inside the shim), rather than now.
fn canonical_file(path: &Path) -> Result<PathBuf> {
    let canonical = path.canonicalize().map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            Error::NotFound {
                kind: "hook script",
                name: path.display().to_string(),
            }
        } else {
            Error::Io(err)
        }
    })?;

    if !canonical.is_file() {
        return Err(Error::Shim(format!("`{}` is not a file", path.display())));
    }
    Ok(canonical)
}
