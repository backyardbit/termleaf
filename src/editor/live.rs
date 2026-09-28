use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::detect::{Environment, MultiplexerKind, ProcessEnvironment, detect};
use super::evidence::OnDisk;
use super::probe::{
    Anchor, Choice, Editor, LoadedFiles, Multiplexer, Pane, Wanted, candidates, choose, own_pane,
    survey,
};
use super::process::{Pid, ProcessTable, ancestors};
use super::safety::{injectable, recheck};
use super::system_processes;
use crate::synctex::SourceLocation;

const PANE_FORMAT: &str = "#{pane_id}\t#{pane_tty}\t#{session_name}\t#{window_index}\t#{pane_last}";

struct LiveTmux {
    socket: PathBuf,
}

impl LiveTmux {
    fn run(&self, arguments: &[&str]) -> Result<String> {
        let output = Command::new("tmux")
            .arg("-S")
            .arg(&self.socket)
            .args(arguments)
            .output()
            .context("could not run tmux")?;
        if !output.status.success() {
            bail!("tmux {} failed", arguments.join(" "));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

impl Multiplexer for LiveTmux {
    fn kind(&self) -> MultiplexerKind {
        MultiplexerKind::Tmux
    }

    fn panes(&self) -> Result<Vec<Pane>> {
        Ok(self
            .run(&["list-panes", "-a", "-F", PANE_FORMAT])?
            .lines()
            .filter_map(|line| {
                let fields: Vec<&str> = line.split('\t').collect();
                let [id, tty, session, window, last] = fields.as_slice() else {
                    return None;
                };
                Some(Pane {
                    id: (*id).to_owned(),
                    anchor: Anchor::Tty(PathBuf::from(tty)),
                    session: (*session).to_owned(),
                    window: (*window).to_owned(),
                    recency: last.parse().unwrap_or_default(),
                })
            })
            .collect())
    }

    fn screen(&self, pane: &str) -> Option<String> {
        self.run(&["capture-pane", "-p", "-t", pane]).ok()
    }

    fn send(&self, _pane: &str, _input: &[u8], _paste: bool) -> Result<()> {
        bail!("the live tmux lister only reads")
    }
}

fn locations(env: &impl Environment) -> Vec<SourceLocation> {
    env.var("TERMLEAF_PROBE_AT")
        .unwrap_or_default()
        .split(',')
        .filter_map(|entry| {
            let (file, line) = entry.rsplit_once(':')?;
            Some(SourceLocation {
                file: PathBuf::from(file),
                line: line.parse().ok()?,
            })
        })
        .collect()
}

fn short(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

fn describe<T: ProcessTable>(editor: &Editor, inputs: &[PathBuf], disk: &OnDisk<'_, T>) -> String {
    let held: Vec<String> = inputs
        .iter()
        .filter(|input| disk.holds(editor, input))
        .map(|input| {
            let how = disk.swap_file(editor, input).map_or_else(
                || "argv".to_owned(),
                |swap| format!("swap {}", short(&swap)),
            );
            format!("{} ({how})", short(input))
        })
        .collect();
    let socket = editor
        .socket
        .as_ref()
        .map(|socket| format!("  socket {}", socket.display()))
        .unwrap_or_default();
    let held = if held.is_empty() {
        "-".to_owned()
    } else {
        held.join(", ")
    };
    format!(
        "{:<5} pid {} start {}  holds {held}{socket}",
        editor.kind.name(),
        editor.identity.pid.0,
        editor.identity.start.0,
    )
}

#[test]
#[ignore = "needs a live tmux session with editors in its panes; run from one of its panes"]
fn probe_every_pane_of_the_live_tmux_session() {
    let env = ProcessEnvironment;
    let table = system_processes().expect("the process table");
    let layers = detect(&env, &ancestors(&table, Pid(std::process::id())));
    let tmux = layers
        .iter()
        .find(|layer| layer.kind == MultiplexerKind::Tmux)
        .expect("run this test inside tmux");
    let own = tmux.own_pane.as_deref();
    let names: Vec<String> = layers
        .iter()
        .map(|layer| {
            format!(
                "{} {}",
                layer.kind.name(),
                layer.own_pane.as_deref().unwrap_or("?")
            )
        })
        .collect();
    println!("\ntermleaf runs in: {}", names.join(", inside "));
    let multiplexer = LiveTmux {
        socket: tmux.control.clone().expect("the tmux socket"),
    };
    let verdicts = survey(&multiplexer, &table, own).expect("tmux lists its panes");
    let at = locations(&env);
    let inputs: Vec<PathBuf> = at.iter().map(|location| location.file.clone()).collect();
    let disk = OnDisk::new(&table, &env);

    println!("\npane  verdict");
    for verdict in &verdicts {
        let line = match &verdict.editor {
            Err(refusal) => refusal.to_string(),
            Ok(editor) => describe(editor, &inputs, &disk),
        };
        println!("{:<5} {line}", verdict.pane.id);
    }

    let found = candidates(multiplexer.kind(), &verdicts);
    println!("\nwho takes the jump");
    for location in &at {
        let choice = choose(
            found.clone(),
            Wanted {
                at: location,
                inputs: &inputs,
            },
            own_pane(&verdicts),
            &disk,
        );
        let text = match &choice {
            Choice::Editor(candidate) => format!(
                "{}:{} -> {}",
                short(&location.file),
                location.line,
                candidate.label()
            ),
            Choice::NoEditor | Choice::Tie(_) => choice.status(location).unwrap_or_default(),
        };
        println!("  {text}");
    }

    println!("\nchecks right before a send");
    for candidate in &found {
        let verdict = recheck(&multiplexer, &table, own, candidate)
            .map_or_else(|refusal| refusal.to_string(), |()| "ok".to_owned());
        println!("  {:<16} {verdict}", candidate.label());
    }
    let evil = inputs
        .first()
        .and_then(|input| input.parent())
        .unwrap_or(Path::new("/tmp"))
        .join("$(id).tex");
    for path in inputs.iter().take(1).chain([&evil]) {
        let verdict = injectable(path)
            .map_or_else(|refusal| refusal.to_string(), |()| "ok to type".to_owned());
        println!("  path {:<16} {verdict}", short(path));
    }

    assert!(verdicts.iter().any(|verdict| verdict.editor.is_err()));
}
