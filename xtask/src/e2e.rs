use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use image::RgbImage;
use serde_json::Value;

const PAPER_THRESHOLD: u8 = 230;
const MIN_PAPER_PERCENT: usize = 10;
const MIN_CHANGED_PIXELS: usize = 100;
const POLL_INTERVAL: Duration = Duration::from_millis(250);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);
const STEP_TIMEOUT: Duration = Duration::from_secs(20);

type Outcome<T> = Result<T, String>;

pub fn run(root: &Path) -> ExitCode {
    if std::env::var_os("CI").is_none() {
        eprintln!(
            "cargo xtask e2e opens a terminal window and captures the screen, so it only runs in CI"
        );
        return ExitCode::FAILURE;
    }
    match scenario(root) {
        Ok(()) => {
            println!("e2e passed");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("e2e failed: {message}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Changed,
    Unchanged,
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

    let fixtures = root.join("tests/fixtures");
    let work = root.join("target/e2e");
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(work.join("screenshots")).map_err(|error| error.to_string())?;
    let doc = work.join("doc.pdf");
    copy(&fixtures.join("three-pages.pdf"), &doc)?;

    let mut session = Session::start(&work)?;
    session.herdr(&[
        "pane",
        "run",
        &session.pane.clone(),
        &format!(
            "'{}' '{}'",
            root.join("target/release/termleaf").display(),
            doc.display()
        ),
    ])?;
    session.step("start", "page 1/3 · doc.pdf", Page::Changed)?;

    session.send("j")?;
    session.step("next-page", "page 2/3 · doc.pdf", Page::Changed)?;

    session.send("G")?;
    session.step("last-page", "page 3/3 · doc.pdf", Page::Changed)?;

    copy(&fixtures.join("five-pages.pdf"), &doc)?;
    session.step("rebuild-keeps-page", "page 3/5 · doc.pdf", Page::Changed)?;

    let five = fs::read(fixtures.join("five-pages.pdf")).map_err(|error| error.to_string())?;
    fs::write(&doc, &five[..five.len() / 2]).map_err(|error| error.to_string())?;
    session.step(
        "half-written-keeps-last-page",
        "page 3/5 · doc.pdf · ✗ unreadable",
        Page::Unchanged,
    )?;

    let staging = work.join("doc.pdf.tmp");
    copy(&fixtures.join("two-pages.pdf"), &staging)?;
    fs::rename(&staging, &doc).map_err(|error| error.to_string())?;
    session.step("rename-replace-clamps", "page 2/2 · doc.pdf", Page::Changed)?;

    session.send("q")?;
    session.wait_for_status("termleaf to quit", |status| !status.contains("doc.pdf"))?;
    Ok(())
}

fn copy(from: &Path, to: &Path) -> Outcome<()> {
    fs::copy(from, to)
        .map(|_| ())
        .map_err(|error| format!("copying {} to {}: {error}", from.display(), to.display()))
}

struct Session {
    name: String,
    terminal: Child,
    pane: String,
    screenshots: PathBuf,
    previous: Option<RgbImage>,
    taken: usize,
}

impl Session {
    fn start(work: &Path) -> Outcome<Self> {
        let name = format!("termleaf-e2e-{}", std::process::id());
        let log = fs::File::create(work.join("ghostty.log")).map_err(|error| error.to_string())?;
        let terminal = Command::new("ghostty")
            .args([
                "--gtk-single-instance=false",
                "--config-default-files=false",
                "--window-decoration=false",
                "--font-size=12",
                "--window-width=110",
                "--window-height=36",
                "-e",
                "herdr",
                "--session",
                &name,
            ])
            .env("GDK_BACKEND", "x11")
            .env("LIBGL_ALWAYS_SOFTWARE", "1")
            .env_remove("DBUS_SESSION_BUS_ADDRESS")
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .map_err(|error| format!("starting ghostty: {error}"))?;
        let mut session = Self {
            name,
            terminal,
            pane: String::new(),
            screenshots: work.join("screenshots"),
            previous: None,
            taken: 0,
        };
        session.pane = poll(STARTUP_TIMEOUT, "herdr to open a pane", || {
            session
                .herdr(&["pane", "list"])
                .ok()
                .and_then(|json| first_pane_id(&json))
        })?;
        Ok(session)
    }

    fn herdr(&self, args: &[&str]) -> Outcome<String> {
        let output = Command::new("herdr")
            .arg("--session")
            .arg(&self.name)
            .args(args)
            .output()
            .map_err(|error| format!("running herdr: {error}"))?;
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        if output.status.success() {
            Ok(text)
        } else {
            Err(format!(
                "herdr {} failed: {text}{}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr)
            ))
        }
    }

    fn send(&self, keys: &str) -> Outcome<()> {
        self.herdr(&["pane", "send-text", &self.pane, keys])
            .map(|_| ())
    }

    fn screen(&self) -> String {
        self.herdr(&["pane", "read", &self.pane, "--source", "visible"])
            .unwrap_or_default()
    }

    fn wait_for_status(&self, wanted: &str, matches: impl Fn(&str) -> bool) -> Outcome<()> {
        poll(STEP_TIMEOUT, wanted, || {
            let screen = self.screen();
            status_line(&screen)
                .filter(|status| matches(status))
                .map(|_| ())
        })
        .map_err(|error| format!("{error}; last screen:\n{}", self.screen()))
    }

    fn step(&mut self, label: &str, status: &str, page: Page) -> Outcome<()> {
        self.wait_for_status(&format!("status `{status}`"), |shown| shown == status)?;
        let previous = self.previous.take();
        let mut last = None;
        let accepted = poll(STEP_TIMEOUT, "the page image", || {
            let screenshot = self.screenshot(label).ok()?;
            let changed = previous
                .as_ref()
                .map_or(usize::MAX, |before| changed_pixels(before, &screenshot));
            let settled = match page {
                Page::Changed => changed >= MIN_CHANGED_PIXELS,
                Page::Unchanged => changed < MIN_CHANGED_PIXELS,
            };
            let ready = page_is_drawn(&screenshot) && settled;
            last = Some(changed);
            ready.then_some(screenshot)
        });
        match accepted {
            Ok(screenshot) => {
                println!("ok   {label}: {status}");
                self.previous = Some(screenshot);
                Ok(())
            }
            Err(error) => Err(format!(
                "step {label}: {error} (changed pixels: {last:?}, screenshots in {})",
                self.screenshots.display()
            )),
        }
    }

    fn screenshot(&mut self, label: &str) -> Outcome<RgbImage> {
        self.taken += 1;
        let path = self
            .screenshots
            .join(format!("{:03}-{label}.png", self.taken));
        let status = Command::new("import")
            .args(["-window", "root"])
            .arg(&path)
            .status()
            .map_err(|error| format!("running import: {error}"))?;
        if !status.success() {
            return Err("import failed".to_owned());
        }
        image::open(&path)
            .map(|image| image.to_rgb8())
            .map_err(|error| error.to_string())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.herdr(&["server", "stop"]);
        let _ = self.terminal.kill();
        let _ = self.terminal.wait();
    }
}

fn poll<T>(
    timeout: Duration,
    waiting_for: &str,
    mut attempt: impl FnMut() -> Option<T>,
) -> Outcome<T> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(value) = attempt() {
            return Ok(value);
        }
        if Instant::now() >= deadline {
            return Err(format!("timed out waiting for {waiting_for}"));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

pub fn first_pane_id(pane_list_json: &str) -> Option<String> {
    let response: Value = serde_json::from_str(pane_list_json).ok()?;
    response
        .pointer("/result/panes/0/pane_id")?
        .as_str()
        .map(str::to_owned)
}

pub fn status_line(screen: &str) -> Option<&str> {
    screen.lines().map(str::trim).rfind(|line| !line.is_empty())
}

pub fn page_is_drawn(screenshot: &RgbImage) -> bool {
    let paper = screenshot
        .pixels()
        .filter(|pixel| pixel.0.iter().all(|&channel| channel >= PAPER_THRESHOLD))
        .count();
    let total = screenshot.pixels().len();
    total > 0 && paper * 100 >= total * MIN_PAPER_PERCENT
}

pub fn changed_pixels(before: &RgbImage, after: &RgbImage) -> usize {
    if before.dimensions() != after.dimensions() {
        return usize::MAX;
    }
    before
        .pixels()
        .zip(after.pixels())
        .filter(|(old, new)| old != new)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    const DARK: Rgb<u8> = Rgb([40, 44, 52]);
    const WHITE: Rgb<u8> = Rgb([255, 255, 255]);

    fn dark_screen() -> RgbImage {
        RgbImage::from_pixel(100, 100, DARK)
    }

    fn screen_with_page(page_width: u32) -> RgbImage {
        let mut screen = dark_screen();
        for x in 0..page_width {
            for y in 0..100 {
                screen.put_pixel(x, y, WHITE);
            }
        }
        screen
    }

    #[test]
    fn finds_the_first_pane_in_a_pane_list() {
        let json = r#"{"id":"cli:pane:list","result":{"panes":[{"pane_id":"w1:p1","focused":true},{"pane_id":"w1:p2"}]}}"#;
        assert_eq!(first_pane_id(json).as_deref(), Some("w1:p1"));
    }

    #[test]
    fn no_pane_in_an_error_response() {
        let json = r#"{"id":"cli:request","error":{"code":"server_not_running"}}"#;
        assert_eq!(first_pane_id(json), None);
    }

    #[test]
    fn status_line_is_the_last_non_blank_line() {
        let screen = "\n\npage 2/3 · doc.pdf   \n\n";
        assert_eq!(status_line(screen), Some("page 2/3 · doc.pdf"));
    }

    #[test]
    fn a_blank_screen_has_no_status_line() {
        assert_eq!(status_line("\n  \n"), None);
    }

    #[test]
    fn a_dark_screen_has_no_page() {
        assert!(!page_is_drawn(&dark_screen()));
    }

    #[test]
    fn a_white_page_on_the_screen_counts_as_drawn() {
        assert!(page_is_drawn(&screen_with_page(40)));
    }

    #[test]
    fn a_thin_white_strip_is_not_a_page() {
        assert!(!page_is_drawn(&screen_with_page(2)));
    }

    #[test]
    fn identical_screens_have_no_changed_pixels() {
        assert_eq!(changed_pixels(&dark_screen(), &dark_screen()), 0);
    }

    #[test]
    fn counts_pixels_that_changed() {
        assert_eq!(changed_pixels(&dark_screen(), &screen_with_page(3)), 300);
    }

    #[test]
    fn screens_of_different_sizes_count_as_fully_changed() {
        let small = RgbImage::from_pixel(10, 10, DARK);
        assert_eq!(changed_pixels(&small, &dark_screen()), usize::MAX);
    }
}
