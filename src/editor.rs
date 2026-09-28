mod detect;
mod evidence;
mod inject;
#[cfg(test)]
mod live;
mod probe;
mod process;
#[cfg(target_os = "linux")]
mod procfs;
#[cfg(any(test, not(target_os = "linux")))]
mod ps;
mod safety;
mod tmux;

use std::path::{Path, PathBuf};

use detect::{MultiplexerKind, ProcessEnvironment, detect};
use evidence::OnDisk;
use inject::{Failure, inject};
use probe::{
    Choice, LoadedFiles, Multiplexer, Refusal, Wanted, candidates, choose, own_pane, survey,
};
use process::{Pid, ProcessTable, ancestors};
use tmux::Tmux;

use crate::inverse::Editors;
use crate::synctex::SourceLocation;

#[cfg(target_os = "linux")]
fn system_processes() -> anyhow::Result<impl ProcessTable> {
    Ok(procfs::Procfs::default())
}

#[cfg(not(target_os = "linux"))]
fn system_processes() -> anyhow::Result<impl ProcessTable> {
    ps::Ps::read()
}

#[derive(Debug)]
pub enum Jumped {
    Sent(String),
    NotChosen(Choice),
    Failed(String, Failure),
}

impl Jumped {
    pub fn status(&self, place: &str) -> String {
        match self {
            Self::Sent(label) => format!("→ {label} · {place}"),
            Self::NotChosen(choice) => choice.status(place).unwrap_or_default(),
            Self::Failed(_, Failure::Refused(Refusal::PathNeedsRpc)) => {
                format!("{place} · path needs RPC")
            }
            Self::Failed(label, failure) => format!("{place} · {label} {failure}"),
        }
    }
}

pub fn jump_with(
    multiplexer: &impl Multiplexer,
    table: &impl ProcessTable,
    own: Option<&str>,
    wanted: Wanted<'_>,
    loaded: &impl LoadedFiles,
) -> Jumped {
    let Ok(verdicts) = survey(multiplexer, table, own) else {
        return Jumped::NotChosen(Choice::NoEditor);
    };
    let found = candidates(multiplexer.kind(), &verdicts);
    match choose(found, wanted, own_pane(&verdicts), loaded) {
        Choice::Editor(candidate) => match inject(multiplexer, table, own, &candidate, wanted.at) {
            Ok(()) => Jumped::Sent(candidate.label()),
            Err(failure) => Jumped::Failed(candidate.label(), failure),
        },
        unchosen => Jumped::NotChosen(unchosen),
    }
}

#[derive(Default)]
pub struct Jumper {
    tmux: Option<(PathBuf, Option<String>, Tmux)>,
}

impl Jumper {
    fn tmux(&mut self, socket: &Path, own: Option<&str>) -> &Tmux {
        let reusable = self
            .tmux
            .as_ref()
            .is_some_and(|(held, held_own, _)| held == socket && held_own.as_deref() == own);
        if !reusable {
            self.tmux = Some((
                socket.to_path_buf(),
                own.map(str::to_owned),
                Tmux::new(socket.to_path_buf(), own.map(str::to_owned)),
            ));
        }
        let (_, _, tmux) = self.tmux.as_ref().expect("the tmux adapter was just set");
        tmux
    }

    fn jump_at(&mut self, at: &SourceLocation, inputs: &[PathBuf]) -> Jumped {
        let env = ProcessEnvironment;
        let Ok(table) = system_processes() else {
            return Jumped::NotChosen(Choice::NoEditor);
        };
        let layers = detect(&env, &ancestors(&table, Pid(std::process::id())));
        let loaded = OnDisk::new(&table, &env);
        let wanted = Wanted { at, inputs };
        for layer in &layers {
            let own = layer.own_pane.as_deref();
            let jumped = match (layer.kind, layer.control.as_deref()) {
                (MultiplexerKind::Tmux, Some(socket)) => {
                    jump_with(self.tmux(socket, own), &table, own, wanted, &loaded)
                }
                _ => continue,
            };
            if !matches!(jumped, Jumped::NotChosen(Choice::NoEditor)) {
                return jumped;
            }
        }
        Jumped::NotChosen(Choice::NoEditor)
    }
}

impl Editors for Jumper {
    fn jump(&mut self, at: &SourceLocation, inputs: &[PathBuf], place: &str) -> String {
        self.jump_at(at, inputs).status(place)
    }
}

#[cfg(test)]
mod tests {
    use super::process::Pid;
    use super::*;

    use super::inject::fake::Recorder;
    use super::probe::fake::layout;
    use super::probe::{Editor, Pane};

    struct Holds(Vec<(u32, &'static str)>);

    impl LoadedFiles for Holds {
        fn holds(&self, editor: &Editor, file: &Path) -> bool {
            self.0
                .iter()
                .any(|(pid, held)| editor.identity.pid == Pid(*pid) && file == Path::new(held))
        }
    }

    fn ch5(line: u32) -> SourceLocation {
        SourceLocation {
            file: PathBuf::from("/tmp/thesis/ch5.tex"),
            line,
        }
    }

    fn jump_in_layout(
        at: &SourceLocation,
        holds: &Holds,
        screens: &[(&str, &str)],
        keep: impl Fn(&Pane) -> bool,
    ) -> (String, Recorder) {
        let (table, mut multiplexer) = layout();
        multiplexer.panes.borrow_mut().retain(|pane| keep(pane));
        for (pane, screen) in screens {
            multiplexer
                .screens
                .insert((*pane).to_owned(), (*screen).to_owned());
        }
        let recorder = Recorder::new(multiplexer);
        let jumped = jump_with(
            &recorder,
            &table,
            Some("%0"),
            Wanted { at, inputs: &[] },
            holds,
        );
        (jumped.status("ch5.tex:77"), recorder)
    }

    #[test]
    fn the_editor_holding_the_file_takes_the_jump_and_says_so() {
        let (status, recorder) = jump_in_layout(
            &ch5(77),
            &Holds(vec![(201, "/tmp/thesis/ch5.tex")]),
            &[],
            |_| true,
        );
        assert_eq!(status, "→ nvim in tmux %1 · ch5.tex:77");
        assert_eq!(
            recorder.sent_to("%1"),
            vec![b"\x1c\x0e:drop /tmp/thesis/ch5.tex | 77\r".to_vec()]
        );
        assert_eq!(recorder.sent.borrow().len(), 1);
    }

    #[test]
    fn a_shell_pane_alone_gets_nothing_and_no_editor_is_found() {
        let (status, recorder) = jump_in_layout(&ch5(77), &Holds(Vec::new()), &[], |pane| {
            pane.id == "%0" || pane.id == "%4"
        });
        assert_eq!(status, "ch5.tex:77 · no editor found");
        assert!(recorder.sent.borrow().is_empty());
    }

    #[test]
    fn a_path_the_editor_would_run_needs_rpc() {
        let at = SourceLocation {
            file: PathBuf::from("/tmp/a|b/ch5.tex"),
            line: 77,
        };
        let (status, recorder) =
            jump_in_layout(&at, &Holds(vec![(201, "/tmp/a|b/ch5.tex")]), &[], |_| true);
        assert_eq!(status, "ch5.tex:77 · path needs RPC");
        assert!(recorder.sent.borrow().is_empty());
    }

    #[test]
    fn a_blocking_prompt_is_named_with_the_editor_it_stopped() {
        let (status, recorder) = jump_in_layout(
            &ch5(77),
            &Holds(vec![(301, "/tmp/thesis/ch5.tex")]),
            &[("%2", "~\nPress ENTER or type command to continue\n")],
            |_| true,
        );
        assert_eq!(
            status,
            "ch5.tex:77 · vim in tmux %2 refused: Press ENTER prompt"
        );
        assert!(recorder.sent.borrow().is_empty());
    }

    #[test]
    fn the_system_process_table_knows_our_own_process() {
        let table = system_processes().expect("the process table");
        let us = table
            .process(Pid(std::process::id()))
            .expect("our own process");
        assert_eq!(table.own_uid(), Some(us.uid));
    }
}
