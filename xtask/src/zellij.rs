use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::jumps::{Host, Outcome, POLL_INTERVAL, PROMPT, Server, poll};
use crate::pane_shell::{Tags, idle};
use crate::tmux::shell_rc;

const ANSWER_WITHIN: Duration = Duration::from_secs(10);
const OPENS_WITHIN: Duration = Duration::from_secs(60);

struct Zellij {
    terminal: PathBuf,
    session: String,
}

fn quoted(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

fn layout(work: &Path, termleaf: &Path, rc: &Path) -> String {
    format!(
        "layout {{\n    cwd {cwd}\n    tab name=\"thesis\" focus=true {{\n        pane split_direction=\"vertical\" {{\n            pane command={termleaf} {{\n                args \"--graphics\" \"kitty\" \"doc.pdf\"\n            }}\n            pane command=\"bash\" {{\n                args \"--noprofile\" \"--rcfile\" {rc}\n            }}\n        }}\n    }}\n    tab name=\"editor\" {{\n        pane command=\"bash\" {{\n            args \"--norc\" \"--noprofile\"\n        }}\n    }}\n}}\n",
        cwd = quoted(&work.display().to_string()),
        termleaf = quoted(&termleaf.display().to_string()),
        rc = quoted(&rc.display().to_string()),
    )
}

fn run(args: &[&str]) -> Outcome<String> {
    let mut child = Command::new("zellij")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("running zellij: {error}"))?;
    let deadline = Instant::now() + ANSWER_WITHIN;
    while child
        .try_wait()
        .map_err(|error| error.to_string())?
        .is_none()
    {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("zellij {} did not answer", args.join(" ")));
        }
        thread::sleep(POLL_INTERVAL);
    }
    let output = child
        .wait_with_output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(format!(
            "zellij {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

pub fn start(work: &Path, termleaf: &Path) -> Outcome<Server> {
    let config = work.join("zellij.kdl");
    fs::write(
        &config,
        "show_startup_tips false\nshow_release_notes false\n",
    )
    .map_err(|error| error.to_string())?;
    let config_dir = work.join("zellij-config");
    fs::create_dir_all(&config_dir).map_err(|error| error.to_string())?;
    let rc = shell_rc(work)?;
    let layout_file = work.join("layout.kdl");
    fs::write(&layout_file, layout(work, termleaf, &rc)).map_err(|error| error.to_string())?;
    let zellij = Zellij {
        terminal: work.join("terminal.sock"),
        session: format!("e2e-{}", std::process::id()),
    };
    let launched = Command::new("tmux")
        .arg("-S")
        .arg(&zellij.terminal)
        .args([
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-x",
            "226",
            "-y",
            "51",
            "-e",
            "TERM=xterm-256color",
        ])
        .arg("-c")
        .arg(work)
        .arg(format!(
            "env -u TMUX -u TMUX_PANE zellij --config-dir '{}' --config '{}' --new-session-with-layout '{}' --session '{}'",
            config_dir.display(),
            config.display(),
            layout_file.display(),
            zellij.session
        ))
        .status()
        .map_err(|error| format!("running tmux as zellij's terminal: {error}"))?;
    if !launched.success() {
        return Err("tmux could not start zellij".to_owned());
    }
    poll("the zellij session", || {
        run(&["list-sessions", "--short", "--no-formatting"])
            .ok()?
            .lines()
            .any(|name| name == zellij.session)
            .then_some(())
    })?;
    let (viewer, shell, editor) = zellij.opened()?;
    poll("the shell prompt", || {
        zellij
            .screen(&shell)
            .contains(PROMPT.trim_end())
            .then_some(())
    })?;
    Ok(Server {
        host: Box::new(zellij),
        viewer,
        editor,
        shell,
    })
}

fn three_panes(json: &str) -> Option<(String, String, String)> {
    let listed: Value = serde_json::from_str(json).ok()?;
    let mut terminals: Vec<(u64, u64, u64)> = listed
        .as_array()?
        .iter()
        .filter(|pane| pane["is_plugin"] == Value::Bool(false))
        .filter_map(|pane| {
            Some((
                pane["tab_position"].as_u64()?,
                pane["pane_x"].as_u64()?,
                pane["id"].as_u64()?,
            ))
        })
        .collect();
    terminals.sort_unstable();
    let name = |index: usize| format!("terminal_{}", terminals[index].2);
    match terminals.as_slice() {
        [(0, _, _), (0, _, _), (1, _, _)] => Some((name(0), name(1), name(2))),
        _ => None,
    }
}

impl Zellij {
    fn action(&self, args: &[&str]) -> Outcome<String> {
        let mut all = vec!["--session", &self.session, "action"];
        all.extend_from_slice(args);
        run(&all)
    }

    fn opened(&self) -> Outcome<(String, String, String)> {
        let deadline = Instant::now() + OPENS_WITHIN;
        loop {
            let listed = self.action(&["list-panes", "-a", "-j"]);
            if let Some(panes) = listed.as_deref().ok().and_then(three_panes) {
                return Ok(panes);
            }
            if Instant::now() >= deadline {
                let terminal = Command::new("tmux")
                    .arg("-S")
                    .arg(&self.terminal)
                    .args(["capture-pane", "-p"])
                    .output()
                    .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
                    .unwrap_or_default();
                return Err(format!(
                    "zellij did not open its panes within {} s\nlist-panes: {}\nterminal:\n{}",
                    OPENS_WITHIN.as_secs(),
                    listed.unwrap_or_else(|error| error),
                    terminal.trim_end()
                ));
            }
            thread::sleep(POLL_INTERVAL);
        }
    }

    fn tags(&self) -> Tags<'_> {
        Tags {
            session_variable: "ZELLIJ_SESSION_NAME",
            session: &self.session,
            pane_variable: "ZELLIJ_PANE_ID",
            servers: &["zellij"],
        }
    }
}

impl Host for Zellij {
    fn label(&self, editor: &str, pane: &str) -> String {
        format!("{editor} in zellij {pane}")
    }

    fn screen(&self, pane: &str) -> String {
        self.action(&["dump-screen", "--pane-id", pane])
            .unwrap_or_default()
    }

    fn bytes(&self, pane: &str, bytes: &[u8]) -> Outcome<()> {
        let decimal: Vec<String> = bytes.iter().map(u8::to_string).collect();
        let mut args = vec!["write", "--pane-id", pane];
        args.extend(decimal.iter().map(String::as_str));
        self.action(&args).map(drop)
    }

    fn respawn(&self, pane: &str, command: &str, _work: &Path) -> Outcome<()> {
        self.bytes(pane, format!("clear; {command}\r").as_bytes())
    }

    fn exited(&self, pane: &str) -> bool {
        let number = pane.strip_prefix("terminal_").unwrap_or(pane);
        idle(&self.tags(), number)
    }

    fn copy_mode(&self, _pane: &str) -> Outcome<bool> {
        Ok(false)
    }

    fn focus_kept(&self) -> Outcome<()> {
        let tabs: Value = serde_json::from_str(&self.action(&["list-tabs", "-j"])?)
            .map_err(|error| format!("zellij list-tabs: {error}"))?;
        let active: Vec<&Value> = tabs
            .as_array()
            .map(|tabs| {
                tabs.iter()
                    .filter(|tab| tab["active"] == Value::Bool(true))
                    .map(|tab| &tab["position"])
                    .collect()
            })
            .unwrap_or_default();
        if active == [&Value::from(0)] {
            Ok(())
        } else {
            Err(format!("zellij moved away from the first tab: {tabs}"))
        }
    }
}

impl Drop for Zellij {
    fn drop(&mut self) {
        let quiet = |args: &[&str]| {
            let _ = Command::new("zellij")
                .args(args)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        };
        quiet(&["kill-session", &self.session]);
        let _ = Command::new("tmux")
            .arg("-S")
            .arg(&self.terminal)
            .arg("kill-server")
            .stderr(Stdio::null())
            .status();
        quiet(&["delete-session", "--force", &self.session]);
    }
}
