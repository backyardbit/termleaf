use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use signal_hook::consts::{SIGHUP, SIGTERM};
use signal_hook::iterator::Signals;
use signal_hook::low_level::emulate_default_handler;

use crate::inverse::{Inverse, STALE};
use crate::synctex::SourceLocation;
use crate::viewer::Viewer;

const MESSAGE_LIMIT: u64 = 4096;
const PATIENCE: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub file: PathBuf,
    pub line: u32,
}

pub fn parse(message: &str) -> Option<Request> {
    let message = message.strip_suffix('\n').unwrap_or(message);
    let (line, rest) = message.strip_prefix("follow ")?.split_once(' ')?;
    let (column, file) = rest.split_once(' ')?;
    column.parse::<u32>().ok()?;
    let line = line.parse::<u32>().ok().filter(|line| *line > 0)?;
    let file = PathBuf::from(file);
    file.is_absolute().then_some(Request { file, line })
}

pub fn directory(variable: impl Fn(&str) -> Option<String>) -> PathBuf {
    let set = |name: &str| variable(name).filter(|value| !value.is_empty());
    match set("XDG_RUNTIME_DIR") {
        Some(runtime) => PathBuf::from(runtime).join("termleaf"),
        None => PathBuf::from(set("TMPDIR").unwrap_or_else(|| "/tmp".to_owned()))
            .join(format!("termleaf-{}", set("USER").unwrap_or_default())),
    }
}

pub struct Listener {
    socket: PathBuf,
}

impl Listener {
    pub fn spawn(
        directory: &Path,
        name: &str,
        on_request: impl Fn(Request) + Send + 'static,
    ) -> Result<Self> {
        fs::create_dir_all(directory)?;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
        remove_stale(directory);
        let socket = directory.join(format!("{name}.sock"));
        let _ = fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket)?;
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                if let Some(request) = receive(stream) {
                    on_request(request);
                }
            }
        });
        remove_on_signals(socket.clone());
        Ok(Self { socket })
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.socket);
    }
}

fn receive(stream: UnixStream) -> Option<Request> {
    stream.set_read_timeout(Some(PATIENCE)).ok()?;
    let mut message = String::new();
    BufReader::new(stream.take(MESSAGE_LIMIT))
        .read_line(&mut message)
        .ok()?;
    parse(&message)
}

fn remove_stale(directory: &Path) {
    for socket in fs::read_dir(directory)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
    {
        let refused = socket
            .extension()
            .is_some_and(|extension| extension == "sock")
            && UnixStream::connect(&socket)
                .is_err_and(|error| error.kind() == std::io::ErrorKind::ConnectionRefused);
        if refused {
            let _ = fs::remove_file(&socket);
        }
    }
}

fn remove_on_signals(socket: PathBuf) {
    let Ok(mut signals) = Signals::new([SIGTERM, SIGHUP]) else {
        return;
    };
    thread::spawn(move || {
        if let Some(signal) = signals.forever().next() {
            let _ = fs::remove_file(&socket);
            let _ = emulate_default_handler(signal);
        }
    });
}

#[derive(Debug, Default)]
pub struct Follow {
    focused: bool,
    reloading: bool,
    held: Option<Request>,
    last_page: Option<usize>,
    segment: Option<String>,
}

impl Follow {
    pub fn focus(&mut self, focused: bool) {
        self.focused = focused;
    }

    pub fn reloading(&mut self) {
        self.reloading = true;
    }

    pub fn reloaded(&mut self, viewer: &mut Viewer, inverse: &mut Inverse) {
        self.reloading = false;
        if let Some(request) = self.held.take() {
            self.request(request, viewer, inverse);
        }
    }

    pub fn beside(&self, notice: Option<&str>) -> Option<String> {
        let parts: Vec<&str> = notice.into_iter().chain(self.segment.as_deref()).collect();
        (!parts.is_empty()).then(|| parts.join(" · "))
    }

    pub fn request(&mut self, request: Request, viewer: &mut Viewer, inverse: &mut Inverse) {
        if self.reloading {
            self.held = Some(request);
            return;
        }
        if self.focused {
            return;
        }
        let (input, target, stale) = match inverse.synctex() {
            Ok((synctex, stale)) => {
                let wanted = canonical(&request.file);
                let Some(input) = synctex
                    .inputs()
                    .find(|input| canonical(input) == wanted)
                    .map(Path::to_path_buf)
                else {
                    return;
                };
                let target = synctex.position_of(&input, request.line);
                (input, target, stale)
            }
            Err(problem) => {
                if inverse.near(&request.file) {
                    self.segment = Some(format!("follow: {problem}"));
                }
                return;
            }
        };
        let place = inverse.describe(&SourceLocation {
            file: input,
            line: request.line,
        });
        let mut segment = format!("follow: {place}");
        match target.filter(|target| target.page < viewer.page_count()) {
            Some(target) => {
                viewer.show(target, self.last_page != Some(target.page));
                self.last_page = Some(target.page);
            }
            None => segment.push_str(" not in the PDF"),
        }
        if stale {
            segment.push_str(" · ");
            segment.push_str(STALE);
        }
        self.segment = Some(segment);
    }
}

fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.components().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inverse::StatusOnly;
    use crate::keys::Command;
    use crate::layout::{CellSize, Pane};
    use crate::pdf::{PageInfo, PageSize};
    use std::fs::File;
    use std::io::Write;
    use std::sync::mpsc;
    use std::time::SystemTime;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/synctex")
            .join(name)
    }

    fn scratch(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let directory = std::env::temp_dir().join(format!("termleaf-follow-{label}-{nanos}"));
        fs::create_dir_all(directory.join("chapters")).unwrap();
        directory
    }

    fn thesis(label: &str) -> PathBuf {
        let directory = scratch(label);
        fs::copy(fixture("thesis.pdf"), directory.join("doc.pdf")).unwrap();
        let mut synctex = String::new();
        flate2::read::GzDecoder::new(File::open(fixture("thesis.synctex.gz")).unwrap())
            .read_to_string(&mut synctex)
            .unwrap();
        let synctex = synctex.replace("/tmp/thesis", directory.to_str().unwrap());
        fs::write(directory.join("doc.synctex"), synctex).unwrap();
        for file in ["chapters/intro.tex", "chapters/method.tex"] {
            fs::copy(fixture(file), directory.join(file)).unwrap();
        }
        directory
    }

    fn viewer(pages: usize) -> Viewer {
        let page = PageInfo {
            size: PageSize {
                width: 612.0,
                height: 792.0,
            },
            links: Vec::new(),
        };
        Viewer::new(
            vec![page; pages],
            CellSize {
                width: 10,
                height: 20,
            },
            Pane {
                columns: 80,
                rows: 24,
            },
        )
    }

    struct Scene {
        directory: PathBuf,
        viewer: Viewer,
        inverse: Inverse,
        follow: Follow,
    }

    impl Scene {
        fn new(label: &str, pages: usize) -> Self {
            let directory = thesis(label);
            Self {
                inverse: Inverse::new(&directory.join("doc.pdf"), Box::new(StatusOnly)),
                viewer: viewer(pages),
                follow: Follow::default(),
                directory,
            }
        }

        fn request(&mut self, file: &str, line: u32) {
            let file = if file.starts_with('/') {
                PathBuf::from(file)
            } else {
                self.directory.join(file)
            };
            self.follow
                .request(Request { file, line }, &mut self.viewer, &mut self.inverse);
        }

        fn reloaded(&mut self) {
            self.follow.reloaded(&mut self.viewer, &mut self.inverse);
        }

        fn segment(&self) -> Option<String> {
            self.follow.beside(None)
        }
    }

    impl Drop for Scene {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    #[test]
    fn a_follow_message_is_a_line_a_column_and_an_absolute_file() {
        assert_eq!(
            parse("follow 12 3 /home/me/my thesis/intro.tex\n"),
            Some(Request {
                file: PathBuf::from("/home/me/my thesis/intro.tex"),
                line: 12,
            })
        );
        for message in [
            "follow 0 1 /intro.tex",
            "follow 12 1 intro.tex",
            "follow 12 x /intro.tex",
            "follow x 1 /intro.tex",
            "follow 12 /intro.tex",
            "jump 12 1 /intro.tex",
        ] {
            assert_eq!(parse(message), None, "{message}");
        }
    }

    #[test]
    fn sockets_live_in_a_directory_of_their_own_per_user() {
        let from = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| (*value).to_owned())
            }
        };
        assert_eq!(
            directory(from(&[("XDG_RUNTIME_DIR", "/run/user/1000")])),
            Path::new("/run/user/1000/termleaf")
        );
        assert_eq!(
            directory(from(&[("XDG_RUNTIME_DIR", ""), ("USER", "ben")])),
            Path::new("/tmp/termleaf-ben")
        );
        assert_eq!(
            directory(from(&[("TMPDIR", "/var/t"), ("USER", "ben")])),
            Path::new("/var/t/termleaf-ben")
        );
    }

    #[test]
    fn a_listening_viewer_takes_follow_lines_and_stale_sockets_are_cleared() {
        let root = scratch("sockets");
        let sockets = root.join("run");
        fs::create_dir_all(&sockets).unwrap();
        drop(UnixListener::bind(sockets.join("1.sock")).unwrap());
        fs::write(sockets.join("notes"), "").unwrap();
        let (sent, received) = mpsc::channel();
        let listener = Listener::spawn(&sockets, "2", move |request| {
            sent.send(request).unwrap();
        })
        .unwrap();
        assert!(!sockets.join("1.sock").exists());
        assert!(sockets.join("notes").exists());
        assert_eq!(
            fs::metadata(&sockets).unwrap().permissions().mode() & 0o777,
            0o700
        );
        for message in [
            &b"\xff\n"[..],
            b"hello\n",
            b"follow 12 3 /thesis/intro.tex\n",
        ] {
            UnixStream::connect(sockets.join("2.sock"))
                .unwrap()
                .write_all(message)
                .unwrap();
        }
        assert_eq!(
            received.recv_timeout(Duration::from_secs(5)),
            Ok(Request {
                file: PathBuf::from("/thesis/intro.tex"),
                line: 12,
            })
        );
        drop(listener);
        assert!(!sockets.join("2.sock").exists());
        assert!(Listener::spawn(&sockets.join("notes/run"), "4", |_| {}).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn each_new_page_moves_the_view_once_and_lines_on_it_stay_put() {
        let mut scene = Scene::new("pages", 5);
        assert_eq!(scene.segment(), None);
        scene.request("chapters/intro.tex", 35);
        assert_eq!(scene.viewer.page(), 2);
        assert_eq!(
            scene.segment().as_deref(),
            Some("follow: chapters/intro.tex:35")
        );
        let top = scene.viewer.view().top;
        scene.request("chapters/intro.tex", 36);
        assert_eq!(scene.viewer.view().top, top);
        scene.viewer.apply(Command::First);
        scene.request("chapters/intro.tex", 36);
        assert_eq!(scene.viewer.page(), 2);
        assert_eq!(
            scene.follow.beside(Some("intro.tex:7")).as_deref(),
            Some("intro.tex:7 · follow: chapters/intro.tex:36")
        );
    }

    #[test]
    fn requests_wait_for_a_reload_and_are_dropped_while_termleaf_has_focus() {
        let mut scene = Scene::new("gates", 5);
        scene.reloaded();
        scene.follow.reloading();
        scene.request("chapters/method.tex", 10);
        assert_eq!(scene.viewer.page(), 0);
        scene.reloaded();
        assert_eq!(scene.viewer.page(), 3);
        scene.follow.focus(true);
        scene.request("chapters/intro.tex", 5);
        assert_eq!(scene.viewer.page(), 3);
        scene.follow.focus(false);
        scene.request("chapters/intro.tex", 5);
        assert_eq!(scene.viewer.page(), 1);
    }

    #[test]
    fn other_documents_are_ignored_and_what_cannot_be_shown_is_reported() {
        let mut scene = Scene::new("reports", 2);
        scene.request("/elsewhere/chapters/intro.tex", 5);
        assert_eq!(scene.segment(), None);
        scene.request("thesis.aux", 1);
        assert_eq!(
            scene.segment().as_deref(),
            Some("follow: thesis.aux:1 not in the PDF")
        );
        scene.request("chapters/method.tex", 10);
        assert_eq!(scene.viewer.page(), 0);
        assert_eq!(
            scene.segment().as_deref(),
            Some("follow: chapters/method.tex:10 not in the PDF")
        );
        File::options()
            .write(true)
            .open(scene.directory.join("doc.synctex"))
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(3600))
            .unwrap();
        scene.request("chapters/intro.tex", 5);
        assert_eq!(
            scene.segment().as_deref(),
            Some("follow: chapters/intro.tex:5 · SyncTeX data is older than the PDF")
        );
        fs::remove_file(scene.directory.join("doc.synctex")).unwrap();
        scene.request("/elsewhere/intro.tex", 5);
        assert_eq!(
            scene.segment().as_deref(),
            Some("follow: chapters/intro.tex:5 · SyncTeX data is older than the PDF")
        );
        scene.request("chapters/intro.tex", 5);
        assert_eq!(
            scene.segment().as_deref(),
            Some("follow: no SyncTeX data: build with -synctex=1")
        );
    }
}
