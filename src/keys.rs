#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Escape,
    Backspace,
    Interrupt,
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
    Quit,
}

#[derive(Debug, Default)]
pub struct KeyParser {
    count: Option<usize>,
    awaiting_second_g: bool,
    command_line: Option<String>,
}

impl KeyParser {
    pub fn command_line(&self) -> Option<&str> {
        self.command_line.as_deref()
    }

    pub fn feed(&mut self, key: Key) -> Option<Command> {
        if key == Key::Interrupt {
            return Some(Command::Quit);
        }
        if self.command_line.is_some() {
            return self.feed_command_line(key);
        }
        let Key::Char(character) = key else {
            self.reset();
            return None;
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
                    'j' => Some(Command::Next(repeat)),
                    'k' => Some(Command::Previous(repeat)),
                    'G' => Some(count.map_or(Command::Last, Command::GoTo)),
                    '+' | '=' => Some(zoom(repeat, 1)),
                    '-' => Some(zoom(repeat, -1)),
                    's' => Some(Command::FitWidth),
                    'a' => Some(Command::FitPage),
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
        }
    }

    fn reset(&mut self) {
        self.count = None;
        self.awaiting_second_g = false;
    }
}

fn zoom(repeat: usize, direction: i32) -> Command {
    Command::Zoom {
        steps: i32::try_from(repeat).unwrap_or(i32::MAX) * direction,
        anchor: None,
    }
}

fn parse_command_line(line: &str) -> Option<Command> {
    match line.trim() {
        "q" | "quit" => Some(Command::Quit),
        "$" => Some(Command::Last),
        number => number.parse().ok().map(Command::GoTo),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
