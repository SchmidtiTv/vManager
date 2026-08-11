//! Talking to pManager: which versions it has a project pinned to.
//!
//! vManager ages old tool versions out; pManager ages old projects out and
//! remembers which version of a tool each one wants (`pmanager project
//! set-version`). They stay separate binaries with separate databases —
//! nothing here reads pManager's database directly. The one thing that
//! crosses the boundary is `pmanager pins --json`: pManager knows which
//! project wants which version, vManager knows which versions are about to
//! be swept, and [`pinned_versions`] is the subprocess call that connects the
//! two, the mirror image of pManager's own `vmanager use` call in
//! `vmanager.rs` there. See `commands::run`.
//!
//! Detection works the same way pManager detects vManager (see its own
//! `vmanager.rs`): there is no separate "is it installed" check, just
//! running the command and treating every way that can fail — not
//! installed, not on `PATH`, a bad exit code, output that will not parse —
//! as "no pins known" rather than as an error that should stop a sweep.
//! pManager is optional; a machine that has never heard of it sweeps exactly
//! as it always has.

use std::path::PathBuf;
use std::process::Command;

use serde::Deserialize;

/// One version pManager has a project pinned to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedVersion {
    /// A vManager-registered folder — the parent of one tool's versions, not
    /// a version itself. Matched against a sweep's own folder by equality, so
    /// both sides need to agree on the same canonical spelling; see
    /// `commands::run::sweep_folder`.
    pub folder: PathBuf,
    pub version: String,
    /// The project that pinned it, kept only to explain a skip in output.
    pub project: String,
}

/// The `pmanager pins --json` document.
#[derive(Debug, Deserialize)]
struct PinsDocument {
    pins: Vec<PinEntry>,
}

#[derive(Debug, Deserialize)]
struct PinEntry {
    folder: String,
    version: String,
    project: String,
}

/// Every version pManager currently has a project pinned to.
///
/// Never fails: pManager not being installed is the common case, not an
/// error, and even a pManager that is on `PATH` but errors out or prints
/// something this build cannot parse is treated the same way — a sweep
/// should not refuse to run because the other tool had a bad day. The cost of
/// getting this wrong is a version being kept a little longer than the
/// newest-few rule alone would keep it, never the other way round.
pub fn pinned_versions() -> Vec<PinnedVersion> {
    pinned_versions_from("pmanager").unwrap_or_default()
}

/// [`pinned_versions`], with the binary name as a parameter so tests can
/// point it at something other than the real `pmanager`.
fn pinned_versions_from(binary: &str) -> Option<Vec<PinnedVersion>> {
    let output = Command::new(binary)
        .arg("pins")
        .arg("--json")
        .output()
        .inspect_err(|err| tracing::debug!(error = %err, "could not run pmanager"))
        .ok()?;

    if !output.status.success() {
        tracing::debug!(status = ?output.status, "pmanager pins failed");
        return None;
    }

    parse_pins(&output.stdout)
}

/// Parse a `pmanager pins --json` document. Split out from
/// [`pinned_versions_from`] so the parsing can be tested without a real
/// `pmanager` on `PATH`.
fn parse_pins(bytes: &[u8]) -> Option<Vec<PinnedVersion>> {
    let document: PinsDocument = serde_json::from_slice(bytes)
        .inspect_err(|err| tracing::warn!(error = %err, "could not parse pmanager pins output"))
        .ok()?;

    Some(
        document
            .pins
            .into_iter()
            .map(|pin| PinnedVersion {
                folder: PathBuf::from(pin.folder),
                version: pin.version,
                project: pin.project,
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_binary_not_on_path_is_reported_rather_than_failing() {
        assert_eq!(
            pinned_versions_from("vmanager-pmanager-detection-test-does-not-exist"),
            None
        );
    }

    #[test]
    fn a_binary_that_runs_but_fails_is_no_pins_known_rather_than_an_error() {
        // `cargo` is on PATH in every environment that can build this crate
        // at all; `cargo pins --json` is not a real subcommand, so this
        // exercises the non-zero-exit path without needing a fake `pmanager`.
        assert_eq!(pinned_versions_from("cargo"), None);
    }

    #[test]
    fn pmanagers_json_document_parses_into_pins() {
        let document = br#"{
            "pins": [
                {"folder": "/tools/node", "version": "20.11", "project": "api-server"},
                {"folder": "/tools/rust", "version": "1.93", "project": "desktop-app"}
            ]
        }"#;

        let pins = parse_pins(document).expect("valid document");
        assert_eq!(
            pins,
            vec![
                PinnedVersion {
                    folder: PathBuf::from("/tools/node"),
                    version: "20.11".to_string(),
                    project: "api-server".to_string(),
                },
                PinnedVersion {
                    folder: PathBuf::from("/tools/rust"),
                    version: "1.93".to_string(),
                    project: "desktop-app".to_string(),
                },
            ]
        );
    }

    #[test]
    fn an_empty_pins_document_parses_into_nothing() {
        assert_eq!(parse_pins(br#"{"pins": []}"#), Some(Vec::new()));
    }

    #[test]
    fn unparseable_output_is_no_pins_known_rather_than_an_error() {
        assert_eq!(parse_pins(b"not json"), None);
    }
}
