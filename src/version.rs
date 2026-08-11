//! Finding the version folders under a registered folder and ordering them
//! oldest to newest.
//!
//! A registered folder is the parent of one tool's versions — `Tools\Blender`
//! containing `4.1`, `4.2`, `4.3`. Each direct child *directory* is a
//! candidate version; files (including the `.7z` archives vManager itself
//! writes) and symlinks are not.
//!
//! Ordering is by the numbers in the folder name: every run of digits becomes
//! one component, so `4.2` sorts before `4.10` the way a human reads it and
//! not the way a string comparison would. Names without any digits are not
//! versions and are left alone — that keeps a stray `current` or `.cache`
//! folder from being archived. The one exception is a folder where *nothing*
//! is numbered: there the whole listing falls back to modification time, so
//! folders that version by date-of-copy still work.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::archive::EXTENSION;
use crate::error::{Error, Result};
use crate::link;

/// Name of the per-folder directory [`crate::shim`] writes generated command
/// shims into. Defined here rather than in `shim.rs` so [`RESERVED_NAMES`]
/// can use it without `version.rs` depending on `shim.rs` (which itself
/// depends on `version.rs` for the newest-unpacked-version fallback — a
/// dependency the other way round would be a cycle).
pub const SHIM_DIR_NAME: &str = "shim";

/// Directory names that are never treated as a version, no matter which
/// branch [`sort`] takes.
///
/// `current` is usually skipped earlier as a symlink or junction (see the
/// "skipping symlink" case in [`list`]), which already covers the common
/// case in both the numbered and unnumbered branches. But a `current` that
/// is a plain directory — left over from an older vManager, or created by
/// hand — and `shim`, which is always a plain directory, only get the
/// no-digits-means-ignored treatment in the *numbered* branch of [`sort`];
/// a folder that versions entirely by date has no numbered branch to fall
/// into, and would otherwise treat either name as just another version to
/// sweep. This set closes that gap for both branches.
pub const RESERVED_NAMES: &[&str] = &[link::LINK_NAME, SHIM_DIR_NAME];

/// A version number extracted from a folder name.
///
/// Comparison is component-wise and numeric: `[4, 2] < [4, 10]`. When one is a
/// prefix of the other the shorter one is older, so `4.1 < 4.1.1` — which is
/// what `Vec<u64>` already does.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(Vec<u64>);

impl Version {
    /// Pull the version out of a folder name, or `None` when it holds no
    /// digits at all.
    ///
    /// Everything that is not a digit is a separator, so `v4.2`, `4_2`,
    /// `Blender-4.2` and `2026-08-04` all parse. A run of digits too long for
    /// a `u64` saturates rather than failing: such a name is not a real
    /// version number, and saturating keeps ordering total.
    pub fn parse(name: &str) -> Option<Self> {
        let mut parts = Vec::new();
        let mut current: Option<u64> = None;

        for ch in name.chars() {
            match ch.to_digit(10) {
                Some(digit) => {
                    let value = current.unwrap_or(0);
                    current = Some(value.saturating_mul(10).saturating_add(u64::from(digit)));
                }
                None => {
                    if let Some(value) = current.take() {
                        parts.push(value);
                    }
                }
            }
        }
        if let Some(value) = current {
            parts.push(value);
        }

        (!parts.is_empty()).then_some(Self(parts))
    }

    /// The numeric components, most significant first.
    pub fn parts(&self) -> &[u64] {
        &self.0
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut parts = self.0.iter();
        if let Some(first) = parts.next() {
            write!(f, "{first}")?;
        }
        for part in parts {
            write!(f, ".{part}")?;
        }
        Ok(())
    }
}

/// One candidate version: a direct child directory of a registered folder.
#[derive(Debug, Clone)]
pub struct VersionFolder {
    /// Full path to the folder.
    pub path: PathBuf,
    /// The folder's own name, as shown in output.
    pub name: String,
    /// The version parsed from [`Self::name`], when it has one.
    pub version: Option<Version>,
    /// Last modification time, used to order unnumbered folders.
    pub modified: SystemTime,
}

/// Which key [`list`] ended up sorting by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderKey {
    /// Folder names carried version numbers.
    Version,
    /// No folder was numbered, so modification time decided the order.
    Modified,
}

impl std::fmt::Display for OrderKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            OrderKey::Version => "version number",
            OrderKey::Modified => "modification time",
        })
    }
}

/// What a registered folder contains.
#[derive(Debug, Clone)]
pub struct Listing {
    /// Version folders, **oldest first** — so the archiving candidates are at
    /// the front and the ones to keep are at the back.
    pub versions: Vec<VersionFolder>,
    /// Child directories that hold no version number and were therefore not
    /// treated as versions. Never archived.
    pub ignored: Vec<PathBuf>,
    /// The key [`Self::versions`] is sorted by.
    pub order: OrderKey,
}

/// List the version folders directly under `root`, oldest first.
///
/// Errors only if `root` cannot be read; an individual entry that cannot be
/// inspected is skipped with a warning rather than failing the whole sweep.
pub fn list(root: &Path) -> Result<Listing> {
    if !root.is_dir() {
        return Err(Error::NotFound {
            kind: "folder",
            name: root.display().to_string(),
        });
    }

    let mut candidates = Vec::new();

    for entry in std::fs::read_dir(root)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                tracing::warn!(root = %root.display(), error = %err, "skipping unreadable entry");
                continue;
            }
        };

        // `file_type` does not follow links, so a junction or symlink pointing
        // at a version folder is skipped instead of being archived and then
        // deleted out from under whatever it points at.
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(err) => {
                tracing::warn!(path = %entry.path().display(), error = %err, "skipping entry");
                continue;
            }
        };

        if file_type.is_symlink() {
            tracing::debug!(path = %entry.path().display(), "skipping symlink");
            continue;
        }
        if !file_type.is_dir() {
            continue;
        }

        let name = entry.file_name().to_string_lossy().into_owned();
        let modified = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);

        candidates.push(VersionFolder {
            version: Version::parse(&name),
            path: entry.path(),
            name,
            modified,
        });
    }

    Ok(sort(candidates))
}

/// One archived version: a `<name>.7z` sitting where its folder used to be.
#[derive(Debug, Clone)]
pub struct ArchivedVersion {
    /// Full path to the archive.
    pub path: PathBuf,
    /// The version's name, with the `.7z` extension stripped back off — so an
    /// archived `4.1` reads as `4.1`, the same as when it was a folder.
    pub name: String,
    /// The version parsed from [`Self::name`], when it has one.
    pub version: Option<Version>,
    /// Size of the archive on disk.
    pub bytes: u64,
}

/// List the archives directly under `root`, oldest first.
///
/// These are the versions that have already been swept. Read from the
/// filesystem rather than the database, so an archive still shows up after the
/// database is deleted — the files are what actually exist.
pub fn archived(root: &Path) -> Result<Vec<ArchivedVersion>> {
    if !root.is_dir() {
        return Err(Error::NotFound {
            kind: "folder",
            name: root.display().to_string(),
        });
    }

    let mut archives = Vec::new();

    for entry in std::fs::read_dir(root)? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();

        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some(EXTENSION) {
            continue;
        }

        let name = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();

        archives.push(ArchivedVersion {
            version: Version::parse(&name),
            bytes: entry.metadata().map(|meta| meta.len()).unwrap_or(0),
            path,
            name,
        });
    }

    archives.sort_by(|a, b| a.version.cmp(&b.version).then_with(|| a.name.cmp(&b.name)));
    Ok(archives)
}

/// Split the candidates into versions and ignored folders, oldest first.
///
/// Split out from [`list`] so the ordering rules can be tested without
/// touching the filesystem.
fn sort(candidates: Vec<VersionFolder>) -> Listing {
    // Pulled out before the numbered/unnumbered decision below, so a
    // reserved name is excluded either way — see [`RESERVED_NAMES`].
    let (candidates, reserved): (Vec<_>, Vec<_>) = candidates
        .into_iter()
        .partition(|entry| !RESERVED_NAMES.contains(&entry.name.as_str()));

    let numbered = candidates.iter().any(|entry| entry.version.is_some());

    if !numbered {
        // Nothing is numbered: this folder versions by date, so every
        // remaining child counts and modification time decides.
        let mut candidates = candidates;
        candidates.sort_by(|a, b| {
            a.modified
                .cmp(&b.modified)
                .then_with(|| a.name.cmp(&b.name))
        });
        return Listing {
            versions: candidates,
            ignored: reserved.into_iter().map(|entry| entry.path).collect(),
            order: OrderKey::Modified,
        };
    }

    let (mut versions, ignored): (Vec<_>, Vec<_>) = candidates
        .into_iter()
        .partition(|entry| entry.version.is_some());

    versions.sort_by(compare);

    let mut ignored: Vec<PathBuf> = ignored.into_iter().map(|entry| entry.path).collect();
    ignored.extend(reserved.into_iter().map(|entry| entry.path));
    ignored.sort();

    Listing {
        versions,
        ignored,
        order: OrderKey::Version,
    }
}

/// Order two numbered folders oldest first, breaking version ties by name so
/// the result is deterministic (`4.1` and `v4.1` both parse to `[4, 1]`).
fn compare(a: &VersionFolder, b: &VersionFolder) -> Ordering {
    match (&a.version, &b.version) {
        (Some(left), Some(right)) => left.cmp(right).then_with(|| a.name.cmp(&b.name)),
        // `sort` only ever compares entries it has already confirmed are
        // numbered, so the remaining arms are unreachable in practice; falling
        // back to the name keeps the ordering total regardless.
        _ => a.name.cmp(&b.name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str, modified_secs: u64) -> VersionFolder {
        VersionFolder {
            path: PathBuf::from("/tools").join(name),
            name: name.to_string(),
            version: Version::parse(name),
            modified: SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(modified_secs),
        }
    }

    fn names(listing: &Listing) -> Vec<&str> {
        listing
            .versions
            .iter()
            .map(|entry| entry.name.as_str())
            .collect()
    }

    #[test]
    fn parses_the_digit_runs_in_a_name() {
        assert_eq!(Version::parse("4.2").unwrap().parts(), &[4, 2]);
        assert_eq!(Version::parse("v4.2.1").unwrap().parts(), &[4, 2, 1]);
        assert_eq!(Version::parse("Blender-4.2").unwrap().parts(), &[4, 2]);
        assert_eq!(Version::parse("2026-08-04").unwrap().parts(), &[2026, 8, 4]);
        assert_eq!(Version::parse("1_0_0-beta").unwrap().parts(), &[1, 0, 0]);
    }

    #[test]
    fn a_name_without_digits_has_no_version() {
        assert!(Version::parse("current").is_none());
        assert!(Version::parse("").is_none());
        assert!(Version::parse(".cache").is_none());
    }

    #[test]
    fn versions_compare_numerically_not_lexically() {
        // The whole point of parsing: "4.10" is newer than "4.9", but sorts
        // before it as a string.
        assert!(Version::parse("4.9") < Version::parse("4.10"));
        assert!(Version::parse("1.2") < Version::parse("1.2.1"));
        assert!(Version::parse("2.0") > Version::parse("1.99.99"));
    }

    #[test]
    fn three_component_versions_order_on_the_patch_number() {
        // Nothing is special about three components — every digit run in the
        // name becomes one, so patch-level releases order the same way major
        // ones do, including across a two-digit minor.
        let listing = sort(vec![
            folder("4.10.4", 100),
            folder("4.9.12", 200),
            folder("4.10.3", 300),
            folder("4.10", 400),
        ]);

        assert_eq!(names(&listing), ["4.9.12", "4.10", "4.10.3", "4.10.4"]);
    }

    #[test]
    fn a_version_number_too_large_for_u64_saturates() {
        let huge = "9".repeat(40);
        assert_eq!(Version::parse(&huge).unwrap().parts(), &[u64::MAX]);
    }

    #[test]
    fn versions_render_dotted() {
        assert_eq!(Version::parse("v4.2.1").unwrap().to_string(), "4.2.1");
    }

    #[test]
    fn numbered_folders_sort_oldest_first() {
        let listing = sort(vec![
            folder("4.10", 300),
            folder("4.2", 100),
            folder("4.9", 200),
        ]);

        assert_eq!(listing.order, OrderKey::Version);
        assert_eq!(names(&listing), ["4.2", "4.9", "4.10"]);
    }

    #[test]
    fn version_order_ignores_modification_time() {
        // Touching an old folder must not make it look new — this is why the
        // name is the primary key.
        let listing = sort(vec![folder("1.0", 9_999), folder("2.0", 1)]);
        assert_eq!(names(&listing), ["1.0", "2.0"]);
    }

    #[test]
    fn unnumbered_folders_are_ignored_when_others_are_numbered() {
        let listing = sort(vec![
            folder("current", 500),
            folder("4.1", 100),
            folder("4.2", 200),
        ]);

        assert_eq!(names(&listing), ["4.1", "4.2"]);
        assert_eq!(listing.ignored, [PathBuf::from("/tools/current")]);
    }

    #[test]
    fn modification_time_decides_when_nothing_is_numbered() {
        let listing = sort(vec![
            folder("newest", 300),
            folder("oldest", 100),
            folder("middle", 200),
        ]);

        assert_eq!(listing.order, OrderKey::Modified);
        assert_eq!(names(&listing), ["oldest", "middle", "newest"]);
        assert!(listing.ignored.is_empty());
    }

    #[test]
    fn reserved_names_are_ignored_even_when_nothing_is_numbered() {
        // The gap this closes: a folder that versions entirely by date has no
        // numbered sibling to route `current`/`shim` into `ignored` via the
        // "no digits" rule, so without this they would be swept up as
        // versions themselves — and, for `shim`, eventually archived away.
        let listing = sort(vec![
            folder("current", 500),
            folder("shim", 50),
            folder("oldest", 100),
            folder("newest", 200),
        ]);

        assert_eq!(listing.order, OrderKey::Modified);
        assert_eq!(names(&listing), ["oldest", "newest"]);
        assert_eq!(
            listing.ignored,
            [
                PathBuf::from("/tools/current"),
                PathBuf::from("/tools/shim")
            ]
        );
    }

    #[test]
    fn reserved_names_are_ignored_alongside_numbered_versions() {
        let listing = sort(vec![
            folder("shim", 500),
            folder("4.1", 100),
            folder("4.2", 200),
        ]);

        assert_eq!(names(&listing), ["4.1", "4.2"]);
        assert_eq!(listing.ignored, [PathBuf::from("/tools/shim")]);
    }

    #[test]
    fn equal_versions_break_the_tie_on_name() {
        let listing = sort(vec![folder("v4.1", 100), folder("4.1", 200)]);
        assert_eq!(names(&listing), ["4.1", "v4.1"]);
    }

    #[test]
    fn lists_only_directories() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("1.0")).unwrap();
        std::fs::create_dir(dir.path().join("2.0")).unwrap();
        // An archive a previous run wrote must not be mistaken for a version.
        std::fs::write(dir.path().join("0.9.7z"), b"7z").unwrap();

        let listing = list(dir.path()).unwrap();
        assert_eq!(names(&listing), ["1.0", "2.0"]);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_versions_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("1.0")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("1.0"), dir.path().join("2.0")).unwrap();

        let listing = list(dir.path()).unwrap();
        assert_eq!(names(&listing), ["1.0"]);
    }

    #[test]
    fn archives_are_listed_oldest_first_with_their_size() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("4.10.7z"), b"newer archive").unwrap();
        std::fs::write(dir.path().join("4.9.7z"), b"older").unwrap();
        // Not archives: a live version, and an unrelated file.
        std::fs::create_dir(dir.path().join("4.11")).unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"hello").unwrap();

        let archived = archived(dir.path()).unwrap();

        let names: Vec<&str> = archived.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, ["4.9", "4.10"]);
        assert_eq!(archived[0].bytes, 5);
        assert_eq!(archived[1].bytes, 13);
        // The name is the version, with the extension stripped back off.
        assert_eq!(archived[1].version.as_ref().unwrap().parts(), &[4, 10]);
    }

    #[test]
    fn a_folder_with_no_archives_lists_none() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("1.0")).unwrap();

        assert!(archived(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn listing_a_missing_folder_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            list(&dir.path().join("absent")),
            Err(Error::NotFound { .. })
        ));
    }
}
