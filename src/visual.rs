use crate::layout::Position;
use crate::renderer::{Generation, Renderer};
use crate::search::Hit;
use crate::selection::{Caret, DocumentText, Granularity, Motion, PageText};
use crate::viewer::Viewer;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisualCommand {
    Start(Granularity),
    Move(Motion, usize),
    SwapEnds,
    Copy,
    Cancel,
}

pub trait TextSource {
    fn read(&self, document: Generation, pages: Vec<usize>);
}

impl TextSource for Renderer {
    fn read(&self, document: Generation, pages: Vec<usize>) {
        Renderer::read(self, document, pages);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Ends {
    anchor: Caret,
    caret: Caret,
}

#[derive(Debug)]
struct Selecting {
    granularity: Granularity,
    ends: Option<Ends>,
    start: Position,
}

#[derive(Debug, Default)]
pub struct Visual {
    document: Option<Generation>,
    text: DocumentText,
    selecting: Option<Selecting>,
    revision: u64,
    notice: Option<String>,
}

impl Visual {
    pub fn active(&self) -> bool {
        self.selecting.is_some()
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn acknowledge(&mut self) {
        self.notice = None;
    }

    pub fn command(
        &mut self,
        command: VisualCommand,
        viewer: &mut Viewer,
        source: &impl TextSource,
        document: Generation,
    ) -> Option<String> {
        match command {
            VisualCommand::Start(granularity) => {
                self.start(granularity, viewer, source, document);
            }
            VisualCommand::Move(motion, count) => self.travel(motion, count, viewer),
            VisualCommand::SwapEnds => {
                if let Some(ends) = self.ends_mut() {
                    *ends = Ends {
                        anchor: ends.caret,
                        caret: ends.anchor,
                    };
                    self.reveal(viewer);
                }
            }
            VisualCommand::Copy => return self.copy(),
            VisualCommand::Cancel => self.stop(),
        }
        None
    }

    fn start(
        &mut self,
        granularity: Granularity,
        viewer: &mut Viewer,
        source: &impl TextSource,
        document: Generation,
    ) {
        if let Some(selecting) = &mut self.selecting {
            if selecting.granularity == granularity {
                self.stop();
            } else {
                selecting.granularity = granularity;
                self.revision += 1;
            }
            return;
        }
        let view = viewer.view();
        let start = view.layout.position_at(view.point_at(0.0, 0.0));
        if self.document != Some(document) {
            self.document = Some(document);
            self.text = DocumentText::new(viewer.page_count());
        }
        let mut missing: Vec<usize> = self.text.missing().collect();
        missing.sort_by_key(|page| page.abs_diff(start.page));
        if !missing.is_empty() {
            source.read(document, missing);
        }
        self.selecting = Some(Selecting {
            granularity,
            ends: None,
            start,
        });
        self.place(viewer);
    }

    fn place(&mut self, viewer: &mut Viewer) {
        let Some(selecting) = &mut self.selecting else {
            return;
        };
        if selecting.ends.is_some() {
            return;
        }
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a position on a page is a few thousand points at most"
        )]
        let top = selecting.start.y as f32;
        match self.text.first_below(selecting.start.page, top) {
            Some(caret) => {
                selecting.ends = Some(Ends {
                    anchor: caret,
                    caret,
                });
                self.revision += 1;
                self.reveal(viewer);
            }
            None if self.text.missing().next().is_none() => {
                self.selecting = None;
                self.notice = Some("no text to select".to_owned());
            }
            None => {}
        }
    }

    fn ends_mut(&mut self) -> Option<&mut Ends> {
        self.selecting.as_mut()?.ends.as_mut()
    }

    fn travel(&mut self, motion: Motion, count: usize, viewer: &mut Viewer) {
        let Some(Ends { anchor, caret }) = self.selecting.as_ref().and_then(|s| s.ends) else {
            return;
        };
        let mut moved = caret;
        for _ in 0..count {
            let next = self.text.moved(moved, motion);
            if next == moved {
                break;
            }
            moved = next;
        }
        if moved == caret {
            return;
        }
        if let Some(ends) = self.ends_mut() {
            *ends = Ends {
                anchor,
                caret: moved,
            };
        }
        self.revision += 1;
        self.reveal(viewer);
    }

    fn reveal(&self, viewer: &mut Viewer) {
        let Some(area) = self
            .selecting
            .as_ref()
            .and_then(|selecting| selecting.ends)
            .and_then(|ends| self.text.area(ends.caret).map(|area| (ends.caret, area)))
        else {
            return;
        };
        let (caret, area) = area;
        viewer.show(
            Position {
                page: caret.page,
                x: f64::from(area.x0),
                y: f64::from(area.y0),
            },
            false,
        );
    }

    fn copy(&mut self) -> Option<String> {
        let selecting = self.selecting.as_ref()?;
        let Some(ends) = selecting.ends else {
            self.stop();
            return None;
        };
        let text = self.text.text(
            self.text
                .span(ends.anchor, ends.caret, selecting.granularity),
        );
        self.stop();
        self.notice = Some(match text.chars().count() {
            1 => "copied 1 character".to_owned(),
            count => format!("copied {count} characters"),
        });
        Some(text)
    }

    fn stop(&mut self) {
        if let Some(selecting) = self.selecting.take()
            && selecting.ends.is_some()
        {
            self.revision += 1;
        }
    }

    pub fn receive(
        &mut self,
        document: Generation,
        page: usize,
        text: Result<PageText, String>,
        viewer: &mut Viewer,
    ) {
        if self.document != Some(document) {
            return;
        }
        self.text.insert(page, text.unwrap_or_default());
        self.place(viewer);
    }

    pub fn reloaded(&mut self) {
        self.stop();
        self.document = None;
        self.text = DocumentText::default();
    }

    pub fn areas(&self) -> Vec<Hit> {
        self.selecting
            .as_ref()
            .and_then(|selecting| {
                let ends = selecting.ends?;
                Some(
                    self.text.areas(
                        self.text
                            .span(ends.anchor, ends.caret, selecting.granularity),
                    ),
                )
            })
            .unwrap_or_default()
    }

    pub fn status(&self) -> Option<String> {
        let Some(selecting) = &self.selecting else {
            return self.notice.clone();
        };
        let mode = match selecting.granularity {
            Granularity::Character => "-- VISUAL --",
            Granularity::Line => "-- VISUAL LINE --",
        };
        Some(match selecting.ends {
            Some(_) => mode.to_owned(),
            None => format!("{mode} reading text…"),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::layout::{CellSize, Pane};
    use crate::pdf::{PageInfo, PageSize, PointRect};
    use crate::selection::{Glyph, TextLine};

    #[derive(Default)]
    struct Requests(RefCell<Vec<(Generation, Vec<usize>)>>);

    impl TextSource for Requests {
        fn read(&self, document: Generation, pages: Vec<usize>) {
            self.0.borrow_mut().push((document, pages));
        }
    }

    const PAGES: usize = 4;

    fn viewer() -> Viewer {
        Viewer::new(
            vec![
                PageInfo {
                    size: PageSize {
                        width: 600.0,
                        height: 800.0
                    },
                    links: Vec::new()
                };
                PAGES
            ],
            CellSize {
                width: 10,
                height: 20,
            },
            Pane {
                columns: 60,
                rows: 20,
            },
        )
    }

    fn row(top: f32, text: &str) -> TextLine {
        TextLine {
            block: 0,
            glyphs: text
                .chars()
                .zip(0u16..)
                .map(|(character, column)| Glyph {
                    character,
                    area: PointRect {
                        x0: 100.0 + f32::from(column) * 6.0,
                        y0: top,
                        x1: 106.0 + f32::from(column) * 6.0,
                        y1: top + 10.0,
                    },
                })
                .collect(),
        }
    }

    fn page_text() -> PageText {
        PageText {
            lines: vec![row(100.0, "first line"), row(120.0, "second line")],
        }
    }

    fn deliver_all(visual: &mut Visual, viewer: &mut Viewer) {
        for page in 0..PAGES {
            visual.receive(1, page, Ok(page_text()), viewer);
        }
    }

    fn started(granularity: Granularity) -> (Visual, Viewer) {
        let mut visual = Visual::default();
        let mut viewer = viewer();
        visual.command(
            VisualCommand::Start(granularity),
            &mut viewer,
            &Requests::default(),
            1,
        );
        deliver_all(&mut visual, &mut viewer);
        (visual, viewer)
    }

    fn run(visual: &mut Visual, viewer: &mut Viewer, commands: &[VisualCommand]) -> Option<String> {
        commands
            .iter()
            .filter_map(|command| visual.command(*command, viewer, &Requests::default(), 1))
            .last()
    }

    #[test]
    fn starting_asks_for_the_text_of_the_nearest_pages_first_and_waits_for_it() {
        let mut visual = Visual::default();
        let mut viewer = viewer();
        viewer.apply(crate::keys::Command::GoTo(3));
        let requests = Requests::default();
        visual.command(
            VisualCommand::Start(Granularity::Character),
            &mut viewer,
            &requests,
            1,
        );
        assert_eq!(*requests.0.borrow(), [(1, vec![2, 1, 3, 0])]);
        assert!(visual.active());
        assert_eq!(
            visual.status().as_deref(),
            Some("-- VISUAL -- reading text…")
        );
        assert!(visual.areas().is_empty());
        visual.receive(1, 2, Ok(page_text()), &mut viewer);
        assert_eq!(visual.status().as_deref(), Some("-- VISUAL --"));
        assert_eq!(visual.areas().len(), 1);
        assert_eq!(visual.areas()[0].page, 2);
    }

    #[test]
    fn text_from_another_document_is_ignored() {
        let mut visual = Visual::default();
        let mut viewer = viewer();
        visual.command(
            VisualCommand::Start(Granularity::Character),
            &mut viewer,
            &Requests::default(),
            1,
        );
        visual.receive(0, 0, Ok(page_text()), &mut viewer);
        assert!(visual.areas().is_empty());
    }

    #[test]
    fn y_copies_what_the_motions_selected_and_leaves_visual_mode() {
        let (mut visual, mut viewer) = started(Granularity::Character);
        let copied = run(
            &mut visual,
            &mut viewer,
            &[
                VisualCommand::Move(Motion::WordForward, 1),
                VisualCommand::Move(Motion::Down, 1),
                VisualCommand::Move(Motion::WordEnd, 1),
                VisualCommand::Copy,
            ],
        );
        assert_eq!(copied.as_deref(), Some("first line second line"));
        assert!(!visual.active());
        assert!(visual.areas().is_empty());
        assert_eq!(visual.status().as_deref(), Some("copied 22 characters"));
        visual.acknowledge();
        assert_eq!(visual.status(), None);
    }

    #[test]
    fn a_count_repeats_a_motion() {
        let (mut visual, mut viewer) = started(Granularity::Character);
        let copied = run(
            &mut visual,
            &mut viewer,
            &[VisualCommand::Move(Motion::Right, 4), VisualCommand::Copy],
        );
        assert_eq!(copied.as_deref(), Some("first"));
    }

    #[test]
    fn line_mode_copies_whole_lines() {
        let (mut visual, mut viewer) = started(Granularity::Line);
        assert_eq!(visual.status().as_deref(), Some("-- VISUAL LINE --"));
        let copied = run(&mut visual, &mut viewer, &[VisualCommand::Copy]);
        assert_eq!(copied.as_deref(), Some("first line"));
    }

    #[test]
    fn o_swaps_the_moving_end() {
        let (mut visual, mut viewer) = started(Granularity::Character);
        let copied = run(
            &mut visual,
            &mut viewer,
            &[
                VisualCommand::Move(Motion::LineEnd, 1),
                VisualCommand::SwapEnds,
                VisualCommand::Move(Motion::WordForward, 1),
                VisualCommand::Copy,
            ],
        );
        assert_eq!(copied.as_deref(), Some("line"));
    }

    #[test]
    fn pressing_the_same_mode_again_or_escape_cancels_and_the_other_mode_switches() {
        let (mut visual, mut viewer) = started(Granularity::Character);
        let revision = visual.revision();
        run(
            &mut visual,
            &mut viewer,
            &[VisualCommand::Start(Granularity::Line)],
        );
        assert_eq!(visual.status().as_deref(), Some("-- VISUAL LINE --"));
        assert!(visual.revision() > revision);
        run(
            &mut visual,
            &mut viewer,
            &[VisualCommand::Start(Granularity::Line)],
        );
        assert!(!visual.active());
        run(
            &mut visual,
            &mut viewer,
            &[
                VisualCommand::Start(Granularity::Character),
                VisualCommand::Cancel,
            ],
        );
        assert!(!visual.active());
        assert!(visual.areas().is_empty());
    }

    #[test]
    fn the_view_follows_the_caret_across_pages() {
        let (mut visual, mut viewer) = started(Granularity::Character);
        run(
            &mut visual,
            &mut viewer,
            &[VisualCommand::Move(Motion::Down, 4)],
        );
        assert_eq!(viewer.page(), 2);
    }

    #[test]
    fn a_document_without_text_ends_visual_mode_with_a_notice() {
        let mut visual = Visual::default();
        let mut viewer = viewer();
        visual.command(
            VisualCommand::Start(Granularity::Character),
            &mut viewer,
            &Requests::default(),
            1,
        );
        for page in 0..PAGES {
            visual.receive(1, page, Err("no text".to_owned()), &mut viewer);
        }
        assert!(!visual.active());
        assert_eq!(visual.status().as_deref(), Some("no text to select"));
    }

    #[test]
    fn a_reload_ends_the_selection_and_reads_the_new_text_next_time() {
        let (mut visual, mut viewer) = started(Granularity::Character);
        let revision = visual.revision();
        visual.reloaded();
        assert!(!visual.active());
        assert!(visual.revision() > revision);
        let requests = Requests::default();
        visual.command(
            VisualCommand::Start(Granularity::Character),
            &mut viewer,
            &requests,
            2,
        );
        assert_eq!(requests.0.borrow().len(), 1);
        assert_eq!(requests.0.borrow()[0].0, 2);
    }
}
