//! The sweep: keep the newest few versions of a tool unpacked and compress the
//! rest.
//!
//! This is what `vmanager run` does every day. For one registered folder the
//! steps are:
//!
//! 1. list the version folders under it, oldest first ([`crate::version`]),
//! 2. set aside the newest `keep` of them,
//! 3. for every older one: write `<version>.vmversion` next to it, confirm the
//!    archive really contains what was walked, and only then delete the
//!    folder.
//!
//! The order matters: the delete is what frees the disk, and it happens last,
//! after the archive is on disk and has been read back. A failure anywhere
//! before that leaves the version folder untouched.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::archive::{self, Summary};
use crate::error::{Error, Result};
use crate::version::{self, OrderKey, VersionFolder};

/// How a sweep should behave.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// How the `.vmversion` files are written.
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
    /// Versions that would otherwise be in [`Self::victims`], but a
    /// `.vmanagerversions` still pins them. Always empty until
    /// [`Self::protect`] is called — `plan` itself only knows the filesystem.
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

    /// A copy of this plan with every victim named in `pinned` moved into
    /// [`Self::protected`]. Names match exactly, the same string a pin file
    /// stores; a pinned version the newest-few rule already keeps stays kept.
    pub fn protect(self, pinned: &HashSet<String>) -> Plan {
        let (protected, victims) = self
            .victims
            .into_iter()
            .partition(|victim| pinned.contains(&victim.name));
        Plan {
            victims,
            protected,
            ..self
        }
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

/// Where the archive for a version folder goes: `<version>.vmversion`
/// alongside it, inside the registered folder.
pub fn destination(root: &Path, victim: &VersionFolder) -> PathBuf {
    root.join(format!("{}.{}", victim.name, archive::EXTENSION))
}

/// One archive [`rename_legacy_archives`] moved to the current extension.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Renamed {
    pub from: PathBuf,
    pub to: PathBuf,
}

/// Rename every `<name>.7z` directly in `root` — archives written before
/// [`archive::EXTENSION`] existed — to `<name>.vmversion`. Fails, renaming
/// nothing further, when the new name is already taken.
pub fn rename_legacy_archives(root: &Path) -> Result<Vec<Renamed>> {
    let mut renamed = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let from = entry.path();
        if !entry.file_type()?.is_file()
            || from.extension().and_then(|ext| ext.to_str()) != Some(archive::LEGACY_EXTENSION)
        {
            continue;
        }

        let to = from.with_extension(archive::EXTENSION);
        if to.exists() {
            return Err(Error::Archive(format!(
                "cannot rename `{}` to `{}`: that file already exists",
                from.display(),
                to.display()
            )));
        }
        std::fs::rename(&from, &to)?;
        renamed.push(Renamed { from, to });
    }
    renamed.sort_by(|a, b| a.from.cmp(&b.from));
    Ok(renamed)
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

/// Unpack the archived version `name` back into `<root>\<name>` and, once
/// the unpacked file count is confirmed, delete the `.vmversion` — the reverse of
/// [`sweep_one`], with the delete again last. Returns the files unpacked.
pub fn restore(root: &Path, name: &str) -> Result<u64> {
    let archive = root.join(format!("{name}.{}", archive::EXTENSION));
    let folder = root.join(name);
    let files = archive::extract(&archive, &folder)?;

    tracing::info!(path = %archive.display(), "removing unpacked archive");
    std::fs::remove_file(&archive).map_err(|err| {
        Error::Archive(format!(
            "unpacked `{}` to `{}` but could not remove the archive: {err}",
            archive.display(),
            folder.display()
        ))
    })?;
    Ok(files)
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

        let dest = dir.path().join("1.0.vmversion");
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
        sevenz_rust2::decompress_file(dir.path().join("1.0.vmversion"), &out).unwrap();
        assert_eq!(
            std::fs::read_to_string(out.join("1.0").join("tool.exe")).unwrap(),
            "build 1.0"
        );
    }

    #[test]
    fn restore_unpacks_the_version_and_removes_its_archive() {
        let dir = tools(&["1.0", "2.0", "3.0"]);
        let plan = plan(dir.path(), 2).unwrap();
        sweep_one(dir.path(), &plan.victims[0], &Options::default()).unwrap();

        assert_eq!(restore(dir.path(), "1.0").unwrap(), 1);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("1.0").join("tool.exe")).unwrap(),
            "build 1.0"
        );
        assert!(!dir.path().join("1.0.vmversion").exists());
    }

    #[test]
    fn legacy_7z_archives_are_renamed_to_the_current_extension() {
        let dir = tools(&["3.0"]);
        std::fs::write(dir.path().join("1.0.7z"), b"old").unwrap();
        std::fs::write(dir.path().join("2.0.vmversion"), b"new").unwrap();

        let renamed = rename_legacy_archives(dir.path()).unwrap();

        assert_eq!(
            renamed,
            [Renamed {
                from: dir.path().join("1.0.7z"),
                to: dir.path().join("1.0.vmversion"),
            }]
        );
        assert_eq!(
            std::fs::read(dir.path().join("1.0.vmversion")).unwrap(),
            b"old"
        );
        assert!(rename_legacy_archives(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn a_legacy_archive_whose_new_name_is_taken_is_an_error() {
        let dir = tools(&["3.0"]);
        std::fs::write(dir.path().join("1.0.7z"), b"old").unwrap();
        std::fs::write(dir.path().join("1.0.vmversion"), b"new").unwrap();

        assert!(rename_legacy_archives(dir.path()).is_err());
        assert!(dir.path().join("1.0.7z").is_file());
    }

    #[test]
    fn protecting_a_pinned_version_moves_it_out_of_the_victims() {
        let dir = tools(&["1.0", "2.0", "3.0", "4.0"]);
        let pinned = HashSet::from(["1.0".to_string()]);
        let plan = plan(dir.path(), 2).unwrap().protect(&pinned);

        assert_eq!(victim_names(&plan), ["2.0"]);
        assert_eq!(plan.protected[0].name, "1.0");
        assert_eq!(plan.version_count(), 4);
    }

    #[test]
    fn protecting_a_version_that_is_already_kept_changes_nothing() {
        let dir = tools(&["1.0", "2.0", "3.0"]);
        let pinned = HashSet::from(["3.0".to_string()]);
        let plan = plan(dir.path(), 2).unwrap().protect(&pinned);

        assert_eq!(victim_names(&plan), ["1.0"]);
        assert!(plan.protected.is_empty());
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
        assert!(dir.path().join("1.0.vmversion").is_file());
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
        assert!(!dir.path().join("1.0.vmversion").exists());
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
        std::fs::write(dir.path().join("1.0.vmversion"), b"not really an archive").unwrap();

        let outcome = sweep_one(dir.path(), &plan.victims[0], &Options::default()).unwrap();

        let Outcome::Skipped { reason } = outcome else {
            panic!("expected a skip, got {outcome:?}");
        };
        assert!(reason.contains("--force"), "{reason}");
        // The point of skipping: neither the stale archive nor the folder is
        // destroyed while the two disagree.
        assert_eq!(
            std::fs::read(dir.path().join("1.0.vmversion")).unwrap(),
            b"not really an archive"
        );
        assert!(dir.path().join("1.0").is_dir());
    }

    #[test]
    fn force_replaces_an_existing_archive() {
        let dir = tools(&["1.0", "2.0", "3.0"]);
        let plan = plan(dir.path(), 2).unwrap();
        std::fs::write(dir.path().join("1.0.vmversion"), b"stale").unwrap();

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
        let dest = dir.path().join("1.0.vmversion");

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
        let dest = dir.path().join("broken.vmversion");
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
