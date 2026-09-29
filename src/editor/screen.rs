use std::fs::{self, DirBuilder};
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

use super::cli::{Cli, owned};
use super::detect::MultiplexerKind;
use super::probe::{Anchor, Multiplexer, Pane};
use super::process::{PaneTags, Pid, ProcessTable, pane_roots};

const PROGRAM: &str = "screen";
const SERVERS: &[&str] = &["screen", "SCREEN"];
const STUFF_LIMIT: usize = 512;
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";
const ESCAPED: &[u8] = b"\\^$'\"";
const HARDCOPY_WAIT: Duration = Duration::from_millis(200);
const HARDCOPY_POLL: Duration = Duration::from_millis(2);

fn stuffed(byte: u8) -> String {
    if (0x20..0x7f).contains(&byte) && !ESCAPED.contains(&byte) {
        char::from(byte).to_string()
    } else {
        format!("\\{byte:03o}")
    }
}

fn stuffed_chunks(input: &[u8]) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut chunk = String::new();
    for byte in input {
        let piece = stuffed(*byte);
        if chunk.len() + piece.len() > STUFF_LIMIT {
            chunks.push(std::mem::take(&mut chunk));
        }
        chunk.push_str(&piece);
    }
    if !chunk.is_empty() {
        chunks.push(chunk);
    }
    chunks
}

fn decoded(text: Vec<u8>) -> String {
    String::from_utf8(text)
        .unwrap_or_else(|invalid| invalid.into_bytes().into_iter().map(char::from).collect())
}

struct Private {
    directory: PathBuf,
}

impl Private {
    fn create() -> Result<Self> {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.subsec_nanos())
            .unwrap_or_default();
        let directory = std::env::temp_dir().join(format!(
            "termleaf-screen-{}-{}-{nanos}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .context("could not create a private directory for screen's hardcopy")?;
        Ok(Self { directory })
    }
}

impl Drop for Private {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

pub struct Screen<'a, C, T> {
    cli: &'a C,
    table: &'a T,
    session: String,
}

impl<'a, C: Cli, T: ProcessTable> Screen<'a, C, T> {
    pub fn new(cli: &'a C, table: &'a T, session: &str) -> Self {
        Self {
            cli,
            table,
            session: session.to_owned(),
        }
    }

    fn run(&self, window: &str, arguments: &[&str]) -> Result<String> {
        let mut all = owned(&["-S", &self.session, "-p", window]);
        all.extend(owned(arguments));
        self.cli.run(PROGRAM, &all)
    }
}

impl<C: Cli, T: ProcessTable> Multiplexer for Screen<'_, C, T> {
    fn kind(&self) -> MultiplexerKind {
        MultiplexerKind::Screen
    }

    fn panes(&self) -> Result<Vec<Pane>> {
        let roots = pane_roots(
            self.table,
            PaneTags {
                session_variable: "STY",
                session: &self.session,
                pane_variable: "WINDOW",
                servers: SERVERS,
            },
        );
        let mut windows: Vec<(u32, Pid)> = roots
            .into_iter()
            .filter_map(|(window, root)| Some((window.parse().ok()?, root)))
            .collect();
        windows.sort_unstable();
        Ok(windows
            .into_iter()
            .map(|(window, root)| Pane {
                id: window.to_string(),
                anchor: Anchor::Process(root),
                session: self.session.clone(),
                window: window.to_string(),
                recency: 0,
            })
            .collect())
    }

    fn screen(&self, pane: &str) -> Option<String> {
        let private = Private::create().ok()?;
        let file = private.directory.join("hardcopy");
        self.run(pane, &["-X", "hardcopy", &file.to_string_lossy()])
            .ok()?;
        let started = Instant::now();
        let mut last = None;
        while started.elapsed() < HARDCOPY_WAIT {
            let read = fs::read(&file).ok();
            if read.is_some() && read == last {
                return read.map(decoded);
            }
            last = read;
            thread::sleep(HARDCOPY_POLL);
        }
        None
    }

    fn send(&self, pane: &str, input: &[u8], paste: bool) -> Result<()> {
        if input.contains(&0) {
            bail!("screen cannot stuff a NUL byte");
        }
        let input = if paste {
            [PASTE_START, input, PASTE_END].concat()
        } else {
            input.to_vec()
        };
        for chunk in stuffed_chunks(&input) {
            self.run(pane, &["-X", "stuff", &chunk])?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::super::cli::fake::FakeCli;
    use super::super::process::fake::FakeTable;
    use super::*;

    const STY: &str = "4242.thesis";

    fn call(window: &str, arguments: &[&str]) -> Vec<String> {
        let mut call = owned(&["screen", "-S", STY, "-p", window]);
        call.extend(owned(arguments));
        call
    }

    #[test]
    fn windows_are_found_through_the_environment_without_asking_screen() {
        let mut table = FakeTable::default();
        table
            .spawn(4242, 1, "screen")
            .spawn(10, 4242, "bash")
            .spawn(11, 10, "termleaf")
            .spawn(20, 4242, "nvim")
            .spawn(21, 20, "nvim")
            .spawn(30, 4242, "bash")
            .spawn(60, 4242, "bash")
            .spawn(70, 4242, "vim");
        for (pid, window) in [(10, "0"), (11, "0"), (20, "1"), (21, "1"), (30, "2")] {
            table
                .variable(pid, "STY", STY)
                .variable(pid, "WINDOW", window);
        }
        table
            .variable(60, "STY", STY)
            .variable(60, "WINDOW", "not a number")
            .variable(70, "STY", STY)
            .variable(70, "WINDOW", "10");
        let cli = FakeCli::default();
        let screen = Screen::new(&cli, &table, STY);
        assert_eq!(screen.kind(), MultiplexerKind::Screen);
        let summary: Vec<(String, Anchor, String, u64)> = screen
            .panes()
            .expect("windows")
            .into_iter()
            .map(|pane| (pane.id, pane.anchor, pane.window, pane.recency))
            .collect();
        assert_eq!(
            summary,
            [
                ("0".to_owned(), Anchor::Process(Pid(10)), "0".to_owned(), 0),
                ("1".to_owned(), Anchor::Process(Pid(20)), "1".to_owned(), 0),
                ("2".to_owned(), Anchor::Process(Pid(30)), "2".to_owned(), 0),
                (
                    "10".to_owned(),
                    Anchor::Process(Pid(70)),
                    "10".to_owned(),
                    0
                ),
            ]
        );
        let gone = Screen::new(&cli, &table, "1.gone");
        assert_eq!(gone.panes().expect("no windows"), []);
        assert!(cli.calls().is_empty());
    }

    #[test]
    fn input_is_stuffed_into_one_window_with_screens_escapes_and_nul_is_refused() {
        let cli = FakeCli::default();
        let table = FakeTable::default();
        let screen = Screen::new(&cli, &table, STY);
        assert!(screen.send("1", b"a\0b", false).is_err());
        screen
            .send(
                "1",
                "\x1c\x0e:drop /t/a^b\\c$HOME'\"é | 77\r".as_bytes(),
                false,
            )
            .expect("stuffed");
        screen.send("2", b"hi", true).expect("stuffed");
        let failing = FakeCli::default().fails(&["stuff"]);
        assert!(
            Screen::new(&failing, &table, STY)
                .send("1", b"x", false)
                .is_err()
        );
        assert_eq!(
            cli.calls(),
            [
                call(
                    "1",
                    &[
                        "-X",
                        "stuff",
                        "\\034\\016:drop /t/a\\136b\\134c\\044HOME\\047\\042\\303\\251 | 77\\015"
                    ]
                ),
                call("2", &["-X", "stuff", "\\033[200~hi\\033[201~"]),
            ]
        );
    }

    #[test]
    fn long_input_is_split_below_screens_command_limit_without_cutting_an_escape() {
        let input = [b'x'; 1000]
            .iter()
            .chain(b"\x1c".repeat(200).iter())
            .copied()
            .collect::<Vec<u8>>();
        let chunks = stuffed_chunks(&input);
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|chunk| chunk.len() <= STUFF_LIMIT));
        assert_eq!(
            chunks.concat(),
            input.iter().map(|byte| stuffed(*byte)).collect::<String>()
        );
        assert!(
            chunks
                .iter()
                .skip(1)
                .all(|chunk| chunk.starts_with('x') || chunk.starts_with("\\034"))
        );
    }

    #[test]
    fn the_screen_is_read_back_through_a_private_hardcopy_that_is_removed() {
        let cli = FakeCli::default().writes_last_argument(&["hardcopy"], "ch5 \u{e9}.tex\n");
        let table = FakeTable::default();
        let screen = Screen::new(&cli, &table, STY);
        assert_eq!(screen.screen("1").as_deref(), Some("ch5 é.tex\n"));
        let calls = cli.calls();
        assert_eq!(calls[0][..7], call("1", &["-X", "hardcopy"]));
        let hardcopy = PathBuf::from(&calls[0][7]);
        assert!(!hardcopy.parent().expect("a private directory").exists());
        assert_eq!(decoded(b"ch5 \xe9.tex".to_vec()), "ch5 é.tex");
        let failing = FakeCli::default().fails(&["hardcopy"]);
        assert_eq!(Screen::new(&failing, &table, STY).screen("1"), None);
        let silent = FakeCli::default();
        assert_eq!(Screen::new(&silent, &table, STY).screen("1"), None);
    }

    #[test]
    fn the_hardcopy_directory_is_private_to_us() {
        let private = Private::create().expect("a private directory");
        let mode = fs::metadata(&private.directory)
            .expect("the directory exists")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
    }
}
