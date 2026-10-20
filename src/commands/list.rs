//! `vmanager list` — what each registered folder currently holds.
//!
//! One line per version, oldest first, whether it is still unpacked or has
//! already been swept into a `.vmversion`. The listing is built from the filesystem;
//! the database is consulted only for what a `.vmversion` cannot tell you by looking
//! at it — how large the version was before compression, and when it was
//! archived. An archive with no record still appears, just with less detail.

use std::io::Write;
use std::path::PathBuf;

use crate::archive::{self, human_bytes};
use crate::commands::Context;
use crate::db::Database;
use crate::error::Result;
use crate::link;
use crate::version::{self, Version};

#[derive(Debug, clap::Args)]
pub struct Args {
    /// List only this folder; repeatable [default: every registered folder]
    #[arg(long, value_name = "FOLDER")]
    folder: Vec<PathBuf>,

    /// List only versions that have been archived
    #[arg(short, long, conflicts_with = "unpacked")]
    archived: bool,

    /// List only versions that are still unpacked
    #[arg(short, long, conflicts_with = "archived")]
    unpacked: bool,
}

impl Args {
    /// Whether the state filters let this entry through. With neither flag
    /// given, everything is shown.
    fn shows(&self, entry: &Entry) -> bool {
        match entry.detail {
            Detail::Unpacked => !self.archived,
            Detail::Archived { .. } => !self.unpacked,
        }
    }
}

/// One version of one tool, in whichever state it is in.
struct Entry {
    name: String,
    version: Option<Version>,
    detail: Detail,
}

enum Detail {
    /// Still a folder on disk.
    Unpacked,
    /// Compressed to a `.vmversion`. The original size and file count are known only
    /// when the database still has the record.
    Archived {
        bytes: u64,
        original: Option<(u64, u64)>,
    },
}

pub fn run(args: &Args, ctx: &Context) -> Result<()> {
    let db = ctx.open_database()?;
    let default_keep = ctx.config()?.keep_versions()?;

    let folders = if args.folder.is_empty() {
        db.folders()?
            .into_iter()
            .map(|record| (record.path, record.keep))
            .collect()
    } else {
        args.folder
            .iter()
            .map(|folder| {
                let path = archive::canonical_dir(folder)?;
                let keep = db.folder_by_path(&path)?.and_then(|record| record.keep);
                Ok((path, keep))
            })
            .collect::<Result<Vec<_>>>()?
    };

    let mut out = std::io::stdout().lock();

    if folders.is_empty() {
        if !ctx.is_quiet() {
            writeln!(out, "no folders registered yet")?;
            writeln!(out, "register one with `vmanager folder add <FOLDER>`")?;
        }
        return Ok(());
    }

    for (path, keep) in &folders {
        let keep = keep.unwrap_or(default_keep);

        let entries = match entries(&db, path) {
            Ok(entries) => entries,
            // A registered folder can disappear — an unplugged drive, an
            // uninstalled tool — and that should not end the listing.
            Err(err) => {
                writeln!(out, "{}  keep {keep}  unavailable: {err}", path.display())?;
                continue;
            }
        };

        // Counted before filtering: `--archived` narrows which lines are shown,
        // it does not change how many versions the folder actually has.
        let unpacked = entries
            .iter()
            .filter(|entry| matches!(entry.detail, Detail::Unpacked))
            .count();
        writeln!(
            out,
            "{}  keep {keep}, {unpacked} unpacked, {} archived",
            path.display(),
            entries.len() - unpacked
        )?;

        let shown: Vec<&Entry> = entries.iter().filter(|entry| args.shows(entry)).collect();
        if shown.is_empty() {
            writeln!(out, "  (none)")?;
            continue;
        }

        // Read once per folder rather than once per entry: a link that has
        // gone stale (its version since archived) still names a version, and
        // that name only needs comparing against what is actually shown.
        let current = link::current(path)?;

        let width = shown
            .iter()
            .map(|entry| entry.name.chars().count())
            .max()
            .unwrap_or(0);

        for entry in &shown {
            write!(out, "  {:<width$}  ", entry.name)?;
            match &entry.detail {
                Detail::Unpacked => writeln!(
                    out,
                    "unpacked{}",
                    if current.as_deref() == Some(entry.name.as_str()) {
                        "  (current)"
                    } else {
                        ""
                    }
                )?,
                Detail::Archived { bytes, original } => {
                    write!(out, "archived  {:>9}", human_bytes(*bytes))?;
                    match original {
                        Some((source_bytes, file_count)) => writeln!(
                            out,
                            "  from {} in {file_count} file(s)",
                            human_bytes(*source_bytes)
                        )?,
                        None => writeln!(out, "  (not in the database)")?,
                    }
                }
            }
        }
    }

    Ok(())
}

/// Every version in `folder`, unpacked and archived, oldest first.
fn entries(db: &Database, folder: &std::path::Path) -> Result<Vec<Entry>> {
    let mut entries = Vec::new();

    for found in version::list(folder)?.versions {
        entries.push(Entry {
            name: found.name,
            version: found.version,
            detail: Detail::Unpacked,
        });
    }

    for found in version::archived(folder)? {
        let record = db.archive_at(&found.path)?;
        entries.push(Entry {
            name: found.name,
            version: found.version,
            detail: Detail::Archived {
                bytes: found.bytes,
                original: record.map(|record| (record.source_bytes, record.file_count)),
            },
        });
    }

    // Oldest first, matching the order the sweep works through them, so the
    // archives sit above the versions still on disk.
    entries.sort_by(|a, b| a.version.cmp(&b.version).then_with(|| a.name.cmp(&b.name)));
    Ok(entries)
}
