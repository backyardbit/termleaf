use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Clear, Widget};

use crate::keys::{Command, KeyParser, OutlineCommand};
use crate::pdf::Heading;
use crate::viewer::Viewer;

const INDENT: usize = 2;
const NO_OUTLINE: &str = "This PDF has no table of contents";

#[derive(Debug, Default)]
pub struct OutlinePanel {
    headings: Vec<Heading>,
    selected: usize,
    shown: bool,
}

impl OutlinePanel {
    pub fn new(headings: Vec<Heading>) -> Self {
        Self {
            headings,
            ..Self::default()
        }
    }

    pub fn shown(&self) -> bool {
        self.shown
    }

    pub fn reloaded(&mut self, headings: Vec<Heading>) {
        self.headings = headings;
        self.selected = self.selected.min(self.last());
    }

    pub fn status(&self, file_name: &str) -> String {
        if self.headings.is_empty() {
            return format!("contents · {file_name}");
        }
        format!(
            "contents {}/{} · {file_name}",
            self.selected + 1,
            self.headings.len()
        )
    }

    pub fn command(&mut self, command: Command, keys: &mut KeyParser, viewer: &mut Viewer) -> bool {
        if let Command::Outline(command) = command {
            self.outline_command(command, viewer);
            return true;
        }
        if !self.shown || command == Command::Quit {
            return false;
        }
        match command {
            Command::Scroll { rows, .. } => self.step(i64::from(rows)),
            Command::Click(at) => {
                let rows = usize::try_from(viewer.view().pane.rows).unwrap_or(usize::MAX);
                let index = self.first_shown(rows) + usize::from(at.row);
                if index < self.headings.len() {
                    self.selected = index;
                    keys.leave_outline();
                    self.outline_command(OutlineCommand::Follow, viewer);
                }
            }
            _ => {}
        }
        true
    }

    fn outline_command(&mut self, command: OutlineCommand, viewer: &mut Viewer) {
        match command {
            OutlineCommand::Show => {
                self.shown = true;
                self.selected = self.heading_at_view(viewer);
            }
            OutlineCommand::Hide => self.shown = false,
            OutlineCommand::Down(count) => self.step(i64::try_from(count).unwrap_or(i64::MAX)),
            OutlineCommand::Up(count) => {
                self.step(i64::try_from(count).unwrap_or(i64::MAX).saturating_neg());
            }
            OutlineCommand::First => self.selected = 0,
            OutlineCommand::Last => self.selected = self.last(),
            OutlineCommand::Follow => {
                self.shown = false;
                if let Some(target) = self.headings.get(self.selected).and_then(|h| h.target) {
                    viewer.follow(target);
                }
            }
        }
    }

    fn heading_at_view(&self, viewer: &Viewer) -> usize {
        let Some(reading) = viewer.position_under(None) else {
            return 0;
        };
        self.headings
            .iter()
            .rposition(|heading| {
                heading.target.is_some_and(|target| {
                    target.page < reading.page
                        || (target.page == reading.page
                            && f64::from(target.top.unwrap_or(0.0)) <= reading.y)
                })
            })
            .unwrap_or(0)
    }

    fn step(&mut self, rows: i64) {
        let selected = i64::try_from(self.selected).unwrap_or(i64::MAX);
        let wanted = selected.saturating_add(rows).max(0);
        self.selected = usize::try_from(wanted)
            .unwrap_or(usize::MAX)
            .min(self.last());
    }

    fn last(&self) -> usize {
        self.headings.len().saturating_sub(1)
    }

    fn first_shown(&self, rows: usize) -> usize {
        self.selected
            .saturating_sub(rows / 2)
            .min(self.headings.len().saturating_sub(rows))
    }
}

impl Widget for &OutlinePanel {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        Clear.render(area, buffer);
        if self.headings.is_empty() {
            buffer.set_stringn(
                area.x,
                area.y,
                NO_OUTLINE,
                usize::from(area.width),
                Style::default(),
            );
            return;
        }
        let rows = usize::from(area.height);
        let first = self.first_shown(rows);
        let width = usize::from(area.width);
        for (row, (index, heading)) in self
            .headings
            .iter()
            .enumerate()
            .skip(first)
            .take(rows)
            .enumerate()
        {
            let y = area.y + u16::try_from(row).unwrap_or(u16::MAX);
            let style = if index == self.selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            let page = heading
                .target
                .map(|target| (target.page + 1).to_string())
                .unwrap_or_default();
            let page_width = page.chars().count();
            let title_width = width.saturating_sub(page_width + 1);
            let indent = " ".repeat(heading.depth.saturating_mul(INDENT));
            buffer.set_style(Rect::new(area.x, y, area.width, 1), style);
            buffer.set_stringn(
                area.x,
                y,
                fitted(&format!("{indent}{}", heading.title), title_width),
                title_width,
                style,
            );
            let page_x = area.x + u16::try_from(width.saturating_sub(page_width)).unwrap_or(0);
            buffer.set_stringn(page_x, y, &page, page_width, style);
        }
    }
}

fn fitted(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(width.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{Key, ScreenCell};
    use crate::layout::{CellSize, Pane};
    use crate::pdf::{LinkTarget, PageInfo, PageSize};

    const CELL: CellSize = CellSize {
        width: 10,
        height: 20,
    };
    const PANE: Pane = Pane {
        columns: 30,
        rows: 4,
    };

    fn heading(title: &str, depth: usize, page: usize, top: f32) -> Heading {
        Heading {
            title: title.to_owned(),
            depth,
            target: Some(LinkTarget {
                page,
                top: Some(top),
            }),
        }
    }

    fn thesis() -> Vec<Heading> {
        vec![
            heading("Introduction", 0, 0, 100.0),
            heading("Background", 1, 0, 400.0),
            heading("Method", 0, 1, 100.0),
            heading("Setup", 1, 1, 300.0),
            heading("Measurements", 1, 1, 600.0),
            heading("Results", 0, 2, 100.0),
            heading("Discussion", 0, 3, 100.0),
        ]
    }

    fn viewer() -> Viewer {
        let page = PageInfo {
            size: PageSize {
                width: 612.0,
                height: 792.0,
            },
            links: Vec::new(),
        };
        Viewer::new(vec![page; 4], CELL, PANE)
    }

    struct Session {
        panel: OutlinePanel,
        keys: KeyParser,
        viewer: Viewer,
    }

    impl Session {
        fn new(headings: Vec<Heading>) -> Self {
            Self {
                panel: OutlinePanel::new(headings),
                keys: KeyParser::default(),
                viewer: viewer(),
            }
        }

        fn type_keys(&mut self, typed: &str) {
            for character in typed.chars() {
                let key = match character {
                    '\n' => Key::Enter,
                    '\u{1b}' => Key::Escape,
                    other => Key::Char(other),
                };
                self.press(key);
            }
        }

        fn press(&mut self, key: Key) {
            if let Some(command) = self.keys.feed(key) {
                self.apply(command);
            }
        }

        fn apply(&mut self, command: Command) -> bool {
            self.panel
                .command(command, &mut self.keys, &mut self.viewer)
        }

        fn selected_title(&self) -> &str {
            &self.panel.headings[self.panel.selected].title
        }

        fn drawn(&self, columns: u16, rows: u16) -> Vec<String> {
            let area = Rect::new(0, 0, columns, rows);
            let mut buffer = Buffer::empty(area);
            (&self.panel).render(area, &mut buffer);
            (0..rows)
                .map(|y| {
                    (0..columns)
                        .map(|x| buffer[(x, y)].symbol().to_owned())
                        .collect::<String>()
                })
                .collect()
        }
    }

    #[test]
    fn showing_the_outline_selects_the_heading_being_read() {
        let mut session = Session::new(thesis());
        session.viewer.apply(Command::GoTo(2));
        session.type_keys("t");
        assert!(session.panel.shown());
        assert_eq!(session.selected_title(), "Method");
    }

    #[test]
    fn j_k_gg_and_g_move_the_selection_and_stop_at_the_ends() {
        let mut session = Session::new(thesis());
        session.type_keys("tj");
        assert_eq!(session.selected_title(), "Background");
        session.type_keys("3j");
        assert_eq!(session.selected_title(), "Measurements");
        session.type_keys("20j");
        assert_eq!(session.selected_title(), "Discussion");
        session.type_keys("k");
        assert_eq!(session.selected_title(), "Results");
        session.type_keys("gg");
        assert_eq!(session.selected_title(), "Introduction");
        session.type_keys("k");
        assert_eq!(session.selected_title(), "Introduction");
        session.type_keys("G");
        assert_eq!(session.selected_title(), "Discussion");
    }

    #[test]
    fn enter_jumps_to_the_heading_closes_the_outline_and_ctrl_o_comes_back() {
        let mut session = Session::new(thesis());
        session.type_keys("t5j\n");
        assert!(!session.panel.shown());
        assert_eq!(session.viewer.page(), 2);
        session.press(Key::Control('o'));
        let command = Command::JumpBack(1);
        assert!(!session.apply(command));
        session.viewer.apply(command);
        assert_eq!(session.viewer.page(), 0);
    }

    #[test]
    fn hiding_the_outline_leaves_the_view_where_it_was() {
        let mut session = Session::new(thesis());
        session.type_keys("tGt");
        assert!(!session.panel.shown());
        assert_eq!(session.viewer.page(), 0);
        session.type_keys("t\u{1b}");
        assert!(!session.panel.shown());
    }

    #[test]
    fn while_shown_the_wheel_moves_the_selection_instead_of_the_page() {
        let mut session = Session::new(thesis());
        session.type_keys("t");
        assert!(session.apply(Command::Scroll {
            columns: 0,
            rows: 2,
        }));
        assert_eq!(session.selected_title(), "Method");
        assert_eq!(session.viewer.view().top, 0);
    }

    #[test]
    fn while_shown_zooming_is_ignored_but_quitting_still_works() {
        let mut session = Session::new(thesis());
        session.type_keys("t");
        assert!(session.apply(Command::Zoom {
            steps: 1,
            anchor: None,
        }));
        assert!(!session.apply(Command::Quit));
    }

    #[test]
    fn when_hidden_page_commands_pass_through() {
        let mut session = Session::new(thesis());
        assert!(!session.apply(Command::Next(1)));
        assert!(!session.apply(Command::Click(ScreenCell { column: 0, row: 0 })));
    }

    #[test]
    fn clicking_a_heading_jumps_to_it_and_gives_the_keys_back() {
        let mut session = Session::new(thesis());
        session.type_keys("t");
        assert!(session.apply(Command::Click(ScreenCell { column: 3, row: 2 })));
        assert!(!session.panel.shown());
        assert_eq!(session.viewer.page(), 1);
        assert_eq!(session.keys.feed(Key::Char('j')), Some(Command::Next(1)));
    }

    #[test]
    fn headings_are_indented_by_depth_with_their_page_on_the_right() {
        let mut session = Session::new(thesis());
        session.type_keys("t");
        assert_eq!(
            session.drawn(20, 3),
            [
                "Introduction       1",
                "  Background       1",
                "Method             2",
            ]
        );
    }

    #[test]
    fn the_selected_heading_is_highlighted() {
        let mut session = Session::new(thesis());
        session.type_keys("tj");
        let area = Rect::new(0, 0, 20, 3);
        let mut buffer = Buffer::empty(area);
        (&session.panel).render(area, &mut buffer);
        let reversed = |y: u16| buffer[(19, y)].modifier.contains(Modifier::REVERSED);
        assert_eq!(
            [reversed(0), reversed(1), reversed(2)],
            [false, true, false]
        );
    }

    #[test]
    fn a_long_outline_scrolls_to_keep_the_selection_in_view() {
        let mut session = Session::new(thesis());
        session.type_keys("tG");
        let drawn = session.drawn(20, 3);
        assert!(drawn[2].starts_with("Discussion"), "{drawn:?}");
        session.type_keys("4k");
        let drawn = session.drawn(20, 3);
        assert!(drawn[1].starts_with("Method"), "{drawn:?}");
    }

    #[test]
    fn a_title_too_long_for_the_pane_is_cut_with_an_ellipsis() {
        let mut session =
            Session::new(vec![heading("A very long chapter title indeed", 0, 0, 0.0)]);
        session.type_keys("t");
        assert_eq!(session.drawn(16, 1), ["A very long c… 1"]);
    }

    #[test]
    fn a_pdf_without_an_outline_says_so_and_still_closes() {
        let mut session = Session::new(Vec::new());
        session.type_keys("tj\n");
        assert!(session.drawn(40, 1)[0].starts_with(NO_OUTLINE));
        assert!(!session.panel.shown());
        assert_eq!(session.viewer.page(), 0);
    }

    #[test]
    fn a_reload_keeps_the_selection_inside_a_shorter_outline() {
        let mut session = Session::new(thesis());
        session.type_keys("tG");
        session
            .panel
            .reloaded(thesis().into_iter().take(3).collect());
        assert!(session.panel.shown());
        assert_eq!(session.selected_title(), "Method");
    }

    #[test]
    fn the_status_names_the_selected_heading_number() {
        let mut session = Session::new(thesis());
        session.type_keys("tjj");
        assert_eq!(session.panel.status("doc.pdf"), "contents 3/7 · doc.pdf");
    }
}
