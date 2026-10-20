# vManager



Keeps the disk from filling up with old versions of your tools.

Point vManager at a folder that holds one subfolder per version, and once a day
it compresses everything except the newest few into 7z archives named
`<version>.vmversion` and deletes the folders it compressed. The old versions stay recoverable; the space they
were using does not.

```
C:\Tools\Blender\          before            C:\Tools\Blender\        after
  4.1\   1.9 GiB                               4.1.vmversion  412 MiB
  4.2\   2.0 GiB                               4.2.vmversion  430 MiB
  4.3\   2.1 GiB                               4.3\           2.1 GiB
  4.10\  2.2 GiB                               4.10\          2.2 GiB
```

Written in Rust, with a local SQLite database recording every folder it watches
and every archive it has made.

## Building

```sh
cargo build --release        # binaries at target/release/vmanager and vman
cargo run -- --help          # run it straight from the source tree
cargo install --path .       # install into ~/.cargo/bin
```

Requires Rust 1.93 or newer (edition 2024). SQLite is compiled in from source,
so there is nothing to install first — and no `7z` binary is needed either;
compression is pure Rust.

## Getting started

```sh
vmanager folder add C:\Tools\Blender     # watch this tool's versions
vmanager folder add C:\Tools\Godot --keep 3
vmanager list                            # see what you have
vmanager run --dry-run                   # see what would be archived
vmanager run                             # do it
vmanager use C:\Tools\Blender 4.3        # point `current` at a version
vmanager hook set C:\Tools\Blender launch C:\hooks\log.ps1
vmanager pin                             # pick versions for this project
vmanager schedule install                # …and every day at 03:00
vmanager completions powershell          # complete commands as you type them
```

## Usage

```
vmanager [OPTIONS] <COMMAND>
```

### Commands

| Command | Description |
| --- | --- |
| `run` | Sweep every registered folder: archive all but the newest versions |
| `list` | Show every version of every registered folder |
| `folder add <FOLDER>` | Register a folder whose versions should be managed |
| `folder remove <FOLDER>` | Unregister a folder; nothing on disk is touched |
| `use <FOLDER> <VERSION>` | Point `<FOLDER>\current` at a version |
| `use <FOLDER>` | Show which version `<FOLDER>\current` points at |
| `use <FOLDER> --clear` | Remove the `current` link |
| `shim list [--folder <FOLDER>]` | Show every shim, its target exe and its hook |
| `shim regenerate [--folder <FOLDER>]` | Re-scan and rewrite a folder's shims |
| `hook set <FOLDER> <COMMAND> <SCRIPT>` | Run `<SCRIPT>` before `<COMMAND>` launches |
| `hook remove <FOLDER> <COMMAND>` | Remove a configured hook |
| `hook list [--folder <FOLDER>]` | Show configured hooks |
| `pin` | Pick tool versions for the current directory in a menu |
| `pin <TOOL> <VERSION>` | Pin one tool's version for the current directory |
| `pin <TOOL>` | Show the tool's pin in the current directory |
| `pin <TOOL> --clear` | Remove the tool's pin |
| `schedule install` | Register the daily run with Windows Task Scheduler |
| `schedule uninstall` | Remove the scheduled task |
| `schedule status` | Show the scheduled task |
| `completions [SHELL]` | Print the shell completion script |

### Global options

These are accepted before or after the subcommand.

| Option | Description |
| --- | --- |
| `-v`, `--verbose` | Increase logging verbosity; repeat for more (`-vv`, `-vvv`) |
| `-q`, `--quiet` | Silence everything except errors |
| `-c`, `--config <FILE>` | Use a specific configuration file |
| `--color <auto\|always\|never>` | Control colored output |
| `-h`, `--help` / `-V`, `--version` | Help and version |

### Environment

| Variable | Effect |
| --- | --- |
| `VMANAGER_CONFIG` | Path to the configuration file (`--config` wins) |
| `NO_COLOR` | Disables color when `--color` is left at `auto` |
| `RUST_LOG` | Fine-grained log filter, e.g. `vmanager=debug`; overrides `-v` |

### Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Success |
| `1` | Failure |
| `2` | Invalid arguments |
| `3` | Configuration error |
| `4` | Not found |
| `5` | Database error |
| `6` | Archive error |
| `130` | Interrupted |

## Managing versions

A **registered folder** is the parent of one tool's versions, not a version
itself. Register `C:\Tools\Blender`, the folder that contains `4.1`, `4.2` and
`4.3` — not `C:\Tools\Blender\4.3`.

```sh
vmanager folder add C:\Tools\Blender          # keep_versions applies (2)
vmanager folder add C:\Tools\Godot --keep 3   # …unless overridden per folder
vmanager folder remove C:\Tools\Godot
```

### What you have

`vmanager list` shows every version of every registered folder, oldest first,
whether it is still unpacked or has already been swept into a `.vmversion`:

```
$ vmanager list
C:\Tools\Blender  keep 2, 2 unpacked, 2 archived
  4.1   archived  412.4 MiB  from 1.9 GiB in 842 file(s)
  4.2   archived  430.1 MiB  from 2.0 GiB in 851 file(s)
  4.3   unpacked
  4.10  unpacked
C:\Tools\Godot  keep 3, 3 unpacked, 0 archived
  3.5   unpacked
  4.0   unpacked
  4.1   unpacked
```

The size on the left is the archive as it sits on disk; `from …` is what it
held before compression, which is what restoring it would cost you. That part
comes from the database — an archive with no record still appears, just marked
`(not in the database)`, because the file is what matters.

```sh
vmanager list --archived              # only the compressed versions
vmanager list --unpacked              # only the ones still taking up room
vmanager list --folder C:\Tools\Godot # one folder; repeatable
```

The `unpacked`/`archived` counts in the header describe the folder itself, so
they stay put when a filter narrows the lines below them.

### The sweep

`vmanager run` visits each registered folder, keeps the newest few versions
unpacked and compresses the rest:

```
$ vmanager run
C:\Tools\Blender  4 version(s), keeping 2
  archived 4.1 -> 4.1.vmversion  842 files, 1.9 GiB -> 412.4 MiB, freed 1.5 GiB
  archived 4.2 -> 4.2.vmversion  851 files, 2.0 GiB -> 430.1 MiB, freed 1.6 GiB
C:\Tools\Godot  3 version(s), keeping 3
  nothing to archive

2 archived, 0 skipped, 0 failed, 3.1 GiB freed
```

Each archive is written *next to* the folder it replaces, so `4.1\` becomes
`4.1.vmversion` in the same place. The file is an ordinary 7z container under
its own extension: `vmanager pin` unpacks one for you, and 7-Zip opens it too
(it recognizes the format by content, not by name). The folder name is stored
inside the archive, so it comes back as `4.1\`.

Archives written before the `.vmversion` extension are named `<version>.7z`.
`vmanager run` renames every `.7z` directly inside a swept folder to
`.vmversion` (not under `--dry-run`) and updates its database record; if the
new name is already taken, that folder fails with an error instead.

Useful flags:

```sh
vmanager run --dry-run              # report only; nothing is written or deleted
vmanager run --keep 3               # override the keep count for this run
vmanager run --folder C:\Tools\Godot  # only this folder; repeatable
vmanager run --keep-source          # write the archives but keep the folders
vmanager run --force                # replace an archive that already exists
vmanager run --level 9 --threads 8  # compression settings for this run
```

`--folder` also accepts a folder that was never registered, which is a good way
to try the sweep on something before committing to it.

### Which version is "oldest"

Ordering is by the numbers in the folder name. Every run of digits is one
component, compared numerically, so `4.9` is older than `4.10` — the answer a
string sort gets wrong. `v4.2`, `4_2`, `Blender-4.2` and `2026-08-04` all
parse.

A folder whose name contains no digits at all is *not* a version. `current\`
(vManager's own link, see [below](#selecting-a-version)) and `shim\` (its
generated [command shims](#command-shims)) are never counted or archived,
full stop — even in a registered folder where nothing else is numbered
either. Any other undigited name, like a `.cache\` a tool drops next to its
versions, is excluded the same way whenever at least one sibling *is*
numbered. A folder where *nothing* is numbered falls back to modification
time and treats every remaining subfolder as a version, so tools versioned by
date-of-copy still work.

Symlinks and junctions are skipped, files are ignored, and the `.vmversion` files from
previous runs are not mistaken for versions — so a daily sweep settles instead
of eating one version per day.

### What protects the old versions

The delete is what frees the disk, so it happens last and only after the
archive has been proven good:

1. the archive is written to a fresh `.vmversion`, and deleted again if compression
   fails — a `.vmversion` on disk is never half-written. That includes the compressor
   crashing outright: it runs in-process, so a panic in it is caught, reported
   as that one folder failing, and the partial archive removed;
2. it is read back and its file count compared against what was walked;
3. only then is the version folder removed.

A failure at any earlier point leaves the version folder exactly where it was.
If an archive of that name already exists, that version is skipped with a
message rather than overwritten, and nothing is deleted — pass `--force` when
you do want it replaced. Use `--keep-source` for a first run if you would
rather inspect the archives before trusting the deletion.

One folder failing does not stop the others: an unplugged drive is reported,
the remaining folders are still swept, and the run ends with a non-zero exit
code so a failed scheduled run is visible in Task Scheduler's history.

A version a project has [pinned](#pinning-versions-per-project) is never
archived, however old it gets:

```
$ vmanager run
C:\Tools\node  3 version(s), keeping 2
  keeping 18.20 (pinned by C:\dev\legacy\.vmanagerversions)
  nothing to archive

0 archived, 0 skipped, 1 protected, 0 failed, 0 B freed
```

Every pin file `vmanager pin` has written is remembered in the database and
read again at the start of each run, before any folder is touched. One that
has since been deleted is forgotten with a warning (not under `--dry-run`);
one that exists but cannot be parsed stops the whole run, since nobody can
tell what it was protecting. Protection only ever applies to a version that
would otherwise be archived.

### Selecting a version

`vmanager use` points a registered folder's `current` at one of its versions —
a directory junction on Windows, a symlink everywhere else — so a fixed path
put on `PATH` once always resolves to whichever version was selected last:

```sh
vmanager use C:\Tools\Node 20.11    # C:\Tools\Node\current -> C:\Tools\Node\20.11
vmanager use C:\Tools\Node          # show what current points at
vmanager use C:\Tools\Node --clear  # remove the link
```

Put `C:\Tools\Node\current` on `PATH` and every later `vmanager use` retargets
what that path resolves to. That is the whole point of a link rather than
editing `PATH` itself: a new process sees the change immediately, without a
fresh shell to pick up an updated environment variable.

A junction rather than a Windows symlink: a symlink to a directory needs
Developer Mode or an elevated prompt, a junction needs neither. `current` is
never itself treated as a version — see [above](#which-version-is-oldest) — so
it is never archived, and pointing it at something that does not exist is
refused rather than left dangling. Once a version it points at *has* been
archived, `vmanager list` and `vmanager use <FOLDER>` still report the name it
was set to, so a stale link is visible rather than silent.

pManager pins a project to a version this way — see its own README for the
`pmanager project set-version` / `pmanager use` half of this.

### Command shims

Every registered folder gets its own `shim\` subfolder, sitting next to its
version folders and `current`:

```
C:\Tools\nodejs\
  22.16.0\
  22.23.2\
  24.18.0\
  current -> 24.18.0
  shim\
    node.ps1  node.cmd
    npm.ps1   npm.cmd
    npx.ps1   npx.cmd
```

`shim\` is created empty the moment `vmanager folder add` runs. It is
populated the first time `vmanager use` or `vmanager run` has something to
scan — one `<command>.ps1` per `.exe`, `.ps1`, `.bat`, `.sh` or `.zsh` found
under `current` (or, lacking that, the newest unpacked version), recursing
into subfolders like `bin\`. `.exe`, `.ps1` and `.bat` targets are launched
directly; `.sh` and `.zsh` targets are handed to `bash`/`zsh` respectively,
which must be on `PATH` at run time (WSL, Git for Windows and Cygwin all
provide one). Put `C:\Tools\nodejs\shim` on `PATH` once, and every later
`vmanager use nodejs <version>` changes what `node`, `npm` and `npx` run
without touching `PATH` again — nothing here modifies `PATH` for you.

Each `<command>.ps1` gets a `<command>.cmd` beside it for callers that find
commands through `PATHEXT` and cannot run a PowerShell script themselves —
`cmd.exe`, IDEs, build tools spawning `node`. The launcher only starts
`powershell.exe -ExecutionPolicy Bypass -File <command>.ps1`, so pins and
hooks behave identically, at the cost of a PowerShell startup per call. It is
written only next to a `.ps1` vmanager generated, and a hand-written `.cmd` of
the same name is left alone.

That last part is the reason a version switch does not rewrite the shims: a
`.ps1` resolves its target through `<folder>\current\...` *at run time*, not
a path baked in for one version, so retargeting the `current` junction is
enough on its own. Regeneration — rewriting which `.ps1` files exist and
what is in them — only happens when the *set* of target names could have
changed: after `vmanager use`, after `vmanager run` (in case the version
`current` pointed at was itself archived), or on demand:

```sh
vmanager shim list                       # every shim, its target, its hook
vmanager shim list --folder C:\Tools\nodejs
vmanager shim regenerate                 # manual re-scan/rewrite, for recovery
```

Regeneration diffs against what is already there rather than clobbering it:
a script you dropped into `shim\` by hand, even one whose name collides with
a discovered command, is left alone — only files vManager itself generated
(marked as such in their header) are rewritten or removed. Two targets
under the same active version that would produce the same command name
(e.g. `install.exe` next to `install.ps1`) is reported as an error naming
both paths, rather than silently picking one.

#### Pre-hooks

A shim can run a script before its command — logging, environment setup,
whatever — and abort loudly, without launching the real target, if that script
exits non-zero:

```sh
vmanager hook set C:\Tools\nodejs node C:\hooks\node-setup.ps1
vmanager hook remove C:\Tools\nodejs node
vmanager hook list
vmanager hook list --folder C:\Tools\nodejs
```

Hooks are keyed by `(folder, command)`, not by command name alone, since the
same command (`node`) can exist under two different registered folders with
two different hooks. Setting one before its shim exists is fine — it takes
effect the moment discovery creates the shim; setting or clearing one on a
shim that already exists patches that single `.ps1` immediately, so a hook
change is never silently stale until the next `use` or `run`.

### Pinning versions per project

`current` is one choice for the whole machine. A project that needs a
different version pins it in a `.vmanagerversions` file in its own directory:

```sh
cd C:\dev\legacy
vmanager pin                    # arrow-key menu: tool, then version
vmanager pin node 18.20         # …or directly
vmanager pin node               # show the pin
vmanager pin node --clear       # remove it
```

```toml
# .vmanagerversions
"node" = "18.20"
```

A tool is named by its registered folder's name (`node` for `C:\Tools\node`),
not its path, so the file can be committed and still works on a machine with
the tools installed elsewhere. Two registered folders with the same name are
an error, since a pin file could not tell them apart.

The menu lists every registered tool with its pin; enter opens a tool's
versions, oldest first, with archived ones marked. ↑/↓ (or `j`/`k`) move,
enter picks, esc goes back, `s` saves, `q` quits without saving. Picking
`(not pinned)` removes the tool's pin, and entries for tools that are not
registered on this machine are kept as they were. It draws on stderr and
needs an interactive terminal; scripts use the direct form.

Pinning an archived version unpacks it next to its `.vmversion` and, once the file
count matches the archive, deletes the `.vmversion`. Pinning also regenerates the
tool's shims, so shims written before pinning existed pick it up.

Every shim looks for the pin at launch: starting in the working directory and
walking up, the first `.vmanagerversions` that pins its tool decides which
version runs; none means `current`. A pinned version that is not unpacked,
that lacks the command, or whose line in the file the shim cannot read aborts
with an error rather than quietly running `current`. The set of shims still comes from `current`, so a command that
only an older pinned version has gets no shim.

## Scheduling the daily run

vManager has no resident daemon. `schedule install` hands `vmanager run` to
Windows Task Scheduler, which already knows how to run something daily, survive
reboots and record the outcome.

```sh
vmanager schedule install                 # daily at 03:00
vmanager schedule install --at 21:30      # …or whenever suits
vmanager schedule install --force         # replace an existing task
vmanager schedule status
vmanager schedule uninstall
```

The task is named `vManager daily sweep` (`--name` changes it) and runs the
executable at its current path, so move the binary and you will want to install
it again. An explicit `--config` is carried into the task.

On non-Windows platforms these commands report that Task Scheduler is not
available; run `vmanager run` from cron or a systemd timer instead.

## Shell completions

`vmanager completions <SHELL>` writes a completion script to stdout. Put it
where your shell looks for one:

```powershell
vmanager completions powershell >> $PROFILE
```

```sh
vmanager completions bash > ~/.local/share/bash-completion/completions/vmanager
vmanager completions zsh  > ~/.zfunc/_vmanager     # ~/.zfunc on $fpath
vmanager completions fish > ~/.config/fish/completions/vmanager.fish
vmanager completions elvish >> ~/.config/elvish/rc.elv
```

Then start a new shell: these files are read once, at startup. To try it
without installing anything, `eval "$(vmanager completions bash)"`.

Omit the shell and it is guessed from the environment — handy in a profile
that is shared between machines, though `$SHELL` is the login shell rather
than the current one and is not set at all under PowerShell, so naming the
shell is the reliable form.

The script is generated from the same command line definition the binary
parses with, so it always covers every current subcommand, flag and value. It
is a snapshot of one version, though: regenerate it after upgrading vmanager.

Completions never read the configuration file or the database, so a broken
config cannot break the shell that runs this at startup.

## Database

A single SQLite file, by default `vmanager.db` inside the platform data
directory (set `data_dir` or `database` to move it). It is created and migrated
automatically on first use — there is nothing to initialize. SQLite is linked
statically via `rusqlite`'s bundled build, so there is no system package to
install and no version skew between machines.

The schema is versioned with SQLite's own `user_version` pragma and migrated on
open: each entry in `MIGRATIONS` (`src/db.rs`) runs inside a transaction, so an
interrupted upgrade leaves the database on its previous version rather than
half-migrated. A database written by a *newer* vmanager is refused rather than
downgraded. To add a table or column, append a migration — never edit one that
has shipped.

Connections open with WAL journaling, `foreign_keys = ON` and a 5 second busy
timeout, so a second `vmanager` waits its turn instead of failing.

Four tables: `folders` is the registry `vmanager run` walks; `archives`
records every `.vmversion` written, pointing back at the folder it came from; `hooks`
holds one row per `(folder, command)` pre-hook `vmanager hook set` has
configured; `pin_files` lists every `.vmanagerversions` `vmanager pin` has
written, so `vmanager run` knows which ones to read. The archive records are what let `vmanager list` report a
version's original size — the one thing a `.vmversion` cannot tell you by looking at
it. Unregistering a folder clears the archives' back-reference (`ON DELETE SET
NULL`) rather than discarding the records — the files are still on disk, so
their history should not vanish with the registration; `hooks` rows are keyed
by the folder's path rather than a foreign key to `folders` at all, so a hook
can be configured (and a shim built) for a folder that was never registered,
same as `vmanager use` and `vmanager run --folder` already allow.

The database is a record, not the source of truth: the archives and version
folders on disk are. Deleting the database loses the history, not the versions.

## Configuration

Configuration is optional — every key has a default. To change one, write a
`config.toml` by hand in the platform configuration directory:

| Platform | Path |
| --- | --- |
| Linux | `~/.config/vmanager/config.toml` |
| macOS | `~/Library/Application Support/vmanager/config.toml` |
| Windows | `%APPDATA%\vmanager\config.toml` |

```toml
keep_versions = 3
compression_level = 9
```

Values are layered, lowest precedence first: built-in defaults, then the
configuration file, then environment variables, then command line flags. A
missing file is fine — the defaults apply — but an unreadable or malformed one
is an error. Unknown keys are rejected so typos surface immediately.

| Key | Default | Meaning |
| --- | --- | --- |
| `data_dir` | platform data directory | Where vManager keeps its data |
| `database` | `vmanager.db` in `data_dir` | SQLite database file |
| `keep_versions` | `2` | Versions left unpacked per folder |
| `compression_level` | `6` | LZMA2 level used by `run`, 0–9 |
| `compression_threads` | `1` | Compression threads used by `run` |

The keep count is resolved per folder, highest precedence first: `run --keep`,
then the folder's own `--keep` from `folder add`, then `keep_versions`. A
`keep_versions` of `0` is rejected rather than obeyed — it would archive every
copy of a tool, which is never what the key is meant to say.

## Layout

```
src/
  main.rs             `vmanager` entry point; hands over to `vmanager::main`
  bin/vman.rs         `vman`, the same program under a shorter name
  lib.rs              library crate; dispatches a parsed command
  cli.rs              clap definitions: global flags and the subcommand list
  version.rs          finding version folders and ordering them oldest first
  sweep.rs            the keep-newest-N policy: plan, archive, verify, delete
  archive.rs          7z creation: walking, filtering, compression
  link.rs             the `current` junction/symlink `use` points and reads
  shim.rs             discovering shim targets, writing shims, hook patching — filesystem-only
  pinfile.rs          reading and writing `.vmanagerversions`
  menu.rs             the `pin` menu as a pure state machine: keys in, lines out
  db.rs               SQLite connection, migrations and queries
  config.rs           config file discovery, parsing and defaults
  error.rs            error type and its exit-code mapping
  logging.rs          tracing subscriber setup (stderr)
  commands/
    mod.rs            shared Context handed to every subcommand
    run.rs            `vmanager run`
    list.rs           `vmanager list`
    folder.rs         `vmanager folder`
    use.rs            `vmanager use`
    shim.rs           `vmanager shim`
    hook.rs           `vmanager hook`
    pin.rs            `vmanager pin`, including the raw-mode terminal driver
    schedule.rs       `vmanager schedule`
    completions.rs    `vmanager completions`
tests/
  cli.rs              end-to-end tests that run the built binary
```

`version.rs`, `sweep.rs`, `shim.rs` and `menu.rs` hold policy and stay free of
I/O they do not need: no
database, no printing, no clap. That is what lets the ordering rules, the
archive-then-delete sequence and shim discovery/regeneration be tested
directly, and it keeps `commands/run.rs` and `commands/shim.rs` down to
gathering settings, looping and reporting. `commands/hook.rs` is the thin CLI
layer over the `hooks` table the same way `commands/folder.rs` is over
`folders` — the database query lives in `db.rs`, not there.

`main.rs` and `bin/vman.rs` are deliberately thin: the logic lives in the
library so tests and any future front-end can drive it without a process
boundary.

### Adding a command

1. Create `src/commands/<name>.rs` with a `#[derive(clap::Args)]` struct and a
   `run(&Args, &Context) -> Result<()>` function.
2. Register the module in `src/commands/mod.rs`.
3. Add a variant to `Command` in `src/cli.rs` and a match arm in `run` in
   `src/lib.rs`.

Write output to stdout and diagnostics to stderr via `tracing`, so that piping
stdout stays useful.

## Development

```sh
cargo test              # unit + end-to-end tests
cargo clippy --all-targets
cargo fmt --check
cargo deny check        # licenses, advisories, sources (needs cargo-deny)
```

CI checks formatting, runs clippy over all targets and builds the release
binary on Windows; it also checks dependency licences and advisories. It does
**not** run the test suite — `cargo test` is a local step. Because clippy uses
`--all-targets` the tests are still compiled and type-checked on every push, so
they cannot silently rot; they just are not executed.

## Licensing of dependencies

Every dependency can be used commercially, including in closed-source products.
The tree resolves to MIT, Apache-2.0, MPL-2.0, Unicode-3.0 and Zlib; no crate
*requires* GPL, LGPL or AGPL terms. (A few are multi-licensed and merely
*offer* a copyleft option alongside a permissive one — `r-efi`, for instance,
is "MIT OR Apache-2.0 OR LGPL-2.1-or-later", and taking MIT is enough.)

Two things are worth knowing:

- **SQLite** itself is in the public domain, and `sevenz-rust2` is a pure-Rust
  implementation of the 7z format under Apache-2.0 — so neither the database
  nor the archiver drags in a copyleft or patent-encumbered component.
- **`option-ext`** (a transitive dependency of `directories`) is MPL-2.0. That
  is file-level copyleft: the obligation is to publish changes to *that crate's
  own files*. Using it unmodified, as here, carries no source-disclosure
  requirement for this project.

The practical obligation across the tree is the usual one — keep the copyright
and license notices when distributing binaries.

`deny.toml` encodes this as an allowlist and CI runs `cargo deny check`, so a
dependency arriving under a license outside that set fails the build instead of
slipping in unnoticed. The list holds only the licenses actually in use, so a
new one is a deliberate decision rather than something pre-approved. Dropping
MPL-2.0 from it means replacing `directories`.

vManager itself has no license declared yet — that is a call for the repository
owner. Until one is chosen the crate is marked `publish = false` and cargo-deny
skips it (`private.ignore`); setting `license` in `Cargo.toml` afterwards lets
that setting go away.
