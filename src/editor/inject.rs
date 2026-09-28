use std::fmt;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use super::probe::{Candidate, EditorKind, Multiplexer, Refusal};
use super::process::ProcessTable;
use super::safety::{injectable, recheck};
use crate::synctex::SourceLocation;

const HELIX_GAP: Duration = Duration::from_millis(50);
const VIM_ESCAPED: &[char] = &[
    ' ', '\t', '\\', '*', '?', '[', '{', '`', '$', '%', '#', '\'', '"', '|', '!', '<',
];

#[derive(Debug)]
pub enum Failure {
    Refused(Refusal),
    Send(anyhow::Error),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(refusal) => refusal.fmt(f),
            Self::Send(error) => write!(f, "could not send: {error}"),
        }
    }
}

impl From<Refusal> for Failure {
    fn from(refusal: Refusal) -> Self {
        Self::Refused(refusal)
    }
}

pub fn target(file: &Path) -> Result<PathBuf, Refusal> {
    let absolute = std::path::absolute(file).map_err(|_| Refusal::PathNeedsRpc)?;
    injectable(&absolute)?;
    Ok(absolute)
}

fn vim_escape(path: &str) -> String {
    let mut escaped = String::with_capacity(path.len());
    for character in path.chars() {
        if VIM_ESCAPED.contains(&character) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

fn helix_argument(path: &str, line: u32) -> String {
    let argument = format!("{path}:{line}:1");
    if path.contains(char::is_whitespace) || path.contains('\\') {
        format!("'{argument}'")
    } else {
        argument
    }
}

pub fn strokes(kind: EditorKind, file: &Path, line: u32) -> Vec<Vec<u8>> {
    let path = file.to_string_lossy();
    match kind {
        EditorKind::Vim | EditorKind::Neovim => {
            vec![format!("\x1c\x0e:drop {} | {line}\r", vim_escape(&path)).into_bytes()]
        }
        EditorKind::Helix => vec![
            b"\x1b".to_vec(),
            format!(":open {}\r", helix_argument(&path, line)).into_bytes(),
        ],
    }
}

pub fn inject(
    multiplexer: &impl Multiplexer,
    table: &impl ProcessTable,
    own_pane: Option<&str>,
    locked: &Candidate,
    at: &SourceLocation,
) -> Result<(), Failure> {
    let file = target(&at.file)?;
    for (index, stroke) in strokes(locked.editor.kind, &file, at.line)
        .iter()
        .enumerate()
    {
        if index > 0 {
            thread::sleep(HELIX_GAP);
        }
        recheck(multiplexer, table, own_pane, locked)?;
        multiplexer
            .send(&locked.pane.id, stroke, false)
            .map_err(Failure::Send)?;
    }
    Ok(())
}

#[cfg(test)]
pub mod fake {
    use std::cell::RefCell;

    use anyhow::{Result, bail};

    use super::super::detect::MultiplexerKind;
    use super::super::probe::fake::FakeMultiplexer;
    use super::super::probe::{Multiplexer, Pane};

    pub struct Recorder {
        pub inner: FakeMultiplexer,
        pub sent: RefCell<Vec<(String, Vec<u8>, bool)>>,
        pub broken: bool,
    }

    impl Recorder {
        pub fn new(inner: FakeMultiplexer) -> Self {
            Self {
                inner,
                sent: RefCell::new(Vec::new()),
                broken: false,
            }
        }

        pub fn sent_to(&self, pane: &str) -> Vec<Vec<u8>> {
            self.sent
                .borrow()
                .iter()
                .filter(|(to, _, _)| to == pane)
                .map(|(_, bytes, _)| bytes.clone())
                .collect()
        }
    }

    impl Multiplexer for Recorder {
        fn kind(&self) -> MultiplexerKind {
            self.inner.kind()
        }

        fn panes(&self) -> Result<Vec<Pane>> {
            self.inner.panes()
        }

        fn screen(&self, pane: &str) -> Option<String> {
            self.inner.screen(pane)
        }

        fn send(&self, pane: &str, input: &[u8], paste: bool) -> Result<()> {
            if self.broken {
                bail!("the server went away");
            }
            self.sent
                .borrow_mut()
                .push((pane.to_owned(), input.to_vec(), paste));
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::super::probe::fake::layout;
    use super::super::probe::{candidates, survey};
    use super::super::process::fake::FakeTable;
    use super::fake::Recorder;
    use super::*;

    fn at(file: &str, line: u32) -> SourceLocation {
        SourceLocation {
            file: PathBuf::from(file),
            line,
        }
    }

    fn locked(id: &str) -> (FakeTable, Recorder, Candidate) {
        let (table, multiplexer) = layout();
        let verdicts = survey(&multiplexer, &table, Some("%0")).expect("panes");
        let candidate = candidates(multiplexer.kind(), &verdicts)
            .into_iter()
            .find(|candidate| candidate.pane.id == id)
            .expect("the pane holds an editor");
        (table, Recorder::new(multiplexer), candidate)
    }

    #[test]
    fn vim_and_neovim_get_normal_mode_then_drop_and_the_line() {
        for kind in [EditorKind::Vim, EditorKind::Neovim] {
            assert_eq!(
                strokes(kind, Path::new("/tmp/thesis/ch5.tex"), 77),
                vec![b"\x1c\x0e:drop /tmp/thesis/ch5.tex | 77\r".to_vec()]
            );
        }
    }

    #[test]
    fn vim_paths_are_escaped_like_fnameescape() {
        assert_eq!(
            strokes(
                EditorKind::Vim,
                Path::new("/tmp/my thesis/a#b[1]\\c.tex"),
                3
            ),
            vec![b"\x1c\x0e:drop /tmp/my\\ thesis/a\\#b\\[1]\\\\c.tex | 3\r".to_vec()]
        );
    }

    #[test]
    fn helix_gets_escape_alone_then_open_with_the_position() {
        assert_eq!(
            strokes(EditorKind::Helix, Path::new("/tmp/thesis/ch5.tex"), 77),
            vec![
                b"\x1b".to_vec(),
                b":open /tmp/thesis/ch5.tex:77:1\r".to_vec()
            ]
        );
    }

    #[test]
    fn helix_paths_with_spaces_are_quoted() {
        assert_eq!(
            strokes(EditorKind::Helix, Path::new("/tmp/my thesis/ch5.tex"), 77)[1],
            b":open '/tmp/my thesis/ch5.tex:77:1'\r".to_vec()
        );
    }

    #[test]
    fn a_relative_source_is_made_absolute_before_it_is_typed() {
        let file = target(Path::new("chapters/ch5.tex")).expect("a plain path");
        assert!(file.is_absolute());
        assert!(file.ends_with("chapters/ch5.tex"));
    }

    #[test]
    fn the_ranked_neovim_gets_one_write() {
        let (table, multiplexer, nvim) = locked("%1");
        inject(
            &multiplexer,
            &table,
            Some("%0"),
            &nvim,
            &at("/tmp/thesis/ch5.tex", 77),
        )
        .expect("the jump is sent");
        assert_eq!(
            *multiplexer.sent.borrow(),
            vec![(
                "%1".to_owned(),
                b"\x1c\x0e:drop /tmp/thesis/ch5.tex | 77\r".to_vec(),
                false
            )]
        );
    }

    #[test]
    fn helix_gets_two_writes_fifty_milliseconds_apart() {
        let (table, multiplexer, hx) = locked("%3");
        let started = Instant::now();
        inject(
            &multiplexer,
            &table,
            Some("%0"),
            &hx,
            &at("/tmp/thesis/ch5.tex", 77),
        )
        .expect("the jump is sent");
        assert!(started.elapsed() >= HELIX_GAP);
        assert_eq!(
            multiplexer.sent_to("%3"),
            vec![
                b"\x1b".to_vec(),
                b":open /tmp/thesis/ch5.tex:77:1\r".to_vec()
            ]
        );
    }

    #[test]
    fn a_path_the_editor_would_run_is_never_typed() {
        let (table, multiplexer, nvim) = locked("%1");
        let refused = inject(
            &multiplexer,
            &table,
            Some("%0"),
            &nvim,
            &at("/tmp/$(id)/ch5.tex", 77),
        );
        assert!(matches!(
            refused,
            Err(Failure::Refused(Refusal::PathNeedsRpc))
        ));
        assert!(multiplexer.sent.borrow().is_empty());
    }

    #[test]
    fn an_editor_that_quit_back_to_the_shell_gets_nothing() {
        let (mut table, multiplexer, vim) = locked("%2");
        table.exit(301).foreground(300, 300);
        let refused = inject(
            &multiplexer,
            &table,
            Some("%0"),
            &vim,
            &at("/tmp/thesis/ch5.tex", 77),
        );
        assert!(matches!(refused, Err(Failure::Refused(Refusal::Shell))));
        assert!(multiplexer.sent.borrow().is_empty());
    }

    #[test]
    fn a_blocking_prompt_stops_the_send() {
        let (table, mut multiplexer, vim) = locked("%2");
        multiplexer.inner.screens.insert(
            "%2".to_owned(),
            "~\nPress ENTER or type command to continue\n".to_owned(),
        );
        let refused = inject(
            &multiplexer,
            &table,
            Some("%0"),
            &vim,
            &at("/tmp/thesis/ch5.tex", 77),
        );
        assert_eq!(
            refused.map_err(|failure| failure.to_string()),
            Err("refused: Press ENTER prompt".to_owned())
        );
        assert!(multiplexer.sent.borrow().is_empty());
    }

    #[test]
    fn a_failed_send_is_reported() {
        let (table, mut multiplexer, nvim) = locked("%1");
        multiplexer.broken = true;
        let failed = inject(
            &multiplexer,
            &table,
            Some("%0"),
            &nvim,
            &at("/tmp/thesis/ch5.tex", 77),
        );
        assert_eq!(
            failed.map_err(|failure| failure.to_string()),
            Err("could not send: the server went away".to_owned())
        );
    }
}
