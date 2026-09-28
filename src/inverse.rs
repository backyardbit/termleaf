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

#[derive(Debug)]
pub struct Inverse {
    pdf: PathBuf,
    directories: Vec<PathBuf>,
    data: Option<Data>,
    notice: Option<(String, Instant)>,
}

impl Inverse {
    pub fn new(pdf: &Path) -> Self {
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
        let mut parts = vec![found.map_or_else(
            || NOTHING_HERE.to_owned(),
            |location| self.describe(&location),
        )];
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
    fn a_point_on_the_page_names_its_source_file_and_line() {
        let directory = scratch("point");
        let mut inverse = Inverse::new(&thesis_in(&directory));
        assert_eq!(answer(&mut inverse, Some(INTRO)), "intro.tex:7");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn sources_beside_the_pdf_are_named_relative_to_it() {
        let directory = scratch("relative");
        let pdf = directory.join("doc.pdf");
        fs::copy(fixture("synctex/thesis.pdf"), &pdf).unwrap();
        let mut text = String::new();
        GzDecoder::new(File::open(fixture("synctex/thesis.synctex.gz")).unwrap())
            .read_to_string(&mut text)
            .unwrap();
        let moved = text.replace("/tmp/thesis/", &format!("{}/", directory.display()));
        fs::write(directory.join("doc.synctex"), moved).unwrap();
        let mut inverse = Inverse::new(&pdf);
        assert_eq!(answer(&mut inverse, Some(INTRO)), "chapters/intro.tex:7");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_pdf_without_synctex_data_says_how_to_build_it() {
        let directory = scratch("missing");
        let pdf = directory.join("doc.pdf");
        fs::copy(fixture("three-pages.pdf"), &pdf).unwrap();
        let mut inverse = Inverse::new(&pdf);
        assert_eq!(
            answer(&mut inverse, Some(INTRO)),
            "no SyncTeX data: build with -synctex=1"
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_point_beside_the_pages_has_no_source() {
        let directory = scratch("beside");
        let mut inverse = Inverse::new(&thesis_in(&directory));
        assert_eq!(answer(&mut inverse, None), "no source here");
        fs::remove_dir_all(directory).unwrap();
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
        let mut inverse = Inverse::new(&pdf);
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
        let mut inverse = Inverse::new(&pdf);
        assert_eq!(
            answer(&mut inverse, Some(INTRO)),
            "SyncTeX data is unreadable"
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn the_synctex_data_is_read_once_and_again_after_the_pdf_reloads() {
        let directory = scratch("cached");
        let mut inverse = Inverse::new(&thesis_in(&directory));
        assert_eq!(answer(&mut inverse, Some(INTRO)), "intro.tex:7");
        fs::remove_file(directory.join("doc.synctex.gz")).unwrap();
        assert_eq!(answer(&mut inverse, Some(INTRO)), "intro.tex:7");
        inverse.reloaded();
        assert_eq!(
            answer(&mut inverse, Some(INTRO)),
            "no SyncTeX data: build with -synctex=1"
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn the_answer_stays_in_the_status_bar_for_four_seconds() {
        let directory = scratch("notice");
        let mut inverse = Inverse::new(&thesis_in(&directory));
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
