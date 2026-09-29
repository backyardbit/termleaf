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

#[derive(Debug, PartialEq)]
struct Stamp {
    synctex: Option<(PathBuf, Option<SystemTime>)>,
    pdf: Option<SystemTime>,
}

impl Stamp {
    fn of(pdf: &Path) -> Self {
        Self {
            synctex: Synctex::beside(pdf).map(|path| {
                let modified = modified(&path);
                (path, modified)
            }),
            pdf: modified(pdf),
        }
    }
}

pub struct Inverse {
    pdf: PathBuf,
    directories: Vec<PathBuf>,
    data: Option<(Stamp, Data)>,
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
        let data = current(&mut self.data, &self.pdf);
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

fn current<'a>(slot: &'a mut Option<(Stamp, Data)>, pdf: &Path) -> &'a Data {
    let now = Stamp::of(pdf);
    if slot.as_ref().is_some_and(|(seen, _)| *seen != now) {
        *slot = None;
    }
    let (_, data) = slot.get_or_insert_with(|| (now, load(pdf)));
    data
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

fn modified(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).ok()?.modified().ok()
}

fn older(synctex: &Path, pdf: &Path) -> bool {
    matches!((modified(synctex), modified(pdf)), (Some(data), Some(document)) if data < document)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

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

    #[test]
    fn synctex_data_written_after_the_pdf_is_read_again() {
        let directory = scratch("rewritten");
        let pdf = directory.join("doc.pdf");
        fs::copy(fixture("synctex/thesis.pdf"), &pdf).unwrap();
        let synctex = directory.join("doc.synctex.gz");
        fs::write(&synctex, b"not synctex").unwrap();
        let mut inverse = Inverse::new(&pdf, Box::new(StatusOnly));
        assert_eq!(
            answer(&mut inverse, Some(INTRO)),
            "SyncTeX data is unreadable"
        );
        fs::copy(fixture("synctex/thesis.synctex.gz"), &synctex).unwrap();
        File::options()
            .write(true)
            .open(&synctex)
            .unwrap()
            .set_modified(SystemTime::now() + Duration::from_secs(60))
            .unwrap();
        assert_eq!(answer(&mut inverse, Some(INTRO)), "intro.tex:7");
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
