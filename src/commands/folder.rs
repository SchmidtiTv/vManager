//! `vmanager folder` — the registry of folders that `vmanager run` sweeps.
//!
//! A registered folder is the parent of one tool's versions, not a version
//! itself: register `C:\Tools\Blender`, which holds `4.1`, `4.2` and `4.3`.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::archive;
use crate::commands::Context;
use crate::error::Result;
use crate::shim;
use crate::version;

#[derive(Debug, clap::Args)]
pub struct Args {
    #[command(subcommand)]
    command: FolderCommand,
}

#[derive(Debug, clap::Subcommand)]
enum FolderCommand {
    /// Register a folder whose version subfolders should be managed
    Add {
        /// Folder containing one subfolder per version
        #[arg(value_name = "FOLDER")]
        folder: PathBuf,

        /// Versions to keep unpacked for this folder [default: keep_versions]
        #[arg(short, long, value_parser = clap::value_parser!(u32).range(1..))]
        keep: Option<u32>,
    },

    /// Unregister a folder; nothing on disk is touched
    Remove {
        /// A folder shown by `vmanager list`
        #[arg(value_name = "FOLDER")]
        folder: PathBuf,
    },
}

pub fn run(args: &Args, ctx: &Context) -> Result<()> {
    match &args.command {
        FolderCommand::Add { folder, keep } => add(ctx, folder, *keep),
        FolderCommand::Remove { folder } => remove(ctx, folder),
    }
}

fn add(ctx: &Context, folder: &Path, keep: Option<u32>) -> Result<()> {
    // Canonicalized on the way in: the same folder reached by two spellings
    // must not become two registrations, each sweeping the other's versions.
    let path = archive::canonical_dir(folder)?;
    let db = ctx.open_database()?;
    let (_, inserted) = db.add_folder(&path, keep)?;

    // Created empty, alongside the version folders, so the folder is ready
    // for `vmanager use`/`vmanager run` to populate the moment either runs —
    // and so the path exists for a user to put on `PATH` right away.
    shim::ensure_dir(&path)?;

    if ctx.is_quiet() {
        return Ok(());
    }

    let mut out = std::io::stdout().lock();
    let action = if inserted { "registered" } else { "updated" };
    writeln!(out, "{action} {}", path.display())?;

    match version::list(&path) {
        Ok(listing) => {
            let keep = match keep {
                Some(keep) => keep,
                None => ctx.config()?.keep_versions()?,
            };
            writeln!(
                out,
                "  {} version(s) present, keeping {keep}, ordered by {}",
                listing.versions.len(),
                listing.order
            )?;
        }
        Err(err) => writeln!(out, "  could not read it yet: {err}")?,
    }

    Ok(())
}

fn remove(ctx: &Context, folder: &Path) -> Result<()> {
    let db = ctx.open_database()?;

    // Unregistering must keep working after the folder itself is gone, so fall
    // back to the path as typed when it can no longer be canonicalized.
    let path = archive::canonical_dir(folder).unwrap_or_else(|_| folder.to_path_buf());
    let record = match db.remove_folder(&path) {
        Err(crate::Error::NotFound { .. }) if path != folder => db.remove_folder(folder)?,
        other => other?,
    };

    if !ctx.is_quiet() {
        writeln!(
            std::io::stdout().lock(),
            "unregistered {}",
            record.path.display()
        )?;
    }
    Ok(())
}
