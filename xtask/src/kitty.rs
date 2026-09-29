use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use serde_json::Value;

use crate::jumps::{Host, Outcome, PROMPT, Server, poll};
use crate::pane_shell::{Tags, idle};
use crate::tmux::shell_rc;

struct Kitty {
    child: Child,
    to: String,
    pid: String,
}

fn session(work: &Path, termleaf: &Path, rc: &Path) -> String {
    format!(
        "new_tab thesis\ncd {work}\nlaunch '{termleaf}' --graphics kitty --no-follow doc.pdf\nlaunch bash --noprofile --rcfile '{rc}'\nnew_tab editor\ncd {work}\nlaunch bash --norc --noprofile\n",
        work = work.display(),
        termleaf = termleaf.display(),
        rc = rc.display(),
    )
}

fn windows(ls: &str) -> Option<(String, String, String)> {
    let listed: Value = serde_json::from_str(ls).ok()?;
    let tabs = listed.get(0)?["tabs"].as_array()?;
    let id = |tab: usize, window: usize| {
        tabs.get(tab)?["windows"].get(window)?["id"]
            .as_u64()
            .map(|id| id.to_string())
    };
    Some((id(0, 0)?, id(0, 1)?, id(1, 0)?))
}

pub fn start(work: &Path, termleaf: &Path) -> Outcome<Server> {
    let rc = shell_rc(work)?;
    let session_file = work.join("kitty-session");
    fs::write(&session_file, session(work, termleaf, &rc)).map_err(|error| error.to_string())?;
    let log = File::create(work.join("kitty.log")).map_err(|error| error.to_string())?;
    let child = Command::new("kitty")
        .args([
            "--config",
            "NONE",
            "-o",
            "allow_remote_control=socket-only",
            "-o",
            "initial_window_width=226c",
            "-o",
            "initial_window_height=51c",
            "-o",
            "remember_window_size=no",
            "-o",
            "font_size=6",
            "-o",
            "enabled_layouts=horizontal",
        ])
        .arg("-o")
        .arg(format!("listen_on=unix:{}", work.join("kitty").display()))
        .arg("--session")
        .arg(&session_file)
        .current_dir(work)
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(|error| error.to_string())?)
        .stderr(log)
        .spawn()
        .map_err(|error| format!("running kitty: {error}"))?;
    let pid = child.id().to_string();
    let kitty = Kitty {
        to: format!("unix:{}-{pid}", work.join("kitty").display()),
        pid,
        child,
    };
    let (viewer, shell, editor) =
        poll("kitty's windows", || windows(&kitty.remote(&["ls"]).ok()?))?;
    kitty.remote(&["focus-tab", "--match", "index:0"])?;
    poll("the shell prompt", || {
        kitty
            .screen(&shell)
            .contains(PROMPT.trim_end())
            .then_some(())
    })?;
    Ok(Server {
        host: Box::new(kitty),
        viewer,
        editor,
        shell,
    })
}

impl Kitty {
    fn remote_with(&self, args: &[&str], input: Option<&[u8]>) -> Outcome<String> {
        let mut child = Command::new("kitty")
            .args(["@", "--to", &self.to])
            .args(args)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("running kitty @: {error}"))?;
        if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
            stdin
                .write_all(input)
                .map_err(|error| format!("feeding kitty @: {error}"))?;
        }
        let output = child
            .wait_with_output()
            .map_err(|error| error.to_string())?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(format!(
                "kitty @ {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr)
            ))
        }
    }

    fn remote(&self, args: &[&str]) -> Outcome<String> {
        self.remote_with(args, None)
    }

    fn tags(&self) -> Tags<'_> {
        Tags {
            session_variable: "KITTY_PID",
            session: &self.pid,
            pane_variable: "KITTY_WINDOW_ID",
            servers: &["kitty"],
        }
    }
}

impl Host for Kitty {
    fn label(&self, editor: &str, pane: &str) -> String {
        format!("{editor} in kitty {pane}")
    }

    fn screen(&self, pane: &str) -> String {
        self.remote(&["get-text", "--match", &format!("id:{pane}")])
            .unwrap_or_default()
    }

    fn bytes(&self, pane: &str, bytes: &[u8]) -> Outcome<()> {
        self.remote_with(
            &[
                "send-text",
                "--match",
                &format!("id:{pane}"),
                "--stdin",
                "--bracketed-paste=disable",
            ],
            Some(bytes),
        )
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
        let listed: Value = serde_json::from_str(&self.remote(&["ls"])?)
            .map_err(|error| format!("kitty @ ls: {error}"))?;
        let active: Vec<bool> = listed[0]["tabs"]
            .as_array()
            .map(|tabs| {
                tabs.iter()
                    .map(|tab| tab["is_active"] == Value::Bool(true))
                    .collect()
            })
            .unwrap_or_default();
        if active.first() == Some(&true) && active.iter().filter(|active| **active).count() == 1 {
            Ok(())
        } else {
            Err(format!("kitty moved away from the thesis tab: {listed}"))
        }
    }
}

impl Drop for Kitty {
    fn drop(&mut self) {
        let _ = Command::new("kill").args(["-TERM", &self.pid]).status();
        if poll("kitty to quit", || self.child.try_wait().ok().flatten()).is_err() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
