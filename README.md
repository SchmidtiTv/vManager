# vManager

## Why

Tools installed as plain version folders (`C:\Tools\<tool>\<version>`) pile
up: every update leaves the old version behind, and each one can take
gigabytes. Deleting them is risky because an old project might still need
one, and switching versions usually means editing `PATH` by hand. vManager
compresses old versions instead of deleting them, keeps the ones your
projects pin, and switches versions through a stable `current` link and
command shims, so `PATH` is set once and never touched again.

## What it does

Keeps the disk from filling up with old tool versions. Point it at a folder
with one subfolder per version; it compresses all but the newest few into
7z archives named `<version>.vmversion` and deletes the compressed folders.

```
C:\Tools\Blender\   before                after
  4.1\    1.9 GiB   ->  4.1.vmversion  412 MiB
  4.2\    2.0 GiB   ->  4.2.vmversion  430 MiB
  4.3\    2.1 GiB   ->  4.3\           2.1 GiB
  4.10\   2.2 GiB   ->  4.10\          2.2 GiB
```

Rust CLI, shipped as `vmanager` and `vman`. SQLite and 7z compression are
compiled in; nothing else to install.

## Build

```sh
cargo build --release      # target/release/vmanager and vman
cargo install --path .
```

Requires Rust 1.93+ (edition 2024).

## Quick start

```sh
vmanager folder add C:\Tools\Blender           # register (parent of the versions)
vmanager folder add C:\Tools\Godot --keep 3
vmanager list
vmanager run --dry-run
vmanager run
vmanager use C:\Tools\Blender 4.3              # point `current` at a version
vmanager schedule install                      # run daily at 03:00
```

## Commands

| Command | Description |
| --- | --- |
| `run` | Archive all but the newest versions in every registered folder |
| `list` | Show every version of every registered folder |
| `folder add <FOLDER> [--keep N]` | Register a folder |
| `folder remove <FOLDER>` | Unregister a folder; nothing on disk is touched |
| `use <FOLDER> [<VERSION>] [--clear]` | Set, show or remove the `current` link |
| `shim list\|regenerate [--folder <FOLDER>]` | Show or rewrite command shims |
| `hook set <FOLDER> <COMMAND> <SCRIPT>` | Run a script before a shimmed command |
| `hook remove <FOLDER> <COMMAND>` | Remove a hook |
| `hook list [--folder <FOLDER>]` | Show hooks |
| `pin [<TOOL> [<VERSION>]] [--clear]` | Pin versions for the current directory (menu without args) |
| `schedule install\|uninstall\|status` | Manage the Windows Task Scheduler entry |
| `completions [SHELL]` | Print a shell completion script |

Useful `run` flags: `--dry-run`, `--keep N`, `--folder <FOLDER>` (repeatable,
also for unregistered folders), `--keep-source`, `--force`, `--level 0-9`,
`--threads N`.

Global options: `-v`/`-vv`/`-vvv`, `-q`, `-c <FILE>`, `--color auto|always|never`.
Environment: `VMANAGER_CONFIG`, `NO_COLOR`, `RUST_LOG`.

Exit codes: 0 ok, 1 failure, 2 bad args, 3 config, 4 not found, 5 database,
6 archive, 130 interrupted.

## How it works

- **Ordering:** versions are sorted by the numbers in their name, compared
  numerically (`4.9` < `4.10`). Folders without digits (`current`, `shim`,
  `.cache`) are not versions. If no folder has digits, modification time is
  used instead.
- **Safety:** the archive is written, read back and its file count verified
  before the folder is deleted. Existing archives are skipped unless `--force`.
  One failing folder does not stop the others, but the exit code is non-zero.
- **`current` link:** a junction (Windows) or symlink pointing at the selected
  version, so a fixed path on `PATH` follows `vmanager use`.
- **Shims:** each registered folder has a `shim\` folder with a `<cmd>.ps1` and
  `<cmd>.cmd` per executable found in `current` (or its `bin\`). Put `shim\` on
  `PATH` once. Hand-written files in `shim\` are left alone.
- **Hooks:** a script run before a shimmed command; non-zero exit aborts it.
- **Pins:** `vmanager pin` writes a `.vmanagerversions` file in the project:

  ```toml
  "node" = "18.20"
  ```

  Shims walk up from the working directory and use the first pin found,
  otherwise `current`. Pinned versions are never archived; pinning an archived
  version unpacks it.

## Configuration

Optional `config.toml` in `%APPDATA%\vmanager\` (Windows),
`~/.config/vmanager/` (Linux) or `~/Library/Application Support/vmanager/`
(macOS). Precedence: defaults < file < env < flags. Unknown keys are errors.

| Key | Default | Meaning |
| --- | --- | --- |
| `data_dir` | platform data dir | Where data is kept |
| `database` | `vmanager.db` in `data_dir` | SQLite file |
| `keep_versions` | `2` | Versions left unpacked (0 is rejected) |
| `compression_level` | `6` | LZMA2 level, 0-9 |
| `compression_threads` | `1` | Compression threads |

The database (tables `folders`, `archives`, `hooks`, `pin_files`) is migrated
automatically. It is a record only; the files on disk are the source of truth.

## Development

```sh
cargo test
cargo clippy --all-targets
cargo fmt --check
cargo deny check
```

CI runs fmt, clippy, release build and `cargo deny`, but not the tests.

To add a command: create `src/commands/<name>.rs` (clap `Args` struct plus
`run(&Args, &Context) -> Result<()>`), register it in `src/commands/mod.rs`,
and add a variant in `src/cli.rs` and a match arm in `src/lib.rs`.

## Licenses

All dependencies are permissive (MIT, Apache-2.0, MPL-2.0, Unicode-3.0, Zlib)
and enforced by `deny.toml`. vManager itself has no license yet
(`publish = false`).
