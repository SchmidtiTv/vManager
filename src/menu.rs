//! The `vmanager pin` menu, as a pure state machine.
//!
//! Two screens: the registered tools, and the versions of the one opened.
//! [`press`] maps a menu and a key to the next step, and [`render`] maps a
//! menu to the lines to draw. Neither touches the terminal — raw mode,
//! reading keys and redrawing live in `commands::pin` — so every navigation
//! rule is testable without one. This mirrors how [`crate::sweep`] and
//! [`crate::shim`] keep policy away from I/O.

/// One version a tool can be pinned to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionChoice {
    pub name: String,
    /// Only a `.vmversion` on disk; picking it unpacks it when the pins are saved.
    pub archived: bool,
}

/// One registered tool and the version it is pinned to, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolChoice {
    /// The registered folder's name, the key in `.vmanagerversions`.
    pub name: String,
    /// Oldest first, the same order `vmanager list` uses.
    pub versions: Vec<VersionChoice>,
    pub pinned: Option<String>,
}

/// Which screen is showing, and where its cursor is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// The tool list; the row after the last tool is "save and exit".
    Tools { cursor: usize },
    /// One tool's versions; the row after the last version is "not pinned".
    Versions { tool: usize, cursor: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Menu {
    pub tools: Vec<ToolChoice>,
    pub screen: Screen,
}

/// The keys the menu understands; the terminal driver maps real keys onto
/// these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Enter,
    Back,
    Save,
    Quit,
}

/// What a key press leads to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Keep the menu open in this state.
    Open(Menu),
    /// Write these choices to the pin file.
    Save(Vec<ToolChoice>),
    /// Leave without writing anything.
    Quit,
}

/// One line of the drawn menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    /// Under the cursor; drawn highlighted.
    pub selected: bool,
    /// Secondary text (archived versions, key hints); drawn dimmed.
    pub dimmed: bool,
}

/// A menu opened on the tool list.
pub fn open(tools: Vec<ToolChoice>) -> Menu {
    Menu {
        tools,
        screen: Screen::Tools { cursor: 0 },
    }
}

/// The step that follows pressing `key` in `menu`.
pub fn press(menu: Menu, key: Key) -> Step {
    if key == Key::Quit {
        return Step::Quit;
    }

    match menu.screen {
        Screen::Tools { cursor } => {
            let rows = menu.tools.len() + 1;
            let on_save = cursor == menu.tools.len();
            match key {
                Key::Up => Step::Open(Menu {
                    screen: Screen::Tools {
                        cursor: up(cursor, rows),
                    },
                    ..menu
                }),
                Key::Down => Step::Open(Menu {
                    screen: Screen::Tools {
                        cursor: down(cursor, rows),
                    },
                    ..menu
                }),
                Key::Save => Step::Save(menu.tools),
                Key::Enter if on_save => Step::Save(menu.tools),
                Key::Enter => {
                    let screen = Screen::Versions {
                        tool: cursor,
                        cursor: initial_version_cursor(&menu.tools[cursor]),
                    };
                    Step::Open(Menu { screen, ..menu })
                }
                Key::Back | Key::Quit => Step::Quit,
            }
        }
        Screen::Versions { tool, cursor } => {
            let rows = menu.tools[tool].versions.len() + 1;
            let back = Screen::Tools { cursor: tool };
            match key {
                Key::Up => Step::Open(Menu {
                    screen: Screen::Versions {
                        tool,
                        cursor: up(cursor, rows),
                    },
                    ..menu
                }),
                Key::Down => Step::Open(Menu {
                    screen: Screen::Versions {
                        tool,
                        cursor: down(cursor, rows),
                    },
                    ..menu
                }),
                Key::Enter => {
                    let pinned = menu.tools[tool]
                        .versions
                        .get(cursor)
                        .map(|version| version.name.clone());
                    let tools = menu
                        .tools
                        .into_iter()
                        .enumerate()
                        .map(|(index, choice)| {
                            if index == tool {
                                ToolChoice {
                                    pinned: pinned.clone(),
                                    ..choice
                                }
                            } else {
                                choice
                            }
                        })
                        .collect();
                    Step::Open(Menu {
                        tools,
                        screen: back,
                    })
                }
                Key::Back => Step::Open(Menu {
                    screen: back,
                    ..menu
                }),
                // Saving is done from the tool list, where every pin is visible.
                Key::Save => Step::Open(menu),
                Key::Quit => Step::Quit,
            }
        }
    }
}

/// The lines that draw `menu`, top to bottom.
pub fn render(menu: &Menu) -> Vec<Line> {
    match menu.screen {
        Screen::Tools { cursor } => {
            let width = name_width(menu.tools.iter().map(|tool| tool.name.as_str()));
            let mut lines: Vec<Line> = menu
                .tools
                .iter()
                .enumerate()
                .map(|(index, tool)| {
                    let pinned = tool.pinned.as_deref().unwrap_or("(not pinned)");
                    row(
                        format!("{:<width$}  {pinned}", tool.name),
                        index == cursor,
                        false,
                    )
                })
                .collect();
            lines.push(row(
                "Save and exit".to_string(),
                cursor == menu.tools.len(),
                false,
            ));
            lines.push(hint("↑/↓ move · enter open · s save · q quit"));
            lines
        }
        Screen::Versions { tool, cursor } => {
            let choice = &menu.tools[tool];
            let width = name_width(choice.versions.iter().map(|version| version.name.as_str()));
            let mut lines = vec![row(format!("{}:", choice.name), false, false)];
            lines.extend(choice.versions.iter().enumerate().map(|(index, version)| {
                let marker = if choice.pinned.as_deref() == Some(version.name.as_str()) {
                    "  (pinned)"
                } else {
                    ""
                };
                let state = if version.archived {
                    "  archived, unpacked on save"
                } else {
                    ""
                };
                row(
                    format!("{:<width$}{state}{marker}", version.name),
                    index == cursor,
                    version.archived,
                )
            }));
            lines.push(row(
                "(not pinned)".to_string(),
                cursor == choice.versions.len(),
                false,
            ));
            lines.push(hint("↑/↓ move · enter pick · esc back · q quit"));
            lines
        }
    }
}

/// The pinned version's row, or the newest version when nothing is pinned.
fn initial_version_cursor(tool: &ToolChoice) -> usize {
    tool.pinned
        .as_deref()
        .and_then(|pinned| {
            tool.versions
                .iter()
                .position(|version| version.name == pinned)
        })
        .unwrap_or(tool.versions.len().saturating_sub(1))
}

fn up(cursor: usize, rows: usize) -> usize {
    (cursor + rows - 1) % rows
}

fn down(cursor: usize, rows: usize) -> usize {
    (cursor + 1) % rows
}

fn name_width<'a>(names: impl Iterator<Item = &'a str>) -> usize {
    names.map(|name| name.chars().count()).max().unwrap_or(0)
}

fn row(text: String, selected: bool, dimmed: bool) -> Line {
    let prefix = if selected { "> " } else { "  " };
    Line {
        text: format!("{prefix}{text}"),
        selected,
        dimmed,
    }
}

fn hint(text: &str) -> Line {
    Line {
        text: text.to_string(),
        selected: false,
        dimmed: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str, versions: &[(&str, bool)], pinned: Option<&str>) -> ToolChoice {
        ToolChoice {
            name: name.to_string(),
            versions: versions
                .iter()
                .map(|(name, archived)| VersionChoice {
                    name: name.to_string(),
                    archived: *archived,
                })
                .collect(),
            pinned: pinned.map(str::to_string),
        }
    }

    fn sample() -> Menu {
        open(vec![
            tool("nodejs", &[("18.20", true), ("22.16.0", false)], None),
            tool("blender", &[("4.2", false)], Some("4.2")),
        ])
    }

    /// Press every key in turn, expecting the menu to stay open throughout.
    fn presses(menu: Menu, keys: &[Key]) -> Menu {
        keys.iter().fold(menu, |menu, key| match press(menu, *key) {
            Step::Open(next) => next,
            other => panic!("expected the menu to stay open after {key:?}, got {other:?}"),
        })
    }

    #[test]
    fn picking_a_version_pins_it_and_returns_to_the_tool_list() {
        let menu = presses(sample(), &[Key::Enter, Key::Up, Key::Enter]);

        assert_eq!(menu.screen, Screen::Tools { cursor: 0 });
        assert_eq!(menu.tools[0].pinned.as_deref(), Some("18.20"));
        assert_eq!(menu.tools[1].pinned.as_deref(), Some("4.2"));
    }

    #[test]
    fn opening_a_tool_starts_on_its_pin_or_its_newest_version() {
        let unpinned = presses(sample(), &[Key::Enter]);
        assert_eq!(unpinned.screen, Screen::Versions { tool: 0, cursor: 1 });

        let pinned = presses(sample(), &[Key::Down, Key::Enter]);
        assert_eq!(pinned.screen, Screen::Versions { tool: 1, cursor: 0 });
    }

    #[test]
    fn the_row_after_the_versions_clears_the_pin() {
        let menu = presses(sample(), &[Key::Down, Key::Enter, Key::Down, Key::Enter]);
        assert_eq!(menu.tools[1].pinned, None);
    }

    #[test]
    fn back_leaves_the_pin_alone() {
        let menu = presses(sample(), &[Key::Down, Key::Enter, Key::Down, Key::Back]);
        assert_eq!(menu.screen, Screen::Tools { cursor: 1 });
        assert_eq!(menu.tools[1].pinned.as_deref(), Some("4.2"));
    }

    #[test]
    fn the_cursor_wraps_around() {
        // Two tools plus "save and exit".
        assert_eq!(
            presses(sample(), &[Key::Up]).screen,
            Screen::Tools { cursor: 2 }
        );
        assert_eq!(
            presses(sample(), &[Key::Down, Key::Down, Key::Down]).screen,
            Screen::Tools { cursor: 0 }
        );
    }

    #[test]
    fn save_returns_the_choices_from_the_key_or_the_last_row() {
        let menu = presses(sample(), &[Key::Enter, Key::Enter]);
        let expected = menu.tools.clone();

        assert_eq!(press(menu.clone(), Key::Save), Step::Save(expected.clone()));
        let on_save_row = presses(menu, &[Key::Up]);
        assert_eq!(press(on_save_row, Key::Enter), Step::Save(expected));
    }

    #[test]
    fn save_is_ignored_inside_a_tool_and_quit_works_everywhere() {
        let inside = presses(sample(), &[Key::Enter]);
        assert_eq!(press(inside.clone(), Key::Save), Step::Open(inside.clone()));
        assert_eq!(press(inside, Key::Quit), Step::Quit);
        assert_eq!(press(sample(), Key::Back), Step::Quit);
    }

    #[test]
    fn render_marks_the_cursor_pins_and_archived_versions() {
        let tools = render(&sample());
        assert_eq!(tools[0].text, "> nodejs   (not pinned)");
        assert!(tools[0].selected);
        assert_eq!(tools[1].text, "  blender  4.2");
        assert_eq!(tools[2].text, "  Save and exit");
        assert!(tools[3].dimmed);

        let versions = render(&presses(sample(), &[Key::Enter]));
        assert_eq!(versions[0].text, "  nodejs:");
        assert_eq!(versions[1].text, "  18.20    archived, unpacked on save");
        assert!(versions[1].dimmed);
        assert_eq!(versions[2].text, "> 22.16.0");
        assert_eq!(versions[3].text, "  (not pinned)");
    }
}
