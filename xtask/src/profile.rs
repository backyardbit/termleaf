mod cpu;
mod report;
pub mod tap;
mod timeline;
mod traffic;

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use cpu::{Role, Snapshot};
use report::{Run, STARTUP, Step};
use timeline::Timeline;

type Outcome<T> = Result<T, String>;

const DOWN: &[u8] = b"\x1b[B";
const HALF_PANE: &[u8] = b"\x04";
const READY_QUIET: Duration = Duration::from_millis(1500);
const STEP_QUIET: Duration = Duration::from_millis(800);
const READY_TIMEOUT: Duration = Duration::from_secs(120);
const STEP_TIMEOUT: Duration = Duration::from_secs(30);
const QUIT_TIMEOUT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(50);
const PIXEL_APP_DIR: &str = "xtask/profile/pixel-pages";

struct Script {
    label: &'static str,
    keys: &'static [u8],
    repeat: usize,
    gap: Duration,
}

const SCRIPT: &[Script] = &[
    Script {
        label: "scroll 40 × 3 rows",
        keys: DOWN,
        repeat: 40,
        gap: Duration::from_millis(30),
    },
    Script {
        label: "8 half panes",
        keys: HALF_PANE,
        repeat: 8,
        gap: Duration::from_millis(250),
    },
    Script {
        label: "8 next pages",
        keys: b"j",
        repeat: 8,
        gap: Duration::from_millis(300),
    },
    Script {
        label: "back to the top",
        keys: b"gg",
        repeat: 1,
        gap: Duration::from_millis(0),
    },
    Script {
        label: "scroll again, 40 × 3 rows",
        keys: DOWN,
        repeat: 40,
        gap: Duration::from_millis(30),
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Viewer {
    Termleaf,
    Pixel,
}

impl Viewer {
    fn label(self) -> &'static str {
        match self {
            Viewer::Termleaf => "termleaf",
            Viewer::Pixel => "PixelGUI",
        }
    }

    fn slug(self) -> &'static str {
        match self {
            Viewer::Termleaf => "termleaf",
            Viewer::Pixel => "pixel",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Terminal {
    Ghostty,
    Kitty,
    Wezterm,
    Xterm,
    Here,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Multiplexer {
    Bare,
    Herdr,
    Tmux,
    Zellij,
}

#[derive(Debug, Clone, Copy)]
struct Setup {
    terminal: Terminal,
    multiplexer: Multiplexer,
}

impl Setup {
    fn name(self) -> String {
        let terminal = match self.terminal {
            Terminal::Ghostty => "Ghostty".to_owned(),
            Terminal::Kitty => "kitty".to_owned(),
            Terminal::Wezterm => "WezTerm".to_owned(),
            Terminal::Xterm => "xterm -ti vt340".to_owned(),
            Terminal::Here => {
                std::env::var("TERM_PROGRAM").unwrap_or_else(|_| "this terminal".to_owned())
            }
        };
        match self.multiplexer {
            Multiplexer::Bare => terminal,
            Multiplexer::Herdr => format!("{terminal} + herdr"),
            Multiplexer::Tmux => format!("{terminal} + tmux"),
            Multiplexer::Zellij => format!("{terminal} + zellij"),
        }
    }

    fn slug(self) -> String {
        self.name()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect()
    }
}

const CI_SETUPS: &[Setup] = &[
    Setup {
        terminal: Terminal::Ghostty,
        multiplexer: Multiplexer::Bare,
    },
    Setup {
        terminal: Terminal::Ghostty,
        multiplexer: Multiplexer::Herdr,
    },
    Setup {
        terminal: Terminal::Kitty,
        multiplexer: Multiplexer::Bare,
    },
    Setup {
        terminal: Terminal::Ghostty,
        multiplexer: Multiplexer::Tmux,
    },
    Setup {
        terminal: Terminal::Wezterm,
        multiplexer: Multiplexer::Bare,
    },
    Setup {
        terminal: Terminal::Xterm,
        multiplexer: Multiplexer::Bare,
    },
    Setup {
        terminal: Terminal::Xterm,
        multiplexer: Multiplexer::Zellij,
    },
];

struct Paths {
    xtask: PathBuf,
    termleaf: PathBuf,
    pixel_app: PathBuf,
    doc: PathBuf,
    work: PathBuf,
}

pub fn run(root: &Path) -> ExitCode {
    if std::env::var_os("CI").is_none() {
        eprintln!(
            "cargo xtask profile opens terminal windows on a virtual display, so it only runs in CI. Use cargo xtask profile-here to profile inside the terminal you are in."
        );
        return ExitCode::FAILURE;
    }
    finish(
        root,
        "target/profile",
        CI_SETUPS,
        &[Viewer::Termleaf, Viewer::Pixel],
    )
}

pub fn here(root: &Path, args: &[String]) -> ExitCode {
    let viewers: Vec<Viewer> = match args.first().map(String::as_str) {
        Some("termleaf") => vec![Viewer::Termleaf],
        Some("pixel") => vec![Viewer::Pixel],
        _ => vec![Viewer::Termleaf, Viewer::Pixel],
    };
    let setups = [Setup {
        terminal: Terminal::Here,
        multiplexer: Multiplexer::Bare,
    }];
    finish(root, "target/profile-here", &setups, &viewers)
}

fn finish(root: &Path, work: &str, setups: &[Setup], viewers: &[Viewer]) -> ExitCode {
    let work = root.join(work);
    let runs = match prepare(root, &work, viewers) {
        Ok(paths) => measure_all(&paths, setups, viewers),
        Err(message) => {
            eprintln!("profile: {message}");
            return ExitCode::FAILURE;
        }
    };
    let machine = cpu::describe();
    let summary = report::markdown(&runs, &machine);
    let _ = fs::write(work.join("summary.md"), &summary);
    let _ = fs::write(
        work.join("results.json"),
        serde_json::to_string_pretty(&report::json(&runs, &machine)).unwrap_or_default(),
    );
    if let Some(path) = std::env::var_os("GITHUB_STEP_SUMMARY")
        && let Ok(mut file) = OpenOptions::new().append(true).open(path)
    {
        let _ = file.write_all(summary.as_bytes());
    }
    println!("{summary}");
    println!("wrote {}", work.join("summary.md").display());
    if runs.iter().all(|run| run.error.is_some()) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn prepare(root: &Path, work: &Path, viewers: &[Viewer]) -> Outcome<Paths> {
    let _ = fs::remove_dir_all(work);
    fs::create_dir_all(work).map_err(|error| error.to_string())?;
    let built = Command::new(env!("CARGO"))
        .args(["build", "--release", "--package", "termleaf"])
        .current_dir(root)
        .status()
        .map_err(|error| format!("running cargo build: {error}"))?;
    if !built.success() {
        return Err("building termleaf failed".to_owned());
    }
    let pixel_app = root.join(PIXEL_APP_DIR);
    if viewers.contains(&Viewer::Pixel) {
        build_pixel_app(&pixel_app)?;
    }
    let doc = work.join("dense.pdf");
    fs::copy(root.join("tests/fixtures/dense.pdf"), &doc)
        .map_err(|error| format!("copying the fixture: {error}"))?;
    Ok(Paths {
        xtask: std::env::current_exe().map_err(|error| error.to_string())?,
        termleaf: root.join("target/release/termleaf"),
        pixel_app,
        doc,
        work: work.to_owned(),
    })
}

fn build_pixel_app(app: &Path) -> Outcome<()> {
    if !app.join("node_modules/.bin/pixel").exists() {
        let installed = Command::new("npm")
            .args(["ci", "--no-audit", "--no-fund"])
            .current_dir(app)
            .status()
            .map_err(|error| format!("running npm ci: {error}"))?;
        if !installed.success() {
            return Err("npm ci for the PixelGUI app failed".to_owned());
        }
    }
    let compiled = Command::new("npx")
        .args(["tsc", "-p", "tsconfig.json"])
        .current_dir(app)
        .status()
        .map_err(|error| format!("running tsc: {error}"))?;
    if !compiled.success() {
        return Err("compiling the PixelGUI app failed".to_owned());
    }
    Ok(())
}

fn measure_all(paths: &Paths, setups: &[Setup], viewers: &[Viewer]) -> Vec<Run> {
    let mut runs = Vec::new();
    for setup in setups {
        for viewer in viewers {
            if setup.terminal != Terminal::Here {
                eprintln!("profile: {} in {}", viewer.label(), setup.name());
            }
            let dir = paths
                .work
                .join(format!("{}-{}", setup.slug(), viewer.slug()));
            let run = match measure(*setup, *viewer, paths, &dir) {
                Ok(run) => run,
                Err(message) => Run {
                    setup: setup.name(),
                    viewer: viewer.label().to_owned(),
                    renderer: String::new(),
                    steps: Vec::new(),
                    error: Some(message),
                },
            };
            if setup.terminal != Terminal::Here {
                eprintln!(
                    "profile: {} in {}: {}",
                    viewer.label(),
                    setup.name(),
                    run.error.as_deref().unwrap_or("done")
                );
            }
            runs.push(run);
        }
    }
    runs
}

fn viewer_command(viewer: Viewer, paths: &Paths, dir: &Path) -> Vec<String> {
    match viewer {
        Viewer::Termleaf => vec![
            paths.termleaf.display().to_string(),
            "--no-pinch".to_owned(),
            "--no-follow".to_owned(),
            paths.doc.display().to_string(),
        ],
        Viewer::Pixel => vec![
            "env".to_owned(),
            format!("PIXEL_PAGES_INFO={}", dir.join("pixel-info.json").display()),
            paths
                .pixel_app
                .join("node_modules/.bin/pixel")
                .display()
                .to_string(),
            paths.pixel_app.display().to_string(),
            "--".to_owned(),
            paths.doc.display().to_string(),
        ],
    }
}

fn shell_line(words: &[String]) -> String {
    words
        .iter()
        .map(|word| format!("'{}'", word.replace('\'', r"'\''")))
        .collect::<Vec<_>>()
        .join(" ")
}

struct Session {
    terminal: Option<Child>,
    multiplexer: Multiplexer,
    name: String,
    dir: PathBuf,
}

impl Drop for Session {
    fn drop(&mut self) {
        match self.multiplexer {
            Multiplexer::Herdr => {
                let _ =
                    quiet(Command::new("herdr").args(["--session", &self.name, "server", "stop"]));
            }
            Multiplexer::Tmux => {
                let _ = quiet(Command::new("tmux").args(["-L", &self.name, "kill-server"]));
            }
            Multiplexer::Zellij => {
                let _ = quiet(
                    Command::new("zellij")
                        .args(["kill-all-sessions", "--yes"])
                        .env("ZELLIJ_SOCKET_DIR", self.dir.join("zellij-sockets")),
                );
            }
            Multiplexer::Bare => {}
        }
        if let Some(terminal) = self.terminal.as_mut() {
            let _ = terminal.kill();
            let _ = terminal.wait();
        }
    }
}

fn quiet(command: &mut Command) -> std::io::Result<std::process::ExitStatus> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
}

fn measure(setup: Setup, viewer: Viewer, paths: &Paths, dir: &Path) -> Outcome<Run> {
    fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    let control = tap::make_control_fifo(dir)?;
    let name = format!("profile-{}-{}", std::process::id(), setup.slug());
    let words = viewer_command(viewer, paths, dir);
    let inner = inner_command(setup.multiplexer, &words, &name, dir)?;
    let mut tap_command = vec![
        paths.xtask.display().to_string(),
        "profile-tap".to_owned(),
        dir.display().to_string(),
        "--".to_owned(),
    ];
    tap_command.extend(inner);

    let before_start = cpu::snapshot();
    let mut session = Session {
        terminal: None,
        multiplexer: setup.multiplexer,
        name: name.clone(),
        dir: dir.to_owned(),
    };
    session.terminal = Some(launch(setup.terminal, &tap_command, dir)?);
    let mut writer = open_control(&control, &mut session)?;
    let mut launched = None;
    if setup.multiplexer == Multiplexer::Herdr {
        let pane = herdr_pane(&name, &mut session)?;
        launched = Some(tap::now_micros());
        herdr(&name, &["pane", "run", &pane, &shell_line(&words)])?;
    }
    wait_ready(dir, &mut session)?;
    let mut cpu_marks: Vec<Snapshot> = vec![before_start, cpu::snapshot()];
    for step in SCRIPT {
        for _ in 0..step.repeat {
            writer
                .write_all(tap::control_line(step.label, step.keys).as_bytes())
                .map_err(|error| format!("sending keys: {error}"))?;
            writer.flush().map_err(|error| error.to_string())?;
            thread::sleep(step.gap);
        }
        wait_quiet(dir, STEP_QUIET, STEP_TIMEOUT, &mut session)?;
        cpu_marks.push(cpu::snapshot());
    }
    let _ = writer.write_all(tap::control_line("quit", b"q").as_bytes());
    let _ = writer.flush();
    let deadline = Instant::now() + QUIT_TIMEOUT;
    while Instant::now() < deadline {
        if exited(&mut session) {
            break;
        }
        thread::sleep(POLL);
    }
    drop(writer);
    drop(session);
    analyse(setup, viewer, dir, launched, &cpu_marks)
}

fn inner_command(
    multiplexer: Multiplexer,
    words: &[String],
    name: &str,
    dir: &Path,
) -> Outcome<Vec<String>> {
    Ok(match multiplexer {
        Multiplexer::Bare => words.to_vec(),
        Multiplexer::Herdr => vec!["herdr".to_owned(), "--session".to_owned(), name.to_owned()],
        Multiplexer::Tmux => {
            let config = dir.join("tmux.conf");
            fs::write(&config, "set -g allow-passthrough on\nset -g status off\n")
                .map_err(|error| error.to_string())?;
            vec![
                "tmux".to_owned(),
                "-L".to_owned(),
                name.to_owned(),
                "-f".to_owned(),
                config.display().to_string(),
                "new-session".to_owned(),
                shell_line(words),
            ]
        }
        Multiplexer::Zellij => {
            let layout = dir.join("layout.kdl");
            let (program, args) = words.split_first().ok_or("an empty viewer command")?;
            let args: String = args.iter().map(|arg| format!(" {arg:?}")).collect();
            fs::write(
                &layout,
                format!(
                    "layout {{\n    pane command={program:?} {{\n        args{args}\n    }}\n}}\n"
                ),
            )
            .map_err(|error| error.to_string())?;
            let config = dir.join("zellij.kdl");
            fs::write(
                &config,
                "show_startup_tips false\nshow_release_notes false\npane_frames false\n",
            )
            .map_err(|error| error.to_string())?;
            let config_dir = dir.join("zellij-config");
            fs::create_dir_all(&config_dir).map_err(|error| error.to_string())?;
            vec![
                "env".to_owned(),
                format!("ZELLIJ_SOCKET_DIR={}", dir.join("zellij-sockets").display()),
                "zellij".to_owned(),
                "--config-dir".to_owned(),
                config_dir.display().to_string(),
                "--config".to_owned(),
                config.display().to_string(),
                "--new-session-with-layout".to_owned(),
                layout.display().to_string(),
                "--session".to_owned(),
                name.to_owned(),
            ]
        }
    })
}

fn launch(terminal: Terminal, command: &[String], dir: &Path) -> Outcome<Child> {
    let log = fs::File::create(dir.join("terminal.log")).map_err(|error| error.to_string())?;
    let mut launcher = match terminal {
        Terminal::Ghostty => {
            let mut launcher = Command::new("ghostty");
            launcher
                .args([
                    "--gtk-single-instance=false",
                    "--config-default-files=false",
                    "--window-decoration=false",
                    "--font-size=12",
                    "--window-width=110",
                    "--window-height=36",
                    "-e",
                ])
                .env("GDK_BACKEND", "x11")
                .env_remove("DBUS_SESSION_BUS_ADDRESS");
            launcher
        }
        Terminal::Kitty => {
            let mut launcher = Command::new("kitty");
            launcher.args([
                "--config",
                "NONE",
                "-o",
                "initial_window_width=110c",
                "-o",
                "initial_window_height=36c",
                "-o",
                "remember_window_size=no",
                "-o",
                "font_size=12",
            ]);
            launcher
        }
        Terminal::Wezterm => {
            let config = dir.join("wezterm.lua");
            fs::write(
                &config,
                "return { initial_cols = 110, initial_rows = 36, font_size = 12.0, front_end = \"Software\" }\n",
            )
            .map_err(|error| error.to_string())?;
            let mut launcher = Command::new("wezterm");
            launcher.arg("--config-file").arg(&config).args([
                "start",
                "--always-new-process",
                "--",
            ]);
            launcher
        }
        Terminal::Xterm => {
            let mut launcher = Command::new("xterm");
            launcher.args([
                "-ti",
                "vt340",
                "-xrm",
                "XTerm*numColorRegisters: 256",
                "-geometry",
                "110x36+0+0",
                "-fa",
                "DejaVu Sans Mono",
                "-fs",
                "12",
                "-e",
            ]);
            launcher
        }
        Terminal::Here => {
            let (program, args) = command.split_first().ok_or("an empty tap command")?;
            return Command::new(program)
                .args(args)
                .spawn()
                .map_err(|error| format!("starting the tap: {error}"));
        }
    };
    launcher
        .args(command)
        .env("LIBGL_ALWAYS_SOFTWARE", "1")
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(|error| error.to_string())?)
        .stderr(log)
        .spawn()
        .map_err(|error| format!("starting {terminal:?}: {error}"))
}

fn open_control(control: &Path, session: &mut Session) -> Outcome<fs::File> {
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        match OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(control)
        {
            Ok(file) => {
                // SAFETY: file is an open descriptor; F_SETFL only changes its status flags.
                unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, 0) };
                return Ok(file);
            }
            Err(error) if error.raw_os_error() == Some(libc::ENXIO) => {}
            Err(error) => return Err(format!("opening the tap's control pipe: {error}")),
        }
        if exited(session) {
            return Err(format!(
                "the terminal exited before the tap started: {}",
                log_tail(&session.dir)
            ));
        }
        if Instant::now() > deadline {
            return Err("the tap never opened its control pipe".to_owned());
        }
        thread::sleep(POLL);
    }
}

fn exited(session: &mut Session) -> bool {
    session
        .terminal
        .as_mut()
        .is_none_or(|terminal| terminal.try_wait().ok().flatten().is_some())
}

fn log_tail(dir: &Path) -> String {
    let log = fs::read_to_string(dir.join("terminal.log")).unwrap_or_default();
    let tail: Vec<&str> = log.lines().rev().take(5).collect();
    tail.into_iter().rev().collect::<Vec<_>>().join(" / ")
}

fn herdr(name: &str, args: &[&str]) -> Outcome<String> {
    let output = Command::new("herdr")
        .args(["--session", name])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("running herdr: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(format!(
            "herdr {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

fn herdr_pane(name: &str, session: &mut Session) -> Outcome<String> {
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        if let Some(pane) = herdr(name, &["pane", "list"])
            .ok()
            .and_then(|json| crate::e2e::first_pane_id(&json))
        {
            let sized = herdr(name, &["pane", "process-info", "--pane", &pane])
                .ok()
                .and_then(|info| crate::e2e::shell_pid(&info))
                .and_then(pane_pixels)
                .is_some_and(|(width, height)| width > 0 && height > 0);
            if sized {
                return Ok(pane);
            }
        }
        if exited(session) || Instant::now() > deadline {
            return Err("herdr never opened a pane with a pixel size".to_owned());
        }
        thread::sleep(POLL);
    }
}

fn pane_pixels(shell: u32) -> Option<(u16, u16)> {
    let tty = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOCTTY)
        .open(format!("/proc/{shell}/fd/0"))
        .ok()?;
    let mut size = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: tty is an open descriptor for the whole call and size is a live winsize for TIOCGWINSZ to fill.
    let read = unsafe { libc::ioctl(tty.as_raw_fd(), libc::TIOCGWINSZ, &raw mut size) };
    (read == 0).then_some((size.ws_xpixel, size.ws_ypixel))
}

fn output_len(dir: &Path) -> u64 {
    fs::metadata(dir.join(tap::OUTPUT_FILE)).map_or(0, |meta| meta.len())
}

fn wait_ready(dir: &Path, session: &mut Session) -> Outcome<()> {
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        let output = fs::read(dir.join(tap::OUTPUT_FILE)).unwrap_or_default();
        let marks = traffic::scan(&output);
        let drawn = marks.iter().any(|mark| mark.traffic.images_sent() > 0)
            || output.windows(7).any(|window| window == b"page 1/");
        if drawn {
            return wait_quiet(dir, READY_QUIET, READY_TIMEOUT, session);
        }
        if exited(session) {
            return Err(format!(
                "the terminal exited before the first page was drawn: {}",
                log_tail(dir)
            ));
        }
        if Instant::now() > deadline {
            return Err("the first page was never drawn".to_owned());
        }
        thread::sleep(Duration::from_millis(200));
    }
}

fn wait_quiet(
    dir: &Path,
    quiet_for: Duration,
    timeout: Duration,
    session: &mut Session,
) -> Outcome<()> {
    let deadline = Instant::now() + timeout;
    let mut length = output_len(dir);
    let mut since = Instant::now();
    loop {
        thread::sleep(POLL);
        let now = output_len(dir);
        if now != length {
            length = now;
            since = Instant::now();
        } else if since.elapsed() >= quiet_for {
            return Ok(());
        }
        if exited(session) {
            return Err(format!("the terminal exited: {}", log_tail(dir)));
        }
        if Instant::now() > deadline {
            return Err("the output never went quiet".to_owned());
        }
    }
}

fn analyse(
    setup: Setup,
    viewer: Viewer,
    dir: &Path,
    launched: Option<u64>,
    cpu_marks: &[Snapshot],
) -> Outcome<Run> {
    let output = fs::read(dir.join(tap::OUTPUT_FILE)).map_err(|error| error.to_string())?;
    let events =
        fs::read_to_string(dir.join(tap::EVENTS_FILE)).map_err(|error| error.to_string())?;
    let timeline = Timeline::parse(&events);
    let marks = traffic::scan(&output);
    let start = launched
        .or(timeline.started)
        .ok_or("the tap recorded no start")?;
    let step_starts: Vec<u64> = SCRIPT
        .iter()
        .map(|step| {
            timeline
                .inputs_labelled(step.label)
                .first()
                .copied()
                .unwrap_or(u64::MAX)
        })
        .collect();
    let quit = timeline
        .inputs_labelled("quit")
        .first()
        .copied()
        .unwrap_or(u64::MAX);
    let mut steps = Vec::new();

    let startup_end = step_starts.first().copied().unwrap_or(quit);
    let startup_traffic = traffic::between(
        &marks,
        timeline.offset_at(start)..timeline.offset_at(startup_end),
    );
    let first_image = marks
        .iter()
        .filter(|mark| mark.at >= timeline.offset_at(start) && mark.traffic.images_sent() > 0)
        .find_map(|mark| timeline.time_of_offset(mark.at));
    steps.push(Step {
        label: STARTUP.to_owned(),
        keys: 0,
        traffic: startup_traffic,
        first_response_ms: first_image.map(|at| elapsed_ms(start, at)),
        settled_ms: timeline
            .last_output_before(startup_end)
            .map(|at| elapsed_ms(start, at)),
        cpu: spent_between(cpu_marks, 0),
    });

    let mut total = startup_traffic;
    for (index, step) in SCRIPT.iter().enumerate() {
        let inputs = timeline.inputs_labelled(step.label);
        let (Some(&first), Some(&last)) = (inputs.first(), inputs.last()) else {
            continue;
        };
        let end = step_starts.get(index + 1).copied().unwrap_or(quit);
        let traffic = traffic::between(&marks, timeline.offset_at(first)..timeline.offset_at(end));
        total += traffic;
        steps.push(Step {
            label: step.label.to_owned(),
            keys: inputs.len(),
            traffic,
            first_response_ms: timeline
                .first_output_after(first)
                .filter(|at| *at < end)
                .map(|at| elapsed_ms(first, at)),
            settled_ms: timeline
                .last_output_before(end)
                .filter(|at| *at > last)
                .map(|at| elapsed_ms(last, at)),
            cpu: spent_between(cpu_marks, index + 1),
        });
    }
    let mut renderer = report::renderer(&total);
    if viewer == Viewer::Pixel
        && let Ok(info) = fs::read_to_string(dir.join("pixel-info.json"))
        && let Ok(info) = serde_json::from_str::<serde_json::Value>(&info)
        && let Some(name) = info.get("renderer").and_then(serde_json::Value::as_str)
    {
        renderer = format!("{renderer} (PixelGUI renderer: {name})");
    }
    Ok(Run {
        setup: setup.name(),
        viewer: viewer.label().to_owned(),
        renderer,
        steps,
        error: None,
    })
}

fn spent_between(marks: &[Snapshot], index: usize) -> BTreeMap<Role, u64> {
    match (marks.get(index), marks.get(index + 1)) {
        (Some(before), Some(after)) => cpu::spent(before, after),
        _ => BTreeMap::new(),
    }
}

fn elapsed_ms(from: u64, to: u64) -> f64 {
    let micros = to.saturating_sub(from);
    let whole = u32::try_from(micros / 1000).unwrap_or(u32::MAX);
    let part = u32::try_from(micros % 1000).unwrap_or(0);
    f64::from(whole) + f64::from(part) / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_lines_quote_every_word() {
        let words = vec!["/a b/termleaf".to_owned(), "it's.pdf".to_owned()];
        assert_eq!(shell_line(&words), r"'/a b/termleaf' 'it'\''s.pdf'");
    }

    #[test]
    fn every_scripted_step_has_its_own_label() {
        let mut labels: Vec<&str> = SCRIPT.iter().map(|step| step.label).collect();
        labels.push(STARTUP);
        labels.push("quit");
        let count = labels.len();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), count);
    }

    #[test]
    fn setup_names_become_directory_names() {
        let setup = Setup {
            terminal: Terminal::Ghostty,
            multiplexer: Multiplexer::Herdr,
        };
        assert_eq!(setup.slug(), "ghostty---herdr");
    }
}
