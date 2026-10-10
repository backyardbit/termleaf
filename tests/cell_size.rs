use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

const QUERY: &[u8] = b"\x1b_Gi=31,";
const KITTY_WITHOUT_CELL_SIZE: &[u8] = b"\x1b_Gi=31;OK\x1b\\\x1b_Gi=32;OK\x1b\\\x1b[?62;22c\x1b[0n";
const TRANSMIT: &[u8] = b"\x1b_Gq=2,a=T,";

fn window(xpixel: u16, ypixel: u16) -> libc::winsize {
    libc::winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: xpixel,
        ws_ypixel: ypixel,
    }
}

fn resize(master: RawFd, size: libc::winsize) {
    // SAFETY: master is an open pty descriptor and size is a live winsize for the call.
    let result = unsafe { libc::ioctl(master, libc::TIOCSWINSZ, &raw const size) };
    assert_eq!(result, 0, "{}", io::Error::last_os_error());
}

fn become_session_leader_on(slave: RawFd) -> io::Result<()> {
    // SAFETY: setsid takes no arguments and only changes this forked child's session.
    if unsafe { libc::setsid() } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: slave is the pty descriptor inherited by this child and TIOCSCTTY takes an integer argument.
    if unsafe { libc::ioctl(slave, libc::TIOCSCTTY as _, 0) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn find(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn kitty_waits_for_a_cell_size_that_arrives_after_startup() {
    let mut size = window(0, 0);
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
    assert_eq!(opened, 0, "{}", io::Error::last_os_error());
    // SAFETY: openpty succeeded, so master is an open descriptor that nothing else owns.
    let master = unsafe { OwnedFd::from_raw_fd(master) };
    // SAFETY: openpty succeeded, so slave is an open descriptor that nothing else owns.
    let slave = unsafe { OwnedFd::from_raw_fd(slave) };
    let slave_fd = slave.as_raw_fd();
    let stdio = || Stdio::from(slave.try_clone().unwrap());
    let mut command = Command::new(env!("CARGO_BIN_EXE_termleaf"));
    command
        .args(["--no-follow", "--no-pinch"])
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/three-pages.pdf"
        ))
        .env("TERM", "xterm-256color")
        .env("HOME", env!("CARGO_TARGET_TMPDIR"))
        .env("XDG_RUNTIME_DIR", env!("CARGO_TARGET_TMPDIR"))
        .env_remove("TMUX")
        .env_remove("TERM_PROGRAM")
        .env_remove("LC_TERMINAL")
        .env_remove("KONSOLE_VERSION")
        .env_remove("WEZTERM_EXECUTABLE")
        .env_remove("ZELLIJ")
        .stdin(stdio())
        .stdout(stdio())
        .stderr(stdio());
    // SAFETY: the hook only calls setsid and ioctl, both async-signal-safe, between fork and exec.
    unsafe {
        command.pre_exec(move || become_session_leader_on(slave_fd));
    }
    let mut child = command.spawn().unwrap();
    drop(command);
    drop(slave);

    let raw_master = master.as_raw_fd();
    let mut terminal = File::from(master);
    let mut reader = terminal.try_clone().unwrap();
    let (seen, transmitted) = mpsc::channel();
    let shown = Arc::new(Mutex::new(Vec::new()));
    let output = Arc::clone(&shown);
    thread::spawn(move || {
        let mut answered = false;
        let mut buffer = [0; 65536];
        while let Ok(read) = reader.read(&mut buffer) {
            if read == 0 {
                break;
            }
            let mut output = output.lock().unwrap();
            output.extend_from_slice(&buffer[..read]);
            if !answered && find(&output, QUERY) {
                answered = true;
                let _ = seen.send(false);
            }
            if find(&output, TRANSMIT) {
                let _ = seen.send(true);
                break;
            }
        }
    });

    assert_eq!(transmitted.recv_timeout(Duration::from_secs(10)), Ok(false));
    terminal.write_all(KITTY_WITHOUT_CELL_SIZE).unwrap();
    thread::sleep(Duration::from_millis(50));
    resize(raw_master, window(800, 480));

    let deadline = Instant::now() + Duration::from_secs(10);
    let drawn = transmitted.recv_timeout(deadline.saturating_duration_since(Instant::now()));
    let exited = child.try_wait().unwrap();
    let _ = child.kill();
    let _ = child.wait();
    let shown = String::from_utf8_lossy(&shown.lock().unwrap()).into_owned();
    assert_eq!(
        drawn,
        Ok(true),
        "termleaf exited with {exited:?} before drawing; it wrote {:?}",
        shown.rsplit("\x1b[?1049l").next().unwrap_or_default()
    );
}
