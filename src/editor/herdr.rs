use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use super::detect::MultiplexerKind;
use super::probe::{Anchor, Multiplexer, Pane};
use super::process::Pid;

const REPLY_WITHIN: Duration = Duration::from_secs(2);
const PASTE_START: &str = "\x1b[200~";
const PASTE_END: &str = "\x1b[201~";

pub struct Herdr {
    program: PathBuf,
    socket: PathBuf,
}

impl Herdr {
    pub fn new(program: PathBuf, socket: PathBuf) -> Self {
        Self { program, socket }
    }

    fn run(&self, args: &[&str]) -> Result<String> {
        let mut child = Command::new(&self.program)
            .args(args)
            .env("HERDR_SOCKET_PATH", &self.socket)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("could not run {}", self.program.display()))?;
        let mut stdout = child.stdout.take().context("no stdout from herdr")?;
        let reader = thread::spawn(move || {
            let mut output = Vec::new();
            stdout.read_to_end(&mut output).map(|_| output)
        });
        let deadline = Instant::now() + REPLY_WITHIN;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                bail!("herdr did not answer within {} s", REPLY_WITHIN.as_secs());
            }
            thread::sleep(Duration::from_millis(1));
        };
        let output = reader
            .join()
            .map_err(|_| anyhow!("the herdr reply was lost"))??;
        let text = String::from_utf8_lossy(&output).into_owned();
        if status.success() {
            Ok(text)
        } else {
            bail!("herdr {} failed: {}", args.join(" "), failure(&text))
        }
    }

    fn request(&self, args: &[&str]) -> Result<Value> {
        reply(&self.run(args)?)
    }
}

impl Multiplexer for Herdr {
    fn kind(&self) -> MultiplexerKind {
        MultiplexerKind::Herdr
    }

    fn panes(&self) -> Result<Vec<Pane>> {
        let snapshot = self.request(&["api", "snapshot"])?;
        Ok(parse_snapshot(&snapshot)
            .into_iter()
            .filter_map(|listed| {
                let info = self
                    .request(&["pane", "process-info", "--pane", &listed.id])
                    .ok()?;
                Some(Pane {
                    anchor: Anchor::Process(shell_of(&info)?),
                    id: listed.id,
                    session: listed.workspace,
                    window: listed.tab,
                    recency: listed.recency,
                })
            })
            .collect())
    }

    fn screen(&self, pane: &str) -> Option<String> {
        self.run(&["pane", "read", target(pane).ok()?, "--source", "visible"])
            .ok()
    }

    fn send(&self, pane: &str, input: &[u8], paste: bool) -> Result<()> {
        let pane = target(pane)?;
        if input.is_empty() {
            return Ok(());
        }
        let text = send_text(input, paste)?;
        self.run(&["pane", "send-text", pane, &text]).map(drop)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub id: String,
    pub workspace: String,
    pub tab: String,
    pub recency: u64,
}

pub fn parse_snapshot(result: &Value) -> Vec<Listed> {
    let snapshot = &result["snapshot"];
    let text = |value: &Value| value.as_str().map(str::to_owned);
    let focused = text(&snapshot["focused_pane_id"]);
    let active_tabs: Vec<String> = list(&snapshot["workspaces"])
        .filter_map(|workspace| text(&workspace["active_tab_id"]))
        .collect();
    let tab_focus: HashMap<String, String> = list(&snapshot["layouts"])
        .filter_map(|layout| Some((text(&layout["tab_id"])?, text(&layout["focused_pane_id"])?)))
        .collect();
    list(&snapshot["panes"])
        .filter_map(|pane| {
            let id = text(&pane["pane_id"])?;
            let tab = text(&pane["tab_id"])?;
            let in_tab = tab_focus.get(&tab) == Some(&id);
            let flag = |on: bool| u64::from(on);
            Some(Listed {
                recency: flag(focused.as_ref() == Some(&id)) << 2
                    | flag(in_tab && active_tabs.contains(&tab)) << 1
                    | flag(in_tab),
                workspace: text(&pane["workspace_id"])?,
                id,
                tab,
            })
        })
        .collect()
}

pub fn shell_of(result: &Value) -> Option<Pid> {
    let info = &result["process_info"];
    info["shell_pid"]
        .as_u64()
        .or_else(|| info["foreground_process_group_id"].as_u64())
        .and_then(|pid| u32::try_from(pid).ok())
        .map(Pid)
}

fn list(value: &Value) -> impl Iterator<Item = &Value> {
    value.as_array().into_iter().flatten()
}

fn reply(text: &str) -> Result<Value> {
    let mut value: Value = serde_json::from_str(text).context("herdr did not reply in JSON")?;
    match value.get_mut("result") {
        Some(result) => Ok(result.take()),
        None => bail!("herdr replied without a result"),
    }
}

fn failure(text: &str) -> String {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|value| value["error"]["message"].as_str().map(str::to_owned))
        .unwrap_or_else(|| text.trim().to_owned())
}

fn target(pane: &str) -> Result<&str> {
    let valid = !pane.is_empty()
        && !pane.starts_with('-')
        && pane
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '-'));
    if valid {
        Ok(pane)
    } else {
        bail!("not a herdr pane id: {pane:?}")
    }
}

pub fn send_text(input: &[u8], paste: bool) -> Result<String> {
    let text = std::str::from_utf8(input).context("herdr sends UTF-8 text")?;
    if text.contains('\0') {
        bail!("herdr cannot send a NUL byte");
    }
    if !paste {
        return Ok(text.to_owned());
    }
    if text.contains(PASTE_END) {
        bail!("a paste cannot hold the end-of-paste sequence");
    }
    Ok(format!("{PASTE_START}{text}{PASTE_END}"))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use serde_json::json;

    use super::*;

    const SOCKET: &str = "/run/herdr/tl5 main.sock";

    fn snapshot() -> Value {
        json!({
            "snapshot": {
                "focused_pane_id": "w1:p1",
                "workspaces": [
                    {"workspace_id": "w1", "active_tab_id": "w1:t1"},
                    {"workspace_id": "w2", "active_tab_id": "w2:t1"}
                ],
                "layouts": [
                    {"tab_id": "w1:t1", "focused_pane_id": "w1:p1"},
                    {"tab_id": "w1:t2", "focused_pane_id": "w1:p4"},
                    {"tab_id": "w2:t1", "focused_pane_id": "w2:p1"}
                ],
                "panes": [
                    {"pane_id": "w1:p1", "tab_id": "w1:t1", "workspace_id": "w1", "focused": true},
                    {"pane_id": "w1:p2", "tab_id": "w1:t1", "workspace_id": "w1", "focused": false},
                    {"pane_id": "w1:p4", "tab_id": "w1:t2", "workspace_id": "w1", "focused": false},
                    {"pane_id": "w2:p1", "tab_id": "w2:t1", "workspace_id": "w2", "focused": false}
                ]
            }
        })
    }

    fn fake(name: &str, body: &str) -> (Herdr, PathBuf) {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let directory = std::env::temp_dir().join(format!("termleaf-herdr-{name}-{nanos}"));
        fs::create_dir_all(&directory).expect("a scratch directory");
        let program = directory.join("herdr");
        let log = directory.join("argv");
        fs::write(
            &program,
            format!(
                "#!/bin/sh\n[ -n \"$FAKE_HERDR_READY\" ] && exit 0\nprintf '%s\\0' \"$HERDR_SOCKET_PATH\" \"$@\" >> '{}'\n{body}\n",
                log.display()
            ),
        )
        .expect("the fake herdr is written");
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755))
            .expect("the fake herdr is executable");
        let deadline = Instant::now() + Duration::from_secs(5);
        while Command::new(&program)
            .env("FAKE_HERDR_READY", "1")
            .status()
            .is_err()
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(5));
        }
        (Herdr::new(program, PathBuf::from(SOCKET)), log)
    }

    fn calls(log: &Path) -> Vec<Vec<String>> {
        let text = fs::read_to_string(log).unwrap_or_default();
        let fields: Vec<String> = text.split('\0').map(str::to_owned).collect();
        let mut calls = Vec::new();
        let mut current: Vec<String> = Vec::new();
        for field in fields.into_iter().filter(|field| !field.is_empty()) {
            if field == SOCKET && !current.is_empty() {
                calls.push(std::mem::take(&mut current));
            }
            current.push(field);
        }
        if !current.is_empty() {
            calls.push(current);
        }
        calls
    }

    #[test]
    fn a_pane_is_anchored_on_its_shell() {
        let without_shell = json!({"process_info": {"foreground_process_group_id": 712}});
        assert_eq!(shell_of(&without_shell), Some(Pid(712)));
    }

    #[test]
    fn a_paste_is_wrapped_in_bracketed_paste() {
        assert_eq!(
            send_text(b"two\nlines", true).unwrap(),
            "\x1b[200~two\nlines\x1b[201~"
        );
        assert!(send_text(b"early\x1b[201~end", true).is_err());
    }

    #[test]
    fn nul_and_bytes_that_are_not_utf8_are_not_sent() {
        assert!(send_text(b"a\0b", false).is_err());
        assert!(send_text(b"\xff\xfe", false).is_err());
    }

    #[test]
    fn only_pane_ids_are_targets() {
        assert_eq!(target("w1:p2").unwrap(), "w1:p2");
        for bad in ["-h", "w1:p2;x"] {
            assert!(target(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_send_is_one_send_text_call_on_the_session_socket() {
        let (herdr, log) = fake("send", ":");
        herdr
            .send(
                "w1:p2",
                "\x1c\x0e:drop /t/my thèsis.tex | 7\r".as_bytes(),
                false,
            )
            .unwrap();
        assert_eq!(
            calls(&log),
            vec![vec![
                SOCKET.to_owned(),
                "pane".to_owned(),
                "send-text".to_owned(),
                "w1:p2".to_owned(),
                "\x1c\x0e:drop /t/my thèsis.tex | 7\r".to_owned(),
            ]]
        );
    }

    #[test]
    fn empty_input_runs_nothing() {
        let (herdr, log) = fake("empty", "echo '{\"result\":{}}'");
        herdr.send("w1:p2", b"", true).unwrap();
        assert!(calls(&log).is_empty());
        assert!(herdr.send("-h", b"", false).is_err());
    }

    #[test]
    fn panes_come_from_the_snapshot_and_each_panes_process_info() {
        let body = format!(
            "case \"$1 $2\" in\n\
             'api snapshot') echo '{}' ;;\n\
             'pane process-info') case \"$4\" in\n\
               w1:p1) echo '{{\"result\":{{\"process_info\":{{\"shell_pid\":101}}}}}}' ;;\n\
               w1:p2) echo '{{\"result\":{{\"process_info\":{{\"shell_pid\":102}}}}}}' ;;\n\
               w1:p4) echo '{{\"result\":{{\"process_info\":{{\"shell_pid\":104}}}}}}' ;;\n\
               *) echo '{{\"error\":{{\"message\":\"pane gone\"}}}}'; exit 1 ;;\n\
             esac ;;\n\
             esac",
            json!({"id": "cli:api:snapshot", "result": snapshot()})
        );
        let (herdr, log) = fake("panes", &body);
        let panes = herdr.panes().unwrap();
        assert_eq!(
            panes,
            vec![
                Pane {
                    id: "w1:p1".to_owned(),
                    anchor: Anchor::Process(Pid(101)),
                    session: "w1".to_owned(),
                    window: "w1:t1".to_owned(),
                    recency: 0b111,
                },
                Pane {
                    id: "w1:p2".to_owned(),
                    anchor: Anchor::Process(Pid(102)),
                    session: "w1".to_owned(),
                    window: "w1:t1".to_owned(),
                    recency: 0,
                },
                Pane {
                    id: "w1:p4".to_owned(),
                    anchor: Anchor::Process(Pid(104)),
                    session: "w1".to_owned(),
                    window: "w1:t2".to_owned(),
                    recency: 0b001,
                },
            ]
        );
        assert_eq!(
            calls(&log)[1][1..],
            ["pane", "process-info", "--pane", "w1:p1"]
        );
    }

    #[test]
    fn a_screen_is_read_from_the_visible_rows() {
        let (herdr, log) = fake("read", "printf 'NOR  intro.tex  12:1\\n'");
        assert_eq!(
            herdr.screen("w1:p2").as_deref(),
            Some("NOR  intro.tex  12:1\n")
        );
        assert_eq!(
            calls(&log)[0][1..],
            ["pane", "read", "w1:p2", "--source", "visible"]
        );
    }

    #[test]
    fn a_silent_herdr_times_out() {
        let (herdr, _) = fake("silent", "exec sleep 10");
        let started = Instant::now();
        let error = herdr.send("w1:p2", b"x", false).unwrap_err().to_string();
        assert!(error.contains("did not answer"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_missing_herdr_is_an_error() {
        let herdr = Herdr::new(
            PathBuf::from("/nonexistent/termleaf-test/herdr"),
            PathBuf::from(SOCKET),
        );
        assert!(herdr.panes().is_err());
    }
}
