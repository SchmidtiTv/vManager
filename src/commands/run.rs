//! `vmanager run` — the daily sweep over every registered folder.
//!
//! One folder failing does not stop the others: a disconnected drive should
//! not leave the rest of the tools ungroomed. Failures are collected, reported
//! at the end and turned into a non-zero exit code, which is what makes a
//! failed scheduled run visible in Task Scheduler's history.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;

use crate::archive::{self, human_bytes};
use crate::commands::Context;
use crate::db::{Database, FolderRecord};
use crate::error::{Error, Result};
use crate::pmanager::{self, PinnedVersion};
use crate::shim;
use crate::sweep::{self, Outcome};

#[derive(Debug, clap::Args)]
pub struct Args {
    /// Sweep only this folder; repeatable [default: every registered folder]
    #[arg(long, value_name = "FOLDER")]
    folder: Vec<PathBuf>,

    /// Versions to keep unpacked, overriding the configured and per-folder values
    #[arg(short, long, value_parser = clap::value_parser!(u32).range(1..))]
    keep: Option<u32>,

    /// Report what would be archived without changing anything
    #[arg(short = 'n', long)]
    dry_run: bool,

    /// Write the archives but leave the version folders in place
    #[arg(long)]
    keep_source: bool,

    /// Replace an archive that already exists
    #[arg(short, long)]
    force: bool,

    /// Compression level, 0 (fastest) to 9 (smallest)
    #[arg(short, long, value_parser = clap::value_parser!(u32).range(archive::MIN_LEVEL as i64..=archive::MAX_LEVEL as i64))]
    level: Option<u32>,

    /// Compression threads; more threads are faster and compress slightly worse
    #[arg(short, long, value_parser = clap::value_parser!(u32).range(1..=256))]
    threads: Option<u32>,

    /// Skip files and folders with this name; repeatable
    #[arg(short = 'x', long, value_name = "NAME")]
    exclude: Vec<String>,
}

/// Running totals across every folder in one sweep.
#[derive(Debug, Default)]
struct Totals {
    archived: u64,
    planned: u64,
    skipped: u64,
    /// Versions that had aged out of the newest few but were left unpacked
    /// because pManager still has a project pinned to them.
    protected: u64,
    failed: u64,
    freed: u64,
}

pub fn run(args: &Args, ctx: &Context) -> Result<()> {
    let config = ctx.config()?;
    let db = ctx.open_database()?;

    let options = sweep::Options {
        archive: archive::Options {
            level: args.level.unwrap_or(config.compression_level()?),
            threads: args.threads.unwrap_or(config.compression_threads()?),
            exclude: args.exclude.clone(),
            overwrite: args.force,
        },
        dry_run: args.dry_run,
        keep_source: args.keep_source,
    };

    // Validated before any folder is touched, so a typo in the config file is
    // an error rather than a sweep that used a surprising number.
    let default_keep = config.keep_versions()?;

    let targets = targets(args, &db)?;
    if targets.is_empty() {
        if !ctx.is_quiet() {
            let mut out = std::io::stdout().lock();
            writeln!(out, "no folders registered yet")?;
            writeln!(out, "register one with `vmanager folder add <FOLDER>`")?;
        }
        return Ok(());
    }

    let mut totals = Totals::default();

    // Fetched once for the whole run rather than per folder: it is one
    // subprocess call regardless of how many folders are swept, and pManager
    // itself never changes mid-run.
    let pinned = pmanager::pinned_versions();

    for target in &targets {
        // `--keep` is a deliberate override for this run; without it the
        // folder's own setting wins, and only then the configured default.
        let keep = args
            .keep
            .or_else(|| target.record.as_ref().and_then(|record| record.keep))
            .unwrap_or(default_keep);

        match sweep_folder(ctx, &db, target, keep, &options, &pinned, &mut totals) {
            Ok(()) => {
                // Not under `--dry-run`, which must leave the disk exactly
                // as it found it. Regenerated even when nothing was
                // archived this run: the active version could have been
                // swept out from under `current` by an earlier `--keep`
                // change or a manual archive, and this is the one place
                // that notices and recovers from that on its own.
                if !args.dry_run
                    && let Err(err) = regenerate_shims(ctx, &db, target)
                {
                    tracing::error!(folder = %target.path.display(), error = %err, "shim regeneration failed");
                    eprintln!("error: {}: {err}", target.path.display());
                    totals.failed += 1;
                }
            }
            Err(err) => {
                // Report and carry on: the next folder may be perfectly healthy.
                tracing::error!(folder = %target.path.display(), error = %err, "sweep failed");
                eprintln!("error: {}: {err}", target.path.display());
                totals.failed += 1;
            }
        }
    }

    report(ctx, &totals, args.dry_run)?;

    if totals.failed > 0 {
        return Err(Error::Archive(format!(
            "{} of {} folder(s) could not be swept",
            totals.failed,
            targets.len()
        )));
    }

    Ok(())
}

/// A folder to sweep, plus its registry row when it has one.
struct Target {
    path: PathBuf,
    record: Option<FolderRecord>,
}

/// The folders this invocation should sweep.
///
/// `--folder` may name a folder that was never registered — handy for trying
/// the sweep on something before committing to it — so a missing registry row
/// is not an error, it just means no per-folder `keep` and no back-reference
/// on the archives.
fn targets(args: &Args, db: &Database) -> Result<Vec<Target>> {
    if args.folder.is_empty() {
        return Ok(db
            .folders()?
            .into_iter()
            .map(|record| Target {
                path: record.path.clone(),
                record: Some(record),
            })
            .collect());
    }

    args.folder
        .iter()
        .map(|folder| {
            let path = archive::canonical_dir(folder)?;
            let record = db.folder_by_path(&path)?;
            Ok(Target { path, record })
        })
        .collect()
}

/// Narrow every pin pManager reported down to just this folder's, keyed by
/// version name — the shape [`sweep::Plan::protect`] matches against.
fn pins_for(pinned: &[PinnedVersion], folder: &std::path::Path) -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for pin in pinned.iter().filter(|pin| pin.folder == folder) {
        map.entry(pin.version.clone())
            .or_default()
            .push(pin.project.clone());
    }
    map
}

fn sweep_folder(
    ctx: &Context,
    db: &Database,
    target: &Target,
    keep: u32,
    options: &sweep::Options,
    pinned: &[PinnedVersion],
    totals: &mut Totals,
) -> Result<()> {
    let mut plan = sweep::plan(&target.path, keep)?;
    plan.protect(&pins_for(pinned, &target.path));
    let quiet = ctx.is_quiet();
    let mut out = std::io::stdout().lock();

    if !quiet {
        writeln!(
            out,
            "{}  {} version(s), keeping {keep}",
            plan.root.display(),
            plan.version_count()
        )?;
        for ignored in &plan.ignored {
            writeln!(out, "  ignoring {} (no version number)", ignored.display())?;
        }
        for protected in &plan.protected {
            let projects: Vec<&str> = pinned
                .iter()
                .filter(|pin| pin.folder == target.path && pin.version == protected.name)
                .map(|pin| pin.project.as_str())
                .collect();
            writeln!(
                out,
                "  keeping {} (pinned by pmanager: {})",
                protected.name,
                projects.join(", ")
            )?;
        }
    }
    totals.protected += plan.protected.len() as u64;

    if plan.is_empty() {
        if !quiet {
            writeln!(out, "  nothing to archive")?;
        }
        return Ok(());
    }

    for victim in &plan.victims {
        let outcome = sweep::sweep_one(&plan.root, victim, options)?;
        totals.freed += outcome.freed_bytes();

        match &outcome {
            Outcome::Archived { summary, deleted } => {
                totals.archived += 1;
                record(db, summary, target);

                if !quiet {
                    writeln!(
                        out,
                        "  archived {} -> {}  {} file(s), {} -> {}{}",
                        victim.name,
                        summary
                            .archive
                            .file_name()
                            .unwrap_or(summary.archive.as_os_str())
                            .to_string_lossy(),
                        summary.file_count,
                        human_bytes(summary.source_bytes),
                        human_bytes(summary.archive_bytes),
                        if *deleted {
                            format!(", freed {}", human_bytes(outcome.freed_bytes()))
                        } else {
                            ", folder kept".to_string()
                        },
                    )?;
                }
            }
            Outcome::Planned => {
                totals.planned += 1;
                if !quiet {
                    writeln!(
                        out,
                        "  would archive {} -> {}",
                        victim.name,
                        sweep::destination(&plan.root, victim)
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                    )?;
                }
            }
            Outcome::Skipped { reason } => {
                totals.skipped += 1;
                if !quiet {
                    writeln!(out, "  skipped {}: {reason}", victim.name)?;
                }
            }
        }
    }

    Ok(())
}

/// Rebuild a target's `shim\` against whatever `current` (or, lacking that,
/// the newest unpacked version) now contains.
fn regenerate_shims(ctx: &Context, db: &Database, target: &Target) -> Result<()> {
    let hooks = db
        .hooks_for_folder(&target.path)?
        .into_iter()
        .map(|hook| (hook.command, hook.script_path))
        .collect();

    let reports = shim::write_shims(&target.path, &hooks)?;
    if !ctx.is_quiet() && !reports.is_empty() {
        writeln!(
            std::io::stdout().lock(),
            "  shim: {} command(s)",
            reports.len()
        )?;
    }
    Ok(())
}

/// Save the archive to the database.
///
/// A database problem is a warning rather than a failure: the archive is
/// already on disk and the folder it replaced is already gone, so failing the
/// sweep at this point would help nobody. The files are the source of truth;
/// the database only records them.
fn record(db: &Database, summary: &crate::archive::Summary, target: &Target) {
    let folder_id = target.record.as_ref().map(|record| record.id);
    match db.record_archive(summary, folder_id) {
        Ok(id) => tracing::info!(id, "recorded archive"),
        Err(err) => tracing::warn!(
            error = %err,
            archive = %summary.archive.display(),
            "archive created but could not be recorded in the database"
        ),
    }
}

fn report(ctx: &Context, totals: &Totals, dry_run: bool) -> Result<()> {
    if ctx.is_quiet() {
        return Ok(());
    }

    let mut out = std::io::stdout().lock();
    if dry_run {
        writeln!(
            out,
            "\ndry run: {} version(s) would be archived, {} skipped, {} protected",
            totals.planned, totals.skipped, totals.protected
        )?;
        return Ok(());
    }

    writeln!(
        out,
        "\n{} archived, {} skipped, {} protected, {} failed, {} freed",
        totals.archived,
        totals.skipped,
        totals.protected,
        totals.failed,
        human_bytes(totals.freed)
    )?;
    Ok(())
}
