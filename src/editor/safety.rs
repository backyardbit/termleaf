use std::fmt;
use std::path::Path;

use super::probe::{Candidate, Multiplexer, Refusal, editor_in};
use super::process::ProcessTable;

const SHELL_ACTIVE: &[char] = &[
    '$', '`', ';', '&', '<', '>', '(', ')', '\'', '"', '\n', '\r',
];
const PROMPT_ROWS: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prompt {
    PressEnter,
    SwapFile,
    YesNo,
}

const PROMPTS: [(Prompt, &str); 3] = [
    (
        Prompt::PressEnter,
        "Press ENTER or type command to continue",
    ),
    (Prompt::SwapFile, "[O]pen Read-Only, (E)dit anyway"),
    (Prompt::YesNo, "(Y)es, [N]o"),
];

impl fmt::Display for Prompt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::PressEnter => "Press ENTER",
            Self::SwapFile => "swap file",
            Self::YesNo => "(Y)es/[N]o",
        })
    }
}

fn squeezed(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

pub fn blocking_prompt(screen: &str) -> Option<Prompt> {
    let mut last_rows: Vec<&str> = screen
        .lines()
        .rev()
        .filter(|row| !row.trim().is_empty())
        .take(PROMPT_ROWS)
        .collect();
    last_rows.reverse();
    let bottom = squeezed(&last_rows.concat());
    PROMPTS
        .iter()
        .find(|(_, text)| bottom.contains(&squeezed(text)))
        .map(|(prompt, _)| *prompt)
}

pub fn injectable(path: &Path) -> Result<(), Refusal> {
    match path.to_str() {
        Some(text) if !text.contains(SHELL_ACTIVE) => Ok(()),
        _ => Err(Refusal::PathNeedsRpc),
    }
}

pub fn recheck(
    multiplexer: &impl Multiplexer,
    table: &impl ProcessTable,
    own_pane: Option<&str>,
    locked: &Candidate,
) -> Result<(), Refusal> {
    if own_pane == Some(locked.pane.id.as_str()) {
        return Err(Refusal::OwnPane);
    }
    let panes = multiplexer.panes().map_err(|_| Refusal::PaneGone)?;
    let pane = panes
        .iter()
        .find(|pane| pane.id == locked.pane.id)
        .ok_or(Refusal::PaneGone)?;
    let editor = editor_in(table, pane)?;
    if editor.identity != locked.editor.identity {
        return Err(Refusal::Replaced);
    }
    match multiplexer
        .screen(&pane.id)
        .as_deref()
        .and_then(blocking_prompt)
    {
        Some(prompt) => Err(Refusal::Prompt(prompt)),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::super::probe::fake::{FakeMultiplexer, layout};
    use super::super::probe::{Multiplexer, candidates, survey};
    use super::super::process::fake::FakeTable;
    use super::*;

    const VIM_HIT_ENTER: &str = "line one\n~\n~\n\"ch5.tex\" 120L, 4033B\nE37: No write since last change\nPress ENTER or type command to continue\n";
    const VIM_SWAP: &str = "E325: ATTENTION\nFound a swap file by the name \".ch5.tex.swp\"\n          owned by: ada   dated: Sun Sep 27 23:49:10 2026\nSwap file \".ch5.tex.swp\" already exists!\n[O]pen Read-Only, (E)dit anyway, (R)ecover, (D)elete it, (Q)uit, (A)bort: \n";
    const VIM_SWAP_WRAPPED: &str = "Swap file \".ch5.tex.swp\" already exists!\n[O]pen Read-Only, (E)dit a\nnyway, (R)ecover, (Q)uit, \n(A)bort: \n";
    const VIM_SAVE: &str = "~\nSave changes to \"ch5.tex\"?\n(Y)es, [N]o, (C)ancel: \n";
    const VIM_NORMAL: &str = "\\section{Results}\nPress ENTER or type command to continue is only text here\n~\n~\n~\n~\n~\nch5.tex                                   77,1           Top\n";
    const HELIX: &str =
        "\\section{Results}\n~\n NOR   ch5.tex                          1 sel  77:1\n";

    fn locked(id: &str) -> (FakeTable, FakeMultiplexer, Candidate) {
        let (table, multiplexer) = layout();
        let verdicts = survey(&multiplexer, &table, Some("%0")).expect("panes");
        let candidate = candidates(multiplexer.kind(), &verdicts)
            .into_iter()
            .find(|candidate| candidate.pane.id == id)
            .expect("the pane holds an editor");
        (table, multiplexer, candidate)
    }

    #[test]
    fn blocking_prompts_are_seen_on_the_last_rows() {
        assert_eq!(blocking_prompt(VIM_HIT_ENTER), Some(Prompt::PressEnter));
        assert_eq!(blocking_prompt(VIM_SWAP), Some(Prompt::SwapFile));
        assert_eq!(blocking_prompt(VIM_SAVE), Some(Prompt::YesNo));
    }

    #[test]
    fn a_prompt_wrapped_by_a_narrow_pane_is_still_seen() {
        assert_eq!(blocking_prompt(VIM_SWAP_WRAPPED), Some(Prompt::SwapFile));
    }

    #[test]
    fn prompt_text_higher_up_in_the_buffer_is_not_a_prompt() {
        assert_eq!(blocking_prompt(VIM_NORMAL), None);
        assert_eq!(blocking_prompt(HELIX), None);
        assert_eq!(blocking_prompt(""), None);
    }

    #[test]
    fn a_plain_path_can_be_typed() {
        assert_eq!(injectable(Path::new("/home/ada/thesis/ch5.tex")), Ok(()));
        assert_eq!(injectable(Path::new("/home/ada/my thesis/ch5.tex")), Ok(()));
    }

    #[test]
    fn a_path_with_shell_active_characters_needs_rpc() {
        for name in [
            "a$b", "a`b", "a;b", "a&b", "a<b", "a>b", "a(b", "a)b", "a'b", "a\"b", "a\nb", "a\rb",
        ] {
            assert_eq!(
                injectable(&PathBuf::from("/tmp").join(name)),
                Err(Refusal::PathNeedsRpc),
                "{name:?}"
            );
        }
    }

    #[test]
    fn the_ranked_editor_passes_its_recheck() {
        let (table, multiplexer, vim) = locked("%2");
        assert_eq!(recheck(&multiplexer, &table, Some("%0"), &vim), Ok(()));
    }

    #[test]
    fn a_closed_pane_fails_the_recheck() {
        let (table, multiplexer, vim) = locked("%2");
        multiplexer
            .panes
            .borrow_mut()
            .retain(|pane| pane.id != "%2");
        assert_eq!(
            recheck(&multiplexer, &table, Some("%0"), &vim),
            Err(Refusal::PaneGone)
        );
    }

    #[test]
    fn an_editor_that_quit_back_to_the_shell_fails_the_recheck() {
        let (mut table, multiplexer, vim) = locked("%2");
        table.exit(301).foreground(300, 300);
        assert_eq!(
            recheck(&multiplexer, &table, Some("%0"), &vim),
            Err(Refusal::Shell)
        );
    }

    #[test]
    fn a_reused_pid_with_another_start_time_fails_the_recheck() {
        let (mut table, multiplexer, vim) = locked("%2");
        table.started(301, 99_999);
        assert_eq!(
            recheck(&multiplexer, &table, Some("%0"), &vim),
            Err(Refusal::Replaced)
        );
    }

    #[test]
    fn another_editor_started_in_the_same_pane_fails_the_recheck() {
        let (mut table, multiplexer, vim) = locked("%2");
        table.exit(301).spawn(302, 300, "vim").foreground(300, 302);
        assert_eq!(
            recheck(&multiplexer, &table, Some("%0"), &vim),
            Err(Refusal::Replaced)
        );
    }

    #[test]
    fn termleafs_own_pane_never_passes() {
        let (table, multiplexer, vim) = locked("%2");
        assert_eq!(
            recheck(&multiplexer, &table, Some("%2"), &vim),
            Err(Refusal::OwnPane)
        );
    }

    #[test]
    fn an_editor_at_a_blocking_prompt_fails_the_recheck() {
        let (table, mut multiplexer, vim) = locked("%2");
        multiplexer
            .screens
            .insert("%2".to_owned(), VIM_HIT_ENTER.to_owned());
        assert_eq!(
            recheck(&multiplexer, &table, Some("%0"), &vim),
            Err(Refusal::Prompt(Prompt::PressEnter))
        );
    }

    #[test]
    fn an_unreadable_screen_skips_only_the_prompt_check() {
        let (table, multiplexer, helix) = locked("%3");
        assert_eq!(multiplexer.screen("%3"), None);
        assert_eq!(recheck(&multiplexer, &table, Some("%0"), &helix), Ok(()));
    }
}
