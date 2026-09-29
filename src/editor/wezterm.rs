use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::cli::{Cli, owned};
use super::detect::MultiplexerKind;
use super::probe::{Anchor, Multiplexer, Pane};

pub struct Wezterm<'a, C> {
    cli: &'a C,
    program: String,
}

impl<'a, C: Cli> Wezterm<'a, C> {
    pub fn new(cli: &'a C, executable_dir: Option<String>) -> Self {
        let program = executable_dir.map_or_else(
            || "wezterm".to_owned(),
            |dir| {
                PathBuf::from(dir)
                    .join("wezterm")
                    .to_string_lossy()
                    .into_owned()
            },
        );
        Self { cli, program }
    }

    fn command(arguments: &[&str]) -> Vec<String> {
        let mut all = owned(&["cli", "--no-auto-start"]);
        all.extend(owned(arguments));
        all
    }
}

fn numbered(pane: &str) -> Result<&str> {
    if pane.is_empty() || !pane.bytes().all(|byte| byte.is_ascii_digit()) {
        bail!("not a WezTerm pane id: {pane:?}");
    }
    Ok(pane)
}

fn listed_panes(list: &str) -> Result<Vec<Pane>> {
    let listed: Vec<Value> =
        serde_json::from_str(list).context("wezterm cli list printed no JSON")?;
    Ok(listed
        .iter()
        .filter_map(|pane| {
            let tty = pane["tty_name"].as_str().filter(|tty| !tty.is_empty())?;
            Some(Pane {
                id: pane["pane_id"].as_u64()?.to_string(),
                anchor: Anchor::Tty(PathBuf::from(tty)),
                session: pane["window_id"].to_string(),
                window: pane["tab_id"].to_string(),
                recency: u64::from(pane["is_active"].as_bool().unwrap_or(false)),
            })
        })
        .collect())
}

impl<C: Cli> Multiplexer for Wezterm<'_, C> {
    fn kind(&self) -> MultiplexerKind {
        MultiplexerKind::Wezterm
    }

    fn panes(&self) -> Result<Vec<Pane>> {
        listed_panes(
            &self
                .cli
                .run(&self.program, &Self::command(&["list", "--format", "json"]))?,
        )
    }

    fn screen(&self, pane: &str) -> Option<String> {
        let pane = numbered(pane).ok()?;
        self.cli
            .run(
                &self.program,
                &Self::command(&["get-text", "--pane-id", pane]),
            )
            .ok()
    }

    fn send(&self, pane: &str, input: &[u8], paste: bool) -> Result<()> {
        let pane = numbered(pane)?;
        if input.is_empty() {
            return Ok(());
        }
        let mut arguments = vec!["send-text", "--pane-id", pane];
        if !paste {
            arguments.push("--no-paste");
        }
        self.cli
            .feed(&self.program, &Self::command(&arguments), input)
            .map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::super::cli::fake::FakeCli;
    use super::*;

    const LIST: &str = r#"[
      {"window_id": 0, "tab_id": 0, "pane_id": 0, "tty_name": "/dev/pts/3", "is_active": false},
      {"window_id": 0, "tab_id": 0, "pane_id": 1, "tty_name": "/dev/pts/4", "is_active": true},
      {"window_id": 0, "tab_id": 1, "pane_id": 2, "tty_name": null, "is_active": true},
      {"window_id": 1, "tab_id": 2, "pane_id": 3, "tty_name": "/dev/pts/7", "is_active": true}
    ]"#;

    fn call(program: &str, arguments: &[&str]) -> Vec<String> {
        let mut call = owned(&[program, "cli", "--no-auto-start"]);
        call.extend(owned(arguments));
        call
    }

    #[test]
    fn panes_are_listed_read_and_sent_to_by_pane_id() {
        let cli = FakeCli::default()
            .prints(&["list"], LIST)
            .prints(&["get-text"], "vim\n");
        let wezterm = Wezterm::new(&cli, Some("/opt/wez".to_owned()));
        let panes = wezterm.panes().expect("WezTerm's JSON");
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
        let tty = |path: &str| Anchor::Tty(PathBuf::from(path));
        assert_eq!(
            summary,
            [
                ("0", &tty("/dev/pts/3"), "0", "0", 0),
                ("1", &tty("/dev/pts/4"), "0", "0", 1),
                ("3", &tty("/dev/pts/7"), "1", "2", 1),
            ]
        );
        assert_eq!(wezterm.screen("1").as_deref(), Some("vim\n"));
        wezterm.send("1", b"\x1b:e ch5.tex\r", false).expect("sent");
        wezterm.send("1", b"x", true).expect("pasted");
        wezterm.send("1", b"", false).expect("nothing to send");
        let program = "/opt/wez/wezterm";
        assert_eq!(
            cli.calls(),
            [
                call(program, &["list", "--format", "json"]),
                call(program, &["get-text", "--pane-id", "1"]),
                call(program, &["send-text", "--pane-id", "1", "--no-paste"]),
                call(program, &["send-text", "--pane-id", "1"]),
            ]
        );
        assert_eq!(cli.fed(), [b"\x1b:e ch5.tex\r".to_vec(), b"x".to_vec()]);
        assert_eq!(wezterm.kind(), MultiplexerKind::Wezterm);
    }

    #[test]
    fn only_a_number_can_name_a_pane() {
        let cli = FakeCli::default().prints(&["list"], "not json");
        let wezterm = Wezterm::new(&cli, None);
        for bad in ["", "1 2", "-1", "%1"] {
            assert_eq!(wezterm.screen(bad), None, "{bad:?}");
            assert!(wezterm.send(bad, b"x", false).is_err(), "{bad:?}");
        }
        assert!(wezterm.panes().is_err());
        assert_eq!(
            cli.calls(),
            [call("wezterm", &["list", "--format", "json"])]
        );
    }
}
