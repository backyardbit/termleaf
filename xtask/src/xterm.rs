use std::fs;
use std::path::Path;
use std::process::{Child, Command, ExitCode, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use icy_sixel::SixelImage;
use image::{Rgb, RgbImage};

const MIN_PSNR_DB: f64 = 30.0;
const MAX_OFFSET: u32 = 4;
const POLL_INTERVAL: Duration = Duration::from_millis(250);
const STEP_TIMEOUT: Duration = Duration::from_secs(30);
const PAGE_ORIGIN: &[u8] = b"\x1b[1;1H";
const SIXEL_START: &[u8] = b"\x1bP";
const STRING_TERMINATOR: &[u8] = b"\x1b\\";

type Outcome<T> = Result<T, String>;

pub fn run(root: &Path) -> ExitCode {
    if std::env::var_os("CI").is_none() {
        eprintln!(
            "cargo xtask xterm opens a terminal window and captures the screen, so it only runs in CI"
        );
        return ExitCode::FAILURE;
    }
    match scenario(root) {
        Ok(()) => {
            println!("xterm passed");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("xterm failed: {message}");
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
    let work = root.join("target/xterm");
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).map_err(|error| error.to_string())?;
    let doc = work.join("doc.pdf");
    fs::copy(root.join("tests/fixtures/three-pages.pdf"), &doc)
        .map_err(|error| format!("copying the fixture: {error}"))?;
    let log = work.join("output.log");
    let termleaf = root.join("target/release/termleaf");
    let mut session = Session {
        terminal: Command::new("xterm")
            .args([
                "-ti",
                "vt340",
                "-xrm",
                "XTerm*numColorRegisters: 256",
                "-bw",
                "0",
                "-b",
                "0",
                "-geometry",
                "100x30+0+0",
                "-fa",
                "DejaVu Sans Mono",
                "-fs",
                "11",
                "-e",
                "script",
                "-q",
                "-f",
                "-c",
                &format!("'{}' --no-pinch '{}'", termleaf.display(), doc.display()),
            ])
            .arg(&log)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("starting xterm: {error}"))?,
    };

    let first = session.frame_shown(&log, &work, "start", 1)?;
    println!("ok   start: first frame shown at {first:.1} dB");
    xdotool(&["mousemove", "300", "200"])?;
    xdotool(&["key", "j"])?;
    let next = session.frame_shown(&log, &work, "next-page", 2)?;
    println!("ok   next-page: new frame shown at {next:.1} dB");
    let before = fs::read(&log).map_err(|error| error.to_string())?;
    let plain = frames(&before)
        .last()
        .and_then(|frame| decode(frame))
        .map_or(0, |image| crate::e2e::highlighted_pixels(&image));
    xdotool(&["type", "--clearmodifiers", "/page"])?;
    xdotool(&["key", "Return"])?;
    poll("a Sixel frame with search highlights", || {
        let output = fs::read(&log).ok()?;
        let written = frames(&output);
        let frame = decode(written.last()?)?;
        (crate::e2e::highlighted_pixels(&frame) > plain + 100).then_some(())
    })?;
    let count = frames(&fs::read(&log).map_err(|error| error.to_string())?).len();
    let search = session.frame_shown(&log, &work, "search-highlight", count)?;
    println!("ok   search-highlight: highlighted frame shown at {search:.1} dB");
    xdotool(&["key", "Escape"])?;
    poll("a Sixel frame without search highlights", || {
        let output = fs::read(&log).ok()?;
        let written = frames(&output);
        let frame = decode(written.last()?)?;
        (crate::e2e::highlighted_pixels(&frame) <= plain).then_some(())
    })?;
    let count = frames(&fs::read(&log).map_err(|error| error.to_string())?).len();
    session.frame_shown(&log, &work, "search-dismissed", count)?;
    xdotool(&["key", "q"])?;
    poll("termleaf to quit", || {
        session.terminal.try_wait().ok().flatten().map(|_| ())
    })
}

struct Session {
    terminal: Child,
}

impl Session {
    fn frame_shown(&self, log: &Path, work: &Path, label: &str, count: usize) -> Outcome<f64> {
        let mut best = 0.0;
        poll(&format!("xterm to show frame {count} or later"), || {
            let output = fs::read(log).ok()?;
            let written = frames(&output);
            if written.len() < count {
                return None;
            }
            let frame = decode(written.last()?)?;
            let screen = screenshot(&work.join(format!("{label}.png"))).ok()?;
            best = best_psnr(&screen, &frame);
            (best >= MIN_PSNR_DB).then_some(best)
        })
        .map_err(|error| format!("{error}; best match {best:.1} dB, {MIN_PSNR_DB} dB wanted"))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.terminal.kill();
        let _ = self.terminal.wait();
    }
}

fn xdotool(args: &[&str]) -> Outcome<()> {
    let status = Command::new("xdotool")
        .args(args)
        .status()
        .map_err(|error| format!("running xdotool: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("xdotool {} failed", args.join(" ")))
    }
}

fn screenshot(path: &Path) -> Outcome<RgbImage> {
    let status = Command::new("import")
        .args(["-window", "root"])
        .arg(path)
        .status()
        .map_err(|error| format!("running import: {error}"))?;
    if !status.success() {
        return Err("import failed".to_owned());
    }
    image::open(path)
        .map(|image| image.to_rgb8())
        .map_err(|error| error.to_string())
}

fn poll<T>(waiting_for: &str, mut attempt: impl FnMut() -> Option<T>) -> Outcome<T> {
    let deadline = Instant::now() + STEP_TIMEOUT;
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

pub fn frames(output: &[u8]) -> Vec<&[u8]> {
    let marker = [PAGE_ORIGIN, SIXEL_START].concat();
    let mut found = Vec::new();
    let mut rest = output;
    while let Some(start) = find(rest, &marker) {
        let sixel = &rest[start + PAGE_ORIGIN.len()..];
        let Some(end) = find(sixel, STRING_TERMINATOR) else {
            break;
        };
        found.push(&sixel[..end + STRING_TERMINATOR.len()]);
        rest = &sixel[end + STRING_TERMINATOR.len()..];
    }
    found
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

pub fn decode(sixel: &[u8]) -> Option<RgbImage> {
    let image = SixelImage::decode(sixel).ok()?;
    let (width, height) = declared_size(sixel)?;
    let stride = image.width;
    Some(RgbImage::from_fn(width, height, |x, y| {
        let at = (usize::try_from(y).unwrap_or(0) * stride + usize::try_from(x).unwrap_or(0)) * 4;
        Rgb([
            image.pixels.get(at).copied().unwrap_or(0),
            image.pixels.get(at + 1).copied().unwrap_or(0),
            image.pixels.get(at + 2).copied().unwrap_or(0),
        ])
    }))
}

fn declared_size(sixel: &[u8]) -> Option<(u32, u32)> {
    let raster = &sixel[find(sixel, b"\"")? + 1..];
    let end = raster
        .iter()
        .position(|byte| !byte.is_ascii_digit() && *byte != b';')?;
    let numbers: Vec<u32> = std::str::from_utf8(&raster[..end])
        .ok()?
        .split(';')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    match numbers.as_slice() {
        [_, _, width, height] => Some((*width, *height)),
        _ => None,
    }
}

pub fn psnr_at(screen: &RgbImage, frame: &RgbImage, left: u32, top: u32) -> f64 {
    if left + frame.width() > screen.width() || top + frame.height() > screen.height() {
        return 0.0;
    }
    let mut squared = 0.0;
    for (x, y, pixel) in frame.enumerate_pixels() {
        let shown = screen.get_pixel(left + x, top + y);
        for (a, b) in pixel.0.iter().zip(shown.0) {
            squared += f64::from(a.abs_diff(b)).powi(2);
        }
    }
    let samples = f64::from(frame.width()) * f64::from(frame.height()) * 3.0;
    if squared == 0.0 {
        return f64::INFINITY;
    }
    10.0 * (255.0 * 255.0 / (squared / samples)).log10()
}

pub fn best_psnr(screen: &RgbImage, frame: &RgbImage) -> f64 {
    (0..=MAX_OFFSET)
        .flat_map(|top| (0..=MAX_OFFSET).map(move |left| (left, top)))
        .map(|(left, top)| psnr_at(screen, frame, left, top))
        .fold(0.0, f64::max)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHITE: Rgb<u8> = Rgb([255, 255, 255]);
    const DARK: Rgb<u8> = Rgb([20, 20, 20]);

    #[test]
    fn finds_each_complete_frame_written_at_the_page_origin() {
        let output = b"\x1b[30;1Hstatus\x1b[1;1H\x1bPq#0!4~\x1b\\\x1b[?25l\x1b[1;1H\x1bPq#1!4~\x1b\\\x1b[1;1H\x1bPq#2";
        assert_eq!(
            frames(output),
            [b"\x1bPq#0!4~\x1b\\".as_slice(), b"\x1bPq#1!4~\x1b\\"]
        );
    }

    #[test]
    fn output_without_frames_has_none() {
        assert!(frames(b"\x1b[30;1Hpage 1/3").is_empty());
    }

    #[test]
    fn a_frame_decodes_at_its_declared_size() {
        let sixel = b"\x1bP9;1;0q\"1;1;4;2#0;2;100;100;100#1;2;0;0;0#0BB??$#1??BB$-\x1b\\";
        let frame = decode(sixel).unwrap();
        assert_eq!(frame.dimensions(), (4, 2));
        assert_eq!(*frame.get_pixel(0, 0), WHITE);
        assert_eq!(*frame.get_pixel(3, 1), Rgb([0, 0, 0]));
    }

    #[test]
    fn a_frame_shown_one_pixel_in_is_still_found() {
        let frame = RgbImage::from_fn(20, 10, |x, _| if x % 3 == 0 { DARK } else { WHITE });
        let mut screen = RgbImage::from_pixel(40, 30, DARK);
        for (x, y, pixel) in frame.enumerate_pixels() {
            screen.put_pixel(x + 1, y + 2, *pixel);
        }
        assert!(best_psnr(&screen, &frame).is_infinite());
    }

    #[test]
    fn a_screen_without_the_frame_scores_low() {
        let frame = RgbImage::from_pixel(20, 10, WHITE);
        let screen = RgbImage::from_pixel(40, 30, DARK);
        assert!(best_psnr(&screen, &frame) < MIN_PSNR_DB);
    }
}
