//! `.vmanagerversions`: which version of each tool a project directory wants.
//!
//! The file is plain TOML, one `<tool> = "<version>"` line per pin, where
//! `<tool>` is the registered folder's own name (`nodejs`, not
//! `C:\Tools\nodejs`) so the file stays valid when committed to a repository
//! and checked out on a machine with the tools installed elsewhere.
//!
//! Three readers depend on that shape: `vmanager pin` edits it, `vmanager
//! run` keeps every pinned version unpacked, and every generated shim parses
//! it at launch (see `crate::shim`). The shims parse it with a line regex,
//! not a TOML parser, so [`write`] only ever produces `key = "value"` lines.

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::{Error, Result};

/// Name of the pin file `vmanager pin` writes into the current directory.
pub const FILE_NAME: &str = ".vmanagerversions";

/// First line of every pin file [`write`] produces.
const HEADER: &str = "# Tool versions pinned by `vmanager pin`. Edit with `vmanager pin`.";

/// Tool name to pinned version name, sorted so rewrites diff cleanly.
pub type Pins = BTreeMap<String, String>;

/// Read and parse the pin file at `path`.
pub fn read(path: &Path) -> Result<Pins> {
    let content = std::fs::read_to_string(path).map_err(|err| {
        Error::Pin(format!(
            "could not read pin file `{}`: {err}",
            path.display()
        ))
    })?;
    parse(&content, path)
}

/// Read the pin file at `path`, or no pins when it does not exist yet.
pub fn read_or_empty(path: &Path) -> Result<Pins> {
    if path.exists() {
        read(path)
    } else {
        Ok(Pins::new())
    }
}

/// Parse a pin file's contents; `path` only names the file in errors.
pub fn parse(content: &str, path: &Path) -> Result<Pins> {
    let pins: Pins = toml::from_str(content).map_err(|err| {
        Error::Pin(format!(
            "`{}` is not a valid pin file (expected `<tool> = \"<version>\"` lines): {err}",
            path.display()
        ))
    })?;

    // Pin files travel with repositories, so a version must name a folder
    // directly inside the tool's folder — never a path out of it.
    match pins.iter().find(|(_, version)| !is_folder_name(version)) {
        Some((tool, version)) => Err(Error::Pin(format!(
            "`{}` pins {tool} to `{version}`, which is not a version folder name",
            path.display()
        ))),
        None => Ok(pins),
    }
}

/// A single path component: not empty, not `.` or `..`, no separators or
/// drive colon. The shim template applies the same rule at launch.
fn is_folder_name(name: &str) -> bool {
    !matches!(name, "" | "." | "..") && !name.contains(['/', '\\', ':'])
}

/// Render pins in the one shape every reader understands.
pub fn render(pins: &Pins) -> String {
    let mut content = String::from(HEADER);
    content.push('\n');
    for (tool, version) in pins {
        content.push_str(&format!("{} = {}\n", quote(tool), quote(version)));
    }
    content
}

/// Write `pins` to `path`, replacing whatever was there.
pub fn write(path: &Path, pins: &Pins) -> Result<()> {
    std::fs::write(path, render(pins)).map_err(|err| {
        Error::Pin(format!(
            "could not write pin file `{}`: {err}",
            path.display()
        ))
    })
}

/// A TOML basic string. Folder and version names never hold quotes or
/// backslashes in practice; escaping them anyway keeps the file parseable.
fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pins(entries: &[(&str, &str)]) -> Pins {
        entries
            .iter()
            .map(|(tool, version)| (tool.to_string(), version.to_string()))
            .collect()
    }

    #[test]
    fn rendered_pins_parse_back_unchanged() {
        let original = pins(&[("nodejs", "22.16.0"), ("Blender", "4.2")]);
        let rendered = render(&original);

        assert!(rendered.starts_with(HEADER), "{rendered}");
        assert!(
            rendered.contains("\"nodejs\" = \"22.16.0\"\n"),
            "{rendered}"
        );
        assert_eq!(parse(&rendered, Path::new("x")).unwrap(), original);
    }

    #[test]
    fn bare_keys_written_by_hand_parse() {
        let parsed = parse("nodejs = \"22.16.0\"\n", Path::new("x")).unwrap();
        assert_eq!(parsed, pins(&[("nodejs", "22.16.0")]));
    }

    #[test]
    fn a_non_string_version_is_rejected() {
        let err = parse("nodejs = 22\n", Path::new("C:\\proj\\.vmanagerversions")).unwrap_err();
        assert!(err.to_string().contains(".vmanagerversions"), "{err}");
    }

    #[test]
    fn a_version_that_is_a_path_out_of_the_tool_folder_is_rejected() {
        for version in ["..", "..\\..\\evil", "../evil", "C:evil", ""] {
            let content = format!("node = {}\n", quote(version));
            assert!(
                parse(&content, Path::new("x")).is_err(),
                "{version:?} must be rejected"
            );
        }
    }

    #[test]
    fn a_missing_file_reads_as_no_pins() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            read_or_empty(&dir.path().join(FILE_NAME))
                .unwrap()
                .is_empty()
        );
        assert!(read(&dir.path().join(FILE_NAME)).is_err());
    }
}
