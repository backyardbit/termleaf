use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use serde_json::Value;

use crate::jumps::{Host, Outcome, PROMPT, Server, poll};
use crate::pane_shell::{Tags, idle};
use crate::tmux::shell_rc;

struct Wezterm {
    child: Child,
    socket: PathBuf,
    socket_text: String,
}

fn lua(text: &Path) -> String {
    format!(
        "\"{}\"",
        text.display()
            .to_string()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    )
}

fn config(work: &Path, termleaf: &Path) -> String {
    format!(
        "return {{\n  default_prog = {{ {}, \"--graphics\", \"iterm2\", \"doc.pdf\" }},\n  default_cwd = {},\n  initial_cols = 226,\n  initial_rows = 51,\n}}\n",
        lua(termleaf),
        lua(work),
    )
}

pub fn start(work: &Path, termleaf: &Path) -> Outcome<Server> {
    let rc = shell_rc(work)?;
    let config_file = work.join("wezterm.lua");
    fs::write(&config_file, config(work, termleaf)).map_err(|error| error.to_string())?;
    let runtime = work.join("xdg");
    fs::create_dir_all(&runtime).map_err(|error| error.to_string())?;
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700))
        .map_err(|error| error.to_string())?;
    let log = File::create(work.join("wezterm.log")).map_err(|error| error.to_string())?;
    let child = Command::new("wezterm-mux-server")
        .current_dir(work)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("WEZTERM_CONFIG_FILE", &config_file)
        .env_remove("WEZTERM_UNIX_SOCKET")
        .env_remove("WEZTERM_PANE")
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(|error| error.to_string())?)
        .stderr(log)
        .spawn()
        .map_err(|error| format!("running wezterm-mux-server: {error}"))?;
    let socket = runtime.join("wezterm/sock");
    let wezterm = Wezterm {
        child,
        socket_text: socket.display().to_string(),
        socket,
    };
    let viewer = poll("the WezTerm mux server", || {
        let listed: Value =
            serde_json::from_str(&wezterm.cli(&["list", "--format", "json"]).ok()?).ok()?;
        listed.get(0)?["pane_id"].as_u64().map(|id| id.to_string())
    })?;
    let work_text = work.display().to_string();
    let rc_text = rc.display().to_string();
    let shell = wezterm
        .cli(&[
            "split-pane",
            "--pane-id",
            &viewer,
            "--right",
            "--cwd",
            &work_text,
            "--",
            "bash",
            "--noprofile",
            "--rcfile",
            &rc_text,
        ])?
        .trim()
        .to_owned();
    let editor = wezterm
        .cli(&[
            "spawn",
            "--pane-id",
            &viewer,
            "--cwd",
            &work_text,
            "--",
            "bash",
            "--norc",
            "--noprofile",
        ])?
        .trim()
        .to_owned();
    wezterm.cli(&["activate-pane", "--pane-id", &viewer])?;
    poll("the shell prompt", || {
        wezterm
            .screen(&shell)
            .contains(PROMPT.trim_end())
            .then_some(())
    })?;
    Ok(Server {
        host: Box::new(wezterm),
        viewer,
        editor,
        shell,
    })
}

impl Wezterm {
    fn cli_with(&self, args: &[&str], input: Option<&[u8]>) -> Outcome<String> {
        let mut child = Command::new("wezterm")
            .args(["cli", "--no-auto-start"])
            .args(args)
            .env("WEZTERM_UNIX_SOCKET", &self.socket)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("running wezterm cli: {error}"))?;
        if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
            stdin
                .write_all(input)
                .map_err(|error| format!("feeding wezterm cli: {error}"))?;
        }
        let output = child
            .wait_with_output()
            .map_err(|error| error.to_string())?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(format!(
                "wezterm cli {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr)
            ))
        }
    }

    fn cli(&self, args: &[&str]) -> Outcome<String> {
        self.cli_with(args, None)
    }

    fn tags(&self) -> Tags<'_> {
        Tags {
            session_variable: "WEZTERM_UNIX_SOCKET",
            session: &self.socket_text,
            pane_variable: "WEZTERM_PANE",
            servers: &["wezterm-mux-server"],
        }
    }
}

impl Host for Wezterm {
    fn label(&self, editor: &str, pane: &str) -> String {
        format!("{editor} in wezterm {pane}")
    }

    fn screen(&self, pane: &str) -> String {
        self.cli(&["get-text", "--pane-id", pane])
            .unwrap_or_default()
    }

    fn bytes(&self, pane: &str, bytes: &[u8]) -> Outcome<()> {
        self.cli_with(&["send-text", "--pane-id", pane, "--no-paste"], Some(bytes))
            .map(drop)
    }

    fn respawn(&self, pane: &str, command: &str, _work: &Path) -> Outcome<()> {
        self.bytes(pane, format!("clear; {command}\r").as_bytes())
    }

    fn exited(&self, pane: &str) -> bool {
        idle(&self.tags(), pane)
    }

    fn copy_mode(&self, _pane: &str) -> Outcome<bool> {
        Ok(false)
    }

    fn focus_kept(&self) -> Outcome<()> {
        let listed = self.cli(&["list", "--format", "json"])?;
        let panes: Value =
            serde_json::from_str(&listed).map_err(|error| format!("wezterm cli list: {error}"))?;
        let viewer_active = panes
            .as_array()
            .and_then(|panes| panes.first())
            .is_some_and(|viewer| viewer["is_active"] == Value::Bool(true));
        if viewer_active {
            Ok(())
        } else {
            Err(format!("WezTerm moved away from termleaf: {listed}"))
        }
    }
}

impl Drop for Wezterm {
    fn drop(&mut self) {
        let _ = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status();
        if poll("wezterm-mux-server to quit", || {
            self.child.try_wait().ok().flatten()
        })
        .is_err()
        {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
