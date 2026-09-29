use std::fs;
use std::path::Path;
use std::process::{Command, ExitCode};
use std::thread;
use std::time::{Duration, Instant};

use crate::jumps::{self, Outcome, Server, poll};
use crate::tmux;

const SAMPLE_EVERY: Duration = Duration::from_millis(10);
const SETTLES_WITHIN: Duration = Duration::from_secs(15);
const QUIET_FOR: Duration = Duration::from_secs(1);
const HELD_KEY_REPEATS: Duration = Duration::from_millis(100);
const STALE: &str = " · SyncTeX data is older than the PDF";

pub fn run(root: &Path) -> ExitCode {
    match scenario(root) {
        Ok(()) => {
            println!("follow passed");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("follow failed: {message}");
            ExitCode::FAILURE
        }
    }
}

fn scenario(root: &Path) -> Outcome<()> {
    let termleaf = jumps::build(root)?;
    let work = jumps::prepare(root, "follow")?;
    fs::copy(
        root.join("tests/fixtures/snippet.vim"),
        work.join("snippet.vim"),
    )
    .map_err(|error| format!("copying the snippet: {error}"))?;
    let sockets = work.join("run");
    let server = tmux::start(&work, &termleaf, false)?;
    let env = format!("env XDG_RUNTIME_DIR='{}'", sockets.display());
    server.respawn(
        &server.viewer,
        &format!("{env} '{}' --graphics kitty doc.pdf", termleaf.display()),
        &work,
    )?;
    server.wait_for_status("termleaf to open the thesis", |status| {
        status == "page 1/5 · doc.pdf"
    })?;
    server.keys(&server.viewer, &["Escape"])?;
    server.respawn(
        &server.editor,
        &format!("{env} vim -u DEFAULTS -i NONE -S snippet.vim chapters/method.tex"),
        &work,
    )?;
    let status = server.wait_for_status("vim to open method.tex", |status| {
        status.ends_with(" · follow: chapters/method.tex:1")
    })?;
    println!("ok   vim opened method.tex: {status}");

    flips_once(
        &server,
        "60G then j j j",
        || server.text(&server.editor, "60Gjjj"),
        "chapters/method.tex:63",
        "page 5/5",
    )?;
    held_j_stays(&server)?;
    flips_once(
        &server,
        ":e chapters/intro.tex",
        || server.text(&server.editor, ":e chapters/intro.tex\r"),
        "chapters/intro.tex:1",
        "page 2/5",
    )?;
    flips_once(
        &server,
        "G",
        || server.text(&server.editor, "G"),
        "chapters/intro.tex:55",
        "page 3/5",
    )?;

    server.keys(&server.viewer, &["F"])?;
    server.wait_for_status("F to turn follow off", |status| {
        status == "page 3/5 · doc.pdf · follow off"
    })?;
    stays(&server, "vim moving while follow is off", || {
        server.text(&server.editor, ":e chapters/method.tex\r60G")
    })?;
    flips_once(
        &server,
        "F again",
        || server.keys(&server.viewer, &["F"]),
        "chapters/method.tex:60",
        "page 5/5",
    )?;
    server.text(&server.viewer, ":nofollow\r")?;
    server.wait_for_status(":nofollow", |status| {
        status == "page 5/5 · doc.pdf · follow off"
    })?;
    stays(&server, "vim moving after :nofollow", || {
        server.text(&server.editor, ":e chapters/intro.tex\r")
    })?;
    flips_once(
        &server,
        ":follow",
        || server.text(&server.viewer, ":follow\r"),
        "chapters/intro.tex:55",
        "page 3/5",
    )?;

    let client = |place: &str| -> Outcome<()> {
        let status = Command::new(&termleaf)
            .args(["--follow", place])
            .env("XDG_RUNTIME_DIR", &sockets)
            .current_dir(&work)
            .status()
            .map_err(|error| format!("running termleaf --follow: {error}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("termleaf --follow {place} exited with {status}"))
        }
    };
    flips_once(
        &server,
        "termleaf --follow chapters/method.tex:10",
        || client("chapters/method.tex:10"),
        "chapters/method.tex:10",
        "page 4/5",
    )?;
    let before = server.status();
    client("/elsewhere/chapters/intro.tex:5")?;
    thread::sleep(QUIET_FOR);
    if server.status() != before {
        return Err(format!(
            "a line of another document moved termleaf: {:?}",
            server.status()
        ));
    }
    println!("ok   a line of another document is ignored: {before}");

    rewrite(&work.join("doc.pdf"))?;
    client("chapters/intro.tex:5")?;
    let status = server.wait_for_status("the follow sent during a reload", |status| {
        status.starts_with("page 2/5 · ")
            && status.ends_with(&format!(" · follow: chapters/intro.tex:5{STALE}"))
    })?;
    println!("ok   a follow sent while the PDF is rewritten lands, and warns: {status}");
    rewrite(&work.join("doc.synctex"))?;
    client("chapters/intro.tex:35")?;
    let status = server.wait_for_status("the fresh SyncTeX data", |status| {
        status.starts_with("page 3/5 · ") && status.ends_with(" · follow: chapters/intro.tex:35")
    })?;
    println!("ok   SyncTeX data written after the PDF is read again: {status}");

    server.respawn(
        &server.viewer,
        &format!(
            "{env} '{}' --graphics kitty --no-follow doc.pdf",
            termleaf.display()
        ),
        &work,
    )?;
    server.wait_for_status("termleaf --no-follow to open the thesis", |status| {
        status == "page 1/5 · doc.pdf · follow off"
    })?;
    server.keys(&server.viewer, &["Escape"])?;
    stays(&server, "a follow line under --no-follow", || {
        client("chapters/method.tex:10")
    })?;
    flips_once(
        &server,
        "F under --no-follow",
        || server.keys(&server.viewer, &["F"]),
        "chapters/method.tex:10",
        "page 4/5",
    )?;

    server.respawn(&server.viewer, "sleep 86400", &work)?;
    poll("termleaf to remove its socket on SIGHUP", || {
        let left = fs::read_dir(sockets.join("termleaf"))
            .map(|entries| {
                entries
                    .flatten()
                    .any(|entry| entry.path().extension().is_some_and(|ext| ext == "sock"))
            })
            .unwrap_or(false);
        (!left).then_some(())
    })?;
    println!("ok   termleaf removed its socket when its pane was killed");
    Ok(())
}

fn rewrite(path: &Path) -> Outcome<()> {
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    fs::write(path, bytes).map_err(|error| format!("rewriting {}: {error}", path.display()))
}

fn page(status: &str) -> Option<String> {
    status
        .contains(" · doc.pdf")
        .then(|| status.split(" · ").next().unwrap_or_default().to_owned())
}

fn pages_until(server: &Server, settled: &str, pages: &mut Vec<String>) -> Outcome<()> {
    let deadline = Instant::now() + SETTLES_WITHIN;
    let mut settled_at: Option<Instant> = None;
    loop {
        let status = server.status();
        if let Some(page) = page(&status)
            && pages.last() != Some(&page)
        {
            pages.push(page);
        }
        if settled_at.is_none() && status.ends_with(&format!(" · follow: {settled}")) {
            settled_at = Some(Instant::now());
        }
        match settled_at {
            Some(at) if at.elapsed() >= QUIET_FOR => return Ok(()),
            None if Instant::now() >= deadline => {
                return Err(format!(
                    "timed out waiting for follow: {settled}; last status: {status:?}"
                ));
            }
            _ => thread::sleep(SAMPLE_EVERY),
        }
    }
}

fn flips_once(
    server: &Server,
    label: &str,
    act: impl FnOnce() -> Outcome<()>,
    settled: &str,
    golden: &str,
) -> Outcome<()> {
    let start = page(&server.status()).unwrap_or_default();
    let mut pages = vec![start.clone()];
    act()?;
    pages_until(server, settled, &mut pages)?;
    if pages != [start.as_str(), golden] {
        return Err(format!(
            "{label}: the page changed {pages:?}, not once to {golden}"
        ));
    }
    println!("ok   {label}: {} → {golden}, once", pages[0]);
    Ok(())
}

fn stays(server: &Server, label: &str, act: impl FnOnce() -> Outcome<()>) -> Outcome<()> {
    let before = server.status();
    act()?;
    let started = Instant::now();
    while started.elapsed() < QUIET_FOR {
        let status = server.status();
        if !status.is_empty() && status != before {
            return Err(format!("{label} changed the status to {status:?}"));
        }
        thread::sleep(SAMPLE_EVERY);
    }
    println!("ok   {label} leaves termleaf alone: {before}");
    Ok(())
}

fn held_j_stays(server: &Server) -> Outcome<()> {
    let start = page(&server.status()).unwrap_or_default();
    let mut pages = vec![start.clone()];
    for _ in 0..8 {
        server.text(&server.editor, "j")?;
        let pressed = Instant::now();
        while pressed.elapsed() < HELD_KEY_REPEATS {
            if let Some(page) = page(&server.status())
                && pages.last() != Some(&page)
            {
                pages.push(page);
            }
            thread::sleep(SAMPLE_EVERY);
        }
    }
    pages_until(server, "chapters/method.tex:71", &mut pages)?;
    if pages != [start.as_str()] {
        return Err(format!("holding j moved the page: {pages:?}"));
    }
    println!("ok   holding j on {start} changes nothing for a second");
    Ok(())
}
