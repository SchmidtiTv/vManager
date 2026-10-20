//! `vmanager run` — the daily sweep over every registered folder.
//!
//! One folder failing does not stop the others: a disconnected drive should
//! not leave the rest of the tools ungroomed. Failures are collected, reported
//! at the end and turned into a non-zero exit code, which is what makes a
//! failed scheduled run visible in Task Scheduler's history.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::archive::{self, human_bytes};
use crate::commands::Context;
use crate::db::{Database, FolderRecord};
use crate::error::{Error, Result};
use crate::pinfile;
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
    /// Versions that aged out of the newest few but stay unpacked because a
    /// `.vmanagerversions` still pins them.
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

    // Read before any folder is touched: a pin file that cannot be parsed
    // means nobody knows what it protects, so nothing is archived at all.
    let pinned = pinned_versions(&db, args.dry_run)?;
    let no_pins = HashMap::new();

    let mut totals = Totals::default();

    for target in &targets {
        // `--keep` is a deliberate override for this run; without it the
        // folder's own setting wins, and only then the configured default.
        let keep = args
            .keep
            .or_else(|| target.record.as_ref().and_then(|record| record.keep))
            .unwrap_or(default_keep);

        let pins = tool_name(&target.path)
            .and_then(|name| pinned.get(&name))
            .unwrap_or(&no_pins);

        match sweep_folder(ctx, &db, target, keep, &options, pins, &mut totals) {
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

/// Version name to the pin files pinning it, for one tool.
type ToolPins = HashMap<String, Vec<PathBuf>>;

/// Every version a remembered `.vmanagerversions` pins, keyed by tool name.
///
/// A pin file that no longer exists belonged to a project that is gone, so
/// it is forgotten with a warning (except under `--dry-run`, which changes
/// nothing). One that exists but cannot be read is an error.
fn pinned_versions(db: &Database, dry_run: bool) -> Result<HashMap<String, ToolPins>> {
    let mut pinned: HashMap<String, ToolPins> = HashMap::new();
    for path in db.pin_files()? {
        if !path.exists() {
            tracing::warn!(path = %path.display(), "pin file no longer exists; forgetting it");
            if !dry_run {
                db.remove_pin_file(&path)?;
            }
            continue;
        }
        for (tool, version) in pinfile::read(&path)? {
            pinned
                .entry(tool)
                .or_default()
                .entry(version)
                .or_default()
                .push(path.clone());
        }
    }
    Ok(pinned)
}

/// The name a pin file uses for the tool in `folder`: the folder's own name.
pub fn tool_name(folder: &Path) -> Option<String> {
    folder
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

fn sweep_folder(
    ctx: &Context,
    db: &Database,
    target: &Target,
    keep: u32,
    options: &sweep::Options,
    pins: &ToolPins,
    totals: &mut Totals,
) -> Result<()> {
    let pinned: HashSet<String> = pins.keys().cloned().collect();
    let plan = sweep::plan(&target.path, keep)?.protect(&pinned);
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
            let files: Vec<String> = pins[&protected.name]
                .iter()
                .map(|file| file.display().to_string())
                .collect();
            writeln!(
                out,
                "  keeping {} (pinned by {})",
                protected.name,
                files.join(", ")
            )?;
        }
    }
    totals.protected += plan.protected.len() as u64;

    // Not under `--dry-run`, which must leave the disk exactly as it found it.
    if !options.dry_run {
        for renamed in sweep::rename_legacy_archives(&target.path)? {
            // A database problem is a warning, as in `record`: the file on
            // disk already has its new name.
            if let Err(err) = db.rename_archive(&renamed.from, &renamed.to) {
                tracing::warn!(error = %err, archive = %renamed.to.display(), "archive renamed but its record could not be updated");
            }
            if !quiet {
                writeln!(
                    out,
                    "  renamed {} -> {}",
                    renamed
                        .from
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy(),
                    renamed.to.file_name().unwrap_or_default().to_string_lossy()
                )?;
            }
        }
    }

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
