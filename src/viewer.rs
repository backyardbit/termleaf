use crate::keys::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileState {
    Fresh,
    Unreadable,
}

#[derive(Debug)]
pub struct Viewer {
    page: usize,
    page_count: usize,
    file_state: FileState,
}

impl Viewer {
    pub fn new(page_count: usize) -> Self {
        Self {
            page: 0,
            page_count: page_count.max(1),
            file_state: FileState::Fresh,
        }
    }

    pub fn page(&self) -> usize {
        self.page
    }

    pub fn page_count(&self) -> usize {
        self.page_count
    }

    pub fn apply(&mut self, command: Command) {
        let last = self.page_count - 1;
        self.page = match command {
            Command::Next(count) => self.page.saturating_add(count).min(last),
            Command::Previous(count) => self.page.saturating_sub(count),
            Command::First => 0,
            Command::Last => last,
            Command::GoTo(number) => number.saturating_sub(1).min(last),
            Command::Quit => self.page,
        };
    }

    pub fn reloaded(&mut self, page_count: usize) {
        self.page_count = page_count.max(1);
        self.page = self.page.min(self.page_count - 1);
        self.file_state = FileState::Fresh;
    }

    pub fn unreadable(&mut self) {
        self.file_state = FileState::Unreadable;
    }

    pub fn status_line(&self, file_name: &str, command_line: Option<&str>) -> String {
        if let Some(line) = command_line {
            return format!(":{line}");
        }
        let position = format!("page {}/{}", self.page + 1, self.page_count);
        match self.file_state {
            FileState::Fresh => format!("{position} · {file_name}"),
            FileState::Unreadable => format!("{position} · {file_name} · ✗ unreadable"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_stops_at_the_last_page() {
        let mut viewer = Viewer::new(3);
        viewer.apply(Command::Next(10));
        assert_eq!(viewer.page(), 2);
    }

    #[test]
    fn previous_stops_at_the_first_page() {
        let mut viewer = Viewer::new(3);
        viewer.apply(Command::Next(1));
        viewer.apply(Command::Previous(5));
        assert_eq!(viewer.page(), 0);
    }

    #[test]
    fn go_to_is_one_based_and_clamped() {
        let mut viewer = Viewer::new(5);
        viewer.apply(Command::GoTo(3));
        assert_eq!(viewer.page(), 2);
        viewer.apply(Command::GoTo(99));
        assert_eq!(viewer.page(), 4);
        viewer.apply(Command::GoTo(0));
        assert_eq!(viewer.page(), 0);
    }

    #[test]
    fn first_and_last_jump_to_the_ends() {
        let mut viewer = Viewer::new(4);
        viewer.apply(Command::Last);
        assert_eq!(viewer.page(), 3);
        viewer.apply(Command::First);
        assert_eq!(viewer.page(), 0);
    }

    #[test]
    fn reload_keeps_the_page() {
        let mut viewer = Viewer::new(5);
        viewer.apply(Command::GoTo(3));
        viewer.reloaded(6);
        assert_eq!(viewer.page(), 2);
    }

    #[test]
    fn reload_clamps_when_the_document_shrinks() {
        let mut viewer = Viewer::new(10);
        viewer.apply(Command::GoTo(9));
        viewer.reloaded(4);
        assert_eq!(viewer.page(), 3);
    }

    #[test]
    fn a_good_reload_clears_the_unreadable_marker() {
        let mut viewer = Viewer::new(2);
        viewer.unreadable();
        viewer.reloaded(2);
        assert_eq!(viewer.status_line("a.pdf", None), "page 1/2 · a.pdf");
    }

    #[test]
    fn status_line_shows_position_and_file() {
        let mut viewer = Viewer::new(12);
        viewer.apply(Command::GoTo(3));
        assert_eq!(
            viewer.status_line("thesis.pdf", None),
            "page 3/12 · thesis.pdf"
        );
    }

    #[test]
    fn status_line_marks_an_unreadable_file() {
        let mut viewer = Viewer::new(12);
        viewer.unreadable();
        assert_eq!(
            viewer.status_line("thesis.pdf", None),
            "page 1/12 · thesis.pdf · ✗ unreadable"
        );
    }

    #[test]
    fn status_line_shows_the_command_line_while_typing() {
        let viewer = Viewer::new(12);
        assert_eq!(viewer.status_line("thesis.pdf", Some("4")), ":4");
    }
}
