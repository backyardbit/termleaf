use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::detect::MultiplexerKind;
use super::evidence::neovim_socket;
use super::process::{Identity, Pid, Process, ProcessTable, is_shell};
use super::safety::Prompt;
use crate::synctex::SourceLocation;

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
    fn kind(&self) -> MultiplexerKind;
    fn panes(&self) -> Result<Vec<Pane>>;
    fn screen(&self, pane: &str) -> Option<String>;
    fn send(&self, pane: &str, input: &[u8], paste: bool) -> Result<()>;

    fn reveal(&self, _pane: &str) -> Result<()> {
        Ok(())
    }
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
    pub session: String,
    pub window: String,
    pub recency: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Editor {
    pub kind: EditorKind,
    pub identity: Identity,
    pub socket: Option<PathBuf>,
    pub via: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub multiplexer: MultiplexerKind,
    pub pane: Pane,
    pub editor: Editor,
}

impl Candidate {
    pub fn label(&self) -> String {
        let via = self
            .editor
            .via
            .as_ref()
            .map(|via| format!(" (via {via})"))
            .unwrap_or_default();
        format!(
            "{}{via} in {} {}",
            self.editor.kind.name(),
            self.multiplexer.name(),
            self.pane.id
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    OwnPane,
    PaneGone,
    NoForeground,
    Shell,
    Program(String),
    OtherUser,
    Replaced,
    Prompt(Prompt),
    Blocked,
    PathNeedsRpc,
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
            Self::Replaced => f.write_str("refused: not the same process"),
            Self::Prompt(prompt) => write!(f, "refused: {prompt} prompt"),
            Self::Blocked => f.write_str("refused: waiting at a prompt"),
            Self::PathNeedsRpc => f.write_str("path needs RPC"),
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
    let own_uid = table.own_uid();
    if Some(leader.uid) != own_uid {
        return Err(Refusal::OtherUser);
    }
    if let Some(kind) = EditorKind::of_program(&leader.program) {
        return Ok(editor_at(table, kind, &leader, None));
    }
    let Some((kind, waited_on)) = waited_on_editor(table, &leader) else {
        return Err(if is_shell(&leader.program) {
            Refusal::Shell
        } else {
            Refusal::Program(leader.program)
        });
    };
    if waited_on.iter().any(|process| Some(process.uid) != own_uid) {
        return Err(Refusal::OtherUser);
    }
    let editor = waited_on.first().ok_or(Refusal::NoForeground)?;
    Ok(editor_at(table, kind, editor, Some(leader.program)))
}

fn editor_at(
    table: &impl ProcessTable,
    kind: EditorKind,
    process: &Process,
    via: Option<String>,
) -> Editor {
    let socket = match kind {
        EditorKind::Neovim => neovim_socket(table, process.pid()),
        EditorKind::Vim | EditorKind::Helix => None,
    };
    Editor {
        kind,
        identity: process.identity,
        socket,
        via,
    }
}

fn lineage<'a>(members: &'a [Process], from: &'a Process) -> Vec<&'a Process> {
    let mut line = vec![from];
    while let Some(parent) = line.last().and_then(|last| {
        members
            .iter()
            .find(|member| member.pid() == last.parent && !line.contains(member))
    }) {
        line.push(parent);
    }
    line
}

fn waited_on_editor(
    table: &impl ProcessTable,
    leader: &Process,
) -> Option<(EditorKind, Vec<Process>)> {
    let members: Vec<Process> = table
        .processes()
        .into_iter()
        .filter(|process| process.group == leader.group)
        .collect();
    let is_editor = |process: &Process| EditorKind::of_program(&process.program).is_some();
    let outermost: Vec<&Process> = members
        .iter()
        .filter(|member| is_editor(member))
        .filter(|member| {
            !lineage(&members, member)
                .iter()
                .skip(1)
                .any(|up| is_editor(up))
        })
        .collect();
    let [editor] = outermost.as_slice() else {
        return None;
    };
    let kind = EditorKind::of_program(&editor.program)?;
    let waiting = lineage(&members, editor);
    if !waiting.iter().any(|process| process.pid() == leader.pid()) {
        return None;
    }
    let accounted = members.iter().all(|member| {
        waiting.iter().any(|up| up.pid() == member.pid())
            || lineage(&members, member)
                .iter()
                .any(|up| up.pid() == editor.pid())
    });
    accounted.then(|| (kind, waiting.into_iter().cloned().collect()))
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

pub fn candidates(multiplexer: MultiplexerKind, verdicts: &[Verdict]) -> Vec<Candidate> {
    verdicts
        .iter()
        .filter_map(|verdict| {
            Some(Candidate {
                multiplexer,
                pane: verdict.pane.clone(),
                editor: verdict.editor.clone().ok()?,
            })
        })
        .collect()
}

pub fn own_pane(verdicts: &[Verdict]) -> Option<&Pane> {
    verdicts
        .iter()
        .find(|verdict| verdict.editor == Err(Refusal::OwnPane))
        .map(|verdict| &verdict.pane)
}

pub trait LoadedFiles {
    fn holds(&self, editor: &Editor, file: &Path) -> bool;
}

#[derive(Debug, Clone, Copy)]
pub struct Wanted<'a> {
    pub at: &'a SourceLocation,
    pub inputs: &'a [PathBuf],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Rank {
    file: bool,
    inputs: bool,
    same_window: bool,
    same_session: bool,
    recency: u64,
}

fn rank(
    candidate: &Candidate,
    wanted: Wanted<'_>,
    here: Option<&Pane>,
    loaded: &impl LoadedFiles,
) -> Rank {
    let holds = |file: &Path| loaded.holds(&candidate.editor, file);
    let same_session = here.is_some_and(|here| here.session == candidate.pane.session);
    Rank {
        file: holds(&wanted.at.file),
        inputs: wanted.inputs.iter().any(|input| holds(input)),
        same_window: same_session && here.is_some_and(|here| here.window == candidate.pane.window),
        same_session,
        recency: candidate.pane.recency,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    Editor(Box<Candidate>),
    NoEditor,
    Tie(usize),
}

pub fn choose(
    candidates: Vec<Candidate>,
    wanted: Wanted<'_>,
    here: Option<&Pane>,
    loaded: &impl LoadedFiles,
) -> Choice {
    let mut ranked: Vec<(Rank, Candidate)> = candidates
        .into_iter()
        .map(|candidate| (rank(&candidate, wanted, here, loaded), candidate))
        .collect();
    ranked.sort_by_key(|(rank, _)| std::cmp::Reverse(*rank));
    let Some(best) = ranked.first().map(|(rank, _)| *rank) else {
        return Choice::NoEditor;
    };
    let tied = ranked.iter().filter(|(rank, _)| *rank == best).count();
    if tied > 1 {
        return Choice::Tie(tied);
    }
    ranked
        .into_iter()
        .next()
        .map_or(Choice::NoEditor, |(_, candidate)| {
            Choice::Editor(Box::new(candidate))
        })
}

impl Choice {
    pub fn status(&self, place: &str) -> Option<String> {
        match self {
            Self::Editor(_) => None,
            Self::NoEditor => Some(format!("{place} · no editor found")),
            Self::Tie(count) => Some(format!("{count} editors could take {place}")),
        }
    }
}

#[cfg(test)]
pub mod fake {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use super::super::process::fake::FakeTable;
    use super::*;

    pub struct FakeMultiplexer {
        pub panes: RefCell<Vec<Pane>>,
        pub screens: HashMap<String, String>,
    }

    impl FakeMultiplexer {
        pub fn new(panes: Vec<Pane>) -> Self {
            Self {
                panes: RefCell::new(panes),
                screens: HashMap::new(),
            }
        }
    }

    impl Multiplexer for FakeMultiplexer {
        fn kind(&self) -> MultiplexerKind {
            MultiplexerKind::Tmux
        }

        fn panes(&self) -> Result<Vec<Pane>> {
            Ok(self.panes.borrow().clone())
        }

        fn screen(&self, pane: &str) -> Option<String> {
            self.screens.get(pane).cloned()
        }

        fn send(&self, _pane: &str, _input: &[u8], _paste: bool) -> Result<()> {
            Ok(())
        }
    }

    pub fn pane(id: &str, shell: u32, window: &str) -> Pane {
        Pane {
            id: id.to_owned(),
            anchor: Anchor::Process(Pid(shell)),
            session: "thesis".to_owned(),
            window: window.to_owned(),
            recency: 0,
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
            pane("%0", 100, "1"),
            pane("%1", 200, "1"),
            pane("%2", 300, "1"),
            pane("%3", 400, "2"),
            pane("%4", 500, "1"),
        ]);
        (table, multiplexer)
    }
}

#[cfg(test)]
mod tests {
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
    fn a_pager_or_git_in_the_foreground_is_refused_by_name() {
        let mut table = FakeTable::default();
        table
            .spawn(10, 1, "zsh")
            .foreground(10, 11)
            .spawn(11, 10, "less");
        assert_eq!(
            editor_in(&table, &pane("%9", 10, "1")),
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
        assert_eq!(
            editor_in(&table, &pane("%9", 10, "1")),
            Err(Refusal::OtherUser)
        );
    }

    #[test]
    fn an_editor_running_in_the_background_of_a_shell_is_refused() {
        let mut table = FakeTable::default();
        table
            .spawn(10, 1, "bash")
            .foreground(10, 10)
            .spawn(11, 10, "vim");
        assert_eq!(editor_in(&table, &pane("%9", 10, "1")), Err(Refusal::Shell));
    }

    fn file_manager_pane() -> FakeTable {
        let mut table = FakeTable::default();
        table
            .spawn(10, 1, "fish")
            .foreground(10, 20)
            .spawn(20, 10, "yazi")
            .spawn(21, 20, "hx")
            .joins(21, 20)
            .spawn(22, 21, "texlab")
            .joins(22, 20);
        table
    }

    #[test]
    fn an_editor_opened_by_a_file_manager_is_found_through_it() {
        let table = file_manager_pane();
        let editor = editor_in(&table, &pane("%9", 10, "1")).expect("hx under yazi");
        assert_eq!(editor.kind, EditorKind::Helix);
        assert_eq!(editor.identity.pid, Pid(21));
        assert_eq!(editor.via.as_deref(), Some("yazi"));
        let candidate = Candidate {
            multiplexer: MultiplexerKind::Rex,
            pane: pane("block:hx", 10, "1"),
            editor,
        };
        assert_eq!(candidate.label(), "hx (via yazi) in rex block:hx");
    }

    #[test]
    fn an_editor_leading_its_group_is_not_reached_through_anything() {
        let (table, multiplexer) = layout();
        let helix = editor_in(&table, &multiplexer.panes().expect("panes")[3]).expect("hx");
        assert_eq!(helix.via, None);
    }

    #[test]
    fn a_neovim_under_a_file_manager_is_its_outer_process_with_the_inner_socket() {
        let mut table = FakeTable::default();
        table
            .spawn(10, 1, "fish")
            .foreground(10, 20)
            .spawn(20, 10, "yazi")
            .spawn(21, 20, "nvim")
            .joins(21, 20)
            .spawn(22, 21, "nvim")
            .joins(22, 20)
            .listens(22, "/run/user/1000/nvim.22.0");
        let editor = editor_in(&table, &pane("%9", 10, "1")).expect("nvim under yazi");
        assert_eq!(editor.identity.pid, Pid(21));
        assert_eq!(
            editor.socket,
            Some(PathBuf::from("/run/user/1000/nvim.22.0"))
        );
    }

    #[test]
    fn an_editor_run_by_a_script_shell_is_found_through_it() {
        let mut table = FakeTable::default();
        table
            .spawn(10, 1, "fish")
            .foreground(10, 20)
            .spawn(20, 10, "sh")
            .spawn(21, 20, "vim")
            .joins(21, 20);
        let editor = editor_in(&table, &pane("%9", 10, "1")).expect("vim under sh -c");
        assert_eq!(editor.via.as_deref(), Some("sh"));
    }

    #[test]
    fn a_group_with_a_process_beside_the_editor_is_refused() {
        let mut beside = file_manager_pane();
        beside.spawn(23, 20, "cat").joins(23, 20);
        assert_eq!(
            editor_in(&beside, &pane("%9", 10, "1")),
            Err(Refusal::Program("yazi".to_owned()))
        );
        let mut piped = FakeTable::default();
        piped
            .spawn(10, 1, "fish")
            .foreground(10, 20)
            .spawn(20, 10, "sh")
            .spawn(21, 20, "hx")
            .joins(21, 20)
            .spawn(22, 20, "tee")
            .joins(22, 20);
        assert_eq!(editor_in(&piped, &pane("%9", 10, "1")), Err(Refusal::Shell));
    }

    #[test]
    fn two_editors_side_by_side_in_one_group_are_refused() {
        let mut table = file_manager_pane();
        table.spawn(23, 20, "vim").joins(23, 20);
        assert_eq!(
            editor_in(&table, &pane("%9", 10, "1")),
            Err(Refusal::Program("yazi".to_owned()))
        );
    }

    #[test]
    fn an_editor_under_a_file_manager_of_another_user_is_refused() {
        let mut leader_elsewhere = file_manager_pane();
        leader_elsewhere.owned_by(20, OUR_UID + 1);
        assert_eq!(
            editor_in(&leader_elsewhere, &pane("%9", 10, "1")),
            Err(Refusal::OtherUser)
        );
        let mut editor_elsewhere = file_manager_pane();
        editor_elsewhere.owned_by(21, OUR_UID + 1);
        assert_eq!(
            editor_in(&editor_elsewhere, &pane("%9", 10, "1")),
            Err(Refusal::OtherUser)
        );
    }

    #[test]
    fn a_pane_whose_process_is_gone_is_refused() {
        let table = FakeTable::default();
        assert_eq!(
            editor_in(&table, &pane("%9", 10, "1")),
            Err(Refusal::PaneGone)
        );
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
            ..pane("4", 0, "1")
        };
        assert_eq!(
            editor_in(&table, &by_tty).map(|editor| editor.kind),
            Ok(EditorKind::Helix)
        );
    }

    struct Holding(Vec<(u32, PathBuf)>);

    impl LoadedFiles for Holding {
        fn holds(&self, editor: &Editor, file: &Path) -> bool {
            self.0
                .iter()
                .any(|(pid, held)| Pid(*pid) == editor.identity.pid && held == file)
        }
    }

    fn at(file: &str, line: u32) -> SourceLocation {
        SourceLocation {
            file: PathBuf::from(file),
            line,
        }
    }

    fn inputs() -> Vec<PathBuf> {
        ["/t/thesis.tex", "/t/ch1.tex", "/t/ch5.tex"]
            .map(PathBuf::from)
            .to_vec()
    }

    fn pick(holding: &[(u32, &str)], at: &SourceLocation) -> Choice {
        let (table, multiplexer) = layout();
        let verdicts = survey(&multiplexer, &table, Some("%0")).expect("panes");
        let inputs = inputs();
        let loaded = Holding(
            holding
                .iter()
                .map(|(pid, file)| (*pid, PathBuf::from(file)))
                .collect(),
        );
        choose(
            candidates(multiplexer.kind(), &verdicts),
            Wanted {
                at,
                inputs: &inputs,
            },
            own_pane(&verdicts),
            &loaded,
        )
    }

    fn chosen(choice: &Choice) -> Option<String> {
        match choice {
            Choice::Editor(candidate) => Some(candidate.label()),
            Choice::NoEditor | Choice::Tie(_) => None,
        }
    }

    #[test]
    fn an_editor_with_another_input_of_the_pdf_beats_one_without() {
        let choice = pick(&[(401, "/t/thesis.tex")], &at("/t/ch5.tex", 77));
        assert_eq!(chosen(&choice).as_deref(), Some("hx in tmux %3"));
    }

    #[test]
    fn a_tie_at_every_step_chooses_nothing_and_says_how_many() {
        let location = at("/t/ch5.tex", 77);
        let choice = pick(&[(201, "/t/ch5.tex"), (301, "/t/ch5.tex")], &location);
        assert_eq!(choice, Choice::Tie(2));
        assert_eq!(
            choice.status("ch5.tex:77").as_deref(),
            Some("2 editors could take ch5.tex:77")
        );
    }

    #[test]
    fn no_editor_says_so_after_the_source_location() {
        let location = at("/t/ch5.tex", 77);
        let choice = choose(
            Vec::new(),
            Wanted {
                at: &location,
                inputs: &[],
            },
            None,
            &Holding(Vec::new()),
        );
        assert_eq!(choice, Choice::NoEditor);
        assert_eq!(
            choice.status("ch5.tex:77").as_deref(),
            Some("ch5.tex:77 · no editor found")
        );
    }

    #[test]
    fn a_chosen_editor_needs_no_status() {
        let location = at("/t/ch5.tex", 77);
        let choice = pick(&[(301, "/t/ch5.tex")], &location);
        assert_eq!(choice.status("ch5.tex:77"), None);
    }

    #[test]
    fn refusals_read_well_in_the_status_bar() {
        assert_eq!(Refusal::Shell.to_string(), "refused: shell");
        assert_eq!(
            Refusal::Program("git".to_owned()).to_string(),
            "refused: git"
        );
        assert_eq!(Refusal::PathNeedsRpc.to_string(), "path needs RPC");
        assert_eq!(
            Refusal::Prompt(Prompt::PressEnter).to_string(),
            "refused: Press ENTER prompt"
        );
    }
}
