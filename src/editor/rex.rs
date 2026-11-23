use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::cli::{Cli, owned};
use super::detect::MultiplexerKind;
use super::probe::{Anchor, Multiplexer, Pane};
use super::process::Pid;

const TERMINAL_FLAVOR: &str = "com.superlogical.terminal";
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

pub struct Rex<'a, C> {
    cli: &'a C,
    program: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Placed {
    block: String,
    window: String,
    recency: u64,
}

impl<'a, C: Cli> Rex<'a, C> {
    pub fn new(cli: &'a C, bin_dir: Option<String>) -> Self {
        let program = bin_dir.map_or_else(
            || "rex".to_owned(),
            |dir| {
                PathBuf::from(dir)
                    .join("rex")
                    .to_string_lossy()
                    .into_owned()
            },
        );
        Self { cli, program }
    }

    fn run(&self, arguments: &[&str]) -> Result<String> {
        self.cli.run(&self.program, &command(arguments))
    }

    fn request(&self, arguments: &[&str]) -> Result<Value> {
        serde_json::from_str(&self.run(arguments)?)
            .with_context(|| format!("rex {} printed no JSON", arguments.join(" ")))
    }

    fn anchor(&self, block: &str) -> Option<Anchor> {
        anchor_of(
            &self
                .request(&["block", "call", "-b", block, "process"])
                .ok()?,
        )
    }
}

fn command(arguments: &[&str]) -> Vec<String> {
    let mut all = owned(&["--autostart=false"]);
    all.extend(owned(arguments));
    all
}

fn block_id(pane: &str) -> Result<&str> {
    let valid = !pane.is_empty()
        && !pane.starts_with('-')
        && pane
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '-' | '_'));
    if valid {
        Ok(pane)
    } else {
        bail!("not a Rex block id: {pane:?}")
    }
}

fn list(value: &Value) -> impl Iterator<Item = &Value> {
    value.as_array().into_iter().flatten()
}

fn text(value: &Value) -> Option<String> {
    value.as_str().map(str::to_owned)
}

fn sessions(listed: &Value) -> Vec<String> {
    list(&listed["sessions"])
        .filter_map(|session| text(&session["session_id"]))
        .collect()
}

fn placed_blocks(session: &Value) -> Vec<Placed> {
    let active = text(&session["active_window_id"]);
    list(&session["windows"])
        .flat_map(|window| {
            let id = text(&window["window_id"]).unwrap_or_default();
            let focused = text(&window["focused_block_id"]);
            let in_active = active.as_ref() == Some(&id);
            list(&window["layers"])
                .flat_map(|layer| list(&layer["blocks"]))
                .filter(|block| {
                    block["flavor"]
                        .as_str()
                        .is_some_and(|flavor| flavor.starts_with(TERMINAL_FLAVOR))
                })
                .filter_map(|block| text(&block["block_id"]))
                .map(|block| {
                    let flag = |on: bool| u64::from(on);
                    let is_focused = focused.as_ref() == Some(&block);
                    Placed {
                        recency: flag(is_focused && in_active) << 1 | flag(is_focused),
                        window: id.clone(),
                        block,
                    }
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

fn anchor_of(process: &Value) -> Option<Anchor> {
    process["child"]["pid"]
        .as_u64()
        .or_else(|| process["foreground"]["pid"].as_u64())
        .and_then(|pid| u32::try_from(pid).ok())
        .map(|pid| Anchor::Process(Pid(pid)))
}

fn pasted(input: &[u8], paste: bool) -> Result<Vec<u8>> {
    if !paste {
        return Ok(input.to_vec());
    }
    if input.windows(PASTE_END.len()).any(|run| run == PASTE_END) {
        bail!("a paste cannot hold the end-of-paste sequence");
    }
    Ok([PASTE_START, input, PASTE_END].concat())
}

impl<C: Cli> Multiplexer for Rex<'_, C> {
    fn kind(&self) -> MultiplexerKind {
        MultiplexerKind::Rex
    }

    fn panes(&self) -> Result<Vec<Pane>> {
        let mut panes = Vec::new();
        for session in sessions(&self.request(&["ls", "--json"])?) {
            let Ok(inspected) = self.request(&["session", "inspect", &session, "--json"]) else {
                continue;
            };
            for placed in placed_blocks(&inspected) {
                let Some(anchor) = self.anchor(&placed.block) else {
                    continue;
                };
                panes.push(Pane {
                    id: placed.block,
                    anchor,
                    session: session.clone(),
                    window: placed.window,
                    recency: placed.recency,
                });
            }
        }
        Ok(panes)
    }

    fn screen(&self, pane: &str) -> Option<String> {
        self.run(&["capture", "-b", block_id(pane).ok()?, "--trim"])
            .ok()
    }

    fn send(&self, pane: &str, input: &[u8], paste: bool) -> Result<()> {
        let block = block_id(pane)?;
        if input.is_empty() {
            return Ok(());
        }
        let input = pasted(input, paste)?;
        self.cli
            .feed(&self.program, &command(&["send", "-b", block]), &input)
            .map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::super::cli::fake::FakeCli;
    use super::super::cli::owned;
    use super::super::probe::Anchor;
    use super::super::process::Pid;
    use super::*;

    const SESSIONS: &str = r#"{"sessions": [
      {"session_id": "session:thesis", "label": "Thesis"},
      {"session_id": "session:notes", "label": "Notes"}
    ]}"#;

    const THESIS: &str = r#"{
      "session_id": "session:thesis",
      "active_window_id": "window:write",
      "windows": [
        {"window_id": "window:write", "focused_block_id": "block:hx",
         "layers": [{"kind": "tiled", "blocks": [
           {"block_id": "block:hx", "flavor": "com.superlogical.terminal.shell"},
           {"block_id": "block:pdf", "flavor": "com.superlogical.terminal.shell"},
           {"block_id": "block:web", "flavor": "com.superlogical.browser"}
         ]}]},
        {"window_id": "window:build", "focused_block_id": "block:make",
         "layers": [{"kind": "tiled", "blocks": [
           {"block_id": "block:make", "flavor": "com.superlogical.terminal.shell"}
         ]}]}
      ]
    }"#;

    const NOTES: &str = r#"{
      "session_id": "session:notes",
      "active_window_id": "window:todo",
      "windows": [
        {"window_id": "window:todo", "focused_block_id": "block:vim",
         "layers": [{"kind": "tiled", "blocks": [
           {"block_id": "block:vim", "flavor": "com.superlogical.terminal.shell"}
         ]}]}
      ]
    }"#;

    fn process(child: Option<u32>, foreground: u32) -> String {
        let child = child.map_or_else(
            || "null".to_owned(),
            |pid| format!(r#"{{"pid": {pid}, "user": "root", "name": "login"}}"#),
        );
        format!(
            r#"{{"last_exit": null, "child": {child},
                "foreground": {{"pid": {foreground}, "name": "hx"}},
                "ancestors": [{{"pid": 1510, "name": "fish"}}]}}"#
        )
    }

    fn call(program: &str, arguments: &[&str]) -> Vec<String> {
        let mut call = owned(&[program, "--autostart=false"]);
        call.extend(owned(arguments));
        call
    }

    fn rex_server() -> FakeCli {
        FakeCli::default()
            .prints(&["ls"], SESSIONS)
            .prints(&["inspect", "session:thesis"], THESIS)
            .prints(&["inspect", "session:notes"], NOTES)
            .prints(&["process", "block:hx"], &process(Some(1491), 27242))
            .prints(&["process", "block:pdf"], &process(Some(1493), 79234))
            .prints(&["process", "block:make"], &process(None, 4100))
            .fails(&["process", "block:vim"])
    }

    #[test]
    fn every_terminal_block_of_every_session_is_a_pane_anchored_at_its_program() {
        let cli = rex_server();
        let rex = Rex::new(&cli, Some("/Applications/Rex/bin".to_owned()));
        let panes = rex.panes().expect("Rex's JSON");
        let summary: Vec<(&str, &Anchor, &str, &str, u64)> = panes
            .iter()
            .map(|pane| {
                (
                    pane.id.as_str(),
                    &pane.anchor,
                    pane.session.as_str(),
                    pane.window.as_str(),
                    pane.recency,
                )
            })
            .collect();
        let at = |pid| Anchor::Process(Pid(pid));
        assert_eq!(
            summary,
            [
                ("block:hx", &at(1491), "session:thesis", "window:write", 3),
                ("block:pdf", &at(1493), "session:thesis", "window:write", 0),
                ("block:make", &at(4100), "session:thesis", "window:build", 1),
            ]
        );
        let program = "/Applications/Rex/bin/rex";
        assert_eq!(
            cli.calls(),
            [
                call(program, &["ls", "--json"]),
                call(program, &["session", "inspect", "session:thesis", "--json"]),
                call(program, &["block", "call", "-b", "block:hx", "process"]),
                call(program, &["block", "call", "-b", "block:pdf", "process"]),
                call(program, &["block", "call", "-b", "block:make", "process"]),
                call(program, &["session", "inspect", "session:notes", "--json"]),
                call(program, &["block", "call", "-b", "block:vim", "process"]),
            ]
        );
        assert_eq!(rex.kind(), MultiplexerKind::Rex);
    }

    #[test]
    fn a_block_is_read_and_typed_into_by_its_id() {
        let cli = FakeCli::default().prints(&["capture"], " NOR   ch5.tex   1 sel  40:1\n");
        let rex = Rex::new(&cli, None);
        assert_eq!(
            rex.screen("block:hx").as_deref(),
            Some(" NOR   ch5.tex   1 sel  40:1\n")
        );
        rex.send("block:hx", b"\x1b:open ch5.tex:77:1\r", false)
            .expect("typed");
        rex.send("block:hx", b"x", true).expect("pasted");
        rex.send("block:hx", b"", false).expect("nothing to type");
        assert_eq!(
            cli.calls(),
            [
                call("rex", &["capture", "-b", "block:hx", "--trim"]),
                call("rex", &["send", "-b", "block:hx"]),
                call("rex", &["send", "-b", "block:hx"]),
            ]
        );
        assert_eq!(
            cli.fed(),
            [
                b"\x1b:open ch5.tex:77:1\r".to_vec(),
                b"\x1b[200~x\x1b[201~".to_vec()
            ]
        );
    }

    #[test]
    fn only_a_block_id_can_name_a_pane() {
        let cli = FakeCli::default();
        let rex = Rex::new(&cli, None);
        for bad in ["", "-b", "--help", "block:a b", "block:a;b", "x\n"] {
            assert_eq!(rex.screen(bad), None, "{bad:?}");
            assert!(rex.send(bad, b"x", false).is_err(), "{bad:?}");
        }
        assert!(cli.calls().is_empty());
    }

    #[test]
    fn a_paste_cannot_end_itself_early() {
        let cli = FakeCli::default();
        let rex = Rex::new(&cli, None);
        assert!(rex.send("block:hx", b"a\x1b[201~b", true).is_err());
        assert!(cli.fed().is_empty());
    }

    #[test]
    fn a_server_that_is_not_running_lists_no_panes() {
        let cli = FakeCli::default().fails(&["ls"]);
        assert!(Rex::new(&cli, None).panes().is_err());
        let garbled = FakeCli::default().prints(&["ls"], "no JSON here");
        assert!(Rex::new(&garbled, None).panes().is_err());
    }
}
