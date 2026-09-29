use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use crate::{herdr, screen, tmux, zellij};
use std::thread;
use std::time::{Duration, Instant};

pub const POLL_INTERVAL: Duration = Duration::from_millis(100);
const STEP_TIMEOUT: Duration = Duration::from_secs(15);
const NOTICE_CLEARS: Duration = Duration::from_millis(4500);
const KEY_TAKES: Duration = Duration::from_secs(3);
const CLICKS: [(u16, u16); 4] = [(60, 34), (40, 42), (50, 26), (30, 10)];
pub const PROMPT: &str = "shell$ ";

pub type Outcome<T> = Result<T, String>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Tmux,
    Herdr,
    Plain,
    Zellij,
    Screen,
}

impl Backend {
    fn name(self) -> &'static str {
        match self {
            Self::Tmux => "tmux",
            Self::Herdr => "herdr",
            Self::Plain => "plain",
            Self::Zellij => "zellij",
            Self::Screen => "screen",
        }
    }
}

pub fn run(root: &Path, backend: Backend) -> ExitCode {
    match scenario(root, backend) {
        Ok(()) => {
            println!("{} passed", backend.name());
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("{} failed: {message}", backend.name());
            ExitCode::FAILURE
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Editor {
    Neovim,
    Vim,
    Helix,
}

impl Editor {
    fn name(self) -> &'static str {
        match self {
            Self::Neovim => "nvim",
            Self::Vim => "vim",
            Self::Helix => "hx",
        }
    }

    fn command(self, work: &Path) -> String {
        match self {
            Self::Neovim => format!(
                "nvim --clean --listen '{}' chapters/intro.tex",
                work.join("nvim.sock").display()
            ),
            Self::Vim => "vim -u DEFAULTS -i NONE chapters/intro.tex".to_owned(),
            Self::Helix => format!(
                "env XDG_CONFIG_HOME='{}' hx chapters/intro.tex",
                work.join("hx-config").display()
            ),
        }
    }

    fn quit(self) -> &'static [u8] {
        match self {
            Self::Neovim | Self::Vim => b"\x1c\x0e:qa!\r",
            Self::Helix => b"\x1b",
        }
    }

    fn to_last_line(self) -> &'static [&'static str] {
        match self {
            Self::Neovim | Self::Vim => &["Escape", "G"],
            Self::Helix => &["Escape", "g", "e"],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct State {
    file: PathBuf,
    line: u32,
    other_modified: bool,
    typed_kept: bool,
    normal_mode: bool,
}

fn scenario(root: &Path, backend: Backend) -> Outcome<()> {
    let built = Command::new(env!("CARGO"))
        .args(["build", "--release", "--package", "termleaf"])
        .current_dir(root)
        .status()
        .map_err(|error| format!("running cargo build: {error}"))?;
    if !built.success() {
        return Err("building termleaf failed".to_owned());
    }
    let work = prepare(root, backend)?;
    let termleaf = root.join("target/release/termleaf");
    let server = match backend {
        Backend::Tmux => tmux::start(&work, &termleaf, false)?,
        Backend::Herdr => herdr::start(&work, &termleaf)?,
        Backend::Plain => tmux::start(&work, &termleaf, true)?,
        Backend::Zellij => zellij::start(&work, &termleaf)?,
        Backend::Screen => screen::start(&work, &termleaf)?,
    };
    server.wait_for_status("termleaf to open the thesis", |status| {
        status == "page 1/5 · doc.pdf"
    })?;
    poll("page two", || {
        if server.status() == "page 1/5 · doc.pdf" {
            server.keys(&server.viewer, &["j"]).ok()?;
            let pressed = Instant::now();
            while server.status() == "page 1/5 · doc.pdf" && pressed.elapsed() < KEY_TAKES {
                thread::sleep(POLL_INTERVAL);
            }
        }
        (server.status() == "page 2/5 · doc.pdf").then_some(())
    })?;
    let shell_screen = server.screen(&server.shell);
    let history = work.join("shell-history");
    let shell_history = fs::read_to_string(&history).unwrap_or_default();

    let mut run = Run {
        server: &server,
        work: work.clone(),
        clicks: 0,
        last_status: String::new(),
    };
    if backend == Backend::Plain {
        run.editor(Editor::Neovim)?;
        run.prompt_blocks(Editor::Neovim, "refused: waiting at a prompt")?;
    } else {
        for editor in [Editor::Neovim, Editor::Vim, Editor::Helix] {
            run.editor(editor)?;
        }
        run.prompt_blocks(Editor::Vim, "refused: Press ENTER prompt")?;
    }
    run.shells_only()?;

    if server.screen(&server.shell) != shell_screen {
        return Err(format!(
            "the shell pane changed:\n{}",
            server.screen(&server.shell)
        ));
    }
    if fs::read_to_string(&history).unwrap_or_default() != shell_history {
        return Err("the shell ran a command".to_owned());
    }
    println!("ok   the shell pane received nothing");
    server.host.focus_kept()?;
    Ok(())
}

fn prepare(root: &Path, backend: Backend) -> Outcome<PathBuf> {
    let fixtures = root.join("tests/fixtures/synctex");
    let work = root.join(format!("target/e2e-{}", backend.name()));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(work.join("chapters")).map_err(|error| error.to_string())?;
    fs::create_dir_all(work.join("hx-config/helix")).map_err(|error| error.to_string())?;
    for file in ["thesis.tex", "chapters/intro.tex", "chapters/method.tex"] {
        fs::copy(fixtures.join(file), work.join(file))
            .map_err(|error| format!("copying {file}: {error}"))?;
    }
    fs::copy(fixtures.join("thesis.pdf"), work.join("doc.pdf"))
        .map_err(|error| format!("copying the PDF: {error}"))?;
    let unpacked = Command::new("gzip")
        .arg("-dc")
        .arg(fixtures.join("thesis.synctex.gz"))
        .output()
        .map_err(|error| format!("running gzip: {error}"))?;
    let synctex = String::from_utf8_lossy(&unpacked.stdout)
        .replace("/tmp/thesis/", &format!("{}/", work.display()));
    fs::write(work.join("doc.synctex"), synctex).map_err(|error| error.to_string())?;
    Ok(work)
}

pub trait Host {
    fn label(&self, editor: &str, pane: &str) -> String;
    fn screen(&self, pane: &str) -> String;
    fn bytes(&self, pane: &str, bytes: &[u8]) -> Outcome<()>;
    fn respawn(&self, pane: &str, command: &str, work: &Path) -> Outcome<()>;
    fn exited(&self, pane: &str) -> bool;
    fn copy_mode(&self, pane: &str) -> Outcome<bool>;

    fn focus_kept(&self) -> Outcome<()> {
        Ok(())
    }
}

pub struct Server {
    pub host: Box<dyn Host>,
    pub viewer: String,
    pub editor: String,
    pub shell: String,
}

impl Server {
    pub fn screen(&self, pane: &str) -> String {
        self.host.screen(pane)
    }

    fn keys(&self, pane: &str, keys: &[&str]) -> Outcome<()> {
        let bytes: Vec<u8> = keys
            .iter()
            .flat_map(|key| match *key {
                "Escape" => b"\x1b".to_vec(),
                "Enter" => b"\r".to_vec(),
                key => key.as_bytes().to_vec(),
            })
            .collect();
        self.bytes(pane, &bytes)
    }

    fn text(&self, pane: &str, text: &str) -> Outcome<()> {
        self.bytes(pane, text.as_bytes())
    }

    fn bytes(&self, pane: &str, bytes: &[u8]) -> Outcome<()> {
        self.host.bytes(pane, bytes)
    }

    fn status(&self) -> String {
        status_line(&self.screen(&self.viewer))
            .unwrap_or_default()
            .to_owned()
    }

    fn wait_for_status(&self, wanted: &str, matches: impl Fn(&str) -> bool) -> Outcome<String> {
        poll(wanted, || {
            let status = self.status();
            matches(&status).then_some(status)
        })
        .map_err(|error| format!("{error}; last status: {:?}", self.status()))
    }

    fn respawn(&self, pane: &str, command: &str, work: &Path) -> Outcome<()> {
        self.host.respawn(pane, command, work)
    }

    fn quit(&self, editor: Editor, work: &Path) -> Outcome<()> {
        self.bytes(&self.editor, editor.quit())?;
        if editor == Editor::Helix {
            thread::sleep(Duration::from_millis(100));
            self.text(&self.editor, ":quit-all!\r")?;
        }
        poll(&format!("{} to quit", editor.name()), || {
            self.host.exited(&self.editor).then_some(())
        })?;
        for entry in fs::read_dir(work.join("chapters")).map_err(|error| error.to_string())? {
            let path = entry.map_err(|error| error.to_string())?.path();
            if path
                .extension()
                .is_some_and(|extension| extension.to_string_lossy().starts_with("sw"))
            {
                let _ = fs::remove_file(path);
            }
        }
        Ok(())
    }
}

struct Run<'a> {
    server: &'a Server,
    work: PathBuf,
    clicks: usize,
    last_status: String,
}

impl Run<'_> {
    fn intro(&self) -> PathBuf {
        self.work.join("chapters/intro.tex")
    }

    fn click(&mut self, button: u8) -> Outcome<()> {
        let (column, row) = CLICKS[self.clicks % CLICKS.len()];
        self.clicks += 1;
        let press = format!("\x1b[<{button};{column};{row}M\x1b[<{button};{column};{row}m");
        self.server.bytes(&self.server.viewer, press.as_bytes())
    }

    fn alt_click(&mut self) -> Outcome<()> {
        self.click(8)
    }

    fn ctrl_click(&mut self) -> Outcome<()> {
        self.click(16)
    }

    fn jumped(&mut self, editor: Editor, trigger: &str) -> Outcome<u32> {
        let wanted = format!(
            "→ {} · chapters/intro.tex:",
            self.server.host.label(editor.name(), &self.server.editor)
        );
        let previous = self.last_status.clone();
        let status = self
            .server
            .wait_for_status(&format!("{trigger} to jump {}", editor.name()), |status| {
                status != previous && status.contains(&wanted)
            })?;
        self.last_status.clone_from(&status);
        status
            .rsplit_once(':')
            .and_then(|(_, line)| line.parse().ok())
            .ok_or_else(|| format!("no line in {status:?}"))
    }

    fn state(&self, editor: Editor) -> Outcome<State> {
        match editor {
            Editor::Neovim => self.neovim_state(),
            Editor::Vim => self.vim_state(),
            Editor::Helix => self.helix_state(),
        }
    }

    fn neovim_state(&self) -> Outcome<State> {
        let expression = "join([expand('%:p'), line('.'), getbufvar(bufnr('method.tex'), '&modified'), search('typed in insert', 'nw') > 0, mode() ==# 'n'], ':')";
        let output = Command::new("nvim")
            .arg("--server")
            .arg(self.work.join("nvim.sock"))
            .args(["--remote-expr", expression])
            .output()
            .map_err(|error| format!("running nvim --server: {error}"))?;
        parse_state(&String::from_utf8_lossy(&output.stdout))
    }

    fn vim_state(&self) -> Outcome<State> {
        let marker = self.work.join("vim-state");
        let _ = fs::remove_file(&marker);
        let command = format!(
            ":call writefile([join([expand('%:p'), line('.'), getbufvar(bufnr('method.tex'), '&modified'), search('typed in insert', 'nw') > 0, mode() ==# 'n'], ':')], '{}')\r",
            marker.display()
        );
        self.server.text(&self.server.editor, &command)?;
        let text = poll("vim to write its state", || {
            fs::read_to_string(&marker).ok()
        })?;
        parse_state(&text)
    }

    fn helix_state(&self) -> Outcome<State> {
        let marker = self.work.join("hx-state");
        let _ = fs::remove_file(&marker);
        let command = format!(
            ":sh echo %{{buffer_name}}:%{{cursor_line}} > '{}'\r",
            marker.display()
        );
        self.server.text(&self.server.editor, &command)?;
        let text = poll("hx to write its state", || {
            fs::read_to_string(&marker)
                .ok()
                .filter(|text| text.ends_with('\n'))
        })?;
        self.server.keys(&self.server.editor, &["Escape"])?;
        let (file, line) = text
            .trim()
            .rsplit_once(':')
            .ok_or_else(|| format!("hx wrote {text:?}"))?;
        Ok(State {
            file: self.work.join(file),
            line: line.parse().map_err(|_| format!("hx wrote {text:?}"))?,
            other_modified: false,
            typed_kept: false,
            normal_mode: true,
        })
    }

    fn check(
        &self,
        editor: Editor,
        label: &str,
        line: u32,
        expect: impl Fn(&State) -> bool,
    ) -> Outcome<()> {
        let intro = self.intro();
        let state = poll_state(
            || self.state(editor),
            |state| state.file == intro && state.line == line,
        )?;
        if !expect(&state) {
            return Err(format!(
                "{} {label}: unexpected state {state:?}",
                editor.name()
            ));
        }
        println!(
            "ok   {} {label}: {} (cursor on line {})",
            editor.name(),
            self.last_status,
            state.line
        );
        Ok(())
    }

    fn settle(&self, editor: Editor) {
        thread::sleep(Duration::from_millis(if editor == Editor::Helix {
            400
        } else {
            200
        }));
    }

    fn editor(&mut self, editor: Editor) -> Outcome<()> {
        let pane = self.server.editor.clone();
        self.server
            .respawn(&pane, &editor.command(&self.work), &self.work)?;
        poll(&format!("{} to start", editor.name()), || {
            self.server
                .screen(&pane)
                .contains("\\chapter")
                .then_some(())
        })?;
        self.settle(editor);
        let method = fs::read_to_string(self.work.join("chapters/method.tex"))
            .map_err(|error| error.to_string())?;

        self.server.keys(&pane, editor.to_last_line())?;
        self.settle(editor);
        self.alt_click()?;
        let line = self.jumped(editor, "Alt+click")?;
        self.check(editor, "from normal mode", line, |_| true)?;

        match editor {
            Editor::Helix => self.server.text(&pane, ":open chapters/method.tex\r")?,
            Editor::Neovim | Editor::Vim => self.server.text(&pane, ":e chapters/method.tex\r")?,
        }
        self.settle(editor);
        self.server.keys(&pane, editor.to_last_line())?;
        self.server.keys(&pane, &["o"])?;
        self.server.text(&pane, "unsaved change")?;
        self.server.keys(&pane, &["Escape"])?;
        self.settle(editor);
        self.alt_click()?;
        let line = self.jumped(editor, "Alt+click")?;
        self.check(
            editor,
            "from another file with unsaved changes",
            line,
            |state| editor == Editor::Helix || state.other_modified,
        )?;
        let on_disk = fs::read_to_string(self.work.join("chapters/method.tex"))
            .map_err(|error| error.to_string())?;
        if on_disk != method {
            return Err(format!(
                "{} wrote the unsaved change to disk",
                editor.name()
            ));
        }

        self.server.keys(&pane, editor.to_last_line())?;
        self.server.keys(&pane, &["o"])?;
        self.server.text(&pane, "typed in insert")?;
        self.settle(editor);
        self.alt_click()?;
        let line = self.jumped(editor, "Alt+click")?;
        self.check(editor, "from insert mode", line, |state| match editor {
            Editor::Neovim => state.typed_kept && state.normal_mode,
            Editor::Vim => state.typed_kept,
            Editor::Helix => true,
        })?;

        self.server.keys(&pane, editor.to_last_line())?;
        match editor {
            Editor::Helix => self.server.text(&pane, ":write-quit")?,
            Editor::Neovim | Editor::Vim => self.server.text(&pane, ":s/typed in insert/lost/")?,
        }
        self.settle(editor);
        self.alt_click()?;
        let line = self.jumped(editor, "Alt+click")?;
        self.check(editor, "from a half-typed command", line, |state| {
            editor == Editor::Helix || state.typed_kept
        })?;

        match editor {
            Editor::Neovim => {
                self.server.keys(&pane, editor.to_last_line())?;
                self.server.keys(&pane, &["d"])?;
                self.settle(editor);
                self.alt_click()?;
                let line = self.jumped(editor, "Alt+click")?;
                self.check(editor, "from a pending d", line, |state| {
                    state.typed_kept && state.normal_mode
                })?;

                self.server.keys(&pane, editor.to_last_line())?;
                self.settle(editor);
                if self.server.host.copy_mode(&pane)? {
                    self.alt_click()?;
                    let line = self.jumped(editor, "Alt+click")?;
                    self.check(editor, "from a pane in copy mode", line, |_| true)?;
                }
            }
            Editor::Vim => {
                self.server.keys(&pane, editor.to_last_line())?;
                self.settle(editor);
                thread::sleep(NOTICE_CLEARS);
                self.last_status.clear();
                self.server.keys(&self.server.viewer, &["e"])?;
                let line = self.jumped(editor, "e at the pointer")?;
                self.check(editor, "from e at the pointer", line, |_| true)?;
            }
            Editor::Helix => {
                self.server.keys(&pane, editor.to_last_line())?;
                self.settle(editor);
                self.ctrl_click()?;
                let line = self.jumped(editor, "Ctrl+click")?;
                self.check(editor, "from Ctrl+click", line, |_| true)?;
            }
        }
        self.server.quit(editor, &self.work)
    }

    fn prompt_blocks(&mut self, editor: Editor, refusal: &str) -> Outcome<()> {
        let pane = self.server.editor.clone();
        self.server
            .respawn(&pane, &editor.command(&self.work), &self.work)?;
        poll(&format!("{} to start", editor.name()), || {
            self.server
                .screen(&pane)
                .contains("\\chapter")
                .then_some(())
        })?;
        self.server.text(&pane, ":echo \"one\\ntwo\"\r")?;
        poll("the Press ENTER prompt", || {
            self.server
                .screen(&pane)
                .contains("Press ENTER or type command to continue")
                .then_some(())
        })?;
        let before = self.server.screen(&pane);
        self.alt_click()?;
        let wanted = format!(
            "· {} {refusal}",
            self.server.host.label(editor.name(), &pane)
        );
        let status = self
            .server
            .wait_for_status("the Press ENTER refusal", |status| {
                status.ends_with(&wanted)
            })?;
        self.last_status.clone_from(&status);
        thread::sleep(Duration::from_millis(500));
        if self.server.screen(&pane) != before {
            return Err(format!(
                "{} at a Press ENTER prompt received keys",
                editor.name()
            ));
        }
        println!(
            "ok   {} at a Press ENTER prompt gets nothing: {status}",
            editor.name()
        );
        self.server.keys(&pane, &["Enter"])?;
        thread::sleep(Duration::from_millis(200));
        self.server.quit(editor, &self.work)
    }

    fn shells_only(&mut self) -> Outcome<()> {
        let pane = self.server.editor.clone();
        self.server
            .respawn(&pane, "bash --norc --noprofile", &self.work)?;
        poll("the second shell", || {
            self.server.screen(&pane).contains('$').then_some(())
        })?;
        thread::sleep(Duration::from_millis(300));
        let before = self.server.screen(&pane);
        self.alt_click()?;
        let status = self.server.wait_for_status("no editor found", |status| {
            status.ends_with(" · no editor found") && status.contains("chapters/intro.tex:")
        })?;
        thread::sleep(Duration::from_millis(500));
        if self.server.screen(&pane) != before {
            return Err("a shell pane received keys".to_owned());
        }
        println!("ok   only shells: {status}");
        Ok(())
    }
}

fn parse_state(text: &str) -> Outcome<State> {
    let fields: Vec<&str> = text.trim().rsplitn(5, ':').collect();
    let [normal, typed, modified, line, file] = fields.as_slice() else {
        return Err(format!("the editor wrote {text:?}"));
    };
    Ok(State {
        file: PathBuf::from(file),
        line: line
            .parse()
            .map_err(|_| format!("the editor wrote {text:?}"))?,
        other_modified: *modified == "1",
        typed_kept: *typed == "1",
        normal_mode: *normal == "1",
    })
}

fn poll_state(read: impl Fn() -> Outcome<State>, done: impl Fn(&State) -> bool) -> Outcome<State> {
    let deadline = Instant::now() + STEP_TIMEOUT;
    let mut last = Err("no state read".to_owned());
    while Instant::now() < deadline {
        last = read();
        if let Ok(state) = &last
            && done(state)
        {
            return last;
        }
        thread::sleep(POLL_INTERVAL * 3);
    }
    Err(format!("the editor did not reach the line: {last:?}"))
}

pub fn poll<T>(waiting_for: &str, mut attempt: impl FnMut() -> Option<T>) -> Outcome<T> {
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

pub fn status_line(screen: &str) -> Option<&str> {
    screen
        .lines()
        .map(str::trim)
        .rfind(|line| line.contains(" · doc.pdf"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_editor_state_is_read_from_the_right() {
        assert_eq!(
            parse_state("/work/chapters/intro.tex:12:1:0:1\n"),
            Ok(State {
                file: PathBuf::from("/work/chapters/intro.tex"),
                line: 12,
                other_modified: true,
                typed_kept: false,
                normal_mode: true,
            })
        );
        assert!(parse_state("E492: Not an editor command").is_err());
    }

    #[test]
    fn the_status_line_is_the_one_naming_the_document() {
        let screen =
            "\\chapter{Intro}\npage 2/5 · doc.pdf · chapters/intro.tex:12 · no editor found\n\n";
        assert_eq!(
            status_line(screen),
            Some("page 2/5 · doc.pdf · chapters/intro.tex:12 · no editor found")
        );
        assert_eq!(status_line("no status here"), None);
    }
}
