use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::jumps::{Host, Outcome, PROMPT, Server, poll};

struct Tmux {
    socket: PathBuf,
}

pub fn start(work: &Path, termleaf: &Path) -> Outcome<Server> {
    let tmux = Tmux {
        socket: work.join("tmux.sock"),
    };
    let work_text = work.display().to_string();
    let viewer = tmux
        .run(&[
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-P",
            "-F",
            "#{pane_id}",
            "-x",
            "200",
            "-y",
            "50",
            "-s",
            "e2e",
            "-c",
            &work_text,
            &format!("'{}' --graphics kitty doc.pdf", termleaf.display()),
        ])?
        .trim()
        .to_owned();
    tmux.run(&["set", "-g", "allow-passthrough", "on"])?;
    tmux.run(&["set", "-g", "remain-on-exit", "on"])?;
    let editor = tmux
        .run(&[
            "split-window",
            "-h",
            "-d",
            "-P",
            "-F",
            "#{pane_id}",
            "-t",
            &viewer,
            "-c",
            &work_text,
            "sleep 86400",
        ])?
        .trim()
        .to_owned();
    let rc = shell_rc(work)?;
    let shell = tmux
        .run(&[
            "split-window",
            "-v",
            "-d",
            "-P",
            "-F",
            "#{pane_id}",
            "-t",
            &editor,
            "-c",
            &work_text,
            &format!("bash --noprofile --rcfile '{}'", rc.display()),
        ])?
        .trim()
        .to_owned();
    poll("the shell prompt", || {
        tmux.screen(&shell)
            .contains(PROMPT.trim_end())
            .then_some(())
    })?;
    Ok(Server {
        host: Box::new(tmux),
        viewer,
        editor,
        shell,
    })
}

pub fn shell_rc(work: &Path) -> Outcome<PathBuf> {
    let rc = work.join("shellrc");
    fs::write(
        &rc,
        format!(
            "PS1='{PROMPT}'\nPROMPT_COMMAND=\"history 1 >> '{}'\"\n",
            work.join("shell-history").display()
        ),
    )
    .map_err(|error| error.to_string())?;
    Ok(rc)
}

impl Tmux {
    fn run(&self, args: &[&str]) -> Outcome<String> {
        let output = Command::new("tmux")
            .arg("-S")
            .arg(&self.socket)
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
}

impl Host for Tmux {
    fn name(&self) -> &'static str {
        "tmux"
    }

    fn screen(&self, pane: &str) -> String {
        self.run(&["capture-pane", "-p", "-t", pane])
            .unwrap_or_default()
    }

    fn bytes(&self, pane: &str, bytes: &[u8]) -> Outcome<()> {
        let hex: Vec<String> = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let mut args = vec!["send-keys", "-t", pane, "-H"];
        args.extend(hex.iter().map(String::as_str));
        self.run(&args).map(drop)
    }

    fn respawn(&self, pane: &str, command: &str, work: &Path) -> Outcome<()> {
        self.run(&[
            "respawn-pane",
            "-k",
            "-t",
            pane,
            "-c",
            &work.display().to_string(),
            command,
        ])
        .map(drop)
    }

    fn exited(&self, pane: &str) -> bool {
        self.run(&["display-message", "-p", "-t", pane, "#{pane_dead}"])
            .is_ok_and(|dead| dead.trim() == "1")
    }

    fn copy_mode(&self, pane: &str) -> Outcome<bool> {
        self.run(&["copy-mode", "-t", pane]).map(|_| true)
    }
}

impl Drop for Tmux {
    fn drop(&mut self) {
        let _ = self.run(&["kill-server"]);
    }
}
