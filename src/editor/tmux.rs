use std::cell::RefCell;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};

use super::detect::MultiplexerKind;
use super::probe::{Anchor, Multiplexer, Pane};
use super::process::Pid;

const PANE_FORMAT: &str = "#{pane_id} #{pane_pid} #{session_id} #{window_id} #{window_activity} #{pane_last} #{pane_active}";
const PASTE_BUFFER: &str = "termleaf";
const REPLY_WITHIN: Duration = Duration::from_secs(2);

pub struct Tmux {
    socket: PathBuf,
    own_pane: Option<String>,
    client: RefCell<Option<Control>>,
}

impl Tmux {
    pub fn new(socket: PathBuf, own_pane: Option<String>) -> Self {
        Self {
            socket,
            own_pane,
            client: RefCell::new(None),
        }
    }

    fn run(&self, commands: &[String]) -> Result<Vec<String>> {
        let mut client = self.client.borrow_mut();
        if !client.as_mut().is_some_and(Control::alive) {
            *client = None;
        }
        let control = match client.as_mut() {
            Some(control) => control,
            None => client.insert(Control::attach(&self.socket, self.own_pane.as_deref())?),
        };
        let reply = control.run(commands);
        if reply.is_err() && !control.alive() {
            *client = None;
        }
        reply
    }

    fn paste(&self, pane: &str, input: &[u8]) -> Result<()> {
        let mut load = Command::new("tmux")
            .arg("-S")
            .arg(&self.socket)
            .args(["load-buffer", "-b", PASTE_BUFFER, "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("could not run tmux")?;
        load.stdin
            .take()
            .context("no stdin for tmux load-buffer")?
            .write_all(input)?;
        if !load.wait()?.success() {
            bail!("tmux load-buffer failed");
        }
        self.run(&[paste_command(pane)?]).map(drop)
    }
}

impl Multiplexer for Tmux {
    fn kind(&self) -> MultiplexerKind {
        MultiplexerKind::Tmux
    }

    fn panes(&self) -> Result<Vec<Pane>> {
        let listing = self.run(&[format!("list-panes -a -F {}", quote(PANE_FORMAT)?)])?;
        Ok(parse_panes(&listing))
    }

    fn screen(&self, pane: &str) -> Option<String> {
        let command = format!("capture-pane -p -t {}", target(pane).ok()?);
        self.run(&[command]).ok().map(|lines| lines.join("\n"))
    }

    fn send(&self, pane: &str, input: &[u8], paste: bool) -> Result<()> {
        if input.is_empty() {
            target(pane)?;
            return Ok(());
        }
        if paste {
            self.paste(pane, input)
        } else {
            self.run(&[send_command(pane, input)?]).map(drop)
        }
    }
}

pub fn parse_panes(listing: &[String]) -> Vec<Pane> {
    listing
        .iter()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(' ').collect();
            let [id, pid, session, window, activity, last, active] = fields.as_slice() else {
                return None;
            };
            let flag = |value: &str| u64::from(value == "1");
            Some(Pane {
                id: (*id).to_owned(),
                anchor: Anchor::Process(Pid(pid.parse().ok()?)),
                session: (*session).to_owned(),
                window: (*window).to_owned(),
                recency: activity.parse::<u64>().unwrap_or_default() << 2
                    | flag(last) << 1
                    | flag(active),
            })
        })
        .collect()
}

fn target(pane: &str) -> Result<&str> {
    match pane.strip_prefix('%') {
        Some(number) if !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()) => {
            Ok(pane)
        }
        _ => bail!("not a tmux pane id: {pane:?}"),
    }
}

fn quote(argument: &str) -> Result<String> {
    if argument.contains(['\'', '\n', '\r']) {
        bail!("cannot quote {argument:?} for tmux");
    }
    Ok(format!("'{argument}'"))
}

pub fn send_command(pane: &str, input: &[u8]) -> Result<String> {
    let pane = target(pane)?;
    let text = std::str::from_utf8(input).context("tmux sends UTF-8 text")?;
    let mut commands = vec![format!(
        "if-shell -F -t {pane} '#{{pane_in_mode}}' 'copy-mode -q -t {pane}'"
    )];
    let mut rest = text;
    while !rest.is_empty() {
        let ascii = rest.find(|c: char| !c.is_ascii()).unwrap_or(rest.len());
        let (run, after) = if ascii > 0 {
            let (run, after) = rest.split_at(ascii);
            let bytes: Vec<String> = run.bytes().map(|byte| format!("{byte:02x}")).collect();
            (format!("send-keys -t {pane} -H {}", bytes.join(" ")), after)
        } else {
            let wide = rest.find(|c: char| c.is_ascii()).unwrap_or(rest.len());
            let (run, after) = rest.split_at(wide);
            (format!("send-keys -t {pane} -l {}", quote(run)?), after)
        };
        commands.push(run);
        rest = after;
    }
    Ok(commands.join(" ; "))
}

pub fn paste_command(pane: &str) -> Result<String> {
    Ok(format!(
        "paste-buffer -d -p -r -b {PASTE_BUFFER} -t {}",
        target(pane)?
    ))
}

struct Control {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    asked: u64,
}

impl Control {
    fn attach(socket: &Path, own_pane: Option<&str>) -> Result<Self> {
        let mut command = Command::new("tmux");
        command
            .arg("-S")
            .arg(socket)
            .args(["-C", "attach-session", "-f", "no-output,ignore-size"]);
        if let Some(pane) = own_pane {
            command.args(["-t", target(pane)?]);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("could not run tmux")?;
        let stdin = child.stdin.take().context("no stdin for tmux")?;
        let stdout = child.stdout.take().context("no stdout from tmux")?;
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).split(b'\n') {
                let Ok(line) = line else { break };
                if sender
                    .send(String::from_utf8_lossy(&line).into_owned())
                    .is_err()
                {
                    break;
                }
            }
        });
        let mut control = Self {
            child,
            stdin,
            lines,
            asked: 0,
        };
        control.run(&[])?;
        Ok(control)
    }

    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn run(&mut self, commands: &[String]) -> Result<Vec<String>> {
        self.asked += 1;
        let token = format!("termleaf-{}-{}", std::process::id(), self.asked);
        let mut script = String::new();
        for command in commands {
            script.push_str(command);
            script.push('\n');
        }
        script.push_str(&format!("display-message -p {token}\n"));
        self.stdin.write_all(script.as_bytes())?;
        self.stdin.flush()?;
        let reply = read_reply(&self.lines, &token, Instant::now() + REPLY_WITHIN);
        if reply.is_err() && self.alive() && !matches!(reply, Err(Reply::Failed(_))) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        reply.map_err(|reply| match reply {
            Reply::Failed(message) => anyhow!("tmux: {message}"),
            Reply::Silent => anyhow!("tmux did not answer"),
        })
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Reply {
    Failed(String),
    Silent,
}

fn read_reply(lines: &Receiver<String>, token: &str, until: Instant) -> Result<Vec<String>, Reply> {
    let mut output = Vec::new();
    let mut failure = None;
    let mut block: Option<(String, Vec<String>)> = None;
    loop {
        let line = lines
            .recv_timeout(until.saturating_duration_since(Instant::now()))
            .map_err(|_| Reply::Silent)?;
        let Some((stamp, collected)) = block.as_mut() else {
            if let Some(stamp) = line.strip_prefix("%begin ") {
                block = Some((stamp.to_owned(), Vec::new()));
            }
            continue;
        };
        let ended = line.strip_prefix("%end ").map(|rest| (rest, false));
        let failed = line.strip_prefix("%error ").map(|rest| (rest, true));
        match ended.or(failed) {
            Some((rest, error)) if rest == stamp => {
                let collected = std::mem::take(collected);
                block = None;
                if collected == [token] {
                    return failure.map_or(Ok(output), |message| Err(Reply::Failed(message)));
                }
                if error {
                    failure.get_or_insert_with(|| collected.join(" "));
                } else {
                    output.extend(collected);
                }
            }
            _ => collected.push(line),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_owned).collect()
    }

    fn replying(text: &str) -> Receiver<String> {
        let (sender, receiver) = mpsc::channel();
        for line in text.lines() {
            sender.send(line.to_owned()).expect("the channel is open");
        }
        receiver
    }

    fn soon() -> Instant {
        Instant::now() + Duration::from_millis(100)
    }

    struct Server {
        socket: PathBuf,
    }

    impl Server {
        fn start(label: &str) -> Option<Self> {
            let socket =
                std::env::temp_dir().join(format!("termleaf-tmux-{label}-{}", std::process::id()));
            let started = Command::new("tmux")
                .arg("-S")
                .arg(&socket)
                .args([
                    "-f",
                    "/dev/null",
                    "new-session",
                    "-d",
                    "-x",
                    "80",
                    "-y",
                    "10",
                    "cat",
                ])
                .status()
                .ok()?;
            started.success().then_some(Self { socket })
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            let _ = Command::new("tmux")
                .arg("-S")
                .arg(&self.socket)
                .arg("kill-server")
                .status();
            let _ = std::fs::remove_file(&self.socket);
        }
    }

    fn wait_for(tmux: &Tmux, pane: &str, text: &str) -> bool {
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until {
            if tmux
                .screen(pane)
                .is_some_and(|screen| screen.contains(text))
            {
                return true;
            }
            thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn a_live_tmux_server_lists_captures_and_receives_through_one_control_client() {
        let Some(server) = Server::start("live") else {
            eprintln!("tmux is not installed; skipped");
            return;
        };
        let tmux = Tmux::new(server.socket.clone(), None);
        let panes = tmux.panes().expect("tmux lists its panes");
        assert_eq!(panes.len(), 1);
        let pane = panes[0].id.clone();
        tmux.send(&pane, "raw é\r".as_bytes(), false)
            .expect("tmux takes raw bytes");
        assert!(wait_for(&tmux, &pane, "raw é"));
        tmux.run(&[format!("copy-mode -t {pane}")])
            .expect("tmux enters copy mode");
        tmux.send(&pane, b"after copy mode\r", false)
            .expect("tmux leaves copy mode first");
        assert!(wait_for(&tmux, &pane, "after copy mode"));
        tmux.send(&pane, b"pasted\r", true)
            .expect("tmux pastes a buffer");
        assert!(wait_for(&tmux, &pane, "pasted"));
        assert!(tmux.send("%99", b"x", false).is_err());
        tmux.send(&pane, b"", false)
            .expect("empty input sends nothing");
        tmux.send(&pane, b"", true)
            .expect("an empty paste sends nothing");
        assert!(tmux.panes().is_ok());
    }

    #[test]
    fn panes_are_listed_with_their_process_session_window_and_recency() {
        let panes = parse_panes(&lines(
            "%0 574618 $0 @0 1790569665 0 1\n%3 576308 $1 @4 1790569600 1 0\nbroken line\n",
        ));
        assert_eq!(
            panes,
            vec![
                Pane {
                    id: "%0".to_owned(),
                    anchor: Anchor::Process(Pid(574_618)),
                    session: "$0".to_owned(),
                    window: "@0".to_owned(),
                    recency: 1_790_569_665 << 2 | 1,
                },
                Pane {
                    id: "%3".to_owned(),
                    anchor: Anchor::Process(Pid(576_308)),
                    session: "$1".to_owned(),
                    window: "@4".to_owned(),
                    recency: 1_790_569_600 << 2 | 2,
                },
            ]
        );
    }

    #[test]
    fn the_last_pane_of_a_window_is_more_recent_than_the_others() {
        let panes = parse_panes(&lines("%1 10 $0 @0 50 1 0\n%2 11 $0 @0 50 0 0\n"));
        assert!(panes[0].recency > panes[1].recency);
    }

    #[test]
    fn raw_bytes_are_sent_as_hex_keys_after_leaving_copy_mode() {
        assert_eq!(
            send_command("%3", b"\x1c\x0e:drop /tmp/ch5.tex | 77\r").unwrap(),
            "if-shell -F -t %3 '#{pane_in_mode}' 'copy-mode -q -t %3' ; \
             send-keys -t %3 -H 1c 0e 3a 64 72 6f 70 20 2f 74 6d 70 2f 63 68 35 2e 74 65 78 20 7c 20 37 37 0d"
        );
    }

    #[test]
    fn text_beyond_ascii_is_sent_literally_between_hex_keys() {
        assert_eq!(
            send_command("%3", "a/é/b".as_bytes()).unwrap(),
            "if-shell -F -t %3 '#{pane_in_mode}' 'copy-mode -q -t %3' ; \
             send-keys -t %3 -H 61 2f ; send-keys -t %3 -l 'é' ; send-keys -t %3 -H 2f 62"
        );
    }

    #[test]
    fn only_pane_ids_are_targets() {
        assert!(send_command("%3; kill-server", b"x").is_err());
        assert!(send_command("thesis:1", b"x").is_err());
        assert!(send_command("%", b"x").is_err());
        assert!(paste_command("%12").is_ok());
    }

    #[test]
    fn a_paste_goes_through_a_buffer_with_bracketed_paste_allowed() {
        assert_eq!(
            paste_command("%3").unwrap(),
            "paste-buffer -d -p -r -b termleaf -t %3"
        );
    }

    #[test]
    fn bytes_that_are_not_utf8_are_not_sent() {
        assert!(send_command("%3", b"\xff").is_err());
    }

    #[test]
    fn a_reply_collects_every_block_up_to_the_token() {
        let lines = replying(
            "%session-changed $0 thesis\n%begin 1 20 1\n%1 10 $0 @0 50 1 0\n%end 1 20 1\n%window-add @2\n%begin 1 21 1\ntermleaf-1-1\n%end 1 21 1\n",
        );
        assert_eq!(
            read_reply(&lines, "termleaf-1-1", soon()),
            Ok(vec!["%1 10 $0 @0 50 1 0".to_owned()])
        );
    }

    #[test]
    fn an_end_line_inside_a_captured_screen_does_not_end_the_block() {
        let lines = replying(
            "%begin 1 20 1\n%end 9 9 9\nrow two\n%end 1 20 1\n%begin 1 21 1\ntermleaf-1-1\n%end 1 21 1\n",
        );
        assert_eq!(
            read_reply(&lines, "termleaf-1-1", soon()),
            Ok(vec!["%end 9 9 9".to_owned(), "row two".to_owned()])
        );
    }

    #[test]
    fn a_failed_command_fails_the_reply() {
        let lines = replying(
            "%begin 1 20 1\ncan't find pane: %9\n%error 1 20 1\n%begin 1 21 1\ntermleaf-1-1\n%end 1 21 1\n",
        );
        assert_eq!(
            read_reply(&lines, "termleaf-1-1", soon()),
            Err(Reply::Failed("can't find pane: %9".to_owned()))
        );
    }

    #[test]
    fn a_silent_server_times_out() {
        let lines = replying("%begin 1 20 1\n");
        assert_eq!(
            read_reply(&lines, "termleaf-1-1", soon()),
            Err(Reply::Silent)
        );
    }
}
