use std::fs;
use std::path::{Path, PathBuf};
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
const TERMLEAF_AUTOCMDS: &str = "luaeval('#vim.tbl_filter(function(a) return vim.startswith(a.group_name or \"\", \"termleaf_follow\") end, vim.api.nvim_get_autocmds({}))')";

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
    fs::copy(root.join("contrib/termleaf.vim"), work.join("termleaf.vim"))
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
        &format!("{env} vim -u DEFAULTS -i NONE -S termleaf.vim chapters/method.tex"),
        &work,
    )?;
    let status = server.wait_for_status("vim to open method.tex", |status| {
        status.ends_with(" · follow: chapters/method.tex:1")
    })?;
    println!("ok   vim opened method.tex: {status}");
    lone_focus_in(&server, "vim")?;

    flips_once(
        &server,
        "60G then j j j",
        || server.text(&server.editor, "60Gjjj"),
        "chapters/method.tex:63",
        "page 5/5",
    )?;
    held_j_stays(&server, "chapters/method.tex:71")?;
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
    turned_off(&server, "F to turn follow off", "page 3/5")?;
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
    turned_off(&server, ":nofollow", "page 5/5")?;
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
    turned_off(
        &server,
        "termleaf --no-follow to open the thesis",
        "page 1/5",
    )?;
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

    let viewer = format!("{env} '{}' --graphics kitty doc.pdf", termleaf.display());
    server.respawn(&server.viewer, &viewer, &work)?;
    server.wait_for_status("termleaf to open the thesis for nvim", |status| {
        status == "page 1/5 · doc.pdf"
    })?;
    server.keys(&server.viewer, &["Escape"])?;
    flips_once(
        &server,
        "nvim --clean opening method.tex",
        || {
            server.respawn(
                &server.editor,
                &format!("{env} nvim --clean chapters/method.tex"),
                &work,
            )
        },
        "nvim at chapters/method.tex:1",
        "page 3/5",
    )?;
    lone_focus_in(&server, "nvim")?;
    flips_once(
        &server,
        "nvim 60G then j j j",
        || server.text(&server.editor, "60Gjjj"),
        "nvim at chapters/method.tex:63",
        "page 5/5",
    )?;
    held_j_stays(&server, "nvim at chapters/method.tex:71")?;
    server.keys(&server.viewer, &["F"])?;
    turned_off(&server, "F to turn follow off with nvim", "page 5/5")?;
    stays(&server, "nvim moving while follow is off", || {
        server.text(&server.editor, ":e chapters/intro.tex\r")
    })?;
    flips_once(
        &server,
        "F again with nvim",
        || server.keys(&server.viewer, &["F"]),
        "nvim at chapters/intro.tex:1",
        "page 2/5",
    )?;

    server.respawn(
        &server.viewer,
        &format!(
            "{env} '{}' --graphics kitty --no-follow doc.pdf",
            termleaf.display()
        ),
        &work,
    )?;
    turned_off(
        &server,
        "termleaf --no-follow to open the thesis for nvim",
        "page 1/5",
    )?;
    server.keys(&server.viewer, &["Escape"])?;
    stays(&server, "nvim moving under --no-follow", || {
        server.text(&server.editor, "G")
    })?;
    let nvim = neovim_socket(&sockets)?;
    let messages = expression(&nvim, "execute('messages')")?;
    let hooks = expression(&nvim, TERMLEAF_AUTOCMDS)?;
    if !messages.is_empty() || hooks != "0" {
        return Err(format!(
            "nvim kept {hooks} termleaf autocmds after its viewer quit, with messages {messages:?}"
        ));
    }
    println!(
        "ok   the quit viewer's autocmds removed themselves, :messages is empty, and --no-follow added none"
    );
    flips_once(
        &server,
        "F under --no-follow with nvim",
        || server.keys(&server.viewer, &["F"]),
        "nvim at chapters/intro.tex:55",
        "page 3/5",
    )?;
    let hooks = expression(&nvim, TERMLEAF_AUTOCMDS)?;
    if hooks != "4" {
        return Err(format!(
            "F registered {hooks} autocmds in nvim, not one group of 4"
        ));
    }
    println!("ok   F registered one group of 4 autocmds in nvim");

    server.respawn(&server.editor, "sleep 86400", &work)?;
    server.respawn(&server.viewer, &viewer, &work)?;
    server.wait_for_status("termleaf to open the thesis for hx", |status| {
        status == "page 1/5 · doc.pdf"
    })?;
    server.keys(&server.viewer, &["Escape"])?;
    let hx = format!(
        "{env} XDG_CONFIG_HOME='{}' hx chapters/method.tex",
        work.join("hx-config").display()
    );
    flips_once(
        &server,
        "hx opening method.tex",
        || server.respawn(&server.editor, &hx, &work),
        "hx at chapters/method.tex:1",
        "page 3/5",
    )?;
    lone_focus_in(&server, "hx")?;
    flips_once(
        &server,
        "hx 60G then j j j",
        || server.text(&server.editor, "60Gjjj"),
        "hx at chapters/method.tex:63",
        "page 5/5",
    )?;
    held_j_stays(&server, "hx at chapters/method.tex:71")?;
    stays(&server, "the hx : prompt covering its statusline", || {
        server.text(&server.editor, ":o")
    })?;
    server.keys(&server.editor, &["Escape"])?;
    server.keys(&server.viewer, &["F"])?;
    turned_off(&server, "F to turn follow off with hx", "page 5/5")?;
    stays(&server, "hx moving while follow is off", || {
        server.text(&server.editor, ":o chapters/intro.tex\r")
    })?;
    flips_once(
        &server,
        "F again with hx",
        || server.keys(&server.viewer, &["F"]),
        "hx at chapters/intro.tex:1",
        "page 2/5",
    )?;
    server.respawn(
        &server.viewer,
        &format!(
            "{env} '{}' --graphics kitty --no-follow doc.pdf",
            termleaf.display()
        ),
        &work,
    )?;
    turned_off(
        &server,
        "termleaf --no-follow to open the thesis for hx",
        "page 1/5",
    )?;
    server.keys(&server.viewer, &["Escape"])?;
    stays(&server, "hx moving under --no-follow", || {
        server.text(&server.editor, "ge")
    })?;
    flips_once(
        &server,
        "F under --no-follow with hx",
        || server.keys(&server.viewer, &["F"]),
        "hx at chapters/intro.tex:55",
        "page 3/5",
    )?;

    hangup_viewer(&server, &work, &sockets)?;
    for attempt in 1..=10 {
        server.respawn(&server.viewer, &viewer, &work)?;
        server.wait_for_status("termleaf to open for the hangup check", |status| {
            status.contains(" · doc.pdf")
        })?;
        hangup_viewer(&server, &work, &sockets)?;
        println!("ok   repeated pane hangup {attempt}/10");
    }
    Ok(())
}

fn viewer_sockets(directory: &Path) -> Vec<PathBuf> {
    fs::read_dir(directory.join("termleaf"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "sock")
        })
        .collect()
}

fn hangup_viewer(server: &Server, work: &Path, sockets: &Path) -> Outcome<()> {
    let before = viewer_sockets(sockets);
    if before.is_empty() {
        return Err("the running viewer has no follow socket".to_owned());
    }
    server.respawn(&server.viewer, "sleep 86400", work)?;
    poll("termleaf to remove its socket on SIGHUP", || {
        viewer_sockets(sockets).is_empty().then_some(())
    }).map_err(|error| {
        let left = viewer_sockets(sockets);
        let pids: Vec<String> = left.iter().filter_map(|path| path.file_stem()?.to_str()?.parse::<u32>().ok().map(|pid| pid.to_string())).collect();
        let processes = Command::new("ps").args(["-p", &pids.join(","), "-o", "pid=,ppid=,pgid=,state=,comm="]).output().map(|output| String::from_utf8_lossy(&output.stdout).into_owned()).unwrap_or_default();
        format!("{error}; sockets before: {before:?}; remaining: {left:?}; processes: {processes:?}; pane: {:?}", server.status())
    })?;
    println!("ok   termleaf removed its socket when its pane was killed: {before:?}");
    Ok(())
}

fn lone_focus_in(server: &Server, editor: &str) -> Outcome<()> {
    server.text(&server.viewer, "\x1b[I")?;
    println!("ok   a lone focus-in sent to termleaf before {editor} moves");
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
                return Err(failure(
                    server,
                    &format!("timed out waiting for follow: {settled}; last status: {status:?}"),
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

fn turned_off(server: &Server, label: &str, page: &str) -> Outcome<()> {
    let wanted = format!("{page} · doc.pdf · follow off");
    poll(label, || (server.whole_status() == wanted).then_some(())).map_err(|error| {
        failure(
            server,
            &format!("{error}; last status: {:?}", server.whole_status()),
        )
    })
}

fn failure(server: &Server, message: &str) -> String {
    format!(
        "{message}\nviewer {} exited: {}\nviewer capture: {:?}\neditor {} capture: {:?}",
        server.viewer,
        server.host.exited(&server.viewer),
        server.screen(&server.viewer),
        server.editor,
        server.screen(&server.editor),
    )
}

fn neovim_socket(sockets: &Path) -> Outcome<PathBuf> {
    fs::read_dir(sockets)
        .map_err(|error| format!("reading {}: {error}", sockets.display()))?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("nvim."))
        })
        .ok_or_else(|| format!("no nvim socket in {}", sockets.display()))
}

fn expression(socket: &Path, expression: &str) -> Outcome<String> {
    let output = Command::new("nvim")
        .arg("--server")
        .arg(socket)
        .args(["--remote-expr", expression])
        .output()
        .map_err(|error| format!("running nvim --server: {error}"))?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn held_j_stays(server: &Server, settled: &str) -> Outcome<()> {
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
    pages_until(server, settled, &mut pages)?;
    if pages != [start.as_str()] {
        return Err(format!("holding j moved the page: {pages:?}"));
    }
    println!("ok   holding j on {start} changes nothing for a second");
    Ok(())
}
