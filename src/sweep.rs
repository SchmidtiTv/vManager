//! The sweep: keep the newest few versions of a tool unpacked and compress the
//! rest.
//!
//! This is what `vmanager run` does every day. For one registered folder the
//! steps are:
//!
//! 1. list the version folders under it, oldest first ([`crate::version`]),
//! 2. set aside the newest `keep` of them,
//! 3. for every older one: write `<version>.7z` next to it, confirm the
//!    archive really contains what was walked, and only then delete the
//!    folder.
//!
//! The order matters: the delete is what frees the disk, and it happens last,
//! after the archive is on disk and has been read back. A failure anywhere
//! before that leaves the version folder untouched.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::archive::{self, Summary};
use crate::error::{Error, Result};
use crate::version::{self, OrderKey, VersionFolder};

/// How a sweep should behave.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// How the `.7z` files are written.
    pub archive: archive::Options,
    /// Work out what would happen and report it, changing nothing on disk.
    pub dry_run: bool,
    /// Archive the old versions but leave the folders in place.
    ///
    /// Nothing is freed this way — it is an escape hatch for a first run,
    /// when you want to see the archives before trusting the deletion.
    pub keep_source: bool,
}

/// What a sweep of one folder intends to do.
#[derive(Debug, Clone)]
pub struct Plan {
    /// The registered folder.
    pub root: PathBuf,
    /// How many versions stay unpacked.
    pub keep: u32,
    /// What the ordering was based on.
    pub order: OrderKey,
    /// The versions being kept, oldest first.
    pub kept: Vec<VersionFolder>,
    /// The versions to archive, oldest first.
    pub victims: Vec<VersionFolder>,
    /// Child folders that are not versions and will not be touched.
    pub ignored: Vec<PathBuf>,
    /// Versions that would otherwise be in [`Self::victims`], but a project
    /// pManager tracks is still pinned to them. Always empty until
    /// [`Self::protect`] is called — `plan` itself knows nothing about
    /// pManager, only the filesystem.
    pub protected: Vec<VersionFolder>,
}

impl Plan {
    /// Total number of version folders found.
    pub fn version_count(&self) -> usize {
        self.kept.len() + self.victims.len() + self.protected.len()
    }

    /// Whether this folder has nothing to do.
    pub fn is_empty(&self) -> bool {
        self.victims.is_empty()
    }

    /// Move any victim named in `pinned` out of [`Self::victims`] and into
    /// [`Self::protected`], so a version that has aged out of the newest few
    /// is still not archived while pManager still has a project pinned to
    /// it.
    ///
    /// `pinned` maps a version *name* — matched exactly, the same string
    /// `vmanager use` would take as `<VERSION>` — to the project name(s)
    /// pinning it, kept only so the caller can explain the skip. Matching is
    /// by name alone: the caller has already narrowed `pinned` down to pins
    /// for this plan's own folder (see `commands::run::sweep_folder`), so
    /// nothing here needs to know a folder path at all.
    pub fn protect(&mut self, pinned: &HashMap<String, Vec<String>>) {
        if pinned.is_empty() {
            return;
        }

        let candidates = std::mem::take(&mut self.victims);
        let (protected, victims) = candidates
            .into_iter()
            .partition(|victim| pinned.contains_key(&victim.name));
        self.protected = protected;
        self.victims = victims;
    }
}

/// What happened to one version folder.
#[derive(Debug)]
pub enum Outcome {
    /// The version was compressed. `deleted` is false under `--keep-source`.
    Archived { summary: Summary, deleted: bool },
    /// A dry run: this version would have been archived.
    Planned,
    /// Nothing was done, for the stated reason. Not a failure.
    Skipped { reason: String },
}

impl Outcome {
    /// Bytes the sweep gave back to the disk: the folder's contents, less the
    /// archive that replaced them. Zero unless the folder was actually
    /// deleted, since an archive alongside its source frees nothing.
    pub fn freed_bytes(&self) -> u64 {
        match self {
            Outcome::Archived {
                summary,
                deleted: true,
            } => summary.source_bytes.saturating_sub(summary.archive_bytes),
            _ => 0,
        }
    }
}

/// Decide what to sweep in `root`, keeping the newest `keep` versions.
pub fn plan(root: &Path, keep: u32) -> Result<Plan> {
    let listing = version::list(root)?;
    let keep_count = (keep.max(1) as usize).min(listing.versions.len());
    let split = listing.versions.len() - keep_count;

    let mut versions = listing.versions;
    let kept = versions.split_off(split);

    Ok(Plan {
        root: root.to_path_buf(),
        keep,
        order: listing.order,
        kept,
        victims: versions,
        ignored: listing.ignored,
        protected: Vec::new(),
    })
}

/// Where the archive for a version folder goes: `<version>.7z` alongside it,
/// inside the registered folder.
pub fn destination(root: &Path, victim: &VersionFolder) -> PathBuf {
    root.join(format!("{}.{}", victim.name, archive::EXTENSION))
}

/// Archive one version folder and, unless told otherwise, delete it.
///
/// Returns [`Outcome::Skipped`] rather than an error when an archive of that
/// name already exists: a sweep that runs daily will meet its own past output,
/// and that is not a failure worth a non-zero exit code. `--force` overwrites
/// instead.
pub fn sweep_one(root: &Path, victim: &VersionFolder, options: &Options) -> Result<Outcome> {
    let dest = destination(root, victim);

    if dest.exists() && !options.archive.overwrite {
        return Ok(Outcome::Skipped {
            reason: format!(
                "`{}` already exists; pass --force to replace it",
                dest.display()
            ),
        });
    }

    if options.dry_run {
        return Ok(Outcome::Planned);
    }

    let summary = archive::create(&victim.path, &dest, &options.archive)?;
    verify(&summary)?;

    if options.keep_source {
        return Ok(Outcome::Archived {
            summary,
            deleted: false,
        });
    }

    tracing::info!(path = %victim.path.display(), "removing archived version");
    std::fs::remove_dir_all(&victim.path).map_err(|err| {
        Error::Archive(format!(
            "archived `{}` to `{}` but could not remove the folder: {err}",
            victim.path.display(),
            dest.display()
        ))
    })?;

    Ok(Outcome::Archived {
        summary,
        deleted: true,
    })
}

/// Read the finished archive back and confirm it holds what was walked.
///
/// Only the 7z header is parsed — entry names and sizes live there, so this
/// costs a seek and a small read rather than a full decompression. It catches
/// a truncated or otherwise unusable archive *before* the source folder is
/// deleted, which is the only moment the check is worth anything.
pub fn verify(summary: &Summary) -> Result<()> {
    let archive = sevenz_rust2::Archive::open(&summary.archive).map_err(|err| {
        Error::Archive(format!(
            "`{}` could not be read back after writing ({err}); the source folder was left alone",
            summary.archive.display()
        ))
    })?;

    let files = archive
        .files
        .iter()
        .filter(|entry| !entry.is_directory)
        .count() as u64;

    if files != summary.file_count {
        return Err(Error::Archive(format!(
            "`{}` holds {files} files but `{}` had {}; the source folder was left alone",
            summary.archive.display(),
            summary.source.display(),
            summary.file_count
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A registered folder holding one version subfolder per name given.
    fn tools(versions: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for version in versions {
            let path = dir.path().join(version);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("tool.exe"), format!("build {version}")).unwrap();
        }
        dir
    }

    fn victim_names(plan: &Plan) -> Vec<&str> {
        plan.victims
            .iter()
            .map(|entry| entry.name.as_str())
            .collect()
    }

    #[test]
    fn keeps_the_newest_and_targets_the_rest() {
        let dir = tools(&["1.0", "2.0", "3.0", "4.0"]);
        let plan = plan(dir.path(), 2).unwrap();

        assert_eq!(victim_names(&plan), ["1.0", "2.0"]);
        assert_eq!(
            plan.kept
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["3.0", "4.0"]
        );
        assert_eq!(plan.version_count(), 4);
    }

    #[test]
    fn nothing_to_do_at_or_below_the_limit() {
        for versions in [&["1.0", "2.0"][..], &["1.0"][..], &[][..]] {
            let dir = tools(versions);
            let plan = plan(dir.path(), 2).unwrap();
            assert!(plan.is_empty(), "should be a no-op for {versions:?}");
        }
    }

    #[test]
    fn protecting_a_pinned_version_moves_it_out_of_the_victims() {
        let dir = tools(&["1.0", "2.0", "3.0", "4.0"]);
        let mut plan = plan(dir.path(), 2).unwrap();
        assert_eq!(victim_names(&plan), ["1.0", "2.0"]);

        let mut pinned = HashMap::new();
        pinned.insert("1.0".to_string(), vec!["legacy-tool".to_string()]);
        plan.protect(&pinned);

        assert_eq!(victim_names(&plan), ["2.0"]);
        assert_eq!(
            plan.protected
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["1.0"]
        );
        // Still counted, just no longer a victim.
        assert_eq!(plan.version_count(), 4);
    }

    #[test]
    fn protecting_with_nothing_pinned_changes_nothing() {
        let dir = tools(&["1.0", "2.0", "3.0"]);
        let mut plan = plan(dir.path(), 2).unwrap();
        let before: Vec<String> = victim_names(&plan).into_iter().map(String::from).collect();

        plan.protect(&HashMap::new());

        assert_eq!(victim_names(&plan), before);
        assert!(plan.protected.is_empty());
    }

    #[test]
    fn protecting_a_version_that_is_already_kept_does_nothing() {
        // Pinned to the newest version, which was never going to be archived
        // anyway — protect() only ever moves things out of victims.
        let dir = tools(&["1.0", "2.0", "3.0"]);
        let mut plan = plan(dir.path(), 2).unwrap();

        let mut pinned = HashMap::new();
        pinned.insert("3.0".to_string(), vec!["api-server".to_string()]);
        plan.protect(&pinned);

        assert_eq!(victim_names(&plan), ["1.0"]);
        assert!(plan.protected.is_empty());
    }

    #[test]
    fn a_keep_of_zero_still_keeps_one() {
        // Sweeping every version away would leave the tool unusable, so the
        // floor is one — the config and the CLI both refuse 0 as well.
        let dir = tools(&["1.0", "2.0"]);
        let plan = plan(dir.path(), 0).unwrap();
        assert_eq!(victim_names(&plan), ["1.0"]);
    }

    #[test]
    fn archives_the_folder_and_deletes_it() {
        let dir = tools(&["1.0", "2.0", "3.0"]);
        let plan = plan(dir.path(), 2).unwrap();
        let victim = &plan.victims[0];

        let outcome = sweep_one(dir.path(), victim, &Options::default()).unwrap();

        let Outcome::Archived { summary, deleted } = outcome else {
            panic!("expected the version to be archived, got {outcome:?}");
        };
        assert!(deleted);
        assert_eq!(summary.file_count, 1);

        let dest = dir.path().join("1.0.7z");
        assert!(dest.is_file(), "the archive should be next to the versions");
        assert!(
            !dir.path().join("1.0").exists(),
            "the folder should be gone"
        );
        assert!(dir.path().join("2.0").exists(), "2.0 must be untouched");
        assert!(dir.path().join("3.0").exists(), "3.0 must be untouched");
    }

    #[test]
    fn the_archive_restores_to_the_original_folder() {
        let dir = tools(&["1.0", "2.0", "3.0"]);
        let plan = plan(dir.path(), 2).unwrap();
        sweep_one(dir.path(), &plan.victims[0], &Options::default()).unwrap();

        let out = dir.path().join("restored");
        sevenz_rust2::decompress_file(dir.path().join("1.0.7z"), &out).unwrap();
        assert_eq!(
            std::fs::read_to_string(out.join("1.0").join("tool.exe")).unwrap(),
            "build 1.0"
        );
    }

    #[test]
    fn keep_source_archives_without_deleting() {
        let dir = tools(&["1.0", "2.0", "3.0"]);
        let plan = plan(dir.path(), 2).unwrap();
        let options = Options {
            keep_source: true,
            ..Options::default()
        };

        let outcome = sweep_one(dir.path(), &plan.victims[0], &options).unwrap();

        assert!(matches!(outcome, Outcome::Archived { deleted: false, .. }));
        assert!(dir.path().join("1.0.7z").is_file());
        assert!(dir.path().join("1.0").is_dir(), "nothing should be deleted");
    }

    #[test]
    fn a_dry_run_touches_nothing() {
        let dir = tools(&["1.0", "2.0", "3.0"]);
        let plan = plan(dir.path(), 2).unwrap();
        let options = Options {
            dry_run: true,
            ..Options::default()
        };

        let outcome = sweep_one(dir.path(), &plan.victims[0], &options).unwrap();

        assert!(matches!(outcome, Outcome::Planned));
        assert_eq!(outcome.freed_bytes(), 0);
        assert!(!dir.path().join("1.0.7z").exists());
        assert!(dir.path().join("1.0").is_dir());
    }

    #[test]
    fn only_a_deleted_folder_counts_as_freed_space() {
        let kept_dir = tools(&["1.0", "2.0", "3.0"]);
        let kept_plan = plan(kept_dir.path(), 2).unwrap();
        let kept = Options {
            keep_source: true,
            ..Options::default()
        };

        // The archive exists either way; only removing the folder frees disk.
        assert_eq!(
            sweep_one(kept_dir.path(), &kept_plan.victims[0], &kept)
                .unwrap()
                .freed_bytes(),
            0
        );

        let dir = tools(&["1.0", "2.0", "3.0"]);
        let deleted_plan = plan(dir.path(), 2).unwrap();
        let outcome = sweep_one(dir.path(), &deleted_plan.victims[0], &Options::default()).unwrap();
        let Outcome::Archived { ref summary, .. } = outcome else {
            panic!("expected an archive, got {outcome:?}");
        };
        assert_eq!(
            outcome.freed_bytes(),
            summary.source_bytes.saturating_sub(summary.archive_bytes)
        );
    }

    #[test]
    fn an_existing_archive_is_skipped_not_overwritten() {
        let dir = tools(&["1.0", "2.0", "3.0"]);
        let plan = plan(dir.path(), 2).unwrap();
        std::fs::write(dir.path().join("1.0.7z"), b"not really an archive").unwrap();

        let outcome = sweep_one(dir.path(), &plan.victims[0], &Options::default()).unwrap();

        let Outcome::Skipped { reason } = outcome else {
            panic!("expected a skip, got {outcome:?}");
        };
        assert!(reason.contains("--force"), "{reason}");
        // The point of skipping: neither the stale archive nor the folder is
        // destroyed while the two disagree.
        assert_eq!(
            std::fs::read(dir.path().join("1.0.7z")).unwrap(),
            b"not really an archive"
        );
        assert!(dir.path().join("1.0").is_dir());
    }

    #[test]
    fn force_replaces_an_existing_archive() {
        let dir = tools(&["1.0", "2.0", "3.0"]);
        let plan = plan(dir.path(), 2).unwrap();
        std::fs::write(dir.path().join("1.0.7z"), b"stale").unwrap();

        let options = Options {
            archive: archive::Options {
                overwrite: true,
                ..archive::Options::default()
            },
            ..Options::default()
        };
        let outcome = sweep_one(dir.path(), &plan.victims[0], &options).unwrap();

        assert!(matches!(outcome, Outcome::Archived { deleted: true, .. }));
        assert!(!dir.path().join("1.0").exists());
    }

    #[test]
    fn an_empty_version_folder_fails_without_deleting_it() {
        let dir = tools(&["2.0", "3.0"]);
        let empty = dir.path().join("1.0");
        std::fs::create_dir(&empty).unwrap();

        let plan = plan(dir.path(), 2).unwrap();
        assert_eq!(victim_names(&plan), ["1.0"]);

        // `archive::create` refuses an empty folder; the folder must survive.
        assert!(sweep_one(dir.path(), &plan.victims[0], &Options::default()).is_err());
        assert!(empty.is_dir());
    }

    #[test]
    fn verification_rejects_a_mismatched_archive() {
        let dir = tools(&["1.0", "2.0", "3.0"]);
        let plan = plan(dir.path(), 2).unwrap();
        let dest = dir.path().join("1.0.7z");

        let mut summary =
            archive::create(&plan.victims[0].path, &dest, &archive::Options::default()).unwrap();
        verify(&summary).expect("a freshly written archive should verify");

        // Claim more files than the archive holds: this is the shape of a
        // truncated write, and it must not be reported as a success.
        summary.file_count += 1;
        let err = verify(&summary).unwrap_err();
        assert!(err.to_string().contains("left alone"), "{err}");
    }

    #[test]
    fn verification_rejects_an_unreadable_archive() {
        let dir = tools(&["1.0"]);
        let dest = dir.path().join("broken.7z");
        std::fs::write(&dest, b"garbage").unwrap();

        let summary = Summary {
            source: dir.path().join("1.0"),
            archive: dest,
            file_count: 1,
            source_bytes: 9,
            archive_bytes: 7,
        };
        assert!(verify(&summary).is_err());
    }

    #[test]
    fn non_version_folders_are_never_swept() {
        let dir = tools(&["1.0", "2.0", "3.0"]);
        std::fs::create_dir(dir.path().join("current")).unwrap();

        let plan = plan(dir.path(), 2).unwrap();

        assert_eq!(victim_names(&plan), ["1.0"]);
        assert_eq!(plan.ignored, [dir.path().join("current")]);
    }
}
