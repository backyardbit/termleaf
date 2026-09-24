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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

    session.send(&WHEEL_DOWN.repeat(80))?;
    session.step(
        "wheel-scrolls-into-page-two",
        "page 2/3 · doc.pdf",
        Page::Changed,
    )?;

    session.send("gg")?;
    session.step("back-to-first-page", "page 1/3 · doc.pdf", Page::Changed)?;

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

    session.send(CONTROL_WHEEL_UP)?;
    session.step(
        "control-wheel-zooms",
        "page 2/2 · 110% · doc.pdf",
        Page::Changed,
    )?;

    session.send(DOUBLE_CLICK)?;
    session.step(
        "double-click-resets-the-zoom",
        "page 2/2 · doc.pdf",
        Page::Changed,
    )?;

    session.send(DOUBLE_CLICK)?;
    session.step(
        "double-click-fits-the-page",
        "page 2/2 · fit page · doc.pdf",
        Page::Changed,
    )?;

    session.send("q")?;
    session.wait_for_status("termleaf to quit", |status| !status.contains("doc.pdf"))?;
    Ok(())
}

const WHEEL_DOWN: &str = "\x1b[<65;40;10M";
const CONTROL_WHEEL_UP: &str = "\x1b[<80;40;10M";
const DOUBLE_CLICK: &str = "\x1b[<0;40;10M\x1b[<0;40;10m\x1b[<0;40;10M\x1b[<0;40;10m";

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
        let mut settle = Settle::new(page, self.previous.take());
        let accepted = poll(STEP_TIMEOUT, "the page image to settle", || {
            let screenshot = self.screenshot(label).ok()?;
            settle.observe(screenshot)
        });
        let last = settle.changed_since_previous_step();
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

fn is_paper(screenshot: &RgbImage, x: u32, y: u32) -> bool {
    screenshot
        .get_pixel(x, y)
        .0
        .iter()
        .all(|&channel| channel >= PAPER_THRESHOLD)
}

pub fn page_region(screenshot: &RgbImage) -> Option<Region> {
    let (width, height) = screenshot.dimensions();
    let tall_columns: Vec<u32> = (0..width)
        .filter(|&x| {
            let paper = (0..height).filter(|&y| is_paper(screenshot, x, y)).count();
            paper * 4 >= usize::try_from(height).unwrap_or(usize::MAX)
        })
        .collect();
    let left = *tall_columns.first()?;
    let right = *tall_columns.last()?;
    let edge = ((right - left + 1) / 10).max(2);
    let beside_page: Vec<u32> = (left.saturating_sub(edge)..left)
        .chain(right.saturating_add(1)..right.saturating_add(edge + 1).min(width))
        .collect();
    let page_rows: Vec<u32> = (0..height)
        .filter(|&y| {
            let inside = (left..=right)
                .filter(|&x| is_paper(screenshot, x, y))
                .count();
            let beside = beside_page
                .iter()
                .filter(|&&x| is_paper(screenshot, x, y))
                .count();
            let inside_width = usize::try_from(right - left + 1).unwrap_or(usize::MAX);
            inside * 2 >= inside_width && beside * 2 < beside_page.len().max(1)
        })
        .collect();
    let top = *page_rows.first()?;
    let mut bottom = *page_rows.last()?;
    let margin = left..left + ((right - left + 1) / 40).max(2);
    let text_at_the_left_edge = |y: u32| margin.clone().any(|x| !is_paper(screenshot, x, y));
    if let Some(status_text) = (top..=bottom)
        .rev()
        .find(|&y| page_rows.contains(&y) && text_at_the_left_edge(y))
    {
        let blank_row = |y: u32| (left..=right).all(|x| is_paper(screenshot, x, y));
        bottom = (top..status_text)
            .rev()
            .find(|&y| blank_row(y))
            .unwrap_or(top);
        bottom = (top..=bottom)
            .rev()
            .find(|&y| page_rows.contains(&y))
            .unwrap_or(top);
    }
    Some(Region {
        x: left,
        y: top,
        width: right - left + 1,
        height: bottom - top + 1,
    })
}

pub fn changed_page_pixels(before: &RgbImage, after: &RgbImage) -> usize {
    if before.dimensions() != after.dimensions() {
        return usize::MAX;
    }
    let Some(region) = page_region(after).or_else(|| page_region(before)) else {
        return changed_pixels(before, after);
    };
    (region.y..region.y + region.height)
        .flat_map(|y| (region.x..region.x + region.width).map(move |x| (x, y)))
        .filter(|&(x, y)| before.get_pixel(x, y) != after.get_pixel(x, y))
        .count()
}

pub fn changed_status_pixels(before: &RgbImage, after: &RgbImage) -> usize {
    if before.dimensions() != after.dimensions() {
        return usize::MAX;
    }
    let Some(region) = page_region(after) else {
        return changed_pixels(before, after);
    };
    let (width, height) = after.dimensions();
    (region.y + region.height..height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .filter(|&(x, y)| before.get_pixel(x, y) != after.get_pixel(x, y))
        .count()
}

struct Settle {
    expectation: Page,
    previous_step: Option<RgbImage>,
    last: Option<RgbImage>,
    still_frames: usize,
}

impl Settle {
    fn new(expectation: Page, previous_step: Option<RgbImage>) -> Self {
        Self {
            expectation,
            previous_step,
            last: None,
            still_frames: 0,
        }
    }

    fn required_still_frames(&self) -> usize {
        match self.expectation {
            Page::Changed => 1,
            Page::Unchanged => 3,
        }
    }

    fn observe(&mut self, screenshot: RgbImage) -> Option<RgbImage> {
        if !page_is_drawn(&screenshot) {
            self.last = None;
            self.still_frames = 0;
            return None;
        }
        let held_still = self
            .last
            .as_ref()
            .is_some_and(|last| changed_page_pixels(last, &screenshot) == 0);
        self.still_frames = if held_still { self.still_frames + 1 } else { 0 };
        self.last = Some(screenshot.clone());
        if self.still_frames < self.required_still_frames() {
            return None;
        }
        let changed = self.previous_step.as_ref().map_or(usize::MAX, |before| {
            changed_page_pixels(before, &screenshot)
        });
        let status_changed = self
            .previous_step
            .as_ref()
            .is_none_or(|before| changed_status_pixels(before, &screenshot) > 0);
        let expected = match self.expectation {
            Page::Changed => changed >= MIN_CHANGED_PIXELS && status_changed,
            Page::Unchanged => changed < MIN_CHANGED_PIXELS,
        };
        expected.then_some(screenshot)
    }

    fn changed_since_previous_step(&self) -> Option<usize> {
        let last = self.last.as_ref()?;
        let previous = self.previous_step.as_ref()?;
        Some(changed_page_pixels(previous, last))
    }
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

    fn fill(
        screen: &mut RgbImage,
        xs: std::ops::Range<u32>,
        ys: std::ops::Range<u32>,
        colour: Rgb<u8>,
    ) {
        for x in xs {
            for y in ys.clone() {
                screen.put_pixel(x, y, colour);
            }
        }
    }

    fn page_above_status_bar(status_text_at: u32) -> RgbImage {
        let mut screen = dark_screen();
        fill(&mut screen, 30..70, 0..80, WHITE);
        fill(&mut screen, 0..100, 90..96, WHITE);
        fill(
            &mut screen,
            status_text_at..status_text_at + 5,
            92..94,
            DARK,
        );
        screen
    }

    #[test]
    fn the_page_region_excludes_the_full_width_status_bar() {
        assert_eq!(
            page_region(&page_above_status_bar(0)),
            Some(Region {
                x: 30,
                y: 0,
                width: 40,
                height: 80
            })
        );
    }

    #[test]
    fn the_page_region_excludes_a_status_bar_that_only_spans_the_pane() {
        let mut screen = dark_screen();
        fill(&mut screen, 40..70, 0..80, WHITE);
        fill(&mut screen, 30..80, 90..96, WHITE);
        assert_eq!(
            page_region(&screen),
            Some(Region {
                x: 40,
                y: 0,
                width: 30,
                height: 80
            })
        );
    }

    #[test]
    fn the_page_region_stops_above_status_text_when_the_page_fills_the_pane() {
        let mut screen = dark_screen();
        fill(&mut screen, 20..80, 0..96, WHITE);
        fill(&mut screen, 40..50, 30..32, DARK);
        fill(&mut screen, 20..45, 92..94, DARK);
        fill(&mut screen, 60..62, 91..94, DARK);
        assert_eq!(
            page_region(&screen),
            Some(Region {
                x: 20,
                y: 0,
                width: 60,
                height: 91
            })
        );
    }

    #[test]
    fn a_dark_screen_has_no_page_region() {
        assert_eq!(page_region(&dark_screen()), None);
    }

    #[test]
    fn status_bar_changes_do_not_count_as_page_changes() {
        let before = page_above_status_bar(0);
        let after = page_above_status_bar(50);
        assert!(changed_pixels(&before, &after) > 0);
        assert_eq!(changed_page_pixels(&before, &after), 0);
    }

    #[test]
    fn changes_inside_the_page_are_counted() {
        let before = page_above_status_bar(0);
        let mut after = before.clone();
        fill(&mut after, 40..50, 10..12, DARK);
        assert_eq!(changed_page_pixels(&before, &after), 20);
    }
    fn page_with_mark(mark_x: u32) -> RgbImage {
        page_with_mark_and_status(mark_x, 0)
    }

    fn page_with_mark_and_status(mark_x: u32, status_text_at: u32) -> RgbImage {
        let mut screen = page_above_status_bar(status_text_at);
        fill(&mut screen, mark_x..mark_x + 10, 10..30, DARK);
        screen
    }

    fn observe_all(settle: &mut Settle, screenshots: &[RgbImage]) -> Vec<bool> {
        screenshots
            .iter()
            .map(|screenshot| settle.observe(screenshot.clone()).is_some())
            .collect()
    }

    #[test]
    fn a_new_page_is_accepted_once_it_holds_still() {
        let mut settle = Settle::new(Page::Changed, Some(page_with_mark(32)));
        let new = page_with_mark_and_status(55, 20);
        assert_eq!(observe_all(&mut settle, &[new.clone(), new]), [false, true]);
    }

    #[test]
    fn a_new_page_under_the_old_status_bar_is_not_the_new_frame_yet() {
        let mut settle = Settle::new(Page::Changed, Some(page_with_mark(32)));
        let new = page_with_mark(55);
        assert_eq!(
            observe_all(&mut settle, &[new.clone(), new.clone(), new]),
            [false, false, false]
        );
    }

    #[test]
    fn the_stale_previous_page_is_never_accepted_as_changed() {
        let old = page_with_mark(32);
        let new = page_with_mark_and_status(55, 20);
        let mut settle = Settle::new(Page::Changed, Some(old.clone()));
        assert_eq!(
            observe_all(
                &mut settle,
                &[old.clone(), old.clone(), old, new.clone(), new]
            ),
            [false, false, false, false, true]
        );
    }

    #[test]
    fn an_unchanged_page_must_hold_still_for_longer() {
        let page = page_with_mark(32);
        let mut settle = Settle::new(Page::Unchanged, Some(page.clone()));
        assert_eq!(
            observe_all(
                &mut settle,
                &[page.clone(), page.clone(), page.clone(), page]
            ),
            [false, false, false, true]
        );
    }

    #[test]
    fn a_different_page_is_never_accepted_as_unchanged() {
        let other = page_with_mark(55);
        let mut settle = Settle::new(Page::Unchanged, Some(page_with_mark(32)));
        assert_eq!(
            observe_all(
                &mut settle,
                &[other.clone(), other.clone(), other.clone(), other]
            ),
            [false, false, false, false]
        );
    }

    #[test]
    fn a_screen_without_a_page_is_never_accepted() {
        let blank = dark_screen();
        let mut settle = Settle::new(Page::Changed, None);
        assert_eq!(
            observe_all(&mut settle, &[blank.clone(), blank.clone(), blank]),
            [false, false, false]
        );
    }

    #[test]
    fn the_first_page_needs_no_previous_step() {
        let page = page_with_mark(32);
        let mut settle = Settle::new(Page::Changed, None);
        assert_eq!(
            observe_all(&mut settle, &[page.clone(), page]),
            [false, true]
        );
    }
}
