use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::jumps::{Host, Outcome, PROMPT, Server, poll};
use crate::tmux::shell_rc;

struct Herdr {
    terminal: PathBuf,
    config: PathBuf,
    session: String,
}

pub fn start(work: &Path, termleaf: &Path) -> Outcome<Server> {
    let config = work.join("herdr-config");
    fs::create_dir_all(config.join("herdr")).map_err(|error| error.to_string())?;
    fs::write(config.join("herdr/config.toml"), "onboarding = false\n")
        .map_err(|error| error.to_string())?;
    let herdr = Herdr {
        terminal: work.join("terminal.sock"),
        config,
        session: format!("e2e-{}", std::process::id()),
    };
    let launched = Command::new("tmux")
        .arg("-S")
        .arg(&herdr.terminal)
        .args([
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-x",
            "226",
            "-y",
            "51",
        ])
        .arg("-c")
        .arg(work)
        .arg(format!(
            "env -u TMUX -u TMUX_PANE XDG_CONFIG_HOME='{}' herdr --session '{}'",
            herdr.config.display(),
            herdr.session
        ))
        .status()
        .map_err(|error| format!("running tmux as herdr's terminal: {error}"))?;
    if !launched.success() {
        return Err("tmux could not start herdr".to_owned());
    }
    let viewer = poll("herdr to open a pane", || {
        let listed = herdr.request(&["pane", "list"]).ok()?;
        listed["panes"][0]["pane_id"].as_str().map(str::to_owned)
    })?;
    let editor = herdr.split(&viewer, "right", work)?;
    let shell = herdr.split(&editor, "down", work)?;
    herdr.bytes(
        &viewer,
        format!("exec '{}' --graphics kitty doc.pdf\r", termleaf.display()).as_bytes(),
    )?;
    let rc = shell_rc(work)?;
    herdr.bytes(
        &shell,
        format!("exec bash --noprofile --rcfile '{}'\r", rc.display()).as_bytes(),
    )?;
    poll("the shell prompt", || {
        herdr
            .screen(&shell)
            .contains(PROMPT.trim_end())
            .then_some(())
    })?;
    Ok(Server {
        host: Box::new(herdr),
        viewer,
        editor,
        shell,
    })
}

impl Herdr {
    fn run(&self, args: &[&str]) -> Outcome<String> {
        let output = Command::new("herdr")
            .arg("--session")
            .arg(&self.session)
            .args(args)
            .env("XDG_CONFIG_HOME", &self.config)
            .output()
            .map_err(|error| format!("running herdr: {error}"))?;
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        if output.status.success() {
            Ok(text)
        } else {
            Err(format!("herdr {} failed: {text}", args.join(" ")))
        }
    }

    fn request(&self, args: &[&str]) -> Outcome<Value> {
        let reply: Value = serde_json::from_str(&self.run(args)?)
            .map_err(|error| format!("herdr {}: {error}", args.join(" ")))?;
        Ok(reply["result"].clone())
    }

    fn split(&self, pane: &str, direction: &str, work: &Path) -> Outcome<String> {
        let work = work.display().to_string();
        let split = self.request(&[
            "pane",
            "split",
            pane,
            "--direction",
            direction,
            "--cwd",
            &work,
        ])?;
        split["pane"]["pane_id"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("herdr split gave no pane: {split}"))
    }
}

impl Host for Herdr {
    fn name(&self) -> &'static str {
        "herdr"
    }

    fn screen(&self, pane: &str) -> String {
        self.run(&["pane", "read", pane, "--source", "visible"])
            .unwrap_or_default()
    }

    fn bytes(&self, pane: &str, bytes: &[u8]) -> Outcome<()> {
        let text = std::str::from_utf8(bytes).map_err(|error| error.to_string())?;
        self.run(&["pane", "send-text", pane, text]).map(drop)
    }

    fn respawn(&self, pane: &str, command: &str, _work: &Path) -> Outcome<()> {
        self.bytes(pane, format!("clear; {command}\r").as_bytes())
    }

    fn exited(&self, pane: &str) -> bool {
        self.request(&["pane", "process-info", "--pane", pane])
            .is_ok_and(|info| {
                let info = &info["process_info"];
                info["shell_pid"].is_u64()
                    && info["shell_pid"] == info["foreground_process_group_id"]
            })
    }

    fn copy_mode(&self, _pane: &str) -> Outcome<bool> {
        Ok(false)
    }
}

impl Drop for Herdr {
    fn drop(&mut self) {
        let _ = self.run(&["server", "stop"]);
        let _ = Command::new("tmux")
            .arg("-S")
            .arg(&self.terminal)
            .arg("kill-server")
            .stderr(Stdio::null())
            .status();
    }
}
