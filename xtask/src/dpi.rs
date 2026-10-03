use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const COLUMNS: u16 = 100;
const ROWS: u16 = 40;
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const STEP_TIMEOUT: Duration = Duration::from_secs(30);

type Outcome<T> = Result<T, String>;

pub fn run(root: &Path) -> ExitCode {
    match scenario(root) {
        Ok(()) => {
            println!("dpi passed");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("dpi failed: {message}");
            ExitCode::FAILURE
        }
    }
}

fn scenario(root: &Path) -> Outcome<()> {
    let built = Command::new(env!("CARGO"))
        .args(["build", "--release", "--package", "termleaf"])
        .current_dir(root)
        .status()
        .map_err(|error| format!("running cargo build: {error}"))?;
    if !built.success() {
        return Err("building termleaf failed".to_owned());
    }
    let work = root.join("target/dpi");
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).map_err(|error| error.to_string())?;
    let doc = work.join("doc.pdf");
    fs::copy(root.join("tests/fixtures/three-pages.pdf"), &doc)
        .map_err(|error| format!("copying the fixture: {error}"))?;
    let termleaf = root.join("target/release/termleaf");
    let mut alone = Command::new(&termleaf);
    alone
        .args(["--no-pinch", "--no-follow", "--graphics", "sixel"])
        .arg(&doc);
    doubles(&work, "alone", alone)?;
    let layout = work.join("layout.kdl");
    fs::write(
        &layout,
        format!(
            "layout {{\n    pane command={:?} {{\n        args \"--no-pinch\" \"--no-follow\" \"--graphics\" \"sixel\" {:?}\n    }}\n}}\n",
            termleaf.display().to_string(),
            doc.display().to_string()
        ),
    )
    .map_err(|error| error.to_string())?;
    let config = work.join("zellij.kdl");
    fs::write(
        &config,
        "show_startup_tips false\nshow_release_notes false\npane_frames false\n",
    )
    .map_err(|error| error.to_string())?;
    let config_dir = work.join("zellij-config");
    fs::create_dir_all(&config_dir).map_err(|error| error.to_string())?;
    let sockets = work.join("zellij-sockets");
    let mut zellij = Command::new("zellij");
    zellij
        .arg("--config-dir")
        .arg(&config_dir)
        .arg("--config")
        .arg(&config)
        .arg("--new-session-with-layout")
        .arg(&layout)
        .args(["--session", &format!("dpi-{}", std::process::id())])
        .env("ZELLIJ_SOCKET_DIR", &sockets)
        .env_remove("ZELLIJ")
        .env_remove("ZELLIJ_SESSION_NAME");
    let result = doubles(&work, "in zellij", zellij);
    let _ = Command::new("zellij")
        .args(["kill-all-sessions", "--yes"])
        .env("ZELLIJ_SOCKET_DIR", &sockets)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    result
}

fn doubles(work: &Path, name: &str, command: Command) -> Outcome<()> {
    let session = Session::start(command, (1000, 800))?;
    let result = session.frame_width(0, 1000).and_then(|start| {
        println!("ok   {name}: first Sixel frame is 1000 px wide at a 10x20 px cell");
        session.resize_pixels((2000, 1600))?;
        session.frame_width(start, 2000)
    });
    let output = session
        .output
        .lock()
        .map(|output| output.clone())
        .unwrap_or_default();
    let _ = fs::write(work.join(format!("{}.log", name.replace(' ', "-"))), output);
    result?;
    println!(
        "ok   {name}: a 20x40 px cell with the same {COLUMNS}x{ROWS} cells repaints 2000 px wide"
    );
    Ok(())
}

struct Session {
    master: OwnedFd,
    child: Child,
    output: Arc<Mutex<Vec<u8>>>,
    pixels: Arc<Mutex<(u16, u16)>>,
}

impl Session {
    fn start(mut command: Command, pixels: (u16, u16)) -> Outcome<Self> {
        let mut size = window(pixels);
        let mut master = -1;
        let mut slave = -1;
        // SAFETY: master and slave are live ints for openpty to fill, the name and termios are null and size outlives the call.
        let opened = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &raw mut size,
            )
        };
        if opened != 0 {
            return Err(format!("openpty: {}", std::io::Error::last_os_error()));
        }
        // SAFETY: openpty succeeded, so master is an open descriptor that nothing else owns.
        let master = unsafe { OwnedFd::from_raw_fd(master) };
        // SAFETY: openpty succeeded, so slave is an open descriptor that nothing else owns.
        let slave = unsafe { OwnedFd::from_raw_fd(slave) };
        let stdio = |fd: &OwnedFd| {
            fd.try_clone()
                .map(Stdio::from)
                .map_err(|error| error.to_string())
        };
        command
            .env("TERM", "xterm-256color")
            .env_remove("TMUX")
            .env_remove("TERM_PROGRAM")
            .stdin(stdio(&slave)?)
            .stdout(stdio(&slave)?)
            .stderr(stdio(&slave)?);
        // SAFETY: the hook only calls the async-signal-safe setsid and ioctl between fork and exec.
        unsafe {
            command.pre_exec(take_the_pty);
        }
        let child = command
            .spawn()
            .map_err(|error| format!("starting {:?}: {error}", command.get_program()))?;
        drop(slave);
        let session = Self {
            master,
            child,
            output: Arc::default(),
            pixels: Arc::new(Mutex::new(pixels)),
        };
        session.answer_queries()?;
        Ok(session)
    }

    fn answer_queries(&self) -> Outcome<()> {
        let mut reader = File::from(self.master.try_clone().map_err(|error| error.to_string())?);
        let mut writer = reader.try_clone().map_err(|error| error.to_string())?;
        let output = Arc::clone(&self.output);
        let pixels = Arc::clone(&self.pixels);
        thread::spawn(move || {
            let mut chunk = vec![0; 1 << 16];
            while let Ok(read) = reader.read(&mut chunk) {
                if read == 0 {
                    return;
                }
                let chunk = &chunk[..read];
                let (width, height) = pixels.lock().map_or((0, 0), |pixels| *pixels);
                for (query, reply) in [
                    (&b"\x1b[14t"[..], format!("\x1b[4;{height};{width}t")),
                    (
                        &b"\x1b[16t"[..],
                        format!("\x1b[6;{};{}t", height / ROWS, width / COLUMNS),
                    ),
                    (&b"\x1b[18t"[..], format!("\x1b[8;{ROWS};{COLUMNS}t")),
                    (&b"\x1b[c"[..], "\x1b[?62;4c".to_owned()),
                ] {
                    for _ in chunk.windows(query.len()).filter(|window| *window == query) {
                        let _ = writer.write_all(reply.as_bytes());
                    }
                }
                if let Ok(mut output) = output.lock() {
                    output.extend_from_slice(chunk);
                }
            }
        });
        Ok(())
    }

    fn resize_pixels(&self, pixels: (u16, u16)) -> Outcome<()> {
        if let Ok(mut current) = self.pixels.lock() {
            *current = pixels;
        }
        let size = window(pixels);
        // SAFETY: master is an open pty descriptor and size is a live winsize for the duration of the call.
        let resized =
            unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &raw const size) };
        if resized < 0 {
            return Err(format!("TIOCSWINSZ: {}", std::io::Error::last_os_error()));
        }
        Ok(())
    }

    fn frame_width(&self, from: usize, width: u32) -> Outcome<usize> {
        let deadline = Instant::now() + STEP_TIMEOUT;
        loop {
            let seen = self
                .output
                .lock()
                .map(|output| output.clone())
                .unwrap_or_default();
            let widths = sixel_widths(seen.get(from..).unwrap_or_default());
            if widths.contains(&width) {
                return Ok(seen.len());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "timed out waiting for a Sixel frame {width} px wide; saw widths {widths:?}"
                ));
            }
            thread::sleep(POLL_INTERVAL);
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn take_the_pty() -> std::io::Result<()> {
    // SAFETY: setsid takes no arguments and only changes this forked child's session.
    if unsafe { libc::setsid() } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: descriptor 0 is the pty slave and TIOCSCTTY takes an int argument.
    if unsafe { libc::ioctl(0, libc::TIOCSCTTY, 0) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn window((width, height): (u16, u16)) -> libc::winsize {
    libc::winsize {
        ws_row: ROWS,
        ws_col: COLUMNS,
        ws_xpixel: width,
        ws_ypixel: height,
    }
}

fn sixel_widths(output: &[u8]) -> Vec<u32> {
    let marker = b"q\"1;1;";
    output
        .windows(marker.len())
        .enumerate()
        .filter(|(_, window)| *window == marker)
        .filter_map(|(at, _)| {
            let digits: Vec<u8> = output[at + marker.len()..]
                .iter()
                .copied()
                .take_while(u8::is_ascii_digit)
                .collect();
            String::from_utf8(digits).ok()?.parse().ok()
        })
        .collect()
}
