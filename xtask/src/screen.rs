use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

use crate::jumps::{Host, Outcome, PROMPT, Server, poll};
use crate::pane_shell::{Tags, idle};
use crate::tmux::shell_rc;

const VIEWER: &str = "0";
const EDITOR: &str = "1";
const SHELL: &str = "2";

struct Screen {
    terminal: PathBuf,
    viewer: PathBuf,
    name: String,
    sty: String,
    hardcopies: PathBuf,
    next: AtomicU32,
}

fn stuffed(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| {
            if (0x20..0x7f).contains(byte) && !b"\\^$'\"".contains(byte) {
                char::from(*byte).to_string()
            } else {
                format!("\\{byte:03o}")
            }
        })
        .collect()
}

pub fn start(work: &Path, termleaf: &Path) -> Outcome<Server> {
    let rc = work.join("screenrc");
    fs::write(&rc, "startup_message off\ndefutf8 on\nvbell off\n")
        .map_err(|error| error.to_string())?;
    let name = format!("e2e-{}", std::process::id());
    let launched = Command::new("tmux")
        .arg("-S")
        .arg(work.join("terminal.sock"))
        .args([
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-x",
            "100",
            "-y",
            "51",
            "-e",
            "TERM=xterm-256color",
        ])
        .arg("-c")
        .arg(work)
        .arg(format!(
            "env -u TMUX -u TMUX_PANE screen -c '{}' -S '{name}' tmux -S '{}' -f /dev/null new-session \"'{}' --graphics kitty --no-follow doc.pdf\" \\; set status off",
            rc.display(),
            work.join("viewer.sock").display(),
            termleaf.display()
        ))
        .status()
        .map_err(|error| format!("running tmux as screen's terminal: {error}"))?;
    if !launched.success() {
        return Err("tmux could not start screen".to_owned());
    }
    let sty = poll("the screen session", || {
        let listed = Command::new("screen").args(["-ls", &name]).output().ok()?;
        String::from_utf8_lossy(&listed.stdout)
            .split_whitespace()
            .find(|word| word.ends_with(&format!(".{name}")))
            .map(str::to_owned)
    })?;
    let hardcopies = work.join("hardcopies");
    fs::create_dir_all(&hardcopies).map_err(|error| error.to_string())?;
    let screen = Screen {
        terminal: work.join("terminal.sock"),
        viewer: work.join("viewer.sock"),
        name,
        sty,
        hardcopies,
        next: AtomicU32::new(0),
    };
    let shell = shell_rc(work)?;
    screen.run(
        None,
        &[
            "-X",
            "screen",
            "-t",
            "editor",
            EDITOR,
            "bash",
            "--norc",
            "--noprofile",
        ],
    )?;
    screen.run(
        None,
        &[
            "-X",
            "screen",
            "-t",
            "shell",
            SHELL,
            "bash",
            "--noprofile",
            "--rcfile",
            &shell.display().to_string(),
        ],
    )?;
    screen.run(None, &["-X", "select", VIEWER])?;
    poll("the shell prompt", || {
        screen
            .screen(SHELL)
            .contains(PROMPT.trim_end())
            .then_some(())
    })?;
    Ok(Server {
        host: Box::new(screen),
        viewer: VIEWER.to_owned(),
        editor: EDITOR.to_owned(),
        shell: SHELL.to_owned(),
    })
}

impl Screen {
    fn run(&self, window: Option<&str>, args: &[&str]) -> Outcome<String> {
        let mut command = Command::new("screen");
        command.args(["-S", &self.name]);
        if let Some(window) = window {
            command.args(["-p", window]);
        }
        let output = command
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|error| format!("running screen: {error}"))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(format!(
                "screen {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stdout)
            ))
        }
    }

    fn viewer(&self, args: &[&str]) -> Outcome<String> {
        let output = Command::new("tmux")
            .arg("-S")
            .arg(&self.viewer)
            .args(args)
            .output()
            .map_err(|error| format!("running tmux: {error}"))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(format!(
                "tmux {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr)
            ))
        }
    }

    fn tags(&self) -> Tags<'_> {
        Tags {
            session_variable: "STY",
            session: &self.sty,
            pane_variable: "WINDOW",
            servers: &["screen", "SCREEN"],
        }
    }
}

impl Host for Screen {
    fn label(&self, editor: &str, pane: &str) -> String {
        format!("{editor} in screen {pane}")
    }

    fn screen(&self, pane: &str) -> String {
        if pane == VIEWER {
            return self.viewer(&["capture-pane", "-p"]).unwrap_or_default();
        }
        let file = self.hardcopies.join(format!(
            "{pane}-{}",
            self.next.fetch_add(1, Ordering::Relaxed)
        ));
        let path = file.display().to_string();
        if self.run(Some(pane), &["-X", "hardcopy", &path]).is_err() {
            return String::new();
        }
        let mut last = None;
        let text = poll("a whole hardcopy", || {
            let read = fs::read(&file).ok();
            let settled = read.is_some() && read == last;
            last = read;
            if settled { last.clone() } else { None }
        })
        .unwrap_or_default();
        let _ = fs::remove_file(&file);
        String::from_utf8_lossy(&text).into_owned()
    }

    fn bytes(&self, pane: &str, bytes: &[u8]) -> Outcome<()> {
        if pane == VIEWER {
            let hex: Vec<String> = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
            let mut args = vec!["send-keys", "-H"];
            args.extend(hex.iter().map(String::as_str));
            return self.viewer(&args).map(drop);
        }
        for chunk in bytes.chunks(100) {
            self.run(Some(pane), &["-X", "stuff", &stuffed(chunk)])?;
        }
        Ok(())
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
        let output = Command::new("tmux")
            .arg("-S")
            .arg(&self.terminal)
            .args(["capture-pane", "-p"])
            .output()
            .map_err(|error| format!("running tmux: {error}"))?;
        let displayed = String::from_utf8_lossy(&output.stdout);
        if displayed.contains(" · doc.pdf") {
            Ok(())
        } else {
            Err(format!("screen no longer shows termleaf:\n{displayed}"))
        }
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let _ = self.viewer(&["kill-server"]);
        let _ = self.run(None, &["-X", "quit"]);
        let _ = Command::new("tmux")
            .arg("-S")
            .arg(&self.terminal)
            .arg("kill-server")
            .stderr(Stdio::null())
            .status();
    }
}
