use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

const ARROW_ROWS: i32 = 3;
const PAN_COLUMNS: i32 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Control(char),
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Enter,
    Escape,
    Backspace,
    Tab,
    Interrupt,
}

pub fn decode(key: KeyEvent) -> Option<Key> {
    if key.kind != KeyEventKind::Press {
        return None;
    }
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('c') if control => Some(Key::Interrupt),
        KeyCode::Char(character) if control => Some(Key::Control(character)),
        KeyCode::Char(character) => Some(Key::Char(character)),
        KeyCode::Up => Some(Key::Up),
        KeyCode::Down => Some(Key::Down),
        KeyCode::Left => Some(Key::Left),
        KeyCode::Right => Some(Key::Right),
        KeyCode::PageUp => Some(Key::PageUp),
        KeyCode::PageDown => Some(Key::PageDown),
        KeyCode::Enter => Some(Key::Enter),
        KeyCode::Esc => Some(Key::Escape),
        KeyCode::Backspace => Some(Key::Backspace),
        KeyCode::Tab => Some(Key::Tab),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenCell {
    pub column: u16,
    pub row: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Next(usize),
    Previous(usize),
    First,
    Last,
    GoTo(usize),
    Scroll {
        columns: i32,
        rows: i32,
    },
    ScrollHalfPanes(i32),
    Zoom {
        steps: i32,
        anchor: Option<ScreenCell>,
    },
    Magnify {
        per_mille: u32,
        anchor: Option<ScreenCell>,
    },
    FitWidth,
    FitPage,
    ToggleFit(ScreenCell),
    Click(ScreenCell),
    Inverse(Option<ScreenCell>),
    ToggleFollow,
    SetFollow(bool),
    SearchSubmit,
    SearchNext(usize),
    SearchPrevious(usize),
    SearchDismiss,
    JumpBack(usize),
    JumpForward(usize),
    Quit,
}

#[derive(Debug, Default)]
pub struct KeyParser {
    count: Option<usize>,
    awaiting_second_g: bool,
    command_line: Option<String>,
    search_line: Option<String>,
}

impl KeyParser {
    pub fn search_line(&self) -> Option<&str> {
        self.search_line.as_deref()
    }

    pub fn take_search(&mut self) -> String {
        self.search_line.take().unwrap_or_default()
    }

    fn feed_search(&mut self, key: Key) -> Option<Command> {
        match key {
            Key::Enter => return Some(Command::SearchSubmit),
            Key::Escape => self.search_line = None,
            Key::Backspace => {
                if let Some(line) = &mut self.search_line {
                    line.pop();
                }
            }
            Key::Char(character) if !character.is_control() => {
                if let Some(line) = &mut self.search_line {
                    line.push(character);
                }
            }
            _ => {}
        }
        None
    }

    pub fn command_line(&self) -> Option<&str> {
        self.command_line.as_deref()
    }

    pub fn feed(&mut self, key: Key) -> Option<Command> {
        if key == Key::Interrupt {
            return Some(Command::Quit);
        }
        if self.search_line.is_some() {
            return self.feed_search(key);
        }
        if self.command_line.is_some() {
            return self.feed_command_line(key);
        }
        if key == Key::Escape {
            self.reset();
            return Some(Command::SearchDismiss);
        }
        let character = match key {
            Key::Char(character) => character,
            Key::Control('o') => return Some(Command::JumpBack(self.take_count())),
            Key::Tab | Key::Control('i') => return Some(Command::JumpForward(self.take_count())),
            other => return navigate(other, self.take_count()),
        };
        if self.awaiting_second_g {
            let count = self.count;
            self.reset();
            return (character == 'g').then(|| count.map_or(Command::First, Command::GoTo));
        }
        match character {
            '1'..='9' => self.push_digit(character),
            '0' if self.count.is_some() => self.push_digit(character),
            'g' => {
                self.awaiting_second_g = true;
                None
            }
            '/' => {
                self.reset();
                self.search_line = Some(String::new());
                None
            }
            ':' => {
                self.reset();
                self.command_line = Some(String::new());
                None
            }
            _ => {
                let count = self.count;
                self.reset();
                let repeat = count.unwrap_or(1);
                match character {
                    'n' => Some(Command::SearchNext(repeat)),
                    'N' => Some(Command::SearchPrevious(repeat)),
                    'j' => Some(Command::Next(repeat)),
                    'k' => Some(Command::Previous(repeat)),
                    'h' => navigate(Key::Left, repeat),
                    'l' => navigate(Key::Right, repeat),
                    'G' => Some(count.map_or(Command::Last, Command::GoTo)),
                    '+' | '=' => Some(zoom(repeat, 1)),
                    '-' => Some(zoom(repeat, -1)),
                    's' => Some(Command::FitWidth),
                    'a' => Some(Command::FitPage),
                    'e' => Some(Command::Inverse(None)),
                    'F' => Some(Command::ToggleFollow),
                    'q' => Some(Command::Quit),
                    _ => None,
                }
            }
        }
    }

    fn push_digit(&mut self, digit: char) -> Option<Command> {
        let value = digit.to_digit(10).and_then(|d| usize::try_from(d).ok());
        self.count = value.map(|d| self.count.unwrap_or(0).saturating_mul(10).saturating_add(d));
        None
    }

    fn feed_command_line(&mut self, key: Key) -> Option<Command> {
        let line = self.command_line.get_or_insert_with(String::new);
        match key {
            Key::Char(character) => {
                line.push(character);
                None
            }
            Key::Backspace => {
                if line.pop().is_none() {
                    self.command_line = None;
                }
                None
            }
            Key::Enter => {
                let command = parse_command_line(line);
                self.command_line = None;
                command
            }
            Key::Escape | Key::Interrupt => {
                self.command_line = None;
                None
            }
            Key::Tab
            | Key::Control(_)
            | Key::Up
            | Key::Down
            | Key::Left
            | Key::Right
            | Key::PageUp
            | Key::PageDown => None,
        }
    }

    fn take_count(&mut self) -> usize {
        let count = self.count.unwrap_or(1);
        self.reset();
        count
    }

    fn reset(&mut self) {
        self.count = None;
        self.awaiting_second_g = false;
    }
}

fn times(repeat: usize, step: i32) -> i32 {
    i32::try_from(repeat)
        .unwrap_or(i32::MAX)
        .saturating_mul(step)
}

fn navigate(key: Key, repeat: usize) -> Option<Command> {
    let scroll = |columns, rows| {
        Some(Command::Scroll {
            columns: times(repeat, columns),
            rows: times(repeat, rows),
        })
    };
    match key {
        Key::Up => scroll(0, -ARROW_ROWS),
        Key::Down => scroll(0, ARROW_ROWS),
        Key::Left => scroll(-PAN_COLUMNS, 0),
        Key::Right => scroll(PAN_COLUMNS, 0),
        Key::Control('d') => Some(Command::ScrollHalfPanes(times(repeat, 1))),
        Key::Control('u') => Some(Command::ScrollHalfPanes(times(repeat, -1))),
        Key::PageDown => Some(Command::ScrollHalfPanes(times(repeat, 2))),
        Key::PageUp => Some(Command::ScrollHalfPanes(times(repeat, -2))),
        _ => None,
    }
}

fn zoom(repeat: usize, direction: i32) -> Command {
    Command::Zoom {
        steps: times(repeat, direction),
        anchor: None,
    }
}

fn parse_command_line(line: &str) -> Option<Command> {
    match line.trim() {
        "q" | "quit" => Some(Command::Quit),
        "$" => Some(Command::Last),
        "follow" => Some(Command::SetFollow(true)),
        "nofollow" => Some(Command::SetFollow(false)),
        number => number.parse().ok().map(Command::GoTo),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_text_keeps_navigation_keys_and_unicode_in_the_prompt() {
        let mut keys = KeyParser::default();
        for character in "/nN café".chars() {
            assert_eq!(keys.feed(Key::Char(character)), None);
        }
        assert_eq!(keys.search_line(), Some("nN café"));
        assert_eq!(keys.feed(Key::Backspace), None);
        assert_eq!(keys.search_line(), Some("nN caf"));
        assert_eq!(keys.feed(Key::Enter), Some(Command::SearchSubmit));
        assert_eq!(keys.take_search(), "nN caf");
        assert_eq!(keys.feed(Key::Char('n')), Some(Command::SearchNext(1)));
        assert_eq!(keys.feed(Key::Char('N')), Some(Command::SearchPrevious(1)));
        assert_eq!(keys.feed(Key::Escape), Some(Command::SearchDismiss));
    }

    #[test]
    fn escape_cancels_only_the_unsubmitted_prompt() {
        let mut keys = KeyParser::default();
        keys.feed(Key::Char('/'));
        keys.feed(Key::Char('q'));
        assert_eq!(keys.feed(Key::Escape), None);
        assert_eq!(keys.search_line(), None);
        assert_eq!(keys.feed(Key::Char('q')), Some(Command::Quit));
    }

    fn run(input: &str) -> Vec<Command> {
        let mut parser = KeyParser::default();
        input
            .chars()
            .map(|c| match c {
                '\n' => Key::Enter,
                '\u{1b}' => Key::Escape,
                '\u{8}' => Key::Backspace,
                other => Key::Char(other),
            })
            .filter_map(|key| parser.feed(key))
            .collect()
    }

    #[test]
    fn control_o_jumps_back_and_tab_or_control_i_jumps_forward() {
        let mut keys = KeyParser::default();
        assert_eq!(keys.feed(Key::Control('o')), Some(Command::JumpBack(1)));
        assert_eq!(keys.feed(Key::Tab), Some(Command::JumpForward(1)));
        assert_eq!(keys.feed(Key::Control('i')), Some(Command::JumpForward(1)));
        keys.feed(Key::Char('3'));
        assert_eq!(keys.feed(Key::Control('o')), Some(Command::JumpBack(3)));
    }

    #[test]
    fn jump_keys_do_nothing_inside_a_prompt() {
        let mut keys = KeyParser::default();
        keys.feed(Key::Char(':'));
        assert_eq!(keys.feed(Key::Control('o')), None);
        assert_eq!(keys.feed(Key::Tab), None);
        assert_eq!(keys.command_line(), Some(""));
        keys.feed(Key::Escape);
        keys.feed(Key::Char('/'));
        assert_eq!(keys.feed(Key::Control('o')), None);
        assert_eq!(keys.search_line(), Some(""));
    }

    #[test]
    fn capital_f_and_the_follow_commands_switch_follow() {
        assert_eq!(
            run("F:nofollow\n:follow\n"),
            [
                Command::ToggleFollow,
                Command::SetFollow(false),
                Command::SetFollow(true)
            ]
        );
    }

    #[test]
    fn j_and_k_move_one_page() {
        assert_eq!(run("jk"), [Command::Next(1), Command::Previous(1)]);
    }

    #[test]
    fn counts_repeat_motions() {
        assert_eq!(run("5j12k"), [Command::Next(5), Command::Previous(12)]);
    }

    #[test]
    fn zero_extends_a_count() {
        assert_eq!(run("10j"), [Command::Next(10)]);
    }

    #[test]
    fn gg_and_capital_g_jump_to_the_ends() {
        assert_eq!(run("ggG"), [Command::First, Command::Last]);
    }

    #[test]
    fn counted_jumps_go_to_a_page() {
        assert_eq!(run("7G3gg"), [Command::GoTo(7), Command::GoTo(3)]);
    }

    #[test]
    fn a_stray_g_is_dropped() {
        assert_eq!(run("gxj"), [Command::Next(1)]);
    }

    #[test]
    fn colon_number_goes_to_a_page() {
        assert_eq!(run(":42\n"), [Command::GoTo(42)]);
    }

    #[test]
    fn colon_dollar_goes_to_the_last_page() {
        assert_eq!(run(":$\n"), [Command::Last]);
    }

    #[test]
    fn colon_q_quits() {
        assert_eq!(run(":q\n"), [Command::Quit]);
    }

    #[test]
    fn escape_cancels_the_command_line() {
        assert_eq!(run(":42\u{1b}j"), [Command::Next(1)]);
    }

    #[test]
    fn backspace_on_an_empty_command_line_leaves_it() {
        assert_eq!(run(":\u{8}j"), [Command::Next(1)]);
    }

    #[test]
    fn command_line_is_visible_while_typing() {
        let mut parser = KeyParser::default();
        parser.feed(Key::Char(':'));
        parser.feed(Key::Char('4'));
        assert_eq!(parser.command_line(), Some("4"));
    }

    #[test]
    fn interrupt_always_quits() {
        let mut parser = KeyParser::default();
        parser.feed(Key::Char(':'));
        assert_eq!(parser.feed(Key::Interrupt), Some(Command::Quit));
    }

    #[test]
    fn plus_and_minus_zoom_around_the_view() {
        assert_eq!(
            run("+-="),
            [
                Command::Zoom {
                    steps: 1,
                    anchor: None
                },
                Command::Zoom {
                    steps: -1,
                    anchor: None
                },
                Command::Zoom {
                    steps: 1,
                    anchor: None
                },
            ]
        );
    }

    #[test]
    fn a_count_zooms_several_steps() {
        assert_eq!(
            run("3-"),
            [Command::Zoom {
                steps: -3,
                anchor: None
            }]
        );
    }

    #[test]
    fn s_and_a_fit_the_width_and_the_page() {
        assert_eq!(run("sa"), [Command::FitWidth, Command::FitPage]);
    }

    #[test]
    fn q_quits() {
        assert_eq!(run("q"), [Command::Quit]);
    }

    fn feed_all(keys: &[Key]) -> Vec<Command> {
        let mut parser = KeyParser::default();
        keys.iter().filter_map(|&key| parser.feed(key)).collect()
    }

    fn pan(columns: i32) -> Command {
        Command::Scroll { columns, rows: 0 }
    }

    fn scroll(rows: i32) -> Command {
        Command::Scroll { columns: 0, rows }
    }

    #[test]
    fn control_d_and_control_u_scroll_half_a_pane() {
        assert_eq!(
            feed_all(&[Key::Control('d'), Key::Control('u')]),
            [Command::ScrollHalfPanes(1), Command::ScrollHalfPanes(-1)]
        );
    }

    #[test]
    fn page_down_and_page_up_scroll_a_whole_pane() {
        assert_eq!(
            feed_all(&[Key::PageDown, Key::PageUp]),
            [Command::ScrollHalfPanes(2), Command::ScrollHalfPanes(-2)]
        );
    }

    #[test]
    fn a_count_repeats_pane_scrolls() {
        assert_eq!(
            feed_all(&[
                Key::Char('3'),
                Key::Control('d'),
                Key::Char('2'),
                Key::PageUp
            ]),
            [Command::ScrollHalfPanes(3), Command::ScrollHalfPanes(-4)]
        );
    }

    #[test]
    fn h_and_l_pan_sideways() {
        assert_eq!(
            run("hl2l"),
            [pan(-PAN_COLUMNS), pan(PAN_COLUMNS), pan(2 * PAN_COLUMNS)]
        );
    }

    #[test]
    fn arrows_scroll_and_pan() {
        assert_eq!(
            feed_all(&[Key::Down, Key::Up, Key::Left, Key::Right]),
            [
                scroll(ARROW_ROWS),
                scroll(-ARROW_ROWS),
                pan(-PAN_COLUMNS),
                pan(PAN_COLUMNS)
            ]
        );
    }

    #[test]
    fn a_count_repeats_arrow_scrolls() {
        assert_eq!(
            feed_all(&[Key::Char('4'), Key::Down]),
            [scroll(4 * ARROW_ROWS)]
        );
    }

    #[test]
    fn navigation_keys_leave_a_command_line_untouched() {
        let mut parser = KeyParser::default();
        for key in [
            Key::Char(':'),
            Key::Char('4'),
            Key::Left,
            Key::PageDown,
            Key::Control('d'),
        ] {
            assert_eq!(parser.feed(key), None);
        }
        assert_eq!(parser.command_line(), Some("4"));
        assert_eq!(parser.feed(Key::Enter), Some(Command::GoTo(4)));
    }

    #[test]
    fn navigation_keys_leave_a_search_prompt_untouched() {
        let mut parser = KeyParser::default();
        for key in [Key::Char('/'), Key::Char('x'), Key::Down, Key::Control('u')] {
            assert_eq!(parser.feed(key), None);
        }
        assert_eq!(parser.search_line(), Some("x"));
    }

    fn press(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn control_c_interrupts() {
        assert_eq!(
            decode(press(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Key::Interrupt)
        );
    }

    #[test]
    fn other_control_letters_decode_as_control_keys() {
        assert_eq!(
            decode(press(KeyCode::Char('d'), KeyModifiers::CONTROL)),
            Some(Key::Control('d'))
        );
        assert_eq!(
            decode(press(KeyCode::Char('u'), KeyModifiers::CONTROL)),
            Some(Key::Control('u'))
        );
    }

    #[test]
    fn control_o_tab_and_control_i_decode_as_jump_keys() {
        assert_eq!(
            decode(press(KeyCode::Char('o'), KeyModifiers::CONTROL)),
            Some(Key::Control('o'))
        );
        assert_eq!(
            decode(press(KeyCode::Char('i'), KeyModifiers::CONTROL)),
            Some(Key::Control('i'))
        );
        assert_eq!(
            decode(press(KeyCode::Tab, KeyModifiers::NONE)),
            Some(Key::Tab)
        );
    }

    #[test]
    fn arrows_and_page_keys_decode() {
        let decoded: Vec<_> = [
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::PageUp,
            KeyCode::PageDown,
        ]
        .into_iter()
        .map(|code| decode(press(code, KeyModifiers::NONE)))
        .collect();
        assert_eq!(
            decoded,
            [
                Some(Key::Up),
                Some(Key::Down),
                Some(Key::Left),
                Some(Key::Right),
                Some(Key::PageUp),
                Some(Key::PageDown)
            ]
        );
    }

    #[test]
    fn shifted_letters_stay_plain_characters() {
        assert_eq!(
            decode(press(KeyCode::Char('G'), KeyModifiers::SHIFT)),
            Some(Key::Char('G'))
        );
    }

    #[test]
    fn key_releases_are_ignored() {
        let release = KeyEvent::new_with_kind(
            KeyCode::Char('j'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        );
        assert_eq!(decode(release), None);
    }
}
