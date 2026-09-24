use std::time::{Duration, Instant};

use crate::keys::{Command, ScreenCell};

const DOUBLE_CLICK: Duration = Duration::from_millis(400);
const WHEEL_ROWS: i32 = 1;
const WHEEL_COLUMNS: i32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wheel {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseInput {
    Press(ScreenCell),
    Drag(ScreenCell),
    Release(ScreenCell),
    Wheel {
        direction: Wheel,
        zoom: bool,
        sideways: bool,
        at: ScreenCell,
    },
}

#[derive(Debug, Clone, Copy)]
struct Press {
    last: ScreenCell,
    dragged: bool,
    double: bool,
}

#[derive(Debug, Default)]
pub struct Gestures {
    press: Option<Press>,
    last_click: Option<(ScreenCell, Instant)>,
}

impl Gestures {
    pub fn feed(&mut self, input: MouseInput, now: Instant) -> Option<Command> {
        match input {
            MouseInput::Wheel {
                direction,
                zoom,
                sideways,
                at,
            } => Some(wheel(direction, zoom, sideways, at)),
            MouseInput::Press(at) => {
                let double = self.last_click.is_some_and(|(clicked, when)| {
                    now.duration_since(when) <= DOUBLE_CLICK && near(clicked, at)
                });
                self.press = Some(Press {
                    last: at,
                    dragged: false,
                    double,
                });
                if double {
                    self.last_click = None;
                    return Some(Command::ToggleFit(at));
                }
                None
            }
            MouseInput::Drag(at) => {
                let press = self.press.as_mut()?;
                let columns = i32::from(press.last.column) - i32::from(at.column);
                let rows = i32::from(press.last.row) - i32::from(at.row);
                press.last = at;
                if columns == 0 && rows == 0 {
                    return None;
                }
                press.dragged = true;
                self.last_click = None;
                Some(Command::Scroll { columns, rows })
            }
            MouseInput::Release(at) => {
                let press = self.press.take()?;
                if press.dragged || press.double {
                    return None;
                }
                self.last_click = Some((at, now));
                Some(Command::Click(at))
            }
        }
    }
}

fn wheel(direction: Wheel, zoom: bool, sideways: bool, at: ScreenCell) -> Command {
    let forward = match direction {
        Wheel::Down | Wheel::Right => 1,
        Wheel::Up | Wheel::Left => -1,
    };
    let vertical = matches!(direction, Wheel::Up | Wheel::Down);
    if zoom && vertical {
        return Command::Zoom {
            steps: -forward,
            anchor: Some(at),
        };
    }
    if vertical && !sideways {
        Command::Scroll {
            columns: 0,
            rows: forward * WHEEL_ROWS,
        }
    } else {
        Command::Scroll {
            columns: forward * WHEEL_COLUMNS,
            rows: 0,
        }
    }
}

fn near(a: ScreenCell, b: ScreenCell) -> bool {
    a.column.abs_diff(b.column) <= 1 && a.row.abs_diff(b.row) <= 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(column: u16, row: u16) -> ScreenCell {
        ScreenCell { column, row }
    }

    fn wheel_input(direction: Wheel, zoom: bool, sideways: bool) -> MouseInput {
        MouseInput::Wheel {
            direction,
            zoom,
            sideways,
            at: cell(4, 7),
        }
    }

    fn feed_all(inputs: &[(MouseInput, u64)]) -> Vec<Command> {
        let start = Instant::now();
        let mut gestures = Gestures::default();
        inputs
            .iter()
            .filter_map(|(input, millis)| {
                gestures.feed(*input, start + Duration::from_millis(*millis))
            })
            .collect()
    }

    #[test]
    fn the_wheel_scrolls_one_row_at_a_time() {
        assert_eq!(
            feed_all(&[
                (wheel_input(Wheel::Down, false, false), 0),
                (wheel_input(Wheel::Up, false, false), 0)
            ]),
            [
                Command::Scroll {
                    columns: 0,
                    rows: 1
                },
                Command::Scroll {
                    columns: 0,
                    rows: -1
                }
            ]
        );
    }

    #[test]
    fn a_sideways_wheel_pans_across() {
        assert_eq!(
            feed_all(&[(wheel_input(Wheel::Right, false, false), 0)]),
            [Command::Scroll {
                columns: WHEEL_COLUMNS,
                rows: 0
            }]
        );
    }

    #[test]
    fn shift_turns_the_wheel_sideways() {
        assert_eq!(
            feed_all(&[(wheel_input(Wheel::Up, false, true), 0)]),
            [Command::Scroll {
                columns: -WHEEL_COLUMNS,
                rows: 0
            }]
        );
    }

    #[test]
    fn control_wheel_zooms_at_the_pointer() {
        assert_eq!(
            feed_all(&[
                (wheel_input(Wheel::Up, true, false), 0),
                (wheel_input(Wheel::Down, true, false), 0)
            ]),
            [
                Command::Zoom {
                    steps: 1,
                    anchor: Some(cell(4, 7))
                },
                Command::Zoom {
                    steps: -1,
                    anchor: Some(cell(4, 7))
                }
            ]
        );
    }

    #[test]
    fn press_and_release_in_place_is_a_click() {
        assert_eq!(
            feed_all(&[
                (MouseInput::Press(cell(3, 3)), 0),
                (MouseInput::Release(cell(3, 3)), 50)
            ]),
            [Command::Click(cell(3, 3))]
        );
    }

    #[test]
    fn dragging_pulls_the_page_along_with_the_pointer() {
        assert_eq!(
            feed_all(&[
                (MouseInput::Press(cell(10, 10)), 0),
                (MouseInput::Drag(cell(8, 13)), 10),
                (MouseInput::Drag(cell(8, 14)), 20),
                (MouseInput::Release(cell(8, 14)), 30)
            ]),
            [
                Command::Scroll {
                    columns: 2,
                    rows: -3
                },
                Command::Scroll {
                    columns: 0,
                    rows: -1
                }
            ]
        );
    }

    #[test]
    fn a_drag_without_a_press_does_nothing() {
        assert!(feed_all(&[(MouseInput::Drag(cell(1, 1)), 0)]).is_empty());
    }

    #[test]
    fn a_quick_second_click_toggles_the_fit_at_the_pointer() {
        assert_eq!(
            feed_all(&[
                (MouseInput::Press(cell(5, 5)), 0),
                (MouseInput::Release(cell(5, 5)), 50),
                (MouseInput::Press(cell(5, 6)), 200),
                (MouseInput::Release(cell(5, 6)), 250)
            ]),
            [Command::Click(cell(5, 5)), Command::ToggleFit(cell(5, 6))]
        );
    }

    #[test]
    fn a_slow_second_click_is_just_another_click() {
        assert_eq!(
            feed_all(&[
                (MouseInput::Press(cell(5, 5)), 0),
                (MouseInput::Release(cell(5, 5)), 50),
                (MouseInput::Press(cell(5, 5)), 900),
                (MouseInput::Release(cell(5, 5)), 950)
            ]),
            [Command::Click(cell(5, 5)), Command::Click(cell(5, 5))]
        );
    }

    #[test]
    fn clicks_far_apart_are_not_a_double_click() {
        assert_eq!(
            feed_all(&[
                (MouseInput::Press(cell(5, 5)), 0),
                (MouseInput::Release(cell(5, 5)), 50),
                (MouseInput::Press(cell(20, 5)), 100),
                (MouseInput::Release(cell(20, 5)), 150)
            ]),
            [Command::Click(cell(5, 5)), Command::Click(cell(20, 5))]
        );
    }
}
