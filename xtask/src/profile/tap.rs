use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const OUTPUT_FILE: &str = "output";
pub const EVENTS_FILE: &str = "events";
pub const CONTROL_FIFO: &str = "control";
const SIZE_POLL: Duration = Duration::from_millis(50);
const DRAIN: Duration = Duration::from_secs(1);

pub fn now_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

pub fn make_control_fifo(dir: &Path) -> Result<PathBuf, String> {
    let fifo = dir.join(CONTROL_FIFO);
    let _ = fs::remove_file(&fifo);
    let path =
        CString::new(fifo.as_os_str().as_encoded_bytes()).map_err(|error| error.to_string())?;
    // SAFETY: path is a live NUL-terminated string for the duration of the call.
    if unsafe { libc::mkfifo(path.as_ptr(), 0o600) } != 0 {
        return Err(format!("mkfifo: {}", std::io::Error::last_os_error()));
    }
    Ok(fifo)
}

pub fn control_line(label: &str, keys: &[u8]) -> String {
    let hex: String = keys.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{label}\t{hex}\n")
}

pub fn parse_control_line(line: &str) -> Option<(String, Vec<u8>)> {
    let (label, hex) = line.trim_end().split_once('\t')?;
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(hex.get(at..at + 2)?, 16).ok())
        .collect::<Option<Vec<u8>>>()?;
    Some((label.to_owned(), bytes))
}

pub fn run(args: &[String]) -> ExitCode {
    let Some(separator) = args.iter().position(|arg| arg == "--") else {
        eprintln!("usage: xtask profile-tap <dir> -- <command> [args]");
        return ExitCode::FAILURE;
    };
    let (Some(dir), Some(program)) = (args.first(), args.get(separator + 1)) else {
        eprintln!("usage: xtask profile-tap <dir> -- <command> [args]");
        return ExitCode::FAILURE;
    };
    match relay(Path::new(dir), program, &args[separator + 2..]) {
        Ok(code) => ExitCode::from(code),
        Err(message) => {
            eprintln!("profile-tap: {message}");
            ExitCode::FAILURE
        }
    }
}

type Events = Arc<Mutex<File>>;

fn record(events: &Events, line: &str) {
    if let Ok(mut file) = events.lock() {
        let _ = file.write_all(line.as_bytes());
    }
}

fn relay(dir: &Path, program: &str, args: &[String]) -> Result<u8, String> {
    let events: Events = Arc::new(Mutex::new(
        File::create(dir.join(EVENTS_FILE)).map_err(|error| error.to_string())?,
    ));
    let output = File::create(dir.join(OUTPUT_FILE)).map_err(|error| error.to_string())?;
    let size = window_size(0).unwrap_or(libc::winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    });
    let (master, slave) = open_pty(size)?;
    let stdio = |fd: &OwnedFd| {
        fd.try_clone()
            .map(Stdio::from)
            .map_err(|error| error.to_string())
    };
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(stdio(&slave)?)
        .stdout(stdio(&slave)?)
        .stderr(stdio(&slave)?);
    // SAFETY: the hook only calls login_tty, which uses setsid, ioctl and dup2, between fork and exec.
    unsafe {
        command.pre_exec(take_the_pty);
    }
    let raw = RawMode::enter();
    record(&events, &format!("s {}\n", now_micros()));
    let mut child = command
        .spawn()
        .map_err(|error| format!("starting {program}: {error}"))?;
    drop(slave);

    let forwarder = forward_output(&master, output, Arc::clone(&events))?;
    forward_input(&master)?;
    follow_size(&master)?;
    take_control(dir, &master, Arc::clone(&events))?;

    let status = child.wait().map_err(|error| error.to_string())?;
    let deadline = std::time::Instant::now() + DRAIN;
    while !forwarder.is_finished() && std::time::Instant::now() < deadline {
        thread::sleep(SIZE_POLL);
    }
    drop(raw);
    Ok(status
        .code()
        .and_then(|code| u8::try_from(code).ok())
        .unwrap_or(1))
}

fn open_pty(mut size: libc::winsize) -> Result<(OwnedFd, OwnedFd), String> {
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
    Ok((master, slave))
}

fn take_the_pty() -> std::io::Result<()> {
    // SAFETY: descriptor 0 is the pty slave, which login_tty makes this forked child's controlling terminal in a new session.
    if unsafe { libc::login_tty(0) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn forward_output(
    master: &OwnedFd,
    mut output: File,
    events: Events,
) -> Result<thread::JoinHandle<()>, String> {
    let mut reader = File::from(master.try_clone().map_err(|error| error.to_string())?);
    Ok(thread::spawn(move || {
        let mut stdout = std::io::stdout().lock();
        let mut chunk = vec![0; 1 << 16];
        let mut written = 0_usize;
        while let Ok(read) = reader.read(&mut chunk) {
            if read == 0 {
                return;
            }
            let bytes = &chunk[..read];
            let _ = stdout.write_all(bytes);
            let _ = stdout.flush();
            let _ = output.write_all(bytes);
            written += read;
            record(&events, &format!("o {} {written}\n", now_micros()));
        }
    }))
}

fn forward_input(master: &OwnedFd) -> Result<(), String> {
    let mut writer = File::from(master.try_clone().map_err(|error| error.to_string())?);
    thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut chunk = vec![0; 4096];
        while let Ok(read) = stdin.read(&mut chunk) {
            if read == 0 || writer.write_all(&chunk[..read]).is_err() {
                return;
            }
        }
    });
    Ok(())
}

fn follow_size(master: &OwnedFd) -> Result<(), String> {
    let master = master.try_clone().map_err(|error| error.to_string())?;
    thread::spawn(move || {
        let mut last = window_size(0).map(key);
        loop {
            thread::sleep(SIZE_POLL);
            let Some(size) = window_size(0) else {
                continue;
            };
            if Some(key(size)) != last {
                last = Some(key(size));
                // SAFETY: master is an open pty descriptor and size is a live winsize for the call.
                unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &raw const size) };
            }
        }
    });
    Ok(())
}

fn key(size: libc::winsize) -> (u16, u16, u16, u16) {
    (size.ws_row, size.ws_col, size.ws_xpixel, size.ws_ypixel)
}

fn take_control(dir: &Path, master: &OwnedFd, events: Events) -> Result<(), String> {
    let fifo = dir.join(CONTROL_FIFO);
    if !fifo.exists() {
        return Ok(());
    }
    let mut writer = File::from(master.try_clone().map_err(|error| error.to_string())?);
    thread::spawn(move || {
        let Ok(control) = OpenOptions::new().read(true).open(&fifo) else {
            return;
        };
        for line in BufReader::new(control).lines() {
            let Ok(line) = line else {
                return;
            };
            let Some((label, keys)) = parse_control_line(&line) else {
                continue;
            };
            record(&events, &format!("i {} {label}\n", now_micros()));
            if writer.write_all(&keys).is_err() {
                return;
            }
        }
    });
    Ok(())
}

fn window_size(fd: i32) -> Option<libc::winsize> {
    let mut size = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: size is a live winsize for TIOCGWINSZ to fill, and an invalid fd only makes the call fail.
    let read = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &raw mut size) };
    (read == 0 && size.ws_col > 0).then_some(size)
}

struct RawMode {
    saved: Option<libc::termios>,
}

impl RawMode {
    fn enter() -> Self {
        // SAFETY: termios is plain data, so all zeroes is a valid value for tcgetattr to overwrite.
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: saved is a live termios for tcgetattr to fill.
        if unsafe { libc::tcgetattr(0, &raw mut saved) } != 0 {
            return Self { saved: None };
        }
        let mut raw = saved;
        // SAFETY: raw is a live termios that cfmakeraw edits in place.
        unsafe { libc::cfmakeraw(&raw mut raw) };
        // SAFETY: raw is a live termios that tcsetattr only reads.
        unsafe { libc::tcsetattr(0, libc::TCSANOW, &raw const raw) };
        Self { saved: Some(saved) }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        if let Some(saved) = self.saved {
            // SAFETY: saved is a live termios that tcsetattr only reads.
            unsafe { libc::tcsetattr(0, libc::TCSANOW, &raw const saved) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_lines_carry_any_bytes_through_hex() {
        let keys = b"\x1b[B\x04j\tq";
        let line = control_line("scroll", keys);
        assert_eq!(
            parse_control_line(&line),
            Some(("scroll".to_owned(), keys.to_vec()))
        );
    }

    #[test]
    fn a_line_without_a_label_is_refused() {
        assert_eq!(parse_control_line("1b5b42\n"), None);
    }
}
