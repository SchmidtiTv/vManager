//! `vmanager use` — point a registered folder's `current` link at one of its
//! versions.
//!
//! This is the one command in vManager that changes something other tools are
//! meant to read. `<folder>/current` is a stable path: put it on `PATH` once,
//! and every later `vmanager use` retargets what that path resolves to
//! without needing a new shell or process to see the change — unlike editing
//! `PATH` itself, which only a fresh shell would pick up.

use std::io::Write;
use std::path::PathBuf;

use crate::archive;
use crate::commands::Context;
use crate::error::Result;
use crate::link;
use crate::shim;

#[derive(Debug, clap::Args)]
pub struct Args {
    /// A folder shown by `vmanager list`
    #[arg(value_name = "FOLDER")]
    folder: PathBuf,

    /// The version to point `current` at [default: show what it is set to]
    //
    // Named `target` rather than `version`: clap gives every subcommand its
    // own `--version`/`-V` flag (`propagate_version = true` on the top-level
    // `Cli`), and a positional argument called `version` collides with it —
    // both would share the clap argument id "version".
    #[arg(value_name = "VERSION", conflicts_with = "clear")]
    target: Option<String>,

    /// Remove the current link instead of setting it
    #[arg(long)]
    clear: bool,
}

pub fn run(args: &Args, ctx: &Context) -> Result<()> {
    let folder = archive::canonical_dir(&args.folder)?;
    let mut out = std::io::stdout().lock();

    if args.clear {
        let existed = link::clear(&folder)?;
        if !ctx.is_quiet() {
            if existed {
                writeln!(out, "cleared {}", folder.join(link::LINK_NAME).display())?;
            } else {
                writeln!(out, "{} has no current link", folder.display())?;
            }
        }
        return Ok(());
    }

    let Some(version) = &args.target else {
        if !ctx.is_quiet() {
            match link::current(&folder)? {
                Some(version) => writeln!(
                    out,
                    "{} -> {version}",
                    folder.join(link::LINK_NAME).display()
                )?,
                None => writeln!(out, "{} has no current link", folder.display())?,
            }
        }
        return Ok(());
    };

    let target = link::set(&folder, version)?;
    if !ctx.is_quiet() {
        writeln!(out, "{} -> {version}", target.display())?;
    }

    // The target set behind `current` could have changed shape (a version
    // with a different `bin\` layout, or a tool that dropped/added a
    // binary), so the shims are regenerated here rather than left to the
    // next `run` —
    // not because retargeting `current` itself needs it: the junction
    // already makes every existing shim resolve to the new version for free.
    regenerate_shims(ctx, &folder)?;

    Ok(())
}

/// Rebuild `<folder>/shim/` against whatever `current` (or, lacking that,
/// the newest unpacked version) now contains.
pub fn regenerate_shims(ctx: &Context, folder: &std::path::Path) -> Result<()> {
    let db = ctx.open_database()?;
    let hooks = db
        .hooks_for_folder(folder)?
        .into_iter()
        .map(|hook| (hook.command, hook.script_path))
        .collect();

    let reports = shim::write_shims(folder, &hooks)?;
    if !ctx.is_quiet() && !reports.is_empty() {
        writeln!(
            std::io::stdout().lock(),
            "  shim: {} command(s)",
            reports.len()
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::GlobalArgs;

    /// A `Context` whose database lives under `root` rather than the real
    /// platform data directory — `run` now opens the database (to regenerate
    /// shims) even when setting a version, not just when it needs to look up
    /// a hook, so every test driving `run` needs one of these.
    fn ctx(root: &std::path::Path) -> Context {
        let config = root.join("config.toml");
        std::fs::write(
            &config,
            format!("data_dir = '{}'\n", root.join("data").display()),
        )
        .unwrap();
        Context::new(GlobalArgs {
            verbose: 0,
            quiet: false,
            config: Some(config),
            color: crate::cli::ColorChoice::Never,
        })
    }

    fn folder(root: &std::path::Path, versions: &[&str]) -> PathBuf {
        for version in versions {
            std::fs::create_dir_all(root.join(version)).unwrap();
        }
        root.to_path_buf()
    }

    #[test]
    fn args_without_a_version_are_valid() {
        use clap::Parser;

        #[derive(clap::Parser)]
        struct Wrapper {
            #[command(flatten)]
            args: Args,
        }

        let parsed = Wrapper::try_parse_from(["vmanager", "C:\\Tools\\Blender"]).unwrap();
        assert!(parsed.args.target.is_none());
        assert!(!parsed.args.clear);
    }

    #[test]
    fn version_and_clear_conflict() {
        use clap::Parser;

        #[derive(clap::Parser)]
        struct Wrapper {
            #[command(flatten)]
            args: Args,
        }

        assert!(
            Wrapper::try_parse_from(["vmanager", "C:\\Tools\\Blender", "4.1", "--clear"]).is_err()
        );
    }

    #[test]
    fn setting_and_then_showing_the_current_link() {
        let dir = tempfile::tempdir().unwrap();
        let tool = folder(dir.path(), &["4.1", "4.2"]);

        run(
            &Args {
                folder: tool.clone(),
                target: Some("4.2".into()),
                clear: false,
            },
            &ctx(dir.path()),
        )
        .unwrap();

        assert_eq!(link::current(&tool).unwrap().as_deref(), Some("4.2"));

        // Calling it again without a version reports what is current rather
        // than requiring one.
        run(
            &Args {
                folder: tool.clone(),
                target: None,
                clear: false,
            },
            &ctx(dir.path()),
        )
        .unwrap();
    }

    #[test]
    fn clearing_removes_the_link() {
        let dir = tempfile::tempdir().unwrap();
        let tool = folder(dir.path(), &["4.1"]);

        run(
            &Args {
                folder: tool.clone(),
                target: Some("4.1".into()),
                clear: false,
            },
            &ctx(dir.path()),
        )
        .unwrap();

        run(
            &Args {
                folder: tool.clone(),
                target: None,
                clear: true,
            },
            &ctx(dir.path()),
        )
        .unwrap();

        assert_eq!(link::current(&tool).unwrap(), None);
    }
}
