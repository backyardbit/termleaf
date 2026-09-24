use crate::keys::{Command, ScreenCell};
use crate::layout::{CellSize, Layout, Pane, Position, View, fit_page, fit_width};
use crate::pdf::{PageInfo, PageSize, Scale, nearest_whole};

const ZOOM_STEP: f64 = 1.1;
const MIN_PIXELS_PER_POINT: f64 = 0.1;
const MAX_PIXELS_PER_POINT: f64 = 12.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileState {
    Fresh,
    Unreadable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zoom {
    FitWidth,
    FitPage,
    Free(Scale),
}

#[derive(Debug)]
pub struct Viewer {
    pages: Vec<PageInfo>,
    zoom: Zoom,
    view: View,
    file_state: FileState,
}

impl Viewer {
    pub fn new(pages: Vec<PageInfo>, cell: CellSize, pane: Pane) -> Self {
        let sizes = sizes(&pages);
        let scale = fit_width(&sizes, cell, pane);
        Self {
            view: View {
                layout: Layout::new(sizes, scale, cell),
                pane,
                top: 0,
                left: 0,
            },
            pages,
            zoom: Zoom::FitWidth,
            file_state: FileState::Fresh,
        }
    }

    pub fn view(&self) -> &View {
        &self.view
    }

    pub fn page(&self) -> usize {
        self.view.current_page()
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    pub fn apply(&mut self, command: Command) {
        let last = self.page_count().saturating_sub(1);
        let page = self.page();
        match command {
            Command::Next(count) => self.go_to_page(page.saturating_add(count).min(last)),
            Command::Previous(count) => self.go_to_page(page.saturating_sub(count)),
            Command::First => self.go_to_page(0),
            Command::Last => self.go_to_page(last),
            Command::GoTo(number) => self.go_to_page(number.saturating_sub(1).min(last)),
            Command::Scroll { columns, rows } => self.scroll(columns, rows),
            Command::Zoom { steps, anchor } => self.zoom_by(steps, anchor),
            Command::FitWidth => self.fit(Zoom::FitWidth, None),
            Command::FitPage => self.fit(Zoom::FitPage, None),
            Command::ToggleFit(at) => {
                let zoom = if self.zoom == Zoom::FitWidth {
                    Zoom::FitPage
                } else {
                    Zoom::FitWidth
                };
                self.fit(zoom, Some(at));
            }
            Command::Click(at) => self.follow_link(at),
            Command::Quit => {}
        }
    }

    pub fn resized(&mut self, cell: CellSize, pane: Pane) {
        let page = self.page();
        let scale = self.scale_for(self.zoom, cell, pane);
        self.relayout(sizes(&self.pages), scale, cell, pane, (0.0, 0.0));
        if self.zoom == Zoom::FitPage {
            self.go_to_page(page);
        }
    }

    pub fn reloaded(&mut self, pages: Vec<PageInfo>) {
        let mut anchor = self.view.layout.position_at(self.view.point_at(0.0, 0.0));
        if anchor.page >= pages.len() {
            anchor = Position {
                page: pages.len().saturating_sub(1),
                x: anchor.x,
                y: 0.0,
            };
        }
        self.pages = pages;
        let cell = self.view.layout.cell();
        let pane = self.view.pane;
        let scale = self.scale_for(self.zoom, cell, pane);
        self.view = View {
            layout: Layout::new(sizes(&self.pages), scale, cell),
            ..self.view.clone()
        };
        let point = self.view.layout.point_of(anchor);
        self.view = self.view.clone().scrolled_to(point, 0.0, 0.0);
        self.file_state = FileState::Fresh;
    }

    pub fn unchanged(&mut self) {
        self.file_state = FileState::Fresh;
    }

    pub fn unreadable(&mut self) {
        self.file_state = FileState::Unreadable;
    }

    pub fn status_line(&self, file_name: &str, command_line: Option<&str>) -> String {
        if let Some(line) = command_line {
            return format!(":{line}");
        }
        let mut parts = vec![format!("page {}/{}", self.page() + 1, self.page_count())];
        match self.zoom {
            Zoom::FitWidth => {}
            Zoom::FitPage => parts.push("fit page".to_owned()),
            Zoom::Free(scale) => {
                let fit = self.scale_for(Zoom::FitWidth, self.view.layout.cell(), self.view.pane);
                let percent = 100.0 * scale.pixels_per_point() / fit.pixels_per_point();
                parts.push(format!("{}%", nearest_whole(percent)));
            }
        }
        parts.push(file_name.to_owned());
        if self.file_state == FileState::Unreadable {
            parts.push("✗ unreadable".to_owned());
        }
        parts.join(" · ")
    }

    fn go_to_page(&mut self, page: usize) {
        self.view.top = self.view.layout.page_top(page);
        self.view = self.view.clone().clamped();
    }

    fn scroll(&mut self, columns: i32, rows: i32) {
        self.view.top = self.view.top.saturating_add_signed(rows);
        self.view.left = self.view.left.saturating_add_signed(columns);
        self.view = self.view.clone().clamped();
    }

    fn zoom_by(&mut self, steps: i32, anchor: Option<ScreenCell>) {
        let current = self.view.layout.scale().pixels_per_point();
        let wanted =
            (current * ZOOM_STEP.powi(steps)).clamp(MIN_PIXELS_PER_POINT, MAX_PIXELS_PER_POINT);
        let scale = Scale::from_pixels_per_point(wanted);
        self.zoom = Zoom::Free(scale);
        let at = self.screen_point(anchor);
        self.relayout(
            sizes(&self.pages),
            scale,
            self.view.layout.cell(),
            self.view.pane,
            at,
        );
    }

    fn fit(&mut self, zoom: Zoom, anchor: Option<ScreenCell>) {
        let page = anchor
            .and_then(|at| self.view.page_under(at.column, at.row))
            .map_or_else(|| self.page(), |position| position.page);
        self.zoom = zoom;
        let cell = self.view.layout.cell();
        let pane = self.view.pane;
        let scale = self.scale_for(zoom, cell, pane);
        let at = self.screen_point(anchor);
        self.relayout(sizes(&self.pages), scale, cell, pane, at);
        if zoom == Zoom::FitPage {
            self.go_to_page(page);
        }
    }

    fn follow_link(&mut self, at: ScreenCell) {
        let Some(position) = self.view.page_under(at.column, at.row) else {
            return;
        };
        let Some(link) = self.pages.get(position.page).and_then(|page| {
            page.links
                .iter()
                .find(|link| link.area.contains(narrow(position.x), narrow(position.y)))
        }) else {
            return;
        };
        let target = Position {
            page: link.target.page.min(self.page_count().saturating_sub(1)),
            x: 0.0,
            y: f64::from(link.target.top.unwrap_or(0.0)),
        };
        let row = self.view.layout.point_of(target).row;
        self.view.top = nearest_whole(row.floor());
        self.view = self.view.clone().clamped();
    }

    fn screen_point(&self, anchor: Option<ScreenCell>) -> (f64, f64) {
        anchor.map_or(
            (
                f64::from(self.view.pane.columns) / 2.0,
                f64::from(self.view.pane.rows) / 2.0,
            ),
            |at| (f64::from(at.column) + 0.5, f64::from(at.row) + 0.5),
        )
    }

    fn scale_for(&self, zoom: Zoom, cell: CellSize, pane: Pane) -> Scale {
        match zoom {
            Zoom::FitWidth => fit_width(&sizes(&self.pages), cell, pane),
            Zoom::FitPage => fit_page(&sizes(&self.pages), cell, pane),
            Zoom::Free(scale) => scale,
        }
    }

    fn relayout(
        &mut self,
        sizes: Vec<PageSize>,
        scale: Scale,
        cell: CellSize,
        pane: Pane,
        (screen_column, screen_row): (f64, f64),
    ) {
        let anchor = self
            .view
            .layout
            .position_at(self.view.point_at(screen_column, screen_row));
        let layout = Layout::new(sizes, scale, cell);
        let point = layout.point_of(anchor);
        self.view = View {
            layout,
            pane,
            top: 0,
            left: 0,
        }
        .scrolled_to(point, screen_column, screen_row);
    }
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "page coordinates in points fit comfortably in f32"
)]
fn narrow(value: f64) -> f32 {
    value as f32
}

fn sizes(pages: &[PageInfo]) -> Vec<PageSize> {
    pages.iter().map(|page| page.size).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf::{Link, LinkTarget, PointRect};

    const CELL: CellSize = CellSize {
        width: 10,
        height: 20,
    };
    const PANE: Pane = Pane {
        columns: 80,
        rows: 24,
    };

    fn letter() -> PageInfo {
        PageInfo {
            size: PageSize {
                width: 612.0,
                height: 792.0,
            },
            links: Vec::new(),
        }
    }

    fn document(pages: usize) -> Vec<PageInfo> {
        vec![letter(); pages]
    }

    fn viewer(pages: usize) -> Viewer {
        Viewer::new(document(pages), CELL, PANE)
    }

    fn cell(column: u16, row: u16) -> ScreenCell {
        ScreenCell { column, row }
    }

    #[test]
    fn opens_on_the_first_page_filling_the_pane_width() {
        let viewer = viewer(3);
        assert_eq!(viewer.page(), 0);
        assert_eq!(viewer.view().layout.page_cells(0).0, 80);
    }

    #[test]
    fn next_stops_at_the_last_page() {
        let mut viewer = viewer(3);
        viewer.apply(Command::Next(10));
        assert_eq!(viewer.page(), 2);
    }

    #[test]
    fn previous_stops_at_the_first_page() {
        let mut viewer = viewer(3);
        viewer.apply(Command::Next(1));
        viewer.apply(Command::Previous(5));
        assert_eq!(viewer.page(), 0);
    }

    #[test]
    fn going_to_a_page_puts_its_top_at_the_top_of_the_view() {
        let mut viewer = viewer(5);
        viewer.apply(Command::GoTo(3));
        assert_eq!(viewer.page(), 2);
        assert_eq!(viewer.view().top, viewer.view().layout.page_top(2));
    }

    #[test]
    fn go_to_is_one_based_and_clamped() {
        let mut viewer = viewer(5);
        viewer.apply(Command::GoTo(99));
        assert_eq!(viewer.page(), 4);
        viewer.apply(Command::GoTo(0));
        assert_eq!(viewer.page(), 0);
    }

    #[test]
    fn first_and_last_jump_to_the_ends() {
        let mut viewer = viewer(4);
        viewer.apply(Command::Last);
        assert_eq!(viewer.page(), 3);
        viewer.apply(Command::First);
        assert_eq!(viewer.page(), 0);
    }

    #[test]
    fn scrolling_moves_the_view_by_rows_and_stops_at_the_top() {
        let mut viewer = viewer(3);
        viewer.apply(Command::Scroll {
            columns: 0,
            rows: 5,
        });
        assert_eq!(viewer.view().top, 5);
        viewer.apply(Command::Scroll {
            columns: 0,
            rows: -9,
        });
        assert_eq!(viewer.view().top, 0);
    }

    #[test]
    fn scrolling_flows_from_one_page_into_the_next() {
        let mut viewer = viewer(3);
        let second = viewer.view().layout.page_top(1);
        viewer.apply(Command::Scroll {
            columns: 0,
            rows: i32::try_from(second).unwrap(),
        });
        assert_eq!(viewer.page(), 1);
    }

    #[test]
    fn at_fit_width_there_is_nothing_to_pan_sideways() {
        let mut viewer = viewer(1);
        viewer.apply(Command::Scroll {
            columns: 4,
            rows: 0,
        });
        assert_eq!(viewer.view().left, 0);
    }

    #[test]
    fn zooming_in_makes_the_page_wider_than_the_pane() {
        let mut viewer = viewer(1);
        viewer.apply(Command::Zoom {
            steps: 3,
            anchor: None,
        });
        assert!(viewer.view().layout.page_cells(0).0 > 100);
    }

    #[test]
    fn zooming_keeps_the_point_under_the_pointer_in_place() {
        let mut viewer = viewer(2);
        let at = cell(30, 10);
        let before = viewer.view().page_under(at.column, at.row).unwrap();
        viewer.apply(Command::Zoom {
            steps: 4,
            anchor: Some(at),
        });
        let after = viewer.view().page_under(at.column, at.row).unwrap();
        assert_eq!(after.page, before.page);
        let tolerance = 20.0 / viewer.view().layout.scale().pixels_per_point();
        assert!((after.x - before.x).abs() <= tolerance);
        assert!((after.y - before.y).abs() <= tolerance);
    }

    #[test]
    fn zoom_has_a_limit() {
        let mut viewer = viewer(1);
        viewer.apply(Command::Zoom {
            steps: 200,
            anchor: None,
        });
        assert!(viewer.view().layout.scale().pixels_per_point() <= MAX_PIXELS_PER_POINT);
    }

    #[test]
    fn fit_page_shows_the_whole_current_page() {
        let mut viewer = viewer(3);
        viewer.apply(Command::GoTo(2));
        viewer.apply(Command::FitPage);
        assert_eq!(viewer.page(), 1);
        assert!(viewer.view().layout.page_cells(1).1 <= PANE.rows);
        assert_eq!(viewer.view().top, viewer.view().layout.page_top(1));
    }

    #[test]
    fn fit_width_returns_from_a_free_zoom() {
        let mut viewer = viewer(1);
        viewer.apply(Command::Zoom {
            steps: 2,
            anchor: None,
        });
        viewer.apply(Command::FitWidth);
        assert_eq!(viewer.zoom, Zoom::FitWidth);
        assert_eq!(viewer.view().layout.page_cells(0).0, 80);
    }

    #[test]
    fn toggling_the_fit_switches_between_width_and_page() {
        let mut viewer = viewer(2);
        viewer.apply(Command::ToggleFit(cell(10, 10)));
        assert_eq!(viewer.zoom, Zoom::FitPage);
        viewer.apply(Command::ToggleFit(cell(10, 10)));
        assert_eq!(viewer.zoom, Zoom::FitWidth);
    }

    #[test]
    fn toggling_the_fit_from_a_free_zoom_returns_to_fit_width() {
        let mut viewer = viewer(2);
        viewer.apply(Command::Zoom {
            steps: 2,
            anchor: None,
        });
        viewer.apply(Command::ToggleFit(cell(10, 10)));
        assert_eq!(viewer.zoom, Zoom::FitWidth);
    }

    fn linked_document() -> Vec<PageInfo> {
        let mut pages = document(3);
        pages[0].links.push(Link {
            area: PointRect {
                x0: 100.0,
                y0: 100.0,
                x1: 200.0,
                y1: 120.0,
            },
            target: LinkTarget {
                page: 2,
                top: Some(300.0),
            },
        });
        pages
    }

    fn cell_over(viewer: &Viewer, x: f64, y: f64) -> ScreenCell {
        let layout = &viewer.view().layout;
        let point = layout.point_of(Position { page: 0, x, y });
        ScreenCell {
            column: u16::try_from(nearest_whole(point.column.floor())).unwrap(),
            row: u16::try_from(nearest_whole(point.row.floor())).unwrap(),
        }
    }

    #[test]
    fn clicking_a_link_jumps_to_its_target() {
        let mut viewer = Viewer::new(linked_document(), CELL, PANE);
        let on_link = cell_over(&viewer, 150.0, 110.0);
        viewer.apply(Command::Click(on_link));
        let layout = &viewer.view().layout;
        let expected = layout.point_of(Position {
            page: 2,
            x: 0.0,
            y: 300.0,
        });
        assert_eq!(viewer.view().top, nearest_whole(expected.row.floor()));
    }

    #[test]
    fn clicking_beside_a_link_does_nothing() {
        let mut viewer = Viewer::new(linked_document(), CELL, PANE);
        let off_link = cell_over(&viewer, 400.0, 110.0);
        viewer.apply(Command::Click(off_link));
        assert_eq!(viewer.view().top, 0);
    }

    #[test]
    fn reload_keeps_the_page() {
        let mut viewer = viewer(5);
        viewer.apply(Command::GoTo(3));
        viewer.reloaded(document(6));
        assert_eq!(viewer.page(), 2);
        assert_eq!(viewer.view().top, viewer.view().layout.page_top(2));
    }

    #[test]
    fn reload_keeps_the_scroll_position_inside_the_page() {
        let mut viewer = viewer(5);
        viewer.apply(Command::GoTo(2));
        viewer.apply(Command::Scroll {
            columns: 0,
            rows: 7,
        });
        let top = viewer.view().top;
        viewer.reloaded(document(5));
        assert_eq!(viewer.view().top, top);
    }

    #[test]
    fn reload_clamps_when_the_document_shrinks() {
        let mut viewer = viewer(10);
        viewer.apply(Command::GoTo(9));
        viewer.reloaded(document(4));
        assert_eq!(viewer.page(), 3);
    }

    #[test]
    fn a_wider_pane_keeps_fit_width_filling_it_and_keeps_the_page() {
        let mut viewer = viewer(4);
        viewer.apply(Command::GoTo(3));
        viewer.resized(
            CELL,
            Pane {
                columns: 120,
                rows: 30,
            },
        );
        assert_eq!(viewer.view().layout.page_cells(0).0, 120);
        assert_eq!(viewer.page(), 2);
    }

    #[test]
    fn a_good_reload_clears_the_unreadable_marker() {
        let mut viewer = viewer(2);
        viewer.unreadable();
        viewer.reloaded(document(2));
        assert_eq!(viewer.status_line("a.pdf", None), "page 1/2 · a.pdf");
    }

    #[test]
    fn status_line_shows_position_and_file() {
        let mut viewer = viewer(12);
        viewer.apply(Command::GoTo(3));
        assert_eq!(
            viewer.status_line("thesis.pdf", None),
            "page 3/12 · thesis.pdf"
        );
    }

    #[test]
    fn status_line_marks_an_unreadable_file() {
        let mut viewer = viewer(12);
        viewer.unreadable();
        assert_eq!(
            viewer.status_line("thesis.pdf", None),
            "page 1/12 · thesis.pdf · ✗ unreadable"
        );
    }

    #[test]
    fn status_line_shows_the_zoom_relative_to_fit_width() {
        let mut viewer = viewer(12);
        viewer.apply(Command::Zoom {
            steps: 1,
            anchor: None,
        });
        assert_eq!(
            viewer.status_line("thesis.pdf", None),
            "page 1/12 · 110% · thesis.pdf"
        );
        viewer.apply(Command::FitPage);
        assert_eq!(
            viewer.status_line("thesis.pdf", None),
            "page 1/12 · fit page · thesis.pdf"
        );
    }

    #[test]
    fn status_line_shows_the_command_line_while_typing() {
        let viewer = viewer(12);
        assert_eq!(viewer.status_line("thesis.pdf", Some("4")), ":4");
    }
}
