//! `vmanager shim` — inspect and rebuild the generated command shims under
//! each registered folder's `shim\`.
//!
//! Day to day, shims are kept current automatically: `vmanager use` and
//! `vmanager run` regenerate a folder's shims after they touch it (see
//! `commands/use.rs` and `commands/run.rs`). `shim regenerate` exists for
//! recovery — a `shim\` that was hand-edited into a bad state, or one that
//! needs rebuilding without switching versions or running a sweep.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;

use crate::archive;
use crate::commands::Context;
use crate::db::Database;
use crate::error::Result;
use crate::shim::{self, ShimOutcome};

#[derive(Debug, clap::Args)]
pub struct Args {
    #[command(subcommand)]
    command: ShimCommand,
}

#[derive(Debug, clap::Subcommand)]
enum ShimCommand {
    /// Show every shim, its target and whether a hook is attached
    List {
        /// List only this folder; repeatable [default: every registered folder]
        #[arg(long, value_name = "FOLDER")]
        folder: Vec<PathBuf>,
    },

    /// Re-scan and rewrite a folder's shims
    Regenerate {
        /// Regenerate only this folder; repeatable [default: every registered folder]
        #[arg(long, value_name = "FOLDER")]
        folder: Vec<PathBuf>,
    },
}

pub fn run(args: &Args, ctx: &Context) -> Result<()> {
    match &args.command {
        ShimCommand::List { folder } => list(ctx, folder),
        ShimCommand::Regenerate { folder } => regenerate(ctx, folder),
    }
}

/// The folders this invocation should act on. `--folder` need not be
/// registered — same "try it before committing" allowance as `vmanager run
/// --folder` and `vmanager list --folder`.
fn targets(db: &Database, folder: &[PathBuf]) -> Result<Vec<PathBuf>> {
    if folder.is_empty() {
        return Ok(db
            .folders()?
            .into_iter()
            .map(|record| record.path)
            .collect());
    }
    folder
        .iter()
        .map(|folder| archive::canonical_dir(folder))
        .collect()
}

fn hooks_for(db: &Database, folder: &std::path::Path) -> Result<HashMap<String, PathBuf>> {
    Ok(db
        .hooks_for_folder(folder)?
        .into_iter()
        .map(|hook| (hook.command, hook.script_path))
        .collect())
}

fn list(ctx: &Context, folder: &[PathBuf]) -> Result<()> {
    let db = ctx.open_database()?;
    let targets = targets(&db, folder)?;

    let mut out = std::io::stdout().lock();
    if targets.is_empty() {
        if !ctx.is_quiet() {
            writeln!(out, "no folders registered yet")?;
            writeln!(out, "register one with `vmanager folder add <FOLDER>`")?;
        }
        return Ok(());
    }

    for target in &targets {
        let hooks = hooks_for(&db, target)?;
        let entries = match shim::discover(target) {
            Ok(entries) => entries,
            Err(err) => {
                writeln!(out, "{}  unavailable: {err}", target.display())?;
                continue;
            }
        };

        writeln!(out, "{}  {} shim(s)", target.display(), entries.len())?;
        if entries.is_empty() {
            writeln!(out, "  (none)")?;
            continue;
        }

        let shim_dir = target.join(shim::SHIM_DIR_NAME);
        let width = entries
            .iter()
            .map(|entry| entry.command.chars().count())
            .max()
            .unwrap_or(0);

        for entry in &entries {
            let on_disk = shim_dir.join(format!("{}.ps1", entry.command)).is_file();
            let hook = hooks
                .get(&entry.command)
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "none".to_string());
            writeln!(
                out,
                "  {:<width$}  -> current\\{}{}  hook: {hook}",
                entry.command,
                entry.relative,
                if on_disk { "" } else { "  (not written yet)" },
            )?;
        }
    }
    Ok(())
}

fn regenerate(ctx: &Context, folder: &[PathBuf]) -> Result<()> {
    let db = ctx.open_database()?;
    let targets = targets(&db, folder)?;
    let quiet = ctx.is_quiet();
    let mut out = std::io::stdout().lock();

    if targets.is_empty() {
        if !quiet {
            writeln!(out, "no folders registered yet")?;
            writeln!(out, "register one with `vmanager folder add <FOLDER>`")?;
        }
        return Ok(());
    }

    let mut failed = 0u64;

    for target in &targets {
        let hooks = match hooks_for(&db, target) {
            Ok(hooks) => hooks,
            Err(err) => {
                eprintln!("error: {}: {err}", target.display());
                failed += 1;
                continue;
            }
        };

        match shim::write_shims(target, &hooks) {
            Ok(reports) => {
                if quiet {
                    continue;
                }
                writeln!(out, "{}  {} shim(s)", target.display(), reports.len())?;
                for report in &reports {
                    let outcome = match report.outcome {
                        ShimOutcome::Created => "created",
                        ShimOutcome::Updated => "updated",
                        ShimOutcome::Unchanged => "unchanged",
                        ShimOutcome::Foreign => "left alone (not vmanager-generated)",
                        ShimOutcome::Removed => "removed (command no longer found)",
                    };
                    writeln!(out, "  {}  {outcome}", report.command)?;
                }
            }
            Err(err) => {
                tracing::error!(folder = %target.display(), error = %err, "shim regeneration failed");
                eprintln!("error: {}: {err}", target.display());
                failed += 1;
            }
        }
    }

    if failed > 0 {
        return Err(crate::error::Error::Shim(format!(
            "{failed} of {} folder(s) could not have their shims regenerated",
            targets.len()
        )));
    }
    Ok(())
}
