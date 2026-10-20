//! `vmanager pin` — choose which version of each registered tool the current
//! directory uses, saved in `./.vmanagerversions`.
//!
//! Without arguments it opens an arrow-key menu (see [`crate::menu`] for its
//! rules; this module only drives the terminal). With arguments it pins or
//! clears one tool directly. Either way the shims pick the pin up at launch,
//! and `vmanager run` keeps every pinned version unpacked.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::style::{Color, ResetColor, SetForegroundColor};
use crossterm::terminal::{self, ClearType};
use crossterm::{cursor, queue};

use crate::archive;
use crate::commands::Context;
use crate::commands::run::tool_name;
use crate::commands::r#use::regenerate_shims;
use crate::db::{Database, FolderRecord};
use crate::error::{Error, Result};
use crate::menu::{self, Key, Line, Menu, Step, ToolChoice, VersionChoice};
use crate::pinfile::{self, Pins};
use crate::sweep;
use crate::version::{self, Version};

#[derive(Debug, clap::Args)]
pub struct Args {
    /// A registered folder's name, e.g. `nodejs` [default: open the menu]
    #[arg(value_name = "TOOL")]
    tool: Option<String>,

    /// The version to pin [default: show the current pin]
    //
    // Named `target` for the same reason as in `vmanager use`: a positional
    // called `version` collides with clap's own `--version`.
    #[arg(value_name = "VERSION", requires = "tool", conflicts_with = "clear")]
    target: Option<String>,

    /// Remove the tool's pin instead of setting it
    #[arg(long, requires = "tool")]
    clear: bool,
}

/// A registered folder, named the way a pin file names it.
struct Tool {
    name: String,
    path: PathBuf,
}

pub fn run(args: &Args, ctx: &Context) -> Result<()> {
    let db = ctx.open_database()?;
    let tools = registered_tools(db.folders()?)?;
    let pin_path = archive::canonical_dir(&std::env::current_dir()?)?.join(pinfile::FILE_NAME);
    let pins = pinfile::read_or_empty(&pin_path)?;

    let Some(name) = &args.tool else {
        return interactive(ctx, &db, &tools, &pin_path, pins);
    };

    if args.clear {
        let mut cleared = pins;
        if cleared.remove(name).is_none() {
            return Err(Error::NotFound {
                kind: "pin",
                name: format!("{name} in {}", pin_path.display()),
            });
        }
        save(&db, &pin_path, &cleared)?;
        if !ctx.is_quiet() {
            writeln!(
                std::io::stdout().lock(),
                "unpinned {name} in {}",
                pin_path.display()
            )?;
        }
        return Ok(());
    }

    let Some(version) = &args.target else {
        if !ctx.is_quiet() {
            let mut out = std::io::stdout().lock();
            match pins.get(name) {
                Some(version) => writeln!(out, "{name} -> {version}  ({})", pin_path.display())?,
                None => writeln!(out, "{name} is not pinned in {}", pin_path.display())?,
            }
        }
        return Ok(());
    };

    let tool = tools
        .iter()
        .find(|tool| &tool.name == name)
        .ok_or_else(|| Error::NotFound {
            kind: "registered tool",
            name: name.clone(),
        })?;
    let choice = version_choices(&tool.path)?
        .into_iter()
        .find(|choice| &choice.name == version)
        .ok_or_else(|| Error::NotFound {
            kind: "version",
            name: format!("{version} of {name}"),
        })?;

    if choice.archived {
        unpack(ctx, tool, version)?;
    }
    let mut pinned = pins;
    pinned.insert(name.clone(), version.clone());
    save(&db, &pin_path, &pinned)?;
    regenerate_shims(ctx, &tool.path)?;

    if !ctx.is_quiet() {
        writeln!(
            std::io::stdout().lock(),
            "pinned {name} -> {version}  ({})",
            pin_path.display()
        )?;
    }
    Ok(())
}

/// The menu path: let the user pick, then unpack, save and regenerate.
fn interactive(
    ctx: &Context,
    db: &Database,
    tools: &[Tool],
    pin_path: &Path,
    pins: Pins,
) -> Result<()> {
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return Err(Error::Pin(
            "the pin menu needs an interactive terminal; use `vmanager pin <TOOL> <VERSION>` instead"
                .to_string(),
        ));
    }
    if tools.is_empty() {
        if !ctx.is_quiet() {
            let mut out = std::io::stdout().lock();
            writeln!(out, "no folders registered yet")?;
            writeln!(out, "register one with `vmanager folder add <FOLDER>`")?;
        }
        return Ok(());
    }

    let choices = tools
        .iter()
        .map(|tool| {
            Ok(ToolChoice {
                name: tool.name.clone(),
                versions: version_choices(&tool.path)?,
                pinned: pins.get(&tool.name).cloned(),
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let title = format!("Pin tool versions in {}", pin_path.display());
    let colorize = ctx.globals().color.should_colorize();
    let Some(chosen) = choose(menu::open(choices), &title, colorize)? else {
        if !ctx.is_quiet() {
            writeln!(std::io::stdout().lock(), "nothing saved")?;
        }
        return Ok(());
    };

    for (tool, choice) in tools.iter().zip(&chosen) {
        let archived = choice.pinned.as_ref().filter(|pinned| {
            choice
                .versions
                .iter()
                .any(|version| &&version.name == pinned && version.archived)
        });
        if let Some(version) = archived {
            unpack(ctx, tool, version)?;
        }
    }

    let merged = merge(pins, &chosen);
    save(db, pin_path, &merged)?;
    for (tool, choice) in tools.iter().zip(&chosen) {
        if choice.pinned.is_some() {
            regenerate_shims(ctx, &tool.path)?;
        }
    }

    if !ctx.is_quiet() {
        let mut out = std::io::stdout().lock();
        for choice in &chosen {
            match &choice.pinned {
                Some(version) => writeln!(out, "pinned {} -> {version}", choice.name)?,
                None => writeln!(out, "{} not pinned", choice.name)?,
            }
        }
        writeln!(out, "saved {}", pin_path.display())?;
    }
    Ok(())
}

/// Every registered folder by name. Two folders with the same name would be
/// indistinguishable in a pin file, so that is an error naming both.
fn registered_tools(folders: Vec<FolderRecord>) -> Result<Vec<Tool>> {
    let mut tools: Vec<Tool> = Vec::new();
    for record in folders {
        let name = tool_name(&record.path).ok_or_else(|| {
            Error::Pin(format!(
                "`{}` has no folder name to pin it by",
                record.path.display()
            ))
        })?;
        if let Some(existing) = tools.iter().find(|tool| tool.name == name) {
            return Err(Error::Pin(format!(
                "`{}` and `{}` are both registered as `{name}`; pin files name tools by folder name, so rename or unregister one of them",
                existing.path.display(),
                record.path.display()
            )));
        }
        tools.push(Tool {
            name,
            path: record.path,
        });
    }
    Ok(tools)
}

/// Every version of the tool in `folder`, unpacked or archived, oldest first.
/// A version that is both (archived with `--keep-source`) counts as unpacked.
fn version_choices(folder: &Path) -> Result<Vec<VersionChoice>> {
    let mut found: Vec<(Option<Version>, VersionChoice)> = version::list(folder)?
        .versions
        .into_iter()
        .map(|unpacked| {
            (
                unpacked.version,
                VersionChoice {
                    name: unpacked.name,
                    archived: false,
                },
            )
        })
        .collect();

    for archived in version::archived(folder)? {
        if !found.iter().any(|(_, choice)| choice.name == archived.name) {
            found.push((
                archived.version,
                VersionChoice {
                    name: archived.name,
                    archived: true,
                },
            ));
        }
    }

    found.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.name.cmp(&b.1.name)));
    Ok(found.into_iter().map(|(_, choice)| choice).collect())
}

/// The pin file's existing entries with the menu's choices applied on top.
/// Entries for tools that are not registered here are kept as they were.
fn merge(pins: Pins, chosen: &[ToolChoice]) -> Pins {
    chosen.iter().fold(pins, |mut merged, choice| {
        match &choice.pinned {
            Some(version) => merged.insert(choice.name.clone(), version.clone()),
            None => merged.remove(&choice.name),
        };
        merged
    })
}

fn unpack(ctx: &Context, tool: &Tool, version: &str) -> Result<()> {
    let files = sweep::restore(&tool.path, version)?;
    if !ctx.is_quiet() {
        writeln!(
            std::io::stdout().lock(),
            "unpacked {} {version}  {files} file(s)",
            tool.name
        )?;
    }
    Ok(())
}

/// Write the pin file and remember it, so `vmanager run` protects its pins.
fn save(db: &Database, pin_path: &Path, pins: &Pins) -> Result<()> {
    pinfile::write(pin_path, pins)?;
    db.add_pin_file(pin_path)
}

/// Raw mode for as long as this lives. Dropping it restores the terminal on
/// every exit path, including errors.
struct RawMode;

impl RawMode {
    fn enable() -> Result<Self> {
        terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let mut err = std::io::stderr();
        if let Err(error) = queue!(err, cursor::Show)
            .and_then(|()| err.flush())
            .and_then(|()| terminal::disable_raw_mode())
        {
            tracing::warn!(error = %error, "could not restore the terminal");
        }
    }
}

/// Run the menu on stderr until the user saves (`Some`) or quits (`None`).
fn choose(start: Menu, title: &str, colorize: bool) -> Result<Option<Vec<ToolChoice>>> {
    let mut err = std::io::stderr();
    let _raw = RawMode::enable()?;
    queue!(err, cursor::Hide)?;

    let mut current = start;
    let mut drawn = 0;
    loop {
        drawn = draw(&mut err, title, &menu::render(&current), drawn, colorize)?;

        let key = match read_key() {
            Ok(key) => key,
            Err(error) => {
                erase(&mut err, drawn)?;
                return Err(error);
            }
        };

        match menu::press(current, key) {
            Step::Open(next) => current = next,
            Step::Save(chosen) => {
                erase(&mut err, drawn)?;
                return Ok(Some(chosen));
            }
            Step::Quit => {
                erase(&mut err, drawn)?;
                return Ok(None);
            }
        }
    }
}

/// Block until a key the menu understands is pressed. Ctrl+C is reported as
/// [`Error::Interrupted`], since raw mode delivers it as a key, not a signal.
fn read_key() -> Result<Key> {
    loop {
        // Windows reports releases as well as presses; only presses count.
        let Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) = event::read()?
        else {
            continue;
        };

        let key = match code {
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                return Err(Error::Interrupted);
            }
            KeyCode::Up | KeyCode::Char('k') => Key::Up,
            KeyCode::Down | KeyCode::Char('j') => Key::Down,
            KeyCode::Enter | KeyCode::Right => Key::Enter,
            KeyCode::Esc | KeyCode::Left | KeyCode::Backspace => Key::Back,
            KeyCode::Char('s') => Key::Save,
            KeyCode::Char('q') => Key::Quit,
            _ => continue,
        };
        return Ok(key);
    }
}

/// Redraw the menu over the `previous` lines drawn last time: move the
/// cursor back up, clear to the end of the screen, write every line again.
/// Returns how many lines are now on screen.
fn draw(
    out: &mut impl Write,
    title: &str,
    lines: &[Line],
    previous: usize,
    colorize: bool,
) -> Result<usize> {
    erase(out, previous)?;

    write!(out, "{title}\r\n")?;
    for line in lines {
        let color = if line.selected {
            Some(Color::Cyan)
        } else if line.dimmed {
            Some(Color::DarkGrey)
        } else {
            None
        };
        match color.filter(|_| colorize) {
            Some(color) => {
                queue!(out, SetForegroundColor(color))?;
                write!(out, "{}", line.text)?;
                queue!(out, ResetColor)?;
            }
            None => write!(out, "{}", line.text)?,
        }
        // Raw mode does not turn `\n` into a carriage return as well.
        write!(out, "\r\n")?;
    }
    out.flush()?;

    Ok(lines.len() + 1)
}

/// Remove the `lines` most recently drawn, leaving the cursor where they began.
fn erase(out: &mut impl Write, lines: usize) -> Result<()> {
    if lines > 0 {
        let up = u16::try_from(lines).unwrap_or(u16::MAX);
        queue!(out, cursor::MoveUp(up))?;
    }
    queue!(
        out,
        cursor::MoveToColumn(0),
        terminal::Clear(ClearType::FromCursorDown)
    )?;
    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(path: &str) -> FolderRecord {
        FolderRecord {
            id: 1,
            path: PathBuf::from(path),
            keep: None,
            added_at: String::new(),
        }
    }

    #[test]
    fn two_registered_folders_with_the_same_name_are_an_error_naming_both() {
        let err = registered_tools(vec![record("/a/nodejs"), record("/b/nodejs")])
            .err()
            .expect("duplicate names must be rejected");
        let message = err.to_string();
        assert!(
            message.contains("/a/nodejs") || message.contains("\\a\\nodejs"),
            "{message}"
        );
        assert!(message.contains("nodejs"), "{message}");
    }

    #[test]
    fn merge_applies_choices_and_keeps_pins_for_other_tools() {
        let pins = Pins::from([
            ("nodejs".to_string(), "18.20".to_string()),
            ("python".to_string(), "3.12".to_string()),
            ("unregistered".to_string(), "1.0".to_string()),
        ]);
        let chosen = [
            ToolChoice {
                name: "nodejs".to_string(),
                versions: Vec::new(),
                pinned: Some("22.16.0".to_string()),
            },
            ToolChoice {
                name: "python".to_string(),
                versions: Vec::new(),
                pinned: None,
            },
        ];

        assert_eq!(
            merge(pins, &chosen),
            Pins::from([
                ("nodejs".to_string(), "22.16.0".to_string()),
                ("unregistered".to_string(), "1.0".to_string()),
            ])
        );
    }

    #[test]
    fn version_choices_list_unpacked_and_archived_oldest_first() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("4.10")).unwrap();
        std::fs::create_dir(dir.path().join("4.2")).unwrap();
        std::fs::write(dir.path().join("4.1.vmversion"), b"").unwrap();

        let choices = version_choices(dir.path()).unwrap();
        let names: Vec<(&str, bool)> = choices
            .iter()
            .map(|choice| (choice.name.as_str(), choice.archived))
            .collect();
        assert_eq!(names, [("4.1", true), ("4.2", false), ("4.10", false)]);
    }
}
