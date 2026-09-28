use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::layout::Position;
use crate::synctex::{SourceLocation, Synctex};

const NOTICE_FOR: Duration = Duration::from_secs(4);
const MISSING: &str = "no SyncTeX data: build with -synctex=1";
const UNREADABLE: &str = "SyncTeX data is unreadable";
const STALE: &str = "SyncTeX data is older than the PDF";
const NOTHING_HERE: &str = "no source here";

#[derive(Debug)]
enum Data {
    Missing,
    Unreadable,
    Parsed { synctex: Synctex, stale: bool },
}

pub trait Editors {
    fn jump(&mut self, at: &SourceLocation, inputs: &[PathBuf], place: &str) -> String;
}

#[cfg(test)]
pub struct StatusOnly;

#[cfg(test)]
impl Editors for StatusOnly {
    fn jump(&mut self, _at: &SourceLocation, _inputs: &[PathBuf], place: &str) -> String {
        place.to_owned()
    }
}

pub struct Inverse {
    pdf: PathBuf,
    directories: Vec<PathBuf>,
    data: Option<Data>,
    notice: Option<(String, Instant)>,
    editors: Box<dyn Editors>,
}

impl Inverse {
    pub fn new(pdf: &Path, editors: Box<dyn Editors>) -> Self {
        let mut directories: Vec<PathBuf> =
            [std::path::absolute(pdf).ok(), fs::canonicalize(pdf).ok()]
                .into_iter()
                .flatten()
                .filter_map(|path| path.parent().map(Path::to_path_buf))
                .collect();
        directories.dedup();
        Self {
            pdf: pdf.to_path_buf(),
            directories,
            data: None,
            notice: None,
            editors,
        }
    }

    pub fn reloaded(&mut self) {
        self.data = None;
    }

    pub fn search(&mut self, at: Option<Position>, now: Instant) {
        let answer = self.answer(at);
        self.notice = Some((answer, now + NOTICE_FOR));
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_ref().map(|(text, _)| text.as_str())
    }

    pub fn notice_ends(&self) -> Option<Instant> {
        self.notice.as_ref().map(|(_, until)| *until)
    }

    pub fn expire(&mut self, now: Instant) {
        if self.notice_ends().is_some_and(|until| now >= until) {
            self.notice = None;
        }
    }

    fn answer(&mut self, at: Option<Position>) -> String {
        let data = self.data.get_or_insert_with(|| load(&self.pdf));
        let Data::Parsed { synctex, stale } = data else {
            return match data {
                Data::Missing => MISSING.to_owned(),
                _ => UNREADABLE.to_owned(),
            };
        };
        let stale = *stale;
        let found = at.and_then(|position| synctex.source_at(position));
        let inputs: Vec<PathBuf> = synctex
            .inputs()
            .filter(|input| {
                self.directories
                    .iter()
                    .any(|directory| input.starts_with(directory))
            })
            .map(Path::to_path_buf)
            .collect();
        let mut parts = vec![match found {
            Some(location) => {
                let place = self.describe(&location);
                self.editors.jump(&location, &inputs, &place)
            }
            None => NOTHING_HERE.to_owned(),
        }];
        if stale {
            parts.push(STALE.to_owned());
        }
        parts.join(" · ")
    }

    fn describe(&self, location: &SourceLocation) -> String {
        let shown = self
            .directories
            .iter()
            .find_map(|directory| location.file.strip_prefix(directory).ok())
            .or_else(|| location.file.file_name().map(Path::new))
            .unwrap_or(&location.file);
        format!("{}:{}", shown.display(), location.line)
    }
}

fn load(pdf: &Path) -> Data {
    let Some(path) = Synctex::beside(pdf) else {
        return Data::Missing;
    };
    match Synctex::open(&path) {
        Ok(synctex) => Data::Parsed {
            stale: older(&path, pdf),
            synctex,
        },
        Err(_) => Data::Unreadable,
    }
}

fn older(synctex: &Path, pdf: &Path) -> bool {
    let modified = |path: &Path| -> Option<SystemTime> { fs::metadata(path).ok()?.modified().ok() };
    matches!((modified(synctex), modified(pdf)), (Some(data), Some(document)) if data < document)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Read;

    use flate2::read::GzDecoder;

    const INTRO: Position = Position {
        page: 1,
        x: 300.0,
        y: 352.0,
    };

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    fn scratch(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let directory = std::env::temp_dir().join(format!("termleaf-inverse-{label}-{nanos}"));
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    fn thesis_in(directory: &Path) -> PathBuf {
        let pdf = directory.join("doc.pdf");
        fs::copy(fixture("synctex/thesis.pdf"), &pdf).unwrap();
        fs::copy(
            fixture("synctex/thesis.synctex.gz"),
            directory.join("doc.synctex.gz"),
        )
        .unwrap();
        pdf
    }

    fn answer(inverse: &mut Inverse, at: Option<Position>) -> String {
        inverse.search(at, Instant::now());
        inverse.notice().unwrap_or_default().to_owned()
    }

    #[test]
    fn synctex_data_older_than_the_pdf_still_answers_with_a_warning() {
        let directory = scratch("stale");
        let pdf = thesis_in(&directory);
        let an_hour_ago = SystemTime::now() - Duration::from_secs(3600);
        File::options()
            .write(true)
            .open(directory.join("doc.synctex.gz"))
            .unwrap()
            .set_modified(an_hour_ago)
            .unwrap();
        let mut inverse = Inverse::new(&pdf, Box::new(StatusOnly));
        assert_eq!(
            answer(&mut inverse, Some(INTRO)),
            "intro.tex:7 · SyncTeX data is older than the PDF"
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn broken_synctex_data_is_reported() {
        let directory = scratch("broken");
        let pdf = directory.join("doc.pdf");
        fs::copy(fixture("synctex/thesis.pdf"), &pdf).unwrap();
        fs::write(directory.join("doc.synctex.gz"), b"not synctex").unwrap();
        let mut inverse = Inverse::new(&pdf, Box::new(StatusOnly));
        assert_eq!(
            answer(&mut inverse, Some(INTRO)),
            "SyncTeX data is unreadable"
        );
        fs::remove_dir_all(directory).unwrap();
    }

    type Asked = std::rc::Rc<std::cell::RefCell<Vec<(SourceLocation, Vec<PathBuf>, String)>>>;

    struct Recording(Asked);

    impl Editors for Recording {
        fn jump(&mut self, at: &SourceLocation, inputs: &[PathBuf], place: &str) -> String {
            self.0
                .borrow_mut()
                .push((at.clone(), inputs.to_vec(), place.to_owned()));
            format!("→ nvim in tmux %1 · {place}")
        }
    }

    #[test]
    fn the_source_line_goes_to_the_editors_with_the_inputs_beside_the_pdf() {
        let directory = scratch("editors");
        let pdf = directory.join("doc.pdf");
        fs::copy(fixture("synctex/thesis.pdf"), &pdf).unwrap();
        let mut text = String::new();
        GzDecoder::new(File::open(fixture("synctex/thesis.synctex.gz")).unwrap())
            .read_to_string(&mut text)
            .unwrap();
        let moved = text.replace("/tmp/thesis/", &format!("{}/", directory.display()));
        fs::write(directory.join("doc.synctex"), moved).unwrap();
        let asked = Asked::default();
        let mut inverse = Inverse::new(&pdf, Box::new(Recording(asked.clone())));
        assert_eq!(
            answer(&mut inverse, Some(INTRO)),
            "→ nvim in tmux %1 · chapters/intro.tex:7"
        );
        let asked = asked.borrow();
        let (at, inputs, place) = &asked[0];
        assert_eq!(at.file, directory.join("chapters/intro.tex"));
        assert_eq!(at.line, 7);
        assert_eq!(place, "chapters/intro.tex:7");
        assert!(inputs.contains(&directory.join("thesis.tex")));
        assert!(inputs.contains(&directory.join("chapters/intro.tex")));
        assert!(inputs.contains(&directory.join("chapters/method.tex")));
        assert!(inputs.iter().all(|input| input.starts_with(&directory)));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_point_without_a_source_asks_no_editor() {
        let directory = scratch("no-editors");
        let asked = Asked::default();
        let mut inverse = Inverse::new(&thesis_in(&directory), Box::new(Recording(asked.clone())));
        assert_eq!(answer(&mut inverse, None), "no source here");
        assert!(asked.borrow().is_empty());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn the_answer_stays_in_the_status_bar_for_four_seconds() {
        let directory = scratch("notice");
        let mut inverse = Inverse::new(&thesis_in(&directory), Box::new(StatusOnly));
        let asked = Instant::now();
        inverse.search(Some(INTRO), asked);
        assert_eq!(inverse.notice_ends(), Some(asked + NOTICE_FOR));
        inverse.expire(asked + Duration::from_millis(3900));
        assert_eq!(inverse.notice(), Some("intro.tex:7"));
        inverse.expire(asked + NOTICE_FOR);
        assert_eq!(inverse.notice(), None);
        fs::remove_dir_all(directory).unwrap();
    }
}
