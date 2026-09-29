use std::collections::HashSet;
use std::fmt;

use anyhow::{Context, Result};
use serde_json::Value;

use super::cli::{Cli, owned};
use super::detect::MultiplexerKind;
use super::probe::{Anchor, Multiplexer, Pane};
use super::process::{PaneTags, ProcessTable, pane_roots};

const PROGRAM: &str = "zellij";
const SERVERS: &[&str] = &["zellij"];
const MINIMUM: Version = Version(0, 44, 0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u32, pub u32, pub u32);

impl fmt::Display for Version {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.0, self.1, self.2)
    }
}

fn parse_version(output: &str) -> Option<Version> {
    let number = output.split_whitespace().nth(1)?;
    let mut parts = number.split('.').map(|part| {
        part.chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse::<u32>()
            .ok()
    });
    let major = parts.next()??;
    let minor = parts.next()??;
    let patch = parts.next().flatten().unwrap_or(0);
    Some(Version(major, minor, patch))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unavailable {
    Missing,
    Outdated(Option<Version>),
}

impl fmt::Display for Unavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => write!(formatter, "zellij not found"),
            Self::Outdated(Some(found)) => {
                write!(formatter, "zellij 0.44+ needed (found {found})")
            }
            Self::Outdated(None) => write!(formatter, "zellij 0.44+ needed"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Listed {
    terminal: u64,
    tab: u64,
    focused: bool,
}

fn parse_panes(json: &str) -> Result<Vec<Listed>> {
    let value: Value = serde_json::from_str(json).context("zellij listed panes as bad JSON")?;
    let entries = value
        .as_array()
        .context("zellij listed panes as non-list")?;
    Ok(entries
        .iter()
        .filter(|entry| entry["is_plugin"] == Value::Bool(false))
        .filter(|entry| entry["exited"] != Value::Bool(true))
        .filter_map(|entry| {
            Some(Listed {
                terminal: entry["id"].as_u64()?,
                tab: entry["tab_id"].as_u64()?,
                focused: entry["is_focused"].as_bool().unwrap_or(false),
            })
        })
        .collect())
}

fn parse_active_tabs(json: &str) -> Result<HashSet<u64>> {
    let value: Value = serde_json::from_str(json).context("zellij listed tabs as bad JSON")?;
    let entries = value.as_array().context("zellij listed tabs as non-list")?;
    Ok(entries
        .iter()
        .filter(|entry| entry["active"] == Value::Bool(true))
        .filter_map(|entry| entry["tab_id"].as_u64())
        .collect())
}

pub struct Zellij<'a, C, T> {
    cli: &'a C,
    table: &'a T,
    session: String,
}

impl<'a, C: Cli, T: ProcessTable> Zellij<'a, C, T> {
    pub fn connect(cli: &'a C, table: &'a T, session: &str) -> Result<Self, Unavailable> {
        let output = cli
            .run(PROGRAM, &owned(&["--version"]))
            .map_err(|_| Unavailable::Missing)?;
        match parse_version(&output) {
            Some(version) if version >= MINIMUM => Ok(Self {
                cli,
                table,
                session: session.to_owned(),
            }),
            version => Err(Unavailable::Outdated(version)),
        }
    }

    fn action(&self, arguments: &[&str]) -> Result<String> {
        let mut all = owned(&["--session", &self.session, "action"]);
        all.extend(owned(arguments));
        self.cli.run(PROGRAM, &all)
    }
}

impl<C: Cli, T: ProcessTable> Multiplexer for Zellij<'_, C, T> {
    fn kind(&self) -> MultiplexerKind {
        MultiplexerKind::Zellij
    }

    fn panes(&self) -> Result<Vec<Pane>> {
        let listed = parse_panes(&self.action(&["list-panes", "-a", "-j"])?)?;
        let active = parse_active_tabs(&self.action(&["list-tabs", "-j"])?)?;
        let roots = pane_roots(
            self.table,
            PaneTags {
                session_variable: "ZELLIJ_SESSION_NAME",
                session: &self.session,
                pane_variable: "ZELLIJ_PANE_ID",
                servers: SERVERS,
            },
        );
        Ok(listed
            .into_iter()
            .filter_map(|pane| {
                let root = roots.get(&pane.terminal.to_string())?;
                let recency = match (pane.focused, active.contains(&pane.tab)) {
                    (true, true) => 2,
                    (true, false) => 1,
                    _ => 0,
                };
                Some(Pane {
                    id: format!("terminal_{}", pane.terminal),
                    anchor: Anchor::Process(*root),
                    session: self.session.clone(),
                    window: pane.tab.to_string(),
                    recency,
                })
            })
            .collect())
    }

    fn screen(&self, pane: &str) -> Option<String> {
        self.action(&["dump-screen", "--pane-id", pane]).ok()
    }

    fn send(&self, pane: &str, input: &[u8], paste: bool) -> Result<()> {
        if input.is_empty() {
            return Ok(());
        }
        let arguments = if paste {
            let text = std::str::from_utf8(input).context("zellij pastes UTF-8 text only")?;
            owned(&["paste", "--pane-id", pane, text])
        } else {
            let mut arguments = owned(&["write", "--pane-id", pane]);
            arguments.extend(input.iter().map(u8::to_string));
            arguments
        };
        self.reveal(pane)?;
        let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
        self.action(&arguments).map(drop)
    }

    fn reveal(&self, pane: &str) -> Result<()> {
        self.action(&["scroll-to-bottom", "--pane-id", pane])
            .map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::super::cli::fake::FakeCli;
    use super::super::process::Pid;
    use super::super::process::fake::FakeTable;
    use super::*;

    const SESSION: &str = "thesis";

    const PANES: &str = r#"[
        {"id": 0, "is_plugin": true, "is_focused": false, "exited": false, "tab_id": 0},
        {"id": 0, "is_plugin": false, "is_focused": true, "exited": false, "tab_id": 0},
        {"id": 1, "is_plugin": false, "is_focused": true, "exited": false, "tab_id": 1},
        {"id": 2, "is_plugin": false, "is_focused": false, "exited": false, "tab_id": 1},
        {"id": 3, "is_plugin": false, "is_focused": false, "exited": true, "tab_id": 1},
        {"id": 9, "is_plugin": false, "is_focused": false, "exited": false, "tab_id": 1},
        {"id": 8, "is_plugin": false, "exited": false}
    ]"#;

    const TABS: &str = r#"[
        {"position": 0, "tab_id": 0, "active": true},
        {"position": 1, "tab_id": 1, "active": false}
    ]"#;

    fn table() -> FakeTable {
        let mut table = FakeTable::default();
        table
            .spawn(500, 1, "zellij")
            .spawn(10, 500, "bash")
            .spawn(11, 10, "termleaf")
            .spawn(20, 500, "nvim")
            .spawn(30, 500, "bash")
            .spawn(40, 500, "bash")
            .spawn(80, 500, "bash");
        for (pid, pane) in [
            (10, "0"),
            (11, "0"),
            (20, "1"),
            (30, "2"),
            (40, "3"),
            (80, "8"),
        ] {
            table
                .variable(pid, "ZELLIJ_SESSION_NAME", SESSION)
                .variable(pid, "ZELLIJ_PANE_ID", pane);
        }
        table
    }

    fn connected(cli: FakeCli) -> FakeCli {
        cli.prints(&["--version"], "zellij 0.45.1\n")
    }

    fn action(arguments: &[&str]) -> Vec<String> {
        let mut call = owned(&["zellij", "--session", SESSION, "action"]);
        call.extend(owned(arguments));
        call
    }

    fn refusal(cli: &FakeCli) -> Option<String> {
        let table = FakeTable::default();
        Zellij::connect(cli, &table, SESSION)
            .err()
            .map(|refused| refused.to_string())
    }

    #[test]
    fn only_a_zellij_new_enough_for_pane_ids_connects() {
        assert_eq!(parse_version("zellij 0.44.0-rc2"), Some(Version(0, 44, 0)));
        assert_eq!(parse_version("zellij 1.2"), Some(Version(1, 2, 0)));
        assert_eq!(parse_version("zellij"), None);
        assert_eq!(refusal(&connected(FakeCli::default())), None);
        let old = FakeCli::default().prints(&["--version"], "zellij 0.43.1\n");
        assert_eq!(
            refusal(&old).as_deref(),
            Some("zellij 0.44+ needed (found 0.43.1)")
        );
        let unknown = FakeCli::default().prints(&["--version"], "zellij unknown\n");
        assert_eq!(refusal(&unknown).as_deref(), Some("zellij 0.44+ needed"));
        let missing = FakeCli::default().fails(&["--version"]);
        assert_eq!(refusal(&missing).as_deref(), Some("zellij not found"));
    }

    #[test]
    fn terminal_panes_are_rooted_through_the_environment_and_ranked_by_focus() {
        let cli = connected(FakeCli::default())
            .prints(&["list-panes"], PANES)
            .prints(&["list-tabs"], TABS);
        let table = table();
        let zellij = Zellij::connect(&cli, &table, SESSION).expect("connected");
        assert_eq!(zellij.kind(), MultiplexerKind::Zellij);
        let summary: Vec<(String, Anchor, String, u64)> = zellij
            .panes()
            .expect("panes")
            .into_iter()
            .map(|pane| (pane.id, pane.anchor, pane.window, pane.recency))
            .collect();
        assert_eq!(
            summary,
            [
                (
                    "terminal_0".to_owned(),
                    Anchor::Process(Pid(10)),
                    "0".to_owned(),
                    2
                ),
                (
                    "terminal_1".to_owned(),
                    Anchor::Process(Pid(20)),
                    "1".to_owned(),
                    1
                ),
                (
                    "terminal_2".to_owned(),
                    Anchor::Process(Pid(30)),
                    "1".to_owned(),
                    0
                ),
            ]
        );
        assert_eq!(
            cli.calls()[1..],
            [
                action(&["list-panes", "-a", "-j"]),
                action(&["list-tabs", "-j"])
            ]
        );
    }

    #[test]
    fn listings_that_are_not_json_lists_are_errors() {
        for bad in ["not json", "{}"] {
            assert!(parse_panes(bad).is_err());
            assert!(parse_active_tabs(bad).is_err());
        }
    }

    #[test]
    fn input_leaves_scroll_mode_and_goes_to_one_pane_as_bytes_or_a_paste() {
        let cli = connected(FakeCli::default());
        let table = FakeTable::default();
        let zellij = Zellij::connect(&cli, &table, SESSION).expect("connected");
        zellij
            .send("terminal_1", b"\x1c\x0e:77\r", false)
            .expect("written");
        zellij
            .send("terminal_2", "/t/my ch5 é.tex".as_bytes(), true)
            .expect("pasted");
        assert!(zellij.send("terminal_2", b"\xff", true).is_err());
        zellij
            .send("terminal_2", b"", false)
            .expect("nothing to do");
        zellij.reveal("terminal_1").expect("revealed");
        assert_eq!(
            cli.calls()[1..],
            [
                action(&["scroll-to-bottom", "--pane-id", "terminal_1"]),
                action(&[
                    "write",
                    "--pane-id",
                    "terminal_1",
                    "28",
                    "14",
                    "58",
                    "55",
                    "55",
                    "13"
                ]),
                action(&["scroll-to-bottom", "--pane-id", "terminal_2"]),
                action(&["paste", "--pane-id", "terminal_2", "/t/my ch5 é.tex"]),
                action(&["scroll-to-bottom", "--pane-id", "terminal_1"]),
            ]
        );
    }

    #[test]
    fn the_screen_is_dumped_from_one_pane() {
        let table = FakeTable::default();
        let cli = connected(FakeCli::default()).prints(&["dump-screen"], "line 77\n");
        let zellij = Zellij::connect(&cli, &table, SESSION).expect("connected");
        assert_eq!(zellij.screen("terminal_1").as_deref(), Some("line 77\n"));
        assert_eq!(
            cli.calls()[1],
            action(&["dump-screen", "--pane-id", "terminal_1"])
        );
        let failing = connected(FakeCli::default()).fails(&["dump-screen"]);
        let zellij = Zellij::connect(&failing, &table, SESSION).expect("connected");
        assert_eq!(zellij.screen("terminal_1"), None);
    }
}
