use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::cli::{Cli, owned};
use super::detect::MultiplexerKind;
use super::probe::{Anchor, Multiplexer, Pane};
use super::process::Pid;

const PROGRAM: &str = "kitty";

pub struct Kitty<'a, C> {
    cli: &'a C,
    to: String,
}

impl<'a, C: Cli> Kitty<'a, C> {
    pub fn new(cli: &'a C, control: &Path) -> Self {
        Self {
            cli,
            to: address(control),
        }
    }

    fn remote(&self, command: &str, arguments: &[&str]) -> Vec<String> {
        let mut all = owned(&["@", "--to", &self.to, command]);
        all.extend(owned(arguments));
        all
    }
}

fn address(control: &Path) -> String {
    let control = control.to_string_lossy();
    if control.starts_with("tcp:") || control.starts_with("fd:") {
        control.into_owned()
    } else {
        format!("unix:{control}")
    }
}

fn matching(window: &str) -> Result<String> {
    if window.is_empty() || !window.bytes().all(|byte| byte.is_ascii_digit()) {
        bail!("not a kitty window id: {window:?}");
    }
    Ok(format!("id:{window}"))
}

fn flag(value: &Value, name: &str) -> bool {
    value[name].as_bool().unwrap_or(false)
}

fn entries<'v>(value: &'v Value, name: &str) -> &'v [Value] {
    value[name].as_array().map_or(&[], Vec::as_slice)
}

fn windows(ls: &str) -> Result<Vec<Pane>> {
    let os_windows: Vec<Value> = serde_json::from_str(ls).context("kitty @ ls printed no JSON")?;
    let mut panes = Vec::new();
    for os_window in &os_windows {
        let focused = flag(os_window, "is_focused") || flag(os_window, "last_focused");
        for tab in entries(os_window, "tabs") {
            for window in entries(tab, "windows") {
                let (Some(id), Some(pid)) = (
                    window["id"].as_u64(),
                    window["pid"]
                        .as_u64()
                        .and_then(|pid| u32::try_from(pid).ok()),
                ) else {
                    continue;
                };
                panes.push(Pane {
                    id: id.to_string(),
                    anchor: Anchor::Process(Pid(pid)),
                    session: os_window["id"].to_string(),
                    window: tab["id"].to_string(),
                    recency: u64::from(focused) << 2
                        | u64::from(flag(tab, "is_active")) << 1
                        | u64::from(flag(window, "is_active")),
                });
            }
        }
    }
    Ok(panes)
}

impl<C: Cli> Multiplexer for Kitty<'_, C> {
    fn kind(&self) -> MultiplexerKind {
        MultiplexerKind::Kitty
    }

    fn panes(&self) -> Result<Vec<Pane>> {
        windows(&self.cli.run(PROGRAM, &self.remote("ls", &[]))?)
    }

    fn screen(&self, pane: &str) -> Option<String> {
        let window = matching(pane).ok()?;
        self.cli
            .run(
                PROGRAM,
                &self.remote("get-text", &["--match", &window, "--extent", "all"]),
            )
            .ok()
    }

    fn send(&self, pane: &str, input: &[u8], paste: bool) -> Result<()> {
        let window = matching(pane)?;
        if input.is_empty() {
            return Ok(());
        }
        let bracketed = if paste {
            "--bracketed-paste=auto"
        } else {
            "--bracketed-paste=disable"
        };
        self.reveal(pane)?;
        self.cli
            .feed(
                PROGRAM,
                &self.remote("send-text", &["--match", &window, "--stdin", bracketed]),
                input,
            )
            .map(drop)
    }

    fn reveal(&self, pane: &str) -> Result<()> {
        let window = matching(pane)?;
        self.cli
            .run(
                PROGRAM,
                &self.remote("scroll-window", &["--match", &window, "end"]),
            )
            .map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::super::cli::fake::FakeCli;
    use super::*;

    const TO: &str = "unix:/tmp/kitty-4000";

    const LS: &str = r#"[
      {"id": 1, "is_focused": true, "last_focused": true, "tabs": [
        {"id": 1, "is_active": true, "windows": [
          {"id": 1, "pid": 4100, "is_active": true},
          {"id": 3, "pid": 4300, "is_active": false}
        ]},
        {"id": 2, "is_active": false, "windows": [
          {"id": 2, "pid": 4200, "is_active": true},
          {"id": 6},
          {"pid": 7}
        ]}
      ]},
      {"id": 4, "is_focused": false, "last_focused": false, "tabs": [
        {"id": 5, "is_active": true, "windows": [{"id": 9, "pid": 4900, "is_active": true}]}
      ]}
    ]"#;

    fn remote(arguments: &[&str]) -> Vec<String> {
        let mut call = owned(&["kitty", "@", "--to", TO]);
        call.extend(owned(arguments));
        call
    }

    #[test]
    fn windows_are_listed_read_and_sent_to_over_the_socket_by_id() {
        let cli = FakeCli::default()
            .prints(&["ls"], LS)
            .prints(&["get-text"], "nvim\n");
        let kitty = Kitty::new(&cli, Path::new("/tmp/kitty-4000"));
        let panes = kitty.panes().expect("kitty's JSON");
        let summary: Vec<(&str, &Anchor, &str, &str, u64)> = panes
            .iter()
            .map(|pane| {
                (
                    pane.id.as_str(),
                    &pane.anchor,
                    pane.session.as_str(),
                    pane.window.as_str(),
                    pane.recency,
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("1", &Anchor::Process(Pid(4100)), "1", "1", 7),
                ("3", &Anchor::Process(Pid(4300)), "1", "1", 6),
                ("2", &Anchor::Process(Pid(4200)), "1", "2", 5),
                ("9", &Anchor::Process(Pid(4900)), "4", "5", 3),
            ]
        );
        assert_eq!(kitty.screen("3").as_deref(), Some("nvim\n"));
        let bytes = b"\x1c\x0e:drop /t/my\\ thesis/ch5.tex | 77\r";
        kitty.send("3", bytes, false).expect("sent");
        kitty.send("3", b"x", true).expect("pasted");
        kitty.send("3", b"", false).expect("nothing to send");
        assert_eq!(
            cli.calls(),
            [
                remote(&["ls"]),
                remote(&["get-text", "--match", "id:3", "--extent", "all"]),
                remote(&["scroll-window", "--match", "id:3", "end"]),
                remote(&[
                    "send-text",
                    "--match",
                    "id:3",
                    "--stdin",
                    "--bracketed-paste=disable"
                ]),
                remote(&["scroll-window", "--match", "id:3", "end"]),
                remote(&[
                    "send-text",
                    "--match",
                    "id:3",
                    "--stdin",
                    "--bracketed-paste=auto"
                ]),
            ]
        );
        assert_eq!(cli.fed(), [bytes.to_vec(), b"x".to_vec()]);
        assert_eq!(kitty.kind(), MultiplexerKind::Kitty);
    }

    #[test]
    fn only_a_number_can_name_a_window() {
        let cli = FakeCli::default();
        let kitty = Kitty::new(&cli, Path::new("/tmp/kitty-4000"));
        for bad in ["", "3 or title:x", "id:3", "-1", "recent:0"] {
            assert_eq!(kitty.screen(bad), None, "{bad:?}");
            assert!(kitty.send(bad, b"x", false).is_err(), "{bad:?}");
            assert!(kitty.reveal(bad).is_err(), "{bad:?}");
        }
        assert!(cli.calls().is_empty());
    }

    #[test]
    fn tcp_and_abstract_sockets_keep_their_address() {
        assert_eq!(
            address(Path::new("tcp:localhost:12488")),
            "tcp:localhost:12488"
        );
        assert_eq!(address(Path::new("fd:3")), "fd:3");
        assert_eq!(address(Path::new("@kitty-4000")), "unix:@kitty-4000");
    }

    #[test]
    fn a_kitty_that_does_not_answer_is_an_error() {
        let cli = FakeCli::default().prints(&["ls"], "Error: Failed to connect");
        let kitty = Kitty::new(&cli, Path::new("/tmp/kitty-4000"));
        assert!(kitty.panes().is_err());
        let failing = FakeCli::default().fails(&["get-text"]);
        assert_eq!(
            Kitty::new(&failing, Path::new("/tmp/kitty-4000")).screen("3"),
            None
        );
    }
}
