//! End-to-end tests driving the built binary the way a user would.
//!
//! Every test that touches configuration passes an explicit `--config` inside a
//! temporary directory, so the suite never reads or writes the real
//! `~/.config/vmanager`.

use assert_cmd::Command;
use predicates::prelude::*;
use vmanager::link;

/// A `vmanager` invocation with a clean environment.
fn vmanager() -> Command {
    let mut cmd = Command::cargo_bin("vmanager").expect("binary is built");
    cmd.env_remove("VMANAGER_CONFIG")
        .env_remove("RUST_LOG")
        .env("NO_COLOR", "1");
    cmd
}

#[test]
fn prints_version() {
    vmanager()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn prints_help_with_exit_code_zero() {
    vmanager()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"))
        .stdout(predicate::str::contains("Exit codes:"));
}

#[test]
fn bare_invocation_shows_help() {
    vmanager()
        .assert()
        .code(2)
        .stderr(predicate::str::contains("Usage:"));
}

#[test]
fn unknown_flag_is_a_usage_error() {
    vmanager().arg("--definitely-not-a-flag").assert().code(2);
}

#[test]
fn unknown_subcommand_is_a_usage_error() {
    vmanager().arg("frobnicate").assert().code(2);
}

#[test]
fn completion_scripts_are_written_to_stdout() {
    for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
        vmanager()
            .args(["completions", shell])
            .assert()
            .success()
            .stdout(predicate::str::contains("vmanager"));
    }
}

#[test]
fn the_completion_script_covers_the_subcommands() {
    vmanager()
        .args(["completions", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::contains("folder"))
        .stdout(predicate::str::contains("schedule"))
        .stdout(predicate::str::contains("--config"));
}

#[test]
fn completions_do_not_need_a_usable_config() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    std::fs::write(&config, "this is not: valid toml\n").unwrap();

    // A shell profile runs this at startup; a broken config must not break the
    // shell, and `list` next door proves the file really is unusable.
    vmanager()
        .args(["completions", "bash", "--config"])
        .arg(&config)
        .assert()
        .success();

    vmanager()
        .args(["list", "--config"])
        .arg(&config)
        .assert()
        .code(3);
}

#[test]
fn an_unknown_shell_is_a_usage_error() {
    vmanager().args(["completions", "tcsh"]).assert().code(2);
}

#[test]
fn the_shell_can_be_left_to_the_environment() {
    vmanager()
        .arg("completions")
        .env("SHELL", "/bin/bash")
        .assert()
        .success()
        .stdout(predicate::str::contains("vmanager"));
}

#[test]
fn the_config_environment_variable_is_honoured() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("from-env.toml");
    std::fs::write(&path, "keep_versions = 0\n").unwrap();

    // A file only VMANAGER_CONFIG points at still has to be read: the sweep
    // rejects the bad value rather than running with the default.
    vmanager()
        .env("VMANAGER_CONFIG", &path)
        .arg("run")
        .assert()
        .failure()
        .stderr(predicate::str::contains("at least 1"));
}

#[test]
fn quiet_suppresses_progress_output() {
    let (dir, config) = isolated();
    let tool = tool_folder(dir.path(), "blender", &["1.0", "2.0"]);

    vmanager()
        .args(["folder", "add"])
        .arg(&tool)
        .args(["--quiet", "--config"])
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn invalid_config_exits_with_the_config_code() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "this is not = = toml\n").unwrap();

    vmanager()
        .args(["run", "--config"])
        .arg(&path)
        .assert()
        .code(3)
        .stderr(predicate::str::contains("invalid config file"))
        .stderr(predicate::str::contains("caused by:"));
}

#[test]
fn an_unknown_config_key_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "keep_verisons = 3\n").unwrap();

    // Typos in a hand-written file must surface rather than be ignored.
    vmanager()
        .args(["run", "--config"])
        .arg(&path)
        .assert()
        .code(3);
}

#[test]
fn logs_go_to_stderr_not_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");

    // `-vv` enables debug logging, which reports the missing config file.
    vmanager()
        .args(["list", "-vv", "--config"])
        .arg(&path)
        .assert()
        .success()
        .stdout(predicate::str::contains("DEBUG").not())
        .stderr(predicate::str::contains("no config file"));
}

/// A config file pointing every data path at a throwaway directory, so these
/// tests never touch the real database.
fn isolated() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    // A TOML *literal* string (single quotes) — a basic string would treat the
    // backslashes in a Windows path as escape sequences and fail to parse.
    std::fs::write(
        &config,
        format!("data_dir = '{}'\n", dir.path().join("data").display()),
    )
    .unwrap();
    (dir, config)
}

// --- managing versions -----------------------------------------------------

/// A tool folder holding one subfolder per version, each with a file in it.
fn tool_folder(root: &std::path::Path, name: &str, versions: &[&str]) -> std::path::PathBuf {
    let tool = root.join(name);
    for version in versions {
        let path = tool.join(version);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("tool.exe"), format!("{name} {version}")).unwrap();
    }
    tool
}

#[test]
fn registering_a_folder_and_sweeping_it_archives_the_oldest_versions() {
    let (dir, config) = isolated();
    let tool = tool_folder(dir.path(), "blender", &["4.1", "4.2", "4.3", "4.10"]);

    vmanager()
        .args(["folder", "add"])
        .arg(&tool)
        .arg("--config")
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("registered"))
        .stdout(predicate::str::contains("4 version(s) present"));

    vmanager()
        .args(["run", "--config"])
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("archived 4.1"))
        .stdout(predicate::str::contains("archived 4.2"))
        .stdout(predicate::str::contains("2 archived"));

    // The two oldest are now archives; the two newest are untouched. Note that
    // 4.10 is kept and 4.2 is not: the ordering is numeric, not alphabetic.
    assert!(tool.join("4.1.7z").is_file());
    assert!(tool.join("4.2.7z").is_file());
    assert!(!tool.join("4.1").exists());
    assert!(!tool.join("4.2").exists());
    assert!(tool.join("4.3").is_dir());
    assert!(tool.join("4.10").is_dir());

    vmanager()
        .args(["list", "--config"])
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("keep 2, 2 unpacked, 2 archived"));
}

#[test]
fn a_second_sweep_has_nothing_left_to_do() {
    let (dir, config) = isolated();
    let tool = tool_folder(dir.path(), "godot", &["1.0", "2.0", "3.0"]);

    let sweep = || {
        let mut cmd = vmanager();
        cmd.args(["folder", "add"])
            .arg(&tool)
            .arg("--config")
            .arg(&config);
        cmd
    };
    sweep().assert().success();

    vmanager()
        .args(["run", "--config"])
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("1 archived"));

    // The .7z left behind must not be mistaken for a version folder, so the
    // daily run settles instead of eating a version every day.
    vmanager()
        .args(["run", "--config"])
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("nothing to archive"))
        .stdout(predicate::str::contains("0 archived"));

    assert!(tool.join("2.0").is_dir());
    assert!(tool.join("3.0").is_dir());
}

#[test]
fn a_dry_run_reports_without_touching_anything() {
    let (dir, config) = isolated();
    let tool = tool_folder(dir.path(), "blender", &["1.0", "2.0", "3.0"]);

    vmanager()
        .args(["folder", "add"])
        .arg(&tool)
        .arg("--config")
        .arg(&config)
        .assert()
        .success();

    vmanager()
        .args(["run", "--dry-run", "--config"])
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("would archive 1.0"))
        .stdout(predicate::str::contains("dry run"));

    assert!(tool.join("1.0").is_dir(), "a dry run must not delete");
    assert!(!tool.join("1.0.7z").exists(), "a dry run must not write");
}

#[test]
fn keep_source_writes_the_archive_but_leaves_the_folder() {
    let (dir, config) = isolated();
    let tool = tool_folder(dir.path(), "blender", &["1.0", "2.0", "3.0"]);

    vmanager()
        .args(["run", "--keep-source", "--folder"])
        .arg(&tool)
        .arg("--config")
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("folder kept"));

    assert!(tool.join("1.0.7z").is_file());
    assert!(tool.join("1.0").is_dir());
}

#[test]
fn keep_is_taken_from_the_flag_then_the_folder_then_the_config() {
    let (dir, config) = isolated();

    // A per-folder override of 1 beats the configured default of 3.
    std::fs::write(
        &config,
        format!(
            "data_dir = '{}'\nkeep_versions = 3\n",
            dir.path().join("data").display()
        ),
    )
    .unwrap();

    let tool = tool_folder(dir.path(), "blender", &["1.0", "2.0", "3.0", "4.0"]);
    vmanager()
        .args(["folder", "add"])
        .arg(&tool)
        .args(["--keep", "1", "--config"])
        .arg(&config)
        .assert()
        .success();

    vmanager()
        .args(["run", "--dry-run", "--config"])
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("keeping 1"))
        .stdout(predicate::str::contains("3 version(s) would be archived"));

    // …and an explicit --keep beats both.
    vmanager()
        .args(["run", "--dry-run", "--keep", "2", "--config"])
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("keeping 2"))
        .stdout(predicate::str::contains("2 version(s) would be archived"));
}

#[test]
fn a_keep_of_zero_is_a_usage_error() {
    let (_dir, config) = isolated();

    vmanager()
        .args(["run", "--keep", "0", "--config"])
        .arg(&config)
        .assert()
        .code(2);
}

#[test]
fn an_invalid_keep_versions_is_a_config_error() {
    let (dir, config) = isolated();
    std::fs::write(
        &config,
        format!(
            "data_dir = '{}'\nkeep_versions = 0\n",
            dir.path().join("data").display()
        ),
    )
    .unwrap();

    vmanager()
        .args(["run", "--config"])
        .arg(&config)
        .assert()
        .failure()
        .stderr(predicate::str::contains("at least 1"));
}

#[test]
fn folders_without_version_numbers_are_left_alone() {
    let (dir, config) = isolated();
    let tool = tool_folder(dir.path(), "blender", &["1.0", "2.0", "3.0"]);
    std::fs::create_dir(tool.join("current")).unwrap();
    std::fs::write(tool.join("current").join("tool.exe"), "live").unwrap();

    vmanager()
        .args(["run", "--folder"])
        .arg(&tool)
        .arg("--config")
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("ignoring"))
        .stdout(predicate::str::contains("1 archived"));

    assert!(tool.join("current").is_dir(), "`current` is not a version");
    assert!(!tool.join("current.7z").exists());
}

#[test]
fn listing_reflects_registration_and_remove_unregisters() {
    let (dir, config) = isolated();
    let tool = tool_folder(dir.path(), "blender", &["1.0", "2.0", "3.0"]);

    vmanager()
        .args(["list", "--config"])
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("no folders registered yet"));

    vmanager()
        .args(["folder", "add"])
        .arg(&tool)
        .arg("--config")
        .arg(&config)
        .assert()
        .success();

    vmanager()
        .args(["list", "--config"])
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("keep 2, 3 unpacked, 0 archived"));

    vmanager()
        .args(["folder", "remove"])
        .arg(&tool)
        .arg("--config")
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("unregistered"));

    vmanager()
        .args(["folder", "remove"])
        .arg(&tool)
        .arg("--config")
        .arg(&config)
        .assert()
        .code(4);
}

#[test]
fn registering_the_same_folder_twice_updates_it() {
    let (dir, config) = isolated();
    let tool = tool_folder(dir.path(), "blender", &["1.0", "2.0"]);

    vmanager()
        .args(["folder", "add"])
        .arg(&tool)
        .arg("--config")
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("registered"));

    vmanager()
        .args(["folder", "add"])
        .arg(&tool)
        .args(["--keep", "5", "--config"])
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("updated"))
        .stdout(predicate::str::contains("keeping 5"));

    vmanager()
        .args(["list", "--config"])
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("keep 5"));
}

#[test]
fn registering_a_missing_folder_is_not_found() {
    let (dir, config) = isolated();

    vmanager()
        .args(["folder", "add"])
        .arg(dir.path().join("absent"))
        .arg("--config")
        .arg(&config)
        .assert()
        .code(4);
}

#[test]
fn a_run_with_nothing_registered_says_so() {
    let (_dir, config) = isolated();

    vmanager()
        .args(["run", "--config"])
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("no folders registered yet"));
}

#[test]
fn one_broken_folder_does_not_stop_the_others() {
    let (dir, config) = isolated();
    let healthy = tool_folder(dir.path(), "blender", &["1.0", "2.0", "3.0"]);
    let gone = dir.path().join("unplugged-drive");
    std::fs::create_dir(&gone).unwrap();

    for folder in [&healthy, &gone] {
        vmanager()
            .args(["folder", "add"])
            .arg(folder)
            .arg("--config")
            .arg(&config)
            .assert()
            .success();
    }
    // `remove_dir_all`, not `remove_dir`: `folder add` now also creates an
    // (empty) `shim\` inside it, so the folder is no longer empty itself.
    std::fs::remove_dir_all(&gone).unwrap();

    // The missing folder fails the run, but only after the healthy one has
    // been swept — a disconnected drive must not strand every other tool.
    vmanager()
        .args(["run", "--config"])
        .arg(&config)
        .assert()
        .failure()
        .stdout(predicate::str::contains("1 archived"))
        .stderr(predicate::str::contains("not found"));

    assert!(healthy.join("1.0.7z").is_file());
}

#[test]
fn an_existing_archive_is_skipped_until_forced() {
    let (dir, config) = isolated();
    let tool = tool_folder(dir.path(), "blender", &["1.0", "2.0", "3.0"]);
    std::fs::write(tool.join("1.0.7z"), b"left over from something else").unwrap();

    let sweep = || {
        let mut cmd = vmanager();
        cmd.args(["run", "--folder"])
            .arg(&tool)
            .arg("--config")
            .arg(&config);
        cmd
    };

    sweep()
        .assert()
        .success()
        .stdout(predicate::str::contains("skipped 1.0"))
        .stdout(predicate::str::contains("--force"));
    assert!(tool.join("1.0").is_dir(), "the folder must survive a skip");

    sweep()
        .arg("--force")
        .assert()
        .success()
        .stdout(predicate::str::contains("archived 1.0"));
    assert!(!tool.join("1.0").exists());
}

#[test]
fn sweeping_an_unregistered_folder_works_without_registering_it() {
    let (dir, config) = isolated();
    let tool = tool_folder(dir.path(), "blender", &["1.0", "2.0", "3.0"]);

    vmanager()
        .args(["run", "--folder"])
        .arg(&tool)
        .arg("--config")
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("1 archived"));

    // Sweeping without registering leaves the registry empty…
    vmanager()
        .args(["list", "--config"])
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("no folders registered yet"));

    // …but the archive is still on disk and the folder still gone.
    assert!(tool.join("1.0.7z").is_file());
    assert!(!tool.join("1.0").exists());
}

#[test]
fn the_archive_of_a_swept_version_restores_it() {
    let (dir, config) = isolated();
    let tool = tool_folder(dir.path(), "blender", &["1.0", "2.0", "3.0"]);

    vmanager()
        .args(["run", "--folder"])
        .arg(&tool)
        .arg("--config")
        .arg(&config)
        .assert()
        .success();

    // The whole point of archiving rather than deleting: the old version can
    // come back, folder name and contents intact.
    let restored = dir.path().join("restored");
    sevenz_rust2::decompress_file(tool.join("1.0.7z"), &restored).unwrap();
    assert_eq!(
        std::fs::read_to_string(restored.join("1.0").join("tool.exe")).unwrap(),
        "blender 1.0"
    );
}

// --- coordinating with pManager ----------------------------------------------

/// Puts a fake `pmanager` on `PATH` that answers `pmanager pins --json` with
/// `pins_json` regardless of what it is actually called with — enough to
/// drive `vmanager run`'s side of the integration without a real pManager
/// installed. Returns the `PATH` to hand to the child process.
#[cfg(unix)]
fn fake_pmanager(dir: &std::path::Path, pins_json: &str) -> std::ffi::OsString {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt;

    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let script = bin.join("pmanager");
    {
        let mut file = std::fs::File::create(&script).unwrap();
        writeln!(file, "#!/bin/sh\ncat <<'PINS'\n{pins_json}\nPINS").unwrap();
    }
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut path = std::ffi::OsString::from(bin.as_os_str());
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());
    path
}

#[cfg(unix)]
#[test]
fn a_version_pinned_by_pmanager_is_kept_even_after_it_ages_out() {
    let (dir, config) = isolated();
    let tool = tool_folder(dir.path(), "node", &["18.20", "20.11", "22.16"]);
    // The database stores canonicalized paths; the pin must be spelled the
    // same way for the two to line up.
    let tool = tool.canonicalize().unwrap();

    let pins = format!(
        r#"{{"pins": [{{"folder": "{}", "version": "18.20", "project": "legacy-tool"}}]}}"#,
        tool.display()
    );
    let path = fake_pmanager(dir.path(), &pins);

    vmanager()
        .args(["run", "--keep", "1", "--folder"])
        .arg(&tool)
        .arg("--config")
        .arg(&config)
        .env("PATH", &path)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "keeping 18.20 (pinned by pmanager: legacy-tool)",
        ))
        .stdout(predicate::str::contains("archived 20.11"))
        .stdout(predicate::str::contains(
            "1 archived, 0 skipped, 1 protected",
        ));

    assert!(
        tool.join("18.20").is_dir(),
        "the pinned version must survive the sweep"
    );
    assert!(!tool.join("20.11").exists());
    assert!(tool.join("22.16").is_dir(), "the newest is kept regardless");
}

#[cfg(unix)]
#[test]
fn a_pin_for_a_different_folder_protects_nothing_here() {
    let (dir, config) = isolated();
    let tool = tool_folder(dir.path(), "node", &["18.20", "20.11"]);
    let elsewhere = dir.path().join("tools").join("rust");
    std::fs::create_dir_all(&elsewhere).unwrap();

    let pins = format!(
        r#"{{"pins": [{{"folder": "{}", "version": "18.20", "project": "other-tool"}}]}}"#,
        elsewhere.display()
    );
    let path = fake_pmanager(dir.path(), &pins);

    vmanager()
        .args(["run", "--keep", "1", "--folder"])
        .arg(&tool)
        .arg("--config")
        .arg(&config)
        .env("PATH", &path)
        .assert()
        .success()
        .stdout(predicate::str::contains("archived 18.20"))
        .stdout(predicate::str::contains(
            "1 archived, 0 skipped, 0 protected",
        ));

    assert!(!tool.join("18.20").exists());
}

#[cfg(unix)]
#[test]
fn pmanager_not_on_path_sweeps_exactly_as_without_it() {
    let (dir, config) = isolated();
    let tool = tool_folder(dir.path(), "node", &["18.20", "20.11"]);

    vmanager()
        .args(["run", "--keep", "1", "--folder"])
        .arg(&tool)
        .arg("--config")
        .arg(&config)
        .env("PATH", "")
        .assert()
        .success()
        .stdout(predicate::str::contains("archived 18.20"))
        .stdout(predicate::str::contains(
            "1 archived, 0 skipped, 0 protected",
        ));
}

#[test]
fn schedule_rejects_a_malformed_time() {
    vmanager()
        .args(["schedule", "install", "--at", "3pm"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("HH:MM"));
}

#[cfg(not(windows))]
#[test]
fn scheduling_off_windows_explains_itself() {
    vmanager()
        .args(["schedule", "install"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Task Scheduler"));
}

#[test]
fn list_shows_archived_and_unpacked_versions_oldest_first() {
    let (dir, config) = isolated();
    let tool = tool_folder(dir.path(), "blender", &["4.1", "4.2", "4.3", "4.10"]);

    vmanager()
        .args(["folder", "add"])
        .arg(&tool)
        .arg("--config")
        .arg(&config)
        .assert()
        .success();
    vmanager()
        .args(["run", "--config"])
        .arg(&config)
        .assert()
        .success();

    let rendered = String::from_utf8(
        vmanager()
            .args(["list", "--config"])
            .arg(&config)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap();

    // Oldest first, archives above the versions still on disk, and 4.10 last
    // because the ordering is numeric.
    let order: Vec<&str> = rendered
        .lines()
        .filter_map(|line| line.strip_prefix("  "))
        .map(|line| line.split_whitespace().next().unwrap_or_default())
        .collect();
    assert_eq!(order, ["4.1", "4.2", "4.3", "4.10"], "got:\n{rendered}");

    assert!(
        rendered.contains("keep 2, 2 unpacked, 2 archived"),
        "{rendered}"
    );
    assert!(rendered.contains("archived"), "{rendered}");
    assert!(rendered.contains("unpacked"), "{rendered}");
    // The original size comes from the database, not from the .7z.
    assert!(rendered.contains("in 1 file(s)"), "{rendered}");
}

#[test]
fn list_can_be_narrowed_to_one_state_or_one_folder() {
    let (dir, config) = isolated();
    let blender = tool_folder(dir.path(), "blender", &["1.0", "2.0", "3.0"]);
    let godot = tool_folder(dir.path(), "godot", &["1.0", "2.0"]);

    for folder in [&blender, &godot] {
        vmanager()
            .args(["folder", "add"])
            .arg(folder)
            .arg("--config")
            .arg(&config)
            .assert()
            .success();
    }
    vmanager()
        .args(["run", "--config"])
        .arg(&config)
        .assert()
        .success();

    vmanager()
        .args(["list", "--archived", "--config"])
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("archived"))
        .stdout(predicate::str::contains("unpacked\n").not());

    vmanager()
        .args(["list", "--unpacked", "--config"])
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("archived  ").not());

    vmanager()
        .args(["list", "--folder"])
        .arg(&godot)
        .arg("--config")
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("godot"))
        .stdout(predicate::str::contains("blender").not());
}

#[test]
fn list_reports_an_archive_the_database_has_never_seen() {
    let (dir, config) = isolated();
    let tool = tool_folder(dir.path(), "blender", &["2.0"]);
    // An archive from an older machine, or one whose database was deleted:
    // the file exists, so it should be listed even without a record.
    std::fs::write(tool.join("1.0.7z"), b"pretend archive").unwrap();

    vmanager()
        .args(["list", "--folder"])
        .arg(&tool)
        .arg("--config")
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("1.0"))
        .stdout(predicate::str::contains("not in the database"));
}

#[test]
fn list_survives_a_folder_that_has_gone_missing() {
    let (dir, config) = isolated();
    let tool = tool_folder(dir.path(), "blender", &["1.0", "2.0"]);

    vmanager()
        .args(["folder", "add"])
        .arg(&tool)
        .arg("--config")
        .arg(&config)
        .assert()
        .success();
    std::fs::remove_dir_all(&tool).unwrap();

    vmanager()
        .args(["list", "--config"])
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("unavailable"));
}

// --- the `current` link -----------------------------------------------------

#[test]
fn use_points_current_at_a_version_and_reports_it_back() {
    // `isolated`, not a bare tempdir: setting a version now also opens the
    // database (to look up hooks and regenerate shims), so this must not
    // touch the real platform database like every other `--config`-bearing
    // test avoids doing.
    let (dir, config) = isolated();
    let tool = tool_folder(dir.path(), "node", &["18.20", "20.11"]);

    vmanager()
        .arg("use")
        .arg(&tool)
        .arg("20.11")
        .arg("--config")
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("current"))
        .stdout(predicate::str::contains("20.11"));

    // Asking again with no version reports what it is currently set to.
    vmanager()
        .arg("use")
        .arg(&tool)
        .arg("--config")
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("20.11"));

    // `list` marks the entry it points at.
    vmanager()
        .args(["list", "--folder"])
        .arg(&tool)
        .arg("--config")
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("20.11").and(predicate::str::contains("(current)")))
        .stdout(predicate::str::contains("18.20  unpacked\n"));
}

#[test]
fn use_can_be_repointed_and_then_cleared() {
    let (dir, config) = isolated();
    let tool = tool_folder(dir.path(), "node", &["18.20", "20.11"]);

    vmanager()
        .arg("use")
        .arg(&tool)
        .arg("18.20")
        .arg("--config")
        .arg(&config)
        .assert()
        .success();
    vmanager()
        .arg("use")
        .arg(&tool)
        .arg("20.11")
        .arg("--config")
        .arg(&config)
        .assert()
        .success();

    vmanager()
        .arg("use")
        .arg(&tool)
        .arg("--config")
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("20.11"));

    vmanager()
        .arg("use")
        .arg(&tool)
        .arg("--clear")
        .arg("--config")
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("cleared"));

    vmanager()
        .arg("use")
        .arg(&tool)
        .arg("--config")
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("no current link"));
}

#[test]
fn use_on_a_version_that_does_not_exist_is_not_found() {
    let (dir, config) = isolated();
    let tool = tool_folder(dir.path(), "node", &["18.20"]);

    vmanager()
        .arg("use")
        .arg(&tool)
        .arg("99.0")
        .arg("--config")
        .arg(&config)
        .assert()
        .code(4);
}

#[test]
fn use_and_a_version_number_conflict_with_clear() {
    let (dir, config) = isolated();
    let tool = tool_folder(dir.path(), "node", &["18.20"]);

    vmanager()
        .arg("use")
        .arg(&tool)
        .arg("18.20")
        .arg("--clear")
        .arg("--config")
        .arg(&config)
        .assert()
        .code(2);
}

#[test]
fn the_current_link_is_not_swept_as_a_version() {
    let (dir, config) = isolated();
    let tool = tool_folder(dir.path(), "node", &["18.20", "20.11"]);

    // Points at the version that will survive the sweep, so the assertions
    // below aren't tangled up with the link going stale too.
    vmanager()
        .arg("use")
        .arg(&tool)
        .arg("20.11")
        .arg("--config")
        .arg(&config)
        .assert()
        .success();

    vmanager()
        .args(["folder", "add"])
        .arg(&tool)
        .arg("--config")
        .arg(&config)
        .arg("--keep")
        .arg("1")
        .assert()
        .success();

    // Sweeping with keep=1 archives the single oldest *version* — the
    // `current` link sitting beside them must never be mistaken for one.
    vmanager()
        .args(["run", "--config"])
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("archived 18.20"));

    assert_eq!(link::current(&tool).unwrap().as_deref(), Some("20.11"));
    assert!(tool.join("20.11").is_dir());
}

// --- command shims and hooks ------------------------------------------------

#[test]
fn folder_add_creates_an_empty_shim_directory() {
    let (dir, config) = isolated();
    let tool = tool_folder(dir.path(), "blender", &["4.1"]);

    vmanager()
        .args(["folder", "add"])
        .arg(&tool)
        .arg("--config")
        .arg(&config)
        .assert()
        .success();

    let shim_dir = tool.join("shim");
    assert!(shim_dir.is_dir());
    assert_eq!(std::fs::read_dir(&shim_dir).unwrap().count(), 0);
}

#[test]
fn folder_remove_leaves_the_shim_directory_and_its_scripts_on_disk() {
    let (dir, config) = isolated();
    let tool = tool_folder(dir.path(), "blender", &["4.1"]);

    vmanager()
        .args(["folder", "add"])
        .arg(&tool)
        .arg("--config")
        .arg(&config)
        .assert()
        .success();
    vmanager()
        .arg("use")
        .arg(&tool)
        .arg("4.1")
        .arg("--config")
        .arg(&config)
        .assert()
        .success();
    assert!(tool.join("shim").join("tool.ps1").is_file());

    vmanager()
        .args(["folder", "remove"])
        .arg(&tool)
        .arg("--config")
        .arg(&config)
        .assert()
        .success();

    // "nothing on disk is touched" applies to `shim\` too.
    assert!(tool.join("shim").join("tool.ps1").is_file());
}

#[test]
fn use_populates_a_shim_for_every_discovered_exe() {
    let (dir, config) = isolated();
    let tool = tool_folder(dir.path(), "node", &["18.20", "20.11"]);

    vmanager()
        .arg("use")
        .arg(&tool)
        .arg("20.11")
        .arg("--config")
        .arg(&config)
        .assert()
        .success();

    let script = tool.join("shim").join("tool.ps1");
    assert!(script.is_file());
    let content = std::fs::read_to_string(&script).unwrap();
    assert!(content.contains("DO NOT EDIT"), "{content}");
    // Resolved through `current`, not baked to `20.11`.
    assert!(content.contains("current\\tool.exe"), "{content}");
    assert!(!content.contains("20.11"), "{content}");
}

#[test]
fn use_shims_ps1_bat_sh_and_zsh_scripts_alongside_exes() {
    let (dir, config) = isolated();
    let tool = dir.path().join("mixed");
    let version = tool.join("1.0");
    std::fs::create_dir_all(&version).unwrap();
    std::fs::write(version.join("tool.exe"), b"exe").unwrap();
    std::fs::write(version.join("deploy.ps1"), b"# deploy").unwrap();
    std::fs::write(version.join("build.bat"), b"@echo build").unwrap();
    std::fs::write(version.join("install.sh"), b"#!/bin/sh\necho install").unwrap();
    std::fs::write(version.join("setup.zsh"), b"#!/bin/zsh\necho setup").unwrap();

    vmanager()
        .arg("use")
        .arg(&tool)
        .arg("1.0")
        .arg("--config")
        .arg(&config)
        .assert()
        .success();

    let shim_dir = tool.join("shim");
    for command in ["tool", "deploy", "build", "install", "setup"] {
        assert!(
            shim_dir.join(format!("{command}.ps1")).is_file(),
            "expected a shim for {command}"
        );
    }

    let sh_shim = std::fs::read_to_string(shim_dir.join("install.ps1")).unwrap();
    assert!(
        sh_shim.contains("& bash $Exe @args"),
        "a .sh target should be launched through bash: {sh_shim}"
    );

    let zsh_shim = std::fs::read_to_string(shim_dir.join("setup.ps1")).unwrap();
    assert!(
        zsh_shim.contains("& zsh $Exe @args"),
        "a .zsh target should be launched through zsh: {zsh_shim}"
    );

    let ps1_shim = std::fs::read_to_string(shim_dir.join("deploy.ps1")).unwrap();
    assert!(
        ps1_shim.contains("& $Exe @args"),
        "a .ps1 target should be launched directly: {ps1_shim}"
    );

    let bat_shim = std::fs::read_to_string(shim_dir.join("build.ps1")).unwrap();
    assert!(
        bat_shim.contains("& $Exe @args"),
        "a .bat target should be launched directly: {bat_shim}"
    );
}

#[test]
fn switching_to_a_version_with_the_same_exe_set_does_not_rewrite_the_shim() {
    let (dir, config) = isolated();
    let tool = tool_folder(dir.path(), "node", &["18.20", "20.11"]);

    vmanager()
        .arg("use")
        .arg(&tool)
        .arg("18.20")
        .arg("--config")
        .arg(&config)
        .assert()
        .success();
    let script = tool.join("shim").join("tool.ps1");
    let before = std::fs::metadata(&script).unwrap().modified().unwrap();

    std::thread::sleep(std::time::Duration::from_millis(20));
    vmanager()
        .arg("use")
        .arg(&tool)
        .arg("20.11")
        .arg("--config")
        .arg(&config)
        .assert()
        .success();

    let after = std::fs::metadata(&script).unwrap().modified().unwrap();
    assert_eq!(
        before, after,
        "the junction alone should retarget the shim; the file itself must not be rewritten"
    );
    assert_eq!(link::current(&tool).unwrap().as_deref(), Some("20.11"));
}

#[test]
fn hook_set_patches_an_already_existing_shim_immediately() {
    let (dir, config) = isolated();
    let tool = tool_folder(dir.path(), "node", &["18.20"]);
    let hook_script = dir.path().join("hook.ps1");
    std::fs::write(&hook_script, "# hook\n").unwrap();

    vmanager()
        .arg("use")
        .arg(&tool)
        .arg("18.20")
        .arg("--config")
        .arg(&config)
        .assert()
        .success();

    vmanager()
        .args(["hook", "set"])
        .arg(&tool)
        .arg("tool")
        .arg(&hook_script)
        .arg("--config")
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("updated"));

    let canonical_hook = hook_script.canonicalize().unwrap();
    let content = std::fs::read_to_string(tool.join("shim").join("tool.ps1")).unwrap();
    assert!(
        content.contains(&canonical_hook.display().to_string()),
        "{content}"
    );
}

#[test]
fn hook_set_before_the_shim_exists_takes_effect_on_the_next_use() {
    let (dir, config) = isolated();
    let tool = tool_folder(dir.path(), "node", &["18.20"]);
    let hook_script = dir.path().join("hook.ps1");
    std::fs::write(&hook_script, "# hook\n").unwrap();

    vmanager()
        .args(["folder", "add"])
        .arg(&tool)
        .arg("--config")
        .arg(&config)
        .assert()
        .success();

    vmanager()
        .args(["hook", "set"])
        .arg(&tool)
        .arg("tool")
        .arg(&hook_script)
        .arg("--config")
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("no shim yet"));
    assert!(!tool.join("shim").join("tool.ps1").exists());

    vmanager()
        .arg("use")
        .arg(&tool)
        .arg("18.20")
        .arg("--config")
        .arg(&config)
        .assert()
        .success();

    let canonical_hook = hook_script.canonicalize().unwrap();
    let content = std::fs::read_to_string(tool.join("shim").join("tool.ps1")).unwrap();
    assert!(
        content.contains(&canonical_hook.display().to_string()),
        "{content}"
    );
}

#[test]
fn hook_remove_clears_the_hook_from_an_existing_shim() {
    let (dir, config) = isolated();
    let tool = tool_folder(dir.path(), "node", &["18.20"]);
    let hook_script = dir.path().join("hook.ps1");
    std::fs::write(&hook_script, "# hook\n").unwrap();

    vmanager()
        .arg("use")
        .arg(&tool)
        .arg("18.20")
        .arg("--config")
        .arg(&config)
        .assert()
        .success();
    vmanager()
        .args(["hook", "set"])
        .arg(&tool)
        .arg("tool")
        .arg(&hook_script)
        .arg("--config")
        .arg(&config)
        .assert()
        .success();

    vmanager()
        .args(["hook", "remove"])
        .arg(&tool)
        .arg("tool")
        .arg("--config")
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("updated"));

    let content = std::fs::read_to_string(tool.join("shim").join("tool.ps1")).unwrap();
    assert!(!content.contains("$Hook"), "{content}");
}

#[test]
fn removing_an_unconfigured_hook_is_not_found() {
    let (dir, config) = isolated();
    let tool = tool_folder(dir.path(), "node", &["18.20"]);

    vmanager()
        .args(["hook", "remove"])
        .arg(&tool)
        .arg("tool")
        .arg("--config")
        .arg(&config)
        .assert()
        .code(4);
}

#[test]
fn hook_set_on_a_missing_script_is_not_found() {
    let (dir, config) = isolated();
    let tool = tool_folder(dir.path(), "node", &["18.20"]);

    vmanager()
        .args(["hook", "set"])
        .arg(&tool)
        .arg("tool")
        .arg(dir.path().join("absent.ps1"))
        .arg("--config")
        .arg(&config)
        .assert()
        .code(4);
}

#[test]
fn hook_list_shows_configured_hooks_and_can_be_narrowed_to_a_folder() {
    let (dir, config) = isolated();
    let node = tool_folder(dir.path(), "node", &["18.20"]);
    let godot = tool_folder(dir.path(), "godot", &["4.1"]);
    let hook_script = dir.path().join("hook.ps1");
    std::fs::write(&hook_script, "# hook\n").unwrap();

    for tool in [&node, &godot] {
        vmanager()
            .args(["hook", "set"])
            .arg(tool)
            .arg("tool")
            .arg(&hook_script)
            .arg("--config")
            .arg(&config)
            .assert()
            .success();
    }

    vmanager()
        .args(["hook", "list", "--config"])
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("node").and(predicate::str::contains("godot")));

    vmanager()
        .args(["hook", "list", "--folder"])
        .arg(&node)
        .arg("--config")
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("node"))
        .stdout(predicate::str::contains("godot").not());
}

#[test]
fn shim_list_shows_discovered_commands_and_hook_status() {
    let (dir, config) = isolated();
    let tool = tool_folder(dir.path(), "node", &["18.20"]);
    let hook_script = dir.path().join("hook.ps1");
    std::fs::write(&hook_script, "# hook\n").unwrap();

    vmanager()
        .arg("use")
        .arg(&tool)
        .arg("18.20")
        .arg("--config")
        .arg(&config)
        .assert()
        .success();
    vmanager()
        .args(["hook", "set"])
        .arg(&tool)
        .arg("tool")
        .arg(&hook_script)
        .arg("--config")
        .arg(&config)
        .assert()
        .success();

    let canonical_hook = hook_script.canonicalize().unwrap();
    vmanager()
        .args(["shim", "list", "--folder"])
        .arg(&tool)
        .arg("--config")
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("tool"))
        .stdout(predicate::str::contains("hook:"))
        .stdout(predicate::str::contains(
            canonical_hook.display().to_string(),
        ));
}

#[test]
fn shim_regenerate_rebuilds_a_cleared_shim_directory() {
    let (dir, config) = isolated();
    let tool = tool_folder(dir.path(), "node", &["18.20"]);

    vmanager()
        .arg("use")
        .arg(&tool)
        .arg("18.20")
        .arg("--config")
        .arg(&config)
        .assert()
        .success();
    std::fs::remove_file(tool.join("shim").join("tool.ps1")).unwrap();
    assert!(!tool.join("shim").join("tool.ps1").exists());

    vmanager()
        .args(["shim", "regenerate", "--folder"])
        .arg(&tool)
        .arg("--config")
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("created"));

    assert!(tool.join("shim").join("tool.ps1").is_file());
}

#[test]
fn a_same_folder_exe_name_collision_names_both_paths_and_exits_with_failure() {
    let (dir, config) = isolated();
    let tool = tool_folder(dir.path(), "node", &["18.20"]);
    // Two exes that both resolve to the `node` command, in different bin
    // directories under the same version.
    std::fs::create_dir_all(tool.join("18.20").join("bin1")).unwrap();
    std::fs::create_dir_all(tool.join("18.20").join("bin2")).unwrap();
    std::fs::write(tool.join("18.20").join("bin1").join("node.exe"), b"a").unwrap();
    std::fs::write(tool.join("18.20").join("bin2").join("node.exe"), b"b").unwrap();

    vmanager()
        .arg("use")
        .arg(&tool)
        .arg("18.20")
        .arg("--config")
        .arg(&config)
        .assert()
        .code(1)
        .stderr(
            predicate::str::contains("node")
                .and(predicate::str::contains("bin1"))
                .and(predicate::str::contains("bin2")),
        );

    // The link itself is still set — only the shim regeneration failed,
    // loudly, after it.
    assert_eq!(link::current(&tool).unwrap().as_deref(), Some("18.20"));
}

#[test]
fn a_dry_run_sweep_does_not_regenerate_shims() {
    let (dir, config) = isolated();
    let tool = tool_folder(dir.path(), "node", &["18.20", "20.11"]);

    vmanager()
        .args(["folder", "add"])
        .arg(&tool)
        .arg("--config")
        .arg(&config)
        .assert()
        .success();
    // Created empty by `folder add`; nothing has called `use` or a real
    // `run` yet.
    assert_eq!(std::fs::read_dir(tool.join("shim")).unwrap().count(), 0);

    vmanager()
        .args(["run", "--dry-run", "--config"])
        .arg(&config)
        .assert()
        .success();

    assert_eq!(
        std::fs::read_dir(tool.join("shim")).unwrap().count(),
        0,
        "a dry run must not write shim scripts either"
    );
}

#[test]
fn run_recovers_the_shim_after_the_active_version_gets_archived() {
    let (dir, config) = isolated();
    let tool = dir.path().join("node");
    // Deliberately different layouts, so the shim content genuinely has to
    // change when discovery falls back from one to the other.
    std::fs::create_dir_all(tool.join("18.20")).unwrap();
    std::fs::write(tool.join("18.20").join("tool.exe"), b"old").unwrap();
    std::fs::create_dir_all(tool.join("20.11").join("bin")).unwrap();
    std::fs::write(tool.join("20.11").join("bin").join("tool.exe"), b"new").unwrap();

    vmanager()
        .arg("use")
        .arg(&tool)
        .arg("18.20")
        .arg("--config")
        .arg(&config)
        .assert()
        .success();
    let before = std::fs::read_to_string(tool.join("shim").join("tool.ps1")).unwrap();
    assert!(before.contains("current\\tool.exe"), "{before}");

    vmanager()
        .args(["folder", "add"])
        .arg(&tool)
        .arg("--config")
        .arg(&config)
        .arg("--keep")
        .arg("1")
        .assert()
        .success();

    // Keeping only the newest archives 18.20 — the very version `current`
    // points at.
    vmanager()
        .args(["run", "--config"])
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("archived 18.20"));

    // `current` is now dangling; discovery falls back to the newest
    // unpacked version, 20.11, whose exe lives under `bin\` — and the shim
    // is rewritten to match without anyone running `use` again first.
    let after = std::fs::read_to_string(tool.join("shim").join("tool.ps1")).unwrap();
    assert!(after.contains("current\\bin\\tool.exe"), "{after}");
}
