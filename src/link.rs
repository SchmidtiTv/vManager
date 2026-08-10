//! The `current` link: a directory junction (Windows) or symlink (Unix) at
//! `<folder>/current`, pointing at whichever version [`crate::commands::r#use`]
//! last selected.
//!
//! [`crate::version::list`] already treats a symlinked or junctioned child as
//! not a version — see its "skipping symlink" case — so `current` living
//! beside the real version folders is never mistaken for one of them,
//! archived or swept along with the rest.
//!
//! A junction rather than a Windows symlink on that platform: a symlink to a
//! directory needs Developer Mode or an elevated prompt, a junction needs
//! neither, and both resolve identically for anything that just reads through
//! `current`.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// Name of the link `use` writes and `list` looks for inside a registered
/// folder.
pub const LINK_NAME: &str = "current";

/// Point `<folder>/current` at `<folder>/<version>`, replacing whatever it
/// pointed at before.
///
/// `version` must already be a folder sitting directly under `folder` —
/// pointing `current` at something that is not there yet would make every
/// reader of the link see a dangling path instead of an error here, where the
/// mistake was actually made.
pub fn set(folder: &Path, version: &str) -> Result<PathBuf> {
    let target = folder.join(version);
    if !target.is_dir() {
        return Err(Error::NotFound {
            kind: "version",
            name: version.to_string(),
        });
    }

    let link = folder.join(LINK_NAME);
    clear_at(&link)?;
    create(&target, &link)?;
    Ok(link)
}

/// The version name `<folder>/current` points at, or `None` when there is no
/// link.
///
/// The target need not still exist — a version that has since been archived
/// still reports the name it was set to, so callers can say what is stale
/// rather than silently going quiet.
pub fn current(folder: &Path) -> Result<Option<String>> {
    let link = folder.join(LINK_NAME);
    match std::fs::read_link(&link) {
        Ok(target) => Ok(Some(
            target
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| target.display().to_string()),
        )),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

/// Remove `<folder>/current`, if it exists. Returns whether there was
/// anything to remove.
pub fn clear(folder: &Path) -> Result<bool> {
    let link = folder.join(LINK_NAME);
    let existed = std::fs::symlink_metadata(&link).is_ok();
    clear_at(&link)?;
    Ok(existed)
}

/// Remove whatever sits at `link`, if anything. [`set`] calls this on a
/// possibly-stale link before replacing it, so it has to cope with a link
/// left over from an earlier version of this tool as readily as with nothing
/// there at all.
fn clear_at(link: &Path) -> Result<()> {
    match std::fs::symlink_metadata(link) {
        Ok(_) => remove(link),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

#[cfg(unix)]
fn remove(link: &Path) -> Result<()> {
    // A directory symlink on Unix is removed the same way any symlink is;
    // `remove_dir` would refuse it, expecting a real directory.
    std::fs::remove_file(link)?;
    Ok(())
}

#[cfg(unix)]
fn create(target: &Path, link: &Path) -> Result<()> {
    std::os::unix::fs::symlink(target, link)?;
    Ok(())
}

#[cfg(windows)]
fn remove(link: &Path) -> Result<()> {
    // A junction is a directory-typed reparse point; removing it the way a
    // real directory is removed drops the reparse point without touching
    // whatever it pointed at.
    std::fs::remove_dir(link)?;
    Ok(())
}

#[cfg(windows)]
fn create(target: &Path, link: &Path) -> Result<()> {
    // No `std::fs` call creates a junction, and a Windows *symlink* to a
    // directory needs Developer Mode or an elevated prompt that a background
    // sweep cannot assume. `mklink /J` is the one thing on every Windows
    // install that makes one without either — the same reasoning that has
    // `schedule.rs` shell out to `schtasks` for the piece only the platform
    // itself can do.
    let output = std::process::Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(link)
        .arg(target)
        .output()?;

    if !output.status.success() {
        return Err(Error::Archive(format!(
            "could not link {} -> {}: {}",
            link.display(),
            target.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn setting_the_link_points_it_at_the_version() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("4.1")).unwrap();

        let link = set(dir.path(), "4.1").unwrap();
        assert_eq!(link, dir.path().join(LINK_NAME));
        assert_eq!(current(dir.path()).unwrap().as_deref(), Some("4.1"));
    }

    #[cfg(unix)]
    #[test]
    fn setting_it_again_repoints_rather_than_failing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("4.1")).unwrap();
        std::fs::create_dir(dir.path().join("4.2")).unwrap();

        set(dir.path(), "4.1").unwrap();
        set(dir.path(), "4.2").unwrap();

        assert_eq!(current(dir.path()).unwrap().as_deref(), Some("4.2"));
    }

    #[test]
    fn a_folder_with_no_link_reports_no_current_version() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(current(dir.path()).unwrap(), None);
    }

    #[test]
    fn pointing_at_a_version_that_does_not_exist_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            set(dir.path(), "9.9"),
            Err(Error::NotFound {
                kind: "version",
                ..
            })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn clearing_removes_the_link_and_reports_whether_one_was_there() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("4.1")).unwrap();
        set(dir.path(), "4.1").unwrap();

        assert!(clear(dir.path()).unwrap());
        assert_eq!(current(dir.path()).unwrap(), None);
        assert!(!clear(dir.path()).unwrap());
    }

    #[test]
    fn clearing_a_folder_with_no_link_reports_nothing_removed() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!clear(dir.path()).unwrap());
    }
}
