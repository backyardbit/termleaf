use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::evidence::neovim_socket;
use super::process::{Identity, Pid, Process, ProcessTable, is_shell};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorKind {
    Helix,
    Vim,
    Neovim,
}

impl EditorKind {
    pub fn of_program(program: &str) -> Option<Self> {
        match program {
            "hx" | "helix" => Some(Self::Helix),
            "nvim" => Some(Self::Neovim),
            vim if vim.starts_with("vim") => Some(Self::Vim),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Helix => "hx",
            Self::Vim => "vim",
            Self::Neovim => "nvim",
        }
    }
}

pub trait Multiplexer {
    fn panes(&self) -> Result<Vec<Pane>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Anchor {
    Process(Pid),
    Tty(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pane {
    pub id: String,
    pub anchor: Anchor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Editor {
    pub kind: EditorKind,
    pub identity: Identity,
    pub socket: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    OwnPane,
    PaneGone,
    NoForeground,
    Shell,
    Program(String),
    OtherUser,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OwnPane => f.write_str("refused: termleaf's own pane"),
            Self::PaneGone => f.write_str("refused: pane is gone"),
            Self::NoForeground => f.write_str("refused: nothing in the foreground"),
            Self::Shell => f.write_str("refused: shell"),
            Self::Program(program) => write!(f, "refused: {program}"),
            Self::OtherUser => f.write_str("refused: another user's process"),
        }
    }
}

pub fn foreground_leader(table: &impl ProcessTable, anchor: &Anchor) -> Result<Process, Refusal> {
    let member = match anchor {
        Anchor::Process(pid) => table.process(*pid),
        Anchor::Tty(tty) => table.on_tty(tty),
    }
    .ok_or(Refusal::PaneGone)?;
    let group = member.foreground_group.ok_or(Refusal::NoForeground)?;
    table
        .process(group)
        .filter(|leader| leader.group == group)
        .ok_or(Refusal::NoForeground)
}

pub fn editor_in(table: &impl ProcessTable, pane: &Pane) -> Result<Editor, Refusal> {
    let leader = foreground_leader(table, &pane.anchor)?;
    if Some(leader.uid) != table.own_uid() {
        return Err(Refusal::OtherUser);
    }
    let Some(kind) = EditorKind::of_program(&leader.program) else {
        return Err(if is_shell(&leader.program) {
            Refusal::Shell
        } else {
            Refusal::Program(leader.program)
        });
    };
    let socket = match kind {
        EditorKind::Neovim => neovim_socket(table, leader.pid()),
        EditorKind::Vim | EditorKind::Helix => None,
    };
    Ok(Editor {
        kind,
        identity: leader.identity,
        socket,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub pane: Pane,
    pub editor: Result<Editor, Refusal>,
}

pub fn survey(
    multiplexer: &impl Multiplexer,
    table: &impl ProcessTable,
    own_pane: Option<&str>,
) -> Result<Vec<Verdict>> {
    Ok(multiplexer
        .panes()?
        .into_iter()
        .map(|pane| {
            let editor = if own_pane == Some(pane.id.as_str()) {
                Err(Refusal::OwnPane)
            } else {
                editor_in(table, &pane)
            };
            Verdict { pane, editor }
        })
        .collect())
}

pub trait LoadedFiles {
    fn holds(&self, editor: &Editor, file: &Path) -> bool;
}

#[cfg(test)]
pub mod fake {
    use std::cell::RefCell;

    use super::super::process::fake::FakeTable;
    use super::*;

    pub struct FakeMultiplexer {
        pub panes: RefCell<Vec<Pane>>,
    }

    impl FakeMultiplexer {
        pub fn new(panes: Vec<Pane>) -> Self {
            Self {
                panes: RefCell::new(panes),
            }
        }
    }

    impl Multiplexer for FakeMultiplexer {
        fn panes(&self) -> Result<Vec<Pane>> {
            Ok(self.panes.borrow().clone())
        }
    }

    pub fn pane(id: &str, shell: u32) -> Pane {
        Pane {
            id: id.to_owned(),
            anchor: Anchor::Process(Pid(shell)),
        }
    }

    pub fn layout() -> (FakeTable, FakeMultiplexer) {
        let mut table = FakeTable::default();
        table
            .spawn(100, 1, "bash")
            .foreground(100, 101)
            .spawn(101, 100, "termleaf")
            .spawn(200, 1, "bash")
            .foreground(200, 201)
            .spawn(201, 200, "nvim")
            .spawn(202, 201, "nvim")
            .listens(202, "/run/user/1000/nvim.202.0")
            .spawn(300, 1, "bash")
            .foreground(300, 301)
            .spawn(301, 300, "vim.gtk3")
            .spawn(400, 1, "bash")
            .foreground(400, 401)
            .spawn(401, 400, "hx")
            .spawn(500, 1, "bash")
            .foreground(500, 500);
        let multiplexer = FakeMultiplexer::new(vec![
            pane("%0", 100),
            pane("%1", 200),
            pane("%2", 300),
            pane("%3", 400),
            pane("%4", 500),
        ]);
        (table, multiplexer)
    }
}

#[cfg(test)]
mod tests {
    use super::super::process::StartTime;
    use super::super::process::fake::{FakeTable, OUR_UID};
    use super::fake::{layout, pane};
    use super::*;

    fn verdicts() -> Vec<(String, Result<EditorKind, Refusal>)> {
        let (table, multiplexer) = layout();
        survey(&multiplexer, &table, Some("%0"))
            .expect("the fake lists panes")
            .into_iter()
            .map(|verdict| (verdict.pane.id, verdict.editor.map(|editor| editor.kind)))
            .collect()
    }

    #[test]
    fn editors_are_known_by_their_executable() {
        assert_eq!(EditorKind::of_program("hx"), Some(EditorKind::Helix));
        assert_eq!(EditorKind::of_program("helix"), Some(EditorKind::Helix));
        assert_eq!(EditorKind::of_program("nvim"), Some(EditorKind::Neovim));
        assert_eq!(EditorKind::of_program("vim"), Some(EditorKind::Vim));
        assert_eq!(EditorKind::of_program("vim.gtk3"), Some(EditorKind::Vim));
        assert_eq!(EditorKind::of_program("vi"), None);
        assert_eq!(EditorKind::of_program("nvim-qt"), None);
        assert_eq!(EditorKind::of_program("less"), None);
    }

    #[test]
    fn editors_are_named_like_the_commands_that_start_them() {
        assert_eq!(
            [EditorKind::Helix, EditorKind::Vim, EditorKind::Neovim].map(EditorKind::name),
            ["hx", "vim", "nvim"]
        );
    }

    #[test]
    fn every_pane_gets_a_verdict_and_the_shell_is_refused() {
        assert_eq!(
            verdicts(),
            [
                ("%0".to_owned(), Err(Refusal::OwnPane)),
                ("%1".to_owned(), Ok(EditorKind::Neovim)),
                ("%2".to_owned(), Ok(EditorKind::Vim)),
                ("%3".to_owned(), Ok(EditorKind::Helix)),
                ("%4".to_owned(), Err(Refusal::Shell)),
            ]
        );
    }

    #[test]
    fn the_editor_is_identified_by_pid_and_start_time() {
        let (table, multiplexer) = layout();
        let panes = multiplexer.panes().expect("panes");
        let editor = editor_in(&table, &panes[2]).expect("vim");
        assert_eq!(
            editor.identity,
            Identity {
                pid: Pid(301),
                start: StartTime(3010)
            }
        );
    }

    #[test]
    fn neovim_brings_the_socket_of_its_embedded_child() {
        let (table, multiplexer) = layout();
        let panes = multiplexer.panes().expect("panes");
        let editor = editor_in(&table, &panes[1]).expect("nvim");
        assert_eq!(
            editor.socket,
            Some(PathBuf::from("/run/user/1000/nvim.202.0"))
        );
    }

    #[test]
    fn a_pager_or_git_in_the_foreground_is_refused_by_name() {
        let mut table = FakeTable::default();
        table
            .spawn(10, 1, "zsh")
            .foreground(10, 11)
            .spawn(11, 10, "less");
        assert_eq!(
            editor_in(&table, &pane("%9", 10)),
            Err(Refusal::Program("less".to_owned()))
        );
    }

    #[test]
    fn an_editor_of_another_user_is_refused() {
        let mut table = FakeTable::default();
        table
            .spawn(10, 1, "bash")
            .foreground(10, 11)
            .spawn(11, 10, "nvim")
            .owned_by(11, OUR_UID + 1);
        assert_eq!(editor_in(&table, &pane("%9", 10)), Err(Refusal::OtherUser));
    }

    #[test]
    fn an_editor_running_in_the_background_of_a_shell_is_refused() {
        let mut table = FakeTable::default();
        table
            .spawn(10, 1, "bash")
            .foreground(10, 10)
            .spawn(11, 10, "vim");
        assert_eq!(editor_in(&table, &pane("%9", 10)), Err(Refusal::Shell));
    }

    #[test]
    fn a_foreground_group_whose_leader_is_gone_is_refused() {
        let mut table = FakeTable::default();
        table
            .spawn(10, 1, "bash")
            .foreground(10, 11)
            .spawn(12, 10, "vim")
            .in_group(12, 11);
        assert_eq!(
            editor_in(&table, &pane("%9", 10)),
            Err(Refusal::NoForeground)
        );
    }

    #[test]
    fn a_pane_whose_process_is_gone_is_refused() {
        let table = FakeTable::default();
        assert_eq!(editor_in(&table, &pane("%9", 10)), Err(Refusal::PaneGone));
    }

    #[test]
    fn a_pane_known_only_by_its_tty_is_probed_through_the_tty() {
        let mut table = FakeTable::default();
        table
            .spawn(10, 1, "bash")
            .foreground(10, 11)
            .spawn(11, 10, "hx")
            .foreground(11, 11)
            .tty("/dev/pts/4", 11);
        let by_tty = Pane {
            anchor: Anchor::Tty(PathBuf::from("/dev/pts/4")),
            ..pane("4", 0)
        };
        assert_eq!(
            editor_in(&table, &by_tty).map(|editor| editor.kind),
            Ok(EditorKind::Helix)
        );
    }

    #[test]
    fn refusals_read_well_in_the_status_bar() {
        assert_eq!(Refusal::Shell.to_string(), "refused: shell");
        assert_eq!(
            Refusal::Program("git".to_owned()).to_string(),
            "refused: git"
        );
    }
}
