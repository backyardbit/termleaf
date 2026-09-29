mod detect;
mod evidence;
mod herdr;
mod inject;
#[cfg(test)]
mod live;
mod nvim;
mod probe;
mod process;
#[cfg(target_os = "linux")]
mod procfs;
#[cfg(any(test, not(target_os = "linux")))]
mod ps;
mod safety;
mod tmux;

use std::path::{Path, PathBuf};

use detect::{Environment, MultiplexerKind, ProcessEnvironment, detect};
use evidence::{OnDisk, neovim_socket};
use herdr::Herdr;
use inject::{Failure, inject};
use nvim::{Rpc, RpcError, Socket};
use probe::{
    Candidate, Choice, Editor, EditorKind, LoadedFiles, Multiplexer, Refusal, Wanted, candidates,
    choose, own_pane, survey,
};
use process::{Pid, ProcessTable, ancestors};
use safety::recheck;
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

const PLAIN_LABEL: &str = "nvim (rpc)";

fn rpc_target(file: &Path) -> PathBuf {
    std::path::absolute(file).unwrap_or_else(|_| file.to_path_buf())
}

fn deliver(
    multiplexer: &impl Multiplexer,
    table: &impl ProcessTable,
    own: Option<&str>,
    candidate: &Candidate,
    at: &SourceLocation,
    rpc: &impl Rpc,
) -> Result<(), Failure> {
    if let (EditorKind::Neovim, Some(socket)) = (candidate.editor.kind, &candidate.editor.socket) {
        recheck(multiplexer, table, own, candidate)?;
        match rpc.jump(socket, &rpc_target(&at.file), at.line) {
            Ok(()) => return Ok(()),
            Err(RpcError::Failed(failure)) => return Err(failure),
            Err(RpcError::Unreachable(_)) => {}
        }
    }
    inject(multiplexer, table, own, candidate, at)
}

pub fn jump_with(
    multiplexer: &impl Multiplexer,
    table: &impl ProcessTable,
    own: Option<&str>,
    wanted: Wanted<'_>,
    loaded: &impl LoadedFiles,
    rpc: &impl Rpc,
) -> Jumped {
    let Ok(verdicts) = survey(multiplexer, table, own) else {
        return Jumped::NotChosen(Choice::NoEditor);
    };
    let found = candidates(multiplexer.kind(), &verdicts);
    match choose(found, wanted, own_pane(&verdicts), loaded) {
        Choice::Editor(candidate) => {
            match deliver(multiplexer, table, own, &candidate, wanted.at, rpc) {
                Ok(()) => Jumped::Sent(candidate.label()),
                Err(failure) => Jumped::Failed(candidate.label(), failure),
            }
        }
        unchosen => Jumped::NotChosen(unchosen),
    }
}

fn plain_neovims(table: &impl ProcessTable) -> Vec<(Editor, PathBuf)> {
    let own_uid = table.own_uid();
    table
        .foreground_leaders()
        .into_iter()
        .filter(|leader| {
            Some(leader.uid) == own_uid
                && EditorKind::of_program(&leader.program) == Some(EditorKind::Neovim)
        })
        .filter_map(|leader| {
            let socket = neovim_socket(table, leader.pid())?;
            let editor = Editor {
                kind: EditorKind::Neovim,
                socket: Some(socket.clone()),
                identity: leader.identity,
            };
            Some((editor, socket))
        })
        .collect()
}

pub fn jump_plain(
    table: &impl ProcessTable,
    wanted: Wanted<'_>,
    loaded: &impl LoadedFiles,
    rpc: &impl Rpc,
) -> Jumped {
    let found = plain_neovims(table);
    let holding: Vec<&(Editor, PathBuf)> = found
        .iter()
        .filter(|(editor, _)| loaded.holds(editor, &wanted.at.file))
        .collect();
    let chosen = if holding.is_empty() {
        found
            .iter()
            .filter(|(editor, _)| {
                wanted
                    .inputs
                    .iter()
                    .any(|input| loaded.holds(editor, input))
            })
            .collect()
    } else {
        holding
    };
    let socket = match chosen.as_slice() {
        [] => return Jumped::NotChosen(Choice::NoEditor),
        [(_, socket)] => socket,
        tied => return Jumped::NotChosen(Choice::Tie(tied.len())),
    };
    let label = PLAIN_LABEL.to_owned();
    match rpc.jump(socket, &rpc_target(&wanted.at.file), wanted.at.line) {
        Ok(()) => Jumped::Sent(label),
        Err(RpcError::Failed(failure)) => Jumped::Failed(label, failure),
        Err(RpcError::Unreachable(error)) => Jumped::Failed(label, Failure::Send(error)),
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
        let rpc = Socket;
        for layer in layers.iter().map(Some).chain([None]) {
            let own = layer.and_then(|layer| layer.own_pane.as_deref());
            let jumped = match layer.map(|layer| (layer.kind, layer.control.as_deref())) {
                Some((MultiplexerKind::Tmux, Some(socket))) => {
                    jump_with(self.tmux(socket, own), &table, own, wanted, &loaded, &rpc)
                }
                Some((MultiplexerKind::Herdr, Some(socket))) => {
                    let program = env
                        .var("HERDR_BIN_PATH")
                        .unwrap_or_else(|| "herdr".to_owned());
                    let herdr = Herdr::new(PathBuf::from(program), socket.to_path_buf());
                    jump_with(&herdr, &table, own, wanted, &loaded, &rpc)
                }
                Some(_) => continue,
                None => jump_plain(&table, wanted, &loaded, &rpc),
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

    use std::cell::RefCell;

    use super::inject::fake::Recorder;
    use super::probe::fake::layout;
    use super::probe::{Editor, Pane};
    use super::process::fake::{FakeTable, OUR_UID};

    struct Nvim {
        answer: fn() -> Result<(), RpcError>,
        calls: RefCell<Vec<(PathBuf, PathBuf, u32)>>,
    }

    impl Nvim {
        fn answering(answer: fn() -> Result<(), RpcError>) -> Self {
            Self {
                answer,
                calls: RefCell::new(Vec::new()),
            }
        }

        fn unreachable() -> Self {
            Self::answering(|| Err(RpcError::Unreachable(anyhow::anyhow!("no socket"))))
        }

        fn blocked() -> Self {
            Self::answering(|| Err(RpcError::Failed(Failure::Refused(Refusal::Blocked))))
        }

        fn sockets(&self) -> Vec<PathBuf> {
            self.calls
                .borrow()
                .iter()
                .map(|call| call.0.clone())
                .collect()
        }
    }

    impl Rpc for Nvim {
        fn jump(&self, socket: &Path, file: &Path, line: u32) -> Result<(), RpcError> {
            self.calls
                .borrow_mut()
                .push((socket.to_path_buf(), file.to_path_buf(), line));
            (self.answer)()
        }
    }

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
        rpc: &Nvim,
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
            rpc,
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
            &Nvim::unreachable(),
        );
        assert_eq!(status, "→ nvim in tmux %1 · ch5.tex:77");
        assert_eq!(
            recorder.sent_to("%1"),
            vec![b"\x1c\x0e:drop /tmp/thesis/ch5.tex | 77\r".to_vec()]
        );
        assert_eq!(recorder.sent.borrow().len(), 1);
    }

    #[test]
    fn a_neovim_with_a_socket_is_jumped_over_rpc_and_nothing_is_typed() {
        let nvim = Nvim::answering(|| Ok(()));
        let (status, recorder) = jump_in_layout(
            &ch5(77),
            &Holds(vec![(201, "/tmp/thesis/ch5.tex")]),
            &[],
            |_| true,
            &nvim,
        );
        assert_eq!(status, "→ nvim in tmux %1 · ch5.tex:77");
        assert!(recorder.sent.borrow().is_empty());
        assert_eq!(
            *nvim.calls.borrow(),
            [(
                PathBuf::from("/run/user/1000/nvim.202.0"),
                PathBuf::from("/tmp/thesis/ch5.tex"),
                77
            )]
        );
    }

    #[test]
    fn a_neovim_refusing_over_rpc_gets_nothing_typed_either() {
        let (status, recorder) = jump_in_layout(
            &ch5(77),
            &Holds(vec![(201, "/tmp/thesis/ch5.tex")]),
            &[],
            |_| true,
            &Nvim::blocked(),
        );
        assert_eq!(
            status,
            "ch5.tex:77 · nvim in tmux %1 refused: waiting at a prompt"
        );
        assert!(recorder.sent.borrow().is_empty());
    }

    fn terminals() -> FakeTable {
        let mut table = FakeTable::default();
        table
            .spawn(201, 1, "nvim")
            .foreground(201, 201)
            .spawn(202, 201, "nvim")
            .listens(202, "/run/user/1000/nvim.202.0")
            .spawn(301, 1, "nvim")
            .foreground(301, 301)
            .listens(301, "/tmp/n.sock")
            .spawn(400, 1, "nvim")
            .foreground(400, 400)
            .owned_by(400, OUR_UID + 1)
            .listens(400, "/tmp/theirs.sock")
            .spawn(500, 1, "vim")
            .foreground(500, 500)
            .listens(500, "/tmp/vim.sock")
            .spawn(600, 1, "nvim")
            .foreground(600, 600);
        table
    }

    fn jump_in_terminals(holds: &Holds, rpc: &Nvim) -> String {
        let main = PathBuf::from("/tmp/thesis/main.tex");
        jump_plain(
            &terminals(),
            Wanted {
                at: &ch5(77),
                inputs: &[main],
            },
            holds,
            rpc,
        )
        .status("ch5.tex:77")
    }

    #[test]
    fn plain_terminal_neovims_must_be_ours_have_a_socket_and_hold_an_input() {
        let nvim = Nvim::answering(|| Ok(()));
        let everyone = [201, 301, 400, 500, 600].map(|pid| (pid, "/tmp/elsewhere.tex"));
        let strangers = [400, 500, 600].map(|pid| (pid, "/tmp/thesis/ch5.tex"));
        for holds in [everyone.to_vec(), strangers.to_vec()] {
            let status = jump_in_terminals(&Holds(holds), &nvim);
            assert_eq!(status, "ch5.tex:77 · no editor found");
        }
        assert!(nvim.calls.borrow().is_empty());
    }

    #[test]
    fn the_plain_neovim_holding_the_file_wins_over_one_holding_another_input() {
        let nvim = Nvim::answering(|| Ok(()));
        let holds = Holds(vec![
            (301, "/tmp/thesis/main.tex"),
            (201, "/tmp/thesis/ch5.tex"),
        ]);
        assert_eq!(
            jump_in_terminals(&holds, &nvim),
            "→ nvim (rpc) · ch5.tex:77"
        );
        assert_eq!(
            *nvim.calls.borrow(),
            [(
                PathBuf::from("/run/user/1000/nvim.202.0"),
                PathBuf::from("/tmp/thesis/ch5.tex"),
                77
            )]
        );
    }

    #[test]
    fn two_plain_neovims_holding_inputs_tie_and_neither_is_jumped() {
        let nvim = Nvim::answering(|| Ok(()));
        let holds = Holds(vec![
            (201, "/tmp/thesis/main.tex"),
            (301, "/tmp/thesis/main.tex"),
        ]);
        assert_eq!(
            jump_in_terminals(&holds, &nvim),
            "2 editors could take ch5.tex:77"
        );
        assert!(nvim.calls.borrow().is_empty());
    }

    #[test]
    fn a_plain_neovim_that_refuses_or_cannot_be_reached_is_reported() {
        let holds = Holds(vec![(301, "/tmp/thesis/main.tex")]);
        let blocked = Nvim::blocked();
        assert_eq!(
            jump_in_terminals(&holds, &blocked),
            "ch5.tex:77 · nvim (rpc) refused: waiting at a prompt"
        );
        assert_eq!(blocked.sockets(), [PathBuf::from("/tmp/n.sock")]);
        assert_eq!(
            jump_in_terminals(&holds, &Nvim::unreachable()),
            "ch5.tex:77 · nvim (rpc) could not send: no socket"
        );
    }

    #[test]
    fn a_shell_pane_alone_gets_nothing_and_no_editor_is_found() {
        let (status, recorder) = jump_in_layout(
            &ch5(77),
            &Holds(Vec::new()),
            &[],
            |pane| pane.id == "%0" || pane.id == "%4",
            &Nvim::unreachable(),
        );
        assert_eq!(status, "ch5.tex:77 · no editor found");
        assert!(recorder.sent.borrow().is_empty());
    }

    #[test]
    fn a_path_the_editor_would_run_needs_rpc() {
        let at = SourceLocation {
            file: PathBuf::from("/tmp/a|b/ch5.tex"),
            line: 77,
        };
        let (status, recorder) = jump_in_layout(
            &at,
            &Holds(vec![(201, "/tmp/a|b/ch5.tex")]),
            &[],
            |_| true,
            &Nvim::unreachable(),
        );
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
            &Nvim::unreachable(),
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
